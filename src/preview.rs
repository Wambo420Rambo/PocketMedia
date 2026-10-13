//! Moving previews: GIFs loop while their tile is visible, and one muted video
//! plays while the pointer rests on its tile (Video preview: Hover only).
//!
//! One thread animates the visible GIFs: it keeps each one's decoder and
//! canvas, makes the next frame when it is due and otherwise waits for a
//! command, so nothing runs while no GIF is visible. A second thread plays the
//! hovered video. Both hand the UI thread only the latest frame per tile; when
//! a window hides, or the tile scrolls away or loses the pointer, its work
//! stops and its decoder is released.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::gif::{self, Composer, GifInfo};
use crate::pixels::{self, Pixels, Source};
use crate::thumbs::{self, Key, Win};
use crate::video::Reader;
use crate::wic::{GifFrames, Wic};

/// GIF canvases above this many pixels are shown as a still first frame (a
/// 2048 x 2048 canvas already needs 16 MiB to compose).
const MAX_GIF_CANVAS: u64 = 2048 * 2048;
/// The pointer must rest this long on a video tile before it plays, so sweeping
/// across the gallery does not start players.
const HOVER_DELAY: Duration = Duration::from_millis(250);
/// GIF frames fall due on a shared 20 ms grid, so frames of several GIFs are
/// handed over (and painted) together instead of one wake-up each.
const GIF_TICK_MS: u128 = 20;

/// `t` rounded up to the next grid line after `epoch`.
fn on_grid(t: Instant, epoch: Instant) -> Instant {
    let ms = t.saturating_duration_since(epoch).as_millis().div_ceil(GIF_TICK_MS) * GIF_TICK_MS;
    epoch + Duration::from_millis(ms as u64)
}

// ---------------------------------------------------------------- GIF frames

/// A GIF being played: its frame table, WIC decoder and canvas.
pub struct GifAnimation<'w> {
    frames: GifFrames<'w>,
    info: GifInfo,
    composer: Composer,
    count: usize,
    next: usize,
    out: (u32, u32),
}

impl<'w> GifAnimation<'w> {
    pub fn open(wic: &'w Wic, path: &Path, max_w: u32, max_h: u32) -> Result<Self, String> {
        let file = File::open(path).map_err(|e| format!("could not be opened: {e}"))?;
        let info = gif::read_info(&mut BufReader::new(file))?;
        if info.width as u64 * info.height as u64 > MAX_GIF_CANVAS {
            return Err(format!("{} x {} is too large to animate", info.width, info.height));
        }
        let frames = wic.gif(path)?;
        let out = pixels::fit(info.width, info.height, max_w, max_h);
        let composer = Composer::new(info.width, info.height);
        let count = info.frames.len();
        Ok(Self { frames, info, composer, count, next: 0, out })
    }

    pub fn frame_count(&self) -> usize {
        self.count
    }

    /// The next frame at thumbnail size, and how long it shows.
    pub fn next(&mut self) -> Result<(Pixels, u32), String> {
        let i = self.next;
        if i == 0 {
            self.composer.reset();
        }
        let raw = self.frames.frame(i as u32)?;
        let mut f = self.info.frames[i];
        // WIC's own size for the frame's pixels wins over the header's.
        f.width = raw.width;
        f.height = raw.height;
        self.composer.draw(&f, &raw.data);
        self.next = (i + 1) % self.count;
        let (w, h) = (self.info.width, self.info.height);
        let src = Source { data: self.composer.canvas(), width: w, height: h, stride: w as usize * 4, opaque: false, bottom_up: false };
        let px = pixels::scale(&src, self.out.0, self.out.1).ok_or("the GIF has no pixels")?;
        Ok((px, f.delay_ms))
    }
}

/// A GIF's thumbnail: its first frame composed on the canvas. Canvases too
/// large to compose use WIC's first frame directly.
pub fn gif_thumb(wic: &Wic, path: &Path, max_w: u32, max_h: u32) -> Result<Pixels, String> {
    match GifAnimation::open(wic, path, max_w, max_h) {
        Ok(mut g) => g.next().map(|(px, _)| px),
        Err(_) => wic.picture_thumb(path, max_w, max_h),
    }
}

// ---------------------------------------------------------------- shared with the threads

