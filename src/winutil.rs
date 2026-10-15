//! Small safe wrappers over Win32 calls that every PocketMedia window uses.

use windows::Win32::Foundation::{HGLOBAL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWMWINDOWATTRIBUTE,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, GetUpdateRect, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    ScreenToClient,
};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TME_HOVER, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VIRTUAL_KEY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetCursorPos, GetSystemMetrics, HICON, IDC_ARROW, IDC_IBEAM, IDI_APPLICATION, IMAGE_ICON, LR_SHARED,
    LoadCursorW, LoadIconW, LoadImageW, SM_CXICON, SM_CXSMICON, SetCursor,
};
use windows::core::PCWSTR;

use crate::ui::layout::Rect;

/// Defined in Win32_UI_Controls; declared here rather than enabling that whole feature.
pub const WM_MOUSELEAVE: u32 = 0x02A3;
/// Also from Win32_UI_Controls: the pointer rested in the window (TrackMouseEvent TME_HOVER).
pub const WM_MOUSEHOVER: u32 = 0x02A1;

/// The PocketMedia logo from the exe's resources (icon group 1, linked by
/// build.rs), at the system's large or small icon size. Windows' generic program
/// icon when the exe was built without it. Shared icons belong to the system,
/// so nobody destroys them.
pub fn app_icon(small: bool) -> HICON {
    // SAFETY: plain metric and resource lookups; MAKEINTRESOURCE(1) is a valid
    // resource-id pointer value, and LR_SHARED handles are never freed by us.
    unsafe {
        let size = GetSystemMetrics(if small { SM_CXSMICON } else { SM_CXICON });
        let module = GetModuleHandleW(None).ok();
        let id = PCWSTR(std::ptr::without_provenance(1)); // MAKEINTRESOURCE(1)
        match LoadImageW(module.map(Into::into), id, IMAGE_ICON, size, size, LR_SHARED) {
            Ok(h) if !h.is_invalid() => HICON(h.0),
            _ => LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
        }
    }
}

/// Rounded Windows 11 corners and no system border (we draw our own).
pub fn style_frame(hwnd: HWND) {
    set_dwm_attribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND.0 as u32);
    set_dwm_attribute(hwnd, DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE);
}

fn set_dwm_attribute(hwnd: HWND, attr: DWMWINDOWATTRIBUTE, value: u32) {
    // SAFETY: value is a 4-byte local, as both attributes expect. Failure (older
    // Windows) only loses the cosmetic effect.
    let _ = unsafe { DwmSetWindowAttribute(hwnd, attr, &value as *const u32 as *const _, 4) };
}

pub fn invalidate(hwnd: HWND) {
    if !hwnd.is_invalid() {
        // SAFETY: an asynchronous repaint request; a stale HWND only makes it fail.
        let _ = unsafe { InvalidateRect(Some(hwnd), None, false) };
    }
}

/// Asks for a repaint of `r` (DIPs) only, rounded out to whole pixels.
pub fn invalidate_rect(hwnd: HWND, r: Rect, dpi: u32) {
    if hwnd.is_invalid() {
        return;
    }
    let s = dpi as f32 / 96.0;
    let rc = RECT {
        left: (r.x * s).floor() as i32 - 1,
        top: (r.y * s).floor() as i32 - 1,
        right: (r.right() * s).ceil() as i32 + 1,
        bottom: (r.bottom() * s).ceil() as i32 + 1,
    };
    // SAFETY: an asynchronous repaint request for a local RECT; a stale HWND only makes it fail.
    let _ = unsafe { InvalidateRect(Some(hwnd), Some(&rc), false) };
}

/// The part of the window waiting to be painted, in DIPs, or None when that is
/// the whole client area. Call it in WM_PAINT before validating.
pub fn update_rect(hwnd: HWND, dpi: u32) -> Option<Rect> {
    let (mut rc, mut client) = (RECT::default(), RECT::default());
    // SAFETY: our own window; out-parameters are locals.
    let ok = unsafe { GetUpdateRect(hwnd, Some(&mut rc), false).as_bool() && GetClientRect(hwnd, &mut client).is_ok() };
    let whole = rc.left <= client.left && rc.top <= client.top && rc.right >= client.right && rc.bottom >= client.bottom;
    if !ok || whole {
        return None;
    }
    let s = 96.0 / dpi.max(1) as f32;
    let (x, y) = (rc.left as f32 * s, rc.top as f32 * s);
    Some(Rect::new(x, y, rc.right as f32 * s - x, rc.bottom as f32 * s - y))
}

