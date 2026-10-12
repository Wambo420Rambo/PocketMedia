//! Small safe wrappers over Win32 calls that every PocketMedia window uses.

use windows::Win32::Foundation::{HGLOBAL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_COLOR_NONE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DWMWINDOWATTRIBUTE,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    ScreenToClient,
};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetCursorPos, IDC_ARROW, IDC_IBEAM, LoadCursorW, SetCursor};

use crate::ui::layout::Rect;

/// Defined in Win32_UI_Controls; declared here rather than enabling that whole feature.
pub const WM_MOUSELEAVE: u32 = 0x02A3;

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
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    let (mut dpi, mut dpi_y) = (96u32, 96u32);
    // SAFETY: out-pointers to locals.
    unsafe {
        let _ = GetCursorPos(&mut cursor);
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let _ = GetMonitorInfoW(monitor, &mut info);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut dpi_y);
    }
    Placement { cursor, work: info.rcWork, dpi }
}

/// Plain text from the clipboard (for Ctrl+V in text boxes), capped at 4096 characters.
pub fn clipboard_text(owner: HWND) -> Option<String> {
    // SAFETY: the clipboard is opened and closed in this block; the memory is
    // read only while locked and copied out before unlocking.
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let text = GetClipboardData(CF_UNICODETEXT.0 as u32).ok().and_then(|h| {
            let mem = HGLOBAL(h.0);
            let ptr = GlobalLock(mem) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0;
            while *ptr.add(len) != 0 && len < 4096 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            let _ = GlobalUnlock(mem);
            Some(s)
        });
        let _ = CloseClipboard();
        text
    }
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
