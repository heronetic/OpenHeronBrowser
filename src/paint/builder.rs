// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Layout tree → display list (stacking order, clipping, scrolling) + hit regions.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use super::*;
use crate::css::mat4::{self, Mat4};
use crate::css::style::*;
use crate::css::values::{Color, Len};
use crate::css::Styles;
use crate::dom::{Document, NodeId};
use crate::layout::inline::FragKind;
use crate::layout::{BoxId, BoxKind, LayoutTree, ReplacedKind, ResourceView};
use crate::text::FontContext;

pub struct PaintInput<'a> {
    pub tree: &'a LayoutTree,
    pub doc: &'a Document,
    pub styles: &'a Styles,
    pub res: &'a dyn ResourceView,
    pub scroll: &'a HashMap<NodeId, (f32, f32)>,
    pub focus: Option<NodeId>,
    pub caret: Option<usize>,
    pub caret_visible: bool,
    pub fonts: &'a mut FontContext,
    pub find: &'a [(NodeId, usize, usize)],
    pub selection: Option<&'a crate::layout::selection::SelSpan>,
    /// The caret of a focused editing host (contenteditable).
    pub edit_caret: Option<EditCaret>,
    /// The selected byte range in the focused text field (anchor, caret order-free).
    pub field_sel: Option<(usize, usize)>,
    /// An open `<select>` dropdown, drawn above the page.
    pub select_popup: Option<SelectPopupPaint>,
}

/// An open `<select>`'s option list (document coordinates).
#[derive(Clone, Debug)]
pub struct SelectPopupPaint {
    pub node: NodeId,
    pub rect: Rect,
    pub item_h: f32,
    /// (label, disabled, group heading, selected)
    pub items: Vec<(String, bool, bool, bool)>,
    pub hover: Option<usize>,
    pub first: usize,
}

#[derive(Clone, Copy, Debug)]
pub enum EditCaret {
    /// At a text position.
    At(crate::layout::selection::SelPoint),
    /// In an editing host with no text: at its content start.
    Empty(NodeId),
}

#[derive(Clone)]
struct ClipEntry {
    rect: Rect,
    radii: Option<Radii>,
    owner: BoxId,
    /// Hit-test transform in effect when the clip was pushed (see `hit_xf`).
    xf: Option<[f32; 6]>,
}

/// `[a, b, c, d, e, f]` affine maps: `m · n`.
fn mul2d(m: &[f32; 6], n: &[f32; 6]) -> [f32; 6] {
    [m[0] * n[0] + m[2] * n[1], m[1] * n[0] + m[3] * n[1], m[0] * n[2] + m[2] * n[3], m[1] * n[2] + m[3] * n[3], m[0] * n[4] + m[2] * n[5] + m[4], m[1] * n[4] + m[3] * n[5] + m[5]]
}

