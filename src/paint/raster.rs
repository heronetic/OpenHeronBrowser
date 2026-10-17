// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Software rasterizer for display lists (tiny-skia + a custom glyph blitter).

use std::collections::HashMap;
use std::rc::Rc;

use tiny_skia::{FillRule, Mask, Paint, PathBuilder, Pixmap, Shader, Stroke, Transform};

use super::*;
use crate::css::values::Color;

// ------------------------------------------------------------------ glyphs

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    font: u32,
    glyph: u16,
    size: u32,
    subpx: u8,
}

pub struct GlyphBitmap {
    left: i32,
    top: i32,
    w: u32,
    h: u32,
    data: Vec<u8>,
    color: bool,
}

#[derive(Default)]
pub struct GlyphCache {
    map: HashMap<GlyphKey, Option<Rc<GlyphBitmap>>>,
    bytes: usize,
}

const GLYPH_CACHE_BUDGET: usize = 24 * 1024 * 1024;

struct OutlineToPath {
    pb: PathBuilder,
    sx: f32,
    sy: f32,
    dx: f32,
    skew: f32,
}

impl OutlineToPath {
    fn tx(&self, x: f32, y: f32) -> (f32, f32) {
        let px = x * self.sx + self.dx + y * self.sx * self.skew;
        let py = -y * self.sy;
        (px, py)
    }
}

impl ttf_parser::OutlineBuilder for OutlineToPath {
    fn move_to(&mut self, x: f32, y: f32) {
        let (a, b) = self.tx(x, y);
        self.pb.move_to(a, b);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (a, b) = self.tx(x, y);
        self.pb.line_to(a, b);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (a, b) = self.tx(x1, y1);
        let (c, d) = self.tx(x, y);
        self.pb.quad_to(a, b, c, d);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (a, b) = self.tx(x1, y1);
        let (c, d) = self.tx(x2, y2);
        let (e, f) = self.tx(x, y);
        self.pb.cubic_to(a, b, c, d, e, f);
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

/// COLR painter: paints layers with solid colors (gradients approximated).
struct ColrPainter<'a> {
    face: &'a ttf_parser::Face<'a>,
    pixmap: Pixmap,
    scale: f32,
    off: (f32, f32),
    path: Option<tiny_skia::Path>,
    transforms: Vec<Transform>,
}

impl<'a> ttf_parser::colr::Painter<'a> for ColrPainter<'a> {
    fn outline_glyph(&mut self, glyph_id: ttf_parser::GlyphId) {
        let mut b = OutlineToPath { pb: PathBuilder::new(), sx: self.scale, sy: self.scale, dx: 0.0, skew: 0.0 };
        self.path = self.face.outline_glyph(glyph_id, &mut b).and_then(|_| b.pb.finish());
    }
    fn paint(&mut self, paint: ttf_parser::colr::Paint<'a>) {
        let color = match paint {
            ttf_parser::colr::Paint::Solid(c) => c,
            ttf_parser::colr::Paint::LinearGradient(g) => g.stops(0, &[]).next().map(|s| s.color).unwrap_or(ttf_parser::RgbaColor::new(0, 0, 0, 255)),
            ttf_parser::colr::Paint::RadialGradient(g) => g.stops(0, &[]).next().map(|s| s.color).unwrap_or(ttf_parser::RgbaColor::new(0, 0, 0, 255)),
            ttf_parser::colr::Paint::SweepGradient(g) => g.stops(0, &[]).next().map(|s| s.color).unwrap_or(ttf_parser::RgbaColor::new(0, 0, 0, 255)),
        };
        if let Some(p) = &self.path {
            let mut paint = Paint::default();
            paint.set_color_rgba8(color.red, color.green, color.blue, color.alpha);
            paint.anti_alias = true;
            let mut t = Transform::from_translate(self.off.0, self.off.1);
            for tr in &self.transforms {
                t = t.pre_concat(*tr);
            }
            self.pixmap.fill_path(p, &paint, FillRule::Winding, t, None);
        }
    }
    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, _clipbox: ttf_parser::colr::ClipBox) {}
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _mode: ttf_parser::colr::CompositeMode) {}
    fn pop_layer(&mut self) {}
    fn push_transform(&mut self, t: ttf_parser::Transform) {
        // Font-unit transform; convert translation to pixels with y flip.
        self.transforms.push(Transform::from_row(t.a, -t.b, -t.c, t.d, t.e * self.scale, -t.f * self.scale));
    }
    fn pop_transform(&mut self) {
        self.transforms.pop();
    }
}

impl GlyphCache {
    fn get(&mut self, font: &crate::text::Font, glyph: u16, size: f32, subpx: u8) -> Option<Rc<GlyphBitmap>> {
        let key = GlyphKey { font: font.uid, glyph, size: (size * 16.0).round() as u32, subpx };
        if let Some(g) = self.map.get(&key) {
            return g.clone();
        }
        let g = rasterize_glyph(font, glyph, size, subpx).map(Rc::new);
        if let Some(b) = &g {
            self.bytes += b.data.len() + 64;
        }
        if self.bytes > GLYPH_CACHE_BUDGET {
            self.map.clear();
            self.bytes = 0;
        }
        self.map.insert(key, g.clone());
        g
    }
}

/// Whether a glyph draws any pixels (a sanity check for system fonts that
/// parse but can't be drawn here).
pub fn glyph_has_ink(font: &crate::text::Font, glyph: u16) -> bool {
    rasterize_glyph(font, glyph, 24.0, 0).map(|g| g.data.iter().any(|&a| a > 0)).unwrap_or(false)
}

fn rasterize_glyph(font: &crate::text::Font, glyph: u16, size: f32, subpx: u8) -> Option<GlyphBitmap> {
    if size <= 0.5 || size > 2000.0 {
        return None;
    }
    let face = font.face();
    let gid = ttf_parser::GlyphId(glyph);
    let scale = size / font.upem;
    // Bitmap color glyphs (sbix / CBDT).
    if font.has_color {
        if let Some(img) = face.glyph_raster_image(gid, size.round().clamp(1.0, 65535.0) as u16) {
            if let ttf_parser::RasterImageFormat::PNG = img.format {
                if let Ok(dec) = image::load_from_memory_with_format(img.data, image::ImageFormat::Png) {
                    let k = size / img.pixels_per_em as f32;
                    let w = ((img.width as f32) * k).round().max(1.0) as u32;
                    let h = ((img.height as f32) * k).round().max(1.0) as u32;
                    let rgba = dec.resize_exact(w, h, image::imageops::FilterType::Triangle).to_rgba8();
                    let mut data = rgba.into_raw();
                    for px in data.chunks_exact_mut(4) {
                        let a = px[3] as u16;
                        px[0] = (px[0] as u16 * a / 255) as u8;
                        px[1] = (px[1] as u16 * a / 255) as u8;
                        px[2] = (px[2] as u16 * a / 255) as u8;
                    }
                    let left = (img.x as f32 * k).round() as i32;
                    let top = -((img.y as f32 + img.height as f32) * k).round() as i32;
                    return Some(GlyphBitmap { left, top, w, h, data, color: true });
                }
            }
        }
        if face.is_color_glyph(gid) {
            let bbox = face.glyph_bounding_box(gid).or_else(|| Some(ttf_parser::Rect { x_min: 0, y_min: -(font.descent as i16), x_max: font.upem as i16, y_max: font.ascent as i16 }))?;
            let left = (bbox.x_min as f32 * scale).floor() as i32 - 1;
            let top = (-(bbox.y_max as f32) * scale).floor() as i32 - 1;
            let w = ((bbox.x_max - bbox.x_min) as f32 * scale).ceil() as u32 + 3;
            let h = ((bbox.y_max - bbox.y_min) as f32 * scale).ceil() as u32 + 3;
            let pixmap = Pixmap::new(w.max(1), h.max(1))?;
            let mut painter = ColrPainter { face, pixmap, scale, off: (-left as f32, -top as f32), path: None, transforms: Vec::new() };
            face.paint_color_glyph(gid, 0, ttf_parser::RgbaColor::new(0, 0, 0, 255), &mut painter)?;
            let data = painter.pixmap.take();
            return Some(GlyphBitmap { left, top, w: w.max(1), h: h.max(1), data, color: true });
        }
    }
    let skew = if font.synth_italic { 0.2 } else { 0.0 };
    let mut b = OutlineToPath { pb: PathBuilder::new(), sx: scale, sy: scale, dx: subpx as f32 / 4.0, skew };
    face.outline_glyph(gid, &mut b)?;
    let path = b.pb.finish()?;
    let bounds = path.bounds();
    let pad = if font.synth_bold { (size / 24.0).ceil() as i32 + 1 } else { 1 };
    let left = bounds.left().floor() as i32 - pad;
    let top = bounds.top().floor() as i32 - pad;
    let w = (bounds.right().ceil() as i32 + pad - left).max(1) as u32;
    let h = (bounds.bottom().ceil() as i32 + pad - top).max(1) as u32;
    if w > 4096 || h > 4096 {
        return None;
    }
    let mut pm = Pixmap::new(w, h)?;
    let mut paint = Paint::default();
    paint.set_color_rgba8(0, 0, 0, 255);
    paint.anti_alias = true;
    let t = Transform::from_translate(-left as f32, -top as f32);
    pm.fill_path(&path, &paint, FillRule::Winding, t, None);
    if font.synth_bold {
        let stroke = Stroke { width: size / 24.0, ..Default::default() };
        pm.stroke_path(&path, &paint, &stroke, t, None);
    }
    let data: Vec<u8> = pm.data().chunks_exact(4).map(|p| p[3]).collect();
    Some(GlyphBitmap { left, top, w, h, data, color: false })
}

/// Outline of a glyph as a path in px (origin at the pen position, y down).
pub fn glyph_path(font: &crate::text::Font, gid: u16, size: f32) -> Option<tiny_skia::Path> {
    let scale = font.scale(size);
    let skew = if font.synth_italic { 0.2 } else { 0.0 };
    let mut b = OutlineToPath { pb: PathBuilder::new(), sx: scale, sy: scale, dx: 0.0, skew };
    font.face().outline_glyph(ttf_parser::GlyphId(gid), &mut b)?;
    b.pb.finish()
}

// --------------------------------------------------------------- rasterizer

struct ClipLevel {
    /// Device-space clip rectangle (intersection with parents).
    rect: (f32, f32, f32, f32),
    /// Rounded shape in device space (path), if any at this level.
    shape: Option<tiny_skia::Path>,
    any_round: bool,
    mask: Option<Option<Mask>>,
}

pub struct Rasterizer {
    pub glyphs: GlyphCache,
    /// 0: draw everything; 1: skip `position: fixed` groups; 2: only them
    /// (the UI caches the scrolling content and overlays the fixed layer).
    pub fixed_mode: u8,
    /// Downscaled copies of images drawn much smaller than their natural size,
    /// keyed by (image, device width, device height); (image kept alive, copy).
    scaled: std::collections::HashMap<(usize, u32, u32), (Arc<ImageData>, Arc<Pixmap>)>,
    /// Bytes the cache keeps alive (the source images and the copies): it is
    /// bounded by size, not only by count, since sources can be large
    /// (frame snapshots that change every frame).
    scaled_bytes: usize,
}

impl Default for Rasterizer {
    fn default() -> Self {
        Self::new()
    }
}

/// An offscreen layer for CSS `filter` (None when the layer limit was hit
/// and the content is drawn straight into the layer below).
struct Layer {
    pm: Option<Pixmap>,
    filters: std::sync::Arc<Vec<Filter>>,
    /// Items drawing the mask image of a `mask` layer.
    mask: Option<std::sync::Arc<Vec<Item>>>,
    /// Device clip rectangle when the layer started.
    clip: (f32, f32, f32, f32),
    /// Blend-mode group (`mix-blend-mode`, `background-blend-mode`, isolation).
    blend: Option<tiny_skia::BlendMode>,
    /// Group opacity applied when compositing the layer.
    opacity: f32,
    /// Device area to composite (None: the whole layer).
    dev_bounds: Option<(f32, f32, f32, f32)>,
}

struct Ctx<'p> {
    pm: &'p mut Pixmap,
    layers: Vec<Layer>,
    transforms: Vec<Transform>,
    clips: Vec<ClipLevel>,
    opacity: Vec<f32>,
    view: Rect,
    scale: f32,
    /// Document scroll offset (CSS px) for `background-attachment: fixed`.
    scroll: (f32, f32),
}

/// A run of `count` adjacent sides' piece of a rounded border ring, starting
/// at `start` (0 top, 1 right, 2 bottom, 3 left), split at the corners where
/// the line from the outer to the inner corner crosses the ring, like Blink.
fn rounded_side_path(r: Rect, radii: Radii, w: [f32; 4], start: usize, count: usize) -> Option<tiny_skia::Path> {
    use std::f32::consts::{FRAC_PI_2, PI};
    let (wt, wr, wb, wl) = (w[0], w[1], w[2], w[3]);
    let inner = Rect::new(r.x + wl, r.y + wt, (r.w - wl - wr).max(0.0), (r.h - wt - wb).max(0.0));
    let ir = radii.inset(w);
    // Corner centers (outer and inner) for TL, TR, BR, BL.
    let oc = [
        (r.x + radii.x[0], r.y + radii.y[0]),
        (r.right() - radii.x[1], r.y + radii.y[1]),
        (r.right() - radii.x[2], r.bottom() - radii.y[2]),
        (r.x + radii.x[3], r.bottom() - radii.y[3]),
    ];
    let icn = [
        (inner.x + ir.x[0], inner.y + ir.y[0]),
        (inner.right() - ir.x[1], inner.y + ir.y[1]),
        (inner.right() - ir.x[2], inner.bottom() - ir.y[2]),
        (inner.x + ir.x[3], inner.bottom() - ir.y[3]),
    ];
    let split = [
        PI + wl.atan2(wt),        // TL: left [180°, s], top [s, 270°]
        1.5 * PI + wt.atan2(wr),  // TR: top [270°, s], right [s, 360°]
        wr.atan2(wb),             // BR: right [0°, s], bottom [s, 90°]
        FRAC_PI_2 + wb.atan2(wl), // BL: bottom [90°, s], left [s, 180°]
    ];
    // Per side: (first corner, arc from, to) and (second corner, arc from, to).
    let arcs = |side: usize| match side {
        0 => ((0, split[0], 1.5 * PI), (1, 1.5 * PI, split[1])),
        1 => ((1, split[1], 2.0 * PI), (2, 0.0, split[2])),
        2 => ((2, split[2], FRAC_PI_2), (3, FRAC_PI_2, split[3])),
        _ => ((3, split[3], PI), (0, PI, split[0])),
    };
    let arc = |pts: &mut Vec<(f32, f32)>, c: (f32, f32), rx: f32, ry: f32, a: f32, b: f32| {
        let n = (((b - a).abs() / (PI / 24.0)).ceil() as usize).clamp(1, 24);
        for k in 0..=n {
            let t = a + (b - a) * k as f32 / n as f32;
            pts.push((c.0 + rx * t.cos(), c.1 + ry * t.sin()));
        }
    };
    let sides: Vec<usize> = (0..count.min(4)).map(|k| (start + k) % 4).collect();
    let mut pts: Vec<(f32, f32)> = Vec::new();
    for &s in &sides {
        let ((c1, a1, b1), (c2, a2, b2)) = arcs(s);
        arc(&mut pts, oc[c1], radii.x[c1], radii.y[c1], a1, b1);
        arc(&mut pts, oc[c2], radii.x[c2], radii.y[c2], a2, b2);
    }
    for &s in sides.iter().rev() {
        let ((c1, a1, b1), (c2, a2, b2)) = arcs(s);
        arc(&mut pts, icn[c2], ir.x[c2], ir.y[c2], b2, a2);
        arc(&mut pts, icn[c1], ir.x[c1], ir.y[c1], b1, a1);
    }
    let mut pb = PathBuilder::new();
    pb.move_to(pts[0].0, pts[0].1);
    for p in &pts[1..] {
        pb.line_to(p.0, p.1);
    }
    pb.close();
    pb.finish()
}

