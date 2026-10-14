//! The Manager's Settings page: geometry, hit-testing and drawing.
//! Cards of rows with switches and buttons, a pinned Save / Cancel bar, the
//! backup panel and the "Change shortcut" dialog over the page.
//!
//! Everything inside the scroll area is laid out with the scroll offset applied.

use super::d2d::{Align, Gfx, Painter};
use super::layout::{Rect, TextStyle};
use super::manager_view::{Frame, FrameHit, FrameLook, paint_border, paint_frame};
use super::widgets::{self, ACCENT, BG, DANGER, GOOD, LINE, MUTED, SELECTED, SUBTLE, SURFACE, TEXT};
use crate::hotkey::Shortcut;
use crate::manager_state::Nav;
use crate::paths;
use crate::settings_state::{BackupPanel, Profile, SFocus, SettingsPage, ShortcutDialog, Verdict};

/// The settings column is at most this wide.
const CONTENT_W: f32 = 700.0;
/// Card padding (sides) and row padding (top and bottom).
const CARD_PAD: f32 = 16.0;
const ROW_PAD: f32 = 14.0;
const BUTTON_H: f32 = 36.0;
const SWITCH: (f32, f32) = (40.0, 22.0);
const KEYCAP_H: f32 = 26.0;
const SMALL_LH: f32 = 18.0;
const TITLE_LH: f32 = 14.0 * 1.45;
const DESC_GAP: f32 = 3.0;

const VIDEO_TITLE: &str = "Video preview on hover";
const VIDEO_DESC: &str = "Plays a video while the pointer is over it. Uses extra CPU and memory. GIFs always loop while visible.";
const SHORTCUT_TITLE: &str = "Global shortcut";
const SHORTCUT_DESC: &str = "Opens the quick popup from any app.";
const SHORTCUT_TAKEN: &str = "Not working: Windows did not give PocketMedia this shortcut. Choose another.";
const CLOSE_AFTER_TITLE: &str = "Close after inserting";
const CLOSE_AFTER_DESC: &str = "Hide the quick popup once a file is pasted.";
const FOLDER_TITLE: &str = "Media folder";
const IMPORT_TITLE: &str = "Import your existing media";
const IMPORT_DESC: &str = "Select a folder. Supported files from it and all subfolders are sorted and copied into Pictures, GIFs and Videos. Original files stay where they are.";
const BACKUP_TITLE: &str = "Back up library";
const BACKUP_DESC: &str = "Includes everything in the Media folder, and settings. Choose a location outside PocketMedia.";
const AUTOSTART_TITLE: &str = "Autostart PocketMedia";
const AUTOSTART_DESC: &str = "Off unless you choose to enable it.";
const SECTIONS: [&str; 4] = ["Performance", "Quick popup", "Library", "Startup"];

pub const DIALOG_TITLE: &str = "Change shortcut";
const DIALOG_TEXT: &str = "Press the keys you want to use: Win, or two of Ctrl, Alt and Shift, together with another key.";
const DIALOG_USE: &str = "Use shortcut";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SHit {
    None,
    Caption,
    Close,
    QuickPopup,
    Nav(usize),
    Profile(usize),
    Video,
    Change,
    CloseAfter,
    OpenFolder,
    ChooseFolder,
    Backup,
    Autostart,
    Cancel,
    Save,
    /// The backup panel's buttons.
    PanelCancel,
    PanelOpen,
    PanelDone,
    /// The shortcut dialog's buttons.
    DialogReset,
    DialogCancel,
    DialogUse,
}

/// A card in the middle of `cover` with a title, a body and buttons at the
/// bottom right (the backup panel and the shortcut dialog).
pub struct Card {
    pub cover: Rect,
    pub card: Rect,
    pub title: Rect,
    /// Top of the body under the title.
    pub body_y: f32,
    pub buttons: Vec<(SFocus, Rect)>,
}

const CARD_PAD_IN: f32 = 24.0;

