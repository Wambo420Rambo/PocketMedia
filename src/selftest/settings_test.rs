//! Self-test steps for the Settings page: popup -> Manager -> Settings,
//! every screen (focused-40 to focused-46), staged edits with Save and
//! Cancel, the keyboard, and Add media from "Choose folder…".

use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{VK_DOWN, VK_LEFT, VK_RETURN, VK_SPACE, VK_TAB};

use super::manager_test;
use super::{
    CMD_MANAGER, CMD_MANAGER_DESCRIBE, CMD_MANAGER_POINT, CMD_RESET, CMD_SETTINGS_POINT, CMD_SETTINGS_SCROLL,
    DESCRIBE, El, Report, Session, capture_screen, describe_foreground, foreground, guard, is_visible, mouse_button,
    mouse_to, mouse_wheel, press, save_capture, send, unpack, wait_until, window_rect, window_text,
};
use crate::ui::manager_view::MHit;
use crate::ui::settings_view::SHit;

/// The scroll offset of focused-43 and focused-44 (Quick popup panel at the top).
const LIBRARY_PANEL_SCROLL: isize = 261;
const END: isize = 100_000;

/// SHit <-> number, for `CMD_SETTINGS_POINT`.
fn code(h: SHit) -> isize {
    match h {
        SHit::None => 0,
        SHit::Close => 1,
        // Not a control: a point inside the scroll area beside the panels' content.
        SHit::Caption => 2,
        SHit::Nav(i) => 10 + i as isize,
        SHit::Profile(i) => 20 + i as isize,
        SHit::Video => 30,
        SHit::VideoRow(i) => 31 + i as isize,
        SHit::Change => 40,
        SHit::CloseAfter => 41,
        SHit::OpenFolder => 42,
        SHit::ChooseFolder => 43,
        SHit::Backup => 44,
        SHit::Autostart => 45,
        SHit::Cancel => 50,
        SHit::Save => 51,
    }
}

pub(super) fn decode(c: isize) -> SHit {
    match c {
        1 => SHit::Close,
        2 => SHit::Caption,
        10..=14 => SHit::Nav((c - 10) as usize),
        20..=21 => SHit::Profile((c - 20) as usize),
        30 => SHit::Video,
        31..=32 => SHit::VideoRow((c - 31) as usize),
        40 => SHit::Change,
        41 => SHit::CloseAfter,
        42 => SHit::OpenFolder,
        43 => SHit::ChooseFolder,
        44 => SHit::Backup,
        45 => SHit::Autostart,
        50 => SHit::Cancel,
        51 => SHit::Save,
        _ => SHit::None,
    }
}

/// Test helpers bound to the Manager window showing Settings.
struct Set<'a> {
    t: &'a Session,
    hwnd: isize,
}

