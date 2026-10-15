//! Small safe wrapper over Direct2D and DirectWrite for drawing one window.
//! COM objects are owned by these structs and released by `Drop` (the `windows`
//! crate's interface types release on drop).

use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_ALPHA_MODE_UNKNOWN, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_RETAIN_CONTENTS, D2D1_RENDER_TARGET_PROPERTIES, ID2D1Bitmap,
    D2D1_DASH_STYLE_CUSTOM, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_ROUNDED_RECT, D2D1_STROKE_STYLE_PROPERTIES,
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush, ID2D1StrokeStyle,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_HIT_TEST_METRICS,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWRITE_TEXT_METRICS, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteInlineObject,
    DWRITE_FONT_AXIS_TAG_WEIGHT, DWRITE_FONT_AXIS_VALUE, IDWriteTextFormat, IDWriteTextFormat3, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_UNKNOWN};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{BOOL, HSTRING, Interface, Result};

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::layout::{Rect, TextStyle};
use crate::pixels::Pixels;

/// Drawing targets made, in any window (for the self-test log).
pub static TARGETS_MADE: AtomicU64 = AtomicU64::new(0);
/// Self-test only: the next frame reports a lost device, as after a GPU reset.
pub static LOSE_NEXT: AtomicBool = AtomicBool::new(false);

/// 0xRRGGBB plus opacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color(pub u32, pub f32);

