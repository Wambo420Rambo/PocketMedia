//! PocketMedia.
//!
//! Phase 0 (risk test): tray icon + menu, the global shortcut Win+Alt+., focus
//! hand-back, paste of a test PNG and dragging it out.
//! The quick popup drawn with Direct2D/DirectWrite over fake items.
//! No real library, import, settings or persistence yet.

#![windows_subsystem = "windows"]

mod paste;
mod popup;
mod popup_state;
mod sample;
mod search;
mod selftest;
mod target;
mod test_png;
mod test_target;
mod text_field;
mod transfer;
mod tray;
mod ui;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::OnceLock;

use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_NOREPEAT, MOD_WIN, RegisterHotKey, UnregisterHotKey, VK_OEM_PERIOD,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use crate::target::{NoTarget, Target};
use crate::test_png::TestImage;

pub const WM_TRAY: u32 = WM_APP + 1;
pub const WM_SELFTEST: u32 = WM_APP + 2;
const HOTKEY_ID: i32 = 1;
pub const TIMER_PASTE: usize = 1;
pub const SHORTCUT_LABEL: &str = "Win + Alt + .";

/// The test PNG, prepared once at startup.
pub struct Assets {
    pub image: TestImage,
    pub bgra_bottom_up: Vec<u8>,
    pub png: Vec<u8>,
    pub file: PathBuf,
}

static ASSETS: OnceLock<Assets> = OnceLock::new();

pub fn assets() -> &'static Assets {
    ASSETS.get().expect("assets are initialised before the message loop")
}

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
    pub hotkey_ok: Cell<bool>,
    /// Self-test mode: report problems in the log instead of message boxes.
    pub quiet: Cell<bool>,
    pub taskbar_created: Cell<u32>,
    pub target: RefCell<Result<Target, NoTarget>>,
    pub status: RefCell<(String, StatusKind)>,
}

thread_local! {
    static APP: App = App {
        main: Cell::new(HWND::default()),
        hotkey_ok: Cell::new(false),
        quiet: Cell::new(false),
        taskbar_created: Cell::new(0),
        target: RefCell::new(Err(NoTarget::NothingFocused)),
        status: RefCell::new((String::new(), StatusKind::Neutral)),
    };
}

pub fn app<R>(f: impl FnOnce(&App) -> R) -> R {
    APP.with(f)
}

/// Records the result of the last paste/drag. Problems are shown as a tray
/// notification, which never takes focus from the app being pasted into.
pub fn set_status(text: impl Into<String>, kind: StatusKind) {
    let text = text.into();
    app(|a| *a.status.borrow_mut() = (text.clone(), kind));
    let (main, quiet) = app(|a| (a.main.get(), a.quiet.get()));
    if kind == StatusKind::Bad && !quiet {
        tray::notify(main, "PocketMedia", &text, true);
    }
}

/// Command-line modes. Normal launch has no arguments.
enum Mode {
    Normal { exit_after_start: bool },
    SelfTest(PathBuf),
    TestTarget(PathBuf),
}

fn parse_mode() -> Mode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--self-test") => Mode::SelfTest(PathBuf::from(args.get(1).cloned().unwrap_or_else(|| "test-results".into()))),
        Some("--test-target") => Mode::TestTarget(PathBuf::from(args.get(1).cloned().unwrap_or_else(|| "test-results".into()))),
        Some("--exit-after-start") => Mode::Normal { exit_after_start: true },
        _ => Mode::Normal { exit_after_start: false },
    }
}

fn main() {
    let mode = parse_mode();
    if let Mode::TestTarget(out) = &mode {
        // SAFETY: called once at startup, before any window exists.
        let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        std::process::exit(test_target::run(out));
    }
    unsafe {
        // Ignore failure: it only fails if awareness was already set (e.g. by a manifest).
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if let Err(e) = OleInitialize(None) {
            fatal(&format!("Could not initialise OLE (needed for drag and drop): {e}"));
            return;
        }
    }

    if let Err(e) = prepare_assets() {
        fatal(&format!("Could not create the test PNG file: {e}"));
        unsafe { OleUninitialize() };
        return;
    }

    if let Err(e) = run(&mode) {
        fatal(&e);
    }

    let _ = std::fs::remove_file(&assets().file);
    if let Some(dir) = assets().file.parent() {
        let _ = std::fs::remove_dir(dir);
    }
    unsafe { OleUninitialize() };
    std::process::exit(selftest::exit_code());
}

fn prepare_assets() -> std::io::Result<()> {
    let image = TestImage::generate();
    let png = image.encode_png();
    let file = test_png::write_test_file(&png)?;
    let bgra_bottom_up = image.bgra_bottom_up();
    let _ = ASSETS.set(Assets { image, bgra_bottom_up, png, file });
    Ok(())
}

fn run(mode: &Mode) -> Result<(), String> {
    app(|a| a.quiet.set(matches!(mode, Mode::SelfTest(_))));
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = WNDCLASSW {
            lpfnWndProc: Some(main_proc),
            hInstance: instance.into(),
            lpszClassName: w!("PocketMediaPhase0Main"),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("Could not register the main window class.".into());
        }
        // Hidden owner window: receives tray, hotkey and timer messages. Never shown.
        let main = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("PocketMediaPhase0Main"),
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
        });

        popup::create(instance.into())?;
        tray::add(main);

        let hotkey = RegisterHotKey(
            Some(main),
            HOTKEY_ID,
            HOT_KEY_MODIFIERS(MOD_WIN.0 | MOD_ALT.0 | MOD_NOREPEAT.0),
            VK_OEM_PERIOD.0 as u32,
        );
        match hotkey {
            Ok(()) => app(|a| a.hotkey_ok.set(true)),
            Err(e) => {
                tray::set_tip(main, "PocketMedia - shortcut unavailable");
                report_hotkey_failure(e);
            }
        }

        match mode {
            Mode::SelfTest(out) => selftest::start(out.clone(), main),
            Mode::Normal { exit_after_start: true } => exit(),
            _ => {}
        }
        message_loop();
    }
    Ok(())
}

fn report_hotkey_failure(e: windows::core::Error) {
    let reason = if e.code() == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult() {
        "Another program (or another copy of PocketMedia) is already using it, \
         or Windows has reserved it."
            .to_string()
    } else {
        format!("Windows refused the registration: {} (0x{:08X}).", e.message(), e.code().0)
    };
    let text = format!(
        "PocketMedia could not register the shortcut {SHORTCUT_LABEL}\n\n{reason}\n\n\
         PocketMedia is still running in the tray. Right-click the tray icon and \
         choose Open to use the popup without the shortcut."
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
            WM_HOTKEY if wparam.0 as i32 == HOTKEY_ID => {
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
                        tray::MenuChoice::Open => popup::open(captured),
                        tray::MenuChoice::Exit => exit(),
                        tray::MenuChoice::None => {}
                    }
                }
                LRESULT(0)
            }
            WM_SELFTEST => selftest::on_ui_command(wparam, lparam),
            WM_TIMER if wparam.0 == TIMER_PASTE => {
                paste::tick();
                LRESULT(0)
            }
            WM_DESTROY => {
                if app(|a| a.hotkey_ok.replace(false)) {
                    let _ = UnregisterHotKey(Some(hwnd), HOTKEY_ID);
                }
                let _ = KillTimer(Some(hwnd), TIMER_PASTE);
                tray::remove(hwnd);
                popup::destroy();
                PostQuitMessage(0);
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
