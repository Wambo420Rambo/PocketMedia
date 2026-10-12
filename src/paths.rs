//! Where PocketMedia keeps its files: `Media\Pictures`, `Media\GIFs`,
//! `Media\Videos`, `Data` and `Cache`, all beside the executable.
//! Created at startup; existing folders and their contents are left alone.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub root: PathBuf,
    pub media: PathBuf,
    pub pictures: PathBuf,
    pub gifs: PathBuf,
    pub videos: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl Paths {
    pub fn under(root: &Path) -> Self {
        let media = root.join("Media");
        Self {
            root: root.to_path_buf(),
            pictures: media.join("Pictures"),
            gifs: media.join("GIFs"),
            videos: media.join("Videos"),
            media,
            data: root.join("Data"),
            cache: root.join("Cache"),
        }
    }

    /// The folder that holds the running executable.
    pub fn beside_exe() -> io::Result<Self> {
        let exe = std::env::current_exe()?;
        let dir = exe.parent().ok_or_else(|| io::Error::other("the program has no parent folder"))?;
        Ok(Self::under(dir))
    }

    /// Every folder PocketMedia needs, parents first.
    pub fn all(&self) -> [&Path; 6] {
        [&self.media, &self.pictures, &self.gifs, &self.videos, &self.data, &self.cache]
    }

    /// Creates any missing folder. On failure, says which folder and why.
    pub fn create(&self) -> Result<(), (PathBuf, io::Error)> {
        for dir in self.all() {
            std::fs::create_dir_all(dir).map_err(|e| (dir.to_path_buf(), e))?;
            if !dir.is_dir() {
                return Err((dir.to_path_buf(), io::Error::other("a file with this name is in the way")));
            }
        }
        Ok(())
    }
}

pub const FAILURE_TITLE: &str = "PocketMedia - cannot create folders";

/// The message shown when the folders cannot be created.
pub fn failure_message(dir: &Path, e: &io::Error) -> String {
    format!(
        "PocketMedia could not create its folder\n\n{}\n\n{e}\n\n\
         PocketMedia keeps your media next to the program. Move it to a folder you can write to, \
         such as C:\\PocketMedia, and start it again.",
        dir.display()
    )
}

static PATHS: OnceLock<Paths> = OnceLock::new();

/// Set once at startup, after the folders exist.
pub fn init(paths: Paths) {
    let _ = PATHS.set(paths);
}

pub fn get() -> Option<&'static Paths> {
    PATHS.get()
}

/// The Media folder for display ("" before startup has set it).
pub fn media_text() -> String {
    get().map(|p| p.media.display().to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pm-paths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn creates_the_layout_from_goal_md() {
        let root = temp("layout");
        let p = Paths::under(&root);
        p.create().unwrap();
        for rel in ["Media/Pictures", "Media/GIFs", "Media/Videos", "Data", "Cache"] {
            assert!(root.join(rel).is_dir(), "{rel}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn existing_folders_and_files_are_kept() {
        let root = temp("keep");
        let p = Paths::under(&root);
        p.create().unwrap();
        std::fs::write(p.pictures.join("cat.png"), b"x").unwrap();
        p.create().unwrap();
        assert_eq!(std::fs::read(p.pictures.join("cat.png")).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_in_the_way_names_the_folder() {
        let root = temp("blocked");
        std::fs::write(root.join("Media"), b"not a folder").unwrap();
        let (dir, e) = Paths::under(&root).create().unwrap_err();
        assert_eq!(dir, root.join("Media"));
        let msg = failure_message(&dir, &e);
        assert!(msg.contains(&root.join("Media").display().to_string()));
        assert_eq!(std::fs::read(root.join("Media")).unwrap(), b"not a folder", "the file is not touched");
        let _ = std::fs::remove_dir_all(&root);
    }
}
