//! Back up library (Settings): copies the originals in Media and a
//! consistent library file to a folder the user picks, on a worker thread.
//!
//! - The library file in the backup is written from a snapshot taken when the
//!   backup starts, and lists exactly the files that were copied, so the two
//!   always agree. While it runs, PocketMedia's own changes (import, Rename,
//!   Move to Recycle Bin) wait; a file removed outside PocketMedia meanwhile is
//!   left out and reported.
//! - Every copy is read back and compared (SHA-256) before the backup counts as
//!   finished. Nothing is ever overwritten: the backup goes into a new folder,
//!   `PocketMedia backup <date> <time>`.
//! - A destination inside PocketMedia's own folder is refused.
//! - On failure or Cancel, the files and folders this backup made are removed;
//!   nothing else is touched. Caches are not backed up; they are made again.
//! - Restoring: copy the backup's Media and Data folders beside pocketmedia.exe
//!   in any folder. Library paths are relative to Media, so another install
//!   path works (`How to restore.txt` says so in the backup).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::digest::{Digest, NewDigest};
use crate::import::{FOLDERS, display_path, media_file};
use crate::jobs::{self, Event, Job};
use crate::store::{FILE_NAME, StoreData};

pub const BUSY: &str = "A backup is running. Try again when it has finished.";
const CHUNK: usize = 1 << 20;
const RESTORE_NOTE: &str = "How to restore.txt";

#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    Copying { done: usize, total: usize },
    Finished { folder: PathBuf, files: usize, bytes: u64, left_out: Vec<String> },
    Failed(String),
    Cancelled,
}

/// Everything a backup needs, captured on the UI thread.
pub struct Plan {
    /// PocketMedia's folder (Media, Data and Cache are in it). The backup may not go inside it.
    pub root: PathBuf,
    pub media: PathBuf,
    /// The library as it is when the backup starts.
    pub data: StoreData,
    /// The folder the user picked; the backup goes into a new folder inside it.
    pub dest: PathBuf,
    /// For the new folder's name: `2026-10-13 1620`.
    pub stamp: String,
}

/// True when `child` is `parent` or inside it (letter case ignored, as on Windows).
fn inside(child: &Path, parent: &Path) -> bool {
    let parts = |p: &Path| -> Vec<String> { p.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect() };
    parts(child).starts_with(&parts(parent))
}

/// The real folder for `dest` (links resolved), refused when it is inside `root`.
pub fn check_destination(dest: &Path, root: &Path) -> Result<PathBuf, String> {
    let shown = display_path(dest);
    let real = fs::canonicalize(dest).map_err(|e| format!("{shown} could not be opened: {e}"))?;
    if !real.is_dir() {
        return Err(format!("{shown} is not a folder."));
    }
    let root = fs::canonicalize(root).map_err(|e| format!("PocketMedia's folder could not be checked: {e}"))?;
    if inside(&real, &root) {
        return Err(format!("Choose a folder outside PocketMedia. A backup cannot go inside {}.", display_path(&root)));
    }
    Ok(real)
}

/// What this backup made, so a failed or cancelled one can be removed.
#[derive(Default)]
struct Made {
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
}

impl Made {
    fn dir(&mut self, path: PathBuf) -> io::Result<()> {
        fs::create_dir(&path)?;
        self.dirs.push(path);
        Ok(())
    }

    /// Removes what this backup made, newest first. Returns what could not be removed.
    fn remove(self) -> Vec<PathBuf> {
        let mut left = Vec::new();
        for f in self.files.iter().rev() {
            if fs::remove_file(f).is_err() && f.exists() {
                left.push(f.clone());
            }
        }
        // remove_dir only removes an empty folder, so nothing else can go.
        for d in self.dirs.iter().rev() {
            if fs::remove_dir(d).is_err() && d.exists() {
                left.push(d.clone());
            }
        }
        left
    }
}

enum Stop {
    Cancelled,
    Failed(String),
}

fn check(cancel: &AtomicBool) -> Result<(), Stop> {
    if cancel.load(Ordering::Relaxed) { Err(Stop::Cancelled) } else { Ok(()) }
}

