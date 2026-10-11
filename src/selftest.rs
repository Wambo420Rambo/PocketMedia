//! `--self-test <dir>`: drives the real app with real Windows input and records
//! PASS/FAIL lines plus screenshots in `<dir>`.
//!
//! A worker thread injects keyboard/mouse input and watches the result; anything
//! that must touch UI-thread state goes through `WM_SELFTEST` messages. Input is
//! only injected while PocketMedia or its own test-target window is in front, so
//! the test can never type or click into another app.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, HBITMAP, HDC, HGDIOBJ,
    ReleaseDC, SRCCOPY, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VIRTUAL_KEY,
    VK_A, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LWIN, VK_MENU, VK_OEM_PERIOD, VK_RETURN, VK_RIGHT, VK_TAB,
};
use windows::Win32::UI::Shell::{NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR};

use crate::ui::layout::Hit;
use crate::{StatusKind, WM_SELFTEST, app, assets, exit, popup, test_target};

const CMD_TARGET: usize = 1;
const CMD_STATUS: usize = 2;
const CMD_FOCUS: usize = 3;
const CMD_POPUP: usize = 4;
const CMD_EXIT: usize = 5;
const CMD_HOTKEY_OK: usize = 6;
const CMD_POINT: usize = 7;
const CMD_DESCRIBE: usize = 8;
const CONFLICT_TITLE: &str = "PocketMedia - shortcut not available";

static EXIT_CODE: AtomicI32 = AtomicI32::new(0);
static ENABLED: AtomicBool = AtomicBool::new(false);
static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static STATUS: Mutex<(String, i32)> = Mutex::new((String::new(), 0));
static DESCRIBE: Mutex<String> = Mutex::new(String::new());

/// Popup elements the test can point at, encoded for `CMD_POINT`.
#[derive(Clone, Copy, Debug)]
enum El {
    Neutral,
    Search,
    Chip(usize),
    List,
    Row(usize),
    GalleryGap,
    ClearHistory,
    EmptyAction,
    Tile(usize),
}

impl El {
    fn code(self) -> isize {
        match self {
            El::Neutral => 0,
            El::Search => 2,
            El::Chip(i) => 10 + i as isize,
            El::List => 20,
            El::Row(i) => 30 + i as isize,
            El::GalleryGap => 40,
            El::ClearHistory => 50,
            El::EmptyAction => 60,
            El::Tile(i) => 100 + i as isize,
        }
    }

    fn hit(code: isize) -> Hit {
        match code {
            1 => Hit::Close,
            2 => Hit::Search,
            10..=13 => Hit::Chip((code - 10) as usize),
            20 => Hit::List,
            30..=31 => Hit::DropdownRow((code - 30) as usize),
            40 => Hit::Scrollbar,
            50 => Hit::ClearHistory,
            60 => Hit::EmptyAction,
            100.. => Hit::Tile((code - 100) as usize),
            _ => Hit::None,
        }
    }
}

pub fn exit_code() -> i32 {
    EXIT_CODE.load(Ordering::SeqCst)
}

/// Something the UI thread wants in the log (e.g. a failure that would
/// normally be a message box).
/// Does nothing outside the self-test, so normal runs keep no log.
pub fn note(text: &str) {
    if !ENABLED.load(Ordering::SeqCst) {
        return;
    }
    if let Ok(mut n) = NOTES.lock() {
        n.push(text.to_string());
    }
}

pub fn start(out: PathBuf, main: HWND) {
    ENABLED.store(true, Ordering::SeqCst);
    let main = main.0 as isize;
    std::thread::spawn(move || {
        let failures = match Report::create(&out) {
            Ok(mut report) => {
                script(&mut report, &out, main);
                report.finish()
            }
            Err(_) => 1,
        };
        EXIT_CODE.store(failures as i32, Ordering::SeqCst);
        post(main, WM_SELFTEST, CMD_EXIT, 0);
    });
}

