//! The library file, `Data\pocketmedia.json`.
//!
//! - **Versioned:** `version` says which format the file uses. A file from a newer
//!   PocketMedia is never overwritten: the app runs without saving and says so.
//! - **Safe saves:** the new contents go to `pocketmedia.json.tmp`, are flushed to
//!   disk, and then replace the real file in one rename. A crash at any moment
//!   leaves either the old file or the new one, never half of one.
//! - **Last known good:** before each save, the current file (if it loaded
//!   correctly) is copied to `pocketmedia.json.bak`.
//! - **Recovery:** a file that cannot be read is kept as
//!   `pocketmedia.json.corrupt-<time>`, reported, and the backup is used instead.
//!   Nothing is discarded silently: every repair is listed in the load report.
//! - **Only when changed:** saving identical contents does nothing.
//!
//! No tag data and no decoded images are ever stored.

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::settings_state::Settings;

/// The format this build reads and writes.
pub const VERSION: u32 = 1;
pub const FILE_NAME: &str = "pocketmedia.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredKind {
    Picture,
    Gif,
    Video,
}

/// One media file. `path` is relative to the Media folder, with `/` separators
/// (for example `Pictures/Cat.png`), so the library survives moving the folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredItem {
    pub id: u32,
    pub name: String,
    pub kind: StoredKind,
    pub path: String,
    #[serde(default)]
    pub size: u64,
    /// Content hash as hex; empty until the file has been hashed.
    #[serde(default)]
    pub hash: String,
    /// Pixel size; 0 when not known.
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    /// When it was added (seconds since 1970, UTC).
    #[serde(default)]
    pub added: u64,
    /// Where it was imported from, for information only (the source is never touched).
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreData {
    pub version: u32,
    /// The next id to hand out. Ids are never reused.
    pub next_id: u32,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub items: Vec<StoredItem>,
    /// Recently used item ids, newest first (at most `HISTORY_MAX`).
    #[serde(default)]
    pub history: Vec<u32>,
}

pub const HISTORY_MAX: usize = 20;

impl Default for StoreData {
    fn default() -> Self {
        Self { version: VERSION, next_id: 1, settings: Settings::default(), items: Vec::new(), history: Vec::new() }
    }
}

/// What happened when the store was opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadOutcome {
    /// No file yet (first start).
    New,
    Loaded,
    /// Loaded, with problems fixed; each is described.
    Repaired(Vec<String>),
    /// The main file was missing (a crash between two renames, or deleted); the backup was used.
    MissingUsedBackup,
    /// The main file could not be read; it was kept under `kept` and the backup used.
    RestoredFromBackup { kept: PathBuf, reason: String },
    /// Neither the file nor its backup could be read. Both are kept; the library starts empty.
    Unreadable { kept: Vec<PathBuf>, reason: String },
    /// Written by a newer PocketMedia: nothing is loaded or saved.
    NewerVersion(u32),
}

impl LoadOutcome {
    /// A message for the user, or None when everything is fine.
    pub fn message(&self) -> Option<String> {
        match self {
            LoadOutcome::New | LoadOutcome::Loaded => None,
            LoadOutcome::Repaired(fixes) => Some(format!("The library file had problems that were fixed:\n\n- {}", fixes.join("\n- "))),
            LoadOutcome::MissingUsedBackup => {
                Some("The library file was missing, so PocketMedia used its backup.".to_string())
            }
            LoadOutcome::RestoredFromBackup { kept, reason } => Some(format!(
                "The library file could not be read ({reason}).\n\nPocketMedia used its backup instead. \
                 The unreadable file was kept as\n{}",
                kept.display()
            )),
            LoadOutcome::Unreadable { kept, reason } => Some(format!(
                "The library file and its backup could not be read ({reason}).\n\nPocketMedia starts with an empty \
                 library. Your media files are not touched. The unreadable files were kept as\n{}",
                kept.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n")
            )),
            LoadOutcome::NewerVersion(v) => Some(format!(
                "The library file was written by a newer version of PocketMedia (format {v}; this version reads \
                 format {VERSION}).\n\nPocketMedia will not change it. Changes you make now are not saved."
            )),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            LoadOutcome::New => "new",
            LoadOutcome::Loaded => "loaded",
            LoadOutcome::Repaired(_) => "repaired",
            LoadOutcome::MissingUsedBackup => "missing-used-backup",
            LoadOutcome::RestoredFromBackup { .. } => "restored-from-backup",
            LoadOutcome::Unreadable { .. } => "unreadable",
            LoadOutcome::NewerVersion(_) => "newer-version",
        }
    }
}

