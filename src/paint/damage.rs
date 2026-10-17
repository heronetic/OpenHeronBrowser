// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Damage between two display lists: the document-space rectangle whose
//! pixels can differ when the page cache (rasterized from `old`) is brought
//! up to date with `new` at the same scroll and scale. The changed run of
//! items between the common prefix and the common suffix is bounded through
//! the transform stack; anything whose extent can't be bounded cheaply
//! (filters, masks, 3D warps, sticky offsets, blend groups, or a change in
//! the push/pop structure seen by the suffix) asks for a full repaint.

use std::sync::Arc;

use tiny_skia::Transform;

use super::{DisplayList, GradSpec, Item, Rect};

/// What the cache needs: `Full` repaint, nothing, or a document-space rect.
#[derive(Debug, PartialEq)]
pub enum Damage {
    Full,
    None,
    /// Pixels to update, and how far (document px) around them content must
    /// be rendered for filters (blur, drop-shadow) to come out the same.
    Rect(Rect, f32),
}

/// How far a filter chain spreads content (document px).
fn filter_extent(f: &[super::Filter]) -> f32 {
    f.iter()
        .map(|f| match f {
            super::Filter::Blur(b) => 3.0 * b,
            super::Filter::DropShadow { x, y, blur, .. } => x.abs() + y.abs() + 3.0 * blur,
            super::Filter::ColorMatrix(_) => 0.0,
        })
        .sum::<f32>()
        .max(0.0)
}

fn grad_eq(a: &Arc<GradSpec>, b: &Arc<GradSpec>) -> bool {
    Arc::ptr_eq(a, b) || (a.kind == b.kind && a.start == b.start && a.end == b.end && a.radius == b.radius && a.stops == b.stops && a.repeating == b.repeating && a.angle == b.angle)
}

/// Whether two items draw identically (same state for pushes).
pub fn same(a: &Item, b: &Item) -> bool {
    use Item::*;
    match (a, b) {
        (Rect { r, color }, Rect { r: r2, color: c2 }) => r == r2 && color == c2,
        (RRect { r, radii, color }, RRect { r: r2, radii: q2, color: c2 }) => r == r2 && radii == q2 && color == c2,
        (Border { r, widths, colors, styles, radii }, Border { r: r2, widths: w2, colors: c2, styles: s2, radii: q2 }) => r == r2 && widths == w2 && colors == c2 && styles == s2 && radii == q2,
        (Text { x, y, run, color }, Text { x: x2, y: y2, run: run2, color: c2 }) => {
            x == x2 && y == y2 && color == c2 && (Arc::ptr_eq(run, run2) || (Arc::ptr_eq(&run.font, &run2.font) && run.size == run2.size && run.glyphs == run2.glyphs))
        }
        (Image { r, img, radii, pixelated }, Image { r: r2, img: i2, radii: q2, pixelated: p2 }) => r == r2 && Arc::ptr_eq(img, i2) && radii == q2 && pixelated == p2,
        (Pattern { area, tile, img, repeat_x, repeat_y, fixed }, Pattern { area: a2, tile: t2, img: i2, repeat_x: x2, repeat_y: y2, fixed: f2 }) => {
            area == a2 && tile == t2 && Arc::ptr_eq(img, i2) && repeat_x == x2 && repeat_y == y2 && fixed == f2
        }
        (Gradient { r, grad, radii }, Gradient { r: r2, grad: g2, radii: q2 }) => r == r2 && radii == q2 && grad_eq(grad, g2),
        (MaskedColor { area, tile, img, color }, MaskedColor { area: a2, tile: t2, img: i2, color: c2 }) => area == a2 && tile == t2 && Arc::ptr_eq(img, i2) && color == c2,
        (GradientTiles { area, tile, grad }, GradientTiles { area: a2, tile: t2, grad: g2 }) => area == a2 && tile == t2 && grad_eq(grad, g2),
        (Shadow { r, radii, blur, spread, color, inset, hole }, Shadow { r: r2, radii: q2, blur: b2, spread: s2, color: c2, inset: i2, hole: h2 }) => {
            r == r2 && radii == q2 && blur == b2 && spread == s2 && color == c2 && inset == i2 && hole == h2
        }
        (InsetShadow { r, radii, hole, hole_radii, blur, color }, InsetShadow { r: r2, radii: q2, hole: h2, hole_radii: hq2, blur: b2, color: c2 }) => {
            r == r2 && radii == q2 && hole == h2 && hole_radii == hq2 && blur == b2 && color == c2
        }
        (Line { x0, y0, x1, y1, width, color, style }, Line { x0: a0, y0: b0, x1: a1, y1: b1, width: w2, color: c2, style: s2 }) => {
            x0 == a0 && y0 == b0 && x1 == a1 && y1 == b1 && width == w2 && color == c2 && style == s2
        }
        (PushClip { r, radii }, PushClip { r: r2, radii: q2 }) => r == r2 && radii == q2,
        (PushClipPath(p), PushClipPath(p2)) => Arc::ptr_eq(p, p2) || p.as_ref() == p2.as_ref(),
        (PushMask(m), PushMask(m2)) => Arc::ptr_eq(m, m2) || (m.len() == m2.len() && m.iter().zip(m2.iter()).all(|(a, b)| same(a, b))),
        (PushOpacity(o), PushOpacity(o2)) => o == o2,
        (PushBlend(m), PushBlend(m2)) => m == m2,
        (PushGroup { opacity, bounds }, PushGroup { opacity: o2, bounds: b2 }) => opacity == o2 && bounds == b2,
        (PushTransform(m), PushTransform(m2)) => m == m2,
        (PushWarp { h, bounds }, PushWarp { h: h2, bounds: b2 }) => h == h2 && bounds == b2,
        (PushFilter(f), PushFilter(f2)) => Arc::ptr_eq(f, f2) || format!("{f:?}") == format!("{f2:?}"),
        (PushSticky(s), PushSticky(s2)) => Arc::ptr_eq(s, s2) || (s.applied == s2.applied && s.viewport == s2.viewport && format!("{:?}", s.geom) == format!("{:?}", s2.geom)),
        (PopClip, PopClip) | (PopMask, PopMask) | (PopOpacity, PopOpacity) | (PopBlend, PopBlend) | (PopGroup, PopGroup) | (PopTransform, PopTransform) | (PopWarp, PopWarp) | (PopFilter, PopFilter) | (BeginFixed, BeginFixed) | (EndFixed, EndFixed) | (PopSticky, PopSticky) => true,
        _ => false,
    }
}

