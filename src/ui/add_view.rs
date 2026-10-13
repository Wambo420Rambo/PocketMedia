//! Add media panel (Focused layout): geometry, hit-testing and drawing.
//! Numbers come from the wireframe CSS (.pm-import-*, .pm-dropzone, .pm-input)
//! and were checked against focused-11/12 in screenshots-dark.

use super::d2d::{Align, Color, Gfx, Painter};
use super::layout::{Rect, TextStyle};
use super::widgets::{self, ACCENT, BG, BORDER, DISABLED, ERROR, MUTED, ON_ACCENT, RAISED, RING, SOFT_TEXT, SURFACE, TEXT};
use crate::add_media::{AddFocus, AddMedia, RowStatus};

const PAD: f32 = 20.0;
const HEAD_PAD_TOP: f32 = 20.0;
const HEAD_PAD_BOTTOM: f32 = 16.0;
const BACK_W: f32 = 32.0;
const BUTTON_H: f32 = 36.0;
const DROP_H: f32 = 136.0;
const INPUT_H: f32 = 30.0;
const ROW_PAD: f32 = 13.0;
const ICON_COL: f32 = 20.0;
const COL_GAP: f32 = 10.0;
const ROUTE_W: f32 = 76.0;
/// The × at the end of each review row (leaves a file out).
const REMOVE_W: f32 = 28.0;
const DROP_FILL: Color = Color::rgb(0x232434); // --pm-soft 35% over --pm-bg
/// The panel is at most this wide and centred, so it reads well in the wide
/// Manager window too (CSS .pm-import-host max-width).
const HOST_MAX_W: f32 = 680.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddHit {
    None,
    Close,
    Back,
    ChooseFiles,
    ImportExisting,
    RowInput(usize),
    /// The × that leaves a file out of the import.
    Remove(usize),
    ChooseAgain,
    Cancel,
    Add,
    /// Anywhere in the scrolling area that is not a control.
    Main,
}

pub struct RowGeom {
    pub row: Rect,
    pub input: Option<Rect>,
    pub ext: Rect,
    pub message: Option<(Rect, String)>,
    pub route: Rect,
    /// The × (while the list can be edited).
    pub remove: Option<Rect>,
}

/// Everything positioned for one frame. Content rects are already shifted by scroll.
pub struct AddLayout {
    pub width: f32,
    pub height: f32,
    /// Right edge of the panel, where its scrollbar goes.
    pub host_right: f32,
    pub close: Rect,
    pub title_bar: Rect,
    pub title: Rect,
    pub back: Rect,
    pub heading: Rect,
    pub subtitle: Rect,
    pub main: Rect,
    pub footer: Rect,
    pub safety: Rect,
    pub cancel: Rect,
    pub add: Rect,
    // Start page
    pub drop: Rect,
    pub choose_files: Rect,
    pub import_existing: Rect,
    pub choice_text_w: f32,
    /// Why choosing failed, under the choices.
    pub error: Option<Rect>,
    // Review page
    pub summary: Rect,
    pub rows: Vec<RowGeom>,
    pub note: Rect,
    pub choose_again: Rect,
    pub content_h: f32,
}

const NOTE: &str = "Files are copied into Pictures, GIFs and Videos automatically. Rename a file here to make it easier to find by search.";
const CHOOSE_FILES: (&str, &str) = ("Choose files", "Select one or more pictures, GIFs or videos.");
const IMPORT_EXISTING: (&str, &str) = (
    "Import your existing media",
    "Choose a folder. Files from it and all subfolders are sorted and copied automatically.",
);

