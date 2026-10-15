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
    VK_A, VK_BACK, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LWIN, VK_MENU, VK_OEM_PERIOD, VK_RETURN, VK_RIGHT, VK_TAB,
};
use windows::Win32::UI::Shell::{NOTIFYICONIDENTIFIER, Shell_NotifyIconGetRect};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR};

use crate::popup::TestTarget as Target;
use crate::ui::add_view::AddHit;
use crate::ui::layout::Hit;
use crate::{StatusKind, WM_SELFTEST, app, exit, manager, popup, test_target};

mod manager_test;
mod media_gen;
mod watch_test;
mod preview_test;
mod seed;
mod settings_test;
mod shortcut_test;
mod uninstall_test;
mod window_test;

const CMD_TARGET: usize = 1;
const CMD_STATUS: usize = 2;
const CMD_FOCUS: usize = 3;
const CMD_POPUP: usize = 4;
const CMD_EXIT: usize = 5;
const CMD_HOTKEY_OK: usize = 6;
const CMD_POINT: usize = 7;
const CMD_DESCRIBE: usize = 8;
const CMD_RESET: usize = 9;
const CMD_MANAGER: usize = 10;
const CMD_MANAGER_POINT: usize = 11;
const CMD_MANAGER_DESCRIBE: usize = 12;
const CMD_MANAGER_ADD_POINT: usize = 13;
const CMD_SETTINGS_POINT: usize = 14;
/// lparam = scroll offset in DIPs (clamped; a large value means the end).
const CMD_SETTINGS_SCROLL: usize = 15;
/// Forgets the app to paste into (as when the popup opens from the tray).
const CMD_FORGET_TARGET: usize = 16;
/// Puts `store::describe()` in DESCRIBE.
const CMD_STORE_DESCRIBE: usize = 17;
/// Reads the store from disk again, as a restart would, and reloads the Settings page.
const CMD_STORE_REOPEN: usize = 18;
/// lparam 0/1: sets Close after inserting (saved, as Settings would).
const CMD_SET_CLOSE_AFTER: usize = 19;
/// The next paste fails as if focus never came back.
const CMD_FAIL_NEXT_PASTE: usize = 20;
/// Posted, never sent: the tray menu's Exit (it may ask first, in a modal box).
/// 21-24 belong to preview_test, 40-42 to watch_test, 50-51 to shortcut_test.
const CMD_REQUEST_EXIT: usize = 30;
/// Starts Back up library into the self-test's backup folder without a click,
/// so the quick popup keeps its review open meanwhile.
const CMD_BACKUP_START: usize = 31;
/// Where the popup last opened, packed, if it opened at a text caret (else not on screen).
const CMD_POPUP_PLACED: usize = 32;
/// Makes the remembered app older than `target::FRESH_FOR`. 1 if done.
const CMD_AGE_TARGET: usize = 33;
/// The next paste reads the target's elevation as unknown.
const CMD_UNKNOWN_ELEVATION: usize = 34;
/// The popup's next frame reports a lost drawing target; the popup
/// is asked to repaint once. Returns the drawing targets made so far.
const CMD_LOSE_TARGET: usize = 35;
/// The drawing targets made so far (`d2d::TARGETS_MADE`).
const CMD_D2D_TARGETS: usize = 36;
/// The shortcut PocketMedia uses, packed by `pack_shortcut`.
const CMD_SHORTCUT: usize = 37;

static EXIT_CODE: AtomicI32 = AtomicI32::new(0);
static ENABLED: AtomicBool = AtomicBool::new(false);
static NOTES: Mutex<Vec<String>> = Mutex::new(Vec::new());
static STATUS: Mutex<(String, i32)> = Mutex::new((String::new(), 0));
static DESCRIBE: Mutex<String> = Mutex::new(String::new());

/// Popup elements the test can point at, encoded for `CMD_POINT`.
#[derive(Clone, Copy, Debug)]
enum El {
    Neutral,
    /// The popup's logo, which drags it.
    Handle,
    Search,
    OpenManager,
    AddMediaButton,
    AddChooseFiles,
    AddImportExisting,
    AddRow(usize),
    /// The × that removes review row `i`.
    AddRemove(usize),
    AddChooseAgain,
    AddCancel,
    AddAdd,
    AddBack,
    AddMain,
    Chip(usize),
    /// Recently used (0) or All media (1).
    Tab(usize),
    GalleryGap,
    ClearHistory,
    EmptyAction,
    Tile(usize),
}

impl El {
    fn code(self) -> isize {
        match self {
            El::Neutral => 0,
            El::Handle => 5,
            El::Search => 2,
            El::OpenManager => 4,
            El::AddMediaButton => 3,
            El::AddChooseFiles => 200,
            El::AddImportExisting => 201,
            El::AddRow(i) => 210 + i as isize,
            El::AddRemove(i) => 240 + i as isize,
            El::AddChooseAgain => 230,
            El::AddCancel => 231,
            El::AddAdd => 232,
            El::AddBack => 233,
            El::AddMain => 234,
            El::Chip(i) => 10 + i as isize,
            El::Tab(i) => 30 + i as isize,
            El::GalleryGap => 40,
            El::ClearHistory => 50,
            El::EmptyAction => 60,
            El::Tile(i) => 100 + i as isize,
        }
    }

    fn target(code: isize) -> Target {
        Target::Add(match code {
            200 => AddHit::ChooseFiles,
            201 => AddHit::ImportExisting,
            210..=229 => AddHit::RowInput((code - 210) as usize),
            240..=259 => AddHit::Remove((code - 240) as usize),
            5 => return Target::Handle,
            230 => AddHit::ChooseAgain,
            231 => AddHit::Cancel,
            232 => AddHit::Add,
            233 => AddHit::Back,
            234 => AddHit::Main,
            _ => return Target::Gallery(Self::hit(code)),
        })
    }

    fn hit(code: isize) -> Hit {
        match code {
            1 => Hit::Close,
            2 => Hit::Search,
            3 => Hit::AddMedia,
            4 => Hit::OpenManager,
            10..=13 => Hit::Chip((code - 10) as usize),
            30..=31 => Hit::ListTab((code - 30) as usize),
            40 => Hit::Scrollbar,
            50 => Hit::ClearHistory,
            60 => Hit::EmptyAction,
            100.. => Hit::Tile((code - 100) as usize),
            _ => Hit::None,
        }
    }
}

