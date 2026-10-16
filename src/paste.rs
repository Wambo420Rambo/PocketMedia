//! Paste a library item into the app that was focused before the popup opened:
//! prepare the clipboard (worker thread), hand focus back, verify, then Ctrl+V.
//!
//! - Ctrl+V is sent only once Windows reports the target (or a window it owns)
//!   as the foreground window and no modifier keys are held. If that does not
//!   happen within about a second, nothing is sent: input must never land in
//!   the wrong app. The popup comes back and says so; the item can be dragged.
//! - Only Ctrl+V is ever sent. Never Enter (no sending chat messages).
//! - With no app to paste into, the item stays on the clipboard and the popup
//!   says "Copied to clipboard" (no Windows notification).
//! - "Close after inserting" (Settings, on by default): when off, the popup
//!   stays on screen without taking focus, and the next paste goes to the same app.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::HWND;
use windows::Win32::System::Ole::DROPEFFECT_COPY;
use windows::Win32::UI::WindowsAndMessaging::{IsIconic, KillTimer, SW_RESTORE, SetForegroundWindow, SetTimer, ShowWindow};

use crate::jobs::{self, Event};
use crate::target::{self, Elevation};
use crate::{StatusKind, TIMER_PASTE, app, library, popup, set_status, transfer, tray};

const TICK_MS: u32 = 25;
const MAX_TICKS: u32 = 40; // ~1 second
const COPIED: &str = "Copied to clipboard";
const NOT_SENT: &str = "Paste didn't go through. It's on the clipboard: press Ctrl+V in the app, or drag the item there.";

thread_local! {
    static TICKS_LEFT: Cell<u32> = const { Cell::new(0) };
    /// The clipboard job being prepared: (job id, item id).
    static PREPARING: Cell<Option<(u64, u32)>> = const { Cell::new(None) };
    /// The item whose Ctrl+V is waiting for focus.
    static PASTING: Cell<Option<u32>> = const { Cell::new(None) };
    /// Ctrl+C in the Manager: the clipboard job being prepared (job id, item id).
    static COPYING: Cell<Option<(u64, u32)>> = const { Cell::new(None) };
}

/// The line the Manager shows after Ctrl+C.
pub const COPIED_IN_MANAGER: &str = "Copied to clipboard. Paste it with Ctrl+V.";

/// The popup's footer (two short lines) when a paste or a drag-out went
/// through but its Recently used entry could not be saved.
pub const PASTE_UNSAVED: &str = "Pasted, but Recently used was not saved. Check the Data folder.";
pub const DRAG_UNSAVED: &str = "Dragged, but Recently used was not saved. Check the Data folder.";

/// The same for the Manager's wider status line, with the reason.
pub fn unsaved_long(what: &str, why: &str) -> String {
    let why = why.trim_end_matches('.');
    format!("{what}, but Recently used could not be saved ({why}). Check that the Data folder beside PocketMedia can be written to.")
}

/// The reason a history save failed, if the user should hear of it. A library
/// file from a newer PocketMedia is never saved; that was said at start-up.
fn unsaved(saved: Result<(), String>) -> Option<String> {
    saved.err().filter(|_| !crate::store::read_only())
}

/// A paste is being prepared or is waiting to send Ctrl+V: the popup must stay
/// (it may come back to say the paste failed).
pub fn pending() -> bool {
    PREPARING.with(|p| p.get()).is_some() || PASTING.with(|p| p.get()).is_some()
}

/// Item `id`'s file in the Media folder.
fn media_file(id: u32) -> Option<PathBuf> {
    library::with(|lib| lib.get(id).map(|i| i.path.clone()))
        .and_then(|rel| crate::paths::get().and_then(|p| crate::import::media_file(&p.media, &rel)))
}

/// Ctrl+C in the Manager: puts item `id` on the clipboard with the same formats
/// as a paste from the popup, and nothing more (no app to paste into, no history).
/// The result is shown in the Manager. False if the item is gone.
pub fn copy(id: u32) -> bool {
    let Some(file) = media_file(id) else { return false };
    let job = jobs::prepare(file);
    COPYING.with(|c| c.set(Some((job.id, id))));
    true
}

/// How a drag of a library item ended.
pub enum Dragged {
    /// The target accepted a copy. Some: its Recently used entry could not be saved (why).
    Copied(Option<String>),
    NoDrop,
    Failed(String),
}

