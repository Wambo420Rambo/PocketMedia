//! The Manager window: library browsing, rename and removal (fake data).
//! State lives in `ManagerState` and the shared `library`, geometry and drawing
//! in `ui::manager_view`. Add media is an `AddPanel`, as in the popup.
//!
//! Created the first time it is shown. × hides it; the tray and shortcut stay.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ScreenToClient, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_A, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F2, VK_F4, VK_HOME, VK_LEFT,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use crate::add_panel::{AddOutcome, AddPanel, Area};
use crate::library::{self, Library, extension};
use crate::manager_state::{MFocus, ManagerState, Nav, Sort};
use crate::text_field::TextField;
use crate::ui::add_view::AddHit;
use crate::ui::d2d::Gfx;
use crate::ui::layout::{COLUMNS, Rect, TextStyle};
use crate::ui::manager_view::{self, MHit, ManagerLayout, ManagerUi};
use crate::ui::widgets::TextMetrics;
use crate::winutil::{self, WM_MOUSELEAVE};

const WHEEL_STEP: f32 = 48.0;
const OPEN_FOLDER_NOTE: &str = "Open folder needs real files, which arrive in Phase 3. These items are fake.";
const SETTINGS_NOTE: &str = "Settings arrives in Phase 2c.";

struct View {
    state: ManagerState,
    /// Add media panel, shown in place of the library while open.
    add: Option<AddPanel>,
    gfx: Gfx,
    dpi: u32,
    hover: MHit,
    pressed: MHit,
    scroll_drag: Option<(f32, f32)>,
    high_surrogate: Option<u16>,
    tracking_leave: bool,
}

/// Work to do after the view borrow is released (these can re-enter the window procedure).
enum Effect {
    None,
    Hide,
    Capture(bool),
    PasteText,
    /// Activate a control once no state is borrowed (keyboard Enter/Space).
    Deferred(MHit),
}

thread_local! {
    static WINDOW: Cell<HWND> = Cell::new(HWND::default());
    static VIEW: RefCell<Option<View>> = const { RefCell::new(None) };
    static POSITIONING: Cell<bool> = const { Cell::new(false) };
}

/// Runs `f` with the view. Returns None if the window does not exist yet or the
/// view is already in use (a re-entrant message), so it can never panic.
fn with_view<R>(f: impl FnOnce(&mut View) -> R) -> Option<R> {
    VIEW.with(|v| v.try_borrow_mut().ok().and_then(|mut v| v.as_mut().map(f)))
}

pub fn hwnd() -> HWND {
    WINDOW.with(|c| c.get())
}

pub fn is_visible() -> bool {
    let h = hwnd();
    // SAFETY: tolerates any HWND.
    !h.is_invalid() && unsafe { IsWindowVisible(h) }.as_bool()
}

fn create() -> Result<(), String> {
    let gfx = Gfx::new().map_err(|e| format!("Direct2D/DirectWrite is not available: {e}"))?;
    // SAFETY: standard class registration and window creation; pointers are to statics.
    let hwnd = unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None).map_err(|e| e.to_string())?.into();
        let class = WNDCLASSW {
            lpfnWndProc: Some(manager_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("PocketMediaManager"),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err("Could not register the Manager window class.".into());
        }
        CreateWindowExW(
            WS_EX_APPWINDOW,
            w!("PocketMediaManager"),
            w!("PocketMedia - Library"),
            WS_POPUP | WS_SYSMENU | WS_MINIMIZEBOX,
            0,
            0,
            manager_view::WIDTH as i32,
            manager_view::HEIGHT as i32,
            None,
            None,
            Some(instance),
            None,
        )
        .map_err(|e| format!("Could not create the Manager window: {e}"))?
    };
    WINDOW.with(|c| c.set(hwnd));
    winutil::style_frame(hwnd);
    let mut state = ManagerState::new();
    library::with(|lib| state.sync(lib));
    VIEW.with(|v| {
        *v.borrow_mut() = Some(View {
            state,
            add: None,
            gfx,
            dpi: 96,
            hover: MHit::None,
            pressed: MHit::None,
            scroll_drag: None,
            high_surrogate: None,
            tracking_leave: false,
        })
    });
    Ok(())
}

