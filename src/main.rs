//! PocketMedia.
//!
//! - Tray icon and menu, the global shortcut (Win+Alt+. by default), focus
//!   hand-back to the app that was in front.
//! - The quick popup, Add media and the Manager, drawn with Direct2D/DirectWrite.
//! - The library: the Media, Data and Cache folders beside the program, the
//!   library file `Data\pocketmedia.json`, import, names on disk, search and the
//!   Recycle Bin.
//! - Transfer: paste and drag of the library's original files.
//! - Previews: thumbnails with a disk and a RAM cache, GIFs that loop while
//!   visible and a muted hover video.
//! - The Media folders are watched for changes made in File Explorer; Back up
//!   library and Autostart.
//! - One PocketMedia at a time, each install with its own identity
//!   (`instance.rs`); backups of the whole Media folder say "(unfinished)" until
//!   checked, and Exit or the end of the Windows session ask before stopping one.

#![windows_subsystem = "windows"]

mod add_media;
mod add_panel;
mod autostart;
mod backup;
mod compose;
mod dialogs;
mod download;
mod drop_target;
mod digest;
mod fileops;
mod gif;
mod gif_writer;
mod hotkey;
mod http;
mod import;
mod instance;
mod jobs;
mod library;
mod manager;
mod manager_state;
mod media_info;
mod names;
mod paste;
mod paths;
mod pixels;
mod popup;
mod popup_state;
mod preview;
mod sample;
mod search;
mod selftest;
mod settings_state;
mod sha256;
mod store;
mod target;
mod test_png;
mod test_target;
mod text_field;
mod thumb_store;
mod thumbs;
mod transfer;
mod tray;
mod twitter;
mod ui;
mod video;
mod visibility;
mod watch;
mod wic;
mod winutil;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::target::{NoTarget, Target};

pub const WM_TRAY: u32 = WM_APP + 1;
pub const WM_SELFTEST: u32 = WM_APP + 2;
/// Posted when a window was hidden or work in a hidden window ended: hidden
/// windows may be released now (`release_hidden`).
pub const WM_RELEASE: u32 = WM_APP + 7;
pub const TIMER_PASTE: usize = 1;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Neutral,
    Good,
    Bad,
}

/// UI-thread state. Only `Cell`s and short-lived `RefCell` borrows, because window
/// procedures re-enter each other (message boxes, menus and drag loops pump messages).
pub struct App {
    pub main: Cell<HWND>,
    /// Self-test mode: report problems in the log instead of message boxes.
    pub quiet: Cell<bool>,
    pub taskbar_created: Cell<u32>,
    /// Sent by a second launch (`instance.rs`).
    pub activate: Cell<u32>,
    pub target: RefCell<Result<Target, NoTarget>>,
    pub status: RefCell<(String, StatusKind)>,
}

thread_local! {
    static APP: App = App {
        main: Cell::new(HWND::default()),
        quiet: Cell::new(false),
        taskbar_created: Cell::new(0),
        activate: Cell::new(0),
        target: RefCell::new(Err(NoTarget::NothingFocused)),
        status: RefCell::new((String::new(), StatusKind::Neutral)),
    };
}

thread_local! {
    /// Lock and unlock notifications for the main window (dropped on WM_DESTROY).
    static SESSION_NOTES: RefCell<Option<visibility::SessionNotes>> = const { RefCell::new(None) };
}

pub fn app<R>(f: impl FnOnce(&App) -> R) -> R {
    APP.with(f)
}

/// Records the result of the last paste/drag (for the self-test log). It is never
/// shown as a Windows notification; the popup shows its own short line instead.
pub fn set_status(text: impl Into<String>, kind: StatusKind) {
    let text = text.into();
    app(|a| *a.status.borrow_mut() = (text, kind));
}

/// Command-line modes. A normal launch has no arguments and opens the popup;
/// `--autostart` (from the Run key) starts quietly in the tray; `--backup` (the
/// uninstaller's Back up library… button) opens Settings at Back up library.
enum Mode {
    Normal { exit_after_start: bool, autostart: bool, backup: bool },
    SelfTest(PathBuf),
    TestTarget(PathBuf),
    /// Self-test only: load the library beside this copy of the program (as a
    /// start does), write what was found to the file, and exit.
    CheckLibrary(PathBuf),
    /// The uninstaller's question: is this copy running, another one, or none?
    /// Writes the answer to the file (see `instance::report_running`).
    WhichRunning(PathBuf),
}