/// Runs on the UI thread in response to `WM_SELFTEST`.
pub fn on_ui_command(wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let value = match wparam.0 {
        CMD_TARGET => app(|a| a.target.borrow().as_ref().map(|t| t.hwnd.0 as isize).unwrap_or(0)),
        CMD_STATUS => {
            let (text, kind) = app(|a| a.status.borrow().clone());
            let kind = match kind {
                StatusKind::Neutral => 0,
                StatusKind::Good => 1,
                StatusKind::Bad => 2,
            };
            if let Ok(mut s) = STATUS.lock() {
                *s = (text, kind);
            }
            kind as isize
        }
        // SAFETY: SetForegroundWindow tolerates any HWND value, including stale ones.
        CMD_FOCUS => unsafe { SetForegroundWindow(hwnd(lparam.0)) }.0 as isize,
        CMD_POPUP => popup::hwnd().0 as isize,
        CMD_HOTKEY_OK => app(|a| a.hotkey_ok.get()) as isize,
        CMD_POINT => match popup::element_center(El::hit(lparam.0)) {
            Some(pt) => ((pt.x as u32 as u64) << 32 | pt.y as u32 as u64) as isize,
            None => isize::MIN,
        },
        CMD_DESCRIBE => {
            if let Ok(mut d) = DESCRIBE.lock() {
                *d = popup::describe_state();
            }
            0
        }
        CMD_EXIT => {
            exit();
            0
        }
        _ => 0,
    };
    LRESULT(value)
}

// ---------------------------------------------------------------- report

struct Report {
    file: File,
    passed: u32,
    failed: u32,
}

impl Report {
    fn create(out: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(out.join("screens"))?;
        let file = File::create(out.join("self-test.log"))?;
        Ok(Self { file, passed: 0, failed: 0 })
    }

    fn line(&mut self, text: &str) {
        let _ = writeln!(self.file, "{text}");
        let _ = self.file.flush();
    }

    fn check(&mut self, name: &str, ok: bool, detail: &str) -> bool {
        if ok {
            self.passed += 1;
        } else {
            self.failed += 1;
        }
        let tag = if ok { "PASS" } else { "FAIL" };
        self.line(&format!("{tag}  {name}  -  {detail}"));
        ok
    }

    /// A check that could not run on this machine; reported, not counted.
    fn skip(&mut self, name: &str, detail: &str) {
        self.line(&format!("SKIP  {name}  -  {detail}"));
    }

    fn info(&mut self, text: &str) {
        self.line(&format!("INFO  {text}"));
    }

    fn finish(mut self) -> u32 {
        let notes = NOTES.lock().map(|n| n.clone()).unwrap_or_default();
        for n in notes {
            self.info(&format!("app note: {n}"));
        }
        let summary = format!("RESULT  {} passed, {} failed", self.passed, self.failed);
        self.line(&summary);
        self.failed
    }
}

// ---------------------------------------------------------------- script

fn script(r: &mut Report, out: &Path, main: isize) {
    let me = std::process::id();
    r.info(&format!("output folder: {}", out.display()));
    for stale in ["paste-received.png", "paste-formats.txt", "drop-received.png", "drop-source.txt"] {
        let _ = std::fs::remove_file(out.join(stale));
    }

    // 1. Tray icon and shortcut registration.
    r.check("tray icon added", tray_icon_rect(main).is_some(), &format!("{:?}", tray_icon_rect(main)));
    let hotkey_ok = send(main, CMD_HOTKEY_OK, 0) == 1;
    r.check(
        "shortcut Win+Alt+. registered",
        hotkey_ok,
        if hotkey_ok { "RegisterHotKey succeeded" } else { "failed - is another PocketMedia running?" },
    );

    let work = primary_work_area();
    r.info(&format!("primary work area {work:?}"));

    // 2. Launch the test target in its own process and give it focus.
    let Ok(exe) = std::env::current_exe() else {
        r.check("launch test target", false, "current_exe failed");
        return;
    };
    let target_proc = Command::new(&exe).arg("--test-target").arg(out).spawn();
    let Ok(mut target_proc) = target_proc else {
        r.check("launch test target", false, "spawn failed");
        return;
    };
    // SAFETY: AllowSetForegroundWindow only takes a process id.
    let _ = unsafe { AllowSetForegroundWindow(target_proc.id()) };
    let mut target = 0isize;
    let found = wait_until(5000, || {
        target = find_window(test_target::CLASS, None, target_proc.id());
        target != 0 && is_visible(target)
    });
    r.check("launch test target", found, &format!("pid {}", target_proc.id()));
    if !found {
        stop_child(&mut target_proc);
        return;
    }
    if foreground() != target {
        send(main, CMD_FOCUS, target);
    }
    let focused = wait_until(1500, || foreground() == target);
    let allowed = [me, target_proc.id()];
    if !r.check("test target has focus", focused, &describe_foreground()) {
        r.info("Windows did not let the test take focus; skipping input tests so no other app gets input.");
        stop_child(&mut target_proc);
        return;
    }

    let t = Session { main, out: out.to_path_buf(), allowed: allowed.to_vec(), target, popup: send(main, CMD_POPUP, 0) };
    if !run_input_tests(r, &t) {
        r.info("input tests stopped early");
    }
    if is_visible(t.popup) && foreground() == t.popup && guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
        wait_until(1500, || !is_visible(t.popup));
    }
    conflict_test(r, out, &exe);
    stop_child(&mut target_proc);
}

