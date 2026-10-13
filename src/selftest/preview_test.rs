//! Preview self-test: thumbnails (made lazily, disk and RAM caches), GIFs that
//! loop only while visible, the muted hover video, the "No preview" placeholder,
//! and a stress test: import 1,000 files, scroll quickly, memory stays
//! flat, and hiding the popup stops GIF animation and video.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Gdi::{GetDC, GetPixel, ReleaseDC};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN};
use windows::Win32::UI::WindowsAndMessaging::WM_CLOSE;

use super::{
    CMD_MANAGER, CMD_MANAGER_POINT, CMD_RESET, DESCRIBE, El, Report, Session, capture_screen, foreground, guard,
    hwnd, is_visible, mouse_to, mouse_wheel, post, press, save_capture, seed, send, wait_until, window_rect,
};
use crate::media_info::fixtures;
use crate::ui::manager_view::MHit;

/// lparam 0: the popup, 1: the Manager. Puts thumbs + preview state in DESCRIBE.
pub(super) const CMD_PREVIEW_DESCRIBE: usize = 21;
/// Forgets the RAM cache (as after a restart); the disk cache stays.
pub(super) const CMD_THUMBS_RESET: usize = 22;
/// lparam 1: Hover only, 0: Off (saved, as Settings would).
pub(super) const CMD_SET_VIDEO_PREVIEW: usize = 23;
/// lparam 0: minimise the Manager, 1: restore it.
pub(super) const CMD_MANAGER_SHOW: usize = 24;

const STRESS_DIR: &str = "stress-src-v2";
const MIB: f64 = 1024.0 * 1024.0;

/// Runs on the UI thread for the commands above.
pub(super) fn on_ui_command(cmd: usize, arg: isize) -> isize {
    match cmd {
        CMD_PREVIEW_DESCRIBE => {
            let win = if arg == 0 { crate::thumbs::Win::Popup } else { crate::thumbs::Win::Manager };
            let d = format!("{} {}", crate::thumbs::describe(win), crate::preview::describe());
            if let Ok(mut out) = DESCRIBE.lock() {
                *out = d;
            }
        }
        CMD_THUMBS_RESET => crate::thumbs::reset(),
        CMD_SET_VIDEO_PREVIEW => {
            let mut s = crate::store::settings();
            s.video_preview =
                if arg != 0 { crate::settings_state::VideoPreview::HoverOnly } else { crate::settings_state::VideoPreview::Off };
            let _ = crate::store::save_settings(s);
            crate::manager::reload_settings();
        }
        CMD_MANAGER_SHOW => crate::manager::test_show_state(arg != 0),
        _ => {}
    }
    0
}

fn describe(t: &Session, win: isize) -> String {
    send(t.main, CMD_PREVIEW_DESCRIBE, win);
    DESCRIBE.lock().map(|d| d.clone()).unwrap_or_default()
}

/// A number from "name=value" in a description (-1 if missing).
fn field(d: &str, name: &str) -> i64 {
    let key = format!("{name}=");
    d.split_whitespace().find_map(|w| w.strip_prefix(&key)).and_then(|v| v.parse().ok()).unwrap_or(-1)
}

/// Waits until every drawn thumbnail of `win` is ready or has no preview.
fn settle(t: &Session, win: isize, ms: u64) -> bool {
    wait_until(ms, || {
        let d = describe(t, win);
        field(&d, "pending") == 0 && field(&d, "drawn") > 0 && field(&d, "busy") == 0
    })
}

fn private_bytes() -> u64 {
    let mut c = PROCESS_MEMORY_COUNTERS_EX { cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32, ..Default::default() };
    // SAFETY: the struct is an EX counters block of the size given; the pseudo
    // handle of the current process needs no closing.
    let ok = unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut c as *mut _ as *mut PROCESS_MEMORY_COUNTERS, c.cb) }.is_ok();
    if ok { c.PrivateUsage as u64 } else { 0 }
}

