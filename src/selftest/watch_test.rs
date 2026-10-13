//! Self-test steps for watching the Media folders (changes made the way
//! File Explorer makes them, in the self-test's own `test-results\app\Media`),
//! Back up library (into `test-results\backup-dest`, then restored into
//! `test-results\restore` and started there), and the Autostart toggle (on the
//! self-test's own registry key). Single instance is in `selftest.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{VK_A, VK_CONTROL};

use super::preview_test::{close_windows, open_all_media};
use super::settings_test::Set;
use super::{
    CMD_MANAGER, CMD_RESET, CMD_SETTINGS_SCROLL, CMD_STORE_REOPEN, DESCRIBE, El, Report, Session, describe_foreground, foreground,
    guard, is_visible, press, send, type_unicode, wait_until,
};
use crate::ui::manager_view::MHit;
use crate::ui::settings_view::SHit;

/// Puts "items=N found=…" for the item whose path is in QUERY in DESCRIBE.
pub(super) const CMD_LIB_FIND: usize = 40;
/// Puts `watch::describe()` in DESCRIBE.
pub(super) const CMD_WATCH_DESCRIBE: usize = 41;
/// Restarts watching with an lparam-byte notification buffer.
pub(super) const CMD_WATCH_RESTART: usize = 42;

static QUERY: Mutex<String> = Mutex::new(String::new());

pub(super) fn on_ui_command(cmd: usize, arg: isize) -> isize {
    let text = match cmd {
        CMD_LIB_FIND => {
            let q = QUERY.lock().map(|q| q.clone()).unwrap_or_default();
            crate::library::with(|lib| {
                let found = lib.index_of_path(&q).map(|i| {
                    let it = &lib.items[i];
                    format!(
                        "{}:{}:{}:{}x{}:hash={}:recent={}",
                        it.id,
                        it.name,
                        it.size,
                        it.width,
                        it.height,
                        if it.hash.is_empty() { "empty" } else { "set" },
                        lib.recent.contains(&it.id)
                    )
                });
                format!("items={} found={}", lib.items.len(), found.unwrap_or_else(|| "none".into()))
            })
        }
        CMD_WATCH_DESCRIBE => crate::watch::describe(),
        CMD_WATCH_RESTART => {
            let started = crate::paths::get().map(|p| crate::watch::start(&p.media, arg as usize));
            format!("{started:?}")
        }
        _ => return 0,
    };
    if let Ok(mut d) = DESCRIBE.lock() {
        *d = text;
    }
    0
}

fn find(t: &Session, rel: &str) -> String {
    if let Ok(mut q) = QUERY.lock() {
        *q = rel.to_string();
    }
    send(t.main, CMD_LIB_FIND, 0);
    DESCRIBE.lock().map(|d| d.clone()).unwrap_or_default()
}

fn items(t: &Session) -> i64 {
    number(&find(t, ""), "items")
}

fn watch(t: &Session) -> String {
    send(t.main, CMD_WATCH_DESCRIBE, 0);
    DESCRIBE.lock().map(|d| d.clone()).unwrap_or_default()
}

fn number(d: &str, key: &str) -> i64 {
    d.split(&format!("{key}=")).nth(1).and_then(|v| v.split([' ', ':']).next()).and_then(|v| v.parse().ok()).unwrap_or(-1)
}

/// The id in "found=<id>:…", or None.
fn found_id(d: &str) -> Option<u32> {
    d.split("found=").nth(1)?.split(':').next()?.parse().ok()
}

fn media(t: &Session) -> PathBuf {
    t.out.join("app").join("Media")
}

/// Writes a file the way File Explorer copies one in: a new name, then its bytes.
fn put(path: &Path, bytes: &[u8]) -> bool {
    std::fs::write(path, bytes).is_ok()
}

pub(super) fn run(r: &mut Report, t: &Session) -> bool {
    r.info("Watching, backup, autostart");
    close_windows(r, t);
    send(t.main, CMD_RESET, 0);
    let ok = watch_tests(r, t);
    let ok = ok && backup_tests(r, t);
    let ok = ok && autostart_tests(r, t);
    crate::autostart::remove_test_key();
    ok
}

