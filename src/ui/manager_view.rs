//! Manager window: geometry, hit-testing and drawing. Numbers come from the
//! wireframe CSS (.pm-layout 152 | 1fr | 198, .pm-sidebar, .pm-main,
//! .pm-inspector) and were checked against focused-20 in screenshots-dark.

use super::d2d::{Align, Gfx, Painter};
use super::layout::{Grid, Rect, TextStyle, empty_parts, text_button_w};
use super::widgets::{
    self, ACCENT, BG, BORDER, DANGER, ERROR, MUTED, SIDEBAR, SOFT, SOFT_TEXT, SURFACE, SearchBox, TEXT,
    TextMetrics, TileLook,
};
use crate::library::{Library, extension};
use crate::manager_state::{MFocus, ManagerState, Nav, Sort};
use crate::popup_state::Empty;

/// Default window size (CSS: max-width 970px, height 700px).
pub const WIDTH: f32 = 970.0;
pub const HEIGHT: f32 = 700.0;

const TITLE_H: f32 = 46.0; // includes its 1px bottom border
const SIDEBAR_W: f32 = 152.0; // includes its 1px right border
const INSPECTOR_W: f32 = 198.0; // includes its 1px left border
const FOOTER_H: f32 = 1.0 + 13.0 + 12.0 * 1.45 + 13.0;
const BUTTON_H: f32 = 36.0;
const SELECT_W: f32 = 142.0;
const SELECT_H: f32 = 28.0;
const NAV_GAP: f32 = 5.0;
const INSPECTOR_THUMB_H: f32 = 115.0;
const INPUT_H: f32 = 9.0 + 9.0 + 14.0 * 1.45 + 2.0;

pub const LEAD: &str = "Find media by name. Rename a file to make it easier to find.";
const CONFIRM_NOTE: &str = "You can restore it from the Recycle Bin.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MHit {
    None,
    /// The title bar outside the × button: drags the window.
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

/// The inspector's controls when an item is selected.
pub struct Inspector {
    pub thumb: Rect,
    pub name_label: Rect,
    pub input: Rect,
    pub error: Option<Rect>,
    pub rename: Rect,
    pub open_folder: Rect,
    pub recycle: Rect,
    pub confirm: Option<Confirm>,
}

/// Title bar and sidebar, shared by the Library and Settings pages.
pub struct Frame {
    pub width: f32,
    pub height: f32,
    pub titlebar: Rect,
    pub title: Rect,
    pub caption: Rect,
    pub close: Rect,
    /// "Quick popup": hides this window and opens the popup (not in the wireframe;
    /// asked for by the owner).
    pub quick_popup: Rect,
    pub sidebar: Rect,
    pub labels: [Rect; 2],
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
    /// `bottom` is where the sidebar ends (the footer's top, or the window's inner bottom).
    pub fn compute(width: f32, height: f32, caption: &str, bottom: f32, gfx: &Gfx) -> Self {
        let titlebar = Rect::new(1.0, 1.0, width - 2.0, TITLE_H);
        let title_w = gfx.measure("PocketMedia", TextStyle::Medium);
        let title = Rect::new(1.0 + 14.0 + 9.0, 1.0, title_w, TITLE_H - 1.0);
        let caption = Rect::new(title.right() + 9.0, 1.0, gfx.measure(caption, TextStyle::Small) + 2.0, TITLE_H - 1.0);
        let close = Rect::new(width - 1.0 - 14.0 - 32.0, 1.0 + (TITLE_H - 1.0 - 28.0) / 2.0, 32.0, 28.0);
        let qw = text_button_w(gfx.measure(QUICK_POPUP, TextStyle::Small));
        let quick_popup = Rect::new(close.x - 8.0 - qw, close.y, qw, close.h);

        // Sidebar: padding 18 10; labels 11px with margins 12 8 8; buttons 36 + 5.
        let body_y = Self::body_y();
        let sidebar = Rect::new(1.0, body_y, SIDEBAR_W, bottom - body_y);
        let nav_x = sidebar.x + 10.0;
        let nav_w = SIDEBAR_W - 1.0 - 20.0;
        let label = |y: f32| Rect::new(nav_x + 8.0, y + 12.0, nav_w - 16.0, TextStyle::Tiny.line());
        let lib_label = label(body_y + 18.0);
        let mut y = lib_label.bottom() + 8.0;
        let all = Rect::new(nav_x, y, nav_w, BUTTON_H);
        let types_label = label(all.bottom() + NAV_GAP);
        y = types_label.bottom() + 8.0;
        let mut nav = [all; 5];
        for slot in nav.iter_mut().skip(1) {
            *slot = Rect::new(nav_x, y, nav_w, BUTTON_H);
            y += BUTTON_H + NAV_GAP;
        }
        Frame { width, height, titlebar, title, caption, close, quick_popup, sidebar, labels: [lib_label, types_label], nav }
    }