/// Set when the self-test wrote its RESULT line.
static FINISHED: AtomicBool = AtomicBool::new(false);
static OUT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The number of failures. A self-test that ended before its RESULT line (the
/// app exited or was closed early) counts as a failure, and the log says so.
pub fn exit_code() -> i32 {
    if ENABLED.load(Ordering::SeqCst) && !FINISHED.load(Ordering::SeqCst) {
        if let Some(out) = OUT.lock().ok().and_then(|o| o.clone())
            && let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(out.join("self-test.log"))
        {
            let _ = writeln!(f, "FAIL  the self-test ended before it finished (PocketMedia exited early)");
            let _ = writeln!(f, "RESULT  ended early, 1 failed");
        }
        return EXIT_CODE.load(Ordering::SeqCst).max(1);
    }
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
    if let Ok(mut o) = OUT.lock() {
        *o = Some(out.clone());
    }
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
        FINISHED.store(true, Ordering::SeqCst);
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
        CMD_HOTKEY_OK => crate::hotkey::ok() as isize,
        CMD_SHORTCUT => pack_shortcut(crate::hotkey::current()),
        CMD_POINT => pack(popup::element_center(El::target(lparam.0))),
        CMD_MANAGER => manager::hwnd().0 as isize,
        CMD_MANAGER_POINT => pack(manager::element_center(manager_test::decode(lparam.0))),
        CMD_SETTINGS_POINT => pack(manager::settings_element_center(settings_test::decode(lparam.0))),
        CMD_STORE_DESCRIBE => {
            if let Ok(mut d) = DESCRIBE.lock() {
                *d = crate::store::describe();
            }
            0
        }
        CMD_SET_CLOSE_AFTER => {
            let mut settings = crate::store::settings();
            settings.close_after_insert = lparam.0 != 0;
            let _ = crate::store::save_settings(settings);
            manager::reload_settings();
            0
        }
        CMD_FAIL_NEXT_PASTE => {
            crate::paste::FAIL_NEXT.store(true, Ordering::Relaxed);
            0
        }
        CMD_STORE_REOPEN => {
            if let Some(p) = crate::paths::get() {
                crate::store::init(&p.data);
            }
            crate::autostart::sync_store();
            manager::reload_settings();
            0
        }
        CMD_FORGET_TARGET => {
            app(|a| *a.target.borrow_mut() = Err(crate::target::NoTarget::Shell));
            0
        }
        CMD_SETTINGS_SCROLL => {
            manager::set_settings_scroll(lparam.0 as f32);
            0
        }
        CMD_MANAGER_ADD_POINT => match El::target(lparam.0) {
            Target::Add(hit) => pack(manager::add_element_center(hit)),
            Target::Gallery(_) | Target::Handle => isize::MIN,
        },
        CMD_MANAGER_DESCRIBE => {
            if let Ok(mut d) = DESCRIBE.lock() {
                *d = manager::describe();
            }
            0
        }
        CMD_RESET => {
            // Fresh library files and windows, so each test section starts the same.
            // The folders are replaced, so watching pauses meanwhile.
            crate::watch::stop();
            crate::autostart::remove_test_key();
            crate::store::reset_for_test();
            crate::autostart::sync_store();
            // A section that changed the shortcut gives the default back.
            if crate::hotkey::current() != crate::hotkey::Shortcut::default()
                && let Err(e) = crate::hotkey::apply(crate::hotkey::Shortcut::default())
            {
                note(&format!("could not restore the default shortcut: {e}"));
            }
            if let Err(e) = seed::reset_library() {
                note(&format!("could not create the test library: {e}"));
            }
            if let Some(p) = crate::paths::get()
                && let Err(e) = crate::watch::start(&p.media, crate::watch::BUFFER)
            {
                note(&e);
            }
            popup::reset();
            manager::reset();
            0
        }
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
        CMD_REQUEST_EXIT => {
            crate::request_exit();
            0
        }
        CMD_POPUP_PLACED => pack(popup::placed().filter(|(caret, _)| *caret).map(|(_, pt)| pt)),
        CMD_AGE_TARGET => {
            let aged = app(|a| match a.target.borrow_mut().as_mut() {
                Ok(t) => match t.captured.checked_sub(crate::target::FRESH_FOR + Duration::from_secs(1)) {
                    Some(earlier) => {
                        t.captured = earlier;
                        true
                    }
                    None => false,
                },
                Err(_) => false,
            });
            aged as isize
        }
        CMD_UNKNOWN_ELEVATION => {
            crate::paste::UNKNOWN_NEXT.store(true, Ordering::Relaxed);
            0
        }
        CMD_LOSE_TARGET => {
            let made = crate::ui::d2d::TARGETS_MADE.load(Ordering::Relaxed);
            crate::ui::d2d::LOSE_NEXT.store(true, Ordering::Relaxed);
            crate::winutil::invalidate(popup::hwnd());
            made as isize
        }
        CMD_D2D_TARGETS => crate::ui::d2d::TARGETS_MADE.load(Ordering::Relaxed) as isize,
        CMD_BACKUP_START => {
            manager::start_backup(crate::dialogs::pick_backup_folder(manager::hwnd()));
            crate::backup::running() as isize
        }
        cmd @ preview_test::CMD_PREVIEW_DESCRIBE..=preview_test::CMD_MANAGER_SHOW => preview_test::on_ui_command(cmd, lparam.0),
        cmd @ watch_test::CMD_LIB_FIND..=watch_test::CMD_WATCH_RESTART => watch_test::on_ui_command(cmd, lparam.0),
        cmd @ shortcut_test::CMD_SC_TRY..=shortcut_test::CMD_SC_FREE => shortcut_test::on_ui_command(cmd, lparam.0),
        _ => 0,
    };
    LRESULT(value)
}

/// Packs a screen point into an LRESULT (isize::MIN = not on screen).
fn pack(pt: Option<windows::Win32::Foundation::POINT>) -> isize {
    match pt {
        Some(pt) => ((pt.x as u32 as u64) << 32 | pt.y as u32 as u64) as isize,
        None => isize::MIN,
    }
}

fn unpack(v: isize) -> Option<(i32, i32)> {
    (v != isize::MIN).then_some((((v as u64) >> 32) as u32 as i32, (v as u64 & 0xFFFF_FFFF) as u32 as i32))
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

    folders_test(r);

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

    // Real files to import, and the library as the wireframe's 8 real files.
    match seed::make_source(out) {
        Ok((files, folder)) => crate::dialogs::set_test_pick(files, folder),
        Err(e) => r.info(&format!("could not create the import source: {e}")),
    }
    send(main, CMD_RESET, 0);

    let t = Session { main, out: out.to_path_buf(), allowed: allowed.to_vec(), target, popup: send(main, CMD_POPUP, 0) };
    if !run_input_tests(r, &t) {
        r.info("input tests stopped early");
    }
    if is_visible(t.popup) && foreground() == t.popup && guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
        wait_until(1500, || !is_visible(t.popup));
    }
    // A second start must keep what is already in the folders.
    let marker = crate::paths::get().map(|p| p.data.join("self-test-marker.txt"));
    if let Some(m) = &marker {
        let _ = std::fs::write(m, b"kept");
    }
    instance_test(r, &t, &exe);
    if let Some(m) = &marker {
        let kept = std::fs::read(m).is_ok_and(|b| b == b"kept");
        r.check("starting again keeps the folders' contents", kept, &m.display().to_string());
        let _ = std::fs::remove_file(m);
    }
    blocked_folder_test(r, out, &exe);
    stop_child(&mut target_proc);
    exit_test(r, main);
}