/// Drags item `id` out of `source` as a copy of its original file (the shell's
/// data object, copy only). A finished copy-drag goes to Recently used.
/// Used by the popup and the Manager.
pub fn drag_item(source: HWND, id: u32) -> Dragged {
    let Some(file) = media_file(id) else {
        return Dragged::Failed("That item is no longer in the library.".into());
    };
    DRAGGING_OUT.store(true, Ordering::Relaxed);
    let dragged = transfer::drag_file_out(source, &file);
    DRAGGING_OUT.store(false, Ordering::Relaxed);
    match dragged {
        Ok(effect) if effect.0 & DROPEFFECT_COPY.0 != 0 => {
            library::with_mut(|lib| lib.record_use(id));
            Dragged::Copied(unsaved(library::persist()))
        }
        Ok(_) => Dragged::NoDrop,
        Err(e) => Dragged::Failed(e),
    }
}

/// A library item is being dragged out of PocketMedia right now.
static DRAGGING_OUT: AtomicBool = AtomicBool::new(false);

/// True while one of our own tiles is being dragged: our windows then refuse the
/// drop, so a tile let go over PocketMedia itself is not imported again.
pub fn dragging_out() -> bool {
    DRAGGING_OUT.load(Ordering::Relaxed)
}

/// Self-test only: the next paste fails as if focus never came back.
pub static FAIL_NEXT: AtomicBool = AtomicBool::new(false);

pub fn cancel() {
    TICKS_LEFT.with(|t| t.set(0));
    PREPARING.with(|p| p.set(None));
    PASTING.with(|p| p.set(None));
    popup::set_keep_open(false);
    let main = app(|a| a.main.get());
    // SAFETY: our own timer on our own window; harmless if not running.
    unsafe {
        let _ = KillTimer(Some(main), TIMER_PASTE);
    }
    // A hidden popup that waited for this paste may be released now.
    crate::release_hidden_later();
}

/// Click or Enter on a tile: start preparing item `id` for the clipboard.
pub fn begin(id: u32) {
    cancel();
    let Some(file) = media_file(id) else {
        popup::show_status("That item is no longer in the library.");
        return;
    };
    let job = jobs::prepare(file);
    PREPARING.with(|p| p.set(Some((job.id, id))));
}

/// A finished clipboard preparation. Returns true if it was a paste job.
pub fn on_prepared(ev: &Event) -> bool {
    let Event::Prepared { job, result } = ev else { return false };
    if COPYING.with(|c| c.get()).is_some_and(|(j, _)| j == *job) {
        COPYING.with(|c| c.set(None));
        let main = app(|a| a.main.get());
        let done = match result {
            Ok(p) => transfer::put_on_clipboard(main, p),
            Err(e) => Err(e.clone()),
        };
        let line = match done {
            Ok(()) => {
                set_status("Copied to clipboard from the Manager.", StatusKind::Good);
                COPIED_IN_MANAGER.to_string()
            }
            Err(e) => {
                set_status(format!("Copy failed: {e}"), StatusKind::Bad);
                format!("Could not copy: {e}")
            }
        };
        crate::manager::show_status(&line);
        return true;
    }
    let Some((_, id)) = PREPARING.with(|p| p.get()).filter(|(j, _)| j == job) else {
        return true; // an older click; a newer one replaced it
    };
    PREPARING.with(|p| p.set(None));
    if !popup::is_visible() {
        return true; // the popup was closed meanwhile: do nothing
    }
    let payload = match result {
        Ok(p) => p,
        Err(e) => {
            set_status(format!("Copy failed: {e}"), StatusKind::Bad);
            popup::show_status(&format!("Could not copy: {e}"));
            return true;
        }
    };
    let main = app(|a| a.main.get());
    if let Err(e) = transfer::put_on_clipboard(main, payload) {
        set_status(format!("Copy failed: {e}"), StatusKind::Bad);
        popup::show_status(&format!("Could not copy: {e}"));
        return true;
    }
    deliver(id);
    true
}

/// Self-test only: the next target's elevation reads as unknown.
pub static UNKNOWN_NEXT: AtomicBool = AtomicBool::new(false);

/// Why Ctrl+V must not be sent, if it must not. Windows silently drops input
/// from a normal app into one that runs as administrator, so a paste there
/// would be reported as sent while nothing arrived. When the target's state
/// cannot be read, it may be such an app: nothing is sent then either, and the
/// item waits on the clipboard.
fn elevation_block(target: Elevation, ours: Elevation) -> Option<&'static str> {
    if ours == Elevation::Elevated {
        return None;
    }
    match target {
        Elevation::Elevated => Some("it runs as administrator"),
        Elevation::Unknown => Some("it may run as administrator"),
        Elevation::NotElevated => None,
    }
}