/// Bounding box of a rectangle mapped through an affine map.
fn map_rect(m: &[f32; 6], r: Rect) -> Rect {
    let pts = [(r.x, r.y), (r.right(), r.y), (r.x, r.bottom()), (r.right(), r.bottom())];
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (x, y) in pts {
        let (px, py) = (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
        x0 = x0.min(px);
        y0 = y0.min(py);
        x1 = x1.max(px);
        y1 = y1.max(py);
    }
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// Characters set upright in vertical text with `text-orientation: mixed`
/// (Unicode Vertical_Orientation U / Tu, roughly: CJK, kana, Hangul, emoji).
fn upright_char(c: char) -> bool {
    let u = c as u32;
    match u {
        // brackets and dashes that rotate (Tr)
        0x3008..=0x3011 | 0x3014..=0x301F | 0x3030 | 0x30A0 | 0x30FC | 0xFF08 | 0xFF09 | 0xFF0D | 0xFF1C..=0xFF1E | 0xFF3B | 0xFF3D | 0xFF3F | 0xFF5B..=0xFF60 | 0xFE59..=0xFE5E => false,
        0x1100..=0x11FF | 0x2E80..=0x2FFF | 0x3000..=0x303F | 0x3040..=0x30FF | 0x3100..=0x312F | 0x3130..=0x318F | 0x3190..=0x31FF | 0x3200..=0x33FF => true,
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xA960..=0xA97F | 0xAC00..=0xD7FF | 0xF900..=0xFAFF | 0xFE10..=0xFE1F | 0xFE30..=0xFE4F => true,
        0xFF01..=0xFF60 | 0xFFE0..=0xFFE6 | 0x1F000..=0x1FAFF | 0x20000..=0x3FFFF => true,
        0x00A7 | 0x00A9 | 0x00AE | 0x00B1 | 0x00BC..=0x00BE | 0x00D7 | 0x00F7 => true,
        _ => false,
    }
}

/// An open 3D rendering context level: a `transform-style: preserve-3d`
/// element whose children are planes of the enclosing context.
struct Ctx3d {
    owner: BoxId,
    /// The owner's transform in the coordinates the context root paints in.
    m: Mat4,
    /// The perspective the owner applies to its children.
    persp: Mat4,
}

/// One flat element of a 3D rendering context, painted on its own and
/// depth-sorted with the others before being drawn.
struct Plane {
    seq: usize,
    m: Mat4,
    rect: Rect,
    items: Vec<Item>,
}

/// Grouping properties force `transform-style: flat` (css-transforms-2).
fn flattens(s: &ComputedStyle) -> bool {
    s.opacity < 0.999 || s.filter.is_some() || s.clip_shape.is_some() || s.mask.is_some() || s.mix_blend != 0 || s.isolate || s.overflow_x.clips() || s.overflow_y.clips()
}

/// Back-to-front order of the planes of a 3D rendering context: Newell-style
/// pairwise plane tests in projected space, topologically sorted, with the
/// centroid depth (then paint order) breaking ties and cycles.
fn sort_planes(planes: Vec<Plane>) -> Vec<Plane> {
    let n = planes.len();
    if n <= 1 {
        return planes;
    }
    struct Info {
        pts: Option<[[f64; 3]; 4]>,
        plane: Option<([f64; 3], f64)>,
        bbox: (f64, f64, f64, f64),
        depth: f64,
    }
    let info: Vec<Info> = planes
        .iter()
        .map(|p| {
            let r = p.rect;
            let cs = [(r.x, r.y), (r.right(), r.y), (r.right(), r.bottom()), (r.x, r.bottom())];
            let mut pts = [[0.0; 3]; 4];
            let mut ok = true;
            for (i, (x, y)) in cs.iter().enumerate() {
                let q = p.m.apply(*x as f64, *y as f64, 0.0);
                if q[3] <= 1e-9 {
                    ok = false;
                    break;
                }
                pts[i] = [q[0] / q[3], q[1] / q[3], q[2] / q[3]];
            }
            let c = p.m.apply((r.x + r.w / 2.0) as f64, (r.y + r.h / 2.0) as f64, 0.0);
            let depth = if c[3] > 1e-9 { c[2] / c[3] } else { f64::NEG_INFINITY };
            let mut bbox = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            let mut plane = None;
            if ok {
                for q in &pts {
                    bbox = (bbox.0.min(q[0]), bbox.1.min(q[1]), bbox.2.max(q[0]), bbox.3.max(q[1]));
                }
                let (a, b, d) = (pts[0], pts[1], pts[3]);
                let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
                let v = [d[0] - a[0], d[1] - a[1], d[2] - a[2]];
                let mut nrm = [u[1] * v[2] - u[2] * v[1], u[2] * v[0] - u[0] * v[2], u[0] * v[1] - u[1] * v[0]];
                let len = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
                if len > 1e-9 {
                    nrm = [nrm[0] / len, nrm[1] / len, nrm[2] / len];
                    if nrm[2] < 0.0 {
                        nrm = [-nrm[0], -nrm[1], -nrm[2]];
                    }
                    if nrm[2] > 1e-6 {
                        plane = Some((nrm, -(nrm[0] * a[0] + nrm[1] * a[1] + nrm[2] * a[2])));
                    }
                }
            }
            Info { pts: ok.then_some(pts), plane, bbox, depth }
        })
        .collect();
    let key = |i: usize| (info[i].depth, planes[i].seq);
    let less = |a: usize, b: usize| {
        let (ka, kb) = (key(a), key(b));
        ka.0.total_cmp(&kb.0).then(ka.1.cmp(&kb.1)) == std::cmp::Ordering::Less
    };
    let mut order: Vec<usize> = Vec::with_capacity(n);
    if n > 256 {
        order = (0..n).collect();
        order.sort_by(|&a, &b| key(a).0.total_cmp(&key(b).0).then(key(a).1.cmp(&key(b).1)));
    } else {
        // side of plane `a` that `b` lies on: 1 in front, -1 behind, 0 coplanar, 2 straddling/unknown.
        let side = |a: usize, b: usize| -> i32 {
            let (Some((nrm, d)), Some(pts)) = (info[a].plane, info[b].pts) else { return 2 };
            const EPS: f64 = 0.05;
            let (mut front, mut back) = (false, false);
            for q in &pts {
                let s = nrm[0] * q[0] + nrm[1] * q[1] + nrm[2] * q[2] + d;
                if s > EPS {
                    front = true;
                } else if s < -EPS {
                    back = true;
                }
            }
            match (front, back) {
                (true, false) => 1,
                (false, true) => -1,
                (false, false) => 0,
                _ => 2,
            }
        };
        // before[b] lists planes that must be drawn before b.
        let mut indeg = vec![0usize; n];
        let mut after: Vec<Vec<usize>> = vec![Vec::new(); n];
        for a in 0..n {
            for b in a + 1..n {
                let (ba, bb) = (info[a].bbox, info[b].bbox);
                if info[a].pts.is_none() || info[b].pts.is_none() || ba.2 <= bb.0 || bb.2 <= ba.0 || ba.3 <= bb.1 || bb.3 <= ba.1 {
                    continue;
                }
                // first → second (draw first earlier)
                let edge = match side(a, b) {
                    1 => Some((a, b)),
                    -1 => Some((b, a)),
                    0 => Some(if planes[a].seq < planes[b].seq { (a, b) } else { (b, a) }),
                    _ => match side(b, a) {
                        1 => Some((b, a)),
                        -1 => Some((a, b)),
                        0 => Some(if planes[a].seq < planes[b].seq { (a, b) } else { (b, a) }),
                        _ => None,
                    },
                };
                if let Some((x, y)) = edge {
                    after[x].push(y);
                    indeg[y] += 1;
                }
            }
        }
        let mut done = vec![false; n];
        for _ in 0..n {
            // The nearest-to-back plane with nothing left to wait for (or, in a cycle, any).
            let mut pick: Option<usize> = None;
            for i in 0..n {
                if !done[i] && indeg[i] == 0 && pick.map(|p| less(i, p)).unwrap_or(true) {
                    pick = Some(i);
                }
            }
            if pick.is_none() {
                for i in 0..n {
                    if !done[i] && pick.map(|p| less(i, p)).unwrap_or(true) {
                        pick = Some(i);
                    }
                }
            }
            let Some(i) = pick else { break };
            done[i] = true;
            order.push(i);
            for &j in &after[i] {
                indeg[j] = indeg[j].saturating_sub(1);
            }
        }
    }
    let mut slots: Vec<Option<Plane>> = planes.into_iter().map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

struct Deferred {
    /// z-index; top-layer boxes sort above every author z-index.
    z: i64,
    order: usize,
    id: BoxId,
    clips: Rc<Vec<ClipEntry>>,
    off: (f32, f32),
    /// The part of `off` that comes from translate-only transforms.
    toff: (f32, f32),
    /// The enclosing scrollers whose offsets are part of `off`.
    scrolls: Rc<Vec<(BoxId, f32, f32)>>,
}

pub struct Builder<'a, 'b> {
    inp: &'b mut PaintInput<'a>,
    items: Vec<Item>,
    fixed: usize,
    in_fixed: bool,
    /// CSS 2.1 Appendix E phase buffers for the innermost painting root:
    /// floats (step 4) and inline content (step 7); block backgrounds go to `items`.
    phases: Vec<[Vec<Item>; 2]>,
    /// Hit regions recorded in each phase buffer (kept in paint order with the items).
    hit_phases: Vec<[Vec<HitRegion>; 2]>,
    off: (f32, f32),
    /// Part of `off` contributed by translate-only transforms of ancestors.
    /// Unlike scroll offsets, these always apply to positioned descendants
    /// (a transformed box is the containing block of everything inside it).
    toff: (f32, f32),
    clips: Vec<ClipEntry>,
    hits: Vec<HitRegion>,
    skip_bg: Option<BoxId>,
    cb_of: HashMap<BoxId, BoxId>,
    order: usize,
    scroller: Option<usize>,
    /// Scroll offsets applied by enclosing scroll containers: (box, dx, dy).
    scroll_stack: Vec<(BoxId, f32, f32)>,
    /// Positive-z descendants of positioned boxes that don't form stacking
    /// contexts, hoisted to the enclosing stacking context's paint loop.
    hoist: Vec<Vec<Deferred>>,
    /// Open 3D rendering context levels (innermost last).
    ctx3d: Vec<Ctx3d>,
    /// Planes collected per 3D rendering context root being painted.
    planes: Vec<Vec<Plane>>,
    plane_seq: usize,
    /// Accumulated 2D transforms (innermost last) mapping current
    /// coordinates to page coordinates, for hit regions.
    hit_xf: Vec<[f32; 6]>,
    /// Writing modes of the vertical roots being painted (innermost last).
    wm: Vec<u8>,
}

fn color_mul(c: Color, _o: f32) -> Color {
    c
}

pub fn radii_px(s: &ComputedStyle, w: f32, h: f32) -> Radii {
    let mut r = Radii::ZERO;
    for (i, (a, b)) in s.border_radius.iter().enumerate() {
        let (rx, ry) = (a.resolve(w).max(0.0), b.resolve(h).max(0.0));
        // A corner with either radius zero is square.
        if rx > 0.0 && ry > 0.0 {
            r.x[i] = rx;
            r.y[i] = ry;
        }
    }
    // Scale all radii down uniformly when adjacent ones overlap (css-backgrounds-3 §5.5).
    let f = [w / (r.x[0] + r.x[1]).max(0.001), h / (r.y[1] + r.y[2]).max(0.001), w / (r.x[2] + r.x[3]).max(0.001), h / (r.y[3] + r.y[0]).max(0.001)]
        .iter()
        .fold(1.0f32, |a, &b| a.min(b));
    if f < 1.0 {
        r = r.map(|v| v * f);
    }
    r
}

/// Radii of the padding (or content) box edge given the border-box radii.
fn inner_radii(r: Radii, border: &[f32; 4], padding: Option<&[f32; 4]>) -> Radii {
    let p = padding.copied().unwrap_or([0.0; 4]);
    r.inset([border[0] + p[0], border[1] + p[1], border[2] + p[2], border[3] + p[3]])
}

fn has_radius(r: &Radii) -> bool {
    r.any()
}

impl<'a, 'b> Builder<'a, 'b> {
    pub fn new(inp: &'b mut PaintInput<'a>) -> Self {
        let mut cb_of = HashMap::new();
        for (i, b) in inp.tree.boxes.iter().enumerate() {
            for &a in &b.abs_children {
                cb_of.insert(a, i);
            }
        }
        Builder { inp, items: Vec::new(), fixed: 0, phases: Vec::new(), hit_phases: Vec::new(), in_fixed: false, off: (0.0, 0.0), toff: (0.0, 0.0), clips: Vec::new(), hits: Vec::new(), skip_bg: None, cb_of, order: 0, scroller: None, scroll_stack: Vec::new(), hoist: Vec::new(), ctx3d: Vec::new(), planes: Vec::new(), plane_seq: 0, hit_xf: Vec::new(), wm: Vec::new() }
    }

    fn emit(&mut self, it: Item) {
        self.items.push(it);
    }

    /// Emit into the block, float and inline phase buffers (clip push/pop).
    fn emit_all(&mut self, it: Item) {
        if let Some(top) = self.phases.last_mut() {
            top[0].push(it.clone());
            top[1].push(it.clone());
        }
        self.items.push(it);
    }

    /// Run `f` with output redirected to a phase buffer (0 floats, 1 inline content).
    fn in_phase(&mut self, phase: usize, f: impl FnOnce(&mut Self)) {
        let Some(top) = self.phases.len().checked_sub(1) else {
            f(self);
            return;
        };
        let mut buf = std::mem::take(&mut self.phases[top][phase]);
        std::mem::swap(&mut self.items, &mut buf);
        let hp = self.hit_phases.len().checked_sub(1).filter(|&h| h == top);
        let mut hbuf = hp.map(|h| std::mem::take(&mut self.hit_phases[h][phase]));
        if let Some(hb) = hbuf.as_mut() {
            std::mem::swap(&mut self.hits, hb);
        }
        f(self);
        if let (Some(h), Some(mut hb)) = (hp, hbuf) {
            std::mem::swap(&mut self.hits, &mut hb);
            self.hit_phases[h][phase] = hb;
        }
        std::mem::swap(&mut self.items, &mut buf);
        self.phases[top][phase] = buf;
    }

    fn current_clip(&self) -> Option<Rect> {
        let mut r: Option<Rect> = None;
        for c in &self.clips {
            let cr = match &c.xf {
                Some(m) => map_rect(m, c.rect),
                None => c.rect,
            };
            r = Some(match r {
                Some(x) => x.intersect(&cr),
                None => cr,
            });
        }
        r
    }

    fn push_hit_xf(&mut self, m: [f32; 6]) {
        let top = match self.hit_xf.last() {
            Some(t) => mul2d(t, &m),
            None => m,
        };
        self.hit_xf.push(top);
    }

    fn hit(&mut self, rect: Rect, node: NodeId, style: &ComputedStyle, text: bool) {
        // Invisible boxes aren't hit (clicks go through a hidden overlay iframe).
        if rect.w <= 0.0 || rect.h <= 0.0 || style.visibility != Visibility::Visible {
            return;
        }
        let cursor = match style.cursor {
            Cursor::Auto => {
                if text && style.user_select {
                    Cursor::Text
                } else {
                    Cursor::Default
                }
            }
            c => c,
        };
        let clip = self.current_clip();
        let rect = match self.hit_xf.last() {
            Some(m) => map_rect(m, rect),
            None => rect,
        };
        self.hits.push(HitRegion { rect, clip, node, fixed: self.in_fixed, cursor, pointer_events: style.pointer_events, scroller: self.scroller });
    }

    /// The HTML inside an inline SVG's `<foreignObject>`s, laid out and
    /// painted on its own (the SVG renderer draws only SVG): each becomes a
    /// picture the SVG embeds at the foreignObject's place, so its masks,
    /// clips and transforms still apply (Discord's avatars and server icons).
    /// Hit regions for foreignObject content, mapped from the SVG's user
    /// space into the page: the deepest visible element of each, so events
    /// bubble through the HTML inside.
    fn foreign_object_hits(&mut self, doc: &Document, svg: NodeId, cr: Rect, fos: &[(NodeId, (String, f32, f32))]) {
        let vb = doc.attr(svg, "viewBox").map(|v| v.split(|c: char| c == ',' || c.is_whitespace()).filter_map(|x| x.parse::<f32>().ok()).collect::<Vec<_>>());
        let (k, tx, ty) = match vb.as_deref() {
            Some([vx, vy, vw, vh]) if *vw > 0.0 && *vh > 0.0 => {
                let k = (cr.w / vw).min(cr.h / vh);
                (k, cr.x + (cr.w - vw * k) / 2.0 - vx * k, cr.y + (cr.h - vh * k) / 2.0 - vy * k)
            }
            _ => (1.0, cr.x, cr.y),
        };
        for (fo, (_, fw, fh)) in fos {
            let num = |a: &str| doc.attr(*fo, a).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).unwrap_or(0.0);
            let r = Rect::new(tx + num("x") * k, ty + num("y") * k, fw * k, fh * k);
            let mut target = *fo;
            for _ in 0..8 {
                let next = doc.element_children(target).filter(|&c| self.inp.styles.get(c).is_some_and(|st| st.display != Display::None && st.pointer_events && !matches!(st.position, Position::Absolute | Position::Fixed) && st.visibility == Visibility::Visible)).last();
                match next {
                    Some(c) => target = c,
                    None => break,
                }
            }
            if target == *fo {
                continue;
            }
            if let Some(st) = self.inp.styles.get(target).cloned() {
                self.hit(r, target, &st, false);
            }
        }
    }

    fn render_foreign_objects(&mut self, doc: &Document, svg: NodeId, w: f32, h: f32) -> Vec<(NodeId, (String, f32, f32))> {
        use std::hash::{Hash, Hasher};
        let mut out = Vec::new();
        let fos: Vec<NodeId> = doc
            .descendants(svg)
            .filter(|&d| doc.element(d).is_some_and(|e| e.ns == crate::dom::Namespace::Svg && e.name.eq_ignore_ascii_case("foreignObject")))
            .take(32)
            .collect();
        if fos.is_empty() || FO_DEPTH.with(|d| d.get()) > 2 {
            return out;
        }
        // Device pixels per user unit: the viewBox scale, at 2x for sharpness.
        let vb = doc.attr(svg, "viewBox").map(|v| v.split(|c: char| c == ',' || c.is_whitespace()).filter_map(|x| x.parse::<f32>().ok()).collect::<Vec<_>>());
        let k = match vb.as_deref() {
            Some([_, _, vw, vh]) if *vw > 0.0 && *vh > 0.0 => (w / vw).min(h / vh),
            _ => 1.0,
        };
        let scale = (k * 2.0).clamp(1.0, 4.0);
        for fo in fos {
            if self.inp.styles.get(fo).is_some_and(|st| st.display == Display::None) {
                continue;
            }
            let num = |a: &str| doc.attr(fo, a).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).unwrap_or(0.0);
            let (fw, fh) = (num("width"), num("height"));
            if !(fw > 0.0 && fh > 0.0) || fw * fh * scale * scale > 4.0e6 || doc.element_children(fo).next().is_none() {
                continue;
            }
            // What the picture depends on: the subtree, its styles, and its
            // images' arrival.
            let mut hs = std::collections::hash_map::DefaultHasher::new();
            (fw.to_bits(), fh.to_bits(), scale.to_bits()).hash(&mut hs);
            for d in doc.descendants(fo) {
                if let Some(e) = doc.element(d) {
                    e.name.hash(&mut hs);
                    for a in &e.attrs {
                        a.name.hash(&mut hs);
                        a.value.hash(&mut hs);
                    }
                    if let Some(st) = self.inp.styles.get(d) {
                        (Rc::as_ptr(st) as usize).hash(&mut hs);
                    }
                    if e.name == "img" {
                        self.inp.res.image_for(doc, d).map(|i| Arc::as_ptr(&i) as usize).hash(&mut hs);
                    }
                } else if let crate::dom::NodeData::Text(t) = &doc.node(d).data {
                    t.hash(&mut hs);
                }
            }
            let key = format!("ohbfo:{:016x}", hs.finish());
            if !crate::page::resources::has_svg_embed(&key) {
                let prev = doc.render_root.replace(fo);
                FO_DEPTH.with(|d| d.set(d.get() + 1));
                let tree = crate::layout::layout_document(doc, self.inp.styles, self.inp.fonts, self.inp.res, fw, fh);
                let empty = HashMap::new();
                let mut inp2 = PaintInput {
                    tree: &tree,
                    doc,
                    styles: self.inp.styles,
                    res: self.inp.res,
                    scroll: &empty,
                    focus: None,
                    caret: None,
                    caret_visible: false,
                    fonts: &mut *self.inp.fonts,
                    find: &[],
                    selection: None,
                    edit_caret: None,
                    field_sel: None,
                    select_popup: None,
                };
                let (list, _) = build_display_list(&mut inp2);
                FO_DEPTH.with(|d| d.set(d.get() - 1));
                doc.render_root.set(prev);
                let (pw, ph) = ((fw * scale).ceil() as u32, (fh * scale).ceil() as u32);
                if let Some(mut pm) = tiny_skia::Pixmap::new(pw.max(1), ph.max(1)) {
                    // The root's background is the canvas background.
                    let bg = list.bg;
                    if bg.a > 0 {
                        pm.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, bg.a));
                    }
                    super::raster::Rasterizer::new().render(&list, &mut pm, (0.0, 0.0), scale, 0.0, (0.0, ph as f32));
                    if std::env::var_os("OHB_DEBUG_SVG").is_some() {
                        let opaque = pm.data().chunks_exact(4).filter(|p| p[3] > 0).count();
                        eprintln!("[fo] {key} {fw}x{fh} scale {scale} items {} boxes {} opaque px {opaque}", list.items.len(), tree.boxes.len());
                    }
                    crate::page::resources::set_svg_embed(&key, &pm);
                }
            }
            out.push((fo, (key, fw, fh)));
        }
        out
    }

    pub fn build(mut self) -> (DisplayList, Vec<HitRegion>) {
        let tree = self.inp.tree;
        let root = tree.root;
        let in_fo = FO_DEPTH.with(|d| d.get()) > 0;
        let bg = if in_fo { Color::TRANSPARENT } else { self.inp.styles.root_bg };
        // Root/body background is propagated to the canvas; don't paint it twice.
        // (Not inside a foreignObject: there the subtree's boxes paint their own.)
        if let (false, Some(&html_box)) = (in_fo, tree.boxes[root].children.first()) {
            let hs = &tree.boxes[html_box].style;
            if hs.background_color.resolve(hs.color).a > 0 || hs.background.is_some() {
                self.skip_bg = Some(html_box);
                // Root background images paint over the whole canvas.
                if let Some(layers) = hs.background.clone() {
                    let r = Rect::new(0.0, 0.0, tree.doc_width, tree.doc_height.max(tree.viewport.1));
                    self.paint_bg_layers(&layers, [r, r, r], Radii::ZERO, hs);
                }
            } else if let Some(body) = self.inp.doc.body() {
                if let Some(&bb) = tree.node_box.get(&body) {
                    let bs = tree.boxes[bb].style.clone();
                    if bs.background_color.resolve(bs.color).a > 0 || bs.background.is_some() {
                        self.skip_bg = Some(bb);
                        if let Some(layers) = bs.background.clone() {
                            let r = Rect::new(0.0, 0.0, tree.doc_width, tree.doc_height.max(tree.viewport.1));
                            self.paint_bg_layers(&layers, [r, r, r], Radii::ZERO, &bs);
                        }
                    }
                }
            }
        }
        let kids = tree.boxes[root].children.clone();
        let mut deferred = Vec::new();
        for c in kids {
            if tree.boxes[c].style.is_positioned() {
                self.defer(c, &mut deferred);
            } else {
                self.paint_stacking(c);
            }
        }
        // Root-level absolute/fixed boxes.
        for &a in &tree.boxes[root].abs_children {
            if !deferred.iter().any(|d: &Deferred| d.id == a) && tree.boxes[a].parent == Some(root) {
                self.defer(a, &mut deferred);
            }
        }
        self.flush_deferred(deferred, false, true);
        self.paint_select_popup();
        (DisplayList { items: self.items, fixed: self.fixed, bg }, self.hits)
    }

    fn defer(&mut self, id: BoxId, list: &mut Vec<Deferred>) {
        let b = &self.inp.tree.boxes[id];
        let top = b.parent == Some(self.inp.tree.root) && (b.pseudo == 5 || b.node.map(|n| self.inp.doc.top_layer.contains(&n)).unwrap_or(false));
        let z = b.style.z_index.unwrap_or(0) as i64 + if top { 1i64 << 40 } else { 0 };
        // An absolutely positioned box escapes the clipping and scrolling of
        // boxes between it and its containing block; clippers that are the
        // containing block or its ancestors still apply (CSS 2.1 §11.1.1).
        let applies = |me: &Self, owner: BoxId, cb: Option<BoxId>| match cb {
            Some(cb) => owner == cb || me.is_ancestor(owner, cb),
            None => false,
        };
        let (clips, off, scrolls) = if b.out_of_flow {
            let cb = self.cb_of.get(&id).copied();
            let clips: Vec<ClipEntry> = self.clips.iter().filter(|c| applies(self, c.owner, cb)).cloned().collect();
            let mut off = self.off;
            let mut scrolls = Vec::new();
            for &(owner, dx, dy) in &self.scroll_stack {
                if applies(self, owner, cb) {
                    scrolls.push((owner, dx, dy));
                } else {
                    off.0 -= dx;
                    off.1 -= dy;
                }
            }
            (clips, off, scrolls)
        } else {
            (self.clips.clone(), self.off, self.scroll_stack.clone())
        };
        self.order += 1;
        list.push(Deferred { z, order: self.order, id, clips: Rc::new(clips), off, toff: self.toff, scrolls: Rc::new(scrolls) });
    }

    fn is_ancestor(&self, anc: BoxId, mut b: BoxId) -> bool {
        let boxes = &self.inp.tree.boxes;
        let mut guard = 0;
        while let Some(p) = boxes[b].parent {
            if p == anc {
                return true;
            }
            b = p;
            guard += 1;
            if guard > 1000 {
                break;
            }
        }
        false
    }

    /// Paint positioned descendants in z-order. `sc`: the owner is a real
    /// stacking context; otherwise (a positioned box with `z-index: auto`)
    /// positive z-index descendants hoisted to it pass on to the enclosing one.
    fn flush_deferred(&mut self, mut list: Vec<Deferred>, negative_only: bool, sc: bool) -> Vec<Deferred> {
        list.sort_by(|a, b| (a.z, a.order).cmp(&(b.z, b.order)));
        let mut rest = Vec::new();
        let mut queue: std::collections::VecDeque<Deferred> = list.into();
        self.hoist.push(Vec::new());
        let pass_up = |me: &mut Self, hoisted: Vec<Deferred>| -> Vec<Deferred> {
            let n = me.hoist.len();
            if sc || n < 2 {
                return hoisted;
            }
            let (up, keep): (Vec<Deferred>, Vec<Deferred>) = hoisted.into_iter().partition(|d| d.z > 0);
            me.hoist[n - 2].extend(up);
            keep
        };
        while let Some(d) = queue.pop_front() {
            // Merge descendants hoisted by the previous item into the z-order.
            let hoisted = std::mem::take(self.hoist.last_mut().unwrap());
            let hoisted = pass_up(self, hoisted);
            if !hoisted.is_empty() {
                let mut all: Vec<Deferred> = queue.drain(..).collect();
                all.push(d);
                all.extend(hoisted);
                all.sort_by(|a, b| (a.z, a.order).cmp(&(b.z, b.order)));
                queue = all.into();
                continue;
            }
            if negative_only && d.z >= 0 {
                rest.push(d);
                continue;
            }
            let saved_clips = std::mem::replace(&mut self.clips, (*d.clips).clone());
            let saved_scrolls = std::mem::replace(&mut self.scroll_stack, (*d.scrolls).clone());
            let saved_off = self.off;
            let saved_toff = self.toff;
            let saved_fixed = self.in_fixed;
            let fixed = self.inp.tree.boxes[d.id].fixed;
            let begin_fixed = fixed && !saved_fixed;
            if fixed {
                self.in_fixed = true;
                self.off = (0.0, 0.0);
                self.toff = (0.0, 0.0);
                self.clips.clear();
                self.scroll_stack.clear();
                if begin_fixed {
                    self.fixed += 1;
                    self.emit(Item::BeginFixed);
                }
            } else {
                self.off = d.off;
                self.toff = d.toff;
            }
            for c in self.clips.clone() {
                self.emit(Item::PushClip { r: c.rect, radii: c.radii });
            }
            self.paint_stacking(d.id);
            for _ in 0..self.clips.len() {
                self.emit(Item::PopClip);
            }
            if begin_fixed {
                self.emit(Item::EndFixed);
            }
            self.clips = saved_clips;
            self.scroll_stack = saved_scrolls;
            self.off = saved_off;
            self.toff = saved_toff;
            self.in_fixed = saved_fixed;
            // Anything the last item hoisted still has to be painted.
            let hoisted = std::mem::take(self.hoist.last_mut().unwrap());
            let hoisted = pass_up(self, hoisted);
            if !hoisted.is_empty() {
                let mut all: Vec<Deferred> = queue.drain(..).collect();
                all.extend(hoisted);
                all.sort_by(|a, b| (a.z, a.order).cmp(&(b.z, b.order)));
                queue = all.into();
            }
        }
        // Hoisted entries left when an outer loop doesn't exist go back to the caller.
        if let Some(left) = self.hoist.pop() {
            rest.extend(left);
        }
        rest
    }

    /// The path of a `clip-path` basic shape in document coordinates.
    fn shape_path(&self, id: BoxId, shape: &crate::css::style::BasicShape, refbox: u8) -> Option<tiny_skia::Path> {
        let b = &self.inp.tree.boxes[id];
        let br = self.border_rect(id);
        let r = match refbox {
            1 => Rect::new(br.x + b.border[3], br.y + b.border[0], br.w - b.border[1] - b.border[3], br.h - b.border[0] - b.border[2]),
            2 => Rect::new(br.x + b.border[3] + b.padding[3], br.y + b.border[0] + b.padding[0], br.w - b.border[1] - b.border[3] - b.padding[1] - b.padding[3], br.h - b.border[0] - b.border[2] - b.padding[0] - b.padding[2]),
            3 => Rect::new(br.x - b.margin[3], br.y - b.margin[0], br.w + b.margin[1] + b.margin[3], br.h + b.margin[0] + b.margin[2]),
            _ => br,
        };
        basic_shape_path(shape, r)
    }

    fn border_rect(&self, id: BoxId) -> Rect {
        let b = &self.inp.tree.boxes[id];
        Rect::new(b.abs_x - self.off.0, b.abs_y - self.off.1, b.w, b.h)
    }

    /// The transform of box `id` (around its transform-origin) in document
    /// coordinates shifted by the current offset; identity when it has none.
    fn local_matrix(&self, id: BoxId) -> Mat4 {
        let b = &self.inp.tree.boxes[id];
        match &b.style.transform {
            Some(ops) => {
                let r = self.border_rect(id);
                let s = &b.style;
                let origin = (s.transform_origin.0.resolve(b.w), s.transform_origin.1.resolve(b.h), s.transform_origin_z);
                mat4::box_matrix(ops, r.x, r.y, b.w, b.h, origin)
            }
            None => Mat4::IDENTITY,
        }
    }

    /// The perspective matrix box `p` applies to its children, if any.
    fn perspective_of(&self, p: BoxId) -> Option<Mat4> {
        let b = &self.inp.tree.boxes[p];
        let d = b.style.perspective?;
        let r = self.border_rect(p);
        Some(mat4::perspective_matrix(d, r.x + b.style.perspective_origin.0.resolve(b.w), r.y + b.style.perspective_origin.1.resolve(b.h)))
    }

    /// Where box `id`'s painted content may reach (border boxes of its
    /// subtree, grown by shadows and outlines), in current coordinates.
    fn ink_bounds(&self, id: BoxId) -> Rect {
        let tree = self.inp.tree;
        let mut r = self.border_rect(id);
        let mut stack = vec![id];
        let mut n = 0;
        while let Some(b) = stack.pop() {
            n += 1;
            if n > 20000 {
                break;
            }
            let bx = &tree.boxes[b];
            let mut br = Rect::new(bx.abs_x - self.off.0, bx.abs_y - self.off.1, bx.w.max(0.0), bx.h.max(0.0));
            let mut grow = 0.0f32;
            if let Some(sh) = &bx.style.box_shadow {
                for x in sh.iter().filter(|x| !x.inset) {
                    grow = grow.max(x.x.abs().max(x.y.abs()) + x.blur * 2.0 + x.spread.max(0.0));
                }
            }
            if bx.style.outline_style != BorderStyle::None {
                grow = grow.max(bx.style.outline_width + bx.style.outline_offset.max(0.0));
            }
            br = br.inflate(grow + 2.0);
            if br.w > 0.0 && br.h > 0.0 {
                r = r.union(&br);
            }
            stack.extend(bx.children.iter().copied());
        }
        r
    }

    /// Paint a box that may establish a stacking context, placing it in its
    /// 3D rendering context when it takes part in one.
    fn paint_stacking(&mut self, id: BoxId) {
        let tree = self.inp.tree;
        let s = &tree.boxes[id].style;
        let p3 = if s.transform.is_some() || s.preserve_3d || !self.ctx3d.is_empty() { crate::layout::parent_3d(tree, id) } else { None };
        let in_ctx = matches!((self.ctx3d.last(), p3), (Some(c), Some(p)) if c.owner == p);
        let preserve = s.preserve_3d && !flattens(s);
        let m = if in_ctx {
            let c = self.ctx3d.last().unwrap();
            Some(c.m.mul(&c.persp).mul(&self.local_matrix(id)))
        } else {
            let t = s.transform.as_ref().map(|_| self.local_matrix(id));
            let persp = if t.is_some() || preserve { p3.and_then(|p| self.perspective_of(p)) } else { None };
            match (persp, t) {
                (None, None) => None,
                (p, t) => Some(p.unwrap_or(Mat4::IDENTITY).mul(&t.unwrap_or(Mat4::IDENTITY))),
            }
        };
        if let Some(m) = &m {
            if !m.is_finite() || (s.backface_hidden && m.is_backface()) {
                return;
            }
        }
        if s.opacity <= 0.001 {
            // Invisible, but still hit-testable (opacity:0 overlays such as
            // styled checkboxes and file inputs): record hit regions only.
            let saved = std::mem::take(&mut self.items);
            let n_planes = self.planes.len();
            let n_last = self.planes.last().map_or(0, |l| l.len());
            let n_ctx = self.ctx3d.len();
            self.paint_stacking_inner(id, m);
            self.planes.truncate(n_planes);
            if let Some(l) = self.planes.last_mut() {
                l.truncate(n_last);
            }
            self.ctx3d.truncate(n_ctx);
            self.items = saved;
            return;
        }
        if !in_ctx && !preserve {
            self.paint_stacking_inner(id, m);
            return;
        }
        // A plane of a 3D rendering context (or the root establishing one).
        let m = m.unwrap_or(Mat4::IDENTITY);
        let seq = self.plane_seq;
        self.plane_seq += 1;
        let root = !in_ctx;
        if root {
            self.planes.push(Vec::new());
        }
        if preserve {
            let persp = self.perspective_of(id).unwrap_or(Mat4::IDENTITY);
            self.ctx3d.push(Ctx3d { owner: id, m, persp });
        }
        let saved = std::mem::take(&mut self.items);
        self.paint_stacking_inner(id, Some(m));
        let items = std::mem::replace(&mut self.items, saved);
        if preserve {
            self.ctx3d.pop();
        }
        let rect = self.border_rect(id);
        if items.iter().any(|it| it.draws()) {
            if let Some(list) = self.planes.last_mut() {
                list.push(Plane { seq, m, rect, items });
            }
        }
        if root {
            let planes = self.planes.pop().unwrap_or_default();
            for p in sort_planes(planes) {
                self.items.extend(p.items);
            }
        }
    }

    /// Paint a box that may establish a stacking context: decorations, content,
    /// then positioned descendants in z-order. `m` is its transform (with any
    /// perspective) in the current coordinates.
    fn paint_stacking_inner(&mut self, id: BoxId, m: Option<Mat4>) {
        let tree = self.inp.tree;
        // A box stuck to the viewport: the rasterizer adjusts its offset for
        // the scroll position it draws at.
        let mut sticky = false;
        if tree.boxes[id].style.position == crate::css::style::Position::Sticky && !self.in_fixed {
            if let Some(&(_, applied)) = tree.sticky.iter().find(|(b, _)| *b == id) {
                if tree.sticky_scroller(id, self.inp.doc) == (None, false) {
                    let geom = tree.sticky_geom(id, applied, tree.viewport);
                    self.emit(Item::PushSticky(Arc::new(crate::paint::StickyItem { geom, applied, viewport: tree.viewport })));
                    self.fixed += 1;
                    sticky = true;
                }
            }
        }
        self.paint_stacking_body(id, m);
        if sticky {
            self.emit(Item::PopSticky);
        }
    }

    fn paint_stacking_body(&mut self, id: BoxId, m: Option<Mat4>) {
        let tree = self.inp.tree;
        let b = &tree.boxes[id];
        let s = b.style.clone();
        // clip-path: inset() / clip: rect() → rectangular clip (fully clipped → skip).
        let mut pushed_shape_clip = false;
        if let Some(cs) = &s.clip_shape {
            let r = self.border_rect(id);
            let cr = match &**cs {
                crate::css::style::ClipShape::Inset(q) => {
                    let (t, rr, bb, l) = (q[0].resolve(b.h), q[1].resolve(b.w), q[2].resolve(b.h), q[3].resolve(b.w));
                    Rect::new(r.x + l, r.y + t, r.w - l - rr, r.h - t - bb)
                }
                // clip-path shapes are applied after the transform (they move with the element).
                crate::css::style::ClipShape::Shape(..) => Rect::new(r.x - 1e6, r.y - 1e6, 2e6 + r.w, 2e6 + r.h),
                crate::css::style::ClipShape::Rect(e) => {
                    if !s.is_positioned() || !matches!(s.position, crate::css::style::Position::Absolute | crate::css::style::Position::Fixed) {
                        Rect::new(r.x - 1e6, r.y - 1e6, 2e6 + r.w, 2e6 + r.h)
                    } else {
                        let t = e[0].as_ref().map(|v| v.resolve(b.h)).unwrap_or(0.0);
                        let rgt = e[1].as_ref().map(|v| v.resolve(b.w)).unwrap_or(b.w);
                        let btm = e[2].as_ref().map(|v| v.resolve(b.h)).unwrap_or(b.h);
                        let l = e[3].as_ref().map(|v| v.resolve(b.w)).unwrap_or(0.0);
                        Rect::new(r.x + l, r.y + t, rgt - l, btm - t)
                    }
                }
            };
            if cr.w <= 0.0 || cr.h <= 0.0 {
                return;
            }
            if !matches!(&**cs, crate::css::style::ClipShape::Shape(..)) {
                self.emit(Item::PushClip { r: cr, radii: None });
                pushed_shape_clip = true;
            }
        }
        // mix-blend-mode / isolation: the element renders into a group that is
        // then blended with (or, for isolation, composited onto) the backdrop.
        let pushed_blend = s.mix_blend != 0 || s.isolate;
        if pushed_blend {
            self.emit(Item::PushBlend(s.mix_blend));
        }
        let mut pushed_opacity = false;
        let opacity_at = self.items.len();
        if s.opacity < 0.999 {
            self.emit(Item::PushOpacity(s.opacity));
            pushed_opacity = true;
        }
        // filter: everything below renders into an offscreen layer.
        let mut pushed_filter = false;
        if let Some(f) = &s.filter {
            let list = resolve_filters(f, s.color);
            if !list.is_empty() {
                self.emit(Item::PushFilter(Arc::new(list)));
                pushed_filter = true;
            }
        }
        // Transforms: translations shift coordinates; 2D ones become matrices;
        // ones with perspective render through a projective warp.
        let saved_off = self.off;
        let saved_toff = self.toff;
        // 0: nothing pushed, 1: PushTransform, 2: PushWarp
        let mut pushed_transform = 0u8;
        if let Some(m) = m {
            match m.flat_affine() {
                Some(a) => {
                    let is_translate = (a[0] - 1.0).abs() < 1e-9 && a[1].abs() < 1e-9 && a[2].abs() < 1e-9 && (a[3] - 1.0).abs() < 1e-9;
                    if is_translate {
                        let (tx, ty) = (a[4] as f32, a[5] as f32);
                        self.off.0 -= tx;
                        self.off.1 -= ty;
                        self.toff.0 -= tx;
                        self.toff.1 -= ty;
                    } else if a.iter().all(|v| v.is_finite()) {
                        let a = a.map(|v| v as f32);
                        self.emit(Item::PushTransform(a));
                        self.push_hit_xf(a);
                        pushed_transform = 1;
                    }
                }
                None => {
                    let bounds = self.ink_bounds(id);
                    self.emit(Item::PushWarp { h: m.homography(), bounds });
                    pushed_transform = 2;
                }
            }
        }
        // clip-path basic shapes and masks, in the element's (transformed) space.
        let mut pushed_path_clip = false;
        if let Some(cs) = &s.clip_shape {
            if let crate::css::style::ClipShape::Shape(shape, refbox) = &**cs {
                match self.shape_path(id, shape, *refbox) {
                    Some(p) => {
                        self.emit(Item::PushClipPath(Arc::new(p)));
                        pushed_path_clip = true;
                    }
                    None => {
                        // An empty shape clips everything away.
                        match pushed_transform {
                            1 => {
                                self.emit(Item::PopTransform);
                                self.hit_xf.pop();
                            }
                            2 => self.emit(Item::PopWarp),
                            _ => {}
                        }
                        self.off = saved_off;
                        self.toff = saved_toff;
                        if pushed_filter {
                            self.emit(Item::PopFilter);
                        }
                        if pushed_opacity {
                            self.emit(Item::PopOpacity);
                        }
                        if pushed_blend {
                            self.emit(Item::PopBlend);
                        }
                        if pushed_shape_clip {
                            self.emit(Item::PopClip);
                        }
                        return;
                    }
                }
            }
        }
        let mut pushed_mask = false;
        if let Some(m) = &s.mask {
            if generic_mask(m, b) {
                let r = self.border_rect(id);
                let mut layers = vec![(**m).clone()];
                if let Some(more) = &s.mask_more {
                    layers.extend(more.iter().cloned());
                }
                for layer in layers.iter_mut() {
                    layer.origin = 0;
                    layer.clip = 0;
                }
                let saved = std::mem::take(&mut self.items);
                // (mask-composite: add — the layers' coverage adds up.)
                self.paint_bg_layers(&Rc::new(layers), [r, r, r], Radii::ZERO, &s);
                let items = std::mem::replace(&mut self.items, saved);
                self.emit(Item::PushMask(Arc::new(items)));
                pushed_mask = true;
            }
        }
        // backdrop-filter: filter what is drawn behind the border box.
        // Only what lies within the nearest Backdrop Root is filtered; one
        // set by an ancestor's group effect (opacity, filter, mask...) isn't
        // modelled, and then (as for the root element) nothing is filtered.
        let backdrop_ok = s.backdrop_filter.is_some() && b.parent.map(|p| tree.boxes[p].parent.is_some()).unwrap_or(false) && {
            let mut ok = true;
            let mut p = b.parent;
            while let Some(pp) = p {
                let ps = &tree.boxes[pp].style;
                if ps.opacity < 0.999 || ps.filter.is_some() || ps.mask.is_some() || ps.clip_shape.is_some() || ps.mix_blend != 0 || ps.backdrop_filter.is_some() || ps.will_change_backdrop_root {
                    ok = false;
                    break;
                }
                p = tree.boxes[pp].parent;
            }
            ok
        };
        if let Some(bf) = s.backdrop_filter.as_ref().filter(|_| backdrop_ok) {
            let list = resolve_filters(bf, s.color);
            if !list.is_empty() {
                let r = self.border_rect(id);
                self.emit(Item::Backdrop { r, filters: Arc::new(list) });
            }
        }
        // Collect deferred positioned descendants while painting normal content
        // into a temporary buffer so negative z-indices can go underneath.
        let saved_items = std::mem::take(&mut self.items);
        let mut deferred = Vec::new();
        self.paint_self(id);
        let after_bg = std::mem::take(&mut self.items);
        self.phases.push([Vec::new(), Vec::new()]);
        self.hit_phases.push([Vec::new(), Vec::new()]);
        self.paint_children(id, &mut deferred);
        let [floats, inline] = self.phases.pop().unwrap_or_default();
        let [hf, hi] = self.hit_phases.pop().unwrap_or_default();
        self.hits.extend(hf);
        self.hits.extend(hi);
        let mut content = std::mem::take(&mut self.items);
        content.extend(floats);
        content.extend(inline);
        // Reassemble: saved + background + negatives + content + positives.
        *(&mut self.items) = saved_items;
        for it in after_bg {
            self.emit(it);
        }
        // A positioned box with `z-index: auto` (and nothing else that creates a
        // stacking context) isn't one: its positive-z descendants belong to the
        // enclosing stacking context (CSS 2.1 Appendix E).
        let creates_sc = s.z_index.is_some()
            || s.opacity < 0.999
            || s.transform.is_some()
            || s.perspective.is_some()
            || s.preserve_3d
            || s.filter.is_some()
            || s.backdrop_filter.is_some()
            || s.clip_shape.is_some()
            || s.mask.is_some()
            || s.mix_blend != 0
            || s.isolate
            || matches!(s.position, crate::css::style::Position::Fixed | crate::css::style::Position::Sticky)
            || b.parent.map(|p| tree.boxes[p].parent.is_none()).unwrap_or(true);
        if !creates_sc && !self.hoist.is_empty() {
            let (up, keep): (Vec<Deferred>, Vec<Deferred>) = deferred.into_iter().partition(|d| d.z > 0);
            self.hoist.last_mut().unwrap().extend(up);
            deferred = keep;
        }
        let rest = self.flush_deferred(deferred, true, creates_sc);
        for it in content {
            self.emit(it);
        }
        self.flush_deferred(rest, false, creates_sc);
        self.paint_outline(id);
        if pushed_mask {
            self.emit(Item::PopMask);
        }
        if pushed_path_clip {
            self.emit(Item::PopClip);
        }
        match pushed_transform {
            1 => {
                self.emit(Item::PopTransform);
                self.hit_xf.pop();
            }
            2 => self.emit(Item::PopWarp),
            _ => {}
        }
        self.off = saved_off;
        self.toff = saved_toff;
        if pushed_filter {
            self.emit(Item::PopFilter);
        }
        if pushed_opacity {
            // Opacity applies to the element as a group: when more than one
            // thing is drawn inside, render them into a layer first.
            let inner = self.items.get(opacity_at + 1..).unwrap_or(&[]);
            let drawn = inner.iter().filter(|it| it.draws()).count();
            if drawn > 1 && matches!(self.items.get(opacity_at), Some(Item::PushOpacity(_))) {
                // Where the group draws (unknown under nested transforms/filters/fixed parts).
                let mut bounds: Option<Rect> = None;
                let mut known = true;
                for it in inner {
                    match it {
                        Item::PushTransform(_) | Item::PushWarp { .. } | Item::PushFilter(_) | Item::BeginFixed | Item::PushMask(_) | Item::PushSticky(_) => {
                            known = false;
                            break;
                        }
                        _ if it.draws() => match it.bounds() {
                            Some(b) => bounds = Some(match bounds { Some(u) => u.union(&b), None => b }),
                            None => {
                                known = false;
                                break;
                            }
                        },
                        _ => {}
                    }
                }
                self.items[opacity_at] = Item::PushGroup { opacity: s.opacity, bounds: if known { bounds } else { None } };
                self.emit(Item::PopGroup);
            } else {
                self.emit(Item::PopOpacity);
            }
        }
        if pushed_blend {
            self.emit(Item::PopBlend);
        }
        if pushed_shape_clip {
            self.emit(Item::PopClip);
        }
    }

    fn paint_children(&mut self, id: BoxId, deferred: &mut Vec<Deferred>) {
        let tree = self.inp.tree;
        let b = &tree.boxes[id];
        let s = b.style.clone();
        let clips = s.overflow_x.clips() || s.overflow_y.clips() || s.contain & crate::css::style::CONTAIN_PAINT != 0;
        let saved_off = self.off;
        let saved_scroller = self.scroller;
        let mut pushed = false;
        let mut pushed_scroll = false;
        if clips && !matches!(b.kind, BoxKind::Replaced(_)) {
            let r = self.border_rect(id);
            let mut pr = Rect::new(r.x + b.border[3], r.y + b.border[0], (b.w - b.border[1] - b.border[3]).max(0.0), (b.h - b.border[0] - b.border[2]).max(0.0));
            let radii = radii_px(&s, b.w, b.h);
            let mut rr = if has_radius(&radii) { Some(radii) } else { None };
            // `overflow: clip` on one axis leaves the other (visible) axis
            // unclipped (paint containment clips both).
            if s.contain & crate::css::style::CONTAIN_PAINT == 0 {
                use crate::css::style::Overflow;
                if s.overflow_x == Overflow::Visible {
                    pr.x -= 1e6;
                    pr.w += 2e6;
                    rr = None;
                }
                if s.overflow_y == Overflow::Visible {
                    pr.y -= 1e6;
                    pr.h += 2e6;
                    rr = None;
                }
            }
            self.emit_all(Item::PushClip { r: pr, radii: rr });
            let xf = self.hit_xf.last().copied();
            self.clips.push(ClipEntry { rect: pr, radii: rr, owner: id, xf });
            pushed = true;
            if let Some(n) = b.node {
                if s.overflow_x.scrollable() || s.overflow_y.scrollable() {
                    if let Some(&(sx, sy)) = self.inp.scroll.get(&n) {
                        let maxx = (b.scroll_w - pr.w).max(0.0);
                        let maxy = (b.scroll_h - pr.h).max(0.0);
                        let (dx, dy) = (sx.clamp(0.0, maxx), sy.clamp(0.0, maxy));
                        self.off.0 += dx;
                        self.off.1 += dy;
                        self.scroll_stack.push((id, dx, dy));
                        pushed_scroll = true;
                    }
                    self.scroller = Some(n);
                }
            }
        }
        // Vertical writing-mode root: its content was laid out in a rotated frame.
        let wm_xf = if b.wm != 0 {
            let r = self.border_rect(id);
            crate::layout::wm_matrix(b, r.x, r.y)
        } else {
            None
        };
        if let Some(m) = wm_xf {
            self.emit_all(Item::PushTransform(m));
            self.push_hit_xf(m);
            self.wm.push(b.wm);
        }
        // Replaced content (inline-content phase).
        if let BoxKind::Replaced(k) = &b.kind {
            let k = k.clone();
            self.in_phase(1, |me| me.paint_replaced(id, k));
        }
        let mut kids = b.children.clone();
        // Flex and grid items paint atomically, like inline blocks, in
        // order-modified document order (css-flexbox §5.4, css-grid §9).
        let atomic_items = s.display.is_flex() || s.display.is_grid();
        if atomic_items {
            kids.sort_by_key(|&k| tree.boxes[k].style.order);
        }
        for c in kids {
            let cb = &tree.boxes[c];
            let textual = matches!(cb.kind, BoxKind::Text(_) | BoxKind::LineBreak | BoxKind::Generated(_));
            if cb.style.is_positioned() && !cb.inline_level && !textual {
                self.defer(c, deferred);
                continue;
            }
            // A flex or grid item with a z-index is z-ordered (and stacks) like a
            // positioned box even when it is `position: static` (css-flexbox §4.3,
            // css-grid §9.4): Google's sign-in field sits above its label this way.
            if atomic_items && !textual && !cb.out_of_flow && cb.style.z_index.is_some() && !matches!(cb.kind, BoxKind::InlineBox) {
                self.defer(c, deferred);
                continue;
            }
            if atomic_items && !textual && !cb.out_of_flow && !matches!(cb.kind, BoxKind::InlineBox) {
                self.paint_atomic(c, deferred);
                continue;
            }
            match cb.kind {
                BoxKind::InlineBox | BoxKind::Text(_) | BoxKind::Generated(_) | BoxKind::LineBreak => {
                    // painted by the IFC; but find positioned descendants
                    self.defer_positioned_in_inline(c, deferred);
                }
                _ => {
                    if cb.inline_level {
                        continue; // atomic inline painted from IFC
                    }
                    if cb.is_float {
                        self.in_phase(0, |me| me.paint_float(c, deferred));
                    } else {
                        self.paint_block(c, deferred);
                    }
                }
            }
        }
        if b.ifc.is_some() {
            self.in_phase(1, |me| me.paint_ifc(id, deferred));
        }
        if let Some(m) = b.marker.clone() {
            self.in_phase(1, |me| me.paint_marker(id, &m));
        }
        if b.collapsed.is_some() {
            self.paint_collapsed_borders(id);
        }
        if wm_xf.is_some() {
            self.emit_all(Item::PopTransform);
            self.hit_xf.pop();
            self.wm.pop();
        }
        if pushed {
            self.emit_all(Item::PopClip);
            self.clips.pop();
        }
        if pushed_scroll {
            self.scroll_stack.pop();
        }
        self.off = saved_off;
        self.scroller = saved_scroller;
    }

    fn defer_positioned_in_inline(&mut self, id: BoxId, deferred: &mut Vec<Deferred>) {
        let tree = self.inp.tree;
        for &c in &tree.boxes[id].children {
            let cb = &tree.boxes[c];
            // Only out-of-flow positioned boxes; in-flow atomic inlines are deferred by paint_ifc,
            // and text boxes share their parent's style (they are never positioned themselves).
            let textual = matches!(cb.kind, BoxKind::Text(_) | BoxKind::LineBreak | BoxKind::Generated(_));
            if cb.style.is_positioned() && !cb.inline_level && !textual {
                self.defer(c, deferred);
            } else if matches!(cb.kind, BoxKind::InlineBox) {
                self.defer_positioned_in_inline(c, deferred);
            } else if cb.is_float && !textual {
                // A float inside an inline element (copy.sh's logo in a link).
                self.in_phase(0, |me| me.paint_float(c, deferred));
            }
        }
    }

    fn paint_block(&mut self, id: BoxId, deferred: &mut Vec<Deferred>) {
        let s = self.inp.tree.boxes[id].style.clone();
        // Elements creating stacking contexts are painted atomically.
        if s.opacity < 0.999 || s.transform.is_some() || s.filter.is_some() || s.clip_shape.is_some() || s.mask.is_some() || s.mix_blend != 0 || s.isolate || s.perspective.is_some() || s.preserve_3d || (s.backface_hidden && !self.ctx3d.is_empty()) {
            self.paint_stacking(id);
            return;
        }
        self.paint_self(id);
        self.paint_children(id, deferred);
        self.paint_outline(id);
    }

    /// Paint a box as one unit (its floats and inline content right after its
    /// own background), letting positioned descendants join the enclosing
    /// stacking context.
    fn paint_atomic(&mut self, id: BoxId, deferred: &mut Vec<Deferred>) {
        let s = self.inp.tree.boxes[id].style.clone();
        if s.opacity < 0.999 || s.transform.is_some() || s.filter.is_some() || s.z_index.is_some() || s.clip_shape.is_some() || s.mask.is_some() || s.mix_blend != 0 || s.isolate || s.perspective.is_some() || s.preserve_3d || (s.backface_hidden && !self.ctx3d.is_empty()) {
            self.paint_stacking(id);
            return;
        }
        let saved = std::mem::take(&mut self.items);
        self.phases.push([Vec::new(), Vec::new()]);
        self.hit_phases.push([Vec::new(), Vec::new()]);
        if let BoxKind::Replaced(_) = self.inp.tree.boxes[id].kind {
            // Replaced items: background, then the content itself.
        }
        self.paint_self(id);
        self.paint_children(id, deferred);
        let [floats, inline] = self.phases.pop().unwrap_or_default();
        let [hf, hi] = self.hit_phases.pop().unwrap_or_default();
        self.hits.extend(hf);
        self.hits.extend(hi);
        let mut content = std::mem::replace(&mut self.items, saved);
        content.extend(floats);
        content.extend(inline);
        for it in content {
            self.emit(it);
        }
        self.paint_outline(id);
    }

    fn paint_float(&mut self, id: BoxId, _deferred: &mut Vec<Deferred>) {
        self.paint_stacking(id);
    }

    // ------------------------------------------------------------ decorations

    fn paint_self(&mut self, id: BoxId) {
        let tree = self.inp.tree;
        let b = &tree.boxes[id];
        let s = b.style.clone();
        let r = self.border_rect(id);
        // The caret of an empty editing host sits at its content start.
        if let (Some(EditCaret::Empty(h)), true) = (self.inp.edit_caret, self.inp.caret_visible) {
            if b.node == Some(h) && b.pseudo == 0 {
                let ch = (s.font_size * 1.2).max(1.0);
                self.emit(Item::Rect { r: Rect::new(r.x + b.border[3] + b.padding[3], r.y + b.border[0] + b.padding[0], 1.5, ch), color: s.caret_color.unwrap_or(s.color) });
            }
        }
        if let Some(n) = b.node {
            let text_like = matches!(&b.kind, BoxKind::Replaced(ReplacedKind::TextInput) | BoxKind::Replaced(ReplacedKind::TextArea));
            self.hit(r, n, &s, text_like);
        } else if b.pseudo == 1 || b.pseudo == 2 {
            // Pseudo-elements hit-test as their originating element.
            if let Some(n) = b.parent.and_then(|p| tree.boxes[p].node) {
                self.hit(r, n, &s, false);
            }
        } else if b.pseudo == 5 {
            // A ::backdrop hit-tests as its top-layer element (the next box).
            if let Some(p) = b.parent {
                let kids = &tree.boxes[p].children;
                if let Some(n) = kids.iter().position(|&k| k == id).and_then(|i| kids.get(i + 1)).and_then(|&k| tree.boxes[k].node) {
                    self.hit(r, n, &s, false);
                }
            }
        }
        if s.visibility != Visibility::Visible {
            return;
        }
        if matches!(b.kind, BoxKind::InlineBox) {
            return;
        }
        if matches!(b.kind, BoxKind::Replaced(ReplacedKind::Checkbox) | BoxKind::Replaced(ReplacedKind::Radio)) && !s.appearance_none {
            return; // native look painted by paint_replaced
        }
        if b.tbl & crate::layout::table::TBL_NO_DECOR != 0 {
            return; // painted through the table's cells
        }
        if let Some(srcs) = b.cell_bgs.clone() {
            self.paint_table_bgs(&srcs, r);
        }
        // A table's border and background surround its grid, not its captions.
        let (ct, cb) = b.caption_space;
        let r = if ct != 0.0 || cb != 0.0 { Rect::new(r.x, r.y + ct, r.w, (r.h - ct - cb).max(0.0)) } else { r };
        self.paint_decorations(id, r, &s, [true, true]);
        // Column rules, centered in the gaps between used columns.
        if let Some(mc) = self.inp.tree.boxes[id].multicol.clone() {
            let (rw, rs, rc) = (s.column_rule.0, s.column_rule.1, s.column_rule.2.resolve(s.color));
            if rs.visible() && rw > 0.0 && rc.a > 0 {
                let b = &self.inp.tree.boxes[id];
                let (x0, y0) = (r.x + b.content_x(), r.y + b.content_y());
                for &(top, h, used) in &mc.sets {
                    for k in 1..used {
                        let x = x0 + k as f32 * (mc.col_w + mc.gap) - mc.gap / 2.0 - rw / 2.0;
                        let rr = Rect::new(x, y0 + top, rw, h);
                        self.emit(Item::Border { r: rr, widths: [0.0, 0.0, 0.0, rw], colors: [rc; 4], styles: [rs; 4], radii: Radii::ZERO });
                    }
                }
            }
        }
    }

    /// Backgrounds of a cell's column group, column, row group and row, in that
    /// order, painted in the cell's area (CSS 2.1 §17.5.1).
    fn paint_table_bgs(&mut self, srcs: &[Option<BoxId>; 4], r: Rect) {
        for &src in srcs.iter().flatten() {
            let s = self.inp.tree.boxes[src].style.clone();
            if s.visibility != Visibility::Visible {
                continue;
            }
            let bg = s.background_color.resolve(s.color);
            if bg.a > 0 {
                self.emit(Item::Rect { r, color: bg });
            }
            if let Some(layers) = s.background.clone() {
                self.paint_bg_layers(&layers, [r, r, r], Radii::ZERO, &s);
            }
        }
    }

    /// The resolved borders of a table in the collapsing model.
    fn paint_collapsed_borders(&mut self, id: BoxId) {
        let b = &self.inp.tree.boxes[id];
        let Some(segs) = b.collapsed.clone() else { return };
        if b.style.visibility != Visibility::Visible {
            return;
        }
        let o = self.border_rect(id);
        for sg in segs.iter() {
            let r = Rect::new(o.x + sg.x, o.y + sg.y, sg.w, sg.h);
            let widths = if sg.horizontal { [sg.width, 0.0, 0.0, 0.0] } else { [0.0, 0.0, 0.0, sg.width] };
            self.emit(Item::Border { r, widths, colors: [sg.color; 4], styles: [sg.style; 4], radii: Radii::ZERO });
        }
    }

    /// Background, shadows and borders for a rectangle (also used for inline fragments).
    fn paint_decorations(&mut self, id: BoxId, r: Rect, s: &ComputedStyle, sides: [bool; 2]) {
        let b = &self.inp.tree.boxes[id];
        // Fieldsets: border and background start at the legend's middle, and
        // the border is interrupted behind the legend.
        let mut legend_cut: Option<Rect> = None;
        let r = match b.legend {
            Some((lg, shift)) => {
                legend_cut = Some(self.border_rect(lg));
                Rect::new(r.x, r.y + shift, r.w, (r.h - shift).max(0.0))
            }
            None => r,
        };
        let mut radii = radii_px(s, r.w, r.h);
        // Chrome's themed controls round their corners even though the
        // computed border-radius stays 0.
        if !has_radius(&radii) && !s.appearance_none {
            if let Some(nr) = native_radius(b, s) {
                radii = Radii::all(nr);
            }
        }
        // Outer shadows.
        if let Some(sh) = &s.box_shadow {
            for shadow in sh.iter().rev() {
                if shadow.inset {
                    continue;
                }
                let c = shadow.color.resolve(s.color);
                if c.a == 0 {
                    continue;
                }
                let sr = Rect::new(r.x + shadow.x - shadow.spread, r.y + shadow.y - shadow.spread, r.w + 2.0 * shadow.spread, r.h + 2.0 * shadow.spread);
                let sradii = radii.map(|v| if v > 0.0 { (v + shadow.spread).max(0.0) } else { 0.0 });
                self.emit(Item::Shadow { r: sr, radii: sradii, blur: shadow.blur, spread: shadow.spread, color: c, inset: false, hole: Some((r, radii)) });
            }
        }
        let skip_bg = self.skip_bg == Some(id);
        // CSS masks with an image: tint the mask with the background (icon fonts' successor).
        let mask = s.mask.as_ref().filter(|m| !generic_mask(m, &self.inp.tree.boxes[id])).and_then(|m| match &m.image {
            BgImage::Url(u) => Some((m.clone(), self.inp.res.image_url(u))),
            _ => None,
        });
        if let Some((m, img)) = mask {
            if let Some(img) = img {
                let mut color = s.background_color.resolve(s.color);
                if color.a == 0 {
                    if let Some(layers) = &s.background {
                        if let Some(c) = layers.iter().find_map(|l| match &l.image {
                            BgImage::Gradient(g) => g.stops.first().map(|st| st.0),
                            _ => None,
                        }) {
                            color = c;
                        }
                    }
                }
                let (iw, ih) = img.natural();
                if color.a > 0 && iw > 0.0 && ih > 0.0 && !skip_bg {
                    let area = r;
                    let (tw, th) = match &m.size {
                        BgSize::Auto => (iw, ih),
                        BgSize::Cover => {
                            let k = (area.w / iw).max(area.h / ih);
                            (iw * k, ih * k)
                        }
                        BgSize::Contain => {
                            let k = (area.w / iw).min(area.h / ih);
                            (iw * k, ih * k)
                        }
                        BgSize::Explicit(w, h) => {
                            let wv = if w.is_auto() { None } else { Some(w.resolve(area.w)) };
                            let hv = if h.is_auto() { None } else { Some(h.resolve(area.h)) };
                            match (wv, hv) {
                                (Some(w), Some(h)) => (w, h),
                                (Some(w), None) => (w, w * ih / iw),
                                (None, Some(h)) => (h * iw / ih, h),
                                (None, None) => (iw, ih),
                            }
                        }
                    };
                    if tw >= 0.5 && th >= 0.5 {
                        let px = area.x + m.pos.0.resolve(area.w - tw);
                        let py = area.y + m.pos.1.resolve(area.h - th);
                        let tile = Rect::new(px, py, tw, th);
                        let mut a = area;
                        if !m.repeat.0 {
                            a = a.intersect(&Rect::new(px, a.y, tw, a.h));
                        }
                        if !m.repeat.1 {
                            a = a.intersect(&Rect::new(a.x, py, a.w, th));
                        }
                        if a.w > 0.0 && a.h > 0.0 {
                            self.emit(Item::MaskedColor { area: a, tile, img, color });
                        }
                    }
                }
            }
        } else if !skip_bg {
            let bg = s.background_color.resolve(s.color);
            let clip_text = s.background.as_ref().map(|l| l.iter().any(|x| x.clip_text)).unwrap_or(false);
            let pad_box = Rect::new(r.x + b.border[3], r.y + b.border[0], (r.w - b.border[1] - b.border[3]).max(0.0), (r.h - b.border[0] - b.border[2]).max(0.0));
            let content_box = Rect::new(pad_box.x + b.padding[3], pad_box.y + b.padding[0], (pad_box.w - b.padding[1] - b.padding[3]).max(0.0), (pad_box.h - b.padding[0] - b.padding[2]).max(0.0));
            let boxes = [r, pad_box, content_box];
            // background-blend-mode: the layers blend within an isolated group.
            let blend_group = s.bg_blend.is_some() && s.background.is_some() && !clip_text;
            if blend_group {
                self.emit(Item::PushBlend(0));
            }
            if bg.a > 0 && !clip_text {
                // The color is clipped by the bottom layer's background-clip.
                let ci = s.background.as_ref().and_then(|l| l.last()).map(|l| l.clip as usize).unwrap_or(0).min(2);
                let cr = boxes[ci];
                let cradii = if ci == 0 { radii } else { inner_radii(radii, &b.border, if ci == 2 { Some(&b.padding) } else { None }) };
                if has_radius(&cradii) {
                    self.emit(Item::RRect { r: cr, radii: cradii, color: bg });
                } else {
                    self.emit(Item::Rect { r: cr, color: bg });
                }
            }
            if let Some(layers) = s.background.clone() {
                if !clip_text {
                    self.paint_bg_layers(&layers, boxes, radii, s);
                }
            }
            if blend_group {
                self.emit(Item::PopBlend);
            }
        }
        // Inset shadows (approximated by a stroked inner ring).
        if let Some(sh) = &s.box_shadow {
            for shadow in sh.iter() {
                if !shadow.inset {
                    continue;
                }
                let c = shadow.color.resolve(s.color);
                if c.a == 0 {
                    continue;
                }
                let pad = Rect::new(r.x + b.border[3], r.y + b.border[0], (r.w - b.border[1] - b.border[3]).max(0.0), (r.h - b.border[0] - b.border[2]).max(0.0));
                let pr = inner_radii(radii, &b.border, None);
                let hole = Rect::new(pad.x + shadow.x + shadow.spread, pad.y + shadow.y + shadow.spread, pad.w - 2.0 * shadow.spread, pad.h - 2.0 * shadow.spread);
                let hr = pr.map(|v| (v - shadow.spread).max(0.0));
                self.emit(Item::InsetShadow { r: pad, radii: pr, hole, hole_radii: hr, blur: shadow.blur, color: c });
            }
        }
        // Borders.
        if b.tbl & crate::layout::table::TBL_NO_BORDER != 0 {
            return; // the table paints the collapsed borders
        }
        if let Some(bi) = s.border_image.clone() {
            let bw = [s.bw(0), s.bw(1), s.bw(2), s.bw(3)];
            if self.paint_border_image(&bi, r, bw) {
                return;
            }
        }
        let widths = [s.bw(0), if sides[1] { s.bw(1) } else { 0.0 }, s.bw(2), if sides[0] { s.bw(3) } else { 0.0 }];
        // Native form controls keep Chrome's UA metrics (2px inset/outset)
        // but are drawn, like Chrome does, with a thin themed outline.
        let native_control = !s.appearance_none
            && (s.border_style.iter().all(|st| matches!(st, BorderStyle::Inset | BorderStyle::Outset)) || matches!(b.kind, BoxKind::Replaced(ReplacedKind::ColorWell)))
            && b.node.map(|n| matches!(self.inp.doc.tag(n), "input" | "button" | "select" | "textarea")).unwrap_or(false);
        if native_control && widths.iter().all(|w| *w > 0.0) {
            let mut c = s.border_color_at(0);
            if matches!(b.kind, BoxKind::Replaced(ReplacedKind::ColorWell)) && c.a == 255 {
                c = Color::rgb(118, 118, 118);
            }
            if c.a > 0 {
                self.emit(Item::Border { r, widths: [1.0; 4], colors: [c; 4], styles: [BorderStyle::Solid; 4], radii });
            }
        } else if widths.iter().any(|w| *w > 0.0) {
            let colors = [s.border_color_at(0), s.border_color_at(1), s.border_color_at(2), s.border_color_at(3)];
            if colors.iter().any(|c| c.a > 0) {
                let cut = legend_cut.and_then(|lr| {
                    // A clip with a hole (wound the other way) where the legend is.
                    let o = r.inflate(4.0);
                    let mut pb = tiny_skia::PathBuilder::new();
                    pb.push_rect(tiny_skia::Rect::from_xywh(o.x, o.y, o.w, o.h)?);
                    pb.move_to(lr.x, lr.y);
                    pb.line_to(lr.x, lr.bottom());
                    pb.line_to(lr.right(), lr.bottom());
                    pb.line_to(lr.right(), lr.y);
                    pb.close();
                    pb.finish()
                });
                if let Some(p) = &cut {
                    self.emit(Item::PushClipPath(Arc::new(p.clone())));
                }
                self.emit(Item::Border { r, widths, colors, styles: s.border_style, radii });
                if cut.is_some() {
                    self.emit(Item::PopClip);
                }
            }
        }
    }

    /// `border-image`: nine-slice the image over the border image area.
    /// Returns false when the image isn't available (the normal border paints).
    fn paint_border_image(&mut self, bi: &crate::css::style::BorderImage, r: Rect, bw: [f32; 4]) -> bool {
        // Border image area: the border box grown by the outsets.
        let o: Vec<f32> = (0..4).map(|k| bi.outset_mult[k].map(|m| m * bw[k]).unwrap_or(bi.outset[k])).collect();
        let area = Rect::new(r.x - o[3], r.y - o[0], r.w + o[1] + o[3], r.h + o[0] + o[2]);
        if area.w <= 0.0 || area.h <= 0.0 {
            return true;
        }
        // The image and its size (gradients are sized to the area).
        let (img, iw, ih) = match &bi.source {
            BgImage::Url(u) => match self.inp.res.image_url(u) {
                Some(i) => {
                    let (w, h) = i.natural();
                    (Some(i), w, h)
                }
                None => return false,
            },
            BgImage::Gradient(_) => (None, area.w, area.h),
            BgImage::None => return false,
        };
        if iw <= 0.0 || ih <= 0.0 {
            return false;
        }
        let sl: Vec<f32> = (0..4).map(|k| {
            let (v, pct) = bi.slice[k];
            let dim = if k % 2 == 0 { ih } else { iw };
            (if pct { v / 100.0 * dim } else { v }).min(dim)
        }).collect();
        let mut wd: Vec<f32> = (0..4).map(|k| match (&bi.width[k], bi.width_mult[k]) {
            (Some(l), _) => l.resolve(if k % 2 == 0 { area.h } else { area.w }),
            (None, Some(m)) => m * bw[k],
            (None, None) => sl[k],
        }).collect();
        let f = (area.w / (wd[1] + wd[3]).max(0.001)).min(area.h / (wd[0] + wd[2]).max(0.001)).min(1.0);
        for v in wd.iter_mut() {
            *v *= f;
        }
        let inner = Rect::new(area.x + wd[3], area.y + wd[0], (area.w - wd[1] - wd[3]).max(0.0), (area.h - wd[0] - wd[2]).max(0.0));
        if let BgImage::Gradient(g) = &bi.source {
            // A gradient drawn across the area, kept to the border ring
            // (the whole area with `fill`).
            let Some(spec) = resolve_gradient(g, area) else { return true };
            let ring = if bi.fill { None } else {
                let mut pb = tiny_skia::PathBuilder::new();
                if let Some(rr) = tiny_skia::Rect::from_xywh(area.x, area.y, area.w, area.h) {
                    pb.push_rect(rr);
                }
                pb.move_to(inner.x, inner.y);
                pb.line_to(inner.x, inner.bottom());
                pb.line_to(inner.right(), inner.bottom());
                pb.line_to(inner.right(), inner.y);
                pb.close();
                pb.finish()
            };
            if let Some(p) = &ring {
                self.emit(Item::PushClipPath(Arc::new(p.clone())));
            }
            self.emit(Item::Gradient { r: area, grad: Arc::new(spec), radii: Radii::ZERO });
            if ring.is_some() {
                self.emit(Item::PopClip);
            }
            return true;
        }
        let img = img.unwrap();
        let (sx, sy) = (img.width as f32 / iw, img.height as f32 / ih);
        let crop = |x: f32, y: f32, w: f32, h: f32| -> Option<Arc<ImageData>> {
            let (px, py) = ((x * sx).round() as i32, (y * sy).round() as i32);
            let (pw, ph) = (((x + w) * sx).round() as i32 - px, ((y + h) * sy).round() as i32 - py);
            if pw <= 0 || ph <= 0 {
                return None;
            }
            let rect = tiny_skia::IntRect::from_xywh(px, py, pw as u32, ph as u32)?;
            let pm = img.pixmap.clone_rect(rect)?;
            Some(Arc::new(ImageData { width: pw as u32, height: ph as u32, pixmap: pm, density: 1.0, nat: None, svg_par: None }))
        };
        let (cw, ch) = ((iw - sl[1] - sl[3]).max(0.0), (ih - sl[0] - sl[2]).max(0.0));
        // Corners.
        let corners = [
            (0.0, 0.0, sl[3], sl[0], Rect::new(area.x, area.y, wd[3], wd[0])),
            (iw - sl[1], 0.0, sl[1], sl[0], Rect::new(inner.right(), area.y, wd[1], wd[0])),
            (iw - sl[1], ih - sl[2], sl[1], sl[2], Rect::new(inner.right(), inner.bottom(), wd[1], wd[2])),
            (0.0, ih - sl[2], sl[3], sl[2], Rect::new(area.x, inner.bottom(), wd[3], wd[2])),
        ];
        for (x, y, w, h, d) in corners {
            if d.w > 0.0 && d.h > 0.0 {
                if let Some(c) = crop(x, y, w, h) {
                    self.emit(Item::Image { r: d, img: c, radii: None, pixelated: false });
                }
            }
        }
        // Edges and the middle, stretched or tiled.
        let tile = |me: &mut Self, src: (f32, f32, f32, f32), dest: Rect, rx: u8, ry: u8, kx: f32, ky: f32| {
            if dest.w <= 0.0 || dest.h <= 0.0 {
                return;
            }
            let Some(c) = crop(src.0, src.1, src.2, src.3) else { return };
            let (mut tw, mut th) = (if rx == 0 { dest.w } else { src.2 * kx }, if ry == 0 { dest.h } else { src.3 * ky });
            if tw <= 0.01 || th <= 0.01 {
                return;
            }
            if rx >= 2 {
                tw = dest.w / (dest.w / tw).round().max(1.0);
            }
            if ry >= 2 {
                th = dest.h / (dest.h / th).round().max(1.0);
            }
            let tx = if rx == 1 { dest.x + (dest.w - tw) / 2.0 } else { dest.x };
            let ty = if ry == 1 { dest.y + (dest.h - th) / 2.0 } else { dest.y };
            if rx == 0 && ry == 0 {
                me.emit(Item::Image { r: dest, img: c, radii: None, pixelated: false });
            } else {
                me.emit(Item::Pattern { area: dest, tile: Rect::new(tx, ty, tw, th), img: c, repeat_x: rx != 0, repeat_y: ry != 0, fixed: false });
            }
        };
        let (rh, rv) = bi.repeat;
        // Scale of the edge tiles: the edge's thickness over the slice's.
        let k_top = if sl[0] > 0.0 { wd[0] / sl[0] } else { 1.0 };
        let k_bot = if sl[2] > 0.0 { wd[2] / sl[2] } else { 1.0 };
        let k_left = if sl[3] > 0.0 { wd[3] / sl[3] } else { 1.0 };
        let k_right = if sl[1] > 0.0 { wd[1] / sl[1] } else { 1.0 };
        tile(self, (sl[3], 0.0, cw, sl[0]), Rect::new(inner.x, area.y, inner.w, wd[0]), rh, 0, k_top, k_top);
        tile(self, (sl[3], ih - sl[2], cw, sl[2]), Rect::new(inner.x, inner.bottom(), inner.w, wd[2]), rh, 0, k_bot, k_bot);
        tile(self, (0.0, sl[0], sl[3], ch), Rect::new(area.x, inner.y, wd[3], inner.h), 0, rv, k_left, k_left);
        tile(self, (iw - sl[1], sl[0], sl[1], ch), Rect::new(inner.right(), inner.y, wd[1], inner.h), 0, rv, k_right, k_right);
        if bi.fill {
            tile(self, (sl[3], sl[0], cw, ch), inner, rh, rv, k_top, k_left);
        }
        true
    }

    fn paint_outline(&mut self, id: BoxId) {
        let b = &self.inp.tree.boxes[id];
        let s = b.style.clone();
        if !s.outline_style.visible() || s.outline_width <= 0.0 || s.visibility != Visibility::Visible {
            return;
        }
        let r = self.border_rect(id);
        let o = s.outline_offset + s.outline_width;
        let rr = r.inflate(o);
        let c = s.outline_color.resolve(s.color);
        let w = s.outline_width;
        let radii = radii_px(&s, r.w, r.h).map(|v| if v > 0.0 { v + o } else { 0.0 });
        self.emit(Item::Border { r: rr, widths: [w; 4], colors: [c; 4], styles: [s.outline_style; 4], radii });
    }

    fn paint_bg_layers(&mut self, layers: &Rc<Vec<BgLayer>>, boxes: [Rect; 3], radii: Radii, s: &ComputedStyle) {
        let vp = self.inp.tree.viewport;
        let blends = s.bg_blend.clone().filter(|_| s.background.as_ref().map(|b| Rc::ptr_eq(b, layers)).unwrap_or(false));
        let mut open_blend = false;
        for (li, layer) in layers.iter().enumerate().rev() {
            if open_blend {
                self.emit(Item::PopBlend);
                open_blend = false;
            }
            if let Some(m) = blends.as_ref().map(|b| b[li % b.len()]).filter(|&m| m != 0) {
                self.emit(Item::PushBlend(m));
                open_blend = true;
            }
            // Painting area (background-clip) and positioning area (background-origin,
            // or the viewport for `background-attachment: fixed`).
            let border_box = boxes[(layer.clip as usize).min(2)];
            let pad_box = if layer.fixed { Rect::new(0.0, 0.0, vp.0, vp.1) } else { boxes[(layer.origin as usize).min(2)] };
            let radii = if layer.clip == 0 { radii } else { Radii::ZERO };
            match &layer.image {
                BgImage::None => {}
                BgImage::Gradient(g) => {
                    // Gradients have no intrinsic size: the tile defaults to the positioning area.
                    let area = pad_box;
                    let (tw, th) = match &layer.size {
                        BgSize::Explicit(w, h) => (if w.is_auto() { area.w } else { w.resolve(area.w) }, if h.is_auto() { area.h } else { h.resolve(area.h) }),
                        _ => (area.w, area.h),
                    };
                    if tw < 0.5 || th < 0.5 {
                        continue;
                    }
                    let (tw, th) = round_tiles(layer, area, tw, th);
                    let (px, py, rep, gap) = place_tiles(layer, area, tw, th);
                    if gap.0 > 0.0 || gap.1 > 0.0 {
                        let tiles = spaced_tiles(border_box, Rect::new(px, py, tw, th), rep, gap);
                        self.emit(Item::PushClip { r: border_box, radii: if has_radius(&radii) { Some(radii) } else { None } });
                        for t in tiles {
                            if let Some(spec) = resolve_gradient(g, t) {
                                self.emit(Item::Gradient { r: t, grad: Arc::new(spec), radii: Radii::ZERO });
                            }
                        }
                        self.emit(Item::PopClip);
                        continue;
                    }
                    let whole = (tw - area.w).abs() < 0.5 && (th - area.h).abs() < 0.5 && (px - area.x).abs() < 0.5 && (py - area.y).abs() < 0.5;
                    if whole {
                        if let Some(spec) = resolve_gradient(g, border_box) {
                            self.emit(Item::Gradient { r: border_box, grad: Arc::new(spec), radii });
                        }
                        continue;
                    }
                    let tile = Rect::new(px, py, tw, th);
                    let rounded = has_radius(&radii);
                    if rounded {
                        self.emit(Item::PushClip { r: border_box, radii: Some(radii) });
                    }
                    if !rep.0 && !rep.1 {
                        if let Some(spec) = resolve_gradient(g, tile) {
                            self.emit(Item::PushClip { r: border_box, radii: None });
                            self.emit(Item::Gradient { r: tile, grad: Arc::new(spec), radii: Radii::ZERO });
                            self.emit(Item::PopClip);
                        }
                    } else {
                        let mut a = border_box;
                        if !rep.0 {
                            a = a.intersect(&Rect::new(px, a.y, tw, a.h));
                        }
                        if !rep.1 {
                            a = a.intersect(&Rect::new(a.x, py, a.w, th));
                        }
                        if a.w > 0.0 && a.h > 0.0 {
                            if let Some(spec) = resolve_gradient(g, Rect::new(0.0, 0.0, tw, th)) {
                                self.emit(Item::GradientTiles { area: a, tile, grad: Arc::new(spec) });
                            }
                        }
                    }
                    if rounded {
                        self.emit(Item::PopClip);
                    }
                }
                BgImage::Url(u) => {
                    let img = match self.inp.res.image_url(u) {
                        Some(i) => i,
                        None => continue,
                    };
                    let (iw, ih) = img.natural();
                    if iw <= 0.0 || ih <= 0.0 {
                        continue;
                    }
                    let area = pad_box;
                    let (tw, th) = match &layer.size {
                        BgSize::Auto => (iw, ih),
                        BgSize::Cover => {
                            let k = (area.w / iw).max(area.h / ih);
                            (iw * k, ih * k)
                        }
                        BgSize::Contain => {
                            let k = (area.w / iw).min(area.h / ih);
                            (iw * k, ih * k)
                        }
                        BgSize::Explicit(w, h) => {
                            let wv = if w.is_auto() { None } else { Some(w.resolve(area.w)) };
                            let hv = if h.is_auto() { None } else { Some(h.resolve(area.h)) };
                            match (wv, hv) {
                                (Some(w), Some(h)) => (w, h),
                                (Some(w), None) => (w, w * ih / iw),
                                (None, Some(h)) => (h * iw / ih, h),
                                (None, None) => (iw, ih),
                            }
                        }
                    };
                    if tw < 0.5 || th < 0.5 {
                        continue;
                    }
                    let (tw, th) = round_tiles(layer, area, tw, th);
                    let (px, py, rep, gap) = place_tiles(layer, area, tw, th);
                    let tile = Rect::new(px, py, tw, th);
                    if (gap.0 > 0.0 || gap.1 > 0.0) && !layer.fixed {
                        let tiles = spaced_tiles(border_box, tile, rep, gap);
                        self.emit(Item::PushClip { r: border_box, radii: if has_radius(&radii) { Some(radii) } else { None } });
                        for t in tiles {
                            self.emit(Item::Image { r: t, img: img.clone(), radii: None, pixelated: s.pixelated });
                        }
                        self.emit(Item::PopClip);
                        continue;
                    }
                    if layer.fixed {
                        // Tile is in viewport coordinates; the rasterizer adds the scroll offset.
                        let rounded = has_radius(&radii);
                        if rounded {
                            self.emit(Item::PushClip { r: border_box, radii: Some(radii) });
                        }
                        let fixed_off = if self.in_fixed { 0.0 } else { 1.0 };
                        self.emit(Item::Pattern { area: border_box, tile, img, repeat_x: rep.0, repeat_y: rep.1, fixed: fixed_off > 0.0 });
                        if rounded {
                            self.emit(Item::PopClip);
                        }
                        continue;
                    }
                    if !rep.0 && !rep.1 {
                        let clip = border_box;
                        let vis = tile.intersect(&clip);
                        if vis.w <= 0.0 || vis.h <= 0.0 {
                            continue;
                        }
                        if vis == tile {
                            self.emit(Item::Image { r: tile, img, radii: if has_radius(&radii) { Some(radii) } else { None }, pixelated: s.pixelated });
                        } else {
                            let rounded = has_radius(&radii);
                            if rounded {
                                self.emit(Item::PushClip { r: border_box, radii: Some(radii) });
                            }
                            self.emit(Item::Pattern { area: vis, tile, img, repeat_x: false, repeat_y: false, fixed: false });
                            if rounded {
                                self.emit(Item::PopClip);
                            }
                        }
                    } else {
                        let mut a = border_box;
                        if !rep.0 {
                            a = a.intersect(&Rect::new(px, a.y, tw, a.h));
                        }
                        if !rep.1 {
                            a = a.intersect(&Rect::new(a.x, py, a.w, th));
                        }
                        if a.w > 0.0 && a.h > 0.0 {
                            let rounded = has_radius(&radii);
                            if rounded {
                                self.emit(Item::PushClip { r: border_box, radii: Some(radii) });
                            }
                            self.emit(Item::Pattern { area: a, tile, img, repeat_x: rep.0, repeat_y: rep.1, fixed: false });
                            if rounded {
                                self.emit(Item::PopClip);
                            }
                        }
                    }
                }
            }
        }
        if open_blend {
            self.emit(Item::PopBlend);
        }
    }

    // ---------------------------------------------------------- inline content

    /// The combined opacity of the inline boxes between a fragment's box and
    /// its inline formatting context (`opacity` on a <span> applies to its text).
    fn inline_opacity(&self, mut b: BoxId, ifc: BoxId, include_self: bool) -> f32 {
        let tree = self.inp.tree;
        let mut op = 1.0f32;
        if !include_self {
            match tree.boxes[b].parent {
                Some(p) => b = p,
                None => return 1.0,
            }
        }
        for _ in 0..64 {
            if b == ifc {
                break;
            }
            op *= tree.boxes[b].style.opacity;
            match tree.boxes[b].parent {
                Some(p) => b = p,
                None => break,
            }
        }
        op
    }

    fn paint_ifc(&mut self, id: BoxId, deferred: &mut Vec<Deferred>) {
        let tree = self.inp.tree;
        let b = &tree.boxes[id];
        let ifc = b.ifc.as_ref().unwrap();
        let ox = b.abs_x + b.content_x() - self.off.0;
        let oy = b.abs_y + b.content_y() - self.off.1;
        // Cull: visible range is unknown here; emit everything (rasterizer culls).
        // Inline box decorations.
        for d in &ifc.decos {
            let db = &tree.boxes[d.box_id];
            let s = db.style.clone();
            let r = Rect::new(ox + d.x0, oy + d.y, (d.x1 - d.x0).max(0.0), d.h);
            if let Some(n) = db.node {
                self.hit(r, n, &s, false);
            }
            if s.visibility == Visibility::Visible {
                let op = self.inline_opacity(d.box_id, id, true);
                if op > 0.001 {
                    if op < 0.999 {
                        self.emit(Item::PushOpacity(op));
                    }
                    self.paint_decorations(d.box_id, r, &s, [d.first, d.last]);
                    if op < 0.999 {
                        self.emit(Item::PopOpacity);
                    }
                }
            }
        }
        for (li, line) in ifc.lines.iter().enumerate() {
            for (fi, f) in line.frags.iter().enumerate() {
                match &f.kind {
                    FragKind::Text { pieces, text, starts } => {
                        if let Some(sp) = self.inp.selection {
                            if let Some((a, b)) = sp.frag_range(id, li, fi) {
                                let x0 = f.x_of_byte(a);
                                let x1 = if b == usize::MAX { f.w } else { f.x_of_byte(b) };
                                if x1 > x0 {
                                    self.emit(Item::Rect { r: Rect::new(ox + f.x + x0, oy + line.y, x1 - x0, line.h), color: Color::rgba(51, 144, 255, 80) });
                                }
                            }
                        }
                        let tb = &tree.boxes[f.box_id];
                        let s = tb.style.clone();
                        let fr = Rect::new(ox + f.x, oy + f.y, f.w, f.h);
                        if let Some(n) = tb.node {
                            self.hit(fr, n, &s, true);
                        } else if let Some(n) = self.originating_node(f.box_id) {
                            self.hit(fr, n, &s, true);
                        }
                        if s.visibility != Visibility::Visible {
                            continue;
                        }
                        let op = self.inline_opacity(f.box_id, id, true);
                        if op <= 0.001 {
                            continue;
                        }
                        if op < 0.999 {
                            self.emit(Item::PushOpacity(op));
                        }
                        let baseline = oy + f.y + f.ascent;
                        // MathML stretchy fences grow to the height of their row.
                        let stretch = f.h > 0.0 && line.h > f.h * 1.3 && tb.node.map(|n| is_stretchy_fence(self.inp.doc, n, text)).unwrap_or(false);
                        if stretch {
                            let k = line.h * 0.92 / f.h;
                            let cy_text = oy + f.y + f.h / 2.0;
                            let cy_line = oy + line.y + line.h / 2.0;
                            self.emit(Item::PushTransform([1.0, 0.0, 0.0, k, 0.0, cy_line - k * cy_text]));
                        }
                        self.paint_text_v(&s, ox + f.x, baseline, pieces, f.w, text, Some(starts), tb.node);
                        if stretch {
                            self.emit(Item::PopTransform);
                        }
                        if op < 0.999 {
                            self.emit(Item::PopOpacity);
                        }
                        if let Some(EditCaret::At(c)) = self.inp.edit_caret {
                            if c.ifc == id && c.line == li && c.frag == fi && self.inp.caret_visible {
                                let cx = ox + f.x + f.x_of_byte(c.byte.min(text.len()));
                                self.emit(Item::Rect { r: Rect::new(cx.round() - 0.5, oy + line.y, 1.5, line.h), color: s.caret_color.unwrap_or(s.color) });
                            }
                        }
                    }
                    FragKind::Ellipsis(sh) => {
                        // Ellipsis (the block's style) or a soft-hyphen hyphen (the text's).
                        let s = tree.boxes[f.box_id].style.clone();
                        let baseline = oy + f.y + f.ascent;
                        let t: Rc<str> = Rc::from(if f.box_id == id { "…" } else { "-" });
                        self.paint_text(&s, ox + f.x, baseline, &[(sh.clone(), 0.0)], f.w, &t, None);
                    }
                    FragKind::Atomic => {
                        let ab = &tree.boxes[f.box_id];
                        if ab.style.is_positioned() {
                            self.defer(f.box_id, deferred);
                        } else {
                            let op = self.inline_opacity(f.box_id, id, false);
                            if op <= 0.001 {
                                continue;
                            }
                            if op < 0.999 {
                                self.emit(Item::PushOpacity(op));
                            }
                            self.paint_stacking(f.box_id);
                            if op < 0.999 {
                                self.emit(Item::PopOpacity);
                            }
                        }
                    }
                }
            }
        }
    }

    fn originating_node(&self, mut b: BoxId) -> Option<NodeId> {
        let tree = self.inp.tree;
        for _ in 0..64 {
            if let Some(n) = tree.boxes[b].node {
                return Some(n);
            }
            b = tree.boxes[b].parent?;
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_text(&mut self, s: &ComputedStyle, x: f32, baseline: f32, pieces: &[(Rc<crate::text::ShapedText>, f32)], width: f32, text: &Rc<str>, node: Option<NodeId>) {
        self.paint_text_v(s, x, baseline, pieces, width, text, None, node)
    }

    /// Glyphs of vertical text: upright ones (CJK with `text-orientation:
    /// mixed`, everything with `upright`) are turned back to stand up in the
    /// rotated frame; in vertical-lr (a transposed frame) sideways glyphs are
    /// mirrored back across the baseline. Returns false when the text isn't vertical.
    fn paint_vertical_glyphs(&mut self, s: &ComputedStyle, x: f32, baseline: f32, pieces: &[(Rc<crate::text::ShapedText>, f32)], text: &Rc<str>, starts: Option<&[u32]>, color: Color) -> bool {
        let Some(&mode) = self.wm.last() else { return false };
        let orient = if matches!(mode, 3 | 4) { 2 } else { s.text_orientation };
        if mode != 2 && orient == 2 {
            return false;
        }
        let em = s.font_size;
        // text-combine-upright: the whole (1em) run stands up as one unit.
        if s.text_combine && orient != 2 {
            let mut runs: Vec<(Arc<crate::text::Font>, f32, Vec<(u16, f32, f32)>)> = Vec::new();
            let mut w = 0.0f32;
            let mut asc_desc = (em * 0.8, em * 0.2);
            for (sh, off) in pieces {
                let mut px = *off;
                for r in &sh.runs {
                    let sc = r.font.scale(r.size);
                    asc_desc = (r.font.ascent * sc, r.font.descent * sc);
                    runs.push((r.font.clone(), r.size, r.glyphs.iter().map(|g| (g.id, px + g.x, g.y)).collect()));
                    px += r.width;
                }
                w = w.max(px);
            }
            let cx = x + w / 2.0;
            let cy = baseline - (asc_desc.0 - asc_desc.1) / 2.0;
            let m = if mode == 2 { [0.0, 1.0, 1.0, 0.0, cx - cy, cy - cx] } else { [0.0, -1.0, 1.0, 0.0, cx - cy, cy + cx] };
            self.emit(Item::PushTransform(m));
            for (font, size, glyphs) in runs {
                self.emit(Item::Text { x, y: baseline, run: Arc::new(GlyphRun { font, size, glyphs }), color });
            }
            self.emit(Item::PopTransform);
            return true;
        }
        // (font, size, glyph, cell x, y, advance, upright, char, substituted)
        let mut glyphs: Vec<(Arc<crate::text::Font>, f32, u16, f32, f32, f32, bool, char, bool)> = Vec::new();
        for (k, (sh, off)) in pieces.iter().enumerate() {
            let start = starts.and_then(|st| st.get(k)).map(|v| *v as usize);
            let mut px = *off;
            for r in &sh.runs {
                for g in &r.glyphs {
                    let ch = start.and_then(|st| text.get(st + g.cluster as usize..)).and_then(|t| t.chars().next()).unwrap_or(' ');
                    // A vertical alternate from the font's `vert` feature is drawn upright as is.
                    let subst = r.font.face().glyph_index(ch).map(|gi| gi.0 != g.id).unwrap_or(false);
                    let up = match orient {
                        1 => !ch.is_whitespace(),
                        2 => false,
                        _ => upright_char(ch) || subst && !ch.is_ascii(),
                    };
                    glyphs.push((r.font.clone(), r.size, g.id, px + g.x, g.y, g.advance, up, ch, subst));
                }
                px += r.width;
            }
        }
        if !glyphs.iter().any(|g| g.6) && mode != 2 {
            return false;
        }
        let mut i = 0;
        while i < glyphs.len() {
            if !glyphs[i].6 {
                // A run of sideways glyphs sharing a font.
                let (font, size) = (glyphs[i].0.clone(), glyphs[i].1);
                let mut run = Vec::new();
                while i < glyphs.len() && !glyphs[i].6 && glyphs[i].0.uid == font.uid && glyphs[i].1 == size {
                    run.push((glyphs[i].2, glyphs[i].3, glyphs[i].4));
                    i += 1;
                }
                if mode == 2 {
                    self.emit(Item::PushTransform([1.0, 0.0, 0.0, -1.0, 0.0, 2.0 * baseline]));
                }
                self.emit(Item::Text { x, y: baseline, run: Arc::new(GlyphRun { font, size, glyphs: run }), color });
                if mode == 2 {
                    self.emit(Item::PopTransform);
                }
                continue;
            }
            let (font, size, gid, gx, gy, adv, _, ch, subst) = glyphs[i].clone();
            i += 1;
            let sc = font.scale(size);
            let (asc, desc) = (font.ascent * sc, font.descent * sc);
            // The glyph's own width, centered in its cell along the line.
            let hw = font.face().glyph_hor_advance(rustybuzz::ttf_parser::GlyphId(gid)).map(|a| a as f32 * sc).unwrap_or(adv);
            let cx = x + gx + adv / 2.0;
            let cy = baseline - (asc - desc) / 2.0;
            let ox = x + gx + (adv - hw) / 2.0;
            let m = if mode == 2 { [0.0, 1.0, 1.0, 0.0, cx - cy, cy - cx] } else { [0.0, -1.0, 1.0, 0.0, cx - cy, cy + cx] };
            // Without a vertical alternate, the ideographic full stop and comma
            // move to the upper right corner of their cell.
            let (dx, dy) = if !subst && matches!(ch, '\u{3001}' | '\u{3002}' | '\u{FF0C}' | '\u{FF0E}') { (0.55 * em, -0.55 * em) } else { (0.0, 0.0) };
            self.emit(Item::PushTransform(m));
            self.emit(Item::Text { x: ox + dx, y: baseline + dy, run: Arc::new(GlyphRun { font, size, glyphs: vec![(gid, 0.0, gy)] }), color });
            self.emit(Item::PopTransform);
        }
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_text_v(&mut self, s: &ComputedStyle, x: f32, baseline: f32, pieces: &[(Rc<crate::text::ShapedText>, f32)], width: f32, _text: &Rc<str>, starts: Option<&[u32]>, node: Option<NodeId>) {
        let color = s.color;
        // Find-in-page highlights.
        if let Some(n) = node {
            if self.inp.find.iter().any(|(fnode, _, _)| *fnode == n) {
                let h = s.font_size * 1.2;
                self.emit(Item::Rect { r: Rect::new(x, baseline - s.font_size, width, h), color: Color::rgba(255, 210, 0, 160) });
            }
        }
        // Background-clip: text → paint gradient through glyphs is unsupported; use first stop color.
        let color = if let Some(layers) = &s.background {
            if layers.iter().any(|l| l.clip_text) {
                layers
                    .iter()
                    .find_map(|l| match &l.image {
                        BgImage::Gradient(g) => g.stops.first().map(|s| s.0),
                        _ => None,
                    })
                    .unwrap_or(color)
            } else {
                color
            }
        } else {
            color
        };
        let mut runs: Vec<(Arc<crate::text::Font>, f32, Vec<(u16, f32, f32)>)> = Vec::new();
        for (sh, off) in pieces {
            let mut px = *off;
            for r in &sh.runs {
                let entry = match runs.iter_mut().find(|e| e.0.uid == r.font.uid && e.1 == r.size) {
                    Some(e) => e,
                    None => {
                        runs.push((r.font.clone(), r.size, Vec::new()));
                        runs.last_mut().unwrap()
                    }
                };
                for g in &r.glyphs {
                    entry.2.push((g.id, px + g.x, g.y));
                }
                px += r.width;
            }
        }
        // Text shadows first.
        if let Some(shadows) = &s.text_shadow {
            for sh in shadows.iter().rev() {
                let c = sh.color.resolve(s.color);
                if c.a == 0 {
                    continue;
                }
                let c = if sh.blur > 0.0 { c.with_alpha_mul(0.6) } else { c };
                for (font, size, glyphs) in &runs {
                    self.emit(Item::Text { x: x + sh.x, y: baseline + sh.y, run: Arc::new(GlyphRun { font: font.clone(), size: *size, glyphs: glyphs.clone() }), color: c });
                }
            }
        }
        if !self.wm.is_empty() && self.paint_vertical_glyphs(s, x, baseline, pieces, _text, starts, color) {
            runs.clear();
        }
        for (font, size, glyphs) in runs {
            if glyphs.is_empty() {
                continue;
            }
            self.emit(Item::Text { x, y: baseline, run: Arc::new(GlyphRun { font, size, glyphs }), color: color_mul(color, 1.0) });
        }
        // Decorations.
        let lines = s.text_decoration_line | s.deco_propagated;
        if lines != 0 && width > 0.0 {
            let font = self.inp.fonts.primary(&s.font_family, s.font_weight, s.font_style);
            let sc = font.scale(s.font_size);
            let thick = s.text_decoration_thickness.unwrap_or((font.underline_thickness * sc).max(1.0));
            let dc = if s.text_decoration_line != 0 { s.text_decoration_color.resolve(s.color) } else { s.deco_propagated_color };
            let style = s.text_decoration_style;
            let mut line = |y: f32| {
                self.emit(Item::Line { x0: x, y0: y, x1: x + width, y1: y, width: thick, color: dc, style });
            };
            if lines & DECO_UNDERLINE != 0 {
                line(baseline + (font.underline_pos * sc).max(1.0) + thick / 2.0);
            }
            if lines & DECO_OVERLINE != 0 {
                line(baseline - font.ascent * sc);
            }
            if lines & DECO_LINE_THROUGH != 0 {
                line(baseline - font.x_height * sc / 2.0);
            }
        }
    }

    fn paint_marker(&mut self, id: BoxId, m: &crate::layout::Marker) {
        let b = &self.inp.tree.boxes[id];
        let s = m.style.clone().unwrap_or_else(|| b.style.clone());
        if s.visibility != Visibility::Visible {
            return;
        }
        let x = b.abs_x + m.x - self.off.0;
        let y = b.abs_y + m.baseline - self.off.1;
        if let Some((img, iw, ih)) = &m.image {
            self.emit(Item::Image { r: Rect::new(x, y - ih, *iw, *ih), img: img.clone(), radii: None, pixelated: false });
        } else if let Some(bullet) = m.bullet {
            let sz = s.font_size * 0.35;
            let cy = y - s.font_size * 0.32;
            let r = Rect::new(x, cy - sz / 2.0, sz, sz);
            match bullet {
                '•' => self.emit(Item::RRect { r, radii: Radii::all(sz / 2.0), color: s.color }),
                '◦' => self.emit(Item::Border { r, widths: [1.0; 4], colors: [s.color; 4], styles: [BorderStyle::Solid; 4], radii: Radii::all(sz / 2.0) }),
                _ => self.emit(Item::Rect { r, color: s.color }),
            }
        } else if let Some(sh) = &m.shaped {
            self.paint_text(&s, x, y, &[(sh.clone(), 0.0)], sh.width, &Rc::from(m.text.as_str()), None);
        }
    }

    // --------------------------------------------------------------- replaced

    fn content_rect(&self, id: BoxId) -> Rect {
        let b = &self.inp.tree.boxes[id];
        let r = self.border_rect(id);
        Rect::new(r.x + b.content_x(), r.y + b.content_y(), b.content_w(), b.content_h())
    }

    fn paint_label(&mut self, s: &ComputedStyle, text: &str, area: Rect, align_center: bool, color: Color) {
        if text.is_empty() {
            return;
        }
        let sh = self.inp.fonts.shape_s(text, s);
        let font = self.inp.fonts.primary(&s.font_family, s.font_weight, s.font_style);
        let sc = font.scale(s.font_size);
        let asc = (font.ascent * sc).round();
        let desc = (font.descent * sc).round();
        let baseline = area.y + (area.h - (asc + desc)) / 2.0 + asc;
        let x = if align_center || matches!(s.text_align, TextAlign::Center | TextAlign::WebkitCenter) {
            area.x + (area.w - sh.width) / 2.0
        } else if (s.text_align == TextAlign::Right || s.text_align == TextAlign::End && !s.direction_rtl || s.text_align == TextAlign::Start && s.direction_rtl) && sh.width < area.w {
            area.x + area.w - sh.width
        } else {
            area.x
        };
        let mut st = s.clone();
        st.color = color;
        st.text_decoration_line = 0;
        st.deco_propagated = 0;
        self.emit(Item::PushClip { r: area.inflate(1.0), radii: None });
        self.paint_text(&st, x, baseline, &[(sh.clone(), 0.0)], sh.width, &Rc::from(text), None);
        self.emit(Item::PopClip);
    }

    fn paint_replaced(&mut self, id: BoxId, kind: ReplacedKind) {
        let tree = self.inp.tree;
        let b = &tree.boxes[id];
        let s = b.style.clone();
        if s.visibility != Visibility::Visible {
            return;
        }
        let cr = self.content_rect(id);
        let doc = self.inp.doc;
        let node = b.node;
        let el = node.and_then(|n| doc.element(n));
        let dark = s.dark_scheme;
        let accent = s.accent_color.unwrap_or(Color::from_hex(if dark { 0x99c8ff } else { 0x0075ff }));
        let disabled = el.map(|e| e.has_attr("disabled")).unwrap_or(false);
        // Chrome's control palette for the used color scheme.
        let (field_bg, field_edge, track_bg, track_edge, mark) = if dark {
            (Color::rgb(59, 59, 59), Color::rgb(133, 133, 133), Color::rgb(59, 59, 59), Color::rgb(133, 133, 133), Color::rgb(59, 59, 59))
        } else {
            (Color::WHITE, Color::rgb(118, 118, 118), Color::rgb(239, 239, 239), Color::rgb(178, 178, 178), Color::WHITE)
        };
        match kind {
            ReplacedKind::Image => {
                if let Some(img) = b.image.clone() {
                    // Natural (CSS px) size: a 2x srcset candidate or a hi-dpi SVG raster
                    // has more pixels than that.
                    let (iw, ih) = img.natural();
                    let dest = object_fit(&s, cr, iw, ih, img.svg_par);
                    let radii = radii_px(&s, b.w, b.h);
                    let needs_clip = dest.x < cr.x - 0.5 || dest.y < cr.y - 0.5 || dest.right() > cr.right() + 0.5 || dest.bottom() > cr.bottom() + 0.5;
                    if needs_clip {
                        self.emit(Item::PushClip { r: cr, radii: if has_radius(&radii) { Some(radii) } else { None } });
                        self.emit(Item::Image { r: dest, img, radii: None, pixelated: s.pixelated });
                        self.emit(Item::PopClip);
                    } else {
                        self.emit(Item::Image { r: dest, img, radii: if has_radius(&radii) { Some(radii) } else { None }, pixelated: s.pixelated });
                    }
                } else if cr.w >= 16.0 && cr.h >= 16.0 {
                    // Broken / loading image placeholder: a light frame plus, once the
                    // load has failed and there is room, a small "broken picture" icon.
                    self.emit(Item::Border { r: cr, widths: [1.0; 4], colors: [Color::rgb(192, 192, 192); 4], styles: [BorderStyle::Solid; 4], radii: Radii::ZERO });
                    if cr.w >= 20.0 && cr.h >= 20.0 && node.map(|n| self.inp.res.image_failed(doc, n)).unwrap_or(false) {
                        let (ix, iy) = (cr.x + 2.0, cr.y + 2.0);
                        self.emit(Item::Rect { r: Rect::new(ix, iy, 14.0, 16.0), color: Color::rgb(255, 255, 255) });
                        self.emit(Item::Rect { r: Rect::new(ix + 2.0, iy + 3.0, 10.0, 6.0), color: Color::rgb(160, 200, 240) });
                        self.emit(Item::Rect { r: Rect::new(ix + 2.0, iy + 8.0, 10.0, 4.0), color: Color::rgb(80, 170, 80) });
                        self.emit(Item::Border { r: Rect::new(ix, iy, 14.0, 16.0), widths: [1.0; 4], colors: [Color::rgb(140, 140, 140); 4], styles: [BorderStyle::Solid; 4], radii: Radii::ZERO });
                    }
                }
            }
            ReplacedKind::Svg => {
                if let Some(n) = node {
                    let fo = self.render_foreign_objects(doc, n, cr.w, cr.h);
                    let had = !fo.is_empty();
                    if had {
                        crate::page::resources::FO_KEYS.with(|k| k.borrow_mut().extend(fo.iter().cloned()));
                    }
                    let img = self.inp.res.svg_for(doc, n, cr.w, cr.h, self.inp.styles);
                    if had {
                        crate::page::resources::FO_KEYS.with(|k| {
                            let mut k = k.borrow_mut();
                            for (f, _) in &fo {
                                k.remove(f);
                            }
                        });
                    }
                    if let Some(img) = img {
                        self.emit(Item::Image { r: cr, img, radii: None, pixelated: s.pixelated });
                    }
                    // The HTML inside foreignObjects takes the clicks over it
                    // (Discord's server and avatar buttons).
                    if had {
                        self.foreign_object_hits(doc, n, cr, &fo);
                    }
                }
            }
            ReplacedKind::TextInput | ReplacedKind::TextArea => {
                let el = match el {
                    Some(e) => e,
                    None => return,
                };
                let is_pw = el.attr("type").map(|t| t.eq_ignore_ascii_case("password")).unwrap_or(false);
                let value = if kind == ReplacedKind::TextArea {
                    el.value.clone().unwrap_or_else(|| doc.text_content(node.unwrap()))
                } else {
                    el.value.clone().or_else(|| el.attr("value").map(|s| s.to_string())).unwrap_or_default()
                };
                let shown = if is_pw { "•".repeat(value.chars().count()) } else { value.clone() };
                let focused = self.inp.focus == node;
                if shown.is_empty() {
                    if let Some(ph) = el.attr("placeholder") {
                        // The ::placeholder color, faded by its opacity.
                        let pc = self.inp.styles.placeholder.get(&node.unwrap()).map(|p| {
                            let mut c = p.color;
                            if p.opacity < 1.0 {
                                c.a = (c.a as f32 * p.opacity.clamp(0.0, 1.0)).round() as u8;
                            }
                            c
                        }).unwrap_or(Color::rgb(117, 117, 117));
                        if kind == ReplacedKind::TextArea {
                            self.paint_multiline(&s, ph, cr, pc);
                        } else {
                            self.paint_label(&s, ph, cr, false, pc);
                        }
                    }
                } else if kind == ReplacedKind::TextArea {
                    let sel = if focused { self.inp.field_sel } else { None };
                    let caret = if focused { Some(self.inp.caret.unwrap_or(value.len()).min(value.len())) } else { None };
                    self.paint_textarea(&s, &shown, cr, sel, caret, self.inp.caret_visible);
                } else {
                    // Scroll the text so the caret stays visible.
                    let sh = self.inp.fonts.shape_s(&shown, &s);
                    let caret_byte = if focused { self.inp.caret.unwrap_or(value.len()).min(value.len()) } else { 0 };
                    let to_shown = |b: usize| if is_pw { shown.char_indices().nth(value[..b.min(value.len())].chars().count()).map(|(i, _)| i).unwrap_or(shown.len()) } else { b.min(shown.len()) };
                    let cx = sh.x_of_byte(to_shown(caret_byte));
                    let scroll = (cx - cr.w + 2.0).max(0.0);
                    let area = Rect::new(cr.x - scroll, cr.y, cr.w + scroll, cr.h);
                    self.emit(Item::PushClip { r: cr.inflate(1.0), radii: None });
                    if let Some((a, b)) = self.inp.field_sel.filter(|_| focused) {
                        let (a, b) = (a.min(b), a.max(b));
                        if a < b {
                            let (x0, x1) = (sh.x_of_byte(to_shown(a)), sh.x_of_byte(to_shown(b)));
                            let h = s.font_size * 1.2;
                            self.emit(Item::Rect { r: Rect::new(area.x + x0, cr.y + (cr.h - h) / 2.0, x1 - x0, h), color: Color::rgba(51, 144, 255, 90) });
                        }
                    }
                    self.paint_label(&s, &shown, area, false, s.color);
                    self.emit(Item::PopClip);
                    if focused && self.inp.caret_visible {
                        let h = s.font_size * 1.2;
                        let x = cr.x + (cx - scroll).min(cr.w - 1.0);
                        self.emit(Item::Rect { r: Rect::new(x, cr.y + (cr.h - h) / 2.0, 1.5, h), color: s.caret_color.unwrap_or(s.color) });
                    }
                }
                if focused && self.inp.caret_visible && shown.is_empty() {
                    let h = s.font_size * 1.2;
                    let (x, y) = if kind == ReplacedKind::TextArea { (cr.x + 1.0, cr.y + 1.0) } else { (cr.x, cr.y + (cr.h - h) / 2.0) };
                    self.emit(Item::Rect { r: Rect::new(x, y, 1.5, h), color: s.caret_color.unwrap_or(s.color) });
                }
                if kind == ReplacedKind::TextArea && s.resize && !matches!(s.overflow_x, Overflow::Visible) && !s.appearance_none {
                    // Resize grip in the bottom-right corner of the padding box.
                    let pr = self.border_rect(id);
                    let (rx, by) = (pr.right() - b.border[1], pr.bottom() - b.border[2]);
                    let g = Color::rgb(102, 102, 102);
                    self.emit(Item::Line { x0: rx - 1.5, y0: by - 8.5, x1: rx - 8.5, y1: by - 1.5, width: 1.0, color: g, style: BorderStyle::Solid });
                    self.emit(Item::Line { x0: rx - 1.5, y0: by - 4.5, x1: rx - 4.5, y1: by - 1.5, width: 1.0, color: g, style: BorderStyle::Solid });
                }
            }
            ReplacedKind::Checkbox | ReplacedKind::Radio => {
                let el = match el {
                    Some(e) => e,
                    None => return,
                };
                if s.appearance_none {
                    return;
                }
                let checked = el.checked.unwrap_or_else(|| el.has_attr("checked"));
                let r = self.border_rect(id);
                let on = if disabled { Color::rgba(118, 118, 118, 77) } else { accent };
                let edge = if disabled { Color::rgba(118, 118, 118, 77) } else { field_edge };
                if kind == ReplacedKind::Radio {
                    let d = r.w.min(r.h);
                    let c = Rect::new(r.x + (r.w - d) / 2.0, r.y + (r.h - d) / 2.0, d, d);
                    if checked {
                        // Accent ring, white gap, accent dot.
                        self.emit(Item::RRect { r: c, radii: Radii::all(d / 2.0), color: on });
                        let g = c.inflate(-d * 0.1);
                        self.emit(Item::RRect { r: g, radii: Radii::all(g.w / 2.0), color: mark });
                        let dot = c.inflate(-d * 0.22);
                        self.emit(Item::RRect { r: dot, radii: Radii::all(dot.w / 2.0), color: on });
                    } else {
                        self.emit(Item::RRect { r: c, radii: Radii::all(d / 2.0), color: field_bg });
                        self.emit(Item::Border { r: c, widths: [1.0; 4], colors: [edge; 4], styles: [BorderStyle::Solid; 4], radii: Radii::all(d / 2.0) });
                    }
                } else if checked {
                    self.emit(Item::RRect { r, radii: Radii::all(2.0), color: on });
                    let (x, y, w, h) = (r.x, r.y, r.w, r.h);
                    let lw = (w / 6.5).max(1.5);
                    self.emit(Item::Line { x0: x + w * 0.2, y0: y + h * 0.52, x1: x + w * 0.42, y1: y + h * 0.74, width: lw, color: mark, style: BorderStyle::Solid });
                    self.emit(Item::Line { x0: x + w * 0.42, y0: y + h * 0.74, x1: x + w * 0.8, y1: y + h * 0.3, width: lw, color: mark, style: BorderStyle::Solid });
                } else {
                    self.emit(Item::RRect { r, radii: Radii::all(2.0), color: field_bg });
                    self.emit(Item::Border { r, widths: [1.0; 4], colors: [edge; 4], styles: [BorderStyle::Solid; 4], radii: Radii::all(2.0) });
                }
            }
            ReplacedKind::Button => {
                let label = select_or_button_label(doc, node);
                // text-indent moves the label (sites hide it with a large
                // negative one); it stays clipped to the control.
                let ind = s.text_indent.resolve(cr.w);
                if ind != 0.0 {
                    self.emit(Item::PushClip { r: cr, radii: None });
                    self.paint_label(&s, &label, Rect::new(cr.x + ind, cr.y, cr.w, cr.h), true, s.color);
                    self.emit(Item::PopClip);
                } else {
                    self.paint_label(&s, &label, cr, true, s.color);
                }
            }
            ReplacedKind::Select => {
                let n = match node {
                    Some(n) => n,
                    None => return,
                };
                let multiple = doc.attr(n, "multiple").is_some();
                let size = doc.attr(n, "size").and_then(|v| v.trim().parse::<i32>().ok()).filter(|v| *v > 0);
                if multiple || size.map(|v| v > 1).unwrap_or(false) {
                    // List box: one row per option, selected rows highlighted.
                    let rows = size.unwrap_or(4).max(1) as f32;
                    let rh = cr.h / rows;
                    self.emit(Item::PushClip { r: cr, radii: None });
                    let mut y = cr.y;
                    for o in doc.descendants(n).filter(|&o| doc.is_tag(o, "option")) {
                        if y >= cr.bottom() {
                            break;
                        }
                        let sel = doc.element(o).map(|e| e.selected.unwrap_or_else(|| e.has_attr("selected"))).unwrap_or(false);
                        let row = Rect::new(cr.x, y, cr.w, rh);
                        if sel {
                            self.emit(Item::Rect { r: row, color: Color::rgb(206, 206, 206) });
                        }
                        let text = crate::dom::collapse_ws(&doc.text_content(o));
                        self.paint_label(&s, &text, Rect::new(row.x + 2.0, row.y, (row.w - 4.0).max(0.0), row.h - 1.0), false, s.color);
                        y += rh;
                    }
                    self.emit(Item::PopClip);
                    return;
                }
                let label = select_or_button_label(doc, node);
                let (ps, pe) = if s.appearance_none { (0.0, 0.0) } else { (4.0, 16.0) };
                let area = Rect::new(cr.x + ps, cr.y, (cr.w - ps - pe).max(0.0), cr.h);
                self.paint_label(&s, &label, area, false, s.color);
                if !s.appearance_none {
                    // Chrome's bold chevron, 4px from the end edge.
                    let ax = cr.right() - 8.0;
                    let ay = cr.y + cr.h / 2.0;
                    let c = s.color;
                    self.emit(Item::Line { x0: ax - 3.5, y0: ay - 1.75, x1: ax, y1: ay + 1.75, width: 2.0, color: c, style: BorderStyle::Solid });
                    self.emit(Item::Line { x0: ax, y0: ay + 1.75, x1: ax + 3.5, y1: ay - 1.75, width: 2.0, color: c, style: BorderStyle::Solid });
                }
            }
            ReplacedKind::DateTime => {
                let el = match el {
                    Some(e) => e,
                    None => return,
                };
                let t = el.attr("type").unwrap_or("date").to_ascii_lowercase();
                let value = el.value.clone().or_else(|| el.attr("value").map(|v| v.to_string())).unwrap_or_default();
                let text = datetime_text(&t, &value);
                let icon = (s.font_size * 0.75).round();
                let area = Rect::new(cr.x + 1.0, cr.y, (cr.w - icon - 8.0).max(0.0), cr.h);
                self.paint_label(&s, &text, area, false, s.color);
                if !s.appearance_none {
                    // Picker indicator: a calendar (a clock for time).
                    let ix = cr.right() - icon - 4.0;
                    let iy = cr.y + ((cr.h - icon) / 2.0).round();
                    let ic = if disabled || dark { s.color } else { Color::rgb(77, 77, 77) };
                    let box_ = Rect::new(ix, iy, icon, icon);
                    if t == "time" {
                        self.emit(Item::Border { r: box_, widths: [1.2; 4], colors: [ic; 4], styles: [BorderStyle::Solid; 4], radii: Radii::all(icon / 2.0) });
                        let (cx, cy) = (ix + icon / 2.0, iy + icon / 2.0);
                        self.emit(Item::Line { x0: cx, y0: cy - icon * 0.3, x1: cx, y1: cy, width: 1.2, color: ic, style: BorderStyle::Solid });
                        self.emit(Item::Line { x0: cx, y0: cy, x1: cx + icon * 0.22, y1: cy + icon * 0.15, width: 1.2, color: ic, style: BorderStyle::Solid });
                    } else {
                        self.emit(Item::Border { r: box_, widths: [1.2; 4], colors: [ic; 4], styles: [BorderStyle::Solid; 4], radii: Radii::all(1.0) });
                        self.emit(Item::Rect { r: Rect::new(ix, iy, icon, (icon * 0.25).max(2.0)), color: ic });
                    }
                }
            }
            ReplacedKind::ColorWell => {
                let el = match el {
                    Some(e) => e,
                    None => return,
                };
                let v = el.value.clone().or_else(|| el.attr("value").map(|v| v.to_string())).unwrap_or_default();
                let col = crate::css::values::parse_hex(v.trim().trim_start_matches('#')).filter(|_| v.trim().len() == 7).unwrap_or(Color::BLACK);
                let sw = Rect::new(cr.x + 2.0, cr.y + 4.0, (cr.w - 4.0).max(0.0), (cr.h - 8.0).max(0.0));
                self.emit(Item::Rect { r: sw, color: col });
                self.emit(Item::Border { r: sw, widths: [1.0; 4], colors: [Color::rgb(119, 119, 119); 4], styles: [BorderStyle::Solid; 4], radii: Radii::ZERO });
            }
            ReplacedKind::File => {
                let el = match el {
                    Some(e) => e,
                    None => return,
                };
                let bw = self.inp.fonts.shape("Choose File", &s.font_family, s.font_weight, s.font_style, s.font_size, 0.0).width + 16.0;
                let btn = Rect::new(cr.x, cr.y, bw, cr.h);
                let (bg, edge, fg) = if disabled {
                    (Color::rgba(239, 239, 239, 77), Color::rgba(118, 118, 118, 77), Color::rgba(16, 16, 16, 77))
                } else if dark {
                    (Color::rgb(107, 107, 107), Color::rgb(107, 107, 107), Color::WHITE)
                } else {
                    (Color::rgb(239, 239, 239), Color::rgb(118, 118, 118), Color::BLACK)
                };
                self.emit(Item::RRect { r: btn, radii: Radii::all(4.0), color: bg });
                self.emit(Item::Border { r: btn, widths: [1.0; 4], colors: [edge; 4], styles: [BorderStyle::Solid; 4], radii: Radii::all(4.0) });
                self.paint_label(&s, "Choose File", btn, true, fg);
                let files = el.attr("data-ohb-files").map(|v| v.to_string());
                let label = files.unwrap_or_else(|| "No file chosen".into());
                let la = Rect::new(btn.right() + 4.0, cr.y, (cr.w - bw - 4.0).max(0.0), cr.h);
                self.paint_label(&s, &label, la, false, s.color);
            }
            ReplacedKind::Range => {
                let el = match el {
                    Some(e) => e,
                    None => return,
                };
                let min: f32 = el.attr("min").and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let max: f32 = el.attr("max").and_then(|v| v.parse().ok()).unwrap_or(100.0);
                let val: f32 = el.value.as_deref().or(el.attr("value")).and_then(|v| v.parse().ok()).unwrap_or((min + max) / 2.0);
                let t = if max > min { ((val - min) / (max - min)).clamp(0.0, 1.0) } else { 0.5 };
                if s.appearance_none {
                    return;
                }
                let on = if disabled { Color::rgb(203, 203, 203) } else { accent };
                // Track (8px, rounded, light grey with a grey edge), filled up to the thumb.
                let th = 16.0f32.min(cr.h.max(8.0));
                let track = Rect::new(cr.x, cr.y + (cr.h / 2.0 - 4.0).round(), cr.w, 8.0);
                let tx = cr.x + th / 2.0 + (cr.w - th).max(0.0) * t;
                self.emit(Item::RRect { r: track, radii: Radii::all(4.0), color: track_bg });
                self.emit(Item::Border { r: track, widths: [1.0; 4], colors: [track_edge; 4], styles: [BorderStyle::Solid; 4], radii: Radii::all(4.0) });
                self.emit(Item::RRect { r: Rect::new(track.x, track.y, (tx - track.x).max(0.0), 8.0), radii: Radii::all(4.0), color: on });
                self.emit(Item::RRect { r: Rect::new(tx - th / 2.0, cr.y + (cr.h - th) / 2.0, th, th), radii: Radii::all(th / 2.0), color: on });
            }
            ReplacedKind::Progress => {
                let el = match el {
                    Some(e) => e,
                    None => return,
                };
                if s.appearance_none {
                    return;
                }
                let r = self.content_rect(id);
                // An 8px rounded track centered in the box.
                let track = Rect::new(r.x, r.y + ((r.h - 8.0) / 2.0).round(), r.w, 8.0_f32.min(r.h));
                let rad = Radii::all(track.h / 2.0);
                self.emit(Item::RRect { r: track, radii: rad, color: track_bg });
                self.emit(Item::Border { r: track, widths: [1.0; 4], colors: [track_edge; 4], styles: [BorderStyle::Solid; 4], radii: rad });
                let num = |a: &str| el.attr(a).and_then(|v| v.trim().parse::<f32>().ok());
                if el.name == "meter" {
                    let mn = num("min").unwrap_or(0.0);
                    let mx = num("max").unwrap_or(1.0).max(mn);
                    let v = num("value").unwrap_or(0.0).clamp(mn, mx);
                    let low = num("low").unwrap_or(mn).clamp(mn, mx);
                    let high = num("high").unwrap_or(mx).clamp(low, mx);
                    let opt = num("optimum").unwrap_or((mn + mx) / 2.0).clamp(mn, mx);
                    // Region of the value relative to the optimum (HTML §4.10.14).
                    let region = |x: f32| if x < low { 0 } else if x > high { 2 } else { 1 };
                    let (ro, rv) = (region(opt), region(v));
                    let color = if ro == rv {
                        Color::rgb(16, 124, 16)
                    } else if ro == 1 || (ro as i32 - rv as i32).abs() == 1 {
                        Color::rgb(255, 185, 0)
                    } else {
                        Color::rgb(216, 59, 1)
                    };
                    let t = if mx > mn { (v - mn) / (mx - mn) } else { 0.0 };
                    if t > 0.0 {
                        self.emit(Item::RRect { r: Rect::new(track.x, track.y, track.w * t, track.h), radii: rad, color });
                    }
                } else {
                    let mx = num("max").filter(|m| *m > 0.0).unwrap_or(1.0);
                    match num("value") {
                        Some(v) => {
                            let t = (v / mx).clamp(0.0, 1.0);
                            if t > 0.0 {
                                self.emit(Item::RRect { r: Rect::new(track.x, track.y, track.w * t, track.h), radii: rad, color: accent });
                            }
                        }
                        // Indeterminate: a short moving segment (drawn at rest).
                        None => self.emit(Item::RRect { r: Rect::new(track.x + track.w * 0.2, track.y, track.w * 0.2, track.h), radii: rad, color: accent }),
                    }
                }
            }
            ReplacedKind::Canvas if node.and_then(|n| self.inp.res.canvas_for(n)).is_some() => {
                let img = self.inp.res.canvas_for(node.unwrap()).unwrap();
                self.emit(Item::Image { r: cr, img, radii: None, pixelated: s.pixelated });
            }
            ReplacedKind::Media => {
                let Some(n) = node else { return };
                let audio = doc.is_tag(n, "audio");
                let controls = doc.attr(n, "controls").is_some();
                if audio {
                    if controls {
                        self.paint_audio_controls(&s, cr);
                    }
                    return;
                }
                // Video: the poster (object-fit), else black while a source exists
                // and the video has controls. Without controls (decorative
                // autoplay loops) it stays transparent like a video whose first
                // frame hasn't arrived, so the page's own fallback background shows.
                if let Some(img) = b.image.clone() {
                    // Natural (CSS px) size: a 2x srcset candidate or a hi-dpi SVG raster
                    // has more pixels than that.
                    let (iw, ih) = img.natural();
                    let dest = object_fit(&s, cr, iw, ih, img.svg_par);
                    self.emit(Item::PushClip { r: cr, radii: None });
                    self.emit(Item::Image { r: dest, img, radii: None, pixelated: s.pixelated });
                    self.emit(Item::PopClip);
                } else if controls && (doc.attr(n, "src").is_some() || doc.element_children(n).any(|c| doc.is_tag(c, "source"))) {
                    self.emit(Item::Rect { r: cr, color: Color::BLACK });
                }
                if controls {
                    self.paint_video_controls(&s, cr);
                }
            }
            ReplacedKind::Frame if node.and_then(|n| self.inp.res.frame_image(n)).is_some() => {
                let img = self.inp.res.frame_image(node.unwrap()).unwrap();
                self.emit(Item::PushClip { r: cr, radii: None });
                self.emit(Item::Image { r: cr, img, radii: None, pixelated: s.pixelated });
                self.emit(Item::PopClip);
            }
            // An <object> showing a document we don't render as a nested frame:
            // paint nothing rather than a placeholder (its page is usually
            // transparent or decorative, e.g. Acid3's support-b).
            ReplacedKind::Frame if node.map_or(false, |n| doc.is_tag(n, "object")) => {}
            ReplacedKind::Canvas | ReplacedKind::Frame => {
                let r = cr;
                let (bg, label) = match kind {
                    ReplacedKind::Media => (Color::rgb(24, 24, 24), "▶  media"),
                    ReplacedKind::Canvas => (Color::TRANSPARENT, ""),
                    _ => (Color::rgb(245, 245, 247), "embedded frame"),
                };
                if bg.a > 0 {
                    self.emit(Item::Rect { r, color: bg });
                }
                if !label.is_empty() && r.w > 60.0 && r.h > 20.0 {
                    let mut st = (*s).clone();
                    st.font_size = 13.0;
                    st.font_family = Rc::new(vec!["sans-serif".into()]);
                    let txt = if kind == ReplacedKind::Frame {
                        node.and_then(|n| doc.attr(n, "src")).and_then(|src| doc.resolve_url(src)).and_then(|u| u.host_str().map(|h| format!("⧉ {h}"))).unwrap_or_else(|| label.to_string())
                    } else {
                        label.to_string()
                    };
                    let col = if kind == ReplacedKind::Media { Color::rgb(220, 220, 220) } else { Color::rgb(110, 110, 120) };
                    self.paint_label(&st, &txt, r, true, col);
                }
            }
        }
    }

    /// A small glyph-like icon drawn with lines: play, volume, fullscreen, menu.
    fn media_icon(&mut self, kind: u8, cx: f32, cy: f32, c: Color) {
        let line = |me: &mut Self, x0: f32, y0: f32, x1: f32, y1: f32, w: f32| me.emit(Item::Line { x0, y0, x1, y1, width: w, color: c, style: BorderStyle::Solid });
        match kind {
            // Play: a filled triangle (built from horizontal strokes).
            0 => {
                for i in 0..12 {
                    let y = cy - 6.0 + i as f32 + 0.5;
                    let half = 6.0 - (y - cy).abs();
                    line(self, cx - 4.0, y, cx - 4.0 + half * 1.6, y, 1.0);
                }
            }
            // Volume: speaker body and a sound wave.
            1 => {
                self.emit(Item::Rect { r: Rect::new(cx - 7.0, cy - 2.5, 3.0, 5.0), color: c });
                for i in 0..9 {
                    let y = cy - 4.5 + i as f32 + 0.5;
                    let half = 4.5 - (y - cy).abs() * 0.6;
                    line(self, cx - 4.0, y, cx - 4.0 + half.max(0.0), y, 1.0);
                }
                line(self, cx + 3.0, cy - 3.5, cx + 4.5, cy, 1.5);
                line(self, cx + 4.5, cy, cx + 3.0, cy + 3.5, 1.5);
            }
            // Fullscreen: four corners.
            2 => {
                let (a, l) = (5.5, 3.0);
                for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                    let (x, y) = (cx + sx * a, cy + sy * a);
                    line(self, x, y, x - sx * l, y, 1.5);
                    line(self, x, y, x, y - sy * l, 1.5);
                }
            }
            // Menu: three dots.
            _ => {
                for i in -1..=1 {
                    let y = cy + i as f32 * 5.0;
                    self.emit(Item::RRect { r: Rect::new(cx - 1.75, y - 1.75, 3.5, 3.5), radii: Radii::all(1.75), color: c });
                }
            }
        }
    }

    /// Chrome's audio control bar: a grey pill with play, time, a seek track,
    /// volume and the overflow menu.
    fn paint_audio_controls(&mut self, s: &ComputedStyle, r: Rect) {
        let bg = Color::rgb(241, 243, 244);
        self.emit(Item::RRect { r, radii: Radii::all((r.h / 2.0).min(27.0)), color: bg });
        let cy = r.y + r.h / 2.0;
        let dim = Color::rgba(0, 0, 0, 102);
        self.media_icon(0, r.x + 26.0, cy, dim);
        let mut st = s.clone();
        st.font_size = 13.0;
        st.font_family = Rc::new(vec!["system-ui".into(), "sans-serif".into()]);
        st.font_weight = 400;
        st.text_align = TextAlign::Left;
        let tw = self.inp.fonts.shape("0:00 / 0:00", &st.font_family, st.font_weight, st.font_style, 13.0, 0.0).width;
        self.paint_label(&st, "0:00 / 0:00", Rect::new(r.x + 47.0, r.y, tw + 2.0, r.h), false, Color::rgb(0, 0, 0));
        let t0 = r.x + 47.0 + tw + 12.0;
        let t1 = r.right() - 72.0;
        if t1 > t0 + 10.0 {
            self.emit(Item::RRect { r: Rect::new(t0, cy - 2.0, t1 - t0, 4.0), radii: Radii::all(2.0), color: Color::rgb(190, 190, 190) });
            self.media_icon(1, r.right() - 54.0, cy, dim);
        }
        self.media_icon(3, r.right() - 26.0, cy, Color::rgb(0, 0, 0));
    }

    /// Chrome's video controls: a dark gradient with play, time, volume,
    /// fullscreen and menu buttons above a seek bar.
    fn paint_video_controls(&mut self, s: &ComputedStyle, r: Rect) {
        if r.w < 40.0 || r.h < 40.0 {
            return;
        }
        let gh = r.h.min(120.0);
        let g = crate::css::style::Gradient {
            radial: false,
            repeating: false,
            angle: 180.0,
            stops: vec![(Color::rgba(0, 0, 0, 0), None), (Color::rgba(0, 0, 0, 230), None)],
            circle: false,
            center: (Len::Pct(50.0), Len::Pct(50.0)),
            conic: false,
            space: None,
            hue: 0,
            extent: 0,
            size: None,
            hints: Vec::new(),
        };
        let gr = Rect::new(r.x, r.bottom() - gh, r.w, gh);
        if let Some(spec) = resolve_gradient(&g, gr) {
            self.emit(Item::PushClip { r, radii: None });
            self.emit(Item::Gradient { r: gr, grad: Arc::new(spec), radii: Radii::ZERO });
            self.emit(Item::PopClip);
        }
        let icon = Color::rgba(255, 255, 255, 140);
        let cy = r.bottom() - 49.0;
        self.media_icon(0, r.x + 25.0, cy, icon);
        let mut st = s.clone();
        st.font_size = 13.0;
        st.font_family = Rc::new(vec!["system-ui".into(), "sans-serif".into()]);
        st.font_weight = 400;
        st.text_align = TextAlign::Left;
        self.paint_label(&st, "0:00", Rect::new(r.x + 49.0, cy - 10.0, 40.0, 20.0), false, Color::WHITE);
        if r.w > 160.0 {
            self.media_icon(1, r.right() - 109.0, cy, icon);
            self.media_icon(2, r.right() - 62.0, cy, icon);
        }
        self.media_icon(3, r.right() - 14.0, cy, icon);
        self.emit(Item::RRect { r: Rect::new(r.x + 16.0, r.bottom() - 25.5, (r.w - 32.0).max(0.0), 4.0), radii: Radii::all(2.0), color: Color::rgba(255, 255, 255, 110) });
    }

    fn paint_select_popup(&mut self) {
        let Some(p) = self.inp.select_popup.clone() else { return };
        let mut s = self.inp.styles.get(p.node).map(|s| (**s).clone()).unwrap_or_else(ComputedStyle::initial);
        s.text_align = TextAlign::Left;
        s.font_size = s.font_size.clamp(11.0, 24.0);
        s.text_decoration_line = 0;
        let r = p.rect;
        self.emit(Item::Shadow { r, radii: Radii::all(4.0), blur: 8.0, spread: 0.0, color: Color::rgba(0, 0, 0, 70), inset: false, hole: None });
        self.emit(Item::RRect { r, radii: Radii::all(4.0), color: Color::WHITE });
        self.emit(Item::Border { r, widths: [1.0; 4], colors: [Color::rgb(160, 160, 160); 4], styles: [BorderStyle::Solid; 4], radii: Radii::all(4.0) });
        self.emit(Item::PushClip { r: r.inflate(-1.0), radii: None });
        let rows = ((r.h - 2.0) / p.item_h).floor().max(1.0) as usize;
        for (k, (label, disabled, group, selected)) in p.items.iter().enumerate().skip(p.first).take(rows) {
            let y = r.y + 1.0 + (k - p.first) as f32 * p.item_h;
            let row = Rect::new(r.x + 1.0, y, r.w - 2.0, p.item_h);
            let hovered = p.hover == Some(k) && !*disabled && !*group;
            if hovered {
                self.emit(Item::Rect { r: row, color: Color::rgb(30, 144, 255) });
            } else if *selected {
                self.emit(Item::Rect { r: row, color: Color::rgb(220, 230, 245) });
            }
            let color = if hovered { Color::WHITE } else if *disabled { Color::rgb(150, 150, 150) } else { Color::rgb(20, 20, 20) };
            let mut st = s.clone();
            st.font_weight = if *group { 700 } else { 400 };
            let indent = if *group { 6.0 } else { 10.0 };
            self.paint_label(&st, label, Rect::new(row.x + indent, row.y, row.w - indent - 4.0, row.h), false, color);
        }
        if p.items.len() > rows {
            // A scroll indicator on the right edge.
            let track = Rect::new(r.right() - 6.0, r.y + 2.0, 4.0, r.h - 4.0);
            let frac = rows as f32 / p.items.len() as f32;
            let top = p.first as f32 / p.items.len() as f32;
            self.emit(Item::RRect { r: Rect::new(track.x, track.y + track.h * top, track.w, (track.h * frac).max(12.0)), radii: Radii::all(2.0), color: Color::rgba(0, 0, 0, 90) });
        }
        self.emit(Item::PopClip);
    }

    /// Wrapped lines of a textarea's text as byte ranges (newlines end lines;
    /// long lines break at spaces, which are dropped at the break).
    fn wrap_textarea(&mut self, s: &ComputedStyle, text: &str, width: f32) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut base = 0;
        for line in text.split('\n') {
            let mut start = 0;
            let mut cur_end = 0;
            let mut have = false;
            let mut pos = 0;
            for word in line.split(' ') {
                let wend = pos + word.len();
                let w = self.inp.fonts.shape(&line[start..wend], &s.font_family, s.font_weight, s.font_style, s.font_size, 0.0).width;
                if w > width && have && pos > start {
                    out.push((base + start, base + cur_end));
                    start = pos;
                }
                cur_end = wend;
                have = true;
                pos = wend + 1;
            }
            out.push((base + start, base + line.len()));
            base += line.len() + 1;
        }
        out
    }

    /// A textarea's text with its selection highlight and caret (scrolled so
    /// the caret line is visible).
    fn paint_textarea(&mut self, s: &ComputedStyle, text: &str, area: Rect, sel: Option<(usize, usize)>, caret: Option<usize>, caret_visible: bool) {
        let lh = s.line_height_px().max(s.font_size * 1.2);
        let lines = self.wrap_textarea(s, text, area.w);
        let caret_line = caret.map(|c| lines.iter().position(|&(a, b)| c >= a && c <= b).unwrap_or(lines.len().saturating_sub(1)));
        let scroll_y = caret_line.map(|i| ((i + 1) as f32 * lh - area.h).max(0.0)).unwrap_or(0.0);
        let width_of = |me: &mut Self, t: &str| me.inp.fonts.shape(t, &s.font_family, s.font_weight, s.font_style, s.font_size, 0.0).width;
        self.emit(Item::PushClip { r: area, radii: None });
        for (i, &(a, b)) in lines.iter().enumerate() {
            let y = area.y + i as f32 * lh - scroll_y;
            if y > area.bottom() {
                break;
            }
            if y + lh < area.y {
                continue;
            }
            if let Some((sa, sb)) = sel {
                let (sa, sb) = (sa.min(sb), sa.max(sb));
                let (x0b, x1b) = (sa.max(a), sb.min(b));
                if sa < sb && x0b <= x1b && sb > a && sa <= b {
                    let x0 = width_of(self, &text[a..x0b]);
                    let mut x1 = width_of(self, &text[a..x1b]);
                    if sb > b {
                        x1 += s.font_size * 0.3;
                    }
                    if x1 > x0 {
                        self.emit(Item::Rect { r: Rect::new(area.x + x0, y, x1 - x0, lh), color: Color::rgba(51, 144, 255, 90) });
                    }
                }
            }
            let line = &text[a..b];
            if !line.is_empty() {
                self.paint_label(s, line, Rect::new(area.x, y, area.w, lh), false, s.color);
            }
        }
        if let (Some(c), Some(i), true) = (caret, caret_line, caret_visible) {
            let (a, _) = lines[i];
            let x = width_of(self, &text[a..c.max(a)]).min(area.w - 1.0);
            let y = area.y + i as f32 * lh - scroll_y;
            let h = s.font_size * 1.2;
            self.emit(Item::Rect { r: Rect::new(area.x + x, y + (lh - h) / 2.0, 1.5, h), color: s.caret_color.unwrap_or(s.color) });
        }
        self.emit(Item::PopClip);
    }

    fn paint_multiline(&mut self, s: &ComputedStyle, text: &str, area: Rect, color: Color) {
        let lh = s.line_height_px().max(s.font_size * 1.2);
        self.emit(Item::PushClip { r: area, radii: None });
        let mut y = area.y;
        for line in text.split('\n') {
            // naive wrapping by measuring words
            let mut cur = String::new();
            let mut lines = Vec::new();
            for word in line.split(' ') {
                let trial = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
                let w = self.inp.fonts.shape(&trial, &s.font_family, s.font_weight, s.font_style, s.font_size, 0.0).width;
                if w > area.w && !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                    cur = word.to_string();
                } else {
                    cur = trial;
                }
            }
            lines.push(cur);
            for l in lines {
                if y > area.bottom() {
                    break;
                }
                self.paint_label(s, &l, Rect::new(area.x, y, area.w, lh), false, color);
                y += lh;
            }
        }
        self.emit(Item::PopClip);
    }
}

