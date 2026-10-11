//! Draws the quick popup. Colors are the wireframe's dark palette (`--pm-*`).

use super::d2d::{Align, Color, Painter};
use super::layout::{Hit, Layout, Rect, TextStyle, fit_media};
use crate::popup_state::{Focus, ListKind, PopupState, TypeFilter};

pub const BG: Color = Color::rgb(0x191D26);
const TITLEBAR: Color = Color::rgb(0x1D222D);
pub const SURFACE: Color = Color::rgb(0x242A36);
pub const RAISED: Color = Color::rgb(0x2B3240);
pub const TEXT: Color = Color::rgb(0xF2F0FC);
pub const MUTED: Color = Color::rgb(0xB0B9CD);
pub const BORDER: Color = Color::rgb(0x424B60);
pub const ACCENT: Color = Color::rgb(0xAB93FF);
pub const ON_ACCENT: Color = Color::rgb(0x201533);
const SOFT: Color = Color::rgb(0x35304E);
pub const SOFT_TEXT: Color = Color::rgb(0xD1C4FF);
const DANGER: Color = Color::rgb(0xFFA5B7);
/// Keyboard focus ring (the wireframe uses the browser's light focus ring).
pub const RING: Color = TEXT;

/// Pointer and window facts that are not part of `PopupState`.
pub struct Ui {
    pub hover: Hit,
    pub dpi: f32,
    pub shortcut_ok: bool,
    pub shortcut_label: &'static str,
    /// Horizontal scroll of the search text, so the caret stays visible.
    pub search_offset: f32,
    pub caret_x: f32,
    pub selection_w: f32,
    pub plus_w: f32,
}

pub fn paint(p: &Painter, s: &PopupState, l: &Layout, ui: &Ui) {
    p.clear(BG);
    titlebar(p, s, l, ui);
    top_actions(p, s, l, ui);
    search(p, s, l, ui);
    filters(p, s, l, ui);
    section(p, s, l, ui);
    gallery(p, s, l, ui);
    footer(p, s, l, ui);
    if let Some(row) = s.dropdown {
        dropdown(p, s, l, row);
    }
    p.stroke(Rect::new(0.0, 0.0, l.width, l.height), 8.0, 1.0, BORDER);
}

fn ring(p: &Painter, s: &PopupState, focus: Focus, r: Rect, radius: f32) {
    if s.focus_visible && s.focus == focus {
        p.stroke(r.inset(-2.0), radius + 2.0, 2.0, RING);
    }
}

/// Transparent button whose border turns lavender on hover (CSS .pm-quiet).
fn quiet_button(p: &Painter, r: Rect, hovered: bool, enabled: bool) {
    if hovered && enabled {
        p.stroke(r, 6.0, 1.0, ACCENT);
    }
}

/// Title bar shared by every popup screen: background, bottom border, title, ×.
pub fn title_bar(p: &Painter, bar: Rect, title: Rect, close: Rect, close_hovered: bool, close_ring: bool) {
    p.fill(bar, 0.0, TITLEBAR);
    p.line(bar.x, bar.bottom() - 0.5, bar.right(), bar.bottom() - 0.5, 1.0, BORDER);
    p.text("PocketMedia", title, TextStyle::Medium, TEXT, Align::Start, true);
    quiet_button(p, close, close_hovered, true);
    p.text("×", close, TextStyle::Body, TEXT, Align::Center, true);
    if close_ring {
        p.stroke(close.inset(-2.0), 8.0, 2.0, RING);
    }
}

fn titlebar(p: &Painter, s: &PopupState, l: &Layout, ui: &Ui) {
    title_bar(p, l.titlebar, l.title, l.close, ui.hover == Hit::Close, s.focus_visible && s.focus == Focus::Close);
    if ui.shortcut_ok {
        p.text(ui.shortcut_label, l.shortcut, TextStyle::Small, MUTED, Align::Start, true);
    } else {
        let r = Rect::new(l.shortcut.x - 40.0, l.shortcut.y, l.shortcut.w + 80.0, l.shortcut.h);
        p.text("Shortcut unavailable", r, TextStyle::Small, DANGER, Align::Center, true);
    }
}

fn top_actions(p: &Painter, s: &PopupState, l: &Layout, ui: &Ui) {
    let om = l.open_manager;
    quiet_button(p, om, ui.hover == Hit::OpenManager, true);
    // 1px border + 11px padding + empty icon slot + 7px gap.
    let text_r = Rect::new(om.x + 19.0, om.y, om.w - 19.0, om.h);
    p.text("Open manager", text_r, TextStyle::Body, TEXT, Align::Start, true);
    ring(p, s, Focus::OpenManager, om, 6.0);

    let am = l.add_media;
    p.fill(am, 6.0, ACCENT);
    let plus = Rect::new(am.x + 12.0, am.y, ui.plus_w, am.h);
    p.text("+", plus, TextStyle::Body, ON_ACCENT, Align::Start, true);
    let label = Rect::new(plus.right() + 7.0, am.y, am.right() - plus.right() - 7.0, am.h);
    p.text("Add media", label, TextStyle::Body, ON_ACCENT, Align::Start, true);
    ring(p, s, Focus::AddMedia, am, 6.0);
}