/// Everything that needs injected input. Returns false if it had to stop.
struct Session {
    main: isize,
    out: PathBuf,
    allowed: Vec<u32>,
    target: isize,
    popup: isize,
}

impl Session {
    fn point(&self, el: El) -> Option<(i32, i32)> {
        let v = send(self.main, CMD_POINT, el.code());
        (v != isize::MIN).then_some((((v as u64) >> 32) as u32 as i32, (v as u64 & 0xFFFF_FFFF) as u32 as i32))
    }

    fn describe(&self) -> String {
        send(self.main, CMD_DESCRIBE, 0);
        DESCRIBE.lock().map(|d| d.clone()).unwrap_or_default()
    }

    fn click(&self, r: &mut Report, el: El) -> bool {
        let Some((x, y)) = self.point(el) else {
            return r.check(&format!("find {el:?}"), false, "not on screen");
        };
        if !guard(r, &self.allowed) {
            return false;
        }
        mouse_to(x, y);
        std::thread::sleep(Duration::from_millis(60));
        mouse_button(true);
        std::thread::sleep(Duration::from_millis(40));
        mouse_button(false);
        std::thread::sleep(Duration::from_millis(200));
        true
    }

    fn park(&self) {
        if let Some((x, y)) = self.point(El::Neutral) {
            mouse_to(x, y);
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    fn shot(&self, r: &mut Report, name: &str) {
        std::thread::sleep(Duration::from_millis(150));
        if let Some(rect) = window_rect(self.popup) {
            save_capture(r, &self.out.join("screens").join(format!("{name}.png")), capture_screen(rect));
        }
    }

    fn open(&self, r: &mut Report) -> bool {
        if is_visible(self.popup) && foreground() == self.popup {
            return true;
        }
        if !guard(r, &self.allowed) {
            return false;
        }
        press_hotkey();
        let ok = wait_until(2000, || is_visible(self.popup) && foreground() == self.popup);
        std::thread::sleep(Duration::from_millis(200));
        ok
    }

    /// Checks the popup's visible list against the expected names.
    fn expect(&self, r: &mut Report, name: &str, expected: &[&str]) {
        let d = self.describe();
        let names = d.split("names=").nth(1).unwrap_or("");
        let got: Vec<&str> = if names.is_empty() { vec![] } else { names.split('|').collect() };
        r.check(name, got == expected, &d);
    }

    fn type_text(&self, r: &mut Report, text: &str) -> bool {
        if !guard(r, &self.allowed) {
            return false;
        }
        type_unicode(text);
        std::thread::sleep(Duration::from_millis(200));
        true
    }

    fn select_all_and_type(&self, r: &mut Report, text: &str) -> bool {
        if !guard(r, &self.allowed) {
            return false;
        }
        press(&[VK_CONTROL, VK_A]);
        self.type_text(r, text)
    }

    fn wheel_gallery(&self, r: &mut Report, notches: i32) -> bool {
        let Some((x, y)) = self.point(El::GalleryGap) else { return false };
        if !guard(r, &self.allowed) {
            return false;
        }
        mouse_to(x, y);
        std::thread::sleep(Duration::from_millis(80));
        mouse_wheel(-120 * notches);
        std::thread::sleep(Duration::from_millis(200));
        true
    }
}

const RECENT: [&str; 8] = [
    "Happy dance.gif", "Square.png", "Race clip.mp4", "Celebration.mp4", "Portrait.png", "Hello.gif", "Wallpaper.jpg", "Reaction.gif",
];
const ALL: [&str; 8] = [
    "Happy dance.gif", "Square.png", "Race clip.mp4", "Hello.gif", "Wallpaper.jpg", "Celebration.mp4", "Portrait.png", "Reaction.gif",
];

fn run_input_tests(r: &mut Report, t: &Session) -> bool {
    let (main, target, popup) = (t.main, t.target, t.popup);

    // 3. Tray menu (opened the way a right-click on the icon does).
    tray_menu_test(r);

    // Put the pointer on the left so the popup opens away from the target window.
    let work = primary_work_area();
    mouse_to(work.left + (work.right - work.left) / 4, work.top + 120);

    // 4. Shortcut opens the popup, which remembers the target and takes focus.
    if !guard(r, &t.allowed) {
        return false;
    }
    press_hotkey();
    r.check("shortcut opens popup", wait_until(2000, || is_visible(popup)), "");
    let remembered = send(main, CMD_TARGET, 0);
    r.check("popup remembers previous app", remembered == target, &format!("remembered {remembered:#x}, target {target:#x}"));
    r.check("popup has focus", wait_until(1000, || foreground() == popup), &describe_foreground());
    if let Some(rect) = window_rect(popup) {
        // SAFETY: GetDpiForWindow tolerates any HWND.
        let dpi = unsafe { GetDpiForWindow(hwnd(popup)) };
        let scale = dpi as f32 / 96.0;
        let (w, h) = ((rect.right - rect.left) as f32 / scale, (rect.bottom - rect.top) as f32 / scale);
        r.info(&format!("popup rect {rect:?}, dpi {dpi}, size {w:.0}x{h:.0} DIP"));
        r.check("popup is 520 x 620 DIP (fixed size)", (w - 520.0).abs() < 1.5 && (h - 620.0).abs() < 1.5, &format!("{w:.1} x {h:.1}"));
    }
    r.check("search box has focus on open", t.describe().contains("focus=Search"), &t.describe());

    // 5. Escape closes it and focus returns to the target.
    if !guard(r, &t.allowed) {
        return false;
    }
    press(&[VK_ESCAPE]);
    r.check("Escape closes popup", wait_until(1500, || !is_visible(popup)), "");
    r.check("focus returns to previous app after Escape", wait_until(1500, || foreground() == target), &describe_foreground());

    // 6. Shortcut toggles: open, then close again.
    if !t.open(r) || !guard(r, &t.allowed) {
        return false;
    }
    press_hotkey();
    r.check("shortcut toggles popup closed", wait_until(1500, || !is_visible(popup)), "");
    r.check("focus returns to previous app after toggle", wait_until(1500, || foreground() == target), &describe_foreground());

    // 7. Popup screens, captured with the names the screenshots have always had.
    if !t.open(r) {
        return false;
    }
    t.park();
    t.shot(r, "focused-01-popup-recent");
    t.expect(r, "Recently used list (focused-01)", &RECENT);
    t.wheel_gallery(r, 5);
    t.park();
    t.shot(r, "focused-01-popup-recent-scrolled1");
    r.check("gallery scrolls inside the window", popup_size_unchanged(popup), "window size unchanged after scrolling");

    t.click(r, El::List);
    t.shot(r, "state-list-open");
    t.click(r, El::Row(1));
    t.park();
    t.shot(r, "focused-02-popup-all-media");
    t.expect(r, "All media list (focused-02)", &ALL);
    t.wheel_gallery(r, 5);
    t.park();
    t.shot(r, "focused-02-popup-all-media-scrolled1");

    for (chip, name, expected) in [
        (1, "focused-03-popup-type-gif", &["Happy dance.gif", "Hello.gif", "Reaction.gif"][..]),
        (2, "focused-03-popup-type-picture", &["Square.png", "Wallpaper.jpg", "Portrait.png"][..]),
        (3, "focused-03-popup-type-video", &["Race clip.mp4", "Celebration.mp4"][..]),
    ] {
        t.click(r, El::Chip(chip));
        t.park();
        t.shot(r, name);
        t.expect(r, &format!("type filter ({name})"), expected);
    }
    t.click(r, El::Chip(0));

    t.click(r, El::Search);
    t.park();
    for (query, name, expected) in [
        ("dance happy", "focused-04-popup-search-words-any-order", &["Happy dance.gif"][..]),
        ("celeb", "focused-05-popup-search-partial", &["Celebration.mp4"][..]),
        ("gif", "focused-05b-popup-search-extension-ignored", &[][..]),
        ("zzzz", "focused-06-popup-empty-search", &[][..]),
    ] {
        if !t.select_all_and_type(r, query) {
            return false;
        }
        t.shot(r, name);
        t.expect(r, &format!("search \"{query}\""), expected);
    }
    t.click(r, El::EmptyAction);
    t.expect(r, "Clear filters restores the list", &ALL);

    // States the wireframe does not show: hover, keyboard selection, focus ring.
    if let Some((x, y)) = t.point(El::Tile(1)) {
        mouse_to(x, y);
        t.shot(r, "state-hover-tile");
    }
    t.click(r, El::Search);
    t.park();
    if !guard(r, &t.allowed) {
        return false;
    }
    press(&[VK_DOWN]);
    press(&[VK_RIGHT]);
    t.shot(r, "state-keyboard-selected");
    let d = t.describe();
    r.check("arrow keys move into the gallery", d.contains("focus=Gallery"), &d);
    if !guard(r, &t.allowed) {
        return false;
    }
    press(&[VK_TAB]);
    t.shot(r, "state-focus-ring");

    // 8. Paste: type a name, Enter -> popup closes -> target receives Ctrl+V.
    t.click(r, El::Search);
    if !t.select_all_and_type(r, "square") || !guard(r, &t.allowed) {
        return false;
    }
    press(&[VK_RETURN]);
    r.check("paste closes popup", wait_until(1500, || !is_visible(popup)), "");
    let received = t.out.join("paste-received.png");
    let got = wait_until(3000, || received.exists());
    let same = got && std::fs::read(&received).map(|b| b == assets().png).unwrap_or(false);
    r.check("target received the PNG by paste", same, if got { "bytes compared with the original" } else { "nothing arrived within 3 s" });
    let formats = std::fs::read_to_string(t.out.join("paste-formats.txt")).unwrap_or_default();
    let has = |f: &str| formats.lines().any(|l| l == f);
    r.check("clipboard formats PNG + CF_DIB + CF_HDROP", has("PNG") && has("CF_DIB") && has("CF_HDROP"), &formats.replace('\n', ", "));
    let (kind, text) = status(main);
    r.check("app records paste result", kind == 1, &text);

    // 9. Drag a tile into the target window.
    if !t.open(r) {
        return false;
    }
    drag_test(r, t);

    // 10. Clear history, then the empty Recently used state.
    if !t.open(r) {
        return false;
    }
    t.click(r, El::List);
    t.click(r, El::Row(0));
    t.click(r, El::ClearHistory);
    t.park();
    t.shot(r, "state-history-cleared");
    let d = t.describe();
    r.check("Clear history empties Recently used", d.contains("list=Recent") && d.contains("count=0"), &d);
    t.click(r, El::EmptyAction);
    t.expect(r, "Show all media switches to All media", &ALL);

    // 11. Clicking outside closes the popup.
    if let Some(rect) = window_rect(target) {
        if !guard(r, &t.allowed) {
            return false;
        }
        mouse_to((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
        std::thread::sleep(Duration::from_millis(60));
        mouse_button(true);
        mouse_button(false);
        r.check("clicking outside closes popup", wait_until(1500, || !is_visible(popup)), "");
    }
    true
}

fn popup_size_unchanged(popup: isize) -> bool {
    let a = window_rect(popup);
    std::thread::sleep(Duration::from_millis(50));
    a.is_some() && a == window_rect(popup)
}

/// The tray menu cannot be tested automatically: Windows 11's taskbar ignored a
/// simulated right-click on the icon (run 4: the click reached Shell_TrayWnd but
/// PocketMedia received no tray event), and a posted message does not reproduce
/// a real click (run 2: the menu closed at once). It is on the manual checklist.
fn tray_menu_test(r: &mut Report) {
    r.skip(
        "tray menu (Open, Exit)",
        "Windows does not deliver simulated clicks to tray icons; checked by hand",
    );
}

fn drag_test(r: &mut Report, t: &Session) {
    let (Some((fx, fy)), Some(to)) = (t.point(El::Tile(0)), window_rect(t.target)) else {
        r.check("drag tile and target found", false, "");
        return;
    };
    if let Some(p) = window_rect(t.popup)
        && overlaps(&p, &to)
    {
        r.check("drag test layout", false, "popup overlaps the target window; screen too small");
        return;
    }
    let (tx, ty) = ((to.left + to.right) / 2, (to.top + to.bottom) / 2);
    if !guard(r, &t.allowed) {
        return;
    }
    mouse_to(fx, fy);
    std::thread::sleep(Duration::from_millis(150));
    mouse_button(true);
    std::thread::sleep(Duration::from_millis(150));
    for i in 1..=5 {
        mouse_to(fx + i * 6, fy + i * 2); // past the system drag threshold
        std::thread::sleep(Duration::from_millis(30));
    }
    for i in 1..=15 {
        mouse_to(fx + (tx - fx) * i / 15, fy + (ty - fy) * i / 15);
        std::thread::sleep(Duration::from_millis(40));
    }
    std::thread::sleep(Duration::from_millis(300));
    // Only release over our own target; otherwise move back and release over the tile.
    let over = window_at(tx, ty);
    if window_pid(over) != window_pid(t.target) {
        mouse_to(fx, fy);
        std::thread::sleep(Duration::from_millis(100));
        mouse_button(false);
        r.check("drag reaches test target", false, &format!("window under pointer belongs to pid {}", window_pid(over)));
        return;
    }
    mouse_button(false);
    let received = t.out.join("drop-received.png");
    let got = wait_until(4000, || received.exists());
    let same = got && std::fs::read(&received).map(|b| b == assets().png).unwrap_or(false);
    r.check("target received the PNG by drag", same, if got { "bytes compared with the original" } else { "nothing dropped within 4 s" });
    r.check("drag copied, original file kept", assets().file.exists(), &assets().file.display().to_string());
    let (kind, text) = status(t.main);
    r.check("app records drag result", kind == 1, &text);
}

fn conflict_test(r: &mut Report, out: &Path, exe: &Path) {
    let second = Command::new(exe).arg("--exit-after-start").spawn();
    let Ok(mut second) = second else {
        r.check("second copy launched", false, "spawn failed");
        return;
    };
    let mut boxed = 0isize;
    let shown = wait_until(5000, || {
        boxed = find_window("#32770", Some(CONFLICT_TITLE), second.id());
        boxed != 0
    });
    if r.check("second copy shows shortcut-unavailable message", shown, "") {
        std::thread::sleep(Duration::from_millis(300));
        let text = longest_child_text(boxed);
        r.check(
            "message explains the conflict",
            text.contains("already using it") && text.contains("tray"),
            &text.replace('\n', " / "),
        );
        save_capture(r, &out.join("screens").join("hotkey-conflict.png"), capture_window(boxed));
        post(boxed, WM_CLOSE, 0, 0);
    }
    let exited = wait_until(5000, || matches!(second.try_wait(), Ok(Some(_))));
    r.check("second copy exits cleanly", exited, "");
    if !exited {
        let _ = second.kill();
    }
}

// ---------------------------------------------------------------- helpers

fn hwnd(v: isize) -> HWND {
    HWND(v as *mut _)
}

fn send(main: isize, cmd: usize, arg: isize) -> isize {
    // SAFETY: SendMessageW to our own window; the handler only reads plain values.
    unsafe { SendMessageW(hwnd(main), WM_SELFTEST, Some(WPARAM(cmd)), Some(LPARAM(arg))) }.0
}

fn post(window: isize, msg: u32, wparam: usize, lparam: isize) {
    // SAFETY: PostMessageW copies the values; a stale HWND only makes it fail.
    let _ = unsafe { PostMessageW(Some(hwnd(window)), msg, WPARAM(wparam), LPARAM(lparam)) };
}

fn status(main: isize) -> (i32, String) {
    let kind = send(main, CMD_STATUS, 0) as i32;
    let text = STATUS.lock().map(|s| s.0.clone()).unwrap_or_default();
    (kind, text)
}

fn wait_until(ms: u64, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn foreground() -> isize {
    // SAFETY: no preconditions.
    unsafe { GetForegroundWindow() }.0 as isize
}

fn window_pid(window: isize) -> u32 {
    let mut pid = 0u32;
    // SAFETY: pid is a valid out pointer; a stale HWND yields 0.
    unsafe { GetWindowThreadProcessId(hwnd(window), Some(&mut pid)) };
    pid
}

fn is_visible(window: isize) -> bool {
    // SAFETY: tolerates any HWND value.
    window != 0 && unsafe { IsWindowVisible(hwnd(window)) }.as_bool()
}

fn window_rect(window: isize) -> Option<RECT> {
    let mut r = RECT::default();
    // SAFETY: r is a valid out pointer.
    unsafe { GetWindowRect(hwnd(window), &mut r) }.ok().map(|_| r)
}

fn window_text(window: isize) -> String {
    let mut buf = [0u16; 1024];
    // SAFETY: buf is writable for its full length.
    let n = unsafe { GetWindowTextW(hwnd(window), &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn window_at(x: i32, y: i32) -> isize {
    // SAFETY: no preconditions.
    let w = unsafe { WindowFromPoint(windows::Win32::Foundation::POINT { x, y }) };
    // SAFETY: GetAncestor tolerates any HWND.
    unsafe { GetAncestor(w, GA_ROOT) }.0 as isize
}

fn describe_foreground() -> String {
    let fg = foreground();
    format!("foreground {fg:#x} \"{}\" pid {}", window_text(fg), window_pid(fg))
}

fn find_window(class: &str, title: Option<&str>, pid: u32) -> isize {
    let class = HSTRING::from(class);
    let title = title.map(HSTRING::from);
    let mut after = HWND::default();
    loop {
        let title_ptr = title.as_ref().map(|t| PCWSTR(t.as_ptr())).unwrap_or(PCWSTR::null());
        // SAFETY: the HSTRINGs outlive the call; HWND_MESSAGE is not used, so this walks top-level windows.
        let found = unsafe { FindWindowExW(None, Some(after), PCWSTR(class.as_ptr()), title_ptr) };
        let Ok(found) = found else { return 0 };
        if window_pid(found.0 as isize) == pid {
            return found.0 as isize;
        }
        after = found;
    }
}

fn longest_child_text(parent: isize) -> String {
    let mut best = String::new();
    let mut child = HWND::default();
    loop {
        // SAFETY: walks the children of a window; a null class matches any.
        let next = unsafe { FindWindowExW(Some(hwnd(parent)), Some(child), PCWSTR::null(), PCWSTR::null()) };
        let Ok(next) = next else { return best };
        let text = window_text(next.0 as isize);
        if text.len() > best.len() {
            best = text;
        }
        child = next;
    }
}

fn tray_icon_rect(main: isize) -> Option<RECT> {
    let id = NOTIFYICONIDENTIFIER {
        cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
        hWnd: hwnd(main),
        uID: 1,
        ..Default::default()
    };
    // SAFETY: id is a fully initialised identifier for our icon.
    unsafe { Shell_NotifyIconGetRect(&id) }.ok()
}

fn primary_work_area() -> RECT {
    let mut r = RECT::default();
    // SAFETY: r is a valid RECT-sized out buffer, as SPI_GETWORKAREA requires.
    let _ = unsafe {
        SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut r as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
    };
    r
}

fn overlaps(a: &RECT, b: &RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// Stops input tests if anything except PocketMedia or its test target is in front.
fn guard(r: &mut Report, allowed: &[u32]) -> bool {
    let pid = window_pid(foreground());
    if allowed.contains(&pid) {
        return true;
    }
    r.check("input guard", false, &format!("another app took focus ({}); stopped to avoid sending it input", describe_foreground()));
    false
}

fn stop_child(child: &mut Child) {
    let w = find_window(test_target::CLASS, None, child.id());
    if w != 0 {
        post(w, WM_CLOSE, 0, 0);
    }
    if !wait_until(3000, || matches!(child.try_wait(), Ok(Some(_)))) {
        let _ = child.kill();
    }
}

// ---------------------------------------------------------------- input

fn send_inputs(inputs: &[INPUT]) {
    // SAFETY: inputs is a valid slice of initialised INPUT structs.
    unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
}

fn key(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                ..Default::default()
            },
        },
    }
}

/// Presses the keys in order, then releases them in reverse order.
fn press(keys: &[VIRTUAL_KEY]) {
    let mut inputs: Vec<INPUT> = keys.iter().map(|k| key(*k, false)).collect();
    inputs.extend(keys.iter().rev().map(|k| key(*k, true)));
    send_inputs(&inputs);
}

/// Types text as Unicode key events (what WM_CHAR delivers for real typing).
fn type_unicode(text: &str) {
    let mut inputs = Vec::new();
    for unit in text.encode_utf16() {
        for up in [false, true] {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wScan: unit,
                        dwFlags: if up { KEYEVENTF_UNICODE | KEYEVENTF_KEYUP } else { KEYEVENTF_UNICODE },
                        ..Default::default()
                    },
                },
            });
        }
    }
    send_inputs(&inputs);
}

fn mouse_wheel(delta: i32) {
    let mut input = mouse(MOUSEEVENTF_WHEEL, 0, 0);
    input.Anonymous.mi.mouseData = delta as u32;
    send_inputs(&[input]);
}

fn press_hotkey() {
    press(&[VK_LWIN, VK_MENU, VK_OEM_PERIOD]);
}

fn mouse(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS, dx: i32, dy: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, dwFlags: flags, ..Default::default() } },
    }
}

