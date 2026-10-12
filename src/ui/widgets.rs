//! Drawing pieces shared by every window: the wireframe's dark palette and its
//! controls (title bar, buttons, search box, select, list, tiles, empty state).

use super::d2d::{Align, Color, Gfx, Painter};
use super::layout::{Rect, TextStyle, fit_media};
use crate::popup_state::Empty;
use crate::sample::Item;
use crate::text_field::TextField;

pub const BG: Color = Color::rgb(0x191D26);
/// Title bar, sidebar and inspector background (`--pm-sidebar`).
pub const SIDEBAR: Color = Color::rgb(0x1D222D);
pub const SURFACE: Color = Color::rgb(0x242A36);
pub const RAISED: Color = Color::rgb(0x2B3240);
pub const TEXT: Color = Color::rgb(0xF2F0FC);
pub const MUTED: Color = Color::rgb(0xB0B9CD);
pub const BORDER: Color = Color::rgb(0x424B60);
pub const ACCENT: Color = Color::rgb(0xAB93FF);
pub const ON_ACCENT: Color = Color::rgb(0x201533);
pub const SOFT: Color = Color::rgb(0x35304E);
pub const SOFT_TEXT: Color = Color::rgb(0xD1C4FF);
pub const DANGER: Color = Color::rgb(0xFFA5B7);
pub const ERROR: Color = Color::rgb(0xFF8A80);
/// Keyboard focus ring (the wireframe uses the browser's light focus ring).
pub const RING: Color = TEXT;
/// Disabled controls are drawn at this opacity, as in the wireframe.
pub const DISABLED: f32 = 0.55;

/// Focus ring 2px outside `r` (CSS :focus-visible).
pub fn ring(p: &Painter, show: bool, r: Rect, radius: f32) {
    if show {
        p.stroke(r.inset(-2.0), radius + 2.0, 2.0, RING);
    }
}

/// Transparent button whose border turns lavender on hover (CSS .pm-quiet).
pub fn quiet_button(p: &Painter, r: Rect, label: &str, style: TextStyle, hovered: bool, enabled: bool) {
    if hovered && enabled {
        p.stroke(r, 6.0, 1.0, ACCENT);
    }
    let c = if enabled { TEXT } else { TEXT.alpha(DISABLED) };
    p.text(label, r, style, c, Align::Center, true);
}

/// Raised button with a border, text at the left (CSS button / .pm-full).
pub fn raised_button(p: &Painter, r: Rect, label: &str, style: TextStyle, hovered: bool) {
    p.fill(r, 6.0, RAISED);
    p.stroke(r, 6.0, 1.0, if hovered { ACCENT } else { BORDER });
    // 1px border + 11px padding + empty icon slot + 7px gap.
    let text = Rect::new(r.x + 19.0, r.y, r.w - 26.0, r.h);
    p.text(label, text, style, TEXT, Align::Start, true);
}

/// Lavender "+ Add media" button (CSS .pm-primary).
pub fn add_media_button(p: &Painter, r: Rect, plus_w: f32) {
    p.fill(r, 6.0, ACCENT);
    let plus = Rect::new(r.x + 12.0, r.y, plus_w, r.h);
    p.text("+", plus, TextStyle::Body, ON_ACCENT, Align::Start, true);
    let label = Rect::new(plus.right() + 7.0, r.y, r.right() - plus.right() - 7.0, r.h);
    p.text("Add media", label, TextStyle::Body, ON_ACCENT, Align::Start, true);
}

/// Title bar shared by every window: background, bottom border, title, ×.
pub fn title_bar(p: &Painter, bar: Rect, title: Rect, close: Rect, close_hovered: bool, close_ring: bool) {
    p.fill(bar, 0.0, SIDEBAR);
    p.line(bar.x, bar.bottom() - 0.5, bar.right(), bar.bottom() - 0.5, 1.0, BORDER);
    p.text("PocketMedia", title, TextStyle::Medium, TEXT, Align::Start, true);
    quiet_button(p, close, "×", TextStyle::Body, close_hovered, true);
    ring(p, close_ring, close, 6.0);
}

/// Geometry of a search box: the outer frame, the text input inside it, and the
/// × that clears it.
#[derive(Clone, Copy)]
pub struct SearchBox {
    pub outer: Rect,
    pub input: Rect,
    pub clear: Rect,
}

