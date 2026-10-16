//! Quick popup geometry in DIPs (the search box is the header,
//! a segmented type control and All media / Recent tabs under it, the gallery,
//! and a footer with Clear history, Manager and Add). Also the Rect, text
//! styles and the tile grid shared by every window.

/// Default popup size.
pub const WIDTH: f32 = 520.0;
pub const HEIGHT: f32 = 620.0;
pub const COLUMNS: usize = 3;

const PAD: f32 = 14.0;
const SEARCH_H: f32 = 48.0;
const LOGO_D: f32 = 28.0;
const CLOSE: f32 = 36.0;
const ROW2_H: f32 = 34.0;
const GALLERY_PAD_RIGHT: f32 = 8.0;
/// Space between tiles; each tile also has TILE_PAD inside it, so wells are 2 x TILE_PAD + GAP apart.
const GAP: f32 = 4.0;
const TILE_PAD: f32 = 5.0;
const THUMB_H: f32 = 96.0;
const NAME_LINE: f32 = 12.0 * 1.45;
const FOOTER_H: f32 = 52.0;
const FOOTER_BUTTON_H: f32 = 32.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px < self.right() && py >= self.y && py < self.bottom()
    }
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, (self.w - 2.0 * d).max(0.0), (self.h - 2.0 * d).max(0.0))
    }
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextStyle {
    /// 14px medium: item counts, card titles.
    Medium,
    /// 14px regular: buttons, search text, settings rows.
    Body,
    /// 12px: chips, select, names, footer, hints.
    Small,
    /// 11px: badges on tiles.
    Tiny,
    /// 16px medium: empty-state heading.
    Heading,
    /// 20px semibold: panel and dialog titles ("Add media", "Change shortcut").
    Title,
    /// 13px: names in the import review.
    Row,
    /// 24px semibold: page headings ("Settings").
    H1,
    /// 12px medium: section labels, keycaps.
    Label,
    /// 15px semibold: "PocketMedia" beside the logo.
    Brand,
    /// 16px bold: the P in the logo.
    Logo,
    /// 16px regular: the quick popup's search text.
    Search,
}

impl TextStyle {
    pub const ALL: [TextStyle; 12] = [
        TextStyle::Medium,
        TextStyle::Body,
        TextStyle::Small,
        TextStyle::Tiny,
        TextStyle::Heading,
        TextStyle::Title,
        TextStyle::Row,
        TextStyle::H1,
        TextStyle::Label,
        TextStyle::Brand,
        TextStyle::Logo,
        TextStyle::Search,
    ];

    pub fn size(self) -> f32 {
        match self {
            TextStyle::Medium | TextStyle::Body => 14.0,
            TextStyle::Small | TextStyle::Label => 12.0,
            TextStyle::Tiny => 11.0,
            TextStyle::Heading | TextStyle::Logo | TextStyle::Search => 16.0,
            TextStyle::Title => 20.0,
            TextStyle::Row => 13.0,
            TextStyle::H1 => 24.0,
            TextStyle::Brand => 15.0,
        }
    }

    /// Font weight (CSS numbers).
    pub fn weight(self) -> f32 {
        match self {
            TextStyle::Medium | TextStyle::Heading | TextStyle::Label => 500.0,
            TextStyle::Title | TextStyle::H1 | TextStyle::Brand => 600.0,
            TextStyle::Logo => 700.0,
            _ => 400.0,
        }
    }

    /// Line box height (line-height 1.45).
    pub fn line(self) -> f32 {
        self.size() * 1.45
    }
}

/// What is under the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    None,
    Close,
    OpenManager,
    AddMedia,
    Search,
    SearchClear,
    Chip(usize),
    /// All media (0) or Recently used (1).
    ListTab(usize),
    Tile(usize),
    Scrollbar,
    EmptyAction,
    ClearHistory,
}