/// The outline of a CSS basic shape whose reference box is `r`.
pub fn basic_shape_path(shape: &crate::css::style::BasicShape, r: Rect) -> Option<tiny_skia::Path> {
    use crate::css::style::{BasicShape, ShapeRadius};
    let (w, h) = (r.w.max(0.0), r.h.max(0.0));
    let radii = |rd: &Option<[(crate::css::values::Len, crate::css::values::Len); 4]>| -> Radii {
        match rd {
            None => Radii::ZERO,
            Some(c) => Radii { x: [c[0].0.resolve(w), c[1].0.resolve(w), c[2].0.resolve(w), c[3].0.resolve(w)], y: [c[0].1.resolve(h), c[1].1.resolve(h), c[2].1.resolve(h), c[3].1.resolve(h)] },
        }
    };
    let rounded = |rect: Rect, rd: Radii| -> Option<tiny_skia::Path> {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return None;
        }
        if rd.x.iter().any(|v| *v > 0.0) {
            crate::paint::raster::rrect_path(rect, rd.scaled_to(rect.w, rect.h))
        } else {
            tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.w, rect.h).map(tiny_skia::PathBuilder::from_rect)
        }
    };
    match shape {
        BasicShape::Inset(q, rd) => {
            let (t, rr, bb, l) = (q[0].resolve(h), q[1].resolve(w), q[2].resolve(h), q[3].resolve(w));
            rounded(Rect::new(r.x + l, r.y + t, w - l - rr, h - t - bb), radii(rd))
        }
        BasicShape::Xywh(v, rd) => rounded(Rect::new(r.x + v[0].resolve(w), r.y + v[1].resolve(h), v[2].resolve(w), v[3].resolve(h)), radii(rd)),
        BasicShape::Circle(rad, at) => {
            let (cx, cy) = (at.0.resolve(w), at.1.resolve(h));
            let rv = match rad {
                ShapeRadius::Len(l) => l.resolve((w * w + h * h).sqrt() / std::f32::consts::SQRT_2),
                ShapeRadius::ClosestSide => cx.abs().min((w - cx).abs()).min(cy.abs()).min((h - cy).abs()),
                ShapeRadius::FarthestSide => cx.abs().max((w - cx).abs()).max(cy.abs()).max((h - cy).abs()),
            };
            if rv <= 0.0 {
                return None;
            }
            tiny_skia::PathBuilder::from_circle(r.x + cx, r.y + cy, rv)
        }
        BasicShape::Ellipse(rx, ry, at) => {
            let (cx, cy) = (at.0.resolve(w), at.1.resolve(h));
            let res = |s: &ShapeRadius, len: f32, c: f32| match s {
                ShapeRadius::Len(l) => l.resolve(len),
                ShapeRadius::ClosestSide => c.abs().min((len - c).abs()),
                ShapeRadius::FarthestSide => c.abs().max((len - c).abs()),
            };
            let (a, bb) = (res(rx, w, cx), res(ry, h, cy));
            if a <= 0.0 || bb <= 0.0 {
                return None;
            }
            tiny_skia::Rect::from_xywh(r.x + cx - a, r.y + cy - bb, 2.0 * a, 2.0 * bb).and_then(tiny_skia::PathBuilder::from_oval)
        }
        BasicShape::Polygon(pts, _) => {
            let mut pb = tiny_skia::PathBuilder::new();
            for (i, (x, y)) in pts.iter().enumerate() {
                let (px, py) = (r.x + x.resolve(w), r.y + y.resolve(h));
                if i == 0 {
                    pb.move_to(px, py);
                } else {
                    pb.line_to(px, py);
                }
            }
            pb.close();
            pb.finish()
        }
        BasicShape::Path(d, _) => {
            let mut pb = tiny_skia::PathBuilder::new();
            let (ox, oy) = (r.x as f64, r.y as f64);
            for seg in svgtypes::SimplifyingPathParser::from(&**d) {
                match seg.ok()? {
                    svgtypes::SimplePathSegment::MoveTo { x, y } => pb.move_to((x + ox) as f32, (y + oy) as f32),
                    svgtypes::SimplePathSegment::LineTo { x, y } => pb.line_to((x + ox) as f32, (y + oy) as f32),
                    svgtypes::SimplePathSegment::CurveTo { x1, y1, x2, y2, x, y } => pb.cubic_to((x1 + ox) as f32, (y1 + oy) as f32, (x2 + ox) as f32, (y2 + oy) as f32, (x + ox) as f32, (y + oy) as f32),
                    svgtypes::SimplePathSegment::Quadratic { x1, y1, x, y } => pb.quad_to((x1 + ox) as f32, (y1 + oy) as f32, (x + ox) as f32, (y + oy) as f32),
                    svgtypes::SimplePathSegment::ClosePath => pb.close(),
                }
            }
            pb.finish()
        }
    }
}

