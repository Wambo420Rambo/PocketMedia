//! Download from Twitter on a worker thread: for each post link, ask Twitter
//! for the post's details, download its pictures, videos and GIFs into a temporary
//! folder, and turn each GIF (an MP4 on Twitter) into a .gif file. The panel then
//! imports the folder's files the usual way (review, names, copy into the
//! library); the temporary folder is removed afterwards and at the next start.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::gif_writer::GifWriter;
use crate::http;
use crate::jobs::{self, Event, Job};
use crate::twitter::{self, MediaType};

/// A GIF made from a Twitter GIF fits in this square (it is never enlarged).
const GIF_MAX_SIDE: u32 = 640;
/// At most 25 frames a second, in 100 ns units.
const GIF_MIN_FRAME: i64 = 400_000;
/// Longer than this (a minute at 25 frames a second) stays an MP4 video.
const GIF_MAX_FRAMES: usize = 1500;
/// A post's details are a few kilobytes.
const DETAILS_LIMIT: usize = 4 * 1024 * 1024;
/// Progress is reported at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Which link (0-based) of how many.
    pub post: usize,
    pub posts: usize,
    /// Bytes of the current video so far, and its size if known.
    pub done: u64,
    pub total: Option<u64>,
    /// Turning the current GIF from MP4 into .gif.
    pub converting: bool,
}

#[derive(Debug)]
pub struct Downloaded {
    /// The temporary folder holding `files`; removed when the panel is done with it.
    pub dir: PathBuf,
    pub files: Vec<PathBuf>,
    /// Links (by their row) that gave nothing, and why.
    pub problems: Vec<(usize, String)>,
}

#[derive(Debug)]
pub enum Step {
    Progress(Progress),
    /// Everything that could be downloaded is in `dir`; Err: nothing was, and why
    /// (`http::STOPPED` after Cancel).
    Done(Result<Downloaded, String>),
}

/// Where downloads wait to be imported: `%TEMP%\PocketMedia-download`.
fn temp_root() -> PathBuf {
    std::env::temp_dir().join("PocketMedia-download")
}

/// Removes downloads left over from an earlier run (only one PocketMedia runs
/// at a time). Called on a worker thread at start-up.
pub fn cleanup_stale() {
    let _ = std::fs::remove_dir_all(temp_root());
}

/// Removes a download's temporary folder off the UI thread.
pub fn discard(dir: PathBuf) {
    let _ = std::thread::Builder::new().name("download-cleanup".into()).spawn(move || {
        let _ = std::fs::remove_dir_all(dir);
    });
}

/// Starts downloading `links`: (row, post number) pairs. Results arrive as `Event::Download`.
pub fn start(links: Vec<(usize, u64)>) -> Result<Job, String> {
    jobs::spawn("download", move |job, cancel| {
        let result = run(job, &links, cancel);
        jobs::post(Event::Download { job, step: Step::Done(result) });
    })
}

fn run(job: u64, links: &[(usize, u64)], cancel: &AtomicBool) -> Result<Downloaded, String> {
    let _com = crate::wic::ComThread::init();
    let dir = temp_root().join(format!("{}-{job}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not make a folder for the download: {e}"))?;
    let mut files = Vec::new();
    let mut problems = Vec::new();
    let mut last_report = Instant::now() - PROGRESS_EVERY;
    let mut report = |p: Progress, force: bool| {
        if force || last_report.elapsed() >= PROGRESS_EVERY {
            last_report = Instant::now();
            jobs::post(Event::Download { job, step: Step::Progress(p) });
        }
    };
    for (k, &(row, id)) in links.iter().enumerate() {
        let mut p = Progress { post: k, posts: links.len(), done: 0, total: None, converting: false };
        report(p, true);
        let outcome = post_files(id, &dir, cancel, &mut |done, total, converting| {
            p.done = done;
            p.total = total;
            p.converting = converting;
            report(p, converting);
        });
        if cancel.load(Ordering::Relaxed) {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(http::STOPPED.into());
        }
        match outcome {
            Ok(mut got) => files.append(&mut got),
            Err(why) => problems.push((row, why)),
        }
    }
    if files.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok(Downloaded { dir, files, problems })
}