/// Shows the Manager (creating it the first time), centred on the pointer's monitor.
pub fn show() {
    if hwnd().is_invalid()
        && let Err(e) = create()
    {
        crate::message_box(&e, "PocketMedia - Manager", MB_ICONERROR);
        return;
    }
    let was_visible = is_visible();
    // The library may have changed in the popup since the Manager was last shown.
    with_view(|v| library::with(|lib| v.state.sync(lib)));
    if !was_visible {
        center_on_cursor_monitor();
    }
    let h = hwnd();
    // SAFETY: our own window.
    unsafe {
        let _ = ShowWindow(h, if IsIconic(h).as_bool() { SW_RESTORE } else { SW_SHOW });
        let _ = SetForegroundWindow(h);
    }
    winutil::invalidate(h);
}

pub fn hide() {
    with_view(|v| {
        v.state.sort_open = None;
        v.hover = MHit::None;
        v.pressed = MHit::None;
        v.scroll_drag = None;
    });
    // SAFETY: our own window; a stale HWND only makes it fail.
    let _ = unsafe { ShowWindow(hwnd(), SW_HIDE) };
}

pub fn destroy() {
    VIEW.with(|v| v.borrow_mut().take());
    let h = WINDOW.with(|c| c.replace(HWND::default()));
    if !h.is_invalid() {
        // SAFETY: our own window, destroyed once.
        let _ = unsafe { DestroyWindow(h) };
    }
}

/// Self-test only: back to the state after start-up (the library is reset separately).
pub fn reset() {
    with_view(|v| {
        v.state = ManagerState::new();
        v.add = None;
        library::with(|lib| v.state.sync(lib));
    });
    winutil::invalidate(hwnd());
}

fn center_on_cursor_monitor() {
    let at = winutil::monitor_at_cursor();
    POSITIONING.with(|p| p.set(true));
    place(at.work, at.dpi);
    // SAFETY: our own window.
    let actual = unsafe { GetDpiForWindow(hwnd()) };
    if actual != 0 && actual != at.dpi {
        place(at.work, actual);
    }
    POSITIONING.with(|p| p.set(false));
}

/// Fixed size (970x700 DIPs), shrunk only if the monitor's work area is smaller.
fn place(work: RECT, dpi: u32) {
    let px = |dip: f32| (dip * dpi as f32 / 96.0).round() as i32;
    let w = px(manager_view::WIDTH).min(work.right - work.left);
    let h = px(manager_view::HEIGHT).min(work.bottom - work.top);
    let x = work.left + (work.right - work.left - w) / 2;
    let y = work.top + (work.bottom - work.top - h) / 2;
    with_view(|v| {
        if v.dpi != dpi {
            v.gfx.reset_target();
        }
        v.dpi = dpi;
    });
    // SAFETY: our own window; plain integers.
    let _ = unsafe { SetWindowPos(hwnd(), None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE) };
}

// ---------------------------------------------------------------- view helpers

impl View {
    fn area(&self) -> Area {
        let (w, h) = winutil::client_size_dip(hwnd(), self.dpi);
        Area { w, h }
    }

    fn layout(&self, lib: &Library) -> ManagerLayout {
        let a = self.area();
        ManagerLayout::compute(&self.state, lib, a.w, a.h, &self.gfx)
    }

    fn hit(&self, x: f32, y: f32) -> MHit {
        if self.add.is_some() {
            return MHit::None;
        }
        library::with(|lib| self.layout(lib).hit(&self.state, lib, x, y))
    }

    fn search_metrics(&self, l: &ManagerLayout) -> TextMetrics {
        TextMetrics::for_search(&self.gfx, &self.state.search, &l.search)
    }

