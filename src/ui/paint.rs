//! Draws the quick popup from `PopupState`, the shared `Library` and `Layout`.

use super::d2d::{Align, Painter};
use super::layout::{Hit, Layout, Rect, TextStyle};
use super::widgets::{self, ACCENT, BORDER, DANGER, MUTED, ON_ACCENT, RAISED, SearchBox, TEXT, TextMetrics, TileLook};
use crate::library::Library;
use crate::popup_state::{Focus, ListKind, PopupState, TypeFilter};

/// Pointer and window facts that are not part of `PopupState`.
pub struct Ui {
    pub hover: Hit,
    pub dpi: f32,
    pub shortcut_ok: bool,
    pub shortcut_label: &'static str,
    pub search: TextMetrics,
    pub plus_w: f32,
}

pub fn search_box(l: &Layout) -> SearchBox {
    SearchBox { outer: l.search, input: l.search_input, clear: l.search_clear }
}

pub fn paint(p: &Painter, s: &PopupState, lib: &Library, l: &Layout, ui: &Ui) {
    p.clear(widgets::BG);
    let ring = |f: Focus| s.focus_visible && s.focus == f;

    // Title bar
    widgets::title_bar(p, l.titlebar, l.title, l.close, ui.hover == Hit::Close, ring(Focus::Close));
    if ui.shortcut_ok {
        p.text(ui.shortcut_label, l.shortcut, TextStyle::Small, MUTED, Align::Start, true);
    } else {
        let r = Rect::new(l.shortcut.x - 40.0, l.shortcut.y, l.shortcut.w + 80.0, l.shortcut.h);
        p.text("Shortcut unavailable", r, TextStyle::Small, DANGER, Align::Center, true);
    }

    // Open manager / Add media
    let om = l.open_manager;
    if ui.hover == Hit::OpenManager {
        p.stroke(om, 6.0, 1.0, ACCENT);
    }
    // 1px border + 11px padding + empty icon slot + 7px gap.
    p.text("Open manager", Rect::new(om.x + 19.0, om.y, om.w - 19.0, om.h), TextStyle::Body, TEXT, Align::Start, true);
    widgets::ring(p, ring(Focus::OpenManager), om, 6.0);
    widgets::add_media_button(p, l.add_media, ui.plus_w);
    widgets::ring(p, ring(Focus::AddMedia), l.add_media, 6.0);

    widgets::search_box(p, &search_box(l), &s.search, s.focus == Focus::Search, ui.search, ui.hover == Hit::SearchClear);

    // Type chips
    p.text("Type", l.type_label, TextStyle::Small, MUTED, Align::Start, true);
    for (i, (chip, filter)) in l.chips.iter().zip(TypeFilter::ORDER).enumerate() {
        if s.filter == filter {
            p.fill(*chip, 6.0, ACCENT);
            p.text(filter.label(), *chip, TextStyle::Small, ON_ACCENT, Align::Center, true);
        } else {
            p.fill(*chip, 6.0, RAISED);
            p.stroke(*chip, 6.0, 1.0, if ui.hover == Hit::Chip(i) { ACCENT } else { BORDER });
            p.text(filter.label(), *chip, TextStyle::Small, TEXT, Align::Center, true);
        }
        widgets::ring(p, ring(Focus::Type(i)), *chip, 6.0);
    }

    // Count and list
    p.text(&s.count_label(lib), l.count, TextStyle::Medium, TEXT, Align::Start, true);
    let open = ui.hover == Hit::List || s.dropdown.is_some();
    widgets::select_box(p, l.list, s.list.label(), open, ring(Focus::List));

    gallery(p, s, lib, l, ui);

    // Footer
    p.line(l.footer.x, l.footer.y + 0.5, l.footer.right(), l.footer.y + 0.5, 1.0, BORDER);
    let hint = s.status.as_deref().unwrap_or("Click / Enter to paste · Drag to attach · Esc to close");
    p.text(hint, l.footer_text, TextStyle::Small, MUTED, Align::Start, true);
    let enabled = s.history_enabled(lib);
    widgets::quiet_button(p, l.clear_history, "Clear history", TextStyle::Small, ui.hover == Hit::ClearHistory, enabled);
    widgets::ring(p, ring(Focus::ClearHistory), l.clear_history, 6.0);

    if let Some(row) = s.dropdown {
        let menu = widgets::Menu { frame: l.dropdown, rows: l.dropdown_rows.to_vec() };
        let labels = ListKind::BOTH.map(ListKind::label);
        let current = ListKind::BOTH.iter().position(|k| *k == s.list).unwrap_or(0);
        widgets::menu(p, &menu, &labels, row, current);
    }
    p.stroke(Rect::new(0.0, 0.0, l.width, l.height), 8.0, 1.0, BORDER);
}

fn gallery(p: &Painter, s: &PopupState, lib: &Library, l: &Layout, ui: &Ui) {
    if let Some(empty) = s.empty(lib) {
        let focus = s.focus_visible && s.focus == Focus::EmptyAction;
        widgets::empty_state(p, l.empty_parts(empty.action()), empty, ui.hover == Hit::EmptyAction, focus);
        return;
    }
    let items = s.visible(lib);
    let selected = s.selected_index(lib);
    let grid = l.grid();
    p.push_clip(l.gallery);
    for (i, item) in items.iter().enumerate() {
        let tile = grid.tile(i, s.scroll);
        if tile.bottom() < l.gallery.y || tile.y > l.gallery.bottom() {
            continue; // draw only visible tiles
        }
        let is_selected = s.focus == Focus::Gallery && selected == Some(i);
        let look = TileLook { selected: is_selected, hovered: ui.hover == Hit::Tile(i), ring: is_selected && s.focus_visible };
        widgets::tile(p, item, tile, look, ui.dpi);
    }
    if let Some(bar) = grid.scrollbar(items.len(), s.scroll) {
        widgets::scrollbar(p, bar, ui.hover == Hit::Scrollbar);
    }
    p.pop_clip();
}