// ---------------------------------------------------------------- watching

fn watch_tests(r: &mut Report, t: &Session) -> bool {
    let m = media(t);
    std::thread::sleep(Duration::from_millis(500));
    let w = watch(t);
    r.check("the Media folders are watched", w.contains("running=true"), &w);

    // Idle means idle: the watcher sleeps until Windows reports a change.
    let (wakes, rescans) = (number(&w, "wakes"), number(&w, "rescans"));
    std::thread::sleep(Duration::from_millis(2500));
    let w = watch(t);
    r.check(
        "while nothing changes, watching does no work (no polling, no scans)",
        number(&w, "wakes") == wakes && number(&w, "rescans") == rescans,
        &format!("wakes {wakes} -> {}, rescans {rescans} -> {}", number(&w, "wakes"), number(&w, "rescans")),
    );

    // 1. A file copied in from outside joins the library, and the popup shows it.
    let before = items(t);
    let added = m.join("Pictures").join("From Explorer.png");
    put(&added, &super::seed::png(64, 48, 201));
    let mut d = String::new();
    let seen = wait_until(5000, || {
        d = find(t, "Pictures/From Explorer.png");
        found_id(&d).is_some()
    });
    r.check(
        "a file copied into Media\\Pictures joins the library",
        seen && d.contains(":From Explorer.png:") && d.contains(":64x48:") && number(&d, "items") == before + 1,
        &d,
    );
    if open_all_media(r, t) {
        t.state_has(r, "…and the popup shows it at once", &["From Explorer.png"]);
        t.shot(r, "phase6-watch-added");
    }
    close_windows(r, t);

    // 2. An outside rename keeps the item (id and recent history).
    let square = find(t, "Pictures/Square.png");
    let id = found_id(&square);
    let renamed = std::fs::rename(m.join("Pictures").join("Square.png"), m.join("Pictures").join("Square renamed.png")).is_ok();
    let mut d = String::new();
    let seen = wait_until(5000, || {
        d = find(t, "Pictures/Square renamed.png");
        found_id(&d).is_some()
    });
    r.check(
        "a file renamed in Explorer keeps its place in the library (same id, still in Recently used)",
        renamed && seen && found_id(&d) == id && d.contains("recent=true") && number(&d, "items") == before + 1,
        &format!("before: {square} / after: {d}"),
    );

    // 3. Replaced with different content: same item, new size, hash no longer trusted.
    let tmp = m.join("Pictures").join("replacement.tmp");
    let replaced = put(&tmp, &super::seed::png(32, 40, 202)) && std::fs::rename(&tmp, m.join("Pictures").join("Square renamed.png")).is_ok();
    let mut d = String::new();
    let seen = wait_until(5000, || {
        d = find(t, "Pictures/Square renamed.png");
        d.contains(":32x40:")
    });
    r.check(
        "a file replaced in Explorer keeps its id and gets its new size",
        replaced && seen && found_id(&d) == id && d.contains("hash=empty") && number(&d, "items") == before + 1,
        &d,
    );

    // 4. Deleted outside: the item leaves the library and its history.
    let hello = found_id(&find(t, "GIFs/Hello.gif"));
    let deleted = std::fs::remove_file(m.join("GIFs").join("Hello.gif")).is_ok();
    let gone = wait_until(5000, || found_id(&find(t, "GIFs/Hello.gif")).is_none());
    r.check(
        "a file deleted in Explorer leaves the library",
        hello.is_some() && deleted && gone && items(t) == before,
        &find(t, "GIFs/Hello.gif"),
    );

    // 5. Files that are not media, temporary import files and subfolders are ignored.
    let count = items(t);
    let _ = std::fs::create_dir_all(m.join("Pictures").join("sub"));
    let ignored = [
        m.join("Pictures").join("notes.txt"),
        m.join("Pictures").join(".pocketmedia-99-1.partial"),
        m.join("Pictures").join("sub").join("deep.png"),
        m.join("loose.png"),
    ];
    for f in &ignored {
        put(f, &super::seed::png(8, 8, 203));
    }
    std::thread::sleep(Duration::from_millis(2000));
    r.check("unsupported files, temporary files and subfolders are ignored", items(t) == count, &watch(t));
    for f in &ignored {
        let _ = std::fs::remove_file(f);
    }
    let _ = std::fs::remove_dir(m.join("Pictures").join("sub"));

    // 6. Many files at once arrive in a few coalesced batches.
    let w0 = watch(t);
    let burst: Vec<PathBuf> = (0..300).map(|i| m.join("Pictures").join(format!("Burst {i:03}.png"))).collect();
    let src = t.out.join("phase6-burst.png");
    put(&src, &super::seed::png(16, 16, 204));
    for f in &burst {
        let _ = std::fs::copy(&src, f);
    }
    let all = wait_until(15_000, || items(t) == count + 300);
    let w1 = watch(t);
    let batches = number(&w1, "batches") - number(&w0, "batches");
    r.check("300 files copied in at once all join the library", all, &format!("items {} (want {})", items(t), count + 300));
    r.check(
        "…in a few coalesced batches, not one update per file",
        all && (1..=30).contains(&batches),
        &format!("{batches} batches, {} events", number(&w1, "events") - number(&w0, "events")),
    );
    r.info(&format!("watch after the burst: {w1}"));

    // 7. PocketMedia's own changes are not counted twice: Rename in the Manager.
    let own_ok = own_rename(r, t, count + 300);
    if !own_ok {
        return false;
    }

    // 8. Lost events (a tiny notification buffer forces an overflow): one bounded
    //    comparison of the folders puts everything right.
    send(t.main, CMD_WATCH_RESTART, 256);
    let w0 = watch(t);
    let start = items(t);
    // (One of them may have been renamed by the Manager above.)
    let gone = burst[..150].iter().filter(|f| std::fs::remove_file(f).is_ok()).count() as i64;
    let settled = wait_until(15_000, || items(t) == start - gone);
    let w1 = watch(t);
    let (overflows, rescans) = (number(&w1, "overflows") - number(&w0, "overflows"), number(&w1, "rescans") - number(&w0, "rescans"));
    if overflows > 0 {
        r.check(
            "after Windows drops events (overflow), a full comparison brings the library up to date",
            settled && rescans >= 1,
            &format!("{overflows} overflow(s), {rescans} comparison(s), items {} (want {})", items(t), start - gone),
        );
    } else {
        r.skip("overflow recovery", &format!("Windows did not overflow a 256-byte buffer this time; items {} (want {})", items(t), start - gone));
        r.check("deleting 150 files at once leaves the library right", settled, &w1);
    }
    send(t.main, CMD_WATCH_RESTART, crate::watch::BUFFER as isize);

    // 9. A category folder deleted in Explorer: it comes back, and its items leave.
    let videos = m.join("Videos");
    let before = items(t);
    let n_videos = std::fs::read_dir(&videos).map(|d| d.count() as i64).unwrap_or(0);
    let removed = std::fs::remove_dir_all(&videos).is_ok();
    let back = wait_until(5000, || videos.is_dir() && items(t) == before - n_videos);
    r.check(
        "a deleted Videos folder is made again, and its items leave the library",
        removed && n_videos > 0 && back,
        &format!("videos {n_videos}, items {} (want {}), {}", items(t), before - n_videos, watch(t)),
    );
    true
}

