//! File operations that need Windows itself: a move that never replaces a file,
//! the Recycle Bin, and showing a file in File Explorer.

use std::io;
use std::path::Path;

use windows::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
use windows::Win32::UI::Shell::{
    FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, SHFILEOPSTRUCTW, SHFileOperationW,
    ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

/// Moves (renames) `from` to `to` on the same volume. Fails with
/// `AlreadyExists` if `to` exists: nothing is ever replaced. A change of letter
/// case only (`a.png` to `A.png`) is allowed, as in Explorer.
pub fn move_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    let (f, t) = (HSTRING::from(from.as_os_str()), HSTRING::from(to.as_os_str()));
    // SAFETY: both HSTRINGs are valid null-terminated paths for the duration of the call.
    // Without MOVEFILE_REPLACE_EXISTING the move fails if `to` exists.
    unsafe { MoveFileExW(&f, &t, MOVEFILE_WRITE_THROUGH) }.map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
}

/// Moves a file to the Recycle Bin (never a permanent delete). Checks that the
/// file is really gone before reporting success.
pub fn recycle(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err("the file is not in the Media folder".into());
    }
    // SHFileOperation takes a list of paths ending in an extra null.
    let mut from: Vec<u16> = path.as_os_str().encode_wide_lossy();
    from.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    // SAFETY: op is fully initialised; `from` is double-null terminated and outlives the call.
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 {
        return Err(format!("Windows reported error {code:#x}"));
    }
    if op.fAnyOperationsAborted.as_bool() {
        return Err("the move was cancelled".into());
    }
    if path.exists() {
        return Err("the file is still there".into());
    }
    Ok(())
}

/// Opens File Explorer with `path` selected.
pub fn show_in_explorer(path: &Path) -> Result<(), String> {
    let args = HSTRING::from(format!("/select,\"{}\"", path.display()));
    // SAFETY: static and local strings outlive the call; no owner window.
    let result = unsafe { ShellExecuteW(None, w!("open"), w!("explorer.exe"), &args, None, SW_SHOWNORMAL) };
    if result.0 as usize > 32 { Ok(()) } else { Err(format!("Windows returned error {}", result.0 as usize)) }
}

trait EncodeWide {
    fn encode_wide_lossy(&self) -> Vec<u16>;
}

impl EncodeWide for std::ffi::OsStr {
    fn encode_wide_lossy(&self) -> Vec<u16> {
        HSTRING::from(self).to_vec()
    }
}
