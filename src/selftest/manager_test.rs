//! Self-test steps for the Manager window: opening it from the popup,
//! every screen (focused-20 to focused-31), rename and removal rules,
//! Add media from the Manager, and the shared library.

use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{VK_A, VK_BACK, VK_C, VK_CONTROL, VK_ESCAPE, VK_TAB, VK_V};

use super::{
    ALL, CMD_FOCUS, CMD_MANAGER, CMD_MANAGER_ADD_POINT, CMD_MANAGER_DESCRIBE, CMD_MANAGER_POINT, CMD_RESET,
    DESCRIBE, El, Report, Session, capture_screen, describe_foreground, drag_to_target, foreground, guard, is_visible,
    media_file, mouse_button, mouse_to, press, save_capture, send, type_unicode, unpack, wait_until, window_rect,
};
use crate::ui::manager_view::MHit;

const BY_NAME: [&str; 8] = [
    "Celebration.mp4", "Happy dance.gif", "Hello.gif", "Portrait.png", "Race clip.mp4", "Reaction.gif", "Square.png", "Wallpaper.jpg",
];

/// MHit <-> number, for `CMD_MANAGER_POINT`.
pub(super) fn code(h: MHit) -> isize {
    match h {
        MHit::None => 0,
        MHit::Close => 1,
        MHit::QuickPopup => 3,
        MHit::Caption => 2,
        MHit::Nav(i) => 10 + i as isize,
        MHit::AddMedia => 20,
        MHit::Search => 21,
        MHit::SearchClear => 22,
        MHit::ClearFilters => 23,
        MHit::Sort => 24,
        MHit::SortRow(i) => 30 + i as isize,
        MHit::Scrollbar => 40,
        MHit::EmptyAction => 41,
        MHit::RenameInput => 50,
        MHit::Rename => 51,
        MHit::OpenFolder => 52,
        MHit::Recycle => 53,
        MHit::ConfirmMove => 54,
        MHit::ConfirmCancel => 55,
        MHit::Tile(i) => 100 + i as isize,
    }
}

pub(super) fn decode(c: isize) -> MHit {
    match c {
        1 => MHit::Close,
        3 => MHit::QuickPopup,
        2 => MHit::Caption,
        10..=14 => MHit::Nav((c - 10) as usize),
        20 => MHit::AddMedia,
        21 => MHit::Search,
        22 => MHit::SearchClear,
        23 => MHit::ClearFilters,
        24 => MHit::Sort,
        30..=31 => MHit::SortRow((c - 30) as usize),
        40 => MHit::Scrollbar,
        41 => MHit::EmptyAction,
        50 => MHit::RenameInput,
        51 => MHit::Rename,
        52 => MHit::OpenFolder,
        53 => MHit::Recycle,
        54 => MHit::ConfirmMove,
        55 => MHit::ConfirmCancel,
        100.. => MHit::Tile((c - 100) as usize),
        _ => MHit::None,
    }
}

/// Test helpers bound to the Manager window.
struct Mgr<'a> {
    t: &'a Session,
    hwnd: isize,
}

