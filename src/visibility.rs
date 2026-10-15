//! When GIFs cannot be seen although their window is open: the session is
//! locked, or other windows cover the whole Manager (or it is on another
//! virtual desktop). Animation stops then, as when the window is hidden
//! Nothing polls: the
//! check runs when the Manager repaints in the background, and while it is
//! covered, Windows reports window moves, closes and minimises (WinEvents).

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, EVENT_OBJECT_CLOAKED, EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE,
    EVENT_OBJECT_UNCLOAKED, EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, GW_HWNDPREV,
    GWL_EXSTYLE, GetWindow, GetWindowLongPtrW, GetWindowRect, IsIconic, IsWindowVisible, OBJID_WINDOW, PostMessageW,
    WINEVENT_OUTOFCONTEXT, WM_APP, WS_EX_LAYERED, WS_EX_TRANSPARENT,
};

/// Posted to the watched window when other windows moved, closed, hid or
/// minimised while it is covered: check again.
pub const WM_COVER_CHANGED: u32 = WM_APP + 6;

/// Rectangles in which the remaining visible pieces are tracked; more pieces
/// than this count as "visible" (stopping is only done when sure).
const MAX_PIECES: usize = 256;

/// True when `covers` together leave no part of `area` showing.
pub fn fully_covered(area: RECT, covers: &[RECT]) -> bool {
    let mut showing = vec![area];
    for c in covers {
        showing = showing.iter().flat_map(|r| subtract(r, c)).collect();
        if showing.is_empty() {
            return true;
        }
        if showing.len() > MAX_PIECES {
            return false;
        }
    }
    showing.is_empty()
}

/// The parts of `r` outside `c`: up to four rectangles.
fn subtract(r: &RECT, c: &RECT) -> Vec<RECT> {
    if c.right <= r.left || c.left >= r.right || c.bottom <= r.top || c.top >= r.bottom || r.left >= r.right || r.top >= r.bottom {
        return if r.left < r.right && r.top < r.bottom { vec![*r] } else { Vec::new() };
    }
    let mut out = Vec::new();
    if c.top > r.top {
        out.push(RECT { bottom: c.top, ..*r });
    }
    if c.bottom < r.bottom {
        out.push(RECT { top: c.bottom, ..*r });
    }
    let (top, bottom) = (r.top.max(c.top), r.bottom.min(c.bottom));
    if c.left > r.left {
        out.push(RECT { left: r.left, top, right: c.left, bottom });
    }
    if c.right < r.right {
        out.push(RECT { left: c.right, top, right: r.right, bottom });
    }
    out
}

/// True when `hwnd` cannot be seen: it is cloaked (another virtual desktop), or
/// the windows above it hide all of it. Windows that may be see-through
/// (layered or click-through) never count as covering.
pub fn is_covered(hwnd: HWND) -> bool {
    if cloaked(hwnd) {
        return true;
    }
    let Some(area) = frame(hwnd) else { return false };
    let mut covers = Vec::new();
    let mut above = hwnd;
    // A bounded walk up the z-order (there are rarely more than a few hundred windows).
    for _ in 0..2000 {
        // SAFETY: GetWindow tolerates any HWND; a stale one only ends the walk.
        match unsafe { GetWindow(above, GW_HWNDPREV) } {
            Ok(w) if !w.is_invalid() => above = w,
            _ => break,
        }
        if opaque(above)
            && let Some(r) = frame(above)
        {
            covers.push(r);
        }
    }
    fully_covered(area, &covers)
}

/// Shown, not minimised, not cloaked, and not possibly see-through.
fn opaque(w: HWND) -> bool {
    // SAFETY: plain queries on a window handle; a stale one only makes them fail.
    let (visible, iconic, ex) = unsafe { (IsWindowVisible(w).as_bool(), IsIconic(w).as_bool(), GetWindowLongPtrW(w, GWL_EXSTYLE) as u32) };
    visible && !iconic && ex & (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0) == 0 && !cloaked(w)
}

fn cloaked(w: HWND) -> bool {
    let mut value = 0u32;
    // SAFETY: `value` is a u32 local, the size DWMWA_CLOAKED writes.
    let ok = unsafe { DwmGetWindowAttribute(w, DWMWA_CLOAKED, &mut value as *mut u32 as *mut _, size_of::<u32>() as u32) };
    ok.is_ok() && value != 0
}

/// The window's visible frame on screen (without the invisible resize borders).
fn frame(w: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    // SAFETY: `r` is a RECT local, the size DWMWA_EXTENDED_FRAME_BOUNDS writes.
    let dwm = unsafe { DwmGetWindowAttribute(w, DWMWA_EXTENDED_FRAME_BOUNDS, &mut r as *mut RECT as *mut _, size_of::<RECT>() as u32) };
    // SAFETY: `r` is a local out-parameter.
    if dwm.is_err() && unsafe { GetWindowRect(w, &mut r) }.is_err() {
        return None;
    }
    (r.left < r.right && r.top < r.bottom).then_some(r)
}

