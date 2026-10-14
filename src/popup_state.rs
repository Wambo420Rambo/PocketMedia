//! What the quick popup shows and where keyboard focus is. No drawing, no Win32.

use crate::library::Library;
use crate::sample::{Item, MediaKind};
use crate::search;
use crate::text_field::TextField;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListKind {
    Recent,
    All,
}

impl ListKind {
    pub const BOTH: [ListKind; 2] = [ListKind::Recent, ListKind::All];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeFilter {
    All,
    Gif,
    Picture,
    Video,
}

impl TypeFilter {
    pub const ORDER: [TypeFilter; 4] = [TypeFilter::All, TypeFilter::Gif, TypeFilter::Picture, TypeFilter::Video];

    pub fn accepts(self, kind: MediaKind) -> bool {
        match self {
            TypeFilter::All => true,
            TypeFilter::Gif => kind == MediaKind::Gif,
            TypeFilter::Picture => kind == MediaKind::Picture,
            TypeFilter::Video => kind == MediaKind::Video,
        }
    }
}

/// Keyboard focus targets (Tab order: `PopupState::focus_order`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Search,
    Close,
    Type(usize),
    /// The Recent / All media tabs (one stop; arrows switch).
    List,
    Gallery,
    EmptyAction,
    ClearHistory,
    OpenManager,
    AddMedia,
}

/// Why the gallery is empty; each case has its own message and action.
#[allow(clippy::enum_variant_names)] // reads naturally as Empty::NoMatch
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Empty {
    NoLibrary,
    NoHistory,
    NoMatch,
}

impl Empty {
    pub fn heading(self) -> &'static str {
        match self {
            Empty::NoLibrary => "Your library is empty",
            Empty::NoHistory => "No recently used media",
            Empty::NoMatch => "No matching media",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Empty::NoLibrary => "Choose files or import your existing collection.",
            Empty::NoHistory => "Your next selection will appear here.",
            Empty::NoMatch => "Try another name or clear your filters.",
        }
    }

    pub fn action(self) -> &'static str {
        match self {
            Empty::NoLibrary => "Add media",
            Empty::NoHistory => "Show all media",
            Empty::NoMatch => "Clear filters",
        }
    }
}

/// The popup's view of the shared `Library`: which list, filter and search, the
/// selection and focus. Methods that need the items take the library.
pub struct PopupState {
    pub list: ListKind,
    pub filter: TypeFilter,
    pub search: TextField,
    /// Index into `visible()`.
    pub selected: usize,
    /// Gallery scroll offset in DIPs.
    pub scroll: f32,
    pub focus: Focus,
    /// Show focus rings (keyboard navigation), as CSS :focus-visible does.
    pub focus_visible: bool,
    /// Result line shown in the footer after an import, until the popup reopens.
    pub status: Option<String>,
}

impl Default for PopupState {
    fn default() -> Self {
        Self::new()
    }
}

impl PopupState {
    pub fn new() -> Self {
        Self {
            list: ListKind::Recent,
            filter: TypeFilter::All,
            search: TextField::default(),
            selected: 0,
            scroll: 0.0,
            focus: Focus::Search,
            focus_visible: false,
            status: None,
        }
    }