/// The colour on screen at (x, y), as (r, g, b).
fn screen_pixel(x: i32, y: i32) -> (u8, u8, u8) {
    // SAFETY: the screen DC is released right after the read.
    unsafe {
        let dc = GetDC(None);
        let c = GetPixel(dc, x, y).0;
        ReleaseDC(None, dc);
        (c as u8, (c >> 8) as u8, (c >> 16) as u8)
    }
}

fn near(a: (u8, u8, u8), b: (u8, u8, u8), tol: i32) -> bool {
    (a.0 as i32 - b.0 as i32).abs() <= tol && (a.1 as i32 - b.1 as i32).abs() <= tol && (a.2 as i32 - b.2 as i32).abs() <= tol
}

/// Which of red, blue, green `c` is (the seed GIFs' frames), if any.
fn primary(c: (u8, u8, u8)) -> Option<char> {
    match c {
        (r, g, b) if r > 200 && g < 60 && b < 60 => Some('R'),
        (r, g, b) if b > 200 && r < 60 && g < 60 => Some('B'),
        (r, g, b) if g > 200 && r < 60 && b < 60 => Some('G'),
        _ => None,
    }
}

/// The centre of the media area of a tile whose centre is `tile`: the thumbnail
/// box sits 12 DIPs above the tile's centre (Layout::thumb).
fn media_point(window: isize, tile: (i32, i32)) -> (i32, i32) {
    // SAFETY: GetDpiForWindow tolerates any HWND.
    let dpi = unsafe { GetDpiForWindow(hwnd(window)) }.max(96);
    (tile.0, tile.1 - (12.2 * dpi as f32 / 96.0).round() as i32)
}

fn popup_media(t: &Session, i: usize) -> Option<(i32, i32)> {
    t.point(El::Tile(i)).map(|p| media_point(t.popup, p))
}

/// The distinct seed-GIF colours seen at `at` over `ms`.
fn colours_over(at: (i32, i32), ms: u64) -> String {
    let mut seen = String::new();
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        if let Some(c) = primary(screen_pixel(at.0, at.1))
            && !seen.contains(c)
        {
            seen.push(c);
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    seen
}

/// A counter of `name` that stays the same for `ms` (nothing is being produced).
fn stays(t: &Session, win: isize, name: &str, ms: u64) -> (bool, String) {
    let before = field(&describe(t, win), name);
    std::thread::sleep(Duration::from_millis(ms));
    let after = describe(t, win);
    (field(&after, name) == before, format!("{name} {before} -> {} ({after})", field(&after, name)))
}

/// A counter of `name` that grows within `ms`.
fn grows(t: &Session, win: isize, name: &str, ms: u64) -> (bool, String) {
    let before = field(&describe(t, win), name);
    let ok = wait_until(ms, || field(&describe(t, win), name) > before + 2);
    let after = describe(t, win);
    (ok, format!("{name} {before} -> {} ({after})", field(&after, name)))
}

fn shot_popup(r: &mut Report, t: &Session, name: &str) {
    std::thread::sleep(Duration::from_millis(150));
    if let Some(rect) = window_rect(t.popup) {
        save_capture(r, &t.out.join("screens").join(format!("{name}.png")), capture_screen(rect));
    }
}

fn close_windows(r: &mut Report, t: &Session) {
    let manager = send(t.main, CMD_MANAGER, 0);
    if is_visible(manager) {
        post(manager, WM_CLOSE, 0, 0);
        wait_until(1500, || !is_visible(manager));
    }
    if is_visible(t.popup) && foreground() == t.popup && guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
        wait_until(1500, || !is_visible(t.popup));
    }
}

/// Opens the popup on All media with every type showing.
fn open_all_media(r: &mut Report, t: &Session) -> bool {
    if !t.open(r) {
        return false;
    }
    t.click(r, El::Chip(0));
    t.click(r, El::List);
    t.click(r, El::Row(1));
    t.park();
    t.describe().contains("list=All")
}