/// The tray menu's Exit, with no backup running, cleans up and ends.
/// Last in the script, because PocketMedia is gone afterwards; the app waits for
/// this report before the process ends (`wait_for_report`).
fn exit_test(r: &mut Report, main: isize) {
    let shortcut = unpack_shortcut(send(main, CMD_SHORTCUT, 0));
    let before = (crate::backup::running(), tray_icon_rect(main).is_some(), send(main, CMD_HOTKEY_OK, 0) == 1);
    let ready = before == (false, true, true);
    r.check(
        "before Exit: no backup runs, the tray icon is there and the shortcut is registered",
        ready,
        &format!("backup running {}, tray icon {}, shortcut {} registered {}", before.0, before.1, shortcut.label(), before.2),
    );
    if !ready {
        return;
    }
    post(main, WM_SELFTEST, CMD_REQUEST_EXIT, 0);
    // SAFETY: IsWindow accepts any value, including a destroyed window's.
    let gone = wait_until(5000, || !unsafe { IsWindow(Some(hwnd(main))) }.as_bool());
    r.check("Exit from the tray menu closes PocketMedia's main window without asking (no backup runs)", gone, "");
    let icon = tray_icon_rect(main);
    r.check("…and removes the tray icon", gone && icon.is_none(), &format!("icon place now {icon:?}"));
    let free = gone && shortcut_free(shortcut);
    r.check(
        "…and lets the shortcut go (another program could register it now)",
        free,
        &format!("{} registered from the self-test thread: {free}", shortcut.label()),
    );
    let ended = wait_until(5000, || LOOP_ENDED.load(Ordering::SeqCst));
    r.check("…and ends the message loop, so the process exits (test.cmd's wait returns)", ended, "");
}

/// Whether `s` can be registered right now, for this thread (let go again at once).
fn shortcut_free(s: crate::hotkey::Shortcut) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey};
    const ID: i32 = 0x0BEE;
    // SAFETY: a thread hotkey with plain integer arguments, let go below.
    let ok = unsafe { RegisterHotKey(None, ID, crate::hotkey::modifiers(s), s.key as u32) }.is_ok();
    if ok {
        // SAFETY: registered just above on this thread.
        let _ = unsafe { UnregisterHotKey(None, ID) };
    }
    ok
}

fn pack_shortcut(s: crate::hotkey::Shortcut) -> isize {
    let bit = |on: bool, n: u32| (on as isize) << n;
    s.key as isize | bit(s.ctrl, 16) | bit(s.alt, 17) | bit(s.shift, 18) | bit(s.win, 19)
}

fn unpack_shortcut(v: isize) -> crate::hotkey::Shortcut {
    let on = |n: u32| v >> n & 1 == 1;
    crate::hotkey::Shortcut { ctrl: on(16), alt: on(17), shift: on(18), win: on(19), key: (v & 0xFFFF) as u16 }
}

static LOOP_ENDED: AtomicBool = AtomicBool::new(false);

/// The app's message loop has ended (main.rs).
pub fn message_loop_ended() {
    LOOP_ENDED.store(true, Ordering::SeqCst);
}

