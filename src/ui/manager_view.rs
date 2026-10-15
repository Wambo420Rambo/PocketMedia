//! Manager window: geometry, hit-testing and drawing. A sidebar
//! with the logo, the library entries with counts, Settings and Quick popup;
//! a top bar with search, sort, Add media and ×; the gallery; and the selected
//! item in a card at the right.

use super::d2d::{Align, Gfx, Painter};
use super::layout::{Grid, Rect, TextStyle, empty_parts};
use super::widgets::{
    self, ACCENT, BORDER, ERROR, FIELD, Icon, MUTED, SELECTED, SIDEBAR, SOFT_TEXT, SUBTLE, SURFACE, SearchBox, SearchLook,
    TEXT, TextMetrics, TileLook,
};
use crate::library::{Library, extension};
use crate::manager_state::{MFocus, ManagerState, Nav, Sort};
use crate::popup_state::Empty;
use crate::sample::MediaKind;

/// Default window size.
pub const WIDTH: f32 = 970.0;
pub const HEIGHT: f32 = 700.0;

pub const SIDEBAR_W: f32 = 196.0;
const NAV_H: f32 = 38.0;
const NAV_GAP: f32 = 4.0;
const BAR_Y: f32 = 15.0;
const BAR_H: f32 = 40.0;
const CLOSE: f32 = 36.0;
const SORT_W: f32 = 150.0;
const INFO_H: f32 = 28.0;
const CARD_W: f32 = 232.0;
const CARD_PAD: f32 = 14.0;
const INSPECTOR_THUMB_H: f32 = 150.0;
const INPUT_H: f32 = 38.0;
const BUTTON_H: f32 = 34.0;
const TILE_PAD: f32 = 5.0;

const CONFIRM_NOTE: &str = "You can restore it from the Recycle Bin.";
pub const DROP_HINT: &str = "Drop files or a folder to add";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MHit {
    None,
    /// The sidebar's logo area: drags the window.
    Caption,
    Close,
    QuickPopup,
    Nav(usize),
    AddMedia,
    Search,
    SearchClear,
    ClearFilters,
    Sort,
    SortRow(usize),
    Tile(usize),
    Scrollbar,
    EmptyAction,
    RenameInput,
    Rename,
    OpenFolder,
    Recycle,
    ConfirmMove,
    ConfirmCancel,
}

/// The confirmation shown in place of "Move to Recycle Bin".
pub struct Confirm {
    pub panel: Rect,
    pub question: Rect,
    pub note: Rect,
    pub move_button: Rect,
    pub cancel: Rect,
}

/// The card's controls when an item is selected.
pub struct Inspector {
    pub thumb: Rect,
    pub input: Rect,
    /// Type and pixel size, or the rename error in its place.
    pub meta: Rect,
    pub error: Option<Rect>,
    pub rename: Rect,
    pub open_folder: Rect,
    pub recycle: Rect,
    pub confirm: Option<Confirm>,
}

/// Sidebar and ×, shared by the Library and Settings pages.
pub struct Frame {
    pub width: f32,
    pub height: f32,
    pub sidebar: Rect,
    /// The logo and name; drags the window.
    pub caption: Rect,
    pub logo: Rect,
    pub brand: Rect,
    pub close: Rect,
    /// Hides this window and opens the quick popup.
    pub quick_popup: Rect,
    /// All media, GIFs, Pictures, Videos, then Settings at the foot.
    pub nav: [Rect; 5],
}

pub const QUICK_POPUP: &str = "Quick popup";

/// What the frame has under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameHit {
    Close,
    QuickPopup,
    Caption,
    Nav(usize),
}