pub(super) fn run(r: &mut Report, t: &Session) -> bool {
    r.info("Previews");
    let _com = crate::wic::ComThread::init();
    close_windows(r, t);
    send(t.main, CMD_RESET, 0);
    let cache = t.out.join("app").join("Cache").join(crate::thumb_store::DIR);
    let _ = std::fs::remove_dir_all(&cache);
    send(t.main, CMD_THUMBS_RESET, 0);
    let videos = seed::real_videos(&t.out);
    if !videos {
        r.info("this Windows could not make H.264 test videos, so the seed videos are header-only stand-ins");
    }

    // 1. Thumbnails, cold cache.
    if !open_all_media(r, t) {
        return r.check("open the popup on All media", false, &t.describe());
    }
    let settled = settle(t, 0, 20_000);
    let d = describe(t, 0);
    let expected_none = if videos { 0 } else { -1 };
    r.check(
        "thumbnails are made for the visible tiles (cold cache)",
        settled && field(&d, "drawn") >= 6 && field(&d, "ready") >= 5,
        &d,
    );
    if expected_none == 0 {
        r.check("every seed file has a preview", field(&d, "no_preview") == 0, &d);
    }
    r.check("thumbnails were written to the disk cache", std::fs::read_dir(&cache).map(|d| d.count()).unwrap_or(0) >= 5, &cache.display().to_string());
    let made = field(&d, "made");
    if let Some(at) = popup_media(t, 1) {
        let c = screen_pixel(at.0, at.1);
        r.check("Square.png's thumbnail shows its real colour (106, 194, 46)", near(c, (106, 194, 46), 8), &format!("{c:?} at {at:?}"));
    }
    if let Some(at) = popup_media(t, 4) {
        let c = screen_pixel(at.0, at.1);
        r.check("Wallpaper.jpg's thumbnail is the decoded JPEG (dark centre square)", near(c, (31, 31, 58), 20), &format!("{c:?} at {at:?}"));
    }
    shot_popup(r, t, "phase5-popup-thumbnails");

    // 2. GIFs loop while visible.
    let (ok, detail) = grows(t, 0, "gif_frames", 3000);
    r.check("visible GIFs animate", ok && field(&describe(t, 0), "gifs_playing") >= 2, &detail);
    if let Some(at) = popup_media(t, 0) {
        let seen = colours_over(at, 1500);
        r.check("Happy dance.gif's tile changes frames on screen (red, blue, green)", seen.len() >= 2, &format!("saw {seen:?} at {at:?}"));
    }
    shot_popup(r, t, "phase5-popup-gif-frame");

    // 3. Hiding the popup stops the animation and releases the RAM cache (Use less memory).
    if !guard(r, &t.allowed) {
        return false;
    }
    press(&[VK_ESCAPE]);
    wait_until(1500, || !is_visible(t.popup));
    std::thread::sleep(Duration::from_millis(300));
    let (still, detail) = stays(t, 0, "gif_frames", 1200);
    r.check("hiding the popup stops GIF animation", still && field(&describe(t, 0), "gifs_playing") == 0, &detail);
    let d = describe(t, 0);
    r.check("with Use less memory the hidden popup lets go of its thumbnails", field(&d, "ram_bytes") == 0, &d);

    // 4. Opening again reads the disk cache: nothing is decoded again.
    if !open_all_media(r, t) {
        return false;
    }
    let settled = settle(t, 0, 10_000);
    let d = describe(t, 0);
    r.check(
        "the second time, thumbnails come from the disk cache",
        settled && field(&d, "made") == made && field(&d, "disk_hits") >= 5,
        &format!("made before {made}; {d}"),
    );

    // 5. Hover video (one, muted, only while hovered).
    if videos {
        hover_tests(r, t);
    } else {
        r.skip("hover video", "no real test videos on this machine (H.264 encoder missing)");
    }

    // 6. The Manager shows thumbnails too, and a minimised Manager stops its GIFs.
    if !manager_tests(r, t) {
        return false;
    }

    // 7. The stress test.
    stress_test(r, t)
}

