//! The Settings page: saved values, the staged draft the user edits, and
//! keyboard focus. Save commits the draft, Cancel discards it. No drawing, no Win32.
//!
//! Saved values live in the store (`store.rs`); Save writes them there.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    LessMemory,
    OpenFaster,
}

impl Profile {
    pub const BOTH: [Profile; 2] = [Profile::LessMemory, Profile::OpenFaster];

    pub fn title(self) -> &'static str {
        match self {
            Profile::LessMemory => "Use less memory",
            Profile::OpenFaster => "Open faster",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Profile::LessMemory => "Release unused windows and video players. Opening a window may take a moment.",
            Profile::OpenFaster => "Keep the quick popup ready. Uses more memory while idle.",
        }
    }

    pub fn index(self) -> usize {
        Self::BOTH.iter().position(|p| *p == self).unwrap_or(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoPreview {
    HoverOnly,
    Off,
}

impl VideoPreview {
    pub const BOTH: [VideoPreview; 2] = [VideoPreview::HoverOnly, VideoPreview::Off];

    pub fn label(self) -> &'static str {
        match self {
            VideoPreview::HoverOnly => "Hover only",
            VideoPreview::Off => "Off",
        }
    }

    pub fn index(self) -> usize {
        Self::BOTH.iter().position(|v| *v == self).unwrap_or(0)
    }
}

/// Saved in the store; a missing field takes its default.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub profile: Profile,
    pub video_preview: VideoPreview,
    pub close_after_insert: bool,
    pub autostart: bool,
}

impl Default for Settings {
    /// Use less memory if missing, Hover only, Close after inserting on, Autostart off.
    fn default() -> Self {
        Self { profile: Profile::LessMemory, video_preview: VideoPreview::HoverOnly, close_after_insert: true, autostart: false }
    }
}

/// Keyboard focus targets on the Settings page, in Tab order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SFocus {
    QuickPopup,
    Close,
    Nav(usize),
    /// The resource-usage radio group (one Tab stop; arrows choose).
    Profile,
    Video,
    Change,
    CloseAfter,
    OpenFolder,
    ChooseFolder,
    Backup,
    Autostart,
    Cancel,
    Save,
}

pub const SAVED: &str = "Settings saved.";
pub const DISCARDED: &str = "Changes discarded.";
pub const UNSAVED: &str = "Unsaved changes";

pub struct SettingsPage {
    pub saved: Settings,
    pub draft: Settings,
    /// Scroll offset of the settings area in DIPs.
    pub scroll: f32,
    pub focus: SFocus,
    pub focus_visible: bool,
    /// Highlighted row while the Video preview list is open.
    pub video_open: Option<usize>,
    /// Result of the last action ("Settings saved.", a not-built-yet note, backup progress).
    pub note: Option<String>,
    /// Back up library is running: its button reads "Cancel backup".
    pub backup_running: bool,
}

impl Default for SettingsPage {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsPage {
    pub fn new() -> Self {
        Self::with_saved(Settings::default())
    }

    /// The page for settings loaded from the store.
    pub fn with_saved(saved: Settings) -> Self {
        Self {
            saved,
            draft: saved,
            scroll: 0.0,
            focus: SFocus::Profile,
            focus_visible: false,
            video_open: None,
            note: None,
            backup_running: false,
        }
    }

    /// Opening the page always starts from the saved values. A running backup's
    /// progress stays.
    pub fn enter(&mut self) {
        self.draft = self.saved;
        self.scroll = 0.0;
        self.video_open = None;
        if !self.backup_running {
            self.note = None;
        }
    }

    pub fn dirty(&self) -> bool {
        self.draft != self.saved
    }

    fn edit(&mut self, change: impl FnOnce(&mut Settings)) {
        change(&mut self.draft);
        self.note = None;
    }

    pub fn set_profile(&mut self, p: Profile) {
        self.edit(|d| d.profile = p);
    }

    pub fn set_video(&mut self, v: VideoPreview) {
        self.edit(|d| d.video_preview = v);
        self.video_open = None;
    }

    pub fn toggle_close_after(&mut self) {
        self.edit(|d| d.close_after_insert = !d.close_after_insert);
    }

    pub fn toggle_autostart(&mut self) {
        self.edit(|d| d.autostart = !d.autostart);
    }

    /// Commits the draft. Does nothing when there is nothing to save (the button is disabled).
    pub fn save(&mut self) {
        if !self.dirty() {
            return;
        }
        self.saved = self.draft;
        self.note = Some(SAVED.into());
        if self.focus == SFocus::Save {
            self.focus = SFocus::Cancel; // a disabled button cannot keep focus
        }
    }