/// One open push while walking a list.
#[derive(Clone, Copy)]
struct Open {
    /// Index of the push item.
    at: usize,
    /// Transform in effect inside it (document space → document space).
    t: Transform,
    /// Inside content whose extent can't be bounded (3D warp).
    opaque: bool,
    fixed: bool,
    /// Spread of enclosing filters (document px).
    pad: f32,
}

pub fn is_push_item(it: &Item) -> bool {
    is_push(it)
}

pub fn is_pop_item(it: &Item) -> bool {
    is_pop(it)
}

fn is_push(it: &Item) -> bool {
    matches!(it, Item::PushClip { .. } | Item::PushClipPath(_) | Item::PushMask(_) | Item::PushOpacity(_) | Item::PushBlend(_) | Item::PushGroup { .. } | Item::PushTransform(_) | Item::PushWarp { .. } | Item::PushFilter(_) | Item::BeginFixed | Item::PushSticky(_))
}

fn is_pop(it: &Item) -> bool {
    matches!(it, Item::PopClip | Item::PopMask | Item::PopOpacity | Item::PopBlend | Item::PopGroup | Item::PopTransform | Item::PopWarp | Item::PopFilter | Item::EndFixed | Item::PopSticky)
}

fn map_rect(t: &Transform, r: Rect) -> Rect {
    let pts = [(r.x, r.y), (r.right(), r.y), (r.x, r.bottom()), (r.right(), r.bottom())];
    let mut mn = (f32::INFINITY, f32::INFINITY);
    let mut mx = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for (x, y) in pts {
        let px = x * t.sx + y * t.kx + t.tx;
        let py = x * t.ky + y * t.sy + t.ty;
        mn = (mn.0.min(px), mn.1.min(py));
        mx = (mx.0.max(px), mx.1.max(py));
    }
    Rect::new(mn.0, mn.1, mx.0 - mn.0, mx.1 - mn.1)
}