pub struct Store {
    dir: PathBuf,
    pub data: StoreData,
    /// The text last written or loaded, to skip saves that change nothing.
    last_saved: Option<String>,
    /// The main file on disk is one this build read or wrote correctly, so it
    /// may become the backup.
    main_good: bool,
    /// A newer format is on disk: never write.
    pub read_only: bool,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Parses and checks a file's text. Returns the data and the repairs made.
fn parse(text: &str) -> Result<(StoreData, Vec<String>), ParseError> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| ParseError::Bad(e.to_string()))?;
    let version = value.get("version").and_then(|v| v.as_u64()).ok_or(ParseError::Bad("no format version".into()))?;
    if version > VERSION as u64 {
        return Err(ParseError::Newer(version as u32));
    }
    let data = migrate(value, version as u32)?;
    Ok(validate(data))
}

/// Reads library file text as a start would (the backup checks what it wrote).
pub fn check_text(text: &str) -> Result<StoreData, String> {
    match parse(text) {
        Ok((data, _)) => Ok(data),
        Err(ParseError::Bad(e)) => Err(e),
        Err(ParseError::Newer(v)) => Err(format!("format version {v} is newer than this PocketMedia")),
    }
}

enum ParseError {
    Bad(String),
    Newer(u32),
}

/// Brings an older format up to `VERSION`. There is only version 1 so far.
fn migrate(value: serde_json::Value, _from: u32) -> Result<StoreData, ParseError> {
    let mut data: StoreData = serde_json::from_value(value).map_err(|e| ParseError::Bad(e.to_string()))?;
    data.version = VERSION;
    Ok(data)
}

/// Fixes what can be fixed without losing anything, and lists each fix.
fn validate(mut data: StoreData) -> (StoreData, Vec<String>) {
    let mut fixes = Vec::new();
    // A record whose path could lead outside the Media folders is
    // left out (the file on disk is not touched; the loaded file is kept as the
    // backup at the next save). A real file in Media is found again at startup.
    data.items.retain(|item| match crate::import::item_path_problem(&item.path) {
        None => true,
        Some(why) => {
            fixes.push(format!("{} was left out of the library: its path \"{}\" is not allowed ({why}). No file was changed", item.name, item.path));
            false
        }
    });
    // Ids stay far below u32::MAX, so handing out new ones can never wrap around.
    let max_id = data.items.iter().map(|i| i.id).max().unwrap_or(0);
    if max_id >= ID_LIMIT || data.next_id >= ID_LIMIT {
        renumber(&mut data);
        fixes.push(format!("the item ids were too large and were numbered again from 1 (next id {})", data.next_id));
    }
    let max_id = data.items.iter().map(|i| i.id).max().unwrap_or(0);
    if data.next_id <= max_id {
        fixes.push(format!("the next id was {} but id {max_id} is in use; it is now {}", data.next_id, max_id + 1));
        data.next_id = max_id + 1;
    }
    let mut seen = HashSet::new();
    for item in &mut data.items {
        if !seen.insert(item.id) {
            let new_id = data.next_id;
            data.next_id += 1; // below ID_LIMIT + number of items: no overflow
            fixes.push(format!("{} shared id {} with another item; it now has id {new_id}", item.name, item.id));
            item.id = new_id;
            seen.insert(new_id);
        }
    }
    let before = data.history.len();
    let mut in_history = HashSet::new();
    data.history.retain(|id| seen.contains(id) && in_history.insert(*id));
    data.history.truncate(HISTORY_MAX);
    if data.history.len() != before {
        fixes.push(format!("{} recent-history entries pointed to missing or repeated items and were dropped", before - data.history.len()));
    }
    (data, fixes)
}

