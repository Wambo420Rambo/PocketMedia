//! One PocketMedia at a time, and every message goes to the right copy.
//!
//! - Each install has its own identity: a hash of its canonical folder is part
//!   of its mutex name (`Local\PocketMedia.<hash>`) and of its main window class
//!   (`PocketMedia.Main.<hash>`). A later launch of the **same** install finds
//!   that window, tells it what to do (show the quick popup, or with `--backup`
//!   open Back up library) and exits. It can never reach another install.
//! - Every copy also holds the generic `Local\PocketMedia.SingleInstance`, which
//!   Setup and the uninstaller wait on (Inno's `AppMutex` cannot name a folder
//!   it does not know yet). A launch of a **different** install that finds it
//!   held hands nothing over: it says which copy is running and does not start
//!   (a launch from autostart just exits).
//! - `--which-running <file>` answers the uninstaller's question "is this copy
//!   running, another one, or none?" without starting anything.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{
    CreateMutexW, OpenMutexW, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    SYNCHRONIZATION_SYNCHRONIZE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, EnumWindows, FindWindowW, GetClassNameW, GetWindowThreadProcessId, PostMessageW,
    RegisterWindowMessageW,
};
use windows::core::{BOOL, HSTRING, PCWSTR, PWSTR, w};

use crate::winutil::OwnedHandle;

/// Held by every running copy; Setup's and the uninstaller's `AppMutex`.
const GENERIC_MUTEX: &str = r"Local\PocketMedia.SingleInstance";
const CLASS_PREFIX: &str = "PocketMedia.Main.";
/// The main window class of earlier test builds, so their folder can be named too.
const OLD_CLASS: &str = "PocketMediaPhase0Main";

/// FNV-1a (64-bit) of the text, as 16 hex digits. An identity, not a secret.
pub fn hash_text(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for unit in text.encode_utf16() {
        for byte in unit.to_le_bytes() {
            h ^= u64::from(byte);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// This install's id: from its folder, resolved (links, `..`, short names) and
/// with letter case ignored, as Windows does.
pub fn install_id(folder: &Path) -> String {
    let real = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
    hash_text(&crate::import::display_path(&real).to_lowercase())
}

/// The folder that holds this program.
pub fn install_folder() -> PathBuf {
    std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)).unwrap_or_default()
}

fn id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| install_id(&install_folder()))
}

fn mutex_name() -> String {
    format!(r"Local\PocketMedia.{}", id())
}

/// The main window class of this install, null-terminated UTF-16 for Win32.
pub fn main_class() -> PCWSTR {
    static CLASS: OnceLock<Vec<u16>> = OnceLock::new();
    let wide = CLASS.get_or_init(|| format!("{CLASS_PREFIX}{}", id()).encode_utf16().chain(Some(0)).collect());
    // The Vec lives in a static and is never changed, so the pointer stays valid.
    PCWSTR(wide.as_ptr())
}

/// Held for the life of the process; the mutexes go when the process ends.
pub struct Instance {
    _mutexes: Vec<OwnedHandle>,
}

/// What a launch found.
pub enum Claim {
    /// No other copy runs: this one runs.
    Mine(Instance),
    /// This same install is already running.
    SameInstall,
    /// A different install is running, from this folder (when it could be found).
    OtherInstall(Option<String>),
}

/// Creates the named mutex. Ok(true) when it already existed (held by another process).
fn create(name: &str) -> Result<(OwnedHandle, bool), ()> {
    let name = HSTRING::from(name);
    // SAFETY: a valid name and no attributes; the handle gets one owner.
    let mutex = unsafe { CreateMutexW(None, false, &name) };
    // SAFETY: read right after CreateMutexW, which set it.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    mutex.map(|h| (OwnedHandle(h), existed)).map_err(|_| ())
}

/// True when a process holds the named mutex (it exists).
fn exists(name: &str) -> bool {
    let name = HSTRING::from(name);
    // SAFETY: a valid name; the handle, if any, is closed right away.
    match unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, &name) } {
        Ok(h) => {
            // SAFETY: just opened, closed once.
            let _ = unsafe { CloseHandle(h) };
            true
        }
        Err(_) => false,
    }
}

pub fn claim() -> Claim {
    let mut held = Vec::new();
    match create(&mutex_name()) {
        Ok((_, true)) => return Claim::SameInstall,
        Ok((h, false)) => held.push(h),
        // Windows would not make the mutex: run rather than refuse to start.
        Err(()) => return Claim::Mine(Instance { _mutexes: held }),
    }
    match create(GENERIC_MUTEX) {
        // Another install holds it (or is starting): hand nothing over.
        Ok((_, true)) => {
            drop(held);
            Claim::OtherInstall(other_folder(5000))
        }
        Ok((h, false)) => {
            held.push(h);
            Claim::Mine(Instance { _mutexes: held })
        }
        Err(()) => Claim::Mine(Instance { _mutexes: held }),
    }
}

/// `--which-running <file>`: writes `none`, `this`, or `other<TAB><folder>`
/// (UTF-8 with a BOM, which the uninstaller's LoadStringsFromFile reads as
/// UTF-8) and returns 0, 1 or 2. Starts nothing and creates nothing else.
pub fn report_running(out: &Path) -> i32 {
    let (code, text) = if exists(&mutex_name()) {
        (1, "this".to_string())
    } else if exists(GENERIC_MUTEX) {
        (2, format!("other\t{}", other_folder(2000).unwrap_or_default()))
    } else {
        (0, "none".to_string())
    };
    if std::fs::write(out, format!("\u{feff}{text}")).is_ok() { code } else { 3 }
}

