//! Watches the Media folders for changes made outside PocketMedia:
//! files added, renamed, replaced or deleted in File Explorer while the app runs.
//!
//! - One thread waits on Windows' directory change notifications for the Media
//!   folder (`ReadDirectoryChangesW`). While nothing changes it sleeps in an
//!   infinite wait: no timer, no polling, no recurring scan.
//! - Events are coalesced: after the first one, more are collected until the
//!   folder has been quiet for 200 ms (at most 1 s), then the touched files are
//!   looked at once on this thread and the result goes to the UI thread as one
//!   `Batch`, which `Library::apply_watch` applies.
//! - Only files directly in `Pictures`, `GIFs` and `Videos` count. Import's
//!   temporary `.partial` files and anything deeper or elsewhere are ignored.
//! - When Windows reports that events were lost (overflow), or a category folder
//!   itself was added, removed or renamed, the folders are compared with the
//!   library once, on a worker thread (`import::reconcile`, bounded to the three
//!   folders). A missing category folder is created again when possible.
//! - PocketMedia's own imports, renames and removals produce events too; they
//!   find the library already up to date, so they change nothing.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::import::{FOLDERS, is_partial};
use crate::jobs::Event;

/// Windows' FILE_ACTION_* values.
pub const ADDED: u32 = 1;
pub const REMOVED: u32 = 2;
pub const MODIFIED: u32 = 3;
pub const RENAMED_OLD: u32 = 4;
pub const RENAMED_NEW: u32 = 5;

/// The normal notification buffer (Windows' limit for network folders).
pub const BUFFER: usize = 64 * 1024;
/// Quiet time that ends a batch, and the longest a batch waits.
const QUIET: Duration = Duration::from_millis(200);
const LONGEST: Duration = Duration::from_millis(1000);
/// More distinct files than this in one batch: compare the folders instead.
const MAX_TOUCHED: usize = 10_000;

/// What a file in a Media folder looks like now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileInfo {
    pub size: u64,
    pub width: u32,
    pub height: u32,
}

/// One coalesced set of changes. Paths are relative to Media, `/`-separated
/// (`Pictures/Cat.png`), as in the library.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Batch {
    /// Renames in the order they happened: (old, new).
    pub renames: Vec<(String, String)>,
    /// Every touched file and what is on disk now (None: it is gone).
    pub files: Vec<(String, Option<FileInfo>)>,
    /// Files whose content was written: a stored hash may no longer match.
    pub written: Vec<String>,
    /// Compare the folders with the library in full.
    pub rescan: bool,
}

/// Where a path reported by Windows (relative to Media, `\`-separated) belongs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// A file directly in a category folder, as a library path.
    File(String),
    /// A category folder itself.
    Folder,
    Ignore,
}

pub fn classify(raw: &str) -> Place {
    let folder = |s: &str| FOLDERS.iter().find(|f| f.eq_ignore_ascii_case(s)).copied();
    let parts: Vec<&str> = raw.split('\\').collect();
    match parts.as_slice() {
        [one] if folder(one).is_some() => Place::Folder,
        [dir, name] => match folder(dir) {
            Some(f) if !name.is_empty() && !is_partial(name) => Place::File(format!("{f}/{name}")),
            _ => Place::Ignore,
        },
        _ => Place::Ignore,
    }
}

/// Reads Windows' FILE_NOTIFY_INFORMATION records: (action, path).
pub fn parse(buf: &[u8]) -> Vec<(u32, String)> {
    let word = |at: usize| buf.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let mut out = Vec::new();
    let mut at = 0usize;
    while let (Some(next), Some(action), Some(len)) = (word(at), word(at + 4), word(at + 8)) {
        let Some(name) = buf.get(at + 12..at + 12 + len as usize) else { break };
        let units: Vec<u16> = name.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        out.push((action, String::from_utf16_lossy(&units)));
        if next == 0 {
            break;
        }
        at += next as usize;
    }
    out
}

