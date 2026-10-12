//! The one in-memory library shared by every window (the popup, the Manager and
//! Add media), so a rename, removal or import in one shows in the others. It is
//! loaded from the store at startup and written back with `persist` after every
//! change.

use std::cell::RefCell;

use crate::import::{Copied, KnownFile, Orphan};
use crate::names::{self, NameError};
use crate::sample::{self, Item, MediaKind};
use crate::search::name_without_extension;
use crate::store::{StoreData, StoredItem, StoredKind};

#[derive(Default)]
pub struct Library {
    /// Most recently added first.
    pub items: Vec<Item>,
    /// Recently used item ids, most recent first (at most 20).
    pub recent: Vec<u32>,
    /// The next id to hand out; ids are never reused.
    pub next_id: u32,
}

thread_local! {
    static LIBRARY: RefCell<Library> = RefCell::new(Library::default());
}

/// Reads the library. Keep the closure short: no window calls inside it.
pub fn with<R>(f: impl FnOnce(&Library) -> R) -> R {
    LIBRARY.with(|l| f(&l.borrow()))
}

/// Changes the library. Keep the closure short: no window calls inside it.
pub fn with_mut<R>(f: impl FnOnce(&mut Library) -> R) -> R {
    LIBRARY.with(|l| f(&mut l.borrow_mut()))
}

/// Replaces the library (startup, and the self-test).
pub fn set(lib: Library) {
    LIBRARY.with(|l| *l.borrow_mut() = lib);
}

/// Writes the library into the store and saves it. Returns a message on failure.
pub fn persist() -> Result<(), String> {
    let (items, history, next_id) = with(|lib| (lib.to_stored(), lib.recent.clone(), lib.next_id));
    crate::store::with(|s| {
        s.data.items = items;
        s.data.history = history;
        s.data.next_id = next_id;
        if s.read_only {
            return Err("the library file is from a newer PocketMedia, so changes are not saved".to_string());
        }
        s.save().map(|_| ()).map_err(|e| format!("the library file could not be saved: {e}"))
    })
    .unwrap_or(Ok(()))
}

fn kind_to_stored(k: MediaKind) -> StoredKind {
    match k {
        MediaKind::Picture => StoredKind::Picture,
        MediaKind::Gif => StoredKind::Gif,
        MediaKind::Video => StoredKind::Video,
    }
}

