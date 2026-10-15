//! Back up library (Settings): copies the whole Media folder and a
//! consistent library file to a folder the user picks, on a worker thread.
//!
//! - Everything under Media is copied, not only library items: the
//!   uninstaller deletes the whole folder, so subfolders, other file types and
//!   loose files are saved too. Temporary `.partial` files are left out, and
//!   junctions and symbolic links are never followed.
//! - The library file in the backup is written from a snapshot taken when the
//!   backup starts, and lists exactly the library items that were copied, so the
//!   two always agree. While it runs, PocketMedia's own changes (import, Rename,
//!   Move to Recycle Bin) wait; a file removed outside PocketMedia meanwhile is
//!   left out and reported.
//! - Every copy is read back and compared (SHA-256) before the backup counts as
//!   finished. Nothing is ever overwritten.
//! - The backup is written into `PocketMedia backup <date> <time> (unfinished)`
//!   and gets its final name, `PocketMedia backup <date> <time>`, only after
//!   every copy was checked. A backup that was interrupted (the PC
//!   shut down, the process was ended) keeps "(unfinished)" in its name, so it
//!   can never be taken for a finished one. Leftovers are never deleted; a later
//!   backup into the same folder mentions them.
//! - A destination inside PocketMedia's own folder is refused.
//! - On failure or Cancel, the files and folders this backup made are removed;
//!   nothing else is touched. Caches are not backed up; they are made again.
//! - Restoring: copy the backup's Media and Data folders beside pocketmedia.exe
//!   in any folder. Library paths are relative to Media, so another install
//!   path works (`How to restore.txt` says so in the backup).

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::digest::{Digest, NewDigest};
use crate::import::{FOLDERS, Publish, display_path, media_file};
use crate::jobs::{self, Event, Job};
use crate::store::{FILE_NAME, StoreData};

pub const BUSY: &str = "A backup is running. Try again when it has finished.";
const CHUNK: usize = 1 << 20;
const RESTORE_NOTE: &str = "How to restore.txt";
const NAME: &str = "PocketMedia backup";
const UNFINISHED: &str = " (unfinished)";

/// Where a running backup is, for the progress panel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// Files finished, of `total`.
    pub done: usize,
    pub total: usize,
    /// Bytes copied, and bytes read back to check the copies, each of `bytes_total`
    /// (the library's sizes; a file replaced meanwhile can make them run over).
    pub copied: u64,
    pub checked: u64,
    pub bytes_total: u64,
    /// The file being copied or checked.
    pub current: String,
    pub checking: bool,
}

impl Step {
    /// How far along, 0 to 1, for the bar: the average of files done and bytes done
    /// (copying and checking each count half of the bytes), so it moves with every
    /// file and still slows down for a large video.
    pub fn fraction(&self) -> f32 {
        let files = (self.total > 0).then(|| self.done as f64 / self.total as f64);
        let bytes = (self.bytes_total > 0).then(|| (self.copied + self.checked) as f64 / (2 * self.bytes_total) as f64);
        let f = match (files, bytes) {
            (Some(f), Some(b)) => (f + b) / 2.0,
            (Some(x), None) | (None, Some(x)) => x,
            (None, None) => 0.0,
        };
        f.clamp(0.0, 1.0) as f32
    }

    /// "120 of 340 files · 1.2 of 3.4 GB"
    pub fn counts(&self) -> String {
        let total = self.bytes_total.max(self.copied);
        format!("{} of {} files · {} of {}", self.done, self.total, size_text(self.copied), size_text(total))
    }