/// Before the process ends: give a running self-test up to 20 s to write its
/// last checks (`exit_test` runs after the window is gone).
pub fn wait_for_report() {
    if !ENABLED.load(Ordering::SeqCst) {
        return;
    }
    let end = Instant::now() + Duration::from_secs(20);
    while !FINISHED.load(Ordering::SeqCst) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
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
        unpack(send(self.main, CMD_POINT, el.code()))
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

    /// Waits (up to `ms`) until the popup's description contains `needle`
    /// (imports run on a worker thread).
    fn wait_state(&self, needle: &str, ms: u64) -> bool {
        wait_until(ms, || self.describe().contains(needle))
    }

    /// Checks that the popup's state description contains every `needle`.
    fn state_has(&self, r: &mut Report, name: &str, needles: &[&str]) -> bool {
        let d = self.describe();
        r.check(name, needles.iter().all(|n| d.contains(n)), &d)
    }

    /// Replaces the text of Add media row `row` (empty text = select all + Backspace).
    fn rename(&self, r: &mut Report, row: usize, text: &str) -> bool {
        if !self.click(r, El::AddRow(row)) || !guard(r, &self.allowed) {
            return false;
        }
        press(&[VK_CONTROL, VK_A]);
        if text.is_empty() {
            press(&[VK_BACK]);
            std::thread::sleep(Duration::from_millis(150));
            true
        } else {
            self.type_text(r, text)
        }
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
        self.wheel_at(r, El::GalleryGap, notches)
    }

    fn wheel_at(&self, r: &mut Report, el: El, notches: i32) -> bool {
        let Some((x, y)) = self.point(el) else { return false };
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
    tray_click_test(r, t);

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
    caret_test(r, t);

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

    t.click(r, El::Tab(1));
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
    // Ctrl+Backspace after a space that is more than one byte used to end the
    // app. U+3000 is what Japanese and Chinese input methods
    // type; U+00A0 comes with pasted text.
    for (typed, kept) in [("猫\u{3000}犬", "猫\u{3000}"), ("dance\u{00A0}happy", "dance\u{00A0}")] {
        if !t.select_all_and_type(r, typed) || !guard(r, &t.allowed) {
            return false;
        }
        press(&[VK_CONTROL, VK_BACK]);
        std::thread::sleep(Duration::from_millis(200));
        let d = t.describe();
        r.check(
            &format!("Ctrl+Backspace after {typed:?} deletes one word and the app keeps running"),
            d.contains(&format!("query={kept:?}")),
            &d,
        );
    }
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
        // Resting on a tile shows its full name (Windows times the rest).
        let shown = t.wait_state("tip=\"Square.png\"", 3000);
        t.shot(r, "state-tile-tooltip");
        r.check("resting the pointer on a tile shows its full name", shown, &t.describe());
        // Off the tiles (moving within the same tile keeps the name, like Windows' own tips).
        t.park();
        let gone = t.wait_state("tip=\"\"", 1500);
        r.check("…and moving off the tile hides it", gone, &t.describe());
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

    // 8. Paste the original file of a picture, a GIF and a video into the target.
    if !paste_tests(r, t) {
        return false;
    }

    lost_target_test(r, t);

    // 9. Drag a tile into the target window.
    if !t.open(r) {
        return false;
    }
    drag_test(r, t);

    // 10. Clear history, then the empty Recently used state.
    if !t.open(r) {
        return false;
    }
    t.click(r, El::Tab(0));
    t.click(r, El::ClearHistory);
    t.park();
    t.shot(r, "state-history-cleared");
    let d = t.describe();
    r.check("Clear history empties Recently used", d.contains("list=Recent") && d.contains("count=0"), &d);
    t.click(r, El::EmptyAction);
    t.expect(r, "Show all media switches to All media", &ALL);

    // 11. Add media, fake files.
    if !add_media_test(r, t) {
        return false;
    }

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

    // 12. The Manager, from a fresh fake library.
    manager_test::run(r, t)
        && settings_test::run(r, t)
        && window_test::run(r, t)
        && preview_test::run(r, t)
        && watch_test::run(r, t)
}

/// Files in the test library's Media folders (not counting temporary files).
fn count_files(out: &Path) -> usize {
    crate::import::FOLDERS
        .iter()
        .filter_map(|f| std::fs::read_dir(out.join("app").join("Media").join(f)).ok())
        .flat_map(|d| d.flatten())
        .filter(|e| !e.file_name().to_string_lossy().starts_with(crate::import::PARTIAL_PREFIX))
        .count()
}

/// After the first import: the copies are real, sorted into their folders, the
/// sources are untouched, and the library file lists them.
fn import_on_disk(r: &mut Report, out: &Path) {
    let media = out.join("app").join("Media");
    let src = out.join("import-src");
    let same = |copy: &str, source: &str| std::fs::read(media.join(copy)).ok() == std::fs::read(src.join(source)).ok().filter(|b| !b.is_empty());
    r.check(
        "copies have the source's exact bytes, under the reviewed names",
        same("Pictures/Cat photo.png", "Cat.png")
            && same("Pictures/Birthday confetti.png", "IMG_4821.png")
            && same("GIFs/Wave.gif", "Wave.gif")
            && same("GIFs/Hello (2).gif", "Hello.gif")
            && same("Videos/Clip.mp4", "Clip.mp4"),
        "",
    );
    r.check(
        "the library's own Hello.gif was not overwritten",
        std::fs::read(media.join("GIFs/Hello.gif")).ok() != std::fs::read(src.join("Hello.gif")).ok(),
        "",
    );
    r.check("the duplicate was not copied", count_files(out) == 13, &format!("{} files", count_files(out)));
    let unchanged = seed::source_unchanged(out);
    r.check("the source files are unchanged", unchanged.is_ok(), &format!("{unchanged:?}"));
    let leftovers = crate::import::cleanup_partials(&media);
    r.check("no temporary files are left", leftovers == 0, &format!("{leftovers}"));
    let stored: Option<serde_json::Value> = std::fs::read_to_string(out.join("app").join("Data").join("pocketmedia.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let entry = |path: &str| {
        stored.as_ref().and_then(|v| v["items"].as_array()).and_then(|items| items.iter().find(|i| i["path"] == path).cloned())
    };
    let cat = entry("Pictures/Cat photo.png");
    r.check(
        "the library file lists the new files with their folder and SHA-256",
        cat.as_ref().is_some_and(|c| c["hash"].as_str().is_some_and(|h| h.len() == 64) && c["name"] == "Cat photo.png")
            && entry("GIFs/Hello (2).gif").is_some(),
        &format!("{cat:?}"),
    );
}

fn add_media_test(r: &mut Report, t: &Session) -> bool {
    if !t.open(r) {
        return false;
    }
    t.click(r, El::AddMediaButton);
    t.park();
    t.shot(r, "focused-11-add-media-start");
    t.state_has(r, "Add media opens on its start page", &["screen=add stage=start"]);

    t.click(r, El::AddChooseFiles);
    t.wait_state("stage=review", 8000);
    t.park();
    t.shot(r, "focused-12-add-media-files-review");
    t.state_has(
        r,
        "Choose files checks the real files: duplicate skipped, taken name numbered",
        &[
            "stage=review",
            "can_add=true",
            "label=\"Add 5 files\"",
            "rows=Cat.png:ok|Wave.gif:ok|Clip.mp4:ok|IMG_4821.png:ok|Portrait.png:skip|Hello (2).gif:ok-numbered",
        ],
    );
    // The × leaves a file out; Choose again below brings the full list back.
    t.click(r, El::AddRemove(2));
    t.park();
    t.shot(r, "phase7-add-media-row-removed");
    t.state_has(
        r,
        "the × on a review row leaves that file out",
        &["stage=review", "label=\"Add 4 files\"", "rows=Cat.png:ok|Wave.gif:ok|IMG_4821.png:ok|Portrait.png:skip|Hello (2).gif:ok-numbered"],
    );
    t.wheel_at(r, El::AddMain, 10);
    t.park();
    t.shot(r, "focused-12-add-media-files-review-scrolled1");
    // Choose again sits at the end of the list, visible now.
    t.click(r, El::AddChooseAgain);
    t.state_has(r, "Choose again returns to the start page", &["stage=start"]);
    t.click(r, El::AddChooseFiles);
    t.wait_state("stage=review", 8000);
    t.click(r, El::AddBack);
    t.state_has(r, "← on the review returns to the start page", &["stage=start"]);
    t.click(r, El::AddChooseFiles);
    t.wait_state("stage=review", 8000);

    // Renames, in the order of screens 12b-12e.
    let steps: [(usize, &str, &str, &[&str], &str); 4] = [
        (3, "Birthday confetti", "focused-12b-add-media-rename-valid", &["Birthday confetti.png:ok", "can_add=true"], "rename to a valid name keeps Add enabled"),
        (0, "Cat/photo", "focused-12c-add-media-rename-illegal-char", &["Cat/photo.png:IllegalChar", "can_add=false"], "\"Cat/photo\" is an error and disables Add"),
        (0, "", "focused-12d-add-media-rename-empty", &["rows=.png:Empty|", "can_add=false"], "an empty name is an error and disables Add"),
        (0, "Birthday confetti", "focused-12e-add-media-rename-duplicate", &["Birthday confetti.png:ok|", "Birthday confetti.png:Taken", "can_add=false"], "the same name as another row is an error and disables Add"),
    ];
    for (row, text, shot, needles, name) in steps {
        if !t.rename(r, row, text) {
            return false;
        }
        t.shot(r, shot);
        t.state_has(r, name, needles);
    }
    // More of the rules, through the real text box.
    for (text, needle) in [
        ("Cat.", "Cat..png:TrailingSpaceOrPeriod"),
        ("CON", "CON.png:Reserved"),
        ("cat photo ", "TrailingSpaceOrPeriod"),
        ("Square", "Square.png:Taken"),
        ("a:b", "a:b.png:IllegalChar"),
    ] {
        if !t.rename(r, 0, text) {
            return false;
        }
        t.state_has(r, &format!("\"{text}\" is rejected"), &[needle, "can_add=false"]);
    }
    if !t.rename(r, 0, "Cat photo") {
        return false;
    }
    t.state_has(r, "fixing the name enables Add again", &["Cat photo.png:ok", "can_add=true"]);

    t.click(r, El::AddAdd);
    t.wait_state("screen=gallery", 15000);
    t.park();
    t.shot(r, "focused-13-add-media-result");
    let mut expected = vec!["Hello (2).gif", "Birthday confetti.png", "Clip.mp4", "Wave.gif", "Cat photo.png"];
    expected.extend(ALL);
    t.expect(r, "added files appear first in All media (focused-13)", &expected);
    t.state_has(r, "the result line reports added and skipped files", &["5 files added · 1 duplicate skipped"]);
    import_on_disk(r, &t.out);
    t.wheel_gallery(r, 10);
    t.park();
    t.shot(r, "focused-13-add-media-result-scrolled1");
    t.wheel_gallery(r, -10);

    t.click(r, El::AddMediaButton);
    t.click(r, El::AddImportExisting);
    t.wait_state("stage=review", 8000);
    t.park();
    t.shot(r, "focused-14-add-media-folder-review");
    t.state_has(
        r,
        "a folder import takes subfolders and skips what is already there (focused-14)",
        &[
            "rows=Cat.png:skip|Clip.mp4:skip|Hello.gif:skip|IMG_4821.png:skip|Portrait.png:skip|Wave.gif:skip|notes.txt:unsupported|Wave.gif:skip|Clip (2).mp4:ok-numbered",
            "summary=\"1 ready to add · 7 duplicates skipped · 1 not supported\"",
            "can_add=true",
        ],
    );
    t.wheel_at(r, El::AddMain, 10);
    t.park();
    t.shot(r, "focused-14-add-media-folder-review-scrolled1");

    t.click(r, El::AddCancel);
    t.park();
    t.shot(r, "focused-15-add-media-cancelled");
    t.state_has(r, "Cancel returns to the gallery unchanged (focused-15)", &["screen=gallery", "count=13"]);
    r.check("Cancel on the review copies nothing", count_files(&t.out) == 13, &format!("{} files", count_files(&t.out)));

    t.click(r, El::AddMediaButton);
    if !guard(r, &t.allowed) {
        return false;
    }
    press(&[VK_ESCAPE]);
    std::thread::sleep(Duration::from_millis(150));
    t.state_has(r, "Escape leaves Add media without closing the popup", &["screen=gallery"]);
    r.check("popup still open after Escape in Add media", is_visible(t.popup), "");
    popup_drop_test(r, t);
    true
}

/// Dropping files on Add media starts the same import. While Add media
/// is open the popup stays open when another window is clicked, so files can be
/// dragged in from File Explorer (here: from the test target).
fn popup_drop_test(r: &mut Report, t: &Session) {
    let src = t.out.join("drop-in");
    let file = src.join("Dropped on popup.png");
    let _ = std::fs::create_dir_all(&src);
    if std::fs::write(&file, seed::png(48, 32, 221)).is_err() {
        r.check("drop test file made", false, &file.display().to_string());
        return;
    }
    if !t.open(r) {
        return;
    }
    t.click(r, El::AddMediaButton);
    let Some(p) = window_rect(t.popup) else { return };
    let to = (p.left + (p.right - p.left) / 2, p.top + (p.bottom - p.top) / 3);
    if drag_from_target(r, t, &file, to, t.popup) {
        let reviewed = t.wait_state("Dropped on popup.png:ok", 8000);
        let result = std::fs::read_to_string(t.out.join("drag-from-result.txt")).unwrap_or_default();
        r.check(
            "files dropped on the quick popup's Add media are checked for adding, like chosen files",
            reviewed && t.describe().contains("stage=review"),
            &format!("drag: {result}; {}", t.describe()),
        );
        r.check("…the popup stayed open while the other app was used for the drag", is_visible(t.popup), "");
        r.check("…the dropped file is still where it was (a copy is made only on Add)", file.is_file(), &file.display().to_string());
        t.shot(r, "state-popup-dropped");
    }
    // Back to the gallery without adding, so the library stays as the next checks expect.
    send(t.main, CMD_FOCUS, t.popup);
    wait_until(1500, || foreground() == t.popup);
    for _ in 0..3 {
        if !t.describe().contains("screen=add") || !guard(r, &t.allowed) {
            break;
        }
        press(&[VK_ESCAPE]);
        std::thread::sleep(Duration::from_millis(200));
    }
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
        "tray menu (Quick popup, Open manager, Settings, Exit)",
        "Windows does not deliver simulated clicks to tray icons; checked by hand",
    );
}

/// A left click on the tray icon opens the quick popup. Windows does
/// not deliver simulated clicks to tray icons, so the test posts the icon's own
/// callback (a left button released), as Windows does for a real click.
fn tray_click_test(r: &mut Report, t: &Session) {
    post(t.main, crate::WM_TRAY, 0, WM_LBUTTONUP as isize);
    let shown = wait_until(2000, || is_visible(t.popup));
    r.check("a left click on the tray icon opens the quick popup (its callback, posted)", shown, &describe_foreground());
    post(t.popup, WM_CLOSE, 0, 0);
    wait_until(1500, || !is_visible(t.popup));
    send(t.main, CMD_FOCUS, t.target);
    wait_until(1500, || foreground() == t.target);
}

/// The popup opens next to the target app's text caret. The test
/// target shows a caret (at `test_target::CARET`) while `caret.txt` exists.
fn caret_test(r: &mut Report, t: &Session) {
    let flag = t.out.join("caret.txt");
    let _ = std::fs::write(&flag, b"on");
    // The caret appears when the target gets the focus back: open the popup and
    // close it again (Escape hands the focus back).
    if !t.open(r) || !guard(r, &t.allowed) {
        let _ = std::fs::remove_file(&flag);
        return;
    }
    press(&[VK_ESCAPE]);
    wait_until(1500, || !is_visible(t.popup) && foreground() == t.target);
    std::thread::sleep(Duration::from_millis(200));
    let mut caret = windows::Win32::Foundation::POINT { x: test_target::CARET.0, y: test_target::CARET.1 };
    // SAFETY: the target's own window; caret is a local.
    let _ = unsafe { windows::Win32::Graphics::Gdi::ClientToScreen(hwnd(t.target), &mut caret) };
    let opened = t.open(r);
    let placed = unpack(send(t.main, CMD_POPUP_PLACED, 0));
    let rect = window_rect(t.popup);
    // The caret's bottom-left corner: same x, a caret's height (18 px at 96 DPI) lower.
    let near = placed.is_some_and(|(x, y)| x == caret.x && (caret.y..=caret.y + 60).contains(&y));
    let beside = match (rect, placed) {
        (Some(p), Some((_, y))) => p.top >= y || p.bottom <= y - 10,
        _ => false,
    };
    r.check(
        "the popup opens next to the app's text caret, not over it",
        opened && near && beside,
        &format!("caret top-left {:?}; popup placed at {placed:?}; popup {rect:?}", (caret.x, caret.y)),
    );
    let _ = std::fs::remove_file(&flag);
    if guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup));
    wait_until(1500, || foreground() == t.target);
}