fn hover_tests(r: &mut Report, t: &Session) {
    if !t.open(r) {
        return;
    }
    t.click(r, El::Chip(3)); // Video: Race clip, Celebration
    t.park();
    settle(t, 0, 10_000);
    let (Some(first), Some(second)) = (popup_media(t, 0), popup_media(t, 1)) else {
        r.check("find the video tiles", false, &t.describe());
        return;
    };
    let (ok, detail) = stays(t, 0, "video_frames", 600);
    r.check("no video plays without hovering", ok && !describe(t, 0).contains("video_playing=true"), &detail);
    if !guard(r, &t.allowed) {
        return;
    }
    mouse_to(first.0, first.1);
    let playing = wait_until(4000, || describe(t, 0).contains("video_playing=true"));
    let (ok, detail) = grows(t, 0, "video_frames", 3000);
    r.check("resting the pointer on a video tile plays it", playing && ok, &detail);
    let a = screen_pixel(first.0 - 20, first.1);
    std::thread::sleep(Duration::from_millis(500));
    let b = screen_pixel(first.0 - 20, first.1);
    r.check("the hovered tile shows moving frames", a != b, &format!("{a:?} then {b:?}"));
    shot_popup(r, t, "phase5-hover-video");
    r.check("playing does not move the tiles", popup_media(t, 0) == Some(first) && popup_media(t, 1) == Some(second), "");

    mouse_to(second.0, second.1);
    let switched = wait_until(3000, || describe(t, 0).contains("hover=Some(6)") && describe(t, 0).contains("video_playing=true"));
    r.check("moving to the other video plays that one instead (at most one plays)", switched, &describe(t, 0));

    t.park();
    let stopped = wait_until(1500, || describe(t, 0).contains("video_playing=false"));
    let (ok, detail) = stays(t, 0, "video_frames", 700);
    r.check("the video stops when hover ends", stopped && ok && describe(t, 0).contains("hover=None"), &detail);

    mouse_to(first.0, first.1);
    wait_until(4000, || describe(t, 0).contains("video_playing=true"));
    if guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup));
    let stopped = wait_until(1500, || describe(t, 0).contains("video_playing=false"));
    let (ok, detail) = stays(t, 0, "video_frames", 700);
    r.check("hiding the popup stops the hover video", stopped && ok, &detail);

    // Video preview: Off.
    send(t.main, CMD_SET_VIDEO_PREVIEW, 0);
    if t.open(r) {
        t.click(r, El::Chip(3));
        mouse_to(first.0, first.1);
        std::thread::sleep(Duration::from_millis(1200));
        let d = describe(t, 0);
        r.check("with Video preview Off, hovering plays nothing", d.contains("video_playing=false") && d.contains("hover=None"), &d);
        t.park();
    }
    send(t.main, CMD_SET_VIDEO_PREVIEW, 1);
    t.click(r, El::Chip(0));
}

fn manager_tests(r: &mut Report, t: &Session) -> bool {
    if !t.open(r) || !t.click(r, El::OpenManager) {
        return false;
    }
    let manager = send(t.main, CMD_MANAGER, 0);
    if !r.check("the Manager opens", wait_until(2000, || is_visible(manager) && foreground() == manager), "") {
        return false;
    }
    // Select the first tile (Happy dance.gif): the inspector shows it too.
    if let Some((x, y)) = super::unpack(send(t.main, CMD_MANAGER_POINT, super::manager_test::code(MHit::Tile(0))))
        && guard(r, &t.allowed)
    {
        mouse_to(x, y);
        super::mouse_button(true);
        super::mouse_button(false);
    }
    if let Some((x, y)) = super::unpack(send(t.main, CMD_MANAGER_POINT, super::manager_test::code(MHit::None))) {
        mouse_to(x, y);
    }
    let settled = settle(t, 1, 15_000);
    let d = describe(t, 1);
    let videos_ok = !seed::real_videos(&t.out) || field(&d, "no_preview") == 0;
    r.check(
        "the Manager's gallery and inspector show thumbnails",
        settled && field(&d, "drawn") >= 9 && field(&d, "ready") + field(&d, "no_preview") == field(&d, "drawn") && videos_ok,
        &d,
    );
    std::thread::sleep(Duration::from_millis(400));
    if let Some(rect) = window_rect(manager) {
        save_capture(r, &t.out.join("screens").join("phase5-manager-thumbnails.png"), capture_screen(rect));
    }
    let (ok, detail) = grows(t, 1, "gif_frames", 3000);
    r.check("GIFs animate in the Manager", ok, &detail);

    send(t.main, CMD_MANAGER_SHOW, 0);
    std::thread::sleep(Duration::from_millis(500));
    let (still, detail) = stays(t, 1, "gif_frames", 1200);
    r.check("a minimised Manager stops its GIFs", still && field(&describe(t, 1), "gifs_playing") == 0, &detail);
    send(t.main, CMD_MANAGER_SHOW, 1);
    let (ok, detail) = grows(t, 1, "gif_frames", 4000);
    r.check("restoring the Manager starts them again", ok, &detail);
    post(manager, WM_CLOSE, 0, 0);
    wait_until(1500, || !is_visible(manager));
    let (still, detail) = stays(t, 1, "gif_frames", 1000);
    r.check("closing the Manager stops its GIFs", still, &detail);
    true
}

