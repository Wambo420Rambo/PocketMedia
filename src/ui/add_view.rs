//! Add media panel: geometry, hit-testing and drawing. The same
//! panel fills the quick popup and the Manager.

use super::d2d::{Align, Color, Gfx, Painter};
use super::layout::{Rect, TextStyle};
use super::widgets::{self, ACCENT, BG, BORDER, ERROR, FIELD, LINE, MUTED, RAISED, RING, SOFT_TEXT, SUBTLE, SURFACE, TEXT};
use crate::add_media::{AddFocus, AddMedia, RowStatus};

const PAD: f32 = 20.0;
/// Height of the head (back button, title, subtitle, ×). Between the back button
/// and the × it drags the window.
pub const HEAD_H: f32 = 76.0;
const BACK_W: f32 = 36.0;
const CLOSE: f32 = 36.0;
const BUTTON_H: f32 = 36.0;
const DROP_H: f32 = 140.0;
const INPUT_H: f32 = 32.0;
const ROW_PAD: f32 = 12.0;
const COL_GAP: f32 = 10.0;
const ROUTE_W: f32 = 70.0;
/// The × at the end of each review row (leaves a file out).
const REMOVE_W: f32 = 28.0;
/// A post link's text box (taller than a review name: it is the page's main field).
const LINK_H: f32 = 36.0;
/// The panel is at most this wide and centred, so it reads well in the wide
/// Manager window too.
const HOST_MAX_W: f32 = 680.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddHit {
    None,
    Close,
    Back,
    ChooseFiles,
    ImportExisting,
    DownloadTwitter,
    /// A post link's text box, and the × beside it.
    LinkInput(usize),
    RemoveLink(usize),
    AddLink,
    PasteLinks,
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

/// One post link on the Download from Twitter page.
pub struct LinkGeom {
    pub input: Rect,
    /// The × (while the links can be edited).
    pub remove: Option<Rect>,
    /// Why the link was not downloaded, under it.
    pub error: Option<(Rect, String)>,
}

/// Everything positioned for one frame. Content rects are already shifted by scroll.
pub struct AddLayout {
    pub width: f32,
    pub height: f32,
    /// Right edge of the panel, where its scrollbar goes.
    pub host_right: f32,
    pub close: Rect,
    pub back: Rect,
    pub heading: Rect,
    pub subtitle: Rect,
    pub main: Rect,
    pub footer: Rect,
    pub safety: Rect,
    /// The notice in place of the safety line, when there is one.
    pub notice: Option<Rect>,
    pub cancel: Rect,
    pub add: Rect,
    // Start page
    pub drop: Rect,
    pub choose_files: Rect,
    pub import_existing: Rect,
    pub download_twitter: Rect,
    pub choice_text_w: f32,
    /// Why choosing failed, under the choices.
    pub error: Option<Rect>,
    // Review page
    pub summary: Rect,
    pub rows: Vec<RowGeom>,
    pub note: Rect,
    pub choose_again: Rect,
    // Download from Twitter page
    pub links_label: Rect,
    pub links: Vec<LinkGeom>,
    /// "Add another link" (None at the most links) and "Paste links", while editable.
    pub add_link: Option<Rect>,
    pub paste_links: Option<Rect>,
    pub links_hint: Rect,
    /// A message for all the links (`AddMedia::error`), under the hint.
    pub links_error: Option<Rect>,
    /// While downloading: the status line and the progress bar under it.
    pub status: Option<Rect>,
    pub bar: Option<Rect>,
    pub content_h: f32,
}

const NOTE: &str = "Files are copied into Pictures, GIFs and Videos automatically. Rename a file here to make it easier to find by search.";
const CHOOSE_FILES: (&str, &str) = ("Choose files", "Select one or more pictures, GIFs or videos.");
const IMPORT_EXISTING: (&str, &str) = (
    "Import your existing media",
    "Select a folder. Supported files from it and all subfolders are sorted and copied into Pictures, GIFs and Videos. Original files stay where they are.",
);
const DOWNLOAD_TWITTER: (&str, &str) =
    ("Download from Twitter", "Paste links to posts to save their pictures, videos and GIFs.");
const LINKS_HINT: &str = "Links to posts on twitter.com or x.com. Pasting several links at once fills one row each.";
const LINK_PLACEHOLDER: &str = "https://x.com/name/status/1234567890";
pub const ADD_LINK: &str = "Add another link";
pub const PASTE_LINKS: &str = "Paste links";
const LINKS_SAFETY: &str = "Only public posts can be downloaded.";

impl AddLayout {
    pub fn compute(a: &AddMedia, width: f32, height: f32, gfx: &Gfx) -> Self {
        // The panel: full width in the popup, centred at 680 in the Manager.
        let host_w = (width - 2.0).min(HOST_MAX_W);
        let host_x = (width - host_w) / 2.0;
        let x0 = host_x + PAD;
        let inner_w = host_w - 2.0 * PAD;
        // Head: [← 36][12][title / subtitle] ... [× 36] at the window's corner.
        let head_y = 1.0 + 16.0;
        let head_h = TextStyle::Title.line() + 2.0 + TextStyle::Small.line();
        let close = Rect::new(width - 1.0 - 14.0 - CLOSE, head_y + (head_h - CLOSE) / 2.0, CLOSE, CLOSE);
        let back = Rect::new(x0, head_y + (head_h - BACK_W) / 2.0, BACK_W, BACK_W);
        let text_x = back.right() + 14.0;
        let heading = Rect::new(text_x, head_y, (close.x - 8.0).min(x0 + inner_w) - text_x, TextStyle::Title.line());
        let subtitle = Rect::new(text_x, heading.bottom() + 2.0, heading.w, TextStyle::Small.line());
        let main_y = 1.0 + HEAD_H + 6.0;

        // Footer: the safety line (or a notice, which may wrap and makes the footer
        // taller), then the actions (review only) at its right.
        let review = a.reviewing() || a.links_page();
        let add_w = widgets::text_button_w(gfx, &a.add_label(), TextStyle::Medium);
        let cancel_w = widgets::text_button_w(gfx, "Cancel", TextStyle::Body);
        let safety_w = if review { inner_w - add_w - 8.0 - cancel_w - 12.0 } else { inner_w };
        let notice_h = a.notice.as_deref().map(|n| gfx.wrapped_height(n, TextStyle::Small, safety_w));
        let footer_h = 16.0 + notice_h.unwrap_or(0.0).max(BUTTON_H) + 16.0;
        let footer = Rect::new(host_x, height - 1.0 - footer_h, host_w, footer_h);
        let actions_y = footer.y + 16.0;
        let add = Rect::new(x0 + inner_w - add_w, actions_y, add_w, BUTTON_H);
        let cancel = Rect::new(add.x - 8.0 - cancel_w, actions_y, cancel_w, BUTTON_H);
        let safety = Rect::new(x0, actions_y, safety_w, BUTTON_H);
        let notice = notice_h.map(|h| Rect::new(x0, actions_y + ((BUTTON_H - h) / 2.0).max(0.0), safety_w, h));

        let main = Rect::new(x0, main_y, inner_w, (footer.y - main_y).max(0.0));
        let top = main.y - a.scroll;

        // Start page.
        let drop = Rect::new(x0, top, inner_w, DROP_H);
        let choice_text_w = inner_w - 18.0 - 18.0 - 12.0 - 10.0;
        let choice_h = |small: &str| 32.0 + TextStyle::Medium.line() + 4.0 + gfx.wrapped_height(small, TextStyle::Small, choice_text_w);
        let choose_files = Rect::new(x0, drop.bottom() + 14.0, inner_w, choice_h(CHOOSE_FILES.1));
        let import_existing = Rect::new(x0, choose_files.bottom() + 10.0, inner_w, choice_h(IMPORT_EXISTING.1));
        let download_twitter = Rect::new(x0, import_existing.bottom() + 10.0, inner_w, choice_h(DOWNLOAD_TWITTER.1));
        let error = a.error.as_deref().map(|e| Rect::new(x0, download_twitter.bottom() + 12.0, inner_w, gfx.wrapped_height(e, TextStyle::Small, inner_w)));

        // Download from Twitter page.
        let links_label = Rect::new(x0, top, inner_w, TextStyle::Medium.line());
        let editable = a.links_editable();
        let input_w = inner_w - 8.0 - REMOVE_W;
        let mut y = links_label.bottom() + 10.0;
        let mut links = Vec::new();
        for (i, _) in a.links.iter().enumerate() {
            let input = Rect::new(x0, y, input_w, LINK_H);
            let remove = editable.then(|| Rect::new(x0 + inner_w - REMOVE_W, y + (LINK_H - REMOVE_W) / 2.0, REMOVE_W, REMOVE_W));
            y = input.bottom();
            let error = a.link_errors.get(i).cloned().flatten().map(|m| {
                let r = Rect::new(x0, y + 4.0, input_w, gfx.wrapped_height(&m, TextStyle::Small, input_w));
                y = r.bottom();
                (r, m)
            });
            links.push(LinkGeom { input, remove, error });
            y += 8.0;
        }
        let (mut add_link, mut paste_links) = (None, None);
        if editable {
            let mut x = x0;
            if a.links.len() < crate::twitter::MAX_LINKS {
                let w = widgets::text_button_w(gfx, ADD_LINK, TextStyle::Body);
                add_link = Some(Rect::new(x, y + 2.0, w, BUTTON_H));
                x += w + 8.0;
            }
            let w = widgets::text_button_w(gfx, PASTE_LINKS, TextStyle::Body);
            paste_links = Some(Rect::new(x, y + 2.0, w, BUTTON_H));
            y += 2.0 + BUTTON_H;
        }
        let links_hint = Rect::new(x0, y + 12.0, inner_w, gfx.wrapped_height(LINKS_HINT, TextStyle::Small, inner_w));
        y = links_hint.bottom();
        let links_error = (a.links_page() && a.error.is_some()).then(|| {
            let h = gfx.wrapped_height(a.error.as_deref().unwrap_or(""), TextStyle::Small, inner_w);
            let r = Rect::new(x0, y + 8.0, inner_w, h);
            y = r.bottom();
            r
        });
        let status = a.download_status().map(|_| {
            let r = Rect::new(x0, y + 14.0, inner_w, TextStyle::Small.line());
            y = r.bottom();
            r
        });
        let bar = status.map(|_| {
            let r = Rect::new(x0, y + 6.0, inner_w, 4.0);
            y = r.bottom();
            r
        });
        let links_bottom = y;

        // Review page.
        let summary = Rect::new(x0, top, inner_w, TextStyle::Small.line());
        let name_x = x0;
        let name_w = inner_w - COL_GAP - ROUTE_W - COL_GAP - REMOVE_W;
        // Every name field is the same width; the extension follows it.
        let ext_max = gfx.measure(".jpeg", TextStyle::Small);
        let input_w = name_w - 6.0 - ext_max;
        let route_x = x0 + inner_w - REMOVE_W - COL_GAP - ROUTE_W;
        let mut y = summary.bottom() + 8.0;
        let mut rows = Vec::new();
        for row in &a.rows {
            let ext_w = gfx.measure(row.ext(), TextStyle::Small);
            let (input, first_h) = match row.status {
                RowStatus::Copy => (Some(Rect::new(name_x, y + ROW_PAD, input_w, INPUT_H)), INPUT_H),
                RowStatus::Skip => (None, TextStyle::Row.line()),
            };
            let msg = row.message();
            let msg_h = msg.as_deref().map_or(0.0, |m| 4.0 + gfx.wrapped_height(m, TextStyle::Small, name_w));
            let inner_h = first_h + msg_h;
            let r = Rect::new(x0, y, inner_w, ROW_PAD + inner_h + ROW_PAD + 1.0);
            let ext = Rect::new(name_x + input_w + 6.0, y + ROW_PAD, ext_w, first_h);
            let message = msg.map(|m| (Rect::new(name_x, y + ROW_PAD + first_h + 4.0, name_w, msg_h - 4.0), m));
            // The destination lines up with the name, not with the row's middle.
            let route = Rect::new(route_x, y + ROW_PAD, ROUTE_W, first_h);
            let remove = a
                .editable()
                .then(|| Rect::new(x0 + inner_w - REMOVE_W, y + ROW_PAD + (first_h - REMOVE_W) / 2.0, REMOVE_W, REMOVE_W));
            rows.push(RowGeom { row: r, input, ext, message, route, remove });
            y = r.bottom();
        }
        let note_h = gfx.wrapped_height(NOTE, TextStyle::Small, inner_w);
        let note = Rect::new(x0, y + 12.0, inner_w, note_h);
        let ca_w = widgets::text_button_w(gfx, "Choose again", TextStyle::Body);
        let choose_again = Rect::new(x0, note.bottom() + 10.0, ca_w, BUTTON_H);

        let content_h = if a.links_page() {
            links_bottom + 18.0 - top
        } else if review {
            choose_again.bottom() + 18.0 - top
        } else {
            error.map_or(download_twitter.bottom(), |e| e.bottom()) + 18.0 - top
        };

        AddLayout {
            width,
            height,
            host_right: host_x + host_w,
            close,
            back,
            heading,
            subtitle,
            main,
            footer,
            safety,
            notice,
            cancel,
            add,
            drop,
            choose_files,
            import_existing,
            download_twitter,
            choice_text_w,
            error,
            summary,
            rows,
            note,
            choose_again,
            links_label,
            links,
            add_link,
            paste_links,
            links_hint,
            links_error,
            status,
            bar,
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
            AddFocus::DownloadTwitter => Some(self.download_twitter),
            AddFocus::Link(i) | AddFocus::RemoveLink(i) => self.links.get(i).map(|g| g.input),
            AddFocus::AddLink => self.add_link,
            AddFocus::PasteLinks => self.paste_links,
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
        if (a.reviewing() || a.links_page()) && self.footer.contains(x, y) {
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
        if a.links_page() {
            if !a.links_editable() {
                return AddHit::Main;
            }
            if let Some(i) = self.links.iter().position(|g| g.input.contains(x, y)) {
                return AddHit::LinkInput(i);
            }
            if let Some(i) = self.links.iter().position(|g| g.remove.is_some_and(|r| r.contains(x, y))) {
                return AddHit::RemoveLink(i);
            }
            if self.add_link.is_some_and(|r| r.contains(x, y)) {
                return AddHit::AddLink;
            }
            if self.paste_links.is_some_and(|r| r.contains(x, y)) {
                return AddHit::PasteLinks;
            }
            return AddHit::Main;
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
        } else if self.download_twitter.contains(x, y) {
            return AddHit::DownloadTwitter;
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
    /// How far the focused link's text is moved left so the caret shows.
    pub text_shift: f32,
}

pub fn paint(p: &Painter, a: &AddMedia, l: &AddLayout, ui: &AddUi) {
    p.clear(BG);
    let ring = |focus: AddFocus, r: Rect, radius: f32| {
        if a.focus_visible && a.focus == focus {
            p.stroke(r.inset(-2.0), radius + 2.0, 2.0, RING);
        }
    };
    let focused = |f: AddFocus| a.focus_visible && a.focus == f;

    // Head
    widgets::back_button(p, l.back, ui.hover == AddHit::Back, focused(AddFocus::Back));
    widgets::close_button(p, l.close, ui.hover == AddHit::Close, focused(AddFocus::Close));
    let (heading, sub) = if a.reviewing() {
        ("Review files", "Check the files before adding them.")
    } else if a.links_page() {
        ("Download from Twitter", "Save pictures, videos and GIFs from one or more posts.")
    } else {
        ("Add media", "Pictures, GIFs and videos, in one place.")
    };
    p.text(heading, l.heading, TextStyle::Title, TEXT, Align::Start, true);
    p.text(sub, l.subtitle, TextStyle::Small, MUTED, Align::Start, true);

    // Scrolling area
    p.push_clip(l.main);
    if a.links_page() {
        links_page(p, a, l, ui, &ring);
    } else if a.reviewing() {
        review(p, a, l, ui, &ring);
    } else {
        start(p, l, ui, &ring);
        if let (Some(r), Some(e)) = (l.error, a.error.as_deref()) {
            p.text_wrapped(e, r.x, r.y, r.w, TextStyle::Small, ERROR);
        }
    }
    p.pop_clip();
    if l.max_scroll() > 0.0 {
        let track = l.main.h;
        let h = (track * track / l.content_h).max(24.0);
        let y = l.main.y + (track - h) * (a.scroll / l.max_scroll());
        widgets::scrollbar(p, Rect::new(l.host_right - 8.0, y, 4.0, h), false);
    }

    // Footer
    p.line(l.footer.x + PAD, l.footer.y + 0.5, l.footer.right() - PAD, l.footer.y + 0.5, 1.0, LINE);
    match (l.notice, a.notice.as_deref()) {
        (Some(r), Some(n)) => p.text_wrapped(n, r.x, r.y, r.w, TextStyle::Small, ACCENT),
        _ => {
            let safety = if a.links_page() || a.from_download { LINKS_SAFETY } else { "Your originals stay where they are." };
            p.text(safety, l.safety, TextStyle::Small, SUBTLE, Align::Start, true)
        }
    }
    if a.reviewing() || a.links_page() {
        let enabled = if a.links_page() { a.can_download() } else { a.can_add() };
        widgets::button(p, l.cancel, &["Cancel"], ui.hover == AddHit::Cancel, focused(AddFocus::Cancel));
        widgets::primary_button(p, l.add, &a.add_label(), enabled, ui.hover == AddHit::Add, focused(AddFocus::Add));
    }

    p.stroke(Rect::new(0.0, 0.0, l.width, l.height), 8.0, 1.0, BORDER);
}

fn up_arrow(p: &Painter, cx: f32, top: f32, c: Color) {
    p.line(cx, top + 1.0, cx, top + 16.0, 2.0, c);
    p.line(cx - 5.5, top + 6.5, cx, top + 1.0, 2.0, c);
    p.line(cx + 5.5, top + 6.5, cx, top + 1.0, 2.0, c);
}

fn chevron_right(p: &Painter, cx: f32, cy: f32, c: Color) {
    p.line(cx - 2.0, cy - 5.0, cx + 3.0, cy, 1.75, c);
    p.line(cx + 3.0, cy, cx - 2.0, cy + 5.0, 1.75, c);
}

fn start(p: &Painter, l: &AddLayout, ui: &AddUi, ring: &dyn Fn(AddFocus, Rect, f32)) {
    // Drop area: a circle with an arrow, the label and the formats, centred.
    p.fill(l.drop, 14.0, ACCENT.alpha(0.06));
    p.stroke_dashed(l.drop, 14.0, ACCENT.alpha(0.7));
    let content_h = 40.0 + 10.0 + TextStyle::Medium.line() + 4.0 + TextStyle::Small.line();
    let y = l.drop.y + (l.drop.h - content_h) / 2.0;
    let cx = l.drop.x + l.drop.w / 2.0;
    p.fill_circle(cx, y + 20.0, 20.0, widgets::SELECTED);
    up_arrow(p, cx, y + 11.0, ACCENT);
    let label = Rect::new(l.drop.x, y + 50.0, l.drop.w, TextStyle::Medium.line());
    p.text("Drop files or a folder here", label, TextStyle::Medium, TEXT, Align::Center, true);
    let formats = Rect::new(l.drop.x, label.bottom() + 4.0, l.drop.w, TextStyle::Small.line());
    p.text("JPG · JPEG · PNG · GIF · MP4", formats, TextStyle::Small, SUBTLE, Align::Center, true);

    for (r, (title, small), hit, focus) in [
        (l.choose_files, CHOOSE_FILES, AddHit::ChooseFiles, AddFocus::ChooseFiles),
        (l.import_existing, IMPORT_EXISTING, AddHit::ImportExisting, AddFocus::ImportExisting),
        (l.download_twitter, DOWNLOAD_TWITTER, AddHit::DownloadTwitter, AddFocus::DownloadTwitter),
    ] {
        let hovered = ui.hover == hit;
        p.fill(r, 12.0, if hovered { RAISED } else { SURFACE });
        let tx = r.x + 18.0;
        let t = Rect::new(tx, r.y + 16.0, l.choice_text_w, TextStyle::Medium.line());
        p.text(title, t, TextStyle::Medium, TEXT, Align::Start, true);
        p.text_wrapped(small, tx, t.bottom() + 4.0, l.choice_text_w, TextStyle::Small, MUTED);
        chevron_right(p, r.right() - 22.0, r.y + r.h / 2.0, if hovered { TEXT } else { SUBTLE });
        ring(focus, r, 12.0);
    }
}

fn review(p: &Painter, a: &AddMedia, l: &AddLayout, ui: &AddUi, ring: &dyn Fn(AddFocus, Rect, f32)) {
    p.text(&a.summary(), l.summary, TextStyle::Small, MUTED, Align::Start, true);
    for (i, (row, g)) in a.rows.iter().zip(&l.rows).enumerate() {
        if g.row.bottom() < l.main.y || g.row.y > l.main.bottom() {
            continue;
        }
        match g.input {
            Some(input) => {
                p.fill(input, widgets::RADIUS, FIELD);
                let focused = a.focus == AddFocus::Row(i);
                let text_r = Rect::new(input.x + 10.0, input.y, input.w - 20.0, input.h);
                p.push_clip(text_r);
                if focused && row.name.all_selected() {
                    p.fill(Rect::new(text_r.x, input.y + 7.0, ui.selection_w, input.h - 14.0), 0.0, ACCENT.alpha(0.35));
                }
                p.text(row.name.text(), Rect::new(text_r.x, text_r.y, 10_000.0, text_r.h), TextStyle::Row, TEXT, Align::Start, true);
                p.pop_clip();
                if row.error.is_some() {
                    p.stroke(input, widgets::RADIUS, 1.5, ERROR);
                } else if focused {
                    p.stroke(input, widgets::RADIUS, 2.0, ACCENT);
                }
                if focused {
                    let x = (text_r.x + ui.caret_x).round() + 0.5;
                    p.line(x, input.y + 8.0, x, input.bottom() - 8.0, 1.0, TEXT);
                }
                p.text(row.ext(), g.ext, TextStyle::Small, SUBTLE, Align::Start, true);
            }
            None => {
                let r = Rect::new(l.summary.x, g.row.y + ROW_PAD, g.route.x - COL_GAP - l.summary.x, TextStyle::Row.line());
                p.text(&row.source, r, TextStyle::Row, MUTED, Align::Start, true);
            }
        }
        if let Some((r, m)) = &g.message {
            let c = if matches!(row.outcome, Some(Err(_))) { ERROR } else { SUBTLE };
            p.text_wrapped(m, r.x, r.y, r.w, TextStyle::Small, c);
        }
        let route_c = if row.status == RowStatus::Skip { SUBTLE } else { SOFT_TEXT };
        let route = match (&row.outcome, row.status) {
            (Some(Ok(_)), _) => "Added",
            (Some(Err(_)), _) => "Not added",
            (_, RowStatus::Skip) => "Skipped",
            _ => row.destination(),
        };
        p.text(route, g.route, TextStyle::Small, route_c, Align::End, true);
        if let Some(r) = g.remove {
            widgets::close_button(p, r, ui.hover == AddHit::Remove(i), false);
            ring(AddFocus::Remove(i), r, widgets::RADIUS);
        }
        p.line(g.row.x, g.row.bottom() - 0.5, g.row.right(), g.row.bottom() - 0.5, 1.0, LINE);
    }
    p.text_wrapped(NOTE, l.note.x, l.note.y, l.note.w, TextStyle::Small, SUBTLE);
    if a.editable() {
        let focus = a.focus_visible && a.focus == AddFocus::ChooseAgain;
        widgets::button(p, l.choose_again, &["Choose again"], ui.hover == AddHit::ChooseAgain, focus);
    }
}

fn links_page(p: &Painter, a: &AddMedia, l: &AddLayout, ui: &AddUi, ring: &dyn Fn(AddFocus, Rect, f32)) {
    p.text("Post links", l.links_label, TextStyle::Medium, TEXT, Align::Start, true);
    let editable = a.links_editable();
    for (i, (field, g)) in a.links.iter().zip(&l.links).enumerate() {
        if g.input.bottom() < l.main.y || g.input.y > l.main.bottom() {
            continue;
        }
        let input = g.input;
        let focused = editable && a.focus == AddFocus::Link(i);
        p.fill(input, widgets::RADIUS, FIELD);
        let text_r = Rect::new(input.x + 10.0, input.y, input.w - 20.0, input.h);
        let shift = if focused { ui.text_shift } else { 0.0 };
        p.push_clip(text_r);
        if field.is_empty() {
            p.text(LINK_PLACEHOLDER, Rect::new(text_r.x, text_r.y, 10_000.0, text_r.h), TextStyle::Row, SUBTLE, Align::Start, true);
        } else {
            if focused && field.all_selected() {
                p.fill(Rect::new(text_r.x - shift, input.y + 8.0, ui.selection_w, input.h - 16.0), 0.0, ACCENT.alpha(0.35));
            }
            let c = if editable { TEXT } else { MUTED };
            p.text(field.text(), Rect::new(text_r.x - shift, text_r.y, 10_000.0, text_r.h), TextStyle::Row, c, Align::Start, true);
        }
        p.pop_clip();
        if g.error.is_some() {
            p.stroke(input, widgets::RADIUS, 1.5, ERROR);
        } else if focused {
            p.stroke(input, widgets::RADIUS, 2.0, ACCENT);
        }
        if focused {
            let x = (text_r.x + ui.caret_x - shift).round() + 0.5;
            p.line(x, input.y + 9.0, x, input.bottom() - 9.0, 1.0, TEXT);
        }
        if let Some((r, m)) = &g.error {
            p.text_wrapped(m, r.x, r.y, r.w, TextStyle::Small, ERROR);
        }
        if let Some(r) = g.remove {
            widgets::close_button(p, r, ui.hover == AddHit::RemoveLink(i), false);
            ring(AddFocus::RemoveLink(i), r, widgets::RADIUS);
        }
    }
    if let Some(r) = l.add_link {
        widgets::button(p, r, &[ADD_LINK], ui.hover == AddHit::AddLink, a.focus_visible && a.focus == AddFocus::AddLink);
    }
    if let Some(r) = l.paste_links {
        widgets::button(p, r, &[PASTE_LINKS], ui.hover == AddHit::PasteLinks, a.focus_visible && a.focus == AddFocus::PasteLinks);
    }
    let h = l.links_hint;
    p.text_wrapped(LINKS_HINT, h.x, h.y, h.w, TextStyle::Small, SUBTLE);
    if let (Some(r), Some(e)) = (l.links_error, a.error.as_deref()) {
        p.text_wrapped(e, r.x, r.y, r.w, TextStyle::Small, ERROR);
    }
    if let (Some(r), Some(status)) = (l.status, a.download_status()) {
        p.text(&status, r, TextStyle::Small, MUTED, Align::Start, true);
    }
    if let Some(bar) = l.bar {
        p.fill(bar, 2.0, RAISED);
        if let Some(f) = a.download_fraction() {
            p.fill(Rect::new(bar.x, bar.y, (bar.w * f).max(4.0), bar.h), 2.0, ACCENT);
        }
    }
}
