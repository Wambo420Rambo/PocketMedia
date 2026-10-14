//! The global shortcut that opens the quick popup: what it is (saved with the
//! settings), its name, the rules for a usable one, and registering it with
//! Windows. Win + Alt + . by default; check that registration really
//! succeeded, say so plainly if not, and let the user choose another.

use std::cell::Cell;

use serde::{Deserialize, Serialize};

/// A key with its modifiers. `key` is a Windows virtual-key code.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Shortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub key: u16,
}

const VK_OEM_PERIOD: u16 = 0xBE;

impl Default for Shortcut {
    /// Win + Alt + . (Win + . is Windows' own emoji panel and stays intact).
    fn default() -> Self {
        Self { ctrl: false, alt: true, shift: false, win: true, key: VK_OEM_PERIOD }
    }
}

/// The name of a key PocketMedia accepts, or None. Punctuation is named as on a
/// US keyboard.
pub fn key_name(vk: u16) -> Option<String> {
    let s = match vk {
        0x30..=0x39 | 0x41..=0x5A => return Some(char::from(vk as u8).to_string()),
        0x70..=0x87 => return Some(format!("F{}", vk - 0x6F)),
        0x60..=0x69 => return Some(format!("Num {}", vk - 0x60)),
        0x20 => "Space",
        0x21 => "PgUp",
        0x22 => "PgDn",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0x2D => "Ins",
        0x2E => "Del",
        0xBA => ";",
        0xBB => "=",
        0xBC => ",",
        0xBD => "-",
        0xBE => ".",
        0xBF => "/",
        0xC0 => "`",
        0xDB => "[",
        0xDC => "\\",
        0xDD => "]",
        0xDE => "'",
        _ => return None,
    };
    Some(s.to_string())
}

/// Shift, Ctrl, Alt and the Windows keys, left, right or either.
pub fn is_modifier(vk: u16) -> bool {
    matches!(vk, 0x10..=0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

impl Shortcut {
    /// The keys in the usual order: Ctrl, Win, Alt, Shift, then the key.
    pub fn keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = [(self.ctrl, "Ctrl"), (self.win, "Win"), (self.alt, "Alt"), (self.shift, "Shift")]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, k)| k.to_string())
            .collect();
        keys.push(key_name(self.key).unwrap_or_else(|| format!("Key {:#04X}", self.key)));
        keys
    }

    /// "Win + Alt + ."
    pub fn label(&self) -> String {
        self.keys().join(" + ")
    }

    /// Why this shortcut cannot be used, before asking Windows. None: ask Windows.
    pub fn problem(&self) -> Option<&'static str> {
        if key_name(self.key).is_none() {
            return Some("PocketMedia can't use that key. Try a letter, a number, F1–F24 or a punctuation key.");
        }
        let others = [self.ctrl, self.alt, self.shift].iter().filter(|m| **m).count();
        // One modifier alone (Ctrl + V, Alt + F) would take over keys other apps use.
        if !(self.win || (others >= 2 && (self.ctrl || self.alt))) {
            return Some("Use Win, or two of Ctrl, Alt and Shift, together with another key.");
        }
        let plain_win = self.win && !self.ctrl && !self.alt && !self.shift;
        if plain_win && matches!(self.key, 0xBE | 0xBA) {
            return Some("Windows uses Win + . and Win + ; for its emoji panel.");
        }
        if plain_win && self.key == 0x4C {
            return Some("Windows uses Win + L to lock the PC.");
        }
        if self.ctrl && self.alt && !self.win && self.key == 0x2E {
            return Some("Windows keeps Ctrl + Alt + Del for itself.");
        }
        None
    }
}

/// Modifiers held right now, while the chooser waits for a key.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Held {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Held {
    pub fn any(&self) -> bool {
        self.ctrl || self.alt || self.shift || self.win
    }

    pub fn keys(&self) -> Vec<&'static str> {
        [(self.ctrl, "Ctrl"), (self.win, "Win"), (self.alt, "Alt"), (self.shift, "Shift")]
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, k)| *k)
            .collect()
    }

    pub fn with(&self, key: u16) -> Shortcut {
        Shortcut { ctrl: self.ctrl, alt: self.alt, shift: self.shift, win: self.win, key }
    }
}

