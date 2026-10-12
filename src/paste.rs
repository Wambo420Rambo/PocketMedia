//! "Paste test PNG": copy, hand focus back to the target, verify, then Ctrl+V.
//! When there is no app to paste into, the image stays on the clipboard and the
//! popup says "Copied to clipboard" (no Windows notification).
//!
//! Ctrl+V is only sent once Windows reports the target (or a window it owns) as
//! the foreground window and no modifier keys are held. If that does not happen
//! within about a second, nothing is sent: input must never land in the wrong app.

use std::cell::Cell;

use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, KillTimer, SW_RESTORE, SetForegroundWindow, SetTimer, ShowWindow,
};

use crate::target::{self, Elevation};
use crate::{StatusKind, TIMER_PASTE, app, assets, popup, set_status, transfer, tray};

const TICK_MS: u32 = 25;
const COPIED: &str = "Copied to clipboard";
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
        popup::show_status(&format!("Could not copy: {e}"));
        return;
    }

    let target = app(|a| a.target.borrow().clone());
    let t = match target {
        Ok(t) => t,
        Err(reason) => {
            set_status(format!("Copied to clipboard; no app to paste into ({}).", reason.describe()), StatusKind::Neutral);
            popup::show_status(COPIED);
            return;
        }
    };
    if !t.still_exists() {
        set_status("Copied to clipboard; the target window has closed.", StatusKind::Neutral);
        popup::show_status(COPIED);
        return;
    }
    if target::process_elevation(Some(t.pid)) == Elevation::Elevated
        && target::process_elevation(None) != Elevation::Elevated
    {
        set_status(format!("Copied to clipboard; {} runs as administrator.", t.exe), StatusKind::Neutral);
        // Windows blocks simulated paste from a normal app into an elevated one.
        popup::show_status(&format!("{COPIED} - press Ctrl+V in {} yourself (it runs as administrator)", t.exe));
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
                tray::set_tip(main, "PocketMedia - copied to clipboard");
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
        set_status(text, StatusKind::Bad);
        tray::set_tip(main, "PocketMedia - copied to clipboard");
    }
}
