//! Import work on a worker thread (disk work never blocks the message
//! loop and is cancelable). Results are queued and the main window is woken
//! with `WM_JOB`; the UI thread then takes them with `take`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::import::{self, Copied, CopyRequest, KnownFile, Plan, Source};

pub const WM_JOB: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 3;
/// Thumbnails or preview frames are ready (thumbs.rs, preview.rs).
pub const WM_PREVIEW: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 4;

#[derive(Debug)]
pub enum Event {
    /// The chosen files were scanned and checked.
    Planned { job: u64, result: Result<Plan, String> },
    /// One copy finished or failed (`index` is the review row).
    Copied { job: u64, index: usize, kind: crate::sample::MediaKind, result: Result<Copied, String> },
    /// Every copy is done, or the job was stopped.
    Finished { job: u64, stopped: bool },
    /// A library item is ready for the clipboard.
    Prepared { job: u64, result: Result<crate::transfer::Payload, String> },
    /// Changes in the Media folders made outside PocketMedia (`watch.rs`).
    Watched(crate::watch::Batch),
    /// Watching the Media folders stopped, and why.
    WatchStopped(String),
    /// A full comparison of the Media folders with a snapshot of `(id, path)`:
    /// files without a record, and records whose file is gone.
    Rescanned { job: u64, orphans: Vec<crate::import::Orphan>, missing: Vec<(u32, String)> },
    /// Back up library progress or its result (`backup.rs`).
    Backup { job: u64, progress: crate::backup::Progress },
    /// A Manager rename or move to the Recycle Bin finished (`manager.rs`).
    FileOp { job: u64, done: crate::manager::FileDone },
}

impl Event {
    pub fn job(&self) -> u64 {
        match self {
            Event::Planned { job, .. }
            | Event::Copied { job, .. }
            | Event::Finished { job, .. }
            | Event::Prepared { job, .. }
            | Event::Rescanned { job, .. }
            | Event::Backup { job, .. }
            | Event::FileOp { job, .. } => *job,
            Event::Watched(_) | Event::WatchStopped(_) => 0,
        }
    }
}

static MAIN: AtomicIsize = AtomicIsize::new(0);
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);
static QUEUE: Mutex<Vec<Event>> = Mutex::new(Vec::new());

/// Where `WM_JOB` goes (the hidden main window).
pub fn init(main: HWND) {
    MAIN.store(main.0 as isize, Ordering::SeqCst);
}

pub fn post(ev: Event) {
    if let Ok(mut q) = QUEUE.lock() {
        q.push(ev);
    }
    let main = HWND(MAIN.load(Ordering::SeqCst) as *mut _);
    // SAFETY: posting to our own window; it tolerates a stale handle.
    let _ = unsafe { PostMessageW(Some(main), WM_JOB, WPARAM(0), LPARAM(0)) };
}

/// Wakes the UI thread with `msg` (from any thread).
pub fn notify(msg: u32) {
    let main = HWND(MAIN.load(Ordering::SeqCst) as *mut _);
    // SAFETY: posting to our own window; it tolerates a stale handle.
    let _ = unsafe { PostMessageW(Some(main), msg, WPARAM(0), LPARAM(0)) };
}

/// Everything that arrived since the last call (UI thread).
pub fn take() -> Vec<Event> {
    QUEUE.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}

/// Only the finished renames and removals, leaving everything else queued
/// (Exit waits for them, `manager::destroy`).
pub fn take_file_ops() -> Vec<Event> {
    let Ok(mut q) = QUEUE.lock() else { return Vec::new() };
    let (mine, rest) = std::mem::take(&mut *q).into_iter().partition(|e| matches!(e, Event::FileOp { .. }));
    *q = rest;
    mine
}