// ---------------------------------------------------------------- session lock

/// The registration for lock and unlock messages (WM_WTSSESSION_CHANGE) to a
/// window; dropping it unregisters.
pub struct SessionNotes(HWND);

impl SessionNotes {
    pub fn register(hwnd: HWND) -> Result<Self, String> {
        // SAFETY: our own live window; undone in Drop.
        unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) }
            .map(|()| SessionNotes(hwnd))
            .map_err(|e| format!("lock notifications are not available: {e}"))
    }
}

impl Drop for SessionNotes {
    fn drop(&mut self) {
        // SAFETY: registered in `register` for this window.
        let _ = unsafe { WTSUnRegisterSessionNotification(self.0) };
    }
}

// ---------------------------------------------------------------- cover watch

/// The covered window to tell (as an isize), and whether a check is already on its way.
static WATCHED: AtomicIsize = AtomicIsize::new(0);
static POSTED: AtomicBool = AtomicBool::new(false);

/// While a window is covered: Windows reports when other windows move, close,
/// hide, minimise, change desktop or come to the front, and the window gets
/// one WM_COVER_CHANGED to check again. Dropping it stops the reports.
pub struct CoverWatch(Vec<HWINEVENTHOOK>);

impl CoverWatch {
    pub fn start(hwnd: HWND) -> Self {
        WATCHED.store(hwnd.0 as isize, Ordering::Relaxed);
        POSTED.store(false, Ordering::Relaxed);
        let ranges = [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
        ];
        let hooks = ranges
            .iter()
            // SAFETY: an out-of-context hook calls `on_event` on this thread's message loop; unhooked in Drop.
            .map(|&(lo, hi)| unsafe { SetWinEventHook(lo, hi, None, Some(on_event), 0, 0, WINEVENT_OUTOFCONTEXT) })
            .filter(|h| !h.is_invalid())
            .collect();
        CoverWatch(hooks)
    }

    /// The next change may post WM_COVER_CHANGED again (call when handling it).
    pub fn rearm(&self) {
        POSTED.store(false, Ordering::Relaxed);
    }
}

impl Drop for CoverWatch {
    fn drop(&mut self) {
        for h in self.0.drain(..) {
            // SAFETY: each hook was set in `start` and is removed once.
            let _ = unsafe { UnhookWinEvent(h) };
        }
        WATCHED.store(0, Ordering::Relaxed);
    }
}

unsafe extern "system" fn on_event(_: HWINEVENTHOOK, _event: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
    // Whole windows only (not carets, cursors or controls inside them).
    if object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 || hwnd.is_invalid() {
        return;
    }
    let watched = WATCHED.load(Ordering::Relaxed);
    if watched == 0 || POSTED.swap(true, Ordering::Relaxed) {
        return;
    }
    // SAFETY: an asynchronous message to our own window; a stale handle only makes it fail.
    let _ = unsafe { PostMessageW(Some(HWND(watched as *mut _)), WM_COVER_CHANGED, Default::default(), Default::default()) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT { left, top, right, bottom }
    }

    #[test]
    fn one_window_over_all_of_it_covers_it() {
        assert!(fully_covered(r(100, 100, 500, 400), &[r(0, 0, 800, 600)]));
        assert!(fully_covered(r(100, 100, 500, 400), &[r(100, 100, 500, 400)]), "exactly the same size");
    }

    #[test]
    fn any_part_left_showing_is_not_covered() {
        assert!(!fully_covered(r(100, 100, 500, 400), &[]));
        assert!(!fully_covered(r(100, 100, 500, 400), &[r(0, 0, 800, 399)]), "one row of pixels shows");
        assert!(!fully_covered(r(100, 100, 500, 400), &[r(101, 0, 800, 600)]), "one column shows");
        assert!(!fully_covered(r(100, 100, 500, 400), &[r(200, 200, 300, 300)]), "a hole around a small window");
    }

    #[test]
    fn several_windows_together_can_cover_it() {
        let area = r(0, 0, 400, 300);
        let halves = [r(0, 0, 200, 300), r(200, 0, 400, 300)];
        assert!(fully_covered(area, &halves));
        let with_gap = [r(0, 0, 199, 300), r(200, 0, 400, 300)];
        assert!(!fully_covered(area, &with_gap));
        let quarters = [r(-10, -10, 210, 160), r(200, -5, 420, 150), r(0, 150, 200, 310), r(195, 140, 400, 300)];
        assert!(fully_covered(area, &quarters));
    }

    #[test]
    fn many_small_pieces_count_as_showing() {
        // A comb of thin windows: never sure, so the GIFs keep playing.
        let area = r(0, 0, 2000, 10);
        let teeth: Vec<RECT> = (0..1000).map(|i| r(i * 2, 0, i * 2 + 1, 10)).collect();
        assert!(!fully_covered(area, &teeth));
    }
}
