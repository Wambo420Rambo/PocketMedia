//! Self-test only: real files to work with, inside the test's own folders
//! (`test-results\app` and `test-results\import-src`). Nothing outside
//! `test-results` is touched.
//!
//! - The library: the wireframe's 8 items as real files with the right pixel
//!   sizes, so every earlier screen check still applies. They are
//!   real media: the GIFs animate, the JPEG and the MP4s decode (made once with
//!   Windows' own encoders and kept in `test-results\seed-media-v1`).
//! - An import source with mixed files, duplicates and subfolders.

use std::path::{Path, PathBuf};

use crate::import::FOLDERS;
use crate::library::{self, Library};
use crate::media_info::fixtures;
use crate::sample::Item;

/// A solid-colour PNG (real and decodable), different for each `seed`.
pub(super) fn png(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let c = [(seed * 53 % 256) as u8, (seed * 97 % 256) as u8, (seed * 151 % 256) as u8, 255];
    let rgba: Vec<u8> = c.iter().copied().cycle().take((w * h * 4) as usize).collect();
    crate::test_png::encode_rgba(w, h, &rgba)
}

/// The bytes for one of the wireframe's items.
fn item_bytes(item: &Item) -> Vec<u8> {
    let (w, h) = (item.width, item.height);
    match crate::library::extension(&item.name).to_ascii_lowercase().as_str() {
        ".png" => png(w, h, item.id),
        ".jpg" => fixtures::jpeg(w as u16, h as u16, &item.name),
        ".gif" => fixtures::gif(w as u16, h as u16, &item.name),
        _ => fixtures::mp4(w as u16, h as u16, &item.name),
    }
}

/// The seed files are made once and kept here (bump the version when the recipe changes).
const SEED_DIR: &str = "seed-media-v1";

/// A looping GIF whose whole canvas is red, blue, then green, 300 ms each.
pub fn animated_gif(w: u32, h: u32, tag: &str) -> Vec<u8> {
    use fixtures::GifFrame;
    let (w, h) = (w as u16, h as u16);
    let frames: Vec<GifFrame> = [1, 2, 3].iter().map(|&c| GifFrame::solid(0, 0, w, h, c, 30, 1, None)).collect();
    fixtures::gif_frames(w, h, &frames, tag)
}

/// The bytes of a made file, made into `dir` the first time.
fn made(dir: &Path, name: &str, make: impl FnOnce(&Path) -> Result<(), String>) -> Option<Vec<u8>> {
    let path = dir.join(name);
    if !path.exists() {
        std::fs::create_dir_all(dir).ok()?;
        // Keeps the extension: the MP4 writer picks its container by it.
        let tmp = dir.join(format!("tmp-{name}"));
        if let Err(e) = make(&tmp) {
            crate::selftest::note(&format!("could not make {name}: {e} (using a header-only stand-in)"));
            let _ = std::fs::remove_file(&tmp);
            return None;
        }
        std::fs::rename(&tmp, &path).ok()?;
    }
    std::fs::read(&path).ok()
}

/// The real media for one of the wireframe's items.
fn seed_bytes(item: &Item, dir: &Path) -> Vec<u8> {
    let (w, h) = (item.width, item.height);
    match crate::library::extension(&item.name).to_ascii_lowercase().as_str() {
        ".gif" => animated_gif(w, h, &item.name),
        ".jpg" => made(dir, &item.name, |p| super::media_gen::jpeg(p, w, h, item.id)).unwrap_or_else(|| item_bytes(item)),
        ".mp4" => made(dir, &item.name, |p| super::media_gen::mp4(p, w, h, 30, 15, item.id)).unwrap_or_else(|| item_bytes(item)),
        _ => item_bytes(item),
    }
}

/// True when the seed videos are real H.264 files (this Windows has the encoder).
pub fn real_videos(out: &Path) -> bool {
    std::fs::read(out.join(SEED_DIR).join("Race clip.mp4")).is_ok_and(|b| b.len() > 4096)
}

