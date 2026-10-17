// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronPaint canvas: the 2D canvas backing store and drawing operations.
//!
//! Geometry arrives from the JS side already transformed into canvas pixel
//! space (paths are flattened to move/line/cubic/close ops), so this module
//! only has to rasterize with tiny-skia.

use std::sync::Arc;

use tiny_skia::{FillRule, LineCap, LineJoin, Mask, Paint, PathBuilder, Pixmap, Shader, Stroke, StrokeDash, Transform};

use crate::css::values::Color;
use crate::paint::ImageData;

/// Upper bound on canvas pixels (keeps hostile pages from exhausting memory).
const MAX_PIXELS: u64 = 16_000_000;

pub fn new_canvas(w: u32, h: u32) -> Arc<ImageData> {
    let (w, h) = clamp_size(w, h);
    Arc::new(ImageData { pixmap: Pixmap::new(w, h).unwrap(), width: w, height: h, density: 1.0, nat: None, svg_par: None })
}

fn clamp_size(w: u32, h: u32) -> (u32, u32) {
    let w = w.clamp(1, 16384);
    let mut h = h.clamp(1, 16384);
    if w as u64 * h as u64 > MAX_PIXELS {
        h = (MAX_PIXELS / w as u64).max(1) as u32;
    }
    (w, h)
}

/// Paint source for fills and strokes.
pub enum Style {
    Color(Color),
    /// x0, y0, x1, y1 and (offset, color) stops, in user space under `t`.
    Linear([f32; 4], Vec<(f32, Color)>),
    /// x0, y0, r0, x1, y1, r1 and stops.
    Radial([f32; 6], Vec<(f32, Color)>),
    Pattern(Arc<ImageData>, bool, bool),
}

pub struct DrawParams {
    pub style: Style,
    pub alpha: f32,
    /// User-space → canvas transform (for gradients/patterns).
    pub t: Transform,
    pub composite: tiny_skia::BlendMode,
    pub clip: Option<Vec<f32>>,
}

pub fn build_path(ops: &[f32]) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    let mut i = 0;
    let mut open = false;
    while i < ops.len() {
        match ops[i] as i32 {
            0 if i + 2 < ops.len() => {
                pb.move_to(ops[i + 1], ops[i + 2]);
                open = true;
                i += 3;
            }
            1 if i + 2 < ops.len() => {
                if !open {
                    pb.move_to(ops[i + 1], ops[i + 2]);
                    open = true;
                } else {
                    pb.line_to(ops[i + 1], ops[i + 2]);
                }
                i += 3;
            }
            2 if i + 6 < ops.len() => {
                if !open {
                    pb.move_to(ops[i + 1], ops[i + 2]);
                    open = true;
                }
                pb.cubic_to(ops[i + 1], ops[i + 2], ops[i + 3], ops[i + 4], ops[i + 5], ops[i + 6]);
                i += 7;
            }
            3 => {
                pb.close();
                i += 1;
            }
            _ => break,
        }
    }
    pb.finish()
}

fn ts_color(c: Color, alpha: f32) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, ((c.a as f32) * alpha.clamp(0.0, 1.0)).round() as u8)
}