impl Card {
    fn compute(cover: Rect, w: f32, body_h: f32, buttons: &[(SFocus, f32)], left_button: Option<(SFocus, f32)>) -> Self {
        let w = w.min(cover.w - 40.0);
        let iw = w - 2.0 * CARD_PAD_IN;
        let title_h = TextStyle::Title.line();
        let h = CARD_PAD_IN + title_h + 12.0 + body_h + 20.0 + BUTTON_H + CARD_PAD_IN;
        let card = Rect::new(cover.x + (cover.w - w) / 2.0, cover.y + ((cover.h - h) / 2.0).max(20.0), w, h);
        let x = card.x + CARD_PAD_IN;
        let title = Rect::new(x, card.y + CARD_PAD_IN, iw, title_h);
        let body_y = title.bottom() + 12.0;
        let buttons_y = body_y + body_h + 20.0;
        let mut right = x + iw;
        let mut out = Vec::new();
        for (f, bw) in buttons.iter().rev() {
            out.push((*f, Rect::new(right - bw, buttons_y, *bw, BUTTON_H)));
            right -= bw + 8.0;
        }
        out.reverse();
        if let Some((f, bw)) = left_button {
            out.insert(0, (f, Rect::new(x - 10.0, buttons_y, bw, BUTTON_H)));
        }
        Card { cover, card, title, body_y, buttons: out }
    }

    fn button(&self, f: SFocus) -> Option<Rect> {
        self.buttons.iter().find(|(b, _)| *b == f).map(|(_, r)| *r)
    }
}

/// The backup panel's body.
pub struct PanelBody {
    pub bar: Rect,
    pub percent: Rect,
    pub line1: Rect,
    pub line2: Rect,
    pub text: Rect,
}

/// The shortcut dialog's body.
pub struct DialogBody {
    pub text: Rect,
    pub capture: Rect,
    pub verdict: Rect,
}

fn panel_label(f: SFocus) -> &'static str {
    match f {
        SFocus::PanelCancel => "Cancel backup",
        SFocus::PanelOpen => "Open folder",
        SFocus::DialogCancel => "Cancel",
        SFocus::DialogUse => DIALOG_USE,
        _ => "Done",
    }
}

pub fn panel_hit(f: SFocus) -> SHit {
    match f {
        SFocus::PanelCancel => SHit::PanelCancel,
        SFocus::PanelOpen => SHit::PanelOpen,
        SFocus::DialogReset => SHit::DialogReset,
        SFocus::DialogCancel => SHit::DialogCancel,
        SFocus::DialogUse => SHit::DialogUse,
        _ => SHit::PanelDone,
    }
}

fn reset_label() -> String {
    format!("Reset to {}", Shortcut::default().label())
}

/// A resource-usage choice card.
pub struct Choice {
    pub card: Rect,
    pub title: Rect,
    pub desc: Rect,
}

/// A settings row: title, small text under it, and a control on the right.
pub struct Row {
    /// Whole row (switch rows toggle from anywhere on it).
    pub area: Rect,
    /// y of the separator above the row, if any.
    pub border: Option<f32>,
    pub title: Rect,
    pub desc: Rect,
    pub control: Rect,
}

pub struct SettingsLayout {
    pub frame: Frame,
    pub heading: Rect,
    /// The scroll area (clip).
    pub view: Rect,
    pub content_h: f32,
    pub labels: [Rect; 4],
    /// Background cards: video, quick popup, library, startup.
    pub cards: [Rect; 4],
    pub choices: [Choice; 2],
    pub video: Row,
    pub shortcut: Row,
    /// Where the shortcut's keycaps start.
    pub keycaps_x: f32,
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
    pub panel: Option<(Card, PanelBody)>,
    pub dialog: Option<(Card, DialogBody)>,
}

