// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronPaint — display lists. The page thread turns a layout tree into a
//! flat, immutable display list (shared via Arc with the UI thread), which
//! the rasterizer draws with culling to the visible viewport.

pub mod builder;
pub mod damage;
pub mod raster;

use std::sync::Arc;

use crate::css::style::BorderStyle;
use crate::css::values::Color;
use crate::text::Font;

/// Decoded image (premultiplied RGBA) shared between threads.
#[derive(Clone)]
pub struct ImageData {
    pub pixmap: tiny_skia::Pixmap,
    pub width: u32,
    pub height: u32,
    /// Device-pixel density for srcset `2x` candidates.
    pub density: f32,
    /// Exact natural size when it isn't a whole number of pixels (SVG).
    pub nat: Option<(f32, f32)>,
    /// An SVG image's own preserveAspectRatio (x align, y align, slice) when it
    /// has a viewBox: drawn into a box of another shape, its content keeps its
    /// aspect ratio inside the box instead of stretching.
    pub svg_par: Option<(f32, f32, bool)>,
}

impl ImageData {
    /// Natural (CSS px) size.
    pub fn natural(&self) -> (f32, f32) {
        self.nat.unwrap_or((self.width as f32 / self.density, self.height as f32 / self.density))
    }
    pub fn from_rgba(width: u32, height: u32, mut rgba: Vec<u8>) -> Option<ImageData> {
        // Premultiply.
        for px in rgba.chunks_exact_mut(4) {
            let a = px[3] as u16;
            if a < 255 {
                px[0] = ((px[0] as u16 * a + 127) / 255) as u8;
                px[1] = ((px[1] as u16 * a + 127) / 255) as u8;
                px[2] = ((px[2] as u16 * a + 127) / 255) as u8;
            }
        }
        let size = tiny_skia::IntSize::from_wh(width, height)?;
        let pixmap = tiny_skia::Pixmap::from_vec(rgba, size)?;
        Some(ImageData { pixmap, width, height, density: 1.0, nat: None, svg_par: None })
    }

    pub fn bytes(&self) -> usize {
        (self.width * self.height * 4) as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        Rect { x, y, w: (r - x).max(0.0), h: (b - y).max(0.0) }
    }
    pub fn inflate(&self, d: f32) -> Rect {
        Rect { x: self.x - d, y: self.y - d, w: self.w + 2.0 * d, h: self.h + 2.0 * d }
    }
    pub fn union(&self, o: &Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect { x, y, w: self.right().max(o.right()) - x, h: self.bottom().max(o.bottom()) - y }
    }
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect { x: self.x + dx, y: self.y + dy, ..*self }
    }
}

/// A gradient resolved to document coordinates (thread-safe).
#[derive(Clone, Debug)]
pub struct GradSpec {
    /// 0 = linear, 1 = radial, 2 = conic
    pub kind: u8,
    pub start: (f32, f32),
    pub end: (f32, f32),
    pub radius: (f32, f32),
    pub stops: Vec<(f32, Color)>,
    pub repeating: bool,
    /// Conic start angle in degrees (clockwise from up).
    pub angle: f32,
}

#[derive(Clone)]
pub struct GlyphRun {
    pub font: Arc<Font>,
    pub size: f32,
    pub glyphs: Vec<(u16, f32, f32)>,
}

/// Corner radii of a rounded rectangle: horizontal (`x`) and vertical (`y`)
/// radius per corner, in the order top-left, top-right, bottom-right,
/// bottom-left. A corner is square unless both of its radii are positive.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Radii {
    pub x: [f32; 4],
    pub y: [f32; 4],
}