/// The message a launch of another install shows instead of starting.
pub fn other_copy_message(other: Option<&str>, backup: bool) -> String {
    let here = crate::import::display_path(&install_folder());
    let running = match other {
        Some(f) if !f.is_empty() => format!("Another PocketMedia is already running, from:\n\n{f}"),
        _ => "Another PocketMedia is already running.".to_string(),
    };
    let nothing = if backup { "\n\nNothing was backed up: that copy has its own library, and it was not asked to do anything." } else { "" };
    format!(
        "{running}\n\nOnly one PocketMedia can run at a time, so this one (in {here}) did not start.{nothing}\n\n\
         To use this one, exit the other: right-click its tray icon and choose Exit. Then start this one again."
    )
}

pub const OTHER_COPY_TITLE: &str = "PocketMedia - another copy is running";

/// The folder of the other install that is running, found from its main window.
/// It may still be starting, so this waits up to `ms` for the window.
fn other_folder(ms: u64) -> Option<String> {
    let mine = format!("{CLASS_PREFIX}{}", id());
    let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    loop {
        if let Some(pid) = find_main_window(&mine) {
            return process_folder(pid);
        }
        if std::time::Instant::now() >= end {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The process id of a PocketMedia main window that is not this install's.
fn find_main_window(mine: &str) -> Option<u32> {
    struct Search<'a> {
        mine: &'a str,
        found: Option<u32>,
    }
    unsafe extern "system" fn each(h: HWND, lparam: LPARAM) -> BOOL {
        // SAFETY: lparam is the &mut Search passed to EnumWindows below, alive for the whole call.
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut buf = [0u16; 128];
        // SAFETY: `buf` is a valid buffer of the length given.
        let n = unsafe { GetClassNameW(h, &mut buf) };
        let class = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        if (class.starts_with(CLASS_PREFIX) || class == OLD_CLASS) && class != search.mine {
            let mut pid = 0u32;
            // SAFETY: `h` comes from EnumWindows; a stale one only makes this fail.
            unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
            if pid != 0 && pid != std::process::id() {
                search.found = Some(pid);
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut search = Search { mine, found: None };
    // SAFETY: the callback only reads window data and writes into `search`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut search as *mut Search as isize)) };
    search.found
}

/// The folder of a process's program file.
fn process_folder(pid: u32) -> Option<String> {
    // SAFETY: a plain id; the handle gets one owner.
    let process = OwnedHandle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let mut buf = vec![0u16; 32_768];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` UTF-16 units, as the call is told.
    unsafe { QueryFullProcessImageNameW(process.0, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    let exe = PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]));
    exe.parent().map(crate::import::display_path)
}

/// What a later launch asks for (the message's wParam): show the quick popup.
pub const SHOW_POPUP: usize = 0;
/// `--backup` (the uninstaller's Back up library… button): open Settings at Back
/// up library and its folder picker.
pub const BACK_UP: usize = 1;

/// The message a later launch sends the running PocketMedia's main window.
pub fn activate_message() -> u32 {
    // SAFETY: a static name; registering the same name again returns the same id.
    unsafe { RegisterWindowMessageW(w!("PocketMedia.Activate")) }
}

/// Asks the running copy of **this install** to do `what` (`SHOW_POPUP` or
/// `BACK_UP`). It may still be starting, so its window is looked for for up to
/// 5 seconds. Returns false if not found.
pub fn activate_running(what: usize) -> bool {
    let msg = activate_message();
    for _ in 0..50 {
        // SAFETY: this install's class name; no window title.
        if let Ok(h) = unsafe { FindWindowW(main_class(), None) } {
            let mut pid = 0u32;
            // SAFETY: `h` was just found; a stale one only makes these calls fail.
            unsafe {
                GetWindowThreadProcessId(h, Some(&mut pid));
                // Lets the running copy bring its popup to the front.
                let _ = AllowSetForegroundWindow(pid);
                return PostMessageW(Some(h), msg, WPARAM(what), LPARAM(0)).is_ok();
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_depends_on_the_folder_only() {
        assert_eq!(hash_text("c:\\pocketmedia"), hash_text("c:\\pocketmedia"));
        assert_ne!(hash_text("c:\\pocketmedia"), hash_text("d:\\pocketmedia"));
        assert_eq!(hash_text("").len(), 16);
        // FNV-1a 64 of nothing is its offset basis.
        assert_eq!(hash_text(""), "cbf29ce484222325");
    }

    #[test]
    fn the_same_folder_spelled_differently_is_the_same_install() {
        let dir = std::env::temp_dir().join(format!("pm-instance-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        assert_eq!(install_id(&dir), install_id(&dir.join("sub").join("..")));
        assert_ne!(install_id(&dir), install_id(&dir.join("sub")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_other_copy_message_names_both_folders_and_what_to_do() {
        let m = other_copy_message(Some(r"C:\dev\target\release"), true);
        assert!(m.contains(r"C:\dev\target\release"), "{m}");
        assert!(m.contains("Nothing was backed up"), "{m}");
        assert!(m.contains("did not start") && m.contains("choose Exit"), "{m}");
        let m = other_copy_message(None, false);
        assert!(m.starts_with("Another PocketMedia is already running.") && !m.contains("backed up"), "{m}");
    }
}
