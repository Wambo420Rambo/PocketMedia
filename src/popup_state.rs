//! What the quick popup shows and where keyboard focus is. No drawing, no Win32.

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

    pub fn label(self) -> &'static str {
        match self {
            ListKind::Recent => "Recently used",
            ListKind::All => "All media",
        }
    }
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

    pub fn label(self) -> &'static str {
        match self {
            TypeFilter::All => "All",
            TypeFilter::Gif => "GIF",
            TypeFilter::Picture => "Picture",
            TypeFilter::Video => "Video",
        }
    }

    fn accepts(self, kind: MediaKind) -> bool {
        match self {
            TypeFilter::All => true,
            TypeFilter::Gif => kind == MediaKind::Gif,
            TypeFilter::Picture => kind == MediaKind::Picture,
            TypeFilter::Video => kind == MediaKind::Video,
        }
    }
}

/// Keyboard focus targets, in Tab order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Close,
    OpenManager,
    AddMedia,
    Search,
    Type(usize),
    List,
    Gallery,
    EmptyAction,
    ClearHistory,
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

pub struct PopupState {
    pub items: Vec<Item>,
    pub recent: Vec<u32>,
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
    /// Highlighted row while the Recently used / All media list is open.
    pub dropdown: Option<usize>,
    /// Result line shown in the footer after an import, until the popup reopens.
    pub status: Option<String>,
}

impl PopupState {
    pub fn new(items: Vec<Item>, recent: Vec<u32>) -> Self {
        Self {
            items,
            recent,
            list: ListKind::Recent,
            filter: TypeFilter::All,
            search: TextField::default(),
            selected: 0,
            scroll: 0.0,
            focus: Focus::Search,
            focus_visible: false,
            dropdown: None,
            status: None,
        }
    }

    pub fn visible(&self) -> Vec<&Item> {
        let base: Vec<&Item> = match self.list {
            ListKind::Recent => self.recent.iter().filter_map(|id| self.items.iter().find(|i| i.id == *id)).collect(),
            ListKind::All => self.items.iter().collect(),
        };
        base.into_iter()
            .filter(|i| self.filter.accepts(i.kind) && search::matches(&i.name, self.search.text()))
            .collect()
    }

    pub fn empty(&self) -> Option<Empty> {
        if !self.visible().is_empty() {
            None
        } else if self.items.is_empty() {
            Some(Empty::NoLibrary)
        } else if self.list == ListKind::Recent && self.recent.is_empty() {
            Some(Empty::NoHistory)
        } else {
            Some(Empty::NoMatch)
        }
    }

    pub fn count_label(&self) -> String {
        let n = self.visible().len();
        format!("{n} item{}", if n == 1 { "" } else { "s" })
    }

    pub fn selected_index(&self) -> Option<usize> {
        let n = self.visible().len();
        (n > 0).then(|| self.selected.min(n - 1))
    }

    /// Fresh start each time the popup opens: empty search with focus, top of the list.
    /// The chosen list and type filter are kept.
    pub fn reset_for_open(&mut self) {
        self.status = None;
        self.search.clear();
        self.focus = Focus::Search;
        self.focus_visible = false;
        self.dropdown = None;
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
        self.dropdown = None;
        self.reset_view();
    }

