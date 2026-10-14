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

/// Saved in the store; a missing field takes its default.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub profile: Profile,
    pub video_preview: VideoPreview,
    pub close_after_insert: bool,
    pub autostart: bool,
    /// Opens the quick popup from any app.
    pub shortcut: crate::hotkey::Shortcut,
}

impl Default for Settings {
    /// Use less memory if missing, Hover only, Close after inserting on, Autostart off.
    fn default() -> Self {
        Self {
            profile: Profile::LessMemory,
            video_preview: VideoPreview::HoverOnly,
            close_after_insert: true,
            autostart: false,
            shortcut: crate::hotkey::Shortcut::default(),
        }
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
    /// The Video preview switch (Hover only when on).
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
    /// The shortcut chooser's buttons.
    DialogReset,
    DialogCancel,
    DialogUse,
}

/// What the shortcut chooser thinks of the keys just pressed.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// Nothing pressed yet.
    Waiting,
    /// Windows would give PocketMedia this shortcut.
    Free,
    /// It cannot be used, and why.
    Refused(String),
}

/// The "Change shortcut" dialog over Settings. Keys pressed while it is open
/// are recorded instead of acting; the draft changes only with Use shortcut.
#[derive(Clone, Debug, PartialEq)]
pub struct ShortcutDialog {
    /// Modifiers held right now, shown until a key completes the shortcut.
    pub held: crate::hotkey::Held,
    pub candidate: Option<crate::hotkey::Shortcut>,
    pub verdict: Verdict,
}

impl ShortcutDialog {
    pub fn can_use(&self) -> bool {
        self.candidate.is_some() && self.verdict == Verdict::Free
    }

    /// The buttons in Tab order; Use shortcut is a stop only when it is enabled.
    pub fn buttons(&self) -> Vec<SFocus> {
        let mut b = vec![SFocus::DialogReset, SFocus::DialogCancel];
        if self.can_use() {
            b.push(SFocus::DialogUse);
        }
        b
    }
}

/// The panel over Settings while a backup runs, and its result afterwards
/// (shows how the backup is going).
#[derive(Clone, Debug, PartialEq)]
pub enum BackupPanel {
    Running(crate::backup::Step),
    /// Finished, failed or cancelled. `folder` is the finished backup, to open.
    Done { title: String, text: String, folder: Option<std::path::PathBuf> },
}