/// The stress set: 600 JPEGs, 100 PNGs, 200 animated GIFs, 50 real MP4s and 50
/// MP4s with no playable video (made once, kept between runs).
fn make_stress(out: &Path) -> Result<PathBuf, String> {
    let dir = out.join(STRESS_DIR);
    // The marker sits beside the folder, so the import does not see it.
    let done = out.join(format!("{STRESS_DIR}.done"));
    if done.exists() {
        return Ok(dir);
    }
    let _ = std::fs::remove_dir_all(&dir);
    let sub = |name: &str| {
        let d = dir.join(name);
        std::fs::create_dir_all(&d).map(|()| d).map_err(|e| e.to_string())
    };
    let (photos, pictures, gifs, clips) = (sub("photos")?, sub("pictures")?, sub("gifs")?, sub("clips")?);
    let write = |p: PathBuf, b: Vec<u8>| std::fs::write(p, b).map_err(|e| e.to_string());
    for i in 0..600u32 {
        let p = photos.join(format!("photo {i:03}.jpg"));
        if super::media_gen::jpeg(&p, 640, 480, i + 1000).is_err() {
            write(p, fixtures::jpeg(640, 480, &format!("photo {i}")))?;
        }
    }
    for i in 0..100u32 {
        let c = [(i * 37 % 256) as u8, (i * 11 % 256) as u8, 200, 255];
        let rgba: Vec<u8> = c.iter().copied().cycle().take(96 * 64 * 4).collect();
        write(pictures.join(format!("picture {i:03}.png")), crate::test_png::encode_rgba(96, 64, &rgba))?;
    }
    for i in 0..200u32 {
        let frames: Vec<fixtures::GifFrame> =
            (0..3).map(|f| fixtures::GifFrame::solid(0, 0, 120, 90, (5 + (i + f * 40) % 120) as u8, 20, 1, None)).collect();
        write(gifs.join(format!("loop {i:03}.gif")), fixtures::gif_frames(120, 90, &frames, &format!("stress {i}")))?;
    }
    for i in 0..50u32 {
        let p = clips.join(format!("clip {i:03}.mp4"));
        if super::media_gen::mp4(&p, 160, 90, 10, 10, i + 3000).is_err() {
            write(p, fixtures::mp4(160, 90, &format!("clip {i}")))?;
        }
    }
    for i in 0..50u32 {
        write(clips.join(format!("unsupported {i:03}.mp4")), fixtures::mp4(320, 180, &format!("unsupported {i}")))?;
    }
    std::fs::write(&done, b"1000 files").map_err(|e| e.to_string())?;
    Ok(dir)
}

fn mib(b: u64) -> String {
    format!("{:.1} MiB", b as f64 / MIB)
}

