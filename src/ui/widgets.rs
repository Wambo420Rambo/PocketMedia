//! Drawing pieces shared by every window: the dark palette with lavender accents
//! and its controls: logo,
//! buttons, search box, select, segmented control, switch, keycaps, tiles.

use super::d2d::{Align, Color, Gfx, Painter};
use super::layout::{Rect, TextStyle, fit_media};
use crate::popup_state::Empty;
use crate::sample::Item;
use crate::thumbs::{self, Key, Look};
use crate::text_field::TextField;

/// Window background.
pub const BG: Color = Color::rgb(0x171A22);
/// Sidebar and the popup's footer.
pub const SIDEBAR: Color = Color::rgb(0x1C2029);
/// Cards (settings groups, the inspector, dialogs).
pub const SURFACE: Color = Color::rgb(0x1F2430);
/// Inputs, the segmented control's track, tile wells.
pub const FIELD: Color = Color::rgb(0x232836);
/// Secondary buttons.
pub const RAISED: Color = Color::rgb(0x2C3242);
pub const RAISED_HOT: Color = Color::rgb(0x363D50);
/// Thin separators inside cards.
pub const LINE: Color = Color::rgb(0x2A3040);
/// The window's own 1px border and menu borders.
pub const BORDER: Color = Color::rgb(0x343B4D);
pub const TEXT: Color = Color::rgb(0xF2F0FC);
pub const MUTED: Color = Color::rgb(0xB0B9CD);
/// Hints and secondary labels (5.7:1 on BG).
pub const SUBTLE: Color = Color::rgb(0x8E97AC);
pub const ACCENT: Color = Color::rgb(0xAB93FF);
pub const ON_ACCENT: Color = Color::rgb(0x201533);
/// Selected navigation row and chosen cards.
pub const SELECTED: Color = Color::rgb(0x2E2946);
/// The chosen segment of a segmented control.
pub const SEGMENT_ON: Color = Color::rgb(0x3A3358);
pub const SOFT_TEXT: Color = Color::rgb(0xD1C4FF);
pub const DANGER: Color = Color::rgb(0xFFA5B7);
pub const ERROR: Color = Color::rgb(0xFF8A80);
pub const GOOD: Color = Color::rgb(0x9EE6B8);
/// Keyboard focus ring.
pub const RING: Color = TEXT;
/// A disabled primary button.
const PRIMARY_OFF: Color = Color::rgb(0x4A4370);
const PRIMARY_OFF_TEXT: Color = Color::rgb(0x9C93C4);
/// Disabled text and quiet buttons are drawn at this opacity.
pub const DISABLED: f32 = 0.5;
/// Corner radius of buttons and fields.
pub const RADIUS: f32 = 8.0;

/// Focus ring 2px outside `r`.
pub fn ring(p: &Painter, show: bool, r: Rect, radius: f32) {
    if show {
        p.stroke(r.inset(-2.0), radius + 2.0, 2.0, RING);
    }
}

/// The logo: a lavender circle with a dark P, `d` across, centred on (cx, cy).
pub fn logo(p: &Painter, cx: f32, cy: f32, d: f32) {
    p.fill_circle(cx, cy, d / 2.0, ACCENT);
    // The P is 16px bold at a 28px logo; it is drawn one size for every logo.
    let r = Rect::new(cx - d / 2.0 + d * 0.03, cy - d / 2.0, d, d);
    p.text("P", r, TextStyle::Logo, ON_ACCENT, Align::Center, true);
}

/// Text-only button: a soft fill on hover.
pub fn quiet_button(p: &Painter, r: Rect, label: &str, style: TextStyle, hovered: bool, enabled: bool) {
    if hovered && enabled {
        p.fill(r, RADIUS, RAISED);
    }
    let c = if enabled { MUTED } else { MUTED.alpha(DISABLED) };
    p.text(label, r, style, if hovered && enabled { TEXT } else { c }, Align::Center, true);
}

/// Secondary button with centred text, one line or more.
pub fn button(p: &Painter, r: Rect, lines: &[&str], hovered: bool, focus_ring: bool) {
    p.fill(r, RADIUS, if hovered { RAISED_HOT } else { RAISED });
    let lh = TextStyle::Body.line();
    let top = r.y + (r.h - lh * lines.len() as f32) / 2.0;
    for (i, line) in lines.iter().enumerate() {
        let lr = Rect::new(r.x, top + i as f32 * lh, r.w, lh);
        p.text(line, lr, TextStyle::Body, TEXT, Align::Center, true);
    }
    ring(p, focus_ring, r, RADIUS);
}