impl AddLayout {
    pub fn compute(a: &AddMedia, width: f32, height: f32, gfx: &Gfx) -> Self {
        let title_bar = Rect::new(1.0, 1.0, width - 2.0, 46.0);
        let close = Rect::new(width - 1.0 - 14.0 - 32.0, 1.0 + (45.0 - 28.0) / 2.0, 32.0, 28.0);
        let title = Rect::new(1.0 + 14.0 + 9.0, 1.0, gfx.measure("PocketMedia", TextStyle::Medium), 45.0);

        // The panel: full width in the popup, centred at 680 in the Manager.
        let host_w = (width - 2.0).min(HOST_MAX_W);
        let host_x = (width - host_w) / 2.0;
        // Head: [back 32][12][title / subtitle], padding 20 20 16.
        let x0 = host_x + PAD;
        let inner_w = host_w - 2.0 * PAD;
        let head_y = 47.0 + HEAD_PAD_TOP;
        let head_h = TextStyle::Title.line() + 4.0 + TextStyle::Small.line();
        let back = Rect::new(x0, head_y + (head_h - BUTTON_H) / 2.0, BACK_W, BUTTON_H);
        let text_x = back.right() + 12.0;
        let heading = Rect::new(text_x, head_y, inner_w - BACK_W - 12.0, TextStyle::Title.line());
        let subtitle = Rect::new(text_x, heading.bottom() + 4.0, heading.w, TextStyle::Small.line());
        let main_y = head_y + head_h + HEAD_PAD_BOTTOM;

        // Footer: padding 14 20, safety line, then actions (review only) after 12px.
        let review = a.reviewing();
        let footer_h = 1.0 + 14.0 + TextStyle::Small.line() + 14.0 + if review { 12.0 + BUTTON_H } else { 0.0 };
        let footer = Rect::new(host_x, height - 1.0 - footer_h, host_w, footer_h);
        let safety = Rect::new(x0 + 7.0, footer.y + 1.0 + 14.0, inner_w - 7.0, TextStyle::Small.line());
        let actions_y = safety.bottom() + 12.0;
        let add_w = 24.0 + gfx.measure(&a.add_label(), TextStyle::Body);
        let add = Rect::new(x0 + inner_w - add_w, actions_y, add_w, BUTTON_H);
        let cancel_w = 24.0 + gfx.measure("Cancel", TextStyle::Body);
        let cancel = Rect::new(add.x - 8.0 - cancel_w, actions_y, cancel_w, BUTTON_H);

        let main = Rect::new(x0, main_y, inner_w, (footer.y - main_y).max(0.0));
        let top = main.y - a.scroll;

        // Start page.
        let drop = Rect::new(x0, top, inner_w, DROP_H);
        let chevron_w = gfx.measure("›", TextStyle::Body);
        let choice_text_w = inner_w - 2.0 - 24.0 - 12.0 - 12.0 - chevron_w;
        let choice_h = |small: &str| 2.0 + 28.0 + TextStyle::Medium.line() + 4.0 + gfx.wrapped_height(small, TextStyle::Small, choice_text_w);
        let choose_files = Rect::new(x0, drop.bottom() + 16.0, inner_w, choice_h(CHOOSE_FILES.1));
        let import_existing = Rect::new(x0, choose_files.bottom() + 8.0, inner_w, choice_h(IMPORT_EXISTING.1));
        let error = a.error.as_deref().map(|e| Rect::new(x0, import_existing.bottom() + 12.0, inner_w, gfx.wrapped_height(e, TextStyle::Small, inner_w)));

        // Review page.
        let summary = Rect::new(x0, top, inner_w, TextStyle::Small.line());
        let name_x = x0 + ICON_COL + COL_GAP;
        let name_w = inner_w - ICON_COL - COL_GAP - COL_GAP - ROUTE_W - COL_GAP - REMOVE_W;
        let route_x = x0 + inner_w - REMOVE_W - COL_GAP - ROUTE_W;
        let mut y = summary.bottom() + 12.0;
        let mut rows = Vec::new();
        for row in &a.rows {
            let ext_w = gfx.measure(row.ext(), TextStyle::Small);
            let (input, first_h) = match row.status {
                RowStatus::Copy => (Some(Rect::new(name_x, y + ROW_PAD, name_w - 4.0 - ext_w, INPUT_H)), INPUT_H),
                RowStatus::Skip => (None, TextStyle::Row.line()),
            };
            let msg = row.message();
            let msg_h = msg.as_deref().map_or(0.0, |m| 3.0 + gfx.wrapped_height(m, TextStyle::Small, name_w));
            let inner_h = first_h + msg_h;
            let r = Rect::new(x0, y, inner_w, ROW_PAD + inner_h + ROW_PAD + 1.0);
            let ext = Rect::new(name_x + name_w - ext_w, y + ROW_PAD, ext_w, INPUT_H);
            let message = msg.map(|m| (Rect::new(name_x, y + ROW_PAD + first_h + 3.0, name_w, msg_h - 3.0), m));
            let route = Rect::new(route_x, y + ROW_PAD, ROUTE_W, inner_h);
            let remove = a
                .editable()
                .then(|| Rect::new(x0 + inner_w - REMOVE_W, y + ROW_PAD + (first_h - REMOVE_W) / 2.0, REMOVE_W, REMOVE_W));
            rows.push(RowGeom { row: r, input, ext, message, route, remove });
            y = r.bottom();
        }
        let note_h = gfx.wrapped_height(NOTE, TextStyle::Small, inner_w);
        let note = Rect::new(x0, y + 12.0, inner_w, note_h);
        let ca_w = 24.0 + gfx.measure("Choose again", TextStyle::Body);
        let choose_again = Rect::new(x0, note.bottom(), ca_w, BUTTON_H);

        let content_h = if review {
            choose_again.bottom() + 18.0 - top
        } else {
            error.map_or(import_existing.bottom(), |e| e.bottom()) + 18.0 - top
        };

        AddLayout {
            width,
            height,
            host_right: host_x + host_w,
            close,
            title_bar,
            title,
            back,
            heading,
            subtitle,
            main,
            footer,
            safety,
            cancel,
            add,
            drop,
            choose_files,
            import_existing,
            choice_text_w,
            error,
            summary,
            rows,
            note,
            choose_again,
            content_h,
        }
    }