/// The latest frame per tile; None means "show the still thumbnail again".
static FRAMES: Mutex<Vec<(Key, Option<Pixels>)>> = Mutex::new(Vec::new());
static NOTIFIED: AtomicBool = AtomicBool::new(false);
/// Counters for the self-test log.
pub static GIF_FRAMES: AtomicU64 = AtomicU64::new(0);
pub static GIFS_PLAYING: AtomicUsize = AtomicUsize::new(0);
pub static VIDEO_FRAMES: AtomicU64 = AtomicU64::new(0);
pub static VIDEO_PLAYING: AtomicBool = AtomicBool::new(false);

fn publish(key: Key, frame: Option<Pixels>) {
    if let Ok(mut f) = FRAMES.lock() {
        f.retain(|(k, _)| *k != key);
        f.push((key, frame));
    }
    if !NOTIFIED.swap(true, Ordering::AcqRel) {
        crate::jobs::notify(crate::jobs::WM_PREVIEW);
    }
}

enum GifCmd {
    Set(Vec<(Key, PathBuf)>),
    Quit,
}

struct Anim<'w> {
    key: Key,
    gif: GifAnimation<'w>,
    due: Instant,
}

fn gif_thread(rx: Receiver<GifCmd>) {
    let _com = crate::wic::ComThread::init();
    let Ok(wic) = Wic::new() else { return };
    let mut anims: Vec<Anim> = Vec::new();
    // Single-frame or unreadable GIFs in the current set: not opened again.
    let mut still: HashSet<Key> = HashSet::new();
    let epoch = Instant::now();
    loop {
        GIFS_PLAYING.store(anims.len(), Ordering::Relaxed);
        let due = anims.iter().map(|a| a.due).min();
        let cmd = match due {
            None => match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            },
            Some(due) => match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            },
        };
        match cmd {
            Some(GifCmd::Quit) => return,
            Some(GifCmd::Set(list)) => {
                let keys: HashSet<Key> = list.iter().map(|(k, _)| *k).collect();
                anims.retain(|a| keys.contains(&a.key)); // dropping one releases its decoder
                still.retain(|k| keys.contains(k));
                let now = Instant::now();
                for (key, path) in list {
                    if still.contains(&key) || anims.iter().any(|a| a.key == key) {
                        continue;
                    }
                    match GifAnimation::open(&wic, &path, key.w, key.h) {
                        Ok(gif) if gif.frame_count() > 1 => anims.push(Anim { key, gif, due: now }),
                        _ => {
                            still.insert(key);
                        }
                    }
                }
            }
            None => {
                let now = Instant::now();
                let mut broken = Vec::new();
                for a in anims.iter_mut().filter(|a| a.due <= now) {
                    match a.gif.next() {
                        Ok((px, delay)) => {
                            publish(a.key, Some(px));
                            GIF_FRAMES.fetch_add(1, Ordering::Relaxed);
                            let next = a.due + Duration::from_millis(delay as u64);
                            // Late (a busy machine): carry on from now instead of catching up.
                            let next = if next < now { now + Duration::from_millis(delay as u64) } else { next };
                            a.due = on_grid(next, epoch);
                        }
                        Err(_) => broken.push(a.key),
                    }
                }
                for key in broken {
                    anims.retain(|a| a.key != key);
                    still.insert(key);
                    publish(key, None);
                }
            }
        }
    }
}

enum VideoCmd {
    Play(Key, PathBuf),
    Stop,
    Quit,
}

fn video_thread(rx: Receiver<VideoCmd>) {
    let _com = crate::wic::ComThread::init();
    let mut next = match rx.recv() {
        Ok(c) => c,
        Err(_) => return,
    };
    loop {
        next = match next {
            VideoCmd::Quit => return,
            VideoCmd::Stop => match rx.recv() {
                Ok(c) => c,
                Err(_) => return,
            },
            VideoCmd::Play(key, path) => match rx.recv_timeout(HOVER_DELAY) {
                Ok(newer) => newer, // the pointer moved on before the delay passed
                Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {
                    VIDEO_PLAYING.store(true, Ordering::Relaxed);
                    let after = play(key, &path, &rx);
                    VIDEO_PLAYING.store(false, Ordering::Relaxed);
                    publish(key, None);
                    match after {
                        Some(c) => c,
                        None => return,
                    }
                }
            },
        };
    }
}