fn parse_mode() -> Mode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = |default: &str| PathBuf::from(args.get(1).cloned().unwrap_or_else(|| default.into()));
    match args.first().map(String::as_str) {
        Some("--self-test") => Mode::SelfTest(path("test-results")),
        Some("--test-target") => Mode::TestTarget(path("test-results")),
        Some("--check-library") => Mode::CheckLibrary(path("check-library.txt")),
        Some("--which-running") => Mode::WhichRunning(path("which-running.txt")),
        Some("--exit-after-start") => Mode::Normal { exit_after_start: true, autostart: false, backup: false },
        Some(autostart::ARG) => Mode::Normal { exit_after_start: false, autostart: true, backup: false },
        Some("--backup") => Mode::Normal { exit_after_start: false, autostart: false, backup: true },
        _ => Mode::Normal { exit_after_start: false, autostart: false, backup: false },
    }
}

fn main() {
    let mode = parse_mode();
    if let Mode::TestTarget(out) = &mode {
        // SAFETY: called once at startup, before any window exists.
        let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        std::process::exit(test_target::run(out));
    }
    if let Mode::WhichRunning(out) = &mode {
        std::process::exit(instance::report_running(out));
    }
    // The folders come first: without them there is nowhere to keep media.
    let folders = paths::Paths::beside_exe()
        .map_err(|e| (PathBuf::from("(program folder)"), e))
        .and_then(|p| p.create().map(|()| p));
    let folders = match (folders, &mode) {
        // The self-test checks the folders beside the program, then works in its own
        // fresh copy under the results folder, so it never touches a real library.
        (Ok(_), Mode::SelfTest(out)) => {
            let p = paths::Paths::under(&out.join("app"));
            p.create().map(|()| p)
        }
        (other, _) => other,
    };
    let paths = match folders {
        Ok(p) => p,
        Err((dir, e)) => {
            message_box(&paths::failure_message(&dir, &e), paths::FAILURE_TITLE, MB_ICONERROR);
            std::process::exit(2);
        }
    };
    if let Mode::CheckLibrary(report) = &mode {
        std::process::exit(check_library(paths, report));
    }
    // One PocketMedia at a time. A later launch of this same install hands its
    // request to the running one; a launch of another install hands nothing over.
    let _instance = match instance::claim() {
        instance::Claim::Mine(i) => i,
        instance::Claim::SameInstall => match mode {
            Mode::Normal { autostart: true, .. } => std::process::exit(0),
            Mode::SelfTest(_) => {
                message_box(
                    "Another PocketMedia is running. Exit it from the tray, then run the self-test again.",
                    "PocketMedia - self-test",
                    MB_ICONWARNING,
                );
                std::process::exit(1);
            }
            Mode::Normal { backup: true, .. } => std::process::exit(if instance::activate_running(instance::BACK_UP) { 0 } else { 4 }),
            _ => std::process::exit(if instance::activate_running(instance::SHOW_POPUP) { 0 } else { 4 }),
        },
        instance::Claim::OtherInstall(folder) => {
            if !matches!(mode, Mode::Normal { autostart: true, .. }) {
                let backup = matches!(mode, Mode::Normal { backup: true, .. });
                message_box(&instance::other_copy_message(folder.as_deref(), backup), instance::OTHER_COPY_TITLE, MB_ICONWARNING);
            }
            std::process::exit(3);
        }
    };
    let outcome = store::init(&paths.data);
    if matches!(mode, Mode::SelfTest(_)) {
        // The self-test never changes what starts with Windows; it starts from "off".
        autostart::use_test_key();
        autostart::remove_test_key();
    }
    autostart::sync_store();
    let media = paths.media.clone();
    paths::init(paths);
    let quiet = matches!(mode, Mode::SelfTest(_));
    if let Some(text) = outcome.message()
        && !quiet
    {
        message_box(&text, "PocketMedia - library file", MB_ICONWARNING);
    }
    let (missing, unsaved) = load_library(&media);
    if let Some(text) = missing
        && !quiet
    {
        message_box(&text, "PocketMedia - library", MB_ICONINFORMATION);
    }
    // The comparison changed the library, but the file could not be saved.
    if let Some(text) = unsaved
        && !quiet
    {
        message_box(&text, "PocketMedia - library file", MB_ICONWARNING);
    }
    // Ignore failure: it only fails if awareness was already set (e.g. by a manifest).
    // SAFETY: called once at startup, before any window exists.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // SAFETY: once, on the thread that will own every window; paired with OleUninitialize below.
    if let Err(e) = unsafe { OleInitialize(None) } {
        fatal(&format!("Could not initialise OLE (needed for drag and drop): {e}"));
        return;
    }

    if let Err(e) = run(&mode) {
        fatal(&e);
    }

    // SAFETY: pairs the OleInitialize above, on the same thread, after every window is gone.
    unsafe { OleUninitialize() };
    selftest::wait_for_report();
    std::process::exit(selftest::exit_code());
}

