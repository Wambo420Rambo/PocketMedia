//! Self-test for hidden windows, notices and Exit, in three quick-run sections:
//!
//! - `release`: with Use less memory the hidden popup and Manager let go
//!   of their windows, and with both hidden the thumbnail workers and the GIF
//!   and video threads end; a made-again popup opens within 200 ms, keeps its
//!   list and filter, takes drops, drags out and toggles with the shortcut; an
//!   import still copying and a backup still running keep their window until
//!   they end; Open faster keeps the popup ready but still releases the Manager.
//! - `notices`: watching that stops or cannot start, a paste or drag-out
//!   whose Recently used entry cannot be saved, and a start-up comparison whose
//!   result cannot be saved are each said in plain words where the user looks.
//! - `exit-import`: Exit while an import copies asks first (No is the
//!   default and keeps it running); Yes stops it cleanly and exits. Signing out
//!   meanwhile makes Windows ask. Last, because PocketMedia is gone afterwards.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::{DM_GETDEFID, IDNO, IDYES, IsWindow, SendMessageW, WM_CLOSE, WM_COMMAND};

use super::preview_test::{self, describe, field, mib, private_bytes, settle};
use super::{
    CMD_BACKUP_START, CMD_FOCUS, CMD_MANAGER_DESCRIBE, CMD_MANAGER_POINT, CMD_REQUEST_EXIT, CMD_RESET, CMD_SHORTCUT, DESCRIBE, El,
    Report, Session, capture_screen, capture_window, find_window, foreground, guard, hwnd, is_visible, longest_child_text, mouse_to,
    post, press, press_hotkey, save_capture, seed, send, status, wait_until, window_rect,
};
use crate::ui::manager_view::MHit;

/// Puts the windows' and threads' state in DESCRIBE (see `state`).
pub(super) const CMD_H_DESCRIBE: usize = 60;
/// Watching is started on a folder that does not exist and the failure is
/// reported as at start-up (`report_watch_failure`). 1 if it failed as it should.
pub(super) const CMD_WATCH_START_FAIL: usize = 61;
/// Watching the real Media folder again. 1 if it started.
pub(super) const CMD_WATCH_RESUME: usize = 62;
/// A file the library does not know is put in Media\Pictures, the next save is
/// made to fail and the library is read as at start-up. DESCRIBE gets the
/// message the user would see; 1 if there is one.
pub(super) const CMD_STARTUP_COMPARE: usize = 63;
/// The question Exit would ask now: 0 none, 1 backup, 2 import.
pub(super) const CMD_EXIT_QUESTION: usize = 64;

const FOUND_AT_START: &str = "Found at start.png";

/// Runs on the UI thread for the commands above.
pub(super) fn on_ui_command(cmd: usize, _arg: isize) -> isize {
    match cmd {
        CMD_H_DESCRIBE => {
            let text = format!(
                "popup_window={} popup_releases={} manager_window={} manager_releases={} thumb_workers={} preview_threads={} \
                 targets_made={} profile={:?} copying={} watch_problem={:?}",
                crate::popup::hwnd().0 as isize,
                crate::popup::releases(),
                crate::manager::hwnd().0 as isize,
                crate::manager::releases(),
                crate::thumbs::workers(),
                crate::preview::threads(),
                crate::ui::d2d::TARGETS_MADE.load(Ordering::Relaxed),
                crate::store::settings().profile,
                crate::add_panel::copies_running(),
                crate::watch::problem().unwrap_or_default(),
            );
            if let Ok(mut d) = DESCRIBE.lock() {
                *d = text;
            }
            0
        }
        CMD_WATCH_START_FAIL => {
            let Some(p) = crate::paths::get() else { return 0 };
            match crate::watch::start(&p.media.join("No such folder"), crate::watch::BUFFER) {
                Ok(()) => 0,
                Err(e) => {
                    crate::report_watch_failure(&e);
                    if let Ok(mut d) = DESCRIBE.lock() {
                        *d = crate::watch::start_failure_message(&e);
                    }
                    1
                }
            }
        }
        CMD_WATCH_RESUME => crate::paths::get().is_some_and(|p| crate::watch::start(&p.media, crate::watch::BUFFER).is_ok()) as isize,
        CMD_STARTUP_COMPARE => {
            let Some(p) = crate::paths::get() else { return 0 };
            if p.root.file_name().and_then(|n| n.to_str()) != Some("app") {
                return 0;
            }
            crate::watch::stop();
            let wrote = std::fs::write(p.pictures.join(FOUND_AT_START), seed::png(30, 20, 77)).is_ok();
            crate::library::FAIL_SAVES.store(1, Ordering::SeqCst);
            let (_, unsaved) = crate::load_library(&p.media);
            crate::library::FAIL_SAVES.store(0, Ordering::SeqCst);
            let _ = crate::watch::start(&p.media, crate::watch::BUFFER);
            let shown = unsaved.is_some();
            if let Ok(mut d) = DESCRIBE.lock() {
                *d = format!("wrote={wrote} message={}", unsaved.unwrap_or_default().replace('\n', " / "));
            }
            shown as isize
        }
        CMD_EXIT_QUESTION => match crate::exit_question() {
            None => 0,
            Some((text, _)) if text == crate::STOP_BACKUP_TEXT => 1,
            Some(_) => 2,
        },
        _ => 0,
    }
}