    /// After an import: unfiltered All media from the top, so new items show first.
    pub fn show_imported(&mut self, status: String) {
        self.list = ListKind::All;
        self.filter = TypeFilter::All;
        self.search.clear();
        self.dropdown = None;
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
    pub fn clear_history(&mut self) {
        self.recent.clear();
        if self.focus == Focus::ClearHistory {
            self.focus = Focus::Search;
        }
        self.reset_view();
    }

    pub fn run_empty_action(&mut self) {
        match self.empty() {
            Some(Empty::NoMatch) => self.clear_filters(),
            Some(Empty::NoHistory) => self.set_list(ListKind::All),
            // Add media is a later phase.
            Some(Empty::NoLibrary) | None => {}
        }
    }

    pub fn history_enabled(&self) -> bool {
        !self.recent.is_empty()
    }

    pub fn focus_order(&self) -> Vec<Focus> {
        let mut order = vec![Focus::Close, Focus::OpenManager, Focus::AddMedia, Focus::Search];
        order.extend((0..TypeFilter::ORDER.len()).map(Focus::Type));
        order.push(Focus::List);
        match self.empty() {
            None => order.push(Focus::Gallery),
            Some(Empty::NoLibrary) => {}
            Some(_) => order.push(Focus::EmptyAction),
        }
        if self.history_enabled() {
            order.push(Focus::ClearHistory);
        }
        order
    }

    pub fn tab(&mut self, back: bool) {
        let order = self.focus_order();
        let current = order.iter().position(|f| *f == self.focus).unwrap_or(3);
        let next = if back { (current + order.len() - 1) % order.len() } else { (current + 1) % order.len() };
        self.focus = order[next];
        self.focus_visible = true;
        self.dropdown = None;
    }

    /// Moves the gallery selection by `delta` tiles, clamped to the list.
    pub fn move_selection(&mut self, delta: isize) {
        let n = self.visible().len();
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
    use crate::sample;

    fn state() -> PopupState {
        PopupState::new(sample::items(), sample::recent())
    }

    fn names(s: &PopupState) -> Vec<&str> {
        s.visible().iter().map(|i| i.name.as_str()).collect()
    }

    #[test]
    fn recent_list_follows_usage_order() {
        let s = state();
        assert_eq!(names(&s)[..3], ["Happy dance.gif", "Square.png", "Race clip.mp4"]);
        assert_eq!(names(&s)[3], "Celebration.mp4");
        assert_eq!(s.count_label(), "8 items");
    }

    #[test]
    fn type_filters_are_independent() {
        let mut s = state();
        s.set_list(ListKind::All);
        s.set_filter(TypeFilter::Gif);
        assert_eq!(names(&s), ["Happy dance.gif", "Hello.gif", "Reaction.gif"]);
        s.set_filter(TypeFilter::Picture);
        assert_eq!(names(&s), ["Square.png", "Wallpaper.jpg", "Portrait.png"]);
        s.set_filter(TypeFilter::Video);
        assert_eq!(names(&s), ["Race clip.mp4", "Celebration.mp4"]);
    }

    #[test]
    fn search_works_within_the_chosen_type() {
        let mut s = state();
        s.set_list(ListKind::All);
        s.search.insert("o");
        s.set_filter(TypeFilter::Video);
        assert_eq!(names(&s), ["Celebration.mp4"]);
        s.set_filter(TypeFilter::Gif);
        assert_eq!(names(&s), ["Hello.gif", "Reaction.gif"]);
    }

    #[test]
    fn wireframe_searches() {
        let mut s = state();
        s.set_list(ListKind::All);
        s.search.insert("dance happy");
        assert_eq!(names(&s), ["Happy dance.gif"]);
        assert_eq!(s.count_label(), "1 item");
        s.search.select_all();
        s.search.insert("celeb");
        assert_eq!(names(&s), ["Celebration.mp4"]);
        s.search.select_all();
        s.search.insert("gif");
        assert!(names(&s).is_empty());
        assert_eq!(s.empty(), Some(Empty::NoMatch));
    }

    #[test]
    fn clear_filters_restores_everything() {
        let mut s = state();
        s.set_filter(TypeFilter::Gif);
        s.search.insert("zzzz");
        s.run_empty_action();
        assert_eq!(s.visible().len(), 8);
        assert_eq!(s.filter, TypeFilter::All);
    }

    #[test]
    fn clear_history_only_clears_usage() {
        let mut s = state();
        s.clear_history();
        assert_eq!(s.empty(), Some(Empty::NoHistory));
        assert_eq!(s.items.len(), 8);
        s.run_empty_action();
        assert_eq!(s.list, ListKind::All);
        assert_eq!(s.visible().len(), 8);
    }

    #[test]
    fn tab_order_wraps_and_skips_missing_controls() {
        let mut s = state();
        s.tab(false);
        assert_eq!(s.focus, Focus::Type(0));
        for _ in 0..4 {
            s.tab(false);
        }
        assert_eq!(s.focus, Focus::List);
        s.tab(false);
        assert_eq!(s.focus, Focus::Gallery);
        s.tab(false);
        assert_eq!(s.focus, Focus::ClearHistory);
        s.tab(false);
        assert_eq!(s.focus, Focus::Close);
        s.clear_history();
        s.focus = Focus::List;
        s.tab(false);
        assert_eq!(s.focus, Focus::EmptyAction);
        s.tab(false);
        assert_eq!(s.focus, Focus::Close, "Clear history is disabled, so it is skipped");
        s.tab(true);
        assert_eq!(s.focus, Focus::EmptyAction);
    }

    #[test]
    fn selection_is_clamped() {
        let mut s = state();
        s.move_selection(-5);
        assert_eq!(s.selected, 0);
        s.move_selection(100);
        assert_eq!(s.selected, 7);
        s.set_filter(TypeFilter::Video);
        assert_eq!(s.selected_index(), Some(0));
    }
}