/// Lavender button with dark text; muted when disabled.
pub fn primary_button(p: &Painter, r: Rect, label: &str, enabled: bool, hovered: bool, focus_ring: bool) {
    let (fill, text) = match (enabled, hovered) {
        (false, _) => (PRIMARY_OFF, PRIMARY_OFF_TEXT),
        (true, true) => (ACCENT.alpha(0.88), ON_ACCENT),
        (true, false) => (ACCENT, ON_ACCENT),
    };
    p.fill(r, RADIUS, fill);
    p.text(label, r, TextStyle::Medium, text, Align::Center, true);
    ring(p, focus_ring, r, RADIUS);
}

/// Text in the danger colour; a soft red fill on hover (Move to Recycle Bin).
pub fn danger_button(p: &Painter, r: Rect, label: &str, hovered: bool, focus_ring: bool) {
    if hovered {
        p.fill(r, RADIUS, DANGER.alpha(0.12));
    }
    p.text(label, r, TextStyle::Body, DANGER, Align::Center, true);
    ring(p, focus_ring, r, RADIUS);
}

/// Filled danger button (the confirmed Move to Recycle Bin).
pub fn danger_fill_button(p: &Painter, r: Rect, label: &str, hovered: bool, focus_ring: bool) {
    p.fill(r, RADIUS, DANGER.alpha(if hovered { 0.88 } else { 1.0 }));
    p.text(label, r, TextStyle::Medium, BG, Align::Center, true);
    ring(p, focus_ring, r, RADIUS);
}

/// × that hides a window or closes a panel.
pub fn close_button(p: &Painter, r: Rect, hovered: bool, focus_ring: bool) {
    if hovered {
        p.fill(r, RADIUS, RAISED);
    }
    let (cx, cy) = r.center();
    let d = 5.0;
    let c = if hovered { TEXT } else { SUBTLE };
    p.line(cx - d, cy - d, cx + d, cy + d, 1.5, c);
    p.line(cx - d, cy + d, cx + d, cy - d, 1.5, c);
    ring(p, focus_ring, r, RADIUS);
}

/// ← back button (Add media).
pub fn back_button(p: &Painter, r: Rect, hovered: bool, focus_ring: bool) {
    p.fill(r, RADIUS, if hovered { RAISED_HOT } else { RAISED });
    let (cx, cy) = r.center();
    let c = TEXT;
    p.line(cx - 6.0, cy, cx + 6.0, cy, 1.5, c);
    p.line(cx - 6.0, cy, cx - 1.5, cy - 4.5, 1.5, c);
    p.line(cx - 6.0, cy, cx - 1.5, cy + 4.5, 1.5, c);
    ring(p, focus_ring, r, RADIUS);
}

/// "+ Add media" (or "+ Add"): the lavender button with its plus drawn as lines.
pub fn add_button(p: &Painter, r: Rect, label: &str, hovered: bool, focus_ring: bool) {
    p.fill(r, RADIUS, ACCENT.alpha(if hovered { 0.88 } else { 1.0 }));
    let tw = p.gfx().measure(label, TextStyle::Medium);
    let total = 10.0 + 7.0 + tw;
    let x = r.x + (r.w - total) / 2.0;
    let cy = r.y + r.h / 2.0;
    p.line(x, cy, x + 10.0, cy, 1.75, ON_ACCENT);
    p.line(x + 5.0, cy - 5.0, x + 5.0, cy + 5.0, 1.75, ON_ACCENT);
    p.text(label, Rect::new(x + 17.0, r.y, tw + 2.0, r.h), TextStyle::Medium, ON_ACCENT, Align::Start, true);
    ring(p, focus_ring, r, RADIUS);
}

/// Width of an `add_button` with this label.
pub fn add_button_w(gfx: &Gfx, label: &str) -> f32 {
    16.0 + 10.0 + 7.0 + gfx.measure(label, TextStyle::Medium) + 16.0
}

/// Width of a 36px text button: 16px padding on each side.
pub fn text_button_w(gfx: &Gfx, label: &str, style: TextStyle) -> f32 {
    32.0 + gfx.measure(label, style)
}

/// Geometry of a search box: the outer frame, the text input inside it, and the
/// × that clears it.
#[derive(Clone, Copy)]
pub struct SearchBox {
    pub outer: Rect,
    pub input: Rect,
    pub clear: Rect,
    pub icon: bool,
}

