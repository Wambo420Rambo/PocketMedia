//! Add media flow state (Focused panel): the start page and the review list with
//! an editable name per file. Phase 2a: the files are fake (see `sample`); nothing
//! is read or copied.

use crate::names::{self, NameError};
use crate::sample::{self, Item, MediaKind};
use crate::search::name_without_extension;
use crate::text_field::TextField;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowStatus {
    Copy,
    /// Identical content is already in the library.
    Skip,
}

#[derive(Debug)]
pub struct ImportRow {
    pub source: String,
    pub kind: MediaKind,
    pub status: RowStatus,
    /// Editable name without the extension (copy rows only).
    pub name: TextField,
    /// A number was added because the name was taken; cleared once the user edits.
    pub auto_renamed: bool,
    pub error: Option<NameError>,
}

impl ImportRow {
    pub fn ext(&self) -> &str {
        &self.source[name_without_extension(&self.source).len()..]
    }

    pub fn destination(&self) -> &'static str {
        match self.kind {
            MediaKind::Picture => "Pictures",
            MediaKind::Gif => "GIFs",
            MediaKind::Video => "Videos",
        }
    }

    /// The line under the name, if any.
    pub fn message(&self) -> Option<&'static str> {
        match (self.status, self.error) {
            (RowStatus::Skip, _) => Some("Already in your library"),
            (_, Some(e)) => Some(e.message()),
            _ if self.auto_renamed => Some("A file with this name already exists, so a number was added. You can change it."),
            _ => None,
        }
    }
}

/// Keyboard focus inside the Add media panel, in Tab order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AddFocus {
    Close,
    Back,
    ChooseFiles,
    ImportExisting,
    Row(usize),
    ChooseAgain,
    Cancel,
    Add,
}

pub struct AddMedia {
    pub rows: Vec<ImportRow>,
    pub focus: AddFocus,
    pub focus_visible: bool,
    pub scroll: f32,
}

impl AddMedia {
    pub fn new() -> Self {
        Self { rows: Vec::new(), focus: AddFocus::ChooseFiles, focus_visible: false, scroll: 0.0 }
    }

    pub fn reviewing(&self) -> bool {
        !self.rows.is_empty()
    }

    /// "Choose files" and "Import your existing media" both produce the wireframe's
    /// example batch in Phase 2a.
    pub fn choose(&mut self, library: &[Item]) {
        self.rows = sample::import_rows()
            .into_iter()
            .map(|(source, kind, status, dest, auto_renamed)| {
                let mut name = TextField::default();
                name.insert(name_without_extension(dest.unwrap_or(source)));
                ImportRow { source: source.into(), kind, status, name, auto_renamed, error: None }
            })
            .collect();
        self.scroll = 0.0;
        // Focus ← rather than the first name: a focused text box always shows its
        // ring, which the review screen design does not. Tab reaches the names next.
        self.focus = AddFocus::Back;
        self.validate(library);
    }

    /// Back to the start page (← or Choose again).
    pub fn reset(&mut self) {
        self.rows.clear();
        self.scroll = 0.0;
        self.focus = AddFocus::ChooseFiles;
    }

    pub fn validate(&mut self, library: &[Item]) {
        let names: Vec<&str> = library.iter().map(|i| i.name.as_str()).collect();
        let copy: Vec<usize> = (0..self.rows.len()).filter(|i| self.rows[*i].status == RowStatus::Copy).collect();
        let results = names::check_batch(copy.iter().map(|i| (self.rows[*i].name.text(), self.rows[*i].ext())), &names);
        for (i, r) in copy.into_iter().zip(results) {
            self.rows[i].error = r;
        }
    }

    /// Call after the user edits row `i`.
    pub fn edited(&mut self, i: usize, library: &[Item]) {
        if let Some(row) = self.rows.get_mut(i) {
            row.auto_renamed = false;
        }
        self.validate(library);
    }

    pub fn ready(&self) -> usize {
        self.rows.iter().filter(|r| r.status == RowStatus::Copy).count()
    }

    pub fn skipped(&self) -> usize {
        self.rows.iter().filter(|r| r.status == RowStatus::Skip).count()
    }

    /// Add stays disabled while any row is invalid or nothing can be added.
    pub fn can_add(&self) -> bool {
        self.ready() > 0 && self.rows.iter().all(|r| r.error.is_none())
    }

    pub fn summary(&self) -> String {
        let s = self.skipped();
        let mut text = format!("{} ready to add", self.ready());
        if s > 0 {
            text += &format!(" · {s} duplicate{} skipped", if s == 1 { "" } else { "s" });
        }
        text
    }

    pub fn add_label(&self) -> String {
        let n = self.ready();
        format!("Add {n} file{}", if n == 1 { "" } else { "s" })
    }