/// Loads the library from the store and makes it agree with the Media folders
/// (an interrupted import leaves no orphan or half-written file):
/// leftover temporary copies are deleted, finished copies without a record are
/// added, and records whose file is gone are removed. Returns a message when
/// files were missing, and one when the changed library could not be saved.
fn load_library(media: &std::path::Path) -> (Option<String>, Option<String>) {
    let data = store::with(|s| s.data.clone()).unwrap_or_default();
    let mut lib = library::Library::from_store(&data);
    import::cleanup_partials(media);
    // Twitter downloads an earlier run did not import (off this thread: it may be large).
    let _ = std::thread::Builder::new().name("download-cleanup".into()).spawn(download::cleanup_stale);
    let records: Vec<(u32, String)> = lib.items.iter().map(|i| (i.id, i.path.clone())).collect();
    let (orphans, missing) = import::reconcile(media, &records);
    let missing_names: Vec<String> =
        missing.iter().filter_map(|id| lib.get(*id).map(|i| i.name.clone())).collect();
    for id in &missing {
        lib.remove(*id);
    }
    for o in &orphans {
        lib.adopt(o);
    }
    let changed = !orphans.is_empty() || !missing.is_empty();
    library::set(lib);
    let unsaved = if changed { library::persist().err().filter(|_| !store::read_only()) } else { None };
    let missing = (!missing_names.is_empty()).then(|| {
        format!(
            "{} file(s) were no longer in the Media folder and were removed from the library:\n\n{}",
            missing_names.len(),
            missing_names.join("\n")
        )
    });
    (missing, unsaved.map(|e| unsaved_after_compare(&e, orphans.len(), missing_names.len())))
}

/// What to say when the start-up comparison changed the library but the
/// library file could not be saved.
fn unsaved_after_compare(why: &str, added: usize, removed: usize) -> String {
    let what = match (added, removed) {
        (a, 0) => format!("added {a} file(s) it found in the Media folder to the library"),
        (0, r) => format!("removed {r} missing file(s) from the library"),
        (a, r) => format!("added {a} file(s) it found in the Media folder and removed {r} missing file(s)"),
    };
    let why = why.trim_end_matches('.');
    format!(
        "PocketMedia {what}, but {why}.\n\nNothing is lost: the library is right while PocketMedia runs, and the \
         same check runs again at the next start. If this message comes back, check that the Data folder beside \
         PocketMedia can be written to (not read-only and not full)."
    )
}

/// `--check-library`: what a start finds in the library beside this program.
fn check_library(paths: paths::Paths, report: &std::path::Path) -> i32 {
    let outcome = store::init(&paths.data);
    let media = paths.media.clone();
    let (message, unsaved) = load_library(&media);
    let names = library::with(|lib| lib.items.iter().map(|i| i.name.clone()).collect::<Vec<_>>());
    let text = format!(
        "store={}\nitems={}\nmissing={}\nunsaved={}\nnames={}\n",
        outcome.label(),
        names.len(),
        message.unwrap_or_default().replace('\n', " / "),
        unsaved.unwrap_or_default().replace('\n', " / "),
        names.join("|")
    );
    if std::fs::write(report, text).is_ok() { 0 } else { 1 }
}