    /// Top of everything under the title bar.
    pub fn body_y() -> f32 {
        1.0 + TITLE_H
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<FrameHit> {
        if self.close.contains(x, y) {
            Some(FrameHit::Close)
        } else if self.quick_popup.contains(x, y) {
            Some(FrameHit::QuickPopup)
        } else if self.titlebar.contains(x, y) {
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

/// Background, border, title bar and sidebar. Call first; `paint_border` last.
pub fn paint_frame(p: &Painter, f: &Frame, caption: &str, selected: &dyn Fn(Nav) -> bool, look: FrameLook) {
    p.clear(BG);
    let close = Some(FrameHit::Close);
    widgets::title_bar(p, f.titlebar, f.title, f.close, look.hover == close, look.ring == close);
    let qp = Some(FrameHit::QuickPopup);
    widgets::quiet_button(p, f.quick_popup, QUICK_POPUP, TextStyle::Small, look.hover == qp, true);
    widgets::ring(p, look.ring == qp, f.quick_popup, 6.0);
    p.text(caption, f.caption, TextStyle::Small, MUTED, Align::Start, true);

    p.fill(f.sidebar, 0.0, SIDEBAR);
    p.line(f.sidebar.right() - 0.5, f.sidebar.y, f.sidebar.right() - 0.5, f.sidebar.bottom(), 1.0, BORDER);
    // CSS letter-spacing: .06em of 11px.
    let spacing = TextStyle::Tiny.size() * 0.06;
    p.text_spaced("LIBRARY", f.labels[0], TextStyle::Tiny, MUTED, spacing);
    p.text_spaced("MEDIA TYPES", f.labels[1], TextStyle::Tiny, MUTED, spacing);
    for (i, (r, nav)) in f.nav.iter().zip(Nav::ORDER).enumerate() {
        let on = selected(nav);
        if on {
            p.fill(*r, 6.0, SOFT);
        }
        if look.hover == Some(FrameHit::Nav(i)) {
            p.stroke(*r, 6.0, 1.0, ACCENT);
        }
        let text = Rect::new(r.x + 19.0, r.y, r.w - 26.0, r.h);
        p.text(nav.label(), text, TextStyle::Body, if on { SOFT_TEXT } else { TEXT }, Align::Start, true);
        widgets::ring(p, look.ring == Some(FrameHit::Nav(i)), *r, 6.0);
    }
}

/// The window's 1px rounded border, over everything else.
pub fn paint_border(p: &Painter, f: &Frame) {
    p.stroke(Rect::new(0.0, 0.0, f.width, f.height), 8.0, 1.0, BORDER);
}

pub struct ManagerLayout {
    pub frame: Frame,
    pub heading: Rect,
    pub add_media: Rect,
    pub lead: Rect,
    pub search: SearchBox,
    pub count: Rect,
    pub clear_filters: Option<Rect>,
    pub sort: Rect,
    pub sort_menu: widgets::Menu,
    pub grid: Grid,
    pub empty: (Rect, Rect, Rect),
    pub inspector: Rect,
    pub insp_heading: Rect,
    pub insp: Option<Inspector>,
    pub status: Option<Rect>,
    pub footer: Rect,
    pub footer_text: Rect,
    pub plus_w: f32,
}

impl ManagerLayout {
    pub fn compute(s: &ManagerState, lib: &Library, width: f32, height: f32, gfx: &Gfx) -> Self {
        let body_y = Frame::body_y();
        let footer = Rect::new(1.0, height - 1.0 - FOOTER_H, width - 2.0, FOOTER_H);
        // 2px above the CSS box: matches where Chrome puts the glyphs (as in the popup).
        let footer_text = Rect::new(footer.x + 20.0, footer.y + 1.0 + 11.0, footer.w - 40.0, TextStyle::Small.line());
        let body_h = footer.y - body_y;
        let frame = Frame::compute(width, height, "Library", footer.y, gfx);

        // Main column: padding 20.
        let main = Rect::new(frame.sidebar.right(), body_y, width - 2.0 - SIDEBAR_W - INSPECTOR_W, body_h);
        let x0 = main.x + 20.0;
        let inner_w = main.w - 40.0;
        let top = body_y + 20.0;
        let plus_w = gfx.measure("+", TextStyle::Body);
        let add_w = 2.0 + 22.0 + plus_w + 7.0 + gfx.measure("Add media", TextStyle::Body);
        let add_media = Rect::new(x0 + inner_w - add_w, top, add_w, BUTTON_H);
        let heading = Rect::new(x0, top, inner_w - add_w - 8.0, BUTTON_H);
        let lead = Rect::new(x0, add_media.bottom() + 12.0 + 5.0, inner_w, TextStyle::Small.line());
        let search = SearchBox::at(x0, lead.bottom() + 18.0, inner_w);

        // Section line: count · [Clear filters] · sort, spread out (space-between).
        let section_y = search.outer.bottom() + 16.0;
        let line_h = if s.has_filters() { BUTTON_H } else { SELECT_H };
        let count_label = s.count_label(lib);
        let count_w = gfx.measure(&count_label, TextStyle::Medium);
        let count = Rect::new(x0, section_y, count_w + 4.0, line_h);
        let sort = Rect::new(x0 + inner_w - SELECT_W, section_y + (line_h - SELECT_H) / 2.0, SELECT_W, SELECT_H);
        let clear_filters = s.has_filters().then(|| {
            let w = text_button_w(gfx.measure("Clear filters", TextStyle::Body));
            let free = inner_w - count_w - w - SELECT_W;
            Rect::new(x0 + count_w + free / 2.0, section_y, w, BUTTON_H)
        });
        let sort_menu = widgets::Menu::below(sort, Sort::BOTH.len());

        let gallery_y = section_y + line_h + 10.0;
        let gallery = Rect::new(x0, gallery_y, inner_w, (footer.y - 20.0 - gallery_y).max(0.0));
        let grid = Grid::new(gallery);
        let empty = empty_parts(gallery, text_button_w(gfx.measure(Empty::NoMatch.action(), TextStyle::Body)));

        // Inspector: padding 18 14.
        let inspector = Rect::new(main.right(), body_y, INSPECTOR_W, body_h);
        let ix = inspector.x + 1.0 + 14.0;
        let iw = INSPECTOR_W - 1.0 - 28.0;
        let insp_heading = Rect::new(ix, body_y + 18.0, iw, TextStyle::Medium.line());
        let mut bottom = insp_heading.bottom();
        let insp = s.selected_item(lib).map(|item| {
            let thumb = Rect::new(ix, insp_heading.bottom() + 14.0, iw, INSPECTOR_THUMB_H);
            let name_label = Rect::new(ix, thumb.bottom() + 16.0, iw, TextStyle::Small.line());
            let input = Rect::new(ix, name_label.bottom() + 6.0, iw, INPUT_H);
            let error = s.rename_error.map(|e| {
                Rect::new(ix, input.bottom() + 4.0, iw, gfx.wrapped_height(e.message(), TextStyle::Small, iw))
            });
            let rename_y = error.map_or(input.bottom(), |r| r.bottom() + 2.0);
            let rename = Rect::new(ix, rename_y, text_button_w(gfx.measure("Rename", TextStyle::Body)), BUTTON_H);
            let open_folder = Rect::new(ix, rename.bottom() + 14.0, iw, BUTTON_H);
            let recycle = Rect::new(ix, open_folder.bottom() + 14.0, iw, BUTTON_H);
            let confirm = s.confirming.then(|| {
                let pad = 12.0;
                let tw = iw - 2.0 - 2.0 * pad;
                let q = question(&item.name);
                let qh = gfx.wrapped_height(&q, TextStyle::Small, tw);
                let nh = gfx.wrapped_height(CONFIRM_NOTE, TextStyle::Small, tw);
                let px = ix + 1.0 + pad;
                let question = Rect::new(px, recycle.y + 1.0 + pad, tw, qh);
                let note = Rect::new(px, question.bottom() + 4.0, tw, nh);
                let move_button = Rect::new(px, note.bottom() + 10.0, tw, BUTTON_H);
                let cancel = Rect::new(px, move_button.bottom() + 6.0, tw, BUTTON_H);
                let panel = Rect::new(ix, recycle.y, iw, cancel.bottom() + pad + 1.0 - recycle.y);
                Confirm { panel, question, note, move_button, cancel }
            });
            bottom = confirm.as_ref().map_or(recycle.bottom(), |c| c.panel.bottom());
            Inspector { thumb, name_label, input, error, rename, open_folder, recycle, confirm }
        });
        let status = s
            .status
            .as_ref()
            .map(|t| Rect::new(ix, bottom + 14.0, iw, gfx.wrapped_height(t, TextStyle::Small, iw)));

        ManagerLayout {
            frame,
            heading,
            add_media,
            lead,
            search,
            count,
            clear_filters,
            sort,
            sort_menu,
            grid,
            empty,
            inspector,
            insp_heading,
            insp,
            status,
            footer,
            footer_text,
            plus_w,
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
    paint_frame(p, &l.frame, "Library", &|n| s.nav_selected(n), look);

    // Main column
    p.text("Your library", l.heading, TextStyle::H1, TEXT, Align::Start, true);
    widgets::add_media_button(p, l.add_media, l.plus_w);
    widgets::ring(p, ring(MFocus::AddMedia), l.add_media, 6.0);
    p.text(LEAD, l.lead, TextStyle::Small, MUTED, Align::Start, true);
    widgets::search_box(p, &l.search, &s.search, s.focus == MFocus::Search, ui.search, ui.hover == MHit::SearchClear);
    p.text(&s.count_label(lib), l.count, TextStyle::Medium, TEXT, Align::Start, true);
    if let Some(r) = l.clear_filters {
        widgets::quiet_button(p, r, "Clear filters", TextStyle::Body, ui.hover == MHit::ClearFilters, true);
        widgets::ring(p, ring(MFocus::ClearFilters), r, 6.0);
    }
    let sort_hot = ui.hover == MHit::Sort || s.sort_open.is_some();
    widgets::select_box(p, l.sort, s.sort.label(), sort_hot, ring(MFocus::Sort));
    gallery(p, s, lib, l, ui);

    inspector(p, s, lib, l, ui, &ring);

    // Footer: the whole library, not the filtered view.
    p.line(l.footer.x, l.footer.y + 0.5, l.footer.right(), l.footer.y + 0.5, 1.0, BORDER);
    let n = lib.items.len();
    let total = format!("{n} item{}", if n == 1 { "" } else { "s" });
    p.text(&total, l.footer_text, TextStyle::Small, MUTED, Align::Start, true);
    p.text("Drop files or a folder to add", l.footer_text, TextStyle::Small, MUTED, Align::End, true);

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
    for (i, item) in items.iter().enumerate() {
        let tile = l.grid.tile(i, s.scroll);
        if tile.bottom() < area.y || tile.y > area.bottom() {
            continue; // draw only visible tiles
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
    p.fill(l.inspector, 0.0, SIDEBAR);
    p.line(l.inspector.x + 0.5, l.inspector.y, l.inspector.x + 0.5, l.inspector.bottom(), 1.0, BORDER);
    let (Some(item), Some(i)) = (s.selected_item(lib), l.insp.as_ref()) else {
        p.text("Select an item", l.insp_heading, TextStyle::Medium, TEXT, Align::Start, true);
        status(p, s, l);
        return;
    };
    p.text("Selected item", l.insp_heading, TextStyle::Medium, TEXT, Align::Start, true);
    widgets::thumb(p, item, i.thumb, ui.dpi);
    p.text("Name", i.name_label, TextStyle::Small, MUTED, Align::Start, true);

    // Name field: editable name, then the fixed extension in muted text.
    let input = i.input;
    let focused = s.focus == MFocus::RenameInput;
    p.fill(input, 6.0, SURFACE);
    p.stroke(input, 6.0, 1.0, BORDER);
    let text_r = Rect::new(input.x + 1.0 + 10.0, input.y, input.w - 22.0, input.h);
    let m = ui.rename;
    p.push_clip(text_r);
    let x = text_r.x - m.offset;
    if focused && s.rename.all_selected() {
        p.fill(Rect::new(x, input.y + 9.0, m.selection_w, input.h - 18.0), 0.0, ACCENT.alpha(0.35));
    }
    p.text(s.rename.text(), Rect::new(x, text_r.y, 10_000.0, text_r.h), TextStyle::Body, TEXT, Align::Start, true);
    let ext_x = x + m.selection_w;
    p.text(extension(&item.name), Rect::new(ext_x, text_r.y, 200.0, text_r.h), TextStyle::Body, MUTED, Align::Start, true);
    p.pop_clip();
    if s.rename_error.is_some() {
        p.stroke(input.inset(-1.0), 7.0, 1.0, ERROR);
    } else if focused {
        p.stroke(input, 6.0, 2.0, widgets::RING);
    }
    if focused {
        let cx = (x + m.caret_x).round() + 0.5;
        p.line(cx, input.y + 10.0, cx, input.bottom() - 10.0, 1.0, TEXT);
    }
    if let (Some(r), Some(e)) = (i.error, s.rename_error) {
        p.text_wrapped(e.message(), r.x, r.y, r.w, TextStyle::Small, ERROR);
    }

    widgets::quiet_button(p, i.rename, "Rename", TextStyle::Body, ui.hover == MHit::Rename, s.can_rename());
    widgets::ring(p, ring(MFocus::Rename), i.rename, 6.0);
    widgets::raised_button(p, i.open_folder, "Open folder", TextStyle::Small, ui.hover == MHit::OpenFolder);
    widgets::ring(p, ring(MFocus::OpenFolder), i.open_folder, 6.0);

    match &i.confirm {
        None => {
            widgets::raised_button(p, i.recycle, "Move to Recycle Bin", TextStyle::Small, ui.hover == MHit::Recycle);
            widgets::ring(p, ring(MFocus::Recycle), i.recycle, 6.0);
        }
        Some(c) => {
            p.fill(c.panel, 6.0, SURFACE);
            p.stroke(c.panel, 6.0, 1.0, DANGER);
            p.text_wrapped(&question(&item.name), c.question.x, c.question.y, c.question.w, TextStyle::Small, TEXT);
            p.text_wrapped(CONFIRM_NOTE, c.note.x, c.note.y, c.note.w, TextStyle::Small, MUTED);
            // Destructive button (CSS .pm-destructive).
            let alpha = if ui.hover == MHit::ConfirmMove { 0.85 } else { 1.0 };
            p.fill(c.move_button, 6.0, DANGER.alpha(alpha));
            p.text("Move to Recycle Bin", c.move_button, TextStyle::Small, BG, Align::Center, true);
            widgets::ring(p, ring(MFocus::ConfirmMove), c.move_button, 6.0);
            widgets::quiet_button(p, c.cancel, "Cancel", TextStyle::Small, ui.hover == MHit::ConfirmCancel, true);
            widgets::ring(p, ring(MFocus::ConfirmCancel), c.cancel, 6.0);
        }
    }
    status(p, s, l);
}

fn status(p: &Painter, s: &ManagerState, l: &ManagerLayout) {
    if let (Some(text), Some(r)) = (&s.status, l.status) {
        p.text_wrapped(text, r.x, r.y, r.w, TextStyle::Small, MUTED);
    }
}