    fn rename_metrics(&self, l: &ManagerLayout, lib: &Library) -> TextMetrics {
        let ext = self.state.selected_item(lib).map_or("", |i| extension(&i.name));
        let width = l.insp.as_ref().map_or(0.0, |i| i.input.w - 22.0) - self.gfx.measure(ext, TextStyle::Body);
        TextMetrics::of(&self.gfx, &self.state.rename, TextStyle::Body, width)
    }

    fn paint(&mut self) {
        let area = self.area();
        if let Some(add) = self.add.as_ref() {
            add.paint(&mut self.gfx, hwnd(), self.dpi as f32, area);
            return;
        }
        library::with(|lib| {
            let l = self.layout(lib);
            let ui = ManagerUi {
                hover: self.hover,
                dpi: self.dpi as f32,
                search: self.search_metrics(&l),
                rename: self.rename_metrics(&l, lib),
            };
            let state = &self.state;
            let _ = self.gfx.draw(hwnd(), ui.dpi, |p| manager_view::paint(p, state, lib, &l, &ui));
        });
    }

    fn clamp_scroll(&mut self, show_selection: bool) {
        library::with(|lib| {
            let l = self.layout(lib);
            let s = &mut self.state;
            let n = s.visible(lib).len();
            s.scroll = s.scroll.clamp(0.0, l.grid.max_scroll(n));
            if show_selection && let Some(i) = s.selected_index(lib) {
                s.scroll = l.grid.scroll_to_show(i, s.scroll);
            }
        });
    }

    /// Click or Enter/Space on a control. Not called inside a library borrow.
    fn activate(&mut self, hit: MHit) -> Effect {
        let s = &mut self.state;
        match hit {
            MHit::Close => return Effect::Hide,
            MHit::Nav(i) => match Nav::ORDER[i] {
                Nav::Settings => s.status = Some(SETTINGS_NOTE.into()),
                nav => {
                    if let Some(filter) = nav.filter() {
                        library::with(|lib| s.set_filter(filter, lib));
                    }
                }
            },
            MHit::AddMedia => self.add = Some(AddPanel::new()),
            MHit::ClearFilters | MHit::EmptyAction => library::with(|lib| s.clear_filters(lib)),
            MHit::Sort => s.sort_open = if s.sort_open.is_some() { None } else { Some(s.sort.index()) },
            MHit::SortRow(i) => {
                library::with(|lib| s.set_sort(Sort::BOTH[i], lib));
                s.focus = MFocus::Sort;
            }
            MHit::SearchClear => {
                s.search.clear();
                library::with(|lib| s.search_changed(lib));
                s.focus = MFocus::Search;
            }
            MHit::Rename => library::with_mut(|lib| s.apply_rename(lib)),
            MHit::OpenFolder => s.status = Some(OPEN_FOLDER_NOTE.into()),
            MHit::Recycle => s.ask_recycle(),
            MHit::ConfirmMove => library::with_mut(|lib| s.confirm_recycle(lib)),
            MHit::ConfirmCancel => s.cancel_recycle(),
            _ => {}
        }
        self.clamp_scroll(true);
        Effect::None
    }

    fn focus_for(hit: MHit) -> Option<MFocus> {
        Some(match hit {
            MHit::Close => MFocus::Close,
            MHit::Nav(i) => MFocus::Nav(i),
            MHit::AddMedia => MFocus::AddMedia,
            MHit::Search | MHit::SearchClear => MFocus::Search,
            MHit::ClearFilters => MFocus::ClearFilters,
            MHit::Sort | MHit::SortRow(_) => MFocus::Sort,
            MHit::Tile(_) => MFocus::Gallery,
            MHit::EmptyAction => MFocus::EmptyAction,
            MHit::RenameInput => MFocus::RenameInput,
            MHit::Rename => MFocus::Rename,
            MHit::OpenFolder => MFocus::OpenFolder,
            MHit::Recycle => MFocus::Recycle,
            MHit::ConfirmMove => MFocus::ConfirmMove,
            MHit::ConfirmCancel => MFocus::ConfirmCancel,
            MHit::None | MHit::Caption | MHit::Scrollbar => return None,
        })
    }

