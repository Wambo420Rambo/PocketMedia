//! The disk cache of thumbnails, in `Cache\thumbs-v1`. One file per item and
//! box size, stamped with the source file's size and modification time, so a
//! changed source, another display size or DPI, or another cache format (the
//! folder name) is simply a miss. Damaged entries are deleted and made again.
//!
//! The cache is capped: when a write takes it over the cap, the entries used
//! least recently are deleted. That happens during cache work only, never on a
//! timer, and only ever touches files in this folder.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::pixels::Pixels;

pub const DIR: &str = "thumbs-v1";
pub const CAP: u64 = 128 * 1024 * 1024;
const MAGIC: &[u8; 4] = b"PMT1";
const HEADER: usize = 4 + 8 + 8 + 4 + 4;
const EXT: &str = "thumb";

/// What identifies the source file's content well enough for a cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub size: u64,
    pub modified: u64,
}

pub fn stamp(path: &Path) -> io::Result<Stamp> {
    let m = fs::metadata(path)?;
    let modified = m.modified()?.duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    Ok(Stamp { size: m.len(), modified })
}

/// A cached result: the thumbnail, or a note that the file has no preview
/// (so an unsupported video is not tried again on every start).
#[derive(Debug, PartialEq, Eq)]
pub enum Cached {
    Thumb(Pixels),
    NoPreview,
}

pub fn file_name(id: u32, box_w: u32, box_h: u32) -> String {
    format!("{id}-{box_w}x{box_h}.{EXT}")
}

pub fn encode(stamp: Stamp, entry: &Cached) -> Vec<u8> {
    let (w, h, data): (u32, u32, &[u8]) = match entry {
        Cached::Thumb(p) => (p.width, p.height, &p.data),
        Cached::NoPreview => (0, 0, &[]),
    };
    let mut b = Vec::with_capacity(HEADER + data.len());
    b.extend(MAGIC);
    b.extend(stamp.size.to_le_bytes());
    b.extend(stamp.modified.to_le_bytes());
    b.extend(w.to_le_bytes());
    b.extend(h.to_le_bytes());
    b.extend(data);
    b
}

/// The entry, if `bytes` is a complete entry for a source with `stamp`.
pub fn decode(bytes: &[u8], stamp: Stamp) -> Option<Cached> {
    if bytes.len() < HEADER || &bytes[..4] != MAGIC {
        return None;
    }
    let u64_at = |i: usize| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap_or_default());
    let u32_at = |i: usize| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap_or_default());
    if (Stamp { size: u64_at(4), modified: u64_at(12) }) != stamp {
        return None;
    }
    let (w, h) = (u32_at(20), u32_at(24));
    let data = &bytes[HEADER..];
    if w == 0 && h == 0 && data.is_empty() {
        return Some(Cached::NoPreview);
    }
    (w > 0 && h > 0 && data.len() as u64 == w as u64 * h as u64 * 4)
        .then(|| Cached::Thumb(Pixels { width: w, height: h, data: data.to_vec() }))
}

struct Entry {
    size: u64,
    used: u64,
}

pub struct DiskCache {
    dir: PathBuf,
    cap: u64,
    /// Read lazily, at the first write.
    index: Option<HashMap<String, Entry>>,
    total: u64,
    clock: u64,
}

impl DiskCache {
    pub fn new(dir: PathBuf, cap: u64) -> Self {
        Self { dir, cap, index: None, total: 0, clock: 0 }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    pub fn read(&mut self, name: &str, stamp: Stamp) -> Option<Cached> {
        let path = self.dir.join(name);
        let bytes = fs::read(&path).ok()?;
        match decode(&bytes, stamp) {
            Some(c) => {
                let t = self.tick();
                if let Some(e) = self.index.as_mut().and_then(|i| i.get_mut(name)) {
                    e.used = t;
                }
                Some(c)
            }
            None => {
                // Stale or damaged: made again by the caller.
                self.forget(name);
                let _ = fs::remove_file(&path);
                None
            }
        }
    }

    fn forget(&mut self, name: &str) {
        if let Some(e) = self.index.as_mut().and_then(|i| i.remove(name)) {
            self.total -= e.size;
        }
    }

    /// The entries already on disk; the oldest file counts as least recently used.
    fn load_index(&mut self) {
        if self.index.is_some() {
            return;
        }
        let mut found: Vec<(String, u64, std::time::SystemTime)> = Vec::new();
        if let Ok(dir) = fs::read_dir(&self.dir) {
            for e in dir.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let Ok(m) = e.metadata() else { continue };
                if !m.is_file() {
                    continue;
                }
                if name.ends_with(".tmp") {
                    let _ = fs::remove_file(e.path()); // a write that never finished
                } else if name.ends_with(EXT) {
                    found.push((name, m.len(), m.modified().unwrap_or(UNIX_EPOCH)));
                }
            }
        }
        found.sort_by_key(|f| f.2);
        let mut index = HashMap::new();
        for (name, size, _) in found {
            let used = self.tick();
            self.total += size;
            index.insert(name, Entry { size, used });
        }
        self.index = Some(index);
    }

