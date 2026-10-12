//! The one in-memory library shared by every window (the popup, the Manager and
//! Add media), so a rename or import in one shows in the others.
//! Fake items from `sample`; Phase 3 replaces this with the real store.

use std::cell::RefCell;

use crate::names::{self, NameError};
use crate::sample::{self, Item};
use crate::search::name_without_extension;

pub struct Library {
    /// Most recently added first.
    pub items: Vec<Item>,
    /// Recently used item ids, most recent first.
    pub recent: Vec<u32>,
}

thread_local! {
    static LIBRARY: RefCell<Library> = RefCell::new(Library::sample());
}

/// Reads the library. Keep the closure short: no window calls inside it.
pub fn with<R>(f: impl FnOnce(&Library) -> R) -> R {
    LIBRARY.with(|l| f(&l.borrow()))
}

/// Changes the library. Keep the closure short: no window calls inside it.
pub fn with_mut<R>(f: impl FnOnce(&mut Library) -> R) -> R {
    LIBRARY.with(|l| f(&mut l.borrow_mut()))
}

impl Library {
    pub fn sample() -> Self {
        Self { items: sample::items(), recent: sample::recent() }
    }

    pub fn get(&self, id: u32) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Checks a new name (without extension) for item `id` with the shared name
    /// rules. Its own current name never counts as taken, so changing only the
    /// letter case is allowed.
    pub fn check_rename(&self, id: u32, base: &str) -> Result<(), NameError> {
        let item = self.get(id).ok_or(NameError::Empty)?;
        names::check(base)?;
        let ext = extension(&item.name);
        let full = names::final_name(base, ext).to_lowercase();
        let taken = self.items.iter().any(|i| i.id != id && i.name.to_lowercase() == full);
        if taken { Err(NameError::Taken) } else { Ok(()) }
    }

    /// Renames item `id`, keeping its extension, id and history entry.
    /// Returns the new full name.
    pub fn rename(&mut self, id: u32, base: &str) -> Result<String, NameError> {
        self.check_rename(id, base)?;
        let item = self.items.iter_mut().find(|i| i.id == id).ok_or(NameError::Empty)?;
        let new_name = names::final_name(base, extension(&item.name));
        item.name = new_name.clone();
        Ok(new_name)
    }

    /// Removes item `id` and its usage history. Returns its name.
    pub fn remove(&mut self, id: u32) -> Option<String> {
        let pos = self.items.iter().position(|i| i.id == id)?;
        self.recent.retain(|r| *r != id);
        Some(self.items.remove(pos).name)
    }
}

/// `.png` for `Square.png`; empty when there is no extension.
pub fn extension(file_name: &str) -> &str {
    &file_name[name_without_extension(file_name).len()..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_keeps_extension_id_and_history() {
        let mut lib = Library::sample();
        assert_eq!(lib.rename(3, "Renamed clip"), Ok("Renamed clip.mp4".into()));
        assert_eq!(lib.get(3).map(|i| i.name.as_str()), Some("Renamed clip.mp4"));
        assert!(lib.recent.contains(&3));
    }

    #[test]
    fn rename_uses_the_shared_name_rules() {
        let lib = Library::sample();
        assert_eq!(lib.check_rename(3, "Cat/photo"), Err(NameError::IllegalChar));
        assert_eq!(lib.check_rename(3, "  "), Err(NameError::Empty));
        assert_eq!(lib.check_rename(3, "nul"), Err(NameError::Reserved));
        assert_eq!(lib.check_rename(3, "clip."), Err(NameError::TrailingSpaceOrPeriod));
    }

    #[test]
    fn rename_never_takes_another_items_name() {
        let mut lib = Library::sample();
        // Race clip.mp4 -> celebration.mp4 collides with Celebration.mp4 (any case).
        assert_eq!(lib.rename(3, "celebration"), Err(NameError::Taken));
        // A different extension is a different file.
        assert_eq!(lib.check_rename(3, "Square"), Ok(()));
        // Changing only the case of its own name is fine.
        assert_eq!(lib.rename(3, "RACE CLIP"), Ok("RACE CLIP.mp4".into()));
    }

    #[test]
    fn remove_drops_the_item_and_its_history() {
        let mut lib = Library::sample();
        assert_eq!(lib.remove(2), Some("Square.png".into()));
        assert!(lib.get(2).is_none());
        assert!(!lib.recent.contains(&2));
        assert_eq!(lib.items.len(), 7);
        assert_eq!(lib.remove(2), None);
    }

    #[test]
    fn extension_of_names() {
        assert_eq!(extension("Square.png"), ".png");
        assert_eq!(extension("clip.final.mp4"), ".mp4");
        assert_eq!(extension("no ext"), "");
    }
}