/// Events collected for the next batch.
#[derive(Debug, Default)]
pub struct Pending {
    pub renames: Vec<(String, String)>,
    pub touched: Vec<String>,
    pub written: Vec<String>,
    pub rescan: bool,
    seen: HashSet<String>,
    old: Option<Place>,
}

impl Pending {
    pub fn add(&mut self, action: u32, raw: &str) {
        let place = classify(raw);
        match action {
            RENAMED_OLD => {
                self.old = Some(place);
                return;
            }
            RENAMED_NEW => match (self.old.take().unwrap_or(Place::Ignore), place) {
                (Place::Folder, _) | (_, Place::Folder) => self.rescan = true,
                (Place::File(a), Place::File(b)) => {
                    self.touch(&a);
                    self.touch(&b);
                    self.renames.push((a, b));
                }
                (Place::File(p), Place::Ignore) | (Place::Ignore, Place::File(p)) => self.touch(&p),
                (Place::Ignore, Place::Ignore) => {}
            },
            ADDED | REMOVED => match place {
                Place::Folder => self.rescan = true,
                Place::File(p) => self.touch(&p),
                Place::Ignore => {}
            },
            // A folder's own "modified" only means its contents changed.
            MODIFIED => {
                if let Place::File(p) = place {
                    self.touch(&p);
                    if !self.written.iter().any(|w| w.eq_ignore_ascii_case(&p)) {
                        self.written.push(p);
                    }
                }
            }
            _ => {}
        }
        if self.touched.len() > MAX_TOUCHED {
            self.rescan = true;
            self.renames.clear();
            self.touched.clear();
            self.written.clear();
            self.seen.clear();
        }
    }

    fn touch(&mut self, rel: &str) {
        if self.seen.insert(rel.to_lowercase()) {
            self.touched.push(rel.to_string());
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.rescan && self.touched.is_empty()
    }
}

/// When the pending batch is due: 200 ms after the last event, at most 1 s after the first.
fn due(first: Instant, last: Instant) -> Instant {
    (last + QUIET).min(first + LONGEST)
}

// ---------------------------------------------------------------- counters (self-test log)

static EVENTS: AtomicU64 = AtomicU64::new(0);
static WAKES: AtomicU64 = AtomicU64::new(0);
/// Wakes because Windows reported a change (the read completed).
static NOTICES: AtomicU64 = AtomicU64::new(0);
/// Wakes because a waiting batch's quiet time was over.
static DUE: AtomicU64 = AtomicU64::new(0);
/// The last change Windows reported: its action number and path under Media (for the self-test's log).
static LAST: Mutex<(u32, String)> = Mutex::new((0, String::new()));
static BATCHES: AtomicU64 = AtomicU64::new(0);
static OVERFLOWS: AtomicU64 = AtomicU64::new(0);
static RESCANS: AtomicU64 = AtomicU64::new(0);
static RUNNING: AtomicBool = AtomicBool::new(false);

// ---------------------------------------------------------------- the watcher thread

struct Watcher {
    stop: Arc<crate::winutil::OwnedHandle>,
    thread: std::thread::JoinHandle<()>,
}

static WATCHER: Mutex<Option<Watcher>> = Mutex::new(None);

/// Starts watching `media` with a notification buffer of `buffer` bytes.
pub fn start(media: &Path, buffer: usize) -> Result<(), String> {
    stop();
    let dir = win::open_dir(media)?;
    let stop = Arc::new(win::event()?);
    let (flag, media) = (stop.clone(), media.to_path_buf());
    let thread = std::thread::Builder::new()
        .name("media-watch".into())
        .spawn(move || {
            RUNNING.store(true, Ordering::SeqCst);
            win::run(&media, &dir, &flag, buffer);
            RUNNING.store(false, Ordering::SeqCst);
        })
        .map_err(|e| format!("could not start watching the Media folder: {e}"))?;
    if let Ok(mut w) = WATCHER.lock() {
        *w = Some(Watcher { stop, thread });
    }
    Ok(())
}

/// Stops watching and waits for the thread (Exit, and the self-test's resets).
pub fn stop() {
    let w = WATCHER.lock().ok().and_then(|mut w| w.take());
    if let Some(w) = w {
        win::signal(&w.stop);
        let _ = w.thread.join();
    }
}

/// Looks at the touched files once and sends the batch to the UI thread.
fn flush(media: &Path, pending: &mut Pending) {
    let p = std::mem::take(pending);
    if p.rescan {
        for f in FOLDERS {
            let dir = media.join(f);
            if !dir.is_dir()
                && let Err(e) = std::fs::create_dir_all(&dir)
            {
                crate::selftest::note(&format!("the {f} folder is missing and could not be created again: {e}"));
            }
        }
    }
    let files = p.touched.into_iter().map(|rel| {
        let info = crate::import::media_file(media, &rel).and_then(|f| file_info(&f));
        (rel, info)
    });
    let batch = Batch { renames: p.renames, files: files.collect(), written: p.written, rescan: p.rescan };
    BATCHES.fetch_add(1, Ordering::Relaxed);
    crate::jobs::post(Event::Watched(batch));
}

fn file_info(path: &Path) -> Option<FileInfo> {
    let m = std::fs::metadata(path).ok().filter(|m| m.is_file())?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let (width, height) = if crate::media_info::kind_for(&name).is_some() { crate::media_info::dimensions(path) } else { (0, 0) };
    Some(FileInfo { size: m.len(), width, height })
}

mod win {
    use super::*;
    use crate::winutil::OwnedHandle;
    use windows::Win32::Foundation::{ERROR_NOTIFY_ENUM_DIR, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_DIR_NAME,
        FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING, ReadDirectoryChangesW,
    };
    use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows::Win32::System::Threading::{CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects};
    use windows::core::HSTRING;