/// Only the events of job `id`, leaving everything else queued (Exit stops an
/// import and records what it copied, `AddPanel::stop_and_wait`).
pub fn take_for(id: u64) -> Vec<Event> {
    let Ok(mut q) = QUEUE.lock() else { return Vec::new() };
    let (mine, rest) = std::mem::take(&mut *q).into_iter().partition(|e| e.job() == id);
    *q = rest;
    mine
}

/// Self-test only: a pause before each copy of an import, so Exit can be chosen
/// while it runs. The pause ends early on Cancel.
pub static TEST_COPY_DELAY_MS: AtomicU64 = AtomicU64::new(0);

fn test_pause(cancel: &AtomicBool) {
    let ms = TEST_COPY_DELAY_MS.load(Ordering::Relaxed);
    let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < end && !cancel.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// A running job. Dropping it does not stop it; `cancel` does.
pub struct Job {
    pub id: u64,
    cancel: Arc<AtomicBool>,
}

impl Job {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn start(name: &str, work: impl FnOnce(u64, &AtomicBool) + Send + 'static) -> Job {
    let id = NEXT_JOB.fetch_add(1, Ordering::Relaxed);
    spawn_as(id, name, work).unwrap_or_else(|e| {
        post(Event::Planned { job: id, result: Err(format!("could not start the import: {e}")) });
        Job { id, cancel: Arc::new(AtomicBool::new(false)) }
    })
}

/// Runs `work` on a new thread. Err when Windows could not start one.
pub fn spawn(name: &str, work: impl FnOnce(u64, &AtomicBool) + Send + 'static) -> Result<Job, String> {
    spawn_as(NEXT_JOB.fetch_add(1, Ordering::Relaxed), name, work)
}

fn spawn_as(id: u64, name: &str, work: impl FnOnce(u64, &AtomicBool) + Send + 'static) -> Result<Job, String> {
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    std::thread::Builder::new()
        .name(format!("{name}-{id}"))
        .spawn(move || work(id, &flag))
        .map(|_| Job { id, cancel })
        .map_err(|e| format!("could not start a worker thread: {e}"))
}

/// Scans the chosen files and decides what to copy.
pub fn plan(source: Source, known: Vec<KnownFile>, media: PathBuf) -> Job {
    start("import", move |job, cancel| {
        let result = import::scan(&source, &media, cancel)
            .and_then(|cands| import::plan(cands, &known, &media, crate::sha256::new, cancel));
        post(Event::Planned { job, result });
    })
}

/// Copies the approved files, one at a time, reporting each.
pub fn copy(requests: Vec<(usize, CopyRequest)>, media: PathBuf) -> Job {
    start("import", move |job, cancel| {
        for (index, req) in requests {
            test_pause(cancel);
            if cancel.load(Ordering::Relaxed) {
                post(Event::Finished { job, stopped: true });
                return;
            }
            let result = import::copy_one(&req, &media, crate::sha256::new, crate::fileops::move_no_replace, cancel);
            // A copy cut short by Cancel left nothing behind; it is not a failure.
            if result.is_err() && cancel.load(Ordering::Relaxed) {
                post(Event::Finished { job, stopped: true });
                return;
            }
            post(Event::Copied { job, index, kind: req.kind, result });
        }
        post(Event::Finished { job, stopped: false });
    })
}

/// Reads and decodes a library file for the clipboard.
pub fn prepare(file: PathBuf) -> Job {
    start("prepare", move |job, _cancel| {
        let _com = crate::wic::ComThread::init();
        post(Event::Prepared { job, result: crate::transfer::prepare(&file) });
    })
}

/// Compares the Media folders with `records` (a snapshot of `(id, path)`).
pub fn rescan(media: PathBuf, records: Vec<(u32, String)>) -> Option<Job> {
    spawn("rescan", move |job, _cancel| {
        let (orphans, missing_ids) = import::reconcile(&media, &records);
        let missing = records.into_iter().filter(|(id, _)| missing_ids.contains(id)).collect();
        post(Event::Rescanned { job, orphans, missing });
    })
    .ok()
}