/// Scrolls the popup's gallery `notches` wheel notches (negative = down), fast:
/// 20 notches per event, 25 ms apart. Returns the highest private bytes seen.
fn fast_scroll(r: &mut Report, t: &Session, notches: i32) -> Option<u64> {
    let (x, y) = t.point(El::GalleryGap)?;
    if !guard(r, &t.allowed) {
        return None;
    }
    mouse_to(x, y);
    std::thread::sleep(Duration::from_millis(80));
    let mut peak = private_bytes();
    let mut left = notches.abs();
    while left > 0 {
        if !guard(r, &t.allowed) {
            return None;
        }
        let n = left.min(20);
        mouse_wheel(120 * n * notches.signum());
        left -= n;
        std::thread::sleep(Duration::from_millis(25));
        peak = peak.max(private_bytes());
    }
    Some(peak)
}

/// Jumps the popup's gallery to the end (`down`) or the top in one go: several
/// big wheel events in a single input batch, handled before the next paint, so
/// the tiles in between are never drawn.
fn jump(r: &mut Report, t: &Session, down: bool) -> bool {
    let Some((x, y)) = t.point(El::GalleryGap) else { return false };
    if !guard(r, &t.allowed) {
        return false;
    }
    mouse_to(x, y);
    std::thread::sleep(Duration::from_millis(80));
    if !guard(r, &t.allowed) {
        return false;
    }
    let delta = if down { -120 * 270 } else { 120 * 270 };
    let inputs: Vec<_> = (0..6)
        .map(|_| {
            let mut i = super::mouse(windows::Win32::UI::Input::KeyboardAndMouse::MOUSEEVENTF_WHEEL, 0, 0);
            i.Anonymous.mi.mouseData = delta as u32;
            i
        })
        .collect();
    super::send_inputs(&inputs);
    true
}

