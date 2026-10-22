//! HTTPS downloads through WinHTTP, the HTTP client built into Windows (it uses
//! the system's proxy settings and certificates). Blocking: call it from a
//! worker thread only. Used by Download from Twitter (`download.rs`).

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_QUERY_CONTENT_LENGTH, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::{HSTRING, PCWSTR, w};

const AGENT: &str = concat!("Mozilla/5.0 (Windows NT 10.0; Win64; x64) PocketMedia/", env!("CARGO_PKG_VERSION"));
/// Name lookup, connecting, sending and each wait for data, in milliseconds.
const TIMEOUT_MS: i32 = 30_000;
const CHUNK: usize = 64 * 1024;

/// A WinHTTP handle, closed when dropped.
struct Handle(*mut core::ffi::c_void);

impl Handle {
    fn new(h: *mut core::ffi::c_void, what: &str) -> Result<Self, String> {
        if h.is_null() { Err(format!("{what}: {}", windows::core::Error::from_thread().message())) } else { Ok(Handle(h)) }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle WinHTTP gave us, closed once.
        let _ = unsafe { WinHttpCloseHandle(self.0) };
    }
}

/// An open response: the request and the handles it depends on, dropped in
/// reverse order (request, connection, session).
struct Response {
    request: Handle,
    _connect: Handle,
    _session: Handle,
    status: u32,
    /// The size the server announced, if it did.
    length: Option<u64>,
}

fn no_connection(e: &windows::core::Error) -> String {
    // 12007 name not resolved, 12029 cannot connect, 12002 timed out.
    match e.code().0 as u32 & 0xFFFF {
        12007 | 12029 => "No internet connection, or Twitter can't be reached.".into(),
        12002 => "Twitter took too long to answer. Try again.".into(),
        _ => format!("The download failed: {}", e.message()),
    }
}

/// Sends `GET https://host/path` and waits for the response headers.
fn get(host: &str, path: &str) -> Result<Response, String> {
    // SAFETY: every handle is owned by a `Handle` and closed on drop; the
    // HSTRINGs outlive the calls that read them.
    unsafe {
        let session = Handle::new(
            WinHttpOpen(&HSTRING::from(AGENT), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0),
            "Windows' internet functions are not available",
        )?;
        let _ = WinHttpSetTimeouts(session.0, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS, TIMEOUT_MS);
        let connect = Handle::new(WinHttpConnect(session.0, &HSTRING::from(host), 443, 0), "Could not connect")?;
        let request = Handle::new(
            WinHttpOpenRequest(connect.0, w!("GET"), &HSTRING::from(path), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), WINHTTP_FLAG_SECURE),
            "Could not start the download",
        )?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(|e| no_connection(&e))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| no_connection(&e))?;
        let status = query_number(&request, WINHTTP_QUERY_STATUS_CODE).unwrap_or(0);
        let length = query_number(&request, WINHTTP_QUERY_CONTENT_LENGTH).map(u64::from);
        Ok(Response { request, _connect: connect, _session: session, status, length })
    }
}

fn query_number(request: &Handle, what: u32) -> Option<u32> {
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: `value` is a u32 buffer of `size` bytes; the index pointer may be null.
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            what | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut value as *mut u32 as *mut _),
            &mut size,
            std::ptr::null_mut(),
        )
    }
    .ok()
    .map(|()| value)
}

impl Response {
    /// The next piece of the body; empty at the end.
    fn read(&self, buf: &mut [u8]) -> Result<usize, String> {
        let mut got = 0u32;
        // SAFETY: `buf` is writable for its length; `got` is a local.
        unsafe { WinHttpReadData(self.request.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut got) }
            .map_err(|e| no_connection(&e))?;
        Ok(got as usize)
    }
}

/// The body of `https://host/path`, at most `limit` bytes. Returns the HTTP status too.
pub fn get_small(host: &str, path: &str, limit: usize, cancel: &AtomicBool) -> Result<(u32, Vec<u8>), String> {
    let r = get(host, path)?;
    let mut body = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        let n = r.read(&mut buf)?;
        if n == 0 {
            return Ok((r.status, body));
        }
        if body.len() + n > limit {
            return Err("Twitter sent an answer PocketMedia can't read.".into());
        }
        body.extend_from_slice(&buf[..n]);
    }
}

pub const STOPPED: &str = "Stopped.";

/// Writes the body of `url` (https only) to `out`. Calls `progress(done, total)`
/// after each piece. Refuses a body larger than `limit` bytes.
pub fn download(
    url: &str,
    out: &mut impl Write,
    limit: u64,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<u64, String> {
    let (host, path) = crate::twitter::split_https(url).ok_or("The file's address is not a secure web address.")?;
    let r = get(host, path)?;
    if r.status != 200 {
        return Err(format!("Twitter did not send the file (error {}).", r.status));
    }
    if r.length.is_some_and(|n| n > limit) {
        return Err(TOO_LARGE.into());
    }
    let mut done = 0u64;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(STOPPED.into());
        }
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > limit {
            return Err(TOO_LARGE.into());
        }
        out.write_all(&buf[..n]).map_err(|e| format!("Could not save the download: {e}"))?;
        progress(done, r.length);
    }
    if r.length.is_some_and(|n| n != done) {
        return Err("The download was cut off. Try again.".into());
    }
    Ok(done)
}

pub const TOO_LARGE: &str = "The file is too large to download (more than 1 GB for a video, 100 MB for a picture).";