fn run(mode: &Mode) -> Result<(), String> {
    app(|a| a.quiet.set(matches!(mode, Mode::SelfTest(_))));
    let main = create_main_window()?;
    app(|a| {
        a.main.set(main);
        // SAFETY: registering a message name has no preconditions.
        a.taskbar_created.set(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });
        a.activate.set(instance::activate_message());
    });
    jobs::init(main);
    // Lock and unlock reach the main window: GIFs stop while locked.
    match visibility::SessionNotes::register(main) {
        Ok(n) => SESSION_NOTES.with(|s| *s.borrow_mut() = Some(n)),
        Err(e) => selftest::note(&e),
    }
    let watching = paths::get().map(|p| watch::start(&p.media, watch::BUFFER));

    // Open faster keeps the small popup ready from the
    // start; with Use less memory it is made when it is first opened.
    if store::settings().profile == settings_state::Profile::OpenFaster {
        popup::create()?;
    }
    tray::add(main);

    let shortcut = store::settings().shortcut;
    if let Err(reason) = hotkey::start(main, shortcut) {
        tray::set_tip(main, "PocketMedia - shortcut unavailable");
        report_hotkey_failure(&shortcut.label(), &reason);
    }
    // Said only now, once the tray icon and the shortcut work: the message box waits
    // for OK, and nothing else should wait with it.
    if let Some(Err(e)) = watching {
        report_watch_failure(&e);
    }

    match mode {
        Mode::SelfTest(out) => selftest::start(out.clone(), main),
        Mode::Normal { exit_after_start: true, .. } => exit(),
        Mode::Normal { backup: true, .. } => manager::back_up_from_uninstaller(),
        Mode::Normal { autostart: false, .. } => popup::open(target::capture_foreground()),
        _ => {}
    }
    message_loop();
    selftest::message_loop_ended();
    Ok(())
}

/// This program's module handle.
pub(crate) fn module() -> windows::Win32::Foundation::HINSTANCE {
    // SAFETY: None asks for the running exe's own module, which always exists.
    unsafe { GetModuleHandleW(None) }.map(Into::into).unwrap_or_default()
}

/// The hidden owner window: receives tray, hotkey and timer messages. Never shown.
fn create_main_window() -> Result<HWND, String> {
    let instance = module();
    let class = WNDCLASSW {
        lpfnWndProc: Some(main_proc),
        hInstance: instance,
        lpszClassName: instance::main_class(),
        ..Default::default()
    };
    // SAFETY: `class` is a local whose strings are static; Windows copies it.
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err("Could not register the main window class.".into());
    }
    // SAFETY: the class was registered above; all other arguments are plain values.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            instance::main_class(),
            w!("PocketMedia"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .map_err(|e| format!("Could not create the main window: {e}"))
}

/// Watching the Media folder could not start. Said like a shortcut that
/// could not be registered: a message now, and a line in the popup and the
/// Manager for as long as it lasts (`watch::problem`).
pub fn report_watch_failure(reason: &str) {
    watch::start_failed(reason);
    if !app(|a| a.quiet.get()) {
        message_box(&watch::start_failure_message(reason), "PocketMedia - Media folder not watched", MB_ICONWARNING);
    }
}

fn report_hotkey_failure(label: &str, reason: &str) {
    let text = format!(
        "PocketMedia could not register the shortcut {label}\n\n{reason}\n\n\
         PocketMedia is still running in the tray. Right-click the tray icon and \
         choose Settings to pick another shortcut, or Quick popup to use the popup \
         without one."
    );
    if app(|a| a.quiet.get()) {
        selftest::note(&format!("hotkey registration failed: {reason}"));
        return;
    }
    message_box(&text, "PocketMedia - shortcut not available", MB_ICONWARNING);
}

fn message_loop() {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a local the call fills in.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) }.0;
        if r == 0 || r == -1 {
            break;
        }
        // SAFETY: `msg` was just filled in by GetMessageW on this thread.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

