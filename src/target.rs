//! The app that was focused before the popup opened ("the target").

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOTOWNER, GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowTextW,
    GetWindowThreadProcessId, IsWindow,
};
use windows::core::PWSTR;

#[derive(Clone, Debug)]
pub struct Target {
    pub hwnd: HWND,
    pub pid: u32,
    pub title: String,
    pub exe: String,
}

/// Why no usable target was captured.
#[derive(Clone, Debug)]
pub enum NoTarget {
    NothingFocused,
    /// The taskbar / tray had focus (popup opened from the tray menu).
    Shell,
    /// One of our own windows had focus.
    Ourselves,
}

impl NoTarget {
    pub fn describe(&self) -> &'static str {
        match self {
            NoTarget::NothingFocused => "no window was focused",
            NoTarget::Shell => "opened from the tray, so the taskbar had focus",
            NoTarget::Ourselves => "PocketMedia itself had focus",
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
    })
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