/// The item is on the clipboard: hand focus to the target and wait to send Ctrl+V.
fn deliver(id: u32) {
    let main = app(|a| a.main.get());
    let t = match app(|a| a.target.borrow().clone()) {
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
    let theirs = if UNKNOWN_NEXT.swap(false, Ordering::Relaxed) { Elevation::Unknown } else { target::process_elevation(Some(t.pid)) };
    if let Some(why) = elevation_block(theirs, target::process_elevation(None)) {
        set_status(format!("Copied to clipboard; {} {why}.", t.exe), StatusKind::Neutral);
        popup::show_status(&format!("{COPIED} - press Ctrl+V in {} yourself ({why})", t.exe));
        return;
    }
    let close_after = crate::store::settings().close_after_insert;
    popup::set_keep_open(!close_after);
    // SAFETY: plain window calls; the target may have closed, which only makes them fail.
    unsafe {
        if IsIconic(t.hwnd).as_bool() {
            let _ = ShowWindow(t.hwnd, SW_RESTORE);
        }
        // Hand focus back while we still own the foreground, then hide.
        let _ = SetForegroundWindow(t.hwnd);
    }
    if close_after {
        popup::close(false);
    }
    PASTING.with(|p| p.set(Some(id)));
    TICKS_LEFT.with(|c| c.set(MAX_TICKS));
    // SAFETY: our own window; the timer is killed in `cancel`.
    unsafe {
        SetTimer(Some(main), TIMER_PASTE, TICK_MS, None);
    }
}

pub fn tick() {
    let left = TICKS_LEFT.with(|c| c.get());
    let id = PASTING.with(|p| p.get());
    let (Some(id), Ok(t)) = (id, app(|a| a.target.borrow().clone())) else {
        cancel();
        return;
    };
    if left == 0 {
        cancel();
        return;
    }
    let main = app(|a| a.main.get());
    let forced_fail = FAIL_NEXT.load(Ordering::Relaxed);

    if !forced_fail && t.owns_foreground() && !transfer::modifiers_held() {
        cancel();
        match transfer::send_ctrl_v() {
            Ok(()) => {
                set_status(format!("Sent Ctrl+V to {}.", t.label()), StatusKind::Good);
                tray::set_tip(main, &format!("PocketMedia - last paste: {}", t.exe));
                // History: recorded when a paste is initiated.
                library::with_mut(|lib| lib.record_use(id));
                if let Some(e) = unsaved(library::persist()) {
                    // Said in the popup's footer, now if it stayed open, else at the next open.
                    set_status(format!("Sent Ctrl+V to {}, but {e}.", t.label()), StatusKind::Bad);
                    if popup::is_visible() {
                        popup::show_status(PASTE_UNSAVED);
                    } else {
                        popup::notice_next_open(PASTE_UNSAVED);
                    }
                }
                popup::refresh();
            }
            Err(e) => {
                set_status(format!("Paste not sent: {e}"), StatusKind::Bad);
                popup::show_failed(NOT_SENT);
            }
        }
        return;
    }

    TICKS_LEFT.with(|c| c.set(left - 1));
    if left - 1 == 0 {
        cancel();
        FAIL_NEXT.store(false, Ordering::Relaxed);
        let why = if transfer::modifiers_held() {
            "a Win/Alt/Shift/Ctrl key was still held down"
        } else {
            "Windows did not give focus back to it"
        };
        set_status(format!("Nothing was pasted into {}: {why}. The item is on the clipboard.", t.exe), StatusKind::Bad);
        popup::show_failed(NOT_SENT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ctrl+V goes only to an app known not to run as
    /// administrator (or from an elevated PocketMedia, which Windows allows).
    #[test]
    fn ctrl_v_only_when_windows_will_deliver_it() {
        use Elevation::*;
        assert_eq!(elevation_block(NotElevated, NotElevated), None);
        assert_eq!(elevation_block(Elevated, NotElevated), Some("it runs as administrator"));
        assert_eq!(elevation_block(Unknown, NotElevated), Some("it may run as administrator"));
        assert_eq!(elevation_block(Unknown, Unknown), Some("it may run as administrator"));
        for theirs in [Elevated, NotElevated, Unknown] {
            assert_eq!(elevation_block(theirs, Elevated), None, "{theirs:?}");
        }
    }
}