    /// "Copying: Happy dance.gif", "Checking: …", then "Writing the library file…".
    pub fn doing(&self) -> String {
        match (self.current.is_empty(), self.checking) {
            (true, _) if self.total > 0 && self.done == self.total => "Writing the library file…".into(),
            (true, _) => "Getting ready…".into(),
            (false, false) => format!("Copying: {}", self.current),
            (false, true) => format!("Checking: {}", self.current),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Progress {
    Copying(Step),
    Finished(Done),
    Failed(String),
    Cancelled,
}

/// A finished, checked backup.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Done {
    pub folder: PathBuf,
    /// Library items copied (they are in the backup's library file).
    pub items: usize,
    /// Other files copied from Media: subfolders, other file types, loose files.
    pub others: usize,
    pub bytes: u64,
    /// Files that left the Media folder during the backup.
    pub left_out: Vec<String>,
    /// Junctions and symbolic links in Media, not followed.
    pub links: usize,
    /// Unfinished backups from before in the same folder (names), left alone.
    pub leftovers: Vec<String>,
    /// Files the drive would not let PocketMedia read back past Windows' file
    /// cache: checked against the copy in memory only.
    pub cached: usize,
}

/// Everything a backup needs, captured on the UI thread.
pub struct Plan {
    /// PocketMedia's folder (Media, Data and Cache are in it). The backup may not go inside it.
    pub root: PathBuf,
    pub media: PathBuf,
    /// The library as it is when the backup starts.
    pub data: StoreData,
    /// The folder the user picked; the backup goes into a new folder inside it.
    pub dest: PathBuf,
    /// For the new folder's name: `2026-10-13 1620`.
    pub stamp: String,
    /// Renames a folder, failing with `AlreadyExists` if the new name is taken.
    pub rename: Publish,
}

/// True when `child` is `parent` or inside it (letter case ignored, as on Windows).
fn inside(child: &Path, parent: &Path) -> bool {
    let parts = |p: &Path| -> Vec<String> { p.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect() };
    parts(child).starts_with(&parts(parent))
}

/// The real folder for `dest` (links resolved), refused when it is inside `root`.
pub fn check_destination(dest: &Path, root: &Path) -> Result<PathBuf, String> {
    let shown = display_path(dest);
    let real = fs::canonicalize(dest).map_err(|e| format!("{shown} could not be opened: {e}"))?;
    if !real.is_dir() {
        return Err(format!("{shown} is not a folder."));
    }
    let root = fs::canonicalize(root).map_err(|e| format!("PocketMedia's folder could not be checked: {e}"))?;
    if inside(&real, &root) {
        return Err(format!("Choose a folder outside PocketMedia. A backup cannot go inside {}.", display_path(&root)));
    }
    Ok(real)
}

/// What this backup made, so a failed or cancelled one can be removed.
#[derive(Default)]
struct Made {
    files: Vec<PathBuf>,
    dirs: Vec<PathBuf>,
}

impl Made {
    fn dir(&mut self, path: PathBuf) -> io::Result<()> {
        fs::create_dir(&path)?;
        self.dirs.push(path);
        Ok(())
    }

    /// Removes what this backup made, newest first. Returns what could not be removed.
    fn remove(self) -> Vec<PathBuf> {
        let mut left = Vec::new();
        for f in self.files.iter().rev() {
            if fs::remove_file(f).is_err() && f.exists() {
                left.push(f.clone());
            }
        }
        // remove_dir only removes an empty folder, so nothing else can go.
        for d in self.dirs.iter().rev() {
            if fs::remove_dir(d).is_err() && d.exists() {
                left.push(d.clone());
            }
        }
        left
    }
}

enum Stop {
    Cancelled,
    Failed(String),
}

fn check(cancel: &AtomicBool) -> Result<(), Stop> {
    if cancel.load(Ordering::Relaxed) { Err(Stop::Cancelled) } else { Ok(()) }
}

/// Runs a backup. `progress` is called as files are copied and checked.
pub fn run(plan: &Plan, digest: NewDigest, cancel: &AtomicBool, progress: &mut dyn FnMut(&Step)) -> Progress {
    let dest = match check_destination(&plan.dest, &plan.root) {
        Ok(d) => d,
        Err(e) => return Progress::Failed(e),
    };
    let mut made = Made::default();
    let result = copy_all(plan, &dest, digest, cancel, progress, &mut made);
    match result {
        Ok(done) => done,
        Err(stop) => {
            let left = made.remove();
            let mut outcome = match stop {
                Stop::Cancelled => Progress::Cancelled,
                Stop::Failed(e) => Progress::Failed(e),
            };
            if !left.is_empty() {
                let note = format!(" The unfinished backup could not be removed completely: {}.", display_path(&left[0]));
                outcome = match outcome {
                    Progress::Failed(e) => Progress::Failed(e + &note),
                    _ => Progress::Failed(format!("The backup was cancelled.{note}")),
                };
            }
            outcome
        }
    }
}

/// `PocketMedia backup 2026-10-13 1620`, or `… (2)` from the second on.
fn base_name(stamp: &str, n: u32) -> String {
    if n == 1 { format!("{NAME} {stamp}") } else { format!("{NAME} {stamp} ({n})") }
}

/// True for a backup folder that never got its final name.
pub fn is_unfinished(name: &str) -> bool {
    name.starts_with(NAME) && name.ends_with(UNFINISHED)
}

/// Temporary files of an import that was interrupted: not worth saving.
fn is_temporary(name: &str) -> bool {
    name.to_lowercase().ends_with(".partial")
}

/// What is under Media: files (relative `/` paths and sizes) and folders,
/// parents first. Links are counted, never followed.
#[derive(Default)]
struct Tree {
    files: Vec<(String, u64)>,
    dirs: Vec<String>,
    links: usize,
}

fn walk(media: &Path, cancel: &AtomicBool) -> Result<Tree, Stop> {
    let mut tree = Tree::default();
    let mut stack = vec![String::new()];
    while let Some(rel) = stack.pop() {
        check(cancel)?;
        let dir = if rel.is_empty() { Some(media.to_path_buf()) } else { media_file(media, &rel) };
        let dir = dir.ok_or_else(|| Stop::Failed(format!("\"{rel}\" in the Media folder is not a plain folder name")))?;
        let unreadable = |e: io::Error| Stop::Failed(format!("{} could not be read: {e}", display_path(&dir)));
        let mut entries: Vec<fs::DirEntry> = fs::read_dir(&dir).map_err(unreadable)?.collect::<Result<_, _>>().map_err(unreadable)?;
        entries.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
        let mut subdirs = Vec::new();
        for e in entries {
            let name = e.file_name().to_string_lossy().to_string();
            let child = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            // On Windows a junction is a link here too (a name-surrogate reparse point).
            let kind = e.file_type().map_err(|err| Stop::Failed(format!("{} could not be read: {err}", display_path(&e.path()))))?;
            if kind.is_symlink() {
                tree.links += 1;
            } else if kind.is_dir() {
                tree.dirs.push(child.clone());
                subdirs.push(child);
            } else if kind.is_file() && !is_temporary(&name) {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                tree.files.push((child, size));
            }
        }
        // Visited in name order (the stack pops the last one first).
        stack.extend(subdirs.into_iter().rev());
    }
    Ok(tree)
}

/// Makes `<dest>\PocketMedia backup <stamp> (unfinished)`: the first number
/// whose finished and unfinished names are both free. Never an existing folder.
fn make_unfinished(dest: &Path, stamp: &str, made: &mut Made) -> Result<PathBuf, Stop> {
    for n in 1..100 {
        let base = base_name(stamp, n);
        if fs::symlink_metadata(dest.join(&base)).is_ok() {
            continue;
        }
        let folder = dest.join(format!("{base}{UNFINISHED}"));
        match made.dir(folder.clone()) {
            Ok(()) => return Ok(folder),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Stop::Failed(format!("the backup folder could not be made in {}: {e}", display_path(dest)))),
        }
    }
    Err(Stop::Failed(format!("{} already holds 99 backups made in the same minute", display_path(dest))))
}

/// Gives a checked backup its final name: the first free `PocketMedia backup <stamp>`.
/// A virus scanner or the search indexer may still hold one of the new files for
/// a moment, which makes Windows refuse the rename; it is tried again for up to
/// about 5 seconds before the backup counts as failed.
fn finish_name(unfinished: &Path, dest: &Path, stamp: &str, rename: Publish, cancel: &AtomicBool) -> Result<PathBuf, Stop> {
    let mut busy_tries = 0;
    let mut n = 1;
    while n < 100 {
        let target = dest.join(base_name(stamp, n));
        if fs::symlink_metadata(&target).is_ok() {
            n += 1;
            continue;
        }
        match rename(unfinished, &target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => n += 1,
            Err(_) if busy_tries < 20 => {
                busy_tries += 1;
                check(cancel)?;
                std::thread::sleep(Duration::from_millis(250));
            }
            Err(e) => return Err(Stop::Failed(format!("the finished backup could not be given its name: {e}"))),
        }
    }
    Err(Stop::Failed("the finished backup could not be given its name: every name is taken".into()))
}

/// Unfinished backups from before in `dest` (not this one), by name.
fn leftovers(dest: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dest)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| is_unfinished(n)).collect())
        .unwrap_or_default();
    names.sort();
    names
}