fn make_shader<'a>(p: &'a DrawParams) -> Option<Shader<'a>> {
    let stops = |s: &Vec<(f32, Color)>| s.iter().map(|(o, c)| tiny_skia::GradientStop::new(o.clamp(0.0, 1.0), ts_color(*c, p.alpha))).collect::<Vec<_>>();
    match &p.style {
        Style::Color(c) => Some(Shader::SolidColor(ts_color(*c, p.alpha))),
        Style::Linear(g, s) => {
            if s.is_empty() {
                return None;
            }
            if s.len() == 1 {
                return Some(Shader::SolidColor(ts_color(s[0].1, p.alpha)));
            }
            tiny_skia::LinearGradient::new(tiny_skia::Point::from_xy(g[0], g[1]), tiny_skia::Point::from_xy(g[2], g[3]), stops(s), tiny_skia::SpreadMode::Pad, p.t)
        }
        Style::Radial(g, s) => {
            if s.is_empty() {
                return None;
            }
            if s.len() == 1 {
                return Some(Shader::SolidColor(ts_color(s[0].1, p.alpha)));
            }
            // tiny-skia's two-point conical gradient: start point/end point with end radius.
            tiny_skia::RadialGradient::new(tiny_skia::Point::from_xy(g[0], g[1]), g[2].max(0.0), tiny_skia::Point::from_xy(g[3], g[4]), g[5].max(0.001), stops(s), tiny_skia::SpreadMode::Pad, p.t)
        }
        Style::Pattern(img, rx, ry) => {
            let mode = if *rx || *ry { tiny_skia::SpreadMode::Repeat } else { tiny_skia::SpreadMode::Pad };
            Some(tiny_skia::Pattern::new(img.pixmap.as_ref(), mode, tiny_skia::FilterQuality::Bilinear, p.alpha, p.t))
        }
    }
}

fn clip_mask(pm: &Pixmap, clip: &Option<Vec<f32>>) -> Option<Mask> {
    let ops = clip.as_ref()?;
    let path = build_path(ops)?;
    let mut m = Mask::new(pm.width(), pm.height())?;
    m.fill_path(&path, FillRule::Winding, true, Transform::identity());
    Some(m)
}

pub fn fill(img: &mut Arc<ImageData>, ops: &[f32], even_odd: bool, p: &DrawParams) {
    let path = match build_path(ops) {
        Some(p) => p,
        None => return,
    };
    let data = Arc::make_mut(img);
    let mask = clip_mask(&data.pixmap, &p.clip);
    let shader = match make_shader(p) {
        Some(s) => s,
        None => return,
    };
    let paint = Paint { shader, anti_alias: true, blend_mode: p.composite, ..Default::default() };
    let rule = if even_odd { FillRule::EvenOdd } else { FillRule::Winding };
    data.pixmap.fill_path(&path, &paint, rule, Transform::identity(), mask.as_ref());
}

#[allow(clippy::too_many_arguments)]
pub fn stroke(img: &mut Arc<ImageData>, ops: &[f32], width: f32, cap: u8, join: u8, miter: f32, dash: &[f32], p: &DrawParams) {
    let path = match build_path(ops) {
        Some(p) => p,
        None => return,
    };
    let data = Arc::make_mut(img);
    let mask = clip_mask(&data.pixmap, &p.clip);
    let shader = match make_shader(p) {
        Some(s) => s,
        None => return,
    };
    let paint = Paint { shader, anti_alias: true, blend_mode: p.composite, ..Default::default() };
    let mut st = Stroke { width: width.max(0.01), miter_limit: miter.max(1.0), ..Default::default() };
    st.line_cap = match cap {
        1 => LineCap::Round,
        2 => LineCap::Square,
        _ => LineCap::Butt,
    };
    st.line_join = match join {
        1 => LineJoin::Round,
        2 => LineJoin::Bevel,
        _ => LineJoin::Miter,
    };
    if dash.len() >= 2 && dash.iter().all(|d| *d >= 0.0) && dash.iter().any(|d| *d > 0.0) {
        let mut d = dash.to_vec();
        if d.len() % 2 == 1 {
            d.extend_from_slice(dash);
        }
        st.dash = StrokeDash::new(d, 0.0);
    }
    data.pixmap.stroke_path(&path, &paint, &st, Transform::identity(), mask.as_ref());
}