impl SearchBox {
    /// CSS .pm-search: 40px tall, 12px padding, an (empty) icon slot and a 9px gap.
    pub fn at(x: f32, y: f32, w: f32) -> Self {
        let outer = Rect::new(x, y, w, 40.0);
        let input = Rect::new(x + 1.0 + 12.0 + 9.0, y + 1.0, w - 2.0 - 24.0 - 9.0, 38.0);
        let clear = Rect::new(input.right() - 22.0, input.y + 7.0, 22.0, 24.0);
        Self { outer, input, clear }
    }
}

/// Where the caret and selection go in a one-line text box, and how far the
/// text is scrolled so the caret stays visible.
#[derive(Clone, Copy, Default)]
pub struct TextMetrics {
    pub offset: f32,
    pub caret_x: f32,
    pub selection_w: f32,
}

impl TextMetrics {
    pub fn of(gfx: &Gfx, field: &TextField, style: TextStyle, visible_w: f32) -> Self {
        let text = field.text();
        let caret_x = gfx.caret_x(text, style, field.caret_utf16());
        let selection_w = gfx.measure(text, style);
        let offset = (caret_x - visible_w + 2.0).max(0.0);
        Self { offset, caret_x, selection_w }
    }

    pub fn for_search(gfx: &Gfx, field: &TextField, b: &SearchBox) -> Self {
        Self::of(gfx, field, TextStyle::Body, b.input.w - 4.0 - 26.0)
    }
}

pub fn search_box(p: &Painter, b: &SearchBox, field: &TextField, focused: bool, m: TextMetrics, clear_hovered: bool) {
    p.fill(b.outer, 6.0, SURFACE);
    p.stroke(b.outer, 6.0, 1.0, BORDER);
    let input = b.input;
    // The input's own 2px padding.
    let mut text_r = Rect::new(input.x + 2.0, input.y, input.w - 4.0, input.h);
    if focused && !field.is_empty() {
        text_r.w -= b.clear.w;
    }
    if field.is_empty() {
        p.text("Search by name…", text_r, TextStyle::Body, MUTED, Align::Start, true);
    } else {
        p.push_clip(text_r);
        if field.all_selected() {
            let sel = Rect::new(text_r.x - m.offset, input.y + 9.0, m.selection_w, input.h - 18.0);
            p.fill(sel, 0.0, ACCENT.alpha(0.35));
        }
        let shifted = Rect::new(text_r.x - m.offset, text_r.y, 10_000.0, text_r.h);
        p.text(field.text(), shifted, TextStyle::Body, TEXT, Align::Start, true);
        p.pop_clip();
    }
    if focused {
        // Text inputs always show their focus ring (CSS :focus-visible), as in focused-04.
        p.stroke(input, 3.0, 2.0, RING);
        let x = (text_r.x + m.caret_x - m.offset).round() + 0.5;
        p.line(x, input.y + 10.0, x, input.bottom() - 10.0, 1.0, TEXT);
        if !field.is_empty() {
            let (cx, cy) = b.clear.center();
            let d = 4.5;
            let c = if clear_hovered { ACCENT } else { TEXT };
            p.line(cx - d, cy - d, cx + d, cy + d, 2.0, c);
            p.line(cx - d, cy + d, cx + d, cy - d, 2.0, c);
        }
    }
}

fn chevron(p: &Painter, cx: f32, cy: f32, c: Color) {
    p.line(cx - 4.0, cy - 2.0, cx, cy + 2.0, 1.5, c);
    p.line(cx, cy + 2.0, cx + 4.0, cy - 2.0, 1.5, c);
}

/// A closed drop-down list (CSS select): label and chevron.
pub fn select_box(p: &Painter, r: Rect, label: &str, highlighted: bool, focus_ring: bool) {
    p.fill(r, 5.0, SURFACE);
    p.stroke(r, 5.0, 1.0, if highlighted { ACCENT } else { BORDER });
    let text_r = Rect::new(r.x + 12.0, r.y, r.w - 32.0, r.h);
    p.text(label, text_r, TextStyle::Small, TEXT, Align::Start, true);
    chevron(p, r.right() - 14.0, r.y + r.h / 2.0, TEXT);
    ring(p, focus_ring, r, 5.0);
}

/// Geometry of an open drop-down list under a select box.
pub struct Menu {
    pub frame: Rect,
    pub rows: Vec<Rect>,
}

const MENU_ROW_H: f32 = 26.0;

