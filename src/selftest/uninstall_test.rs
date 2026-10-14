//! Self-test steps for uninstall safety:
//! - A second copy of the program in `test-results\other` hands nothing
//!   to this one, with `--backup` or without, and says which copy is running;
//!   `--which-running` (the uninstaller's question) answers from both copies.
//! - The uninstaller's `--backup` while Add media is open or Settings has
//!   unsaved edits shows a note saying what to finish first, and opens no picker.
//! - While a backup runs, its folder says "(unfinished)", Windows is asked
//!   to wait before signing out or shutting down, and Exit asks first.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Shutdown::ShutdownBlockReasonQuery;
use windows::Win32::UI::WindowsAndMessaging::{IDNO, SendMessageW, WM_CLOSE, WM_COMMAND, WM_QUERYENDSESSION};
use windows::core::PWSTR;

use super::watch_test::{open_manager, page};
use super::preview_test::close_windows;
use super::settings_test::Set;
use super::{
    CMD_FOCUS, CMD_MANAGER, CMD_REQUEST_EXIT, El, Report, Session, capture_window, find_window, foreground, hwnd, is_visible,
    longest_child_text, post, save_capture, send, tray_icon_rect, wait_until,
};
use crate::ui::manager_view::MHit;
use crate::ui::settings_view::SHit;

/// Starts `exe` with `args` and waits up to `ms` for it to end. Its exit code, or
/// None (then it was ended).
fn run_for(exe: &Path, args: &[&str], ms: u64) -> Option<i32> {
    let mut child = Command::new(exe).args(args).spawn().ok()?;
    let mut code = None;
    wait_until(ms, || {
        code = child.try_wait().ok().flatten().and_then(|s| s.code());
        code.is_some()
    });
    if code.is_none() {
        let _ = child.kill();
    }
    code
}

/// `--which-running <file>`: the exit code and the answer (without its BOM).
fn which_running(exe: &Path, file: &Path) -> (Option<i32>, String) {
    let _ = std::fs::remove_file(file);
    let code = run_for(exe, &["--which-running", &file.display().to_string()], 8000);
    let text = std::fs::read_to_string(file).unwrap_or_default();
    (code, text.trim_start_matches('\u{feff}').to_string())
}

fn folder_of(exe: &Path) -> String {
    exe.parent().map(crate::import::display_path).unwrap_or_default()
}

// ----------------------------------------------------------------

/// A copy of the program in `test-results\other` while this one runs.
pub(super) fn other_copy(r: &mut Report, t: &Session, exe: &Path) {
    r.info("A second copy of the program, in another folder");
    let dir = t.out.join("other");
    let _ = std::fs::remove_dir_all(&dir);
    let copy = dir.join("pocketmedia.exe");
    if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| std::fs::copy(exe, &copy)) {
        r.check("prepare the copy in test-results\\other", false, &e.to_string());
        return;
    }
    let mine = folder_of(exe);

    // The uninstaller's question, asked from both copies.
    let (code, text) = which_running(&copy, &dir.join("which-running.txt"));
    let expected = format!("other\t{mine}");
    r.check(
        "--which-running from the other copy: a different PocketMedia is running, and from where (code 2)",
        code == Some(2) && text.eq_ignore_ascii_case(&expected),
        &format!("code {code:?}, answer {text:?}, expected {expected:?}"),
    );
    let (code, text) = which_running(exe, &t.out.join("which-running-this.txt"));
    r.check("--which-running from this copy: this one is running (code 1)", code == Some(1) && text == "this", &format!("code {code:?}, answer {text:?}"));

    // --backup from the other copy: a message, nothing handed over.
    let manager = send(t.main, CMD_MANAGER, 0);
    let manager_shown = manager != 0 && is_visible(manager);
    let asked = crate::dialogs::backup_picker_asked();
    let other_box = |child: u32| find_window("#32770", Some(crate::instance::OTHER_COPY_TITLE), child);
    let Ok(mut child) = Command::new(&copy).arg("--backup").spawn() else {
        r.check("the other copy starts with --backup", false, "spawn failed");
        return;
    };
    let mut boxed = 0isize;
    let shown = wait_until(10_000, || {
        boxed = other_box(child.id());
        boxed != 0
    });
    let text = if shown {
        std::thread::sleep(Duration::from_millis(300));
        longest_child_text(boxed)
    } else {
        String::new()
    };
    r.check(
        "the other copy with --backup does not hand it to this one: it says which copy is running and that nothing was backed up",
        shown && text.to_lowercase().contains(&mine.to_lowercase()) && text.contains("Nothing was backed up"),
        &text.replace('\n', " / "),
    );
    if shown {
        save_capture(r, &t.out.join("screens").join("state-other-copy-backup.png"), capture_window(boxed));
        // WM_CLOSE answers an OK-only box, as in blocked_folder_test (a posted
        // WM_COMMAND IDOK did not end it in run 2).
        post(boxed, WM_CLOSE, 0, 0);
    }
    let mut code = None;
    let ended = wait_until(10_000, || {
        code = child.try_wait().ok().flatten().and_then(|s| s.code());
        code.is_some()
    });
    if !ended {
        r.info(&format!("the other copy was still running; its windows: box still there = {}", other_box(child.id()) != 0));
        let _ = child.kill();
    }
    std::thread::sleep(Duration::from_millis(500));
    let manager_now = send(t.main, CMD_MANAGER, 0);
    r.check(
        "…it exits without starting (code 3), and this PocketMedia opened no Settings and no backup picker",
        code == Some(3)
            && crate::dialogs::backup_picker_asked() == asked
            && (manager_shown || manager_now == 0 || !is_visible(manager_now)),
        &format!("exit code {code:?}; pickers {} -> {}", asked, crate::dialogs::backup_picker_asked()),
    );

    // A plain launch of the other copy: the same message, without "backed up".
    let popup_was = is_visible(t.popup);
    if let Ok(mut child) = Command::new(&copy).spawn() {
        let mut boxed = 0isize;
        let shown = wait_until(10_000, || {
            boxed = other_box(child.id());
            boxed != 0
        });
        let text = if shown { longest_child_text(boxed) } else { String::new() };
        if shown {
            post(boxed, WM_CLOSE, 0, 0);
        }
        let mut code = None;
        let ended = wait_until(10_000, || {
            code = child.try_wait().ok().flatten().and_then(|s| s.code());
            code.is_some()
        });
        if !ended {
            let _ = child.kill();
        }
        r.check(
            "a plain launch of the other copy says which copy is running, exits (code 3) and does not show this one's popup",
            shown && text.contains("did not start") && !text.contains("backed up") && code == Some(3) && is_visible(t.popup) == popup_was,
            &format!("exit code {code:?}; {}", text.replace('\n', " / ")),
        );
    }

    // From autostart: it ends quietly, with no message.
    let code = run_for(&copy, &[crate::autostart::ARG], 10_000);
    r.check("the other copy started by autostart ends quietly (code 3, no message)", code == Some(3), &format!("exit code {code:?}"));
    r.check("this PocketMedia keeps its tray icon and shortcut", tray_icon_rect(t.main).is_some() && send(t.main, super::CMD_HOTKEY_OK, 0) == 1, "");
}