pub struct Layout {
    pub width: f32,
    pub height: f32,
    /// The header strip; outside its controls it drags the window.
    pub titlebar: Rect,
    pub logo: Rect,
    pub close: Rect,
    pub search: Rect,
    pub search_input: Rect,
    pub search_clear: Rect,
    pub chips_track: Rect,
    pub chips: [Rect; 4],
    pub tabs: [Rect; 2],
    /// Width of each tab's label (the count follows it).
    pub tab_label_w: [f32; 2],
    pub gallery: Rect,
    pub footer: Rect,
    pub footer_text: Rect,
    pub clear_history: Rect,
    pub open_manager: Rect,
    pub add_media: Rect,
    /// Width of each empty-state button label ("Clear filters", ...), keyed by label.
    empty_action_w: Vec<(&'static str, f32)>,
}

pub const TAB_LABELS: [&str; 2] = ["All media", "Recent"];
pub const CHIP_LABELS: [&str; 4] = ["All", "GIF", "Picture", "Video"];
pub const FOOTER_ADD: &str = "Add";
pub const FOOTER_MANAGER: &str = "Manager";

impl Layout {
    pub fn compute(width: f32, height: f32, measure: &dyn Fn(&str, TextStyle) -> f32, empty_actions: &[&'static str]) -> Self {
        let left = 1.0 + PAD;
        let right = width - 1.0 - PAD;

        // Header: [logo 28][10][search][8][× 36].
        let titlebar = Rect::new(1.0, 1.0, width - 2.0, PAD + SEARCH_H + 6.0);
        let sy = 1.0 + PAD;
        let logo = Rect::new(left, sy + (SEARCH_H - LOGO_D) / 2.0, LOGO_D, LOGO_D);
        let close = Rect::new(right - CLOSE, sy + (SEARCH_H - CLOSE) / 2.0, CLOSE, CLOSE);
        let search = Rect::new(logo.right() + 10.0, sy, close.x - 8.0 - logo.right() - 10.0, SEARCH_H);
        let sb = super::widgets::SearchBox::at(search.x, search.y, search.w, search.h, false);

        // Row 2: segmented type control at the left, list tabs at the right.
        let y2 = search.bottom() + 10.0;
        let mut chips = [Rect::default(); 4];
        let mut x = left + 3.0;
        for (chip, label) in chips.iter_mut().zip(CHIP_LABELS) {
            let w = 28.0 + measure(label, TextStyle::Small);
            *chip = Rect::new(x, y2 + 3.0, w, 28.0);
            x += w + 2.0;
        }
        let chips_track = Rect::new(left, y2, x - 2.0 + 3.0 - left, ROW2_H);
        // Each tab: label, a space, and room for a four-digit count.
        let count_w = measure("0000", TextStyle::Small);
        let tab_label_w = TAB_LABELS.map(|l| measure(l, TextStyle::Label));
        let tab_w = |i: usize| tab_label_w[i] + 6.0 + count_w;
        let recent = Rect::new(right - tab_w(1), y2, tab_w(1), ROW2_H);
        let all = Rect::new(recent.x - 14.0 - tab_w(0), y2, tab_w(0), ROW2_H);

        let footer = Rect::new(1.0, height - 1.0 - FOOTER_H, width - 2.0, FOOTER_H);
        let fy = footer.y + (FOOTER_H - FOOTER_BUTTON_H) / 2.0;
        let add_w = 16.0 + 10.0 + 7.0 + measure(FOOTER_ADD, TextStyle::Medium) + 16.0;
        let add_media = Rect::new(right - add_w, fy, add_w, FOOTER_BUTTON_H);
        let om_w = 24.0 + measure(FOOTER_MANAGER, TextStyle::Small);
        let open_manager = Rect::new(add_media.x - 6.0 - om_w, fy, om_w, FOOTER_BUTTON_H);
        let ch_w = 24.0 + measure("Clear history", TextStyle::Small);
        let clear_history = Rect::new(open_manager.x - 2.0 - ch_w, fy, ch_w, FOOTER_BUTTON_H);
        let footer_text = Rect::new(left + 2.0, footer.y + 6.0, clear_history.x - 10.0 - left - 2.0, FOOTER_H - 12.0);

        let gy = y2 + ROW2_H + 10.0;
        let gallery = Rect::new(left - TILE_PAD, gy, right - left + TILE_PAD + GALLERY_PAD_RIGHT - 4.0, (footer.y - 6.0 - gy).max(0.0));

        let empty_action_w = empty_actions.iter().map(|l| (*l, 32.0 + measure(l, TextStyle::Body))).collect();

        Layout {
            width,
            height,
            titlebar,
            logo,
            close,
            search,
            search_input: sb.input,
            search_clear: sb.clear,
            chips_track,
            chips,
            tabs: [all, recent],
            tab_label_w,
            gallery,
            footer,
            footer_text,
            clear_history,
            open_manager,
            add_media,
            empty_action_w,
        }
    }

    pub fn search_box(&self) -> super::widgets::SearchBox {
        super::widgets::SearchBox { outer: self.search, input: self.search_input, clear: self.search_clear, icon: false }
    }

    pub fn grid(&self) -> Grid {
        Grid::new(self.gallery)
    }

    /// The media well inside a tile.
    pub fn thumb(tile: Rect) -> Rect {
        Rect::new(tile.x + TILE_PAD, tile.y + TILE_PAD, tile.w - 2.0 * TILE_PAD, THUMB_H)
    }

    pub fn name(tile: Rect) -> Rect {
        let t = Self::thumb(tile);
        Rect::new(t.x + 2.0, t.bottom() + 6.0, t.w - 4.0, NAME_LINE)
    }

    pub fn empty_parts(&self, action: &str) -> (Rect, Rect, Rect) {
        let w = self.empty_action_w.iter().find(|(l, _)| *l == action).map(|(_, w)| *w).unwrap_or(120.0);
        empty_parts(self.gallery, w)
    }

    pub fn hit(&self, x: f32, y: f32, view: &HitView) -> Hit {
        let simple = [
            (self.close, Hit::Close),
            (self.open_manager, Hit::OpenManager),
            (self.add_media, Hit::AddMedia),
            (self.clear_history, Hit::ClearHistory),
        ];
        if let Some((_, hit)) = simple.iter().find(|(r, _)| r.contains(x, y)) {
            return *hit;
        }
        if view.search_has_text && view.search_focused && self.search_clear.contains(x, y) {
            return Hit::SearchClear;
        }
        if self.search.contains(x, y) {
            return Hit::Search;
        }
        if let Some(i) = self.chips.iter().position(|c| c.contains(x, y)) {
            return Hit::Chip(i);
        }
        if let Some(i) = self.tabs.iter().position(|c| c.contains(x, y)) {
            return Hit::ListTab(i);
        }
        if !self.gallery.contains(x, y) {
            return Hit::None;
        }
        if let Some(action) = view.empty_action {
            return if self.empty_parts(action).2.contains(x, y) { Hit::EmptyAction } else { Hit::None };
        }
        let grid = self.grid();
        if grid.on_scrollbar(view.tiles, view.scroll, x, y) {
            return Hit::Scrollbar;
        }
        grid.hit(view.tiles, view.scroll, x, y).map_or(Hit::None, Hit::Tile)
    }

    /// The header outside its controls: drags the window.
    pub fn is_caption(&self, x: f32, y: f32) -> bool {
        self.titlebar.contains(x, y) && !self.search.contains(x, y) && !self.close.contains(x, y)
    }
}

/// A scrolling gallery of tiles in three columns, with room for the scrollbar
/// at the right. Shared by the popup and the Manager.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub area: Rect,
    pub tile_w: f32,
    pub tile_h: f32,
}

impl Grid {
    pub fn new(area: Rect) -> Self {
        let tile_w = (area.w - GALLERY_PAD_RIGHT - GAP * (COLUMNS as f32 - 1.0)) / COLUMNS as f32;
        // Padding, well, 6px gap, name line, padding.
        let tile_h = TILE_PAD + THUMB_H + 6.0 + NAME_LINE + TILE_PAD;
        Self { area, tile_w, tile_h }
    }