fn drag_test(r: &mut Report, t: &Session) {
    // The first tile of whichever list is showing.
    let d = t.describe();
    let dragged = d.split("names=").nth(1).and_then(|n| n.split('|').next()).unwrap_or("").to_string();
    let _ = std::fs::remove_file(t.out.join("drop-received.png"));
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
    if !drag_to_target(r, t, (fx, fy)) {
        return;
    }
    let received = t.out.join("drop-received.png");
    let got = wait_until(4000, || received.exists());
    let source = media_file(&t.out, &dragged);
    let original = std::fs::read(&source).ok();
    let same = got && original.is_some() && std::fs::read(&received).ok() == original;
    r.check(
        &format!("target received {dragged} by drag, byte for byte"),
        same,
        if got { "bytes compared with the library file" } else { "nothing dropped within 4 s" },
    );
    let from = std::fs::read_to_string(t.out.join("drop-source.txt")).unwrap_or_default();
    r.check("the drag offered the library's own file", from == source.display().to_string(), &from);
    r.check("drag copied, the library file is kept", source.is_file(), &source.display().to_string());
    let (kind, text) = status(t.main);
    r.check("app records drag result", kind == 1, &text);
    t.open(r);
    t.click(r, El::Tab(0));
    let d = t.describe();
    r.check("a finished copy-drag goes to the top of Recently used", d.contains(&format!("names={dragged}|")), &d);
}