impl Frame {
    pub fn compute(width: f32, height: f32, gfx: &Gfx) -> Self {
        let sidebar = Rect::new(1.0, 1.0, SIDEBAR_W, height - 2.0);
        let logo = Rect::new(sidebar.x + 20.0, 1.0 + 18.0, 26.0, 26.0);
        let brand = Rect::new(logo.right() + 10.0, logo.y, gfx.measure("PocketMedia", TextStyle::Brand) + 4.0, logo.h);
        let caption = Rect::new(sidebar.x, sidebar.y, SIDEBAR_W, 62.0);
        let nav_x = sidebar.x + 12.0;
        let nav_w = SIDEBAR_W - 24.0;
        let mut nav = [Rect::default(); 5];
        let mut y = caption.bottom() + 6.0;
        for slot in nav.iter_mut().take(4) {
            *slot = Rect::new(nav_x, y, nav_w, NAV_H);
            y += NAV_H + NAV_GAP;
        }
        let quick_popup = Rect::new(nav_x, sidebar.bottom() - 14.0 - NAV_H, nav_w, NAV_H);
        nav[4] = Rect::new(nav_x, quick_popup.y - NAV_GAP - NAV_H, nav_w, NAV_H);
        let close = Rect::new(width - 1.0 - 14.0 - CLOSE, BAR_Y + (BAR_H - CLOSE) / 2.0, CLOSE, CLOSE);
        Frame { width, height, sidebar, caption, logo, brand, close, quick_popup, nav }
    }