impl Color {
    pub const fn rgb(hex: u32) -> Self {
        Color(hex, 1.0)
    }
    pub const fn alpha(self, a: f32) -> Self {
        Color(self.0, a)
    }
    fn d2d(self) -> D2D1_COLOR_F {
        let c = |shift: u32| ((self.0 >> shift) & 0xFF) as f32 / 255.0;
        D2D1_COLOR_F { r: c(16), g: c(8), b: c(0), a: self.1 }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
}

impl Align {
    fn dwrite(self) -> DWRITE_TEXT_ALIGNMENT {
        match self {
            Align::Start => DWRITE_TEXT_ALIGNMENT_LEADING,
            Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
            Align::End => DWRITE_TEXT_ALIGNMENT_TRAILING,
        }
    }
}

/// CSS line-height used by the wireframe (1.45) for wrapped text.
const LINE_HEIGHT: f32 = 1.45;

struct Format {
    style: TextStyle,
    format: IDWriteTextFormat,
    ellipsis: IDWriteInlineObject,
}

/// Device-independent resources (factories, fonts) plus the window's render target.
pub struct Gfx {
    factory: ID2D1Factory,
    dwrite: IDWriteFactory,
    formats: Vec<Format>,
    /// 3px dash, 2px gap: the wireframe's 1px dashed border.
    dashed: ID2D1StrokeStyle,
    target: Option<(ID2D1HwndRenderTarget, ID2D1SolidColorBrush)>,
    /// Bitmaps of the pictures drawn in the last full frame (they belong to the
    /// render target, so they go with it).
    images: RefCell<Vec<Image>>,
    /// The last frame lost its target and never reached the screen (see `take_lost`).
    lost: bool,
}

/// Identifies one picture slot: an item id and the thumbnail box size.
pub type ImageKey = (u32, u32, u32);

struct Image {
    key: ImageKey,
    version: u64,
    size: (u32, u32),
    bitmap: ID2D1Bitmap,
    used: bool,
}

/// Prefer Windows 11's Segoe UI Variable (what the wireframe names), else Segoe UI.
fn font_family(dwrite: &IDWriteFactory) -> HSTRING {
    let wanted = HSTRING::from("Segoe UI Variable Text");
    let mut collection = None;
    // SAFETY: out-pointer to a local Option; `false` = do not check for updates.
    let found = unsafe { dwrite.GetSystemFontCollection(&mut collection, false) }.ok().and(collection).is_some_and(|c| {
        let (mut index, mut exists) = (0u32, BOOL(0));
        // SAFETY: valid HSTRING and out-pointers to locals.
        unsafe { c.FindFamilyName(&wanted, &mut index, &mut exists) }.is_ok() && exists.as_bool()
    });
    if found { wanted } else { HSTRING::from("Segoe UI") }
}

/// Segoe UI Variable has no "Medium" named instance, so DirectWrite rounds weight
/// 500 up to Semibold. Setting the variable weight axis gives the exact weight.
/// Older Windows without IDWriteTextFormat3 keeps the rounded weight.
fn set_true_weight(format: &IDWriteTextFormat, weight: f32) {
    if let Ok(f3) = format.cast::<IDWriteTextFormat3>() {
        let axis = [DWRITE_FONT_AXIS_VALUE { axisTag: DWRITE_FONT_AXIS_TAG_WEIGHT, value: weight }];
        // SAFETY: f3 is the same live format; the slice outlives the call.
        let _ = unsafe { f3.SetFontAxisValues(&axis) };
    }
}

impl Gfx {
    pub fn new() -> Result<Self> {
        // SAFETY: plain factory creation; the results are owned COM pointers.
        let factory: ID2D1Factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
        // SAFETY: as above.
        let dwrite: IDWriteFactory = unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };
        let family = font_family(&dwrite);
        let mut formats = Vec::new();
        for style in TextStyle::ALL {
            let weight = DWRITE_FONT_WEIGHT(style.weight() as i32);
            // SAFETY: valid HSTRINGs; sizes are positive constants.
            let format = unsafe {
                dwrite.CreateTextFormat(
                    &family,
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    style.size(),
                    &HSTRING::from("en-us"),
                )?
            };
            // SAFETY: format is a live text format created above.
            unsafe { format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)? };
            if style.weight() != 400.0 {
                set_true_weight(&format, style.weight());
            }
            // SAFETY: as above.
            let ellipsis = unsafe { dwrite.CreateEllipsisTrimmingSign(&format)? };
            formats.push(Format { style, format, ellipsis });
        }
        let dash_props = D2D1_STROKE_STYLE_PROPERTIES { dashStyle: D2D1_DASH_STYLE_CUSTOM, ..Default::default() };
        // SAFETY: plain property struct and a dash array that outlives the call.
        let dashed = unsafe { factory.CreateStrokeStyle(&dash_props, Some(&[3.0, 2.0]))? };
        Ok(Self { factory, dwrite, formats, dashed, target: None, images: RefCell::new(Vec::new()), lost: false })
    }

    fn format(&self, style: TextStyle) -> &Format {
        // Every TextStyle gets a format in `new`, so this always finds one.
        self.formats.iter().find(|f| f.style == style).unwrap_or(&self.formats[0])
    }

    fn layout(&self, text: &str, style: TextStyle, max_w: f32, max_h: f32) -> Option<IDWriteTextLayout> {
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: `wide` outlives the call; the format is live.
        unsafe { self.dwrite.CreateTextLayout(&wide, &self.format(style).format, max_w.max(0.0), max_h.max(0.0)) }.ok()
    }

    /// A word-wrapped layout `width` wide with lines `lh` apart.
    fn wrapped(&self, text: &str, style: TextStyle, width: f32, lh: f32) -> Option<IDWriteTextLayout> {
        let layout = self.layout(text, style, width, 10_000.0)?;
        // SAFETY: layout is live; plain values.
        unsafe {
            layout.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP).ok()?;
            layout.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, lh, lh * 0.8).ok()?;
        }
        Some(layout)
    }

    /// Height of `text` wrapped to `width` (lines x line height), in DIPs.
    pub fn wrapped_height(&self, text: &str, style: TextStyle, width: f32) -> f32 {
        self.line_count(text, style, width) as f32 * style.size() * LINE_HEIGHT
    }

    /// Number of lines `text` takes when wrapped to `width` (at least 1).
    pub fn line_count(&self, text: &str, style: TextStyle, width: f32) -> usize {
        let Some(layout) = self.wrapped(text, style, width, style.line()) else { return 1 };
        let mut m = DWRITE_TEXT_METRICS::default();
        // SAFETY: out-pointer to a local struct.
        match unsafe { layout.GetMetrics(&mut m) } {
            Ok(()) => m.lineCount.max(1) as usize,
            Err(_) => 1,
        }
    }

    /// Advance width of `text` in DIPs.
    pub fn measure(&self, text: &str, style: TextStyle) -> f32 {
        let Some(layout) = self.layout(text, style, 10_000.0, 100.0) else { return 0.0 };
        let mut m = DWRITE_TEXT_METRICS::default();
        // SAFETY: out-pointer to a local struct.
        match unsafe { layout.GetMetrics(&mut m) } {
            Ok(()) => m.widthIncludingTrailingWhitespace,
            Err(_) => 0.0,
        }
    }

    /// X offset of the caret before UTF-16 position `pos`.
    pub fn caret_x(&self, text: &str, style: TextStyle, pos: usize) -> f32 {
        let Some(layout) = self.layout(text, style, 10_000.0, 100.0) else { return 0.0 };
        let (mut x, mut y) = (0.0f32, 0.0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        // SAFETY: out-pointers to locals.
        match unsafe { layout.HitTestTextPosition(pos as u32, false, &mut x, &mut y, &mut m) } {
            Ok(()) => x,
            Err(_) => 0.0,
        }
    }

    /// UTF-16 caret position nearest to x (for clicks in the search box).
    pub fn position_at(&self, text: &str, style: TextStyle, x: f32) -> usize {
        let Some(layout) = self.layout(text, style, 10_000.0, 100.0) else { return 0 };
        let (mut trailing, mut inside) = (BOOL(0), BOOL(0));
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        // SAFETY: out-pointers to locals.
        if unsafe { layout.HitTestPoint(x, 1.0, &mut trailing, &mut inside, &mut m) }.is_err() {
            return 0;
        }
        m.textPosition as usize + if trailing.as_bool() { m.length as usize } else { 0 }
    }

    /// Draws one frame. `draw` gets a painter in DIPs. With `dirty`, only that
    /// area is redrawn (the rest of the window keeps its pixels); a new or
    /// resized target always gets a full frame. A lost device is recreated on
    /// the next frame.
    pub fn draw(&mut self, hwnd: HWND, dpi: f32, dirty: Option<Rect>, draw: impl FnOnce(&Painter)) -> Result<()> {
        let mut rc = RECT::default();
        // SAFETY: hwnd is our live window; rc is a local out-parameter.
        unsafe { GetClientRect(hwnd, &mut rc)? };
        let size = D2D_SIZE_U { width: (rc.right - rc.left) as u32, height: (rc.bottom - rc.top) as u32 };
        let mut dirty = dirty;
        if self.target.is_none() {
            dirty = None;
            self.images.borrow_mut().clear();
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_UNKNOWN, alphaMode: D2D1_ALPHA_MODE_UNKNOWN },
                dpiX: dpi,
                dpiY: dpi,
                ..Default::default()
            };
            // Keep the pixels between frames, so a partial frame can redraw one tile.
            let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES { hwnd, pixelSize: size, presentOptions: D2D1_PRESENT_OPTIONS_RETAIN_CONTENTS };
            // SAFETY: property structs are fully initialised locals.
            let target = unsafe { self.factory.CreateHwndRenderTarget(&props, &hwnd_props)? };
            // SAFETY: target is live; the color is a plain struct.
            let brush = unsafe { target.CreateSolidColorBrush(&Color::rgb(0).d2d(), None)? };
            self.target = Some((target, brush));
            TARGETS_MADE.fetch_add(1, Ordering::Relaxed);
        }
        let Some((target, brush)) = &self.target else { return Ok(()) };
        // SAFETY: the target is live; size and dpi are plain values.
        unsafe {
            if target.GetPixelSize() != size {
                target.Resize(&size)?;
                dirty = None;
            }
            target.SetDpi(dpi, dpi);
            target.SetAntialiasMode(D2D1_ANTIALIAS_MODE_PER_PRIMITIVE);
            target.BeginDraw();
        }
        let full = dirty.is_none();
        if full {
            self.images.borrow_mut().iter_mut().for_each(|i| i.used = false);
        }
        let painter = Painter { gfx: self, target, brush, dirty };
        if let Some(d) = dirty {
            painter.push_clip(d);
            draw(&painter);
            painter.pop_clip();
        } else {
            draw(&painter);
        }
        // SAFETY: paired with BeginDraw above.
        let result = unsafe { target.EndDraw(None, None) };
        if full {
            self.images.borrow_mut().retain(|i| i.used);
        }
        // Self-test only: report this frame as a lost device.
        let result = if LOSE_NEXT.swap(false, Ordering::Relaxed) { Err(D2DERR_RECREATE_TARGET.into()) } else { result };
        if let Err(e) = result {
            if e.code() == D2DERR_RECREATE_TARGET {
                self.target = None;
                self.images.borrow_mut().clear();
                // This frame never reached the screen: another one is needed, which
                // makes the new target. The window asks for it after
                // its WM_PAINT has marked the window painted (`take_lost`).
                self.lost = true;
                return Ok(());
            }
            return Err(e);
        }
        Ok(())
    }

    /// Lets go of every picture bitmap (the window was hidden).
    pub fn release_images(&mut self) {
        self.images.borrow_mut().clear();
    }

    /// Releases the render target (e.g. after a DPI change).
    /// True once after a frame lost its drawing target. The window procedure
    /// then asks for a new frame, after ValidateRect (a request made during the
    /// frame would be cancelled by it).
    pub fn take_lost(&mut self) -> bool {
        std::mem::take(&mut self.lost)
    }

    pub fn reset_target(&mut self) {
        self.target = None;
        self.images.borrow_mut().clear();
    }
}