/// Runs a backup. `progress(done, total)` is called as files are copied.
pub fn run(plan: &Plan, digest: NewDigest, cancel: &AtomicBool, progress: &mut dyn FnMut(usize, usize)) -> Progress {
    let dest = match check_destination(&plan.dest, &plan.root) {
        Ok(d) => d,
        Err(e) => return Progress::Failed(e),
    };
    let mut made = Made::default();
    let result = copy_all(plan, &dest, digest, cancel, progress, &mut made);
    match result {
        Ok(done) => done,
        Err(stop) => {
            let left = made.remove();
            let mut outcome = match stop {
                Stop::Cancelled => Progress::Cancelled,
                Stop::Failed(e) => Progress::Failed(e),
            };
            if !left.is_empty() {
                let note = format!(" The unfinished backup could not be removed completely: {}.", display_path(&left[0]));
                outcome = match outcome {
                    Progress::Failed(e) => Progress::Failed(e + &note),
                    _ => Progress::Failed(format!("The backup was cancelled.{note}")),
                };
            }
            outcome
        }
    }
}

fn copy_all(
    plan: &Plan,
    dest: &Path,
    digest: NewDigest,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize),
    made: &mut Made,
) -> Result<Progress, Stop> {
    let failed = |what: &str, e: &dyn std::fmt::Display| Stop::Failed(format!("{what}: {e}"));
    // A new folder, never one that exists.
    let mut folder = dest.join(format!("PocketMedia backup {}", plan.stamp));
    let mut n = 2;
    loop {
        match made.dir(folder.clone()) {
            Ok(()) => break,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && n < 100 => {
                folder = dest.join(format!("PocketMedia backup {} ({n})", plan.stamp));
                n += 1;
            }
            Err(e) => return Err(failed(&format!("the backup folder could not be made in {}", display_path(dest)), &e)),
        }
    }
    let media = folder.join("Media");
    let data = folder.join("Data");
    for dir in [media.clone()].into_iter().chain(FOLDERS.iter().map(|f| media.join(f))).chain([data.clone()]) {
        made.dir(dir).map_err(|e| failed("the backup folders could not be made", &e))?;
    }

    let mut kept = plan.data.clone();
    let total = kept.items.len();
    let mut left_out = Vec::new();
    let mut bytes = 0u64;
    let mut copied = Vec::with_capacity(total);
    for (i, item) in plan.data.items.iter().enumerate() {
        check(cancel)?;
        progress(i, total);
        let src = media_file(&plan.media, &item.path);
        let dst = media_file(&media, &item.path);
        let delay = TEST_DELAY_MS.load(Ordering::Relaxed);
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        match copy_verified(&src, &dst, digest, cancel, made)? {
            None => left_out.push(item.name.clone()),
            Some((size, hash)) => {
                bytes += size;
                copied.push((item.id, size, hash));
            }
        }
    }
    progress(total, total);

    // The library file lists exactly what was copied, with the sizes and hashes just checked.
    kept.items.retain(|it| copied.iter().any(|(id, _, _)| *id == it.id));
    for it in &mut kept.items {
        if let Some((_, size, hash)) = copied.iter().find(|(id, _, _)| *id == it.id) {
            it.size = *size;
            it.hash = hash.clone();
        }
    }
    kept.history.retain(|id| kept.items.iter().any(|it| it.id == *id));
    let text = serde_json::to_string_pretty(&kept).map_err(|e| failed("the library file could not be written", &e))?;
    let json = data.join(FILE_NAME);
    write_new(&json, text.as_bytes(), made).map_err(|e| failed("the library file could not be written", &e))?;
    let back = fs::read_to_string(&json).map_err(|e| failed("the library file could not be read back", &e))?;
    match crate::store::check_text(&back) {
        Ok(read) if read == kept => {}
        Ok(_) => return Err(Stop::Failed("the library file read back differently".into())),
        Err(e) => return Err(failed("the library file could not be read back", &e)),
    }
    let note = restore_note(&plan.stamp);
    write_new(&folder.join(RESTORE_NOTE), note.as_bytes(), made).map_err(|e| failed("the backup could not be finished", &e))?;
    check(cancel)?;
    Ok(Progress::Finished { folder, files: copied.len(), bytes, left_out })
}

fn write_new(path: &Path, bytes: &[u8], made: &mut Made) -> io::Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
    made.files.push(path.to_path_buf());
    f.write_all(bytes)?;
    f.sync_all()
}

fn hash_of(path: &Path, digest: NewDigest, cancel: &AtomicBool) -> Result<String, Stop> {
    let mut f = File::open(path).map_err(|e| Stop::Failed(format!("{} could not be read back: {e}", display_path(path))))?;
    let mut d: Box<dyn Digest> = digest().map_err(Stop::Failed)?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        check(cancel)?;
        let n = f.read(&mut buf).map_err(|e| Stop::Failed(format!("{} could not be read back: {e}", display_path(path))))?;
        if n == 0 {
            return Ok(d.finish());
        }
        d.update(&buf[..n]);
    }
}