/// Walk `items[..end]`, accumulating the document-space bounds of drawing
/// items at indices `>= start`. Returns the open-push stack at `end` and the
/// bounds, or None when something in the range can't be bounded.
fn walk(items: &[Item], start: usize, end: usize, scroll: (f32, f32), skip_fixed: bool) -> Option<(Vec<Open>, Option<Rect>)> {
    let mut stack: Vec<Open> = Vec::new();
    let mut acc: Option<Rect> = None;
    for (i, it) in items[..end].iter().enumerate() {
        let top = stack.last().copied();
        let t = top.map(|o| o.t).unwrap_or(Transform::identity());
        let opaque = top.map(|o| o.opaque).unwrap_or(false);
        let fixed = top.map(|o| o.fixed).unwrap_or(false);
        let pad = top.map(|o| o.pad).unwrap_or(0.0);
        let in_range = i >= start;
        if is_push(it) {
            let mut npad = pad;
            let (nt, nop, nfixed) = match it {
                Item::PushTransform(m) => (t.pre_concat(Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5])), opaque, fixed),
                // Fixed content is in viewport coordinates.
                Item::BeginFixed => (Transform::from_translate(scroll.0, scroll.1), opaque, true),
                // Sticky content is offset by its stuck position at this scroll.
                Item::PushSticky(st) => {
                    let (dx, dy) = st.delta(scroll);
                    (t.pre_concat(Transform::from_translate(dx, dy)), opaque, fixed)
                }
                Item::PushFilter(f) => {
                    let sc = t.sx.abs().max(t.sy.abs()).max(t.kx.abs()).max(t.ky.abs()).max(1.0);
                    npad += filter_extent(f) * sc;
                    (t, opaque, fixed)
                }
                Item::PushWarp { .. } => (t, true, fixed),
                _ => (t, opaque, fixed),
            };
            stack.push(Open { at: i, t: nt, opaque: nop, fixed: nfixed, pad: npad });
            continue;
        }
        if is_pop(it) {
            stack.pop();
            continue;
        }
        if !in_range || (fixed && skip_fixed) {
            continue;
        }
        if opaque {
            return None;
        }
        let b = it.bounds()?;
        let r = map_rect(&t, b).inflate(pad);
        if !(r.w.is_finite() && r.h.is_finite()) {
            return None;
        }
        acc = Some(match acc {
            Some(a) => a.union(&r),
            None => r,
        });
    }
    Some((stack, acc))
}

/// Damage for updating a cache drawn from `old` to `new` (same scroll/scale).
/// `skip_fixed`: the cache holds no fixed content (it is drawn separately).
pub fn damage(old: &DisplayList, new: &DisplayList, scroll: (f32, f32), skip_fixed: bool) -> Damage {
    if old.bg != new.bg {
        return Damage::Full;
    }
    let (a, b) = (&old.items, &new.items);
    let n = a.len().min(b.len());
    let mut p = 0;
    while p < n && same(&a[p], &b[p]) {
        p += 1;
    }
    if p == a.len() && p == b.len() {
        return Damage::None;
    }
    let mut s = 0;
    while s < n - p && same(&a[a.len() - 1 - s], &b[b.len() - 1 - s]) {
        s += 1;
    }
    let (mut ea, mut eb) = (a.len() - s, b.len() - s);
    let why = std::env::var_os("OHB_DAMAGE_WHY").is_some();
    if why {
        eprintln!("[damage-why] prefix {p} suffix {s}: old {}..{} new {}..{}", p, ea, p, eb);
        for (i, it) in a[p..ea.min(p + 6)].iter().enumerate() {
            eprintln!("   old[{}] {:?}", p + i, it);
        }
        for (i, it) in b[p..eb.min(p + 6)].iter().enumerate() {
            eprintln!("   new[{}] {:?}", p + i, it);
        }
    }
    let mut tries = 0;
    let (ra, rb) = loop {
        let Some((sa, ra)) = walk(a, p, ea, scroll, skip_fixed) else {
            if why { eprintln!("   -> unboundable in old"); }
            return Damage::Full;
        };
        let Some((sb, rb)) = walk(b, p, eb, scroll, skip_fixed) else {
            if why { eprintln!("   -> unboundable in new"); }
            return Damage::Full;
        };
        // The suffix must see the same open pushes in both lists. When it
        // doesn't (an opacity or transform changed, its content didn't),
        // the change reaches to where those pushes close: extend the range
        // through the common suffix to there (the suffix is identical in
        // both lists, so both ends move together).
        let k = sa.iter().zip(sb.iter()).position(|(x, y)| !same(&a[x.at], &b[y.at])).unwrap_or(sa.len().min(sb.len()));
        if sa.len() == sb.len() && k == sa.len() {
            break (ra, rb);
        }
        tries += 1;
        if sa.len() != sb.len() || tries > 8 {
            if why {
                eprintln!("   -> open pushes differ: {:?} vs {:?}", sa.iter().map(|o| &a[o.at]).collect::<Vec<_>>(), sb.iter().map(|o| &b[o.at]).collect::<Vec<_>>());
            }
            return Damage::Full;
        }
        let mut depth = sa.len();
        let mut ext = None;
        for (j, it) in a[ea..].iter().enumerate() {
            if is_push(it) {
                depth += 1;
            } else if is_pop(it) {
                depth -= 1;
                if depth <= k {
                    ext = Some(j + 1);
                    break;
                }
            }
        }
        let Some(m) = ext else {
            if why { eprintln!("   -> differing push never closes"); }
            return Damage::Full;
        };
        ea += m;
        eb += m;
    };
    let margin = filter_margin(old).max(filter_margin(new));
    match (ra, rb) {
        (None, None) => Damage::None,
        (Some(r), None) | (None, Some(r)) => Damage::Rect(r.inflate(2.0), margin),
        (Some(r), Some(q)) => Damage::Rect(r.union(&q).inflate(2.0), margin),
    }
}

