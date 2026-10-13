//! The quick popup window: input handling, show/hide and focus hand-back.
//! State lives in `PopupState` and the shared `library`, geometry in
//! `ui::layout`, drawing in `ui::paint`. Add media is an `AddPanel`.
//!
//! The items are fake (Phase 1-2). Clicking or pressing Enter on a tile runs the
//! Phase 0 paste of the test PNG, and dragging a tile drags the test PNG, so
//! the transfer risk test keeps working until Phase 4 replaces it.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ScreenToClient, ValidateRect};
use windows::Win32::System::Ole::DROPEFFECT_COPY;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    DragDetect, VIRTUAL_KEY, VK_A, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F4, VK_HOME,
    VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use crate::add_panel::{AddOutcome, AddPanel, Area};
use crate::library::{self, Library};
use crate::popup_state::{Focus, ListKind, PopupState, TypeFilter};
use crate::target::{self, NoTarget, Target};
use crate::ui::add_view::AddHit;
use crate::ui::d2d::Gfx;
use crate::ui::layout::{self, COLUMNS, Grid, Hit, HitView, Layout, TextStyle};
use crate::ui::paint::{self, Ui};
use crate::ui::widgets::TextMetrics;
use crate::winutil::{self, WM_MOUSELEAVE};
use crate::{SHORTCUT_LABEL, StatusKind, app, manager, paste, set_status, transfer};

const EMPTY_ACTIONS: [&str; 3] = ["Clear filters", "Show all media", "Add media"];
const WHEEL_STEP: f32 = 48.0;

struct View {
    state: PopupState,
    /// Add media panel, shown in place of the gallery while open.
    add: Option<AddPanel>,
    gfx: Gfx,
    layout: Option<Layout>,
    dpi: u32,
    hover: Hit,
    pressed: Hit,
    scroll_drag: Option<(f32, f32)>,
    high_surrogate: Option<u16>,
    tracking_leave: bool,
}

/// Work to do after the view borrow is released (these can re-enter the window procedure).
enum Effect {
    None,
    Close(bool),
    Activate,
    TileDown(POINT),
    PasteText,
    Capture(bool),
    OpenManager,
    /// Activate a control once no state is borrowed (keyboard Enter/Space).
    Deferred(Hit),
    /// Show the Add media file or folder picker (modal).
    Pick(crate::dialogs::Pick),
}

