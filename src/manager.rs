//! The Manager window: library browsing, rename and removal, and the Settings
//! page (fake data, settings in memory). State lives in `ManagerState`
//! and the shared `library`; geometry and drawing in `ui::manager_view` and
//! `ui::settings_view`. Add media is an `AddPanel`, as in the popup.
//!
//! Created the first time it is shown. × hides it; the tray and shortcut stay.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{ScreenToClient, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    DragDetect, VIRTUAL_KEY, VK_A, VK_BACK, VK_C, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F2, VK_F4, VK_HOME, VK_LEFT,
    VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP, VK_V,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use crate::add_panel::{AddOutcome, AddPanel, Area};
use crate::library::{self, Library, extension};
use crate::manager_state::{MFocus, ManagerState, Nav, Page, Sort};
use crate::settings_state::{BackupPanel, Profile, SFocus, VideoPreview};
use crate::hotkey::{self, Shortcut};
use crate::text_field::TextField;
use crate::ui::add_view::AddHit;
use crate::ui::d2d::Gfx;
use crate::thumbs::{self, Win};
use crate::ui::layout::{COLUMNS, Rect, TextStyle};
use crate::ui::manager_view::{self, MHit, ManagerLayout, ManagerUi};
use crate::ui::settings_view::{self, SHit, SettingsLayout, SettingsUi};
use crate::ui::widgets::TextMetrics;
use crate::winutil::{self, WM_MOUSELEAVE};

const WHEEL_STEP: f32 = 48.0;

struct View {
    state: ManagerState,
    /// Add media panel, shown in place of the library while open.
    add: Option<AddPanel>,
    gfx: Gfx,
    dpi: u32,
    hover: MHit,
    pressed: MHit,
    /// Pointer state on the Settings page.
    s_hover: SHit,
    s_pressed: SHit,
    scroll_drag: Option<(f32, f32)>,
    high_surrogate: Option<u16>,
    tracking_leave: bool,
}

/// Work to do after the view borrow is released (these can re-enter the window procedure).
enum Effect {
    None,
    Hide,
    /// Hide this window and open the quick popup.
    QuickPopup,
    Capture(bool),
    PasteText,
    /// Activate a control once no state is borrowed (keyboard Enter/Space).
    Deferred(MHit),
    /// Show the Add media file or folder picker (modal).
    Pick(crate::dialogs::Pick),
    /// Show the folder picker for Back up library (modal).
    PickBackup,
    /// Pressed on a tile: a drag takes a copy of its file out, like the quick popup.
    TileDown,
    /// Ctrl+C: put the item's file on the clipboard, like the quick popup.
    Copy(u32),
}