fn copy_all(
    plan: &Plan,
    dest: &Path,
    digest: NewDigest,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(&Step),
    made: &mut Made,
) -> Result<Progress, Stop> {
    let failed = |what: &str, e: &dyn std::fmt::Display| Stop::Failed(format!("{what}: {e}"));
    let mut step = Step::default();
    progress(&step);
    let tree = walk(&plan.media, cancel)?;
    let library: HashSet<String> = plan.data.items.iter().map(|i| i.path.to_lowercase()).collect();
    let others: Vec<&(String, u64)> = tree.files.iter().filter(|(rel, _)| !library.contains(&rel.to_lowercase())).collect();

    // A new folder, never one that exists. It is named "(unfinished)" until every copy is checked.
    let folder = make_unfinished(dest, &plan.stamp, made)?;
    let media = folder.join("Media");
    let data = folder.join("Data");
    let mut dirs = vec![media.clone()];
    let mut seen = HashSet::new();
    for rel in FOLDERS.iter().map(|f| f.to_string()).chain(tree.dirs.iter().cloned()) {
        if seen.insert(rel.to_lowercase()) {
            dirs.push(media_file(&media, &rel).ok_or_else(|| Stop::Failed(format!("\"{rel}\" is not a plain folder name")))?);
        }
    }
    dirs.push(data.clone());
    for dir in dirs {
        made.dir(dir).map_err(|e| failed("the backup folders could not be made", &e))?;
    }

    let mut kept = plan.data.clone();
    let total = plan.data.items.len() + others.len();
    let mut left_out = Vec::new();
    let mut bytes = 0u64;
    let mut copied = Vec::with_capacity(plan.data.items.len());
    let mut others_done = 0;
    let mut cached = 0;
    step.total = total;
    step.bytes_total = plan.data.items.iter().map(|i| i.size).sum::<u64>() + others.iter().map(|(_, size)| size).sum::<u64>();
    // Library items first (with their records), then everything else in Media.
    let jobs = plan.data.items.iter().map(|it| (Some(it), it.path.as_str(), it.name.clone()));
    let jobs = jobs.chain(others.iter().map(|(rel, _)| (None, rel.as_str(), rel.rsplit('/').next().unwrap_or(rel).to_string())));
    for (i, (item, rel, name)) in jobs.enumerate() {
        check(cancel)?;
        step.done = i;
        step.current = name;
        step.checking = false;
        progress(&step);
        // Library paths were checked on load; a walked path is a plain name.
        let (Some(src), Some(dst)) = (media_file(&plan.media, rel), media_file(&media, rel)) else {
            return Err(Stop::Failed(format!("\"{rel}\" is not a plain path inside the Media folder")));
        };
        let delay = TEST_DELAY_MS.load(Ordering::Relaxed);
        if delay > 0 {
            std::thread::sleep(Duration::from_millis(delay));
        }
        let mut on_bytes = |n: u64, checking: bool| {
            if checking {
                step.checked += n;
            } else {
                step.copied += n;
            }
            step.checking = checking;
            progress(&step);
        };
        match (copy_verified(&src, &dst, digest, cancel, made, &mut cached, &mut on_bytes)?, item) {
            (None, Some(item)) => left_out.push(item.name.clone()),
            (None, None) => left_out.push(rel.to_string()),
            (Some((size, hash)), Some(item)) => {
                bytes += size;
                copied.push((item.id, size, hash));
            }
            (Some((size, _)), None) => {
                bytes += size;
                others_done += 1;
            }
        }
    }
    step.done = total;
    step.current.clear();
    progress(&step);

    // The library file lists exactly what was copied, with the sizes and hashes just checked.
    kept.items.retain(|it| copied.iter().any(|(id, _, _)| *id == it.id));
    for it in &mut kept.items {
        if let Some((_, size, hash)) = copied.iter().find(|(id, _, _)| *id == it.id) {
            it.size = *size;
            it.hash = hash.clone();
        }
    }
    kept.history.retain(|id| kept.items.iter().any(|it| it.id == *id));
    let text = serde_json::to_string_pretty(&kept).map_err(|e| failed("the library file could not be written", &e))?;
    let json = data.join(FILE_NAME);
    write_new(&json, text.as_bytes(), made).map_err(|e| failed("the library file could not be written", &e))?;
    let mut back = Vec::new();
    let from_drive = read_back(&json, cancel, &mut |chunk: &[u8], restart: bool| {
        if restart {
            back.clear();
        }
        back.extend_from_slice(chunk);
        Ok(())
    })?;
    if !from_drive {
        cached += 1;
    }
    let back = String::from_utf8(back).map_err(|e| failed("the library file could not be read back", &e))?;
    match crate::store::check_text(&back) {
        Ok(read) if read == kept => {}
        Ok(_) => return Err(Stop::Failed("the library file read back differently".into())),
        Err(e) => return Err(failed("the library file could not be read back", &e)),
    }
    let note = restore_note(&plan.stamp);
    write_new(&folder.join(RESTORE_NOTE), note.as_bytes(), made).map_err(|e| failed("the backup could not be finished", &e))?;
    check(cancel)?;
    // Everything is checked: only now does it get the name of a finished backup.
    let folder = finish_name(&folder, dest, &plan.stamp, plan.rename, cancel)?;
    Ok(Progress::Finished(Done {
        folder,
        items: copied.len(),
        others: others_done,
        bytes,
        left_out,
        links: tree.links,
        leftovers: leftovers(dest),
        cached,
    }))
}

fn write_new(path: &Path, bytes: &[u8], made: &mut Made) -> io::Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
    made.files.push(path.to_path_buf());
    f.write_all(bytes)?;
    f.sync_all()
}

/// The hash of a copy, read back from the drive (see `read_back`), and whether
/// it came from the drive itself.
fn hash_of(path: &Path, digest: NewDigest, cancel: &AtomicBool, on_bytes: &mut dyn FnMut(u64, bool)) -> Result<(String, bool), Stop> {
    let mut d: Box<dyn Digest> = digest().map_err(Stop::Failed)?;
    let from_drive = read_back(path, cancel, &mut |chunk: &[u8], restart: bool| {
        if restart {
            d = digest().map_err(Stop::Failed)?;
            return Ok(());
        }
        d.update(chunk);
        on_bytes(chunk.len() as u64, true);
        Ok(())
    })?;
    Ok((d.finish(), from_drive))
}

/// Takes each chunk read back; `(&[], true)` means "start again" (see `read_back`).
type ReadChunk<'a> = dyn FnMut(&[u8], bool) -> Result<(), Stop> + 'a;

