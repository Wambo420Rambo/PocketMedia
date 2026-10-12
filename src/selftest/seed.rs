//! Self-test only: real files to work with, inside the test's own folders
//! (`test-results\app` and `test-results\import-src`). Nothing outside
//! `test-results` is touched.
//!
//! - The library: the wireframe's 8 items as real files with the right pixel
//!   sizes in their headers, so every earlier screen check still applies.
//! - An import source with mixed files, duplicates and subfolders.

use std::path::{Path, PathBuf};

use crate::import::FOLDERS;
use crate::library::{self, Library};
use crate::media_info::fixtures;
use crate::sample::Item;

/// A solid-colour PNG (real and decodable), different for each `seed`.
fn png(w: u32, h: u32, seed: u32) -> Vec<u8> {
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
    let mut lib = Library::sample();
    for item in &mut lib.items {
        let bytes = item_bytes(item);
        item.size = bytes.len() as u64;
        std::fs::write(crate::import::media_file(&p.media, &item.path), bytes).map_err(|e| e.to_string())?;
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