thread_local! {
    static WINDOW: Cell<HWND> = Cell::new(HWND::default());
    static VIEW: RefCell<Option<View>> = const { RefCell::new(None) };
    static POSITIONING: Cell<bool> = const { Cell::new(false) };
    static TITLE_PAGE: Cell<Option<Page>> = const { Cell::new(None) };
    /// Self-test only: the folder Open folder was asked to show.
    static OPENED: RefCell<Option<String>> = const { RefCell::new(None) };
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
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            hIcon: winutil::app_icon(false),
            hIconSm: winutil::app_icon(true),
            lpfnWndProc: Some(manager_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("PocketMediaManager"),
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
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
    state.settings = crate::settings_state::SettingsPage::with_saved(crate::store::settings());
    library::with(|lib| state.sync(lib));
    VIEW.with(|v| {
        *v.borrow_mut() = Some(View {
            state,
            add: None,
            gfx,
            dpi: 96,
            hover: MHit::None,
            pressed: MHit::None,
            s_hover: SHit::None,
            s_pressed: SHit::None,
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

/// The tray menu's Settings: the Manager at its Settings page. An open Add media
/// panel stays open (closing it could lose a review in progress).
pub fn show_settings() {
    show();
    with_view(|v| {
        if v.add.is_none() && v.state.page != Page::Settings {
            library::with(|lib| v.state.navigate(Nav::Settings, lib));
            v.s_hover = SHit::None;
        }
    });
    winutil::invalidate(hwnd());
}

/// The uninstaller's Back up library… button (`pocketmedia.exe --backup`): shows
/// Settings scrolled to Back up library and opens its folder picker, as clicking
/// the button there does. A running backup is only shown.
///
/// An open Add media panel and unsaved Settings edits are left as they are, so
/// nothing in progress is lost, and a note says what to finish first.
/// Before, nothing visible happened and the user could think the backup
/// had been made.
pub fn back_up_from_uninstaller() {
    show();
    if hwnd().is_invalid() {
        return;
    }
    let pick = with_view(|v| {
        if let Some(panel) = v.add.as_mut() {
            panel.add.notice = Some(crate::add_media::BACKUP_WAITS.into());
            return false;
        }
        if v.state.page == Page::Settings && v.state.settings.dirty() {
            v.state.settings.show_note(SETTINGS_WAIT);
            return false;
        }
        library::with(|lib| v.state.navigate(Nav::Settings, lib));
        v.s_hover = SHit::None;
        v.state.settings.scroll = 100_000.0;
        v.clamp_settings_scroll(false);
        !crate::backup::running()
    })
    .unwrap_or(false);
    winutil::invalidate(hwnd());
    if pick {
        run(Effect::PickBackup);
    }
}

/// The uninstaller asked for Back up library while Settings had unsaved changes.
/// Short: it shares the line left of Cancel / Save with "Unsaved changes · ".
pub const SETTINGS_WAIT: &str = "Save or cancel first, then Back up library.";

/// The line under the inspector (copy and drag results). Empty clears it.
pub fn show_status(text: &str) {
    with_view(|v| v.state.status = (!text.is_empty()).then(|| text.to_string()));
    winutil::invalidate(hwnd());
}

pub fn hide() {
    with_view(|v| {
        v.state.sort_open = None;
        if v.state.settings.shortcut_dialog.is_some() {
            v.state.settings.close_shortcut_dialog();
        }
        v.hover = MHit::None;
        v.pressed = MHit::None;
        v.s_hover = SHit::None;
        v.s_pressed = SHit::None;
        v.scroll_drag = None;
    });
    hotkey::set_paused(false);
    release_previews();
    // SAFETY: our own window; a stale HWND only makes it fail.
    let _ = unsafe { ShowWindow(hwnd(), SW_HIDE) };
}

/// Hidden or minimised: thumbnail work for the Manager is cancelled and its
/// GIFs and hover video stop. With Use less memory its picture bitmaps go too.
fn release_previews() {
    let profile = crate::store::settings().profile;
    thumbs::hidden(Win::Manager, profile);
    if profile == Profile::LessMemory {
        with_view(|v| v.gfx.release_images());
    }
}

/// Self-test only: minimise (false) or restore (true) the Manager.
pub fn test_show_state(restore: bool) {
    // SAFETY: our own window; a stale HWND only makes it fail.
    let _ = unsafe { ShowWindow(hwnd(), if restore { SW_RESTORE } else { SW_MINIMIZE }) };
}

/// Back up library progress or its result, shown beside Save and Cancel.
pub fn backup_changed(progress: &crate::backup::Progress, text: &str, running: bool) {
    use crate::backup::Progress;
    let panel = match progress {
        Progress::Copying(step) => BackupPanel::Running(step.clone()),
        Progress::Finished(d) => {
            BackupPanel::Done { title: "Backup finished".into(), text: text.into(), folder: Some(d.folder.clone()) }
        }
        Progress::Failed(_) => BackupPanel::Done { title: "Backup failed".into(), text: text.into(), folder: None },
        Progress::Cancelled => BackupPanel::Done { title: "Backup cancelled".into(), text: text.into(), folder: None },
    };
    with_view(|v| {
        v.state.settings.backup_running = running;
        v.state.settings.show_note(text);
        v.state.settings.set_backup_panel(panel);
    });
    winutil::invalidate(hwnd());
}

/// The library changed outside the Manager (the Media folders were changed in
/// File Explorer). The rename field reloads if the selected item was renamed.
pub fn library_changed(renamed: &[u32]) {
    with_view(|v| {
        if v.state.selected.is_some_and(|id| renamed.contains(&id)) {
            v.state.reload_rename();
        }
        library::with(|lib| v.state.sync(lib));
        v.clamp_scroll(false);
    });
    winutil::invalidate(hwnd());
}

/// Repaints the Manager if it is on screen (a thumbnail arrived).
pub fn refresh() {
    if is_visible() {
        winutil::invalidate(hwnd());
    }
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
        v.state.settings = crate::settings_state::SettingsPage::with_saved(crate::store::settings());
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

    fn on_settings(&self) -> bool {
        self.add.is_none() && self.state.page == Page::Settings
    }

    /// The shortcut chooser is open and takes the keyboard.
    fn choosing(&self) -> bool {
        self.on_settings() && self.state.settings.shortcut_dialog.is_some()
    }

    fn settings_layout(&self) -> SettingsLayout {
        let a = self.area();
        SettingsLayout::compute(&self.state.settings, a.w, a.h, &self.gfx)
    }

    fn settings_hit(&self, x: f32, y: f32) -> SHit {
        if !self.on_settings() {
            return SHit::None;
        }
        self.settings_layout().hit(&self.state.settings, x, y)
    }

    /// Where the window can be dragged: the sidebar's logo area, or the head of
    /// the Add media panel between its buttons.
    fn is_caption(&self, x: f32, y: f32) -> bool {
        if let Some(add) = self.add.as_ref() {
            return add.is_caption(&self.gfx, self.area(), x, y);
        }
        if self.state.settings.shortcut_dialog.is_some() && self.on_settings() {
            return false;
        }
        let a = self.area();
        manager_view::Frame::compute(a.w, a.h, &self.gfx).caption.contains(x, y)
    }

    fn hit(&self, x: f32, y: f32) -> MHit {
        if self.add.is_some() || self.state.page != Page::Library {
            return MHit::None;
        }
        library::with(|lib| self.layout(lib).hit(&self.state, lib, x, y))
    }

    fn search_metrics(&self, l: &ManagerLayout) -> TextMetrics {
        TextMetrics::for_search(&self.gfx, &self.state.search, &l.search, TextStyle::Body)
    }

    fn rename_metrics(&self, l: &ManagerLayout, lib: &Library) -> TextMetrics {
        let ext = self.state.selected_item(lib).map_or("", |i| extension(&i.name));
        let width = l.insp.as_ref().map_or(0.0, |i| i.input.w - 24.0) - self.gfx.measure(ext, TextStyle::Body);
        TextMetrics::of(&self.gfx, &self.state.rename, TextStyle::Body, width)
    }

    fn paint(&mut self, dirty: Option<Rect>) {
        let area = self.area();
        // SAFETY: tolerates any HWND.
        let minimised = unsafe { IsIconic(hwnd()) }.as_bool();
        thumbs::begin_frame(Win::Manager, self.dpi, dirty.is_none() || minimised);
        if minimised {
            // Nothing shows: an empty frame, so no thumbnail is wanted and no GIF plays.
        } else if let Some(add) = self.add.as_ref() {
            add.paint(&mut self.gfx, hwnd(), self.dpi as f32, area);
        } else if self.state.page == Page::Settings {
            // The button label follows the backup itself, even if an event was missed.
            self.state.settings.backup_running = crate::backup::running();
            let l = self.settings_layout();
            let ui = SettingsUi { hover: self.s_hover, shortcut_ok: hotkey::ok() };
            let s = &self.state.settings;
            let _ = self.gfx.draw(hwnd(), self.dpi as f32, None, |p| settings_view::paint(p, s, &l, &ui));
        } else {
            library::with(|lib| {
                let l = self.layout(lib);
                let ui = ManagerUi {
                    hover: self.hover,
                    dpi: self.dpi as f32,
                    search: self.search_metrics(&l),
                    rename: self.rename_metrics(&l, lib),
                };
                let state = &self.state;
                let _ = self.gfx.draw(hwnd(), ui.dpi, dirty, |p| manager_view::paint(p, state, lib, &l, &ui));
            });
        }
        thumbs::end_frame(Win::Manager);
    }

    /// The video tile under the pointer, when Video preview is Hover only.
    fn hover_video(&self) -> Option<(thumbs::Key, std::path::PathBuf)> {
        let MHit::Tile(i) = self.hover else { return None };
        if crate::store::settings().video_preview != VideoPreview::HoverOnly {
            return None;
        }
        let media = crate::paths::get()?.media.clone();
        let dpi = self.dpi as f32;
        let (key, rel) = library::with(|lib| {
            let item = *self.state.visible(lib).get(i)?;
            if item.kind != crate::sample::MediaKind::Video {
                return None;
            }
            let tile = self.layout(lib).grid.tile(i, self.state.scroll);
            let key = crate::ui::widgets::thumb_key(item.id, crate::ui::layout::Layout::thumb(tile), dpi);
            Some((key, item.path.clone()))
        })?;
        let path = crate::import::media_file(&media, &rel)?;
        (!thumbs::is_no_preview(&key)).then_some((key, path))
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
            MHit::QuickPopup => return Effect::QuickPopup,
            MHit::Nav(i) => {
                library::with(|lib| s.navigate(Nav::ORDER[i], lib));
                self.hover = MHit::None;
            }
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
            MHit::Rename => rename_selected(s),
            MHit::OpenFolder => s.status = show_selected_in_explorer(s),
            MHit::Recycle => s.ask_recycle(),
            MHit::ConfirmMove => recycle_selected(s),
            MHit::ConfirmCancel => s.cancel_recycle(),
            _ => {}
        }
        self.clamp_scroll(true);
        Effect::None
    }

    fn focus_for(hit: MHit) -> Option<MFocus> {
        Some(match hit {
            MHit::Close => MFocus::Close,
            MHit::QuickPopup => MFocus::QuickPopup,
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
                self.state.settings.focus = SFocus::ChooseFolder;
                Effect::Capture(false)
            }
            AddOutcome::Imported(status) => {
                self.add = None;
                if self.state.page == Page::Settings {
                    // Back where the import started; the result shows in the action bar.
                    self.state.settings.focus = SFocus::ChooseFolder;
                    self.state.settings.show_note(&status);
                    library::with(|lib| self.state.sync(lib));
                } else {
                    library::with(|lib| self.state.show_imported(status, lib));
                }
                Effect::Capture(false)
            }
            AddOutcome::Capture(on) => Effect::Capture(on),
            AddOutcome::PasteText => Effect::PasteText,
            AddOutcome::Pick(kind) => Effect::Pick(kind),
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
        if self.on_settings() {
            return self.settings_mouse_down(x, y);
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
                        Self::caret_at(&self.gfx, &mut self.state.search, m, l.search.input.x, x);
                    } else if let Some(i) = &l.insp {
                        let m = self.rename_metrics(&l, lib);
                        Self::caret_at(&self.gfx, &mut self.state.rename, m, i.input.x + 12.0, x);
                    }
                });
                Effect::None
            }
            MHit::Tile(i) => {
                let picked = library::with(|lib| {
                    let id = self.state.visible(lib).get(i).map(|item| item.id);
                    if let Some(id) = id {
                        self.state.select(id, lib);
                    }
                    id.is_some()
                });
                if picked { Effect::TileDown } else { Effect::None }
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
        if self.on_settings() {
            let pressed = std::mem::replace(&mut self.s_pressed, SHit::None);
            if pressed == SHit::None {
                return Effect::None;
            }
            winutil::release_capture();
            return if self.settings_hit(x, y) == pressed { self.activate_settings(pressed) } else { Effect::None };
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
        if self.on_settings() {
            self.s_hover = self.settings_hit(x, y);
            return;
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
        crate::preview::hover(Win::Manager, self.hover_video());
    }

    fn wheel(&mut self, delta: i16) {
        let area = self.area();
        if let Some(add) = self.add.as_mut() {
            return add.wheel(&self.gfx, area, delta);
        }
        if self.on_settings() {
            let st = &mut self.state.settings;
            if st.shortcut_dialog.is_some() {
                return;
            }
            st.scroll -= delta as f32 / 120.0 * WHEEL_STEP;
            return self.clamp_settings_scroll(false);
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
        if self.on_settings() {
            let effect = self.key_settings(vk, shift, alt);
            self.clamp_settings_scroll(true);
            return effect;
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
        // Ctrl+C copies the selected item's file, like the quick popup (the text
        // fields keep their own keys).
        if ctrl && vk == VK_C && !matches!(s.focus, MFocus::Search | MFocus::RenameInput) {
            return s.selected_item(lib).map_or(Effect::None, |item| Effect::Copy(item.id));
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
                    MFocus::QuickPopup => MHit::QuickPopup,
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

    // ------------------------------------------------------------ Settings page

    /// Keeps the settings scroll in range and, for keyboard moves, the focused control in view.
    fn clamp_settings_scroll(&mut self, show_focus: bool) {
        let l = self.settings_layout();
        let st = &mut self.state.settings;
        st.scroll = st.scroll.clamp(0.0, l.max_scroll());
        if show_focus {
            st.scroll = l.scroll_to_show(st, st.focus);
        }
    }

    fn settings_focus_for(hit: SHit) -> Option<SFocus> {
        Some(match hit {
            SHit::Close => SFocus::Close,
            SHit::QuickPopup => SFocus::QuickPopup,
            SHit::Nav(i) => SFocus::Nav(i),
            SHit::Profile(_) => SFocus::Profile,
            SHit::Video => SFocus::Video,
            SHit::Change => SFocus::Change,
            SHit::CloseAfter => SFocus::CloseAfter,
            SHit::OpenFolder => SFocus::OpenFolder,
            SHit::ChooseFolder => SFocus::ChooseFolder,
            SHit::Backup => SFocus::Backup,
            SHit::PanelCancel => SFocus::PanelCancel,
            SHit::PanelOpen => SFocus::PanelOpen,
            SHit::PanelDone => SFocus::PanelDone,
            SHit::Autostart => SFocus::Autostart,
            SHit::Cancel => SFocus::Cancel,
            SHit::Save => SFocus::Save,
            SHit::DialogReset => SFocus::DialogReset,
            SHit::DialogCancel => SFocus::DialogCancel,
            SHit::DialogUse => SFocus::DialogUse,
            SHit::None | SHit::Caption => return None,
        })
    }

    fn settings_mouse_down(&mut self, x: f32, y: f32) -> Effect {
        let hit = self.settings_hit(x, y);
        let st = &mut self.state.settings;
        let use_off = hit == SHit::DialogUse && !st.shortcut_dialog.as_ref().is_some_and(|d| d.can_use());
        if (hit == SHit::Save && !st.dirty()) || use_off {
            return Effect::None; // disabled
        }
        st.focus_visible = false;
        if let Some(f) = Self::settings_focus_for(hit) {
            st.focus = f;
        }
        match hit {
            SHit::None | SHit::Caption => Effect::None,
            other => {
                self.s_pressed = other;
                Effect::Capture(true)
            }
        }
    }

    /// Click or Enter/Space on a Settings control.
    fn activate_settings(&mut self, hit: SHit) -> Effect {
        let st = &mut self.state.settings;
        match hit {
            SHit::Close => return Effect::Hide,
            SHit::QuickPopup => return Effect::QuickPopup,
            SHit::Nav(i) if Nav::ORDER[i] == Nav::Settings => {} // already here; keep the draft
            SHit::Nav(i) => {
                let s = &mut self.state;
                library::with(|lib| s.navigate(Nav::ORDER[i], lib));
                self.s_hover = SHit::None;
                self.clamp_scroll(true);
                return Effect::None;
            }
            SHit::Profile(i) => st.set_profile(Profile::BOTH[i]),
            SHit::Video => st.toggle_video(),
            SHit::Change => st.open_shortcut_dialog(),
            SHit::DialogReset => {
                let d = Shortcut::default();
                st.dialog_pressed(d, hotkey::is_free(d));
            }
            SHit::DialogCancel => st.close_shortcut_dialog(),
            SHit::DialogUse => st.use_shortcut(),
            SHit::CloseAfter => st.toggle_close_after(),
            SHit::OpenFolder => {
                let note = open_media_folder();
                self.state.settings.show_note(&note);
            }
            SHit::ChooseFolder => {
                self.add = Some(AddPanel::for_settings());
                self.s_hover = SHit::None;
                return Effect::Pick(crate::dialogs::Pick::Folder);
            }
            SHit::Backup if crate::backup::running() => {
                crate::backup::cancel();
                st.show_note("Cancelling the backup…");
            }
            SHit::Backup => return Effect::PickBackup,
            SHit::PanelCancel => {
                crate::backup::cancel();
                st.show_note("Cancelling the backup…");
            }
            SHit::PanelOpen => {
                let folder = match &st.backup_panel {
                    // Without the \\?\ prefix canonicalize adds: File Explorer wants a plain path.
                    Some(BackupPanel::Done { folder: Some(f), .. }) => std::path::PathBuf::from(crate::import::display_path(f)),
                    _ => return Effect::None,
                };
                let note = open_folder_note(&folder, "the backup");
                self.state.settings.show_note(&note);
            }
            SHit::PanelDone => st.close_backup_panel(),
            SHit::Autostart => st.toggle_autostart(),
            SHit::Cancel => st.cancel(),
            SHit::Save => {
                let mut problems = Vec::new();
                // A new shortcut is registered first; if Windows refuses it, the old one stays.
                if st.draft.shortcut != st.saved.shortcut {
                    match hotkey::apply(st.draft.shortcut) {
                        Ok(()) => crate::tray::set_tip(crate::app(|a| a.main.get()), "PocketMedia"),
                        Err(e) => {
                            problems.push(format!("{} could not be used: {e} The shortcut is unchanged.", st.draft.shortcut.label()));
                            st.draft.shortcut = st.saved.shortcut;
                        }
                    }
                }
                let autostart = (st.draft.autostart != st.saved.autostart).then_some(st.draft.autostart);
                st.save();
                if let Some(on) = autostart
                    && let Err(e) = crate::autostart::set(on)
                {
                    // The registry is the truth: the toggle shows what it says.
                    st.saved.autostart = crate::autostart::is_on();
                    st.draft.autostart = st.saved.autostart;
                    // A message that already says what happened to Autostart is shown as it is.
                    problems.push(if e.starts_with("Autostart ") {
                        e
                    } else {
                        format!("Autostart could not be turned {}: {}.", if on { "on" } else { "off" }, e.trim_end_matches('.'))
                    });
                }
                if let Err(e) = crate::store::save_settings(st.saved) {
                    problems.push(format!("Settings could not be saved: {e}"));
                }
                if !problems.is_empty() {
                    st.show_note(&problems.join(" "));
                }
            }
            SHit::None | SHit::Caption => {}
        }
        Effect::None
    }

    /// Keys while the shortcut chooser is open: Tab, Enter, Space and Escape
    /// (without Ctrl, Alt or Win) work the dialog; anything else is recorded.
    fn key_dialog(&mut self, vk: VIRTUAL_KEY) -> Effect {
        let st = &mut self.state.settings;
        let held = hotkey::held_now();
        if hotkey::is_modifier(vk.0) {
            st.dialog_held(held);
            return Effect::None;
        }
        let plain = !held.ctrl && !held.alt && !held.win;
        st.focus_visible = true;
        match vk {
            VK_TAB if plain => st.tab(held.shift),
            VK_ESCAPE if plain && !held.shift => st.close_shortcut_dialog(),
            VK_RETURN | VK_SPACE if plain && !held.shift => {
                let hit = settings_view::panel_hit(st.focus);
                return self.activate_settings(hit);
            }
            _ => {
                let candidate = held.with(vk.0);
                st.dialog_pressed(candidate, hotkey::is_free(candidate));
            }
        }
        Effect::None
    }

    fn key_settings(&mut self, vk: VIRTUAL_KEY, shift: bool, _alt: bool) -> Effect {
        if self.state.settings.shortcut_dialog.is_some() {
            return self.key_dialog(vk);
        }
        let st = &mut self.state.settings;
        if vk == VK_TAB {
            st.tab(shift);
            return Effect::None;
        }
        // Escape closes a finished backup's panel (a running backup is stopped only with its button).
        if vk == VK_ESCAPE && matches!(st.backup_panel, Some(BackupPanel::Done { .. })) {
            st.focus_visible = true;
            return self.activate_settings(SHit::PanelDone);
        }
        let press = matches!(vk, VK_RETURN | VK_SPACE);
        let hit = match st.focus {
            SFocus::Nav(i) => match vk {
                VK_UP if i > 0 => {
                    st.focus = SFocus::Nav(i - 1);
                    SHit::None
                }
                VK_DOWN if i + 1 < Nav::ORDER.len() => {
                    st.focus = SFocus::Nav(i + 1);
                    SHit::None
                }
                _ if press => SHit::Nav(i),
                _ => SHit::None,
            },
            // A radio group: arrows choose, as in a browser.
            SFocus::Profile => match vk {
                VK_LEFT | VK_UP => SHit::Profile(0),
                VK_RIGHT | VK_DOWN => SHit::Profile(1),
                _ => SHit::None,
            },
            // Switches toggle with Space (and Enter).
            SFocus::Video if press => SHit::Video,
            SFocus::CloseAfter if press => SHit::CloseAfter,
            SFocus::Autostart if press => SHit::Autostart,
            SFocus::Video | SFocus::CloseAfter | SFocus::Autostart => SHit::None,
            focus if press => match focus {
                SFocus::Close => SHit::Close,
                SFocus::QuickPopup => SHit::QuickPopup,
                SFocus::Change => SHit::Change,
                SFocus::OpenFolder => SHit::OpenFolder,
                SFocus::ChooseFolder => SHit::ChooseFolder,
                SFocus::Backup => SHit::Backup,
                SFocus::Cancel => SHit::Cancel,
                SFocus::Save => SHit::Save,
                SFocus::PanelCancel | SFocus::PanelOpen | SFocus::PanelDone => settings_view::panel_hit(focus),
                SFocus::DialogReset | SFocus::DialogCancel | SFocus::DialogUse => settings_view::panel_hit(focus),
                _ => SHit::None,
            },
            _ => SHit::None,
        };
        st.focus_visible = true;
        self.activate_settings(hit)
    }

    fn insert_text(&mut self, text: &str) {
        if let Some(add) = self.add.as_mut() {
            return add.insert(text);
        }
        if self.state.page == Page::Settings {
            return; // no text boxes on the Settings page
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
        Effect::QuickPopup => {
            hide();
            // Paste goes to the app remembered when the popup last opened, if it still exists.
            let target = crate::app(|a| a.target.borrow().clone());
            crate::popup::open(target);
        }
        Effect::Capture(true) => winutil::set_capture(h),
        Effect::Capture(false) => winutil::release_capture(),
        Effect::Pick(kind) => {
            let source = crate::dialogs::pick(h, kind);
            // SAFETY: our own window; back in front after the picker.
            let _ = unsafe { SetForegroundWindow(h) };
            let effect = with_view(|v| {
                let outcome = v.add.as_mut().map_or(AddOutcome::None, |a| a.picked(source));
                v.add_outcome(outcome)
            });
            if let Some(effect) = effect {
                run(effect);
            }
        }
        Effect::PickBackup => {
            let folder = crate::dialogs::pick_backup_folder(h);
            // SAFETY: our own window; back in front after the picker.
            let _ = unsafe { SetForegroundWindow(h) };
            let note = match folder {
                None => "Backup cancelled: no folder was chosen.".to_string(),
                Some(dest) => match crate::backup::start(dest) {
                    Ok(()) => {
                        with_view(|v| {
                            v.state.settings.backup_running = true;
                            v.state.settings.set_backup_panel(BackupPanel::Running(crate::backup::Step::default()));
                        });
                        "Backing up…".to_string()
                    }
                    Err(e) => format!("Backup failed: {e}"),
                },
            };
            with_view(|v| v.state.settings.show_note(&note));
            winutil::invalidate(h);
        }
        Effect::TileDown => {
            let mut pt = POINT::default();
            // SAFETY: pt is a valid out pointer.
            let _ = unsafe { GetCursorPos(&mut pt) };
            // DragDetect runs its own loop until the pointer passes the system drag
            // threshold (drag) or the button is released (a click: just the selection).
            // SAFETY: our own window; pt is in screen coordinates.
            if unsafe { DragDetect(h, pt) }.as_bool() {
                let id = with_view(|v| library::with(|lib| v.state.selected_item(lib).map(|i| i.id))).flatten();
                if let Some(id) = id {
                    match crate::paste::drag_item(h, id) {
                        crate::paste::Dragged::Copied => {
                            crate::set_status("Drag from the Manager: the target accepted a copy.", crate::StatusKind::Good);
                            show_status("Dropped a copy. The original stays in your library.");
                        }
                        crate::paste::Dragged::NoDrop => crate::set_status("Drag ended without a drop.", crate::StatusKind::Neutral),
                        crate::paste::Dragged::Failed(e) => {
                            crate::set_status(e.clone(), crate::StatusKind::Bad);
                            show_status(&format!("Could not drag: {e}"));
                        }
                    }
                }
            }
        }
        Effect::Copy(id) => {
            if !crate::paste::copy(id) {
                show_status("That item is no longer in the library.");
            }
        }
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
    sync_title(h);
    // While the shortcut chooser is open, pressing the shortcut records it instead of opening the popup.
    let choosing = with_view(|v| v.on_settings() && v.state.settings.shortcut_dialog.is_some()).unwrap_or(false);
    hotkey::set_paused(choosing && is_visible());
    winutil::invalidate(h);
}

/// A worker result. Returns true if the Manager's Add media panel owned it.
pub fn on_job(ev: &crate::jobs::Event) -> bool {
    let effect = with_view(|v| {
        if !v.add.as_ref().is_some_and(|a| a.owns(ev)) {
            return None;
        }
        let outcome = v.add.as_mut()?.on_job(ev);
        Some(v.add_outcome(outcome))
    })
    .flatten();
    let handled = effect.is_some();
    if let Some(effect) = effect {
        run(effect);
    }
    handled
}

fn media_path(rel: &str) -> Option<std::path::PathBuf> {
    crate::paths::get().and_then(|p| crate::import::media_file(&p.media, rel))
}

/// Rename: the file first, then the record. A failure changes nothing.
fn rename_selected(s: &mut ManagerState) {
    if crate::backup::running() {
        s.status = Some(crate::backup::BUSY.into());
        return;
    }
    let plan = library::with(|lib| {
        let id = s.selected.filter(|_| s.can_rename())?;
        let item = lib.get(id)?;
        let new_rel = lib.renamed_path(id, s.rename.text().trim_start())?;
        (item.path != new_rel).then(|| (item.path.clone(), new_rel))
    });
    if let Some((old, new)) = plan {
        let (Some(from), Some(to)) = (media_path(&old), media_path(&new)) else { return };
        if let Some(id) = s.selected {
            crate::preview::release_item(id);
        }
        if let Err(e) = crate::fileops::move_no_replace(&from, &to) {
            s.status = Some(if e.kind() == std::io::ErrorKind::AlreadyExists {
                "A file with that name is already in the Media folder. Choose another name.".into()
            } else {
                format!("Could not rename the file: {e}")
            });
            return;
        }
    }
    library::with_mut(|lib| s.apply_rename(lib));
    if let Err(e) = library::persist() {
        s.status = Some(format!("Renamed, but {e}."));
    }
}

/// Move to Recycle Bin (after the confirmation): the managed copy only.
fn recycle_selected(s: &mut ManagerState) {
    if crate::backup::running() {
        s.cancel_recycle();
        s.status = Some(crate::backup::BUSY.into());
        return;
    }
    let Some((name, rel)) = library::with(|lib| s.selected_item(lib).map(|i| (i.name.clone(), i.path.clone()))) else { return };
    let Some(path) = media_path(&rel) else { return };
    if let Some(id) = s.selected {
        crate::preview::release_item(id);
    }
    match crate::fileops::recycle(&path) {
        Ok(()) => {
            library::with_mut(|lib| s.confirm_recycle(lib));
            if let Err(e) = library::persist() {
                s.status = Some(format!("{name} is in the Recycle Bin, but {e}."));
            }
        }
        Err(e) => s.recycle_failed(&name, &e),
    }
}

/// Open folder in the inspector: File Explorer with the file selected. Under
/// the self-test Explorer is not started; the path is recorded instead.
fn show_selected_in_explorer(s: &ManagerState) -> Option<String> {
    let rel = library::with(|lib| s.selected_item(lib).map(|i| i.path.clone()))?;
    let path = media_path(&rel)?;
    if crate::app(|a| a.quiet.get()) {
        OPENED.with(|o| *o.borrow_mut() = Some(path.display().to_string()));
        return None;
    }
    crate::fileops::show_in_explorer(&path).err().map(|e| format!("Could not open File Explorer: {e}."))
}

/// Opens the real Media folder in File Explorer and returns the line to show.
/// Under the self-test, Explorer is not started (it would take the foreground
/// from the test); the path that would open is recorded for `describe`.
fn open_media_folder() -> String {
    let Some(p) = crate::paths::get() else { return "The Media folder is not set up.".into() };
    open_folder_note(&p.media, "the Media folder")
}

/// Opens `folder` in File Explorer (the self-test only records it) and says how it went.
fn open_folder_note(folder: &std::path::Path, what: &str) -> String {
    if crate::app(|a| a.quiet.get()) {
        OPENED.with(|o| *o.borrow_mut() = Some(folder.display().to_string()));
        return format!("Opened {what}.");
    }
    match winutil::open_folder(folder) {
        Ok(()) => format!("Opened {what}."),
        Err(e) => format!("Could not open {what}: {e}."),
    }
}

/// Window title (taskbar, Alt+Tab) follows the page.
fn sync_title(h: HWND) {
    let Some(page) = with_view(|v| v.state.page) else { return };
    if TITLE_PAGE.with(|t| t.replace(Some(page))) != Some(page) {
        let title = if page == Page::Settings { w!("PocketMedia - Settings") } else { w!("PocketMedia - Library") };
        // SAFETY: our own window; a static string.
        let _ = unsafe { SetWindowTextW(h, title) };
    }
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
            with_view(|v| {
                let dirty = winutil::update_rect(hwnd, v.dpi);
                v.paint(dirty)
            });
            // SAFETY: our own window; marks it painted.
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            // The logo area moves the window, like a normal caption.
            let pt = screen_to_client(hwnd, lparam);
            let caption = with_view(|v| {
                let (x, y) = winutil::to_dip(pt, v.dpi);
                v.is_caption(x, y)
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
                v.s_hover = SHit::None;
                if let Some(add) = v.add.as_mut() {
                    add.hover = AddHit::None;
                }
            });
            crate::preview::hover(Win::Manager, None);
            Effect::None
        }
        // Minimised: nothing is visible, so previews stop like when hidden.
        WM_SIZE if wparam.0 as u32 == SIZE_MINIMIZED => {
            release_previews();
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
        // The shortcut chooser records Alt combinations and shows held modifiers.
        WM_KEYDOWN | WM_SYSKEYDOWN if with_view(|v| v.choosing()).unwrap_or(false) => {
            let vk = VIRTUAL_KEY(wparam.0 as u16);
            with_view(|v| v.key_dialog(vk)).unwrap_or(Effect::None)
        }
        WM_KEYUP | WM_SYSKEYUP | WM_SYSCHAR if with_view(|v| v.choosing()).unwrap_or(false) => {
            if msg != WM_SYSCHAR {
                with_view(|v| v.state.settings.dialog_held(hotkey::held_now()));
            }
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
        // Signing out or shutting down while a backup runs: Windows
        // asks first and shows the reason set on this window. The main window
        // stops the backup if the session ends anyway.
        WM_QUERYENDSESSION => return LRESULT(crate::backup::may_end_session()),
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
        // The title bar is the same on both pages.
        if v.state.page == Page::Settings {
            let f = v.settings_layout().frame;
            let r = match hit {
                MHit::Close => f.close,
                MHit::QuickPopup => f.quick_popup,
                MHit::Nav(i) => *f.nav.get(i)?,
                _ => return None,
            };
            return Some(winutil::rect_center_on_screen(h, v.dpi, r));
        }
        let r = library::with(|lib| {
            let l = v.layout(lib);
            let n = v.state.visible(lib).len();
            let insp = l.insp.as_ref();
            let confirm = insp.and_then(|i| i.confirm.as_ref());
            Some(match hit {
                MHit::Close => l.frame.close,
                MHit::QuickPopup => l.frame.quick_popup,
                MHit::Nav(i) => *l.frame.nav.get(i)?,
                MHit::AddMedia => l.add_media,
                MHit::Search => l.search.outer,
                MHit::SearchClear => l.search.clear,
                MHit::ClearFilters => l.clear_filters?,
                MHit::Sort => l.sort,
                MHit::SortRow(i) => *l.sort_menu.rows.get(i)?,
                MHit::Tile(i) if i < n => l.grid.tile(i, v.state.scroll),
                // A point in the gap between the first two columns: over the gallery, not a tile.
                MHit::Scrollbar => Rect::new(l.grid.tile(0, 0.0).right(), l.grid.area.y + 40.0, 2.0, 1.0),
                MHit::EmptyAction if n == 0 => l.empty.2,
                MHit::RenameInput => insp?.input,
                MHit::Rename => insp?.rename,
                MHit::OpenFolder => insp?.open_folder,
                MHit::Recycle if confirm.is_none() => insp?.recycle,
                MHit::ConfirmMove => confirm?.move_button,
                MHit::ConfirmCancel => confirm?.cancel,
                // A neutral point: the middle of the line under the top bar (no control there).
                MHit::None => Rect::new((l.count.x + l.card.x) / 2.0, l.count.y + l.count.h / 2.0, 1.0, 1.0),
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

/// Screen-space centre of a Settings element (None if the page is not showing).
pub fn settings_element_center(hit: SHit) -> Option<POINT> {
    let h = hwnd();
    with_view(|v| {
        if !v.on_settings() {
            return None;
        }
        let l = v.settings_layout();
        let r = match hit {
            SHit::Close => l.frame.close,
            SHit::QuickPopup => l.frame.quick_popup,
            SHit::Nav(i) => *l.frame.nav.get(i)?,
            SHit::Profile(i) => l.choices.get(i)?.card,
            SHit::Video => l.video.control,
            SHit::Change => l.shortcut.control,
            SHit::CloseAfter => l.close_after.control,
            SHit::OpenFolder => l.folder.control,
            SHit::ChooseFolder => l.import.control,
            SHit::Backup => l.backup.control,
            SHit::Autostart => l.autostart.control,
            SHit::Cancel => l.cancel,
            SHit::Save => l.save,
            SHit::PanelCancel | SHit::PanelOpen | SHit::PanelDone => {
                l.panel.as_ref()?.0.buttons.iter().find(|(f, _)| settings_view::panel_hit(*f) == hit)?.1
            }
            SHit::DialogReset | SHit::DialogCancel | SHit::DialogUse => {
                l.dialog.as_ref()?.0.buttons.iter().find(|(f, _)| settings_view::panel_hit(*f) == hit)?.1
            }
            // A neutral point: the right end of the page heading.
            SHit::None => Rect::new(l.heading.right() - 20.0, l.heading.y + l.heading.h / 2.0, 1.0, 1.0),
            // Test-only: a spot inside the scroll area but beside every control (for the wheel).
            SHit::Caption => Rect::new(l.view.right() - 4.0, l.view.y + l.view.h / 2.0, 1.0, 1.0),
        };
        Some(winutil::rect_center_on_screen(h, v.dpi, r))
    })
    .flatten()
}

/// Self-test only: the chooser checks `sc` as if it had been pressed (for a
/// shortcut another program holds, whose key presses never reach this window).
pub fn test_dialog_press(sc: Shortcut) {
    with_view(|v| v.state.settings.dialog_pressed(sc, hotkey::is_free(sc)));
    winutil::invalidate(hwnd());
}

/// Self-test only: takes the saved settings from the store again (after it was re-read).
pub fn reload_settings() {
    with_view(|v| {
        let page = crate::settings_state::SettingsPage::with_saved(crate::store::settings());
        v.state.settings = page;
    });
    winutil::invalidate(hwnd());
}

/// Self-test only: puts the Settings page at a scroll offset (clamped), to
/// frame the same part of the page as a reference screenshot.
pub fn set_settings_scroll(y: f32) {
    with_view(|v| {
        v.state.settings.scroll = y;
        v.clamp_settings_scroll(false);
    });
    winutil::invalidate(hwnd());
}

/// "screen=manager filter=… selected=… names=a|b" (or "screen=settings …") for self-test checks.
pub fn describe() -> String {
    with_view(|v| {
        if let Some(add) = v.add.as_ref() {
            return format!("host=manager {}", add.describe());
        }
        if v.state.page == Page::Settings {
            let st = &v.state.settings;
            let (d, saved) = (st.draft, st.saved);
            let l = v.settings_layout();
            let visible = |r: Rect| r.y >= l.view.y - 0.5 && r.bottom() <= l.view.bottom() + 0.5;
            return format!(
                "screen=settings profile={:?} video={:?} close_after={} autostart={} saved={:?}/{:?}/{}/{} dirty={} \
                 focus={:?} focus_visible={} dialog={} scroll={:.0} max_scroll={:.0} autostart_visible={} \
                 line={:?} opened={:?} media={:?} nav_selected={} backup_running={} panel={} shortcut={:?} \
                 saved_shortcut={:?} registered={:?} hotkey_ok={}",
                d.profile,
                d.video_preview,
                d.close_after_insert,
                d.autostart,
                saved.profile,
                saved.video_preview,
                saved.close_after_insert,
                saved.autostart,
                st.dirty(),
                st.focus,
                st.focus_visible,
                match &st.shortcut_dialog {
                    None => "none".to_string(),
                    Some(d) => format!(
                        "{}:{}",
                        d.candidate.map(|c| c.label()).unwrap_or_default(),
                        match &d.verdict {
                            crate::settings_state::Verdict::Waiting => "waiting".to_string(),
                            crate::settings_state::Verdict::Free => "free".to_string(),
                            crate::settings_state::Verdict::Refused(e) => format!("refused({e})"),
                        }
                    ),
                },
                st.scroll,
                l.max_scroll(),
                visible(l.autostart.control),
                st.status_line(),
                OPENED.with(|o| o.borrow().clone().unwrap_or_default()),
                crate::paths::media_text(),
                Nav::ORDER.iter().filter(|n| v.state.nav_selected(**n)).map(|n| n.label()).collect::<Vec<_>>().join("+"),
                st.backup_running,
                match &st.backup_panel {
                    None => "none".to_string(),
                    Some(BackupPanel::Running(step)) => format!("running:{}:{}", step.counts(), step.doing()),
                    Some(BackupPanel::Done { title, .. }) => format!("done:{title}"),
                },
                d.shortcut.label(),
                saved.shortcut.label(),
                hotkey::current().label(),
                hotkey::ok(),
            );
        }
        let s = &v.state;
        library::with(|lib| {
            let names: Vec<&str> = s.visible(lib).iter().map(|i| i.name.as_str()).collect();
            let scrollbar = v.layout(lib).grid.scrollbar(names.len(), s.scroll).is_some();
            format!(
                "screen=manager filter={:?} sort={:?} query={:?} focus={:?} selected={:?} rename={:?} error={:?} \
                 can_rename={} confirming={} status={:?} scrollbar={scrollbar} opened={:?} total={} count={} names={}",
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
                OPENED.with(|o| o.borrow().clone().unwrap_or_default()),
                lib.items.len(),
                names.len(),
                names.join("|")
            )
        })
    })
    .unwrap_or_else(|| "screen=manager-not-created".into())
}
