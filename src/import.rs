//! The one import pipeline, used by every entry point. Disk work
//! only: it runs on a worker thread (`jobs.rs`), checks `cancel` often, and
//! never writes to a source.
//!
//! 1. **Scan** the chosen files, or a folder and all its subfolders (links and
//!    junctions are not followed; the Media folder itself is skipped).
//! 2. **Plan**: unsupported files are listed and skipped; identical content is
//!    skipped (size first, then a hash only when sizes match), against the
//!    library and earlier files in the same batch; a name that is taken gets
//!    ` (2)`, ` (3)`… before the extension.
//! 3. **Copy** each file to a temporary `.pocketmedia-….partial` file beside its
//!    destination, flush it, then publish it under its final name with a move
//!    that never replaces an existing file. A half-written copy is never visible
//!    under a real name, and `cleanup_partials` removes leftovers on startup.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::digest::{Digest, NewDigest};
use crate::media_info;
use crate::sample::MediaKind;
use crate::search::name_without_extension;

pub const PARTIAL_PREFIX: &str = ".pocketmedia-";
pub const PARTIAL_SUFFIX: &str = ".partial";
const CHUNK: usize = 1 << 20;
/// Highest number tried for "name (n)".
const MAX_NUMBER: u32 = 9999;

pub const FOLDERS: [&str; 3] = ["Pictures", "GIFs", "Videos"];

/// Moves `from` to `to`, failing with `AlreadyExists` if `to` exists.
pub type Publish = fn(&Path, &Path) -> io::Result<()>;

/// What the user chose.
#[derive(Clone, Debug)]
pub enum Source {
    Files(Vec<PathBuf>),
    Folder(PathBuf),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub source: PathBuf,
    /// File name with extension.
    pub name: String,
    pub size: u64,
    /// None: not a supported format.
    pub kind: Option<MediaKind>,
    /// Why it could not be read while the files were listed: it
    /// becomes a skipped row with this reason instead of stopping the import.
    pub problem: Option<String>,
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) { Err("Cancelled.".into()) } else { Ok(()) }
}

pub fn is_partial(name: &str) -> bool {
    name.starts_with(PARTIAL_PREFIX) && name.ends_with(PARTIAL_SUFFIX)
}

fn candidate(path: &Path, size: u64) -> Candidate {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Candidate { source: path.to_path_buf(), kind: media_info::kind_for(&name), name, size, problem: None }
}

/// A file or folder that could not be read while listing.
fn unreadable(path: &Path, why: String) -> Candidate {
    Candidate { problem: Some(why), ..candidate(path, 0) }
}

/// Lists what the user chose. `media` is PocketMedia's own Media folder, which
/// is never imported from.
pub fn scan(source: &Source, media: &Path, cancel: &AtomicBool) -> Result<Vec<Candidate>, String> {
    let media_real = fs::canonicalize(media).unwrap_or_else(|_| media.to_path_buf());
    match source {
        Source::Files(files) => {
            let mut out = Vec::new();
            for f in files {
                cancelled(cancel)?;
                match fs::metadata(f) {
                    Ok(m) if m.is_file() => out.push(candidate(f, m.len())),
                    Ok(_) => {}
                    Err(e) => out.push(unreadable(f, format!("{} could not be read: {e}", display_path(f)))),
                }
            }
            Ok(out)
        }
        Source::Folder(root) => {
            let root_real = fs::canonicalize(root).map_err(|e| format!("{} could not be opened: {e}", root.display()))?;
            if root_real.starts_with(&media_real) {
                return Err("That folder is inside PocketMedia's own Media folder. Choose a folder outside it.".into());
            }
            let mut out = Vec::new();
            let mut stack = vec![root_real];
            while let Some(dir) = stack.pop() {
                cancelled(cancel)?;
                // A subfolder or file that cannot be read is listed with the reason,
                // so the result never looks complete when it is not.
                let entries = match fs::read_dir(&dir) {
                    Ok(entries) => entries,
                    Err(e) => {
                        out.push(unreadable(&dir, format!("the folder {} could not be opened: {e}", display_path(&dir))));
                        continue;
                    }
                };
                let mut files = Vec::new();
                let mut dirs = Vec::new();
                for e in entries {
                    let e = match e {
                        Ok(e) => e,
                        Err(err) => {
                            out.push(unreadable(&dir, format!("part of the folder {} could not be read: {err}", display_path(&dir))));
                            continue;
                        }
                    };
                    // symlink_metadata does not follow links; on Windows, junctions count as links too.
                    let m = match fs::symlink_metadata(e.path()) {
                        Ok(m) => m,
                        Err(err) => {
                            out.push(unreadable(&e.path(), format!("{} could not be read: {err}", display_path(&e.path()))));
                            continue;
                        }
                    };
                    if m.file_type().is_symlink() {
                        continue;
                    }
                    if m.is_dir() {
                        let real = fs::canonicalize(e.path()).unwrap_or_else(|_| e.path());
                        if real != media_real {
                            dirs.push(e.path());
                        }
                    } else if m.is_file() && !is_partial(&e.file_name().to_string_lossy()) {
                        files.push((e.path(), m.len()));
                    }
                }
                // Files of a folder first, by name; then its subfolders, by name.
                files.sort_by_key(|(p, _)| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
                out.extend(files.into_iter().map(|(p, size)| candidate(&p, size)));
                dirs.sort_by_key(|p| std::cmp::Reverse(p.file_name().map(|n| n.to_string_lossy().to_lowercase())));
                stack.extend(dirs);
            }
            Ok(out)
        }
    }
}

/// Hashes a whole file.
pub fn hash_file(path: &Path, digest: NewDigest, cancel: &AtomicBool) -> Result<String, String> {
    let mut d = digest()?;
    let mut f = File::open(path).map_err(|e| format!("{} could not be read: {e}", display_path(path)))?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        cancelled(cancel)?;
        let n = f.read(&mut buf).map_err(|e| format!("{} could not be read: {e}", display_path(path)))?;
        if n == 0 {
            return Ok(d.finish());
        }
        d.update(&buf[..n]);
    }
}