impl SearchBox {
    /// `icon`: a magnifier at the left (the Manager); the popup has the logo beside it instead.
    pub fn at(x: f32, y: f32, w: f32, h: f32, icon: bool) -> Self {
        let outer = Rect::new(x, y, w, h);
        let left = if icon { 12.0 + 16.0 + 10.0 } else { 16.0 };
        let input = Rect::new(x + left, y + 2.0, w - left - 12.0, h - 4.0);
        let clear = Rect::new(input.right() - 24.0, y + (h - 24.0) / 2.0, 24.0, 24.0);
        Self { outer, input, clear, icon }
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

    pub fn for_search(gfx: &Gfx, field: &TextField, b: &SearchBox, style: TextStyle) -> Self {
        Self::of(gfx, field, style, b.input.w - 28.0)
    }
}

fn magnifier(p: &Painter, cx: f32, cy: f32, c: Color) {
    p.stroke_circle(cx - 1.5, cy - 1.5, 5.0, 1.75, c);
    p.line(cx + 2.2, cy + 2.2, cx + 6.0, cy + 6.0, 1.75, c);
}

/// What a search box shows besides its text.
pub struct SearchLook<'a> {
    pub focused: bool,
    pub clear_hovered: bool,
    pub placeholder: &'a str,
    /// Text at the right while the box is empty (the popup's shortcut), and its colour.
    pub hint: Option<(&'a str, Color)>,
    pub style: TextStyle,
}

pub fn search_box(p: &Painter, b: &SearchBox, field: &TextField, m: TextMetrics, look: &SearchLook) {
    let radius = if b.outer.h >= 44.0 { 12.0 } else { 10.0 };
    p.fill(b.outer, radius, FIELD);
    if look.focused {
        p.stroke(b.outer, radius, 2.0, ACCENT);
    }
    if b.icon {
        magnifier(p, b.outer.x + 12.0 + 8.0, b.outer.y + b.outer.h / 2.0, SUBTLE);
    }
    let input = b.input;
    let mut text_r = input;
    let show_clear = look.focused && !field.is_empty();
    if show_clear {
        text_r.w -= b.clear.w + 4.0;
    }
    if field.is_empty() {
        let hint_w = look.hint.map_or(0.0, |(h, _)| p.gfx().measure(h, TextStyle::Small) + 8.0);
        let r = Rect::new(text_r.x, text_r.y, text_r.w - hint_w, text_r.h);
        p.text(look.placeholder, r, look.style, SUBTLE, Align::Start, true);
        if let Some((hint, c)) = look.hint {
            p.text(hint, input, TextStyle::Small, c, Align::End, true);
        }
    } else {
        p.push_clip(text_r);
        if field.all_selected() {
            let sel = Rect::new(text_r.x - m.offset, input.y + 8.0, m.selection_w, input.h - 16.0);
            p.fill(sel, 0.0, ACCENT.alpha(0.35));
        }
        let shifted = Rect::new(text_r.x - m.offset, text_r.y, 10_000.0, text_r.h);
        p.text(field.text(), shifted, look.style, TEXT, Align::Start, true);
        p.pop_clip();
    }
    if look.focused {
        let x = (text_r.x + m.caret_x - m.offset).round() + 0.5;
        let inset = (input.h - look.style.line()).max(8.0) / 2.0;
        p.line(x, input.y + inset, x, input.bottom() - inset, 1.0, TEXT);
        if show_clear {
            close_button(p, b.clear, look.clear_hovered, false);
        }
    }
}

fn chevron(p: &Painter, cx: f32, cy: f32, c: Color) {
    p.line(cx - 4.0, cy - 2.0, cx, cy + 2.0, 1.5, c);
    p.line(cx, cy + 2.0, cx + 4.0, cy - 2.0, 1.5, c);
}

/// A closed drop-down list: label and chevron.
pub fn select_box(p: &Painter, r: Rect, label: &str, highlighted: bool, focus_ring: bool) {
    p.fill(r, RADIUS, if highlighted { RAISED } else { FIELD });
    let text_r = Rect::new(r.x + 12.0, r.y, r.w - 34.0, r.h);
    p.text(label, text_r, TextStyle::Small, TEXT, Align::Start, true);
    chevron(p, r.right() - 16.0, r.y + r.h / 2.0, MUTED);
    ring(p, focus_ring, r, RADIUS);
}

/// Geometry of an open drop-down list under a select box.
pub struct Menu {
    pub frame: Rect,
    pub rows: Vec<Rect>,
}

const MENU_ROW_H: f32 = 30.0;

impl Menu {
    pub fn below(select: Rect, rows: usize) -> Self {
        let frame = Rect::new(select.x, select.bottom() + 4.0, select.w, rows as f32 * MENU_ROW_H + 8.0);
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
    p.fill(m.frame.inset(-1.0), RADIUS + 1.0, BORDER);
    p.fill(m.frame, RADIUS, SURFACE);
    for (i, (row, label)) in m.rows.iter().zip(labels).enumerate() {
        if i == highlighted {
            p.fill(*row, 6.0, SELECTED);
        }
        let text_r = Rect::new(row.x + 10.0, row.y, row.w - 20.0, row.h);
        p.text(label, text_r, TextStyle::Small, if i == current { SOFT_TEXT } else { TEXT }, Align::Start, true);
    }
}

/// A segmented control: one track, the chosen segment raised.
pub fn segmented(p: &Painter, track: Rect, segs: &[Rect], labels: &[&str], chosen: usize, hover: Option<usize>, focus: Option<usize>) {
    p.fill(track, 10.0, FIELD);
    for (i, (r, label)) in segs.iter().zip(labels).enumerate() {
        let on = i == chosen;
        if on {
            p.fill(*r, 8.0, SEGMENT_ON);
        } else if hover == Some(i) {
            p.fill(*r, 8.0, RAISED);
        }
        let style = if on { TextStyle::Label } else { TextStyle::Small };
        p.text(label, *r, style, if on || hover == Some(i) { TEXT } else { MUTED }, Align::Center, true);
        ring(p, focus == Some(i), *r, 8.0);
    }
}

/// A 40x22 on/off switch.
pub fn switch(p: &Painter, r: Rect, on: bool, hovered: bool, focus_ring: bool) {
    let track = if on {
        ACCENT.alpha(if hovered { 0.88 } else { 1.0 })
    } else if hovered {
        Color::rgb(0x454D61)
    } else {
        Color::rgb(0x3A4152)
    };
    p.fill(r, r.h / 2.0, track);
    let d = r.h - 6.0;
    let cx = if on { r.right() - 3.0 - d / 2.0 } else { r.x + 3.0 + d / 2.0 };
    p.fill_circle(cx, r.y + r.h / 2.0, d / 2.0, if on { ON_ACCENT } else { MUTED });
    ring(p, focus_ring, r, r.h / 2.0);
}

/// Width of a row of keycaps for `keys` in `style`.
pub fn keycaps_w(gfx: &Gfx, keys: &[&str], style: TextStyle, gap: f32) -> f32 {
    keys.iter().map(|k| (gfx.measure(k, style) + 2.0 * keycap_pad(style)).max(keycap_min(style))).sum::<f32>()
        + gap * keys.len().saturating_sub(1) as f32
}

fn keycap_pad(style: TextStyle) -> f32 {
    if style.size() > 14.0 { 14.0 } else { 8.0 }
}

fn keycap_min(style: TextStyle) -> f32 {
    style.size() * 1.9
}

/// Keycaps from (x, centre y), `h` tall. `accent` draws them lavender (just pressed).
#[allow(clippy::too_many_arguments)]
pub fn keycaps(p: &Painter, x: f32, cy: f32, h: f32, keys: &[&str], style: TextStyle, gap: f32, accent: bool) {
    let mut kx = x;
    for k in keys {
        let w = (p.gfx().measure(k, style) + 2.0 * keycap_pad(style)).max(keycap_min(style));
        let r = Rect::new(kx, cy - h / 2.0, w, h);
        let (fill, edge, text) = if accent { (SEGMENT_ON, Color::rgb(0x5A4F8A), TEXT) } else { (RAISED, Color::rgb(0x3E465A), MUTED) };
        p.fill(r, 6.0, edge);
        p.fill(Rect::new(r.x, r.y, r.w, r.h - 2.0), 6.0, fill);
        p.text(k, Rect::new(r.x, r.y, r.w, r.h - 2.0), style, text, Align::Center, true);
        kx += w + gap;
    }
}

/// How a gallery tile is drawn.
#[derive(Clone, Copy, Default)]
pub struct TileLook {
    pub selected: bool,
    pub hovered: bool,
    pub ring: bool,
}

/// A gallery tile: the media well and the file name under it.
pub fn tile(p: &Painter, item: &Item, r: Rect, look: TileLook, dpi: f32) {
    let well = super::layout::Layout::thumb(r);
    thumb_look(p, item, well, dpi, look.hovered && !look.selected);
    if look.selected {
        p.stroke(well.inset(-3.0), 13.0, 2.0, ACCENT);
    }
    if look.ring {
        p.stroke(well.inset(-6.0), 16.0, 2.0, RING);
    }
    let name = super::layout::Layout::name(r);
    // The type badge already says what it is, so the tile shows the name without its extension.
    let shown = crate::search::name_without_extension(&item.name);
    p.text(shown, name, TextStyle::Small, if look.selected { SOFT_TEXT } else { TEXT }, Align::Start, true);
}

/// The thumbnail for a well, in device pixels: the media area 6px inside it.
pub fn thumb_key(id: u32, thumb: Rect, dpi: f32) -> Key {
    let inner = thumb.inset(1.0);
    Key::new(id, inner.w - 12.0, inner.h - 12.0, dpi)
}

/// Asks for the thumbnail of a tile just outside the visible area (prefetch).
pub fn prefetch_tile(item: &Item, tile: Rect, dpi: f32) {
    thumbs::prefetch(item, thumb_key(item.id, super::layout::Layout::thumb(tile), dpi));
}

/// The media well used by tiles and the inspector.
pub fn thumb(p: &Painter, item: &Item, thumb: Rect, dpi: f32) {
    thumb_look(p, item, thumb, dpi, false);
}

/// The media (a moving frame, the thumbnail, or a placeholder while it is made),
/// centred and never cropped or enlarged past its own pixels, with the
/// type badge and pixel size. A frame that starts playing never moves anything.
fn thumb_look(p: &Painter, item: &Item, thumb: Rect, dpi: f32, hovered: bool) {
    p.fill(thumb, 10.0, if hovered { RAISED } else { FIELD });
    let inner = thumb.inset(1.0);
    let key = thumb_key(item.id, thumb, dpi);
    let look = thumbs::look(item, key, thumb);
    let shown = match crate::preview::frame(&key) {
        Some(frame) => Look::Ready(frame.0, frame.1),
        None => look,
    };
    match shown {
        Look::Ready(px, version) => {
            let s = 96.0 / dpi.max(1.0);
            let (fw, fh) = (px.width as f32 * s, px.height as f32 * s);
            // Whole device pixels, so the bitmap maps 1:1 and stays sharp.
            let snap = |v: f32| (v / s).round() * s;
            let media = Rect::new(snap(inner.x + (inner.w - fw) / 2.0), snap(inner.y + (inner.h - fh) / 2.0), fw, fh);
            p.image((key.id, key.w, key.h), version, &px, media);
        }
        Look::Pending => {
            let known = item.width > 0 && item.height > 0;
            let (pw, ph) = if known { (item.width, item.height) } else { (300, 300) };
            let (fw, fh) = fit_media(inner.w, inner.h, pw, ph, dpi);
            let media = Rect::new(inner.x + (inner.w - fw) / 2.0, inner.y + (inner.h - fh) / 2.0, fw, fh);
            p.fill(media, 4.0, SELECTED);
        }
        Look::NoPreview => {
            // A truthful placeholder: the file is still there to paste or drag.
            let r = inner.inset(6.0);
            p.stroke_dashed(r, 6.0, BORDER);
            p.text("No preview", r, TextStyle::Tiny, SUBTLE, Align::Center, true);
        }
    }

    let chip = Color::rgb(0x0C0E14).alpha(0.78);
    let badge = item.kind.badge();
    let bw = p.gfx().measure(badge, TextStyle::Tiny) + 10.0;
    let badge_r = Rect::new(inner.x + 5.0, inner.y + 5.0, bw, TextStyle::Tiny.line() + 3.0);
    p.fill(badge_r, 5.0, chip);
    p.text(badge, badge_r, TextStyle::Tiny, TEXT, Align::Center, true);

    if item.width > 0 && item.height > 0 {
        let size = format!("{} × {}", item.width, item.height);
        let sw = p.gfx().measure(&size, TextStyle::Tiny) + 10.0;
        let sh = TextStyle::Tiny.line() + 3.0;
        let size_r = Rect::new(inner.right() - 5.0 - sw, inner.bottom() - 5.0 - sh, sw, sh);
        p.fill(size_r, 5.0, chip);
        p.text(&size, size_r, TextStyle::Tiny, MUTED, Align::Center, true);
    }
}

/// "No matching media" and friends, with their action button.
pub fn empty_state(p: &Painter, parts: (Rect, Rect, Rect), empty: Empty, hovered: bool, focus_ring: bool) {
    let (heading, hint, b) = parts;
    p.text(empty.heading(), heading, TextStyle::Heading, TEXT, Align::Center, true);
    p.text(empty.hint(), hint, TextStyle::Body, MUTED, Align::Center, true);
    button(p, b, &[empty.action()], hovered, focus_ring);
}

/// Thin scrollbar thumb.
pub fn scrollbar(p: &Painter, bar: Rect, hovered: bool) {
    p.fill(bar, 2.0, if hovered { MUTED } else { Color::rgb(0x454D61) });
}

/// Small line icons for the Manager's sidebar, 16x16 centred in `r`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    All,
    Gif,
    Picture,
    Video,
    Settings,
    Popup,
}

pub fn icon(p: &Painter, which: Icon, r: Rect, c: Color) {
    let (cx, cy) = r.center();
    let b = Rect::new(cx - 8.0, cy - 8.0, 16.0, 16.0);
    let w = 1.5;
    match which {
        Icon::All => {
            for (dx, dy) in [(0.0, 0.0), (9.0, 0.0), (0.0, 9.0), (9.0, 9.0)] {
                p.stroke(Rect::new(b.x + dx, b.y + dy, 7.0, 7.0), 2.0, w, c);
            }
        }
        Icon::Gif => {
            p.stroke(b, 4.0, w, c);
            p.stroke_circle(cx, cy, 3.5, w, c);
        }
        Icon::Picture => {
            p.stroke(b, 4.0, w, c);
            p.line(b.x + 3.0, b.bottom() - 4.0, b.x + 7.0, cy, w, c);
            p.line(b.x + 7.0, cy, b.x + 10.0, cy + 3.0, w, c);
            p.line(b.x + 10.0, cy + 3.0, b.right() - 3.0, b.bottom() - 4.0, w, c);
            p.fill_circle(b.right() - 5.0, b.y + 5.0, 1.5, c);
        }
        Icon::Video => {
            p.stroke(b, 4.0, w, c);
            p.line(cx - 2.0, cy - 3.5, cx - 2.0, cy + 3.5, w, c);
            p.line(cx - 2.0, cy - 3.5, cx + 3.5, cy, w, c);
            p.line(cx - 2.0, cy + 3.5, cx + 3.5, cy, w, c);
        }
        Icon::Settings => {
            p.stroke_circle(cx, cy, 3.0, w, c);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::FRAC_PI_4;
                let (s, co) = a.sin_cos();
                p.line(cx + co * 5.5, cy + s * 5.5, cx + co * 7.5, cy + s * 7.5, 2.0, c);
            }
        }
        Icon::Popup => {
            p.stroke(Rect::new(b.x, b.y + 3.0, 13.0, 13.0), 3.0, w, c);
            p.line(cx - 1.0, cy + 1.0, b.right(), b.y, w, c);
            p.line(b.right() - 5.0, b.y, b.right(), b.y, w, c);
            p.line(b.right(), b.y, b.right(), b.y + 5.0, w, c);
        }
    }
}