/// How far around a region content must be rendered for the list's filters
/// (blur, drop-shadow) to come out as in a full rendering (document px).
pub fn filter_margin(list: &DisplayList) -> f32 {
    let m = list.items.iter().filter_map(|it| match it { Item::PushFilter(f) | Item::Backdrop { filters: f, .. } => Some(filter_extent(f)), _ => None }).fold(0.0f32, f32::max);
    // Nested filters spread further; a generous margin keeps blurs exact.
    if m > 0.0 { m * 2.0 + 4.0 } else { 0.0 }
}

/// Whether an item draws (or clips with) curved outlines, which the
/// rasterizer approximates differently when they're cut by a surface edge.
fn curved(it: &Item) -> bool {
    match it {
        Item::RRect { .. } | Item::Shadow { .. } | Item::InsetShadow { .. } | Item::PushClipPath(_) => true,
        Item::Border { radii, styles, .. } => radii.any() || styles.iter().any(|s| matches!(s, crate::css::style::BorderStyle::Dotted | crate::css::style::BorderStyle::Dashed)),
        Item::Gradient { radii, .. } => radii.any(),
        Item::Image { radii, .. } => radii.map(|r| r.any()).unwrap_or(false),
        Item::PushClip { radii, .. } => radii.map(|r| r.any()).unwrap_or(false),
        Item::Line { .. } => true,
        _ => false,
    }
}

/// `r` grown to contain every curved item (document space) that overlaps
/// it, so a partial repaint never cuts a curve the full repaint doesn't.
pub fn grow_for_curves(list: &DisplayList, scroll: (f32, f32), skip_fixed: bool, r: Rect) -> Rect {
    let mut out = r;
    let mut stack: Vec<(Transform, bool)> = Vec::new();
    for it in &list.items {
        let (t, fixed) = stack.last().copied().unwrap_or((Transform::identity(), false));
        if is_push(it) {
            let nt = match it {
                Item::PushTransform(m) => t.pre_concat(Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5])),
                Item::BeginFixed => Transform::from_translate(scroll.0, scroll.1),
                Item::PushSticky(st) => {
                    let (dx, dy) = st.delta(scroll);
                    t.pre_concat(Transform::from_translate(dx, dy))
                }
                _ => t,
            };
            let nfixed = fixed || matches!(it, Item::BeginFixed);
            // A clip with a curved outline also counts.
            if curved(it) && !(nfixed && skip_fixed) {
                let b = match it {
                    Item::PushClip { r, .. } => Some(*r),
                    Item::PushClipPath(p) => {
                        let b = p.bounds();
                        Some(Rect::new(b.left(), b.top(), b.width(), b.height()))
                    }
                    _ => None,
                };
                if let Some(b) = b {
                    let m = map_rect(&t, b);
                    if m.intersects(&r) {
                        out = out.union(&m);
                    }
                }
            }
            stack.push((nt, nfixed));
            continue;
        }
        if is_pop(it) {
            stack.pop();
            continue;
        }
        if fixed && skip_fixed {
            continue;
        }
        let rotated = t.kx != 0.0 || t.ky != 0.0;
        if !(curved(it) || rotated) {
            continue;
        }
        if let Some(b) = it.bounds() {
            let m = map_rect(&t, b);
            if m.intersects(&r) {
                out = out.union(&m);
            }
        }
    }
    out
}