fn stress_test(r: &mut Report, t: &Session) -> bool {
    let started = Instant::now();
    let src = match make_stress(&t.out) {
        Ok(d) => d,
        Err(e) => return r.check("make the 1,000-file stress set", false, &e),
    };
    r.info(&format!("stress set ready in {:.1} s: {}", started.elapsed().as_secs_f32(), src.display()));
    crate::dialogs::set_test_pick(Vec::new(), src);

    // Import it through Add media, as a user would.
    if !t.open(r) {
        return false;
    }
    t.click(r, El::AddMediaButton);
    t.click(r, El::AddImportExisting);
    let reviewed = t.wait_state("stage=review", 120_000);
    r.check("the 1,000 files are reviewed", reviewed && t.describe().contains("can_add=true"), &t.describe().chars().take(300).collect::<String>());
    let started = Instant::now();
    t.click(r, El::AddAdd);
    let imported = t.wait_state("screen=gallery", 300_000);
    let d = t.describe();
    r.check("1,000 files imported", imported && d.contains("1000 files added"), &d.chars().take(300).collect::<String>());
    r.info(&format!("import took {:.1} s", started.elapsed().as_secs_f32()));

    if !open_all_media(r, t) {
        return false;
    }
    settle(t, 0, 20_000);
    std::thread::sleep(Duration::from_millis(500));
    let d = describe(t, 0);
    let made0 = field(&d, "made");
    let baseline = private_bytes();
    r.info(&format!("All media: {}; private bytes {}", t.describe().split(" names=").next().unwrap_or(""), mib(baseline)));

    // A jump to the end: only the tiles shown there get thumbnails.
    if !jump(r, t, true) {
        return r.check("jump to the end of the gallery", false, "input stopped");
    }
    settle(t, 0, 20_000);
    let d = describe(t, 0);
    let made_jump = field(&d, "made") - made0;
    r.check(
        "jumping to the end makes thumbnails only for the tiles shown there",
        (0..=30).contains(&made_jump) && t.describe().contains("count=1008"),
        &format!("{made_jump} made; {d}"),
    );
    jump(r, t, false);
    settle(t, 0, 20_000);

    // Down to the end and back, fast, twice.
    let mut peak = baseline;
    let mut after = Vec::new();
    let mut queued_max = 0;
    for pass in 1..=2 {
        for notches in [-1100, 1100] {
            let made_before = field(&describe(t, 0), "made");
            let Some(p) = fast_scroll(r, t, notches) else {
                return r.check("scroll the popup quickly", false, "input stopped");
            };
            peak = peak.max(p);
            // Right after the scroll the queue must hold only what is on screen now.
            let queued = field(&describe(t, 0), "wanted");
            queued_max = queued_max.max(queued);
            settle(t, 0, 20_000);
            std::thread::sleep(Duration::from_millis(400));
            after.push(private_bytes());
            let d = describe(t, 0);
            r.info(&format!(
                "pass {pass}, {}: {} made on the way; queued right after {queued}; private {} ({d})",
                if notches < 0 { "to the end" } else { "to the top" },
                field(&d, "made") - made_before,
                mib(*after.last().unwrap_or(&0))
            ));
        }
    }
    let d = describe(t, 0);
    r.check(
        "when fast scrolling stops, only the tiles on screen are still queued",
        (0..=30).contains(&queued_max),
        &format!("at most {queued_max} queued right after a scroll"),
    );
    r.check("the RAM cache stays within its 8 MiB budget", (0..=8 << 20).contains(&field(&d, "ram_bytes")), &d);
    let last = *after.last().unwrap_or(&0);
    let first_top = after.get(1).copied().unwrap_or(last);
    let growth = last as i64 - baseline as i64;
    let repeat = last as i64 - first_top as i64;
    r.info(&format!(
        "memory: start {}, peak {}, after pass 1 {}, after pass 2 {}",
        mib(baseline),
        mib(peak),
        mib(first_top),
        mib(last)
    ));
    r.check("memory stays flat while scrolling 1,000 files (growth under 32 MiB)", growth < 32 << 20, &format!("{:+.1} MiB", growth as f64 / MIB));
    r.check("scrolling through again adds nothing (under 8 MiB)", repeat < 8 << 20, &format!("{:+.1} MiB", repeat as f64 / MIB));
    let cache_bytes: u64 = std::fs::read_dir(t.out.join("app").join("Cache").join(crate::thumb_store::DIR))
        .map(|d| d.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum())
        .unwrap_or(0);
    r.check("the disk cache stays under 128 MiB", cache_bytes <= crate::thumb_store::CAP, &mib(cache_bytes));

    // A video Windows cannot play shows a truthful placeholder and still pastes.
    t.click(r, El::Search);
    t.select_all_and_type(r, "unsupported");
    settle(t, 0, 15_000);
    let d = describe(t, 0);
    r.check("unsupported videos show the No preview placeholder", field(&d, "drawn") >= 3 && field(&d, "no_preview") == field(&d, "drawn"), &d);
    shot_popup(r, t, "phase5-no-preview");
    let formats = t.out.join("paste-formats.txt");
    let _ = std::fs::remove_file(&formats);
    if guard(r, &t.allowed) {
        press(&[VK_RETURN]);
    }
    let got = wait_until(4000, || formats.exists());
    let text = std::fs::read_to_string(&formats).unwrap_or_default();
    r.check("…and pasting one still sends the file", got && text.lines().any(|l| l == "CF_HDROP"), &text.replace('\n', ", "));

    // Hiding stops everything and frees the thumbnails.
    if open_all_media(r, t) {
        settle(t, 0, 10_000);
        let _ = grows(t, 0, "gif_frames", 2000);
        if guard(r, &t.allowed) {
            press(&[VK_ESCAPE]);
        }
        wait_until(1500, || !is_visible(t.popup));
        std::thread::sleep(Duration::from_millis(300));
        let (still, detail) = stays(t, 0, "gif_frames", 1500);
        let d = describe(t, 0);
        r.check(
            "after the stress test, hiding the popup stops GIFs and video",
            still && d.contains("video_playing=false") && field(&d, "gifs_playing") == 0,
            &detail,
        );
        r.check("…and releases the RAM cache", field(&d, "ram_bytes") == 0, &d);
        std::thread::sleep(Duration::from_millis(1500));
        r.info(&format!("private bytes with the popup hidden: {}", mib(private_bytes())));
    }
    true
}