/// A MathML `<mo>` holding a fence or bar, which stretches vertically.
fn is_stretchy_fence(doc: &Document, node: NodeId, text: &str) -> bool {
    let node = if doc.element(node).is_some() { node } else { match doc.parent_element(node) { Some(p) => p, None => return false } };
    let Some(el) = doc.element(node) else { return false };
    if el.ns != crate::dom::Namespace::MathMl || el.name.as_str() != "mo" || el.attr("stretchy").map(|v| v.eq_ignore_ascii_case("false")).unwrap_or(false) {
        return false;
    }
    matches!(text.trim(), "(" | ")" | "[" | "]" | "{" | "}" | "|" | "\u{2016}" | "\u{27E8}" | "\u{27E9}" | "\u{2308}" | "\u{2309}" | "\u{230A}" | "\u{230B}" | "\u{2223}" | "\u{2225}")
}

fn select_or_button_label(doc: &Document, node: Option<NodeId>) -> String {
    let n = match node {
        Some(n) => n,
        None => return String::new(),
    };
    let el = match doc.element(n) {
        Some(e) => e,
        None => return String::new(),
    };
    if el.name == "select" {
        let opts: Vec<NodeId> = doc.descendants(n).filter(|&o| doc.is_tag(o, "option")).collect();
        let sel = opts
            .iter()
            .copied()
            .find(|&o| doc.element(o).map(|e| e.selected.unwrap_or_else(|| e.has_attr("selected"))).unwrap_or(false))
            .or(opts.first().copied());
        return sel.map(|o| crate::dom::collapse_ws(&doc.text_content(o))).unwrap_or_default();
    }
    if let Some(v) = el.value.clone().or_else(|| el.attr("value").map(|s| s.to_string())) {
        // A present value attribute is used even when empty (as in Chrome).
        if !v.is_empty() || el.has_attr("value") {
            return v;
        }
    }
    match el.attr("type").unwrap_or("").to_ascii_lowercase().as_str() {
        "submit" => "Submit".into(),
        "reset" => "Reset".into(),
        "file" => "Choose File".into(),
        _ => String::new(),
    }
}

