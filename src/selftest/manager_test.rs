//! Self-test steps for the Manager window: opening it from the popup,
//! every screen (focused-20 to focused-31), rename and removal rules,
//! Add media from the Manager, and the shared library.

use std::time::{Duration, Instant};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_A, VK_BACK, VK_C, VK_CONTROL, VK_DELETE, VK_END, VK_ESCAPE, VK_F2, VK_HOME, VK_NEXT, VK_PRIOR, VK_TAB, VK_V,
};

use super::{
    ALL, CMD_FOCUS, CMD_HOTKEY_OK, CMD_MANAGER, CMD_MANAGER_ADD_POINT, CMD_MANAGER_DESCRIBE, CMD_MANAGER_POINT, CMD_RESET,
    DESCRIBE, El, Report, Session, capture_screen, describe_foreground, drag_to_target, foreground, guard, is_visible,
    media_file, mouse_button, mouse_to, post, press, save_capture, send, type_unicode, unpack, wait_until, window_rect,
};
use windows::Win32::UI::WindowsAndMessaging::WM_CLOSE;
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

/// Waits (up to 10 s) until the Manager's rename or removal has been recorded.
pub(super) fn wait_file_op() -> bool {
    let done = wait_until(10_000, || !crate::manager::file_op_running());
    std::thread::sleep(Duration::from_millis(100));
    done
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
    r.check("Open manager closes the popup", wait_until(1000, || !is_visible(t.popup())), "");
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
    // The rename runs on a worker thread. The test makes it wait 1.5 s
    // before touching the disk; meanwhile the window must keep answering.
    let videos = t.out.join("app").join("Media").join("Videos");
    crate::manager::slow_next_file_op(1500);
    m.click(r, MHit::Rename);
    let asked = Instant::now();
    let answered = send(t.main, CMD_HOTKEY_OK, 0) >= 0;
    let took = asked.elapsed();
    let running = crate::manager::file_op_running();
    let d = m.describe();
    r.check(
        "while a rename runs, PocketMedia keeps answering (the rename is on a worker thread)",
        answered && running && took < Duration::from_millis(500) && videos.join("Race clip.mp4").is_file() && d.contains("Renaming to Renamed clip.mp4…"),
        &format!("answered in {} ms; rename still running {running}; file not renamed yet {}; {d}", took.as_millis(), videos.join("Race clip.mp4").is_file()),
    );
    let finished = wait_file_op();
    r.check("…and the rename finishes", finished, "");
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
    // A file Windows would not recycle (no Recycle Bin on the drive, larger than
    // the bin, or the bin set to delete right away) is refused, never deleted.
    // The self-test makes Windows' answer "no" for one file.
    let clip = media.join("Videos").join("Renamed clip.mp4");
    crate::fileops::refuse_next_for_test(true);
    m.click(r, MHit::Recycle);
    m.click(r, MHit::ConfirmMove);
    wait_file_op();
    crate::fileops::refuse_next_for_test(false);
    m.has(
        r,
        "a file Windows would delete permanently is kept, and the line says so",
        &["total=8", "confirming=false", "selected=\"Renamed clip.mp4\"", "Renamed clip.mp4 was not removed:", "delete it permanently"],
    );
    r.check("the refused file is still in the Media folder, unchanged", clip.is_file(), &clip.display().to_string());
    m.shot(r, "state-manager-recycle-refused");

    let bin_before = crate::fileops::recycle_bin_items(&media);
    m.click(r, MHit::Recycle);
    m.click(r, MHit::ConfirmMove);
    wait_file_op();
    m.has(
        r,
        "confirming moves it to the Recycle Bin and selects the next item",
        &["total=7", "selected=\"Hello.gif\"", "Renamed clip.mp4 was moved to the Recycle Bin."],
    );
    r.check("the file left the Media folder", !clip.exists(), "");
    let bin_after = crate::fileops::recycle_bin_items(&media);
    r.check(
        "the file is in the Recycle Bin (its item count went up)",
        matches!((bin_before, bin_after), (Some(a), Some(b)) if b > a),
        &format!("items before {bin_before:?}, after {bin_after:?}"),
    );

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

    if !keyboard_tests(r, t, &m) {
        return false;
    }
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
    wait_until(1500, || !is_visible(t.popup()));

    // × hides the Manager; the app keeps running in the tray.
    send(t.main, CMD_FOCUS, hwnd);
    wait_until(1500, || foreground() == hwnd);
    m.click(r, MHit::Close);
    r.check("× hides the Manager", wait_until(1500, || !is_visible(hwnd)), "");
    // The UI thread still answers; the hidden Manager lets go of its window
    // and is made again the next time it is opened.
    let released = wait_until(1500, || send(t.main, CMD_MANAGER, 0) == 0);
    r.check(
        "the app is still running after ×, and the hidden Manager let go of its window",
        released && send(t.main, CMD_HOTKEY_OK, 0) == 1,
        &format!("Manager window now {:#x} (was {hwnd:#x})", send(t.main, CMD_MANAGER, 0)),
    );
    true
}