pub fn set_capture(hwnd: HWND) {
    // SAFETY: our own window.
    unsafe { SetCapture(hwnd) };
}

pub fn release_capture() {
    // SAFETY: no preconditions; harmless if we do not hold capture.
    let _ = unsafe { ReleaseCapture() };
}

pub fn key_down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: no preconditions.
    (unsafe { GetKeyState(vk.0 as i32) }) < 0
}

/// Asks for one WM_MOUSELEAVE when the pointer leaves `hwnd`. Returns true if armed.
pub fn track_mouse_leave(hwnd: HWND) -> bool {
    let mut tme = TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
    // SAFETY: tme is a fully initialised local.
    unsafe { TrackMouseEvent(&mut tme) }.is_ok()
}

/// Asks for one WM_MOUSEHOVER when the pointer rests in `hwnd` for the system's
/// hover time (and keeps the WM_MOUSELEAVE request). Windows times it; nothing
/// here polls. Returns true if armed.
pub fn track_mouse_hover(hwnd: HWND) -> bool {
    const HOVER_DEFAULT: u32 = 0xFFFF_FFFF;
    let mut tme =
        TRACKMOUSEEVENT { cbSize: size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_HOVER | TME_LEAVE, hwndTrack: hwnd, dwHoverTime: HOVER_DEFAULT };
    // SAFETY: tme is a fully initialised local.
    unsafe { TrackMouseEvent(&mut tme) }.is_ok()
}

pub fn set_cursor(ibeam: bool) {
    // SAFETY: system cursors are shared resources; we never free them.
    unsafe {
        if let Ok(c) = LoadCursorW(None, if ibeam { IDC_IBEAM } else { IDC_ARROW }) {
            SetCursor(Some(c));
        }
    }
}

/// Client-area point packed in a mouse message's LPARAM, in device pixels.
pub fn lparam_point(lparam: LPARAM) -> POINT {
    POINT { x: (lparam.0 & 0xFFFF) as i16 as i32, y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 }
}

pub fn to_dip(pt: POINT, dpi: u32) -> (f32, f32) {
    let s = 96.0 / dpi.max(1) as f32;
    (pt.x as f32 * s, pt.y as f32 * s)
}

pub fn client_to_screen(hwnd: HWND, mut pt: POINT) -> POINT {
    // SAFETY: our own window; pt is a local.
    let _ = unsafe { ClientToScreen(hwnd, &mut pt) };
    pt
}

/// Screen-space centre of a DIP rectangle in `hwnd`'s client area.
pub fn rect_center_on_screen(hwnd: HWND, dpi: u32, r: Rect) -> POINT {
    let (cx, cy) = r.center();
    let s = dpi as f32 / 96.0;
    client_to_screen(hwnd, POINT { x: (cx * s).round() as i32, y: (cy * s).round() as i32 })
}

/// The pointer position in `hwnd`'s client area, in DIPs.
pub fn cursor_dip(hwnd: HWND, dpi: u32) -> Option<(f32, f32)> {
    let mut pt = POINT::default();
    // SAFETY: out-pointer to a local; our own window.
    unsafe {
        GetCursorPos(&mut pt).ok()?;
        let _ = ScreenToClient(hwnd, &mut pt);
    }
    Some(to_dip(pt, dpi))
}

/// Client size in DIPs.
pub fn client_size_dip(hwnd: HWND, dpi: u32) -> (f32, f32) {
    let mut rc = RECT::default();
    // SAFETY: our window; rc is a local.
    let _ = unsafe { GetClientRect(hwnd, &mut rc) };
    let s = 96.0 / dpi.max(1) as f32;
    ((rc.right - rc.left) as f32 * s, (rc.bottom - rc.top) as f32 * s)
}

/// The pointer, and the work area and DPI of the monitor it is on.
pub struct Placement {
    pub cursor: POINT,
    pub work: RECT,
    pub dpi: u32,
}

pub fn monitor_at_cursor() -> Placement {
    let mut cursor = POINT::default();
    // SAFETY: out-pointer to a local.
    let _ = unsafe { GetCursorPos(&mut cursor) };
    monitor_at(cursor)
}

