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
mod dialogs;
mod digest;
mod fileops;
mod gif;
mod hotkey;
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
mod ui;
mod video;
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
    if let Some(text) = load_library(&media)
        && !quiet
    {
        message_box(&text, "PocketMedia - library", MB_ICONINFORMATION);
    }
    unsafe {
        // Ignore failure: it only fails if awareness was already set (e.g. by a manifest).
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if let Err(e) = OleInitialize(None) {
            fatal(&format!("Could not initialise OLE (needed for drag and drop): {e}"));
            return;
        }
    }

    if let Err(e) = run(&mode) {
        fatal(&e);
    }

    unsafe { OleUninitialize() };
    std::process::exit(selftest::exit_code());
}

/// Loads the library from the store and makes it agree with the Media folders
/// (an interrupted import leaves no orphan or half-written file):
/// leftover temporary copies are deleted, finished copies without a record are
/// added, and records whose file is gone are removed. Returns a message when
/// files were missing.
fn load_library(media: &std::path::Path) -> Option<String> {
    let data = store::with(|s| s.data.clone()).unwrap_or_default();
    let mut lib = library::Library::from_store(&data);
    import::cleanup_partials(media);
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
    if changed {
        let _ = library::persist();
    }
    (!missing_names.is_empty()).then(|| {
        format!(
            "{} file(s) were no longer in the Media folder and were removed from the library:\n\n{}",
            missing_names.len(),
            missing_names.join("\n")
        )
    })
}

/// `--check-library`: what a start finds in the library beside this program.
fn check_library(paths: paths::Paths, report: &std::path::Path) -> i32 {
    let outcome = store::init(&paths.data);
    let media = paths.media.clone();
    let message = load_library(&media);
    let names = library::with(|lib| lib.items.iter().map(|i| i.name.clone()).collect::<Vec<_>>());
    let text = format!(
        "store={}\nitems={}\nmissing={}\nnames={}\n",
        outcome.label(),
        names.len(),
        message.unwrap_or_default().replace('\n', " / "),
        names.join("|")
    );
    if std::fs::write(report, text).is_ok() { 0 } else { 1 }
}

fn run(mode: &Mode) -> Result<(), String> {
    app(|a| a.quiet.set(matches!(mode, Mode::SelfTest(_))));
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(main_proc),
            hInstance: instance.into(),
            lpszClassName: instance::main_class(),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("Could not register the main window class.".into());
        }
        // Hidden owner window: receives tray, hotkey and timer messages. Never shown.
        let main = CreateWindowExW(
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
            Some(instance.into()),
            None,
        )
        .map_err(|e| format!("Could not create the main window: {e}"))?;
        app(|a| {
            a.main.set(main);
            a.taskbar_created.set(RegisterWindowMessageW(w!("TaskbarCreated")));
            a.activate.set(instance::activate_message());
        });
        jobs::init(main);
        if let Some(p) = paths::get()
            && let Err(e) = watch::start(&p.media, watch::BUFFER)
        {
            selftest::note(&e);
        }

        popup::create(instance.into())?;
        tray::add(main);

        let shortcut = store::settings().shortcut;
        if let Err(reason) = hotkey::start(main, shortcut) {
            tray::set_tip(main, "PocketMedia - shortcut unavailable");
            report_hotkey_failure(&shortcut.label(), &reason);
        }

        match mode {
            Mode::SelfTest(out) => selftest::start(out.clone(), main),
            Mode::Normal { exit_after_start: true, .. } => exit(),
            Mode::Normal { backup: true, .. } => manager::back_up_from_uninstaller(),
            Mode::Normal { autostart: false, .. } => popup::open(target::capture_foreground()),
            _ => {}
        }
        message_loop();
    }
    Ok(())
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

unsafe fn message_loop() {
    unsafe {
        let mut msg = MSG::default();
        loop {
            let r = GetMessageW(&mut msg, None, 0, 0).0;
            if r == 0 || r == -1 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

extern "system" fn main_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
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
            jobs::WM_JOB => {
                for ev in jobs::take() {
                    if watch::on_event(&ev) || backup::on_event(&ev) {
                        continue;
                    }
                    if !paste::on_prepared(&ev) && !popup::on_job(&ev) {
                        manager::on_job(&ev);
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
            // Signing out or shutting down while a backup runs: Windows asks first,
            // showing backup::SHUTDOWN_REASON (set when the backup started).
            WM_QUERYENDSESSION => LRESULT(backup::may_end_session()),
            WM_ENDSESSION => {
                if wparam.0 != 0 {
                    // The session ends anyway: stop the backup and let it clean up.
                    backup::shutdown();
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                hotkey::stop();
                let _ = KillTimer(Some(hwnd), TIMER_PASTE);
                watch::stop();
                backup::shutdown();
                preview::shutdown();
                thumbs::shutdown();
                tray::remove(hwnd);
                manager::destroy();
                popup::destroy();
                PostQuitMessage(0);
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
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// The tray menu's Exit. While a backup runs it asks first; No
/// keeps PocketMedia and the backup running.
pub fn request_exit() {
    if backup::running() && !ask_yes_no(STOP_BACKUP_TEXT, STOP_BACKUP_TITLE) {
        return;
    }
    exit();
}

pub const STOP_BACKUP_TITLE: &str = "PocketMedia - backup running";
pub const STOP_BACKUP_TEXT: &str = "A backup is running. Stop it?\n\n\
    Yes stops the backup, removes the unfinished copy and exits PocketMedia.\n\
    No lets the backup finish; you can exit PocketMedia afterwards.";

/// A Yes/No question with No as the default button. True for Yes.
fn ask_yes_no(text: &str, caption: &str) -> bool {
    let text = transfer::to_wide(text);
    let caption = transfer::to_wide(caption);
    // SAFETY: both strings are null-terminated UTF-16 that outlive the call.
    let answer = unsafe {
        MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(caption.as_ptr()), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND)
    };
    answer == IDYES
}

pub fn exit() {
    let main = app(|a| a.main.get());
    unsafe {
        let _ = DestroyWindow(main);
    }
}

pub fn message_box(text: &str, caption: &str, icon: MESSAGEBOX_STYLE) {
    let text = transfer::to_wide(text);
    let caption = transfer::to_wide(caption);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            icon | MB_OK | MB_SETFOREGROUND,
        );
    }
}

fn fatal(text: &str) {
    message_box(text, "PocketMedia - cannot start", MB_ICONERROR);
}