    fn add_outcome(&mut self, outcome: AddOutcome) -> Effect {
        match outcome {
            AddOutcome::None => Effect::None,
            AddOutcome::CloseWindow => {
                self.add = None;
                Effect::Hide
            }
            AddOutcome::Leave => {
                self.add = None;
                self.state.focus = MFocus::AddMedia;
                Effect::Capture(false)
            }
            AddOutcome::Imported(status) => {
                self.add = None;
                library::with(|lib| self.state.show_imported(status, lib));
                Effect::Capture(false)
            }
            AddOutcome::Capture(on) => Effect::Capture(on),
            AddOutcome::PasteText => Effect::PasteText,
        }
    }

    /// Caret position for a click at `x` in a text box whose text starts at `text_x`.
    fn caret_at(gfx: &Gfx, field: &mut TextField, m: TextMetrics, text_x: f32, x: f32) {
        let pos = gfx.position_at(field.text(), TextStyle::Body, x - text_x + m.offset);
        field.set_caret_utf16(pos);
    }

    fn mouse_down(&mut self, x: f32, y: f32) -> Effect {
        let area = self.area();
        if let Some(add) = self.add.as_mut() {
            let outcome = add.mouse_down(&self.gfx, area, x, y);
            return self.add_outcome(outcome);
        }
        let hit = self.hit(x, y);
        if self.state.sort_open.is_some() && !matches!(hit, MHit::SortRow(_) | MHit::Sort) {
            self.state.sort_open = None; // a click outside only closes the list
            return Effect::None;
        }
        self.state.focus_visible = false;
        if let Some(f) = Self::focus_for(hit) {
            self.state.focus = f;
        }
        match hit {
            MHit::Search | MHit::RenameInput => {
                library::with(|lib| {
                    let l = self.layout(lib);
                    if hit == MHit::Search {
                        let m = self.search_metrics(&l);
                        Self::caret_at(&self.gfx, &mut self.state.search, m, l.search.input.x + 2.0, x);
                    } else if let Some(i) = &l.insp {
                        let m = self.rename_metrics(&l, lib);
                        Self::caret_at(&self.gfx, &mut self.state.rename, m, i.input.x + 11.0, x);
                    }
                });
                Effect::None
            }
            MHit::Tile(i) => {
                library::with(|lib| {
                    let id = self.state.visible(lib).get(i).map(|item| item.id);
                    if let Some(id) = id {
                        self.state.select(id, lib);
                    }
                });
                Effect::None
            }
            MHit::Scrollbar => {
                self.scroll_drag = Some((y, self.state.scroll));
                Effect::Capture(true)
            }
            MHit::None | MHit::Caption => Effect::None,
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
        let pressed = std::mem::replace(&mut self.pressed, MHit::None);
        if pressed == MHit::None {
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
            library::with(|lib| {
                let l = self.layout(lib);
                let content = l.grid.content_height(self.state.visible(lib).len());
                let ratio = if l.grid.area.h > 0.0 { content / l.grid.area.h } else { 1.0 };
                self.state.scroll = start_scroll + (y - start_y) * ratio;
            });
            self.clamp_scroll(false);
        }
        self.hover = self.hit(x, y);
    }

    fn wheel(&mut self, delta: i16) {
        let area = self.area();
        if let Some(add) = self.add.as_mut() {
            return add.wheel(&self.gfx, area, delta);
        }
        self.state.sort_open = None;
        self.state.scroll -= delta as f32 / 120.0 * WHEEL_STEP;
        self.clamp_scroll(false);
    }

    fn key(&mut self, vk: VIRTUAL_KEY, ctrl: bool, shift: bool, alt: bool) -> Effect {
        let area = self.area();
        if let Some(add) = self.add.as_mut() {
            let outcome = add.key(&self.gfx, area, vk, ctrl, shift);
            return self.add_outcome(outcome);
        }
        let grid = library::with(|lib| self.layout(lib).grid);
        let effect = library::with(|lib| self.key_library(lib, vk, ctrl, shift, alt, grid.page()));
        self.clamp_scroll(true);
        effect
    }

    /// Keys for the library screen. Runs inside a library read, so it must not
    /// change the library: actions that do are returned as `Effect::Deferred`.
    fn key_library(&mut self, lib: &Library, vk: VIRTUAL_KEY, ctrl: bool, shift: bool, alt: bool, page: isize) -> Effect {
        let s = &mut self.state;
        if let Some(row) = s.sort_open {
            match vk {
                VK_UP | VK_DOWN => s.sort_open = Some(if vk == VK_UP { 0 } else { 1 }),
                VK_RETURN | VK_SPACE => s.set_sort(Sort::BOTH[row], lib),
                VK_ESCAPE => s.sort_open = None,
                VK_TAB => s.tab(lib, shift),
                _ => {}
            }
            return Effect::None;
        }
        match vk {
            VK_TAB => {
                s.tab(lib, shift);
                return Effect::None;
            }
            VK_ESCAPE if s.confirming => {
                s.cancel_recycle();
                return Effect::None;
            }
            _ => {}
        }
        let press = matches!(vk, VK_RETURN | VK_SPACE);
        match s.focus {
            MFocus::Search => {
                if edit_keys(&mut s.search, vk, ctrl) {
                    s.search_changed(lib);
                }
                match vk {
                    VK_V if ctrl => return Effect::PasteText,
                    VK_DOWN if !s.visible(lib).is_empty() => {
                        s.focus = MFocus::Gallery;
                        s.focus_visible = true;
                    }
                    VK_ESCAPE if !s.search.is_empty() => {
                        s.search.clear();
                        s.search_changed(lib);
                    }
                    _ => {}
                }
            }
            MFocus::RenameInput => {
                if edit_keys(&mut s.rename, vk, ctrl) {
                    s.rename_edited(lib);
                }
                match vk {
                    VK_V if ctrl => return Effect::PasteText,
                    VK_RETURN => return Effect::Deferred(MHit::Rename),
                    VK_ESCAPE => s.revert_rename(lib),
                    _ => {}
                }
            }
            MFocus::Gallery => {
                let cols = COLUMNS as isize;
                match vk {
                    VK_LEFT => s.move_selection(lib, -1),
                    VK_RIGHT => s.move_selection(lib, 1),
                    VK_DOWN => s.move_selection(lib, cols),
                    VK_UP if s.selected_index(lib).unwrap_or(0) < COLUMNS => s.focus = MFocus::Search,
                    VK_UP => s.move_selection(lib, -cols),
                    VK_HOME => s.move_selection(lib, -10_000),
                    VK_END => s.move_selection(lib, 10_000),
                    VK_NEXT => s.move_selection(lib, page),
                    VK_PRIOR => s.move_selection(lib, -page),
                    VK_DELETE => s.ask_recycle(),
                    // F2 or Enter: rename the selected item, as in Explorer.
                    VK_F2 | VK_RETURN if s.selected.is_some() => {
                        s.focus = MFocus::RenameInput;
                        s.rename.select_all();
                    }
                    _ => return Effect::None,
                }
                s.focus_visible = true;
            }
            MFocus::Nav(i) => match vk {
                VK_UP if i > 0 => s.focus = MFocus::Nav(i - 1),
                VK_DOWN if i + 1 < Nav::ORDER.len() => s.focus = MFocus::Nav(i + 1),
                _ if press => return Effect::Deferred(MHit::Nav(i)),
                _ => {}
            },
            MFocus::Sort => match vk {
                VK_DOWN if alt => s.sort_open = Some(s.sort.index()),
                VK_F4 | VK_RETURN | VK_SPACE => s.sort_open = Some(s.sort.index()),
                VK_UP => s.set_sort(Sort::RecentlyAdded, lib),
                VK_DOWN => s.set_sort(Sort::Name, lib),
                _ => {}
            },
            focus if press => {
                let hit = match focus {
                    MFocus::Close => MHit::Close,
                    MFocus::AddMedia => MHit::AddMedia,
                    MFocus::ClearFilters => MHit::ClearFilters,
                    MFocus::EmptyAction => MHit::EmptyAction,
                    MFocus::Rename => MHit::Rename,
                    MFocus::OpenFolder => MHit::OpenFolder,
                    MFocus::Recycle => MHit::Recycle,
                    MFocus::ConfirmMove => MHit::ConfirmMove,
                    MFocus::ConfirmCancel => MHit::ConfirmCancel,
                    _ => MHit::None,
                };
                return Effect::Deferred(hit);
            }
            _ => {}
        }
        Effect::None
    }

    fn insert_text(&mut self, text: &str) {
        if let Some(add) = self.add.as_mut() {
            return add.insert(text);
        }
        let s = &mut self.state;
        library::with(|lib| match s.focus {
            MFocus::RenameInput => {
                s.rename.insert(text);
                s.rename_edited(lib);
            }
            // Typing in the gallery goes to the search box, as in the popup.
            MFocus::Search | MFocus::Gallery if s.sort_open.is_none() => {
                s.focus = MFocus::Search;
                s.search.insert(text);
                s.search_changed(lib);
            }
            _ => {}
        });
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

/// Caret and editing keys for a one-line text box. Returns true if the text changed.
fn edit_keys(f: &mut TextField, vk: VIRTUAL_KEY, ctrl: bool) -> bool {
    match vk {
        VK_LEFT => f.left(),
        VK_RIGHT => f.right(),
        VK_HOME => f.home(),
        VK_END => f.end(),
        VK_A if ctrl => f.select_all(),
        VK_BACK if ctrl => {
            f.delete_word_back();
            return true;
        }
        VK_BACK => {
            f.backspace();
            return true;
        }
        VK_DELETE => {
            f.delete();
            return true;
        }
        _ => {}
    }
    false
}

fn run(effect: Effect) {
    let h = hwnd();
    match effect {
        Effect::None => {}
        Effect::Hide => hide(),
        Effect::Capture(true) => winutil::set_capture(h),
        Effect::Capture(false) => winutil::release_capture(),
        Effect::PasteText => {
            if let Some(text) = winutil::clipboard_text(h) {
                with_view(|v| v.insert_text(&text));
            }
        }
        Effect::Deferred(hit) => {
            if let Some(effect) = with_view(|v| v.activate(hit)) {
                run(effect);
            }
        }
    }
    winutil::invalidate(h);
}

fn screen_to_client(hwnd: HWND, lparam: LPARAM) -> POINT {
    let mut pt = winutil::lparam_point(lparam);
    // SAFETY: our own window; pt is a local.
    let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
    pt
}

extern "system" fn manager_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let effect = match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            with_view(|v| v.paint());
            // SAFETY: our own window; marks it painted.
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            // The title bar (outside ×) moves the window, like a normal caption.
            let pt = screen_to_client(hwnd, lparam);
            let caption = with_view(|v| {
                let (x, y) = winutil::to_dip(pt, v.dpi);
                let in_bar = (1.0..47.0).contains(&y);
                in_bar && v.hit(x, y) != MHit::Close && !(v.add.is_some() && x > v.area().w - 48.0)
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
                v.hover = MHit::None;
                if let Some(add) = v.add.as_mut() {
                    add.hover = AddHit::None;
                }
            });
            Effect::None
        }
        WM_LBUTTONDOWN => with_view(|v| {
            let (x, y) = winutil::to_dip(winutil::lparam_point(lparam), v.dpi);
            v.mouse_down(x, y)
        })
        .unwrap_or(Effect::None),
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
                // SAFETY: forwarding unchanged (keeps Alt+F4 and Alt+Space).
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
                None => matches!(v.hover, MHit::Search | MHit::RenameInput),
            })
            .unwrap_or(false);
            winutil::set_cursor(beam);
            return LRESULT(1);
        }
        WM_DPICHANGED => {
            with_view(|v| {
                v.dpi = ((wparam.0 >> 16) & 0xFFFF) as u32;
                v.gfx.reset_target();
            });
            if !POSITIONING.with(|p| p.get()) {
                // SAFETY: Windows passes a valid RECT pointer with WM_DPICHANGED.
                let r = unsafe { &*(lparam.0 as *const RECT) };
                // SAFETY: our own window.
                let _ = unsafe { SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE) };
            }
            Effect::None
        }
        // Alt+F4 or the taskbar's Close: hide, like ×. Only Exit ends the app.
        WM_CLOSE => Effect::Hide,
        // SAFETY: forwarding the unchanged message.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    };
    run(effect);
    LRESULT(0)
}