/// Sector alignment for reads that bypass the file cache. 4096 is a multiple of
/// every sector size in common use (512 and 4096 bytes).
const ALIGN: usize = 4096;

/// Reads a copy back in chunks to check it. Right after writing,
/// a plain read is served from Windows' file cache in memory, which proves the
/// copy loop but not the drive. So the copy is opened without that cache
/// (FILE_FLAG_NO_BUFFERING), which reads what the drive holds. A drive that does
/// not allow it (some network and virtual drives) is read the plain way: then
/// `chunk(&[], true)` restarts the reader, and the result is false.
fn read_back(path: &Path, cancel: &AtomicBool, chunk: &mut ReadChunk) -> Result<bool, Stop> {
    let failed = |e: io::Error| Stop::Failed(format!("{} could not be read back: {e}", display_path(path)));
    let mut buf = vec![0u8; CHUNK + ALIGN];
    // A Vec<u8> of this size is never so misaligned that the offset exceeds ALIGN.
    let start = buf.as_ptr().align_offset(ALIGN).min(ALIGN);
    let buf = &mut buf[start..start + CHUNK];
    match open_from_drive(path) {
        Ok(mut f) => match read_all(&mut f, buf, cancel, chunk) {
            Ok(()) => return Ok(true),
            Err(Err(e)) if e.kind() == io::ErrorKind::InvalidInput => chunk(&[], true)?,
            Err(Err(e)) => return Err(failed(e)),
            Err(Ok(stop)) => return Err(stop),
        },
        Err(e) if e.kind() == io::ErrorKind::InvalidInput => {}
        Err(e) => return Err(failed(e)),
    }
    let mut f = File::open(path).map_err(failed)?;
    match read_all(&mut f, buf, cancel, chunk) {
        Ok(()) => Ok(false),
        Err(Err(e)) => Err(failed(e)),
        Err(Ok(stop)) => Err(stop),
    }
}

/// Reads `f` to its end, chunk by chunk. Err(Ok) stops the backup; Err(Err) is a read error.
fn read_all(f: &mut File, buf: &mut [u8], cancel: &AtomicBool, chunk: &mut ReadChunk) -> Result<(), Result<Stop, io::Error>> {
    loop {
        check(cancel).map_err(Ok)?;
        let n = f.read(buf).map_err(Err)?;
        if n == 0 {
            return Ok(());
        }
        chunk(&buf[..n], false).map_err(Ok)?;
    }
}

/// Opens `path` for reading past Windows' file cache (reads must use a sector-aligned
/// buffer whose length is a multiple of the sector size; `read_back` does).
#[cfg(windows)]
fn open_from_drive(path: &Path) -> io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::FILE_FLAG_NO_BUFFERING;
    OpenOptions::new().read(true).custom_flags(FILE_FLAG_NO_BUFFERING.0).open(path)
}

/// PocketMedia runs on Windows only; this exists for the unit tests on other systems.
#[cfg(not(windows))]
fn open_from_drive(path: &Path) -> io::Result<File> {
    File::open(path)
}

/// Copies one file and checks the copy. None: the source is gone (removed
/// outside PocketMedia during the backup).
/// `on_bytes(n, checking)` reports each chunk copied, then each chunk read back.
fn copy_verified(
    src: &Path,
    dst: &Path,
    digest: NewDigest,
    cancel: &AtomicBool,
    made: &mut Made,
    cached: &mut usize,
    on_bytes: &mut dyn FnMut(u64, bool),
) -> Result<Option<(u64, String)>, Stop> {
    let name = display_path(src);
    let mut from = match File::open(src) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Stop::Failed(format!("{name} could not be read: {e}"))),
    };
    let write_err = |e: io::Error| Stop::Failed(format!("{} could not be written: {e}", display_path(dst)));
    let mut to = OpenOptions::new().write(true).create_new(true).open(dst).map_err(write_err)?;
    made.files.push(dst.to_path_buf());
    let mut d: Box<dyn Digest> = digest().map_err(Stop::Failed)?;
    let mut buf = vec![0u8; CHUNK];
    let mut size = 0u64;
    loop {
        check(cancel)?;
        let n = from.read(&mut buf).map_err(|e| Stop::Failed(format!("{name} could not be read: {e}")))?;
        if n == 0 {
            break;
        }
        to.write_all(&buf[..n]).map_err(write_err)?;
        d.update(&buf[..n]);
        size += n as u64;
        on_bytes(n as u64, false);
    }
    to.sync_all().map_err(write_err)?;
    drop(to);
    let hash = d.finish();
    let (back, from_drive) = hash_of(dst, digest, cancel, on_bytes)?;
    if back != hash {
        return Err(Stop::Failed(format!("the copy of {name} does not match the original")));
    }
    if !from_drive {
        *cached += 1;
    }
    Ok(Some((size, hash)))
}

fn restore_note(stamp: &str) -> String {
    format!(
        "PocketMedia backup made on {stamp}.\r\n\r\n\
         It holds your pictures, GIFs and videos (Media) and your library and settings (Data).\r\n\
         Media also holds every other file that was in PocketMedia's Media folder, \
         such as subfolders you made there yourself.\r\n\r\n\
         To restore it:\r\n\
         1. Exit PocketMedia: right-click its tray icon and choose Exit.\r\n\
         2. Copy the Media and Data folders from here into the folder that holds pocketmedia.exe, \
         in place of the Media and Data folders there. Any PocketMedia folder works, not only the original one.\r\n\
         3. Start PocketMedia.\r\n\r\n\
         Thumbnails are not in the backup; PocketMedia makes them again as you browse.\r\n"
    )
}

/// `1.2 GB`, `340 KB`.
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} bytes") } else { format!("{v:.1} {}", UNITS[u]) }
}