impl Menu {
    pub fn below(select: Rect, rows: usize) -> Self {
        let frame = Rect::new(select.x, select.bottom() + 2.0, select.w, rows as f32 * MENU_ROW_H + 8.0);
        let rows = (0..rows)
            .map(|i| Rect::new(frame.x + 4.0, frame.y + 4.0 + i as f32 * MENU_ROW_H, frame.w - 8.0, MENU_ROW_H))
            .collect();
        Self { frame, rows }
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<usize> {
        self.rows.iter().position(|r| r.contains(x, y))
    }
}

pub fn menu(p: &Painter, m: &Menu, labels: &[&str], highlighted: usize, current: usize) {
    p.fill(m.frame, 6.0, SURFACE);
    p.stroke(m.frame, 6.0, 1.0, BORDER);
    for (i, (row, label)) in m.rows.iter().zip(labels).enumerate() {
        if i == highlighted {
            p.fill(*row, 4.0, SOFT);
        }
        let text_r = Rect::new(row.x + 8.0, row.y, row.w - 16.0, row.h);
        p.text(label, text_r, TextStyle::Small, if i == current { SOFT_TEXT } else { TEXT }, Align::Start, true);
    }
}

/// How a gallery tile is drawn.
#[derive(Clone, Copy, Default)]
pub struct TileLook {
    pub selected: bool,
    pub hovered: bool,
    pub ring: bool,
}

/// A gallery tile: thumbnail box plus the file name (CSS .pm-tile).
pub fn tile(p: &Painter, item: &Item, r: Rect, look: TileLook, dpi: f32) {
    if look.selected {
        p.fill(r, 6.0, SOFT);
        p.stroke(r, 6.0, 1.0, ACCENT);
    } else if look.hovered {
        p.stroke(r, 6.0, 1.0, ACCENT);
    }
    if look.ring {
        p.stroke(r, 6.0, 2.0, RING);
    }
    thumb(p, item, super::layout::Layout::thumb(r), dpi);
    p.text(&item.name, super::layout::Layout::name(r), TextStyle::Small, TEXT, Align::Start, true);
}

/// Thumbnail box: aspect-correct media placeholder, type badge and pixel size
/// (CSS .pm-thumb). Used by tiles and the Manager's inspector.
pub fn thumb(p: &Painter, item: &Item, thumb: Rect, dpi: f32) {
    p.fill(thumb, 6.0, RAISED);
    p.stroke(thumb, 6.0, 1.0, BORDER);
    let inner = thumb.inset(1.0);
    // Unknown size (fake imports): a square box and no size badge, as in focused-13.
    let known = item.width > 0 && item.height > 0;
    let (pw, ph) = if known { (item.width, item.height) } else { (300, 300) };
    let (fw, fh) = fit_media(inner.w, inner.h, pw, ph, dpi);
    let media = Rect::new(inner.x + (inner.w - fw) / 2.0, inner.y + (inner.h - fh) / 2.0, fw, fh);
    p.fill(media, 3.0, SOFT);
    p.stroke(media, 3.0, 1.0, SOFT_TEXT);

    let badge = item.kind.badge();
    let bw = p.gfx().measure(badge, TextStyle::Tiny) + 10.0;
    let badge_r = Rect::new(inner.x + 5.0, inner.y + 5.0, bw, TextStyle::Tiny.line() + 4.0);
    p.fill(badge_r, 4.0, SURFACE);
    p.text(badge, badge_r, TextStyle::Tiny, TEXT, Align::Center, true);

    if known {
        let size = format!("{} × {}", item.width, item.height);
        let sw = p.gfx().measure(&size, TextStyle::Tiny) + 8.0;
        let sh = TextStyle::Tiny.line() + 2.0;
        let size_r = Rect::new(inner.right() - 4.0 - sw, inner.bottom() - 4.0 - sh, sw, sh);
        p.fill(size_r, 3.0, SURFACE);
        p.text(&size, size_r, TextStyle::Tiny, MUTED, Align::Center, true);
    }
}

/// "No matching media" and friends, with their action button (CSS .pm-empty).
pub fn empty_state(p: &Painter, parts: (Rect, Rect, Rect), empty: Empty, hovered: bool, focus_ring: bool) {
    let (heading, hint, button) = parts;
    p.text(empty.heading(), heading, TextStyle::Heading, TEXT, Align::Center, true);
    p.text(empty.hint(), hint, TextStyle::Body, MUTED, Align::Center, true);
    quiet_button(p, button, empty.action(), TextStyle::Body, hovered, true);
    ring(p, focus_ring, button, 6.0);
}

/// Thin scrollbar thumb (the wireframe's `scrollbar-width: thin`).
pub fn scrollbar(p: &Painter, bar: Rect, hovered: bool) {
    p.fill(bar, 2.0, if hovered { MUTED } else { BORDER });
}