// ---------------------------------------------------------------- self-test hooks

/// Screen-space centre of a Manager element, for the self-test to click.
pub fn element_center(hit: MHit) -> Option<POINT> {
    let h = hwnd();
    with_view(|v| {
        if v.add.is_some() {
            return None;
        }
        let r = library::with(|lib| {
            let l = v.layout(lib);
            let n = v.state.visible(lib).len();
            let insp = l.insp.as_ref();
            let confirm = insp.and_then(|i| i.confirm.as_ref());
            Some(match hit {
                MHit::Close => l.close,
                MHit::Nav(i) => *l.nav.get(i)?,
                MHit::AddMedia => l.add_media,
                MHit::Search => l.search.outer,
                MHit::SearchClear => l.search.clear,
                MHit::ClearFilters => l.clear_filters?,
                MHit::Sort => l.sort,
                MHit::SortRow(i) => *l.sort_menu.rows.get(i)?,
                MHit::Tile(i) if i < n => l.grid.tile(i, v.state.scroll),
                // A point in the gap between the first two columns: over the gallery, not a tile.
                MHit::Scrollbar => Rect::new(l.grid.tile(0, 0.0).right() + 1.0, l.grid.area.y + 40.0, 10.0, 1.0),
                MHit::EmptyAction if n == 0 => l.empty.2,
                MHit::RenameInput => insp?.input,
                MHit::Rename => insp?.rename,
                MHit::OpenFolder => insp?.open_folder,
                MHit::Recycle if confirm.is_none() => insp?.recycle,
                MHit::ConfirmMove => confirm?.move_button,
                MHit::ConfirmCancel => confirm?.cancel,
                // A neutral point: the empty middle of the title bar.
                MHit::None => Rect::new(l.caption.right() + 40.0, l.titlebar.y + 10.0, 1.0, 1.0),
                _ => return None,
            })
        })?;
        Some(winutil::rect_center_on_screen(h, v.dpi, r))
    })
    .flatten()
}