    /// Writes an entry (through a temporary file), then trims the cache to its cap.
    pub fn write(&mut self, name: &str, stamp: Stamp, entry: &Cached) -> io::Result<()> {
        self.load_index();
        fs::create_dir_all(&self.dir)?;
        let bytes = encode(stamp, entry);
        let tmp = self.dir.join(format!("{name}.tmp"));
        fs::write(&tmp, &bytes)?;
        if let Err(e) = fs::rename(&tmp, self.dir.join(name)) {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
        self.forget(name);
        let used = self.tick();
        if let Some(i) = self.index.as_mut() {
            i.insert(name.to_string(), Entry { size: bytes.len() as u64, used });
        }
        self.total += bytes.len() as u64;
        self.trim(name);
        Ok(())
    }

    /// Deletes least recently used entries (never `keep`) until the cache is
    /// back under 7/8 of its cap.
    fn trim(&mut self, keep: &str) {
        if self.total <= self.cap {
            return;
        }
        let target = self.cap / 8 * 7;
        let mut order: Vec<(u64, String)> = match &self.index {
            Some(i) => i.iter().filter(|(n, _)| n.as_str() != keep).map(|(n, e)| (e.used, n.clone())).collect(),
            None => return,
        };
        order.sort();
        for (_, name) in order {
            if self.total <= target {
                break;
            }
            if fs::remove_file(self.dir.join(&name)).is_ok() || !self.dir.join(&name).exists() {
                self.forget(&name);
            }
        }
    }

    #[cfg(test)]
    pub fn total(&self) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pm-thumbstore-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn thumb(w: u32, h: u32, v: u8) -> Cached {
        Cached::Thumb(Pixels { width: w, height: h, data: vec![v; (w * h * 4) as usize] })
    }

    const STAMP: Stamp = Stamp { size: 1234, modified: 99 };

    #[test]
    fn entries_round_trip_and_check_their_source() {
        let t = thumb(3, 2, 7);
        let bytes = encode(STAMP, &t);
        assert_eq!(decode(&bytes, STAMP), Some(t));
        assert_eq!(decode(&bytes, Stamp { size: 1234, modified: 100 }), None, "source changed");
        assert_eq!(decode(&bytes[..bytes.len() - 1], STAMP), None, "cut short");
        assert_eq!(decode(b"junk", STAMP), None);
        assert_eq!(decode(&encode(STAMP, &Cached::NoPreview), STAMP), Some(Cached::NoPreview));
    }

    #[test]
    fn a_stale_or_damaged_entry_is_deleted() {
        let dir = temp_dir("stale");
        let mut c = DiskCache::new(dir.clone(), CAP);
        let name = file_name(5, 128, 72);
        c.write(&name, STAMP, &thumb(2, 2, 1)).unwrap();
        assert_eq!(c.read(&name, STAMP), Some(thumb(2, 2, 1)));
        assert_eq!(c.read(&name, Stamp { size: 1, modified: 1 }), None);
        assert!(!dir.join(&name).exists());
        assert_eq!(c.total(), 0);
        fs::write(dir.join(&name), b"PMT1 damaged").unwrap();
        assert_eq!(c.read(&name, STAMP), None);
        assert!(!dir.join(&name).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cap_deletes_the_least_recently_used() {
        let dir = temp_dir("cap");
        let one = encode(STAMP, &thumb(4, 4, 0)).len() as u64;
        let mut c = DiskCache::new(dir.clone(), one * 3);
        for id in 1..=3 {
            c.write(&file_name(id, 8, 8), STAMP, &thumb(4, 4, id as u8)).unwrap();
        }
        assert!(c.read(&file_name(1, 8, 8), STAMP).is_some()); // 1 is now the newest
        c.write(&file_name(4, 8, 8), STAMP, &thumb(4, 4, 4)).unwrap();
        assert!(c.total() <= one * 3);
        assert!(dir.join(file_name(1, 8, 8)).exists(), "recently read entry kept");
        assert!(!dir.join(file_name(2, 8, 8)).exists(), "least recently used deleted");
        assert!(dir.join(file_name(4, 8, 8)).exists(), "the new entry kept");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn existing_entries_count_and_leftover_temp_files_go() {
        let dir = temp_dir("existing");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(file_name(9, 8, 8)), encode(STAMP, &thumb(1, 1, 9))).unwrap();
        fs::write(dir.join("x.thumb.tmp"), b"half").unwrap();
        fs::write(dir.join("notes.txt"), b"not ours").unwrap();
        let mut c = DiskCache::new(dir.clone(), CAP);
        c.write(&file_name(10, 8, 8), STAMP, &Cached::NoPreview).unwrap();
        assert_eq!(c.total(), (HEADER as u64 + 4) + HEADER as u64);
        assert!(!dir.join("x.thumb.tmp").exists());
        assert!(dir.join("notes.txt").exists(), "files that are not cache entries are left alone");
        let _ = fs::remove_dir_all(&dir);
    }
}