    pub fn visible<'a>(&self, lib: &'a Library) -> Vec<&'a Item> {
        self.visible_in(lib, self.list)
    }

    /// What `list` would show with the current type and search.
    fn visible_in<'a>(&self, lib: &'a Library, list: ListKind) -> Vec<&'a Item> {
        let base: Vec<&Item> = match list {
            ListKind::Recent => lib.recent.iter().filter_map(|id| lib.get(*id)).collect(),
            ListKind::All => lib.items.iter().collect(),
        };
        base.into_iter()
            .filter(|i| self.filter.accepts(i.kind) && search::matches(&i.name, self.search.text()))
            .collect()
    }

    pub fn empty(&self, lib: &Library) -> Option<Empty> {
        if !self.visible(lib).is_empty() {
            None
        } else if lib.items.is_empty() {
            Some(Empty::NoLibrary)
        } else if self.list == ListKind::Recent && lib.recent.is_empty() {
            Some(Empty::NoHistory)
        } else {
            Some(Empty::NoMatch)
        }
    }

    /// The number on a list's tab.
    pub fn count_for(&self, lib: &Library, list: ListKind) -> usize {
        self.visible_in(lib, list).len()
    }

    pub fn selected_index(&self, lib: &Library) -> Option<usize> {
        let n = self.visible(lib).len();
        (n > 0).then(|| self.selected.min(n - 1))
    }

    /// Fresh start each time the popup opens: empty search with focus, top of the list.
    /// The chosen list and type filter are kept.
    pub fn reset_for_open(&mut self) {
        self.status = None;
        self.search.clear();
        self.focus = Focus::Search;
        self.focus_visible = false;
        self.reset_view();
    }

    pub fn reset_view(&mut self) {
        self.selected = 0;
        self.scroll = 0.0;
    }

    pub fn set_filter(&mut self, filter: TypeFilter) {
        self.filter = filter;
        self.reset_view();
    }

    pub fn set_list(&mut self, list: ListKind) {
        self.list = list;
        self.reset_view();
    }

    /// After an import: unfiltered All media from the top, so new items show first.
    pub fn show_imported(&mut self, status: String) {
        self.list = ListKind::All;
        self.filter = TypeFilter::All;
        self.search.clear();
        self.focus = Focus::Search;
        self.status = Some(status);
        self.reset_view();
    }

    pub fn clear_filters(&mut self) {
        self.search.clear();
        self.filter = TypeFilter::All;
        self.reset_view();
    }

    /// Clears usage records only; items are untouched.
    pub fn clear_history(&mut self, lib: &mut Library) {
        lib.recent.clear();
        if self.focus == Focus::ClearHistory {
            self.focus = Focus::Search;
        }
        self.reset_view();
    }

    pub fn run_empty_action(&mut self, lib: &Library) {
        match self.empty(lib) {
            Some(Empty::NoMatch) => self.clear_filters(),
            Some(Empty::NoHistory) => self.set_list(ListKind::All),
            // Add media is a later phase.
            Some(Empty::NoLibrary) | None => {}
        }
    }

    pub fn history_enabled(&self, lib: &Library) -> bool {
        !lib.recent.is_empty()
    }

    pub fn focus_order(&self, lib: &Library) -> Vec<Focus> {
        // Visual order: header, type and tabs, gallery, footer.
        let mut order = vec![Focus::Search, Focus::Close];
        order.extend((0..TypeFilter::ORDER.len()).map(Focus::Type));
        order.push(Focus::List);
        match self.empty(lib) {
            None => order.push(Focus::Gallery),
            Some(Empty::NoLibrary) => {}
            Some(_) => order.push(Focus::EmptyAction),
        }
        if self.history_enabled(lib) {
            order.push(Focus::ClearHistory);
        }
        order.extend([Focus::OpenManager, Focus::AddMedia]);
        order
    }

    pub fn tab(&mut self, lib: &Library, back: bool) {
        let order = self.focus_order(lib);
        let current = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        let next = if back { (current + order.len() - 1) % order.len() } else { (current + 1) % order.len() };
        self.focus = order[next];
        self.focus_visible = true;
    }

    /// Moves the gallery selection by `delta` tiles, clamped to the list.
    pub fn move_selection(&mut self, lib: &Library, delta: isize) {
        let n = self.visible(lib).len();
        if n == 0 {
            return;
        }
        let cur = self.selected.min(n - 1) as isize;
        self.selected = (cur + delta).clamp(0, n as isize - 1) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> (PopupState, Library) {
        (PopupState::new(), Library::sample())
    }

    fn names<'a>(s: &PopupState, lib: &'a Library) -> Vec<&'a str> {
        s.visible(lib).iter().map(|i| i.name.as_str()).collect()
    }

    #[test]
    fn recent_list_follows_usage_order() {
        let (s, lib) = state();
        assert_eq!(names(&s, &lib)[..3], ["Happy dance.gif", "Square.png", "Race clip.mp4"]);
        assert_eq!(names(&s, &lib)[3], "Celebration.mp4");
        assert_eq!(s.count_for(&lib, ListKind::Recent), 8);
    }

    #[test]
    fn type_filters_are_independent() {
        let (mut s, lib) = state();
        s.set_list(ListKind::All);
        s.set_filter(TypeFilter::Gif);
        assert_eq!(names(&s, &lib), ["Happy dance.gif", "Hello.gif", "Reaction.gif"]);
        s.set_filter(TypeFilter::Picture);
        assert_eq!(names(&s, &lib), ["Square.png", "Wallpaper.jpg", "Portrait.png"]);
        s.set_filter(TypeFilter::Video);
        assert_eq!(names(&s, &lib), ["Race clip.mp4", "Celebration.mp4"]);
    }

    #[test]
    fn search_works_within_the_chosen_type() {
        let (mut s, lib) = state();
        s.set_list(ListKind::All);
        s.search.insert("o");
        s.set_filter(TypeFilter::Video);
        assert_eq!(names(&s, &lib), ["Celebration.mp4"]);
        s.set_filter(TypeFilter::Gif);
        assert_eq!(names(&s, &lib), ["Hello.gif", "Reaction.gif"]);
    }

    #[test]
    fn wireframe_searches() {
        let (mut s, lib) = state();
        s.set_list(ListKind::All);
        s.search.insert("dance happy");
        assert_eq!(names(&s, &lib), ["Happy dance.gif"]);
        assert_eq!(s.count_for(&lib, ListKind::All), 1);
        s.search.select_all();
        s.search.insert("celeb");
        assert_eq!(names(&s, &lib), ["Celebration.mp4"]);
        s.search.select_all();
        s.search.insert("gif");
        assert!(names(&s, &lib).is_empty());
        assert_eq!(s.empty(&lib), Some(Empty::NoMatch));
    }

    #[test]
    fn clear_filters_restores_everything() {
        let (mut s, lib) = state();
        s.set_filter(TypeFilter::Gif);
        s.search.insert("zzzz");
        s.run_empty_action(&lib);
        assert_eq!(s.visible(&lib).len(), 8);
        assert_eq!(s.filter, TypeFilter::All);
    }

    #[test]
    fn clear_history_only_clears_usage() {
        let (mut s, mut lib) = state();
        s.clear_history(&mut lib);
        assert_eq!(s.empty(&lib), Some(Empty::NoHistory));
        assert_eq!(lib.items.len(), 8);
        s.run_empty_action(&lib);
        assert_eq!(s.list, ListKind::All);
        assert_eq!(s.visible(&lib).len(), 8);
    }

    #[test]
    fn tab_order_follows_the_screen_and_skips_missing_controls() {
        let (mut s, mut lib) = state();
        s.tab(&lib, false);
        assert_eq!(s.focus, Focus::Close);
        s.tab(&lib, false);
        assert_eq!(s.focus, Focus::Type(0));
        for _ in 0..4 {
            s.tab(&lib, false);
        }
        assert_eq!(s.focus, Focus::List);
        s.tab(&lib, false);
        assert_eq!(s.focus, Focus::Gallery);
        s.tab(&lib, false);
        assert_eq!(s.focus, Focus::ClearHistory);
        s.tab(&lib, false);
        assert_eq!(s.focus, Focus::OpenManager);
        s.tab(&lib, false);
        assert_eq!(s.focus, Focus::AddMedia);
        s.tab(&lib, false);
        assert_eq!(s.focus, Focus::Search, "wraps");
        s.clear_history(&mut lib);
        s.focus = Focus::List;
        s.tab(&lib, false);
        assert_eq!(s.focus, Focus::EmptyAction);
        s.tab(&lib, false);
        assert_eq!(s.focus, Focus::OpenManager, "Clear history is disabled, so it is skipped");
        s.tab(&lib, true);
        assert_eq!(s.focus, Focus::EmptyAction);
    }

    #[test]
    fn each_tab_counts_its_own_list() {
        let (mut s, mut lib) = state();
        s.set_filter(TypeFilter::Gif);
        assert_eq!(s.count_for(&lib, ListKind::All), 3);
        lib.recent.truncate(1);
        assert_eq!(s.count_for(&lib, ListKind::Recent), 1, "Happy dance.gif");
    }

    #[test]
    fn selection_is_clamped() {
        let (mut s, lib) = state();
        s.move_selection(&lib, -5);
        assert_eq!(s.selected, 0);
        s.move_selection(&lib, 100);
        assert_eq!(s.selected, 7);
        s.set_filter(TypeFilter::Video);
        assert_eq!(s.selected_index(&lib), Some(0));
    }
}