fn search(p: &Painter, s: &PopupState, l: &Layout, ui: &Ui) {
    p.fill(l.search, 6.0, SURFACE);
    p.stroke(l.search, 6.0, 1.0, BORDER);
    let focused = s.focus == Focus::Search;
    let input = l.search_input;
    // The input's own 2px padding.
    let mut text_r = Rect::new(input.x + 2.0, input.y, input.w - 4.0, input.h);
    if focused && !s.search.is_empty() {
        text_r.w -= l.search_clear.w;
    }
    if s.search.is_empty() {
        p.text("Search by name…", text_r, TextStyle::Body, MUTED, Align::Start, true);
    } else {
        p.push_clip(text_r);
        if s.search.all_selected() {
            let sel = Rect::new(text_r.x - ui.search_offset, input.y + 9.0, ui.selection_w, input.h - 18.0);
            p.fill(sel, 0.0, ACCENT.alpha(0.35));
        }
        let shifted = Rect::new(text_r.x - ui.search_offset, text_r.y, 10_000.0, text_r.h);
        p.text(s.search.text(), shifted, TextStyle::Body, TEXT, Align::Start, true);
        p.pop_clip();
    }
    if focused {
        // Text inputs always show their focus ring (CSS :focus-visible), as in focused-04.
        p.stroke(input, 3.0, 2.0, RING);
        let x = (text_r.x + ui.caret_x - ui.search_offset).round() + 0.5;
        p.line(x, input.y + 10.0, x, input.bottom() - 10.0, 1.0, TEXT);
        if !s.search.is_empty() {
            let (cx, cy) = l.search_clear.center();
            let d = 4.5;
            let c = if ui.hover == Hit::SearchClear { ACCENT } else { TEXT };
            p.line(cx - d, cy - d, cx + d, cy + d, 2.0, c);
            p.line(cx - d, cy + d, cx + d, cy - d, 2.0, c);
        }
    }
}

fn filters(p: &Painter, s: &PopupState, l: &Layout, ui: &Ui) {
    p.text("Type", l.type_label, TextStyle::Small, MUTED, Align::Start, true);
    for (i, (chip, filter)) in l.chips.iter().zip(TypeFilter::ORDER).enumerate() {
        if s.filter == filter {
            p.fill(*chip, 6.0, ACCENT);
            p.text(filter.label(), *chip, TextStyle::Small, ON_ACCENT, Align::Center, true);
        } else {
            p.fill(*chip, 6.0, RAISED);
            let border = if ui.hover == Hit::Chip(i) { ACCENT } else { BORDER };
            p.stroke(*chip, 6.0, 1.0, border);
            p.text(filter.label(), *chip, TextStyle::Small, TEXT, Align::Center, true);
        }
        ring(p, s, Focus::Type(i), *chip, 6.0);
    }
}

fn chevron(p: &Painter, cx: f32, cy: f32, c: Color) {
    p.line(cx - 4.0, cy - 2.0, cx, cy + 2.0, 1.5, c);
    p.line(cx, cy + 2.0, cx + 4.0, cy - 2.0, 1.5, c);
}

fn section(p: &Painter, s: &PopupState, l: &Layout, ui: &Ui) {
    p.text(&s.count_label(), l.count, TextStyle::Medium, TEXT, Align::Start, true);
    p.fill(l.list, 5.0, SURFACE);
    let border = if ui.hover == Hit::List || s.dropdown.is_some() { ACCENT } else { BORDER };
    p.stroke(l.list, 5.0, 1.0, border);
    let text_r = Rect::new(l.list.x + 12.0, l.list.y, l.list.w - 32.0, l.list.h);
    p.text(s.list.label(), text_r, TextStyle::Small, TEXT, Align::Start, true);
    chevron(p, l.list.right() - 14.0, l.list.y + l.list.h / 2.0, TEXT);
    ring(p, s, Focus::List, l.list, 5.0);
}