/// A file already in the library, for duplicate and name checks.
#[derive(Clone, Debug)]
pub struct KnownFile {
    pub id: u32,
    pub name: String,
    pub size: u64,
    /// Empty when not hashed yet.
    pub hash: String,
    pub abs: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Copy,
    /// Identical content is already in the library (its name).
    InLibrary(String),
    /// Identical to an earlier file in this batch (its name).
    InBatch(String),
    Unsupported,
    /// The file could not be read; the reason.
    Unreadable(String),
}

#[derive(Clone, Debug)]
pub struct Planned {
    pub cand: Candidate,
    pub status: Status,
    /// Suggested name without the extension.
    pub base: String,
    /// The name was taken, so a number was added.
    pub auto_renamed: bool,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub rows: Vec<Planned>,
    /// Library files hashed while planning, so the hash is kept for next time.
    pub library_hashes: Vec<(u32, String)>,
}

/// The extension of a file name, with its dot (`.png`).
pub fn ext_of(name: &str) -> &str {
    &name[name_without_extension(name).len()..]
}

/// `base`, or `base (2)`, `base (3)`… whichever is not taken (in `taken`,
/// lowercase full names) and not already a file in `dir`.
pub fn unique_base(base: &str, ext: &str, taken: &HashSet<String>, dir: &Path) -> (String, bool) {
    let free = |b: &str| {
        let full = format!("{b}{ext}");
        !taken.contains(&full.to_lowercase()) && !dir.join(&full).exists()
    };
    if free(base) {
        return (base.to_string(), false);
    }
    for n in 2..=MAX_NUMBER {
        let b = format!("{base} ({n})");
        if free(&b) {
            return (b, true);
        }
    }
    (format!("{base} ({})", MAX_NUMBER + 1), true)
}

/// Hashes `path` into `slot` unless that was done already. `Ok(Err(why))` when
/// the file cannot be read; `Err` only when the user cancelled.
fn hash_into(slot: &mut String, path: &Path, digest: NewDigest, cancel: &AtomicBool) -> Result<Result<(), String>, String> {
    if slot.is_empty() {
        match hash_file(path, digest, cancel) {
            Ok(h) => *slot = h,
            Err(why) => {
                cancelled(cancel)?;
                return Ok(Err(why));
            }
        }
    }
    Ok(Ok(()))
}