// ---------------------------------------------------------------- Windows

use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, HWND};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey, UnregisterHotKey,
    VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};

/// The id of the registered shortcut on the main window.
pub const ID: i32 = 1;
/// A short-lived registration that only asks whether a shortcut is free.
const TRIAL_ID: i32 = 2;

thread_local! {
    static CURRENT: Cell<Shortcut> = Cell::new(Shortcut::default());
    static MAIN: Cell<HWND> = Cell::new(HWND::default());
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
    static PAUSED: Cell<bool> = const { Cell::new(false) };
}

/// The shortcut PocketMedia uses (registered or not).
pub fn current() -> Shortcut {
    CURRENT.with(|c| c.get())
}

/// True while the shortcut is registered (or only paused by the chooser).
pub fn ok() -> bool {
    REGISTERED.with(|r| r.get()) || PAUSED.with(|p| p.get())
}

fn modifiers(s: Shortcut) -> HOT_KEY_MODIFIERS {
    let mut m = MOD_NOREPEAT.0;
    for (on, flag) in [(s.ctrl, MOD_CONTROL), (s.alt, MOD_ALT), (s.shift, MOD_SHIFT), (s.win, MOD_WIN)] {
        if on {
            m |= flag.0;
        }
    }
    HOT_KEY_MODIFIERS(m)
}

fn reason(e: &windows::core::Error) -> String {
    if e.code() == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult() {
        "Another app already uses it, or Windows keeps it for itself.".into()
    } else {
        format!("Windows refused it: {} (0x{:08X}).", e.message(), e.code().0)
    }
}

fn register_raw(main: HWND, id: i32, s: Shortcut) -> Result<(), String> {
    // SAFETY: `main` is our own window; plain integer arguments.
    unsafe { RegisterHotKey(Some(main), id, modifiers(s), s.key as u32) }.map_err(|e| reason(&e))
}

fn unregister_raw(main: HWND, id: i32) {
    // SAFETY: as above; unregistering an id that is not registered only fails.
    let _ = unsafe { UnregisterHotKey(Some(main), id) };
}

/// Registers `s` as PocketMedia's shortcut on `main` (start-up).
pub fn start(main: HWND, s: Shortcut) -> Result<(), String> {
    MAIN.with(|m| m.set(main));
    CURRENT.with(|c| c.set(s));
    let result = s.problem().map_or_else(|| register_raw(main, ID, s), |p| Err(p.to_string()));
    REGISTERED.with(|r| r.set(result.is_ok()));
    result
}

/// Switches to `new`. On failure the old shortcut is registered again and stays.
pub fn apply(new: Shortcut) -> Result<(), String> {
    let main = MAIN.with(|m| m.get());
    if let Some(p) = new.problem() {
        return Err(p.to_string());
    }
    let was = REGISTERED.with(|r| r.replace(false));
    if was {
        unregister_raw(main, ID);
    }
    match register_raw(main, ID, new) {
        Ok(()) => {
            CURRENT.with(|c| c.set(new));
            REGISTERED.with(|r| r.set(true));
            PAUSED.with(|p| p.set(false));
            Ok(())
        }
        Err(e) => {
            if was && !PAUSED.with(|p| p.get()) {
                REGISTERED.with(|r| r.set(register_raw(main, ID, current()).is_ok()));
            }
            Err(e)
        }
    }
}

/// Whether Windows would give PocketMedia `s` right now. While the chooser is
/// open PocketMedia's own shortcut is paused, so choosing it again reads as free.
pub fn is_free(s: Shortcut) -> Result<(), String> {
    if let Some(p) = s.problem() {
        return Err(p.to_string());
    }
    let main = MAIN.with(|m| m.get());
    register_raw(main, TRIAL_ID, s)?;
    unregister_raw(main, TRIAL_ID);
    Ok(())
}

/// While the shortcut chooser is open the shortcut is let go, so pressing it
/// there records it instead of opening the popup. Idempotent.
pub fn set_paused(paused: bool) {
    let main = MAIN.with(|m| m.get());
    if main.is_invalid() || PAUSED.with(|p| p.get()) == paused {
        return;
    }
    if paused {
        if REGISTERED.with(|r| r.replace(false)) {
            unregister_raw(main, ID);
            PAUSED.with(|p| p.set(true));
        }
    } else {
        PAUSED.with(|p| p.set(false));
        REGISTERED.with(|r| r.set(register_raw(main, ID, current()).is_ok()));
    }
}