fn state(t: &Session) -> String {
    send(t.main, CMD_H_DESCRIBE, 0);
    DESCRIBE.lock().map(|d| d.clone()).unwrap_or_default()
}

fn manager_describe(t: &Session) -> String {
    send(t.main, CMD_MANAGER_DESCRIBE, 0);
    DESCRIBE.lock().map(|d| d.clone()).unwrap_or_default()
}

fn exists(window: isize) -> bool {
    // SAFETY: IsWindow accepts any value, including a destroyed window's.
    window != 0 && unsafe { IsWindow(Some(hwnd(window))) }.as_bool()
}

/// The `status="…"` text of a popup or Manager description.
fn status_of(d: &str) -> String {
    d.split("status=\"").nth(1).and_then(|s| s.split('"').next()).unwrap_or("").to_string()
}

/// Puts the pointer at the left of the screen, so the popup opens away from the test target.
fn pointer_left() {
    let work = super::primary_work_area();
    mouse_to(work.left + (work.right - work.left) / 4, work.top + 120);
}

/// Gives the test target the focus, so the popup remembers it as the app to paste into.
fn focus_target(t: &Session) {
    send(t.main, CMD_FOCUS, t.target);
    wait_until(1500, || foreground() == t.target);
}

/// Escape on the popup (when it is in front) and waits until it is hidden.
fn close_popup(r: &mut Report, t: &Session) -> bool {
    let p = t.popup();
    if is_visible(p) && foreground() == p && guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup()))
}

/// `n` small pictures named "`stem` N.png" in a fresh folder under the results.
fn source(out: &Path, dir: &str, stem: &str, n: u32) -> (PathBuf, Vec<PathBuf>) {
    let folder = out.join(dir);
    let _ = std::fs::remove_dir_all(&folder);
    let _ = std::fs::create_dir_all(&folder);
    let files: Vec<PathBuf> = (1..=n)
        .map(|i| {
            let f = folder.join(format!("{stem} {i}.png"));
            let _ = std::fs::write(&f, seed::png(40 + i, 30, 900 + i));
            f
        })
        .collect();
    (folder, files)
}

fn media(t: &Session) -> PathBuf {
    t.out.join("app").join("Media")
}

/// Files in Media\Pictures whose name starts with `stem`, and leftover temporary copies there.
fn copies(t: &Session, stem: &str) -> (usize, usize) {
    let names: Vec<String> = std::fs::read_dir(media(t).join("Pictures"))
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect())
        .unwrap_or_default();
    (names.iter().filter(|n| n.starts_with(stem)).count(), names.iter().filter(|n| crate::import::is_partial(n)).count())
}