/// Drawing calls for one frame, all in DIPs.
pub struct Painter<'a> {
    gfx: &'a Gfx,
    target: &'a ID2D1HwndRenderTarget,
    brush: &'a ID2D1SolidColorBrush,
    /// The area being redrawn, or None for the whole window.
    dirty: Option<Rect>,
}

/// A Direct2D point. The `windows` crate types it as `windows_numerics::Vector2`,
/// a separate crate it does not re-export; only `windows` is a dependency, so the
/// point is built from its exact layout instead (two consecutive f32: X, Y).
fn pt<T: Copy>(x: f32, y: f32) -> T {
    assert_eq!(size_of::<T>(), size_of::<[f32; 2]>(), "Direct2D point must be two f32");
    // SAFETY: T is only ever inferred as Vector2, which is #[repr(C)] { X: f32, Y: f32 };
    // the size is checked above and any bit pattern is a valid f32.
    unsafe { std::mem::transmute_copy(&[x, y]) }
}

fn rect_f(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F { left: r.x, top: r.y, right: r.right(), bottom: r.bottom() }
}

impl Painter<'_> {
    /// False when `r` lies outside the area being redrawn (nothing to draw).
    pub fn touches(&self, r: Rect) -> bool {
        self.dirty.is_none_or(|d| r.x < d.right() && d.x < r.right() && r.y < d.bottom() && d.y < r.bottom())
    }

    /// True for a full frame (every drawn picture is listed, see thumbs.rs).
    pub fn is_full(&self) -> bool {
        self.dirty.is_none()
    }

    /// Draws `px` (premultiplied BGRA) into `dest`. The bitmap is kept while the
    /// picture is drawn in every full frame, and refilled when `version` changes.
    pub fn image(&self, key: ImageKey, version: u64, px: &Pixels, dest: Rect) {
        let mut images = self.gfx.images.borrow_mut();
        let found = images.iter().position(|i| i.key == key);
        if let Some(i) = found {
            images[i].used = true;
        }
        if !self.touches(dest) || px.width == 0 || px.height == 0 {
            return;
        }
        let size = (px.width, px.height);
        let found = match found {
            Some(i) if images[i].version == version => Some(i),
            Some(i) if images[i].size == size => {
                // SAFETY: the bitmap is `size` big and `px.data` holds exactly that many rows.
                let ok = unsafe { images[i].bitmap.CopyFromMemory(None, px.data.as_ptr() as *const _, px.width * 4) }.is_ok();
                if ok {
                    images[i].version = version;
                    Some(i)
                } else {
                    images.swap_remove(i);
                    None
                }
            }
            Some(i) => {
                images.swap_remove(i);
                None
            }
            None => None,
        };
        let index = match found {
            Some(i) => i,
            None => {
                let props = D2D1_BITMAP_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                    dpiX: 96.0,
                    dpiY: 96.0,
                };
                let wanted = D2D_SIZE_U { width: px.width, height: px.height };
                // SAFETY: `px.data` is width x height x 4 bytes and outlives the call.
                let Ok(bitmap) = (unsafe { self.target.CreateBitmap(wanted, Some(px.data.as_ptr() as *const _), px.width * 4, &props) }) else {
                    return;
                };
                images.push(Image { key, version, size, bitmap, used: true });
                images.len() - 1
            }
        };
        // SAFETY: called during a frame; the bitmap belongs to this target.
        unsafe {
            self.target.DrawBitmap(&images[index].bitmap, Some(&rect_f(dest)), 1.0, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, None)
        };
    }

    fn color(&self, c: Color) {
        // SAFETY: the brush is live for the frame.
        unsafe { self.brush.SetColor(&c.d2d()) };
    }

    pub fn clear(&self, c: Color) {
        // SAFETY: called between BeginDraw and EndDraw.
        unsafe { self.target.Clear(Some(&c.d2d())) };
    }

    pub fn fill(&self, r: Rect, radius: f32, c: Color) {
        if !self.touches(r) {
            return;
        }
        self.color(c);
        let rr = D2D1_ROUNDED_RECT { rect: rect_f(r), radiusX: radius, radiusY: radius };
        // SAFETY: called during a frame with a live brush.
        unsafe { self.target.FillRoundedRectangle(&rr, self.brush) };
    }

    /// Stroke drawn inside `r`, like a CSS border.
    pub fn stroke(&self, r: Rect, radius: f32, width: f32, c: Color) {
        if !self.touches(r.inset(-width)) {
            return;
        }
        self.color(c);
        let r = r.inset(width / 2.0);
        let rr = D2D1_ROUNDED_RECT { rect: rect_f(r), radiusX: (radius - width / 2.0).max(0.0), radiusY: (radius - width / 2.0).max(0.0) };
        // SAFETY: as above.
        unsafe { self.target.DrawRoundedRectangle(&rr, self.brush, width, None) };
    }

    pub fn line(&self, x1: f32, y1: f32, x2: f32, y2: f32, width: f32, c: Color) {
        let bounds = Rect::new(x1.min(x2), y1.min(y2), (x2 - x1).abs(), (y2 - y1).abs()).inset(-width);
        if !self.touches(bounds) {
            return;
        }
        self.color(c);
        // SAFETY: as above.
        unsafe { self.target.DrawLine(pt(x1, y1), pt(x2, y2), self.brush, width, None) };
    }

    /// One line of text in `r`, vertically centred or top-aligned, with "…" when it does not fit.
    pub fn text(&self, s: &str, r: Rect, style: TextStyle, c: Color, align: Align, vcenter: bool) {
        if !self.touches(r) {
            return;
        }
        let Some(layout) = self.gfx.layout(s, style, r.w, r.h) else { return };
        let f = self.gfx.format(style);
        let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, delimiter: 0, delimiterCount: 0 };
        let para = if vcenter { DWRITE_PARAGRAPH_ALIGNMENT_CENTER } else { DWRITE_PARAGRAPH_ALIGNMENT_NEAR };
        self.color(c);
        // SAFETY: layout and ellipsis sign are live; called during a frame.
        unsafe {
            let _ = layout.SetTextAlignment(align.dwrite());
            let _ = layout.SetParagraphAlignment(para);
            let _ = layout.SetTrimming(&trimming, &f.ellipsis);
            self.target.DrawTextLayout(pt(r.x, r.y), &layout, self.brush, D2D1_DRAW_TEXT_OPTIONS_CLIP);
        }
    }

    /// Word-wrapped text starting at (x, y), `width` wide.
    pub fn text_wrapped(&self, s: &str, x: f32, y: f32, width: f32, style: TextStyle, c: Color) {
        self.text_wrapped_lh(s, x, y, width, style, c, style.size() * LINE_HEIGHT);
    }

    /// Word-wrapped text with lines `lh` apart (CSS line-height in DIPs).
    #[allow(clippy::too_many_arguments)]
    pub fn text_wrapped_lh(&self, s: &str, x: f32, y: f32, width: f32, style: TextStyle, c: Color, lh: f32) {
        let Some(layout) = self.gfx.wrapped(s, style, width, lh) else { return };
        self.color(c);
        // SAFETY: layout is live; called during a frame.
        unsafe { self.target.DrawTextLayout(pt(x, y), &layout, self.brush, D2D1_DRAW_TEXT_OPTIONS_NONE) };
    }

    /// 1px dashed border inside `r` (CSS `border: 1px dashed`).
    pub fn stroke_dashed(&self, r: Rect, radius: f32, c: Color) {
        if !self.touches(r) {
            return;
        }
        self.color(c);
        let r = r.inset(0.5);
        let rr = D2D1_ROUNDED_RECT { rect: rect_f(r), radiusX: radius - 0.5, radiusY: radius - 0.5 };
        // SAFETY: called during a frame; the stroke style is owned by Gfx.
        unsafe { self.target.DrawRoundedRectangle(&rr, self.brush, 1.0, &self.gfx.dashed) };
    }

    /// Filled circle (radio buttons).
    pub fn fill_circle(&self, cx: f32, cy: f32, r: f32, c: Color) {
        if !self.touches(Rect::new(cx - r, cy - r, r * 2.0, r * 2.0)) {
            return;
        }
        self.color(c);
        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
        // SAFETY: called during a frame with a live brush.
        unsafe { self.target.FillEllipse(&e, self.brush) };
    }

    /// Circle outline, `width` wide, centred on the radius.
    pub fn stroke_circle(&self, cx: f32, cy: f32, r: f32, width: f32, c: Color) {
        if !self.touches(Rect::new(cx - r - width, cy - r - width, (r + width) * 2.0, (r + width) * 2.0)) {
            return;
        }
        self.color(c);
        let e = D2D1_ELLIPSE { point: pt(cx, cy), radiusX: r, radiusY: r };
        // SAFETY: called during a frame with a live brush.
        unsafe { self.target.DrawEllipse(&e, self.brush, width, None) };
    }

    pub fn push_clip(&self, r: Rect) {
        // SAFETY: paired with pop_clip by the caller within the same frame.
        unsafe { self.target.PushAxisAlignedClip(&rect_f(r), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
    }

    pub fn pop_clip(&self) {
        // SAFETY: only called after push_clip.
        unsafe { self.target.PopAxisAlignedClip() };
    }

    pub fn gfx(&self) -> &Gfx {
        self.gfx
    }
}