/// Screen-space centre of an Add media element while the panel is open in the Manager.
pub fn add_element_center(hit: AddHit) -> Option<POINT> {
    let h = hwnd();
    with_view(|v| {
        let area = v.area();
        let r = v.add.as_ref()?.element(&v.gfx, area, hit)?;
        Some(winutil::rect_center_on_screen(h, v.dpi, r))
    })
    .flatten()
}

/// "screen=manager filter=… selected=… names=a|b" for self-test checks.
pub fn describe() -> String {
    with_view(|v| {
        if let Some(add) = v.add.as_ref() {
            return format!("host=manager {}", add.describe());
        }
        let s = &v.state;
        library::with(|lib| {
            let names: Vec<&str> = s.visible(lib).iter().map(|i| i.name.as_str()).collect();
            format!(
                "screen=manager filter={:?} sort={:?} query={:?} focus={:?} selected={:?} rename={:?} error={:?} \
                 can_rename={} confirming={} status={:?} total={} count={} names={}",
                s.filter,
                s.sort,
                s.search.text(),
                s.focus,
                s.selected_item(lib).map(|i| i.name.as_str()).unwrap_or(""),
                s.rename.text(),
                s.rename_error,
                s.can_rename(),
                s.confirming,
                s.status.as_deref().unwrap_or(""),
                lib.items.len(),
                names.len(),
                names.join("|")
            )
        })
    })
    .unwrap_or_else(|| "screen=manager-not-created".into())
}
