//! The media item types, plus the wireframe's 8 example items. The
//! app shows the real library; the examples remain for unit tests and as the
//! files the self-test creates (`selftest/seed.rs`).

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

impl MediaKind {
    /// The folder under Media that holds this kind.
    pub fn folder(self) -> &'static str {
        match self {
            MediaKind::Gif => "GIFs",
            MediaKind::Picture => "Pictures",
            MediaKind::Video => "Videos",
        }
    }
}

/// One media file in the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: u32,
    /// File name with extension, as shown and searched.
    pub name: String,
    pub kind: MediaKind,
    /// Pixel size; 0 when unknown.
    pub width: u32,
    pub height: u32,
    /// Relative to the Media folder, `/`-separated: `Pictures/Cat.png`.
    pub path: String,
    pub size: u64,
    /// SHA-256 as hex; empty until needed (hashing happens only during import).
    pub hash: String,
    /// Seconds since 1970 when it was added.
    pub added: u64,
    /// Where it was imported from (information only).
    pub source: Option<String>,
}

impl Item {
    /// An item stored at `<kind folder>/<name>`.
    pub fn new(id: u32, name: &str, kind: MediaKind, width: u32, height: u32) -> Self {
        Item {
            id,
            name: name.into(),
            kind,
            width,
            height,
            path: format!("{}/{name}", kind.folder()),
            size: 0,
            hash: String::new(),
            added: 0,
            source: None,
        }
    }
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
    .map(|(id, name, kind, width, height)| Item::new(id, name, kind, width, height))
    .collect()
}

/// Recently used order, most recent first (as in the wireframe).
pub fn recent() -> Vec<u32> {
    vec![1, 2, 3, 6, 7, 4, 5, 8]
}
