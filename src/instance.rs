//! One PocketMedia per user: the first launch holds a named mutex;
//! a later launch finds the running one's window, tells it to show the quick
//! popup, and exits. A launch from autostart that finds one running just exits.

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, LPARAM, WPARAM};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FindWindowW, GetWindowThreadProcessId, PostMessageW, RegisterWindowMessageW,
};
use windows::core::w;

use crate::winutil::OwnedHandle;

/// Held for the life of the process; the mutex goes when the process ends.
pub struct Instance {
    _mutex: Option<OwnedHandle>,
}

/// Returns None when another PocketMedia is already running.
pub fn claim() -> Option<Instance> {
    // SAFETY: a static name and no attributes; the handle gets one owner.
    let mutex = unsafe { CreateMutexW(None, false, w!("Local\\PocketMedia.SingleInstance")) };
    // SAFETY: read right after CreateMutexW, which set it.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    match mutex {
        Ok(h) if existed => {
            drop(OwnedHandle(h));
            None
        }
        Ok(h) => Some(Instance { _mutex: Some(OwnedHandle(h)) }),
        // Windows would not make the mutex: run rather than refuse to start.
        Err(_) => Some(Instance { _mutex: None }),
    }
}

/// The message a later launch sends the running PocketMedia's main window.
pub fn activate_message() -> u32 {
    // SAFETY: a static name; registering the same name again returns the same id.
    unsafe { RegisterWindowMessageW(w!("PocketMedia.Activate")) }
}

/// Asks the running PocketMedia to show its popup. It may still be starting, so
/// its window is looked for for up to 5 seconds. Returns false if not found.
pub fn activate_running(main_class: windows::core::PCWSTR) -> bool {
    let msg = activate_message();
    for _ in 0..50 {
        // SAFETY: a static class name; no window title.
        if let Ok(h) = unsafe { FindWindowW(main_class, None) } {
            let mut pid = 0u32;
            // SAFETY: `h` was just found; a stale one only makes these calls fail.
            unsafe {
                GetWindowThreadProcessId(h, Some(&mut pid));
                // Lets the running copy bring its popup to the front.
                let _ = AllowSetForegroundWindow(pid);
                return PostMessageW(Some(h), msg, WPARAM(0), LPARAM(0)).is_ok();
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}