thread_local! {
    static POPUP: Cell<HWND> = Cell::new(HWND::default());
    static VIEW: RefCell<Option<View>> = const { RefCell::new(None) };
    static DRAGGING: Cell<bool> = const { Cell::new(false) };
    /// A picker is open: losing activation to it must not close the popup.
    static MODAL: Cell<bool> = const { Cell::new(false) };
    /// Close after inserting is off and a paste is handing focus to the target:
    /// the popup stays on screen.
    static KEEP_OPEN: Cell<bool> = const { Cell::new(false) };
    static POSITIONING: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` with the view. Returns None if the view is missing or already in use
/// (a re-entrant message), so a nested message can never panic on the borrow.
fn with_view<R>(f: impl FnOnce(&mut View) -> R) -> Option<R> {
    VIEW.with(|v| v.try_borrow_mut().ok().and_then(|mut v| v.as_mut().map(f)))
}

pub fn hwnd() -> HWND {
    POPUP.with(|c| c.get())
}

pub fn create(instance: HINSTANCE) -> Result<(), String> {
    let gfx = Gfx::new().map_err(|e| format!("Direct2D/DirectWrite is not available: {e}"))?;
    // SAFETY: standard class registration and window creation; pointers are to statics.
    let hwnd = unsafe {
        let class = WNDCLASSW {
            lpfnWndProc: Some(popup_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("PocketMediaPopup"),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("Could not register the popup window class.".into());
        }
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("PocketMediaPopup"),
            w!("PocketMedia"),
            WS_POPUP,
            0,
            0,
            layout::WIDTH as i32,
            layout::HEIGHT as i32,
            None,
            None,
            Some(instance),
            None,
        )
        .map_err(|e| format!("Could not create the popup: {e}"))?
    };
    POPUP.with(|c| c.set(hwnd));
    winutil::style_frame(hwnd);
    VIEW.with(|v| {
        *v.borrow_mut() = Some(View {
            state: PopupState::new(),
            add: None,
            gfx,
            layout: None,
            dpi: 96,
            hover: Hit::None,
            pressed: Hit::None,
            scroll_drag: None,
            high_surrogate: None,
            tracking_leave: false,
        })
    });
    Ok(())
}

pub fn destroy() {
    VIEW.with(|v| v.borrow_mut().take());
    let hwnd = POPUP.with(|c| c.replace(HWND::default()));
    if !hwnd.is_invalid() {
        // SAFETY: our own window, destroyed once.
        let _ = unsafe { DestroyWindow(hwnd) };
    }
}

pub fn is_visible() -> bool {
    // SAFETY: tolerates any HWND.
    unsafe { IsWindowVisible(hwnd()) }.as_bool()
}

fn invalidate() {
    winutil::invalidate(hwnd());
}

pub fn toggle_from_hotkey() {
    // SAFETY: no preconditions.
    let focused = unsafe { GetForegroundWindow() } == hwnd();
    if is_visible() && focused {
        close(true);
    } else {
        // Capture first: once shown, the popup is the foreground window.
        open(target::capture_foreground());
    }
}

pub fn open(captured: Result<Target, NoTarget>) {
    paste::cancel();
    app(|a| *a.target.borrow_mut() = captured);
    with_view(|v| {
        v.state.reset_for_open();
        // The shortcut always opens the quick picker, never a half-finished
        // review; an import that is still copying stays on screen.
        if !v.add.as_ref().is_some_and(AddPanel::busy) {
            v.add = None;
        }
    });
    position_near_cursor();
    let h = hwnd();
    // SAFETY: our own window.
    unsafe {
        let _ = ShowWindow(h, SW_SHOW);
        let _ = SetForegroundWindow(h);
    }
    invalidate();
}

/// A short line in the popup's footer, in place of the hint (e.g. "Copied to clipboard").
pub fn show_status(text: &str) {
    with_view(|v| v.state.status = Some(text.into()));
    invalidate();
}

/// The item under the gallery selection (the one Enter or a click acts on).
fn selected_item_id() -> Option<u32> {
    with_view(|v| library::with(|lib| v.state.selected_index(lib).and_then(|i| v.state.visible(lib).get(i).map(|it| it.id))))
        .flatten()
}

/// Keep the popup on screen while a paste hands focus away (Close after inserting off).
pub fn set_keep_open(on: bool) {
    KEEP_OPEN.with(|k| k.set(on));
}

/// Shows the popup again without taking focus, so the target app keeps it.
fn show_without_focus() {
    let h = hwnd();
    // SAFETY: our own window.
    let _ = unsafe { ShowWindow(h, SW_SHOWNOACTIVATE) };
    invalidate();
}

/// A paste did not go through: the popup comes back (without focus) and says so,
/// so the item can be dragged instead.
pub fn show_failed(text: &str) {
    show_status(text);
    show_without_focus();
}

/// Repaint after the library changed elsewhere (history after a paste).
pub fn refresh() {
    invalidate();
}

/// A worker result. Returns true if the popup's Add media panel owned it.
pub fn on_job(ev: &crate::jobs::Event) -> bool {
    let effect = with_view(|v| {
        if !v.add.as_ref().is_some_and(|a| a.owns(ev)) {
            return None;
        }
        let a = v.add.as_mut()?;
        let outcome = a.on_job(ev);
        Some(v.add_outcome(outcome))
    })
    .flatten();
    let handled = effect.is_some();
    if let Some(effect) = effect {
        run(effect);
    }
    handled
}

/// Hides the popup. With `restore_focus`, focus goes back to the remembered app.
pub fn close(restore_focus: bool) {
    if !is_visible() {
        return;
    }
    with_view(|v| {
        v.state.dropdown = None;
        if let Some(add) = v.add.as_mut() {
            add.clear_hover();
        }
        v.hover = Hit::None;
        v.pressed = Hit::None;
        v.scroll_drag = None;
    });
    if restore_focus
        && let Ok(t) = app(|a| a.target.borrow().clone())
        && t.still_exists()
    {
        // SAFETY: SetForegroundWindow tolerates any HWND.
        let _ = unsafe { SetForegroundWindow(t.hwnd) };
    }
    // SAFETY: our own window.
    let _ = unsafe { ShowWindow(hwnd(), SW_HIDE) };
}

/// Self-test only: back to the state after start-up (the library is reset separately).
pub fn reset() {
    with_view(|v| {
        v.state = PopupState::new();
        v.add = None;
    });
}

fn position_near_cursor() {
    let at = winutil::monitor_at_cursor();
    POSITIONING.with(|p| p.set(true));
    place(at.cursor, at.work, at.dpi);
    // SAFETY: our own window.
    let actual = unsafe { GetDpiForWindow(hwnd()) };
    if actual != 0 && actual != at.dpi {
        place(at.cursor, at.work, actual);
    }
    POSITIONING.with(|p| p.set(false));
}

/// Fixed size (520x620 DIPs), shrunk only if the monitor's work area is smaller.
fn place(pt: POINT, work: RECT, dpi: u32) {
    let px = |dip: f32| (dip * dpi as f32 / 96.0).round() as i32;
    let w = px(layout::WIDTH).min(work.right - work.left);
    let h = px(layout::HEIGHT).min(work.bottom - work.top - px(16.0));
    let gap = px(12.0);
    let mut x = pt.x - w / 2;
    let mut y = pt.y + gap;
    if y + h > work.bottom {
        y = pt.y - h - gap;
    }
    x = x.clamp(work.left, (work.right - w).max(work.left));
    y = y.clamp(work.top, (work.bottom - h).max(work.top));
    with_view(|v| {
        if v.dpi != dpi {
            v.gfx.reset_target();
        }
        v.dpi = dpi;
        v.layout = None;
    });
    // SAFETY: our own window; plain integers.
    let _ = unsafe { SetWindowPos(hwnd(), Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE) };
}

// ---------------------------------------------------------------- view helpers

impl View {
    fn ensure_layout(&mut self) -> &Layout {
        if self.layout.is_none() {
            let (w, h) = winutil::client_size_dip(hwnd(), self.dpi);
            let gfx = &self.gfx;
            self.layout = Some(Layout::compute(w, h, &|t, s| gfx.measure(t, s), SHORTCUT_LABEL, &EMPTY_ACTIONS));
        }
        self.layout.as_ref().expect("layout was just computed")
    }

    fn area(&mut self) -> Area {
        let l = self.ensure_layout();
        Area { w: l.width, h: l.height }
    }

    fn hit(&mut self, x: f32, y: f32) -> Hit {
        let view = library::with(|lib| HitView {
            tiles: self.state.visible(lib).len(),
            scroll: self.state.scroll,
            dropdown_open: self.state.dropdown.is_some(),
            search_has_text: !self.state.search.is_empty(),
            empty_action: self.state.empty(lib).map(|e| e.action()),
        });
        self.ensure_layout().hit(x, y, &view)
    }

    fn clamp_scroll(&mut self) {
        let n = library::with(|lib| self.state.visible(lib).len());
        let max = self.ensure_layout().grid().max_scroll(n);
        self.state.scroll = self.state.scroll.clamp(0.0, max);
    }

    fn search_metrics(&mut self) -> TextMetrics {
        let b = paint::search_box(self.ensure_layout());
        TextMetrics::for_search(&self.gfx, &self.state.search, &b)
    }

    fn paint(&mut self) {
        let area = self.area();
        if let Some(add) = self.add.as_ref() {
            add.paint(&mut self.gfx, hwnd(), self.dpi as f32, area);
            return;
        }
        let ui = Ui {
            hover: self.hover,
            dpi: self.dpi as f32,
            shortcut_ok: app(|a| a.hotkey_ok.get()),
            shortcut_label: SHORTCUT_LABEL,
            search: self.search_metrics(),
            plus_w: self.gfx.measure("+", TextStyle::Body),
        };
        let View { state, gfx, layout, dpi, .. } = self;
        if let Some(layout) = layout.as_ref() {
            library::with(|lib| {
                let _ = gfx.draw(hwnd(), *dpi as f32, |p| paint::paint(p, state, lib, layout, &ui));
            });
        }
    }

    /// Click or Enter/Space on a control.
    fn activate(&mut self, hit: Hit) -> Effect {
        let s = &mut self.state;
        match hit {
            Hit::Close => return Effect::Close(true),
            Hit::OpenManager => return Effect::OpenManager,
            Hit::AddMedia => self.add = Some(AddPanel::new()),
            Hit::Chip(i) => s.set_filter(TypeFilter::ORDER[i]),
            Hit::List => s.dropdown = if s.dropdown.is_some() { None } else { Some(list_index(s.list)) },
            Hit::DropdownRow(i) => {
                s.set_list(ListKind::BOTH[i]);
                s.focus = Focus::List;
            }
            Hit::ClearHistory => {
                let cleared = library::with_mut(|lib| {
                    let on = s.history_enabled(lib);
                    if on {
                        s.clear_history(lib);
                    }
                    on
                });
                if cleared && let Err(e) = library::persist() {
                    s.status = Some(format!("History cleared, but {e}."));
                }
            }
            Hit::EmptyAction => library::with(|lib| s.run_empty_action(lib)),
            Hit::Tile(_) => return Effect::Activate,
            Hit::SearchClear => {
                s.search.clear();
                s.reset_view();
                s.focus = Focus::Search;
            }
            _ => {}
        }
        Effect::None
    }

    fn focus_for(hit: Hit) -> Option<Focus> {
        Some(match hit {
            Hit::Close => Focus::Close,
            Hit::OpenManager => Focus::OpenManager,
            Hit::AddMedia => Focus::AddMedia,
            Hit::Search | Hit::SearchClear => Focus::Search,
            Hit::Chip(i) => Focus::Type(i),
            Hit::List | Hit::DropdownRow(_) => Focus::List,
            Hit::Tile(_) => Focus::Gallery,
            Hit::EmptyAction => Focus::EmptyAction,
            Hit::ClearHistory => Focus::ClearHistory,
            Hit::None | Hit::Scrollbar => return None,
        })
    }

    /// Turns what the Add media panel asks for into popup effects.
    fn add_outcome(&mut self, outcome: AddOutcome) -> Effect {
        match outcome {
            AddOutcome::None => Effect::None,
            AddOutcome::CloseWindow => Effect::Close(true),
            AddOutcome::Leave => {
                self.add = None;
                self.state.focus = Focus::AddMedia;
                Effect::Capture(false)
            }
            AddOutcome::Imported(status) => {
                self.add = None;
                self.state.show_imported(status);
                Effect::Capture(false)
            }
            AddOutcome::Capture(on) => Effect::Capture(on),
            AddOutcome::PasteText => Effect::PasteText,
            AddOutcome::Pick(kind) => Effect::Pick(kind),
        }
    }

    fn mouse_down(&mut self, x: f32, y: f32, screen: POINT) -> Effect {
        let area = self.area();
        if let Some(add) = self.add.as_mut() {
            let outcome = add.mouse_down(&self.gfx, area, x, y);
            return self.add_outcome(outcome);
        }
        let hit = self.hit(x, y);
        if self.state.dropdown.is_some() && !matches!(hit, Hit::DropdownRow(_) | Hit::List) {
            self.state.dropdown = None; // a click outside only closes the list
            return Effect::None;
        }
        self.state.focus_visible = false;
        if let Some(f) = Self::focus_for(hit) {
            self.state.focus = f;
        }
        match hit {
            Hit::Search => {
                let input_x = self.ensure_layout().search_input.x + 2.0;
                let m = self.search_metrics();
                let pos = self.gfx.position_at(self.state.search.text(), TextStyle::Body, x - input_x + m.offset);
                self.state.search.set_caret_utf16(pos);
                Effect::None
            }
            Hit::Tile(i) => {
                self.state.selected = i;
                Effect::TileDown(screen)
            }
            Hit::Scrollbar => {
                self.scroll_drag = Some((y, self.state.scroll));
                Effect::Capture(true)
            }
            Hit::None => Effect::None,
            other => {
                self.pressed = other;
                Effect::Capture(true)
            }
        }
    }

    fn mouse_up(&mut self, x: f32, y: f32) -> Effect {
        let area = self.area();
        if let Some(add) = self.add.as_mut() {
            let outcome = add.mouse_up(&self.gfx, area, x, y);
            return self.add_outcome(outcome);
        }
        if self.scroll_drag.take().is_some() {
            return Effect::Capture(false);
        }
        let pressed = std::mem::replace(&mut self.pressed, Hit::None);
        if pressed == Hit::None {
            return Effect::None;
        }
        winutil::release_capture();
        if self.hit(x, y) == pressed { self.activate(pressed) } else { Effect::None }
    }

    fn mouse_move(&mut self, x: f32, y: f32) {
        let area = self.area();
        if let Some(add) = self.add.as_mut() {
            return add.mouse_move(&self.gfx, area, x, y);
        }
        if let Some((start_y, start_scroll)) = self.scroll_drag {
            let n = library::with(|lib| self.state.visible(lib).len());
            let l = self.ensure_layout();
            let content = l.grid().content_height(n);
            let ratio = if l.gallery.h > 0.0 { content / l.gallery.h } else { 1.0 };
            self.state.scroll = start_scroll + (y - start_y) * ratio;
            self.clamp_scroll();
        }
        self.hover = self.hit(x, y);
    }

    fn wheel(&mut self, delta: i16) {
        let area = self.area();
        if let Some(add) = self.add.as_mut() {
            return add.wheel(&self.gfx, area, delta);
        }
        self.state.dropdown = None;
        self.state.scroll -= delta as f32 / 120.0 * WHEEL_STEP;
        self.clamp_scroll();
    }

    fn key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool, alt: bool) -> Effect {
        let area = self.area();
        if let Some(add) = self.add.as_mut() {
            let outcome = add.key(&self.gfx, area, vk, ctrl, shift);
            return self.add_outcome(outcome);
        }
        let grid = self.ensure_layout().grid();
        library::with(|lib| self.key_gallery(lib, grid, vk, ctrl, shift, alt))
    }

    /// Keys for the gallery screen. Runs inside a library read, so it must not
    /// change the library: actions that do are returned as `Effect::Deferred`.
    fn key_gallery(&mut self, lib: &Library, grid: Grid, vk: VIRTUAL_KEY, ctrl: bool, shift: bool, alt: bool) -> Effect {
        let page = grid.page();
        let s = &mut self.state;
        if let Some(row) = s.dropdown {
            match vk {
                VK_UP | VK_DOWN => s.dropdown = Some(if vk == VK_UP { 0 } else { 1 }),
                VK_RETURN | VK_SPACE => s.set_list(ListKind::BOTH[row]),
                VK_ESCAPE => s.dropdown = None,
                VK_TAB => s.tab(lib, shift),
                _ => {}
            }
            return Effect::None;
        }
        match vk {
            VK_ESCAPE => return Effect::Close(true),
            VK_TAB => {
                s.tab(lib, shift);
                return Effect::None;
            }
            _ => {}
        }
        match s.focus {
            Focus::Search => match vk {
                VK_LEFT => s.search.left(),
                VK_RIGHT => s.search.right(),
                VK_HOME => s.search.home(),
                VK_END => s.search.end(),
                VK_BACK if ctrl => {
                    s.search.delete_word_back();
                    s.reset_view();
                }
                VK_BACK => {
                    s.search.backspace();
                    s.reset_view();
                }
                VK_DELETE => {
                    s.search.delete();
                    s.reset_view();
                }
                VK_A if ctrl => s.search.select_all(),
                VK_V if ctrl => return Effect::PasteText,
                VK_DOWN if s.empty(lib).is_none() => {
                    s.focus = Focus::Gallery;
                    s.focus_visible = true;
                }
                VK_RETURN => return s.selected_index(lib).map_or(Effect::None, |_| Effect::Activate),
                _ => {}
            },
            Focus::Gallery => {
                let cols = COLUMNS as isize;
                match vk {
                    VK_LEFT => s.move_selection(lib, -1),
                    VK_RIGHT => s.move_selection(lib, 1),
                    VK_DOWN => s.move_selection(lib, cols),
                    VK_UP if s.selected_index(lib).unwrap_or(0) < COLUMNS => s.focus = Focus::Search,
                    VK_UP => s.move_selection(lib, -cols),
                    VK_HOME => s.move_selection(lib, -10_000),
                    VK_END => s.move_selection(lib, 10_000),
                    VK_NEXT => s.move_selection(lib, page),
                    VK_PRIOR => s.move_selection(lib, -page),
                    VK_RETURN | VK_SPACE => return s.selected_index(lib).map_or(Effect::None, |_| Effect::Activate),
                    _ => return Effect::None,
                }
                s.focus_visible = true;
                if let Some(i) = s.selected_index(lib) {
                    s.scroll = grid.scroll_to_show(i, s.scroll);
                }
            }
            Focus::Type(i) => match vk {
                VK_LEFT if i > 0 => s.focus = Focus::Type(i - 1),
                VK_RIGHT if i + 1 < TypeFilter::ORDER.len() => s.focus = Focus::Type(i + 1),
                VK_RETURN | VK_SPACE => s.set_filter(TypeFilter::ORDER[i]),
                _ => {}
            },
            Focus::List => match vk {
                VK_DOWN if alt => s.dropdown = Some(list_index(s.list)),
                VK_F4 | VK_RETURN | VK_SPACE => s.dropdown = Some(list_index(s.list)),
                VK_UP => s.set_list(ListKind::Recent),
                VK_DOWN => s.set_list(ListKind::All),
                _ => {}
            },
            Focus::Close => {
                if matches!(vk, VK_RETURN | VK_SPACE) {
                    return Effect::Close(true);
                }
            }
            Focus::ClearHistory | Focus::EmptyAction | Focus::OpenManager | Focus::AddMedia => {
                if matches!(vk, VK_RETURN | VK_SPACE) {
                    let hit = match s.focus {
                        Focus::ClearHistory => Hit::ClearHistory,
                        Focus::EmptyAction => Hit::EmptyAction,
                        Focus::OpenManager => Hit::OpenManager,
                        _ => Hit::AddMedia,
                    };
                    return Effect::Deferred(hit);
                }
            }
        }
        Effect::None
    }

    fn search_edit(&mut self, f: impl FnOnce(&mut crate::text_field::TextField)) {
        f(&mut self.state.search);
        self.state.reset_view();
    }

    fn insert_text(&mut self, text: &str) {
        if let Some(add) = self.add.as_mut() {
            return add.insert(text);
        }
        // Typing while the gallery has focus goes to the search box.
        if matches!(self.state.focus, Focus::Search | Focus::Gallery) && self.state.dropdown.is_none() {
            self.state.focus = Focus::Search;
            self.search_edit(|f| f.insert(text));
        }
    }

    fn char_input(&mut self, unit: u16) {
        let text = match unit {
            0xD800..=0xDBFF => {
                self.high_surrogate = Some(unit);
                return;
            }
            0xDC00..=0xDFFF => match self.high_surrogate.take() {
                Some(high) => String::from_utf16_lossy(&[high, unit]),
                None => return,
            },
            u if u < 0x20 || u == 0x7F => return, // control keys are handled in WM_KEYDOWN
            u => String::from_utf16_lossy(&[u]),
        };
        self.insert_text(&text);
    }
}

fn list_index(list: ListKind) -> usize {
    ListKind::BOTH.iter().position(|l| *l == list).unwrap_or(0)
}

fn run(effect: Effect) {
    match effect {
        Effect::None => {}
        Effect::Close(restore) => close(restore),
        Effect::Activate => {
            if let Some(id) = selected_item_id() {
                paste::begin(id);
            }
        }
        Effect::TileDown(pt) => {
            invalidate();
            // DragDetect runs its own loop until the pointer passes the system drag
            // threshold (drag) or the button is released (click).
            // SAFETY: our own window; pt is in screen coordinates.
            if unsafe { DragDetect(hwnd(), pt) }.as_bool() {
                drag_out();
            } else {
                run(Effect::Activate);
            }
        }
        Effect::PasteText => {
            if let Some(text) = winutil::clipboard_text(hwnd()) {
                with_view(|v| v.insert_text(&text));
            }
        }
        Effect::Capture(true) => winutil::set_capture(hwnd()),
        Effect::Capture(false) => winutil::release_capture(),
        Effect::OpenManager => {
            close(false);
            manager::show();
        }
        Effect::Pick(kind) => {
            MODAL.with(|m| m.set(true));
            let source = crate::dialogs::pick(hwnd(), kind);
            MODAL.with(|m| m.set(false));
            // SAFETY: our own window; back in front after the picker.
            let _ = unsafe { SetForegroundWindow(hwnd()) };
            let effect = with_view(|v| {
                let outcome = v.add.as_mut().map_or(AddOutcome::None, |a| a.picked(source));
                v.add_outcome(outcome)
            });
            if let Some(effect) = effect {
                run(effect);
            }
        }
        Effect::Deferred(hit) => {
            if let Some(effect) = with_view(|v| v.activate(hit)) {
                run(effect);
            }
        }
    }
    invalidate();
}

/// Phase 1 stand-in: drags the Phase 0 test PNG (fake items have no files).
fn drag_out() {
    paste::cancel();
    let Some(id) = selected_item_id() else { return };
    let file = library::with(|lib| lib.get(id).map(|i| i.path.clone()))
        .and_then(|rel| crate::paths::get().map(|p| crate::import::media_file(&p.media, &rel)));
    let Some(file) = file else { return };
    DRAGGING.with(|d| d.set(true));
    // Copy only: the shell's data object for the file, with DROPEFFECT_COPY alone.
    let result = transfer::drag_file_out(hwnd(), &file);
    DRAGGING.with(|d| d.set(false));
    match result {
        Ok(effect) if effect.0 & DROPEFFECT_COPY.0 != 0 => {
            set_status("Drag: the target accepted a copy.", StatusKind::Good);
            // History: recorded when a copy-drag completes.
            library::with_mut(|lib| lib.record_use(id));
            let _ = library::persist();
        }
        Ok(_) => set_status("Drag ended without a drop.", StatusKind::Neutral),
        Err(e) => set_status(e, StatusKind::Bad),
    }
}

extern "system" fn popup_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let effect = match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            with_view(|v| v.paint());
            // SAFETY: our own window; marks it painted.
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            return LRESULT(0);
        }
        WM_SIZE => {
            with_view(|v| v.layout = None);
            Effect::None
        }
        WM_NCHITTEST => {
            // The title bar (outside ×) moves the popup, like a normal caption.
            let mut pt = winutil::lparam_point(lparam);
            // SAFETY: our own window; pt is a local.
            let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
            let caption = with_view(|v| {
                let (x, y) = winutil::to_dip(pt, v.dpi);
                let l = v.ensure_layout();
                let (bar, close, w) = (l.titlebar, l.close, l.width);
                let add_close = v.add.is_some() && x > w - 48.0;
                bar.contains(x, y) && !close.contains(x, y) && !add_close
            });
            return LRESULT(if caption == Some(true) { HTCAPTION as isize } else { HTCLIENT as isize });
        }
        WM_MOUSEMOVE => {
            with_view(|v| {
                if !v.tracking_leave {
                    v.tracking_leave = winutil::track_mouse_leave(hwnd);
                }
                let (x, y) = winutil::to_dip(winutil::lparam_point(lparam), v.dpi);
                v.mouse_move(x, y);
            });
            Effect::None
        }
        WM_MOUSELEAVE => {
            with_view(|v| {
                v.tracking_leave = false;
                v.hover = Hit::None;
                if let Some(add) = v.add.as_mut() {
                    add.hover = AddHit::None;
                }
            });
            Effect::None
        }
        WM_LBUTTONDOWN => {
            let pt = winutil::lparam_point(lparam);
            let screen = winutil::client_to_screen(hwnd, pt);
            with_view(|v| {
                let (x, y) = winutil::to_dip(pt, v.dpi);
                v.mouse_down(x, y, screen)
            })
            .unwrap_or(Effect::None)
        }
        WM_LBUTTONUP => with_view(|v| {
            let (x, y) = winutil::to_dip(winutil::lparam_point(lparam), v.dpi);
            v.mouse_up(x, y)
        })
        .unwrap_or(Effect::None),
        WM_MOUSEWHEEL => {
            with_view(|v| {
                v.wheel(((wparam.0 >> 16) & 0xFFFF) as i16);
                if let Some((x, y)) = winutil::cursor_dip(hwnd, v.dpi) {
                    v.mouse_move(x, y);
                }
            });
            Effect::None
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = VIRTUAL_KEY(wparam.0 as u16);
            let alt = msg == WM_SYSKEYDOWN;
            if alt && vk != VK_DOWN {
                // SAFETY: forwarding unchanged (keeps Alt+F4 etc.).
                return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
            }
            let (ctrl, shift) = (winutil::key_down(VK_CONTROL), winutil::key_down(VK_SHIFT));
            with_view(|v| v.key(vk, ctrl, shift, alt)).unwrap_or(Effect::None)
        }
        WM_CHAR => {
            with_view(|v| v.char_input(wparam.0 as u16));
            Effect::None
        }
        WM_SETCURSOR if (lparam.0 & 0xFFFF) as u32 == HTCLIENT => {
            let beam = with_view(|v| match v.add.as_ref() {
                Some(add) => add.text_hover(),
                None => v.hover == Hit::Search,
            })
            .unwrap_or(false);
            winutil::set_cursor(beam);
            return LRESULT(1);
        }
        WM_ACTIVATE => {
            // Clicking outside closes the popup, except while dragging out of it.
            let inactive = (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE;
            let held = DRAGGING.with(|d| d.get()) || MODAL.with(|m| m.get()) || KEEP_OPEN.with(|k| k.get());
            if inactive && is_visible() && !held {
                Effect::Close(false)
            } else {
                Effect::None
            }
        }
        WM_DPICHANGED => {
            with_view(|v| {
                v.dpi = ((wparam.0 >> 16) & 0xFFFF) as u32;
                v.gfx.reset_target();
                v.layout = None;
            });
            if !POSITIONING.with(|p| p.get()) {
                // SAFETY: Windows passes a valid RECT pointer with WM_DPICHANGED.
                let r = unsafe { &*(lparam.0 as *const RECT) };
                // SAFETY: our own window.
                let _ = unsafe { SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE) };
            }
            Effect::None
        }
        WM_CLOSE => Effect::Close(true),
        // SAFETY: forwarding the unchanged message.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    };
    run(effect);
    LRESULT(0)
}

// ---------------------------------------------------------------- self-test hooks

/// A popup element the self-test can point at.
#[derive(Clone, Copy, Debug)]
pub enum TestTarget {
    Gallery(Hit),
    Add(AddHit),
}

/// Screen-space centre of a popup element, for the self-test to click.
pub fn element_center(target: TestTarget) -> Option<POINT> {
    let h = hwnd();
    with_view(|v| {
        let area = v.area();
        let element = match target {
            TestTarget::Gallery(hit) if v.add.is_none() => hit,
            TestTarget::Add(hit) => {
                let r = v.add.as_ref()?.element(&v.gfx, area, hit)?;
                return Some(winutil::rect_center_on_screen(h, v.dpi, r));
            }
            _ => return None,
        };
        let (n, action) = library::with(|lib| (v.state.visible(lib).len(), v.state.empty(lib).map(|e| e.action())));
        let scroll = v.state.scroll;
        let l = v.ensure_layout();
        let r = match element {
            Hit::Close => l.close,
            Hit::OpenManager => l.open_manager,
            Hit::AddMedia => l.add_media,
            Hit::Search => l.search,
            Hit::SearchClear => l.search_clear,
            Hit::Chip(i) => *l.chips.get(i)?,
            Hit::List => l.list,
            Hit::DropdownRow(i) => *l.dropdown_rows.get(i)?,
            Hit::Tile(i) if i < n => l.grid().tile(i, scroll),
            Hit::ClearHistory => l.clear_history,
            Hit::EmptyAction => l.empty_parts(action?).2,
            // A point in the gap between the first two columns: over the gallery, not a tile.
            Hit::Scrollbar => layout::Rect::new(l.grid().tile(0, 0.0).right() + 1.0, l.gallery.y + l.gallery.h / 2.0, 10.0, 1.0),
            // A neutral point: the empty middle of the title bar.
            Hit::None => layout::Rect::new(l.title.right() + 20.0, l.titlebar.y + 10.0, 1.0, 1.0),
            _ => return None,
        };
        Some(winutil::rect_center_on_screen(h, v.dpi, r))
    })
    .flatten()
}

/// "screen=gallery list=… filter=… count=… names=a|b|c" for self-test checks.
pub fn describe_state() -> String {
    with_view(|v| {
        if let Some(add) = v.add.as_ref() {
            return add.describe();
        }
        let s = &v.state;
        library::with(|lib| {
            let names: Vec<&str> = s.visible(lib).iter().map(|i| i.name.as_str()).collect();
            format!(
                "screen=gallery status={:?} list={:?} filter={:?} query={:?} focus={:?} count={} names={}",
                s.status.as_deref().unwrap_or(""),
                s.list,
                s.filter,
                s.search.text(),
                s.focus,
                names.len(),
                names.join("|")
            )
        })
    })
    .unwrap_or_default()
}