    /// Left edge of the page right of the sidebar.
    pub fn content_x(&self) -> f32 {
        self.sidebar.right() + 20.0
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<FrameHit> {
        if self.close.contains(x, y) {
            Some(FrameHit::Close)
        } else if self.quick_popup.contains(x, y) {
            Some(FrameHit::QuickPopup)
        } else if self.caption.contains(x, y) {
            Some(FrameHit::Caption)
        } else {
            self.nav.iter().position(|r| r.contains(x, y)).map(FrameHit::Nav)
        }
    }
}

/// Pointer and focus state of the frame's controls.
pub struct FrameLook {
    pub hover: Option<FrameHit>,
    pub ring: Option<FrameHit>,
}

const NAV_ICONS: [Icon; 5] = [Icon::All, Icon::Gif, Icon::Picture, Icon::Video, Icon::Settings];

/// Background, sidebar and ×. `counts`: items per library entry (the Library page).
pub fn paint_frame(p: &Painter, f: &Frame, selected: &dyn Fn(Nav) -> bool, counts: Option<[usize; 4]>, look: FrameLook) {
    p.clear(widgets::BG);
    p.fill(f.sidebar, 0.0, SIDEBAR);
    let (lx, ly) = f.logo.center();
    widgets::logo(p, lx, ly, f.logo.w);
    p.text("PocketMedia", f.brand, TextStyle::Brand, TEXT, Align::Start, true);

    let row = |r: Rect, icon: Icon, label: &str, on: bool, hot: bool, ring: bool, count: Option<usize>| {
        if on {
            p.fill(r, 10.0, SELECTED);
        } else if hot {
            p.fill(r, 10.0, widgets::FIELD);
        }
        let ic = if on { ACCENT } else { MUTED };
        widgets::icon(p, icon, Rect::new(r.x + 10.0, r.y, 16.0, r.h), ic);
        let text = Rect::new(r.x + 36.0, r.y, r.w - 36.0 - 40.0, r.h);
        p.text(label, text, TextStyle::Body, if on { TEXT } else { MUTED }, Align::Start, true);
        if let Some(n) = count {
            let cr = Rect::new(r.right() - 48.0, r.y, 38.0, r.h);
            p.text(&n.to_string(), cr, TextStyle::Small, SUBTLE, Align::End, true);
        }
        widgets::ring(p, ring, r, 10.0);
    };
    for (i, (r, nav)) in f.nav.iter().zip(Nav::ORDER).enumerate() {
        let hit = Some(FrameHit::Nav(i));
        let count = counts.and_then(|c| c.get(i).copied());
        row(*r, NAV_ICONS[i], nav.label(), selected(nav), look.hover == hit, look.ring == hit, count);
    }
    let qp = Some(FrameHit::QuickPopup);
    row(f.quick_popup, Icon::Popup, QUICK_POPUP, false, look.hover == qp, look.ring == qp, None);
    let close = Some(FrameHit::Close);
    widgets::close_button(p, f.close, look.hover == close, look.ring == close);
}

/// The window's 1px rounded border, over everything else.
pub fn paint_border(p: &Painter, f: &Frame) {
    p.stroke(Rect::new(0.0, 0.0, f.width, f.height), 8.0, 1.0, BORDER);
}

pub struct ManagerLayout {
    pub frame: Frame,
    pub add_media: Rect,
    pub search: SearchBox,
    pub sort: Rect,
    pub sort_menu: widgets::Menu,
    pub count: Rect,
    pub clear_filters: Option<Rect>,
    pub drop_hint: Rect,
    pub grid: Grid,
    pub empty: (Rect, Rect, Rect),
    pub card: Rect,
    pub insp_heading: Rect,
    pub insp: Option<Inspector>,
    pub status: Option<Rect>,
}

impl ManagerLayout {
    pub fn compute(s: &ManagerState, lib: &Library, width: f32, height: f32, gfx: &Gfx) -> Self {
        let frame = Frame::compute(width, height, gfx);
        let x0 = frame.content_x();

        // Top bar: [search][8][sort][8][Add media][8][×].
        let add_w = widgets::add_button_w(gfx, "Add media");
        let add_media = Rect::new(frame.close.x - 8.0 - add_w, BAR_Y, add_w, BAR_H);
        let sort = Rect::new(add_media.x - 8.0 - SORT_W, BAR_Y, SORT_W, BAR_H);
        let search = SearchBox::at(x0, BAR_Y, sort.x - 8.0 - x0, BAR_H, true);
        let sort_menu = widgets::Menu::below(sort, Sort::BOTH.len());

        // The selected item's card at the right, under the bar.
        let top = BAR_Y + BAR_H + 12.0;
        let bottom = height - 1.0 - 16.0;
        let card = Rect::new(width - 1.0 - 16.0 - CARD_W, top, CARD_W, bottom - top);

        // A line under the bar: count, Clear filters, the drop hint.
        let count_w = gfx.measure(&s.count_label(lib), TextStyle::Medium) + 4.0;
        let count = Rect::new(x0, top, count_w, INFO_H);
        let clear_filters = s.has_filters().then(|| {
            let w = 24.0 + gfx.measure("Clear filters", TextStyle::Small);
            Rect::new(count.right() + 8.0, top, w, INFO_H)
        });
        let right = card.x - 16.0;
        let drop_hint = Rect::new(x0, top, right - x0 - 4.0, INFO_H);

        let gy = top + INFO_H + 8.0;
        let gallery = Rect::new(x0 - TILE_PAD, gy, right - x0 + TILE_PAD, (bottom - gy).max(0.0));
        let grid = Grid::new(gallery);
        let empty = empty_parts(gallery, 32.0 + gfx.measure(Empty::NoMatch.action(), TextStyle::Body));

        let ix = card.x + CARD_PAD;
        let iw = CARD_W - 2.0 * CARD_PAD;
        let insp_heading = Rect::new(ix, card.y + CARD_PAD, iw, TextStyle::Medium.line());
        let mut below = card.y + CARD_PAD;
        let insp = s.selected_item(lib).map(|item| {
            let thumb = Rect::new(ix, card.y + CARD_PAD, iw, INSPECTOR_THUMB_H);
            let input = Rect::new(ix, thumb.bottom() + 12.0, iw, INPUT_H);
            let error = s.rename_error.map(|e| {
                Rect::new(ix, input.bottom() + 6.0, iw, gfx.wrapped_height(e.message(), TextStyle::Small, iw))
            });
            let meta = error.unwrap_or(Rect::new(ix, input.bottom() + 6.0, iw, TextStyle::Small.line()));
            let half = (iw - 8.0) / 2.0;
            let rename = Rect::new(ix, meta.bottom() + 12.0, half, BUTTON_H);
            let open_folder = Rect::new(rename.right() + 8.0, rename.y, half, BUTTON_H);
            let recycle = Rect::new(ix, card.bottom() - CARD_PAD - BUTTON_H, iw, BUTTON_H);
            let confirm = s.confirming.then(|| {
                let pad = 12.0;
                let tw = iw - 2.0 * pad;
                let q = question(&item.name);
                let qh = gfx.wrapped_height(&q, TextStyle::Small, tw);
                let nh = gfx.wrapped_height(CONFIRM_NOTE, TextStyle::Small, tw);
                let h = pad + qh + 4.0 + nh + 10.0 + BUTTON_H + 6.0 + BUTTON_H + pad;
                let panel = Rect::new(ix, card.bottom() - CARD_PAD - h, iw, h);
                let px = ix + pad;
                let question = Rect::new(px, panel.y + pad, tw, qh);
                let note = Rect::new(px, question.bottom() + 4.0, tw, nh);
                let move_button = Rect::new(px, note.bottom() + 10.0, tw, BUTTON_H);
                let cancel = Rect::new(px, move_button.bottom() + 6.0, tw, BUTTON_H);
                Confirm { panel, question, note, move_button, cancel }
            });
            below = rename.bottom();
            Inspector { thumb, input, meta, error, rename, open_folder, recycle, confirm }
        });
        let status = s.status.as_ref().map(|t| {
            let y = if insp.is_some() { below + 14.0 } else { insp_heading.bottom() + 10.0 };
            Rect::new(ix, y, iw, gfx.wrapped_height(t, TextStyle::Small, iw))
        });

        ManagerLayout {
            frame,
            add_media,
            search,
            sort,
            sort_menu,
            count,
            clear_filters,
            drop_hint,
            grid,
            empty,
            card,
            insp_heading,
            insp,
            status,
        }
    }