extern "system" fn main_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_HOTKEY if wparam.0 as i32 == hotkey::ID => {
            popup::toggle_from_hotkey();
            LRESULT(0)
        }
        WM_TRAY => {
            let event = (lparam.0 as u32) & 0xFFFF;
            if event != WM_MOUSEMOVE {
                selftest::note(&format!("tray event {event:#06x}"));
            }
            if event == WM_LBUTTONUP {
                // A left click on the icon opens the quick popup.
                // The taskbar has focus now, so there is no app to paste into.
                popup::open(target::capture_foreground());
            }
            if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                // Capture before the menu takes focus. From the tray this is
                // normally the taskbar, which `capture_foreground` rejects.
                let captured = target::capture_foreground();
                match tray::show_menu(hwnd) {
                    tray::MenuChoice::QuickPopup => popup::open(captured),
                    tray::MenuChoice::Manager => manager::show(),
                    tray::MenuChoice::Settings => manager::show_settings(),
                    tray::MenuChoice::Exit => request_exit(),
                    tray::MenuChoice::None => {}
                }
            }
            LRESULT(0)
        }
        WM_SELFTEST => selftest::on_ui_command(wparam, lparam),
        WM_RELEASE => {
            release_hidden();
            LRESULT(0)
        }
        jobs::WM_JOB => {
            for ev in jobs::take() {
                if watch::on_event(&ev) || backup::on_event(&ev) {
                    continue;
                }
                if !paste::on_prepared(&ev) && !popup::on_job(&ev) {
                    manager::on_job(ev);
                }
            }
            LRESULT(0)
        }
        jobs::WM_PREVIEW => {
            thumbs::on_wake();
            preview::on_wake();
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_PASTE => {
            paste::tick();
            LRESULT(0)
        }
        // Signing out or shutting down while a backup or an import runs: Windows
        // asks first, showing backup::SHUTDOWN_REASON or add_panel::SHUTDOWN_REASON
        // (set when it started).
        WM_QUERYENDSESSION => LRESULT(may_end_session()),
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                // The session ends anyway: stop the backup and let it clean up,
                // and stop an import, recording and saving what it copied.
                backup::shutdown();
                stop_imports(std::time::Duration::from_secs(3));
            }
            LRESULT(0)
        }
        WM_WTSSESSION_CHANGE if matches!(wparam.0 as u32, WTS_SESSION_LOCK | WTS_SESSION_UNLOCK) => {
            let locked = wparam.0 as u32 == WTS_SESSION_LOCK;
            selftest::note(if locked { "session locked: GIFs paused" } else { "session unlocked: GIFs resume" });
            preview::set_locked(locked);
            if !locked {
                winutil::invalidate(popup::hwnd());
                winutil::invalidate(manager::hwnd());
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            SESSION_NOTES.with(|s| s.borrow_mut().take());
            hotkey::stop();
            // SAFETY: our own window and timer id; a timer that is not running only fails.
            let _ = unsafe { KillTimer(Some(hwnd), TIMER_PASTE) };
            watch::stop();
            backup::shutdown();
            preview::shutdown();
            thumbs::shutdown();
            tray::remove();
            manager::destroy();
            popup::destroy();
            // SAFETY: no preconditions; ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        m if m != 0 && m == app(|a| a.activate.get()) => {
            // Another launch of PocketMedia: do what it asked, here.
            if wparam.0 == instance::BACK_UP {
                manager::back_up_from_uninstaller();
            } else {
                popup::open(target::capture_foreground());
            }
            LRESULT(0)
        }
        m if m != 0 && m == app(|a| a.taskbar_created.get()) => {
            // Explorer restarted: the tray icon is gone and must be re-added.
            tray::add(hwnd);
            LRESULT(0)
        }
        // SAFETY: passes the window procedure's own arguments on unchanged.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Windows that are hidden let go of what they hold, if nothing in them
/// is in progress. With Use less memory and both windows hidden, the thumbnail
/// workers and the GIF and video threads end too (they start again when needed).
fn release_hidden() {
    popup::release_if_idle();
    manager::release_if_idle();
    let less = store::settings().profile == settings_state::Profile::LessMemory;
    if less && !popup::is_visible() && !manager::is_visible() {
        thumbs::retire_workers();
        preview::stop_threads();
    }
}

/// Asks for `release_hidden` once the current message is handled (from any window code).
pub fn release_hidden_later() {
    let main = app(|a| a.main.get());
    // SAFETY: posting to our own window; a destroyed window only makes it fail.
    let _ = unsafe { PostMessageW(Some(main), WM_RELEASE, WPARAM(0), LPARAM(0)) };
}

/// WM_QUERYENDSESSION, for the main window and the Manager: "not yet" (0) while a
/// backup or an import runs, so Windows shows the reason and lets the user choose.
pub fn may_end_session() -> isize {
    isize::from(backup::may_end_session() == 1 && !add_panel::copies_running())
}

/// The question Exit asks now, if any (the self-test reads it without exiting).
pub fn exit_question() -> Option<(&'static str, &'static str)> {
    if backup::running() {
        Some((STOP_BACKUP_TEXT, STOP_BACKUP_TITLE))
    } else if add_panel::copies_running() {
        Some((STOP_IMPORT_TEXT, STOP_IMPORT_TITLE))
    } else {
        None
    }
}

/// The tray menu's Exit. While a backup or an import runs
/// it asks first; No keeps PocketMedia and the work running. Yes stops an import
/// cleanly before exiting: the copy in progress finishes or is removed, and what
/// was copied is recorded and saved.
pub fn request_exit() {
    if backup::running() && !ask_yes_no(STOP_BACKUP_TEXT, STOP_BACKUP_TITLE) {
        return;
    }
    if add_panel::copies_running() && !ask_yes_no(STOP_IMPORT_TEXT, STOP_IMPORT_TITLE) {
        return;
    }
    stop_imports(std::time::Duration::from_secs(10));
    exit();
}

/// Stops the imports that are copying, in the popup and the Manager, and waits
/// (at most `limit` each) until they have recorded and saved what they copied.
fn stop_imports(limit: std::time::Duration) {
    for line in [popup::stop_import(limit), manager::stop_import(limit)].into_iter().flatten() {
        selftest::note(&format!("import stopped for Exit: {line}"));
    }
}

pub const STOP_IMPORT_TITLE: &str = "PocketMedia - import running";
pub const STOP_IMPORT_TEXT: &str = "An import is running. Stop it?\n\n\
    Yes stops the import, keeps the files already copied and exits PocketMedia.\n\
    No lets the import finish; you can exit PocketMedia afterwards.";

pub const STOP_BACKUP_TITLE: &str = "PocketMedia - backup running";
pub const STOP_BACKUP_TEXT: &str = "A backup is running. Stop it?\n\n\
    Yes stops the backup, removes the unfinished copy and exits PocketMedia.\n\
    No lets the backup finish; you can exit PocketMedia afterwards.";

/// A Yes/No question with No as the default button. True for Yes. Topmost, so
/// the quick popup (itself topmost, with an import in its Add media) cannot hide it.
fn ask_yes_no(text: &str, caption: &str) -> bool {
    let text = transfer::to_wide(text);
    let caption = transfer::to_wide(caption);
    // SAFETY: both strings are null-terminated UTF-16 that outlive the call.
    let answer = unsafe {
        MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(caption.as_ptr()), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND | MB_TOPMOST)
    };
    answer == IDYES
}