impl Radii {
    pub const ZERO: Radii = Radii { x: [0.0; 4], y: [0.0; 4] };
    /// The same circular radius on every corner.
    pub fn all(r: f32) -> Radii {
        Radii { x: [r; 4], y: [r; 4] }
    }
    /// Scaled down uniformly so adjacent corners don't overlap in a w×h box (css-backgrounds-3 §5.5).
    pub fn scaled_to(self, w: f32, h: f32) -> Radii {
        let f = [w / (self.x[0] + self.x[1]).max(0.001), h / (self.y[1] + self.y[2]).max(0.001), w / (self.x[2] + self.x[3]).max(0.001), h / (self.y[3] + self.y[0]).max(0.001)].iter().fold(1.0f32, |a, &b| a.min(b));
        if f < 1.0 {
            self.map(|v| v * f)
        } else {
            self
        }
    }
    /// True if any corner is rounded.
    pub fn any(&self) -> bool {
        (0..4).any(|i| self.x[i] > 0.0 && self.y[i] > 0.0)
    }
    /// Apply `f` to every radius.
    pub fn map(self, f: impl Fn(f32) -> f32) -> Radii {
        Radii { x: self.x.map(&f), y: self.y.map(&f) }
    }
    /// Radii of a rect inset by `d` = [top, right, bottom, left] (negative grows),
    /// as for the padding edge inside a border (css-backgrounds-3 §5.2).
    pub fn inset(self, d: [f32; 4]) -> Radii {
        let (t, r, b, l) = (d[0], d[1], d[2], d[3]);
        Radii {
            x: [(self.x[0] - l).max(0.0), (self.x[1] - r).max(0.0), (self.x[2] - r).max(0.0), (self.x[3] - l).max(0.0)],
            y: [(self.y[0] - t).max(0.0), (self.y[1] - t).max(0.0), (self.y[2] - b).max(0.0), (self.y[3] - b).max(0.0)],
        }
    }
}

/// A resolved CSS filter function (lengths in CSS px, amounts as fractions).
#[derive(Clone, Debug)]
pub enum Filter {
    Blur(f32),
    /// 4×5 color matrix applied to unpremultiplied RGBA (row-major, last column is the offset).
    ColorMatrix([f32; 20]),
    DropShadow { x: f32, y: f32, blur: f32, color: Color },
}

#[derive(Clone)]
pub enum Item {
    Rect { r: Rect, color: Color },
    RRect { r: Rect, radii: Radii, color: Color },
    Border { r: Rect, widths: [f32; 4], colors: [Color; 4], styles: [BorderStyle; 4], radii: Radii },
    Text { x: f32, y: f32, run: Arc<GlyphRun>, color: Color },
    Image { r: Rect, img: Arc<ImageData>, radii: Option<Radii>, pixelated: bool },
    Pattern { area: Rect, tile: Rect, img: Arc<ImageData>, repeat_x: bool, repeat_y: bool, fixed: bool },
    Gradient { r: Rect, grad: Arc<GradSpec>, radii: Radii },
    /// Mask image (alpha) tinted with `color`, tiled per `tile` over `area`.
    MaskedColor { area: Rect, tile: Rect, img: Arc<ImageData>, color: Color },
    /// A gradient image of size `tile` (spec relative to the tile's own origin) repeated over `area`.
    GradientTiles { area: Rect, tile: Rect, grad: Arc<GradSpec> },
    /// Outer box-shadow: shape `r` (already offset/spread), blurred; `hole` (the
    /// element's border box) is cut out so the shadow never shows through it.
    Shadow { r: Rect, radii: Radii, blur: f32, spread: f32, color: Color, inset: bool, hole: Option<(Rect, Radii)> },
    /// Inset box-shadow: fills the padding box `r` outside the (blurred) `hole`.
    InsetShadow { r: Rect, radii: Radii, hole: Rect, hole_radii: Radii, blur: f32, color: Color },
    Line { x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Color, style: BorderStyle },
    PushClip { r: Rect, radii: Option<Radii> },
    /// Clip to an arbitrary path in document coordinates (`clip-path`); ends with PopClip.
    PushClipPath(Arc<tiny_skia::Path>),
    PopClip,
    /// Render the following items into a layer whose alpha is multiplied by
    /// the mask image drawn by these items (`mask-image`).
    PushMask(Arc<Vec<Item>>),
    PopMask,
    PushOpacity(f32),
    PopOpacity,
    /// Render into a group composited with a blend mode (0 = normal: an isolated group).
    PushBlend(u8),
    PopBlend,
    /// Group opacity: render into a layer composited with this alpha
    /// (`bounds`: where the group draws, in the current coordinates, if known).
    PushGroup { opacity: f32, bounds: Option<Rect> },
    PopGroup,
    PushTransform([f32; 6]),
    PopTransform,
    /// Render the following items (up to the matching PopWarp) into a layer
    /// covering `bounds` (current coordinates), then composite it through the
    /// projective map `h` (row-major 3x3 on (x, y, 1)): an element with a 3D
    /// transform flattened into the plane it is drawn on.
    PushWarp { h: [f64; 9], bounds: Rect },
    PopWarp,
    /// Render the following items into an offscreen layer, then apply the
    /// filter chain and composite it back (CSS `filter`).
    PushFilter(Arc<Vec<Filter>>),
    PopFilter,
    /// `backdrop-filter`: filter what is already drawn under `r`.
    Backdrop { r: Rect, filters: Arc<Vec<Filter>> },
    /// Start/end of `position: fixed` content (viewport coordinates), kept
    /// inline so it paints in correct z-order.
    BeginFixed,
    EndFixed,
    /// Content of a box stuck to the viewport: offset at raster time by how
    /// far its sticky offset at the current scroll differs from `applied`.
    PushSticky(std::sync::Arc<StickyItem>),
    PopSticky,
}

