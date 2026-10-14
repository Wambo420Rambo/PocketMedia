//! Self-test steps for the shortcut chooser:
//! Change opens it; real key presses are recorded; a key without modifiers and
//! a shortcut another program holds are refused with the reason; Use and Save
//! register the new shortcut and let the old one go; a shortcut Windows refuses
//! at Save leaves the old one working; Reset and Escape.
//!
//! "Another program" is this test thread: a hotkey registered here is held for
//! the whole system, exactly as another app's would be. Its key presses would go
//! to this thread, so the chooser is told about it through `CMD_SC_TRY`, which
//! runs the same check a key press does.

use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey, VIRTUAL_KEY, VK_CONTROL,
    VK_ESCAPE, VK_F7, VK_F8, VK_MENU, VK_P, VK_RETURN, VK_SHIFT,
};

use super::settings_test::Set;
use super::{CMD_FOCUS, CMD_POPUP, Report, is_visible, press, send, wait_until};
use crate::hotkey::Shortcut;
use crate::ui::settings_view::SHit;

/// lparam = an encoded shortcut: the chooser checks it as if it had been pressed.
pub(super) const CMD_SC_TRY: usize = 50;
/// lparam = an encoded shortcut: 1 if Windows would give it to PocketMedia now.
pub(super) const CMD_SC_FREE: usize = 51;

const F7: Shortcut = Shortcut { ctrl: true, alt: true, shift: true, win: false, key: 0x76 };
const F8: Shortcut = Shortcut { ctrl: true, alt: true, shift: true, win: false, key: 0x77 };
const F7_LABEL: &str = "Ctrl + Alt + Shift + F7";
const HOLD_ID: i32 = 0x7E57;

pub(super) fn encode(s: Shortcut) -> isize {
    let bit = |on: bool, n: u32| if on { 1isize << n } else { 0 };
    s.key as isize | bit(s.ctrl, 16) | bit(s.alt, 17) | bit(s.shift, 18) | bit(s.win, 19)
}

fn decode(v: isize) -> Shortcut {
    let on = |n: u32| v & (1 << n) != 0;
    Shortcut { ctrl: on(16), alt: on(17), shift: on(18), win: on(19), key: (v & 0xFFFF) as u16 }
}

/// Runs on the UI thread.
pub(super) fn on_ui_command(cmd: usize, lparam: isize) -> isize {
    match cmd {
        CMD_SC_TRY => {
            crate::manager::test_dialog_press(decode(lparam));
            0
        }
        CMD_SC_FREE => crate::hotkey::is_free(decode(lparam)).is_ok() as isize,
        _ => 0,
    }
}

fn chord(key: VIRTUAL_KEY) {
    press(&[VK_CONTROL, VK_MENU, VK_SHIFT, key]);
    settle();
}

fn settle() {
    std::thread::sleep(Duration::from_millis(250));
}

/// Holds `F8` for this thread, as another program would. False if it could not.
fn hold_f8() -> bool {
    let m = HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0 | MOD_SHIFT.0 | MOD_NOREPEAT.0);
    // SAFETY: a thread hotkey (no window); released by `release_f8` on this thread.
    unsafe { RegisterHotKey(None, HOLD_ID, m, F8.key as u32) }.is_ok()
}

fn release_f8() {
    // SAFETY: as above; unregistering an id that is not registered only fails.
    let _ = unsafe { UnregisterHotKey(None, HOLD_ID) };
}

fn free(s: &Set, sc: Shortcut) -> bool {
    send(s.t.main, CMD_SC_FREE, encode(sc)) == 1
}