/// Draw `src` (sub-rect sx,sy,sw,sh) into the destination quad given by the
/// user-space rect dx,dy,dw,dh under transform `t`.
#[allow(clippy::too_many_arguments)]
pub fn draw_image(img: &mut Arc<ImageData>, src: &ImageData, s: [f32; 4], d: [f32; 4], t: Transform, alpha: f32, composite: tiny_skia::BlendMode, smooth: bool, clip: &Option<Vec<f32>>) {
    let [sx, sy, sw, sh] = s;
    let [dx, dy, dw, dh] = d;
    if sw <= 0.0 || sh <= 0.0 || dw == 0.0 || dh == 0.0 {
        return;
    }
    // Image pixels are in device px of the source; account for its density.
    let scale_x = dw / sw;
    let scale_y = dh / sh;
    let dens = src.density.max(0.01);
    let pt = t.pre_concat(Transform::from_row(scale_x / dens, 0.0, 0.0, scale_y / dens, dx - sx * scale_x, dy - sy * scale_y));
    // Fast path: an unscaled, unrotated copy to whole pixels (sprite blits in
    // canvas games) is a plain source-over loop, without the pipeline.
    // With nearest sampling (smoothing off) a fractional offset is the same
    // copy: pixels whose centers fall in the rect, each reading the source
    // pixel under its center.
    let whole = |v: f32| v.fract() == 0.0;
    if clip.is_none() && matches!(composite, tiny_skia::BlendMode::SourceOver) && alpha >= 1.0 && pt.sx == 1.0 && pt.sy == 1.0 && pt.kx == 0.0 && pt.ky == 0.0 && t.sx == 1.0 && t.sy == 1.0 && t.kx == 0.0 && t.ky == 0.0 {
        let (l, tp) = (dx.min(dx + dw) + t.tx, dy.min(dy + dh) + t.ty);
        let (r, b) = (l + dw.abs(), tp + dh.abs());
        if !smooth || (whole(pt.tx) && whole(pt.ty) && whole(l) && whole(tp) && whole(r) && whole(b)) {
            let first = |v: f32| (v - 0.5).ceil() as i64;
            let (x0, y0) = (first(l), first(tp));
            blit_over(img, src, x0, y0, first(r) - x0, first(b) - y0, (pt.tx - 0.5).ceil() as i64, (pt.ty - 0.5).ceil() as i64);
            return;
        }
    }
    let q = if smooth { tiny_skia::FilterQuality::Bilinear } else { tiny_skia::FilterQuality::Nearest };
    let shader = tiny_skia::Pattern::new(src.pixmap.as_ref(), tiny_skia::SpreadMode::Pad, q, alpha.clamp(0.0, 1.0), pt);
    let paint = Paint { shader, anti_alias: false, blend_mode: composite, ..Default::default() };
    let rect = match tiny_skia::Rect::from_xywh(dx.min(dx + dw), dy.min(dy + dh), dw.abs(), dh.abs()) {
        Some(r) => r,
        None => return,
    };
    let path = PathBuilder::from_rect(rect).transform(t);
    let data = Arc::make_mut(img);
    let mask = clip_mask(&data.pixmap, clip);
    if let Some(path) = path {
        data.pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), mask.as_ref());
    }
}

