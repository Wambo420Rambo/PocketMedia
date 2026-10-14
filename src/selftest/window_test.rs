//! Self-test steps for moving between windows and the popup's own behaviour
//! Quick popup from the Manager, dragging the
//! popup by its title bar, and "Copied to clipboard" when there is no app to
//! paste into (no Windows notification).

use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;

use super::{
    CMD_FORGET_TARGET, CMD_MANAGER, CMD_MANAGER_POINT, El, Report, Session, foreground, guard, is_visible,
    mouse_button, mouse_to, press, send, status, unpack, wait_until, window_rect,
};
use crate::ui::manager_view::MHit;

pub(super) fn run(r: &mut Report, t: &Session) -> bool {
    // Manager -> Quick popup: the Manager hides and the popup opens in front.
    if !t.open(r) || !t.click(r, El::OpenManager) {
        return false;
    }
    let manager = send(t.main, CMD_MANAGER, 0);
    if !r.check("the Manager opens", wait_until(2000, || is_visible(manager) && foreground() == manager), "") {
        return false;
    }
    let Some((x, y)) = unpack(send(t.main, CMD_MANAGER_POINT, super::manager_test::code(MHit::QuickPopup))) else {
        return r.check("find the Manager's Quick popup button", false, "not on screen");
    };
    if !guard(r, &t.allowed) {
        return false;
    }
    mouse_to(x, y);
    std::thread::sleep(Duration::from_millis(150));
    if let Some(rect) = window_rect(manager) {
        super::save_capture(r, &t.out.join("screens").join("state-manager-quick-popup-hover.png"), super::capture_screen(rect));
    }
    mouse_button(true);
    std::thread::sleep(Duration::from_millis(40));
    mouse_button(false);
    let switched = wait_until(2000, || !is_visible(manager) && is_visible(t.popup) && foreground() == t.popup);
    r.check("Quick popup hides the Manager and opens the popup", switched, &super::describe_foreground());
    if !switched {
        return false;
    }

    // The popup moves by its header (the logo is a handle).
    let (Some(before), Some((sx, sy))) = (window_rect(t.popup), t.point(El::Handle)) else {
        return r.check("find the popup's handle", false, "");
    };
    if !guard(r, &t.allowed) {
        return false;
    }
    let (dx, dy) = (80, 50);
    mouse_to(sx, sy);
    std::thread::sleep(Duration::from_millis(80));
    mouse_button(true);
    for step in 1..=10 {
        std::thread::sleep(Duration::from_millis(20));
        mouse_to(sx + dx * step / 10, sy + dy * step / 10);
    }
    std::thread::sleep(Duration::from_millis(80));
    mouse_button(false);
    std::thread::sleep(Duration::from_millis(250));
    let after = window_rect(t.popup);
    let moved = after.is_some_and(|a| (a.left - before.left - dx).abs() <= 2 && (a.top - before.top - dy).abs() <= 2);
    r.check("the popup can be dragged by its header", moved && is_visible(t.popup), &format!("{before:?} -> {after:?}"));
    r.check("the popup stays in front after the drag", foreground() == t.popup, &super::describe_foreground());
    if guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup));

    // No app to paste into (as when opened from the tray): copy, stay open, say so.
    if !t.open(r) {
        return false;
    }
    send(t.main, CMD_FORGET_TARGET, 0);
    t.click(r, El::Tile(0));
    std::thread::sleep(Duration::from_millis(300));
    t.park();
    t.shot(r, "state-popup-copied");
    r.check("with no app to paste into, the popup stays open", is_visible(t.popup), "");
    t.state_has(r, "the popup says \"Copied to clipboard\"", &["status=\"Copied to clipboard\""]);
    let (_, text) = status(t.main);
    r.check("the image is on the clipboard and nothing else is reported", text.starts_with("Copied to clipboard"), &text);
    if guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup));
    true
}