/// Library file writes since start (the self-test counts them).
static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times the library file was written since start.
pub fn writes() -> u64 {
    WRITES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Ids at or above this are renumbered on load (`next_id` must never wrap).
const ID_LIMIT: u32 = u32::MAX / 2;

/// Gives the items ids 1, 2, 3… in their current order. History follows the
/// first item that had each old id.
fn renumber(data: &mut StoreData) {
    let mut map = std::collections::HashMap::new();
    for (n, item) in data.items.iter_mut().enumerate() {
        let new_id = n as u32 + 1; // fewer than ID_LIMIT items
        map.entry(item.id).or_insert(new_id);
        item.id = new_id;
    }
    data.history = data.history.iter().filter_map(|id| map.get(id).copied()).collect();
    data.next_id = data.items.len() as u32 + 1;
}

impl Store {
    pub fn main_path(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }
    pub fn backup_path(&self) -> PathBuf {
        self.dir.join(format!("{FILE_NAME}.bak"))
    }
    pub fn temp_path(&self) -> PathBuf {
        self.dir.join(format!("{FILE_NAME}.tmp"))
    }

    /// Opens the store in `dir` (the Data folder). Never fails: problems are in the outcome.
    pub fn open(dir: &Path) -> (Store, LoadOutcome) {
        let mut store =
            Store { dir: dir.to_path_buf(), data: StoreData::default(), last_saved: None, main_good: false, read_only: false };
        // A leftover temporary file is an unfinished save; the real file is intact.
        let _ = fs::remove_file(store.temp_path());

        let main = store.main_path();
        let main_reason = match fs::read_to_string(&main) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => Some(format!("it could not be opened: {e}")),
            Ok(text) => match parse(&text) {
                Ok((data, fixes)) => {
                    store.data = data;
                    store.main_good = true;
                    // Repairs are not on disk yet, so a repaired store saves on the next change.
                    store.last_saved = fixes.is_empty().then_some(text);
                    return (store, if fixes.is_empty() { LoadOutcome::Loaded } else { LoadOutcome::Repaired(fixes) });
                }
                Err(ParseError::Newer(v)) => {
                    store.read_only = true;
                    return (store, LoadOutcome::NewerVersion(v));
                }
                Err(ParseError::Bad(reason)) => Some(reason),
            },
        };

        // The main file is missing or unreadable: keep it, then try the backup.
        let kept_main = main_reason.as_ref().and_then(|_| store.keep_aside(&main));
        let backup = fs::read_to_string(store.backup_path()).ok().map(|t| (parse(&t), t));
        match (main_reason, backup) {
            (None, None) => (store, LoadOutcome::New),
            (reason, Some((Ok((data, _)), _))) => {
                store.data = data;
                let outcome = match (reason, kept_main) {
                    (Some(reason), Some(kept)) => LoadOutcome::RestoredFromBackup { kept, reason },
                    _ => LoadOutcome::MissingUsedBackup,
                };
                (store, outcome)
            }
            (_, Some((Err(ParseError::Newer(v)), _))) => {
                store.read_only = true;
                (store, LoadOutcome::NewerVersion(v))
            }
            (reason, Some((Err(ParseError::Bad(why)), _))) => {
                let mut kept: Vec<PathBuf> = kept_main.into_iter().collect();
                kept.extend(store.keep_aside(&store.backup_path()));
                let reason = reason.unwrap_or_else(|| why.clone());
                (store, LoadOutcome::Unreadable { kept, reason })
            }
            (Some(reason), None) => (store, LoadOutcome::Unreadable { kept: kept_main.into_iter().collect(), reason }),
        }
    }

    /// Copies an unreadable file to `<name>.corrupt-<time>` and returns that path.
    fn keep_aside(&self, path: &Path) -> Option<PathBuf> {
        let name = path.file_name()?.to_string_lossy().to_string();
        let mut target = self.dir.join(format!("{name}.corrupt-{}", now_secs()));
        let mut n = 2;
        while target.exists() {
            target = self.dir.join(format!("{name}.corrupt-{}-{n}", now_secs()));
            n += 1;
        }
        fs::copy(path, &target).ok()?;
        Some(target)
    }

    /// Writes the store if it changed. Returns Ok(false) when nothing was written.
    pub fn save(&mut self) -> io::Result<bool> {
        if self.read_only {
            return Ok(false);
        }
        let text = serde_json::to_string_pretty(&self.data).map_err(io::Error::other)?;
        if self.last_saved.as_deref() == Some(text.as_str()) {
            return Ok(false);
        }
        // 1. The new contents, flushed to disk.
        let tmp = self.temp_path();
        {
            let mut f = File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
        }
        // 2. The current good file becomes the last-known-good backup.
        let main = self.main_path();
        if self.main_good && main.exists() {
            let bak_tmp = self.dir.join(format!("{FILE_NAME}.bak.tmp"));
            fs::copy(&main, &bak_tmp)?;
            // Flushing needs a handle opened for writing on Windows (FlushFileBuffers).
            OpenOptions::new().write(true).open(&bak_tmp)?.sync_all()?;
            fs::rename(&bak_tmp, self.backup_path())?;
        }
        // 3. One rename replaces the real file (MoveFileEx with replace on Windows).
        fs::rename(&tmp, &main)?;
        self.main_good = true;
        self.last_saved = Some(text);
        WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(true)
    }
}