fn object_fit(s: &ComputedStyle, cr: Rect, iw: f32, ih: f32, svg_par: Option<(f32, f32, bool)>) -> Rect {
    if iw <= 0.0 || ih <= 0.0 {
        return cr;
    }
    let (w, h) = match s.object_fit {
        // An SVG with a viewBox fills the box as its viewport, which keeps the
        // drawing's aspect ratio (preserveAspectRatio, default xMidYMid meet).
        ObjectFit::Fill => match svg_par {
            Some((ax, ay, slice)) => {
                let k = if slice { (cr.w / iw).max(cr.h / ih) } else { (cr.w / iw).min(cr.h / ih) };
                let (w, h) = (iw * k, ih * k);
                return Rect::new(cr.x + (cr.w - w) * ax, cr.y + (cr.h - h) * ay, w, h);
            }
            None => return cr,
        },
        ObjectFit::Contain => {
            let k = (cr.w / iw).min(cr.h / ih);
            (iw * k, ih * k)
        }
        ObjectFit::Cover => {
            let k = (cr.w / iw).max(cr.h / ih);
            (iw * k, ih * k)
        }
        ObjectFit::None => (iw, ih),
        ObjectFit::ScaleDown => {
            let k = (cr.w / iw).min(cr.h / ih).min(1.0);
            (iw * k, ih * k)
        }
    };
    let x = cr.x + s.object_position.0.resolve(cr.w - w);
    let y = cr.y + s.object_position.1.resolve(cr.h - h);
    Rect::new(x, y, w, h)
}