    pub fn row_pitch(&self) -> f32 {
        self.tile_h + GAP
    }

    pub fn tile(&self, index: usize, scroll: f32) -> Rect {
        let col = (index % COLUMNS) as f32;
        let row = (index / COLUMNS) as f32;
        Rect::new(
            self.area.x + col * (self.tile_w + GAP),
            self.area.y + row * self.row_pitch() - scroll,
            self.tile_w,
            self.tile_h,
        )
    }

    pub fn content_height(&self, count: usize) -> f32 {
        let rows = count.div_ceil(COLUMNS) as f32;
        if rows == 0.0 { 0.0 } else { rows * self.tile_h + (rows - 1.0) * GAP }
    }

    /// Content that fits to within half a DIP does not scroll (rounding, not overflow).
    fn overflows(&self, count: usize) -> bool {
        self.content_height(count) > self.area.h + 0.5
    }

    pub fn max_scroll(&self, count: usize) -> f32 {
        if self.overflows(count) { self.content_height(count) - self.area.h } else { 0.0 }
    }

    /// Scroll offset that keeps tile `index` fully visible.
    pub fn scroll_to_show(&self, index: usize, scroll: f32) -> f32 {
        let top = (index / COLUMNS) as f32 * self.row_pitch();
        let bottom = top + self.tile_h;
        if top < scroll {
            top
        } else if bottom > scroll + self.area.h {
            bottom - self.area.h
        } else {
            scroll
        }
    }