/// Source-over copy of `src` (premultiplied) into the destination rect
/// (x0, y0, w, h); destination pixel (x, y) reads source pixel (x - ox, y - oy).
/// Source pixels outside the image are transparent (SpreadMode::Pad never
/// reaches them: the rect is inside the source's own area for these draws).
fn blit_over(img: &mut Arc<ImageData>, src: &ImageData, x0: i64, y0: i64, w: i64, h: i64, ox: i64, oy: i64) {
    let data = Arc::make_mut(img);
    let (dw, dh) = (data.pixmap.width() as i64, data.pixmap.height() as i64);
    let (sw, sh) = (src.pixmap.width() as i64, src.pixmap.height() as i64);
    // Clip to the destination, and to the source's pixels.
    let xa = x0.max(0).max(ox);
    let ya = y0.max(0).max(oy);
    let xb = (x0 + w).min(dw).min(ox + sw);
    let yb = (y0 + h).min(dh).min(oy + sh);
    if xa >= xb || ya >= yb {
        return;
    }
    let sp = src.pixmap.data();
    let dp = data.pixmap.data_mut();
    let n = (xb - xa) as usize;
    for y in ya..yb {
        let di = ((y * dw + xa) * 4) as usize;
        let si = (((y - oy) * sw + (xa - ox)) * 4) as usize;
        let srow = &sp[si..si + n * 4];
        let drow = &mut dp[di..di + n * 4];
        for (d, s) in drow.chunks_exact_mut(4).zip(srow.chunks_exact(4)) {
            let a = s[3] as u32;
            if a == 255 {
                d.copy_from_slice(s);
            } else if a != 0 {
                let ia = 255 - a;
                for k in 0..4 {
                    // s + d·(1−a), rounded like the raster pipeline's lowp.
                    let v = d[k] as u32 * ia + 128;
                    d[k] = (s[k] as u32 + ((v + (v >> 8)) >> 8)).min(255) as u8;
                }
            }
        }
    }
}

/// Unpremultiplied RGBA bytes of a region (out-of-bounds pixels are transparent).
pub fn get_image_data(img: &ImageData, x: i32, y: i32, w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    let pw = img.pixmap.width() as i32;
    let ph = img.pixmap.height() as i32;
    let src = img.pixmap.data();
    for row in 0..h as i32 {
        let sy = y + row;
        if sy < 0 || sy >= ph {
            continue;
        }
        for col in 0..w as i32 {
            let sx = x + col;
            if sx < 0 || sx >= pw {
                continue;
            }
            let si = ((sy * pw + sx) * 4) as usize;
            let di = ((row * w as i32 + col) * 4) as usize;
            let a = src[si + 3];
            if a == 0 {
                continue;
            }
            let un = |c: u8| ((c as u32 * 255 + a as u32 / 2) / a as u32).min(255) as u8;
            out[di] = un(src[si]);
            out[di + 1] = un(src[si + 1]);
            out[di + 2] = un(src[si + 2]);
            out[di + 3] = a;
        }
    }
    out
}

/// Write unpremultiplied RGBA bytes into the canvas (no blending, like putImageData).
pub fn put_image_data(img: &mut Arc<ImageData>, bytes: &[u8], w: u32, h: u32, x: i32, y: i32) {
    let data = Arc::make_mut(img);
    let pw = data.pixmap.width() as i32;
    let ph = data.pixmap.height() as i32;
    let dst = data.pixmap.data_mut();
    for row in 0..h as i32 {
        let dy = y + row;
        if dy < 0 || dy >= ph {
            continue;
        }
        for col in 0..w as i32 {
            let dx = x + col;
            if dx < 0 || dx >= pw {
                continue;
            }
            let si = ((row * w as i32 + col) * 4) as usize;
            if si + 3 >= bytes.len() {
                return;
            }
            let di = ((dy * pw + dx) * 4) as usize;
            let a = bytes[si + 3] as u32;
            let pm = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            dst[di] = pm(bytes[si]);
            dst[di + 1] = pm(bytes[si + 1]);
            dst[di + 2] = pm(bytes[si + 2]);
            dst[di + 3] = a as u8;
        }
    }
}

pub fn to_png(img: &ImageData) -> Option<Vec<u8>> {
    img.pixmap.encode_png().ok()
}