/// The full file name over a tile ("with the full name as a
/// tooltip"). Drawn last, inside `bounds`: under the tile, or above it when there
/// is no room below. Long names wrap (at most 4 lines).
pub fn tooltip(p: &Painter, text: &str, tile: Rect, bounds: Rect) {
    const PAD_X: f32 = 8.0;
    const PAD_Y: f32 = 6.0;
    let style = TextStyle::Small;
    let max_text = (bounds.w - 16.0).min(320.0) - 2.0 * PAD_X;
    if max_text <= 0.0 {
        return;
    }
    let text_w = (p.gfx().measure(text, style) + 1.0).min(max_text);
    let lines = p.gfx().line_count(text, style, text_w).clamp(1, 4);
    let (w, h) = (text_w + 2.0 * PAD_X, lines as f32 * style.line() + 2.0 * PAD_Y);
    let x = (tile.x + (tile.w - w) / 2.0).clamp(bounds.x + 8.0, (bounds.right() - w - 8.0).max(bounds.x + 8.0));
    let below = tile.bottom() + 4.0;
    let y = if below + h <= bounds.bottom() - 4.0 { below } else { (tile.y - h - 4.0).max(bounds.y + 4.0) };
    let r = Rect::new(x, y, w, h);
    p.fill(r, 8.0, RAISED);
    p.stroke(r, 8.0, 1.0, BORDER);
    p.push_clip(r);
    p.text_wrapped(text, x + PAD_X, y + PAD_Y, text_w, style, TEXT);
    p.pop_clip();
}
