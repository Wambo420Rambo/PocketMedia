//! Phase 1 only: the eight fake in-memory items from the wireframe. No files.
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