pub fn resolve_gradient(g: &Gradient, r: Rect) -> Option<GradSpec> {
    use crate::css::values::MixSpace;
    let n = g.stops.len();
    if n == 0 {
        return None;
    }
    let mut angle = 0.0;
    let (start, end, radius, len) = if g.radial {
        let cx = r.x + g.center.0.resolve(r.w);
        let cy = r.y + g.center.1.resolve(r.h);
        let (l, rt, t, b) = ((cx - r.x).abs(), (r.right() - cx).abs(), (cy - r.y).abs(), (r.bottom() - cy).abs());
        let (fx, fy) = (l.max(rt), t.max(b));
        let (nx, ny) = (l.min(rt), t.min(b));
        const S2: f32 = std::f32::consts::SQRT_2;
        let (rx, ry) = if let Some((a, b)) = &g.size {
            if g.circle {
                (a.resolve(r.w), a.resolve(r.w))
            } else {
                (a.resolve(r.w), b.resolve(r.h))
            }
        } else {
            match (g.extent, g.circle) {
                (1, true) => (nx.min(ny), nx.min(ny)),
                (1, false) => (nx, ny),
                (3, true) => (fx.max(fy), fx.max(fy)),
                (3, false) => (fx, fy),
                (2, true) => {
                    let d = (nx * nx + ny * ny).sqrt();
                    (d, d)
                }
                (2, false) => (nx * S2, ny * S2),
                (_, true) => {
                    let d = (fx * fx + fy * fy).sqrt();
                    (d, d)
                }
                (_, false) => (fx * S2, fy * S2),
            }
        };
        ((cx, cy), (cx, cy), (rx.max(0.5), ry.max(0.5)), rx.max(0.5))
    } else if g.conic {
        let cx = r.x + g.center.0.resolve(r.w);
        let cy = r.y + g.center.1.resolve(r.h);
        angle = g.angle;
        ((cx, cy), (cx, cy), (r.w.max(r.h), r.w.max(r.h)), 1.0)
    } else {
        let a = g.angle.to_radians();
        let (sn, cs) = (a.sin(), a.cos());
        let len = (r.w * sn).abs() + (r.h * cs).abs();
        let cx = r.x + r.w / 2.0;
        let cy = r.y + r.h / 2.0;
        let dx = sn * len / 2.0;
        let dy = -cs * len / 2.0;
        ((cx - dx, cy - dy), (cx + dx, cy + dy), (0.0, 0.0), len.max(0.001))
    };
    let frac = |l: &Len| match l {
        Len::Pct(p) => p / 100.0,
        other => other.resolve(len) / len,
    };
    // Resolve stop positions to fractions.
    let mut pos: Vec<Option<f32>> = g.stops.iter().map(|(_, p)| p.as_ref().map(frac)).collect();
    if pos[0].is_none() {
        pos[0] = Some(0.0);
    }
    if pos[n - 1].is_none() {
        pos[n - 1] = Some(1.0);
    }
    // Monotonic.
    let mut last = pos[0].unwrap();
    for p in pos.iter_mut() {
        if let Some(v) = p {
            if *v < last {
                *v = last;
            }
            last = *v;
        }
    }
    // Interpolate missing.
    let mut i = 0;
    while i < n {
        if pos[i].is_none() {
            let start_i = i - 1;
            let mut j = i;
            while pos[j].is_none() {
                j += 1;
            }
            let (a, b) = (pos[start_i].unwrap(), pos[j].unwrap());
            for k in i..j {
                pos[k] = Some(a + (b - a) * (k - start_i) as f32 / (j - start_i) as f32);
            }
            i = j;
        }
        i += 1;
    }
    let raw: Vec<(f32, Color)> = g.stops.iter().zip(pos).map(|((c, _), p)| (p.unwrap_or(0.0), *c)).collect();
    // CSS interpolates gradients with premultiplied alpha, in the requested
    // color space, honoring color hints; the rasterizer interpolates plain
    // sRGB linearly. Where that would differ, add intermediate stops.
    let space = g.space.unwrap_or(MixSpace::Srgb);
    let polar = matches!(space, MixSpace::Oklch | MixSpace::Lch | MixSpace::Hsl);
    let mut stops: Vec<(f32, Color)> = Vec::with_capacity(raw.len());
    for (i, &(p, c)) in raw.iter().enumerate() {
        if i > 0 {
            let (p0, c0) = raw[i - 1];
            if p > p0 {
                let hint = g.hints.iter().find(|(k, _)| *k == i).map(|(_, l)| ((frac(l) - p0) / (p - p0)).clamp(0.0, 1.0));
                let hint = hint.filter(|h| (h - 0.5).abs() > 1e-4);
                let force_hue = polar && g.hue == 1;
                if hint.is_some() || force_hue || (c0 != c && (c0.a != c.a || space != MixSpace::Srgb)) {
                    let steps = if polar { 36 } else if hint.is_some() { 16 } else { 8 };
                    for k in 1..steps {
                        let x = k as f32 / steps as f32;
                        let t = match hint {
                            Some(h) if h <= 0.0 => 1.0,
                            Some(h) if h >= 1.0 => 0.0,
                            Some(h) => x.powf(0.5f32.ln() / h.ln()),
                            None => x,
                        };
                        stops.push((p0 + (p - p0) * x, crate::css::values::interpolate_color_hue(space, g.hue, c0, c, t)));
                    }
                }
            }
        }
        stops.push((p, c));
    }
    Some(GradSpec { kind: if g.radial { 1 } else if g.conic { 2 } else { 0 }, start, end, radius, stops, repeating: g.repeating, angle })
}

