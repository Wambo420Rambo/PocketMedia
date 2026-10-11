//! "Paste test PNG": copy, hand focus back to the target, verify, then Ctrl+V.
//!
//! Ctrl+V is only sent once Windows reports the target (or a window it owns) as
//! the foreground window and no modifier keys are held. If that does not happen
//! within about a second, nothing is sent: input must never land in the wrong app.

use std::cell::Cell;

use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, KillTimer, SW_RESTORE, SetForegroundWindow, SetTimer, ShowWindow,
};

use crate::target::{self, Elevation};
use crate::{SHORTCUT_LABEL, StatusKind, TIMER_PASTE, app, assets, popup, set_status, transfer, tray};

const TICK_MS: u32 = 25;
const MAX_TICKS: u32 = 40; // ~1 second

thread_local! {
    static TICKS_LEFT: Cell<u32> = const { Cell::new(0) };
}

pub fn cancel() {
    TICKS_LEFT.with(|t| t.set(0));
    let main = app(|a| a.main.get());
    unsafe {
        let _ = KillTimer(Some(main), TIMER_PASTE);
    }
}

pub fn begin() {
    cancel();
    let main = app(|a| a.main.get());
    let a = assets();

    if let Err(e) = transfer::put_on_clipboard(main, &a.png, &a.image, &a.file) {
        set_status(format!("Copy failed: {e}"), StatusKind::Bad);
        return;
    }

    let target = app(|a| a.target.borrow().clone());
    let t = match target {
        Ok(t) => t,
        Err(reason) => {
            set_status(
                format!(
                    "Copied to the clipboard, but there is no app to paste into ({}). \
                     Press Esc, click into the app you want, then press {SHORTCUT_LABEL}.",
                    reason.describe()
                ),
                StatusKind::Bad,
            );
            return;
        }
    };
    if !t.still_exists() {
        set_status(
            "Copied to the clipboard, but the target window has closed. Nothing was pasted.",
            StatusKind::Bad,
        );
        return;
    }
    if target::process_elevation(Some(t.pid)) == Elevation::Elevated
        && target::process_elevation(None) != Elevation::Elevated
    {
        set_status(
            format!(
                "Copied to the clipboard, but {} runs as administrator. Windows blocks \
                 simulated paste from a normal app into it. Press Ctrl+V there yourself.",
                t.exe
            ),
            StatusKind::Bad,
        );
        return;
    }

    unsafe {
        if IsIconic(t.hwnd).as_bool() {
            let _ = ShowWindow(t.hwnd, SW_RESTORE);
        }
        // Hand focus back while we still own the foreground, then hide.
        let _ = SetForegroundWindow(t.hwnd);
    }
    popup::close(false);

    TICKS_LEFT.with(|c| c.set(MAX_TICKS));
    unsafe {
        SetTimer(Some(main), TIMER_PASTE, TICK_MS, None);
    }
}

pub fn tick() {
    let left = TICKS_LEFT.with(|c| c.get());
    if left == 0 {
        cancel();
        return;
    }
    let Ok(t) = app(|a| a.target.borrow().clone()) else {
        cancel();
        return;
    };
    let main = app(|a| a.main.get());

    if t.owns_foreground() && !transfer::modifiers_held() {
        cancel();
        match transfer::send_ctrl_v() {
            Ok(()) => {
                set_status(format!("Sent Ctrl+V to {}.", t.label()), StatusKind::Good);
                tray::set_tip(main, &format!("PocketMedia - last paste: {}", t.exe));
            }
            Err(e) => {
                set_status(format!("Paste not sent: {e}"), StatusKind::Bad);
                tray::notify(main, "PocketMedia - paste not sent", &e, true);
            }
        }
        return;
    }

    TICKS_LEFT.with(|c| c.set(left - 1));
    if left - 1 == 0 {
        cancel();
        let why = if transfer::modifiers_held() {
            "a Win/Alt/Shift/Ctrl key was still held down"
        } else {
            "Windows did not give focus back to it"
        };
        let text = format!(
            "Nothing was pasted into {}: {why}. The test PNG is on the clipboard - \
             press Ctrl+V yourself, or reopen the popup and drag it.",
            t.exe
        );
        set_status(text.clone(), StatusKind::Bad);
        tray::notify(main, "PocketMedia - paste not sent", &text, true);
    }
}