/// Items in the library file on disk whose path starts with `prefix`.
fn recorded(t: &Session, prefix: &str) -> usize {
    let stored: Option<serde_json::Value> = std::fs::read_to_string(t.out.join("app").join("Data").join("pocketmedia.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    stored
        .as_ref()
        .and_then(|v| v["items"].as_array())
        .map(|items| items.iter().filter(|i| i["path"].as_str().is_some_and(|p| p.starts_with(prefix))).count())
        .unwrap_or(0)
}

/// Opens Add media in the popup, chooses `files` and starts adding them; true once it copies.
fn start_import(r: &mut Report, t: &Session, folder: PathBuf, files: Vec<PathBuf>) -> bool {
    let n = files.len();
    crate::dialogs::set_test_pick(files, folder);
    if !t.open(r) {
        return false;
    }
    t.click(r, El::AddMediaButton);
    t.click(r, El::AddChooseFiles);
    let reviewed = t.wait_state("stage=review", 8000) && t.describe().contains(&format!("label=\"Add {n} files\""));
    if !r.check(&format!("(setup: the review offers the {n} files)"), reviewed, &t.describe()) {
        return false;
    }
    t.click(r, El::AddAdd);
    r.check("(setup: the import is copying)", t.wait_state("stage=copying", 3000), &t.describe())
}

/// Presses the shortcut and times the popup: until its window shows, and until
/// every tile it draws has its picture. Returns (window ms, pictures ms).
fn timed_open(r: &mut Report, t: &Session) -> Option<(f64, f64)> {
    if !guard(r, &t.allowed) {
        return None;
    }
    press_hotkey();
    let start = Instant::now();
    let mut shown = None;
    while start.elapsed() < Duration::from_secs(3) {
        let p = t.popup();
        if shown.is_none() && is_visible(p) {
            shown = Some(start.elapsed().as_secs_f64() * 1000.0);
        }
        if let Some(s) = shown {
            let d = describe(t, 0);
            if field(&d, "drawn") > 0 && field(&d, "pending") == 0 && field(&d, "busy") == 0 {
                let pictures = start.elapsed().as_secs_f64() * 1000.0;
                wait_until(1000, || foreground() == t.popup());
                return Some((s, pictures));
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    None
}

fn shot_window(r: &mut Report, t: &Session, window: isize, name: &str) {
    std::thread::sleep(Duration::from_millis(200));
    if let Some(rect) = window_rect(window) {
        save_capture(r, &t.out.join("screens").join(format!("{name}.png")), capture_screen(rect));
    }
}

/// Clicks a Manager element (as the other Manager checks do).
fn manager_click(r: &mut Report, t: &Session, hit: MHit) -> bool {
    let Some((x, y)) = super::unpack(send(t.main, CMD_MANAGER_POINT, super::manager_test::code(hit))) else {
        return r.check(&format!("find Manager {hit:?}"), false, "not on screen");
    };
    if !guard(r, &t.allowed) {
        return false;
    }
    mouse_to(x, y);
    std::thread::sleep(Duration::from_millis(60));
    super::mouse_button(true);
    std::thread::sleep(Duration::from_millis(40));
    super::mouse_button(false);
    std::thread::sleep(Duration::from_millis(250));
    true
}

/// Opens the Manager from the popup; its window, once it is in front.
fn open_manager(r: &mut Report, t: &Session) -> Option<isize> {
    if !t.open(r) || !t.click(r, El::OpenManager) {
        return None;
    }
    let mut m = 0;
    let shown = wait_until(3000, || {
        m = t.manager();
        is_visible(m) && foreground() == m
    });
    shown.then_some(m)
}

// ----------------------------------------------------------------

pub(super) fn release(r: &mut Report, t: &Session) -> bool {
    r.info("Hidden windows let go of what they hold");
    preview_test::close_windows(r, t);
    send(t.main, CMD_RESET, 0);
    send(t.main, preview_test::CMD_SET_PROFILE, 0);
    focus_target(t);
    pointer_left();

    // 1. Browse All media with its GIFs playing, then hide the popup on GIFs.
    if !preview_test::open_all_media(r, t) {
        return r.check("open the popup on All media", false, &t.describe());
    }
    settle(t, 0, 15_000);
    let _ = preview_test::grows(t, 0, "gif_frames", 2000);
    let first = t.popup();
    std::thread::sleep(Duration::from_millis(300));
    let open_bytes = private_bytes();
    r.info(&format!("popup open on All media: {}; private bytes {}", state(t), mib(open_bytes)));
    t.click(r, El::Chip(1));
    t.park();
    if !close_popup(r, t) {
        return r.check("Escape hides the popup", false, &t.describe());
    }
    let released = wait_until(2000, || t.popup() == 0);
    r.check(
        "with Use less memory, the hidden popup lets go of its window",
        released && !exists(first),
        &format!("window {first:#x} still exists: {}; {}", exists(first), state(t)),
    );
    let ended = wait_until(3000, || {
        let s = state(t);
        field(&s, "thumb_workers") == 0 && field(&s, "preview_threads") == 0
    });
    r.check("…and with no window shown, the thumbnail workers and the GIF and video threads end", ended, &state(t));
    std::thread::sleep(Duration::from_millis(700));
    let hidden_bytes = private_bytes();
    r.check(
        "…and PocketMedia holds less memory than while the popup was open",
        hidden_bytes < open_bytes,
        &format!("private bytes: open {}, hidden and released {}", mib(open_bytes), mib(hidden_bytes)),
    );
    r.info(&format!("released popup, kept state: {}", t.describe()));

    // 2. Made again on the next open: timed, on the filter it was left on.
    let targets = field(&state(t), "targets_made");
    let Some((shown, pictures)) = timed_open(r, t) else {
        return r.check("the released popup opens again with the shortcut", false, &t.describe());
    };
    let again = t.popup();
    let d = t.describe();
    r.check(
        "the shortcut makes the released popup again, on the list and filter it was left on (All media, GIFs)",
        again != 0 && again != first && d.contains("list=All") && d.contains("filter=Gif") && d.contains("window=open"),
        &format!("window {first:#x} -> {again:#x}; drawing targets made {targets} -> {}; {d}", field(&state(t), "targets_made")),
    );
    shot_window(r, t, again, "state-popup-made-again");
    t.click(r, El::Chip(0));
    t.park();
    let mut times = vec![(shown, pictures)];
    let mut windows = vec![again];
    for _ in 0..5 {
        if !close_popup(r, t) || !wait_until(2000, || t.popup() == 0) {
            return r.check("the popup is released again when hidden", false, &state(t));
        }
        let Some(time) = timed_open(r, t) else {
            return r.check("the released popup opens again", false, &t.describe());
        };
        times.push(time);
        windows.push(t.popup());
    }
    let mut sorted: Vec<f64> = times.iter().map(|t| t.1).collect();
    sorted.sort_by(f64::total_cmp);
    let worst = sorted.last().copied().unwrap_or(f64::MAX);
    let mut distinct = windows.clone();
    distinct.sort_unstable();
    distinct.dedup();
    r.check(
        "a made-again popup is on screen with every tile's picture within 200 ms (warm disk cache)",
        worst <= 200.0 && distinct.len() == windows.len(),
        &format!(
            "6 opens, window then pictures (ms): {}; median {:.0}, worst {worst:.0}; {} different windows",
            times.iter().map(|(s, p)| format!("{s:.0}/{p:.0}")).collect::<Vec<_>>().join(", "),
            sorted[sorted.len() / 2],
            distinct.len()
        ),
    );

    // 3. The shortcut toggles a made-again popup: closed (and released), then open.
    let p = t.popup();
    if !guard(r, &t.allowed) || foreground() != p {
        return r.check("the made-again popup is in front", false, &super::describe_foreground());
    }
    press_hotkey();
    let closed = wait_until(1500, || !is_visible(p)) && wait_until(1500, || t.popup() == 0);
    press_hotkey();
    let opened = wait_until(2000, || is_visible(t.popup()) && foreground() == t.popup());
    r.check("the shortcut still toggles the popup: it closes (and is released), then opens again", closed && opened, &t.describe());

    // 4. The made-again popup takes files dropped on its Add media...
    let drop_dir = t.out.join("drop-in");
    let dropped = drop_dir.join("Dropped on a made-again popup.png");
    let _ = std::fs::create_dir_all(&drop_dir);
    let _ = std::fs::write(&dropped, seed::png(44, 30, 231));
    t.click(r, El::AddMediaButton);
    if let Some(w) = window_rect(t.popup()) {
        let to = (w.left + (w.right - w.left) / 2, w.top + (w.bottom - w.top) / 3);
        if super::drag_from_target(r, t, &dropped, to, t.popup()) {
            let reviewed = t.wait_state("Dropped on a made-again popup.png:ok", 8000);
            r.check("files dropped on a made-again popup's Add media are checked for adding", reviewed, &t.describe());
        }
    }
    if guard(r, &t.allowed) {
        send(t.main, CMD_FOCUS, t.popup());
        wait_until(1500, || foreground() == t.popup());
        press(&[VK_ESCAPE]);
        std::thread::sleep(Duration::from_millis(200));
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup()));

    // ...and drags a tile out as a copy.
    focus_target(t);
    if preview_test::open_all_media(r, t) {
        let d = t.describe();
        let name = d.split("names=").nth(1).and_then(|n| n.split('|').next()).unwrap_or("").to_string();
        let received = t.out.join("drop-received.png");
        let _ = std::fs::remove_file(&received);
        if let Some(from) = t.point(El::Tile(0))
            && super::drag_to_target(r, t, from)
        {
            let got = wait_until(4000, || received.exists());
            let same = got && std::fs::read(&received).ok() == std::fs::read(super::media_file(&t.out, &name)).ok();
            r.check(&format!("a made-again popup drags {name} out as a copy, byte for byte"), same, if got { "compared" } else { "nothing dropped" });
        }
        send(t.main, CMD_FOCUS, t.popup());
        wait_until(1500, || foreground() == t.popup());
        close_popup(r, t);
    }

    // 5. An import still copying keeps the hidden popup until it ends.
    // 0.6 s before each copy: still copying well after the popup is closed.
    jobs_delay(600);
    let (folder, files) = source(&t.out, "release-src", "Kept while copying", 5);
    focus_target(t);
    if start_import(r, t, folder, files) {
        let p = t.popup();
        t.click(r, El::AddClose);
        let hidden = wait_until(1500, || !is_visible(p));
        std::thread::sleep(Duration::from_millis(600));
        let kept = t.popup() == p && exists(p);
        r.check(
            "closing the popup while its import copies hides it but keeps it (and the import) until the import ends",
            hidden && kept && state(t).contains("copying=true"),
            &format!("hidden {hidden}; window {p:#x} kept: {kept}; {}", state(t)),
        );
        let done = wait_until(15_000, || state(t).contains("copying=false"));
        let released = wait_until(2000, || t.popup() == 0);
        let (n, partial) = copies(t, "Kept while copying");
        r.check(
            "…when it ends, all 5 files are in Media and the popup is released then",
            done && released && n == 5 && partial == 0,
            &format!("{n} copied, {partial} temporary files left; {}", state(t)),
        );
    }
    jobs_delay(0);

    // 6. The Manager is released when hidden, and keeps the item that was selected.
    let Some(m1) = open_manager(r, t) else {
        return r.check("the Manager opens from the popup", false, &super::describe_foreground());
    };
    settle(t, 1, 15_000);
    manager_click(r, t, MHit::Tile(2));
    let chosen = selected(&manager_describe(t));
    post(m1, WM_CLOSE, 0, 0);
    wait_until(1500, || !is_visible(m1));
    let released = wait_until(2000, || t.manager() == 0);
    r.check(
        "the hidden Manager lets go of its window",
        released && !exists(m1),
        &format!("window {m1:#x} still exists: {}; {}; {}", exists(m1), manager_describe(t), state(t)),
    );
    let asked = Instant::now();
    let Some(m2) = open_manager(r, t) else {
        return r.check("the released Manager opens again", false, &super::describe_foreground());
    };
    let ms = asked.elapsed().as_millis();
    let d = manager_describe(t);
    r.check(
        "…it is made again when it is opened, with the same item selected",
        m2 != m1 && d.contains("screen=manager") && !chosen.is_empty() && selected(&d) == chosen,
        &format!("window {m1:#x} -> {m2:#x} in {ms} ms (from the popup's Open manager); selected {chosen:?} -> {:?}", selected(&d)),
    );
    shot_window(r, t, m2, "state-manager-made-again");

    // 7. A backup running from the Manager keeps it until the backup ends.
    let dest = t.out.join("release-backup");
    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::create_dir_all(&dest);
    crate::dialogs::set_test_backup(Some(dest.clone()));
    crate::backup::TEST_DELAY_MS.store(400, Ordering::SeqCst);
    let started = send(t.main, CMD_BACKUP_START, 0) != 0;
    post(m2, WM_CLOSE, 0, 0);
    let hidden = wait_until(1500, || !is_visible(m2));
    std::thread::sleep(Duration::from_millis(600));
    let kept = t.manager() == m2 && exists(m2) && crate::backup::running();
    r.check(
        "hiding the Manager while its backup runs keeps the Manager (and the backup)",
        started && hidden && kept,
        &format!("backup started {started}; hidden {hidden}; window kept {kept}; {}", state(t)),
    );
    crate::backup::cancel();
    let stopped = wait_until(10_000, || !crate::backup::running());
    let released = wait_until(2000, || t.manager() == 0);
    r.check("…once the backup has ended (cancelled here), the hidden Manager is released", stopped && released, &state(t));
    crate::backup::TEST_DELAY_MS.store(0, Ordering::SeqCst);
    crate::dialogs::set_test_backup(None);
    let _ = std::fs::remove_dir_all(&dest);

    // 8. Open faster: the popup is kept ready; the Manager is still released.
    let chosen = send(t.main, preview_test::CMD_SET_PROFILE, 1) == 1;
    focus_target(t);
    if !preview_test::open_all_media(r, t) {
        send(t.main, preview_test::CMD_SET_PROFILE, 0);
        return r.check("open the popup (Open faster)", false, &t.describe());
    }
    settle(t, 0, 10_000);
    let p = t.popup();
    close_popup(r, t);
    std::thread::sleep(Duration::from_millis(700));
    let kept = t.popup() == p && exists(p);
    r.check("with Open faster, the hidden popup is kept ready (not released)", chosen && kept, &format!("window {p:#x} kept: {kept}; {}", state(t)));
    if let Some((shown, pictures)) = timed_open(r, t) {
        r.info(&format!("Open faster, kept popup: window {shown:.0} ms, pictures {pictures:.0} ms"));
    }
    if let Some(m) = open_manager(r, t) {
        post(m, WM_CLOSE, 0, 0);
        wait_until(1500, || !is_visible(m));
        let released = wait_until(2000, || t.manager() == 0);
        r.check("…and the hidden Manager is still released with Open faster", released && !exists(m), &state(t));
    }
    send(t.main, preview_test::CMD_SET_PROFILE, 0);
    // Back to Use less memory: the next hide releases the popup again.
    if t.open(r) {
        close_popup(r, t);
    }
    let released = wait_until(2000, || t.popup() == 0);
    std::thread::sleep(Duration::from_millis(700));
    r.info(&format!("back on Use less memory, everything hidden: released {released}; private bytes {}; {}", mib(private_bytes()), state(t)));
    send(t.main, CMD_RESET, 0);
    true
}

fn jobs_delay(ms: u64) {
    crate::jobs::TEST_COPY_DELAY_MS.store(ms, Ordering::SeqCst);
}

fn selected(d: &str) -> String {
    d.split("selected=\"").nth(1).and_then(|s| s.split('"').next()).unwrap_or("").to_string()
}

// ----------------------------------------------------------------

pub(super) fn notices(r: &mut Report, t: &Session) -> bool {
    r.info("Failures that used to be silent are said");
    preview_test::close_windows(r, t);
    send(t.main, CMD_RESET, 0);
    focus_target(t);
    pointer_left();

    // 1. Watching stops while the popup is open.
    if !t.open(r) {
        return r.check("open the popup", false, &t.describe());
    }
    crate::watch::test_break();
    let said = t.wait_state(crate::watch::POPUP_NOTICE, 3000);
    let w = super::watch_test::watch(t);
    r.check(
        "when watching the Media folder stops, the open popup says so in its footer",
        said && w.contains("running=false") && w.contains("problem=\"the self-test stopped it\""),
        &format!("{}; {w}", status_of(&t.describe())),
    );
    t.shot(r, "state-popup-watch-stopped");
    close_popup(r, t);
    let again = t.open(r) && status_of(&t.describe()) == crate::watch::POPUP_NOTICE;
    r.check("…and says it again each time it opens, while it lasts", again, &t.describe());
    if let Some(m) = open_manager(r, t) {
        let d = manager_describe(t);
        let line = status_of(&d);
        r.check(
            "…and the Manager's status line says what happened and what to do",
            line.contains("not following the Media folder (the self-test stopped it)") && line.contains("Exit in the tray menu"),
            &line,
        );
        shot_window(r, t, m, "state-manager-watch-stopped");
        post(m, WM_CLOSE, 0, 0);
        wait_until(1500, || !is_visible(m));
    }

    // 2. Watching cannot start (as at start-up, where a message box says it).
    let failed = send(t.main, CMD_WATCH_START_FAIL, 0) == 1;
    let message = DESCRIBE.lock().map(|d| d.clone()).unwrap_or_default();
    r.check(
        "when watching cannot start, the start-up message says what happened and what to do",
        failed && message.contains("could not watch its Media folder") && message.contains("started again") && message.contains("could not watch the Media folder:"),
        &message.replace('\n', " / "),
    );
    r.info("the self-test runs quietly, so that message is not shown as a box here; outside it, it is (like the shortcut's)");
    focus_target(t);
    let said = t.open(r) && status_of(&t.describe()) == crate::watch::POPUP_NOTICE;
    r.check("…and the popup says so too", said, &t.describe());
    close_popup(r, t);

    // 3. Watching again: the line goes.
    let resumed = send(t.main, CMD_WATCH_RESUME, 0) == 1;
    focus_target(t);
    let clear = t.open(r) && status_of(&t.describe()).is_empty();
    r.check("when watching runs again, the popup's line goes away", resumed && clear, &format!("{}; {}", t.describe(), super::watch_test::watch(t)));
    close_popup(r, t);

    // 4. A paste whose Recently used entry cannot be saved.
    focus_target(t);
    let formats = t.out.join("paste-formats.txt");
    let _ = std::fs::remove_file(&formats);
    crate::library::FAIL_SAVES.store(1, Ordering::SeqCst);
    if !t.open(r) {
        crate::library::FAIL_SAVES.store(0, Ordering::SeqCst);
        return false;
    }
    t.click(r, El::Search);
    if !t.select_all_and_type(r, "square") || !guard(r, &t.allowed) {
        crate::library::FAIL_SAVES.store(0, Ordering::SeqCst);
        return false;
    }
    press(&[VK_RETURN]);
    let closed = wait_until(3000, || !is_visible(t.popup()));
    let pasted = wait_until(3000, || formats.exists());
    let (kind, text) = status(t.main);
    crate::library::FAIL_SAVES.store(0, Ordering::SeqCst);
    r.check(
        "a paste whose Recently used entry cannot be saved still pastes",
        closed && pasted && kind == 2 && text.contains("could not be saved"),
        &text,
    );
    focus_target(t);
    let shown = t.open(r) && status_of(&t.describe()) == crate::paste::PASTE_UNSAVED;
    r.check("…and the popup says so in its footer the next time it opens", shown, &t.describe());
    t.shot(r, "state-popup-paste-not-saved");
    close_popup(r, t);
    let once = t.open(r) && status_of(&t.describe()).is_empty();
    r.check("…once", once, &t.describe());
    close_popup(r, t);

    // 5. A drag-out whose Recently used entry cannot be saved.
    focus_target(t);
    pointer_left();
    if t.open(r) {
        let received = t.out.join("drop-received.png");
        let _ = std::fs::remove_file(&received);
        crate::library::FAIL_SAVES.store(1, Ordering::SeqCst);
        if let Some(from) = t.point(El::Tile(0))
            && super::drag_to_target(r, t, from)
        {
            let got = wait_until(4000, || received.exists());
            let said = wait_until(1500, || status_of(&t.describe()) == crate::paste::DRAG_UNSAVED);
            let (_, text) = status(t.main);
            r.check(
                "a drag-out whose Recently used entry cannot be saved still drops the copy, and the popup says so",
                got && said && text.contains("could not be saved"),
                &format!("dropped {got}; {text}; {}", status_of(&t.describe())),
            );
            t.shot(r, "state-popup-drag-not-saved");
        }
        crate::library::FAIL_SAVES.store(0, Ordering::SeqCst);
        if is_visible(t.popup()) {
            send(t.main, CMD_FOCUS, t.popup());
            wait_until(1500, || foreground() == t.popup());
        }
        close_popup(r, t);
    }

    // 6. The start-up comparison found a file, but the library could not be saved.
    let shown = send(t.main, CMD_STARTUP_COMPARE, 0) == 1;
    let d = DESCRIBE.lock().map(|d| d.clone()).unwrap_or_default();
    let found = super::watch_test::find(t, &format!("Pictures/{FOUND_AT_START}"));
    let on_disk = recorded(t, &format!("Pictures/{FOUND_AT_START}"));
    r.check(
        "when the start-up comparison's result cannot be saved, a message says what happened and what to do",
        shown && d.contains("added 1 file(s)") && d.contains("could not be saved") && d.contains("Data folder"),
        &d,
    );
    r.check(
        "…and the found file is in the library while PocketMedia runs, though not yet in the library file",
        !found.contains("found=none") && on_disk == 0,
        &format!("{found}; in the library file: {on_disk}"),
    );
    send(t.main, CMD_RESET, 0);
    true
}

// ----------------------------------------------------------------

/// A Yes/No box of ours titled `title`: its window, its text and its default button.
fn question(title: &str) -> Option<(isize, String, usize)> {
    let me = std::process::id();
    let mut boxed = 0isize;
    if !wait_until(4000, || {
        boxed = find_window("#32770", Some(title), me);
        boxed != 0 && is_visible(boxed)
    }) {
        return None;
    }
    // Long enough for Windows to finish laying out and drawing the box.
    std::thread::sleep(Duration::from_millis(800));
    let text = longest_child_text(boxed);
    // SAFETY: SendMessageW to a dialog box of ours; DM_GETDEFID only reads its default button.
    let def = unsafe { SendMessageW(hwnd(boxed), DM_GETDEFID, Some(WPARAM(0)), Some(LPARAM(0))) }.0 as usize & 0xFFFF;
    Some((boxed, text, def))
}

/// Last in the script: Exit while an import copies. No keeps it running;
/// Yes stops it and exits. When no import can be started, the plain Exit check
/// runs instead, so the run still ends.
pub(super) fn exit_during_import(r: &mut Report, t: &Session) {
    r.info("Exit while an import copies");
    preview_test::close_windows(r, t);
    send(t.main, CMD_RESET, 0);
    let before = send(t.main, CMD_EXIT_QUESTION, 0);
    r.check("with no backup and no import running, Exit asks nothing", before == 0, &format!("question {before}"));
    let stem = "Exit test";
    let (folder, files) = source(&t.out, "exit-src", stem, 6);
    // 1.2 s before each copy: the import is still copying when Yes is chosen.
    jobs_delay(1200);
    focus_target(t);
    if !start_import(r, t, folder, files) {
        jobs_delay(0);
        super::exit_test(r, t.main);
        return;
    }
    std::thread::sleep(Duration::from_millis(800));
    let asks = send(t.main, CMD_EXIT_QUESTION, 0);
    let answer = super::uninstall_test::end_session_answer(t.main);
    let reason = super::uninstall_test::block_reason(t.main);
    r.check(
        "while an import copies, signing out or shutting down makes Windows ask first, with the reason \"An import is running. Stop it?…\"",
        asks == 2 && answer == 0 && reason == crate::add_panel::SHUTDOWN_REASON,
        &format!("Exit question {asks}; WM_QUERYENDSESSION answer {answer}; reason {reason:?}"),
    );

    // No: everything keeps running.
    post(t.main, crate::WM_SELFTEST, CMD_REQUEST_EXIT, 0);
    let asked = question(crate::STOP_IMPORT_TITLE);
    let (boxed, text, def) = asked.clone().unwrap_or_default();
    r.check(
        "Exit while an import copies asks first: \"An import is running. Stop it?\", with No as the default button",
        asked.is_some() && text.starts_with("An import is running. Stop it?") && def == IDNO.0 as usize,
        &format!("{}; default button {def} (No is {})", text.replace('\n', " / "), IDNO.0),
    );
    if asked.is_some() {
        save_capture(r, &t.out.join("screens").join("state-exit-import-running.png"), capture_window(boxed));
        post(boxed, WM_COMMAND, IDNO.0 as usize, 0);
    }
    let closed = wait_until(3000, || !is_visible(boxed));
    std::thread::sleep(Duration::from_millis(300));
    let running = state(t).contains("copying=true") && super::tray_icon_rect(t.main).is_some();
    r.check("…No keeps PocketMedia and the import running", closed && running, &format!("{}; {}", state(t), t.describe()));

    // Yes: the import stops cleanly and PocketMedia exits.
    let shortcut = super::unpack_shortcut(send(t.main, CMD_SHORTCUT, 0));
    let (copied_before, _) = copies(t, stem);
    post(t.main, crate::WM_SELFTEST, CMD_REQUEST_EXIT, 0);
    let Some((boxed, _, _)) = question(crate::STOP_IMPORT_TITLE) else {
        r.check("Exit asks again", false, "no box");
        jobs_delay(0);
        return;
    };
    post(boxed, WM_COMMAND, IDYES.0 as usize, 0);
    let gone = wait_until(15_000, || !exists(t.main));
    jobs_delay(0);
    r.check("…Yes stops the import and exits PocketMedia", gone, "");
    let (copied, partial) = copies(t, stem);
    let saved = recorded(t, &format!("Pictures/{stem}"));
    r.check(
        "…every file the import had copied is in the library file, the rest were not copied, and no temporary file is left",
        gone && copied >= copied_before && copied < 6 && copied == saved && partial == 0,
        &format!("{copied} of 6 copied ({copied_before} when Yes was chosen), {saved} in the library file, {partial} temporary files"),
    );
    super::after_exit(r, t.main, shortcut, gone);
}