/// `background-repeat: round`: shrink or stretch the tile so a whole number
/// fits the positioning area (keeping the aspect ratio when only one axis
/// rounds and the other size is `auto`).
fn round_tiles(layer: &BgLayer, area: Rect, tw: f32, th: f32) -> (f32, f32) {
    let rx = layer.repeat.0 && layer.rmode.0 == 2;
    let ry = layer.repeat.1 && layer.rmode.1 == 2;
    if !rx && !ry {
        return (tw, th);
    }
    let (mut w, mut h) = (tw, th);
    if rx && area.w > 0.0 {
        w = area.w / (area.w / tw).round().max(1.0);
    }
    if ry && area.h > 0.0 {
        h = area.h / (area.h / th).round().max(1.0);
    }
    let (aw, ah) = match &layer.size {
        BgSize::Auto => (true, true),
        BgSize::Explicit(a, b) => (a.is_auto(), b.is_auto()),
        _ => (false, false),
    };
    if rx && !ry && ah {
        h = th * w / tw;
    }
    if ry && !rx && aw {
        w = tw * h / th;
    }
    (w, h)
}

/// Tile origin, effective repetition and `space` gaps for a background layer.
fn place_tiles(layer: &BgLayer, area: Rect, tw: f32, th: f32) -> (f32, f32, (bool, bool), (f32, f32)) {
    let mut px = area.x + layer.pos.0.resolve(area.w - tw);
    let mut py = area.y + layer.pos.1.resolve(area.h - th);
    let mut rep = layer.repeat;
    let mut gap = (0.0, 0.0);
    if rep.0 && layer.rmode.0 == 1 {
        let n = (area.w / tw + 1e-4).floor();
        if n >= 2.0 {
            gap.0 = (area.w - n * tw) / (n - 1.0);
            px = area.x;
            if gap.0 < 0.01 {
                gap.0 = 0.0;
            }
        } else {
            rep.0 = false;
        }
    }
    if rep.1 && layer.rmode.1 == 1 {
        let n = (area.h / th + 1e-4).floor();
        if n >= 2.0 {
            gap.1 = (area.h - n * th) / (n - 1.0);
            py = area.y;
            if gap.1 < 0.01 {
                gap.1 = 0.0;
            }
        } else {
            rep.1 = false;
        }
    }
    (px, py, rep, gap)
}