/// Rename the selected item in the Manager; the watcher sees the rename too and
/// must change nothing more.
fn own_rename(r: &mut Report, t: &Session, expect_items: i64) -> bool {
    let Some((s, _)) = open_manager(r, t) else { return false };
    page(r, &s, false);
    s.manager_click(r, MHit::Tile(0));
    let d = s.describe();
    let selected = d.split("selected=\"").nth(1).and_then(|v| v.split('"').next()).unwrap_or("").to_string();
    s.manager_click(r, MHit::RenameInput);
    if !guard(r, &t.allowed) {
        return false;
    }
    press(&[VK_CONTROL, VK_A]);
    type_unicode("Renamed by PocketMedia");
    std::thread::sleep(Duration::from_millis(200));
    s.manager_click(r, MHit::Rename);
    let ext = crate::library::extension(&selected).to_string();
    let folder = match crate::media_info::kind_for(&selected) {
        Some(crate::sample::MediaKind::Gif) => "GIFs",
        Some(crate::sample::MediaKind::Video) => "Videos",
        _ => "Pictures",
    };
    let rel = format!("{folder}/Renamed by PocketMedia{ext}");
    let id_after = found_id(&find(t, &rel));
    std::thread::sleep(Duration::from_millis(1500)); // the watcher's batch arrives meanwhile
    let d = find(t, &rel);
    r.check(
        "a Rename in the Manager is not counted again by the watcher",
        id_after.is_some() && found_id(&d) == id_after && number(&d, "items") == expect_items,
        &format!("renamed {selected:?}: {d}"),
    );
    close_windows(r, t);
    true
}