fn mouse_to(x: i32, y: i32) {
    // SAFETY: GetSystemMetrics has no preconditions.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(2),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(2),
        )
    };
    let nx = ((x - vx) as i64 * 65_535 / (vw - 1) as i64) as i32;
    let ny = ((y - vy) as i64 * 65_535 / (vh - 1) as i64) as i32;
    send_inputs(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny)]);
}

fn mouse_button(down: bool) {
    send_inputs(&[mouse(if down { MOUSEEVENTF_LEFTDOWN } else { MOUSEEVENTF_LEFTUP }, 0, 0)]);
}

// ---------------------------------------------------------------- capture

/// GDI handles released on drop.
struct ScreenDc(HDC);
impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: obtained from GetDC(None) and released once.
        unsafe { ReleaseDC(None, self.0) };
    }
}
struct MemDc(HDC);
impl Drop for MemDc {
    fn drop(&mut self) {
        // SAFETY: created by CreateCompatibleDC and deleted once.
        let _ = unsafe { DeleteDC(self.0) };
    }
}
struct Bitmap(HBITMAP);
impl Drop for Bitmap {
    fn drop(&mut self) {
        // SAFETY: created by CreateDIBSection, deselected before drop, deleted once.
        let _ = unsafe { DeleteObject(self.0.into()) };
    }
}