/// Tile rectangles covering `clip` for a tiling with gaps between tiles.
fn spaced_tiles(clip: Rect, tile: Rect, rep: (bool, bool), gap: (f32, f32)) -> Vec<Rect> {
    let axis = |r: bool, t0: f32, tl: f32, g: f32, c0: f32, cl: f32| -> Vec<f32> {
        if !r {
            return vec![t0];
        }
        let period = tl + g;
        let k0 = ((c0 - t0) / period).floor() as i64;
        let k1 = ((c0 + cl - t0) / period).ceil() as i64;
        (k0..=k1).take(512).map(|k| t0 + k as f32 * period).collect()
    };
    let xs = axis(rep.0, tile.x, tile.w, gap.0, clip.x, clip.w);
    let ys = axis(rep.1, tile.y, tile.h, gap.1, clip.y, clip.h);
    let mut out = Vec::new();
    for &y in &ys {
        for &x in &xs {
            let t = Rect::new(x, y, tile.w, tile.h);
            if t.intersect(&clip).w > 0.0 && t.intersect(&clip).h > 0.0 {
                out.push(t);
            }
        }
    }
    out.truncate(8192);
    out
}

/// Corner radius Chrome's theme gives a native control whose CSS radius is 0.
fn native_radius(b: &crate::layout::LayoutBox, s: &ComputedStyle) -> Option<f32> {
    let BoxKind::Replaced(k) = &b.kind else { return None };
    let ua_border = |c: Color| c == Color::rgb(118, 118, 118) || c == Color::rgb(133, 133, 133) || c == Color::rgba(118, 118, 118, 77);
    match k {
        ReplacedKind::TextInput | ReplacedKind::DateTime if s.border_style[0] == BorderStyle::Inset => Some(2.0),
        ReplacedKind::Button if s.border_style[0] == BorderStyle::Outset => Some(4.0),
        ReplacedKind::ColorWell => Some(4.0),
        ReplacedKind::Select | ReplacedKind::TextArea if s.border_style[0] == BorderStyle::Solid && ua_border(s.border_color_at(0)) => Some(2.0),
        _ => None,
    }
}

/// Display text of a date/time input: (text, is the empty pattern).
fn datetime_text(kind: &str, value: &str) -> String {
    let v = value.trim();
    let two = |n: u32| format!("{n:02}");
    let time12 = |t: &str| -> Option<String> {
        let mut it = t.split(':');
        let h: u32 = it.next()?.parse().ok()?;
        let m: u32 = it.next()?.get(..2)?.parse().ok()?;
        if h > 23 || m > 59 {
            return None;
        }
        let (h12, ap) = match h {
            0 => (12, "AM"),
            1..=11 => (h, "AM"),
            12 => (12, "PM"),
            _ => (h - 12, "PM"),
        };
        Some(format!("{}:{} {}", two(h12), two(m), ap))
    };
    let date = |d: &str| -> Option<String> {
        let p: Vec<&str> = d.split('-').collect();
        if p.len() != 3 {
            return None;
        }
        let (y, m, dd): (u32, u32, u32) = (p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?);
        if !(1..=12).contains(&m) || !(1..=31).contains(&dd) {
            return None;
        }
        Some(format!("{}/{}/{:04}", two(m), two(dd), y))
    };
    match kind {
        "time" => time12(v).unwrap_or_else(|| "--:-- --".into()),
        "datetime-local" => v
            .split_once(['T', ' '])
            .and_then(|(d, t)| Some(format!("{}, {}", date(d)?, time12(t)?)))
            .unwrap_or_else(|| "mm/dd/yyyy, --:-- --".into()),
        "month" => {
            let ok = v.len() == 7 && v.as_bytes()[4] == b'-' && v[..4].parse::<u32>().is_ok() && v[5..].parse::<u32>().map(|m| (1..=12).contains(&m)).unwrap_or(false);
            if ok { v.to_string() } else { "--------- ----".into() }
        }
        "week" => {
            let w = v.split_once("-W").and_then(|(y, w)| Some((y.parse::<u32>().ok()?, w.parse::<u32>().ok()?)));
            match w {
                Some((y, w)) if (1..=53).contains(&w) => format!("Week {}, {:04}", two(w), y),
                _ => "Week --, ----".into(),
            }
        }
        _ => date(v).unwrap_or_else(|| "mm/dd/yyyy".into()),
    }
}

/// Build the display list + hit regions for a laid-out page.
/// Masks that need a real layer: gradients, and images over real content
/// (a lone image mask on an empty box tints its background instead).
fn generic_mask(m: &BgLayer, b: &crate::layout::LayoutBox) -> bool {
    match &m.image {
        BgImage::Gradient(_) => true,
        BgImage::Url(_) => !b.children.is_empty() || matches!(b.kind, crate::layout::BoxKind::Replaced(_)),
        _ => false,
    }
}

thread_local! {
    /// foreignObject nesting while painting one (an SVG inside its HTML…).
    static FO_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub fn build_display_list(inp: &mut PaintInput) -> (DisplayList, Vec<HitRegion>) {
    let b = Builder::new(inp);
    b.build()
}

/// Turn computed `filter` functions into display-list filters. Color
/// functions become 4×5 matrices (Filter Effects 1, applied in sRGB as
/// browsers do for the CSS shorthand functions); identity steps are dropped.
fn resolve_filters(ops: &[crate::css::style::FilterOp], current: Color) -> Vec<super::Filter> {
    use crate::css::style::FilterOp as F;
    use super::Filter;
    let mat3 = |m: [f32; 9]| -> [f32; 20] {
        [m[0], m[1], m[2], 0.0, 0.0, m[3], m[4], m[5], 0.0, 0.0, m[6], m[7], m[8], 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]
    };
    let mut out = Vec::new();
    for op in ops {
        let f = match *op {
            F::Blur(r) if r > 0.0 => Filter::Blur(r),
            F::Blur(_) => continue,
            F::Brightness(a) if (a - 1.0).abs() > 1e-4 => Filter::ColorMatrix(mat3([a, 0.0, 0.0, 0.0, a, 0.0, 0.0, 0.0, a])),
            F::Contrast(a) if (a - 1.0).abs() > 1e-4 => {
                let o = (1.0 - a) / 2.0;
                Filter::ColorMatrix([a, 0.0, 0.0, 0.0, o, 0.0, a, 0.0, 0.0, o, 0.0, 0.0, a, 0.0, o, 0.0, 0.0, 0.0, 1.0, 0.0])
            }
            F::Grayscale(a) if a > 1e-4 => {
                let k = 1.0 - a;
                Filter::ColorMatrix(mat3([
                    0.2126 + 0.7874 * k, 0.7152 - 0.7152 * k, 0.0722 - 0.0722 * k,
                    0.2126 - 0.2126 * k, 0.7152 + 0.2848 * k, 0.0722 - 0.0722 * k,
                    0.2126 - 0.2126 * k, 0.7152 - 0.7152 * k, 0.0722 + 0.9278 * k,
                ]))
            }
            F::Sepia(a) if a > 1e-4 => {
                let k = 1.0 - a;
                Filter::ColorMatrix(mat3([
                    0.393 + 0.607 * k, 0.769 - 0.769 * k, 0.189 - 0.189 * k,
                    0.349 - 0.349 * k, 0.686 + 0.314 * k, 0.168 - 0.168 * k,
                    0.272 - 0.272 * k, 0.534 - 0.534 * k, 0.131 + 0.869 * k,
                ]))
            }
            F::Saturate(v) if (v - 1.0).abs() > 1e-4 => Filter::ColorMatrix(mat3([
                0.213 + 0.787 * v, 0.715 - 0.715 * v, 0.072 - 0.072 * v,
                0.213 - 0.213 * v, 0.715 + 0.285 * v, 0.072 - 0.072 * v,
                0.213 - 0.213 * v, 0.715 - 0.715 * v, 0.072 + 0.928 * v,
            ])),
            F::HueRotate(deg) if deg.rem_euclid(360.0) > 1e-3 => {
                let (sn, cs) = deg.to_radians().sin_cos();
                Filter::ColorMatrix(mat3([
                    0.213 + cs * 0.787 - sn * 0.213, 0.715 - cs * 0.715 - sn * 0.715, 0.072 - cs * 0.072 + sn * 0.928,
                    0.213 - cs * 0.213 + sn * 0.143, 0.715 + cs * 0.285 + sn * 0.140, 0.072 - cs * 0.072 - sn * 0.283,
                    0.213 - cs * 0.213 - sn * 0.787, 0.715 - cs * 0.715 + sn * 0.715, 0.072 + cs * 0.928 + sn * 0.072,
                ]))
            }
            F::Invert(a) if a > 1e-4 => {
                let k = 1.0 - 2.0 * a;
                Filter::ColorMatrix([k, 0.0, 0.0, 0.0, a, 0.0, k, 0.0, 0.0, a, 0.0, 0.0, k, 0.0, a, 0.0, 0.0, 0.0, 1.0, 0.0])
            }
            F::Opacity(a) if a < 1.0 - 1e-4 => Filter::ColorMatrix([1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, a, 0.0]),
            F::DropShadow { x, y, blur, color } => Filter::DropShadow { x, y, blur, color: color.resolve(current) },
            _ => continue,
        };
        out.push(f);
    }
    out
}
