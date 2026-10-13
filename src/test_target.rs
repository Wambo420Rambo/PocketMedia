//! `--test-target <dir>`: a plain window the self-test pastes and drops into.
//! It runs in its own process so the test crosses a process boundary, like a
//! real target app. It records exactly what it received into `<dir>`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::Win32::Foundation::{HGLOBAL, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, COLOR_WINDOW, DT_CENTER, DT_NOPREFIX, DT_WORDBREAK, DrawTextW, EndPaint, FillRect,
    GetSysColorBrush, InvalidateRect, PAINTSTRUCT,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW,
    OpenClipboard, RegisterClipboardFormatW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_V};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

pub const CLASS: &str = "PocketMediaTestTarget";

static OUT: OnceLock<PathBuf> = OnceLock::new();
thread_local! {
    static LAST: std::cell::RefCell<String> = std::cell::RefCell::new("Waiting for a paste or a drop.".into());
}

/// Runs the target window until it is closed. Returns the process exit code.
pub fn run(out: &Path) -> i32 {
    if std::fs::create_dir_all(out).is_err() {
        return 2;
    }
    let _ = OUT.set(out.to_path_buf());
    match create_window() {
        Ok(()) => {
            pump();
            0
        }
        Err(_) => 3,
    }
}

fn create_window() -> windows::core::Result<()> {
    // SAFETY: plain Win32 window setup on this thread; all pointers are to locals or statics.
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(proc_),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            lpszClassName: w!("PocketMediaTestTarget"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let scale = |v: i32| v * GetDpiForSystem() as i32 / 96;
        let mut work = RECT::default();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))?;
        let (w, h) = (scale(420), scale(320));
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("PocketMediaTestTarget"),
            w!("PocketMedia test target"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            work.right - w - scale(40),
            work.top + scale(80),
            w,
            h,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        DragAcceptFiles(hwnd, true);
        let _ = SetForegroundWindow(hwnd);
    }
    Ok(())
}

fn pump() {
    let mut msg = MSG::default();
    // SAFETY: standard message loop on the thread that owns the window.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn set_last(text: String, hwnd: HWND) {
    LAST.with(|l| *l.borrow_mut() = text);
    // SAFETY: hwnd is our own live window.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, true);
    }
}

fn write_result(name: &str, bytes: &[u8]) {
    let Some(out) = OUT.get() else { return };
    let tmp = out.join(format!("{name}.tmp"));
    // Written via a temporary file so the self-test never reads half a file.
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, out.join(name));
    }
}

/// Closes the clipboard when dropped.
struct ClipboardOpen;
impl ClipboardOpen {
    fn open(owner: HWND) -> Option<Self> {
        for _ in 0..10 {
            // SAFETY: owner is our live window; closed again in Drop.
            if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
                return Some(Self);
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        None
    }
}
impl Drop for ClipboardOpen {
    fn drop(&mut self) {
        // SAFETY: only constructed after a successful OpenClipboard.
        let _ = unsafe { CloseClipboard() };
    }
}

fn format_name(format: u32) -> String {
    match format {
        2 => "CF_BITMAP".into(),
        8 => "CF_DIB".into(),
        15 => "CF_HDROP".into(),
        17 => "CF_DIBV5".into(),
        _ => {
            let mut buf = [0u16; 128];
            // SAFETY: buf is a valid writable buffer of the given length.
            let n = unsafe { GetClipboardFormatNameW(format, &mut buf) };
            if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { format!("#{format}") }
        }
    }
}

/// Copies one clipboard format's global memory into a Vec.
fn clipboard_bytes(format: u32) -> Option<Vec<u8>> {
    // SAFETY: the clipboard is open (caller holds ClipboardOpen); the handle stays
    // valid until the clipboard closes, and we copy out while it is locked.
    unsafe {
        let handle = GetClipboardData(format).ok()?;
        let mem = HGLOBAL(handle.0);
        let size = GlobalSize(mem);
        let ptr = GlobalLock(mem) as *const u8;
        if ptr.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(ptr, size).to_vec();
        let _ = GlobalUnlock(mem);
        Some(bytes)
    }
}

/// The first path in CF_HDROP (the clipboard must be open).
fn clipboard_hdrop() -> Option<String> {
    let bytes = clipboard_bytes(15)?;
    // DROPFILES: pFiles (offset of the list) is the first u32; fWide at byte 16.
    let offset = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
    let wide: Vec<u16> = bytes.get(offset..)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|u| *u != 0).collect();
    Some(String::from_utf16_lossy(&wide))
}