pub fn composite_mode(name: &str) -> tiny_skia::BlendMode {
    use tiny_skia::BlendMode::*;
    match name {
        "source-over" => SourceOver,
        "source-in" => SourceIn,
        "source-out" => SourceOut,
        "source-atop" => SourceAtop,
        "destination-over" => DestinationOver,
        "destination-in" => DestinationIn,
        "destination-out" => DestinationOut,
        "destination-atop" => DestinationAtop,
        "lighter" => Plus,
        "copy" => Source,
        "xor" => Xor,
        "multiply" => Multiply,
        "screen" => Screen,
        "overlay" => Overlay,
        "darken" => Darken,
        "lighten" => Lighten,
        "color-dodge" => ColorDodge,
        "color-burn" => ColorBurn,
        "hard-light" => HardLight,
        "soft-light" => SoftLight,
        "difference" => Difference,
        "exclusion" => Exclusion,
        "hue" => Hue,
        "saturation" => Saturation,
        "color" => Color,
        "luminosity" => Luminosity,
        "clear" => Clear,
        _ => SourceOver,
    }
}

/// Shape and draw text. `families` etc. describe the canvas font; `align`:
/// 0 start/left, 1 center, 2 end/right; `baseline`: 0 alphabetic, 1 top,
/// 2 middle, 3 bottom, 4 hanging, 5 ideographic.
#[allow(clippy::too_many_arguments)]
pub fn draw_text(
    img: &mut Arc<ImageData>,
    fonts: &mut crate::text::FontContext,
    text: &str,
    x: f32,
    y: f32,
    font: (&std::rc::Rc<Vec<String>>, u16, crate::css::style::FontStyle, f32),
    align: u8,
    baseline: u8,
    max_width: Option<f32>,
    stroke_w: Option<f32>,
    p: &DrawParams,
) {
    let (fam, weight, style, size) = font;
    let shaped = fonts.shape(text, fam, weight, style, size, 0.0);
    let primary = fonts.primary(fam, weight, style);
    let sc = primary.scale(size);
    let mut width = shaped.width;
    let mut sx = 1.0;
    if let Some(mw) = max_width {
        if mw > 0.0 && width > mw {
            sx = mw / width;
            width = mw;
        }
    }
    let ox = match align {
        1 => x - width / 2.0,
        2 => x - width,
        _ => x,
    };
    let (asc, desc) = (primary.ascent * sc, -primary.descent.abs() * sc);
    let oy = match baseline {
        1 | 4 => y + asc,
        2 => y + (asc + desc) / 2.0,
        3 | 5 => y + desc,
        _ => y,
    };
    let mut pb = PathBuilder::new();
    let mut run_x = 0.0;
    for run in &shaped.runs {
        for g in &run.glyphs {
            if let Some(gp) = crate::paint::raster::glyph_path(&run.font, g.id, run.size) {
                let t = Transform::from_row(sx, 0.0, 0.0, 1.0, ox + (run_x + g.x) * sx, oy + g.y);
                if let Some(gp) = gp.transform(t) {
                    pb.push_path(&gp);
                }
            }
        }
        run_x += run.width;
    }
    let path = match pb.finish() {
        Some(p) => p,
        None => return,
    };
    let path = match path.transform(p.t) {
        Some(p) => p,
        None => return,
    };
    let data = Arc::make_mut(img);
    let mask = clip_mask(&data.pixmap, &p.clip);
    let shader = match make_shader(p) {
        Some(s) => s,
        None => return,
    };
    let paint = Paint { shader, anti_alias: true, blend_mode: p.composite, ..Default::default() };
    match stroke_w {
        Some(w) => {
            let st = Stroke { width: w.max(0.01), ..Default::default() };
            data.pixmap.stroke_path(&path, &paint, &st, Transform::identity(), mask.as_ref());
        }
        None => data.pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), mask.as_ref()),
    }
}

pub fn measure_text(fonts: &mut crate::text::FontContext, text: &str, font: (&std::rc::Rc<Vec<String>>, u16, crate::css::style::FontStyle, f32)) -> (f32, f32, f32) {
    let (fam, weight, style, size) = font;
    let shaped = fonts.shape(text, fam, weight, style, size, 0.0);
    let primary = fonts.primary(fam, weight, style);
    let sc = primary.scale(size);
    (shaped.width, primary.ascent * sc, primary.descent.abs() * sc)
}