/// Copies one file and checks the copy. None: the source is gone (removed
/// outside PocketMedia during the backup).
fn copy_verified(src: &Path, dst: &Path, digest: NewDigest, cancel: &AtomicBool, made: &mut Made) -> Result<Option<(u64, String)>, Stop> {
    let name = display_path(src);
    let mut from = match File::open(src) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Stop::Failed(format!("{name} could not be read: {e}"))),
    };
    let write_err = |e: io::Error| Stop::Failed(format!("{} could not be written: {e}", display_path(dst)));
    let mut to = OpenOptions::new().write(true).create_new(true).open(dst).map_err(write_err)?;
    made.files.push(dst.to_path_buf());
    let mut d: Box<dyn Digest> = digest().map_err(Stop::Failed)?;
    let mut buf = vec![0u8; CHUNK];
    let mut size = 0u64;
    loop {
        check(cancel)?;
        let n = from.read(&mut buf).map_err(|e| Stop::Failed(format!("{name} could not be read: {e}")))?;
        if n == 0 {
            break;
        }
        to.write_all(&buf[..n]).map_err(write_err)?;
        d.update(&buf[..n]);
        size += n as u64;
    }
    to.sync_all().map_err(write_err)?;
    drop(to);
    let hash = d.finish();
    if hash_of(dst, digest, cancel)? != hash {
        return Err(Stop::Failed(format!("the copy of {name} does not match the original")));
    }
    Ok(Some((size, hash)))
}

fn restore_note(stamp: &str) -> String {
    format!(
        "PocketMedia backup made on {stamp}.\r\n\r\n\
         It holds your pictures, GIFs and videos (Media) and your library and settings (Data).\r\n\r\n\
         To restore it:\r\n\
         1. Exit PocketMedia: right-click its tray icon and choose Exit.\r\n\
         2. Copy the Media and Data folders from here into the folder that holds pocketmedia.exe, \
         in place of the Media and Data folders there. Any PocketMedia folder works, not only the original one.\r\n\
         3. Start PocketMedia.\r\n\r\n\
         Thumbnails are not in the backup; PocketMedia makes them again as you browse.\r\n"
    )
}

/// `1.2 GB`, `340 KB`.
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} bytes") } else { format!("{v:.1} {}", UNITS[u]) }
}

/// The line Settings shows for a backup event.
pub fn describe(p: &Progress) -> String {
    match p {
        Progress::Copying { done, total } => format!("Backing up… {done} of {total} files"),
        Progress::Finished { folder, files, bytes, left_out } => {
            let name = folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let mut s = format!("Backup finished and checked: {files} files ({}) in \"{name}\".", size_text(*bytes));
            if !left_out.is_empty() {
                s += &format!(" {} file(s) left the Media folder during the backup and are not in it.", left_out.len());
            }
            s
        }
        Progress::Failed(e) => format!("Backup failed: {e}"),
        Progress::Cancelled => "Backup cancelled. The unfinished backup was removed.".into(),
    }
}

// ---------------------------------------------------------------- UI thread

static JOB: Mutex<Option<Job>> = Mutex::new(None);
/// Self-test only: a pause after each file, so Cancel backup can be pressed mid-way.
pub static TEST_DELAY_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Set while the worker runs (cleared after it has cleaned up).
static WORKING: AtomicBool = AtomicBool::new(false);

/// A backup is running: PocketMedia's own changes wait.
pub fn running() -> bool {
    JOB.lock().map(|j| j.is_some()).unwrap_or(false)
}

/// `2026-10-13 1620`, local time.
fn stamp() -> String {
    // SAFETY: GetLocalTime has no preconditions and returns a plain struct.
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute)
}

/// Starts a backup into `dest`. Its progress and result arrive as `Event::Backup`.
pub fn start(dest: PathBuf) -> Result<(), String> {
    if running() {
        return Err(BUSY.into());
    }
    let p = crate::paths::get().ok_or("the PocketMedia folders are not set up")?;
    // The library file's view of the library, as last saved (persist runs after every change).
    let data = crate::store::with(|s| s.data.clone()).ok_or("the library is not loaded")?;
    let plan = Plan { root: p.root.clone(), media: p.media.clone(), data, dest, stamp: stamp() };
    WORKING.store(true, Ordering::SeqCst);
    let job = jobs::spawn("backup", move |job, cancel| {
        let mut last = Instant::now() - Duration::from_secs(1);
        let mut report = |done: usize, total: usize| {
            if last.elapsed() >= Duration::from_millis(100) || done == total {
                last = Instant::now();
                jobs::post(Event::Backup { job, progress: Progress::Copying { done, total } });
            }
        };
        let result = run(&plan, crate::sha256::new, cancel, &mut report);
        WORKING.store(false, Ordering::SeqCst);
        jobs::post(Event::Backup { job, progress: result });
    })
    .inspect_err(|_| WORKING.store(false, Ordering::SeqCst))?;
    if let Ok(mut j) = JOB.lock() {
        *j = Some(job);
    }
    Ok(())
}

