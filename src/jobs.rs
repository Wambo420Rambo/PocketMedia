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
}

impl Event {
    pub fn job(&self) -> u64 {
        match self {
            Event::Planned { job, .. } | Event::Copied { job, .. } | Event::Finished { job, .. } | Event::Prepared { job, .. } => *job,
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

fn post(ev: Event) {
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

fn start(work: impl FnOnce(u64, &AtomicBool) + Send + 'static) -> Job {
    let id = NEXT_JOB.fetch_add(1, Ordering::Relaxed);
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let spawned = std::thread::Builder::new().name(format!("import-{id}")).spawn(move || work(id, &flag));
    if let Err(e) = spawned {
        post(Event::Planned { job: id, result: Err(format!("could not start the import: {e}")) });
    }
    Job { id, cancel }
}

/// Scans the chosen files and decides what to copy.
pub fn plan(source: Source, known: Vec<KnownFile>, media: PathBuf) -> Job {
    start(move |job, cancel| {
        let result = import::scan(&source, &media, cancel)
            .and_then(|cands| import::plan(cands, &known, &media, crate::sha256::new, cancel));
        post(Event::Planned { job, result });
    })
}

/// Copies the approved files, one at a time, reporting each.
pub fn copy(requests: Vec<(usize, CopyRequest)>, media: PathBuf) -> Job {
    start(move |job, cancel| {
        for (index, req) in requests {
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
    start(move |job, _cancel| {
        let _com = crate::wic::ComThread::init();
        post(Event::Prepared { job, result: crate::transfer::prepare(&file) });
    })
}