pub fn exit() {
    let main = app(|a| a.main.get());
    // SAFETY: our own main window, on its own thread; a destroyed window only fails.
    let _ = unsafe { DestroyWindow(main) };
}

pub fn message_box(text: &str, caption: &str, icon: MESSAGEBOX_STYLE) {
    let text = transfer::to_wide(text);
    let caption = transfer::to_wide(caption);
    // SAFETY: both strings are null-terminated UTF-16 that outlive the call.
    unsafe { MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(caption.as_ptr()), icon | MB_OK | MB_SETFOREGROUND) };
}

fn fatal(text: &str) {
    message_box(text, "PocketMedia - cannot start", MB_ICONERROR);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start-up message names what the comparison did, the reason, and what to do.
    #[test]
    fn unsaved_comparison_says_what_and_what_to_do() {
        let why = "the library file could not be saved: Access is denied.";
        let added = unsaved_after_compare(why, 2, 0);
        assert!(added.starts_with("PocketMedia added 2 file(s) it found in the Media folder to the library, but the library file"));
        assert!(unsaved_after_compare(why, 0, 3).contains("removed 3 missing file(s) from the library"));
        assert!(unsaved_after_compare(why, 1, 1).contains("added 1 file(s) it found in the Media folder and removed 1 missing file(s)"));
        for text in [added, unsaved_after_compare(why, 0, 1)] {
            assert!(text.contains("Access is denied") && text.contains("Nothing is lost") && text.contains("Data folder"), "{text}");
        }
    }

    /// The question reads like the backup's, and both start with the question itself.
    #[test]
    fn exit_questions_start_with_the_question() {
        assert!(STOP_IMPORT_TEXT.starts_with("An import is running. Stop it?\n\nYes stops the import"));
        assert!(STOP_BACKUP_TEXT.starts_with("A backup is running. Stop it?\n\nYes stops the backup"));
        assert!(add_panel::SHUTDOWN_REASON.starts_with("An import is running. Stop it?"));
    }

    /// The popup's footer holds two short lines; these notices fit in them
    /// (about 36 characters a line at 12 px in its 225 DIP).
    #[test]
    fn popup_notices_are_short() {
        for text in [watch::POPUP_NOTICE, paste::PASTE_UNSAVED, paste::DRAG_UNSAVED] {
            assert!(text.chars().count() <= 70, "{text}");
        }
    }
}