struct Capture {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// Renders into a fresh 32-bit DIB and returns its pixels as RGBA.
fn capture_with(width: i32, height: i32, draw: impl FnOnce(HDC) -> bool) -> Result<Capture, String> {
    if width <= 0 || height <= 0 {
        return Err("empty rectangle".into());
    }
    // SAFETY: each handle is wrapped in an owner that releases it; the DIB pointer is
    // read only while the bitmap is alive and after GDI has finished drawing.
    unsafe {
        let screen = ScreenDc(GetDC(None));
        let mem = MemDc(CreateCompatibleDC(Some(screen.0)));
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bitmap = Bitmap(
            CreateDIBSection(Some(mem.0), &info, DIB_RGB_COLORS, &mut bits, None, 0).map_err(|e| e.to_string())?,
        );
        let old: HGDIOBJ = SelectObject(mem.0, bitmap.0.into());
        let ok = draw(mem.0);
        SelectObject(mem.0, old);
        if !ok || bits.is_null() {
            return Err("drawing failed".into());
        }
        let bgra = std::slice::from_raw_parts(bits as *const u8, (width * height * 4) as usize);
        let mut rgba = Vec::with_capacity(bgra.len());
        for px in bgra.chunks_exact(4) {
            rgba.extend_from_slice(&[px[2], px[1], px[0], 0xFF]);
        }
        drop(bitmap);
        Ok(Capture { width: width as u32, height: height as u32, rgba })
    }
}

/// What is actually on screen in `rect` (includes rounded corners and overlap).
fn capture_screen(rect: RECT) -> Result<Capture, String> {
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    capture_with(w, h, |dc| {
        // SAFETY: GetDC(None) has no preconditions; ScreenDc releases it.
        let screen = ScreenDc(unsafe { GetDC(None) });
        // SAFETY: both DCs are valid for the duration of the call.
        unsafe { BitBlt(dc, 0, 0, w, h, Some(screen.0), rect.left, rect.top, SRCCOPY | CAPTUREBLT) }.is_ok()
    })
}

/// The window's own rendering, even if something covers it.
fn capture_window(window: isize) -> Result<Capture, String> {
    let Some(rect) = window_rect(window) else { return Err("window gone".into()) };
    capture_with(rect.right - rect.left, rect.bottom - rect.top, |dc| {
        // SAFETY: dc is valid; flag 2 = PW_RENDERFULLCONTENT.
        unsafe { PrintWindow(hwnd(window), dc, PRINT_WINDOW_FLAGS(2)) }.as_bool()
    })
}

fn save_capture(r: &mut Report, path: &Path, capture: Result<Capture, String>) {
    match capture {
        Ok(c) => {
            let png = crate::test_png::encode_rgba(c.width, c.height, &c.rgba);
            match std::fs::write(path, png) {
                Ok(()) => r.info(&format!("screenshot {} ({}x{})", path.display(), c.width, c.height)),
                Err(e) => r.info(&format!("could not save {}: {e}", path.display())),
            }
        }
        Err(e) => r.info(&format!("could not capture {}: {e}", path.display())),
    }
}