impl Set<'_> {
    fn describe(&self) -> String {
        send(self.t.main, CMD_MANAGER_DESCRIBE, 0);
        DESCRIBE.lock().map(|d| d.clone()).unwrap_or_default()
    }

    fn has(&self, r: &mut Report, name: &str, needles: &[&str]) -> bool {
        let d = self.describe();
        r.check(name, needles.iter().all(|n| d.contains(n)), &d)
    }

    fn click_at(&self, r: &mut Report, what: &str, point: Option<(i32, i32)>) -> bool {
        let Some((x, y)) = point else {
            return r.check(&format!("find {what}"), false, "not on screen");
        };
        if !guard(r, &self.t.allowed) {
            return false;
        }
        mouse_to(x, y);
        std::thread::sleep(Duration::from_millis(60));
        mouse_button(true);
        std::thread::sleep(Duration::from_millis(40));
        mouse_button(false);
        std::thread::sleep(Duration::from_millis(200));
        true
    }

    fn point(&self, hit: SHit) -> Option<(i32, i32)> {
        unpack(send(self.t.main, CMD_SETTINGS_POINT, code(hit)))
    }

    fn click(&self, r: &mut Report, hit: SHit) -> bool {
        self.click_at(r, &format!("Settings {hit:?}"), self.point(hit))
    }

    /// A Library-page control (the sidebar while the Library page shows).
    fn manager_click(&self, r: &mut Report, hit: MHit) -> bool {
        let point = unpack(send(self.t.main, CMD_MANAGER_POINT, manager_test::code(hit)));
        self.click_at(r, &format!("Manager {hit:?}"), point)
    }

    fn add_click(&self, r: &mut Report, el: El) -> bool {
        let point = unpack(send(self.t.main, super::CMD_MANAGER_ADD_POINT, el.code()));
        self.click_at(r, &format!("Settings Add media {el:?}"), point)
    }

    fn scroll_to(&self, y: isize) {
        send(self.t.main, CMD_SETTINGS_SCROLL, y);
        std::thread::sleep(Duration::from_millis(100));
    }

    /// Mouse to an empty spot of the title bar, so nothing shows a hover border.
    fn park(&self) {
        if let Some((x, y)) = self.point(SHit::None) {
            mouse_to(x, y);
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    fn shot(&self, r: &mut Report, name: &str) {
        std::thread::sleep(Duration::from_millis(150));
        if let Some(rect) = window_rect(self.hwnd) {
            save_capture(r, &self.t.out.join("screens").join(format!("{name}.png")), capture_screen(rect));
        }
    }

    /// The number after `key=` in the description.
    fn number(&self, key: &str) -> f32 {
        let d = self.describe();
        d.split(&format!("{key}=")).nth(1).and_then(|v| v.split(' ').next()).and_then(|v| v.parse().ok()).unwrap_or(-1.0)
    }
}

/// Injected keys are queued input; the description is a sent message, which the
/// UI thread answers first. Wait until the keys have been handled.
fn settle() {
    std::thread::sleep(Duration::from_millis(200));
}

pub(super) fn run(r: &mut Report, t: &Session) -> bool {
    send(t.main, CMD_RESET, 0);

    // Popup -> Manager -> Settings.
    if !t.open(r) || !t.click(r, El::OpenManager) {
        return false;
    }
    let hwnd = send(t.main, CMD_MANAGER, 0);
    let shown = wait_until(2000, || hwnd != 0 && is_visible(hwnd) && foreground() == hwnd);
    if !r.check("the Manager opens again from the popup after ×", shown, &describe_foreground()) {
        return false;
    }
    let s = Set { t, hwnd };
    s.manager_click(r, MHit::Nav(4));
    s.park();
    s.has(
        r,
        "Settings opens from the Manager sidebar with the defaults",
        &[
            "screen=settings",
            "profile=LessMemory video=HoverOnly close_after=true autostart=false",
            "dirty=false",
            "line=\"\"",
        ],
    );
    let d = s.describe();
    r.check("only the Settings entry is highlighted", d.ends_with("nav_selected=Settings"), &d);
    let title = window_text(hwnd);
    r.check("the window title follows the page", title == "PocketMedia - Settings", &title);

    // focused-40: saved state; then the end of the scroll area.
    s.shot(r, "focused-40-settings-default");
    if let Some((x, y)) = s.point(SHit::Caption)
        && guard(r, &t.allowed)
    {
        mouse_to(x, y);
        std::thread::sleep(Duration::from_millis(100));
        mouse_wheel(-120 * 10);
        std::thread::sleep(Duration::from_millis(250));
    }
    let (scroll, max) = (s.number("scroll"), s.number("max_scroll"));
    r.check("the mouse wheel scrolls Settings to its end", max > 0.0 && (scroll - max).abs() < 1.0, &s.describe());
    s.has(r, "the last setting is reachable by scrolling", &["autostart_visible=true"]);
    s.park();
    s.shot(r, "focused-40-settings-default-scrolled1");
    let save_at_end = s.point(SHit::Save);

    // focused-41: staged edits.
    s.scroll_to(0);
    let save_at_top = s.point(SHit::Save);
    r.check(
        "Save and Cancel stay pinned while the page scrolls",
        save_at_end.is_some() && save_at_end == save_at_top,
        &format!("{save_at_end:?} vs {save_at_top:?}"),
    );
    s.click(r, SHit::CloseAfter);
    s.scroll_to(END);
    s.click(r, SHit::Autostart);
    s.shot(r, "focused-41-settings-edited-unsaved");
    s.has(
        r,
        "edits are staged and \"Unsaved changes\" shows (focused-41)",
        &["close_after=false autostart=true", "saved=LessMemory/HoverOnly/true/false", "dirty=true", "line=\"Unsaved changes\""],
    );

    // focused-42: profile, video preview, and the shortcut's Change button.
    s.scroll_to(0);
    s.click(r, SHit::Profile(1));
    s.click(r, SHit::Video);
    s.shot(r, "state-settings-video-open");
    s.has(r, "the Video preview list opens", &["video_open=true"]);
    s.click(r, SHit::VideoRow(1));
    s.click(r, SHit::Change);
    s.shot(r, "focused-42-settings-shortcut-chooser");
    s.has(
        r,
        "profile and video preview are staged; Change says it is not built yet (focused-42)",
        &["profile=OpenFaster video=Off", "dirty=true", "Unsaved changes · Changing the shortcut is not built yet."],
    );
    s.scroll_to(END);
    s.park();
    s.shot(r, "focused-42-settings-shortcut-chooser-scrolled1");

    // focused-43 / focused-44: Open folder and Back up library.
    s.scroll_to(LIBRARY_PANEL_SCROLL);
    s.click(r, SHit::OpenFolder);
    s.park();
    s.shot(r, "focused-43-settings-open-folder");
    s.has(r, "Open folder says the real folder arrives in Phase 3 (focused-43)", &["dirty=true", "Phase 3"]);
    s.click(r, SHit::Backup);
    s.shot(r, "focused-44-settings-backup");
    s.has(r, "Back up library says when it arrives (focused-44)", &["dirty=true", "Phase 6"]);
    s.scroll_to(END);
    s.park();
    s.shot(r, "focused-44-settings-backup-scrolled1");

    // focused-45: Cancel discards every staged edit.
    s.scroll_to(0);
    s.click(r, SHit::Cancel);
    s.shot(r, "focused-45-settings-after-cancel");
    s.has(
        r,
        "Cancel discards the edits (focused-45)",
        &["profile=LessMemory video=HoverOnly close_after=true autostart=false", "dirty=false", "line=\"Changes discarded.\""],
    );

    // focused-46: Save commits; Save is disabled again.
    s.click(r, SHit::Profile(1));
    s.click(r, SHit::Save);
    s.shot(r, "focused-46-settings-after-save");
    s.has(r, "Save commits the edits (focused-46)", &["saved=OpenFaster/HoverOnly/true/false", "dirty=false", "line=\"Settings saved.\""]);
    s.click(r, SHit::Save);
    s.has(r, "a disabled Save does nothing", &["dirty=false", "line=\"Settings saved.\""]);

    // Leaving the page drops unsaved edits; saved ones stay.
    s.click(r, SHit::CloseAfter);
    s.click(r, SHit::Nav(0));
    s.has(r, "a sidebar type returns to the library", &["screen=manager", "filter=All"]);
    let title = window_text(hwnd);
    r.check("the window title says Library again", title == "PocketMedia - Library", &title);
    s.manager_click(r, MHit::Nav(4));
    s.has(r, "coming back shows the saved values, not the dropped edit", &["profile=OpenFaster", "close_after=true", "dirty=false"]);
    s.click(r, SHit::Nav(4));
    s.has(r, "clicking Settings while on Settings keeps the page", &["screen=settings"]);

    // Keyboard: Tab through the page; arrows, Space and Enter work the controls.
    if guard(r, &t.allowed) {
        press(&[VK_TAB]);
        s.park();
        s.shot(r, "state-settings-keyboard-focus");
        s.has(r, "Tab from the sidebar reaches the resource choice", &["focus=Profile", "focus_visible=true"]);
        press(&[VK_LEFT]);
        press(&[VK_TAB]);
        press(&[VK_DOWN]);
        settle();
        s.has(r, "arrow keys change the radio group and the select", &["profile=LessMemory video=Off", "dirty=true"]);
        for _ in 0..6 {
            press(&[VK_TAB]);
        }
        std::thread::sleep(Duration::from_millis(150));
        s.has(r, "Tab scrolls the focused setting into view", &["focus=Autostart", "autostart_visible=true"]);
        press(&[VK_SPACE]);
        press(&[VK_TAB]);
        press(&[VK_TAB]);
        settle();
        s.has(r, "Save is a Tab stop once something changed", &["focus=Save"]);
        press(&[VK_RETURN]);
        std::thread::sleep(Duration::from_millis(150));
        s.has(r, "Enter on Save commits", &["saved=LessMemory/Off/true/true", "dirty=false", "focus=Cancel"]);
    }

    // "Choose folder…" opens Add media at its review step; leaving it returns here.
    s.scroll_to(LIBRARY_PANEL_SCROLL);
    s.click(r, SHit::ChooseFolder);
    s.park();
    s.shot(r, "state-settings-choose-folder");
    s.has(r, "Choose folder… opens Add media at the review step", &["host=manager", "stage=review"]);
    s.add_click(r, El::AddCancel);
    s.has(r, "Cancel in Add media returns to Settings", &["screen=settings", "focus=ChooseFolder"]);
    s.click(r, SHit::ChooseFolder);
    s.add_click(r, El::AddAdd);
    s.has(r, "an import from Settings returns to Settings with the result", &["screen=settings", "4 files added"]);
    s.click(r, SHit::Nav(0));
    s.has(r, "the imported files are in the library", &["screen=manager", "total=12"]);

    // × hides the window from the Settings page too.
    s.manager_click(r, MHit::Nav(4));
    s.click(r, SHit::Close);
    r.check("× hides the window from Settings", wait_until(1500, || !is_visible(hwnd)), "");
    true
}