/// Lays out one row starting at `y` (above its separator, if any).
#[allow(clippy::too_many_arguments)]
fn row(gfx: &Gfx, x: f32, w: f32, y: f32, border: bool, desc: &str, control: (f32, f32), extra_w: f32) -> Row {
    let top = if border { y + 1.0 } else { y };
    let content_y = top + ROW_PAD;
    let (cw, ch) = control;
    let control_x = x + w - cw;
    let text_w = control_x - extra_w - 16.0 - x;
    let desc_h = gfx.line_count(desc, TextStyle::Small, text_w) as f32 * SMALL_LH;
    let text_h = TITLE_LH + DESC_GAP + desc_h;
    let content_h = text_h.max(ch);
    let ty = content_y + (content_h - text_h) / 2.0;
    let title = Rect::new(x, ty, text_w, TITLE_LH);
    let desc = Rect::new(x, title.bottom() + DESC_GAP, text_w, desc_h);
    let control = Rect::new(control_x, content_y + (content_h - ch) / 2.0, cw, ch);
    let bottom = content_y + content_h + ROW_PAD;
    Row { area: Rect::new(x - CARD_PAD, y, w + 2.0 * CARD_PAD, bottom - y), border: border.then_some(y), title, desc, control }
}

impl SettingsLayout {
    pub fn compute(s: &SettingsPage, width: f32, height: f32, gfx: &Gfx) -> Self {
        let frame = Frame::compute(width, height, gfx);
        let x0 = frame.content_x();
        let col_w = CONTENT_W.min(frame.close.x - 16.0 - x0);
        let heading = Rect::new(x0, frame.close.y, col_w, frame.close.h);

        // Pinned bar: line, 14 padding, buttons, 16 window padding.
        let buttons_y = height - 1.0 - 16.0 - BUTTON_H;
        let bar_y = buttons_y - 14.0 - 1.0;
        let bar_x = (x0, x0 + col_w);
        let save_w = widgets::text_button_w(gfx, "Save changes", TextStyle::Medium);
        let save = Rect::new(bar_x.1 - save_w, buttons_y, save_w, BUTTON_H);
        let cancel_w = widgets::text_button_w(gfx, "Cancel", TextStyle::Body);
        let cancel = Rect::new(save.x - 8.0 - cancel_w, buttons_y, cancel_w, BUTTON_H);
        let note = Rect::new(x0, buttons_y, cancel.x - 12.0 - x0, BUTTON_H);

        let view_y = heading.bottom() + 12.0;
        let view = Rect::new(x0, view_y, col_w + 8.0, bar_y - 6.0 - view_y);
        let ix = x0 + CARD_PAD;
        let iw = col_w - 2.0 * CARD_PAD;

        let top = view_y - s.scroll;
        let mut y = top;
        let mut labels = [Rect::default(); 4];
        let mut cards = [Rect::default(); 4];
        let mut section = |i: usize, y: &mut f32| {
            if i > 0 {
                *y += 22.0;
            }
            labels[i] = Rect::new(x0 + 2.0, *y, col_w, TextStyle::Label.line());
            *y += TextStyle::Label.line() + 8.0;
            *y
        };

        // Performance: two choice cards, then the video switch in a card.
        let p0 = section(0, &mut y);
        let card_w = (col_w - 10.0) / 2.0;
        let text_w = card_w - 32.0;
        let lines = Profile::BOTH.iter().map(|p| gfx.line_count(p.description(), TextStyle::Small, text_w)).max().unwrap_or(1);
        let card_h = 16.0 + TITLE_LH + 4.0 + lines as f32 * SMALL_LH + 16.0;
        let choice = |i: usize| {
            let card = Rect::new(x0 + i as f32 * (card_w + 10.0), p0, card_w, card_h);
            let title = Rect::new(card.x + 16.0, card.y + 16.0, text_w, TITLE_LH);
            let desc = Rect::new(title.x, title.bottom() + 4.0, text_w, lines as f32 * SMALL_LH);
            Choice { card, title, desc }
        };
        let choices = [choice(0), choice(1)];
        let c0 = p0 + card_h + 10.0;
        let video = row(gfx, ix, iw, c0, false, VIDEO_DESC, SWITCH, 0.0);
        cards[0] = Rect::new(x0, c0, col_w, video.area.bottom() - c0);
        y = cards[0].bottom();

        // Quick popup: shortcut (keycaps and Change), Close after inserting.
        let p1 = section(1, &mut y);
        let change_w = widgets::text_button_w(gfx, "Change", TextStyle::Body);
        let keys = s.draft.shortcut.keys();
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        let caps_w = widgets::keycaps_w(gfx, &keys, TextStyle::Label, 4.0);
        let shortcut = row(gfx, ix, iw, p1, false, SHORTCUT_TAKEN, (change_w, BUTTON_H), caps_w + 12.0);
        let keycaps_x = shortcut.control.x - 12.0 - caps_w;
        let close_after = row(gfx, ix, iw, shortcut.area.bottom(), true, CLOSE_AFTER_DESC, SWITCH, 0.0);
        cards[1] = Rect::new(x0, p1, col_w, close_after.area.bottom() - p1);
        y = cards[1].bottom();

        // Library: media folder, import, backup.
        let p2 = section(2, &mut y);
        let button_w = ["Open folder", "Choose folder…", "Back up library", "Cancel backup"]
            .iter()
            .map(|l| widgets::text_button_w(gfx, l, TextStyle::Body))
            .fold(0.0, f32::max);
        let b = (button_w, BUTTON_H);
        let folder = row(gfx, ix, iw, p2, false, &paths::media_text(), b, 0.0);
        let import = row(gfx, ix, iw, folder.area.bottom(), true, IMPORT_DESC, b, 0.0);
        let backup = row(gfx, ix, iw, import.area.bottom(), true, BACKUP_DESC, b, 0.0);
        cards[2] = Rect::new(x0, p2, col_w, backup.area.bottom() - p2);
        y = cards[2].bottom();

        // Startup.
        let p3 = section(3, &mut y);
        let autostart = row(gfx, ix, iw, p3, false, AUTOSTART_DESC, SWITCH, 0.0);
        cards[3] = Rect::new(x0, p3, col_w, autostart.area.bottom() - p3);
        y = cards[3].bottom();

        let content_h = y + 16.0 - top;
        let cover = Rect::new(frame.sidebar.right(), 1.0, width - 1.0 - frame.sidebar.right(), height - 2.0);
        let panel = s.backup_panel.as_ref().map(|p| panel_layout(p, cover, gfx));
        let dialog = s.shortcut_dialog.as_ref().map(|d| dialog_layout(d, Rect::new(1.0, 1.0, width - 2.0, height - 2.0), gfx));
        SettingsLayout {
            frame,
            heading,
            view,
            content_h,
            labels,
            cards,
            choices,
            video,
            shortcut,
            keycaps_x,
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
            panel,
            dialog,
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
            SFocus::PanelCancel | SFocus::PanelOpen | SFocus::PanelDone => self.panel.as_ref()?.0.button(f)?,
            SFocus::DialogReset | SFocus::DialogCancel | SFocus::DialogUse => self.dialog.as_ref()?.0.button(f)?,
        })
    }

    /// Scroll offset that brings `f` fully into the scroll area.
    pub fn scroll_to_show(&self, s: &SettingsPage, f: SFocus) -> f32 {
        let Some(r) = self.focus_rect(s, f) else { return s.scroll };
        let pinned = matches!(
            f,
            SFocus::Close
                | SFocus::QuickPopup
                | SFocus::Nav(_)
                | SFocus::Cancel
                | SFocus::Save
                | SFocus::PanelCancel
                | SFocus::PanelOpen
                | SFocus::PanelDone
                | SFocus::DialogReset
                | SFocus::DialogCancel
                | SFocus::DialogUse
        );
        if pinned {
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

    pub fn hit(&self, _s: &SettingsPage, x: f32, y: f32) -> SHit {
        // The dialog is modal: only its buttons answer.
        if let Some((card, _)) = &self.dialog {
            return card.buttons.iter().find(|(_, r)| r.contains(x, y)).map_or(SHit::None, |(f, _)| panel_hit(*f));
        }
        match self.frame.hit(x, y) {
            Some(FrameHit::Close) => return SHit::Close,
            Some(FrameHit::QuickPopup) => return SHit::QuickPopup,
            Some(FrameHit::Caption) => return SHit::Caption,
            Some(FrameHit::Nav(i)) => return SHit::Nav(i),
            None => {}
        }
        // The backup panel covers the page: only its buttons answer.
        if let Some((card, _)) = &self.panel {
            return card.buttons.iter().find(|(_, r)| r.contains(x, y)).map_or(SHit::None, |(f, _)| panel_hit(*f));
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
            (self.shortcut.control, SHit::Change),
            (self.folder.control, SHit::OpenFolder),
            (self.import.control, SHit::ChooseFolder),
            (self.backup.control, SHit::Backup),
            // A switch row toggles from anywhere on the row.
            (self.video.area, SHit::Video),
            (self.close_after.area, SHit::CloseAfter),
            (self.autostart.area, SHit::Autostart),
        ];
        controls.iter().find(|(r, _)| r.contains(x, y)).map_or(SHit::None, |(_, h)| *h)
    }
}

fn panel_layout(panel: &BackupPanel, cover: Rect, gfx: &Gfx) -> (Card, PanelBody) {
    let w = 460.0f32.min(cover.w - 40.0);
    let iw = w - 2.0 * CARD_PAD_IN;
    let body_h = match panel {
        BackupPanel::Running(_) => 8.0 + 14.0 + TextStyle::Small.line() + 4.0 + TextStyle::Small.line(),
        BackupPanel::Done { text, .. } => gfx.wrapped_height(text, TextStyle::Small, iw),
    };
    let buttons: Vec<(SFocus, f32)> =
        panel.buttons().into_iter().map(|f| (f, widgets::text_button_w(gfx, panel_label(f), TextStyle::Medium))).collect();
    let card = Card::compute(cover, w, body_h, &buttons, None);
    let x = card.card.x + CARD_PAD_IN;
    let percent_w = gfx.measure("100 %", TextStyle::Small) + 4.0;
    let bar = Rect::new(x, card.body_y, iw - percent_w - 10.0, 8.0);
    let percent = Rect::new(bar.right() + 10.0, card.body_y - 5.0, percent_w, 18.0);
    let line1 = Rect::new(x, bar.bottom() + 14.0, iw, TextStyle::Small.line());
    let line2 = Rect::new(x, line1.bottom() + 4.0, iw, TextStyle::Small.line());
    let text = Rect::new(x, card.body_y, iw, body_h);
    (card, PanelBody { bar, percent, line1, line2, text })
}

fn dialog_layout(d: &ShortcutDialog, cover: Rect, gfx: &Gfx) -> (Card, DialogBody) {
    let w = 460.0;
    let iw = w - 2.0 * CARD_PAD_IN;
    let text_h = gfx.wrapped_height(DIALOG_TEXT, TextStyle::Small, iw);
    let verdict_h = match &d.verdict {
        Verdict::Refused(e) => gfx.wrapped_height(e, TextStyle::Small, iw),
        _ => TextStyle::Small.line(),
    };
    let capture_h = 76.0;
    let body_h = text_h + 14.0 + capture_h + 10.0 + verdict_h;
    let mut buttons = vec![(SFocus::DialogCancel, widgets::text_button_w(gfx, "Cancel", TextStyle::Body))];
    buttons.push((SFocus::DialogUse, widgets::text_button_w(gfx, DIALOG_USE, TextStyle::Medium)));
    let reset = (SFocus::DialogReset, widgets::text_button_w(gfx, &reset_label(), TextStyle::Small) - 12.0);
    let card = Card::compute(cover, w, body_h, &buttons, Some(reset));
    let x = card.card.x + CARD_PAD_IN;
    let text = Rect::new(x, card.body_y, iw, text_h);
    let capture = Rect::new(x, text.bottom() + 14.0, iw, capture_h);
    let verdict = Rect::new(x, capture.bottom() + 10.0, iw, verdict_h);
    (card, DialogBody { text, capture, verdict })
}

/// Pointer facts that are not part of `SettingsPage`.
pub struct SettingsUi {
    pub hover: SHit,
    /// The saved shortcut is registered (or paused by the chooser).
    pub shortcut_ok: bool,
}

fn title_and_desc(p: &Painter, r: &Row, title: &str, desc: &str, desc_color: super::d2d::Color) {
    p.text(title, r.title, TextStyle::Body, TEXT, Align::Start, true);
    p.text_wrapped_lh(desc, r.desc.x, r.desc.y, r.desc.w, TextStyle::Small, desc_color, SMALL_LH);
}

fn separator(p: &Painter, r: &Row) {
    if let Some(y) = r.border {
        p.line(r.area.x + CARD_PAD, y + 0.5, r.area.right() - CARD_PAD, y + 0.5, 1.0, LINE);
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
    paint_frame(p, &l.frame, &|n| n == Nav::Settings, None, look);
    p.text("Settings", l.heading, TextStyle::H1, TEXT, Align::Start, true);

    p.push_clip(l.view);
    for (r, label) in l.labels.iter().zip(SECTIONS) {
        p.text(label, *r, TextStyle::Label, MUTED, Align::Start, true);
    }
    for r in &l.cards {
        p.fill(*r, 12.0, SURFACE);
    }

    for (i, (c, profile)) in l.choices.iter().zip(Profile::BOTH).enumerate() {
        let on = s.draft.profile == profile;
        p.fill(c.card, 12.0, if on { SELECTED } else { SURFACE });
        if on {
            p.stroke(c.card, 12.0, 2.0, ACCENT);
        } else if hover(SHit::Profile(i)) {
            p.stroke(c.card, 12.0, 1.0, widgets::BORDER);
        }
        widgets::ring(p, on && ring(SFocus::Profile), c.card, 12.0);
        p.text(profile.title(), c.title, TextStyle::Medium, TEXT, Align::Start, true);
        let dc = if on { widgets::SOFT_TEXT } else { MUTED };
        p.text_wrapped_lh(profile.description(), c.desc.x, c.desc.y, c.desc.w, TextStyle::Small, dc, SMALL_LH);
    }

    title_and_desc(p, &l.video, VIDEO_TITLE, VIDEO_DESC, MUTED);
    let video_on = s.draft.video_preview == crate::settings_state::VideoPreview::HoverOnly;
    widgets::switch(p, l.video.control, video_on, hover(SHit::Video), ring(SFocus::Video));

    // The shortcut, as keycaps; red text when Windows refused the saved one.
    let refused = !ui.shortcut_ok && s.draft.shortcut == s.saved.shortcut;
    let (desc, dc) = if refused { (SHORTCUT_TAKEN, DANGER) } else { (SHORTCUT_DESC, MUTED) };
    title_and_desc(p, &l.shortcut, SHORTCUT_TITLE, desc, dc);
    let keys = s.draft.shortcut.keys();
    let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
    let cy = l.shortcut.control.y + l.shortcut.control.h / 2.0;
    widgets::keycaps(p, l.keycaps_x, cy, KEYCAP_H, &keys, TextStyle::Label, 4.0, false);
    widgets::button(p, l.shortcut.control, &["Change"], hover(SHit::Change), ring(SFocus::Change));
    separator(p, &l.close_after);
    title_and_desc(p, &l.close_after, CLOSE_AFTER_TITLE, CLOSE_AFTER_DESC, MUTED);
    widgets::switch(p, l.close_after.control, s.draft.close_after_insert, hover(SHit::CloseAfter), ring(SFocus::CloseAfter));

    title_and_desc(p, &l.folder, FOLDER_TITLE, &paths::media_text(), MUTED);
    widgets::button(p, l.folder.control, &["Open folder"], hover(SHit::OpenFolder), ring(SFocus::OpenFolder));
    separator(p, &l.import);
    title_and_desc(p, &l.import, IMPORT_TITLE, IMPORT_DESC, MUTED);
    widgets::button(p, l.import.control, &["Choose folder…"], hover(SHit::ChooseFolder), ring(SFocus::ChooseFolder));
    separator(p, &l.backup);
    title_and_desc(p, &l.backup, BACKUP_TITLE, BACKUP_DESC, MUTED);
    let backup_label = if s.backup_running { "Cancel backup" } else { "Back up library" };
    widgets::button(p, l.backup.control, &[backup_label], hover(SHit::Backup), ring(SFocus::Backup));

    title_and_desc(p, &l.autostart, AUTOSTART_TITLE, AUTOSTART_DESC, MUTED);
    widgets::switch(p, l.autostart.control, s.draft.autostart, hover(SHit::Autostart), ring(SFocus::Autostart));
    p.pop_clip();

    // Pinned action bar.
    p.line(l.bar_x.0, l.bar_y + 0.5, l.bar_x.1, l.bar_y + 0.5, 1.0, LINE);
    let line = s.status_line();
    let note_c = if s.dirty() { widgets::SOFT_TEXT } else { MUTED };
    // One line when it fits; a longer message (an Autostart or save problem)
    // wraps, centred on the buttons, as long as it stays inside the bar (below
    // its divider), so its advice is not cut off.
    let wrapped = p.gfx().wrapped_height(&line, TextStyle::Small, l.note.w);
    let room = 2.0 * (l.note.y + l.note.h / 2.0 - l.bar_y) - 8.0;
    if p.gfx().measure(&line, TextStyle::Small) <= l.note.w || wrapped > room {
        p.text(&line, l.note, TextStyle::Small, note_c, Align::Start, true);
    } else {
        p.text_wrapped(&line, l.note.x, l.note.y + (l.note.h - wrapped) / 2.0, l.note.w, TextStyle::Small, note_c);
    }
    widgets::button(p, l.cancel, &["Cancel"], hover(SHit::Cancel), ring(SFocus::Cancel));
    widgets::primary_button(p, l.save, "Save changes", s.dirty(), hover(SHit::Save), ring(SFocus::Save));

    if let (Some(panel), Some(pl)) = (&s.backup_panel, &l.panel) {
        paint_panel(p, s, panel, pl, ui);
    }
    if let (Some(d), Some(dl)) = (&s.shortcut_dialog, &l.dialog) {
        paint_dialog(p, s, d, dl, ui);
    }
    paint_border(p, &l.frame);
}

fn card_buttons(p: &Painter, s: &SettingsPage, card: &Card, ui: &SettingsUi, primary: SFocus, enabled: &dyn Fn(SFocus) -> bool) {
    for (f, r) in &card.buttons {
        let ring = s.focus_visible && s.focus == *f;
        let hot = ui.hover == panel_hit(*f);
        if *f == primary {
            widgets::primary_button(p, *r, panel_label(*f), enabled(*f), hot, ring);
        } else if *f == SFocus::DialogReset {
            widgets::quiet_button(p, *r, &reset_label(), TextStyle::Small, hot, true);
            widgets::ring(p, ring, *r, widgets::RADIUS);
        } else {
            widgets::button(p, *r, &[panel_label(*f)], hot, ring);
        }
    }
}

fn paint_card(p: &Painter, c: &Card, title: &str) {
    p.fill(c.cover, 0.0, BG.alpha(0.78));
    p.fill(c.card.inset(-1.0), 17.0, widgets::BORDER);
    p.fill(c.card, 16.0, SURFACE);
    p.text(title, c.title, TextStyle::Title, TEXT, Align::Start, true);
}

fn paint_panel(p: &Painter, s: &SettingsPage, panel: &BackupPanel, l: &(Card, PanelBody), ui: &SettingsUi) {
    let (card, b) = l;
    match panel {
        BackupPanel::Running(step) => {
            paint_card(p, card, "Backing up your library");
            p.fill(b.bar, 4.0, widgets::FIELD);
            let f = step.fraction();
            if f > 0.0 {
                p.fill(Rect::new(b.bar.x, b.bar.y, (b.bar.w * f).max(8.0), b.bar.h), 4.0, ACCENT);
            }
            p.text(&format!("{} %", (f * 100.0).floor() as u32), b.percent, TextStyle::Small, TEXT, Align::End, true);
            p.text(&step.counts(), b.line1, TextStyle::Small, TEXT, Align::Start, true);
            p.text(&step.doing(), b.line2, TextStyle::Small, MUTED, Align::Start, true);
        }
        BackupPanel::Done { title, text, .. } => {
            paint_card(p, card, title);
            p.text_wrapped(text, b.text.x, b.text.y, b.text.w, TextStyle::Small, MUTED);
        }
    }
    card_buttons(p, s, card, ui, SFocus::PanelDone, &|_| true);
}

fn paint_dialog(p: &Painter, s: &SettingsPage, d: &ShortcutDialog, l: &(Card, DialogBody), ui: &SettingsUi) {
    let (card, b) = l;
    paint_card(p, card, DIALOG_TITLE);
    p.text_wrapped(DIALOG_TEXT, b.text.x, b.text.y, b.text.w, TextStyle::Small, MUTED);

    // The keys: held modifiers while waiting, then the whole shortcut.
    p.fill(b.capture, 12.0, BG);
    let edge = match d.verdict {
        Verdict::Waiting => ACCENT.alpha(0.55),
        Verdict::Free => GOOD,
        Verdict::Refused(_) => DANGER,
    };
    p.stroke(b.capture, 12.0, 2.0, edge);
    let owned: Vec<String> = match d.candidate {
        Some(c) if !d.held.any() => c.keys(),
        _ => d.held.keys().iter().map(|k| k.to_string()).collect(),
    };
    let keys: Vec<&str> = owned.iter().map(String::as_str).collect();
    let (_, cy) = b.capture.center();
    if keys.is_empty() {
        p.text("Press the keys now…", b.capture, TextStyle::Body, SUBTLE, Align::Center, true);
    } else {
        let w = widgets::keycaps_w(p.gfx(), &keys, TextStyle::Heading, 8.0);
        let x = b.capture.x + (b.capture.w - w) / 2.0;
        widgets::keycaps(p, x, cy, 36.0, &keys, TextStyle::Heading, 8.0, true);
    }

    match &d.verdict {
        Verdict::Waiting => {
            let now = format!("Now: {}", crate::hotkey::current().label());
            p.text(&now, b.verdict, TextStyle::Small, SUBTLE, Align::Start, true);
        }
        Verdict::Free => {
            let (x, y) = (b.verdict.x, b.verdict.y + b.verdict.h / 2.0);
            p.line(x + 1.0, y, x + 4.5, y + 3.5, 2.0, GOOD);
            p.line(x + 4.5, y + 3.5, x + 11.0, y - 4.0, 2.0, GOOD);
            let r = Rect::new(x + 18.0, b.verdict.y, b.verdict.w - 18.0, b.verdict.h);
            p.text("Available on this computer.", r, TextStyle::Small, GOOD, Align::Start, true);
        }
        Verdict::Refused(e) => p.text_wrapped(e, b.verdict.x, b.verdict.y, b.verdict.w, TextStyle::Small, DANGER),
    }
    card_buttons(p, s, card, ui, SFocus::DialogUse, &|f| f != SFocus::DialogUse || d.can_use());
}