/// "1 library item", "3 other files".
fn count(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// The line Settings shows for a backup event.
pub fn describe(p: &Progress) -> String {
    match p {
        Progress::Copying(s) => format!("Backing up… {} of {} files", s.done, s.total),
        Progress::Finished(d) => {
            let name = d.folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            // "Checked" only when every file was read back from the drive itself.
            let how = if d.cached == 0 { "finished and checked" } else { "finished, copied and flushed" };
            let mut s = format!(
                "Backup {how}: {} + {} ({}) in \"{name}\".",
                count(d.items, "library item"),
                count(d.others, "other file"),
                size_text(d.bytes)
            );
            if d.cached > 0 {
                s += &format!(
                    " The drive did not allow reading {} back from the drive itself, so {} checked only against Windows' copy in memory.",
                    count(d.cached, "file"),
                    if d.cached == 1 { "it was" } else { "they were" }
                );
            }
            if !d.left_out.is_empty() {
                s += &format!(" {} left the Media folder during the backup and {} not in it.", count(d.left_out.len(), "file"), if d.left_out.len() == 1 { "is" } else { "are" });
            }
            if d.links > 0 {
                s += &format!(
                    " {} in Media {} not followed (a junction or link points elsewhere, so what it points to is not in the backup).",
                    count(d.links, "link"),
                    if d.links == 1 { "was" } else { "were" }
                );
            }
            if let Some(first) = d.leftovers.first() {
                s += &format!(
                    " An unfinished backup from before is also in that folder (\"{first}\"{}). It is not complete; delete it when you no longer need it.",
                    if d.leftovers.len() > 1 { format!(" and {} more", d.leftovers.len() - 1) } else { String::new() }
                );
            }
            s
        }
        Progress::Failed(e) => format!("Backup failed: {e}"),
        Progress::Cancelled => "Backup cancelled. The unfinished backup was removed.".into(),
    }
}

// ---------------------------------------------------------------- UI thread

static JOB: Mutex<Option<Job>> = Mutex::new(None);
/// Self-test only: a pause after each file, so Cancel backup can be pressed mid-way.
pub static TEST_DELAY_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Set while the worker runs (cleared after it has cleaned up).
static WORKING: AtomicBool = AtomicBool::new(false);

/// A backup is running: PocketMedia's own changes wait.
pub fn running() -> bool {
    JOB.lock().map(|j| j.is_some()).unwrap_or(false)
}

/// `2026-10-13 1620`, local time.
fn stamp() -> String {
    // SAFETY: GetLocalTime has no preconditions and returns a plain struct.
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute)
}

/// Starts a backup into `dest`. Its progress and result arrive as `Event::Backup`.
pub fn start(dest: PathBuf) -> Result<(), String> {
    if running() {
        return Err(BUSY.into());
    }
    let p = crate::paths::get().ok_or("the PocketMedia folders are not set up")?;
    // Imports save in batches, so bring the library file up to date
    // first. Even if that save fails, `persist` has put the library into the store,
    // which is what the backup's library file is written from.
    let _ = crate::library::persist();
    let data = crate::store::with(|s| s.data.clone()).ok_or("the library is not loaded")?;
    let plan = Plan { root: p.root.clone(), media: p.media.clone(), data, dest, stamp: stamp(), rename: crate::fileops::move_no_replace };
    WORKING.store(true, Ordering::SeqCst);
    let job = jobs::spawn("backup", move |job, cancel| {
        let mut last = Instant::now() - Duration::from_secs(1);
        // At most ten updates a second (and always the last one).
        let mut report = |step: &Step| {
            if last.elapsed() >= Duration::from_millis(100) || step.done == step.total {
                last = Instant::now();
                jobs::post(Event::Backup { job, progress: Progress::Copying(step.clone()) });
            }
        };
        let result = run(&plan, crate::sha256::new, cancel, &mut report);
        WORKING.store(false, Ordering::SeqCst);
        jobs::post(Event::Backup { job, progress: result });
    })
    .inspect_err(|_| WORKING.store(false, Ordering::SeqCst))?;
    if let Ok(mut j) = JOB.lock() {
        *j = Some(job);
    }
    block_shutdown(true);
    Ok(())
}

/// While a backup runs, Windows asks before signing out or shutting down:
/// the main window answers WM_QUERYENDSESSION with "not yet",
/// and this is the reason Windows shows beside "Shut down anyway" and Cancel.
pub const SHUTDOWN_REASON: &str = "A backup is running. Stop it? Choose Cancel to let it finish.";

/// Sets or clears the reason on the hidden main window and on the Manager (the
/// window the backup runs in, which Windows lists by name), so Windows shows it
/// whichever window it asks.
fn block_shutdown(on: bool) {
    use windows::Win32::System::Shutdown::{ShutdownBlockReasonCreate, ShutdownBlockReasonDestroy};
    let reason = windows::core::HSTRING::from(SHUTDOWN_REASON);
    for window in [crate::app(|a| a.main.get()), crate::manager::hwnd()] {
        if window.is_invalid() {
            continue;
        }
        // SAFETY: our own window, on the thread that made it; the HSTRING outlives the call.
        let _ = unsafe { if on { ShutdownBlockReasonCreate(window, &reason) } else { ShutdownBlockReasonDestroy(window) } };
    }
}

/// WM_QUERYENDSESSION, for the main window and the Manager: while a backup runs
/// the answer is "not yet" (0), so Windows shows SHUTDOWN_REASON and lets the user
/// choose; otherwise 1.
pub fn may_end_session() -> isize {
    isize::from(!running())
}

/// Cancel backup (the button while it runs). The result arrives as an event.
pub fn cancel() {
    if let Ok(j) = JOB.lock()
        && let Some(job) = j.as_ref()
    {
        job.cancel();
    }
}

/// A backup event (WM_JOB): updates Settings. Returns false for other events.
pub fn on_event(ev: &Event) -> bool {
    let Event::Backup { job, progress } = ev else { return false };
    let current = JOB.lock().ok().and_then(|j| j.as_ref().map(|j| j.id));
    if current != Some(*job) {
        return true; // from a backup that already reported its end
    }
    let done = !matches!(progress, Progress::Copying(_));
    if done && let Ok(mut j) = JOB.lock() {
        *j = None;
    }
    if done {
        block_shutdown(false);
    }
    if let Progress::Finished(d) = progress {
        crate::selftest::note(&format!("backup folder: {} (left out: {:?})", display_path(&d.folder), d.left_out));
    }
    crate::manager::backup_changed(progress, &describe(progress), !done);
    true
}