impl Mgr<'_> {
    fn describe(&self) -> String {
        send(self.t.main, CMD_MANAGER_DESCRIBE, 0);
        DESCRIBE.lock().map(|d| d.clone()).unwrap_or_default()
    }

    fn has(&self, r: &mut Report, name: &str, needles: &[&str]) -> bool {
        let d = self.describe();
        r.check(name, needles.iter().all(|n| d.contains(n)), &d)
    }

    fn expect(&self, r: &mut Report, name: &str, expected: &[&str]) {
        let d = self.describe();
        let names = d.split("names=").nth(1).unwrap_or("");
        let got: Vec<&str> = if names.is_empty() { vec![] } else { names.split('|').collect() };
        r.check(name, got == expected, &d);
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

    fn click(&self, r: &mut Report, hit: MHit) -> bool {
        let point = unpack(send(self.t.main, CMD_MANAGER_POINT, code(hit)));
        self.click_at(r, &format!("Manager {hit:?}"), point)
    }

    /// Clicks an Add media element while the panel is open in the Manager.
    fn add_click(&self, r: &mut Report, el: El) -> bool {
        let point = unpack(send(self.t.main, CMD_MANAGER_ADD_POINT, el.code()));
        self.click_at(r, &format!("Manager Add media {el:?}"), point)
    }

    fn park(&self) {
        if let Some((x, y)) = unpack(send(self.t.main, CMD_MANAGER_POINT, code(MHit::None))) {
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

    /// Replaces the name in the inspector's name field (empty = clear it).
    fn rename(&self, r: &mut Report, text: &str) -> bool {
        if !self.click(r, MHit::RenameInput) || !guard(r, &self.t.allowed) {
            return false;
        }
        press(&[VK_CONTROL, VK_A]);
        if text.is_empty() {
            press(&[VK_BACK]);
        } else {
            type_unicode(text);
        }
        std::thread::sleep(Duration::from_millis(200));
        true
    }
}

pub(super) fn run(r: &mut Report, t: &Session) -> bool {
    send(t.main, CMD_RESET, 0);

    // Popup -> Manager.
    if !t.open(r) || !t.click(r, El::OpenManager) {
        return false;
    }
    let hwnd = send(t.main, CMD_MANAGER, 0);
    let shown = wait_until(2000, || hwnd != 0 && is_visible(hwnd) && foreground() == hwnd);
    r.check("Open manager shows the Manager", shown, &describe_foreground());
    r.check("Open manager closes the popup", wait_until(1000, || !is_visible(t.popup)), "");
    if !shown {
        return false;
    }
    let m = Mgr { t, hwnd };
    if let Some(rect) = window_rect(hwnd) {
        // SAFETY: GetDpiForWindow tolerates any HWND.
        let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(super::hwnd(hwnd)) };
        let scale = dpi as f32 / 96.0;
        let (w, h) = ((rect.right - rect.left) as f32 / scale, (rect.bottom - rect.top) as f32 / scale);
        r.info(&format!("manager rect {rect:?}, dpi {dpi}"));
        r.check("Manager is 970 x 700 DIP (fixed size)", (w - 970.0).abs() < 1.5 && (h - 700.0).abs() < 1.5, &format!("{w:.1} x {h:.1}"));
    }
    m.park();
    m.has(r, "Manager starts with the first item selected", &["selected=\"Happy dance.gif\"", "rename=\"Happy dance\"", "total=8"]);
    m.has(r, "no scrollbar when the gallery fits", &["scrollbar=false"]);
    m.expect(r, "Manager lists the library, recently added first", &ALL);

    // focused-20: an item selected in the inspector.
    m.click(r, MHit::Tile(1));
    m.park();
    m.shot(r, "focused-20-manager-default");
    m.has(r, "clicking a tile shows it in the inspector (focused-20)", &["selected=\"Square.png\"", "rename=\"Square\""]);

    // focused-22: sidebar media types.
    for (nav, shot, expected, first) in [
        (1, "focused-22-manager-type-gif", &["Happy dance.gif", "Hello.gif", "Reaction.gif"][..], "Happy dance.gif"),
        (2, "focused-22-manager-type-picture", &["Square.png", "Wallpaper.jpg", "Portrait.png"][..], "Square.png"),
        (3, "focused-22-manager-type-video", &["Race clip.mp4", "Celebration.mp4"][..], "Race clip.mp4"),
    ] {
        m.click(r, MHit::Nav(nav));
        m.park();
        m.shot(r, shot);
        m.expect(r, &format!("sidebar type ({shot})"), expected);
        m.has(r, &format!("{shot}: the first item of the type is selected"), &[&format!("selected=\"{first}\"")]);
    }
    m.click(r, MHit::ClearFilters);
    m.expect(r, "Clear filters returns to All media", &ALL);

    // focused-24: sort by name.
    m.click(r, MHit::Sort);
    m.shot(r, "state-manager-sort-open");
    m.click(r, MHit::SortRow(1));
    m.park();
    m.shot(r, "focused-24-manager-sort-name");
    m.expect(r, "Sort by Name (focused-24)", &BY_NAME);
    m.click(r, MHit::Sort);
    m.click(r, MHit::SortRow(0));
    m.expect(r, "Sort back to Recently added", &ALL);

    // Rename uses the same rules as Add media; Rename stays disabled while invalid.
    m.click(r, MHit::Tile(2));
    for (text, needle, name) in [
        ("Cat/photo", "error=Some(IllegalChar)", "\"Cat/photo\" is an error and disables Rename"),
        ("", "error=Some(Empty)", "an empty name is an error and disables Rename"),
        ("Celebration", "error=Some(Taken)", "another item's name is an error and disables Rename"),
        ("CON", "error=Some(Reserved)", "a reserved name is an error"),
    ] {
        if !m.rename(r, text) {
            return false;
        }
        if text == "Cat/photo" {
            m.shot(r, "state-manager-rename-error");
        }
        m.has(r, name, &[needle, "can_rename=false"]);
    }
    if !m.rename(r, "Renamed clip") {
        return false;
    }
    m.has(r, "a valid name enables Rename", &["error=None", "can_rename=true"]);
    m.click(r, MHit::Rename);
    m.park();
    m.shot(r, "focused-28-manager-rename");
    m.has(
        r,
        "Rename keeps the extension and the item (focused-28)",
        &["selected=\"Renamed clip.mp4\"", "status=\"Renamed to Renamed clip.mp4.\"", "|Renamed clip.mp4|"],
    );
    let media = t.out.join("app").join("Media");
    r.check(
        "Rename renames the file on disk",
        media.join("Videos").join("Renamed clip.mp4").is_file() && !media.join("Videos").join("Race clip.mp4").exists(),
        "",
    );

    // focused-30: Open folder (fake data has no files yet).
    m.click(r, MHit::OpenFolder);
    m.park();
    m.shot(r, "focused-30-manager-open-folder");
    // Explorer is not started under the self-test (it would take the foreground);
    // the file it would select is checked. Opening it for real is a manual check.
    let selected = media.join("Videos").join("Renamed clip.mp4").display().to_string();
    m.has(r, "Open folder shows the file in File Explorer (focused-30)", &[&format!("opened={selected:?}")]);

    // focused-29: Move to Recycle Bin asks first.
    m.click(r, MHit::Recycle);
    m.park();
    m.shot(r, "focused-29-manager-recycle-confirm");
    m.has(r, "Move to Recycle Bin asks for confirmation (focused-29)", &["confirming=true", "total=8"]);
    m.has(r, "the confirmation replaces the Open folder note", &["status=\"\""]);
    m.click(r, MHit::ConfirmCancel);
    m.has(r, "Cancel keeps the item", &["confirming=false", "total=8", "selected=\"Renamed clip.mp4\""]);
    m.click(r, MHit::Recycle);
    m.click(r, MHit::ConfirmMove);
    m.has(
        r,
        "confirming moves it to the Recycle Bin and selects the next item",
        &["total=7", "selected=\"Hello.gif\"", "Renamed clip.mp4 was moved to the Recycle Bin."],
    );
    r.check("the file left the Media folder", !media.join("Videos").join("Renamed clip.mp4").exists(), "");

    // focused-31: a search with no match.
    if !m.click(r, MHit::Search) || !guard(r, &t.allowed) {
        return false;
    }
    type_unicode("zzzz");
    m.park();
    m.shot(r, "focused-31-manager-empty-search");
    m.has(r, "no match empties the gallery and inspector (focused-31)", &["count=0", "selected=\"\"", "query=\"zzzz\""]);
    m.click(r, MHit::EmptyAction);
    m.has(r, "Clear filters in the empty state brings everything back", &["count=7", "query=\"\""]);

    // Keyboard: Tab moves a visible focus ring through the inspector.
    m.click(r, MHit::Tile(0));
    if guard(r, &t.allowed) {
        press(&[VK_TAB]);
        press(&[VK_TAB]);
        m.park();
        m.shot(r, "state-manager-keyboard-focus");
        m.has(r, "Tab moves focus from the gallery to the inspector", &["focus=Rename"]);
    }

    // Add media from the Manager.
    m.click(r, MHit::AddMedia);
    m.park();
    m.shot(r, "state-manager-add-media");
    m.has(r, "Add media opens inside the Manager", &["host=manager", "stage=start"]);
    m.add_click(r, El::AddChooseFiles);
    wait_until(8000, || m.describe().contains("stage=review"));
    m.has(r, "Choose files shows the review list in the Manager", &["stage=review", "can_add=true"]);
    m.add_click(r, El::AddAdd);
    wait_until(15000, || m.describe().starts_with("screen=manager"));
    m.park();
    m.shot(r, "state-manager-after-import");
    m.has(
        r,
        "files added from the Manager show first, newest selected",
        &["total=12", "selected=\"Hello (2).gif\"", "names=Hello (2).gif|IMG_4821.png|Clip.mp4|Wave.gif|Cat.png|", "5 files added"],
    );

    transfer_tests(r, t, &m);

    // The popup sees the same library: the rename, the removal and the import.
    if !t.open(r) {
        return false;
    }
    t.click(r, El::Tab(1));
    t.state_has(r, "the popup shows the Manager's changes", &["count=12", "names=Hello (2).gif|IMG_4821.png|", "Hello.gif"]);
    let d = t.describe();
    r.check("the removed item is gone from the popup too", !d.contains("Renamed clip") && !d.contains("Race clip"), &d);
    if guard(r, &t.allowed) {
        press(&[VK_ESCAPE]);
    }
    wait_until(1500, || !is_visible(t.popup));

    // × hides the Manager; the app keeps running in the tray.
    send(t.main, CMD_FOCUS, hwnd);
    wait_until(1500, || foreground() == hwnd);
    m.click(r, MHit::Close);
    r.check("× hides the Manager", wait_until(1500, || !is_visible(hwnd)), "");
    // The UI thread still answers and the (hidden) window still exists.
    r.check("the app is still running after ×", send(t.main, CMD_MANAGER, 0) == hwnd, "");
    true
}

/// Like the quick popup: Ctrl+C on the selected item puts its
/// file on the clipboard, and a tile drags out to another app as a copy.
fn transfer_tests(r: &mut Report, t: &Session, m: &Mgr) {
    let d = m.describe();
    let name = d.split("names=").nth(1).and_then(|n| n.split('|').next()).unwrap_or("").to_string();
    let file = media_file(&t.out, &name);

    // Ctrl+C here, then Ctrl+V in the test target: the target gets the library file.
    for f in ["paste-formats.txt", "paste-received.png", "paste-hdrop.txt", "paste-dib.txt"] {
        let _ = std::fs::remove_file(t.out.join(f));
    }
    m.click(r, MHit::Tile(0));
    if !guard(r, &t.allowed) {
        return;
    }
    press(&[VK_CONTROL, VK_C]);
    let copied = wait_until(4000, || m.describe().contains(crate::paste::COPIED_IN_MANAGER));
    m.park();
    m.shot(r, "phase7-manager-copied");
    send(t.main, CMD_FOCUS, t.target);
    wait_until(1500, || foreground() == t.target);
    if guard(r, &t.allowed) {
        press(&[VK_CONTROL, VK_V]);
    }
    let formats = t.out.join("paste-formats.txt");
    let got = wait_until(4000, || formats.exists());
    let hdrop = std::fs::read_to_string(t.out.join("paste-hdrop.txt")).unwrap_or_default();
    r.check(
        &format!("Ctrl+C in the Manager copies {name}; Ctrl+V in another app gets the library file"),
        copied && got && hdrop == file.display().to_string(),
        &format!("status shown: {copied}; target got: {hdrop:?}"),
    );

    // Drag a tile from the Manager to the test target.
    send(t.main, CMD_FOCUS, m.hwnd);
    wait_until(1500, || foreground() == m.hwnd);
    let _ = std::fs::remove_file(t.out.join("drop-received.png"));
    let Some(from) = unpack(send(t.main, CMD_MANAGER_POINT, code(MHit::Tile(0)))) else {
        r.check("find the Manager's first tile", false, "");
        return;
    };
    if !drag_to_target(r, t, from) {
        return;
    }
    let received = t.out.join("drop-received.png");
    let dropped = wait_until(4000, || received.exists());
    let same = dropped && std::fs::read(&received).ok().is_some_and(|b| std::fs::read(&file).ok() == Some(b));
    let from_file = std::fs::read_to_string(t.out.join("drop-source.txt")).unwrap_or_default();
    r.check(
        &format!("a tile drags out of the Manager: the target gets {name}, byte for byte"),
        same && from_file == file.display().to_string(),
        &from_file,
    );
    r.check("…as a copy: the library file stays", file.is_file(), &file.display().to_string());
    send(t.main, CMD_FOCUS, m.hwnd);
    wait_until(1500, || foreground() == m.hwnd);
    m.has(r, "…and the Manager says a copy was dropped", &["Dropped a copy"]);
}
