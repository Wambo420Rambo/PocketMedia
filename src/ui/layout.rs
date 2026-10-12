//! Quick popup geometry in DIPs. Every number comes from the wireframe CSS
//! (`.pm-popup`, `.pm-titlebar`, `.pm-body`, `.pm-tile`, ...), so the native
//! window lines up with reference/…/screenshots-dark/focused-0*.png.

/// Default popup size (CSS: max-width 520px, height 620px).
pub const WIDTH: f32 = 520.0;
pub const HEIGHT: f32 = 620.0;
pub const COLUMNS: usize = 3;

const BORDER: f32 = 1.0;
const TITLE_H: f32 = 46.0; // includes its 1px bottom border
const PAD: f32 = 16.0;
const ACTIONS_H: f32 = 36.0;
const SEARCH_H: f32 = 40.0;
const CHIP_H: f32 = 32.0;
const SELECT_W: f32 = 142.0;
const SELECT_H: f32 = 28.0; // measured in the reference (213..241)
const GALLERY_PAD_RIGHT: f32 = 6.0;
const GAP: f32 = 12.0;
const THUMB_H: f32 = 86.0;
const NAME_LINE: f32 = 12.0 * 1.45;
const FOOTER_H: f32 = 55.0; // 13 + 28 + 13 + 1px top border
const DROPDOWN_ROW_H: f32 = 26.0;

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
    /// 14px medium: window title, item count.
    Medium,
    /// 14px regular: buttons, search text.
    Body,
    /// 12px: chips, select, names, footer, shortcut hint.
    Small,
    /// 11px: badges on tiles.
    Tiny,
    /// 16px medium: empty-state heading.
    Heading,
    /// 21px medium: panel titles ("Add media", "Review files").
    Title,
    /// 13px: names in the import review.
    Row,
    /// 22px medium: window headings ("Your library").
    H1,
}

impl TextStyle {
    pub const ALL: [TextStyle; 8] = [
        TextStyle::Medium,
        TextStyle::Body,
        TextStyle::Small,
        TextStyle::Tiny,
        TextStyle::Heading,
        TextStyle::Title,
        TextStyle::Row,
        TextStyle::H1,
    ];

    pub fn size(self) -> f32 {
        match self {
            TextStyle::Medium | TextStyle::Body => 14.0,
            TextStyle::Small => 12.0,
            TextStyle::Tiny => 11.0,
            TextStyle::Heading => 16.0,
            TextStyle::Title => 21.0,
            TextStyle::Row => 13.0,
            TextStyle::H1 => 22.0,
        }
    }
    pub fn medium(self) -> bool {
        matches!(self, TextStyle::Medium | TextStyle::Heading | TextStyle::Title | TextStyle::H1)
    }
    /// CSS line box height (line-height 1.45).
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
    List,
    DropdownRow(usize),
    Tile(usize),
    Scrollbar,
    EmptyAction,
    ClearHistory,
}