// ----------------------------------------------------------------

/// The uninstaller's `--backup` while Add media is open, then while Settings has
/// unsaved edits: a visible note, no picker, nothing lost.
pub(super) fn backup_waits(r: &mut Report, t: &Session, exe: &Path) {
    r.info("Back up library from the uninstaller while something is unfinished");
    close_windows(r, t);
    crate::dialogs::set_test_backup(None);
    let Some((s, _)) = open_manager(r, t) else { return };
    page(r, &s, false);
    s.manager_click(r, MHit::AddMedia);
    if !r.check("Add media is open in the Manager", wait_until(2000, || s.describe().contains("screen=add")), &s.describe()) {
        return;
    }
    let asked = crate::dialogs::backup_picker_asked();
    let code = run_for(exe, &["--backup"], 10_000);
    let noted = wait_until(4000, || s.describe().contains(crate::add_media::BACKUP_WAITS));
    let d = s.describe();
    r.check(
        "--backup while Add media is open shows a note to finish or cancel Add media first, and opens no picker",
        code == Some(0) && noted && d.contains("screen=add") && crate::dialogs::backup_picker_asked() == asked,
        &format!("exit code {code:?}; {d}"),
    );
    s.park();
    s.shot(r, "state-add-media-backup-waits");
    s.add_click(r, El::AddBack);
    r.check("…← leaves Add media as usual", wait_until(2000, || !s.describe().contains("screen=add")), &s.describe());

    // Settings with an unsaved change.
    page(r, &s, true);
    s.scroll_to(100_000);
    s.click(r, SHit::Autostart);
    if !r.check("Settings has an unsaved change", wait_until(2000, || s.describe().contains("dirty=true")), &s.describe()) {
        close_windows(r, t);
        return;
    }
    let code = run_for(exe, &["--backup"], 10_000);
    let noted = wait_until(4000, || s.describe().contains(crate::manager::SETTINGS_WAIT));
    let d = s.describe();
    r.check(
        "--backup while Settings has unsaved changes says to save or cancel first, keeps the change and opens no picker",
        code == Some(0) && noted && d.contains("dirty=true") && crate::dialogs::backup_picker_asked() == asked,
        &format!("exit code {code:?}; {d}"),
    );
    s.park();
    s.shot(r, "state-settings-backup-waits");
    s.click(r, SHit::Cancel);
    r.check("…Cancel discards the change", wait_until(2000, || s.describe().contains("dirty=false")), &s.describe());
    close_windows(r, t);
}