    pub fn max_scroll(&self) -> f32 {
        (self.content_h - self.main.h).max(0.0)
    }

    /// Scroll offset (given the current one) that brings `r` fully into view.
    pub fn scroll_to_show(&self, r: Rect, scroll: f32) -> f32 {
        let (top, bottom) = (self.main.y, self.main.bottom());
        let s = if r.y < top {
            scroll - (top - r.y)
        } else if r.bottom() > bottom {
            scroll + (r.bottom() - bottom)
        } else {
            scroll
        };
        s.clamp(0.0, self.max_scroll())
    }

    pub fn focus_rect(&self, a: &AddMedia) -> Option<Rect> {
        match a.focus {
            AddFocus::Row(i) | AddFocus::Remove(i) => self.rows.get(i).map(|g| g.row),
            AddFocus::ChooseAgain => Some(self.choose_again),
            AddFocus::ChooseFiles => Some(self.choose_files),
            AddFocus::ImportExisting => Some(self.import_existing),
            _ => None,
        }
    }

    pub fn hit(&self, a: &AddMedia, x: f32, y: f32) -> AddHit {
        if self.close.contains(x, y) {
            return AddHit::Close;
        }
        if self.back.contains(x, y) {
            return AddHit::Back;
        }
        if a.reviewing() && self.footer.contains(x, y) {
            if self.add.contains(x, y) {
                return AddHit::Add;
            }
            if self.cancel.contains(x, y) {
                return AddHit::Cancel;
            }
            return AddHit::None;
        }
        if !self.main.contains(x, y) {
            return AddHit::None;
        }
        if a.reviewing() {
            if !a.editable() {
                return AddHit::Main;
            }
            if let Some(i) = self.rows.iter().position(|g| g.input.is_some_and(|r| r.contains(x, y))) {
                return AddHit::RowInput(i);
            }
            if let Some(i) = self.rows.iter().position(|g| g.remove.is_some_and(|r| r.contains(x, y))) {
                return AddHit::Remove(i);
            }
            if self.choose_again.contains(x, y) {
                return AddHit::ChooseAgain;
            }
        } else if self.choose_files.contains(x, y) {
            return AddHit::ChooseFiles;
        } else if self.import_existing.contains(x, y) {
            return AddHit::ImportExisting;
        }
        AddHit::Main
    }
}

/// What the painter needs besides the state and layout.
pub struct AddUi {
    pub hover: AddHit,
    /// Caret x within the focused row's text, from DirectWrite hit-testing.
    pub caret_x: f32,
    pub selection_w: f32,
}

