//! The one in-memory library shared by every window (the popup, the Manager and
//! Add media), so a rename, removal or import in one shows in the others. It is
//! loaded from the store at startup and written back with `persist` after every
//! change.

use std::cell::RefCell;

use crate::import::{Copied, KnownFile, Orphan};
use crate::media_info;
use crate::watch::Batch;
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
            .filter_map(|i| {
                let abs = crate::import::media_file(media, &i.path)?;
                Some(KnownFile { id: i.id, name: i.name.clone(), size: i.size, hash: i.hash.clone(), abs })
            })
            .collect()
    }

    /// The record of the file at `rel` (letter case ignored, as Windows does).
    pub fn index_of_path(&self, rel: &str) -> Option<usize> {
        let rel = rel.to_lowercase();
        self.items.iter().position(|i| i.path.to_lowercase() == rel)
    }

    /// Adds a finished copy, newest first. Returns its id. If the folder watcher
    /// already added the file, that record takes the copy's details and moves first.
    pub fn add_copied(&mut self, c: &Copied, kind: MediaKind) -> u32 {
        if let Some(i) = self.index_of_path(&c.rel_path) {
            let mut item = self.items.remove(i);
            item.name = c.name.clone();
            item.kind = kind;
            (item.width, item.height) = (c.width, c.height);
            item.path = c.rel_path.clone();
            item.size = c.size;
            item.hash = c.hash.clone();
            item.source = Some(c.source.clone());
            let id = item.id;
            self.items.insert(0, item);
            return id;
        }
        let id = self.next_id.max(1);
        self.next_id = id.saturating_add(1);
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

    /// Adds a file found in a Media folder without a record (after an interrupted
    /// import, or put there outside PocketMedia). A file that has a record keeps it.
    pub fn adopt(&mut self, o: &Orphan) -> u32 {
        if let Some(i) = self.index_of_path(&o.rel_path) {
            return self.items[i].id;
        }
        let id = self.next_id.max(1);
        self.next_id = id.saturating_add(1);
        let mut item = Item::new(id, &o.name, o.kind, o.width, o.height);
        item.path = o.rel_path.clone();
        item.size = o.size;
        item.added = now_secs();
        self.items.insert(0, item);
        id
    }

    /// Applies changes made in the Media folders outside PocketMedia (`watch.rs`).
    /// Renames keep the item's id and history; a file that is gone, or now has an
    /// unsupported extension, leaves the library; a new supported file joins it;
    /// a replaced file gets its new size and pixel size. Changes PocketMedia made
    /// itself find the library already up to date and change nothing.
    pub fn apply_watch(&mut self, b: &Batch) -> Applied {
        let mut a = Applied::default();
        for (old, new) in &b.renames {
            let Some(i) = self.index_of_path(old) else { continue };
            if self.index_of_path(new).is_some_and(|j| j != i) {
                continue; // another record owns the new name; the old one goes below
            }
            let item = &mut self.items[i];
            item.name = file_name(new).to_string();
            item.path = new.clone();
            if let Some(kind) = media_info::kind_for(&item.name) {
                item.kind = kind;
            }
            a.renamed.push(item.id);
        }
        for (rel, info) in &b.files {
            let kind = media_info::kind_for(file_name(rel));
            match (self.index_of_path(rel), info, kind) {
                (Some(i), None, _) | (Some(i), Some(_), None) => {
                    let id = self.items[i].id;
                    self.remove(id);
                    a.removed.push(id);
                }
                (Some(i), Some(f), Some(_)) => {
                    let written = b.written.iter().any(|w| w.eq_ignore_ascii_case(rel));
                    let item = &mut self.items[i];
                    if written || item.size != f.size || (item.width, item.height) != (f.width, f.height) {
                        item.hash.clear(); // made again by the next import that needs it
                        item.size = f.size;
                        (item.width, item.height) = (f.width, f.height);
                        a.replaced.push(item.id);
                    }
                }
                (None, Some(f), Some(kind)) => {
                    let o = Orphan { rel_path: rel.clone(), name: file_name(rel).to_string(), kind, size: f.size, width: f.width, height: f.height };
                    a.added.push(self.adopt(&o));
                }
                (None, _, _) => {}
            }
        }
        a
    }

    /// Applies a full comparison of the folders (`import::reconcile`, run on a
    /// worker from a snapshot of `(id, path)`). A record that changed meanwhile is
    /// left alone; a file that got a record meanwhile is not added twice.
    pub fn apply_rescan(&mut self, orphans: &[Orphan], missing: &[(u32, String)]) -> Applied {
        let mut a = Applied::default();
        for (id, path) in missing {
            if self.get(*id).is_some_and(|i| i.path == *path) {
                self.remove(*id);
                a.removed.push(*id);
            }
        }
        for o in orphans {
            if self.index_of_path(&o.rel_path).is_none() {
                a.added.push(self.adopt(o));
            }
        }
        a
    }

    /// Keeps hashes computed while planning an import.
    pub fn set_hashes(&mut self, hashes: &[(u32, String)]) {
        for (id, h) in hashes {
            if let Some(i) = self.items.iter_mut().find(|i| i.id == *id) {
                i.hash = h.clone();
            }
        }
    }

    /// Records that an item was used (a paste was started, or a copy-drag
    /// finished): newest first, 20 distinct items at most.
    pub fn record_use(&mut self, id: u32) {
        self.recent.retain(|r| *r != id);
        self.recent.insert(0, id);
        self.recent.truncate(crate::store::HISTORY_MAX);
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

/// What a change in the Media folders did to the library (item ids).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied {
    pub added: Vec<u32>,
    pub renamed: Vec<u32>,
    pub replaced: Vec<u32>,
    pub removed: Vec<u32>,
}

impl Applied {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.renamed.is_empty() && self.replaced.is_empty() && self.removed.is_empty()
    }

    pub fn merge(&mut self, other: Applied) {
        self.added.extend(other.added);
        self.renamed.extend(other.renamed);
        self.replaced.extend(other.replaced);
        self.removed.extend(other.removed);
    }
}