/// Plays `path` in a loop at its own pace until a command arrives, which is
/// returned (None: the app is ending). The reader is released on return.
fn play(key: Key, path: &Path, rx: &Receiver<VideoCmd>) -> Option<VideoCmd> {
    let wait = || rx.recv().ok();
    let Ok(mut reader) = Reader::open(path) else { return wait() };
    let mut clock: Option<(Instant, i64)> = None;
    loop {
        match reader.next_frame(key.w, key.h) {
            Ok(Some((time, px))) => {
                let (start, t0) = *clock.get_or_insert((Instant::now(), time));
                let due = start + Duration::from_micros(((time - t0).max(0) / 10) as u64);
                let left = due.saturating_duration_since(Instant::now());
                let cmd = if left.is_zero() {
                    rx.try_recv().ok()
                } else {
                    match rx.recv_timeout(left) {
                        Ok(c) => Some(c),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => return None,
                    }
                };
                if cmd.is_some() {
                    return cmd;
                }
                publish(key, Some(px));
                VIDEO_FRAMES.fetch_add(1, Ordering::Relaxed);
            }
            // The end: start again from the beginning.
            Ok(None) => {
                if reader.seek(0).is_err() {
                    return wait();
                }
                clock = None;
            }
            Err(e) => {
                crate::selftest::note(&format!("hover video stopped: {e}"));
                return wait();
            }
        }
        if clock.is_some_and(|(s, _)| s.elapsed() > Duration::from_secs(3600)) {
            clock = None; // keep the arithmetic small on very long hovers
        }
    }
}

// ---------------------------------------------------------------- UI thread

#[derive(Default)]
struct Ui {
    gifs: [Vec<(Key, PathBuf)>; 2],
    sent: Vec<Key>,
    hover: Option<(Win, Key)>,
    frames: HashMap<Key, (Rc<Pixels>, u64)>,
    gif_tx: Option<Sender<GifCmd>>,
    video_tx: Option<Sender<VideoCmd>>,
}

thread_local! {
    static UI: RefCell<Ui> = RefCell::new(Ui::default());
}

fn ui<R>(f: impl FnOnce(&mut Ui) -> R) -> R {
    UI.with(|u| f(&mut u.borrow_mut()))
}

fn spawn<C: Send + 'static>(name: &str, run: fn(Receiver<C>)) -> Option<Sender<C>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name(name.into()).spawn(move || run(rx)).ok().map(|_| tx)
}

/// The moving frame to draw for `key`, if it is playing.
pub fn frame(key: &Key) -> Option<(Rc<Pixels>, u64)> {
    ui(|u| u.frames.get(key).cloned())
}

/// The GIFs `win` shows (from its last full frame), whose first frame is ready.
pub fn set_gifs(win: Win, list: Vec<(Key, PathBuf)>) {
    let dropped = ui(|u| {
        u.gifs[win as usize] = list;
        send_gifs(u)
    });
    for k in &dropped {
        thumbs::invalidate(k);
    }
}

/// Sends the GIF thread the GIFs to animate when the set changed. Returns the
/// keys whose moving frame is no longer shown.
fn send_gifs(u: &mut Ui) -> Vec<Key> {
    let mut all: Vec<(Key, PathBuf)> = u.gifs.iter().flatten().cloned().collect();
    all.sort_by_key(|(k, _)| *k);
    all.dedup_by_key(|(k, _)| *k);
    let keys: Vec<Key> = all.iter().map(|(k, _)| *k).collect();
    if keys == u.sent {
        return Vec::new();
    }
    u.sent = keys;
    if u.gif_tx.is_none() && !all.is_empty() {
        u.gif_tx = spawn("gif-preview", gif_thread);
    }
    if let Some(tx) = &u.gif_tx {
        let _ = tx.send(GifCmd::Set(all));
    }
    let hover = u.hover.map(|(_, k)| k);
    let gone: Vec<Key> = u.frames.keys().filter(|k| !u.sent.contains(k) && Some(**k) != hover).copied().collect();
    for k in &gone {
        u.frames.remove(k);
    }
    gone
}