pub(super) fn run(r: &mut Report, s: &Set) {
    let default = Shortcut::default();
    s.scroll_to(0);
    s.click(r, SHit::Change);
    s.has(r, "Change opens the shortcut chooser, waiting for keys", &["dialog=:waiting", "focus=DialogCancel"]);
    r.check("while choosing, PocketMedia lets its shortcut go (so pressing it is recorded)", free(s, default), "");

    if !super::guard(r, &s.t.allowed) {
        return;
    }
    press(&[VK_P]);
    settle();
    s.has(r, "a key without Win, Ctrl or Alt is refused, with the reason", &["dialog=P:refused(Use Win, or two of Ctrl, Alt and Shift"]);
    s.shot(r, "shortcut-chooser-refused");

    chord(VK_F7);
    s.has(r, "a free shortcut pressed for real is recorded as available", &[&format!("dialog={F7_LABEL}:free"), "focus=DialogUse"]);
    s.park();
    s.shot(r, "shortcut-chooser-available");

    let held = hold_f8();
    r.check("the test holds Ctrl + Alt + Shift + F8 like another app", held, "");
    send(s.t.main, CMD_SC_TRY, encode(F8));
    s.has(r, "a shortcut another program holds is refused", &["dialog=Ctrl + Alt + Shift + F8:refused(Another app already uses it"]);
    s.shot(r, "shortcut-chooser-taken");
    release_f8();

    // Use, then Save: the new shortcut is registered and the old one let go.
    chord(VK_F7);
    press(&[VK_RETURN]);
    settle();
    s.has(
        r,
        "Use shortcut stages it until Save",
        &["dialog=none", &format!("shortcut=\"{F7_LABEL}\""), "saved_shortcut=\"Win + Alt + .\"", "dirty=true", "focus=Change"],
    );
    s.click(r, SHit::Save);
    s.has(
        r,
        "Save registers the new shortcut",
        &[&format!("saved_shortcut=\"{F7_LABEL}\""), &format!("registered=\"{F7_LABEL}\""), "hotkey_ok=true", "dirty=false"],
    );
    r.check("the old shortcut is let go", free(s, default), "");
    r.check("the new one is PocketMedia's now", !free(s, F7), "");
    let file = super::settings_test::read_json("pocketmedia.json");
    r.check(
        "the shortcut is saved in the library file",
        file.as_ref().is_some_and(|v| {
            let sc = &v["settings"]["shortcut"];
            sc["key"] == 0x76 && sc["ctrl"] == true && sc["alt"] == true && sc["shift"] == true && sc["win"] == false
        }),
        &format!("{file:?}"),
    );
    s.park();
    s.shot(r, "shortcut-settings-new");

    // The new shortcut opens the quick popup.
    if super::guard(r, &s.t.allowed) {
        chord(VK_F7);
        let popup = send(s.t.main, CMD_POPUP, 0);
        let opened = wait_until(1500, || is_visible(popup));
        r.check("pressing the new shortcut opens the quick popup", opened, "");
        if opened {
            press(&[VK_ESCAPE]);
            wait_until(1500, || !is_visible(popup));
        }
        send(s.t.main, CMD_FOCUS, s.hwnd);
        std::thread::sleep(Duration::from_millis(300));
    }

    // A shortcut Windows refuses at Save: the old one stays and keeps working.
    if !super::guard(r, &s.t.allowed) {
        return;
    }
    s.click(r, SHit::Change);
    chord(VK_F8);
    s.has(r, "Ctrl + Alt + Shift + F8 is free before the test takes it", &["dialog=Ctrl + Alt + Shift + F8:free"]);
    let held = hold_f8();
    press(&[VK_RETURN]);
    settle();
    s.click(r, SHit::Save);
    s.has(
        r,
        "a shortcut Windows refuses at Save leaves the old one in place, and says so",
        &["could not be used", &format!("saved_shortcut=\"{F7_LABEL}\""), &format!("registered=\"{F7_LABEL}\""), "hotkey_ok=true"],
    );
    r.check("(the test held Ctrl + Alt + Shift + F8)", held, "");
    release_f8();

    // Reset offers the default; Use and Save put it back.
    s.click(r, SHit::Change);
    s.click(r, SHit::DialogReset);
    s.has(r, "Reset offers Win + Alt + . again", &["dialog=Win + Alt + .:free"]);
    s.click(r, SHit::DialogUse);
    s.click(r, SHit::Save);
    s.has(
        r,
        "the default shortcut is back",
        &["saved_shortcut=\"Win + Alt + .\"", "registered=\"Win + Alt + .\"", "hotkey_ok=true", "dirty=false"],
    );
    r.check("Ctrl + Alt + Shift + F7 is let go again", free(s, F7), "");

    // Escape closes the chooser without a change; the shortcut works again.
    s.click(r, SHit::Change);
    if super::guard(r, &s.t.allowed) {
        chord(VK_F7);
        press(&[VK_ESCAPE]);
        settle();
    }
    s.has(r, "Escape closes the chooser and changes nothing", &["dialog=none", "dirty=false", "focus=Change"]);
    r.check("after the chooser closes, PocketMedia holds its shortcut again", !free(s, default), "");
}