/// Replaces the test library with the 8 example files. Refuses to run anywhere
/// but the self-test's own folder.
pub fn reset_library() -> Result<(), String> {
    let p = crate::paths::get().ok_or("no folders")?;
    if p.root.file_name().and_then(|n| n.to_str()) != Some("app") {
        return Err(format!("refusing to reset {}", p.root.display()));
    }
    for folder in FOLDERS {
        let dir = p.media.join(folder);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    let seed_dir = p.root.parent().map(|o| o.join(SEED_DIR)).ok_or("no results folder")?;
    let mut lib = Library::sample();
    for item in &mut lib.items {
        let bytes = seed_bytes(item, &seed_dir);
        item.size = bytes.len() as u64;
        let file = crate::import::media_file(&p.media, &item.path).ok_or("a seed path is not plain")?;
        std::fs::write(file, bytes).map_err(|e| e.to_string())?;
    }
    library::set(lib);
    library::persist()
}

/// One file of the import source: path relative to the source folder, and its bytes.
pub fn source_files() -> Vec<(&'static str, Vec<u8>)> {
    let portrait = Library::sample().items.into_iter().find(|i| i.name == "Portrait.png").map(|i| item_bytes(&i)).unwrap_or_default();
    let wave = fixtures::gif(300, 300, "wave source");
    vec![
        ("Cat.png", png(64, 48, 101)),
        ("Wave.gif", wave.clone()),
        ("Clip.mp4", fixtures::mp4(1280, 720, "clip source")),
        ("IMG_4821.png", png(40, 30, 102)),
        // Identical to the library's Portrait.png: skipped as a duplicate.
        ("Portrait.png", portrait),
        // Same name as the library's Hello.gif, different content: becomes "Hello (2)".
        ("Hello.gif", fixtures::gif(300, 300, "another hello")),
        ("more/notes.txt", b"not media".to_vec()),
        // Identical to Wave.gif above: a duplicate inside the batch.
        ("more/Wave.gif", wave),
        ("more/deep/Clip.mp4", fixtures::mp4(640, 360, "a different clip")),
    ]
}

/// The order "Choose files" returns (as if picked in the dialog).
pub const PICKED: [&str; 6] = ["Cat.png", "Wave.gif", "Clip.mp4", "IMG_4821.png", "Portrait.png", "Hello.gif"];

/// Writes the import source and returns (picked files, the folder).
pub fn make_source(out: &Path) -> std::io::Result<(Vec<PathBuf>, PathBuf)> {
    let dir = out.join("import-src");
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, bytes) in source_files() {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
    }
    Ok((PICKED.iter().map(|n| dir.join(n)).collect(), dir))
}

/// A junction inside the import source,
/// `import-src\more\linked`, that leads to `junction-target\Outside.png` outside
/// it. A folder import must not follow it. Returns what was made, for the log.
pub fn make_junction(out: &Path) -> Result<String, String> {
    let target = out.join("junction-target");
    let _ = std::fs::remove_dir_all(&target);
    std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
    std::fs::write(target.join(JUNCTION_FILE), png(30, 20, 103)).map_err(|e| e.to_string())?;
    let link = junction(out);
    let made = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(&link)
        .arg(&target)
        .output()
        .map_err(|e| format!("mklink could not run: {e}"))?;
    let said = String::from_utf8_lossy(&made.stdout).trim().to_string() + &String::from_utf8_lossy(&made.stderr);
    if !made.status.success() {
        return Err(format!("mklink /J failed: {said}"));
    }
    Ok(said)
}

/// The junction `make_junction` makes, and the file it leads to.
pub fn junction(out: &Path) -> PathBuf {
    out.join("import-src").join("more").join("linked")
}
pub const JUNCTION_FILE: &str = "Outside.png";

/// True when every source file still has exactly the bytes it was made with.
pub fn source_unchanged(out: &Path) -> Result<(), String> {
    let dir = out.join("import-src");
    for (rel, bytes) in source_files() {
        match std::fs::read(dir.join(rel)) {
            Ok(b) if b == bytes => {}
            Ok(_) => return Err(format!("{rel} changed")),
            Err(e) => return Err(format!("{rel}: {e}")),
        }
    }
    Ok(())
}