    /// Thin scrollbar thumb inside the right padding.
    pub fn scrollbar(&self, count: usize, scroll: f32) -> Option<Rect> {
        if !self.overflows(count) {
            return None;
        }
        let content = self.content_height(count);
        let track = self.area.h;
        let h = (track * track / content).max(24.0);
        let y = self.area.y + (track - h) * (scroll / self.max_scroll(count));
        Some(Rect::new(self.area.right() - 4.0, y, 4.0, h))
    }

    /// Tiles per visible page (whole rows), for Page Up / Page Down.
    pub fn page(&self) -> isize {
        (self.area.h / self.row_pitch()).floor().max(1.0) as isize * COLUMNS as isize
    }

    /// Index of the tile under (x, y), if any.
    pub fn hit(&self, count: usize, scroll: f32, x: f32, y: f32) -> Option<usize> {
        if !self.area.contains(x, y) {
            return None;
        }
        (0..count).find(|i| self.tile(*i, scroll).contains(x, y))
    }

    /// True over the scrollbar thumb's column when there is one.
    pub fn on_scrollbar(&self, count: usize, scroll: f32, x: f32, y: f32) -> bool {
        self.area.contains(x, y) && self.scrollbar(count, scroll).is_some_and(|bar| x >= bar.x - 2.0)
    }
}

/// Empty state inside a gallery: heading, hint and a button `button_w` wide.
pub fn empty_parts(gallery: Rect, button_w: f32) -> (Rect, Rect, Rect) {
    let g = gallery;
    let heading = Rect::new(g.x + 16.0, g.y + 56.0, g.w - 32.0, TextStyle::Heading.line());
    let hint = Rect::new(heading.x, heading.bottom() + 8.0, heading.w, TextStyle::Body.line());
    let button = Rect::new(g.x + (g.w - button_w) / 2.0, hint.bottom() + 18.0, button_w, 36.0);
    (heading, hint, button)
}

/// The bits of popup state hit-testing needs.
pub struct HitView {
    pub tiles: usize,
    pub scroll: f32,
    pub search_has_text: bool,
    pub search_focused: bool,
    pub empty_action: Option<&'static str>,
}

/// Size of the media preview inside a well: aspect preserved, 6px clear on every
/// side, never cropped, and never larger than the media's own pixels at this DPI
/// (no cropping or stretching, no upscaling past native pixels).
pub fn fit_media(box_w: f32, box_h: f32, px_w: u32, px_h: u32, dpi: f32) -> (f32, f32) {
    if px_w == 0 || px_h == 0 {
        return (0.0, 0.0);
    }
    let native = 96.0 / dpi.max(1.0); // DIPs per media pixel at 1:1
    let scale = native.min((box_w - 12.0) / px_w as f32).min((box_h - 12.0) / px_h as f32).max(0.0);
    (px_w as f32 * scale, px_h as f32 * scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rough Segoe UI average advance, good enough for geometry tests.
    fn measure(text: &str, style: TextStyle) -> f32 {
        text.chars().count() as f32 * style.size() * 0.55
    }

    fn layout() -> Layout {
        Layout::compute(WIDTH, HEIGHT, &measure, &["Clear filters"])
    }

    #[test]
    fn popup_parts_fit_and_do_not_overlap() {
        let l = layout();
        let g = l.grid();
        assert!(l.search.right() < l.close.x && l.logo.right() < l.search.x);
        assert!(l.chips_track.right() < l.tabs[0].x, "type control and tabs share a row");
        assert!(l.tabs[0].right() < l.tabs[1].x);
        assert!(l.gallery.y > l.chips_track.bottom() && l.gallery.bottom() < l.footer.y);
        assert!(l.footer_text.right() < l.clear_history.x && l.clear_history.right() <= l.open_manager.x);
        assert!(l.open_manager.right() < l.add_media.x && l.add_media.right() <= WIDTH - 1.0);
        assert!(g.tile_w > 140.0, "three columns of wide tiles: {}", g.tile_w);
        assert!(g.tile(2, 0.0).right() <= l.gallery.right() - GALLERY_PAD_RIGHT + 0.01);
        assert!((g.tile(1, 0.0).x - g.tile(0, 0.0).right() - GAP).abs() < 0.01);
    }

    #[test]
    fn the_header_drags_but_its_controls_do_not() {
        let l = layout();
        let (lx, ly) = l.logo.center();
        assert!(l.is_caption(lx, ly), "the logo is a handle");
        let (sx, sy) = l.search.center();
        assert!(!l.is_caption(sx, sy));
        let (cx, cy) = l.close.center();
        assert!(!l.is_caption(cx, cy));
    }

    #[test]
    fn fit_keeps_aspect_and_stays_inside() {
        for (w, h) in [(1280, 720), (720, 1280), (400, 200), (1920, 1080), (300, 300)] {
            let (fw, fh) = fit_media(140.0, 84.0, w, h, 96.0);
            assert!(fw <= 128.01 && fh <= 72.01, "{w}x{h} -> {fw}x{fh}");
            assert!((fw / fh - w as f32 / h as f32).abs() < 0.01);
        }
    }

    #[test]
    fn fit_matches_wireframe_example() {
        // Happy dance.gif 100x100 in a 140x84 box -> 72x72.
        let (fw, fh) = fit_media(140.0, 84.0, 100, 100, 96.0);
        assert!((fw - 72.0).abs() < 0.01 && (fh - 72.0).abs() < 0.01);
    }

    #[test]
    fn fit_never_upscales_past_native_pixels() {
        assert_eq!(fit_media(140.0, 84.0, 20, 10, 96.0), (20.0, 10.0));
        // At 200% one media pixel is half a DIP.
        assert_eq!(fit_media(140.0, 84.0, 20, 10, 192.0), (10.0, 5.0));
    }

    #[test]
    fn scrolling_shows_the_selected_row() {
        let l = layout().grid();
        assert_eq!(l.max_scroll(8), (l.content_height(8) - l.area.h).max(0.0));
        let s = l.scroll_to_show(7, 0.0);
        assert!(l.tile(7, s).bottom() <= l.area.bottom() + 0.01);
        assert_eq!(l.scroll_to_show(0, s), 0.0);
        assert!(l.scrollbar(3, 0.0).is_none());
        assert!(l.scrollbar(12, 0.0).is_some());
    }

    #[test]
    fn content_that_exactly_fits_does_not_scroll() {
        let probe = Grid::new(Rect::new(0.0, 0.0, 500.0, 100.0));
        let exact = probe.content_height(8);
        let g = Grid::new(Rect::new(0.0, 0.0, 500.0, exact - 0.001));
        assert_eq!(g.max_scroll(8), 0.0);
        assert!(g.scrollbar(8, 0.0).is_none());
        let g = Grid::new(Rect::new(0.0, 0.0, 500.0, exact - 40.0));
        assert!(g.max_scroll(8) > 0.0);
        assert!(g.scrollbar(8, 0.0).is_some());
    }

    #[test]
    fn hit_testing_finds_controls_and_tiles() {
        let l = layout();
        let view = HitView { tiles: 8, scroll: 0.0, search_has_text: false, search_focused: true, empty_action: None };
        let (cx, cy) = l.close.center();
        assert_eq!(l.hit(cx, cy, &view), Hit::Close);
        let (tx, ty) = l.grid().tile(4, 0.0).center();
        assert_eq!(l.hit(tx, ty, &view), Hit::Tile(4));
        let gap_x = l.grid().tile(0, 0.0).right() + GAP / 2.0;
        assert_eq!(l.hit(gap_x, ty, &view), Hit::None, "the gap between columns is not a tile");
        let (sx, sy) = l.chips[2].center();
        assert_eq!(l.hit(sx, sy, &view), Hit::Chip(2));
        let (ax, ay) = l.tabs[1].center();
        assert_eq!(l.hit(ax, ay, &view), Hit::ListTab(1));
    }
}