/// Page Down and Page Up on 48 items, more than a page holds
/// (with 12 items a page held them all). Leaves the library reset.
pub(super) fn page_keys(r: &mut Report, t: &Session) -> bool {
    super::preview_test::close_windows(r, t);
    send(t.main, CMD_RESET, 0);
    let total = send(t.main, super::CMD_MANY_ITEMS, 40);
    if !t.open(r) || !t.click(r, El::OpenManager) {
        return false;
    }
    let hwnd = send(t.main, CMD_MANAGER, 0);
    if !wait_until(2000, || hwnd != 0 && is_visible(hwnd) && foreground() == hwnd) {
        return r.check("Page keys: the Manager opens", false, &describe_foreground());
    }
    let m = Mgr { t, hwnd };
    m.click(r, MHit::Tile(0));
    m.park();
    let d = m.describe();
    let names: Vec<String> = d.split("names=").nth(1).unwrap_or("").split('|').filter(|n| !n.is_empty()).map(String::from).collect();
    let page = d.split("page=").nth(1).and_then(|p| p.split_whitespace().next()).and_then(|p| p.parse::<usize>().ok()).unwrap_or(0);
    let n = names.len();
    // Fair only when two pages down is still short of the end.
    let fair = page >= 1 && 2 * page < n;
    r.check(
        "Page keys: the library holds more than two pages of items",
        fair && d.contains("focus=Gallery"),
        &format!("{n} items (library said {total}), a page is {page}; {}", d.chars().take(200).collect::<String>()),
    );
    if !fair {
        return true;
    }
    for (vk, want, name) in [
        (VK_NEXT, page, "Page Down moves the selection one page (the rows that fit) down"),
        (VK_NEXT, 2 * page, "…a second Page Down moves another page"),
        (VK_PRIOR, page, "Page Up moves it back up a page"),
    ] {
        if !guard(r, &t.allowed) {
            return false;
        }
        press(&[vk]);
        std::thread::sleep(Duration::from_millis(200));
        let d = m.describe();
        let got = selected(&d);
        r.check(
            &format!("Manager: {name}"),
            got == names[want] && d.contains("focus=Gallery"),
            &format!("selected {got:?}, wanted {:?} (item {want} of {n}, a page is {page} items)", names[want]),
        );
        if want == 2 * page {
            m.park();
            m.shot(r, "state-manager-page-down");
        }
    }
    post(hwnd, WM_CLOSE, 0, 0);
    wait_until(1500, || !is_visible(hwnd));
    send(t.main, CMD_RESET, 0);
    true
}

/// The `selected="…"` name in a Manager description.
fn selected(d: &str) -> String {
    d.split("selected=\"").nth(1).and_then(|s| s.split('"').next()).unwrap_or("").to_string()
}