/// Decides what happens to each file. Hashes only files whose size matches
/// another file's.
pub fn plan(cands: Vec<Candidate>, known: &[KnownFile], media: &Path, digest: NewDigest, cancel: &AtomicBool) -> Result<Plan, String> {
    let mut plan = Plan::default();
    let mut lib_by_size: HashMap<u64, Vec<usize>> = HashMap::new();
    for (i, k) in known.iter().enumerate() {
        lib_by_size.entry(k.size).or_default().push(i);
    }
    let mut lib_hash: Vec<String> = known.iter().map(|k| k.hash.clone()).collect();
    let mut taken: HashSet<String> = known.iter().map(|k| k.name.to_lowercase()).collect();
    // Earlier files of this batch that will be copied: (size, name, source, hash once known).
    let mut batch: Vec<(u64, String, PathBuf, String)> = Vec::new();

    for cand in cands {
        cancelled(cancel)?;
        if let Some(why) = &cand.problem {
            let status = Status::Unreadable(why.clone());
            let base = name_without_extension(&cand.name).to_string();
            plan.rows.push(Planned { cand, status, base, auto_renamed: false });
            continue;
        }
        let Some(kind) = cand.kind else {
            let base = name_without_extension(&cand.name).to_string();
            plan.rows.push(Planned { cand, status: Status::Unsupported, base, auto_renamed: false });
            continue;
        };
        // Only Cancel stops the plan; a file that cannot be read becomes a row
        // with the reason.
        let mut own_hash = String::new();
        let mut status = Status::Copy;
        'compare: {
            for &i in lib_by_size.get(&cand.size).map(Vec::as_slice).unwrap_or(&[]) {
                if lib_hash[i].is_empty() {
                    let Ok(h) = hash_file(&known[i].abs, digest, cancel) else {
                        cancelled(cancel)?;
                        continue; // a library file that cannot be read is not a duplicate
                    };
                    plan.library_hashes.push((known[i].id, h.clone()));
                    lib_hash[i] = h;
                }
                if let Err(why) = hash_into(&mut own_hash, &cand.source, digest, cancel)? {
                    status = Status::Unreadable(why);
                    break 'compare;
                }
                if own_hash == lib_hash[i] {
                    status = Status::InLibrary(known[i].name.clone());
                    break 'compare;
                }
            }
            for earlier in batch.iter_mut().filter(|b| b.0 == cand.size) {
                // An earlier file that cannot be read now is not a match; its own
                // copy reports the problem.
                if hash_into(&mut earlier.3, &earlier.2, digest, cancel)?.is_err() {
                    continue;
                }
                if let Err(why) = hash_into(&mut own_hash, &cand.source, digest, cancel)? {
                    status = Status::Unreadable(why);
                    break 'compare;
                }
                if own_hash == earlier.3 {
                    status = Status::InBatch(earlier.1.clone());
                    break 'compare;
                }
            }
        }
        let stem = name_without_extension(&cand.name).trim().to_string();
        let ext = ext_of(&cand.name).to_string();
        let (base, auto_renamed) = if status == Status::Copy {
            let (b, renamed) = unique_base(&stem, &ext, &taken, &media.join(kind.folder()));
            taken.insert(format!("{b}{ext}").to_lowercase());
            batch.push((cand.size, cand.name.clone(), cand.source.clone(), own_hash.clone()));
            (b, renamed)
        } else {
            (stem, false)
        };
        plan.rows.push(Planned { cand, status, base, auto_renamed });
    }
    Ok(plan)
}

/// One file to copy, with the name the user approved.
#[derive(Clone, Debug)]
pub struct CopyRequest {
    pub source: PathBuf,
    pub kind: MediaKind,
    /// Name without extension, already validated.
    pub base: String,
    /// Extension with its dot, kept from the source.
    pub ext: String,
}

/// A finished copy.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Copied {
    pub name: String,
    /// Relative to Media: `Pictures/Cat.png`.
    pub rel_path: String,
    pub size: u64,
    pub hash: String,
    pub width: u32,
    pub height: u32,
    /// A file with the approved name appeared meanwhile, so a number was added.
    pub renamed: bool,
    pub source: String,
}