/// Exit: let the shortcut go.
pub fn stop() {
    let main = MAIN.with(|m| m.get());
    if REGISTERED.with(|r| r.replace(false)) {
        unregister_raw(main, ID);
    }
    PAUSED.with(|p| p.set(false));
}

/// The modifiers held right now (for the chooser).
pub fn held_now() -> Held {
    // SAFETY: GetKeyState reads this thread's keyboard state; any key code is valid.
    let down = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
    Held {
        ctrl: down(VK_CONTROL.0),
        alt: down(VK_MENU.0),
        shift: down(VK_SHIFT.0),
        win: down(VK_LWIN.0) || down(VK_RWIN.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sc(ctrl: bool, alt: bool, shift: bool, win: bool, key: u16) -> Shortcut {
        Shortcut { ctrl, alt, shift, win, key }
    }

    #[test]
    fn the_default_is_win_alt_period() {
        let d = Shortcut::default();
        assert_eq!(d.label(), "Win + Alt + .");
        assert_eq!(d.problem(), None);
    }

    #[test]
    fn labels_list_modifiers_in_the_usual_order() {
        assert_eq!(sc(true, true, true, true, 0x50).label(), "Ctrl + Win + Alt + Shift + P");
        assert_eq!(sc(true, false, true, false, 0x20).label(), "Ctrl + Shift + Space");
        assert_eq!(sc(false, true, false, true, 0x75).keys(), ["Win", "Alt", "F6"]);
        assert_eq!(sc(true, true, false, false, 0x63).label(), "Ctrl + Alt + Num 3");
    }

    #[test]
    fn one_modifier_alone_is_refused_except_win() {
        assert!(sc(true, false, false, false, 0x56).problem().is_some(), "Ctrl + V would take over paste");
        assert!(sc(false, true, false, false, 0x46).problem().is_some(), "Alt + F opens menus");
        assert!(sc(false, false, true, false, 0x41).problem().is_some());
        assert!(sc(false, false, false, false, 0x41).problem().is_some());
        assert_eq!(sc(false, false, false, true, 0x4B).problem(), None, "Win + K");
        assert_eq!(sc(true, true, false, false, 0x50).problem(), None, "Ctrl + Alt + P");
        assert_eq!(sc(true, false, true, false, 0x20).problem(), None, "Ctrl + Shift + Space");
        assert!(sc(false, false, true, false, 0x20).problem().is_some(), "Shift alone is not enough");
    }

    #[test]
    fn keys_windows_keeps_are_refused() {
        assert!(sc(false, false, false, true, 0xBE).problem().is_some(), "Win + . stays Windows' own");
        assert!(sc(false, false, false, true, 0xBA).problem().is_some(), "Win + ;");
        assert!(sc(false, false, false, true, 0x4C).problem().is_some(), "Win + L");
        assert!(sc(true, true, false, false, 0x2E).problem().is_some(), "Ctrl + Alt + Del");
        assert_eq!(sc(false, true, false, true, 0xBE).problem(), None, "Win + Alt + . is fine");
    }

    #[test]
    fn unnamed_keys_are_refused() {
        for vk in [0x09, 0x0D, 0x1B, 0x08, 0x2C, 0x14] {
            assert!(sc(true, true, false, false, vk).problem().is_some(), "{vk:#x}");
        }
        assert!(is_modifier(0xA0) && is_modifier(0x5B) && is_modifier(0x11));
        assert!(!is_modifier(0x41));
    }

    #[test]
    fn held_modifiers_make_the_shortcut() {
        let h = Held { ctrl: true, shift: true, ..Default::default() };
        assert_eq!(h.keys(), ["Ctrl", "Shift"]);
        assert_eq!(h.with(0x4B).label(), "Ctrl + Shift + K");
        assert!(!Held::default().any());
    }

    #[test]
    fn saved_and_loaded_unchanged() {
        let s = sc(true, false, true, false, 0x75);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Shortcut>(&json).unwrap(), s);
    }
}