    /// Adds the rows to the (fake) library. Newest first, like the wireframe.
    /// Returns the status line, or None if adding is not allowed.
    pub fn import_into(&mut self, library: &mut Vec<Item>) -> Option<String> {
        if !self.can_add() {
            return None;
        }
        let first_id = library.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        let added = self.ready();
        for (id, row) in (first_id..).zip(self.rows.iter().filter(|r| r.status == RowStatus::Copy)) {
            library.push(Item {
                id,
                name: names::final_name(row.name.text(), row.ext()),
                kind: row.kind,
                width: 0,
                height: 0,
            });
        }
        library.sort_by_key(|i| std::cmp::Reverse(i.id));
        let s = self.skipped();
        let status = format!(
            "{added} file{} added · {s} duplicate{} skipped · Originals left where they were.",
            if added == 1 { "" } else { "s" },
            if s == 1 { "" } else { "s" }
        );
        self.reset();
        Some(status)
    }

    pub fn focus_order(&self) -> Vec<AddFocus> {
        let mut order = vec![AddFocus::Close, AddFocus::Back];
        if self.reviewing() {
            order.extend(
                (0..self.rows.len()).filter(|i| self.rows[*i].status == RowStatus::Copy).map(AddFocus::Row),
            );
            order.extend([AddFocus::ChooseAgain, AddFocus::Cancel]);
            if self.can_add() {
                order.push(AddFocus::Add);
            }
        } else {
            order.extend([AddFocus::ChooseFiles, AddFocus::ImportExisting]);
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

    fn review() -> (AddMedia, Vec<Item>) {
        let lib = sample::items();
        let mut a = AddMedia::new();
        a.choose(&lib);
        (a, lib)
    }

    fn rename(a: &mut AddMedia, lib: &[Item], row: usize, to: &str) {
        a.rows[row].name.select_all();
        a.rows[row].name.backspace();
        a.rows[row].name.insert(to);
        a.edited(row, lib);
    }

    #[test]
    fn example_batch_matches_the_wireframe() {
        let (a, _) = review();
        let names: Vec<&str> = a.rows.iter().map(|r| r.name.text()).collect();
        assert_eq!(names, ["Cat", "Wave", "Clip (2)", "IMG_4821", "Portrait"]);
        assert_eq!(a.summary(), "4 ready to add · 1 duplicate skipped");
        assert_eq!(a.add_label(), "Add 4 files");
        assert_eq!(a.rows[0].ext(), ".png");
        assert_eq!(a.rows[2].destination(), "Videos");
        assert!(a.rows[2].message().unwrap().starts_with("A file with this name already exists"));
        assert_eq!(a.rows[4].message(), Some("Already in your library"));
        assert!(a.can_add());
    }

    #[test]
    fn owner_test_cases_disable_add() {
        let (mut a, lib) = review();
        rename(&mut a, &lib, 0, "Cat/photo");
        assert_eq!(a.rows[0].error, Some(NameError::IllegalChar));
        assert!(!a.can_add());
        rename(&mut a, &lib, 0, "");
        assert_eq!(a.rows[0].error, Some(NameError::Empty));
        assert!(!a.can_add());
        rename(&mut a, &lib, 0, "Wave");
        // Different extensions (.png vs .gif) are different files...
        assert!(a.can_add());
        rename(&mut a, &lib, 3, "wave");
        // ...but two .png rows with the same name (any case) collide; the later row is flagged.
        assert_eq!(a.rows[3].error, Some(NameError::Taken));
        assert!(!a.can_add());
        rename(&mut a, &lib, 3, "Birthday confetti");
        assert!(a.can_add());
    }

    #[test]
    fn editing_clears_the_auto_rename_note() {
        let (mut a, lib) = review();
        rename(&mut a, &lib, 2, "Clip final");
        assert_eq!(a.rows[2].message(), None);
    }

    #[test]
    fn import_puts_new_items_first_and_second_batch_collides() {
        let (mut a, mut lib) = review();
        rename(&mut a, &lib, 0, "Cat photo");
        rename(&mut a, &lib, 3, "Birthday confetti");
        let status = a.import_into(&mut lib).unwrap();
        assert_eq!(status, "4 files added · 1 duplicate skipped · Originals left where they were.");
        let names: Vec<&str> = lib.iter().take(5).map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["Birthday confetti.png", "Clip (2).mp4", "Wave.gif", "Cat photo.png", "Reaction.gif"]);
        assert!(!a.reviewing());
        // focused-14: the same batch again now collides with Wave and Clip (2).
        a.choose(&lib);
        assert_eq!(a.rows[1].error, Some(NameError::Taken));
        assert_eq!(a.rows[2].error, Some(NameError::Taken));
        assert_eq!(a.rows[0].error, None);
        assert!(!a.can_add());
        assert_eq!(a.import_into(&mut lib), None);
    }

    #[test]
    fn tab_skips_add_while_disabled() {
        let (mut a, lib) = review();
        rename(&mut a, &lib, 0, "");
        a.focus = AddFocus::Cancel;
        a.tab(false);
        assert_eq!(a.focus, AddFocus::Close);
    }
}