/// The gallery keys Home, End, Page Down, Page Up, Delete (asks
/// first; Escape keeps the item) and F2 (renames, the whole name selected), on
/// the 12 items after the Manager's import. Nothing is renamed or removed.
fn keyboard_tests(r: &mut Report, t: &Session, m: &Mgr) -> bool {
    send(t.main, CMD_FOCUS, m.hwnd);
    wait_until(1500, || foreground() == m.hwnd);
    m.click(r, MHit::Tile(0));
    m.park();
    let d = m.describe();
    let names: Vec<String> = d.split("names=").nth(1).unwrap_or("").split('|').filter(|n| !n.is_empty()).map(String::from).collect();
    let page = d.split("page=").nth(1).and_then(|p| p.split_whitespace().next()).and_then(|p| p.parse::<usize>().ok()).unwrap_or(0);
    let n = names.len();
    if n < 2 || !d.contains("focus=Gallery") {
        return r.check("Manager keys: the gallery has the focus and at least 2 items", false, &d);
    }
    let key = |r: &mut Report, vk: VIRTUAL_KEY| -> Option<String> {
        if !guard(r, &t.allowed) {
            return None;
        }
        press(&[vk]);
        std::thread::sleep(Duration::from_millis(200));
        Some(m.describe())
    };
    // Page Down and Page Up are checked on a library bigger than a page (`page_keys`):
    // here a page holds all 12 items, so Page Down would land where End does.
    for (vk, want, name) in [(VK_END, n - 1, "End selects the last item"), (VK_HOME, 0, "Home selects the first item")] {
        let Some(d) = key(r, vk) else { return false };
        let got = selected(&d);
        r.check(
            &format!("Manager: {name}"),
            got == names[want] && d.contains("focus=Gallery"),
            &format!("selected {got:?}, wanted {:?} (item {want} of {n}, a page is {page} items)", names[want]),
        );
    }

    // Delete asks first, like the button; Escape keeps the item.
    let first = names[0].clone();
    let file = media_file(&t.out, &first);
    let Some(d) = key(r, VK_DELETE) else { return false };
    m.park();
    m.shot(r, "state-manager-delete-key-confirm");
    r.check(
        "Manager: Delete asks before moving the selected item to the Recycle Bin",
        d.contains("confirming=true") && d.contains(&format!("total={n}")) && selected(&d) == first,
        &d,
    );
    let Some(d) = key(r, VK_ESCAPE) else { return false };
    r.check(
        "…Escape answers no: the item and its file stay",
        d.contains("confirming=false") && d.contains(&format!("total={n}")) && file.is_file(),
        &format!("{} is a file: {}; {d}", file.display(), file.is_file()),
    );

    // F2 starts a rename with the whole name selected; Escape gives the name back.
    m.click(r, MHit::Tile(0));
    let base = first.rsplit_once('.').map_or(first.as_str(), |(b, _)| b).to_string();
    let Some(d) = key(r, VK_F2) else { return false };
    r.check("Manager: F2 starts renaming the selected item", d.contains("focus=RenameInput") && d.contains(&format!("rename={base:?}")), &d);
    if !guard(r, &t.allowed) {
        return false;
    }
    type_unicode("Keyboard name");
    std::thread::sleep(Duration::from_millis(200));
    let d = m.describe();
    r.check("…with the whole name selected: typing replaces it", d.contains("rename=\"Keyboard name\""), &d);
    let Some(d) = key(r, VK_ESCAPE) else { return false };
    r.check(
        "…and Escape gives the name back; nothing was renamed",
        d.contains(&format!("rename={base:?}")) && selected(&d) == first && file.is_file(),
        &d,
    );
    true
}

/// Like the quick popup: Ctrl+C on the selected item puts its
/// file on the clipboard, and a tile drags out to another app as a copy.
fn transfer_tests(r: &mut Report, t: &Session, m: &Mgr) {
    // The Manager's tiles show the full name when the pointer rests on them.
    send(t.main, super::CMD_FOCUS, m.hwnd);
    wait_until(1500, || foreground() == m.hwnd);
    if let Some((x, y)) = unpack(send(t.main, CMD_MANAGER_POINT, code(MHit::Tile(1)))) {
        mouse_to(x, y);
        let want = m.describe().split("names=").nth(1).and_then(|n| n.split('|').nth(1)).unwrap_or("").to_string();
        let shown = wait_until(3000, || m.describe().contains(&format!("tip={want:?}")));
        m.shot(r, "state-manager-tile-tooltip");
        r.check(&format!("resting the pointer on a Manager tile shows its full name ({want})"), shown && !want.is_empty(), &m.describe());
        m.park();
        let gone = wait_until(1500, || m.describe().contains("tip=\"\""));
        r.check("…and moving off the tile hides it (Manager)", gone, &m.describe());
    }
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

    // Files dropped on the Manager (its line says "Drop files or a
    // folder to add") go to Add media's review, like chosen files.
    let src = t.out.join("drop-in");
    let file = src.join("Dropped on the Manager.png");
    let _ = std::fs::create_dir_all(&src);
    if std::fs::write(&file, super::seed::png(40, 40, 222)).is_err() {
        r.check("drop test file made", false, &file.display().to_string());
        return;
    }
    let Some(w) = window_rect(m.hwnd) else { return };
    let to = (w.left + (w.right - w.left) / 2, w.top + (w.bottom - w.top) / 2);
    if super::drag_from_target(r, t, &file, to, m.hwnd) {
        let reviewed = wait_until(8000, || m.describe().contains("Dropped on the Manager.png:ok"));
        let result = std::fs::read_to_string(t.out.join("drag-from-result.txt")).unwrap_or_default();
        let d = m.describe();
        r.check(
            "files dropped on the Manager are checked for adding in its Add media, like chosen files",
            reviewed && d.contains("host=manager") && d.contains("stage=review"),
            &format!("drag: {result}; {d}"),
        );
        r.check("…the dropped file is still where it was (a copy is made only on Add)", file.is_file(), &file.display().to_string());
        m.shot(r, "state-manager-dropped");
    }
    // Back to the library without adding, so the counts after this stay as expected.
    send(t.main, super::CMD_FOCUS, m.hwnd);
    wait_until(1500, || foreground() == m.hwnd);
    for _ in 0..3 {
        if !m.describe().starts_with("host=manager") || !guard(r, &t.allowed) {
            break;
        }
        press(&[VK_ESCAPE]);
        std::thread::sleep(Duration::from_millis(200));
    }
}