/// Bring `pm` (the viewport rendered at `scroll`/`scale`, device px) up to
/// date with `list` inside the damaged document rect `r`: content is
/// rendered `margin` px further out (filters reach that far) into a small
/// pixmap and the damaged part copied in. Returns the updated fraction of
/// the pixmap, or None (nothing done) when the region exceeds `max_frac`.
pub fn patch(raster: &mut super::raster::Rasterizer, list: &DisplayList, pm: &mut tiny_skia::Pixmap, scroll: (f32, f32), scale: f32, r: Rect, margin: f32, max_frac: f32, skip_fixed: bool) -> Option<f32> {
    let (w, h) = (pm.width(), pm.height());
    let dev = |r: Rect| -> (u32, u32, u32, u32) {
        let x0 = (((r.x - scroll.0) * scale).floor().max(0.0) as u32).min(w);
        let y0 = (((r.y - scroll.1) * scale).floor().max(0.0) as u32).min(h);
        let x1 = (((r.right() - scroll.0) * scale).ceil().max(0.0) as u32).min(w);
        let y1 = (((r.bottom() - scroll.1) * scale).ceil().max(0.0) as u32).min(h);
        (x0, y0, x1, y1)
    };
    let (x0, y0, x1, y1) = dev(r);
    if x1 <= x0 || y1 <= y0 {
        return Some(0.0);
    }
    let frac = ((x1 - x0) as f32 * (y1 - y0) as f32) / (w as f32 * h as f32);
    let (ox0, oy0, ox1, oy1) = dev(grow_for_curves(list, scroll, skip_fixed, r.inflate(margin)));
    let outer_frac = ((ox1 - ox0) as f32 * (oy1 - oy0) as f32) / (w as f32 * h as f32);
    if frac > max_frac || outer_frac > 0.9 {
        return None;
    }
    let mut small = tiny_skia::Pixmap::new(ox1 - ox0, oy1 - oy0)?;
    let bg = if list.bg.a == 0 { crate::css::values::Color::WHITE } else { list.bg };
    small.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, 255));
    raster.render_at(list, &mut small, scroll, scale, (-(ox0 as f32), -(oy0 as f32)), (0.0, (oy1 - oy0) as f32));
    let inner = if (ox0, oy0, ox1, oy1) == (x0, y0, x1, y1) { small } else { small.clone_rect(tiny_skia::IntRect::from_xywh((x0 - ox0) as i32, (y0 - oy0) as i32, x1 - x0, y1 - y0)?)? };
    let paint = tiny_skia::PixmapPaint { blend_mode: tiny_skia::BlendMode::Source, ..Default::default() };
    pm.draw_pixmap(x0 as i32, y0 as i32, inner.as_ref(), &paint, tiny_skia::Transform::identity(), None);
    Some(frac)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::values::Color;

    fn rect(x: f32, y: f32, w: f32, h: f32, c: u8) -> Item {
        Item::Rect { r: Rect::new(x, y, w, h), color: Color { r: c, g: 0, b: 0, a: 255 } }
    }

    fn list(items: Vec<Item>) -> DisplayList {
        DisplayList { items, fixed: 0, bg: Color::WHITE }
    }

    #[test]
    fn unchanged_is_none() {
        let a = list(vec![rect(0.0, 0.0, 10.0, 10.0, 1)]);
        let b = list(vec![rect(0.0, 0.0, 10.0, 10.0, 1)]);
        assert_eq!(damage(&a, &b, (0.0, 0.0), false), Damage::None);
    }

    #[test]
    fn changed_item_bounds() {
        let a = list(vec![rect(0.0, 0.0, 10.0, 10.0, 1), rect(50.0, 50.0, 10.0, 10.0, 1), rect(0.0, 100.0, 5.0, 5.0, 1)]);
        let b = list(vec![rect(0.0, 0.0, 10.0, 10.0, 1), rect(50.0, 50.0, 10.0, 10.0, 2), rect(0.0, 100.0, 5.0, 5.0, 1)]);
        assert_eq!(damage(&a, &b, (0.0, 0.0), false), Damage::Rect(Rect::new(48.0, 48.0, 14.0, 14.0), 0.0));
    }

    #[test]
    fn transformed_item() {
        let t = Item::PushTransform([1.0, 0.0, 0.0, 1.0, 100.0, 0.0]);
        let a = list(vec![t.clone(), rect(0.0, 0.0, 10.0, 10.0, 1), Item::PopTransform]);
        let b = list(vec![t, rect(0.0, 0.0, 10.0, 10.0, 2), Item::PopTransform]);
        assert_eq!(damage(&a, &b, (0.0, 0.0), false), Damage::Rect(Rect::new(98.0, -2.0, 14.0, 14.0), 0.0));
    }

    #[test]
    fn changed_push_damages_its_content() {
        let a = list(vec![rect(0.0, 0.0, 1.0, 1.0, 1), Item::PushOpacity(0.5), rect(10.0, 10.0, 10.0, 10.0, 1), Item::PopOpacity, rect(90.0, 90.0, 1.0, 1.0, 1)]);
        let b = list(vec![rect(0.0, 0.0, 1.0, 1.0, 1), Item::PushOpacity(0.7), rect(10.0, 10.0, 10.0, 10.0, 1), Item::PopOpacity, rect(90.0, 90.0, 1.0, 1.0, 1)]);
        assert_eq!(damage(&a, &b, (0.0, 0.0), false), Damage::Rect(Rect::new(8.0, 8.0, 14.0, 14.0), 0.0));
        let t1 = list(vec![Item::PushTransform([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]), rect(0.0, 0.0, 10.0, 10.0, 1), Item::PopTransform]);
        let t2 = list(vec![Item::PushTransform([1.0, 0.0, 0.0, 1.0, 50.0, 0.0]), rect(0.0, 0.0, 10.0, 10.0, 1), Item::PopTransform]);
        assert_eq!(damage(&t1, &t2, (0.0, 0.0), false), Damage::Rect(Rect::new(-2.0, -2.0, 64.0, 14.0), 0.0));
    }

    #[test]
    fn inside_filter_spreads() {
        let f = Item::PushFilter(Arc::new(vec![super::super::Filter::Blur(4.0)]));
        let a = list(vec![f.clone(), rect(0.0, 0.0, 10.0, 10.0, 1), Item::PopFilter]);
        let b = list(vec![f, rect(0.0, 0.0, 10.0, 10.0, 2), Item::PopFilter]);
        assert_eq!(damage(&a, &b, (0.0, 0.0), false), Damage::Rect(Rect::new(-14.0, -14.0, 38.0, 38.0), 28.0));
    }

    #[test]
    fn fixed_uses_viewport_coords() {
        let a = list(vec![Item::BeginFixed, rect(0.0, 0.0, 10.0, 10.0, 1), Item::EndFixed]);
        let b = list(vec![Item::BeginFixed, rect(0.0, 0.0, 10.0, 10.0, 2), Item::EndFixed]);
        assert_eq!(damage(&a, &b, (0.0, 500.0), false), Damage::Rect(Rect::new(-2.0, 498.0, 14.0, 14.0), 0.0));
        assert_eq!(damage(&a, &b, (0.0, 500.0), true), Damage::None);
    }
}