thread_local! {
    static STORE: RefCell<Option<Store>> = const { RefCell::new(None) };
    static OUTCOME: RefCell<Option<LoadOutcome>> = const { RefCell::new(None) };
}

/// Opens the store for this run (UI thread). Returns the outcome for reporting.
pub fn init(dir: &Path) -> LoadOutcome {
    let (store, outcome) = Store::open(dir);
    STORE.with(|s| *s.borrow_mut() = Some(store));
    OUTCOME.with(|o| *o.borrow_mut() = Some(outcome.clone()));
    outcome
}

/// Runs `f` with the store, if it is open.
pub fn with<R>(f: impl FnOnce(&mut Store) -> R) -> Option<R> {
    STORE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

/// The saved settings (defaults if the store is not open).
pub fn settings() -> Settings {
    with(|s| s.data.settings).unwrap_or_default()
}

/// Stores new settings and saves. Returns a message if saving failed.
pub fn save_settings(settings: Settings) -> Result<(), String> {
    with(|s| {
        s.data.settings = settings;
        if s.read_only {
            return Err("the library file is from a newer PocketMedia, so changes are not saved".to_string());
        }
        s.save().map(|_| ()).map_err(|e| e.to_string())
    })
    .unwrap_or(Ok(()))
}

/// For the self-test: "outcome=… version=… items=… path=…".
pub fn describe() -> String {
    let outcome = OUTCOME.with(|o| o.borrow().as_ref().map(|o| o.label()).unwrap_or("closed"));
    with(|s| {
        format!(
            "outcome={outcome} read_only={} version={} next_id={} items={} history={} path={}",
            s.read_only,
            s.data.version,
            s.data.next_id,
            s.data.items.len(),
            s.data.history.len(),
            s.main_path().display()
        )
    })
    .unwrap_or_else(|| format!("outcome={outcome}"))
}

/// Self-test only: an empty store with no files, as on a first start.
pub fn reset_for_test() {
    with(|s| {
        for p in [s.main_path(), s.backup_path(), s.temp_path()] {
            let _ = fs::remove_file(p);
        }
        s.data = StoreData::default();
        s.last_saved = None;
        s.main_good = false;
        s.read_only = false;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_state::Profile;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pm-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn item(id: u32, name: &str) -> StoredItem {
        StoredItem {
            id,
            name: name.into(),
            kind: StoredKind::Picture,
            path: format!("Pictures/{name}"),
            size: 3,
            hash: "abc".into(),
            width: 10,
            height: 20,
            added: 1,
            source: None,
        }
    }

    #[test]
    fn first_start_is_new_and_the_first_save_creates_the_file() {
        let dir = temp("new");
        let (mut s, outcome) = Store::open(&dir);
        assert_eq!(outcome, LoadOutcome::New);
        assert!(s.save().unwrap());
        let text = fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(text.contains("\"version\": 1"));
        assert!(!dir.join("pocketmedia.json.tmp").exists());
        assert!(!dir.join("pocketmedia.json.bak").exists(), "no backup before there is a good file");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_data_comes_back_after_a_restart() {
        let dir = temp("roundtrip");
        let (mut s, _) = Store::open(&dir);
        s.data.items.push(item(1, "Cat.png"));
        s.data.next_id = 2;
        s.data.history = vec![1];
        s.data.settings.profile = Profile::OpenFaster;
        s.data.settings.close_after_insert = false;
        s.save().unwrap();
        let (s2, outcome) = Store::open(&dir);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(s2.data, s.data);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unchanged_data_is_not_written_again() {
        let dir = temp("unchanged");
        let (mut s, _) = Store::open(&dir);
        assert!(s.save().unwrap());
        assert!(!s.save().unwrap());
        let (mut s2, _) = Store::open(&dir);
        assert!(!s2.save().unwrap(), "a freshly loaded store has nothing to save");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_save_keeps_the_previous_good_file_as_backup() {
        let dir = temp("backup");
        let (mut s, _) = Store::open(&dir);
        s.data.items.push(item(1, "A.png"));
        s.data.next_id = 2;
        s.save().unwrap();
        let first = fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        s.data.items.push(item(2, "B.png"));
        s.data.next_id = 3;
        s.save().unwrap();
        assert_eq!(fs::read_to_string(dir.join("pocketmedia.json.bak")).unwrap(), first);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_file_is_kept_and_the_backup_is_used() {
        let dir = temp("corrupt");
        let (mut s, _) = Store::open(&dir);
        s.data.items.push(item(1, "A.png"));
        s.data.next_id = 2;
        s.save().unwrap();
        s.data.items.push(item(2, "B.png"));
        s.data.next_id = 3;
        s.save().unwrap(); // backup now holds A only
        fs::write(dir.join(FILE_NAME), "{\"version\": 1, \"items\": [tru").unwrap();
        let (mut s2, outcome) = Store::open(&dir);
        let LoadOutcome::RestoredFromBackup { kept, .. } = &outcome else { panic!("{outcome:?}") };
        assert_eq!(fs::read_to_string(kept).unwrap(), "{\"version\": 1, \"items\": [tru", "the bad file is kept as it was");
        assert_eq!(s2.data.items.len(), 1);
        assert!(outcome.message().unwrap().contains("backup"));
        // Saving now must not turn the corrupt file into the backup.
        s2.data.next_id = 5;
        s2.save().unwrap();
        let bak = fs::read_to_string(dir.join("pocketmedia.json.bak")).unwrap();
        assert!(bak.contains("A.png"), "the backup is still the last good file");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn both_files_unreadable_starts_empty_and_keeps_both() {
        let dir = temp("both");
        fs::write(dir.join(FILE_NAME), "garbage").unwrap();
        fs::write(dir.join("pocketmedia.json.bak"), "also garbage").unwrap();
        let (s, outcome) = Store::open(&dir);
        let LoadOutcome::Unreadable { kept, .. } = &outcome else { panic!("{outcome:?}") };
        assert_eq!(kept.len(), 2);
        assert!(s.data.items.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crash_during_save_leaves_the_old_file_intact() {
        let dir = temp("crash");
        let (mut s, _) = Store::open(&dir);
        s.data.items.push(item(1, "A.png"));
        s.data.next_id = 2;
        s.save().unwrap();
        // A save that died before its rename: half a temporary file.
        fs::write(dir.join("pocketmedia.json.tmp"), "{\"version\": 1, \"ite").unwrap();
        let (s2, outcome) = Store::open(&dir);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(s2.data.items.len(), 1);
        assert!(!dir.join("pocketmedia.json.tmp").exists(), "the unfinished save is cleaned up");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newer_format_is_never_overwritten() {
        let dir = temp("newer");
        let newer = "{\"version\": 99, \"whatever\": true}";
        fs::write(dir.join(FILE_NAME), newer).unwrap();
        let (mut s, outcome) = Store::open(&dir);
        assert_eq!(outcome, LoadOutcome::NewerVersion(99));
        s.data.next_id = 7;
        assert!(!s.save().unwrap());
        assert_eq!(fs::read_to_string(dir.join(FILE_NAME)).unwrap(), newer);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn problems_are_repaired_and_reported_not_dropped_silently() {
        let dir = temp("repair");
        let text = r#"{"version":1,"next_id":1,"items":[
            {"id":1,"name":"A.png","kind":"picture","path":"Pictures/A.png"},
            {"id":1,"name":"B.gif","kind":"gif","path":"GIFs/B.gif"}],
            "history":[1,1,9]}"#;
        fs::write(dir.join(FILE_NAME), text).unwrap();
        let (s, outcome) = Store::open(&dir);
        let LoadOutcome::Repaired(fixes) = &outcome else { panic!("{outcome:?}") };
        assert_eq!(s.data.items.len(), 2, "both items kept");
        assert_ne!(s.data.items[0].id, s.data.items[1].id);
        assert!(s.data.next_id > s.data.items.iter().map(|i| i.id).max().unwrap());
        assert_eq!(s.data.history, vec![1]);
        assert_eq!(fixes.len(), 3, "{fixes:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// A record whose path could lead outside the Media folders never
    /// gets into the library; each one is reported, and the file as it was becomes
    /// the backup at the next save.
    #[test]
    fn paths_outside_the_media_folders_are_left_out_and_reported() {
        let dir = temp("paths");
        let text = r#"{"version":1,"next_id":9,"items":[
            {"id":1,"name":"ok.png","kind":"picture","path":"Pictures/ok.png"},
            {"id":2,"name":"x.png","kind":"picture","path":"../x.png"},
            {"id":3,"name":"x.png","kind":"picture","path":"C:/x.png"},
            {"id":4,"name":"x.png","kind":"picture","path":"Pictures\\..\\..\\x.png"},
            {"id":5,"name":"b.png","kind":"picture","path":"Pictures/a/b.png"},
            {"id":6,"name":"x.png","kind":"picture","path":"/x.png"},
            {"id":7,"name":"x.png","kind":"picture","path":"Pictures/.."},
            {"id":8,"name":"x.png","kind":"picture","path":"Pictures/C:x.png"}],
            "history":[2,1,5]}"#;
        fs::write(dir.join(FILE_NAME), text).unwrap();
        let (mut s, outcome) = Store::open(&dir);
        let LoadOutcome::Repaired(fixes) = &outcome else { panic!("{outcome:?}") };
        assert_eq!(s.data.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1]);
        assert_eq!(s.data.history, vec![1]);
        assert_eq!(fixes.iter().filter(|f| f.contains("was left out of the library")).count(), 7, "{fixes:?}");
        // The next save keeps the file as it was as the backup: nothing is lost for good.
        s.data.settings.close_after_insert = !s.data.settings.close_after_insert;
        assert!(s.save().unwrap());
        assert_eq!(fs::read_to_string(dir.join("pocketmedia.json.bak")).unwrap(), text);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Ids near u32::MAX are numbered again, so a new id can never wrap
    /// around to one already in use.
    #[test]
    fn huge_ids_are_numbered_again_and_never_wrap() {
        let dir = temp("ids");
        let text = r#"{"version":1,"next_id":4294967295,"items":[
            {"id":4294967295,"name":"A.png","kind":"picture","path":"Pictures/A.png"},
            {"id":7,"name":"B.png","kind":"picture","path":"Pictures/B.png"}],
            "history":[4294967295]}"#;
        fs::write(dir.join(FILE_NAME), text).unwrap();
        let (s, outcome) = Store::open(&dir);
        assert!(matches!(outcome, LoadOutcome::Repaired(_)), "{outcome:?}");
        assert_eq!(s.data.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(s.data.history, vec![1], "history follows its item");
        assert_eq!(s.data.next_id, 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_settings_fall_back_to_the_defaults() {
        let dir = temp("defaults");
        fs::write(dir.join(FILE_NAME), r#"{"version":1,"next_id":1}"#).unwrap();
        let (s, outcome) = Store::open(&dir);
        assert_eq!(outcome, LoadOutcome::Loaded);
        assert_eq!(s.data.settings, Settings::default(), "a missing profile means Use less memory");
        let _ = fs::remove_dir_all(&dir);
    }
}