    /// Discards the draft.
    pub fn cancel(&mut self) {
        self.draft = self.saved;
        self.video_open = None;
        self.note = Some(DISCARDED.into());
        if self.focus == SFocus::Save {
            self.focus = SFocus::Cancel;
        }
    }

    /// A note for a control that is not built yet; keeps the draft.
    pub fn show_note(&mut self, note: &str) {
        self.note = Some(note.into());
    }

    /// The line left of Cancel / Save.
    pub fn status_line(&self) -> String {
        match (self.dirty(), &self.note) {
            (true, Some(n)) => format!("{UNSAVED} · {n}"),
            (true, None) => UNSAVED.into(),
            (false, Some(n)) => n.clone(),
            (false, None) => String::new(),
        }
    }

    pub fn focus_order(&self) -> Vec<SFocus> {
        let mut order = vec![SFocus::QuickPopup, SFocus::Close];
        order.extend((0..5).map(SFocus::Nav));
        order.extend([
            SFocus::Profile,
            SFocus::Video,
            SFocus::Change,
            SFocus::CloseAfter,
            SFocus::OpenFolder,
            SFocus::ChooseFolder,
            SFocus::Backup,
            SFocus::Autostart,
            SFocus::Cancel,
        ]);
        if self.dirty() {
            order.push(SFocus::Save);
        }
        order
    }

    pub fn tab(&mut self, back: bool) {
        let order = self.focus_order();
        let cur = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if back { (cur + order.len() - 1) % order.len() } else { (cur + 1) % order.len() };
        self.focus = order[next];
        self.focus_visible = true;
        self.video_open = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_goal() {
        let s = Settings::default();
        assert_eq!(s.profile, Profile::LessMemory);
        assert_eq!(s.video_preview, VideoPreview::HoverOnly);
        assert!(s.close_after_insert);
        assert!(!s.autostart);
    }

    #[test]
    fn edits_are_staged_until_save() {
        let mut p = SettingsPage::new();
        assert!(!p.dirty());
        assert_eq!(p.status_line(), "");
        p.toggle_close_after();
        p.toggle_autostart();
        assert!(p.dirty());
        assert_eq!(p.status_line(), "Unsaved changes");
        assert!(p.saved.close_after_insert, "saved values do not change while editing");
        p.save();
        assert!(!p.dirty());
        assert!(!p.saved.close_after_insert);
        assert!(p.saved.autostart);
        assert_eq!(p.status_line(), "Settings saved.");
    }

    #[test]
    fn cancel_discards_the_draft() {
        let mut p = SettingsPage::new();
        p.set_profile(Profile::OpenFaster);
        p.set_video(VideoPreview::Off);
        p.cancel();
        assert_eq!(p.draft, Settings::default());
        assert_eq!(p.saved, Settings::default());
        assert!(!p.dirty());
        assert_eq!(p.status_line(), "Changes discarded.");
    }

    #[test]
    fn undoing_an_edit_by_hand_is_not_dirty() {
        let mut p = SettingsPage::new();
        p.toggle_autostart();
        p.toggle_autostart();
        assert!(!p.dirty());
    }

    #[test]
    fn save_with_nothing_changed_does_nothing() {
        let mut p = SettingsPage::new();
        p.save();
        assert_eq!(p.note, None);
    }

    #[test]
    fn leaving_and_coming_back_drops_unsaved_edits() {
        let mut p = SettingsPage::new();
        p.set_profile(Profile::OpenFaster);
        p.save();
        p.set_video(VideoPreview::Off);
        p.enter();
        assert_eq!(p.draft.profile, Profile::OpenFaster, "saved edits stay");
        assert_eq!(p.draft.video_preview, VideoPreview::HoverOnly, "unsaved edits go");
        assert_eq!(p.note, None);
    }

    #[test]
    fn notes_join_the_unsaved_line_and_clear_on_the_next_edit() {
        let mut p = SettingsPage::new();
        p.toggle_autostart();
        p.show_note("Back up library is not built yet.");
        assert_eq!(p.status_line(), "Unsaved changes · Back up library is not built yet.");
        p.toggle_close_after();
        assert_eq!(p.status_line(), "Unsaved changes");
    }

    #[test]
    fn save_is_a_tab_stop_only_when_enabled() {
        let mut p = SettingsPage::new();
        assert!(!p.focus_order().contains(&SFocus::Save));
        p.focus = SFocus::Cancel;
        p.tab(false);
        assert_eq!(p.focus, SFocus::QuickPopup, "wraps past the disabled Save");
        p.toggle_autostart();
        p.focus = SFocus::Cancel;
        p.tab(false);
        assert_eq!(p.focus, SFocus::Save);
        p.save();
        assert_eq!(p.focus, SFocus::Cancel, "focus leaves the now-disabled Save");
    }
}
