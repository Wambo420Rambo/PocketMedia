//! The Manager's Settings page: geometry, hit-testing and drawing. Numbers come
//! from the wireframe CSS (.pm-settings, .pm-panel, .pm-row, .pm-choice, the
//! pinned action bar) and were measured on focused-40 and focused-41.
//!
//! Everything inside the scroll area is laid out with the scroll offset applied.

use super::d2d::{Align, Gfx, Painter};
use super::layout::{Rect, TextStyle, text_button_w};
use super::manager_view::{Frame, FrameHit, FrameLook, paint_border, paint_frame};
use super::widgets::{self, ACCENT, BG, BORDER, MUTED, SOFT, SURFACE, TEXT};
use crate::manager_state::Nav;
use crate::paths;
use crate::settings_state::{Profile, SFocus, SettingsPage, VideoPreview};

/// The settings column is at most 700 wide; panels leave 10 for the scrollbar gutter.
const CONTENT_W: f32 = 700.0;
const PANEL_W: f32 = 690.0;
/// Panel border + padding.
const PANEL_PAD: f32 = 16.0;
const BUTTON_H: f32 = 36.0;
const SELECT_W: f32 = 160.0;
/// Checkbox 16px plus the browser's 3px margins.
const CHECK: f32 = 16.0;
const CHECK_BOX_H: f32 = CHECK + 6.0;
const RADIO: f32 = 13.0;
/// Small text in settings rows uses line-height 1.5 (18px), measured.
const SMALL_LH: f32 = 18.0;
const TITLE_LH: f32 = 14.0 * 1.45;
/// Gap between a title and its small text (CSS margin 4, plus about 0.5-1 px of
/// Chrome's pixel rounding, measured on focused-40 and focused-41).
const DESC_GAP: f32 = 4.5;
const DESC_GAP_CARD: f32 = 5.5;
/// "Choose folder…" wraps onto two lines in the wireframe (its button is squeezed
/// by the long description); it is drawn the same way.
const CHOOSE_W: f32 = 96.0;
const CHOOSE_H: f32 = 56.0;