impl std::fmt::Debug for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let r4 = |r: &Rect| format!("[{:.1},{:.1} {:.1}x{:.1}]", r.x, r.y, r.w, r.h);
        match self {
            Item::Rect { r, color } => write!(f, "Rect {} {:?}", r4(r), color),
            Item::RRect { r, radii, color } => write!(f, "RRect {} {:?} {:?}", r4(r), radii, color),
            Item::Border { r, widths, radii, .. } => write!(f, "Border {} w={:?} radii={:?}", r4(r), widths, radii),
            Item::Text { x, y, run, .. } => write!(f, "Text {:.1},{:.1} glyphs={} size={}", x, y, run.glyphs.len(), run.size),
            Item::Image { r, .. } => write!(f, "Image {}", r4(r)),
            Item::Pattern { area, tile, .. } => write!(f, "Pattern {} tile={}", r4(area), r4(tile)),
            Item::Gradient { r, .. } => write!(f, "Gradient {}", r4(r)),
            Item::GradientTiles { area, tile, .. } => write!(f, "GradientTiles {} tile={}", r4(area), r4(tile)),
            Item::MaskedColor { area, tile, color, .. } => write!(f, "MaskedColor {} tile={} {:?}", r4(area), r4(tile), color),
            Item::Shadow { r, blur, spread, inset, .. } => write!(f, "Shadow {} blur={} spread={} inset={}", r4(r), blur, spread, inset),
            Item::InsetShadow { r, hole, .. } => write!(f, "InsetShadow {} hole={}", r4(r), r4(hole)),
            Item::Line { x0, y0, x1, y1, width, .. } => write!(f, "Line {:.1},{:.1}-{:.1},{:.1} w={}", x0, y0, x1, y1, width),
            Item::PushClip { r, radii } => write!(f, "PushClip {} {:?}", r4(r), radii),
            Item::PopClip => write!(f, "PopClip"),
            Item::PushClipPath(p) => write!(f, "PushClipPath {:?}", p.bounds()),
            Item::PushMask(m) => write!(f, "PushMask ({} items)", m.len()),
            Item::PopMask => write!(f, "PopMask"),
            Item::PushOpacity(o) => write!(f, "PushOpacity {o}"),
            Item::PopOpacity => write!(f, "PopOpacity"),
            Item::PushBlend(m) => write!(f, "PushBlend {m}"),
            Item::PopBlend => write!(f, "PopBlend"),
            Item::PushGroup { opacity, .. } => write!(f, "PushGroup {opacity}"),
            Item::PopGroup => write!(f, "PopGroup"),
            Item::PushTransform(m) => write!(f, "PushTransform {:?}", m),
            Item::PopTransform => write!(f, "PopTransform"),
            Item::PushWarp { h, bounds } => write!(f, "PushWarp {:?} {}", h, r4(bounds)),
            Item::PopWarp => write!(f, "PopWarp"),
            Item::PushFilter(fl) => write!(f, "PushFilter {:?}", fl),
            Item::PopFilter => write!(f, "PopFilter"),
            Item::Backdrop { r, filters } => write!(f, "Backdrop {} {:?}", r4(r), filters),
            Item::BeginFixed => write!(f, "BeginFixed"),
            Item::PushSticky(s) => write!(f, "PushSticky({:?})", s.applied),
            Item::PopSticky => write!(f, "PopSticky"),
            Item::EndFixed => write!(f, "EndFixed"),
        }
    }
}