/// Drags from `from` (a tile) to a point of the test target that no other window
/// covers, and releases there. If the target cannot be reached, the button is
/// released back over the tile and a FAIL is logged; returns false.
fn drag_to_target(r: &mut Report, t: &Session, from: (i32, i32)) -> bool {
    let Some(to) = window_rect(t.target) else {
        return r.check("drag target found", false, "");
    };
    let pid = window_pid(t.target);
    // The centre first, then a grid over the target (another window may cover part of it).
    let spot = std::iter::once((4, 4))
        .chain((1..8).flat_map(|i| (1..8).map(move |j| (i, j))))
        .map(|(i, j)| (to.left + (to.right - to.left) * i / 8, to.top + (to.bottom - to.top) * j / 8))
        .find(|&(x, y)| window_pid(window_at(x, y)) == pid);
    let Some((tx, ty)) = spot else {
        return r.check("drag reaches test target", false, "other windows cover the whole test target");
    };
    let (fx, fy) = from;
    if !guard(r, &t.allowed) {
        return false;
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
    if window_pid(over) != pid {
        mouse_to(fx, fy);
        std::thread::sleep(Duration::from_millis(100));
        mouse_button(false);
        return r.check("drag reaches test target", false, &format!("window under pointer belongs to pid {}", window_pid(over)));
    }
    mouse_button(false);
    true
}

/// Drags `file` from the test target (a separate process, like File Explorer)
/// to the screen point `to`, which must be on PocketMedia's window `into`, and
/// releases there. The target starts a real OLE drag (copy only) when the button
/// goes down on it. Returns false (with a FAIL) if the drag could not be made.
fn drag_from_target(r: &mut Report, t: &Session, file: &Path, to: (i32, i32), into: isize) -> bool {
    let order = t.out.join("drag-from.txt");
    let _ = std::fs::remove_file(t.out.join("drag-from-result.txt"));
    if std::fs::write(&order, file.display().to_string()).is_err() {
        return r.check("drag source prepared", false, &order.display().to_string());
    }
    let Some(from) = window_rect(t.target) else {
        return r.check("drag source found", false, "");
    };
    let pid = window_pid(t.target);
    // A point of the target that no other window covers.
    let spot = (1..8)
        .flat_map(|i| (1..8).map(move |j| (i, j)))
        .map(|(i, j)| (from.left + (from.right - from.left) * i / 8, from.top + (from.bottom - from.top) * j / 8))
        .find(|&(x, y)| window_pid(window_at(x, y)) == pid);
    let Some((fx, fy)) = spot else {
        let _ = std::fs::remove_file(&order);
        return r.check("drag starts on the test target", false, "other windows cover the whole test target");
    };
    if !guard(r, &t.allowed) {
        let _ = std::fs::remove_file(&order);
        return false;
    }
    mouse_to(fx, fy);
    std::thread::sleep(Duration::from_millis(150));
    mouse_button(true);
    std::thread::sleep(Duration::from_millis(250));
    for i in 1..=5 {
        mouse_to(fx - i * 6, fy + i * 2); // past the system drag threshold
        std::thread::sleep(Duration::from_millis(30));
    }
    let (tx, ty) = to;
    for i in 1..=15 {
        mouse_to(fx + (tx - fx) * i / 15, fy + (ty - fy) * i / 15);
        std::thread::sleep(Duration::from_millis(40));
    }
    std::thread::sleep(Duration::from_millis(300));
    let over = window_at(tx, ty);
    if over != into {
        // Not over the window it should drop on: Escape cancels the drag.
        press(&[VK_ESCAPE]);
        mouse_button(false);
        let _ = std::fs::remove_file(&order);
        return r.check("drag reaches PocketMedia", false, &format!("window under the pointer: {over:#x}, wanted {into:#x}"));
    }
    mouse_button(false);
    let done = t.out.join("drag-from-result.txt");
    wait_until(3000, || done.exists());
    let _ = std::fs::remove_file(&order);
    true
}

/// A library file in the self-test's Media folder, by name.
fn media_file(out: &Path, name: &str) -> PathBuf {
    let folder = match crate::media_info::kind_for(name) {
        Some(k) => k.folder(),
        None => "",
    };
    out.join("app").join("Media").join(folder).join(name)
}

/// Paste the original file of each kind into the test target, check
/// the clipboard formats, history, Close after inserting off, and a failed paste.
fn paste_tests(r: &mut Report, t: &Session) -> bool {
    let fresh = |t: &Session| {
        for f in ["paste-formats.txt", "paste-received.png", "paste-hdrop.txt", "paste-dib.txt"] {
            let _ = std::fs::remove_file(t.out.join(f));
        }
    };
    let paste = |r: &mut Report, t: &Session, query: &str| -> bool {
        fresh(t);
        if !t.open(r) {
            return false;
        }
        t.click(r, El::Search);
        if !t.select_all_and_type(r, query) || !guard(r, &t.allowed) {
            return false;
        }
        press(&[VK_RETURN]);
        true
    };
    // (search, file, formats that must be there, formats that must not)
    let cases: [(&str, &str, &[&str], &[&str]); 3] = [
        ("square", "Square.png", &["PNG", "CF_DIB", "CF_HDROP", "Preferred DropEffect"], &[]),
        ("happy dance", "Happy dance.gif", &["GIF", "CF_HDROP", "Preferred DropEffect"], &["PNG", "CF_DIB"]),
        ("race clip", "Race clip.mp4", &["CF_HDROP", "Preferred DropEffect"], &["PNG", "CF_DIB", "GIF"]),
    ];
    for (query, name, needed, absent) in cases {
        if !paste(r, t, query) {
            return false;
        }
        r.check(&format!("paste of {name} closes the popup"), wait_until(3000, || !is_visible(t.popup)), "");
        let formats_file = t.out.join("paste-formats.txt");
        let got = wait_until(3000, || formats_file.exists());
        let formats = std::fs::read_to_string(&formats_file).unwrap_or_default();
        let has = |f: &str| formats.lines().any(|l| l == f);
        r.check(
            &format!("{name}: the target received a paste with the right formats"),
            got && needed.iter().all(|f| has(f)) && !absent.iter().any(|f| has(f)),
            &formats.replace('\n', ", "),
        );
        let file = media_file(&t.out, name);
        let hdrop = std::fs::read_to_string(t.out.join("paste-hdrop.txt")).unwrap_or_default();
        r.check(&format!("{name}: the clipboard offers the original file"), hdrop == file.display().to_string(), &hdrop);
        if name.ends_with(".png") {
            let png = std::fs::read(t.out.join("paste-received.png")).ok();
            r.check("the PNG on the clipboard is the original file, byte for byte", png.is_some() && png == std::fs::read(&file).ok(), "");
            let dib = std::fs::read_to_string(t.out.join("paste-dib.txt")).unwrap_or_default();
            r.check("the bitmap on the clipboard has the picture's size (300 x 300)", dib == "300 300", &dib);
        }
        let (kind, text) = status(t.main);
        r.check(&format!("{name}: the app records the paste"), kind == 1, &text);
    }
    t.open(r);
    t.click(r, El::Tab(0));
    t.state_has(
        r,
        "pastes go to the top of Recently used, newest first",
        &["names=Race clip.mp4|Happy dance.gif|Square.png|"],
    );
    if guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup));

    // Close after inserting off: the popup stays (without focus), the paste still arrives.
    send(t.main, CMD_SET_CLOSE_AFTER, 0);
    if !paste(r, t, "hello") {
        return false;
    }
    let formats_file = t.out.join("paste-formats.txt");
    let got = wait_until(3000, || formats_file.exists());
    r.check("with Close after inserting off, the paste still arrives", got, "");
    r.check("…and the popup stays on screen", is_visible(t.popup), "");
    r.check("…without taking focus from the app", foreground() == t.target, &describe_foreground());
    t.shot(r, "state-popup-stays-open");
    send(t.main, CMD_SET_CLOSE_AFTER, 1);
    if t.open(r) && guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup));

    // A paste that cannot be sent: the popup comes back and says so.
    send(t.main, CMD_FAIL_NEXT_PASTE, 0);
    if !paste(r, t, "wallpaper") {
        return false;
    }
    let back = wait_until(4000, || is_visible(t.popup) && t.describe().contains("Paste didn't go through"));
    r.check("a failed paste brings the popup back and says so", back, &t.describe());
    r.check("nothing was sent to the app", !t.out.join("paste-formats.txt").exists(), "");
    t.shot(r, "state-popup-paste-failed");
    if guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup));

    // When Windows cannot say whether the app runs as administrator,
    // Ctrl+V might be dropped without a word, so it is not sent at all.
    send(t.main, CMD_UNKNOWN_ELEVATION, 0);
    if !paste(r, t, "portrait") {
        return false;
    }
    let told = wait_until(3000, || t.describe().contains("press Ctrl+V in"));
    std::thread::sleep(Duration::from_millis(800));
    let (_, text) = status(t.main);
    r.check(
        "when the app's administrator state is unknown, nothing is sent and the popup says to press Ctrl+V",
        told && text.contains("may run as administrator") && !t.out.join("paste-formats.txt").exists(),
        &format!("{text} | {}", t.describe()),
    );
    // Reopened, the search is empty again: Recently used must not start with it.
    if guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup));
    send(t.main, CMD_FOCUS, t.target);
    wait_until(1500, || foreground() == t.target);
    if !t.open(r) {
        return false;
    }
    t.click(r, El::Tab(0));
    let d = t.describe();
    r.check(
        "…and it is not counted as used",
        d.contains("list=Recent") && d.contains("query=\"\"") && !d.contains("names=Portrait.png"),
        &d,
    );
    if guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup));
    true
}