/// `Cat.png` for `Pictures/Cat.png`.
fn file_name(rel: &str) -> &str {
    rel.rsplit_once('/').map_or(rel, |(_, n)| n)
}

/// `.png` for `Square.png`; empty when there is no extension.
pub fn extension(file_name: &str) -> &str {
    &file_name[name_without_extension(file_name).len()..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::FileInfo;

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
    fn history_keeps_20_distinct_items_newest_first() {
        let mut lib = Library::sample();
        lib.record_use(5);
        assert_eq!(lib.recent[..3], [5, 1, 2]);
        assert_eq!(lib.recent.iter().filter(|r| **r == 5).count(), 1);
        for id in 100..130 {
            lib.record_use(id);
        }
        assert_eq!(lib.recent.len(), 20);
        assert_eq!(lib.recent[0], 129);
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

    fn info(size: u64, w: u32, h: u32) -> Option<FileInfo> {
        Some(FileInfo { size, width: w, height: h })
    }

    fn sample_with_sizes() -> Library {
        let mut lib = Library::sample();
        for i in &mut lib.items {
            i.size = 1000 + i.id as u64;
            i.hash = format!("h{}", i.id);
        }
        lib
    }

    #[test]
    fn an_outside_rename_keeps_id_and_history() {
        let mut lib = sample_with_sizes();
        let b = Batch {
            renames: vec![("Pictures/Square.png".into(), "Pictures/Box.png".into())],
            files: vec![("Pictures/Square.png".into(), None), ("Pictures/Box.png".into(), info(1002, 300, 300))],
            ..Default::default()
        };
        let a = lib.apply_watch(&b);
        assert_eq!(a, Applied { renamed: vec![2], ..Default::default() });
        let item = lib.get(2).unwrap();
        assert_eq!((item.name.as_str(), item.path.as_str(), item.hash.as_str()), ("Box.png", "Pictures/Box.png", "h2"));
        assert!(lib.recent.contains(&2));
        assert_eq!(lib.items.len(), 8);
    }

    #[test]
    fn renames_in_a_row_follow_the_same_item() {
        let mut lib = sample_with_sizes();
        let b = Batch {
            renames: vec![
                ("Pictures/Square.png".into(), "Pictures/tmp.png".into()),
                ("Pictures/tmp.png".into(), "Pictures/Final.png".into()),
            ],
            files: vec![
                ("Pictures/Square.png".into(), None),
                ("Pictures/tmp.png".into(), None),
                ("Pictures/Final.png".into(), info(1002, 300, 300)),
            ],
            ..Default::default()
        };
        lib.apply_watch(&b);
        assert_eq!(lib.get(2).unwrap().path, "Pictures/Final.png");
        assert_eq!(lib.items.len(), 8);
    }

    #[test]
    fn a_case_only_rename_is_a_rename() {
        let mut lib = sample_with_sizes();
        let b = Batch {
            renames: vec![("Pictures/Square.png".into(), "Pictures/SQUARE.png".into())],
            files: vec![("Pictures/Square.png".into(), info(1002, 300, 300)), ("Pictures/SQUARE.png".into(), info(1002, 300, 300))],
            ..Default::default()
        };
        let a = lib.apply_watch(&b);
        assert_eq!(lib.get(2).unwrap().name, "SQUARE.png");
        assert_eq!((a.renamed, a.removed, a.added), (vec![2], vec![], vec![]));
    }

    #[test]
    fn renaming_to_an_unsupported_extension_removes_the_item() {
        let mut lib = sample_with_sizes();
        let b = Batch {
            renames: vec![("Pictures/Square.png".into(), "Pictures/Square.txt".into())],
            files: vec![("Pictures/Square.png".into(), None), ("Pictures/Square.txt".into(), info(1002, 0, 0))],
            ..Default::default()
        };
        let a = lib.apply_watch(&b);
        assert!(lib.get(2).is_none());
        assert_eq!(a.removed, vec![2]);
        assert!(!lib.recent.contains(&2));
    }

    #[test]
    fn deleted_files_leave_and_new_files_join() {
        let mut lib = sample_with_sizes();
        let b = Batch {
            files: vec![
                ("Videos/Race clip.mp4".into(), None),
                ("GIFs/New one.gif".into(), info(77, 10, 20)),
                ("GIFs/notes.txt".into(), info(5, 0, 0)),
                ("GIFs/gone before we looked.gif".into(), None),
            ],
            ..Default::default()
        };
        let a = lib.apply_watch(&b);
        assert_eq!(a.removed, vec![3]);
        assert_eq!(a.added, vec![9]);
        let new = lib.get(9).unwrap();
        assert_eq!((new.name.as_str(), new.kind, new.size, new.width, new.height), ("New one.gif", MediaKind::Gif, 77, 10, 20));
        assert_eq!(lib.items[0].id, 9, "a new file shows first");
        assert_eq!(lib.items.len(), 8);
    }

    #[test]
    fn a_replaced_file_gets_its_new_size_and_loses_its_hash() {
        let mut lib = sample_with_sizes();
        let b = Batch { files: vec![("Pictures/Square.png".into(), info(5000, 64, 64))], ..Default::default() };
        let a = lib.apply_watch(&b);
        assert_eq!(a.replaced, vec![2]);
        let item = lib.get(2).unwrap();
        assert_eq!((item.size, item.width, item.height, item.hash.as_str()), (5000, 64, 64, ""));
        // Same size, but its content was written: the hash is no longer trusted.
        let b = Batch { files: vec![("GIFs/Hello.gif".into(), info(1004, 300, 300))], written: vec!["GIFs/hello.gif".into()], ..Default::default() };
        assert_eq!(lib.apply_watch(&b).replaced, vec![4]);
        assert_eq!(lib.get(4).unwrap().hash, "");
    }

    #[test]
    fn pocketmedias_own_changes_change_nothing() {
        let mut lib = sample_with_sizes();
        // The Manager already renamed item 2 and recycled item 3.
        lib.rename(2, "Box").unwrap();
        lib.remove(3);
        let before = lib.to_stored();
        let b = Batch {
            renames: vec![("Pictures/Square.png".into(), "Pictures/Box.png".into())],
            files: vec![
                ("Pictures/Square.png".into(), None),
                ("Pictures/Box.png".into(), info(1002, 300, 300)),
                ("Videos/Race clip.mp4".into(), None),
            ],
            ..Default::default()
        };
        assert!(lib.apply_watch(&b).is_empty());
        assert_eq!(lib.to_stored(), before);
    }

    #[test]
    fn an_import_and_the_watcher_never_make_two_records() {
        let c = Copied { name: "New.png".into(), rel_path: "Pictures/New.png".into(), size: 3, hash: "abc".into(), source: "C:\\src\\New.png".into(), width: 1, height: 1, renamed: false };
        // The watcher saw the file first.
        let mut lib = Library::sample();
        let b = Batch { files: vec![("Pictures/New.png".into(), info(3, 1, 1))], ..Default::default() };
        let seen = lib.apply_watch(&b).added[0];
        lib.items.rotate_left(1); // something else became first meanwhile
        assert_eq!(lib.add_copied(&c, MediaKind::Picture), seen);
        assert_eq!(lib.items.len(), 9);
        assert_eq!(lib.items[0].id, seen, "the import's file shows first");
        assert_eq!(lib.items[0].hash, "abc");
        assert_eq!(lib.items[0].source.as_deref(), Some("C:\\src\\New.png"));
        // The import's record came first.
        let mut lib = Library::sample();
        let id = lib.add_copied(&c, MediaKind::Picture);
        assert!(lib.apply_watch(&b).is_empty());
        assert_eq!((lib.items.len(), lib.items[0].id), (9, id));
    }

    #[test]
    fn a_full_comparison_skips_records_that_changed_meanwhile() {
        let mut lib = sample_with_sizes();
        let orphan = |rel: &str| Orphan { rel_path: rel.into(), name: file_name(rel).into(), kind: MediaKind::Picture, size: 1, width: 1, height: 1 };
        // Snapshot: items 2 and 3 were missing. Item 2 was renamed since.
        lib.rename(2, "Box").unwrap();
        let a = lib.apply_rescan(
            &[orphan("Pictures/Found.png"), orphan("Pictures/box.png")],
            &[(2, "Pictures/Square.png".into()), (3, "Videos/Race clip.mp4".into())],
        );
        assert_eq!(a.removed, vec![3]);
        assert_eq!(a.added.len(), 1);
        assert!(lib.get(2).is_some());
        assert_eq!(lib.items.iter().filter(|i| i.path.eq_ignore_ascii_case("Pictures/Box.png")).count(), 1);
    }

    #[test]
    fn extension_of_names() {
        assert_eq!(extension("Square.png"), ".png");
        assert_eq!(extension("clip.final.mp4"), ".mp4");
        assert_eq!(extension("no ext"), "");
    }
}