impl Item {
    /// Whether this item paints pixels (as opposed to state push/pop).
    pub fn draws(&self) -> bool {
        !matches!(
            self,
            Item::PushClip { .. } | Item::PopClip | Item::PushClipPath(_) | Item::PushMask(_) | Item::PopMask | Item::PushOpacity(_) | Item::PopOpacity | Item::PushTransform(_) | Item::PopTransform | Item::PushWarp { .. } | Item::PopWarp | Item::PushFilter(_) | Item::PopFilter | Item::PushBlend(_) | Item::PopBlend | Item::PushGroup { .. } | Item::PopGroup | Item::BeginFixed | Item::EndFixed | Item::PushSticky(_) | Item::PopSticky
        )
    }

    /// Conservative bounds (document coordinates) for culling. None = always process.
    pub fn bounds(&self) -> Option<Rect> {
        Some(match self {
            Item::Rect { r, .. } | Item::RRect { r, .. } | Item::Border { r, .. } | Item::Image { r, .. } | Item::Gradient { r, .. } => *r,
            Item::Pattern { area, .. } | Item::GradientTiles { area, .. } | Item::MaskedColor { area, .. } => *area,
            Item::Text { x, y, run, .. } => {
                let w = run.glyphs.last().map(|g| g.1 + run.size).unwrap_or(0.0);
                Rect::new(x - run.size * 0.5, y - run.size * 1.5, w + run.size, run.size * 2.5)
            }
            Item::Shadow { r, blur, spread, .. } => r.inflate(blur * 2.0 + spread.abs() + 2.0),
            Item::InsetShadow { r, .. } => *r,
            Item::Backdrop { r, filters } => {
                let e: f32 = filters.iter().map(|f| if let Filter::Blur(s) = f { 3.0 * s } else { 0.0 }).sum();
                r.inflate(e + 2.0)
            }
            Item::Line { x0, y0, x1, y1, width, .. } => {
                Rect::new(x0.min(*x1) - width, y0.min(*y1) - width, (x1 - x0).abs() + 2.0 * width, (y1 - y0).abs() + 2.0 * width)
            }
            _ => return None,
        })
    }
}

#[derive(Default)]
pub struct DisplayList {
    pub items: Vec<Item>,
    /// Number of `position: fixed` groups in `items` (they disable scroll blitting).
    pub fixed: usize,
    pub bg: Color,
}

#[derive(Debug)]
pub struct StickyItem {
    pub geom: crate::layout::StickyGeom,
    pub applied: (f32, f32),
    pub viewport: (f32, f32),
}

impl StickyItem {
    /// Extra offset at document scroll `scroll`.
    pub fn delta(&self, scroll: (f32, f32)) -> (f32, f32) {
        let (dx, dy) = self.geom.offset([scroll.0, scroll.1, scroll.0 + self.viewport.0, scroll.1 + self.viewport.1]);
        (dx - self.applied.0, dy - self.applied.1)
    }
}

#[derive(Clone, Debug)]
pub struct HitRegion {
    pub rect: Rect,
    pub clip: Option<Rect>,
    pub node: usize,
    pub fixed: bool,
    pub cursor: crate::css::style::Cursor,
    pub pointer_events: bool,
    /// Scroll container box this region scrolls with (node id), if any.
    pub scroller: Option<usize>,
}


#[cfg(test)]
mod tests {
    use super::Radii;

    #[test]
    fn radii_inset_per_axis() {
        let r = Radii { x: [20.0, 20.0, 20.0, 20.0], y: [10.0, 10.0, 10.0, 10.0] };
        // top 2, right 4, bottom 6, left 8
        let i = r.inset([2.0, 4.0, 6.0, 8.0]);
        assert_eq!(i.x, [12.0, 16.0, 16.0, 12.0]);
        assert_eq!(i.y, [8.0, 8.0, 4.0, 4.0]);
        assert!(Radii::all(3.0).any());
        assert!(!Radii { x: [5.0; 4], y: [0.0; 4] }.any());
    }
}