pub(crate) fn rrect_path(r: Rect, radii: Radii) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let k = 0.447_715_2; // 1 - kappa
    let (x, y, w, h) = (r.x, r.y, r.w, r.h);
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    // Per corner (horizontal, vertical) radius; square unless both are positive.
    let c = |i: usize| if radii.x[i] > 0.0 && radii.y[i] > 0.0 { (radii.x[i], radii.y[i]) } else { (0.0, 0.0) };
    let ((tlx, tly), (trx, try_), (brx, bry), (blx, bly)) = (c(0), c(1), c(2), c(3));
    pb.move_to(x + tlx, y);
    pb.line_to(x + w - trx, y);
    if trx > 0.0 {
        pb.cubic_to(x + w - trx * k, y, x + w, y + try_ * k, x + w, y + try_);
    }
    pb.line_to(x + w, y + h - bry);
    if brx > 0.0 {
        pb.cubic_to(x + w, y + h - bry * k, x + w - brx * k, y + h, x + w - brx, y + h);
    }
    pb.line_to(x + blx, y + h);
    if blx > 0.0 {
        pb.cubic_to(x + blx * k, y + h, x, y + h - bly * k, x, y + h - bly);
    }
    pb.line_to(x, y + tly);
    if tlx > 0.0 {
        pb.cubic_to(x, y + tly * k, x + tlx * k, y, x + tlx, y);
    }
    pb.close();
    pb.finish()
}

/// Blink's dash pattern for a dashed or dotted stroke of `len` px and thickness `w`
/// (StrokeData::SetupPaintDashPathEffect): 3:2 dashes below 3px, 2:1 above, with the
/// gap stretched so the stroke starts and ends on a dash. Returns the stroke dash and
/// whether the caps are round (wide dots).
fn blink_dash(len: f32, w: f32, dotted: bool, closed: bool) -> (Option<tiny_skia::StrokeDash>, bool) {
    let best_gap = |dash: f32, gap: f32| -> f32 {
        let avail = if closed { len } else { len + gap };
        let min_n = (avail / (dash + gap)).floor().max(1.0);
        let max_n = min_n + 1.0;
        let (min_g, max_g) = if closed { (min_n, max_n) } else { (min_n - 1.0, max_n - 1.0) };
        let min_gap = if min_g > 0.0 { (len - min_n * dash) / min_g } else { f32::INFINITY };
        let max_gap = (len - max_n * dash) / max_g;
        if max_gap <= 0.0 || (min_gap - gap).abs() < (max_gap - gap).abs() {
            min_gap
        } else {
            max_gap
        }
    };
    let w = w.max(0.5);
    if !dotted || w <= 3.0 {
        let (dash, gap) = if dotted { (w, w) } else if w >= 3.0 { (2.0 * w, w) } else { (3.0 * w, 2.0 * w) };
        if len <= dash * 2.0 {
            return (None, false);
        }
        let two = 2.0 * dash + gap + if closed { gap } else { 0.0 };
        let (d, g) = if len <= two {
            let m = len / two;
            (dash * m, gap * m)
        } else {
            let g = best_gap(dash, gap);
            (dash, if g.is_finite() && g > 0.0 { g } else { gap })
        };
        return (tiny_skia::StrokeDash::new(vec![d, g], 0.0), false);
    }
    let per = w * 2.0;
    if len < per {
        return (tiny_skia::StrokeDash::new(vec![0.01, per], 0.0), true);
    }
    let g = best_gap(w, w);
    let g = if g.is_finite() && g > 0.0 { g } else { w };
    (tiny_skia::StrokeDash::new(vec![0.01, (g + w - 0.01 - 0.01).max(0.1)], 0.0), true)
}

/// Approximate length of a rounded rect's outline.
fn rrect_length(r: Rect, radii: Radii) -> f32 {
    let mut len = 2.0 * (r.w + r.h);
    for i in 0..4 {
        let (a, b) = (radii.x[i], radii.y[i]);
        if a > 0.0 && b > 0.0 {
            // Ramanujan's ellipse perimeter, a quarter of it.
            let h = ((a - b) / (a + b)).powi(2);
            let q = std::f32::consts::PI * (a + b) * (1.0 + 3.0 * h / (10.0 + (4.0 - 3.0 * h).sqrt())) / 4.0;
            len += q - a - b;
        }
    }
    len
}

fn rect_path(r: Rect) -> Option<tiny_skia::Path> {
    tiny_skia::Rect::from_xywh(r.x, r.y, r.w.max(0.01), r.h.max(0.01)).map(PathBuilder::from_rect)
}

fn ts_color(c: Color, opacity: f32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, (c.a as f32 * opacity).round().clamp(0.0, 255.0) as u8)
}

fn paint_color(c: Color, opacity: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(ts_color(c, opacity));
    p.anti_alias = true;
    p
}

fn darken(c: Color, f: f32) -> Color {
    Color::rgba((c.r as f32 * f) as u8, (c.g as f32 * f) as u8, (c.b as f32 * f) as u8, c.a)
}

fn lighten(c: Color, f: f32) -> Color {
    let l = |v: u8| (v as f32 + (255.0 - v as f32) * f) as u8;
    Color::rgba(l(c.r), l(c.g), l(c.b), c.a)
}