/// Exit (after the user agreed to stop a running backup) or the end of the
/// Windows session: a running backup is cancelled, and its worker gets a moment
/// to remove the unfinished copy. If it cannot finish that in time, the folder
/// keeps its "(unfinished)" name.
pub fn shutdown() {
    cancel();
    let end = Instant::now() + Duration::from_secs(3);
    while WORKING.load(Ordering::SeqCst) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest::testing::fnv;
    use crate::store::StoredItem;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pm-backup-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn item(id: u32, rel: &str) -> StoredItem {
        let name = rel.rsplit_once('/').unwrap().1.to_string();
        serde_json::from_value(serde_json::json!({
            "id": id, "name": name, "kind": "picture", "path": rel, "size": 0, "hash": "",
            "width": 1, "height": 1, "added": 5
        }))
        .unwrap()
    }

    /// A PocketMedia folder with three media files and a library naming them.
    fn library(root: &Path) -> Plan {
        let media = root.join("app").join("Media");
        for f in FOLDERS {
            fs::create_dir_all(media.join(f)).unwrap();
        }
        fs::create_dir_all(root.join("app").join("Data")).unwrap();
        fs::write(media.join("Pictures/a.png"), b"aaaa").unwrap();
        fs::write(media.join("GIFs/b.gif"), vec![7u8; 3 * CHUNK + 5]).unwrap();
        fs::write(media.join("Videos/c.mp4"), b"cc").unwrap();
        let data = StoreData {
            items: vec![item(1, "Pictures/a.png"), item(2, "GIFs/b.gif"), item(3, "Videos/c.mp4")],
            history: vec![3, 1],
            next_id: 4,
            ..StoreData::default()
        };
        let dest = root.join("dest");
        fs::create_dir_all(&dest).unwrap();
        Plan { root: root.join("app"), media, data, dest, stamp: "2026-10-13 1620".into(), rename: no_replace }
    }

    /// A folder rename that never replaces (MoveFileEx without replace, on Windows).
    fn no_replace(from: &Path, to: &Path) -> io::Result<()> {
        if fs::symlink_metadata(to).is_ok() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        fs::rename(from, to)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut n: Vec<String> = fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
        n.sort();
        n
    }

    fn run_plan(plan: &Plan) -> (Progress, Vec<Step>) {
        let mut seen = Vec::new();
        let p = run(plan, fnv, &AtomicBool::new(false), &mut |s| seen.push(s.clone()));
        (p, seen)
    }

    #[test]
    fn copies_media_and_a_matching_library_file() {
        let root = temp("full");
        let plan = library(&root);
        let (p, seen) = run_plan(&plan);
        let Progress::Finished(Done { folder, items, others, bytes, left_out, links, leftovers, cached }) = p else { panic!("{p:?}") };
        assert_eq!(folder, fs::canonicalize(&plan.dest).unwrap().join("PocketMedia backup 2026-10-13 1620"));
        assert_eq!((items, others, bytes, left_out.len(), links, leftovers.len(), cached), (3, 0, 4 + 3 * CHUNK as u64 + 5 + 2, 0, 0, 0, 0));
        assert_eq!(names(&plan.dest), vec!["PocketMedia backup 2026-10-13 1620"], "no (unfinished) folder is left");
        let last = seen.last().unwrap();
        assert_eq!((last.done, last.total), (3, 3));
        for rel in ["Pictures/a.png", "GIFs/b.gif", "Videos/c.mp4"] {
            assert_eq!(fs::read(media_file(&folder.join("Media"), rel).unwrap()).unwrap(), fs::read(media_file(&plan.media, rel).unwrap()).unwrap(), "{rel}");
        }
        let data = crate::store::check_text(&fs::read_to_string(folder.join("Data").join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(data.items.len(), 3);
        assert_eq!(data.history, vec![3, 1]);
        assert_eq!(data.items[0].size, 4);
        assert!(!data.items[0].hash.is_empty(), "the checked hash is recorded");
        assert!(folder.join(RESTORE_NOTE).is_file());
        assert!(!folder.join("Cache").exists(), "caches are made again, not backed up");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_second_backup_never_overwrites_the_first() {
        let root = temp("twice");
        let plan = library(&root);
        let (Progress::Finished(Done { folder: first, .. }), _) = run_plan(&plan) else { panic!() };
        let (Progress::Finished(Done { folder: second, .. }), _) = run_plan(&plan) else { panic!() };
        assert_ne!(first, second);
        assert!(second.ends_with("PocketMedia backup 2026-10-13 1620 (2)"));
        assert!(first.join(RESTORE_NOTE).is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_destination_inside_pocketmedia_is_refused() {
        let root = temp("inside");
        let mut plan = library(&root);
        for dest in [plan.media.join("GIFs"), plan.root.clone(), plan.root.join("Data")] {
            plan.dest = dest.clone();
            let (p, _) = run_plan(&plan);
            assert!(matches!(&p, Progress::Failed(e) if e.starts_with("Choose a folder outside PocketMedia")), "{dest:?}: {p:?}");
        }
        assert_eq!(fs::read_dir(plan.media.join("GIFs")).unwrap().count(), 1, "nothing was written there");
        // A folder next to it whose name starts the same is outside.
        let beside = root.join("app backups");
        fs::create_dir_all(&beside).unwrap();
        plan.dest = beside;
        assert!(matches!(run_plan(&plan).0, Progress::Finished { .. }));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_destination_is_a_reported_failure() {
        let root = temp("missing");
        let mut plan = library(&root);
        plan.dest = root.join("not there");
        assert!(matches!(run_plan(&plan).0, Progress::Failed(e) if e.contains("could not be opened")));
        plan.dest = plan.media.join("Pictures/a.png");
        assert!(matches!(run_plan(&plan).0, Progress::Failed(_)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_removed_meanwhile_is_left_out_of_both_media_and_library() {
        let root = temp("gone");
        let plan = library(&root);
        fs::remove_file(plan.media.join("Pictures/a.png")).unwrap();
        let (Progress::Finished(Done { folder, items, left_out, .. }), _) = run_plan(&plan) else { panic!() };
        assert_eq!((items, left_out), (2, vec!["a.png".to_string()]));
        let data = crate::store::check_text(&fs::read_to_string(folder.join("Data").join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(data.items.iter().map(|i| i.id).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(data.history, vec![3], "history keeps only items in the backup");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn progress_counts_files_and_bytes_and_only_moves_forward() {
        let root = temp("progress");
        let mut plan = library(&root);
        for it in &mut plan.data.items {
            it.size = fs::metadata(media_file(&plan.media, &it.path).unwrap()).unwrap().len();
        }
        let total: u64 = plan.data.items.iter().map(|i| i.size).sum();
        let (p, seen) = run_plan(&plan);
        assert!(matches!(p, Progress::Finished { .. }), "{p:?}");
        assert!(seen.windows(2).all(|w| w[1].fraction() >= w[0].fraction()), "the bar never goes back");
        assert!(seen.iter().any(|s| s.current == "a.png" && !s.checking), "it names the file being copied");
        assert!(seen.iter().any(|s| s.checking), "and says when it checks a copy");
        let last = seen.last().unwrap();
        assert_eq!((last.copied, last.checked, last.bytes_total), (total, total, total));
        assert_eq!(last.fraction(), 1.0);
        assert_eq!(last.doing(), "Writing the library file…");
        assert!(last.counts().starts_with("3 of 3 files · "), "{}", last.counts());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn step_text_and_bar() {
        let s = Step { done: 120, total: 340, copied: 1288490189, checked: 0, bytes_total: 3650722201, current: "Happy dance.gif".into(), checking: false };
        assert_eq!(s.counts(), "120 of 340 files · 1.2 GB of 3.4 GB");
        assert_eq!(s.doing(), "Copying: Happy dance.gif");
        assert!((s.fraction() - (120.0 / 340.0 + 0.176) / 2.0).abs() < 0.01, "{}", s.fraction());
        let tiny_first = Step { done: 7, total: 156, copied: 8_900, checked: 8_900, bytes_total: 3_460_000, ..Step::default() };
        assert!(tiny_first.fraction() > 0.02, "small files first still move the bar");
        let over = Step { copied: 10, checked: 10, bytes_total: 5, ..Step::default() };
        assert_eq!(over.fraction(), 1.0, "a file that grew never pushes the bar past the end");
        assert!(over.counts().ends_with("10 bytes of 10 bytes"), "{}", over.counts());
    }

    #[test]
    fn cancel_removes_what_this_backup_made_and_nothing_else() {
        let root = temp("cancel");
        let plan = library(&root);
        fs::write(plan.dest.join("mine.txt"), b"keep").unwrap();
        let cancel = AtomicBool::new(false);
        let p = run(&plan, fnv, &cancel, &mut |s| {
            if s.done == 2 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert_eq!(p, Progress::Cancelled);
        let left: Vec<String> = fs::read_dir(&plan.dest).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
        assert_eq!(left, vec!["mine.txt"]);
        assert_eq!(fs::read_to_string(plan.dest.join("mine.txt")).unwrap(), "keep");
        assert!(plan.media.join("Pictures/a.png").is_file(), "the library is untouched");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_backup_restores_onto_another_install_path() {
        let root = temp("restore");
        let plan = library(&root);
        let (Progress::Finished(Done { folder, .. }), _) = run_plan(&plan) else { panic!() };
        // Another PocketMedia folder: the backup's Media and Data go beside its exe.
        let other = crate::paths::Paths::under(&root.join("elsewhere"));
        copy_tree(&folder.join("Media"), &other.media);
        copy_tree(&folder.join("Data"), &other.data);
        other.create().unwrap();
        let (store, outcome) = crate::store::Store::open(&other.data);
        assert_eq!(outcome, crate::store::LoadOutcome::Loaded);
        let records: Vec<(u32, String)> = store.data.items.iter().map(|i| (i.id, i.path.clone())).collect();
        let (orphans, missing) = crate::import::reconcile(&other.media, &records);
        assert_eq!((records.len(), orphans.len(), missing.len()), (3, 0, 0));
        let _ = fs::remove_dir_all(&root);
    }

    fn copy_tree(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for e in fs::read_dir(from).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                copy_tree(&e.path(), &to.join(e.file_name()));
            } else {
                fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }

    /// The uninstaller deletes everything in Media, so the backup
    /// holds everything in Media: subfolders, other file types, loose files.
    #[test]
    fn the_whole_media_folder_is_backed_up_not_only_library_items() {
        let root = temp("tree");
        let plan = library(&root);
        let extra = [
            ("Pictures/Trip/day 1/x.png", b"sub".to_vec()),
            ("Pictures/y.webp", b"webp".to_vec()),
            ("loose note.txt", b"loose".to_vec()),
            ("Videos/clip.mov", vec![9u8; CHUNK + 1]),
        ];
        for (rel, bytes) in &extra {
            let path = media_file(&plan.media, rel).unwrap();
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        fs::create_dir_all(plan.media.join("Pictures/empty folder")).unwrap();
        // Temporary files of an interrupted import are not saved.
        fs::write(plan.media.join("Pictures/.pocketmedia-7-1.partial"), b"half").unwrap();
        fs::write(plan.media.join("GIFs/download.gif.PARTIAL"), b"half").unwrap();
        let (p, seen) = run_plan(&plan);
        let Progress::Finished(d) = p else { panic!("{p:?}") };
        assert_eq!((d.items, d.others), (3, 4));
        let backup = d.folder.join("Media");
        for (rel, bytes) in &extra {
            assert_eq!(&fs::read(media_file(&backup, rel).unwrap()).unwrap(), bytes, "{rel}");
        }
        assert!(backup.join("Pictures/empty folder").is_dir(), "empty folders come along");
        assert!(!backup.join("Pictures/.pocketmedia-7-1.partial").exists());
        assert!(!backup.join("GIFs/download.gif.PARTIAL").exists());
        // The library file still lists only library items.
        let data = crate::store::check_text(&fs::read_to_string(d.folder.join("Data").join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(data.items.len(), 3);
        assert_eq!(seen.last().unwrap().total, 7, "progress counts every file");
        let line = describe(&Progress::Finished(d));
        assert!(line.starts_with("Backup finished and checked: 3 library items + 4 other files ("), "{line}");
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn links_in_media_are_counted_but_never_followed() {
        let root = temp("links");
        let plan = library(&root);
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("not mine.png"), b"x").unwrap();
        std::os::unix::fs::symlink(&outside, plan.media.join("Pictures/linked folder")).unwrap();
        std::os::unix::fs::symlink(outside.join("not mine.png"), plan.media.join("linked.png")).unwrap();
        let (Progress::Finished(d), _) = run_plan(&plan) else { panic!() };
        assert_eq!((d.items, d.others, d.links), (3, 0, 2));
        assert!(fs::symlink_metadata(d.folder.join("Media/Pictures/linked folder")).is_err());
        assert!(fs::symlink_metadata(d.folder.join("Media/linked.png")).is_err());
        let line = describe(&Progress::Finished(d));
        assert!(line.contains("2 links in Media were not followed"), "{line}");
        let _ = fs::remove_dir_all(&root);
    }

    /// The same on Windows, with a real junction (`mklink /J`), as File Explorer
    /// users and the installer test make them.
    #[cfg(windows)]
    #[test]
    fn junctions_in_media_are_never_followed() {
        let root = temp("junction");
        let plan = library(&root);
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("not mine.png"), b"x").unwrap();
        let link = plan.media.join("Pictures").join("linked folder");
        let made = std::process::Command::new("cmd").arg("/c").arg("mklink").arg("/J").arg(&link).arg(&outside).output().unwrap();
        assert!(made.status.success() && fs::symlink_metadata(&link).is_ok(), "mklink /J failed: {made:?}");
        let (Progress::Finished(d), _) = run_plan(&plan) else { panic!() };
        assert_eq!((d.items, d.others, d.links), (3, 0, 1));
        assert!(fs::symlink_metadata(d.folder.join("Media/Pictures/linked folder")).is_err());
        assert!(outside.join("not mine.png").is_file(), "what it points to is untouched");
        let _ = fs::remove_dir(&link);
        let _ = fs::remove_dir_all(&root);
    }

    /// Until every copy is checked the folder says "(unfinished)",
    /// so a backup cut off half-way (the process ended) never looks finished.
    #[test]
    fn an_interrupted_backup_leaves_only_an_unfinished_folder() {
        let root = temp("interrupted");
        let plan = library(&root);
        let dest = fs::canonicalize(&plan.dest).unwrap();
        let mut during = Vec::new();
        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(&plan, fnv, &AtomicBool::new(false), &mut |s| {
                if s.done == 1 && during.is_empty() {
                    during = names(&dest);
                }
                if s.done == 2 {
                    panic!("the process ends here");
                }
            })
        }));
        assert!(ended.is_err());
        assert_eq!(during, vec!["PocketMedia backup 2026-10-13 1620 (unfinished)"], "while it runs");
        assert_eq!(names(&dest), vec!["PocketMedia backup 2026-10-13 1620 (unfinished)"], "after it was cut off");
        assert!(!dest.join("PocketMedia backup 2026-10-13 1620 (unfinished)").join(RESTORE_NOTE).exists());

        // The next backup leaves the leftover alone, takes a free name and mentions it.
        let (Progress::Finished(d), _) = run_plan(&plan) else { panic!() };
        assert_eq!(d.folder, dest.join("PocketMedia backup 2026-10-13 1620"));
        assert_eq!(d.leftovers, vec!["PocketMedia backup 2026-10-13 1620 (unfinished)"]);
        assert_eq!(names(&dest), vec!["PocketMedia backup 2026-10-13 1620", "PocketMedia backup 2026-10-13 1620 (unfinished)"]);
        let line = describe(&Progress::Finished(d));
        assert!(line.contains("An unfinished backup from before is also in that folder"), "{line}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_finished_name_taken_meanwhile_is_never_replaced() {
        let root = temp("taken");
        let plan = library(&root);
        let dest = fs::canonicalize(&plan.dest).unwrap();
        // Someone makes "PocketMedia backup … " while this backup runs.
        let p = run(&plan, fnv, &AtomicBool::new(false), &mut |s| {
            if s.done == 1 {
                let _ = fs::create_dir(dest.join("PocketMedia backup 2026-10-13 1620"));
                let _ = fs::write(dest.join("PocketMedia backup 2026-10-13 1620").join("theirs.txt"), b"keep");
            }
        });
        let Progress::Finished(d) = p else { panic!("{p:?}") };
        assert_eq!(d.folder, dest.join("PocketMedia backup 2026-10-13 1620 (2)"));
        assert_eq!(names(&dest.join("PocketMedia backup 2026-10-13 1620")), vec!["theirs.txt"]);
        let _ = fs::remove_dir_all(&root);
    }

    static REFUSALS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// Like Windows while a virus scanner still holds a new file: refuses twice.
    fn busy_then_free(from: &Path, to: &Path) -> io::Result<()> {
        if REFUSALS.fetch_add(1, Ordering::SeqCst) < 2 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        no_replace(from, to)
    }

    #[test]
    fn a_folder_held_for_a_moment_still_gets_its_final_name() {
        let root = temp("held");
        let mut plan = library(&root);
        plan.rename = busy_then_free;
        let (p, _) = run_plan(&plan);
        let Progress::Finished(d) = p else { panic!("{p:?}") };
        assert!(d.folder.ends_with("PocketMedia backup 2026-10-13 1620"));
        assert_eq!(REFUSALS.load(Ordering::SeqCst), 3);
        assert_eq!(names(&plan.dest), vec!["PocketMedia backup 2026-10-13 1620"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unfinished_names_are_recognised() {
        assert!(is_unfinished("PocketMedia backup 2026-10-13 1620 (unfinished)"));
        assert!(is_unfinished("PocketMedia backup 2026-10-13 1620 (2) (unfinished)"));
        assert!(!is_unfinished("PocketMedia backup 2026-10-13 1620"));
        assert!(!is_unfinished("Holiday (unfinished)"));
    }

    #[test]
    fn result_line_counts_items_and_other_files() {
        let one = Done { folder: PathBuf::from("x/PocketMedia backup 1"), items: 1, others: 1, bytes: 2048, ..Done::default() };
        assert_eq!(
            describe(&Progress::Finished(one)),
            "Backup finished and checked: 1 library item + 1 other file (2.0 KB) in \"PocketMedia backup 1\"."
        );
        let gone = Done { items: 2, left_out: vec!["a.png".into()], ..Done::default() };
        assert!(describe(&Progress::Finished(gone)).contains("2 library items + 0 other files"));
        // "checked" only when every copy was read back from the drive itself.
        let cache = Done { folder: PathBuf::from("PocketMedia backup 1"), items: 1, cached: 2, ..Done::default() };
        let line = describe(&Progress::Finished(cache));
        assert!(line.starts_with("Backup finished, copied and flushed: 1 library item"), "{line}");
        assert!(line.contains("did not allow reading 2 files back from the drive itself, so they were checked only against"), "{line}");
        assert!(!line.contains("checked:"), "{line}");
    }

    #[test]
    fn inside_compares_whole_folder_names_ignoring_case() {
        assert!(inside(Path::new("/x/App/Media"), Path::new("/x/app")));
        assert!(inside(Path::new("/x/app"), Path::new("/x/app")));
        assert!(!inside(Path::new("/x/app backups"), Path::new("/x/app")));
        assert!(!inside(Path::new("/x"), Path::new("/x/app")));
    }

    #[test]
    fn sizes_read_plainly() {
        assert_eq!(size_text(900), "900 bytes");
        assert_eq!(size_text(2048), "2.0 KB");
        assert_eq!(size_text(5 << 30), "5.0 GB");
    }
}