    /// The Media folder, opened for change notifications. It is not shared for
    /// delete, so the Media folder itself cannot be removed while PocketMedia runs;
    /// the category folders inside it can.
    pub fn open_dir(media: &Path) -> Result<OwnedHandle, String> {
        let path = HSTRING::from(media.as_os_str());
        // SAFETY: `path` is a valid null-terminated string for the call; the handle gets one owner.
        unsafe {
            CreateFileW(
                &path,
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
        }
        .map(OwnedHandle)
        .map_err(|e| format!("could not watch the Media folder: {}", e.message()))
    }

    /// A manual-reset event, not signalled.
    pub fn event() -> Result<OwnedHandle, String> {
        // SAFETY: no pointers are passed; the handle gets one owner.
        unsafe { CreateEventW(None, true, false, None) }.map(OwnedHandle).map_err(|e| e.message())
    }

    pub fn signal(h: &OwnedHandle) {
        // SAFETY: `h` is a live event handle owned by the caller.
        let _ = unsafe { SetEvent(h.0) };
    }

    pub fn run(media: &Path, dir: &OwnedHandle, stop: &OwnedHandle, buffer: usize) {
        let io = match event() {
            Ok(h) => h,
            Err(e) => return crate::selftest::note(&format!("watching stopped: {e}")),
        };
        // u64 words keep the buffer DWORD-aligned, as Windows requires.
        let mut buf = vec![0u64; buffer.max(64).div_ceil(8)];
        let mut ov = OVERLAPPED { hEvent: io.0, ..Default::default() };
        let mut pending = Pending::default();
        let mut times: Option<(Instant, Instant)> = None;
        let mut reading = false;
        let filter = FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME | FILE_NOTIFY_CHANGE_SIZE | FILE_NOTIFY_CHANGE_LAST_WRITE;
        loop {
            if !reading {
                // SAFETY: `io` is our event; no read is outstanding on it.
                let _ = unsafe { ResetEvent(io.0) };
                // SAFETY: `buf` and `ov` stay alive and unmoved until this read completes,
                // or is cancelled and waited for at the end of this function.
                let started = unsafe {
                    ReadDirectoryChangesW(dir.0, buf.as_mut_ptr().cast(), (buf.len() * 8) as u32, true, filter, None, Some(&mut ov), None)
                };
                if let Err(e) = started {
                    crate::selftest::note(&format!("watching stopped: {}", e.message()));
                    pending.rescan = true;
                    flush(media, &mut pending);
                    return;
                }
                reading = true;
            }
            let timeout = match times {
                None => INFINITE,
                Some((first, last)) => due(first, last).saturating_duration_since(Instant::now()).as_millis() as u32,
            };
            // SAFETY: both handles are live for the whole wait.
            let woke = unsafe { WaitForMultipleObjects(&[stop.0, io.0], false, timeout) };
            WAKES.fetch_add(1, Ordering::Relaxed);
            if woke == WAIT_OBJECT_0 {
                break;
            }
            if woke != WAIT_TIMEOUT && woke.0 != WAIT_OBJECT_0.0 + 1 {
                // WAIT_FAILED: never spin; stop watching and say so.
                crate::selftest::note("watching stopped: the wait failed");
                break;
            }
            if woke.0 == WAIT_OBJECT_0.0 + 1 {
                NOTICES.fetch_add(1, Ordering::Relaxed);
                reading = false;
                let mut n = 0u32;
                // SAFETY: the read on `ov` has completed: its event is signalled.
                match unsafe { GetOverlappedResult(dir.0, &ov, &mut n, false) } {
                    Ok(()) if n == 0 => {
                        OVERFLOWS.fetch_add(1, Ordering::Relaxed);
                        pending.rescan = true;
                    }
                    Ok(()) => {
                        let bytes: Vec<u8> = buf.iter().flat_map(|w| w.to_le_bytes()).take(n as usize).collect();
                        for (action, path) in parse(&bytes) {
                            EVENTS.fetch_add(1, Ordering::Relaxed);
                            if let Ok(mut last) = LAST.lock() {
                                *last = (action, path.clone());
                            }
                            pending.add(action, &path);
                        }
                    }
                    Err(e) if e.code() == ERROR_NOTIFY_ENUM_DIR.to_hresult() => {
                        OVERFLOWS.fetch_add(1, Ordering::Relaxed);
                        pending.rescan = true;
                    }
                    Err(e) => {
                        crate::selftest::note(&format!("watching stopped: {}", e.message()));
                        pending.rescan = true;
                        flush(media, &mut pending);
                        return;
                    }
                }
                let now = Instant::now();
                times = if pending.is_empty() { None } else { Some((times.map_or(now, |t| t.0), now)) };
                continue;
            }
            // The quiet time is over.
            if times.take().is_some() {
                DUE.fetch_add(1, Ordering::Relaxed);
                flush(media, &mut pending);
            }
        }
        if reading {
            let mut n = 0u32;
            // SAFETY: cancels our own outstanding read, then waits until Windows no
            // longer uses `buf` and `ov`.
            unsafe {
                let _ = CancelIoEx(dir.0, Some(&ov));
                let _ = GetOverlappedResult(dir.0, &ov, &mut n, true);
            }
        }
    }
}

// ---------------------------------------------------------------- UI thread

thread_local! {
    /// A comparison of the folders is running / another one was asked for meanwhile.
    static RESCAN: std::cell::Cell<(bool, bool)> = const { std::cell::Cell::new((false, false)) };
}

/// Applies watcher results to the library (WM_JOB). Returns false for other events.
pub fn on_event(ev: &Event) -> bool {
    match ev {
        Event::Watched(batch) => {
            let applied = crate::library::with_mut(|lib| lib.apply_watch(batch));
            if batch.rescan {
                request_rescan();
            }
            after(applied);
        }
        Event::Rescanned { orphans, missing, .. } => {
            RESCANS.fetch_add(1, Ordering::Relaxed);
            let applied = crate::library::with_mut(|lib| lib.apply_rescan(orphans, missing));
            let again = RESCAN.with(|r| r.replace((false, false)).1);
            if again {
                request_rescan();
            }
            after(applied);
        }
        _ => return false,
    }
    true
}

/// Compares the folders with the library on a worker thread (one at a time).
fn request_rescan() {
    let (running, _) = RESCAN.with(|r| r.get());
    if running {
        RESCAN.with(|r| r.set((true, true)));
        return;
    }
    let Some(media) = crate::paths::get().map(|p| p.media.clone()) else { return };
    let records = crate::library::with(|lib| lib.items.iter().map(|i| (i.id, i.path.clone())).collect());
    let started = crate::jobs::rescan(media, records).is_some();
    RESCAN.with(|r| r.set((started, false)));
}

thread_local! {
    static APPLIED: std::cell::RefCell<crate::library::Applied> = std::cell::RefCell::new(Default::default());
}

/// Saves the library and redraws the windows after a change.
fn after(applied: crate::library::Applied) {
    if applied.is_empty() {
        return;
    }
    for id in applied.replaced.iter().chain(&applied.removed) {
        crate::preview::forget(*id);
        crate::thumbs::forget(*id);
    }
    if let Err(e) = crate::library::persist() {
        crate::selftest::note(&format!("after a change in the Media folder, {e}"));
    }
    crate::selftest::note(&format!(
        "Media folder changed: {} added, {} renamed, {} replaced, {} removed",
        applied.added.len(),
        applied.renamed.len(),
        applied.replaced.len(),
        applied.removed.len()
    ));
    crate::popup::library_changed();
    crate::manager::library_changed(&applied.renamed);
    APPLIED.with(|a| a.borrow_mut().merge(applied));
}

/// For the self-test log.
pub fn describe() -> String {
    let a = APPLIED.with(|a| a.borrow().clone());
    format!(
        "watch running={} events={} wakes={} notices={} due={} batches={} overflows={} rescans={} added={} renamed={} replaced={} removed={} last={}",
        RUNNING.load(Ordering::SeqCst),
        EVENTS.load(Ordering::Relaxed),
        WAKES.load(Ordering::Relaxed),
        NOTICES.load(Ordering::Relaxed),
        DUE.load(Ordering::Relaxed),
        BATCHES.load(Ordering::Relaxed),
        OVERFLOWS.load(Ordering::Relaxed),
        RESCANS.load(Ordering::Relaxed),
        a.added.len(),
        a.renamed.len(),
        a.replaced.len(),
        a.removed.len(),
        last_change(),
    )
}

/// The last change Windows reported, for the self-test's log: "modified Pictures/Cat.png".
fn last_change() -> String {
    let Ok(last) = LAST.lock() else { return "?".into() };
    let what = match last.0 {
        0 => return "none".into(),
        1 => "added",
        2 => "removed",
        3 => "modified",
        4 => "renamed-from",
        5 => "renamed-to",
        _ => "other",
    };
    format!("{what} \"{}\"", last.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(action: u32, name: &str, last: bool) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let len = units.len() * 2;
        let size = (12 + len).div_ceil(4) * 4;
        let mut b = Vec::new();
        b.extend_from_slice(&(if last { 0 } else { size as u32 }).to_le_bytes());
        b.extend_from_slice(&action.to_le_bytes());
        b.extend_from_slice(&(len as u32).to_le_bytes());
        for u in units {
            b.extend_from_slice(&u.to_le_bytes());
        }
        b.resize(size, 0);
        b
    }

    #[test]
    fn parses_windows_records() {
        let mut buf = record(RENAMED_OLD, "Pictures\\a.png", false);
        buf.extend(record(RENAMED_NEW, "Pictures\\Ü b.png", false));
        buf.extend(record(REMOVED, "GIFs\\x.gif", true));
        assert_eq!(
            parse(&buf),
            vec![(RENAMED_OLD, "Pictures\\a.png".into()), (RENAMED_NEW, "Pictures\\Ü b.png".into()), (REMOVED, "GIFs\\x.gif".into())]
        );
        // A cut-short buffer gives what is complete, never a panic.
        assert_eq!(parse(&buf[..buf.len() - 3]).len(), 2);
        assert!(parse(&[1, 2, 3]).is_empty());
    }

    #[test]
    fn only_files_directly_in_the_category_folders_count() {
        assert_eq!(classify("Pictures\\Cat.png"), Place::File("Pictures/Cat.png".into()));
        assert_eq!(classify("videos\\Clip.MP4"), Place::File("Videos/Clip.MP4".into()));
        assert_eq!(classify("GIFs"), Place::Folder);
        assert_eq!(classify("Pictures\\sub\\Cat.png"), Place::Ignore);
        assert_eq!(classify("loose.png"), Place::Ignore);
        assert_eq!(classify("Other\\Cat.png"), Place::Ignore);
        assert_eq!(classify("Pictures\\.pocketmedia-1-2.partial"), Place::Ignore);
    }

    #[test]
    fn events_are_coalesced_per_file() {
        let mut p = Pending::default();
        p.add(ADDED, "Pictures\\a.png");
        p.add(MODIFIED, "Pictures\\a.png");
        p.add(MODIFIED, "Pictures\\A.PNG");
        p.add(MODIFIED, "Pictures");
        assert_eq!(p.touched, vec!["Pictures/a.png"]);
        assert_eq!(p.written, vec!["Pictures/a.png"]);
        assert!(!p.rescan);
    }

    #[test]
    fn renames_pair_old_and_new_names() {
        let mut p = Pending::default();
        p.add(RENAMED_OLD, "Pictures\\a.png");
        p.add(RENAMED_NEW, "Pictures\\b.png");
        // An import publishing its temporary copy: only the new name matters.
        p.add(RENAMED_OLD, "GIFs\\.pocketmedia-7-1.partial");
        p.add(RENAMED_NEW, "GIFs\\New.gif");
        // Moved into a subfolder: the old name is gone.
        p.add(RENAMED_OLD, "Videos\\c.mp4");
        p.add(RENAMED_NEW, "Videos\\keep\\c.mp4");
        assert_eq!(p.renames, vec![("Pictures/a.png".to_string(), "Pictures/b.png".to_string())]);
        assert_eq!(p.touched, vec!["Pictures/a.png", "Pictures/b.png", "GIFs/New.gif", "Videos/c.mp4"]);
        assert!(!p.rescan);
    }

    #[test]
    fn a_category_folder_change_asks_for_a_full_comparison() {
        for (a, path) in [(REMOVED, "Videos"), (ADDED, "GIFs")] {
            let mut p = Pending::default();
            p.add(a, path);
            assert!(p.rescan, "{path}");
        }
        let mut p = Pending::default();
        p.add(RENAMED_OLD, "Pictures");
        p.add(RENAMED_NEW, "Pictures old");
        assert!(p.rescan);
    }

    #[test]
    fn too_many_files_become_one_comparison() {
        let mut p = Pending::default();
        for i in 0..=MAX_TOUCHED {
            p.add(ADDED, &format!("Pictures\\{i}.png"));
        }
        assert!(p.rescan);
        assert!(p.touched.is_empty(), "memory stays bounded");
    }

    #[test]
    fn a_batch_waits_for_quiet_but_not_forever() {
        let t0 = Instant::now();
        assert_eq!(due(t0, t0), t0 + QUIET);
        assert_eq!(due(t0, t0 + Duration::from_millis(950)), t0 + LONGEST);
    }
}