impl BackupPanel {
    /// The panel's buttons, in Tab order (the last is the default one).
    pub fn buttons(&self) -> Vec<SFocus> {
        match self {
            BackupPanel::Running(_) => vec![SFocus::PanelCancel],
            BackupPanel::Done { folder: Some(_), .. } => vec![SFocus::PanelOpen, SFocus::PanelDone],
            BackupPanel::Done { .. } => vec![SFocus::PanelDone],
        }
    }
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
    /// Result of the last action ("Settings saved.", a not-built-yet note, backup progress).
    pub note: Option<String>,
    /// Back up library is running: its button reads "Cancel backup".
    pub backup_running: bool,
    /// Shown over the page while it is Some.
    pub backup_panel: Option<BackupPanel>,
    /// The shortcut chooser, over everything while it is Some.
    pub shortcut_dialog: Option<ShortcutDialog>,
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
            note: None,
            backup_running: false,
            backup_panel: None,
            shortcut_dialog: None,
        }
    }

    /// Opening the page always starts from the saved values. A running backup's
    /// progress stays.
    pub fn enter(&mut self) {
        self.draft = self.saved;
        self.scroll = 0.0;
        self.shortcut_dialog = None;
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

    pub fn toggle_video(&mut self) {
        self.edit(|d| {
            d.video_preview = match d.video_preview {
                VideoPreview::HoverOnly => VideoPreview::Off,
                VideoPreview::Off => VideoPreview::HoverOnly,
            }
        });
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

    /// Shows the backup panel (or updates it). Focus moves to its default button.
    pub fn set_backup_panel(&mut self, panel: BackupPanel) {
        let first = self.backup_panel.as_ref().map(std::mem::discriminant) != Some(std::mem::discriminant(&panel));
        let buttons = panel.buttons();
        // Focus moves to the panel when it appears, or when it was on something the
        // panel covers; the sidebar keeps it otherwise.
        let sidebar = matches!(self.focus, SFocus::QuickPopup | SFocus::Close | SFocus::Nav(_));
        if first || !(sidebar || buttons.contains(&self.focus)) {
            self.focus = *buttons.last().unwrap_or(&SFocus::Backup);
        }
        self.backup_panel = Some(panel);
    }

    /// Done on the panel: back to the page, at Back up library.
    pub fn close_backup_panel(&mut self) {
        self.backup_panel = None;
        self.focus = SFocus::Backup;
    }

    // ------------------------------------------------------------ shortcut chooser

    pub fn open_shortcut_dialog(&mut self) {
        self.shortcut_dialog =
            Some(ShortcutDialog { held: Default::default(), candidate: None, verdict: Verdict::Waiting });
        self.focus = SFocus::DialogCancel;
    }

    /// Escape, Cancel: the draft is unchanged.
    pub fn close_shortcut_dialog(&mut self) {
        self.shortcut_dialog = None;
        self.focus = SFocus::Change;
    }

    /// Modifiers pressed or released while nothing is chosen yet.
    pub fn dialog_held(&mut self, held: crate::hotkey::Held) {
        if let Some(d) = self.shortcut_dialog.as_mut() {
            d.held = held;
        }
    }

    /// A key that completes a shortcut, with Windows' answer for it (or the rules').
    pub fn dialog_pressed(&mut self, shortcut: crate::hotkey::Shortcut, free: Result<(), String>) {
        if let Some(d) = self.shortcut_dialog.as_mut() {
            d.held = Default::default();
            d.candidate = Some(shortcut);
            d.verdict = match free {
                Ok(()) => Verdict::Free,
                Err(e) => Verdict::Refused(e),
            };
            self.focus = if d.can_use() { SFocus::DialogUse } else { SFocus::DialogCancel };
        }
    }

    /// Use shortcut: the draft takes it (saved with Save changes).
    pub fn use_shortcut(&mut self) {
        let Some(s) = self.shortcut_dialog.as_ref().filter(|d| d.can_use()).and_then(|d| d.candidate) else { return };
        self.edit(|d| d.shortcut = s);
        self.close_shortcut_dialog();
    }

    pub fn focus_order(&self) -> Vec<SFocus> {
        if let Some(d) = &self.shortcut_dialog {
            return d.buttons();
        }
        // Visual order: sidebar (with Quick popup at its foot), ×, then the page.
        let mut order: Vec<SFocus> = (0..5).map(SFocus::Nav).collect();
        order.extend([SFocus::QuickPopup, SFocus::Close]);
        // While the backup panel shows, only the sidebar and its buttons can be reached.
        if let Some(panel) = &self.backup_panel {
            order.extend(panel.buttons());
            return order;
        }
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backup_panel_takes_focus_and_tab_order() {
        let mut p = SettingsPage::new();
        p.set_backup_panel(BackupPanel::Running(crate::backup::Step::default()));
        assert_eq!(p.focus, SFocus::PanelCancel);
        assert!(!p.focus_order().contains(&SFocus::Backup), "the page under the panel cannot be reached");
        assert!(p.focus_order().contains(&SFocus::Nav(0)), "the sidebar still can");
        p.focus = SFocus::Nav(1);
        p.set_backup_panel(BackupPanel::Running(crate::backup::Step { done: 1, total: 2, ..Default::default() }));
        assert_eq!(p.focus, SFocus::Nav(1), "progress updates leave focus alone");
        p.set_backup_panel(BackupPanel::Done { title: "Backup finished".into(), text: "ok".into(), folder: Some("D:/b".into()) });
        assert_eq!(p.focus, SFocus::PanelDone);
        assert_eq!(p.focus_order()[7..], [SFocus::PanelOpen, SFocus::PanelDone]);
        p.close_backup_panel();
        assert_eq!((p.backup_panel.is_none(), p.focus), (true, SFocus::Backup));
    }

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
        p.toggle_video();
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
        p.toggle_video();
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
    fn the_video_switch_flips_between_hover_only_and_off() {
        let mut p = SettingsPage::new();
        p.toggle_video();
        assert_eq!(p.draft.video_preview, VideoPreview::Off);
        p.toggle_video();
        assert_eq!(p.draft.video_preview, VideoPreview::HoverOnly);
        assert!(!p.dirty());
    }

    #[test]
    fn a_new_shortcut_is_staged_until_save() {
        use crate::hotkey::{Held, Shortcut};
        let mut p = SettingsPage::new();
        p.open_shortcut_dialog();
        assert_eq!(p.focus_order(), [SFocus::DialogReset, SFocus::DialogCancel], "Use shortcut waits for keys");
        p.dialog_held(Held { ctrl: true, ..Default::default() });
        let wanted = Shortcut { ctrl: true, alt: true, shift: false, win: false, key: 0x50 };
        p.dialog_pressed(wanted, Ok(()));
        assert_eq!(p.focus, SFocus::DialogUse);
        assert!(!p.shortcut_dialog.as_ref().unwrap().held.any(), "the held keys became the shortcut");
        p.use_shortcut();
        assert!(p.shortcut_dialog.is_none());
        assert_eq!(p.focus, SFocus::Change);
        assert_eq!(p.draft.shortcut, wanted);
        assert_eq!(p.saved.shortcut, Shortcut::default(), "nothing is saved yet");
        assert_eq!(p.status_line(), UNSAVED);
        p.save();
        assert_eq!(p.saved.shortcut, wanted);
    }

    #[test]
    fn a_refused_shortcut_cannot_be_used() {
        use crate::hotkey::Shortcut;
        let mut p = SettingsPage::new();
        p.open_shortcut_dialog();
        let taken = Shortcut { ctrl: true, alt: true, shift: false, win: false, key: 0x54 };
        p.dialog_pressed(taken, Err("Another app already uses it.".into()));
        assert_eq!(p.focus, SFocus::DialogCancel);
        assert!(!p.focus_order().contains(&SFocus::DialogUse));
        p.use_shortcut();
        assert!(p.shortcut_dialog.is_some(), "Use shortcut does nothing");
        assert!(!p.dirty());
        p.close_shortcut_dialog();
        assert!(p.shortcut_dialog.is_none() && !p.dirty());
    }

    #[test]
    fn old_library_files_without_a_shortcut_get_the_default() {
        let s: Settings = serde_json::from_str(r#"{"profile":"open_faster"}"#).unwrap();
        assert_eq!(s.shortcut, crate::hotkey::Shortcut::default());
        assert_eq!(s.profile, Profile::OpenFaster);
    }

    #[test]
    fn save_is_a_tab_stop_only_when_enabled() {
        let mut p = SettingsPage::new();
        assert!(!p.focus_order().contains(&SFocus::Save));
        p.focus = SFocus::Cancel;
        p.tab(false);
        assert_eq!(p.focus, SFocus::Nav(0), "wraps past the disabled Save");
        p.toggle_autostart();
        p.focus = SFocus::Cancel;
        p.tab(false);
        assert_eq!(p.focus, SFocus::Save);
        p.save();
        assert_eq!(p.focus, SFocus::Cancel, "focus leaves the now-disabled Save");
    }
}
