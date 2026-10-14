//! Autostart PocketMedia (Settings): a value in the current user's
//! `Run` key that starts this program with `--autostart`, which goes quietly to
//! the tray. The registry is the truth: Settings shows the toggle on only when
//! the value is there and starts this copy of PocketMedia.
//!
//! The self-test points this at its own key (`Software\PocketMedia\SelfTest\Run`),
//! so a test run never changes what starts with Windows.

use std::sync::Mutex;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegDeleteKeyW, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::HSTRING;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const TEST_PARENT: &str = r"Software\PocketMedia";
const TEST_KEY: &str = r"Software\PocketMedia\SelfTest\Run";
const VALUE: &str = "PocketMedia";
pub const ARG: &str = "--autostart";

static USE_TEST_KEY: Mutex<bool> = Mutex::new(false);

fn key() -> &'static str {
    if USE_TEST_KEY.lock().map(|t| *t).unwrap_or(false) { TEST_KEY } else { RUN_KEY }
}

/// Self-test only: from now on, use the test key instead of the real Run key.
pub fn use_test_key() {
    if let Ok(mut t) = USE_TEST_KEY.lock() {
        *t = true;
    }
}

/// Self-test only: puts `text` in the test key's value (as another copy's
/// Autostart would). Refused unless the test key is in use, so the real Run key
/// is never written this way.
pub fn write_for_test(text: &str) -> Result<(), String> {
    if key() != TEST_KEY {
        return Err("only the self-test key can be written this way".into());
    }
    let (key, value) = (HSTRING::from(TEST_KEY), HSTRING::from(VALUE));
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    // SAFETY: `wide` is a null-terminated UTF-16 string of the length given.
    check(unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &key, &value, REG_SZ.0, Some(wide.as_ptr().cast()), (wide.len() * 2) as u32) })
}

fn check(e: WIN32_ERROR) -> Result<(), String> {
    if e == ERROR_SUCCESS { Ok(()) } else { Err(windows::core::Error::from_hresult(e.to_hresult()).message()) }
}

/// What the value should hold: this program, quoted, with `--autostart`.
pub fn command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("the program's location is unknown: {e}"))?;
    Ok(format!("\"{}\" {ARG}", crate::import::display_path(&exe)))
}

/// The value as stored, if any.
pub fn read() -> Option<String> {
    let (key, value) = (HSTRING::from(key()), HSTRING::from(VALUE));
    let mut bytes = 0u32;
    // SAFETY: a size query; no buffer is passed.
    let e = unsafe { RegGetValueW(HKEY_CURRENT_USER, &key, &value, RRF_RT_REG_SZ, None, None, Some(&mut bytes)) };
    if e != ERROR_SUCCESS || bytes == 0 {
        return None;
    }
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: `buf` holds `len` bytes, as the call is told.
    let e = unsafe { RegGetValueW(HKEY_CURRENT_USER, &key, &value, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut len)) };
    if e != ERROR_SUCCESS {
        return None;
    }
    let text = String::from_utf16_lossy(&buf[..(len as usize / 2)]);
    Some(text.trim_end_matches('\0').to_string())
}

/// On when the value starts this copy of PocketMedia.
pub fn is_on() -> bool {
    match (read(), command()) {
        (Some(v), Ok(c)) => v.eq_ignore_ascii_case(&c),
        _ => false,
    }
}

/// The program a Run value starts: the quoted path at its start, or its first word.
pub fn program_of(value: &str) -> Option<String> {
    let v = value.trim();
    let exe = match v.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => v.split(' ').next()?,
    };
    (!exe.is_empty()).then(|| exe.to_string())
}

/// When the value starts another PocketMedia that still exists, the reason it
/// is kept (one copy must not take over another's Autostart).
/// A value whose program is gone is stale and may be replaced.
pub fn other_copy_problem(stored: Option<&str>, mine: &str) -> Option<String> {
    let stored = stored?;
    if stored.eq_ignore_ascii_case(mine) {
        return None;
    }
    let exe = std::path::PathBuf::from(program_of(stored)?);
    if !exe.is_file() {
        return None;
    }
    let folder = exe.parent().map(crate::import::display_path).unwrap_or_default();
    Some(format!("Autostart is left off: it starts the PocketMedia in {folder}. Turn it off there first."))
}

/// Turns autostart on or off, then checks that the registry says so. It never
/// replaces a value that starts another PocketMedia, and off removes only a
/// value that starts this one.
pub fn set(on: bool) -> Result<(), String> {
    let (key, value) = (HSTRING::from(key()), HSTRING::from(VALUE));
    let stored = read();
    if let Some(problem) = other_copy_problem(stored.as_deref(), &command()?) {
        if on {
            return Err(problem);
        }
        return Ok(()); // Off: this copy has no value to remove; the other copy's stays.
    }
    if on {
        let cmd: Vec<u16> = command()?.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `cmd` is a null-terminated UTF-16 string of the length given.
        let e = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, &key, &value, REG_SZ.0, Some(cmd.as_ptr().cast()), (cmd.len() * 2) as u32) };
        check(e)?;
    } else {
        // SAFETY: plain strings; a value that is not there is not an error.
        let e = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &key, &value) };
        if e != ERROR_FILE_NOT_FOUND {
            check(e)?;
        }
    }
    if is_on() == on { Ok(()) } else { Err("Windows did not keep the change".into()) }
}

/// Settings shows what the registry says, not what the library file remembers
/// (the file may come from a backup made on another install).
pub fn sync_store() {
    let on = is_on();
    crate::store::with(|s| s.data.settings.autostart = on);
}

/// Self-test only: removes the test key and, if empty, its parent.
pub fn remove_test_key() {
    let (tree, parent) = (HSTRING::from(r"Software\PocketMedia\SelfTest"), HSTRING::from(TEST_PARENT));
    // SAFETY: plain strings; these keys belong to the self-test only. RegDeleteKeyW
    // fails (and keeps it) when the parent has anything else in it.
    unsafe {
        let _ = RegDeleteTreeW(HKEY_CURRENT_USER, &tree);
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, &tree);
        let _ = RegDeleteKeyW(HKEY_CURRENT_USER, &parent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_program_is_read_from_a_run_value() {
        assert_eq!(program_of(r#""C:\Pocket Media\pocketmedia.exe" --autostart"#).as_deref(), Some(r"C:\Pocket Media\pocketmedia.exe"));
        assert_eq!(program_of(r"C:\pm\pocketmedia.exe --autostart").as_deref(), Some(r"C:\pm\pocketmedia.exe"));
        assert_eq!(program_of("  "), None);
        assert_eq!(program_of("\"\""), None);
    }

    #[test]
    fn another_existing_copys_value_is_kept_and_a_stale_one_is_not() {
        let dir = std::env::temp_dir().join(format!("pm-autostart-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join("pocketmedia.exe");
        std::fs::write(&other, b"exe").unwrap();
        let theirs = format!("\"{}\" --autostart", other.display());
        let mine = r#""C:\PocketMedia\pocketmedia.exe" --autostart"#;
        let problem = other_copy_problem(Some(&theirs), mine).unwrap();
        assert!(problem.contains(&dir.display().to_string()) && problem.starts_with("Autostart is left off: it starts the PocketMedia in") && problem.ends_with("Turn it off there first."), "{problem}");
        assert_eq!(other_copy_problem(Some(&mine.to_uppercase()), mine), None, "this copy's own value");
        assert_eq!(other_copy_problem(None, mine), None);
        std::fs::remove_file(&other).unwrap();
        assert_eq!(other_copy_problem(Some(&theirs), mine), None, "its program is gone: stale");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
