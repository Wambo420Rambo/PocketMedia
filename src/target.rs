//! The app that was focused before the popup opened ("the target").

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, POINT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOTOWNER, GUITHREADINFO, GetAncestor, GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetWindowTextW,
    GetWindowThreadProcessId, IsWindow,
};
use windows::core::PWSTR;

use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Target {
    pub hwnd: HWND,
    pub pid: u32,
    pub title: String,
    pub exe: String,
    /// Where the app's text caret was (screen pixels, its bottom-left corner),
    /// when the app shows a standard caret (the popup opens near it).
    pub caret: Option<POINT>,
    /// When it was captured.
    pub captured: Instant,
}

/// How long the Manager's Quick popup still pastes into the app remembered from
/// the last time the popup opened. After that it only copies: the
/// user may have moved on to another chat, and input must not land where they
/// no longer expect it.
pub const FRESH_FOR: Duration = Duration::from_secs(120);

/// The remembered target, if it was captured less than `FRESH_FOR` before `now`.
pub fn fresh(t: Result<Target, NoTarget>, now: Instant) -> Result<Target, NoTarget> {
    match t {
        Ok(t) if now.saturating_duration_since(t.captured) >= FRESH_FOR => Err(NoTarget::Expired),
        other => other,
    }
}

/// Why no usable target was captured.
#[derive(Clone, Debug)]
pub enum NoTarget {
    NothingFocused,
    /// The taskbar / tray had focus (popup opened from the tray menu).
    Shell,
    /// One of our own windows had focus.
    Ourselves,
    /// The app was remembered too long ago (see `FRESH_FOR`).
    Expired,
}

impl NoTarget {
    pub fn describe(&self) -> &'static str {
        match self {
            NoTarget::NothingFocused => "no window was focused",
            NoTarget::Shell => "opened from the tray, so the taskbar had focus",
            NoTarget::Ourselves => "PocketMedia itself had focus",
            NoTarget::Expired => "the app from before was remembered more than 2 minutes ago",
        }
    }
}

/// Reads the foreground window right now. Call this *before* our popup takes focus.
pub fn capture_foreground() -> Result<Target, NoTarget> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_invalid() {
        return Err(NoTarget::NothingFocused);
    }
    let pid = window_pid(hwnd);
    if pid == unsafe { GetCurrentProcessId() } {
        return Err(NoTarget::Ourselves);
    }
    let class = class_name(hwnd);
    if is_shell_surface(&class) {
        return Err(NoTarget::Shell);
    }
    Ok(Target {
        hwnd,
        pid,
        title: window_title(hwnd),
        exe: process_exe_name(pid).unwrap_or_else(|| "unknown process".into()),
        caret: caret_of(hwnd),
        captured: Instant::now(),
    })
}

/// The text caret of the thread that owns `hwnd`, as a screen point at its
/// bottom-left corner. None when that thread shows no standard caret (many apps
/// draw their own, for example browsers); the popup then opens at the pointer.
pub fn caret_of(hwnd: HWND) -> Option<POINT> {
    // SAFETY: a stale HWND only yields thread 0, which fails below.
    let thread = unsafe { GetWindowThreadProcessId(hwnd, None) };
    if thread == 0 {
        return None;
    }
    let mut info = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
    // SAFETY: `info` is a local with cbSize set, as the call requires.
    unsafe { GetGUIThreadInfo(thread, &mut info) }.ok()?;
    let r = info.rcCaret;
    if info.hwndCaret.is_invalid() || r.bottom <= r.top {
        return None;
    }
    let mut pt = POINT { x: r.left, y: r.bottom };
    // SAFETY: hwndCaret came from Windows just now; pt is a local.
    unsafe { ClientToScreen(info.hwndCaret, &mut pt) }.as_bool().then_some(pt)
}

impl Target {
    pub fn label(&self) -> String {
        let title = if self.title.is_empty() { "(untitled window)" } else { &self.title };
        format!("{title}  [{}]", self.exe)
    }

    pub fn still_exists(&self) -> bool {
        let alive = unsafe { IsWindow(Some(self.hwnd)) }.as_bool();
        alive && window_pid(self.hwnd) == self.pid
    }

    /// True when `hwnd` is the target or a window owned by it (e.g. its dialog).
    pub fn owns_foreground(&self) -> bool {
        let fg = unsafe { GetForegroundWindow() };
        if fg.is_invalid() {
            return false;
        }
        if fg == self.hwnd {
            return true;
        }
        let root = |h: HWND| unsafe { GetAncestor(h, GA_ROOTOWNER) };
        root(fg) == root(self.hwnd)
    }
}

/// Elevation state of a process, as far as a standard user can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Elevation {
    Elevated,
    NotElevated,
    Unknown,
}

pub fn process_elevation(pid: Option<u32>) -> Elevation {
    unsafe {
        let process = match pid {
            None => GetCurrentProcess(),
            Some(pid) => match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(h) => h,
                Err(_) => return Elevation::Unknown,
            },
        };
        let result = token_elevation(process);
        if pid.is_some() {
            let _ = CloseHandle(process);
        }
        result
    }
}

unsafe fn token_elevation(process: HANDLE) -> Elevation {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token).is_err() {
            return Elevation::Unknown;
        }
        let mut info = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut info as *mut _ as *mut _),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        );
        let _ = CloseHandle(token);
        match ok {
            Ok(()) if info.TokenIsElevated != 0 => Elevation::Elevated,
            Ok(()) => Elevation::NotElevated,
            Err(_) => Elevation::Unknown,
        }
    }
}

fn is_shell_surface(class: &str) -> bool {
    matches!(
        class,
        "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "NotifyIconOverflowWindow"
            | "TopLevelWindowForOverflowXamlIsland"
    )
}

fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn process_exe_name(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        ok.ok()?;
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        Some(full.rsplit('\\').next().unwrap_or(&full).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured(ago: Duration, now: Instant) -> Result<Target, NoTarget> {
        Ok(Target { hwnd: HWND::default(), pid: 1, title: String::new(), exe: "chat.exe".into(), caret: None, captured: now - ago })
    }

    /// The Manager's Quick popup pastes only into an app remembered
    /// a short while ago; after that it copies.
    #[test]
    fn a_remembered_app_expires() {
        // Ahead of the clock, so `now - ago` never goes before the clock's start.
        let now = Instant::now() + Duration::from_secs(7200);
        assert!(fresh(captured(Duration::from_secs(5), now), now).is_ok());
        assert!(fresh(captured(FRESH_FOR - Duration::from_secs(1), now), now).is_ok());
        assert!(matches!(fresh(captured(FRESH_FOR, now), now), Err(NoTarget::Expired)));
        assert!(matches!(fresh(captured(Duration::from_secs(3600), now), now), Err(NoTarget::Expired)));
        assert!(matches!(fresh(Err(NoTarget::Shell), now), Err(NoTarget::Shell)), "no app stays no app");
        assert_eq!(FRESH_FOR.as_secs(), 120, "the message says 2 minutes");
    }
}
