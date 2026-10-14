//! Draws the quick popup from `PopupState`, the shared `Library` and `Layout`.

use super::d2d::{Align, Painter};
use super::layout::{CHIP_LABELS, FOOTER_ADD, FOOTER_MANAGER, Hit, Layout, Rect, TAB_LABELS, TextStyle};
use super::widgets::{self, ACCENT, BG, BORDER, DANGER, LINE, MUTED, SIDEBAR, SUBTLE, SearchLook, TEXT, TextMetrics, TileLook};
use crate::library::Library;
use crate::popup_state::{Focus, ListKind, PopupState, TypeFilter};

/// Pointer and window facts that are not part of `PopupState`.
pub struct Ui {
    pub hover: Hit,
    pub dpi: f32,
    pub shortcut_ok: bool,
    pub shortcut_label: String,
    pub search: TextMetrics,
}

pub const HINT: &str = "Enter to paste · Drag to attach";

pub fn paint(p: &Painter, s: &PopupState, lib: &Library, l: &Layout, ui: &Ui) {
    p.clear(BG);
    let ring = |f: Focus| s.focus_visible && s.focus == f;

    // Header: logo, search (with the shortcut while it is empty), ×.
    let (lx, ly) = l.logo.center();
    widgets::logo(p, lx, ly, l.logo.w);
    let hint = if ui.shortcut_ok { (ui.shortcut_label.as_str(), SUBTLE) } else { ("Shortcut unavailable", DANGER) };
    let look = SearchLook {
        focused: s.focus == Focus::Search,
        clear_hovered: ui.hover == Hit::SearchClear,
        placeholder: "Search your media…",
        hint: Some(hint),
        style: TextStyle::Search,
    };
    widgets::search_box(p, &l.search_box(), &s.search, ui.search, &look);
    widgets::close_button(p, l.close, ui.hover == Hit::Close, ring(Focus::Close));

    // Type: a segmented control.
    let chosen = TypeFilter::ORDER.iter().position(|f| *f == s.filter).unwrap_or(0);
    let hover = match ui.hover {
        Hit::Chip(i) => Some(i),
        _ => None,
    };
    let focus = match s.focus {
        Focus::Type(i) if s.focus_visible => Some(i),
        _ => None,
    };
    widgets::segmented(p, l.chips_track, &l.chips, &CHIP_LABELS, chosen, hover, focus);

    // Recent / All media tabs, each with its count.
    for (i, (tab, kind)) in l.tabs.iter().zip(ListKind::BOTH).enumerate() {
        let on = s.list == kind;
        let label_r = Rect::new(tab.x, tab.y, l.tab_label_w[i] + 2.0, tab.h);
        let hot = ui.hover == Hit::ListTab(i);
        let c = if on || hot { TEXT } else { MUTED };
        p.text(TAB_LABELS[i], label_r, TextStyle::Label, c, Align::Start, true);
        let count = s.count_for(lib, kind).to_string();
        let count_r = Rect::new(label_r.right() + 4.0, tab.y, tab.w - label_r.w - 4.0, tab.h);
        p.text(&count, count_r, TextStyle::Small, SUBTLE, Align::Start, true);
        if on {
            p.fill(Rect::new(tab.x, tab.bottom() - 4.0, l.tab_label_w[i], 2.0), 1.0, ACCENT);
        }
        if s.focus_visible && s.focus == Focus::List && on {
            widgets::ring(p, true, Rect::new(tab.x - 4.0, tab.y + 3.0, l.tab_label_w[i] + 8.0, tab.h - 6.0), 6.0);
        }
    }

    gallery(p, s, lib, l, ui);

    // Footer: hint or result, then Clear history, Manager and Add.
    p.fill(l.footer, 0.0, SIDEBAR);
    p.line(l.footer.x, l.footer.y + 0.5, l.footer.right(), l.footer.y + 0.5, 1.0, LINE);
    match s.status.as_deref() {
        Some(status) => {
            let lines = p.gfx().line_count(status, TextStyle::Small, l.footer_text.w).min(2);
            let h = lines as f32 * TextStyle::Small.line();
            let y = l.footer_text.y + (l.footer_text.h - h) / 2.0;
            p.push_clip(l.footer_text);
            p.text_wrapped(status, l.footer_text.x, y, l.footer_text.w, TextStyle::Small, MUTED);
            p.pop_clip();
        }
        None => p.text(HINT, l.footer_text, TextStyle::Small, SUBTLE, Align::Start, true),
    }
    let enabled = s.history_enabled(lib);
    widgets::quiet_button(p, l.clear_history, "Clear history", TextStyle::Small, ui.hover == Hit::ClearHistory, enabled);
    widgets::ring(p, ring(Focus::ClearHistory), l.clear_history, widgets::RADIUS);
    widgets::quiet_button(p, l.open_manager, FOOTER_MANAGER, TextStyle::Small, ui.hover == Hit::OpenManager, true);
    widgets::ring(p, ring(Focus::OpenManager), l.open_manager, widgets::RADIUS);
    widgets::add_button(p, l.add_media, FOOTER_ADD, ui.hover == Hit::AddMedia, ring(Focus::AddMedia));

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
    let margin = grid.row_pitch();
    for (i, item) in items.iter().enumerate() {
        let tile = grid.tile(i, s.scroll);
        if tile.bottom() < l.gallery.y || tile.y > l.gallery.bottom() {
            // Draw only visible tiles; ask early for the next row each way.
            if p.is_full() && tile.bottom() >= l.gallery.y - margin && tile.y <= l.gallery.bottom() + margin {
                widgets::prefetch_tile(item, tile, ui.dpi);
            }
            continue;
        }
        if !p.touches(tile) {
            continue; // a partial frame for another tile
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