fn dropdown(p: &Painter, s: &PopupState, l: &Layout, highlighted: usize) {
    p.fill(l.dropdown, 6.0, SURFACE);
    p.stroke(l.dropdown, 6.0, 1.0, BORDER);
    for (i, (row, kind)) in l.dropdown_rows.iter().zip(ListKind::BOTH).enumerate() {
        if i == highlighted {
            p.fill(*row, 4.0, SOFT);
        }
        let text_r = Rect::new(row.x + 8.0, row.y, row.w - 16.0, row.h);
        let c = if s.list == kind { SOFT_TEXT } else { TEXT };
        p.text(kind.label(), text_r, TextStyle::Small, c, Align::Start, true);
    }
}

fn gallery(p: &Painter, s: &PopupState, l: &Layout, ui: &Ui) {
    if let Some(empty) = s.empty() {
        let (heading, hint, button) = l.empty_parts(empty.action());
        p.text(empty.heading(), heading, TextStyle::Heading, TEXT, Align::Center, true);
        p.text(empty.hint(), hint, TextStyle::Body, MUTED, Align::Center, true);
        quiet_button(p, button, ui.hover == Hit::EmptyAction, true);
        p.text(empty.action(), button, TextStyle::Body, TEXT, Align::Center, true);
        ring(p, s, Focus::EmptyAction, button, 6.0);
        return;
    }
    let items = s.visible();
    let selected = s.selected_index();
    p.push_clip(l.gallery);
    for (i, item) in items.iter().enumerate() {
        let tile = l.tile(i, s.scroll);
        if tile.bottom() < l.gallery.y || tile.y > l.gallery.bottom() {
            continue; // draw only visible tiles
        }
        let is_selected = s.focus == Focus::Gallery && selected == Some(i);
        if is_selected {
            p.fill(tile, 6.0, SOFT);
            p.stroke(tile, 6.0, 1.0, ACCENT);
        } else if ui.hover == Hit::Tile(i) {
            p.stroke(tile, 6.0, 1.0, ACCENT);
        }
        if is_selected && s.focus_visible {
            p.stroke(tile, 6.0, 2.0, RING);
        }

        let thumb = Layout::thumb(tile);
        p.fill(thumb, 6.0, RAISED);
        p.stroke(thumb, 6.0, 1.0, BORDER);
        let inner = thumb.inset(1.0);
        // Unknown size (fake imports): a square box and no size badge, as in focused-13.
        let known = item.width > 0 && item.height > 0;
        let (pw, ph) = if known { (item.width, item.height) } else { (300, 300) };
        let (fw, fh) = fit_media(inner.w, inner.h, pw, ph, ui.dpi);
        let media = Rect::new(inner.x + (inner.w - fw) / 2.0, inner.y + (inner.h - fh) / 2.0, fw, fh);
        p.fill(media, 3.0, SOFT);
        p.stroke(media, 3.0, 1.0, SOFT_TEXT);

        let badge = item.kind.badge();
        let bw = p.gfx().measure(badge, TextStyle::Tiny) + 10.0;
        let badge_r = Rect::new(inner.x + 5.0, inner.y + 5.0, bw, 11.0 * 1.45 + 4.0);
        p.fill(badge_r, 4.0, SURFACE);
        p.text(badge, badge_r, TextStyle::Tiny, TEXT, Align::Center, true);

        if known {
            let size = format!("{} × {}", item.width, item.height);
            let sw = p.gfx().measure(&size, TextStyle::Tiny) + 8.0;
            let sh = 11.0 * 1.45 + 2.0;
            let size_r = Rect::new(inner.right() - 4.0 - sw, inner.bottom() - 4.0 - sh, sw, sh);
            p.fill(size_r, 3.0, SURFACE);
            p.text(&size, size_r, TextStyle::Tiny, MUTED, Align::Center, true);
        }

        p.text(&item.name, Layout::name(tile), TextStyle::Small, TEXT, Align::Start, true);
    }
    if let Some(bar) = l.scrollbar(items.len(), s.scroll) {
        let c = if ui.hover == Hit::Scrollbar { MUTED } else { BORDER };
        p.fill(bar, 2.0, c);
    }
    p.pop_clip();
}

fn footer(p: &Painter, s: &PopupState, l: &Layout, ui: &Ui) {
    p.line(l.footer.x, l.footer.y + 0.5, l.footer.right(), l.footer.y + 0.5, 1.0, BORDER);
    let hint = s.status.as_deref().unwrap_or("Click / Enter to paste · Drag to attach · Esc to close");
    p.text(
        hint,
        l.footer_text,
        TextStyle::Small,
        MUTED,
        Align::Start,
        true,
    );
    let enabled = s.history_enabled();
    quiet_button(p, l.clear_history, ui.hover == Hit::ClearHistory, enabled);
    let c = if enabled { TEXT } else { TEXT.alpha(0.55) };
    p.text("Clear history", l.clear_history, TextStyle::Small, c, Align::Center, true);
    ring(p, s, Focus::ClearHistory, l.clear_history, 6.0);
}