fn on_paste(hwnd: HWND) {
    let Some(_open) = ClipboardOpen::open(hwnd) else {
        set_last("Ctrl+V received, but the clipboard was busy.".into(), hwnd);
        return;
    };
    let mut names = Vec::new();
    let mut format = 0u32;
    loop {
        // SAFETY: the clipboard is open; EnumClipboardFormats only reads.
        format = unsafe { EnumClipboardFormats(format) };
        if format == 0 {
            break;
        }
        names.push(format_name(format));
    }
    // The file offered with the paste (CF_HDROP), and the bitmap's size (CF_DIB).
    if let Some(path) = clipboard_hdrop() {
        write_result("paste-hdrop.txt", path.as_bytes());
    }
    if let Some(dib) = clipboard_bytes(8) {
        let w = dib.get(4..8).map(|b| i32::from_le_bytes(b.try_into().unwrap_or_default())).unwrap_or(0);
        let h = dib.get(8..12).map(|b| i32::from_le_bytes(b.try_into().unwrap_or_default())).unwrap_or(0);
        write_result("paste-dib.txt", format!("{w} {h}").as_bytes());
    }
    // SAFETY: registering a format name has no preconditions.
    let png = unsafe { RegisterClipboardFormatW(w!("PNG")) };
    match clipboard_bytes(png) {
        Some(bytes) => {
            write_result("paste-received.png", &bytes);
            set_last(format!("Ctrl+V: received a {} byte PNG.\nFormats: {}", bytes.len(), names.join(", ")), hwnd);
        }
        None => set_last(format!("Ctrl+V: no PNG on the clipboard.\nFormats: {}", names.join(", ")), hwnd),
    }
    // Written last: the self-test waits for this file, then reads the others.
    write_result("paste-formats.txt", names.join("\n").as_bytes());
}

fn on_drop(hwnd: HWND, drop: HDROP) {
    let mut buf = [0u16; 1024];
    // SAFETY: drop is the handle Windows passed with WM_DROPFILES; buf is writable.
    let count = unsafe { DragQueryFileW(drop, u32::MAX, None) };
    let len = unsafe { DragQueryFileW(drop, 0, Some(&mut buf)) } as usize;
    // SAFETY: we are done with the handle; DragFinish releases it exactly once.
    unsafe { DragFinish(drop) };
    let source = String::from_utf16_lossy(&buf[..len]);
    write_result("drop-source.txt", source.as_bytes());
    match std::fs::read(&source) {
        Ok(bytes) => {
            write_result("drop-received.png", &bytes);
            set_last(format!("Drop: {count} file(s), first is\n{source}"), hwnd);
        }
        Err(e) => set_last(format!("Drop received but the file could not be read: {e}"), hwnd),
    }
}

fn paint(hwnd: HWND) {
    let text = LAST.with(|l| l.borrow().clone());
    let mut wide: Vec<u16> = format!("PocketMedia test target\n\n{text}").encode_utf16().collect();
    // SAFETY: paint cycle on our own window; BeginPaint/EndPaint are paired.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut r = RECT::default();
        let _ = GetClientRect(hwnd, &mut r);
        FillRect(hdc, &r, GetSysColorBrush(COLOR_WINDOW));
        r.left += 16;
        r.top += 16;
        r.right -= 16;
        DrawTextW(hdc, &mut wide, &mut r, DT_CENTER | DT_WORDBREAK | DT_NOPREFIX);
        let _ = EndPaint(hwnd, &ps);
    }
}

extern "system" fn proc_(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_KEYDOWN if wparam.0 == VK_V.0 as usize => {
            // SAFETY: GetKeyState has no preconditions.
            if unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 {
                on_paste(hwnd);
            }
            LRESULT(0)
        }
        WM_DROPFILES => {
            on_drop(hwnd, HDROP(wparam.0 as *mut _));
            LRESULT(0)
        }
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: no preconditions.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: forwarding the unchanged message to the default handler.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