/// `point`, and the work area and DPI of the monitor it is on (the nearest one).
pub fn monitor_at(point: POINT) -> Placement {
    let cursor = point;
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let (mut dpi, mut dpi_y) = (96u32, 96u32);
    // SAFETY: out-pointers to locals.
    unsafe {
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let _ = GetMonitorInfoW(monitor, &mut info);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
    }
    Placement { cursor, work: info.rcWork, dpi }
}

/// How many UTF-16 units of a clipboard block of `bytes` may be read: whole
/// units inside the block, and at most 4096.
fn text_limit(bytes: usize) -> usize {
    (bytes / 2).min(4096)
}

/// The clipboard, opened by this thread, with one owner: dropping it closes it.
pub struct OpenClipboard(());

impl OpenClipboard {
    /// Another app can hold the clipboard open for a moment; tries `attempts`
    /// times, 15 ms apart.
    pub fn open(owner: HWND, attempts: u32) -> Result<Self, String> {
        for attempt in 0..attempts {
            // SAFETY: `owner` is our own window; closed by Drop only after success.
            if unsafe { windows::Win32::System::DataExchange::OpenClipboard(Some(owner)) }.is_ok() {
                return Ok(Self(()));
            }
            if attempt + 1 < attempts {
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
        }
        Err("The clipboard is in use by another app. Try again.".into())
    }
}

impl Drop for OpenClipboard {
    fn drop(&mut self) {
        // SAFETY: this value exists only after a successful open, so it is closed once.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Plain text from the clipboard (for Ctrl+V in text boxes), capped at 4096 characters.
pub fn clipboard_text(owner: HWND) -> Option<String> {
    let _open = OpenClipboard::open(owner, 1).ok()?;
    // SAFETY: the clipboard is open (above); the handle is checked by Windows.
    let mem = HGLOBAL(unsafe { GetClipboardData(CF_UNICODETEXT.0 as u32) }.ok()?.0);
    // SAFETY: `mem` is the clipboard's text block; it stays valid while the clipboard is open.
    let ptr = unsafe { GlobalLock(mem) } as *const u16;
    if ptr.is_null() {
        return None;
    }
    // Never past the block, even when the other app left out the NUL.
    // SAFETY: as above.
    let max = text_limit(unsafe { GlobalSize(mem) });
    // SAFETY: `ptr` points at a locked block of at least `max` units (text_limit).
    let units = unsafe { std::slice::from_raw_parts(ptr, max) };
    let len = units.iter().position(|&u| u == 0).unwrap_or(max);
    let text = String::from_utf16_lossy(&units[..len]);
    // SAFETY: unlocks the lock taken above; `units` is not used after this.
    let _ = unsafe { GlobalUnlock(mem) };
    Some(text)
}

/// Opens a folder in File Explorer.
pub fn open_folder(dir: &std::path::Path) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, w};
    let file = HSTRING::from(dir.as_os_str());
    // SAFETY: the HSTRINGs outlive the call; no owner window or parameters.
    let result = unsafe { ShellExecuteW(None, w!("open"), &file, None, None, SW_SHOWNORMAL) };
    // ShellExecute returns a value above 32 on success.
    if result.0 as usize > 32 { Ok(()) } else { Err(format!("Windows returned error {}", result.0 as usize)) }
}

/// A kernel handle (event, mutex, directory) with one owner, closed on drop.
pub struct OwnedHandle(pub windows::Win32::Foundation::HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: this struct is the handle's only owner, so it is closed exactly once.
            let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
        }
    }
}

// SAFETY: a kernel handle is a process-wide value; any thread may wait on or close it.
unsafe impl Send for OwnedHandle {}
// SAFETY: as above; the methods used through `&OwnedHandle` (wait, SetEvent) are thread-safe.
unsafe impl Sync for OwnedHandle {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scan for the end of the text stays inside the block.
    #[test]
    fn clipboard_text_stays_inside_its_block() {
        assert_eq!(text_limit(0), 0);
        assert_eq!(text_limit(1), 0, "half a unit is not read");
        assert_eq!(text_limit(7), 3);
        assert_eq!(text_limit(10), 5);
        assert_eq!(text_limit(1 << 20), 4096);
    }
}