// ----------------------------------------------------------------

fn names(dir: &Path) -> Vec<String> {
    let mut n: Vec<String> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    n.sort();
    n
}

/// WM_QUERYENDSESSION's answer from a window of ours (1: Windows may end the session).
fn end_session_answer(window: isize) -> isize {
    // SAFETY: SendMessageW to our own window; the handler only reads a flag.
    unsafe { SendMessageW(hwnd(window), WM_QUERYENDSESSION, Some(WPARAM(0)), Some(LPARAM(0))) }.0
}

/// The reason a window gives Windows for blocking shutdown, or "".
fn block_reason(window: isize) -> String {
    let mut buf = vec![0u16; 512];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` holds `len` UTF-16 units, as the call is told.
    match unsafe { ShutdownBlockReasonQuery(hwnd(window), Some(PWSTR(buf.as_mut_ptr())), &mut len) } {
        Ok(()) => String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)]),
        Err(_) => String::new(),
    }
}

/// While a slowed backup runs in `s` (the Manager's Settings) into `dest`.
pub(super) fn while_backing_up(r: &mut Report, t: &Session, s: &Set, dest: &Path) {
    let n = names(dest);
    r.check(
        "while it runs, the backup folder is named \"PocketMedia backup … (unfinished)\"",
        n.len() == 1 && crate::backup::is_unfinished(&n[0]),
        &format!("{n:?}"),
    );
    let answers = (end_session_answer(t.main), end_session_answer(s.hwnd));
    let reason = block_reason(s.hwnd);
    r.check(
        "signing out or shutting down now: Windows is asked to wait, with the reason \"A backup is running. Stop it?…\"",
        answers == (0, 0) && reason == crate::backup::SHUTDOWN_REASON && block_reason(t.main) == reason,
        &format!("answers {answers:?}; reason {reason:?}"),
    );

    // Exit from the tray menu: asks first. No keeps everything running.
    // Only while the backup still runs: otherwise Exit would end the self-test.
    if !crate::backup::running() {
        r.check("Exit while a backup runs asks first", false, "the backup had already ended; the check was not run");
        return;
    }
    post(t.main, crate::WM_SELFTEST, CMD_REQUEST_EXIT, 0);
    let me = std::process::id();
    let mut boxed = 0isize;
    let asked = wait_until(3000, || {
        boxed = find_window("#32770", Some(crate::STOP_BACKUP_TITLE), me);
        boxed != 0 && is_visible(boxed)
    });
    let text = if asked {
        // Long enough for Windows to finish laying out and drawing the box.
        std::thread::sleep(Duration::from_millis(800));
        longest_child_text(boxed)
    } else {
        String::new()
    };
    r.check("Exit while a backup runs asks first: \"A backup is running. Stop it?\"", asked && text.starts_with("A backup is running. Stop it?"), &text.replace('\n', " / "));
    if asked {
        save_capture(r, &t.out.join("screens").join("state-exit-backup-running.png"), capture_window(boxed));
        post(boxed, WM_COMMAND, IDNO.0 as usize, 0);
    }
    let closed = wait_until(3000, || !is_visible(boxed));
    r.check(
        "…No keeps PocketMedia and the backup running",
        closed && s.describe().contains("backup_running=true") && tray_icon_rect(t.main).is_some(),
        &s.describe(),
    );
    send(t.main, CMD_FOCUS, s.hwnd);
    wait_until(1500, || foreground() == s.hwnd);
}

/// After the full backup finished in `dest`.
pub(super) fn after_backup(r: &mut Report, t: &Session, s: &Set, dest: &Path) {
    let n = names(dest);
    r.check(
        "the finished backup has its final name, and no \"(unfinished)\" folder is left",
        n.len() == 1 && n[0].starts_with("PocketMedia backup ") && !crate::backup::is_unfinished(&n[0]),
        &format!("{n:?}"),
    );
    let answers = (end_session_answer(t.main), end_session_answer(s.hwnd));
    r.check(
        "after the backup, Windows may sign out and shut down again (no reason left)",
        answers == (1, 1) && block_reason(s.hwnd).is_empty() && block_reason(t.main).is_empty(),
        &format!("answers {answers:?}"),
    );
}

// ----------------------------------------------------------------

/// Files the uninstaller would delete that are not library items: a subfolder,
/// another file type, a loose file, plus a temporary file the backup skips.
pub(super) fn extra_media(media: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    vec![
        (media.join("Pictures").join("Trip").join("day 1").join("beach.png"), b"a picture in a subfolder".to_vec()),
        (media.join("Pictures").join("holiday.webp"), b"a .webp picture".to_vec()),
        (media.join("loose note.txt"), b"a loose file in Media".to_vec()),
    ]
}

pub(super) fn partial_file(media: &Path) -> PathBuf {
    media.join("GIFs").join("download.gif.partial")
}
