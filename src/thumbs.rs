//! Thumbnails: made lazily for what a window shows, on two worker threads, and
//! kept at two levels: the disk cache (`thumb_store`, 128 MiB) and one RAM
//! cache shared by both windows, with a byte budget (8 MiB for Use less memory,
//! 16 MiB for Open faster) and least-recently-used eviction.
//!
//! Each full paint lists every thumbnail it draws (`look`) plus a one-row
//! prefetch margin (`prefetch`), and `end_frame` turns that list into the
//! window's wanted list, replacing the old one. A tile that scrolled away, was
//! filtered out or whose window hid is therefore dropped before a worker starts
//! on it, and a result nobody wants any more is thrown away.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

use crate::pixels::Pixels;
use crate::sample::{Item, MediaKind};
use crate::settings_state::Profile;
use crate::thumb_store::{self, Cached, DiskCache};
use crate::ui::layout::Rect;

/// One thumbnail: an item at a box size in device pixels (so another DPI or
/// box is another thumbnail).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub id: u32,
    pub w: u32,
    pub h: u32,
}

impl Key {
    /// The thumbnail for `item` inside a box of `w` x `h` DIPs at `dpi`.
    pub fn new(id: u32, w: f32, h: f32, dpi: f32) -> Self {
        let px = |dip: f32| (dip * dpi / 96.0).floor().max(1.0) as u32;
        Key { id, w: px(w), h: px(h) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Win {
    Popup,
    Manager,
}

impl Win {
    fn index(self) -> usize {
        self as usize
    }
}

pub enum Look {
    Ready(Rc<Pixels>, u64),
    Pending,
    /// The file has no preview (unsupported or damaged); transfer still works.
    NoPreview,
}

pub fn budget(profile: Profile) -> usize {
    match profile {
        Profile::LessMemory => 8 * 1024 * 1024,
        Profile::OpenFaster => 16 * 1024 * 1024,
    }
}

/// A unique number for each new picture, so a window's bitmap knows to refresh.
pub fn next_version() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------- RAM cache

struct Entry {
    px: Rc<Pixels>,
    version: u64,
    used: u64,
}

/// The RAM cache: pixels by key, within a byte budget, least recently used out first.
#[derive(Default)]
pub struct Ram {
    map: HashMap<Key, Entry>,
    bytes: usize,
    clock: u64,
}

impl Ram {
    pub fn get(&mut self, key: &Key) -> Option<(Rc<Pixels>, u64)> {
        self.clock += 1;
        let e = self.map.get_mut(key)?;
        e.used = self.clock;
        Some((e.px.clone(), e.version))
    }

    pub fn contains(&self, key: &Key) -> bool {
        self.map.contains_key(key)
    }

    /// Adds `px`, then evicts the least recently used entries (never one in
    /// `keep`, which is on screen) until the cache fits `budget` again.
    pub fn insert(&mut self, key: Key, px: Pixels, budget: usize, keep: &HashSet<Key>) {
        self.clock += 1;
        self.bytes += px.bytes();
        if let Some(old) = self.map.insert(key, Entry { px: Rc::new(px), version: next_version(), used: self.clock }) {
            self.bytes -= old.px.bytes();
        }
        while self.bytes > budget {
            let victim = self.map.iter().filter(|(k, _)| !keep.contains(k) && **k != key).min_by_key(|(_, e)| e.used).map(|(k, _)| *k);
            let Some(victim) = victim else { break };
            if let Some(e) = self.map.remove(&victim) {
                self.bytes -= e.px.bytes();
            }
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }

    /// Drops every size of item `id`'s thumbnail.
    pub fn remove_id(&mut self, id: u32) {
        let bytes = &mut self.bytes;
        self.map.retain(|k, e| {
            let keep = k.id != id;
            if !keep {
                *bytes -= e.px.bytes();
            }
            keep
        });
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

// ---------------------------------------------------------------- shared with the workers

#[derive(Clone)]
struct Req {
    key: Key,
    path: PathBuf,
    kind: MediaKind,
}

#[derive(Default)]
struct Queue {
    /// What each window wants, in drawing order (visible first, then prefetch).
    wanted: [Vec<Req>; 2],
    busy: Vec<Key>,
    workers: usize,
    quit: bool,
}

impl Queue {
    fn wants(&self, key: &Key) -> bool {
        self.wanted.iter().any(|w| w.iter().any(|r| r.key == *key))
    }

    fn next(&mut self) -> Option<Req> {
        let req = self.wanted.iter().flatten().find(|r| !self.busy.contains(&r.key))?.clone();
        self.busy.push(req.key);
        Some(req)
    }

    fn done(&mut self, key: &Key) {
        self.busy.retain(|k| k != key);
        for w in &mut self.wanted {
            w.retain(|r| r.key != *key);
        }
    }
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue { wanted: [Vec::new(), Vec::new()], busy: Vec::new(), workers: 0, quit: false });
static WAKE: Condvar = Condvar::new();
/// Finished thumbnails for the UI thread; Err means the file has no preview.
static DONE: Mutex<Vec<(Key, Result<Pixels, String>)>> = Mutex::new(Vec::new());
static NOTIFIED: AtomicBool = AtomicBool::new(false);
static DISK: Mutex<Option<DiskCache>> = Mutex::new(None);
const WORKERS: usize = 2;

/// Counters for the self-test log.
pub static MADE: AtomicU64 = AtomicU64::new(0);
pub static DISK_HITS: AtomicU64 = AtomicU64::new(0);
pub static DROPPED: AtomicU64 = AtomicU64::new(0);
pub static NO_PREVIEW: AtomicU64 = AtomicU64::new(0);

fn queue() -> std::sync::MutexGuard<'static, Queue> {
    QUEUE.lock().unwrap_or_else(|p| p.into_inner())
}

fn still_wanted(key: &Key) -> bool {
    queue().wants(key)
}

fn worker() {
    let _com = crate::wic::ComThread::init();
    let wic = crate::wic::Wic::new();
    loop {
        let req = {
            let mut q = queue();
            loop {
                if q.quit {
                    return;
                }
                if let Some(r) = q.next() {
                    break r;
                }
                q = WAKE.wait(q).unwrap_or_else(|p| p.into_inner());
            }
        };
        let result = make(&req, wic.as_ref().map_err(|e| e.clone()));
        let wanted = {
            let mut q = queue();
            let wanted = q.wants(&req.key);
            q.done(&req.key);
            wanted
        };
        match result {
            Some(r) if wanted => {
                if let Ok(mut d) = DONE.lock() {
                    d.push((req.key, r));
                }
                if !NOTIFIED.swap(true, Ordering::AcqRel) {
                    crate::jobs::notify(crate::jobs::WM_PREVIEW);
                }
            }
            _ => {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

fn disk<R>(f: impl FnOnce(&mut DiskCache) -> R) -> Option<R> {
    let mut d = DISK.lock().unwrap_or_else(|p| p.into_inner());
    if d.is_none() {
        let dir = crate::paths::get()?.cache.join(thumb_store::DIR);
        *d = Some(DiskCache::new(dir, thumb_store::CAP));
    }
    d.as_mut().map(f)
}

/// Makes one thumbnail: from the disk cache, else by decoding (and then cached).
/// None when it stopped because nobody wants it any more.
fn make(req: &Req, wic: Result<&crate::wic::Wic, String>) -> Option<Result<Pixels, String>> {
    let Ok(stamp) = thumb_store::stamp(&req.path) else {
        return Some(Err("the file is missing".into()));
    };
    let name = thumb_store::file_name(req.key.id, req.key.w, req.key.h);
    if let Some(cached) = disk(|d| d.read(&name, stamp)).flatten() {
        DISK_HITS.fetch_add(1, Ordering::Relaxed);
        return Some(match cached {
            Cached::Thumb(p) => Ok(p),
            Cached::NoPreview => Err("no preview".into()),
        });
    }
    if !still_wanted(&req.key) {
        return None;
    }
    let (w, h) = (req.key.w, req.key.h);
    let made = match req.kind {
        MediaKind::Picture => wic.and_then(|wic| wic.picture_thumb(&req.path, w, h)),
        MediaKind::Gif => wic.and_then(|wic| crate::preview::gif_thumb(wic, &req.path, w, h)),
        MediaKind::Video => crate::video::poster(&req.path, w, h),
    };
    MADE.fetch_add(1, Ordering::Relaxed);
    let entry = match made {
        Ok(p) => Cached::Thumb(p),
        Err(e) => {
            NO_PREVIEW.fetch_add(1, Ordering::Relaxed);
            crate::selftest::note(&format!("no preview for {}: {e}", req.path.display()));
            Cached::NoPreview
        }
    };
    // A failed cache write only costs the work next time.
    let _ = disk(|d| d.write(&name, stamp, &entry));
    Some(match entry {
        Cached::Thumb(p) => Ok(p),
        Cached::NoPreview => Err("no preview".into()),
    })
}

/// Stops the workers (Exit). Work in progress finishes and is dropped.
pub fn shutdown() {
    let mut q = queue();
    q.quit = true;
    q.wanted = [Vec::new(), Vec::new()];
    WAKE.notify_all();
}

// ---------------------------------------------------------------- UI thread

/// One thumbnail drawn (or prefetched) in a full frame.
struct Seen {
    key: Key,
    kind: MediaKind,
    rel: String,
    /// Where it is drawn, in DIPs; None for prefetch.
    rect: Option<Rect>,
}

#[derive(Default)]
struct Frame {
    collecting: bool,
    dpi: u32,
    seen: Vec<Seen>,
}

#[derive(Default)]
struct Ui {
    ram: Ram,
    none: HashSet<Key>,
    frames: [Frame; 2],
    current: Option<Win>,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

fn ui<R>(f: impl FnOnce(&mut Ui) -> R) -> R {
    UI.with(|u| f(&mut u.borrow_mut()))
}

/// Starts a frame of `win`. Only a full frame (the whole window) records what is drawn.
pub fn begin_frame(win: Win, dpi: u32, full: bool) {
    ui(|u| {
        u.current = Some(win);
        let f = &mut u.frames[win.index()];
        f.dpi = dpi;
        f.collecting = full;
        if full {
            f.seen.clear();
        }
    });
}

fn record(u: &mut Ui, item: &Item, key: Key, rect: Option<Rect>) {
    let Some(win) = u.current else { return };
    let f = &mut u.frames[win.index()];
    if f.collecting && !f.seen.iter().any(|s| s.key == key && s.rect.is_some() == rect.is_some()) {
        f.seen.push(Seen { key, kind: item.kind, rel: item.path.clone(), rect });
    }
}

/// The thumbnail to draw for `item` in `rect`, noting that it is on screen.
pub fn look(item: &Item, key: Key, rect: Rect) -> Look {
    ui(|u| {
        record(u, item, key, Some(rect));
        if u.none.contains(&key) {
            return Look::NoPreview;
        }
        match u.ram.get(&key) {
            Some((px, version)) => Look::Ready(px, version),
            None => Look::Pending,
        }
    })
}

/// Notes a thumbnail just outside the visible area, to be made after the visible ones.
pub fn prefetch(item: &Item, key: Key) {
    ui(|u| record(u, item, key, None));
}

pub fn is_no_preview(key: &Key) -> bool {
    ui(|u| u.none.contains(key))
}

/// Ends a frame: a full frame's list becomes the window's wanted list and the
/// set of GIFs that may animate.
pub fn end_frame(win: Win) {
    let Some(media) = crate::paths::get().map(|p| p.media.clone()) else { return };
    let (wanted, gifs) = ui(|u| {
        let f = &mut u.frames[win.index()];
        if !f.collecting {
            return (None, Vec::new());
        }
        f.collecting = false;
        let mut seen: Vec<&Seen> = f.seen.iter().filter(|s| s.rect.is_some()).collect();
        seen.extend(f.seen.iter().filter(|s| s.rect.is_none()));
        let (ram, none) = (&u.ram, &u.none);
        let mut wanted: Vec<Req> = Vec::new();
        for s in seen {
            if !ram.contains(&s.key) && !none.contains(&s.key) && !wanted.iter().any(|r| r.key == s.key) {
                wanted.push(Req { key: s.key, path: crate::import::media_file(&media, &s.rel), kind: s.kind });
            }
        }
        let gifs = f
            .seen
            .iter()
            .filter(|s| s.kind == MediaKind::Gif && s.rect.is_some() && ram.contains(&s.key))
            .map(|s| (s.key, crate::import::media_file(&media, &s.rel)))
            .collect();
        (Some(wanted), gifs)
    });
    let Some(wanted) = wanted else { return };
    let mut q = queue();
    let start = !wanted.is_empty() && q.workers == 0 && !q.quit;
    q.wanted[win.index()] = wanted;
    if start {
        for i in 0..WORKERS {
            if std::thread::Builder::new().name(format!("thumbs-{i}")).spawn(worker).is_ok() {
                q.workers += 1;
            }
        }
    }
    WAKE.notify_all();
    drop(q);
    crate::preview::set_gifs(win, gifs);
}

/// The window was hidden or minimised: its work is cancelled, its animation and
/// hover video stop, and with Use less memory the RAM cache is released once no
/// window shows thumbnails.
pub fn hidden(win: Win, profile: Profile) {
    queue().wanted[win.index()].clear();
    let release = ui(|u| {
        u.frames[win.index()].seen.clear();
        let other_showing = u.frames.iter().any(|f| !f.seen.is_empty());
        let release = profile == Profile::LessMemory && !other_showing;
        if release {
            u.ram.clear();
        }
        release
    });
    crate::preview::hidden(win);
    if release {
        crate::selftest::note("thumbnail RAM cache released (Use less memory, window hidden)");
    }
}

/// Where `key` is drawn: (window, rect in DIPs, dpi).
pub fn rects_for(key: &Key) -> Vec<(Win, Rect, u32)> {
    ui(|u| {
        let mut out = Vec::new();
        for (i, f) in u.frames.iter().enumerate() {
            let win = if i == 0 { Win::Popup } else { Win::Manager };
            for s in f.seen.iter().filter(|s| s.key == *key) {
                if let Some(r) = s.rect {
                    out.push((win, r, f.dpi));
                }
            }
        }
        out
    })
}

/// Repaints `key`'s tiles only.
pub fn invalidate(key: &Key) {
    for (win, r, dpi) in rects_for(key) {
        let hwnd = match win {
            Win::Popup => crate::popup::hwnd(),
            Win::Manager => crate::manager::hwnd(),
        };
        crate::winutil::invalidate_rect(hwnd, r, dpi);
    }
}

/// Takes finished thumbnails into the RAM cache and repaints their tiles (WM_PREVIEW).
pub fn on_wake() {
    NOTIFIED.store(false, Ordering::Release);
    let done = DONE.lock().map(|mut d| std::mem::take(&mut *d)).unwrap_or_default();
    if done.is_empty() {
        return;
    }
    let budget = budget(crate::store::settings().profile);
    let keys: Vec<Key> = ui(|u| {
        let keep: HashSet<Key> = u.frames.iter().flat_map(|f| f.seen.iter().map(|s| s.key)).collect();
        done.into_iter()
            .map(|(key, r)| {
                match r {
                    Ok(px) => u.ram.insert(key, px, budget, &keep),
                    Err(_) => {
                        u.none.insert(key);
                    }
                }
                key
            })
            .collect()
    });
    for key in &keys {
        invalidate(key);
    }
    // A GIF whose first frame just arrived may start animating.
    if keys.iter().any(|k| ui(|u| u.frames.iter().flat_map(|f| &f.seen).any(|s| s.key == *k && s.kind == MediaKind::Gif))) {
        crate::popup::refresh();
        crate::manager::refresh();
    }
}

/// For the self-test log: the RAM cache and what the last frame of `win` showed.
pub fn describe(win: Win) -> String {
    let q = queue();
    let wanted = q.wanted[win.index()].len();
    let busy = q.busy.len();
    drop(q);
    ui(|u| {
        let f = &u.frames[win.index()];
        let drawn: Vec<&Seen> = f.seen.iter().filter(|s| s.rect.is_some()).collect();
        let ready = drawn.iter().filter(|s| u.ram.contains(&s.key)).count();
        let none = drawn.iter().filter(|s| u.none.contains(&s.key)).count();
        format!(
            "thumbs drawn={} ready={} no_preview={} pending={} wanted={} busy={} ram_bytes={} ram_items={} made={} disk_hits={} dropped={} failed={}",
            drawn.len(),
            ready,
            none,
            drawn.len() - ready - none,
            wanted,
            busy,
            u.ram.bytes(),
            u.ram.len(),
            MADE.load(Ordering::Relaxed),
            DISK_HITS.load(Ordering::Relaxed),
            DROPPED.load(Ordering::Relaxed),
            NO_PREVIEW.load(Ordering::Relaxed),
        )
    })
}

/// Item `id`'s file changed (replaced or gone outside PocketMedia): its
/// thumbnails in RAM are dropped, so the next paint makes them again. The disk
/// cache notices the change itself (entries are stamped with size and time).
pub fn forget(id: u32) {
    let keys: Vec<Key> = ui(|u| {
        u.ram.remove_id(id);
        u.none.retain(|k| k.id != id);
        u.frames.iter().flat_map(|f| f.seen.iter().map(|s| s.key)).filter(|k| k.id == id).collect()
    });
    for k in &keys {
        invalidate(k);
    }
}

/// Self-test only: forget everything in RAM (as after a restart).
pub fn reset() {
    ui(|u| {
        u.ram.clear();
        u.none.clear();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(n: usize) -> Pixels {
        Pixels { width: n as u32, height: 1, data: vec![0; n * 4] }
    }

    fn key(id: u32) -> Key {
        Key { id, w: 128, h: 72 }
    }

    #[test]
    fn forgetting_an_item_drops_all_its_sizes() {
        let mut ram = Ram::default();
        let keep = HashSet::new();
        ram.insert(Key { id: 1, w: 10, h: 10 }, px(100), 1 << 20, &keep);
        ram.insert(Key { id: 1, w: 20, h: 20 }, px(400), 1 << 20, &keep);
        ram.insert(Key { id: 2, w: 10, h: 10 }, px(100), 1 << 20, &keep);
        ram.remove_id(1);
        assert_eq!((ram.len(), ram.bytes()), (1, 400));
        assert!(ram.contains(&Key { id: 2, w: 10, h: 10 }));
    }

    #[test]
    fn keys_are_in_device_pixels() {
        assert_eq!(Key::new(1, 128.0, 72.0, 96.0), Key { id: 1, w: 128, h: 72 });
        assert_eq!(Key::new(1, 128.0, 72.0, 144.0), Key { id: 1, w: 192, h: 108 });
        assert_ne!(Key::new(1, 128.0, 72.0, 96.0), Key::new(1, 128.0, 72.0, 120.0), "another DPI is another thumbnail");
    }

    #[test]
    fn the_ram_cache_keeps_to_its_budget_dropping_the_least_recently_used() {
        let mut ram = Ram::default();
        let none = HashSet::new();
        for id in 1..=3 {
            ram.insert(key(id), px(10), 120, &none); // 40 bytes each
        }
        assert_eq!((ram.len(), ram.bytes()), (3, 120));
        ram.get(&key(1)); // 1 is now the most recent
        ram.insert(key(4), px(10), 120, &none);
        assert!(ram.contains(&key(1)) && !ram.contains(&key(2)) && ram.contains(&key(4)));
        assert!(ram.bytes() <= 120);
    }

    #[test]
    fn what_is_on_screen_is_never_evicted() {
        let mut ram = Ram::default();
        let on_screen: HashSet<Key> = [key(1), key(2)].into();
        ram.insert(key(1), px(10), 80, &on_screen);
        ram.insert(key(2), px(10), 80, &on_screen);
        ram.insert(key(3), px(10), 80, &on_screen);
        assert!(ram.contains(&key(1)) && ram.contains(&key(2)), "visible tiles stay even over budget");
        assert!(ram.contains(&key(3)), "the new entry stays");
        ram.insert(key(4), px(10), 80, &on_screen);
        assert!(!ram.contains(&key(3)));
        ram.clear();
        assert_eq!((ram.len(), ram.bytes()), (0, 0));
    }

    #[test]
    fn replacing_an_entry_counts_its_bytes_once() {
        let mut ram = Ram::default();
        ram.insert(key(1), px(10), 1000, &HashSet::new());
        ram.insert(key(1), px(20), 1000, &HashSet::new());
        assert_eq!((ram.len(), ram.bytes()), (1, 80));
    }

    #[test]
    fn the_queue_serves_wanted_work_once_and_forgets_finished_work() {
        let req = |id| Req { key: key(id), path: PathBuf::new(), kind: MediaKind::Picture };
        let mut q = Queue::default();
        q.wanted[0] = vec![req(1), req(2)];
        q.wanted[1] = vec![req(2), req(3)];
        assert_eq!(q.next().map(|r| r.key.id), Some(1));
        assert_eq!(q.next().map(|r| r.key.id), Some(2));
        assert_eq!(q.next().map(|r| r.key.id), Some(3), "busy work is not handed out twice");
        assert!(q.next().is_none());
        q.done(&key(2));
        assert!(!q.wants(&key(2)), "finished work leaves every list");
        q.wanted[0].clear(); // the window scrolled: 1 is no longer wanted
        assert!(!q.wants(&key(1)));
        assert!(q.wants(&key(3)));
    }

    #[test]
    fn budgets_follow_the_resource_profile() {
        assert_eq!(budget(Profile::LessMemory), 8 << 20);
        assert_eq!(budget(Profile::OpenFaster), 16 << 20);
    }
}
