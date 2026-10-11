//! Phases 1-2 only: fake in-memory data from the wireframe. No files.
//! Phase 3 replaces this with the real library.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MediaKind {
    Gif,
    Picture,
    Video,
}

impl MediaKind {
    pub fn badge(self) -> &'static str {
        match self {
            MediaKind::Gif => "GIF · loop",
            MediaKind::Picture => "Picture",
            MediaKind::Video => "Video",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub id: u32,
    pub name: String,
    pub kind: MediaKind,
    /// Pixel size; 0 when unknown (fake imports have no real file to read).
    pub width: u32,
    pub height: u32,
}

pub fn items() -> Vec<Item> {
    use MediaKind::*;
    [
        (1, "Happy dance.gif", Gif, 100, 100),
        (2, "Square.png", Picture, 300, 300),
        (3, "Race clip.mp4", Video, 1280, 720),
        (4, "Hello.gif", Gif, 300, 300),
        (5, "Wallpaper.jpg", Picture, 1920, 1080),
        (6, "Celebration.mp4", Video, 720, 1280),
        (7, "Portrait.png", Picture, 600, 1200),
        (8, "Reaction.gif", Gif, 400, 200),
    ]
    .into_iter()
    .map(|(id, name, kind, width, height)| Item { id, name: name.into(), kind, width, height })
    .collect()
}

/// Recently used order, most recent first (as in the wireframe).
pub fn recent() -> Vec<u32> {
    vec![1, 2, 3, 6, 7, 4, 5, 8]
}

/// Phase 2a only: the files "chosen" in Add media (the wireframe's example batch):
/// (source name, kind, status, automatic new name, auto-renamed).
pub fn import_rows() -> Vec<(&'static str, MediaKind, crate::add_media::RowStatus, Option<&'static str>, bool)> {
    use crate::add_media::RowStatus::{Copy, Skip};
    use MediaKind::*;
    vec![
        ("Cat.png", Picture, Copy, None, false),
        ("Wave.gif", Gif, Copy, None, false),
        ("Clip.mp4", Video, Copy, Some("Clip (2).mp4"), true),
        ("IMG_4821.png", Picture, Copy, None, false),
        ("Portrait.png", Picture, Skip, None, false),
    ]
}