impl<'p> Ctx<'p> {
    /// The pixmap drawing currently goes to: the innermost live layer, or the target.
    fn pmx(&mut self) -> &mut Pixmap {
        for l in self.layers.iter_mut().rev() {
            if let Some(pm) = l.pm.as_mut() {
                return pm;
            }
        }
        self.pm
    }
    fn t(&self) -> Transform {
        *self.transforms.last().unwrap()
    }
    fn op(&self) -> f32 {
        *self.opacity.last().unwrap_or(&1.0)
    }
    /// Snap a rectangle's edges to device pixels (axis-aligned transforms only).
    fn snap(&self, r: Rect) -> Rect {
        let t = self.t();
        if !self.simple_transform() || t.sx == 0.0 || t.sy == 0.0 || r.w < 1.0 || r.h < 1.0 {
            return r;
        }
        let x0 = ((r.x * t.sx + t.tx).round() - t.tx) / t.sx;
        let y0 = ((r.y * t.sy + t.ty).round() - t.ty) / t.sy;
        let x1 = ((r.right() * t.sx + t.tx).round() - t.tx) / t.sx;
        let y1 = ((r.bottom() * t.sy + t.ty).round() - t.ty) / t.sy;
        Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
    }
    /// Snap a border width to whole device pixels (at least one when non-zero).
    fn snap_w(&self, w: f32) -> f32 {
        let t = self.t();
        if w <= 0.0 || !self.simple_transform() || t.sx == 0.0 {
            return w;
        }
        (w * t.sx).round().max(1.0) / t.sx
    }
    fn simple_transform(&self) -> bool {
        let t = self.t();
        t.kx == 0.0 && t.ky == 0.0
    }
    fn to_dev(&self, r: Rect) -> (f32, f32, f32, f32) {
        let t = self.t();
        if self.simple_transform() {
            let x0 = r.x * t.sx + t.tx;
            let y0 = r.y * t.sy + t.ty;
            let x1 = r.right() * t.sx + t.tx;
            let y1 = r.bottom() * t.sy + t.ty;
            (x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
        } else {
            let pts = [(r.x, r.y), (r.right(), r.y), (r.x, r.bottom()), (r.right(), r.bottom())];
            let mut mn = (f32::INFINITY, f32::INFINITY);
            let mut mx = (f32::NEG_INFINITY, f32::NEG_INFINITY);
            for (x, y) in pts {
                let px = x * t.sx + y * t.kx + t.tx;
                let py = x * t.ky + y * t.sy + t.ty;
                mn = (mn.0.min(px), mn.1.min(py));
                mx = (mx.0.max(px), mx.1.max(py));
            }
            (mn.0, mn.1, mx.0, mx.1)
        }
    }
    fn clip_rect(&self) -> (f32, f32, f32, f32) {
        match self.clips.last() {
            Some(c) => c.rect,
            None => (0.0, 0.0, self.pm.width() as f32, self.pm.height() as f32),
        }
    }
    /// Decide how to draw an item with document bounds `b`:
    /// None = skip, Some(false) = draw without mask, Some(true) = needs mask.
    fn classify(&self, b: Rect) -> Option<bool> {
        let (x0, y0, x1, y1) = self.to_dev(b);
        let (cx0, cy0, cx1, cy1) = self.clip_rect();
        if x1 <= cx0 || y1 <= cy0 || x0 >= cx1 || y0 >= cy1 {
            return None;
        }
        // Clip edges on the surface's own edges need no mask: drawing clips
        // there anyway. (This also makes a partial repaint into a small
        // surface antialias exactly like the full one.)
        let (sw, sh) = (self.pm.width() as f32, self.pm.height() as f32);
        let in_x0 = x0 >= cx0 || cx0 <= 0.0;
        let in_y0 = y0 >= cy0 || cy0 <= 0.0;
        let in_x1 = x1 <= cx1 || cx1 >= sw;
        let in_y1 = y1 <= cy1 || cy1 >= sh;
        let inside = in_x0 && in_y0 && in_x1 && in_y1;
        let round = self.clips.last().map(|c| c.any_round).unwrap_or(false);
        Some(!inside || round)
    }
    fn mask(&mut self) -> Option<&Mask> {
        let n = self.clips.len();
        if n == 0 {
            return None;
        }
        if self.clips[n - 1].mask.is_none() {
            let t_mask = raster_prof().then(std::time::Instant::now);
            let _guard = MaskTimer(t_mask);
            let (w, h) = (self.pm.width(), self.pm.height());
            // Fast path: a single rounded shape inside the clip rectangle is
            // the whole mask (filling just the shape is far cheaper than
            // intersecting over the full surface).
            let shapes: Vec<&tiny_skia::Path> = self.clips.iter().filter_map(|l| l.shape.as_ref()).collect();
            if shapes.len() == 1 {
                let (x0, y0, x1, y1) = self.clips[n - 1].rect;
                let b = shapes[0].bounds();
                if b.left() >= x0 && b.top() >= y0 && b.right() <= x1 && b.bottom() <= y1 {
                    let m = Mask::new(w, h).map(|mut m| {
                        m.fill_path(shapes[0], FillRule::Winding, true, Transform::identity());
                        m
                    });
                    self.clips[n - 1].mask = Some(m);
                    return self.clips[n - 1].mask.as_ref().unwrap().as_ref();
                }
            }
            let m = Mask::new(w, h).and_then(|mut m| {
                let (x0, y0, x1, y1) = self.clips[n - 1].rect;
                let r = tiny_skia::Rect::from_ltrb(x0.max(0.0), y0.max(0.0), x1.max(x0 + 0.01), y1.max(y0 + 0.01))?;
                m.fill_path(&PathBuilder::from_rect(r), FillRule::Winding, true, Transform::identity());
                for lvl in &self.clips {
                    if let Some(shape) = &lvl.shape {
                        m.intersect_path(shape, FillRule::Winding, true, Transform::identity());
                    }
                }
                Some(m)
            });
            self.clips[n - 1].mask = Some(m);
        }
        self.clips[n - 1].mask.as_ref().unwrap().as_ref()
    }
    /// Draw a clipped fill through a small offscreen tile covering only the
    /// visible part of the item, with a tile-sized clip mask, instead of a
    /// clip mask the size of the whole surface. Returns false if not suitable.
    fn fill_path_tile(&mut self, path: &tiny_skia::Path, paint: &Paint, bounds: Rect) -> bool {
        if paint.blend_mode != tiny_skia::BlendMode::SourceOver {
            return false;
        }
        // (An already-built full mask for this clip level could be reused,
        // but then an item's antialiasing would depend on what was drawn
        // before it, and partial repaints must match full ones exactly.)
        let (x0, y0, x1, y1) = self.to_dev(bounds);
        let (cx0, cy0, cx1, cy1) = self.clip_rect();
        let (sw, sh) = {
            let pm = self.pmx();
            (pm.width() as f32, pm.height() as f32)
        };
        let rx0 = x0.max(cx0).max(0.0).floor();
        let ry0 = y0.max(cy0).max(0.0).floor();
        let rx1 = x1.min(cx1).min(sw).ceil();
        let ry1 = y1.min(cy1).min(sh).ceil();
        if rx1 <= rx0 || ry1 <= ry0 {
            return true;
        }
        let (rw, rh) = ((rx1 - rx0) as u32, (ry1 - ry0) as u32);
        // Only worth it when the tile is much smaller than the surface.
        if (rw as u64) * (rh as u64) * 4 > (sw as u64) * (sh as u64) {
            return false;
        }
        let shift = Transform::from_translate(-rx0, -ry0);
        let Some(mut mask) = Mask::new(rw, rh) else { return false };
        if let Some(r) = tiny_skia::Rect::from_ltrb(cx0 - rx0, cy0 - ry0, cx1 - rx0, cy1 - ry0) {
            mask.fill_path(&PathBuilder::from_rect(r), FillRule::Winding, true, Transform::identity());
        }
        for lvl in &self.clips {
            if let Some(shape) = &lvl.shape {
                mask.intersect_path(shape, FillRule::Winding, true, shift);
            }
        }
        let Some(mut tile) = Pixmap::new(rw, rh) else { return false };
        let t = self.t().post_concat(shift);
        tile.fill_path(path, paint, FillRule::EvenOdd, t, Some(&mask));
        self.pmx().draw_pixmap(rx0 as i32, ry0 as i32, tile.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
        true
    }

    fn fill_path(&mut self, path: &tiny_skia::Path, paint: &Paint, bounds: Rect) {
        let need_mask = match self.classify(bounds) {
            None => return,
            Some(m) => m,
        };
        let t = self.t();
        if need_mask && self.fill_path_tile(path, paint, bounds) {
            return;
        }
        if need_mask {
            if let Some(m) = self.mask() {
                let m = m as *const Mask;
                // SAFETY: the mask lives in self.clips, which isn't touched by fill_path.
                self.pmx().fill_path(path, paint, FillRule::EvenOdd, t, Some(unsafe { &*m }));
                return;
            }
        }
        self.pmx().fill_path(path, paint, FillRule::EvenOdd, t, None);
    }
    fn stroke_path(&mut self, path: &tiny_skia::Path, paint: &Paint, stroke: &Stroke, bounds: Rect) {
        let need_mask = match self.classify(bounds) {
            None => return,
            Some(m) => m,
        };
        let t = self.t();
        if need_mask {
            if let Some(m) = self.mask() {
                let m = m as *const Mask;
                // SAFETY: as above.
                self.pmx().stroke_path(path, paint, stroke, t, Some(unsafe { &*m }));
                return;
            }
        }
        self.pmx().stroke_path(path, paint, stroke, t, None);
    }
    fn fill_rect(&mut self, r: Rect, c: Color) {
        if c.a == 0 || r.w <= 0.0 || r.h <= 0.0 {
            return;
        }
        let paint = paint_color(c, self.op());
        if self.simple_transform() && !self.clips.last().map(|c| c.any_round).unwrap_or(false) {
            // Fast path: intersect geometry with the clip rect.
            let (mut x0, mut y0, mut x1, mut y1) = self.to_dev(r);
            // Snap to device pixels like Chrome, so adjacent boxes don't leave seams.
            if x1 - x0 >= 1.0 {
                x0 = x0.round();
                x1 = x1.round();
            }
            if y1 - y0 >= 1.0 {
                y0 = y0.round();
                y1 = y1.round();
            }
            let (cx0, cy0, cx1, cy1) = self.clip_rect();
            let (x0, y0, x1, y1) = (x0.max(cx0), y0.max(cy0), x1.min(cx1), y1.min(cy1));
            if x1 <= x0 || y1 <= y0 {
                return;
            }
            if let Some(rr) = tiny_skia::Rect::from_ltrb(x0, y0, x1, y1) {
                self.pmx().fill_rect(rr, &paint, Transform::identity(), None);
            }
            return;
        }
        if let Some(p) = rect_path(r) {
            self.fill_path(&p, &paint, r);
        }
    }
}

impl Rasterizer {
    pub fn new() -> Self {
        Rasterizer { glyphs: GlyphCache::default(), fixed_mode: 0, scaled: std::collections::HashMap::new(), scaled_bytes: 0 }
    }

    /// Render a display list. `scroll` is in document px; `scale` maps CSS px to device px.
    /// `offset_y` is where the viewport's top edge sits in the pixmap (device px);
    /// `clip` is the absolute device-row band (y0, y1) that may be touched.
    pub fn render(&mut self, list: &DisplayList, pm: &mut Pixmap, scroll: (f32, f32), scale: f32, offset_y: f32, clip: (f32, f32)) {
        self.render_at(list, pm, scroll, scale, (0.0, offset_y), clip)
    }

    /// Like `render`, with the viewport's top-left corner at `offset` (device
    /// px) in `pm`: a pixmap covering just part of the viewport (a damaged
    /// region) is drawn with `offset` = minus that region's origin.
    pub fn render_at(&mut self, list: &DisplayList, pm: &mut Pixmap, scroll: (f32, f32), scale: f32, offset: (f32, f32), clip: (f32, f32)) {
        let (offset_x, offset_y) = offset;
        let (w, h) = (pm.width() as f32, pm.height() as f32);
        let (y0, y1) = (clip.0.max(0.0), clip.1.min(h));
        if y1 <= y0 {
            return;
        }
        let view = Rect::new(scroll.0 - offset_x / scale, scroll.1 + (y0 - offset_y) / scale, w / scale, (y1 - y0) / scale);
        let base = Transform::from_row(scale, 0.0, 0.0, scale, -scroll.0 * scale + offset_x, -scroll.1 * scale + offset_y);
        let base_clip = (0.0, y0, w, y1);
        let mut ctx = Ctx {
            pm,
            transforms: vec![base],
            clips: vec![ClipLevel { rect: base_clip, shape: None, any_round: false, mask: None }],
            opacity: vec![1.0],
            layers: Vec::new(),
            view,
            scale,
            scroll,
        };
        let fixed_t = Transform::from_row(scale, 0.0, 0.0, scale, offset_x, offset_y);
        let fixed_view = Rect::new(-offset_x / scale, (y0 - offset_y) / scale, w / scale, (y1 - y0) / scale);
        self.draw_items(&mut ctx, &list.items, true, Some((fixed_t, fixed_view)));
        raster_prof_report();
    }

    fn draw_items(&mut self, ctx: &mut Ctx, items: &[Item], cull: bool, fixed: Option<(Transform, Rect)>) {
        let mut transform_depth = 0;
        let mut fixed_saved: Vec<(Vec<Transform>, Rect, usize, i32, (f32, f32))> = Vec::new();
        let mut i = 0;
        let mut fixed_depth = 0usize;
        while i < items.len() {
            let it = &items[i];
            i += 1;
            // Fixed-layer split: skip the fixed groups, or everything else.
            if self.fixed_mode == 1 && matches!(it, Item::BeginFixed) && fixed.is_some() {
                let mut depth = 1;
                while i < items.len() && depth > 0 {
                    match items[i] {
                        Item::BeginFixed => depth += 1,
                        Item::EndFixed => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                continue;
            }
            match it {
                Item::BeginFixed => fixed_depth += 1,
                Item::EndFixed => fixed_depth = fixed_depth.saturating_sub(1),
                _ => {}
            }
            if self.fixed_mode == 2 && fixed_depth == 0 && !matches!(it, Item::BeginFixed | Item::EndFixed) {
                continue;
            }
            if let Item::PushWarp { h, bounds } = it {
                // The warped subtree is drawn as one unit up to its PopWarp.
                let mut depth = 1;
                let mut j = i;
                while j < items.len() {
                    match items[j] {
                        Item::PushWarp { .. } => depth += 1,
                        Item::PopWarp => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    j += 1;
                }
                let inner = &items[i..j.min(items.len())];
                i = (j + 1).min(items.len());
                self.draw_warp(ctx, h, *bounds, inner);
                continue;
            }
            // Culling (only valid without extra transforms).
            if cull && transform_depth == 0 {
                if let Some(b) = it.bounds() {
                    if !b.intersects(&ctx.view.inflate(2.0)) {
                        continue;
                    }
                }
            }
            let prof_t0 = if raster_prof() { Some(std::time::Instant::now()) } else { None };
            match it {
                Item::Rect { r, color } => ctx.fill_rect(*r, *color),
                Item::Backdrop { r, filters } => {
                    if ctx.simple_transform() {
                        let d = ctx.to_dev(*r);
                        let clip = ctx.clip_rect();
                        let scale = ctx.scale;
                        apply_backdrop(ctx.pmx(), d, clip, filters, scale);
                    }
                }
                Item::RRect { r, radii, color } => {
                    let r = &ctx.snap(*r);
                    if let Some(p) = rrect_path(*r, *radii) {
                        let paint = paint_color(*color, ctx.op());
                        ctx.fill_path(&p, &paint, *r);
                    }
                }
                Item::Border { r, widths, colors, styles, radii } => self.draw_border(ctx, *r, *widths, *colors, *styles, *radii),
                Item::Text { x, y, run, color } => self.draw_text(ctx, *x, *y, run, *color),
                Item::Image { r, img, radii, pixelated } => {
                    if r.w <= 0.0 || r.h <= 0.0 {
                        continue;
                    }
                    let sx = r.w / img.width as f32;
                    let sy = r.h / img.height as f32;
                    let dev_scale = sx * ctx.scale;
                    // Device size the image is drawn at (axis-aligned transforms only).
                    let t = ctx.t();
                    let simple = t.kx == 0.0 && t.ky == 0.0;
                    let (dw, dh) = ((r.w * t.sx.abs()).round().max(1.0) as u32, (r.h * t.sy.abs()).round().max(1.0) as u32);
                    let scaled = if !*pixelated && simple && dev_scale < 0.75 && dw < img.width && dh < img.height && dw * dh <= 4096 * 4096 {
                        self.downscaled(img, dw, dh)
                    } else {
                        None
                    };
                    let quality = if *pixelated {
                        tiny_skia::FilterQuality::Nearest
                    } else if scaled.is_some() {
                        tiny_skia::FilterQuality::Bilinear
                    } else if dev_scale < 0.6 {
                        tiny_skia::FilterQuality::Bicubic
                    } else {
                        tiny_skia::FilterQuality::Bilinear
                    };
                    let (src, ssx, ssy) = match scaled.as_ref() {
                        Some(pm) => (pm.as_ref().as_ref(), r.w / pm.width() as f32, r.h / pm.height() as f32),
                        None => (img.pixmap.as_ref(), sx, sy),
                    };
                    let shader = tiny_skia::Pattern::new(src, tiny_skia::SpreadMode::Pad, quality, ctx.op(), Transform::from_row(ssx, 0.0, 0.0, ssy, r.x, r.y));
                    let mut paint = Paint { shader, anti_alias: true, ..Default::default() };
                    paint.anti_alias = radii.is_some();
                    let path = match radii {
                        Some(rd) => rrect_path(*r, *rd),
                        None => rect_path(*r),
                    };
                    let t_img = raster_prof().then(std::time::Instant::now);
                    if let Some(p) = path {
                        ctx.fill_path(&p, &paint, *r);
                    }
                    if let Some(t0) = t_img {
                        if t0.elapsed().as_millis() >= 3 {
                            eprintln!("[raster] slow image {}x{} -> {:.0}x{:.0} css (dev {}x{}) scaled={} {:?}", img.width, img.height, r.w, r.h, dw, dh, scaled.is_some(), t0.elapsed());
                        }
                    }
                }
                Item::Pattern { area, tile, img, repeat_x, repeat_y, fixed } => {
                    if tile.w <= 0.0 || tile.h <= 0.0 {
                        continue;
                    }
                    let mut area = *area;
                    let mut tile = *tile;
                    if *fixed {
                        tile.x += ctx.scroll.0;
                        tile.y += ctx.scroll.1;
                        if !*repeat_x {
                            area = area.intersect(&Rect::new(tile.x, area.y, tile.w, area.h));
                        }
                        if !*repeat_y {
                            area = area.intersect(&Rect::new(area.x, tile.y, area.w, tile.h));
                        }
                        if area.w <= 0.0 || area.h <= 0.0 {
                            continue;
                        }
                    }
                    let (area, tile) = (&area, &tile);
                    // Fast path: tiles a whole number of device pixels in size
                    // under a scale/translate transform repeat a copy resampled
                    // once to device resolution, pixel-aligned (no per-pixel
                    // bilinear filtering every frame). Not for sprites (an area
                    // much smaller than the tile).
                    let t = ctx.t();
                    let (dw, dh) = (tile.w * t.sx, tile.h * t.sy);
                    let mut done = false;
                    if t.kx == 0.0 && t.ky == 0.0 && dw >= 1.0 && dh >= 1.0 && (dw - dw.round()).abs() < 0.01 && (dh - dh.round()).abs() < 0.01 && dw * dh <= 2048.0 * 2048.0 && area.w * area.h >= tile.w * tile.h * 0.5 {
                        let (iw, ih) = (dw.round() as u32, dh.round() as u32);
                        if let Some(res) = self.resampled(img, iw, ih) {
                            let ox = ((t.sx * tile.x + t.tx).round() - t.tx) / t.sx;
                            let oy = ((t.sy * tile.y + t.ty).round() - t.ty) / t.sy;
                            let shader = tiny_skia::Pattern::new(res.as_ref().as_ref(), tiny_skia::SpreadMode::Repeat, tiny_skia::FilterQuality::Nearest, ctx.op(), Transform::from_row(1.0 / t.sx, 0.0, 0.0, 1.0 / t.sy, ox, oy));
                            let paint = Paint { shader, anti_alias: false, ..Default::default() };
                            if let Some(p) = rect_path(*area) {
                                ctx.fill_path(&p, &paint, *area);
                            }
                            done = true;
                        }
                    }
                    if !done {
                        let sx = tile.w / img.width as f32;
                        let sy = tile.h / img.height as f32;
                        let shader = tiny_skia::Pattern::new(img.pixmap.as_ref(), tiny_skia::SpreadMode::Repeat, tiny_skia::FilterQuality::Bilinear, ctx.op(), Transform::from_row(sx, 0.0, 0.0, sy, tile.x, tile.y));
                        let paint = Paint { shader, anti_alias: false, ..Default::default() };
                        if let Some(p) = rect_path(*area) {
                            ctx.fill_path(&p, &paint, *area);
                        }
                    }
                }
                Item::Gradient { r, grad, radii } => self.draw_gradient(ctx, *r, grad, *radii),
                Item::GradientTiles { area, tile, grad } => self.draw_gradient_tiles(ctx, *area, *tile, grad),
                Item::MaskedColor { area, tile, img, color } => self.draw_masked_color(ctx, *area, *tile, img, *color),
                Item::Shadow { r, radii, blur, spread, color, inset, hole } => {
                    if *inset || !self.draw_outer_shadow(ctx, *r, *radii, *blur, *color, *hole) {
                        self.draw_shadow(ctx, *r, *radii, *blur, *spread, *color, *inset)
                    }
                }
                Item::InsetShadow { r, radii, hole, hole_radii, blur, color } => self.draw_inset_shadow(ctx, *r, *radii, *hole, *hole_radii, *blur, *color),
                Item::Line { x0, y0, x1, y1, width, color, style } => {
                    let mut pb = PathBuilder::new();
                    pb.move_to(*x0, *y0);
                    pb.line_to(*x1, *y1);
                    if let Some(p) = pb.finish() {
                        let mut stroke = Stroke { width: *width, ..Default::default() };
                        if matches!(style, BorderStyle::Dashed | BorderStyle::Dotted) {
                            let len = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
                            let (dash, round) = blink_dash(len, *width, *style == BorderStyle::Dotted, false);
                            stroke.dash = dash;
                            if round {
                                stroke.line_cap = tiny_skia::LineCap::Round;
                            }
                        }
                        let paint = paint_color(*color, ctx.op());
                        let b = Rect::new(x0.min(*x1) - width, y0.min(*y1) - width, (x1 - x0).abs() + 2.0 * width, (y1 - y0).abs() + 2.0 * width);
                        ctx.stroke_path(&p, &paint, &stroke, b);
                        if *style == BorderStyle::Double {
                            let mut pb = PathBuilder::new();
                            pb.move_to(*x0, *y0 + width * 2.0);
                            pb.line_to(*x1, *y1 + width * 2.0);
                            if let Some(p2) = pb.finish() {
                                ctx.stroke_path(&p2, &paint, &stroke, b.translate(0.0, width * 2.0));
                            }
                        }
                    }
                }
                Item::PushClip { r, radii } => {
                    let (x0, y0, x1, y1) = ctx.to_dev(*r);
                    let (px0, py0, px1, py1) = ctx.clip_rect();
                    let rect = (x0.max(px0), y0.max(py0), x1.min(px1).max(x0.max(px0)), y1.min(py1).max(y0.max(py0)));
                    let parent_round = ctx.clips.last().map(|c| c.any_round).unwrap_or(false);
                    let shape = match radii {
                        Some(rd) if rd.any() => rrect_path(*r, *rd).and_then(|p| p.transform(ctx.t())),
                        _ => None,
                    };
                    let any_round = parent_round || shape.is_some();
                    ctx.clips.push(ClipLevel { rect, shape, any_round, mask: None });
                }
                Item::PushClipPath(p) => {
                    let (px0, py0, px1, py1) = ctx.clip_rect();
                    let dev = (**p).clone().transform(ctx.t());
                    let (rect, shape) = match dev {
                        Some(dp) => {
                            let b = dp.bounds();
                            let (x0, y0) = (b.left().max(px0), b.top().max(py0));
                            ((x0, y0, b.right().min(px1).max(x0), b.bottom().min(py1).max(y0)), Some(dp))
                        }
                        None => ((px0, py0, px0, py0), None),
                    };
                    ctx.clips.push(ClipLevel { rect, shape, any_round: true, mask: None });
                }
                Item::PushMask(m) => {
                    let (w, h) = (ctx.pm.width(), ctx.pm.height());
                    let live = ctx.layers.iter().filter(|l| l.pm.is_some()).count();
                    let mut clip = ctx.clip_rect();
                    let end = matching_pop(items, i);
                    if let Some(b) = content_dev_bounds(&items[i..end], ctx.t(), ctx.scroll, ctx.scale) {
                        clip = intersect4(clip, b);
                    }
                    let empty = clip.2 <= clip.0 || clip.3 <= clip.1;
                    let pm = if live < 6 && !empty { Pixmap::new(w, h) } else { None };
                    ctx.layers.push(Layer { pm, filters: std::sync::Arc::new(Vec::new()), clip, mask: Some(m.clone()), blend: None, opacity: 1.0, dev_bounds: None });
                }
                Item::PopMask => {
                    if let Some(layer) = ctx.layers.pop() {
                        if let (Some(mut pm), Some(items)) = (layer.pm, layer.mask) {
                            let (w, h) = (pm.width(), pm.height());
                            if let Some(mut mpm) = Pixmap::new(w, h) {
                                {
                                    let mut mctx = Ctx {
                                        pm: &mut mpm,
                                        layers: Vec::new(),
                                        transforms: vec![ctx.t()],
                                        clips: vec![ClipLevel { rect: (0.0, 0.0, w as f32, h as f32), shape: None, any_round: false, mask: None }],
                                        opacity: vec![1.0],
                                        view: ctx.view,
                                        scale: ctx.scale,
                                        scroll: ctx.scroll,
                                    };
                                    self.draw_items(&mut mctx, &items, false, None);
                                }
                                // Keep the layer where the mask image is opaque
                                // (only inside the content's area).
                                let (cx0, cy0) = ((layer.clip.0.floor().max(0.0) as usize).min(w as usize), (layer.clip.1.floor().max(0.0) as usize).min(h as usize));
                                let (cx1, cy1) = ((layer.clip.2.ceil().max(0.0) as usize).min(w as usize), (layer.clip.3.ceil().max(0.0) as usize).min(h as usize));
                                let mp = mpm.pixels();
                                let dp = pm.pixels_mut();
                                for y in cy0..cy1 {
                                    for x in cx0..cx1 {
                                        let k = y * w as usize + x;
                                        let a = mp[k].alpha() as u32;
                                        if a == 255 {
                                            continue;
                                        }
                                        let d = dp[k];
                                        let f = |c: u8| ((c as u32 * a + 127) / 255) as u8;
                                        dp[k] = tiny_skia::PremultipliedColorU8::from_rgba(f(d.red()), f(d.green()), f(d.blue()), f(d.alpha())).unwrap_or(d);
                                    }
                                }
                                if cx1 > cx0 && cy1 > cy0 {
                                    if let Some(sub) = pm.clone_rect(tiny_skia::IntRect::from_xywh(cx0 as i32, cy0 as i32, (cx1 - cx0) as u32, (cy1 - cy0) as u32).unwrap()) {
                                        ctx.pmx().draw_pixmap(cx0 as i32, cy0 as i32, sub.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
                                    }
                                }
                            } else {
                                ctx.pmx().draw_pixmap(0, 0, pm.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
                            }
                        }
                    }
                }
                Item::PopClip => {
                    if ctx.clips.len() > 1 {
                        ctx.clips.pop();
                    }
                }
                Item::PushBlend(m) => {
                    let (w, h) = (ctx.pm.width(), ctx.pm.height());
                    let live = ctx.layers.iter().filter(|l| l.pm.is_some()).count();
                    // An isolated group with normal blending draws exactly like
                    // its contents drawn in place unless something inside blends
                    // with a non-normal mode: then no offscreen layer is needed.
                    let needs_layer = *m != 0 || {
                        let mut depth = 1;
                        let mut j = i;
                        let mut nested = false;
                        while j < items.len() && depth > 0 {
                            match &items[j] {
                                Item::PushBlend(k) => {
                                    depth += 1;
                                    if *k != 0 {
                                        nested = true;
                                        break;
                                    }
                                }
                                Item::PopBlend => depth -= 1,
                                _ => {}
                            }
                            j += 1;
                        }
                        nested
                    };
                    let pm = if live < 8 && needs_layer { Pixmap::new(w, h) } else { None };
                    let clip = ctx.clip_rect();
                    ctx.layers.push(Layer { pm, filters: std::sync::Arc::new(Vec::new()), clip, mask: None, blend: Some(blend_mode_of(*m)), opacity: 1.0, dev_bounds: None });
                }
                Item::PopBlend | Item::PopGroup => {
                    if let Some(layer) = ctx.layers.pop() {
                        if let Some(pm) = layer.pm {
                            match layer.dev_bounds {
                                // Composite only where the group drew.
                                Some(r) => {
                                    if let Some(rr) = tiny_skia::Rect::from_ltrb(r.0, r.1, r.2, r.3) {
                                        let pat = tiny_skia::Pattern::new(pm.as_ref(), tiny_skia::SpreadMode::Pad, tiny_skia::FilterQuality::Nearest, layer.opacity, Transform::identity());
                                        let mut paint = tiny_skia::Paint::default();
                                        paint.shader = pat;
                                        paint.blend_mode = layer.blend.unwrap_or(tiny_skia::BlendMode::SourceOver);
                                        ctx.pmx().fill_rect(rr, &paint, Transform::identity(), None);
                                    }
                                }
                                None => {
                                    let paint = tiny_skia::PixmapPaint { blend_mode: layer.blend.unwrap_or(tiny_skia::BlendMode::SourceOver), opacity: layer.opacity, ..Default::default() };
                                    ctx.pmx().draw_pixmap(0, 0, pm.as_ref(), &paint, Transform::identity(), None);
                                }
                            }
                        } else if matches!(it, Item::PopGroup) && ctx.opacity.len() > 1 {
                            // No layer (too deep): the group opacity was applied per item.
                            ctx.opacity.pop();
                        }
                    }
                }
                Item::PushGroup { opacity: o, bounds } => {
                    let (w, h) = (ctx.pm.width(), ctx.pm.height());
                    let clip = ctx.clip_rect();
                    // Device-space area the group covers (whole pixels, within the clip).
                    let dev_bounds = bounds.and_then(|b| {
                        let t = ctx.t();
                        let pts = [(b.x, b.y), (b.right(), b.y), (b.x, b.bottom()), (b.right(), b.bottom())];
                        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                        for (px, py) in pts {
                            let (dx, dy) = (t.sx * px + t.kx * py + t.tx, t.ky * px + t.sy * py + t.ty);
                            x0 = x0.min(dx);
                            y0 = y0.min(dy);
                            x1 = x1.max(dx);
                            y1 = y1.max(dy);
                        }
                        let (x0, y0) = ((x0 - 2.0).floor().max(clip.0).max(0.0), (y0 - 2.0).floor().max(clip.1).max(0.0));
                        let (x1, y1) = ((x1 + 2.0).ceil().min(clip.2).min(w as f32), (y1 + 2.0).ceil().min(clip.3).min(h as f32));
                        Some((x0, y0, x1.max(x0), y1.max(y0)))
                    });
                    let empty = dev_bounds.map(|r| r.2 <= r.0 || r.3 <= r.1).unwrap_or(false);
                    let live = ctx.layers.iter().filter(|l| l.pm.is_some()).count();
                    let pm = if live < 8 && !empty { Pixmap::new(w, h) } else { None };
                    if pm.is_none() {
                        let cur = ctx.op();
                        ctx.opacity.push(cur * o);
                    }
                    ctx.layers.push(Layer { pm, filters: std::sync::Arc::new(Vec::new()), clip, mask: None, blend: Some(tiny_skia::BlendMode::SourceOver), opacity: *o, dev_bounds });
                }
                Item::PushOpacity(o) => {
                    let cur = ctx.op();
                    ctx.opacity.push(cur * o);
                }
                Item::PopOpacity => {
                    if ctx.opacity.len() > 1 {
                        ctx.opacity.pop();
                    }
                }
                Item::PushTransform(m) => {
                    let t = ctx.t().pre_concat(Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5]));
                    ctx.transforms.push(t);
                    transform_depth += 1;
                }
                Item::PushWarp { .. } | Item::PopWarp => {}
                Item::PopTransform => {
                    if ctx.transforms.len() > 1 {
                        ctx.transforms.pop();
                        transform_depth -= 1;
                    }
                }
                Item::PushFilter(f) => {
                    let (w, h) = (ctx.pm.width(), ctx.pm.height());
                    let live = ctx.layers.iter().filter(|l| l.pm.is_some()).count();
                    // Only the content's area (grown by the filter's spread)
                    // is scanned, filtered and composited.
                    let mut clip = ctx.clip_rect();
                    let end = matching_pop(items, i);
                    if let Some(b) = content_dev_bounds(&items[i..end], ctx.t(), ctx.scroll, ctx.scale) {
                        let (l, t, r, bt) = filter_spread(f);
                        let sc = ctx.scale;
                        clip = intersect4(clip, (b.0 - l * sc - 2.0, b.1 - t * sc - 2.0, b.2 + r * sc + 2.0, b.3 + bt * sc + 2.0));
                    }
                    let empty = clip.2 <= clip.0 || clip.3 <= clip.1;
                    let pm = if live < 6 && !empty { Pixmap::new(w, h) } else { None };
                    ctx.layers.push(Layer { pm, filters: f.clone(), clip, mask: None, blend: None, opacity: 1.0, dev_bounds: None });
                }
                Item::PopFilter => {
                    if let Some(layer) = ctx.layers.pop() {
                        if let Some(pm) = layer.pm {
                            let scale = ctx.scale;
                            composite_filtered(pm, &layer.filters, layer.clip, scale, ctx.pmx());
                        }
                    }
                }
                Item::PushSticky(st) => {
                    let (dx, dy) = st.delta(ctx.scroll);
                    let t = ctx.t().pre_concat(Transform::from_translate(dx, dy));
                    ctx.transforms.push(t);
                    transform_depth += 1;
                }
                Item::PopSticky => {
                    if ctx.transforms.len() > 1 {
                        ctx.transforms.pop();
                        transform_depth -= 1;
                    }
                }
                Item::BeginFixed => {
                    if let Some((ft, fv)) = fixed {
                        let saved = (std::mem::replace(&mut ctx.transforms, vec![ft]), ctx.view, ctx.clips.len(), transform_depth, ctx.scroll);
                        fixed_saved.push(saved);
                        ctx.view = fv;
                        ctx.scroll = (0.0, 0.0);
                        transform_depth = 0;
                        // Fixed content escapes ancestors' overflow clips.
                        let base = ctx.clips[0].rect;
                        ctx.clips.push(ClipLevel { rect: base, shape: None, any_round: false, mask: None });
                    }
                }
                Item::EndFixed => {
                    if let Some((t, v, n, d, sc)) = fixed_saved.pop() {
                        ctx.scroll = sc;
                        ctx.transforms = t;
                        ctx.view = v;
                        ctx.clips.truncate(n.max(1));
                        transform_depth = d;
                    }
                }
            }
            if let Some(t0) = prof_t0 {
                raster_prof_add(item_kind(it), t0.elapsed());
            }
        }
    }

    /// An area-averaged copy of `img` at `w`×`h` device pixels (cached): a
    /// large image drawn small is filtered once instead of every frame.
    /// `img` resampled to exactly `w`×`h` pixels (cached): box-filtered when
    /// shrinking, bilinear when growing, the image itself at its own size.
    fn resampled(&mut self, img: &Arc<ImageData>, w: u32, h: u32) -> Option<Arc<Pixmap>> {
        if w <= img.pixmap.width() && h <= img.pixmap.height() && (w, h) != (img.pixmap.width(), img.pixmap.height()) {
            return self.downscaled(img, w, h);
        }
        let key = (Arc::as_ptr(img) as usize, w, h);
        if let Some((_, pm)) = self.scaled.get(&key) {
            return Some(pm.clone());
        }
        if self.scaled.len() > 256 || self.scaled_bytes > SCALED_CACHE_BYTES {
            self.scaled.clear();
            self.scaled_bytes = 0;
        }
        let mut out = Pixmap::new(w, h)?;
        // Repeat-edge bilinear so the tile's seams match a repeating shader.
        let same = (w, h) == (img.pixmap.width(), img.pixmap.height());
        let q = if same { tiny_skia::FilterQuality::Nearest } else { tiny_skia::FilterQuality::Bilinear };
        let shader = tiny_skia::Pattern::new(img.pixmap.as_ref(), tiny_skia::SpreadMode::Repeat, q, 1.0, Transform::from_scale(w as f32 / img.pixmap.width() as f32, h as f32 / img.pixmap.height() as f32));
        let paint = Paint { shader, anti_alias: false, blend_mode: tiny_skia::BlendMode::Source, ..Default::default() };
        out.fill_rect(tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, h as f32)?, &paint, Transform::identity(), None);
        let pm = Arc::new(out);
        self.scaled_bytes += img.pixmap.data().len() + pm.data().len();
        self.scaled.insert(key, (img.clone(), pm.clone()));
        Some(pm)
    }

    fn downscaled(&mut self, img: &Arc<ImageData>, w: u32, h: u32) -> Option<Arc<Pixmap>> {
        let key = (Arc::as_ptr(img) as usize, w, h);
        if let Some((_, pm)) = self.scaled.get(&key) {
            return Some(pm.clone());
        }
        if self.scaled.len() > 256 || self.scaled_bytes > SCALED_CACHE_BYTES {
            self.scaled.clear();
            self.scaled_bytes = 0;
        }
        let (sw, sh) = (img.pixmap.width() as usize, img.pixmap.height() as usize);
        let src = img.pixmap.data();
        let mut out = Pixmap::new(w, h)?;
        {
            let dst = out.data_mut();
            let (fx, fy) = (sw as f32 / w as f32, sh as f32 / h as f32);
            for y in 0..h as usize {
                let y0 = (y as f32 * fy) as usize;
                let y1 = (((y + 1) as f32 * fy).ceil() as usize).clamp(y0 + 1, sh);
                for x in 0..w as usize {
                    let x0 = (x as f32 * fx) as usize;
                    let x1 = (((x + 1) as f32 * fx).ceil() as usize).clamp(x0 + 1, sw);
                    let mut acc = [0u32; 4];
                    for yy in y0..y1 {
                        let row = yy * sw * 4;
                        for xx in x0..x1 {
                            let o = row + xx * 4;
                            acc[0] += src[o] as u32;
                            acc[1] += src[o + 1] as u32;
                            acc[2] += src[o + 2] as u32;
                            acc[3] += src[o + 3] as u32;
                        }
                    }
                    let n = ((y1 - y0) * (x1 - x0)) as u32;
                    let o = (y * w as usize + x) * 4;
                    for c in 0..4 {
                        dst[o + c] = ((acc[c] + n / 2) / n) as u8;
                    }
                }
            }
        }
        let pm = Arc::new(out);
        self.scaled_bytes += img.pixmap.data().len() + pm.data().len();
        self.scaled.insert(key, (img.clone(), pm.clone()));
        Some(pm)
    }

    /// Draw `inner` into an offscreen layer covering `b` (local coordinates)
    /// and composite it through the projective map `h` (local → current
    /// coordinates) by inverse-mapped bilinear sampling.
    fn draw_warp(&mut self, ctx: &mut Ctx, h: &[f64; 9], b: Rect, inner: &[Item]) {
        use crate::css::mat4::{inv3, mul3};
        if b.w <= 0.0 || b.h <= 0.0 {
            return;
        }
        let t = ctx.t();
        let tm = [t.sx as f64, t.kx as f64, t.tx as f64, t.ky as f64, t.sy as f64, t.ty as f64, 0.0, 0.0, 1.0];
        let g = mul3(&tm, h);
        let ap = |m: &[f64; 9], x: f64, y: f64| [m[0] * x + m[1] * y + m[2], m[3] * x + m[4] * y + m[5], m[6] * x + m[7] * y + m[8]];
        // Device bounding box of the plane, clipped to the half-space in front of the viewer.
        let (bx0, by0, bx1, by1) = (b.x as f64, b.y as f64, b.right() as f64, b.bottom() as f64);
        let hom = [ap(&g, bx0, by0), ap(&g, bx1, by0), ap(&g, bx1, by1), ap(&g, bx0, by1)];
        const EPS: f64 = 1e-6;
        let mut poly: Vec<[f64; 3]> = Vec::new();
        for k in 0..4 {
            let (a, c) = (hom[k], hom[(k + 1) % 4]);
            if a[2] >= EPS {
                poly.push(a);
            }
            if (a[2] >= EPS) != (c[2] >= EPS) {
                let s = (EPS - a[2]) / (c[2] - a[2]);
                poly.push([a[0] + (c[0] - a[0]) * s, a[1] + (c[1] - a[1]) * s, EPS]);
            }
        }
        if poly.is_empty() {
            return;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in &poly {
            let (px, py) = (p[0] / p[2], p[1] / p[2]);
            x0 = x0.min(px);
            y0 = y0.min(py);
            x1 = x1.max(px);
            y1 = y1.max(py);
        }
        let (cx0, cy0, cx1, cy1) = ctx.clip_rect();
        let (pw, ph) = (ctx.pm.width() as f64, ctx.pm.height() as f64);
        let dx0 = x0.max(cx0 as f64).max(0.0).floor();
        let dy0 = y0.max(cy0 as f64).max(0.0).floor();
        let dx1 = x1.min(cx1 as f64).min(pw).ceil();
        let dy1 = y1.min(cy1 as f64).min(ph).ceil();
        if dx1 <= dx0 || dy1 <= dy0 || !(dx1 - dx0).is_finite() || !(dy1 - dy0).is_finite() {
            return;
        }
        let Some(gi) = inv3(&g) else { return };
        // Only the part of the plane that lands in the visible device box needs rendering.
        let mut lb = (bx0, by0, bx1, by1);
        if poly.len() == 4 && hom.iter().all(|p| p[2] >= EPS) {
            let pts = [ap(&gi, dx0, dy0), ap(&gi, dx1, dy0), ap(&gi, dx1, dy1), ap(&gi, dx0, dy1)];
            if pts.iter().all(|p| p[2] > 1e-12) {
                let (mut u0, mut v0, mut u1, mut v1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                for p in &pts {
                    u0 = u0.min(p[0] / p[2]);
                    v0 = v0.min(p[1] / p[2]);
                    u1 = u1.max(p[0] / p[2]);
                    v1 = v1.max(p[1] / p[2]);
                }
                lb = (lb.0.max(u0 - 2.0), lb.1.max(v0 - 2.0), lb.2.min(u1 + 2.0), lb.3.min(v1 + 2.0));
            }
        }
        if lb.2 <= lb.0 || lb.3 <= lb.1 {
            return;
        }
        // Layer resolution: device pixels per local unit where the plane is magnified most.
        let k0 = ((t.sx * t.sy - t.kx * t.ky).abs() as f64).sqrt().max(0.01);
        let mut k = k0;
        for (x, y) in [(lb.0, lb.1), (lb.2, lb.1), (lb.2, lb.3), (lb.0, lb.3)] {
            let p = ap(&g, x, y);
            if p[2] <= EPS {
                continue;
            }
            let w2 = p[2] * p[2];
            let j00 = (g[0] * p[2] - p[0] * g[6]) / w2;
            let j01 = (g[1] * p[2] - p[0] * g[7]) / w2;
            let j10 = (g[3] * p[2] - p[1] * g[6]) / w2;
            let j11 = (g[4] * p[2] - p[1] * g[7]) / w2;
            let s = (j00 * j11 - j01 * j10).abs().sqrt();
            if s.is_finite() {
                k = k.max(s.min(k0 * 4.0));
            }
        }
        let (lw, lh) = (lb.2 - lb.0, lb.3 - lb.1);
        let max_px = 16.0e6;
        if lw * k * lh * k > max_px {
            k = (max_px / (lw * lh)).sqrt();
        }
        k = k.min(8192.0 / lw).min(8192.0 / lh);
        let (w, hgt) = ((lw * k).ceil().max(1.0) as u32, (lh * k).ceil().max(1.0) as u32);
        let Some(mut lpm) = Pixmap::new(w, hgt) else { return };
        {
            let lt = Transform::from_row(k as f32, 0.0, 0.0, k as f32, (-lb.0 * k) as f32, (-lb.1 * k) as f32);
            let mut sub = Ctx {
                pm: &mut lpm,
                layers: Vec::new(),
                transforms: vec![lt],
                clips: vec![ClipLevel { rect: (0.0, 0.0, w as f32, hgt as f32), shape: None, any_round: false, mask: None }],
                opacity: vec![1.0],
                view: Rect::new(lb.0 as f32, lb.1 as f32, lw as f32, lh as f32),
                scale: k as f32,
                scroll: ctx.scroll,
            };
            self.draw_items(&mut sub, inner, false, None);
        }
        // Layer pixel → device.
        let linv = [1.0 / k, 0.0, lb.0, 0.0, 1.0 / k, lb.1, 0.0, 0.0, 1.0];
        let Some(gi) = inv3(&mul3(&g, &linv)) else { return };
        let op = ctx.op();
        let round = ctx.clips.last().map(|c| c.any_round).unwrap_or(false);
        let mask: Option<*const Mask> = if round { ctx.mask().map(|m| m as *const Mask) } else { None };
        let src = lpm.pixels();
        let (sw, sh) = (w as i64, hgt as i64);
        let texel = |x: i64, y: i64| -> [f32; 4] {
            if x < 0 || y < 0 || x >= sw || y >= sh {
                return [0.0; 4];
            }
            let p = src[(y * sw + x) as usize];
            [p.red() as f32, p.green() as f32, p.blue() as f32, p.alpha() as f32]
        };
        let dst = ctx.pmx();
        let dw = dst.width() as usize;
        let dpx = dst.pixels_mut();
        for y in dy0 as usize..dy1 as usize {
            let fy = y as f64 + 0.5;
            for x in dx0 as usize..dx1 as usize {
                let fx = x as f64 + 0.5;
                let sz = gi[6] * fx + gi[7] * fy + gi[8];
                if sz <= 1e-12 {
                    continue;
                }
                let u = (gi[0] * fx + gi[1] * fy + gi[2]) / sz - 0.5;
                let v = (gi[3] * fx + gi[4] * fy + gi[5]) / sz - 0.5;
                if u < -1.0 || v < -1.0 || u > sw as f64 || v > sh as f64 {
                    continue;
                }
                let (ux, vy) = (u.floor(), v.floor());
                let (fu, fv) = ((u - ux) as f32, (v - vy) as f32);
                let (ix, iy) = (ux as i64, vy as i64);
                let (c00, c10, c01, c11) = (texel(ix, iy), texel(ix + 1, iy), texel(ix, iy + 1), texel(ix + 1, iy + 1));
                let mut c = [0.0f32; 4];
                for ch in 0..4 {
                    let top = c00[ch] + (c10[ch] - c00[ch]) * fu;
                    let bot = c01[ch] + (c11[ch] - c01[ch]) * fu;
                    c[ch] = top + (bot - top) * fv;
                }
                let mut a = op;
                if let Some(m) = mask {
                    // SAFETY: the mask lives in ctx.clips, which isn't modified here.
                    a *= unsafe { (*m).data()[y * dw + x] } as f32 / 255.0;
                }
                if c[3] * a <= 0.0 {
                    continue;
                }
                let d = &mut dpx[y * dw + x];
                let inv = 1.0 - c[3] * a / 255.0;
                let mix = |s: f32, d: u8| (s * a + d as f32 * inv).round().clamp(0.0, 255.0) as u8;
                let na = mix(c[3], d.alpha());
                let (nr, ng, nb) = (mix(c[0], d.red()).min(na), mix(c[1], d.green()).min(na), mix(c[2], d.blue()).min(na));
                if let Some(px) = tiny_skia::PremultipliedColorU8::from_rgba(nr, ng, nb, na) {
                    *d = px;
                }
            }
        }
    }

    fn draw_text(&mut self, ctx: &mut Ctx, x: f32, y: f32, run: &GlyphRun, color: Color) {
        let op = ctx.op();
        if color.a == 0 || op <= 0.0 {
            return;
        }
        let t = ctx.t();
        if !ctx.simple_transform() || (t.sx - t.sy).abs() > 0.001 {
            // Slow path: draw outlines as paths under the full transform.
            let face = run.font.face();
            let scale = run.size / run.font.upem;
            let paint = paint_color(color, op);
            for &(gid, gx, gy) in &run.glyphs {
                let mut b = OutlineToPath { pb: PathBuilder::new(), sx: scale, sy: scale, dx: 0.0, skew: if run.font.synth_italic { 0.2 } else { 0.0 } };
                if face.outline_glyph(ttf_parser::GlyphId(gid), &mut b).is_some() {
                    if let Some(p) = b.pb.finish().and_then(|p| p.transform(Transform::from_translate(x + gx, y + gy))) {
                        let bb = Rect::new(x + gx - run.size, y + gy - run.size * 1.5, run.size * 3.0, run.size * 3.0);
                        let need = match ctx.classify(bb) {
                            None => continue,
                            Some(m) => m,
                        };
                        let mask = if need { ctx.mask().map(|m| m as *const Mask) } else { None };
                        // SAFETY: mask borrowed from ctx.clips, not mutated during the call.
                        ctx.pmx().fill_path(&p, &paint, FillRule::Winding, t, mask.map(|m| unsafe { &*m }));
                    }
                }
            }
            return;
        }
        let scale = t.sx;
        let dev_size = run.size * scale;
        let (cx0, cy0, cx1, cy1) = ctx.clip_rect();
        let round = ctx.clips.last().map(|c| c.any_round).unwrap_or(false);
        let mask_ptr: Option<*const Mask> = if round { ctx.mask().map(|m| m as *const Mask) } else { None };
        let pw = ctx.pm.width() as i32;
        let ph = ctx.pm.height() as i32;
        let (cr, cg, cb) = (color.r as u32, color.g as u32, color.b as u32);
        let ca = (color.a as f32 * op) as u32;
        for &(gid, gx, gy) in &run.glyphs {
            let dx = (x + gx) * scale + t.tx;
            let dy = (y + gy) * scale + t.ty;
            let ix = dx.floor();
            // Quarter-pixel glyph positions; a fraction that rounds up to 4/4
            // moves to the next whole pixel.
            let subpx = ((dx - ix) * 4.0).round() as u8;
            let (ix, sub) = if subpx == 4 { (ix + 1.0, 0) } else { (ix, subpx) };
            let g = match self.glyphs.get(&run.font, gid, dev_size, sub) {
                Some(g) => g,
                None => continue,
            };
            let gx0 = ix as i32 + g.left;
            let gy0 = dy.round() as i32 + g.top;
            let gx1 = gx0 + g.w as i32;
            let gy1 = gy0 + g.h as i32;
            let x0 = gx0.max(cx0.floor() as i32).max(0);
            let y0 = gy0.max(cy0.floor() as i32).max(0);
            let x1 = gx1.min(cx1.ceil() as i32).min(pw);
            let y1 = gy1.min(cy1.ceil() as i32).min(ph);
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            // SAFETY: mask pointer refers to ctx.clips which isn't modified here.
            let mask: Option<&Mask> = mask_ptr.map(|m| unsafe { &*m });
            let data = ctx.pmx().data_mut();
            for py in y0..y1 {
                let row = ((py - gy0) as u32 * g.w) as usize;
                for px in x0..x1 {
                    let gi = row + (px - gx0) as usize;
                    let mut mval = 255u32;
                    if let Some(m) = mask {
                        mval = m.data()[(py * pw + px) as usize] as u32;
                        if mval == 0 {
                            continue;
                        }
                    }
                    let di = ((py * pw + px) * 4) as usize;
                    if g.color {
                        let si = gi * 4;
                        let sa = g.data[si + 3] as u32 * mval / 255;
                        if sa == 0 {
                            continue;
                        }
                        let k = (op * 255.0) as u32 * mval / 255;
                        let sr = g.data[si] as u32 * k / 255;
                        let sg = g.data[si + 1] as u32 * k / 255;
                        let sb = g.data[si + 2] as u32 * k / 255;
                        let sa = sa * (op * 255.0) as u32 / 255;
                        let inv = 255 - sa;
                        data[di] = (sr + data[di] as u32 * inv / 255).min(255) as u8;
                        data[di + 1] = (sg + data[di + 1] as u32 * inv / 255).min(255) as u8;
                        data[di + 2] = (sb + data[di + 2] as u32 * inv / 255).min(255) as u8;
                        data[di + 3] = (sa + data[di + 3] as u32 * inv / 255).min(255) as u8;
                    } else {
                        let cov = g.data[gi] as u32;
                        if cov == 0 {
                            continue;
                        }
                        let a = cov * ca / 255 * mval / 255;
                        if a == 0 {
                            continue;
                        }
                        let inv = 255 - a;
                        data[di] = ((cr * a + data[di] as u32 * inv) / 255) as u8;
                        data[di + 1] = ((cg * a + data[di + 1] as u32 * inv) / 255) as u8;
                        data[di + 2] = ((cb * a + data[di + 2] as u32 * inv) / 255) as u8;
                        data[di + 3] = (a + data[di + 3] as u32 * inv / 255).min(255) as u8;
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_border(&mut self, ctx: &mut Ctx, r: Rect, w: [f32; 4], colors: [Color; 4], styles: [BorderStyle; 4], radii: Radii) {
        let op = ctx.op();
        let r = ctx.snap(r);
        let w = w.map(|v| ctx.snap_w(v));
        let visible: Vec<usize> = (0..4).filter(|&i| w[i] > 0.0 && styles[i].visible() && colors[i].a > 0).collect();
        if visible.is_empty() {
            return;
        }
        let uniform = visible.len() == 4 && (1..4).all(|i| colors[i] == colors[0] && styles[i] == styles[0]);
        let has_r = radii.any();
        let simple_style = |s: BorderStyle| matches!(s, BorderStyle::Solid | BorderStyle::Inset | BorderStyle::Outset | BorderStyle::Groove | BorderStyle::Ridge | BorderStyle::Double);
        if has_r && (uniform || visible.iter().all(|&i| simple_style(styles[i]))) && visible.iter().all(|&i| simple_style(styles[i])) {
            // Ring between outer and inner rounded rects.
            let inner = Rect::new(r.x + w[3], r.y + w[0], (r.w - w[1] - w[3]).max(0.0), (r.h - w[0] - w[2]).max(0.0));
            let ir = radii.inset(w);
            let mut pb = PathBuilder::new();
            if let Some(o) = rrect_path(r, radii) {
                pb.push_path(&o);
            }
            if inner.w > 0.0 && inner.h > 0.0 {
                if let Some(i) = rrect_path(inner, ir) {
                    pb.push_path(&i);
                }
            }
            let same = visible.len() == 4 && (1..4).all(|i| colors[i] == colors[0]) && !visible.iter().any(|&i| matches!(styles[i], BorderStyle::Inset | BorderStyle::Outset));
            if same {
                if let Some(p) = pb.finish() {
                    let c = colors[visible[0]];
                    ctx.fill_path(&p, &paint_color(c, op), r);
                }
            } else {
                // Sides of different colors (or missing sides, e.g. a spinner's
                // transparent bottom/left): each side's piece of the ring, split at
                // the corners along the outer-to-inner corner diagonal.
                let side_color = |i: usize| {
                    let top_left = i == 0 || i == 3;
                    match styles[i] {
                        BorderStyle::Inset if top_left => blink_dark(colors[i]),
                        BorderStyle::Outset if !top_left => blink_dark(colors[i]),
                        _ => colors[i],
                    }
                };
                let shown = |i: usize| visible.contains(&i);
                // Runs of adjacent sides with one color are drawn as one piece
                // (no antialiasing seams between them).
                let start = (0..4).find(|&i| !shown(i) || !shown((i + 3) % 4) || side_color(i) != side_color((i + 3) % 4)).unwrap_or(0);
                let mut k = 0;
                while k < 4 {
                    let i = (start + k) % 4;
                    if !shown(i) {
                        k += 1;
                        continue;
                    }
                    let c = side_color(i);
                    let mut n = 1;
                    while k + n < 4 && shown((i + n) % 4) && side_color((i + n) % 4) == c {
                        n += 1;
                    }
                    if let Some(p) = rounded_side_path(r, radii, w, i, n) {
                        ctx.fill_path(&p, &paint_color(c, op), r);
                    }
                    k += n;
                }
            }
            return;
        }
        let dashy = |s: BorderStyle| matches!(s, BorderStyle::Dashed | BorderStyle::Dotted);
        if has_r && uniform && dashy(styles[0]) && (1..4).all(|i| w[i] == w[0]) {
            // Like Blink: stroke the centerline of the whole rounded border with one
            // dash pattern that fits the closed path evenly.
            let hw = w[0] / 2.0;
            let cr = Rect::new(r.x + hw, r.y + hw, (r.w - w[0]).max(0.0), (r.h - w[0]).max(0.0));
            let crad = radii.inset([hw; 4]);
            if let Some(p) = rrect_path(cr, crad) {
                let (dash, round) = blink_dash(rrect_length(cr, crad), w[0], styles[0] == BorderStyle::Dotted, true);
                let mut stroke = Stroke { width: w[0], dash, ..Default::default() };
                if round {
                    stroke.line_cap = tiny_skia::LineCap::Round;
                }
                ctx.stroke_path(&p, &paint_color(colors[0], op), &stroke, r);
            }
            return;
        }
        let (x0, y0, x1, y1) = (r.x, r.y, r.right(), r.bottom());
        let (wt, wr, wb, wl) = (w[0], w[1], w[2], w[3]);
        for &i in &visible {
            let style = styles[i];
            let base = colors[i];
            // Blink: inset darkens the top/left sides, outset the bottom/right;
            // groove is an inset outer half and an outset inner half (ridge the reverse).
            let top_left = i == 0 || i == 3;
            let shade = |inset: bool| if inset == top_left { blink_dark(base) } else { base };
            let c = match style {
                BorderStyle::Inset => shade(true),
                BorderStyle::Outset => shade(false),
                _ => base,
            };
            let paint = paint_color(c, op);
            match style {
                BorderStyle::Dashed | BorderStyle::Dotted => {
                    let width = w[i];
                    let (ax, ay, bx, by) = match i {
                        0 => (x0, y0 + width / 2.0, x1, y0 + width / 2.0),
                        1 => (x1 - width / 2.0, y0, x1 - width / 2.0, y1),
                        2 => (x0, y1 - width / 2.0, x1, y1 - width / 2.0),
                        _ => (x0 + width / 2.0, y0, x0 + width / 2.0, y1),
                    };
                    let mut pb = PathBuilder::new();
                    pb.move_to(ax, ay);
                    pb.line_to(bx, by);
                    if let Some(p) = pb.finish() {
                        let mut stroke = Stroke { width, ..Default::default() };
                        let len = if i == 0 || i == 2 { x1 - x0 } else { y1 - y0 };
                        let (dash, round) = blink_dash(len, width, style == BorderStyle::Dotted, false);
                        stroke.dash = dash;
                        if round {
                            stroke.line_cap = tiny_skia::LineCap::Round;
                        }
                        ctx.stroke_path(&p, &paint, &stroke, r);
                    }
                }
                _ => {
                    let full = match i {
                        0 => [(x0, y0), (x1, y0), (x1 - wr, y0 + wt), (x0 + wl, y0 + wt)],
                        1 => [(x1, y0), (x1, y1), (x1 - wr, y1 - wb), (x1 - wr, y0 + wt)],
                        2 => [(x1, y1), (x0, y1), (x0 + wl, y1 - wb), (x1 - wr, y1 - wb)],
                        _ => [(x0, y1), (x0, y0), (x0 + wl, y0 + wt), (x0 + wl, y1 - wb)],
                    };
                    let lerp = |a: (f32, f32), b: (f32, f32), t: f32| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
                    let band = |t0: f32, t1: f32| [lerp(full[0], full[3], t0), lerp(full[1], full[2], t0), lerp(full[1], full[2], t1), lerp(full[0], full[3], t1)];
                    let polys: Vec<([(f32, f32); 4], Color)> = match style {
                        BorderStyle::Double if w[i] >= 3.0 => vec![(band(0.0, 1.0 / 3.0), c), (band(2.0 / 3.0, 1.0), c)],
                        BorderStyle::Groove | BorderStyle::Ridge if w[i] >= 2.0 => {
                            let outer_inset = style == BorderStyle::Groove;
                            vec![(band(0.0, 0.5), shade(outer_inset)), (band(0.5, 1.0), shade(!outer_inset))]
                        }
                        BorderStyle::Groove => vec![(full, shade(true))],
                        BorderStyle::Ridge => vec![(full, shade(false))],
                        _ => vec![(full, c)],
                    };
                    for (poly, pc) in polys {
                        let mut pb = PathBuilder::new();
                        pb.move_to(poly[0].0, poly[0].1);
                        for p in &poly[1..] {
                            pb.line_to(p.0, p.1);
                        }
                        pb.close();
                        if let Some(p) = pb.finish() {
                            // Axis-aligned single-pixel borders: avoid AA blur.
                            let mut pt = paint_color(pc, op);
                            pt.anti_alias = has_r;
                            ctx.fill_path(&p, &pt, r);
                        }
                    }
                }
            }
        }
    }

    fn draw_gradient(&mut self, ctx: &mut Ctx, r: Rect, g: &GradSpec, radii: Radii) {
        let op = ctx.op();
        let shader = match gradient_shader(g, op) {
            Some(s) => s,
            None => return,
        };
        let paint = Paint { shader, anti_alias: true, ..Default::default() };
        let path = if radii.any() { rrect_path(r, radii) } else { rect_path(r) };
        if let Some(p) = path {
            ctx.fill_path(&p, &paint, r);
        }
    }

    /// Fill `area` with `color` through the alpha of `img` tiled at `tile`.
    fn draw_masked_color(&mut self, ctx: &mut Ctx, area: Rect, tile: Rect, img: &ImageData, color: Color) {
        let t = ctx.t();
        let sc = (t.sx * t.sx + t.ky * t.ky).sqrt().max(0.01);
        let pw = (area.w * sc).ceil().max(1.0);
        let ph = (area.h * sc).ceil().max(1.0);
        if pw * ph > 4_000_000.0 {
            return;
        }
        let mut tmp = match Pixmap::new(pw as u32, ph as u32) {
            Some(p) => p,
            None => return,
        };
        // 1. Paint the (tiled) mask image into the temporary layer.
        let ix = tile.w / img.width as f32 * sc;
        let iy = tile.h / img.height as f32 * sc;
        let shader = tiny_skia::Pattern::new(img.pixmap.as_ref(), tiny_skia::SpreadMode::Repeat, tiny_skia::FilterQuality::Bilinear, 1.0, Transform::from_row(ix, 0.0, 0.0, iy, (tile.x - area.x) * sc, (tile.y - area.y) * sc));
        let paint = Paint { shader, anti_alias: false, ..Default::default() };
        if let Some(rr) = tiny_skia::Rect::from_xywh(0.0, 0.0, pw, ph) {
            tmp.fill_rect(rr, &paint, Transform::identity(), None);
            // 2. Keep only the mask's alpha, recolored.
            let mut p2 = paint_color(color, 1.0);
            p2.blend_mode = tiny_skia::BlendMode::SourceIn;
            tmp.fill_rect(rr, &p2, Transform::identity(), None);
        }
        // 3. Composite the tinted layer.
        let op = ctx.op();
        let shader = tiny_skia::Pattern::new(tmp.as_ref(), tiny_skia::SpreadMode::Pad, tiny_skia::FilterQuality::Bilinear, op, Transform::from_row(1.0 / sc, 0.0, 0.0, 1.0 / sc, area.x, area.y));
        let paint = Paint { shader, anti_alias: false, ..Default::default() };
        if let Some(p) = rect_path(area) {
            ctx.fill_path(&p, &paint, area);
        }
    }

    /// Repeat a gradient tile over an area: the tile is rasterized once at device
    /// resolution and used as a repeating pattern.
    fn draw_gradient_tiles(&mut self, ctx: &mut Ctx, area: Rect, tile: Rect, g: &GradSpec) {
        let t = ctx.t();
        let sc = (t.sx * t.sx + t.ky * t.ky).sqrt().max(0.01);
        let pw = (tile.w * sc).ceil().max(1.0);
        let ph = (tile.h * sc).ceil().max(1.0);
        if pw * ph > 4_000_000.0 {
            // Huge tile: just paint the gradient over the area once.
            let rr = Rect::new(tile.x, tile.y, tile.w, tile.h);
            self.draw_gradient(ctx, rr.intersect(&area), g, Radii::ZERO);
            return;
        }
        let mut tp = match Pixmap::new(pw as u32, ph as u32) {
            Some(p) => p,
            None => return,
        };
        let shader = match gradient_shader(g, 1.0) {
            Some(s) => s,
            None => return,
        };
        let paint = Paint { shader, anti_alias: false, ..Default::default() };
        if let Some(rr) = tiny_skia::Rect::from_xywh(0.0, 0.0, tile.w, tile.h) {
            tp.fill_rect(rr, &paint, Transform::from_scale(pw / tile.w, ph / tile.h), None);
        }
        let op = ctx.op();
        let shader = tiny_skia::Pattern::new(tp.as_ref(), tiny_skia::SpreadMode::Repeat, tiny_skia::FilterQuality::Bilinear, op, Transform::from_row(tile.w / pw, 0.0, 0.0, tile.h / ph, tile.x, tile.y));
        let paint = Paint { shader, anti_alias: false, ..Default::default() };
        if let Some(p) = rect_path(area) {
            ctx.fill_path(&p, &paint, area);
        }
    }
}

/// Build a tiny-skia shader for a resolved CSS gradient (coordinates in local px).
/// Blink's `Color::Dark()`, used for inset/outset/groove/ridge shading.
fn blink_dark(c: Color) -> Color {
    if c.r == 0 && c.g == 0 && c.b == 0 {
        return Color::rgba(0x54, 0x54, 0x54, c.a);
    }
    let v = c.r.max(c.g).max(c.b) as f32 / 255.0;
    let m = if v == 0.0 { 0.0 } else { ((v - 0.33) / v).max(0.0) };
    let f = |x: u8| (x as f32 * m).round() as u8;
    Color::rgba(f(c.r), f(c.g), f(c.b), c.a)
}

/// CSS `<blend-mode>` code → tiny-skia blend mode.
fn blend_mode_of(m: u8) -> tiny_skia::BlendMode {
    use tiny_skia::BlendMode as B;
    match m {
        1 => B::Multiply,
        2 => B::Screen,
        3 => B::Overlay,
        4 => B::Darken,
        5 => B::Lighten,
        6 => B::ColorDodge,
        7 => B::ColorBurn,
        8 => B::HardLight,
        9 => B::SoftLight,
        10 => B::Difference,
        11 => B::Exclusion,
        12 => B::Hue,
        13 => B::Saturation,
        14 => B::Color,
        15 => B::Luminosity,
        16 => B::Plus,
        _ => B::SourceOver,
    }
}

fn gradient_shader(g: &GradSpec, op: f32) -> Option<Shader<'static>> {
    {
        let mut stops: Vec<tiny_skia::GradientStop> = g.stops.iter().map(|(p, c)| tiny_skia::GradientStop::new(*p, ts_color(*c, op))).collect();
        let mode = if g.repeating { tiny_skia::SpreadMode::Repeat } else { tiny_skia::SpreadMode::Pad };
        if g.repeating {
            // Normalize stop positions to the repeating interval.
            let first = g.stops.first().map(|s| s.0).unwrap_or(0.0);
            let last = g.stops.last().map(|s| s.0).unwrap_or(1.0);
            let span = (last - first).max(0.0001);
            stops = g.stops.iter().map(|(p, c)| tiny_skia::GradientStop::new((p - first) / span, ts_color(*c, op))).collect();
        }
        let shader: Option<Shader> = match g.kind {
            0 => {
                let (mut s, mut e) = (g.start, g.end);
                if g.repeating {
                    let first = g.stops.first().map(|s| s.0).unwrap_or(0.0);
                    let last = g.stops.last().map(|s| s.0).unwrap_or(1.0);
                    let d = (e.0 - s.0, e.1 - s.1);
                    let ns = (s.0 + d.0 * first, s.1 + d.1 * first);
                    let ne = (s.0 + d.0 * last, s.1 + d.1 * last);
                    s = ns;
                    e = ne;
                }
                tiny_skia::LinearGradient::new(tiny_skia::Point::from_xy(s.0, s.1), tiny_skia::Point::from_xy(e.0, e.1), stops, mode, Transform::identity())
            }
            1 => {
                let (rx, ry) = g.radius;
                let c = tiny_skia::Point::from_xy(g.start.0, g.start.1);
                let t = Transform::from_translate(g.start.0, g.start.1).pre_scale(1.0, ry / rx.max(0.001)).pre_translate(-g.start.0, -g.start.1);
                if g.repeating {
                    // The repeating interval runs from the first to the last
                    // stop's radius; a concentric two-circle gradient tiles
                    // inwards as well as outwards.
                    let first = g.stops.first().map(|s| s.0).unwrap_or(0.0);
                    let last = g.stops.last().map(|s| s.0).unwrap_or(1.0);
                    let (r0, r1) = (rx * first.max(0.0), rx * last.max(0.0));
                    if r1 - r0 < 0.01 {
                        None
                    } else if r0 < 0.001 {
                        tiny_skia::RadialGradient::new(c, 0.0, c, r1, stops, mode, t)
                    } else {
                        tiny_skia::RadialGradient::new(c, r0, c, r1, stops, mode, t)
                    }
                } else {
                    tiny_skia::RadialGradient::new(c, 0.0, c, rx, stops, mode, t)
                }
            }
            _ => {
                // Conic: a sweep gradient. CSS measures clockwise from 12
                // o'clock, Skia's sweep from 3 o'clock: rotate by angle - 90°.
                let c = tiny_skia::Point::from_xy(g.start.0, g.start.1);
                let t = Transform::from_rotate_at(g.angle - 90.0, g.start.0, g.start.1);
                if g.repeating {
                    let first = g.stops.first().map(|s| s.0).unwrap_or(0.0);
                    let last = g.stops.last().map(|s| s.0).unwrap_or(1.0);
                    if last - first < 0.0001 {
                        None
                    } else {
                        // tiny-skia skips the scale stage when the bias is 0, so
                        // start one period later (equivalent under Repeat).
                        let span = (last - first) * 360.0;
                        let a0 = if first.abs() < 1e-6 { span } else { first * 360.0 };
                        tiny_skia::SweepGradient::new(c, a0, a0 + span, stops, mode, t)
                    }
                } else {
                    tiny_skia::SweepGradient::new(c, 0.0, 360.0, stops, mode, t)
                }
            }
        };
        match shader {
            Some(s) => Some(s),
            None => g.stops.first().map(|(_, c)| Shader::SolidColor(ts_color(*c, op))),
        }
    }
}

impl Rasterizer {

    #[allow(clippy::too_many_arguments)]
    fn draw_inset_shadow(&mut self, ctx: &mut Ctx, r: Rect, radii: Radii, hole: Rect, hole_radii: Radii, blur: f32, color: Color) {
        let op = ctx.op();
        if r.w <= 0.0 || r.h <= 0.0 || color.a == 0 {
            return;
        }
        if (hole.w <= 0.0 || hole.h <= 0.0) && !radii.any() {
            ctx.fill_rect(r, color);
            return;
        }
        let steps = if blur <= 0.5 { 1 } else { ((blur / 2.0).ceil() as usize).clamp(2, 8) };
        for k in 0..steps {
            let d = if steps == 1 { 0.0 } else { blur * ((k as f32 + 0.5) / steps as f32 - 0.5) };
            let h = hole.inflate(d);
            // Keep the hole inside the padding box (approximates clipping to it).
            let x0 = h.x.max(r.x);
            let y0 = h.y.max(r.y);
            let x1 = h.right().min(r.right());
            let y1 = h.bottom().min(r.bottom());
            let mut pb = PathBuilder::new();
            if let Some(outer) = rrect_path(r, radii) {
                pb.push_path(&outer);
            } else {
                continue;
            }
            if x1 - x0 > 0.5 && y1 - y0 > 0.5 {
                let hr = hole_radii.map(|v| (v + d).max(0.0));
                if let Some(inner) = rrect_path(Rect::new(x0, y0, x1 - x0, y1 - y0), hr) {
                    pb.push_path(&inner);
                }
            }
            if let Some(p) = pb.finish() {
                let c = color.with_alpha_mul(1.0 / steps as f32);
                ctx.fill_path(&p, &paint_color(c, op), r);
            }
        }
    }

    /// Box-shadow with a real (3-pass box ≈ Gaussian) blur, rendered in a
    /// temporary layer with the element's border box cut out. Returns false if
    /// the fast path doesn't apply (rotations/huge areas).
    fn draw_outer_shadow(&mut self, ctx: &mut Ctx, r: Rect, radii: Radii, blur: f32, color: Color, hole: Option<(Rect, Radii)>) -> bool {
        if !ctx.simple_transform() || color.a == 0 {
            return color.a == 0;
        }
        let t = ctx.t();
        let (sx, sy) = (t.sx, t.sy);
        if sx <= 0.0 || sy <= 0.0 {
            return false;
        }
        let margin = blur + 2.0;
        let b = r.inflate(margin);
        let x0 = (b.x * sx + t.tx).floor();
        let y0 = (b.y * sy + t.ty).floor();
        let x1 = (b.right() * sx + t.tx).ceil();
        let y1 = (b.bottom() * sy + t.ty).ceil();
        // Only the part inside the current clip matters.
        let (cx0, cy0, cx1, cy1) = ctx.clip_rect();
        let (vx0, vy0, vx1, vy1) = (x0.max(cx0 - margin * sx), y0.max(cy0 - margin * sy), x1.min(cx1 + margin * sx), y1.min(cy1 + margin * sy));
        if vx1 <= vx0 || vy1 <= vy0 {
            return true;
        }
        let (w, h) = ((vx1 - vx0) as u32, (vy1 - vy0) as u32);
        if (w as u64) * (h as u64) > 6_000_000 {
            return false;
        }
        let local = Transform::from_row(sx, 0.0, 0.0, sy, t.tx - vx0, t.ty - vy0);
        let mut mask = match Mask::new(w, h) {
            Some(m) => m,
            None => return false,
        };
        if let Some(p) = rrect_path(r, radii) {
            mask.fill_path(&p, FillRule::Winding, true, local);
        }
        if blur > 0.5 {
            box_blur3(mask.data_mut(), w as usize, h as usize, blur / 2.0 * sx);
        }
        let mut tmp = match Pixmap::new(w, h) {
            Some(p) => p,
            None => return false,
        };
        tmp.fill(ts_color(color, ctx.op()));
        // Apply the blurred coverage.
        {
            let m = mask.data();
            for (px, &a) in tmp.data_mut().chunks_exact_mut(4).zip(m.iter()) {
                if a == 255 {
                    continue;
                }
                let k = a as u32;
                for c in px.iter_mut() {
                    *c = ((*c as u32 * k + 127) / 255) as u8;
                }
            }
        }
        if let Some((hr, hrad)) = hole {
            if let Some(p) = rrect_path(hr, hrad) {
                let mut clear = Paint::default();
                clear.blend_mode = tiny_skia::BlendMode::Clear;
                clear.anti_alias = true;
                tmp.fill_path(&p, &clear, FillRule::Winding, local, None);
            }
        }
        let shader = tiny_skia::Pattern::new(tmp.as_ref(), tiny_skia::SpreadMode::Pad, tiny_skia::FilterQuality::Nearest, 1.0, Transform::from_row(1.0 / sx, 0.0, 0.0, 1.0 / sy, (vx0 - t.tx) / sx, (vy0 - t.ty) / sy));
        let paint = Paint { shader, anti_alias: false, ..Default::default() };
        let area = Rect::new((vx0 - t.tx) / sx, (vy0 - t.ty) / sy, w as f32 / sx, h as f32 / sy);
        if let Some(p) = rect_path(area) {
            ctx.fill_path(&p, &paint, area);
        }
        true
    }

    fn draw_shadow(&mut self, ctx: &mut Ctx, r: Rect, radii: Radii, blur: f32, spread: f32, color: Color, inset: bool) {
        let op = ctx.op();
        if inset {
            let width = (blur / 2.0 + spread).max(1.0);
            if let Some(p) = rrect_path(r.inflate(-width / 2.0), radii) {
                let stroke = Stroke { width, ..Default::default() };
                let c = color.with_alpha_mul(0.5);
                ctx.stroke_path(&p, &paint_color(c, op), &stroke, r);
            }
            return;
        }
        if blur <= 0.5 {
            if let Some(p) = rrect_path(r, radii) {
                ctx.fill_path(&p, &paint_color(color, op), r);
            }
            return;
        }
        // Approximate a Gaussian blur with stacked translucent rounded rects.
        let steps = ((blur / 2.0).ceil() as usize).clamp(2, 8);
        let alpha_each = 1.0 / steps as f32;
        for k in 0..steps {
            let t = (k as f32 + 0.5) / steps as f32; // 0..1
            let grow = blur * (1.0 - t) - blur * 0.5;
            let rr = r.inflate(grow);
            if rr.w <= 0.0 || rr.h <= 0.0 {
                continue;
            }
            let rad = radii.map(|v| (v + grow).max(0.0).max(if grow > 0.0 { grow } else { 0.0 }));
            if let Some(p) = rrect_path(rr, rad) {
                let c = color.with_alpha_mul(alpha_each * 1.2);
                ctx.fill_path(&p, &paint_color(c, op), rr);
            }
        }
    }
}

/// Three box-blur passes (horizontal + vertical) approximating a Gaussian with `sigma`.
fn box_blur3(data: &mut [u8], w: usize, h: usize, sigma: f32) {
    if sigma < 0.5 || w == 0 || h == 0 {
        return;
    }
    // Box sizes for 3 passes (W3C / Kutskir).
    let n = 3.0f32;
    let w_ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = w_ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma - n * (wl * wl) as f32 - 4.0 * n * wl as f32 - 3.0 * n) / (-4.0 * wl as f32 - 4.0);
    let m = m_ideal.round() as i32;
    let sizes = [0, 1, 2].map(|i| if i < m { wl } else { wu });
    let mut buf = vec![0u8; data.len()];
    for s in sizes {
        let r = ((s - 1) / 2).max(0) as usize;
        if r == 0 {
            continue;
        }
        box_h(data, &mut buf, w, h, r);
        box_v(&buf, data, w, h, r);
    }
}

fn box_h(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let div = (2 * r + 1) as u32;
    for y in 0..h {
        let row = &src[y * w..(y + 1) * w];
        let out = &mut dst[y * w..(y + 1) * w];
        let mut acc: u32 = 0;
        for i in 0..=r.min(w - 1) {
            acc += row[i] as u32;
        }
        for x in 0..w {
            out[x] = (acc / div) as u8;
            let add = x + r + 1;
            if add < w {
                acc += row[add] as u32;
            }
            if x >= r {
                acc -= row[x - r] as u32;
            }
        }
    }
}

fn box_v(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize) {
    let div = (2 * r + 1) as u32;
    let mut acc = vec![0u32; w];
    for y in 0..=r.min(h - 1) {
        for x in 0..w {
            acc[x] += src[y * w + x] as u32;
        }
    }
    for y in 0..h {
        for x in 0..w {
            dst[y * w + x] = (acc[x] / div) as u8;
        }
        let add = y + r + 1;
        if add < h {
            for x in 0..w {
                acc[x] += src[add * w + x] as u32;
            }
        }
        if y >= r {
            let sub = y - r;
            for x in 0..w {
                acc[x] -= src[sub * w + x] as u32;
            }
        }
    }
}

// ------------------------------------------------------------------ filters

/// Apply a filter chain to the drawn part of `layer` (within `clip`) and
/// composite the result onto `dst` with source-over.
/// Index of the pop matching the push just before `start` (items.len() if none).
fn matching_pop(items: &[Item], start: usize) -> usize {
    let mut depth = 1usize;
    for (j, it) in items[start..].iter().enumerate() {
        if crate::paint::damage::is_push_item(it) {
            depth += 1;
        } else if crate::paint::damage::is_pop_item(it) {
            depth -= 1;
            if depth == 0 {
                return start + j;
            }
        }
    }
    items.len()
}

/// How far a filter chain spreads content, in CSS px (left, top, right, bottom).
fn filter_spread(filters: &[Filter]) -> (f32, f32, f32, f32) {
    let (mut l, mut t, mut r, mut b) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for f in filters {
        match f {
            Filter::Blur(s) => {
                let e = 3.0 * s;
                l += e;
                t += e;
                r += e;
                b += e;
            }
            Filter::DropShadow { x, y, blur, .. } => {
                let e = 1.5 * blur;
                l += (e - x).max(0.0);
                r += (e + x).max(0.0);
                t += (e - y).max(0.0);
                b += (e + y).max(0.0);
            }
            Filter::ColorMatrix(_) => {}
        }
    }
    (l, t, r, b)
}

/// Device-space bounds of what `items` (a group's content) can paint when
/// drawn under transform `t`, or None when unknown (fixed content, 3D warps).
fn content_dev_bounds(items: &[Item], t: Transform, scroll: (f32, f32), scale: f32) -> Option<(f32, f32, f32, f32)> {
    let mut stack: Vec<(Transform, (f32, f32, f32, f32))> = vec![(t, (0.0, 0.0, 0.0, 0.0))];
    let mut acc: Option<(f32, f32, f32, f32)> = None;
    let map = |t: &Transform, r: Rect, pad: (f32, f32, f32, f32)| -> (f32, f32, f32, f32) {
        let pts = [(r.x, r.y), (r.right(), r.y), (r.x, r.bottom()), (r.right(), r.bottom())];
        let (mut x0, mut y0, mut x1, mut y1) = (f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
        for (x, y) in pts {
            let px = x * t.sx + y * t.kx + t.tx;
            let py = x * t.ky + y * t.sy + t.ty;
            x0 = x0.min(px);
            y0 = y0.min(py);
            x1 = x1.max(px);
            y1 = y1.max(py);
        }
        (x0 - pad.0 - 1.0, y0 - pad.1 - 1.0, x1 + pad.2 + 1.0, y1 + pad.3 + 1.0)
    };
    for it in items {
        let (ct, pad) = *stack.last()?;
        match it {
            Item::BeginFixed | Item::PushWarp { .. } => return None,
            Item::PushTransform(m) => stack.push((ct.pre_concat(Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5])), pad)),
            Item::PushSticky(st) => {
                let (dx, dy) = st.delta(scroll);
                stack.push((ct.pre_concat(Transform::from_translate(dx, dy)), pad));
            }
            Item::PushFilter(f) => {
                let (l, tp, r, b) = filter_spread(f);
                let k = scale * ct.sx.abs().max(ct.sy.abs()).max(ct.kx.abs()).max(ct.ky.abs()).max(1.0) / scale.max(0.0001);
                stack.push((ct, (pad.0 + l * scale * k, pad.1 + tp * scale * k, pad.2 + r * scale * k, pad.3 + b * scale * k)));
            }
            it if crate::paint::damage::is_push_item(it) => stack.push((ct, pad)),
            it if crate::paint::damage::is_pop_item(it) => {
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            _ => {
                let b = it.bounds()?;
                let r = map(&ct, b, pad);
                if !(r.0.is_finite() && r.1.is_finite() && r.2.is_finite() && r.3.is_finite()) {
                    return None;
                }
                acc = Some(match acc {
                    Some(a) => (a.0.min(r.0), a.1.min(r.1), a.2.max(r.2), a.3.max(r.3)),
                    None => r,
                });
            }
        }
    }
    Some(acc.unwrap_or((0.0, 0.0, 0.0, 0.0)))
}

fn intersect4(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    let x0 = a.0.max(b.0);
    let y0 = a.1.max(b.1);
    (x0, y0, a.2.min(b.2).max(x0), a.3.min(b.3).max(y0))
}

fn composite_filtered(layer: Pixmap, filters: &[Filter], clip: (f32, f32, f32, f32), scale: f32, dst: &mut Pixmap) {
    let (pw, ph) = (layer.width() as i32, layer.height() as i32);
    let (cx0, cy0) = ((clip.0.floor() as i32).max(0), (clip.1.floor() as i32).max(0));
    let (cx1, cy1) = ((clip.2.ceil() as i32).min(pw), (clip.3.ceil() as i32).min(ph));
    if cx1 <= cx0 || cy1 <= cy0 {
        return;
    }
    // Bounding box of what was drawn.
    let px = layer.pixels();
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for y in cy0..cy1 {
        let row = &px[(y * pw) as usize..((y + 1) * pw) as usize];
        let mut first = None;
        for x in cx0..cx1 {
            if row[x as usize].alpha() != 0 {
                first = Some(x);
                break;
            }
        }
        if let Some(f) = first {
            let mut last = f;
            for x in (f..cx1).rev() {
                if row[x as usize].alpha() != 0 {
                    last = x;
                    break;
                }
            }
            x0 = x0.min(f);
            x1 = x1.max(last + 1);
            y0 = y0.min(y);
            y1 = y1.max(y + 1);
        }
    }
    if x1 <= x0 {
        return;
    }
    // Grow the region by how far the filters can spread content.
    let (mut l, mut t, mut r, mut b) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for f in filters {
        match f {
            Filter::Blur(s) => {
                let e = 3.0 * s * scale;
                l += e;
                t += e;
                r += e;
                b += e;
            }
            Filter::DropShadow { x, y, blur, .. } => {
                let e = 1.5 * blur * scale;
                l += (e - x * scale).max(0.0);
                r += (e + x * scale).max(0.0);
                t += (e - y * scale).max(0.0);
                b += (e + y * scale).max(0.0);
            }
            Filter::ColorMatrix(_) => {}
        }
    }
    let rx0 = (x0 - l.ceil() as i32).max(cx0);
    let ry0 = (y0 - t.ceil() as i32).max(cy0);
    let rx1 = (x1 + r.ceil() as i32).min(cx1);
    let ry1 = (y1 + b.ceil() as i32).min(cy1);
    let (w, h) = ((rx1 - rx0) as usize, (ry1 - ry0) as usize);
    if w == 0 || h == 0 {
        return;
    }
    // Copy the region out as premultiplied RGBA f32.
    let mut buf = vec![[0f32; 4]; w * h];
    for y in 0..h {
        let src = &px[((ry0 as usize + y) * pw as usize + rx0 as usize)..][..w];
        for (x, p) in src.iter().enumerate() {
            buf[y * w + x] = [p.red() as f32, p.green() as f32, p.blue() as f32, p.alpha() as f32];
        }
    }
    run_filters(&mut buf, w, h, filters, scale);
    let mut data = Vec::with_capacity(w * h * 4);
    for p in &buf {
        let a = p[3].round().clamp(0.0, 255.0);
        data.extend_from_slice(&[p[0].round().clamp(0.0, a) as u8, p[1].round().clamp(0.0, a) as u8, p[2].round().clamp(0.0, a) as u8, a as u8]);
    }
    if let Some(region) = tiny_skia::IntSize::from_wh(w as u32, h as u32).and_then(|sz| Pixmap::from_vec(data, sz)) {
        dst.draw_pixmap(rx0, ry0, region.as_ref(), &tiny_skia::PixmapPaint::default(), Transform::identity(), None);
    }
}

/// Apply a filter chain to a premultiplied RGBA f32 buffer.
fn run_filters(buf: &mut Vec<[f32; 4]>, w: usize, h: usize, filters: &[Filter], scale: f32) {
    for f in filters {
        match f {
            Filter::Blur(s) => gaussian_blur(buf, w, h, s * scale),
            Filter::ColorMatrix(m) => {
                for p in buf.iter_mut() {
                    let a = p[3] / 255.0;
                    let (r, g, b) = if a > 0.0 { (p[0] / 255.0 / a, p[1] / 255.0 / a, p[2] / 255.0 / a) } else { (0.0, 0.0, 0.0) };
                    let row = |i: usize| (m[i] * r + m[i + 1] * g + m[i + 2] * b + m[i + 3] * a + m[i + 4]).clamp(0.0, 1.0);
                    let (nr, ng, nb, na) = (row(0), row(5), row(10), row(15));
                    *p = [nr * na * 255.0, ng * na * 255.0, nb * na * 255.0, na * 255.0];
                }
            }
            Filter::DropShadow { x, y, blur, color } => {
                let ca = color.a as f32 / 255.0;
                let mut sh: Vec<[f32; 4]> = buf.iter().map(|p| {
                    let a = p[3] * ca;
                    [color.r as f32 * a / 255.0, color.g as f32 * a / 255.0, color.b as f32 * a / 255.0, a]
                }).collect();
                gaussian_blur(&mut sh, w, h, blur * 0.5 * scale);
                let (dx, dy) = ((x * scale).round() as i32, (y * scale).round() as i32);
                let mut out = vec![[0f32; 4]; w * h];
                for yy in 0..h as i32 {
                    for xx in 0..w as i32 {
                        let (sx, sy) = (xx - dx, yy - dy);
                        let shp = if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 { sh[(sy as usize) * w + sx as usize] } else { [0.0; 4] };
                        let src = buf[(yy as usize) * w + xx as usize];
                        let k = 1.0 - src[3] / 255.0;
                        out[(yy as usize) * w + xx as usize] = [src[0] + shp[0] * k, src[1] + shp[1] * k, src[2] + shp[2] * k, src[3] + shp[3] * k];
                    }
                }
                *buf = out;
            }
        }
    }
}

/// `backdrop-filter`: filter what is already drawn under the device rect
/// `r` (x0, y0, x1, y1) and put the result back in place, clipped to `clip`.
fn apply_backdrop(dst: &mut Pixmap, r: (f32, f32, f32, f32), clip: (f32, f32, f32, f32), filters: &[Filter], scale: f32) {
    let (pw, ph) = (dst.width() as i32, dst.height() as i32);
    let ir = (r.0.max(clip.0).floor() as i32, r.1.max(clip.1).floor() as i32, r.2.min(clip.2).ceil() as i32, r.3.min(clip.3).ceil() as i32);
    let (ix0, iy0, ix1, iy1) = (ir.0.max(0), ir.1.max(0), ir.2.min(pw), ir.3.min(ph));
    if ix1 <= ix0 || iy1 <= iy0 {
        return;
    }
    let mut spread = 0.0f32;
    for f in filters {
        if let Filter::Blur(s) = f {
            spread += 3.0 * s * scale;
        }
    }
    // The filter's input is the backdrop within the element's bounds; the
    // blur reads past them mirrored (edge mode "mirror", as Chromium).
    let mirror = |v: i32, lo: i32, hi: i32| -> i32 {
        let v = if v < lo { lo + (lo - 1 - v) } else if v >= hi { hi - 1 - (v - hi) } else { v };
        v.clamp(lo, hi - 1)
    };
    let e = spread.ceil() as i32;
    let (rx0, ry0, rx1, ry1) = (ix0 - e, iy0 - e, ix1 + e, iy1 + e);
    let (w, h) = ((rx1 - rx0) as usize, (ry1 - ry0) as usize);
    let mut buf = vec![[0f32; 4]; w * h];
    {
        let px = dst.pixels();
        for y in 0..h {
            let sy = mirror(ry0 + y as i32, iy0, iy1) as usize;
            for x in 0..w {
                let sx = mirror(rx0 + x as i32, ix0, ix1) as usize;
                let p = px[sy * pw as usize + sx];
                buf[y * w + x] = [p.red() as f32, p.green() as f32, p.blue() as f32, p.alpha() as f32];
            }
        }
    }
    run_filters(&mut buf, w, h, filters, scale);
    let data = dst.data_mut();
    for y in iy0..iy1 {
        for x in ix0..ix1 {
            let p = buf[(y - ry0) as usize * w + (x - rx0) as usize];
            let a = p[3].round().clamp(0.0, 255.0);
            let o = ((y * pw + x) * 4) as usize;
            data[o] = p[0].round().clamp(0.0, a) as u8;
            data[o + 1] = p[1].round().clamp(0.0, a) as u8;
            data[o + 2] = p[2].round().clamp(0.0, a) as u8;
            data[o + 3] = a as u8;
        }
    }
}

/// Approximate a Gaussian blur of standard deviation `sigma` (device px)
/// with three box blurs (W3C/Kovesi box sizes), in place.
fn gaussian_blur(buf: &mut [[f32; 4]], w: usize, h: usize, sigma: f32) {
    if sigma < 0.3 || w == 0 || h == 0 {
        return;
    }
    let n = 3.0f32;
    let w_ideal = (12.0 * sigma * sigma / n + 1.0).sqrt();
    let mut wl = w_ideal.floor() as i32;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m = ((12.0 * sigma * sigma - n * (wl * wl) as f32 - 4.0 * n * wl as f32 - 3.0 * n) / (-4.0 * wl as f32 - 4.0)).round() as i32;
    let mut tmp = vec![[0f32; 4]; buf.len()];
    for i in 0..3 {
        let size = if i < m { wl } else { wu };
        let r = ((size - 1) / 2).max(0) as usize;
        if r == 0 {
            continue;
        }
        box_blur_pass(buf, &mut tmp, w, h, r, true);
        box_blur_pass(&tmp, buf, w, h, r, false);
    }
}

/// One box-blur pass of radius `r` (horizontal or vertical); pixels outside are transparent.
fn box_blur_pass(src: &[[f32; 4]], dst: &mut [[f32; 4]], w: usize, h: usize, r: usize, horizontal: bool) {
    let (len, lines) = if horizontal { (w, h) } else { (h, w) };
    let idx = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let mut acc = [0f32; 4];
        for i in 0..=r.min(len - 1) {
            let p = src[idx(line, i)];
            for c in 0..4 {
                acc[c] += p[c];
            }
        }
        for i in 0..len {
            for c in 0..4 {
                dst[idx(line, i)][c] = acc[c] * norm;
            }
            if i + r + 1 < len {
                let p = src[idx(line, i + r + 1)];
                for c in 0..4 {
                    acc[c] += p[c];
                }
            }
            if i >= r {
                let p = src[idx(line, i - r)];
                for c in 0..4 {
                    acc[c] -= p[c];
                }
            }
        }
    }
}


fn raster_prof() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("OHB_RASTER_PROF").is_some())
}

/// Upper bound on what the resampled-image cache keeps alive.
const SCALED_CACHE_BYTES: usize = 96 << 20;

thread_local! {
    static RASTER_PROF: std::cell::RefCell<std::collections::HashMap<&'static str, (u32, std::time::Duration)>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn raster_prof_add(kind: &'static str, d: std::time::Duration) {
    RASTER_PROF.with(|m| {
        let mut m = m.borrow_mut();
        let e = m.entry(kind).or_insert((0, std::time::Duration::ZERO));
        e.0 += 1;
        e.1 += d;
    });
}

/// Debugging aid (OHB_RASTER_PROF): print where raster time went, then reset.
pub fn raster_prof_report() {
    if !raster_prof() {
        return;
    }
    RASTER_PROF.with(|m| {
        let mut v: Vec<(&'static str, (u32, std::time::Duration))> = m.borrow_mut().drain().collect();
        v.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
        for (k, (n, d)) in v.iter().take(12) {
            eprintln!("[raster] {k:<14} {n:>6} items {d:?}");
        }
    });
}

fn item_kind(it: &Item) -> &'static str {
    match it {
        Item::Rect { .. } => "Rect",
        Item::RRect { .. } => "RRect",
        Item::Border { .. } => "Border",
        Item::Text { .. } => "Text",
        Item::Image { .. } => "Image",
        Item::Pattern { .. } => "Pattern",
        Item::Gradient { .. } => "Gradient",
        Item::MaskedColor { .. } => "MaskedColor",
        Item::GradientTiles { .. } => "GradientTiles",
        Item::Shadow { .. } => "Shadow",
        Item::InsetShadow { .. } => "InsetShadow",
        Item::Line { .. } => "Line",
        Item::PushClip { .. } => "PushClip",
        Item::PushClipPath(_) => "PushClipPath",
        Item::PopClip => "PopClip",
        Item::PushMask(_) => "PushMask",
        Item::PopMask => "PopMask",
        Item::PushOpacity(_) => "PushOpacity",
        Item::PopOpacity => "PopOpacity",
        Item::PushBlend(_) => "PushBlend",
        Item::PopBlend => "PopBlend",
        Item::PushGroup { .. } => "PushGroup",
        Item::PopGroup => "PopGroup",
        Item::PushTransform(_) => "PushTransform",
        Item::PopTransform => "PopTransform",
        Item::PushWarp { .. } => "PushWarp",
        Item::PopWarp => "PopWarp",
        Item::PushFilter(_) => "PushFilter",
        Item::PopFilter => "PopFilter",
        Item::Backdrop { .. } => "Backdrop",
        Item::BeginFixed => "BeginFixed",
        Item::EndFixed => "EndFixed",
        Item::PushSticky(_) => "PushSticky",
        Item::PopSticky => "PopSticky",
    }
}

struct MaskTimer(Option<std::time::Instant>);
impl Drop for MaskTimer {
    fn drop(&mut self) {
        if let Some(t0) = self.0 {
            raster_prof_add("(clip mask)", t0.elapsed());
        }
    }
}

/// Whether a display list's `position: fixed` groups can be drawn as a
/// separate layer over the cached scrolling content at this scroll position:
/// every fixed group sits outside any layer, transform or warp, and nothing
/// painted after the first one overlaps the fixed layer on screen (so drawing
/// the overlay last keeps the paint order).
pub fn fixed_layer_ok(list: &DisplayList, scroll: (f32, f32)) -> bool {
    if list.fixed == 0 {
        return false;
    }
    // The fixed layer's extent (viewport coordinates).
    let mut fixed_area: Option<Rect> = None;
    {
        let mut depth = 0;
        for it in &list.items {
            match it {
                Item::BeginFixed => depth += 1,
                Item::EndFixed => depth -= 1,
                _ if depth > 0 && it.draws() => match it.bounds() {
                    Some(b) => fixed_area = Some(fixed_area.map_or(b, |a| a.union(&b))),
                    None => return false,
                },
                _ => {}
            }
        }
    }
    let Some(area) = fixed_area else { return true };
    let mut layer_depth = 0i32;
    let mut xf_depth = 0i32;
    let mut fixed_depth = 0i32;
    let mut seen_fixed = false;
    for it in &list.items {
        match it {
            Item::BeginFixed => {
                if fixed_depth == 0 && (layer_depth != 0 || xf_depth != 0) {
                    return false;
                }
                fixed_depth += 1;
                seen_fixed = true;
            }
            Item::EndFixed => fixed_depth -= 1,
            _ if fixed_depth > 0 => {}
            Item::PushTransform(_) | Item::PushWarp { .. } | Item::PushSticky(_) => xf_depth += 1,
            Item::PopTransform | Item::PopWarp | Item::PopSticky => xf_depth -= 1,
            Item::PushOpacity(_) | Item::PushGroup { .. } | Item::PushBlend(_) | Item::PushFilter(_) | Item::PushMask(_) => layer_depth += 1,
            Item::PopOpacity | Item::PopGroup | Item::PopBlend | Item::PopFilter | Item::PopMask => layer_depth -= 1,
            _ if seen_fixed && it.draws() => {
                if xf_depth != 0 {
                    return false;
                }
                match it.bounds() {
                    Some(b) => {
                        let on_screen = Rect::new(b.x - scroll.0, b.y - scroll.1, b.w, b.h);
                        if on_screen.intersects(&area) {
                            return false;
                        }
                    }
                    None => return false,
                }
            }
            _ => {}
        }
    }
    true
}
