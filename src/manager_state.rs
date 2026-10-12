//! What the Manager window shows: type filter, name search, sort, the selected
//! item with its rename field, and keyboard focus. No drawing, no Win32.

use crate::library::{Library, extension};
use crate::names::NameError;
use crate::popup_state::TypeFilter;
use crate::sample::Item;
use crate::search::{self, name_without_extension};
use crate::text_field::TextField;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort {
    RecentlyAdded,
    Name,
}

impl Sort {
    pub const BOTH: [Sort; 2] = [Sort::RecentlyAdded, Sort::Name];

    pub fn label(self) -> &'static str {
        match self {
            Sort::RecentlyAdded => "Recently added",
            Sort::Name => "Name",
        }
    }

    pub fn index(self) -> usize {
        Self::BOTH.iter().position(|s| *s == self).unwrap_or(0)
    }
}

/// Sidebar entries, top to bottom.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Nav {
    All,
    Gif,
    Picture,
    Video,
    Settings,
}

impl Nav {
    pub const ORDER: [Nav; 5] = [Nav::All, Nav::Gif, Nav::Picture, Nav::Video, Nav::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Nav::All => "All media",
            Nav::Gif => "GIF",
            Nav::Picture => "Picture",
            Nav::Video => "Video",
            Nav::Settings => "Settings",
        }
    }

    /// The type filter this entry shows; None for Settings.
    pub fn filter(self) -> Option<TypeFilter> {
        match self {
            Nav::All => Some(TypeFilter::All),
            Nav::Gif => Some(TypeFilter::Gif),
            Nav::Picture => Some(TypeFilter::Picture),
            Nav::Video => Some(TypeFilter::Video),
            Nav::Settings => None,
        }
    }
}

/// Keyboard focus targets, in Tab order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MFocus {
    Close,
    Nav(usize),
    AddMedia,
    Search,
    ClearFilters,
    Sort,
    Gallery,
    EmptyAction,
    RenameInput,
    Rename,
    OpenFolder,
    Recycle,
    ConfirmMove,
    ConfirmCancel,
}

pub struct ManagerState {
    pub filter: TypeFilter,
    pub search: TextField,
    pub sort: Sort,
    /// Selected item id; shown in the inspector.
    pub selected: Option<u32>,
    /// Gallery scroll offset in DIPs.
    pub scroll: f32,
    pub focus: MFocus,
    pub focus_visible: bool,
    /// Highlighted row while the sort list is open.
    pub sort_open: Option<usize>,
    /// Editable name of the selected item, without its extension.
    pub rename: TextField,
    rename_for: Option<u32>,
    pub rename_error: Option<NameError>,
    /// The "Move to Recycle Bin?" confirmation is showing.
    pub confirming: bool,
    /// Result of the last action, shown in the inspector.
    pub status: Option<String>,
}

impl Default for ManagerState {
    fn default() -> Self {
        Self::new()
    }
}

impl ManagerState {
    pub fn new() -> Self {
        Self {
            filter: TypeFilter::All,
            search: TextField::default(),
            sort: Sort::RecentlyAdded,
            selected: None,
            scroll: 0.0,
            focus: MFocus::Gallery,
            focus_visible: false,
            sort_open: None,
            rename: TextField::default(),
            rename_for: None,
            rename_error: None,
            confirming: false,
            status: None,
        }
    }