static PARTIAL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Copies one file into the Media folder. Never writes to the source, never
/// replaces a file, and leaves no partial file behind on failure or cancel.
pub fn copy_one(req: &CopyRequest, media: &Path, digest: NewDigest, publish: Publish, cancel: &AtomicBool) -> Result<Copied, String> {
    let folder = req.kind.folder();
    let dir = media.join(folder);
    fs::create_dir_all(&dir).map_err(|e| format!("{} could not be created: {e}", dir.display()))?;
    let n = PARTIAL_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = dir.join(format!("{PARTIAL_PREFIX}{}-{n}{PARTIAL_SUFFIX}", std::process::id()));
    let result = write_partial(req, &tmp, digest, cancel).and_then(|(size, hash)| {
        let mut base = req.base.clone();
        let mut renamed = false;
        let mut number = 1;
        loop {
            let name = format!("{base}{}", req.ext);
            match publish(&tmp, &dir.join(&name)) {
                Ok(()) => {
                    let (width, height) = media_info::dimensions(&dir.join(&name));
                    return Ok(Copied {
                        rel_path: format!("{folder}/{name}"),
                        name,
                        size,
                        hash,
                        width,
                        height,
                        renamed,
                        source: display_path(&req.source),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists && number < MAX_NUMBER => {
                    number += 1;
                    base = format!("{} ({number})", req.base);
                    renamed = true;
                }
                Err(e) => return Err(format!("{name} could not be saved: {e}")),
            }
        }
    });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// The full path of a file in the Media folder from its `/`-separated relative
/// path, or None when the path could lead outside Media: an empty
/// part, `.` or `..`, a `\`, a drive (`C:`) or a control character.
pub fn media_file(media: &Path, rel: &str) -> Option<PathBuf> {
    let mut path = media.to_path_buf();
    for part in rel.split('/') {
        if !safe_part(part) {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

/// One plain file or folder name: nothing that `Path::push` could turn into a
/// parent, a root, a drive or another separator.
fn safe_part(part: &str) -> bool {
    !part.is_empty() && part != "." && part != ".." && !part.chars().any(|c| matches!(c, '\\' | '/' | ':') || c.is_control())
}

/// Why a library record's path is not `<Pictures|GIFs|Videos>/<file name>`, or
/// None when it is. Checked when the library file is loaded, so a
/// damaged or foreign file can never point Rename, the Recycle Bin, paste or a
/// backup at a file outside the Media folders.
pub fn item_path_problem(rel: &str) -> Option<&'static str> {
    let Some((folder, file)) = rel.split_once('/') else {
        return Some("it is not in the Pictures, GIFs or Videos folder");
    };
    if !["Pictures", "GIFs", "Videos"].iter().any(|f| f.eq_ignore_ascii_case(folder)) {
        return Some("it is not in the Pictures, GIFs or Videos folder");
    }
    if !safe_part(file) {
        return Some("it points outside PocketMedia's Media folder");
    }
    None
}

/// A path for people: without the `\\?\` prefix that canonical Windows paths carry.
pub fn display_path(p: &Path) -> String {
    let s = p.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

fn write_partial(req: &CopyRequest, tmp: &Path, digest: NewDigest, cancel: &AtomicBool) -> Result<(u64, String), String> {
    let source_name = req.source.display();
    // Opened for reading only: the source is never changed.
    let mut src = File::open(&req.source).map_err(|e| format!("{source_name} could not be read: {e}"))?;
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .map_err(|e| format!("could not write in the Media folder: {e}"))?;
    let mut d: Box<dyn Digest> = digest()?;
    let mut buf = vec![0u8; CHUNK];
    let mut size = 0u64;
    loop {
        cancelled(cancel)?;
        let n = src.read(&mut buf).map_err(|e| format!("{source_name} could not be read: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| format!("could not write in the Media folder: {e}"))?;
        d.update(&buf[..n]);
        size += n as u64;
    }
    out.sync_all().map_err(|e| format!("could not write in the Media folder: {e}"))?;
    Ok((size, d.finish()))
}

/// Removes temporary files left by an import that was interrupted (for example,
/// the app was ended). Returns how many were removed.
pub fn cleanup_partials(media: &Path) -> usize {
    let mut removed = 0;
    for folder in FOLDERS {
        let Ok(entries) = fs::read_dir(media.join(folder)) else { continue };
        for e in entries.flatten() {
            if is_partial(&e.file_name().to_string_lossy()) && fs::remove_file(e.path()).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// A supported file in a Media folder that has no library record.
#[derive(Clone, Debug, PartialEq)]
pub struct Orphan {
    pub rel_path: String,
    pub name: String,
    pub kind: MediaKind,
    pub size: u64,
    pub width: u32,
    pub height: u32,
}

/// Compares the Media folders with the library records (`(id, relative path)`).
/// Returns files without a record (for example, copied just before the app was
/// ended) and the ids of records whose file is gone.
pub fn reconcile(media: &Path, records: &[(u32, String)]) -> (Vec<Orphan>, Vec<u32>) {
    let known: HashSet<String> = records.iter().map(|(_, p)| p.to_lowercase()).collect();
    let mut orphans = Vec::new();
    for folder in FOLDERS {
        let Ok(entries) = fs::read_dir(media.join(folder)) else { continue };
        let mut found: Vec<(String, u64, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let m = e.metadata().ok()?;
                let name = e.file_name().to_string_lossy().to_string();
                (m.is_file() && !is_partial(&name)).then(|| (name, m.len(), e.path()))
            })
            .collect();
        found.sort_by_key(|f| f.0.to_lowercase());
        for (name, size, path) in found {
            let rel = format!("{folder}/{name}");
            let Some(kind) = media_info::kind_for(&name) else { continue };
            if !known.contains(&rel.to_lowercase()) {
                let (width, height) = media_info::dimensions(&path);
                orphans.push(Orphan { rel_path: rel, name, kind, size, width, height });
            }
        }
    }
    let missing = records.iter().filter(|(_, p)| !media_file(media, p).is_some_and(|f| f.is_file())).map(|(id, _)| *id).collect();
    (orphans, missing)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::digest::testing::fnv;

    pub fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pm-import-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Like MoveFileEx without replace: hard link, then remove the old name.
    pub fn publish(from: &Path, to: &Path) -> io::Result<()> {
        fs::hard_link(from, to)?;
        fs::remove_file(from)
    }

    fn media(root: &Path) -> PathBuf {
        let m = root.join("Media");
        for f in FOLDERS {
            fs::create_dir_all(m.join(f)).unwrap();
        }
        m
    }

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn mtime(p: &Path) -> std::time::SystemTime {
        fs::metadata(p).unwrap().modified().unwrap()
    }

    fn no_cancel() -> AtomicBool {
        AtomicBool::new(false)
    }

    /// Only plain names inside Media make a path.
    #[test]
    fn media_paths_never_leave_the_media_folder() {
        let media = Path::new("/pm/Media");
        assert_eq!(media_file(media, "Pictures/Cat.png"), Some(media.join("Pictures").join("Cat.png")));
        assert_eq!(media_file(media, "Pictures/Trip/day 1/a.png"), Some(media.join("Pictures").join("Trip").join("day 1").join("a.png")));
        for bad in ["../x.png", "Pictures/../../x.png", "/x.png", "C:/x.png", "Pictures\\..\\x.png", "Pictures//x.png", "", "Pictures/.", "Pictures/a\u{0}b"] {
            assert_eq!(media_file(media, bad), None, "{bad:?}");
        }
        assert_eq!(item_path_problem("GIFs/Hello.gif"), None);
        assert_eq!(item_path_problem("pictures/Cat.png"), None, "letter case as Windows ignores it");
        for bad in ["Cat.png", "Other/Cat.png", "Pictures/a/b.png", "Pictures/..", "../Pictures/x.png", "Videos/C:x.mp4"] {
            assert!(item_path_problem(bad).is_some(), "{bad:?}");
        }
    }

    #[test]
    fn folder_scan_takes_all_subfolders_and_lists_unsupported_files() {
        let root = temp("scan");
        let media = media(&root);
        let src = root.join("src");
        write(&src.join("b.png"), b"b");
        write(&src.join("A.GIF"), b"a");
        write(&src.join("notes.txt"), b"t");
        write(&src.join("sub/deep/c.mp4"), b"c");
        write(&src.join("sub/d.jpeg"), b"d");
        let c = scan(&Source::Folder(src.clone()), &media, &no_cancel()).unwrap();
        let names: Vec<&str> = c.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["A.GIF", "b.png", "notes.txt", "d.jpeg", "c.mp4"]);
        assert_eq!(c[2].kind, None);
        assert_eq!(c[0].kind, Some(MediaKind::Gif));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_media_folder_is_never_a_source() {
        let root = temp("media-src");
        let media = media(&root);
        write(&media.join("Pictures/x.png"), b"x");
        write(&root.join("y.png"), b"y");
        // Choosing the parent of Media skips Media itself.
        let c = scan(&Source::Folder(root.clone()), &media, &no_cancel()).unwrap();
        assert_eq!(c.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["y.png"]);
        // Choosing Media (or inside it) is refused.
        assert!(scan(&Source::Folder(media.join("Pictures")), &media, &no_cancel()).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_not_followed() {
        let root = temp("links");
        let media = media(&root);
        let src = root.join("src");
        write(&src.join("a.png"), b"a");
        std::os::unix::fs::symlink(&src, src.join("loop")).unwrap();
        let c = scan(&Source::Folder(src), &media, &no_cancel()).unwrap();
        assert_eq!(c.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    fn known(media: &Path, id: u32, rel: &str, bytes: &[u8]) -> KnownFile {
        write(&media.join(rel), bytes);
        let name = rel.rsplit('/').next().unwrap().to_string();
        KnownFile { id, name, size: bytes.len() as u64, hash: String::new(), abs: media.join(rel) }
    }

    #[test]
    fn duplicates_are_skipped_and_taken_names_get_a_number() {
        let root = temp("plan");
        let media = media(&root);
        let lib = vec![known(&media, 1, "Pictures/Cat.png", b"cat"), known(&media, 2, "GIFs/Wave.gif", b"wave")];
        let src = root.join("src");
        write(&src.join("Cat.png"), b"cat"); // same content as the library's
        write(&src.join("Wave.gif"), b"other wave"); // same name, different content
        write(&src.join("New.png"), b"new");
        write(&src.join("sub/New.png"), b"new"); // same content, earlier in the batch
        write(&src.join("sub/Wave.gif"), b"third wave"); // same name again
        write(&src.join("sub/readme.txt"), b"?");
        let cands = scan(&Source::Folder(src), &media, &no_cancel()).unwrap();
        let plan = plan(cands, &lib, &media, fnv, &no_cancel()).unwrap();
        let got: Vec<(String, Status, bool)> =
            plan.rows.iter().map(|r| (format!("{}{}", r.base, ext_of(&r.cand.name)), r.status.clone(), r.auto_renamed)).collect();
        assert_eq!(
            got,
            [
                ("Cat.png".into(), Status::InLibrary("Cat.png".into()), false),
                ("New.png".into(), Status::Copy, false),
                ("Wave (2).gif".into(), Status::Copy, true),
                ("New.png".into(), Status::InBatch("New.png".into()), false),
                ("readme.txt".into(), Status::Unsupported, false),
                ("Wave (3).gif".into(), Status::Copy, true),
            ]
        );
        assert_eq!(plan.library_hashes.len(), 1, "only the same-size library file was hashed");
        let _ = fs::remove_dir_all(&root);
    }

    /// A file that cannot be read becomes a row with the reason; the
    /// rest of the batch is planned as usual.
    #[test]
    fn a_file_that_cannot_be_read_becomes_a_row_and_the_rest_go_on() {
        let root = temp("unreadable");
        let media = media(&root);
        let src = root.join("src");
        write(&src.join("A.png"), b"same1");
        write(&src.join("B.png"), b"same2"); // same size: B is hashed and compared with A
        write(&src.join("C.gif"), b"c");
        let files = vec![src.join("A.png"), src.join("B.png"), src.join("C.gif"), src.join("gone.png")];
        let cands = scan(&Source::Files(files), &media, &no_cancel()).unwrap();
        fs::remove_file(src.join("B.png")).unwrap(); // vanishes between listing and checking
        let first = plan(cands, &[], &media, fnv, &no_cancel()).unwrap();
        let got: Vec<(&str, bool)> = first.rows.iter().map(|r| (r.cand.name.as_str(), matches!(r.status, Status::Unreadable(_)))).collect();
        assert_eq!(got, [("A.png", false), ("B.png", true), ("C.gif", false), ("gone.png", true)]);
        assert_eq!(first.rows[0].status, Status::Copy);
        assert_eq!(first.rows[2].status, Status::Copy);
        let Status::Unreadable(why) = &first.rows[1].status else { panic!() };
        assert!(why.contains("B.png") && why.contains("could not be read"), "{why}");

        // An earlier file of the batch that cannot be read is not taken for a match.
        write(&src.join("A2.png"), b"same3");
        let cands = scan(&Source::Files(vec![src.join("A.png"), src.join("A2.png")]), &media, &no_cancel()).unwrap();
        fs::remove_file(src.join("A.png")).unwrap();
        let second = plan(cands, &[], &media, fnv, &no_cancel()).unwrap();
        assert_eq!(second.rows.iter().map(|r| r.status.clone()).collect::<Vec<_>>(), [Status::Copy, Status::Copy]);
        let _ = fs::remove_dir_all(&root);
    }

    /// A subfolder that cannot be opened is listed, not skipped silently.
    #[cfg(windows)]
    #[test]
    fn a_folder_that_cannot_be_opened_is_listed() {
        let root = temp("locked-folder");
        let media = media(&root);
        let src = root.join("src");
        write(&src.join("ok.png"), b"ok");
        write(&src.join("locked/inside.png"), b"in");
        let locked = src.join("locked");
        let icacls = |args: &[&str]| std::process::Command::new("icacls").arg(&locked).args(args).output().unwrap();
        assert!(icacls(&["/deny", "*S-1-1-0:(RX)"]).status.success());
        let cands = scan(&Source::Folder(src.clone()), &media, &no_cancel());
        let _ = icacls(&["/remove:d", "*S-1-1-0"]);
        let cands = cands.unwrap();
        assert!(cands.iter().any(|c| c.name == "ok.png" && c.problem.is_none()));
        assert!(cands.iter().any(|c| c.name == "locked" && c.problem.as_deref().is_some_and(|p| p.contains("could not be opened"))), "{cands:?}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_already_in_the_folder_counts_as_taken() {
        let root = temp("disk-taken");
        let media = media(&root);
        write(&media.join("Pictures/Dog.png"), b"not in the library");
        let (b, renamed) = unique_base("Dog", ".png", &HashSet::new(), &media.join("Pictures"));
        assert_eq!((b.as_str(), renamed), ("Dog (2)", true));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn copy_keeps_the_source_and_never_overwrites() {
        let root = temp("copy");
        let media = media(&root);
        let src = root.join("src/Cat.png");
        write(&src, b"cat bytes");
        let before = mtime(&src);
        write(&media.join("Pictures/Cat.png"), b"someone else's cat");
        let req = CopyRequest { source: src.clone(), kind: MediaKind::Picture, base: "Cat".into(), ext: ".png".into() };
        let c = copy_one(&req, &media, fnv, publish, &no_cancel()).unwrap();
        assert_eq!(c.name, "Cat (2).png", "the existing file is not replaced");
        assert!(c.renamed);
        assert_eq!(c.rel_path, "Pictures/Cat (2).png");
        assert_eq!(fs::read(media.join("Pictures/Cat.png")).unwrap(), b"someone else's cat");
        assert_eq!(fs::read(media.join("Pictures/Cat (2).png")).unwrap(), b"cat bytes");
        assert_eq!(fs::read(&src).unwrap(), b"cat bytes", "source unchanged");
        assert_eq!(mtime(&src), before);
        assert_eq!(c.size, 9);
        assert_eq!(cleanup_partials(&media), 0, "no temporary file left");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_cancelled_copy_leaves_nothing() {
        let root = temp("cancel");
        let media = media(&root);
        let src = root.join("src/Big.mp4");
        write(&src, &vec![7u8; 3 * CHUNK]);
        let req = CopyRequest { source: src, kind: MediaKind::Video, base: "Big".into(), ext: ".mp4".into() };
        let stop = AtomicBool::new(true);
        assert!(copy_one(&req, &media, fnv, publish, &stop).is_err());
        assert_eq!(fs::read_dir(media.join("Videos")).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn startup_cleanup_and_reconcile_after_an_interrupted_import() {
        let root = temp("reconcile");
        let media = media(&root);
        // A copy that was cut off, a finished copy whose record was never saved,
        // a recorded file, and a record whose file is gone.
        write(&media.join("Videos/.pocketmedia-123-0.partial"), b"half");
        write(&media.join("GIFs/Done.gif"), &media_info::fixtures::gif(40, 30, "x"));
        write(&media.join("Pictures/Known.png"), b"k");
        write(&media.join("Pictures/notes.txt"), b"not media");
        assert_eq!(cleanup_partials(&media), 1);
        let records = vec![(1, "Pictures/Known.png".to_string()), (2, "Videos/Gone.mp4".to_string())];
        let (orphans, missing) = reconcile(&media, &records);
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].rel_path, "GIFs/Done.gif");
        assert_eq!((orphans[0].width, orphans[0].height), (40, 30));
        assert_eq!(missing, [2]);
        let _ = fs::remove_dir_all(&root);
    }
}