pub fn paint(p: &Painter, a: &AddMedia, l: &AddLayout, ui: &AddUi) {
    p.clear(BG);
    widgets::title_bar(p, l.title_bar, l.title, l.close, ui.hover == AddHit::Close, a.focus_visible && a.focus == AddFocus::Close);

    let ring = |focus: AddFocus, r: Rect, radius: f32| {
        if a.focus_visible && a.focus == focus {
            p.stroke(r.inset(-2.0), radius + 2.0, 2.0, RING);
        }
    };
    let quiet = |r: Rect, hit: AddHit| {
        if ui.hover == hit {
            p.stroke(r, 6.0, 1.0, ACCENT);
        }
    };

    // Head
    quiet(l.back, AddHit::Back);
    p.text("←", l.back, TextStyle::Body, TEXT, Align::Center, true);
    ring(AddFocus::Back, l.back, 6.0);
    let (heading, sub) = if a.reviewing() {
        ("Review files", "Check the files before adding them.")
    } else {
        ("Add media", "Pictures, GIFs and videos, in one place.")
    };
    p.text(heading, l.heading, TextStyle::Title, TEXT, Align::Start, true);
    p.text(sub, l.subtitle, TextStyle::Small, MUTED, Align::Start, true);

    // Scrolling area
    p.push_clip(l.main);
    if a.reviewing() {
        review(p, a, l, ui, &ring, &quiet);
    } else {
        start(p, l, ui, &ring);
        if let (Some(r), Some(e)) = (l.error, a.error.as_deref()) {
            p.text_wrapped(e, r.x, r.y, r.w, TextStyle::Small, ERROR);
        }
    }
    if l.max_scroll() > 0.0 {
        let track = l.main.h;
        let h = (track * track / l.content_h).max(24.0);
        let y = l.main.y + (track - h) * (a.scroll / l.max_scroll());
        p.pop_clip();
        p.fill(Rect::new(l.host_right - 6.0, y, 4.0, h), 2.0, BORDER);
    } else {
        p.pop_clip();
    }

    // Footer
    p.line(l.footer.x, l.footer.y + 0.5, l.footer.right(), l.footer.y + 0.5, 1.0, BORDER);
    p.text("Your originals stay where they are.", l.safety, TextStyle::Small, MUTED, Align::Start, true);
    if a.reviewing() {
        quiet(l.cancel, AddHit::Cancel);
        p.text("Cancel", l.cancel, TextStyle::Body, TEXT, Align::Center, true);
        ring(AddFocus::Cancel, l.cancel, 6.0);
        let alpha = if a.can_add() { 1.0 } else { DISABLED };
        p.fill(l.add, 6.0, ACCENT.alpha(alpha));
        p.text(&a.add_label(), l.add, TextStyle::Body, ON_ACCENT.alpha(alpha), Align::Center, true);
        ring(AddFocus::Add, l.add, 6.0);
    }

    p.stroke(Rect::new(0.0, 0.0, l.width, l.height), 8.0, 1.0, BORDER);
}

fn up_arrow(p: &Painter, cx: f32, top: f32, c: Color) {
    p.line(cx, top + 1.0, cx, top + 14.0, 1.5, c);
    p.line(cx - 4.5, top + 5.5, cx, top + 1.0, 1.5, c);
    p.line(cx + 4.5, top + 5.5, cx, top + 1.0, 1.5, c);
}

fn start(p: &Painter, l: &AddLayout, ui: &AddUi, ring: &dyn Fn(AddFocus, Rect, f32)) {
    // Drop area: arrow (24) + 7 + label (20.3) + 7 + formats (17.4), centred.
    p.fill(l.drop, 8.0, DROP_FILL);
    p.stroke_dashed(l.drop, 8.0, ACCENT);
    let content_h = 24.0 + 7.0 + TextStyle::Body.line() + 7.0 + TextStyle::Small.line();
    let y = l.drop.y + (l.drop.h - content_h) / 2.0;
    up_arrow(p, l.drop.x + l.drop.w / 2.0, y + 5.0, ACCENT);
    let label = Rect::new(l.drop.x, y + 31.0, l.drop.w, TextStyle::Body.line());
    p.text("Drop files or a folder here", label, TextStyle::Medium, TEXT, Align::Center, true);
    let formats = Rect::new(l.drop.x, label.bottom() + 7.0, l.drop.w, TextStyle::Small.line());
    p.text("JPG · JPEG · PNG · GIF · MP4", formats, TextStyle::Small, MUTED, Align::Center, true);

    for (r, (title, small), hit, focus) in [
        (l.choose_files, CHOOSE_FILES, AddHit::ChooseFiles, AddFocus::ChooseFiles),
        (l.import_existing, IMPORT_EXISTING, AddHit::ImportExisting, AddFocus::ImportExisting),
    ] {
        let hovered = ui.hover == hit;
        p.fill(r, 6.0, if hovered { RAISED } else { SURFACE });
        p.stroke(r, 6.0, 1.0, if hovered { ACCENT } else { BORDER });
        let tx = r.x + 1.0 + 12.0 + 12.0;
        let t = Rect::new(tx, r.y + 1.0 + 14.0, l.choice_text_w, TextStyle::Medium.line());
        p.text(title, t, TextStyle::Medium, TEXT, Align::Start, true);
        p.text_wrapped(small, tx, t.bottom() + 4.0, l.choice_text_w, TextStyle::Small, MUTED);
        let chevron = Rect::new(r.right() - 1.0 - 12.0 - 8.0, r.y, 8.0, r.h);
        p.text("›", chevron, TextStyle::Body, TEXT, Align::Center, true);
        ring(focus, r, 6.0);
    }
}