/// Downloads post `id`'s pictures, videos and GIFs into `dir`. Err when none could be.
fn post_files(id: u64, dir: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(u64, Option<u64>, bool)) -> Result<Vec<PathBuf>, String> {
    let (status, body) = http::get_small(twitter::API_HOST, &twitter::details_path(id), DETAILS_LIMIT, cancel)?;
    let post = match status {
        200 => twitter::parse_post(id, &body)?,
        404 => return Err(twitter::NOT_FOUND.into()),
        429 => return Err("Twitter is limiting requests right now. Try again in a few minutes.".into()),
        n => return Err(format!("Twitter did not send the post (error {n}).")),
    };
    let mut files = Vec::new();
    let mut first_error = None;
    for (i, media) in post.media.iter().enumerate() {
        let base = twitter::file_base(&post, i, post.media.len());
        match media_file(media, &base, dir, cancel, progress) {
            Ok(path) => files.push(path),
            Err(e) if cancel.load(Ordering::Relaxed) => return Err(e),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    match (files.is_empty(), first_error) {
        (true, Some(e)) => Err(e),
        _ => Ok(files),
    }
}

/// Downloads one picture, video or GIF. A GIF that can't be turned into a .gif stays an MP4.
fn media_file(
    media: &twitter::Media,
    base: &str,
    dir: &Path,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64, Option<u64>, bool),
) -> Result<PathBuf, String> {
    let path = dir.join(format!("{base}.{}", media.ext));
    let limit = if media.kind == MediaType::Photo { twitter::MAX_PHOTO_BYTES } else { twitter::MAX_VIDEO_BYTES };
    let saved = (|| {
        let file = std::fs::File::create(&path).map_err(|e| format!("Could not save the download: {e}"))?;
        let mut out = std::io::BufWriter::new(file);
        http::download(&media.url, &mut out, limit, cancel, |done, total| progress(done, total, false))?;
        std::io::Write::flush(&mut out).map_err(|e| format!("Could not save the download: {e}"))
    })();
    if let Err(e) = saved {
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    if media.kind != MediaType::Gif {
        return Ok(path);
    }
    progress(0, None, true);
    let gif = dir.join(format!("{base}.gif"));
    match to_gif(&path, &gif, cancel) {
        Ok(()) => {
            let _ = std::fs::remove_file(&path);
            Ok(gif)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&gif);
            if cancel.load(Ordering::Relaxed) { Err(e) } else { Ok(path) }
        }
    }
}

/// Hundredths of a second, rounded, from 100 ns units.
fn centiseconds(t: i64) -> i64 {
    (t + 50_000).div_euclid(100_000)
}

/// How long a frame shows: the time to the next one, in GIF's hundredths.
fn delay(from: i64, to: i64) -> u16 {
    (centiseconds(to) - centiseconds(from)).clamp(2, u16::MAX as i64) as u16
}

/// Writes `mp4`'s frames to `gif`, at most 25 a second and at most 640 pixels a side.
fn to_gif(mp4: &Path, gif: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let mut reader = crate::video::Reader::open(mp4)?;
    let end = reader.duration();
    let file = std::fs::File::create(gif).map_err(|e| e.to_string())?;
    let mut writer: Option<GifWriter<std::io::BufWriter<std::fs::File>>> = None;
    let mut pending: Option<(i64, crate::pixels::Pixels)> = None;
    let mut frames = 0usize;
    let mut file = Some(file);
    while let Some((t, px)) = reader.next_frame(GIF_MAX_SIDE, GIF_MAX_SIDE)? {
        if cancel.load(Ordering::Relaxed) {
            return Err(http::STOPPED.into());
        }
        if pending.as_ref().is_some_and(|(pt, _)| t - pt < GIF_MIN_FRAME) {
            continue;
        }
        if let Some((pt, prev)) = pending.take() {
            put(&mut writer, &mut file, &prev, delay(pt, t))?;
            frames += 1;
            if frames > GIF_MAX_FRAMES {
                return Err("too long for a GIF".into());
            }
        }
        pending = Some((t, px));
    }
    let (pt, last) = pending.ok_or("the GIF has no frames")?;
    let last_end = end.filter(|e| *e > pt).unwrap_or(pt + 1_000_000);
    put(&mut writer, &mut file, &last, delay(pt, last_end))?;
    writer.ok_or("the GIF has no frames")?.finish().map_err(|e| e.to_string())?;
    Ok(())
}

/// Adds a frame, starting the file at the first one. Every frame must be the
/// size of the first.
fn put(
    writer: &mut Option<GifWriter<std::io::BufWriter<std::fs::File>>>,
    file: &mut Option<std::fs::File>,
    px: &crate::pixels::Pixels,
    delay_cs: u16,
) -> Result<(), String> {
    if writer.is_none() {
        let f = file.take().ok_or("the GIF could not be written")?;
        let (w, h) = (u16::try_from(px.width).map_err(|_| "too large")?, u16::try_from(px.height).map_err(|_| "too large")?);
        *writer = Some(GifWriter::new(std::io::BufWriter::new(f), w, h).map_err(|e| e.to_string())?);
    }
    let w = writer.as_mut().ok_or("the GIF could not be written")?;
    if (px.width, px.height) != w.size() {
        return Err("the video changes size".into());
    }
    w.frame(&px.data, delay_cs).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_delays_follow_the_video_clock_without_drifting() {
        // 30 frames a second, every other frame kept by the 25-a-second limit:
        // the delays alternate so the total stays right.
        let times: Vec<i64> = (0..10).map(|i| i * 333_333 * 2).collect();
        let delays: Vec<u16> = times.windows(2).map(|w| delay(w[0], w[1])).collect();
        let total: i64 = delays.iter().map(|d| *d as i64).sum();
        assert_eq!(total, centiseconds(times[9]) - centiseconds(times[0]));
        assert!(delays.iter().all(|d| (6..=7).contains(d)), "{delays:?}");
        assert_eq!(delay(0, 0), 2, "never shorter than browsers show");
    }
}