/// What scrolling from `old` to `new` changes in sticky content, beyond
/// what shifting the cached pixels by the scroll delta accounts for.
pub enum StickyDamage {
    None,
    /// Repaint this document rect (both the old and the new positions).
    Rect(Rect),
    Full,
}

pub fn sticky_damage(list: &DisplayList, old: (f32, f32), new: (f32, f32)) -> StickyDamage {
    let mut acc: Option<Rect> = None;
    // Per open sticky: offset at the old and at the new scroll.
    let mut stack: Vec<((f32, f32), (f32, f32))> = Vec::new();
    for it in &list.items {
        match it {
            Item::PushSticky(st) => {
                let (a, b) = (st.delta(old), st.delta(new));
                let (pa, pb) = stack.last().map(|s| (s.0, s.1)).unwrap_or(((0.0, 0.0), (0.0, 0.0)));
                stack.push(((pa.0 + a.0, pa.1 + a.1), (pb.0 + b.0, pb.1 + b.1)));
            }
            Item::PopSticky => {
                stack.pop();
            }
            _ if stack.is_empty() => {}
            Item::PushTransform(_) | Item::PushWarp { .. } | Item::BeginFixed => return StickyDamage::Full,
            _ if it.draws() => {
                let (a, b) = *stack.last().unwrap();
                if (a.0 - b.0).abs() < 0.01 && (a.1 - b.1).abs() < 0.01 {
                    continue;
                }
                let Some(r) = it.bounds() else { return StickyDamage::Full };
                let r1 = Rect::new(r.x + a.0, r.y + a.1, r.w, r.h);
                let r2 = Rect::new(r.x + b.0, r.y + b.1, r.w, r.h);
                let u = r1.union(&r2);
                acc = Some(acc.map_or(u, |x| x.union(&u)));
            }
            _ => {}
        }
    }
    match acc {
        Some(r) => StickyDamage::Rect(r),
        None => StickyDamage::None,
    }
}