fn kind_from_stored(k: StoredKind) -> MediaKind {
    match k {
        StoredKind::Picture => MediaKind::Picture,
        StoredKind::Gif => MediaKind::Gif,
        StoredKind::Video => MediaKind::Video,
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Library {
    /// The wireframe's 8 example items (unit tests).
    pub fn sample() -> Self {
        Self { items: sample::items(), recent: sample::recent(), next_id: 9 }
    }

    pub fn from_store(data: &StoreData) -> Self {
        let items = data
            .items
            .iter()
            .map(|s| Item {
                id: s.id,
                name: s.name.clone(),
                kind: kind_from_stored(s.kind),
                width: s.width,
                height: s.height,
                path: s.path.clone(),
                size: s.size,
                hash: s.hash.clone(),
                added: s.added,
                source: s.source.clone(),
            })
            .collect();
        Self { items, recent: data.history.clone(), next_id: data.next_id.max(1) }
    }

    pub fn to_stored(&self) -> Vec<StoredItem> {
        self.items
            .iter()
            .map(|i| StoredItem {
                id: i.id,
                name: i.name.clone(),
                kind: kind_to_stored(i.kind),
                path: i.path.clone(),
                size: i.size,
                hash: i.hash.clone(),
                width: i.width,
                height: i.height,
                added: i.added,
                source: i.source.clone(),
            })
            .collect()
    }

    pub fn get(&self, id: u32) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn names(&self) -> Vec<&str> {
        self.items.iter().map(|i| i.name.as_str()).collect()
    }

    /// Every file, for duplicate and name checks during import.
    pub fn known_files(&self, media: &std::path::Path) -> Vec<KnownFile> {
        self.items
            .iter()
            .map(|i| KnownFile { id: i.id, name: i.name.clone(), size: i.size, hash: i.hash.clone(), abs: crate::import::media_file(media, &i.path) })
            .collect()
    }

    /// Adds a finished copy, newest first. Returns its id.
    pub fn add_copied(&mut self, c: &Copied, kind: MediaKind) -> u32 {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        self.items.insert(
            0,
            Item {
                id,
                name: c.name.clone(),
                kind,
                width: c.width,
                height: c.height,
                path: c.rel_path.clone(),
                size: c.size,
                hash: c.hash.clone(),
                added: now_secs(),
                source: Some(c.source.clone()),
            },
        );
        id
    }

    /// Adds a file found in a Media folder without a record (after an interrupted import).
    pub fn adopt(&mut self, o: &Orphan) -> u32 {
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        let mut item = Item::new(id, &o.name, o.kind, o.width, o.height);
        item.path = o.rel_path.clone();
        item.size = o.size;
        item.added = now_secs();
        self.items.insert(0, item);
        id
    }

    /// Keeps hashes computed while planning an import.
    pub fn set_hashes(&mut self, hashes: &[(u32, String)]) {
        for (id, h) in hashes {
            if let Some(i) = self.items.iter_mut().find(|i| i.id == *id) {
                i.hash = h.clone();
            }
        }
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

    /// The relative path item `id` would have after a rename to `base`.
    pub fn renamed_path(&self, id: u32, base: &str) -> Option<String> {
        let item = self.get(id)?;
        let new_name = names::final_name(base, extension(&item.name));
        let folder = item.path.rsplit_once('/').map_or("", |(f, _)| f);
        Some(if folder.is_empty() { new_name } else { format!("{folder}/{new_name}") })
    }

    /// Renames item `id` in the library (the file is renamed first, by the caller),
    /// keeping its extension, id and history. Returns the new full name.
    pub fn rename(&mut self, id: u32, base: &str) -> Result<String, NameError> {
        self.check_rename(id, base)?;
        let path = self.renamed_path(id, base).ok_or(NameError::Empty)?;
        let item = self.items.iter_mut().find(|i| i.id == id).ok_or(NameError::Empty)?;
        let new_name = names::final_name(base, extension(&item.name));
        item.name = new_name.clone();
        item.path = path;
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
    fn rename_keeps_extension_id_folder_and_history() {
        let mut lib = Library::sample();
        assert_eq!(lib.rename(3, "Renamed clip"), Ok("Renamed clip.mp4".into()));
        let item = lib.get(3).unwrap();
        assert_eq!(item.name, "Renamed clip.mp4");
        assert_eq!(item.path, "Videos/Renamed clip.mp4");
        assert!(lib.recent.contains(&3));
    }

    #[test]
    fn rename_uses_the_shared_name_rules() {
        let lib = Library::sample();
        assert_eq!(lib.check_rename(3, "Cat/photo"), Err(NameError::IllegalChar));
        assert_eq!(lib.check_rename(3, "  "), Err(NameError::Empty));
        assert_eq!(lib.check_rename(3, "nul"), Err(NameError::Reserved));
        assert_eq!(lib.check_rename(3, "clip."), Err(NameError::TrailingSpaceOrPeriod));
        assert_eq!(lib.check_rename(3, &"x".repeat(121)), Err(NameError::TooLong));
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
    fn store_round_trip_keeps_everything() {
        let mut lib = Library::sample();
        lib.items[0].hash = "abc".into();
        lib.items[0].source = Some("C:\\src\\a.gif".into());
        let data = StoreData { items: lib.to_stored(), history: lib.recent.clone(), next_id: 9, ..StoreData::default() };
        let back = Library::from_store(&data);
        assert_eq!(back.items, lib.items);
        assert_eq!(back.recent, lib.recent);
        assert_eq!(back.next_id, 9);
    }

    #[test]
    fn imports_get_new_ids_and_go_first() {
        let mut lib = Library::sample();
        let c = Copied { name: "New.png".into(), rel_path: "Pictures/New.png".into(), size: 3, ..Default::default() };
        let id = lib.add_copied(&c, MediaKind::Picture);
        assert_eq!(id, 9);
        assert_eq!(lib.items[0].name, "New.png");
        assert_eq!(lib.next_id, 10);
    }

    #[test]
    fn extension_of_names() {
        assert_eq!(extension("Square.png"), ".png");
        assert_eq!(extension("clip.final.mp4"), ".mp4");
        assert_eq!(extension("no ext"), "");
    }
}