    pub fn hit(&self, s: &ManagerState, lib: &Library, x: f32, y: f32) -> MHit {
        if s.sort_open.is_some() {
            if let Some(i) = self.sort_menu.hit(x, y) {
                return MHit::SortRow(i);
            }
            if self.sort_menu.frame.contains(x, y) {
                return MHit::None;
            }
        }
        match self.frame.hit(x, y) {
            Some(FrameHit::Close) => return MHit::Close,
            Some(FrameHit::QuickPopup) => return MHit::QuickPopup,
            Some(FrameHit::Caption) => return MHit::Caption,
            Some(FrameHit::Nav(i)) => return MHit::Nav(i),
            None => {}
        }
        let simple = [(self.add_media, MHit::AddMedia), (self.sort, MHit::Sort)];
        if let Some((_, hit)) = simple.iter().find(|(r, _)| r.contains(x, y)) {
            return *hit;
        }
        if self.clear_filters.is_some_and(|r| r.contains(x, y)) {
            return MHit::ClearFilters;
        }
        if !s.search.is_empty() && s.focus == MFocus::Search && self.search.clear.contains(x, y) {
            return MHit::SearchClear;
        }
        if self.search.outer.contains(x, y) {
            return MHit::Search;
        }
        if let Some(i) = &self.insp {
            let confirm = i.confirm.as_ref();
            let controls = [
                (Some(i.input), MHit::RenameInput),
                (Some(i.rename), MHit::Rename),
                (Some(i.open_folder), MHit::OpenFolder),
                (confirm.map(|c| c.move_button), MHit::ConfirmMove),
                (confirm.map(|c| c.cancel), MHit::ConfirmCancel),
                (confirm.is_none().then_some(i.recycle), MHit::Recycle),
            ];
            if let Some((_, hit)) = controls.iter().find(|(r, _)| r.is_some_and(|r| r.contains(x, y))) {
                return *hit;
            }
        }
        let n = s.visible(lib).len();
        if n == 0 {
            return if self.empty.2.contains(x, y) { MHit::EmptyAction } else { MHit::None };
        }
        if self.grid.on_scrollbar(n, s.scroll, x, y) {
            return MHit::Scrollbar;
        }
        self.grid.hit(n, s.scroll, x, y).map_or(MHit::None, MHit::Tile)
    }
}

fn question(name: &str) -> String {
    format!("Move {name} to the Recycle Bin?")
}

/// Pointer and text facts that are not part of `ManagerState`.
pub struct ManagerUi {
    pub hover: MHit,
    pub dpi: f32,
    pub search: TextMetrics,
    pub rename: TextMetrics,
    /// The gallery tile whose full name shows as a tooltip.
    pub tip: Option<usize>,
}

/// Items per sidebar entry: everything, then GIFs, pictures and videos.
pub fn counts(lib: &Library) -> [usize; 4] {
    let n = |k: MediaKind| lib.items.iter().filter(|i| i.kind == k).count();
    [lib.items.len(), n(MediaKind::Gif), n(MediaKind::Picture), n(MediaKind::Video)]
}

pub fn paint(p: &Painter, s: &ManagerState, lib: &Library, l: &ManagerLayout, ui: &ManagerUi) {
    let ring = |f: MFocus| s.focus_visible && s.focus == f;
    let frame_hit = |h: MHit| match h {
        MHit::Close => Some(FrameHit::Close),
        MHit::QuickPopup => Some(FrameHit::QuickPopup),
        MHit::Nav(i) => Some(FrameHit::Nav(i)),
        _ => None,
    };
    let look = FrameLook {
        hover: frame_hit(ui.hover),
        ring: match s.focus {
            MFocus::Close if s.focus_visible => Some(FrameHit::Close),
            MFocus::QuickPopup if s.focus_visible => Some(FrameHit::QuickPopup),
            MFocus::Nav(i) if s.focus_visible => Some(FrameHit::Nav(i)),
            _ => None,
        },
    };
    paint_frame(p, &l.frame, &|n| s.nav_selected(n), Some(counts(lib)), look);

    // Top bar
    let look = SearchLook {
        focused: s.focus == MFocus::Search,
        clear_hovered: ui.hover == MHit::SearchClear,
        placeholder: "Search by name…",
        hint: None,
        style: TextStyle::Body,
    };
    widgets::search_box(p, &l.search, &s.search, ui.search, &look);
    let sort_hot = ui.hover == MHit::Sort || s.sort_open.is_some();
    widgets::select_box(p, l.sort, s.sort.label(), sort_hot, ring(MFocus::Sort));
    widgets::add_button(p, l.add_media, "Add media", ui.hover == MHit::AddMedia, ring(MFocus::AddMedia));

    // The line under it
    p.text(&s.count_label(lib), l.count, TextStyle::Medium, TEXT, Align::Start, true);
    if let Some(r) = l.clear_filters {
        widgets::quiet_button(p, r, "Clear filters", TextStyle::Small, ui.hover == MHit::ClearFilters, true);
        widgets::ring(p, ring(MFocus::ClearFilters), r, widgets::RADIUS);
    }
    p.text(DROP_HINT, l.drop_hint, TextStyle::Small, SUBTLE, Align::End, true);

    gallery(p, s, lib, l, ui);
    inspector(p, s, lib, l, ui, &ring);

    // The full name of the tile the pointer rests on, over the gallery.
    if let Some(i) = ui.tip
        && let Some(item) = s.visible(lib).get(i).copied()
    {
        let tile = l.grid.tile(i, s.scroll);
        if tile.bottom() > l.grid.area.y && tile.y < l.grid.area.bottom() {
            widgets::tooltip(p, &item.name, tile, l.grid.area);
        }
    }
    if let Some(row) = s.sort_open {
        let labels = Sort::BOTH.map(Sort::label);
        widgets::menu(p, &l.sort_menu, &labels, row, s.sort.index());
    }
    paint_border(p, &l.frame);
}

fn gallery(p: &Painter, s: &ManagerState, lib: &Library, l: &ManagerLayout, ui: &ManagerUi) {
    let items = s.visible(lib);
    if items.is_empty() {
        let empty = if lib.items.is_empty() { Empty::NoLibrary } else { Empty::NoMatch };
        let focus = s.focus_visible && s.focus == MFocus::EmptyAction;
        widgets::empty_state(p, l.empty, empty, ui.hover == MHit::EmptyAction, focus);
        return;
    }
    let area = l.grid.area;
    p.push_clip(area);
    let margin = l.grid.row_pitch();
    for (i, item) in items.iter().enumerate() {
        let tile = l.grid.tile(i, s.scroll);
        if tile.bottom() < area.y || tile.y > area.bottom() {
            // Draw only visible tiles; ask early for the next row each way.
            if p.is_full() && tile.bottom() >= area.y - margin && tile.y <= area.bottom() + margin {
                widgets::prefetch_tile(item, tile, ui.dpi);
            }
            continue;
        }
        if !p.touches(tile) {
            continue; // a partial frame for another tile
        }
        let selected = s.selected == Some(item.id);
        let look = TileLook {
            selected,
            hovered: ui.hover == MHit::Tile(i),
            ring: selected && s.focus_visible && s.focus == MFocus::Gallery,
        };
        widgets::tile(p, item, tile, look, ui.dpi);
    }
    if let Some(bar) = l.grid.scrollbar(items.len(), s.scroll) {
        widgets::scrollbar(p, bar, ui.hover == MHit::Scrollbar);
    }
    p.pop_clip();
}

fn inspector(p: &Painter, s: &ManagerState, lib: &Library, l: &ManagerLayout, ui: &ManagerUi, ring: &dyn Fn(MFocus) -> bool) {
    p.fill(l.card, 14.0, SURFACE);
    let (Some(item), Some(i)) = (s.selected_item(lib), l.insp.as_ref()) else {
        p.text("Select an item", l.insp_heading, TextStyle::Medium, MUTED, Align::Start, true);
        p.text_wrapped(
            "Its preview, name and actions show here.",
            l.insp_heading.x,
            l.insp_heading.bottom() + 2.0,
            l.insp_heading.w,
            TextStyle::Small,
            SUBTLE,
        );
        status(p, s, l);
        return;
    };
    widgets::thumb(p, item, i.thumb, ui.dpi);

    // Name field: editable name, then the fixed extension in muted text.
    let input = i.input;
    let focused = s.focus == MFocus::RenameInput;
    p.fill(input, widgets::RADIUS, FIELD);
    let text_r = Rect::new(input.x + 12.0, input.y, input.w - 24.0, input.h);
    let m = ui.rename;
    p.push_clip(text_r);
    let x = text_r.x - m.offset;
    if focused && s.rename.all_selected() {
        p.fill(Rect::new(x, input.y + 9.0, m.selection_w, input.h - 18.0), 0.0, ACCENT.alpha(0.35));
    }
    p.text(s.rename.text(), Rect::new(x, text_r.y, 10_000.0, text_r.h), TextStyle::Body, TEXT, Align::Start, true);
    let ext_x = x + m.selection_w;
    p.text(extension(&item.name), Rect::new(ext_x, text_r.y, 200.0, text_r.h), TextStyle::Body, SUBTLE, Align::Start, true);
    p.pop_clip();
    if s.rename_error.is_some() {
        p.stroke(input, widgets::RADIUS, 1.5, ERROR);
    } else if focused {
        p.stroke(input, widgets::RADIUS, 2.0, ACCENT);
    }
    if focused {
        let cx = (x + m.caret_x).round() + 0.5;
        p.line(cx, input.y + 10.0, cx, input.bottom() - 10.0, 1.0, TEXT);
    }
    match (i.error, s.rename_error) {
        (Some(r), Some(e)) => p.text_wrapped(e.message(), r.x, r.y, r.w, TextStyle::Small, ERROR),
        _ => {
            let size = if item.width > 0 && item.height > 0 { format!(" · {} × {}", item.width, item.height) } else { String::new() };
            p.text(&format!("{}{size}", item.kind.badge()), i.meta, TextStyle::Small, SUBTLE, Align::Start, true);
        }
    }

    let can = s.can_rename();
    if can {
        widgets::button(p, i.rename, &["Rename"], ui.hover == MHit::Rename, ring(MFocus::Rename));
    } else {
        p.fill(i.rename, widgets::RADIUS, widgets::RAISED.alpha(0.5));
        p.text("Rename", i.rename, TextStyle::Body, MUTED.alpha(widgets::DISABLED), Align::Center, true);
        widgets::ring(p, ring(MFocus::Rename), i.rename, widgets::RADIUS);
    }
    widgets::button(p, i.open_folder, &["Open folder"], ui.hover == MHit::OpenFolder, ring(MFocus::OpenFolder));

    match &i.confirm {
        None => widgets::danger_button(p, i.recycle, "Move to Recycle Bin", ui.hover == MHit::Recycle, ring(MFocus::Recycle)),
        Some(c) => {
            p.fill(c.panel, 10.0, widgets::FIELD);
            p.stroke(c.panel, 10.0, 1.0, widgets::DANGER.alpha(0.6));
            p.text_wrapped(&question(&item.name), c.question.x, c.question.y, c.question.w, TextStyle::Small, TEXT);
            p.text_wrapped(CONFIRM_NOTE, c.note.x, c.note.y, c.note.w, TextStyle::Small, MUTED);
            widgets::danger_fill_button(p, c.move_button, "Move to Recycle Bin", ui.hover == MHit::ConfirmMove, ring(MFocus::ConfirmMove));
            widgets::quiet_button(p, c.cancel, "Cancel", TextStyle::Body, ui.hover == MHit::ConfirmCancel, true);
            widgets::ring(p, ring(MFocus::ConfirmCancel), c.cancel, widgets::RADIUS);
        }
    }
    status(p, s, l);
}

fn status(p: &Painter, s: &ManagerState, l: &ManagerLayout) {
    if let (Some(text), Some(r)) = (&s.status, l.status) {
        p.text_wrapped(text, r.x, r.y, r.w, TextStyle::Small, SOFT_TEXT);
    }
}