/// Popup -> Open manager. Returns the Settings driver (the Manager shows the Library page).
fn open_manager<'a>(r: &mut Report, t: &'a Session) -> Option<(Set<'a>, isize)> {
    if !t.open(r) || !t.click(r, El::OpenManager) {
        return None;
    }
    let hwnd = send(t.main, CMD_MANAGER, 0);
    let shown = wait_until(2000, || hwnd != 0 && is_visible(hwnd) && foreground() == hwnd);
    if !r.check("the Manager opens", shown, &describe_foreground()) {
        return None;
    }
    Some((Set { t, hwnd }, hwnd))
}

/// Shows the Settings page (true) or the Library page (false) of the Manager.
fn page(r: &mut Report, s: &Set, settings: bool) {
    let on_settings = s.describe().starts_with("screen=settings");
    match (settings, on_settings) {
        (true, false) => {
            s.manager_click(r, MHit::Nav(4));
        }
        (false, true) => {
            s.click(r, SHit::Nav(0));
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- backup

fn note_has(s: &Set, needle: &str, ms: u64) -> bool {
    wait_until(ms, || s.describe().contains(needle))
}

fn backup_tests(r: &mut Report, t: &Session) -> bool {
    let dest = t.out.join("backup-dest");
    let _ = std::fs::remove_dir_all(&dest);
    let _ = std::fs::create_dir_all(&dest);
    let Some((s, _)) = open_manager(r, t) else { return false };
    page(r, &s, true);
    s.scroll_to(100_000);
    s.park();
    s.shot(r, "phase6-settings-backup");

    // The picker cancelled.
    crate::dialogs::set_test_backup(None);
    s.click(r, SHit::Backup);
    s.has(r, "Back up library: closing the folder picker is reported", &["Backup cancelled: no folder was chosen."]);

    // Inside PocketMedia's folder: refused, nothing written.
    let gifs = media(t).join("GIFs");
    let gif_count = std::fs::read_dir(&gifs).map(|d| d.count()).unwrap_or(0);
    for (what, folder) in [("the Media\\GIFs folder", gifs.clone()), ("PocketMedia's own folder", t.out.join("app"))] {
        crate::dialogs::set_test_backup(Some(folder));
        s.click(r, SHit::Backup);
        let refused = note_has(&s, "Choose a folder outside PocketMedia", 5000);
        r.check(&format!("a backup into {what} is refused"), refused && !s.describe().contains("backup_running=true"), &s.describe());
    }
    r.check("…and nothing was written there", std::fs::read_dir(&gifs).map(|d| d.count()).unwrap_or(0) == gif_count, "");

    // A destination that cannot be used: the failure is reported.
    let not_folder = t.out.join("phase6-not-a-folder.txt");
    put(&not_folder, b"a file");
    crate::dialogs::set_test_backup(Some(not_folder));
    s.click(r, SHit::Backup);
    r.check("a backup that cannot be made reports the failure", note_has(&s, "Backup failed:", 5000), &s.describe());

    // Cancel while copying (each file is slowed down so there is time to press it).
    crate::dialogs::set_test_backup(Some(dest.clone()));
    crate::backup::TEST_DELAY_MS.store(60, Ordering::SeqCst);
    s.click(r, SHit::Backup);
    let running = note_has(&s, "Backing up", 3000) && s.describe().contains("backup_running=true");
    r.check("Back up library runs and shows its progress; the button becomes Cancel backup", running, &s.describe());
    s.shot(r, "phase6-settings-backup-running");
    if running {
        // PocketMedia's own changes wait: Choose folder… (import) is refused while it runs.
        s.click(r, SHit::ChooseFolder);
        let d = s.describe();
        r.check("an import cannot start during a backup", d.contains(crate::backup::BUSY), &d);
        // The refusal leaves the Add media start page; ← goes back to Settings.
        s.add_click(r, El::AddBack);
        // …and so does Rename on the Library page.
        page(r, &s, false);
        s.manager_click(r, MHit::Tile(0));
        s.manager_click(r, MHit::RenameInput);
        if guard(r, &t.allowed) {
            press(&[VK_CONTROL, VK_A]);
            type_unicode("Not now");
            std::thread::sleep(Duration::from_millis(200));
        }
        s.manager_click(r, MHit::Rename);
        let d = s.describe();
        r.check("a Rename waits for the backup", d.contains(crate::backup::BUSY), &d);
        page(r, &s, true);
        s.scroll_to(100_000);
        r.check("Settings still shows the running backup", s.describe().contains("backup_running=true"), &s.describe());
        s.click(r, SHit::Backup);
        let cancelled = note_has(&s, "Backup cancelled", 10_000);
        let left = std::fs::read_dir(&dest).map(|d| d.count()).unwrap_or(99);
        r.check("Cancel backup stops it and removes the unfinished copy", cancelled && left == 0, &format!("{} / {left} left", s.describe()));
    }
    crate::backup::TEST_DELAY_MS.store(0, Ordering::SeqCst);

    // A full backup.
    s.click(r, SHit::Backup);
    let done = note_has(&s, "Backup finished and checked", 60_000);
    r.check("Back up library finishes and checks every copy", done, &s.describe());
    s.park();
    s.shot(r, "phase6-settings-backup-finished");
    close_windows(r, t);
    if done { backup_contents(r, t, &dest) } else { true }
}

/// The backup holds every library file, byte for byte, and a library file that
/// lists exactly them; restored into another folder, PocketMedia finds them all.
fn backup_contents(r: &mut Report, t: &Session, dest: &Path) -> bool {
    let Some(folder) = std::fs::read_dir(dest).ok().and_then(|d| d.flatten().map(|e| e.path()).find(|p| p.is_dir())) else {
        return r.check("the backup folder exists", false, &dest.display().to_string());
    };
    r.info(&format!("backup folder: {}", folder.display()));
    let json = std::fs::read_to_string(folder.join("Data").join(crate::store::FILE_NAME)).unwrap_or_default();
    let data = crate::store::check_text(&json);
    let library = items(t);
    let Ok(data) = data else {
        return r.check("the backup's library file loads", false, &format!("{data:?}"));
    };
    r.check(
        "the backup's library file lists every library item",
        data.items.len() as i64 == library,
        &format!("{} in the backup, {library} in the library", data.items.len()),
    );
    let app_media = media(t);
    let mismatched: Vec<&str> = data
        .items
        .iter()
        .filter(|i| {
            let a = std::fs::read(crate::import::media_file(&app_media, &i.path)).ok();
            let b = std::fs::read(crate::import::media_file(&folder.join("Media"), &i.path)).ok();
            a.is_none() || a != b
        })
        .map(|i| i.name.as_str())
        .collect();
    r.check("every backed-up file matches the original byte for byte", mismatched.is_empty(), &format!("{mismatched:?}"));
    let files_in_backup: usize = crate::import::FOLDERS
        .iter()
        .filter_map(|f| std::fs::read_dir(folder.join("Media").join(f)).ok())
        .map(|d| d.count())
        .sum();
    r.check("the backup holds no other media files", files_in_backup == data.items.len(), &files_in_backup.to_string());
    r.check(
        "caches are not backed up; the restore note is there",
        !folder.join("Cache").exists() && folder.join("How to restore.txt").is_file(),
        "",
    );

    // Restore into another folder and start PocketMedia there.
    let restore = t.out.join("restore");
    let _ = std::fs::remove_dir_all(&restore);
    let exe = restore.join("pocketmedia.exe");
    let copied = std::fs::create_dir_all(&restore).is_ok()
        && std::env::current_exe().is_ok_and(|e| std::fs::copy(e, &exe).is_ok())
        && copy_tree(&folder.join("Media"), &restore.join("Media"))
        && copy_tree(&folder.join("Data"), &restore.join("Data"));
    if !r.check("the backup is copied into another PocketMedia folder", copied, &restore.display().to_string()) {
        return true;
    }
    let report = restore.join("check-library.txt");
    let status = Command::new(&exe).arg("--check-library").arg(&report).status();
    let text = std::fs::read_to_string(&report).unwrap_or_default();
    let names: Vec<&str> = text.lines().find_map(|l| l.strip_prefix("names=")).map(|n| n.split('|').collect()).unwrap_or_default();
    let all_named = data.items.iter().all(|i| names.contains(&i.name.as_str()));
    r.check(
        "restored into another folder, PocketMedia loads every item and finds every file",
        status.is_ok_and(|s| s.success())
            && text.contains("store=loaded")
            && text.contains(&format!("items={}\n", data.items.len()))
            && text.contains("missing=\n")
            && all_named,
        &text.replace('\n', " "),
    );
    true
}

fn copy_tree(from: &Path, to: &Path) -> bool {
    if std::fs::create_dir_all(to).is_err() {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(from) else { return false };
    entries.flatten().all(|e| {
        let target = to.join(e.file_name());
        if e.path().is_dir() { copy_tree(&e.path(), &target) } else { std::fs::copy(e.path(), target).is_ok() }
    })
}

// ---------------------------------------------------------------- autostart

fn autostart_tests(r: &mut Report, t: &Session) -> bool {
    let Ok(command) = crate::autostart::command() else {
        return r.check("the program's location is known", false, "");
    };
    let Some((s, _)) = open_manager(r, t) else { return false };
    page(r, &s, true);
    s.scroll_to(100_000);
    s.has(r, "Autostart is off when Windows has no entry", &["autostart=false", "dirty=false"]);
    s.click(r, SHit::Autostart);
    s.click(r, SHit::Save);
    let value = crate::autostart::read();
    r.check(
        "Autostart on + Save adds the entry that starts this program quietly (--autostart)",
        value.as_deref() == Some(command.as_str()) && command.ends_with("\" --autostart"),
        &format!("{value:?}"),
    );
    s.has(r, "…and Settings shows it saved", &["saved=", "/true dirty=false"]);
    s.click(r, SHit::Autostart);
    s.click(r, SHit::Save);
    let value = crate::autostart::read();
    r.check("Autostart off + Save removes the entry", value.is_none(), &format!("{value:?}"));

    // Settings shows what Windows has, not what the library file remembers.
    let set = crate::autostart::set(true);
    send(t.main, CMD_STORE_REOPEN, 0);
    s.scroll_to(100_000);
    s.has(r, "Settings follows the entry Windows has (on)", &["autostart=true"]);
    let unset = crate::autostart::set(false);
    send(t.main, CMD_STORE_REOPEN, 0);
    s.has(r, "Settings follows the entry Windows has (off)", &["autostart=false"]);
    r.check("the entry can be added and removed", set.is_ok() && unset.is_ok(), &format!("{set:?} {unset:?}"));
    send(t.main, CMD_SETTINGS_SCROLL, 0);
    close_windows(r, t);
    true
}