    /// Items after the type filter and name search, in the chosen order.
    pub fn visible<'a>(&self, lib: &'a Library) -> Vec<&'a Item> {
        let mut items: Vec<&Item> = lib
            .items
            .iter()
            .filter(|i| self.filter.accepts(i.kind) && search::matches(&i.name, self.search.text()))
            .collect();
        if self.sort == Sort::Name {
            // Stable, case-insensitive; equal names keep their recently-added order.
            items.sort_by_cached_key(|i| i.name.to_lowercase());
        }
        items
    }

    pub fn count_label(&self, lib: &Library) -> String {
        let n = self.visible(lib).len();
        format!("{n} item{}", if n == 1 { "" } else { "s" })
    }

    /// "Clear filters" shows whenever a search or type filter is active.
    pub fn has_filters(&self) -> bool {
        !self.search.text().trim().is_empty() || self.filter != TypeFilter::All
    }

    pub fn nav_selected(&self, nav: Nav) -> bool {
        nav.filter() == Some(self.filter)
    }

    pub fn selected_index(&self, lib: &Library) -> Option<usize> {
        let id = self.selected?;
        self.visible(lib).iter().position(|i| i.id == id)
    }

    pub fn selected_item<'a>(&self, lib: &'a Library) -> Option<&'a Item> {
        self.selected.and_then(|id| lib.get(id))
    }

    /// Keeps the selection on a visible item (the first one if it was filtered
    /// out) and loads the rename field when the selection changes. Call after
    /// anything that changes the list.
    pub fn sync(&mut self, lib: &Library) {
        let visible = self.visible(lib);
        if !self.selected.is_some_and(|id| visible.iter().any(|i| i.id == id)) {
            self.selected = visible.first().map(|i| i.id);
        }
        if self.rename_for != self.selected {
            self.rename_for = self.selected;
            self.rename.clear();
            if let Some(item) = self.selected_item(lib) {
                self.rename.insert(name_without_extension(&item.name));
            }
            self.rename_error = None;
            self.confirming = false;
        }
        // Never leave focus on a control that is no longer shown.
        let inspector = matches!(
            self.focus,
            MFocus::RenameInput | MFocus::Rename | MFocus::OpenFolder | MFocus::Recycle | MFocus::ConfirmMove | MFocus::ConfirmCancel
        );
        if (self.focus == MFocus::Gallery && visible.is_empty()) || (inspector && self.selected.is_none()) {
            self.focus = MFocus::Search;
        } else if !self.confirming && matches!(self.focus, MFocus::ConfirmMove | MFocus::ConfirmCancel) {
            self.focus = MFocus::Recycle;
        } else if self.focus == MFocus::ClearFilters && !self.has_filters() {
            self.focus = MFocus::Search;
        }
    }

    pub fn select(&mut self, id: u32, lib: &Library) {
        if self.selected != Some(id) {
            self.selected = Some(id);
            self.status = None;
        }
        self.sync(lib);
    }

    /// Moves the selection by `delta` positions in the visible list, clamped.
    pub fn move_selection(&mut self, lib: &Library, delta: isize) {
        let visible = self.visible(lib);
        if visible.is_empty() {
            return;
        }
        let cur = self.selected_index(lib).unwrap_or(0) as isize;
        let next = (cur + delta).clamp(0, visible.len() as isize - 1) as usize;
        let id = visible[next].id;
        self.select(id, lib);
    }

    pub fn set_filter(&mut self, filter: TypeFilter, lib: &Library) {
        self.filter = filter;
        self.scroll = 0.0;
        self.sync(lib);
    }

    pub fn set_sort(&mut self, sort: Sort, lib: &Library) {
        self.sort = sort;
        self.sort_open = None;
        self.scroll = 0.0;
        self.sync(lib);
    }

    pub fn search_changed(&mut self, lib: &Library) {
        self.scroll = 0.0;
        self.sync(lib);
    }

    pub fn clear_filters(&mut self, lib: &Library) {
        self.search.clear();
        self.filter = TypeFilter::All;
        self.scroll = 0.0;
        self.sync(lib);
    }

    /// Validates the rename field as the user types (same rules as import).
    pub fn rename_edited(&mut self, lib: &Library) {
        self.rename_error = self.selected.and_then(|id| lib.check_rename(id, self.rename.text()).err());
    }

    /// Escape in the name field: back to the item's current name.
    pub fn revert_rename(&mut self, lib: &Library) {
        self.rename_for = None;
        self.sync(lib);
    }

    pub fn can_rename(&self) -> bool {
        self.selected.is_some() && self.rename_error.is_none()
    }

    /// Applies the rename. The extension never changes.
    pub fn apply_rename(&mut self, lib: &mut Library) {
        let Some(id) = self.selected else { return };
        let current = lib.get(id).map(|i| i.name.clone()).unwrap_or_default();
        let wanted = format!("{}{}", self.rename.text().trim_start(), extension(&current));
        if wanted == current {
            return;
        }
        match lib.rename(id, self.rename.text()) {
            Ok(new_name) => {
                self.rename.clear();
                self.rename.insert(name_without_extension(&new_name));
                self.rename_error = None;
                self.status = Some(format!("Renamed to {new_name}."));
                self.sync(lib);
            }
            Err(e) => self.rename_error = Some(e),
        }
    }

    pub fn ask_recycle(&mut self) {
        if self.selected.is_some() {
            self.confirming = true;
            self.focus = MFocus::ConfirmMove;
        }
    }

    pub fn cancel_recycle(&mut self) {
        self.confirming = false;
        self.focus = MFocus::Recycle;
    }

    /// Removes the selected item after confirmation and selects the one that
    /// took its place. Phase 2: fake data, so no file is moved.
    pub fn confirm_recycle(&mut self, lib: &mut Library) {
        let Some(id) = self.selected.filter(|_| self.confirming) else { return };
        let index = self.selected_index(lib).unwrap_or(0);
        let Some(name) = lib.remove(id) else { return };
        self.confirming = false;
        let visible = self.visible(lib);
        self.selected = visible.get(index.min(visible.len().saturating_sub(1))).map(|i| i.id);
        self.status = Some(format!("{name} removed from the library (fake data: no file was moved)."));
        self.focus = if self.selected.is_some() { MFocus::Gallery } else { MFocus::Search };
        self.sync(lib);
    }

    /// After an import: unfiltered, newest first, with the newest item selected.
    pub fn show_imported(&mut self, status: String, lib: &Library) {
        self.filter = TypeFilter::All;
        self.search.clear();
        self.sort = Sort::RecentlyAdded;
        self.sort_open = None;
        self.scroll = 0.0;
        self.selected = lib.items.first().map(|i| i.id);
        self.focus = MFocus::Gallery;
        self.sync(lib);
        self.status = Some(status);
    }

    pub fn focus_order(&self, lib: &Library) -> Vec<MFocus> {
        let mut order = vec![MFocus::Close];
        order.extend((0..Nav::ORDER.len()).map(MFocus::Nav));
        order.extend([MFocus::AddMedia, MFocus::Search]);
        if self.has_filters() {
            order.push(MFocus::ClearFilters);
        }
        order.push(MFocus::Sort);
        order.push(if self.visible(lib).is_empty() { MFocus::EmptyAction } else { MFocus::Gallery });
        if self.selected.is_some() {
            order.extend([MFocus::RenameInput, MFocus::Rename, MFocus::OpenFolder]);
            if self.confirming {
                order.extend([MFocus::ConfirmMove, MFocus::ConfirmCancel]);
            } else {
                order.push(MFocus::Recycle);
            }
        }
        order
    }

    pub fn tab(&mut self, lib: &Library, back: bool) {
        let order = self.focus_order(lib);
        let cur = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if back { (cur + order.len() - 1) % order.len() } else { (cur + 1) % order.len() };
        self.focus = order[next];
        self.focus_visible = true;
        self.sort_open = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (ManagerState, Library) {
        let lib = Library::sample();
        let mut m = ManagerState::new();
        m.sync(&lib);
        (m, lib)
    }

    fn names<'a>(m: &ManagerState, lib: &'a Library) -> Vec<&'a str> {
        m.visible(lib).iter().map(|i| i.name.as_str()).collect()
    }

    fn set_rename(m: &mut ManagerState, lib: &Library, text: &str) {
        m.rename.select_all();
        m.rename.backspace();
        m.rename.insert(text);
        m.rename_edited(lib);
    }

    #[test]
    fn starts_with_the_first_item_selected_and_its_name_loaded() {
        let (m, lib) = setup();
        assert_eq!(m.selected, Some(1));
        assert_eq!(m.rename.text(), "Happy dance");
        assert_eq!(m.count_label(&lib), "8 items");
        assert!(!m.has_filters());
    }

    #[test]
    fn type_filter_selects_the_first_visible_item() {
        let (mut m, lib) = setup();
        m.select(2, &lib);
        m.set_filter(TypeFilter::Gif, &lib);
        assert_eq!(names(&m, &lib), ["Happy dance.gif", "Hello.gif", "Reaction.gif"]);
        assert_eq!(m.selected, Some(1));
        assert!(m.has_filters());
        assert!(m.nav_selected(Nav::Gif));
        m.set_filter(TypeFilter::Video, &lib);
        assert_eq!(names(&m, &lib), ["Race clip.mp4", "Celebration.mp4"]);
    }

    #[test]
    fn sort_by_name_is_case_insensitive() {
        let (mut m, lib) = setup();
        m.set_sort(Sort::Name, &lib);
        assert_eq!(
            names(&m, &lib),
            [
                "Celebration.mp4", "Happy dance.gif", "Hello.gif", "Portrait.png", "Race clip.mp4", "Reaction.gif",
                "Square.png", "Wallpaper.jpg"
            ]
        );
        m.set_sort(Sort::RecentlyAdded, &lib);
        assert_eq!(names(&m, &lib)[0], "Happy dance.gif");
    }

    #[test]
    fn search_uses_the_shared_rule_and_empties_the_inspector() {
        let (mut m, lib) = setup();
        m.search.insert("zzzz");
        m.search_changed(&lib);
        assert!(names(&m, &lib).is_empty());
        assert_eq!(m.selected, None);
        m.clear_filters(&lib);
        assert_eq!(m.visible(&lib).len(), 8);
        assert_eq!(m.selected, Some(1));
    }

    #[test]
    fn rename_validates_live_and_keeps_the_extension() {
        let (mut m, mut lib) = setup();
        m.select(3, &lib);
        assert_eq!(m.rename.text(), "Race clip");
        set_rename(&mut m, &lib, "Cat/photo");
        assert_eq!(m.rename_error, Some(NameError::IllegalChar));
        assert!(!m.can_rename());
        set_rename(&mut m, &lib, "");
        assert_eq!(m.rename_error, Some(NameError::Empty));
        set_rename(&mut m, &lib, "Celebration");
        assert_eq!(m.rename_error, Some(NameError::Taken));
        m.apply_rename(&mut lib);
        assert_eq!(lib.get(3).map(|i| i.name.as_str()), Some("Race clip.mp4"), "invalid rename changes nothing");
        set_rename(&mut m, &lib, "Renamed clip");
        assert!(m.can_rename());
        m.apply_rename(&mut lib);
        assert_eq!(lib.get(3).map(|i| i.name.as_str()), Some("Renamed clip.mp4"));
        assert_eq!(m.status.as_deref(), Some("Renamed to Renamed clip.mp4."));
        assert_eq!(m.selected, Some(3), "same item, same id");
    }

    #[test]
    fn escape_reverts_the_name_field() {
        let (mut m, lib) = setup();
        set_rename(&mut m, &lib, "Cat/photo");
        m.revert_rename(&lib);
        assert_eq!(m.rename.text(), "Happy dance");
        assert_eq!(m.rename_error, None);
    }

    #[test]
    fn recycle_needs_confirmation_and_selects_the_next_item() {
        let (mut m, mut lib) = setup();
        m.select(2, &lib);
        m.confirm_recycle(&mut lib);
        assert_eq!(lib.items.len(), 8, "nothing happens without confirmation");
        m.ask_recycle();
        assert!(m.confirming);
        m.cancel_recycle();
        assert!(!m.confirming);
        assert_eq!(lib.items.len(), 8);
        m.ask_recycle();
        m.confirm_recycle(&mut lib);
        assert_eq!(lib.items.len(), 7);
        assert!(lib.get(2).is_none());
        assert_eq!(m.selected, Some(3), "Race clip took Square's place");
        assert_eq!(m.rename.text(), "Race clip");
    }

    #[test]
    fn removing_the_last_visible_item_selects_the_new_last() {
        let (mut m, mut lib) = setup();
        m.set_filter(TypeFilter::Video, &lib);
        m.select(6, &lib);
        m.ask_recycle();
        m.confirm_recycle(&mut lib);
        assert_eq!(m.selected, Some(3));
    }

    #[test]
    fn changing_selection_cancels_a_pending_confirmation() {
        let (mut m, lib) = setup();
        m.ask_recycle();
        m.move_selection(&lib, 1);
        assert!(!m.confirming);
        assert_eq!(m.selected, Some(2));
    }

    #[test]
    fn import_shows_newest_first_unfiltered() {
        let (mut m, mut lib) = setup();
        m.set_filter(TypeFilter::Video, &lib);
        m.set_sort(Sort::Name, &lib);
        lib.items.insert(0, Item { id: 99, name: "New.png".into(), kind: crate::sample::MediaKind::Picture, width: 0, height: 0 });
        m.show_imported("1 file added".into(), &lib);
        assert_eq!(m.filter, TypeFilter::All);
        assert_eq!(m.sort, Sort::RecentlyAdded);
        assert_eq!(m.selected, Some(99));
        assert_eq!(names(&m, &lib)[0], "New.png");
    }

    #[test]
    fn tab_order_follows_the_screen() {
        let (mut m, lib) = setup();
        let order = m.focus_order(&lib);
        assert_eq!(order[0], MFocus::Close);
        assert!(!order.contains(&MFocus::ClearFilters));
        assert!(order.contains(&MFocus::Recycle));
        m.set_filter(TypeFilter::Gif, &lib);
        m.ask_recycle();
        let order = m.focus_order(&lib);
        assert!(order.contains(&MFocus::ClearFilters));
        assert!(order.ends_with(&[MFocus::ConfirmMove, MFocus::ConfirmCancel]));
        m.focus = MFocus::ConfirmCancel;
        m.tab(&lib, false);
        assert_eq!(m.focus, MFocus::Close);
    }
}