pub struct Layout {
    pub width: f32,
    pub height: f32,
    pub titlebar: Rect,
    pub title: Rect,
    pub shortcut: Rect,
    pub close: Rect,
    pub open_manager: Rect,
    pub add_media: Rect,
    pub search: Rect,
    pub search_input: Rect,
    pub search_clear: Rect,
    pub type_label: Rect,
    pub chips: [Rect; 4],
    pub count: Rect,
    pub list: Rect,
    pub dropdown: Rect,
    pub dropdown_rows: [Rect; 2],
    pub gallery: Rect,
    pub footer: Rect,
    pub footer_text: Rect,
    pub clear_history: Rect,
    /// Width of each empty-state button label ("Clear filters", ...), keyed by label.
    empty_action_w: Vec<(&'static str, f32)>,
}

/// Width of a 36px-high button: 1px border + 11px padding on each side, plus an
/// optional icon slot followed by the wireframe's 7px gap.
fn button_w(text_w: f32, icon_w: Option<f32>) -> f32 {
    2.0 + 22.0 + text_w + icon_w.map_or(0.0, |w| w + 7.0)
}

impl Layout {
    pub fn compute(
        width: f32,
        height: f32,
        measure: &dyn Fn(&str, TextStyle) -> f32,
        shortcut_label: &str,
        empty_actions: &[&'static str],
    ) -> Self {
        let left = BORDER + PAD;
        let right = width - BORDER - PAD;
        let inner_w = right - left;

        // Title bar: [icon 0][9][title][9][auto][shortcut][9][auto][close], padding 14.
        let titlebar = Rect::new(BORDER, BORDER, width - 2.0 * BORDER, TITLE_H);
        let title_w = measure("PocketMedia", TextStyle::Medium);
        let short_w = measure(shortcut_label, TextStyle::Small);
        let tl = BORDER + 14.0;
        let tr = width - BORDER - 14.0;
        let close = Rect::new(tr - 32.0, BORDER + (TITLE_H - 1.0 - 28.0) / 2.0, 32.0, 28.0);
        let free = (tr - tl) - (9.0 + title_w + 9.0 + short_w + 9.0 + 32.0);
        let title = Rect::new(tl + 9.0, BORDER, title_w, TITLE_H - 1.0);
        let shortcut = Rect::new(title.right() + 9.0 + free.max(0.0) / 2.0, BORDER, short_w, TITLE_H - 1.0);

        let mut y = BORDER + TITLE_H + PAD;
        let open_manager = Rect::new(left, y, button_w(measure("Open manager", TextStyle::Body), Some(0.0)), ACTIONS_H);
        let add_w = button_w(measure("Add media", TextStyle::Body), Some(measure("+", TextStyle::Body)));
        let add_media = Rect::new(right - add_w, y, add_w, ACTIONS_H);
        y += ACTIONS_H + 12.0;

        let search = Rect::new(left, y, inner_w, SEARCH_H);
        // Input box: 1px border + 12px padding + empty icon slot + 9px gap; 38px tall
        // (x 39..490 in the reference).
        let search_input = Rect::new(left + 1.0 + 12.0 + 9.0, y + 1.0, inner_w - 2.0 - 24.0 - 9.0, SEARCH_H - 2.0);
        let search_clear = Rect::new(search_input.right() - 22.0, search_input.y + 7.0, 22.0, 24.0);
        y += SEARCH_H + 14.0;

        let label_w = measure("Type", TextStyle::Small);
        let type_label = Rect::new(left, y, label_w, CHIP_H);
        let mut x = left + label_w + 2.0 + 6.0;
        let mut chips = [Rect::default(); 4];
        for (chip, label) in chips.iter_mut().zip(["All", "GIF", "Picture", "Video"]) {
            let w = 2.0 + 20.0 + measure(label, TextStyle::Small);
            *chip = Rect::new(x, y, w, CHIP_H);
            x += w + 6.0;
        }
        y += CHIP_H + 16.0;

        let count = Rect::new(left, y, inner_w - SELECT_W - 8.0, SELECT_H);
        let list = Rect::new(right - SELECT_W, y, SELECT_W, SELECT_H);
        let dropdown = Rect::new(list.x, list.bottom() + 2.0, SELECT_W, 2.0 * DROPDOWN_ROW_H + 8.0);
        let dropdown_rows = [
            Rect::new(dropdown.x + 4.0, dropdown.y + 4.0, SELECT_W - 8.0, DROPDOWN_ROW_H),
            Rect::new(dropdown.x + 4.0, dropdown.y + 4.0 + DROPDOWN_ROW_H, SELECT_W - 8.0, DROPDOWN_ROW_H),
        ];
        y += SELECT_H + 10.0;

        let footer = Rect::new(BORDER, height - BORDER - FOOTER_H, width - 2.0 * BORDER, FOOTER_H);
        let ch_w = 2.0 + 14.0 + measure("Clear history", TextStyle::Small);
        let clear_history = Rect::new(footer.right() - 20.0 - ch_w, footer.y + 1.0 + 11.0, ch_w, 28.0);
        let footer_x = footer.x + 20.0;
        // 2px higher than the CSS box: matches where Chrome puts the glyphs in the reference.
        let footer_text = Rect::new(footer_x, footer.y + 1.0 + 11.0, clear_history.x - 8.0 - footer_x, NAME_LINE);

        let gallery = Rect::new(left, y, inner_w, (footer.y - PAD - y).max(0.0));

        let empty_action_w = empty_actions.iter().map(|l| (*l, button_w(measure(l, TextStyle::Body), None))).collect();

        Layout {
            width,
            height,
            titlebar,
            title,
            shortcut,
            close,
            open_manager,
            add_media,
            search,
            search_input,
            search_clear,
            type_label,
            chips,
            count,
            list,
            dropdown,
            dropdown_rows,
            gallery,
            footer,
            footer_text,
            clear_history,
            empty_action_w,
        }
    }

    pub fn grid(&self) -> Grid {
        Grid::new(self.gallery)
    }

    /// The bordered thumbnail box inside a tile (tile border 1 + padding 4).
    pub fn thumb(tile: Rect) -> Rect {
        Rect::new(tile.x + 5.0, tile.y + 5.0, tile.w - 10.0, THUMB_H)
    }

    pub fn name(tile: Rect) -> Rect {
        let t = Self::thumb(tile);
        Rect::new(t.x, t.bottom() + 7.0, t.w, NAME_LINE)
    }

    pub fn empty_parts(&self, action: &str) -> (Rect, Rect, Rect) {
        let w = self.empty_action_w.iter().find(|(l, _)| *l == action).map(|(_, w)| *w).unwrap_or(120.0);
        empty_parts(self.gallery, w)
    }

    pub fn hit(&self, x: f32, y: f32, view: &HitView) -> Hit {
        if view.dropdown_open {
            for (i, row) in self.dropdown_rows.iter().enumerate() {
                if row.contains(x, y) {
                    return Hit::DropdownRow(i);
                }
            }
            if self.dropdown.contains(x, y) {
                return Hit::None;
            }
        }
        let simple = [
            (self.close, Hit::Close),
            (self.open_manager, Hit::OpenManager),
            (self.add_media, Hit::AddMedia),
            (self.list, Hit::List),
            (self.clear_history, Hit::ClearHistory),
        ];
        if let Some((_, hit)) = simple.iter().find(|(r, _)| r.contains(x, y)) {
            return *hit;
        }
        if view.search_has_text && self.search_clear.contains(x, y) {
            return Hit::SearchClear;
        }
        if self.search.contains(x, y) {
            return Hit::Search;
        }
        if let Some(i) = self.chips.iter().position(|c| c.contains(x, y)) {
            return Hit::Chip(i);
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
}

/// A scrolling gallery of tiles in three columns (CSS .pm-media-grid: gap 12,
/// 6px right padding for the scrollbar). Shared by the popup and the Manager.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub area: Rect,
    pub tile_w: f32,
    pub tile_h: f32,
}

impl Grid {
    pub fn new(area: Rect) -> Self {
        let tile_w = (area.w - GALLERY_PAD_RIGHT - GAP * (COLUMNS as f32 - 1.0)) / COLUMNS as f32;
        // Tile border 1 + padding 4 (x2), thumbnail, 7px gap, name line.
        let tile_h = 2.0 + 8.0 + THUMB_H + 7.0 + NAME_LINE;
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

    pub fn max_scroll(&self, count: usize) -> f32 {
        (self.content_height(count) - self.area.h).max(0.0)
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

    /// Thin scrollbar thumb inside the 6px right padding.
    pub fn scrollbar(&self, count: usize, scroll: f32) -> Option<Rect> {
        let content = self.content_height(count);
        if content <= self.area.h {
            return None;
        }
        let track = self.area.h;
        let h = (track * track / content).max(24.0);
        let y = self.area.y + (track - h) * (scroll / self.max_scroll(count));
        Some(Rect::new(self.area.right() - 5.0, y, 4.0, h))
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

/// Empty state inside a gallery: heading, hint and a button `button_w` wide
/// (CSS .pm-empty: padding 46px 16px; heading 16px + 12px gap; actions +18px).
pub fn empty_parts(gallery: Rect, button_w: f32) -> (Rect, Rect, Rect) {
    let g = gallery;
    let heading = Rect::new(g.x + 16.0, g.y + 46.0, g.w - 32.0, TextStyle::Heading.line());
    let hint = Rect::new(heading.x, heading.bottom() + 12.0, heading.w, TextStyle::Body.line());
    let button = Rect::new(g.x + (g.w - button_w) / 2.0, hint.bottom() + 18.0, button_w, ACTIONS_H);
    (heading, hint, button)
}

/// Width of a 36px-high text button: 1px border + 11px padding on each side.
pub fn text_button_w(text_w: f32) -> f32 {
    2.0 + 22.0 + text_w
}

/// The bits of popup state hit-testing needs.
pub struct HitView {
    pub tiles: usize,
    pub scroll: f32,
    pub dropdown_open: bool,
    pub search_has_text: bool,
    pub empty_action: Option<&'static str>,
}

/// Size of the media preview inside a thumbnail box: aspect preserved, 6px clear
/// on every side, never cropped, and never larger than the media's own pixels at
/// this DPI (CSS .pm-media-fit, plus the "no upscaling past native pixels" rule).
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
        Layout::compute(WIDTH, HEIGHT, &measure, "Win + Alt + .", &["Clear filters"])
    }

    #[test]
    fn grid_matches_wireframe() {
        let l = layout();
        let g = l.grid();
        assert_eq!(g.tile_w, 152.0);
        assert!((g.tile_h - 120.4).abs() < 0.01);
        assert_eq!(l.gallery.y, 251.0);
        assert_eq!(l.footer.y, 564.0);
        assert_eq!(l.gallery.bottom(), 548.0);
        assert_eq!((l.search_input.x, l.search_input.right()), (39.0, 490.0));
        assert_eq!(g.tile(1, 0.0).x, 181.0);
        assert_eq!(g.tile(2, 0.0).x, 345.0);
        assert!((g.tile(3, 0.0).y - (251.0 + 132.4)).abs() < 0.01);
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
        // Happy dance.gif 100x100 in a 140x84 box -> 72x72 in the reference.
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
        assert!(l.scrollbar(8, 0.0).is_some());
    }

    #[test]
    fn hit_testing_finds_controls_and_tiles() {
        let l = layout();
        let view = HitView { tiles: 8, scroll: 0.0, dropdown_open: false, search_has_text: false, empty_action: None };
        let (cx, cy) = l.close.center();
        assert_eq!(l.hit(cx, cy, &view), Hit::Close);
        let (tx, ty) = l.grid().tile(4, 0.0).center();
        assert_eq!(l.hit(tx, ty, &view), Hit::Tile(4));
        // The 12px gap between columns is not a tile.
        assert_eq!(l.hit(175.0, 300.0, &view), Hit::None);
        let (sx, sy) = l.chips[2].center();
        assert_eq!(l.hit(sx, sy, &view), Hit::Chip(2));
    }
}