pub const LEAD: &str = "Choose how PocketMedia behaves.";
const VIDEO_TITLE: &str = "Video preview";
const VIDEO_DESC: &str = "Video playback uses extra CPU and memory. Choose Off to save resources.";
const GIF_NOTE: &str = "GIFs always loop while visible. Playback stops when the window is hidden.";
const SHORTCUT_TITLE: &str = "Global shortcut";
pub const SHORTCUT: &str = "Win + Alt + .";
const CLOSE_AFTER_TITLE: &str = "Close after inserting";
const FOLDER_TITLE: &str = "Media folder";
const IMPORT_TITLE: &str = "Import your existing media";
const IMPORT_DESC: &str = "Select a folder. Supported files from it and all subfolders are sorted and copied into Pictures, GIFs and Videos. Original files stay where they are.";
const BACKUP_TITLE: &str = "Back up library";
const BACKUP_DESC: &str = "Includes media and settings. Choose a backup location outside PocketMedia.";
const AUTOSTART_TITLE: &str = "Autostart PocketMedia";
const AUTOSTART_DESC: &str = "Off unless you choose to enable it.";
const SECTIONS: [&str; 4] = ["RESOURCE USAGE", "QUICK POPUP", "LIBRARY", "STARTUP"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SHit {
    None,
    Caption,
    Close,
    QuickPopup,
    Nav(usize),
    Profile(usize),
    Video,
    VideoRow(usize),
    Change,
    CloseAfter,
    OpenFolder,
    ChooseFolder,
    Backup,
    Autostart,
    Cancel,
    Save,
}

/// A resource-usage choice card.
pub struct Choice {
    pub card: Rect,
    pub radio: Rect,
    pub title: Rect,
    pub desc: Rect,
}

/// A settings row: title, optional small text under it, and a control on the right.
pub struct Row {
    /// Whole row (for clicks on checkbox rows).
    pub area: Rect,
    /// y of the separator above the row, if any.
    pub border: Option<f32>,
    pub title: Rect,
    pub desc: Option<Rect>,
    pub control: Rect,
}

pub struct SettingsLayout {
    pub frame: Frame,
    pub heading: Rect,
    pub lead: Rect,
    /// The scroll area (clip).
    pub view: Rect,
    pub content_h: f32,
    pub labels: [Rect; 4],
    pub panels: [Rect; 4],
    pub choices: [Choice; 2],
    pub video: Row,
    pub video_menu: widgets::Menu,
    pub gif_note: Rect,
    pub shortcut: Row,
    pub close_after: Row,
    pub folder: Row,
    pub import: Row,
    pub backup: Row,
    pub autostart: Row,
    pub bar_y: f32,
    pub bar_x: (f32, f32),
    pub note: Rect,
    pub cancel: Rect,
    pub save: Rect,
}

/// Lays out one row starting at `y` (above its separator, if any).
fn row(gfx: &Gfx, x: f32, w: f32, y: f32, border: bool, desc: Option<&str>, control: (f32, f32)) -> Row {
    let top = if border { y + 1.0 } else { y };
    let content_y = top + 10.0;
    let (cw, ch) = control;
    let box_h = if ch == CHECK { CHECK_BOX_H } else { ch };
    let control_x = if ch == CHECK { x + w - 3.0 - cw } else { x + w - cw };
    let text_w = control_x - 14.0 - x;
    let desc_h = desc.map(|d| gfx.line_count(d, TextStyle::Small, text_w) as f32 * SMALL_LH);
    let text_h = TITLE_LH + desc_h.map_or(0.0, |h| DESC_GAP + h);
    let content_h = text_h.max(box_h);
    let title = Rect::new(x, content_y, text_w, TITLE_LH);
    let desc = desc_h.map(|h| Rect::new(x, title.bottom() + DESC_GAP, text_w, h));
    let control = Rect::new(control_x, content_y + (content_h - ch) / 2.0, cw, ch);
    let bottom = content_y + content_h + 10.0;
    Row { area: Rect::new(x, y, w, bottom - y), border: border.then_some(y), title, desc, control }
}

impl SettingsLayout {
    pub fn compute(s: &SettingsPage, width: f32, height: f32, gfx: &Gfx) -> Self {
        let frame = Frame::compute(width, height, "Settings", height - 1.0, gfx);
        let body_y = Frame::body_y();
        let x0 = frame.sidebar.right() + 20.0;
        let inner_w = width - 1.0 - 20.0 - x0;
        let heading = Rect::new(x0, body_y + 20.0, inner_w, 22.0 * 1.25);
        let lead = Rect::new(x0, heading.bottom() + 5.0, inner_w, TextStyle::Small.line());

        // Pinned action bar: 1px line, 14 padding, 36px buttons, 20 window padding.
        let buttons_y = height - 1.0 - 20.0 - BUTTON_H;
        let bar_y = buttons_y - 14.0 - 1.0;
        let bar_x = (x0, x0 + CONTENT_W.min(inner_w));
        let save_w = text_button_w(gfx.measure("Save changes", TextStyle::Body));
        let save = Rect::new(bar_x.1 - save_w, buttons_y, save_w, BUTTON_H);
        let cancel_w = text_button_w(gfx.measure("Cancel", TextStyle::Body));
        let cancel = Rect::new(save.x - 8.0 - cancel_w, buttons_y, cancel_w, BUTTON_H);
        let note = Rect::new(x0, buttons_y, cancel.x - 12.0 - x0, BUTTON_H);

        // Measured: the scroll area starts 84 below the title bar and ends 12 above the bar line.
        let view_y = body_y + 84.0;
        let view = Rect::new(x0, view_y, bar_x.1 - x0, bar_y - 12.0 - view_y);
        let panel_w = PANEL_W.min(view.w - 10.0);
        let ix = x0 + PANEL_PAD;
        let iw = panel_w - 2.0 * PANEL_PAD;

        let top = view_y - s.scroll;
        let mut y = top;
        let mut labels = [Rect::default(); 4];
        let mut panels = [Rect::default(); 4];
        // Section label (17.4) + 10, then the panel; sections are 16 apart.
        let mut section = |i: usize, y: &mut f32| {
            if i > 0 {
                *y += 16.0;
            }
            labels[i] = Rect::new(x0, *y, panel_w, TextStyle::Label.line());
            *y += TextStyle::Label.line() + 10.0;
            *y
        };

        // Resource usage: two choice cards (gap 10), the video row and a note.
        let p0 = section(0, &mut y);
        let card_w = (iw - 10.0) / 2.0;
        let text_w = card_w - 2.0 - 24.0 - RADIO - 9.0;
        let lines = Profile::BOTH.iter().map(|p| gfx.line_count(p.description(), TextStyle::Small, text_w)).max().unwrap_or(1);
        let card_h = 1.0 + 12.0 + TITLE_LH + DESC_GAP_CARD + lines as f32 * SMALL_LH + 12.0 + 1.0;
        let choice = |i: usize| {
            let card = Rect::new(ix + i as f32 * (card_w + 10.0), p0 + PANEL_PAD, card_w, card_h);
            let title = Rect::new(card.x + 13.0 + RADIO + 9.0, card.y + 13.0, text_w, TITLE_LH);
            let radio = Rect::new(card.x + 13.0, title.y + (TITLE_LH - RADIO) / 2.0, RADIO, RADIO);
            let desc = Rect::new(title.x, title.bottom() + DESC_GAP_CARD, text_w, lines as f32 * SMALL_LH);
            Choice { card, radio, title, desc }
        };
        let choices = [choice(0), choice(1)];
        let video = row(gfx, ix, iw, p0 + PANEL_PAD + card_h, false, Some(VIDEO_DESC), (SELECT_W, BUTTON_H));
        let video_menu = widgets::Menu::below(video.control, VideoPreview::BOTH.len());
        let gif_note = Rect::new(ix, video.area.bottom() + 2.5, iw, SMALL_LH);
        y = gif_note.bottom() + PANEL_PAD;
        panels[0] = Rect::new(x0, p0, panel_w, y - p0);

        // Quick popup: shortcut with Change, Close after inserting.
        let p1 = section(1, &mut y);
        let change = (text_button_w(gfx.measure("Change", TextStyle::Body)), BUTTON_H);
        let shortcut = row(gfx, ix, iw, p1 + PANEL_PAD, false, Some(SHORTCUT), change);
        let close_after = row(gfx, ix, iw, shortcut.area.bottom(), true, None, (CHECK, CHECK));
        y = close_after.area.bottom() + PANEL_PAD;
        panels[1] = Rect::new(x0, p1, panel_w, y - p1);

        // Library: media folder, import, backup.
        let p2 = section(2, &mut y);
        let open = (text_button_w(gfx.measure("Open folder", TextStyle::Body)), BUTTON_H);
        let folder = row(gfx, ix, iw, p2 + PANEL_PAD, false, Some(&paths::media_text()), open);
        let import = row(gfx, ix, iw, folder.area.bottom(), true, Some(IMPORT_DESC), (CHOOSE_W, CHOOSE_H));
        let backup_w = text_button_w(gfx.measure("Back up library", TextStyle::Body));
        let backup = row(gfx, ix, iw, import.area.bottom(), true, Some(BACKUP_DESC), (backup_w, BUTTON_H));
        y = backup.area.bottom() + PANEL_PAD;
        panels[2] = Rect::new(x0, p2, panel_w, y - p2);

        // Startup.
        let p3 = section(3, &mut y);
        let autostart = row(gfx, ix, iw, p3 + PANEL_PAD, false, Some(AUTOSTART_DESC), (CHECK, CHECK));
        y = autostart.area.bottom() + PANEL_PAD;
        panels[3] = Rect::new(x0, p3, panel_w, y - p3);

        let content_h = y + 18.5 - top;
        SettingsLayout {
            frame,
            heading,
            lead,
            view,
            content_h,
            labels,
            panels,
            choices,
            video,
            video_menu,
            gif_note,
            shortcut,
            close_after,
            folder,
            import,
            backup,
            autostart,
            bar_y,
            bar_x,
            note,
            cancel,
            save,
        }
    }

    pub fn max_scroll(&self) -> f32 {
        (self.content_h - self.view.h).max(0.0)
    }

    /// Where a focusable control is (scrolled), for scroll-into-view and the self-test.
    pub fn focus_rect(&self, s: &SettingsPage, f: SFocus) -> Option<Rect> {
        Some(match f {
            SFocus::Close => self.frame.close,
            SFocus::QuickPopup => self.frame.quick_popup,
            SFocus::Nav(i) => *self.frame.nav.get(i)?,
            SFocus::Profile => self.choices[s.draft.profile.index()].card,
            SFocus::Video => self.video.control,
            SFocus::Change => self.shortcut.control,
            SFocus::CloseAfter => self.close_after.control,
            SFocus::OpenFolder => self.folder.control,
            SFocus::ChooseFolder => self.import.control,
            SFocus::Backup => self.backup.control,
            SFocus::Autostart => self.autostart.control,
            SFocus::Cancel => self.cancel,
            SFocus::Save => self.save,
        })
    }

    /// Scroll offset that brings `f` fully into the scroll area.
    pub fn scroll_to_show(&self, s: &SettingsPage, f: SFocus) -> f32 {
        let Some(r) = self.focus_rect(s, f) else { return s.scroll };
        let inside = matches!(f, SFocus::Close | SFocus::QuickPopup | SFocus::Nav(_) | SFocus::Cancel | SFocus::Save);
        if inside {
            return s.scroll;
        }
        let margin = 8.0;
        let target = if r.y - margin < self.view.y {
            s.scroll - (self.view.y - (r.y - margin))
        } else if r.bottom() + margin > self.view.bottom() {
            s.scroll + (r.bottom() + margin - self.view.bottom())
        } else {
            s.scroll
        };
        target.clamp(0.0, self.max_scroll())
    }

    pub fn hit(&self, s: &SettingsPage, x: f32, y: f32) -> SHit {
        if s.video_open.is_some() {
            if let Some(i) = self.video_menu.hit(x, y) {
                return SHit::VideoRow(i);
            }
            if self.video_menu.frame.contains(x, y) {
                return SHit::None;
            }
        }
        match self.frame.hit(x, y) {
            Some(FrameHit::Close) => return SHit::Close,
            Some(FrameHit::QuickPopup) => return SHit::QuickPopup,
            Some(FrameHit::Caption) => return SHit::Caption,
            Some(FrameHit::Nav(i)) => return SHit::Nav(i),
            None => {}
        }
        if self.cancel.contains(x, y) {
            return SHit::Cancel;
        }
        if self.save.contains(x, y) {
            return SHit::Save;
        }
        if !self.view.contains(x, y) {
            return SHit::None;
        }
        if let Some(i) = self.choices.iter().position(|c| c.card.contains(x, y)) {
            return SHit::Profile(i);
        }
        let controls = [
            (self.video.control, SHit::Video),
            (self.shortcut.control, SHit::Change),
            (self.folder.control, SHit::OpenFolder),
            (self.import.control, SHit::ChooseFolder),
            (self.backup.control, SHit::Backup),
            // A checkbox row toggles from anywhere on the row.
            (self.close_after.area, SHit::CloseAfter),
            (self.autostart.area, SHit::Autostart),
        ];
        controls.iter().find(|(r, _)| r.contains(x, y)).map_or(SHit::None, |(_, h)| *h)
    }
}

/// Pointer facts that are not part of `SettingsPage`.
pub struct SettingsUi {
    pub hover: SHit,
}

fn title_and_desc(p: &Painter, r: &Row, title: &str, desc: Option<&str>) {
    p.text(title, r.title, TextStyle::Body, TEXT, Align::Start, true);
    if let (Some(d), Some(dr)) = (desc, r.desc) {
        p.text_wrapped_lh(d, dr.x, dr.y, dr.w, TextStyle::Small, MUTED, SMALL_LH);
    }
}

fn separator(p: &Painter, r: &Row) {
    if let Some(y) = r.border {
        p.line(r.area.x, y + 0.5, r.area.right(), y + 0.5, 1.0, BORDER);
    }
}

pub fn paint(p: &Painter, s: &SettingsPage, l: &SettingsLayout, ui: &SettingsUi) {
    let ring = |f: SFocus| s.focus_visible && s.focus == f;
    let hover = |h: SHit| ui.hover == h;
    let look = FrameLook {
        hover: match ui.hover {
            SHit::Close => Some(FrameHit::Close),
            SHit::QuickPopup => Some(FrameHit::QuickPopup),
            SHit::Nav(i) => Some(FrameHit::Nav(i)),
            _ => None,
        },
        ring: match s.focus {
            SFocus::Close if s.focus_visible => Some(FrameHit::Close),
            SFocus::QuickPopup if s.focus_visible => Some(FrameHit::QuickPopup),
            SFocus::Nav(i) if s.focus_visible => Some(FrameHit::Nav(i)),
            _ => None,
        },
    };
    paint_frame(p, &l.frame, "Settings", &|n| n == Nav::Settings, look);

    p.text("Settings", l.heading, TextStyle::H1, TEXT, Align::Start, true);
    p.text(LEAD, l.lead, TextStyle::Small, MUTED, Align::Start, true);

    p.push_clip(l.view);
    // CSS letter-spacing: .04em of 12px.
    let spacing = TextStyle::Label.size() * 0.04;
    for (r, label) in l.labels.iter().zip(SECTIONS) {
        p.text_spaced(label, *r, TextStyle::Label, MUTED, spacing);
    }
    for r in &l.panels {
        p.fill(*r, 8.0, SURFACE);
        p.stroke(*r, 8.0, 1.0, BORDER);
    }

    for (i, (c, profile)) in l.choices.iter().zip(Profile::BOTH).enumerate() {
        let on = s.draft.profile == profile;
        p.fill(c.card, 6.0, if on { SOFT } else { BG });
        p.stroke(c.card, 6.0, 1.0, if on || hover(SHit::Profile(i)) { ACCENT } else { BORDER });
        widgets::radio(p, c.radio, on, hover(SHit::Profile(i)), on && ring(SFocus::Profile));
        p.text(profile.title(), c.title, TextStyle::Body, TEXT, Align::Start, true);
        p.text_wrapped_lh(profile.description(), c.desc.x, c.desc.y, c.desc.w, TextStyle::Small, MUTED, SMALL_LH);
    }

    title_and_desc(p, &l.video, VIDEO_TITLE, Some(VIDEO_DESC));
    let video_hot = hover(SHit::Video) || s.video_open.is_some();
    widgets::field_select(p, l.video.control, s.draft.video_preview.label(), video_hot, ring(SFocus::Video));
    p.text_wrapped_lh(GIF_NOTE, l.gif_note.x, l.gif_note.y, l.gif_note.w, TextStyle::Small, MUTED, SMALL_LH);

    title_and_desc(p, &l.shortcut, SHORTCUT_TITLE, Some(SHORTCUT));
    widgets::button(p, l.shortcut.control, &["Change"], hover(SHit::Change), ring(SFocus::Change));
    separator(p, &l.close_after);
    title_and_desc(p, &l.close_after, CLOSE_AFTER_TITLE, None);
    let hot = hover(SHit::CloseAfter);
    widgets::checkbox(p, l.close_after.control, s.draft.close_after_insert, hot, ring(SFocus::CloseAfter));

    title_and_desc(p, &l.folder, FOLDER_TITLE, Some(&paths::media_text()));
    widgets::quiet_button(p, l.folder.control, "Open folder", TextStyle::Body, hover(SHit::OpenFolder), true);
    widgets::ring(p, ring(SFocus::OpenFolder), l.folder.control, 6.0);
    separator(p, &l.import);
    title_and_desc(p, &l.import, IMPORT_TITLE, Some(IMPORT_DESC));
    widgets::button(p, l.import.control, &["Choose", "folder…"], hover(SHit::ChooseFolder), ring(SFocus::ChooseFolder));
    separator(p, &l.backup);
    title_and_desc(p, &l.backup, BACKUP_TITLE, Some(BACKUP_DESC));
    let backup_label = if s.backup_running { "Cancel backup" } else { "Back up library" };
    widgets::button(p, l.backup.control, &[backup_label], hover(SHit::Backup), ring(SFocus::Backup));

    title_and_desc(p, &l.autostart, AUTOSTART_TITLE, Some(AUTOSTART_DESC));
    let hot = hover(SHit::Autostart);
    widgets::checkbox(p, l.autostart.control, s.draft.autostart, hot, ring(SFocus::Autostart));
    p.pop_clip();

    // Pinned action bar.
    p.line(l.bar_x.0, l.bar_y + 0.5, l.bar_x.1, l.bar_y + 0.5, 1.0, BORDER);
    p.text(&s.status_line(), l.note, TextStyle::Small, MUTED, Align::Start, true);
    widgets::button(p, l.cancel, &["Cancel"], hover(SHit::Cancel), ring(SFocus::Cancel));
    widgets::primary_button(p, l.save, "Save changes", s.dirty(), hover(SHit::Save), ring(SFocus::Save));

    if let Some(row) = s.video_open {
        let labels = VideoPreview::BOTH.map(VideoPreview::label);
        widgets::menu(p, &l.video_menu, &labels, row, s.draft.video_preview.index());
    }
    paint_border(p, &l.frame);
}