/// After a GPU reset or a remote desktop reconnect, Direct2D asks
/// for a new drawing target. The window must ask for a new frame by itself.
/// Run on a popup with nothing animating (an empty search), so no GIF frame
/// repaints it by chance.
fn lost_target_test(r: &mut Report, t: &Session) {
    if !t.open(r) {
        return;
    }
    t.click(r, El::Search);
    if !t.select_all_and_type(r, "zzzz") {
        return;
    }
    t.park();
    std::thread::sleep(Duration::from_millis(500));
    let before = send(t.main, CMD_LOSE_TARGET, 0);
    let redrawn = wait_until(2000, || send(t.main, CMD_D2D_TARGETS, 0) > before);
    let after = send(t.main, CMD_D2D_TARGETS, 0);
    t.shot(r, "state-popup-after-lost-target");
    r.check(
        "after its drawing target is lost, the popup draws itself again",
        redrawn && is_visible(t.popup),
        &format!("drawing targets made {before} -> {after}; {}", t.describe()),
    );
    if t.select_all_and_type(r, "") && guard(r, &t.allowed) {
        press(&[VK_BACK]);
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup));
}

/// The folders beside the program.
fn folders_test(r: &mut Report) {
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else {
        r.check("program folder found", false, "current_exe failed");
        return;
    };
    r.info(&format!("program folder: {}", dir.display()));
    for rel in [r"Media\Pictures", r"Media\GIFs", r"Media\Videos", "Data", "Cache"] {
        let path = dir.join(rel);
        r.check(&format!("folder {rel} exists beside the program"), path.is_dir(), &path.display().to_string());
    }
}

/// A copy of the program in a folder where a file named "Media" blocks the
/// Media folder: it must say which folder failed, leave the file alone and exit.
fn blocked_folder_test(r: &mut Report, out: &Path, exe: &Path) {
    let dir = out.join("blocked-folder");
    let _ = std::fs::remove_dir_all(&dir);
    let copy = dir.join("pocketmedia.exe");
    let blocker = dir.join("Media");
    let prepared = std::fs::create_dir_all(&dir)
        .and_then(|()| std::fs::copy(exe, &copy))
        .and_then(|_| std::fs::write(&blocker, b"not a folder"));
    if let Err(e) = prepared {
        r.check("prepare the blocked-folder copy", false, &e.to_string());
        return;
    }
    let Ok(mut child) = Command::new(&copy).arg("--exit-after-start").spawn() else {
        r.check("blocked-folder copy launched", false, "spawn failed");
        return;
    };
    let mut boxed = 0isize;
    let shown = wait_until(5000, || {
        boxed = find_window("#32770", Some(crate::paths::FAILURE_TITLE), child.id());
        boxed != 0
    });
    if r.check("a folder that cannot be created shows a message", shown, "") {
        std::thread::sleep(Duration::from_millis(300));
        let text = longest_child_text(boxed);
        r.check(
            "the message names the folder and what to do",
            text.contains(&blocker.display().to_string()) && text.contains("Move it to a folder you can write to"),
            &text.replace('\n', " / "),
        );
        save_capture(r, &out.join("screens").join("folder-error.png"), capture_window(boxed));
        post(boxed, WM_CLOSE, 0, 0);
    }
    let mut code = None;
    let exited = wait_until(5000, || {
        code = child.try_wait().ok().flatten().and_then(|s| s.code());
        code.is_some()
    });
    r.check("it exits without starting", exited && code == Some(2), &format!("exit code {code:?}"));
    if !exited {
        let _ = child.kill();
    }
    let untouched = std::fs::read(&blocker).is_ok_and(|b| b == b"not a folder");
    r.check("the file in the way is left alone", untouched, "");
    let _ = std::fs::remove_dir_all(&dir);
}