/// Cancel backup (the button while it runs). The result arrives as an event.
pub fn cancel() {
    if let Ok(j) = JOB.lock()
        && let Some(job) = j.as_ref()
    {
        job.cancel();
    }
}

/// A backup event (WM_JOB): updates Settings. Returns false for other events.
pub fn on_event(ev: &Event) -> bool {
    let Event::Backup { job, progress } = ev else { return false };
    let current = JOB.lock().ok().and_then(|j| j.as_ref().map(|j| j.id));
    if current != Some(*job) {
        return true; // from a backup that already reported its end
    }
    let done = !matches!(progress, Progress::Copying { .. });
    if done && let Ok(mut j) = JOB.lock() {
        *j = None;
    }
    if let Progress::Finished { folder, left_out, .. } = progress {
        crate::selftest::note(&format!("backup folder: {} (left out: {left_out:?})", display_path(folder)));
    }
    crate::manager::backup_changed(&describe(progress), !done);
    true
}

/// Exit: a running backup is cancelled, and its worker gets a moment to remove
/// the unfinished copy.
pub fn shutdown() {
    cancel();
    let end = Instant::now() + Duration::from_secs(3);
    while WORKING.load(Ordering::SeqCst) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::testing::fnv;
    use crate::store::StoredItem;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pm-backup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn item(id: u32, rel: &str) -> StoredItem {
        let name = rel.rsplit_once('/').unwrap().1.to_string();
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "kind": "picture", "path": rel, "size": 0, "hash": "",
            "width": 1, "height": 1, "added": 5
        }))
        .unwrap()
    }

    /// A PocketMedia folder with three media files and a library naming them.
    fn library(root: &Path) -> Plan {
        let media = root.join("app").join("Media");
        for f in FOLDERS {
            fs::create_dir_all(media.join(f)).unwrap();
        }
        fs::create_dir_all(root.join("app").join("Data")).unwrap();
        fs::write(media.join("Pictures/a.png"), b"aaaa").unwrap();
        fs::write(media.join("GIFs/b.gif"), vec![7u8; 3 * CHUNK + 5]).unwrap();
        fs::write(media.join("Videos/c.mp4"), b"cc").unwrap();
        let data = StoreData {
            items: vec![item(1, "Pictures/a.png"), item(2, "GIFs/b.gif"), item(3, "Videos/c.mp4")],
            history: vec![3, 1],
            next_id: 4,
            ..StoreData::default()
        };
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();
        Plan { root: root.join("app"), media, data, dest, stamp: "2026-10-13 1620".into() }
    }

    fn run_plan(plan: &Plan) -> (Progress, Vec<(usize, usize)>) {
        let mut seen = Vec::new();
        let p = run(plan, fnv, &AtomicBool::new(false), &mut |d, t| seen.push((d, t)));
        (p, seen)
    }

    #[test]
    fn copies_media_and_a_matching_library_file() {
        let root = temp("full");
        let plan = library(&root);
        let (p, seen) = run_plan(&plan);
        let Progress::Finished { folder, files, bytes, left_out } = p else { panic!("{p:?}") };
        assert_eq!(folder, fs::canonicalize(&plan.dest).unwrap().join("PocketMedia backup 2026-10-13 1620"));
        assert_eq!((files, bytes, left_out.len()), (3, 4 + 3 * CHUNK as u64 + 5 + 2, 0));
        assert_eq!(seen.last(), Some(&(3, 3)));
        for rel in ["Pictures/a.png", "GIFs/b.gif", "Videos/c.mp4"] {
            assert_eq!(fs::read(media_file(&folder.join("Media"), rel)).unwrap(), fs::read(media_file(&plan.media, rel)).unwrap(), "{rel}");
        }
        let data = crate::store::check_text(&fs::read_to_string(folder.join("Data").join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(data.items.len(), 3);
        assert_eq!(data.history, vec![3, 1]);
        assert_eq!(data.items[0].size, 4);
        assert!(!data.items[0].hash.is_empty(), "the checked hash is recorded");
        assert!(folder.join(RESTORE_NOTE).is_file());
        assert!(!folder.join("Cache").exists(), "caches are made again, not backed up");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_backup_never_overwrites_the_first() {
        let root = temp("twice");
        let plan = library(&root);
        let (Progress::Finished { folder: first, .. }, _) = run_plan(&plan) else { panic!() };
        let (Progress::Finished { folder: second, .. }, _) = run_plan(&plan) else { panic!() };
        assert_ne!(first, second);
        assert!(second.ends_with("PocketMedia backup 2026-10-13 1620 (2)"));
        assert!(first.join(RESTORE_NOTE).is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_destination_inside_pocketmedia_is_refused() {
        let root = temp("inside");
        let mut plan = library(&root);
        for dest in [plan.media.join("GIFs"), plan.root.clone(), plan.root.join("Data")] {
            plan.dest = dest.clone();
            let (p, _) = run_plan(&plan);
            assert!(matches!(&p, Progress::Failed(e) if e.starts_with("Choose a folder outside PocketMedia")), "{dest:?}: {p:?}");
        }
        assert_eq!(fs::read_dir(plan.media.join("GIFs")).unwrap().count(), 1, "nothing was written there");
        // A folder next to it whose name starts the same is outside.
        let beside = root.join("app backups");
        fs::create_dir_all(&beside).unwrap();
        plan.dest = beside;
        assert!(matches!(run_plan(&plan).0, Progress::Finished { .. }));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_destination_is_a_reported_failure() {
        let root = temp("missing");
        let mut plan = library(&root);
        plan.dest = root.join("not there");
        assert!(matches!(run_plan(&plan).0, Progress::Failed(e) if e.contains("could not be opened")));
        plan.dest = plan.media.join("Pictures/a.png");
        assert!(matches!(run_plan(&plan).0, Progress::Failed(_)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_removed_meanwhile_is_left_out_of_both_media_and_library() {
        let root = temp("gone");
        let plan = library(&root);
        fs::remove_file(plan.media.join("Pictures/a.png")).unwrap();
        let (Progress::Finished { folder, files, left_out, .. }, _) = run_plan(&plan) else { panic!() };
        assert_eq!((files, left_out), (2, vec!["a.png".to_string()]));
        let data = crate::store::check_text(&fs::read_to_string(folder.join("Data").join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(data.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(data.history, vec![3], "history keeps only items in the backup");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_removes_what_this_backup_made_and_nothing_else() {
        let root = temp("cancel");
        let plan = library(&root);
        fs::write(plan.dest.join("mine.txt"), b"keep").unwrap();
        let cancel = AtomicBool::new(false);
        let p = run(&plan, fnv, &cancel, &mut |done, _| {
            if done == 2 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(p, Progress::Cancelled);
        let left: Vec<String> = fs::read_dir(&plan.dest).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
        assert_eq!(left, vec!["mine.txt"]);
        assert_eq!(fs::read_to_string(plan.dest.join("mine.txt")).unwrap(), "keep");
        assert!(plan.media.join("Pictures/a.png").is_file(), "the library is untouched");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_backup_restores_onto_another_install_path() {
        let root = temp("restore");
        let plan = library(&root);
        let (Progress::Finished { folder, .. }, _) = run_plan(&plan) else { panic!() };
        // Another PocketMedia folder: the backup's Media and Data go beside its exe.
        let other = crate::paths::Paths::under(&root.join("elsewhere"));
        copy_tree(&folder.join("Media"), &other.media);
        copy_tree(&folder.join("Data"), &other.data);
        other.create().unwrap();
        let (store, outcome) = crate::store::Store::open(&other.data);
        assert_eq!(outcome, crate::store::LoadOutcome::Loaded);
        let records: Vec<(u32, String)> = store.data.items.iter().map(|i| (i.id, i.path.clone())).collect();
        let (orphans, missing) = crate::import::reconcile(&other.media, &records);
        assert_eq!((records.len(), orphans.len(), missing.len()), (3, 0, 0));
        let _ = fs::remove_dir_all(&root);
    }

    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for e in fs::read_dir(from).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                copy_tree(&e.path(), &to.join(e.file_name()));
            } else {
                fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }

    #[test]
    fn inside_compares_whole_folder_names_ignoring_case() {
        assert!(inside(Path::new("/x/App/Media"), Path::new("/x/app")));
        assert!(inside(Path::new("/x/app"), Path::new("/x/app")));
        assert!(!inside(Path::new("/x/app backups"), Path::new("/x/app")));
        assert!(!inside(Path::new("/x"), Path::new("/x/app")));
    }

    #[test]
    fn sizes_read_plainly() {
        assert_eq!(size_text(900), "900 bytes");
        assert_eq!(size_text(2048), "2.0 KB");
        assert_eq!(size_text(5 << 30), "5.0 GB");
    }
}