fn review(
    p: &Painter,
    a: &AddMedia,
    l: &AddLayout,
    ui: &AddUi,
    ring: &dyn Fn(AddFocus, Rect, f32),
    quiet: &dyn Fn(Rect, AddHit),
) {
    p.text(&a.summary(), l.summary, TextStyle::Small, MUTED, Align::Start, true);
    for (i, (row, g)) in a.rows.iter().zip(&l.rows).enumerate() {
        if g.row.bottom() < l.main.y || g.row.y > l.main.bottom() {
            continue;
        }
        match g.input {
            Some(input) => {
                p.fill(input, 6.0, SURFACE);
                p.stroke(input, 6.0, 1.0, BORDER);
                let focused = a.focus == AddFocus::Row(i);
                let text_r = Rect::new(input.x + 1.0 + 8.0, input.y, input.w - 18.0, input.h);
                p.push_clip(text_r);
                if focused && row.name.all_selected() {
                    p.fill(Rect::new(text_r.x, input.y + 7.0, ui.selection_w, input.h - 14.0), 0.0, ACCENT.alpha(0.35));
                }
                p.text(row.name.text(), Rect::new(text_r.x, text_r.y, 10_000.0, text_r.h), TextStyle::Row, TEXT, Align::Start, true);
                p.pop_clip();
                if row.error.is_some() {
                    // CSS outline: 1px outside the border.
                    p.stroke(input.inset(-1.0), 7.0, 1.0, ERROR);
                } else if focused {
                    p.stroke(input, 6.0, 2.0, RING);
                }
                if focused {
                    let x = (text_r.x + ui.caret_x).round() + 0.5;
                    p.line(x, input.y + 7.0, x, input.bottom() - 7.0, 1.0, TEXT);
                }
                p.text(row.ext(), g.ext, TextStyle::Small, MUTED, Align::Start, true);
            }
            None => {
                let x = l.summary.x + ICON_COL + COL_GAP;
                let r = Rect::new(x, g.row.y + ROW_PAD, g.route.x - COL_GAP - x, TextStyle::Row.line());
                p.text(&row.source, r, TextStyle::Row, TEXT, Align::Start, true);
            }
        }
        if let Some((r, m)) = &g.message {
            let c = if matches!(row.outcome, Some(Err(_))) { ERROR } else { MUTED };
            p.text_wrapped(m, r.x, r.y, r.w, TextStyle::Small, c);
        }
        let route_c = if row.status == RowStatus::Skip { MUTED } else { SOFT_TEXT };
        let route = match (&row.outcome, row.status) {
            (Some(Ok(_)), _) => "Added",
            (Some(Err(_)), _) => "Not added",
            (_, RowStatus::Skip) => "Skipped",
            _ => row.destination(),
        };
        p.text(route, g.route, TextStyle::Small, route_c, Align::End, true);
        if let Some(r) = g.remove {
            widgets::quiet_button(p, r, "×", TextStyle::Body, ui.hover == AddHit::Remove(i), true);
            ring(AddFocus::Remove(i), r, 6.0);
        }
        p.line(g.row.x, g.row.bottom() - 0.5, g.row.right(), g.row.bottom() - 0.5, 1.0, BORDER);
    }
    p.text_wrapped(NOTE, l.note.x, l.note.y, l.note.w, TextStyle::Small, MUTED);
    if a.editable() {
        quiet(l.choose_again, AddHit::ChooseAgain);
        p.text("Choose again", l.choose_again, TextStyle::Body, TEXT, Align::Center, true);
        ring(AddFocus::ChooseAgain, l.choose_again, 6.0);
    }
}