/// Item `id`'s file was replaced or removed outside PocketMedia: its hover
/// video stops and its GIF decoder is dropped, so a replaced GIF is opened
/// again from the new file once its thumbnail is made.
pub fn forget(id: u32) {
    release_item(id);
    let dropped = ui(|u| {
        for list in &mut u.gifs {
            list.retain(|(k, _)| k.id != id);
        }
        send_gifs(u)
    });
    for k in &dropped {
        thumbs::invalidate(k);
    }
}

/// The pointer is over a video tile of `win` (Some), or not over one (None).
pub fn hover(win: Win, target: Option<(Key, PathBuf)>) {
    let stopped = ui(|u| {
        let wanted = target.as_ref().map(|(k, _)| (win, *k));
        if u.hover == wanted || (target.is_none() && u.hover.is_some_and(|(w, _)| w != win)) {
            return None;
        }
        let old = u.hover.take().map(|(_, k)| k);
        if let Some(k) = old {
            u.frames.remove(&k);
        }
        u.hover = wanted;
        if u.video_tx.is_none() && target.is_some() {
            u.video_tx = spawn("video-preview", video_thread);
        }
        if let Some(tx) = &u.video_tx {
            let _ = tx.send(match target {
                Some((k, path)) => VideoCmd::Play(k, path),
                None => VideoCmd::Stop,
            });
        }
        old
    });
    if let Some(k) = stopped {
        thumbs::invalidate(&k);
    }
}

/// Stops the hover video if it plays item `id` and waits (at most a second)
/// until its player has closed the file, so the file can be renamed or moved
/// to the Recycle Bin. (GIFs are read through handles that allow both.)
pub fn release_item(id: u32) {
    let Some((win, _)) = ui(|u| u.hover.filter(|(_, k)| k.id == id)) else { return };
    hover(win, None);
    let end = Instant::now() + Duration::from_secs(1);
    while VIDEO_PLAYING.load(Ordering::Relaxed) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `win` was hidden: its GIFs stop, and its hover video if it had one.
pub fn hidden(win: Win) {
    if ui(|u| u.hover.is_some_and(|(w, _)| w == win)) {
        hover(win, None);
    }
    set_gifs(win, Vec::new());
}

/// Takes the latest frames from the threads and repaints just those tiles (WM_PREVIEW).
pub fn on_wake() {
    NOTIFIED.store(false, Ordering::Release);
    let frames = FRAMES.lock().map(|mut f| std::mem::take(&mut *f)).unwrap_or_default();
    let mut changed = Vec::new();
    ui(|u| {
        for (key, frame) in frames {
            let active = u.sent.contains(&key) || u.hover.is_some_and(|(_, k)| k == key);
            match frame {
                Some(px) if active => {
                    u.frames.insert(key, (Rc::new(px), thumbs::next_version()));
                }
                _ => {
                    u.frames.remove(&key);
                }
            }
            changed.push(key);
        }
    });
    for key in &changed {
        thumbs::invalidate(key);
    }
}

/// Stops both threads (Exit); players release their decoders as they stop.
pub fn shutdown() {
    ui(|u| {
        if let Some(tx) = u.gif_tx.take() {
            let _ = tx.send(GifCmd::Quit);
        }
        if let Some(tx) = u.video_tx.take() {
            let _ = tx.send(VideoCmd::Quit);
        }
        u.frames.clear();
    });
}

/// For the self-test log.
pub fn describe() -> String {
    let (gifs_wanted, hover, frames) = ui(|u| (u.sent.len(), u.hover.map(|(_, k)| k.id), u.frames.len()));
    format!(
        "preview gifs_wanted={} gifs_playing={} gif_frames={} hover={:?} video_playing={} video_frames={} frames_held={}",
        gifs_wanted,
        GIFS_PLAYING.load(Ordering::Relaxed),
        GIF_FRAMES.load(Ordering::Relaxed),
        hover,
        VIDEO_PLAYING.load(Ordering::Relaxed),
        VIDEO_FRAMES.load(Ordering::Relaxed),
        frames,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gif_frames_fall_due_on_a_shared_grid() {
        let epoch = Instant::now();
        let at = |ms: u64| epoch + Duration::from_millis(ms);
        assert_eq!(on_grid(at(0), epoch), at(0));
        assert_eq!(on_grid(at(1), epoch), at(20));
        assert_eq!(on_grid(at(20), epoch), at(20));
        assert_eq!(on_grid(at(95), epoch), at(100));
    }
}