/// One PocketMedia at a time: a second launch of this
/// install shows this one's popup and exits; a second launch from autostart exits
/// without showing anything; a copy in another folder hands nothing over.
fn instance_test(r: &mut Report, t: &Session, exe: &Path) {
    let launch = |args: &[&str]| -> Option<i32> {
        let mut child = Command::new(exe).args(args).spawn().ok()?;
        let mut code = None;
        wait_until(8000, || {
            code = child.try_wait().ok().flatten().and_then(|s| s.code());
            code.is_some()
        });
        if code.is_none() {
            let _ = child.kill();
        }
        code
    };
    let popup_hidden = !is_visible(t.popup);
    let code = launch(&[]);
    r.check("a second launch exits at once (exit code 0)", code == Some(0), &format!("exit code {code:?}"));
    let shown = wait_until(3000, || is_visible(t.popup));
    // In front only when Windows lets it be (a launch from Explorer does).
    wait_until(1000, || foreground() == t.popup);
    r.check(
        "…and the running PocketMedia shows its popup instead",
        popup_hidden && shown,
        &format!("hidden before: {popup_hidden}; {}", describe_foreground()),
    );
    if is_visible(t.popup) {
        post(t.popup, WM_CLOSE, 0, 0);
        wait_until(1500, || !is_visible(t.popup));
    }
    let code = launch(&[crate::autostart::ARG]);
    std::thread::sleep(Duration::from_millis(800));
    r.check(
        "a second launch from autostart exits quietly (no popup)",
        code == Some(0) && !is_visible(t.popup),
        &format!("exit code {code:?}, popup visible {}", is_visible(t.popup)),
    );
    r.check("the tray icon and shortcut stay with the running copy", tray_icon_rect(t.main).is_some() && send(t.main, CMD_HOTKEY_OK, 0) == 1, "");

    // The uninstaller's Back up library… button: a launch with --backup shows
    // Settings at Back up library and opens its folder picker (closed here).
    let manager = send(t.main, CMD_MANAGER, 0);
    if is_visible(manager) {
        post(manager, WM_CLOSE, 0, 0);
        wait_until(1500, || !is_visible(manager));
    }
    crate::dialogs::set_test_backup(None);
    let describe = || {
        send(t.main, CMD_MANAGER_DESCRIBE, 0);
        DESCRIBE.lock().map(|d| d.clone()).unwrap_or_default()
    };
    let code = launch(&["--backup"]);
    let manager = send(t.main, CMD_MANAGER, 0);
    let done = wait_until(4000, || is_visible(manager) && describe().contains("Backup cancelled: no folder was chosen."));
    let d = describe();
    r.check(
        "a launch with --backup (the uninstaller's Back up library…) opens Settings and the backup folder picker",
        code == Some(0) && done && d.contains("screen=settings"),
        &format!("exit code {code:?}; {d}"),
    );
    if is_visible(manager) {
        post(manager, WM_CLOSE, 0, 0);
        wait_until(1500, || !is_visible(manager));
    }
    // Back up library from the uninstaller, and a second copy of the program.
    uninstall_test::backup_waits(r, t, exe);
    uninstall_test::other_copy(r, t, exe);
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

#[cfg(test)]
mod tests {
    /// The Exit check asks the UI thread for the shortcut; it must arrive unchanged.
    #[test]
    fn a_shortcut_survives_the_trip_from_the_window_thread() {
        use super::*;
        for s in [
            crate::hotkey::Shortcut::default(),
            crate::hotkey::Shortcut { ctrl: true, alt: false, shift: true, win: false, key: 0x75 },
            crate::hotkey::Shortcut { ctrl: true, alt: true, shift: true, win: true, key: 0xFE },
        ] {
            assert_eq!(unpack_shortcut(pack_shortcut(s)), s);
        }
    }

    /// Every self-test command has its own number: a shared one runs the wrong
    /// command (CMD_REQUEST_EXIT once shared 21 with CMD_PREVIEW_DESCRIBE and
    /// ended the self-test early).
    #[test]
    fn self_test_commands_have_distinct_numbers() {
        use super::*;
        let all = [
            CMD_TARGET,
            CMD_STATUS,
            CMD_FOCUS,
            CMD_POPUP,
            CMD_EXIT,
            CMD_HOTKEY_OK,
            CMD_POINT,
            CMD_DESCRIBE,
            CMD_RESET,
            CMD_MANAGER,
            CMD_MANAGER_POINT,
            CMD_MANAGER_DESCRIBE,
            CMD_MANAGER_ADD_POINT,
            CMD_SETTINGS_POINT,
            CMD_SETTINGS_SCROLL,
            CMD_FORGET_TARGET,
            CMD_STORE_DESCRIBE,
            CMD_STORE_REOPEN,
            CMD_SET_CLOSE_AFTER,
            CMD_FAIL_NEXT_PASTE,
            CMD_REQUEST_EXIT,
            CMD_BACKUP_START,
            CMD_POPUP_PLACED,
            CMD_AGE_TARGET,
            CMD_UNKNOWN_ELEVATION,
            CMD_LOSE_TARGET,
            CMD_D2D_TARGETS,
            CMD_SHORTCUT,
            preview_test::CMD_PREVIEW_DESCRIBE,
            preview_test::CMD_THUMBS_RESET,
            preview_test::CMD_SET_VIDEO_PREVIEW,
            preview_test::CMD_MANAGER_SHOW,
            watch_test::CMD_LIB_FIND,
            watch_test::CMD_WATCH_DESCRIBE,
            watch_test::CMD_WATCH_RESTART,
            shortcut_test::CMD_SC_TRY,
            shortcut_test::CMD_SC_FREE,
        ];
        let mut sorted = all.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "two self-test commands share a number: {all:?}");
        // The ranges the UI thread dispatches on hold only their own module's commands.
        for c in all {
            let in_preview = (preview_test::CMD_PREVIEW_DESCRIBE..=preview_test::CMD_MANAGER_SHOW).contains(&c);
            let in_watch = (watch_test::CMD_LIB_FIND..=watch_test::CMD_WATCH_RESTART).contains(&c);
            let in_shortcut = (shortcut_test::CMD_SC_TRY..=shortcut_test::CMD_SC_FREE).contains(&c);
            let own = [
                preview_test::CMD_PREVIEW_DESCRIBE,
                preview_test::CMD_THUMBS_RESET,
                preview_test::CMD_SET_VIDEO_PREVIEW,
                preview_test::CMD_MANAGER_SHOW,
                watch_test::CMD_LIB_FIND,
                watch_test::CMD_WATCH_DESCRIBE,
                watch_test::CMD_WATCH_RESTART,
                shortcut_test::CMD_SC_TRY,
                shortcut_test::CMD_SC_FREE,
            ]
            .contains(&c);
            assert!(own || !(in_preview || in_watch || in_shortcut), "command {c} falls in another module's range");
        }
    }
}
