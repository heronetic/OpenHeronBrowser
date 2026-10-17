// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Block formatting context: normal flow, floats, margin collapsing.

use super::*;

#[derive(Clone, Debug)]
pub struct FloatBox {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub left: bool,
    /// `shape-outside`: the float area per row (for line boxes only).
    pub shape: Option<Rc<FloatShape>>,
}

/// A float's `shape-outside` area sampled in rows of `step` px from the
/// margin box top: each row's (left, right) extent from the margin box's
/// left edge, or None where the shape is empty.
#[derive(Debug)]
pub struct FloatShape {
    pub step: f32,
    pub rows: Vec<Option<(f32, f32)>>,
}

impl FloatShape {
    /// The shape's horizontal extent over rows [y0, y1) (margin-box relative).
    pub fn extent(&self, y0: f32, y1: f32) -> Option<(f32, f32)> {
        let a = ((y0 / self.step).floor().max(0.0)) as usize;
        let b = ((y1 / self.step).ceil().max(0.0) as usize).min(self.rows.len());
        let mut out: Option<(f32, f32)> = None;
        for r in self.rows.get(a..b.max(a)).unwrap_or(&[]).iter().flatten() {
            out = Some(match out {
                Some((l, rr)) => (l.min(r.0), rr.max(r.1)),
                None => *r,
            });
        }
        out
    }
}

#[derive(Default, Debug)]
pub struct FloatCtx {
    pub floats: Vec<FloatBox>,
}

impl FloatCtx {
    /// Available horizontal band [x0, x1) for a band [y, y+h) inside [cx0, cx1].
    pub fn band(&self, y: f32, h: f32, cx0: f32, cx1: f32) -> (f32, f32) {
        let mut x0 = cx0;
        let mut x1 = cx1;
        let h = h.max(0.01);
        for f in &self.floats {
            if f.y < y + h && f.y + f.h > y && f.h > 0.0 {
                if f.left {
                    x0 = x0.max(f.x + f.w);
                } else {
                    x1 = x1.min(f.x);
                }
            }
        }
        (x0, x1.max(x0))
    }

    /// Find a position for a float of size (w, h) at or below y_min.
    pub fn place(&mut self, w: f32, h: f32, left: bool, y_min: f32, cx0: f32, cx1: f32) -> (f32, f32) {
        let mut y = y_min;
        // A float may not be placed above earlier floats.
        if let Some(last) = self.floats.last() {
            y = y.max(last.y);
        }
        for _ in 0..200 {
            let (x0, x1) = self.band(y, h, cx0, cx1);
            if x1 - x0 >= w - 0.01 || (x0 <= cx0 + 0.01 && x1 >= cx1 - 0.01) {
                let x = if left { x0 } else { x1 - w };
                self.floats.push(FloatBox { x, y, w, h, left, shape: None });
                return (x, y);
            }
            // Move down to the next float bottom.
            let next = self
                .floats
                .iter()
                .filter(|f| f.y + f.h > y + 0.01)
                .map(|f| f.y + f.h)
                .fold(f32::INFINITY, f32::min);
            if !next.is_finite() {
                break;
            }
            y = next;
        }
        let x = if left { cx0 } else { cx1 - w };
        self.floats.push(FloatBox { x, y, w, h, left, shape: None });
        (x, y)
    }

    /// Like `band`, for line boxes: floats with `shape-outside` only exclude
    /// their shape.
    pub fn band_lines(&self, y: f32, h: f32, cx0: f32, cx1: f32) -> (f32, f32) {
        let mut x0 = cx0;
        let mut x1 = cx1;
        let h = h.max(0.01);
        for f in &self.floats {
            if f.y < y + h && f.y + f.h > y && f.h > 0.0 {
                let (l, r) = match &f.shape {
                    Some(sh) => match sh.extent(y - f.y, y + h - f.y) {
                        Some((a, b)) => (f.x + a.max(0.0), f.x + b.min(f.w)),
                        None => continue,
                    },
                    None => (f.x, f.x + f.w),
                };
                if f.left {
                    x0 = x0.max(r);
                } else {
                    x1 = x1.min(l);
                }
            }
        }
        (x0, x1.max(x0))
    }

    /// Attach a shape to the float placed last.
    pub fn set_last_shape(&mut self, shape: Option<Rc<FloatShape>>) {
        if let Some(f) = self.floats.last_mut() {
            f.shape = shape;
        }
    }

    pub fn clear_y(&self, clear: Clear) -> f32 {
        self.floats
            .iter()
            .filter(|f| match clear {
                Clear::Left => f.left,
                Clear::Right => !f.left,
                Clear::Both => true,
                Clear::None => false,
            })
            .map(|f| f.y + f.h)
            .fold(f32::NEG_INFINITY, f32::max)
    }

    pub fn bottom(&self) -> f32 {
        self.floats.iter().map(|f| f.y + f.h).fold(0.0, f32::max)
    }

    pub fn has_floats_in(&self, y: f32, h: f32) -> bool {
        self.floats.iter().any(|f| f.y < y + h.max(0.01) && f.y + f.h > y)
    }
}

/// A text style on the first formatted line: properties it shares with the
/// block (i.e. inherited unchanged) take the `::first-line` values.
fn first_line_style(t: &ComputedStyle, block: &ComputedStyle, fl: &ComputedStyle) -> ComputedStyle {
    let mut s = t.clone();
    macro_rules! take {
        ($($f:ident),*) => {{ $( if t.$f == block.$f { s.$f = fl.$f.clone(); } )* }};
    }
    take!(color, font_family, font_size, font_weight, font_style, font_variant_small_caps, letter_spacing, word_spacing, text_transform, text_shadow);
    if fl.text_decoration_line != 0 && t.text_decoration_line == block.text_decoration_line {
        s.text_decoration_line = fl.text_decoration_line;
        s.text_decoration_color = fl.text_decoration_color.clone();
    }
    s
}

impl Layout<'_> {
    /// Text boxes of an inline formatting context in order (through inline boxes).
    fn ifc_text_boxes(&self, id: BoxId, out: &mut Vec<BoxId>) {
        for &c in &self.boxes[id].children {
            let b = &self.boxes[c];
            if b.out_of_flow || b.is_float {
                continue;
            }
            match b.kind {
                BoxKind::Text(_) | BoxKind::Generated(_) => out.push(c),
                BoxKind::InlineBox => self.ifc_text_boxes(c, out),
                _ => {}
            }
        }
    }

    /// Inline layout with `::first-line` styles: lay the content out with the
    /// first-line styles everywhere to find where the first line ends, split
    /// the text there and give only the first line's text those styles.
    fn layout_inline_first_line(&mut self, id: BoxId, fl: &Rc<ComputedStyle>, content_w: f32, cb_h: Option<f32>, fctx: &mut FloatCtx, origin: (f32, f32)) -> inline::InlineLayout {
        let block = self.boxes[id].style.clone();
        let mut texts = Vec::new();
        self.ifc_text_boxes(id, &mut texts);
        // Undo the styling of an earlier pass over this block.
        for &t in &texts {
            if let Some(orig) = self.fl_orig.get(&t) {
                self.boxes[t].style = orig.clone();
            }
        }
        let originals: Vec<Rc<ComputedStyle>> = texts.iter().map(|&t| self.boxes[t].style.clone()).collect();
        for (k, &t) in texts.iter().enumerate() {
            self.fl_orig.insert(t, originals[k].clone());
            self.boxes[t].style = Rc::new(first_line_style(&originals[k], &block, fl));
        }
        let floats = fctx.floats.len();
        let probe = inline::layout_inline(self, id, content_w, cb_h, fctx, origin);
        fctx.floats.truncate(floats);
        for (k, &t) in texts.iter().enumerate() {
            self.boxes[t].style = originals[k].clone();
        }
        // Non-space characters of each text box on the first line, and whether it continues.
        let mut on_first: Vec<(BoxId, usize)> = Vec::new();
        if let Some(line) = probe.lines.first() {
            for f in &line.frags {
                if let inline::FragKind::Text { text, .. } = &f.kind {
                    let n = text.chars().filter(|c| !c.is_whitespace()).count();
                    match on_first.iter_mut().find(|e| e.0 == f.box_id) {
                        Some(e) => e.1 += n,
                        None => on_first.push((f.box_id, n)),
                    }
                }
            }
        }
        let continues = |b: BoxId| probe.lines.iter().skip(1).any(|l| l.frags.iter().any(|f| f.box_id == b));
        for (t, n) in on_first {
            let Some(k) = texts.iter().position(|&x| x == t) else { continue };
            if continues(t) {
                // Split the text box after the first line's characters.
                let BoxKind::Text(text) = self.boxes[t].kind.clone() else {
                    self.boxes[t].style = Rc::new(first_line_style(&originals[k], &block, fl));
                    continue;
                };
                let mut seen = 0;
                let mut cut = text.len();
                for (i, c) in text.char_indices() {
                    if seen == n {
                        // keep following whitespace on the first line
                        if !c.is_whitespace() {
                            cut = i;
                            break;
                        }
                        continue;
                    }
                    if !c.is_whitespace() {
                        seen += 1;
                    }
                }
                if cut == 0 || cut >= text.len() {
                    if cut != 0 {
                        self.boxes[t].style = Rc::new(first_line_style(&originals[k], &block, fl));
                    }
                    continue;
                }
                let Some(parent) = self.boxes[t].parent else { continue };
                let tail = self.new_box(self.boxes[t].node, originals[k].clone(), BoxKind::Text(Rc::from(&text[cut..])), Some(parent));
                self.boxes[t].kind = BoxKind::Text(Rc::from(&text[..cut]));
                if let Some(pos) = self.boxes[parent].children.iter().position(|&c| c == t) {
                    self.boxes[parent].children.insert(pos + 1, tail);
                }
                self.boxes[t].style = Rc::new(first_line_style(&originals[k], &block, fl));
            } else {
                self.boxes[t].style = Rc::new(first_line_style(&originals[k], &block, fl));
            }
        }
        inline::layout_inline(self, id, content_w, cb_h, fctx, origin)
    }

    /// Rasterize a float's `shape-outside` (grown by `shape-margin`, clipped
    /// to its margin box) into per-row extents.
    pub fn float_shape(&self, id: BoxId) -> Option<Rc<FloatShape>> {
        use crate::css::style::ShapeOutside;
        let b = &self.boxes[id];
        let so = b.style.shape_outside.clone()?;
        let (mw, mh) = (b.outer_w(), b.outer_h());
        if mw <= 0.0 || mh <= 0.0 {
            return None;
        }
        let m = b.margin;
        let border_box = crate::paint::Rect::new(m[3], m[0], b.w, b.h);
        let refbox = |k: u8| match k {
            1 => crate::paint::Rect::new(border_box.x + b.border[3], border_box.y + b.border[0], b.w - b.border[1] - b.border[3], b.h - b.border[0] - b.border[2]),
            2 => crate::paint::Rect::new(border_box.x + b.border[3] + b.padding[3], border_box.y + b.border[0] + b.padding[0], (b.w - self.hpad(id)).max(0.0), (b.h - self.vpad(id)).max(0.0)),
            3 => crate::paint::Rect::new(0.0, 0.0, mw, mh),
            _ => border_box,
        };
        let path = match &*so {
            ShapeOutside::Shape(sh, k) => crate::paint::builder::basic_shape_path(sh, refbox(*k)),
            ShapeOutside::Box(k) => {
                let r = refbox(*k);
                let radii = crate::paint::builder::radii_px(&b.style, b.w, b.h);
                // The border radii, grown or shrunk to the reference box.
                let d = match k {
                    3 => [m[0], m[1], m[2], m[3]],
                    1 => [-b.border[0], -b.border[1], -b.border[2], -b.border[3]],
                    2 => [-(b.border[0] + b.padding[0]), -(b.border[1] + b.padding[1]), -(b.border[2] + b.padding[2]), -(b.border[3] + b.padding[3])],
                    _ => [0.0; 4],
                };
                let mut rr = radii;
                // corners tl tr br bl: x grows with the left/right offset, y with top/bottom
                let (dx, dy) = ([d[3], d[1], d[1], d[3]], [d[0], d[0], d[2], d[2]]);
                for i in 0..4 {
                    if rr.x[i] > 0.0 && rr.y[i] > 0.0 {
                        rr.x[i] = (rr.x[i] + dx[i]).max(0.0);
                        rr.y[i] = (rr.y[i] + dy[i]).max(0.0);
                    }
                }
                if r.w <= 0.0 || r.h <= 0.0 {
                    None
                } else if rr.any() {
                    crate::paint::raster::rrect_path(r, rr.scaled_to(r.w, r.h))
                } else {
                    tiny_skia::Rect::from_xywh(r.x, r.y, r.w, r.h).map(tiny_skia::PathBuilder::from_rect)
                }
            }
        };
        let margin = b.style.shape_margin.resolve(mw).max(0.0);
        // Sample at up to 1px rows (coarser for very tall floats).
        let scale = (1.0f32).min(2048.0 / mw.max(mh));
        let (pw, ph) = ((mw * scale).ceil().max(1.0) as u32, (mh * scale).ceil().max(1.0) as u32);
        let mut rows = vec![None; ph as usize];
        if let (Some(path), Some(mut pm)) = (path, tiny_skia::Pixmap::new(pw, ph)) {
            let mut paint = tiny_skia::Paint::default();
            paint.set_color_rgba8(0, 0, 0, 255);
            paint.anti_alias = true;
            let t = tiny_skia::Transform::from_scale(scale, scale);
            pm.fill_path(&path, &paint, tiny_skia::FillRule::Winding, t, None);
            if margin > 0.0 {
                let stroke = tiny_skia::Stroke { width: 2.0 * margin, line_join: tiny_skia::LineJoin::Round, line_cap: tiny_skia::LineCap::Round, ..Default::default() };
                pm.stroke_path(&path, &paint, &stroke, t, None);
            }
            let px = pm.pixels();
            for (y, row) in rows.iter_mut().enumerate() {
                let line = &px[y * pw as usize..(y + 1) * pw as usize];
                let first = line.iter().position(|p| p.alpha() > 127);
                let last = line.iter().rposition(|p| p.alpha() > 127);
                if let (Some(a), Some(z)) = (first, last) {
                    *row = Some((a as f32 / scale, (z + 1) as f32 / scale));
                }
            }
        }
        Some(Rc::new(FloatShape { step: 1.0 / scale, rows }))
    }

    /// A laid-out box whose top and bottom margins collapse through it
    /// (no height, borders, padding, line boxes or non-collapsing children).
    fn self_collapsing(&self, c: BoxId, depth: u32) -> bool {
        let b = &self.boxes[c];
        if depth > 32 || b.h != 0.0 || b.border[0] + b.border[2] + b.padding[0] + b.padding[2] != 0.0 || b.is_float || self.is_bfc(c) && depth > 0 {
            return false;
        }
        if let Some(i) = b.ifc.as_ref() {
            // Inline content that produced no line boxes (e.g. only collapsible whitespace).
            return i.lines.is_empty();
        }
        b.children.iter().all(|&k| self.boxes[k].out_of_flow || self.boxes[k].is_float || self.self_collapsing(k, depth + 1))
    }
}

/// A set of adjoining margins: the largest positive and the most negative.
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Strut {
    pub pos: f32,
    pub neg: f32,
}

impl Strut {
    pub fn of(m: f32) -> Strut {
        if m >= 0.0 {
            Strut { pos: m, neg: 0.0 }
        } else {
            Strut { pos: 0.0, neg: m }
        }
    }
    pub fn join(self, o: Strut) -> Strut {
        Strut { pos: self.pos.max(o.pos), neg: self.neg.min(o.neg) }
    }
    pub fn sum(self) -> f32 {
        self.pos + self.neg
    }
}

impl Layout<'_> {
    fn top_strut(&self, c: BoxId) -> Strut {
        self.boxes[c].top_strut.unwrap_or(Strut::of(self.boxes[c].margin[0]))
    }
    fn bot_strut(&self, c: BoxId) -> Strut {
        self.boxes[c].bot_strut.unwrap_or(Strut::of(self.boxes[c].margin[2]))
    }
}

impl<'a> Layout<'a> {
    /// Find the containing block for an out-of-flow box and register it.
    pub fn register_oof(&mut self, id: BoxId, static_x: f32, static_y: f32) {
        self.boxes[id].static_pos = (static_x, static_y);
        let fixed = self.boxes[id].style.position == Position::Fixed;
        let mut cb = self.boxes[id].parent;
        let mut root = 0;
        while let Some(c) = cb {
            root = c;
            if !fixed {
                if self.boxes[c].style.is_abs_cb() && self.boxes[c].parent.is_some() {
                    break;
                }
            } else if self.boxes[c].style.is_fixed_cb() && self.boxes[c].parent.is_some() {
                break;
            }
            cb = self.boxes[c].parent;
        }
        let cb = cb.unwrap_or(root);
        self.boxes[id].fixed = fixed && self.boxes[cb].parent.is_none();
        if !self.boxes[cb].abs_children.contains(&id) {
            self.boxes[cb].abs_children.push(id);
        }
        // The root viewport box lays out its list at the very end.
        if cb != root || self.boxes[cb].parent.is_some() {
            // nothing: positioned ancestors lay out their lists at the end of their own layout
        }
    }

    pub fn first_baseline(&self, id: BoxId) -> Option<f32> {
        let b = &self.boxes[id];
        if let Some(ifc) = &b.ifc {
            return ifc.lines.first().map(|l| b.content_y() + l.y + l.baseline);
        }
        if let Some(bl) = b.baseline {
            // Replaced boxes, flex containers (whose layout already chose the
            // baseline of their first item) and tables (their first row's)
            // know their own baseline.
            if matches!(b.kind, BoxKind::Replaced(_))
                || b.style.display.is_flex()
                || b.style.display.is_grid()
                || matches!(b.style.display, crate::css::style::Display::Table | crate::css::style::Display::InlineTable)
            {
                return Some(bl);
            }
        }
        for &c in &b.children {
            let cb = &self.boxes[c];
            if cb.out_of_flow || cb.is_float {
                continue;
            }
            if let Some(bl) = self.first_baseline(c) {
                return Some(cb.y + bl);
            }
        }
        None
    }

    pub fn last_baseline(&self, id: BoxId) -> Option<f32> {
        let b = &self.boxes[id];
        if let Some(ifc) = &b.ifc {
            return ifc.lines.last().map(|l| b.content_y() + l.y + l.baseline);
        }
        if matches!(b.style.display, crate::css::style::Display::Table | crate::css::style::Display::InlineTable) {
            if let Some(bl) = b.baseline {
                return Some(bl);
            }
        }
        // Form controls have a baseline (their text's), also when block-level.
        if let BoxKind::Replaced(k) = &b.kind {
            if matches!(k, ReplacedKind::TextInput | ReplacedKind::Button | ReplacedKind::Select | ReplacedKind::DateTime | ReplacedKind::File | ReplacedKind::TextArea) {
                return b.baseline;
            }
            return None;
        }
        for &c in b.children.iter().rev() {
            let cb = &self.boxes[c];
            if cb.out_of_flow || cb.is_float {
                continue;
            }
            // A scroll container inside an inline-block contributes its
            // bottom margin edge (Blink's inline-block baseline).
            if cb.style.is_scroll_container() && !cb.style.display.is_flex() && !cb.style.display.is_grid() && !matches!(cb.kind, BoxKind::Replaced(_)) {
                return Some(cb.y + cb.h + cb.margin[2]);
            }
            if let Some(bl) = self.last_baseline(c) {
                return Some(cb.y + bl);
            }
        }
        None
    }

    /// Lay out a float box (shrink-to-fit width) — position set by caller.
    /// The legend a fieldset renders in its border: its first child that is a
    /// `legend` element and neither floated nor absolutely positioned.
    fn rendered_legend(&self, id: BoxId) -> Option<BoxId> {
        let n = self.boxes[id].node?;
        if !self.doc.is_tag(n, "fieldset") {
            return None;
        }
        self.boxes[id].children.iter().copied().find(|&c| {
            let b = &self.boxes[c];
            b.pseudo == 0 && !b.is_float && !b.out_of_flow && !b.inline_level && b.node.map(|ln| self.doc.is_tag(ln, "legend")).unwrap_or(false)
        })
    }

    pub fn layout_float(&mut self, id: BoxId, cb_w: f32, cb_h: Option<f32>) {
        self.layout_box(id, cb_w, cb_h, None, (0.0, 0.0));
    }

    pub fn compute_block_width(&mut self, id: BoxId, cb_w: f32) -> f32 {
        let shrink = {
            let b = &self.boxes[id];
            // Buttons (and button-like inputs, selects) keep a fit-content width
            // even when block-level (HTML rendering §15.5.4).
            let button_like = b.node.map(|n| match self.doc.tag(n) {
                "button" | "select" => true,
                "input" => matches!(self.doc.attr(n, "type").map(|t| t.to_ascii_lowercase()).as_deref(), Some("button" | "submit" | "reset" | "file" | "color" | "image")),
                _ => false,
            });
            b.inline_level || b.is_float || b.out_of_flow || matches!(b.style.display, Display::TableCell | Display::Table | Display::InlineTable) || (button_like == Some(true) && b.pseudo == 0)
        };
        let s = self.boxes[id].style.clone();
        let m = self.boxes[id].margin;
        let w = match self.specified_w(id, Some(cb_w)) {
            Some(w) => w,
            None => {
                let avail = (cb_w - m[1] - m[3]).max(0.0);
                match s.width {
                    Len::MinContent => self.intrinsic_widths(id).0,
                    Len::MaxContent => self.intrinsic_widths(id).1,
                    Len::FitContent => self.shrink_to_fit(id, avail),
                    _ => {
                        if shrink {
                            self.shrink_to_fit(id, avail)
                        } else {
                            avail
                        }
                    }
                }
            }
        };
        let w = self.clamp_w(id, w, cb_w);
        let parent_rtl = self.boxes[id].parent.map(|p| self.boxes[p].style.direction_rtl && self.boxes[p].style.text_align != TextAlign::WebkitCenter).unwrap_or(false);
        // Auto margins (block-level, in-flow only).
        let b = &mut self.boxes[id];
        if !b.inline_level && !b.is_float && !b.out_of_flow {
            let free = cb_w - w - b.margin[1] - b.margin[3];
            let la = s.margin[3].is_auto();
            let ra = s.margin[1].is_auto();
            if la && ra {
                b.margin[3] = (free / 2.0).max(0.0);
                b.margin[1] = b.margin[3];
            } else if la {
                b.margin[3] = free.max(0.0);
            } else if ra {
                b.margin[1] = free;
            } else if free != 0.0 && parent_rtl {
                // Over-constrained in a right-to-left containing block: the left
                // margin absorbs the difference (CSS 2.1 §10.3.3).
                b.margin[3] += free;
            } else if free > 0.0 {
                // `text-align: -webkit-center` on the parent centers block children.
                let centered = b.parent.map(|p| self.boxes[p].style.text_align == TextAlign::WebkitCenter).unwrap_or(false);
                if centered {
                    let b = &mut self.boxes[id];
                    b.margin[3] += free / 2.0;
                    b.margin[1] += free / 2.0;
                }
            }
        }
        w
    }

    /// Block container layout (block or inline children).
    /// Quirks mode (HTML §quirks): the border-box height an auto-height html
    /// element (the viewport less its margins) or body (the html element's
    /// content height less its margins) stretches to.
    fn quirk_fill_h(&self, id: BoxId, cb_h: Option<f32>) -> Option<f32> {
        if !self.doc.quirks {
            return None;
        }
        let b = &self.boxes[id];
        let node = b.node?;
        if b.is_float || b.out_of_flow || b.inline_level || !matches!(b.style.display, Display::Block | Display::FlowRoot) {
            return None;
        }
        let is_html = self.doc.is_tag(node, "html") && self.doc.parent_element(node).is_none();
        let is_body = self.doc.is_tag(node, "body") && self.doc.parent_element(node).map(|p| self.doc.is_tag(p, "html") && self.doc.parent_element(p).is_none()).unwrap_or(false);
        if !is_html && !is_body {
            return None;
        }
        let avail = if is_html { self.vh } else { cb_h? };
        Some((avail - b.margin[0] - b.margin[2]).max(0.0))
    }

    pub fn layout_block(&mut self, id: BoxId, cb_w: f32, cb_h: Option<f32>, parent_fctx: Option<&mut FloatCtx>, foff: (f32, f32)) {
        self.resolve_box_metrics(id, cb_w);
        let w = self.compute_block_width(id, cb_w);
        self.boxes[id].w = w;
        let content_w = (w - self.hpad(id)).max(0.0);
        let spec_h = self.specified_h(id, cb_h);
        // Quirks mode: the html element fills the viewport and the body fills
        // the html element (as minimum heights, definite for percentages).
        let fill_h = if spec_h.is_none() { self.quirk_fill_h(id, cb_h) } else { None };
        // Quirks mode "percentage height calculation": an auto-height block
        // lets percentages inside resolve against its own containing block.
        let quirk_pass = spec_h.is_none() && fill_h.is_none() && self.doc.quirks && {
            let b = &self.boxes[id];
            matches!(b.style.display, Display::Block | Display::ListItem | Display::FlowRoot) && !b.is_float && !b.out_of_flow && !b.inline_level && b.parent.is_some()
        };
        // Children resolve percentages against the used height (min/max applied).
        let own_cb_h = if let Some(f) = fill_h {
            Some((self.clamp_h(id, f, cb_h) - self.vpad(id)).max(0.0))
        } else if (self.boxes[id].cb_passthrough || quirk_pass) && spec_h.is_none() {
            cb_h
        } else {
            spec_h.map(|h| (self.clamp_h(id, h, cb_h) - self.vpad(id)).max(0.0))
        };
        let bfc = self.is_bfc(id);
        let mut own = FloatCtx::default();
        // `foff` is where our margin box starts in the float context's
        // coordinates; floats and line bands use our border box.
        let (fctx, off): (&mut FloatCtx, (f32, f32)) = match parent_fctx {
            Some(p) if !bfc => (p, (foff.0 + self.boxes[id].margin[3], foff.1)),
            _ => (&mut own, (0.0, 0.0)),
        };
        // A fieldset's rendered legend sits in its block-start border
        // (HTML §15.3.13): shrink-to-fit, then the content starts below it.
        let legend = self.rendered_legend(id);
        self.boxes[id].legend = None;
        if let Some(lg) = legend {
            self.resolve_box_metrics(lg, content_w);
            let was = self.boxes[lg].is_float;
            self.boxes[lg].is_float = true;
            self.layout_box(lg, content_w, own_cb_h, None, (0.0, 0.0));
            self.boxes[lg].is_float = was;
            let (lw, lh, m) = (self.boxes[lg].w, self.boxes[lg].h, self.boxes[lg].margin);
            let ls = self.boxes[lg].style.clone();
            let (bt, x0) = (self.boxes[id].border[0], self.boxes[id].content_x());
            let outer_h = lh + m[0] + m[2];
            let rtl = self.boxes[id].style.direction_rtl;
            let x = if ls.margin[3].is_auto() && ls.margin[1].is_auto() {
                x0 + (content_w - lw) / 2.0
            } else if ls.margin[3].is_auto() || rtl && !ls.margin[1].is_auto() {
                x0 + content_w - lw - m[1]
            } else {
                x0 + m[3]
            };
            self.boxes[lg].x = x;
            self.boxes[lg].y = m[0];
            let extra = (outer_h - bt).max(0.0);
            self.boxes[id].padding[0] += extra;
            self.boxes[id].legend = Some((lg, extra / 2.0));
        }
        let cx = self.boxes[id].content_x();
        let cy = self.boxes[id].content_y();
        let kids = self.boxes[id].children.clone();
        let has_inline = kids.iter().any(|&c| self.boxes[c].inline_level);
        let mut content_h;
        let mut first_child_collapsed_top: Option<Strut> = None;
        let mut last_mb_through: Option<Strut> = None;
        if has_inline {
            let fl = self.boxes[id].node.and_then(|n| self.styles.first_line.get(&n).cloned());
            let mut ifc = match fl {
                Some(fl) => self.layout_inline_first_line(id, &fl, content_w, own_cb_h, fctx, (off.0 + cx, off.1 + cy)),
                None => inline::layout_inline(self, id, content_w, own_cb_h, fctx, (off.0 + cx, off.1 + cy)),
            };
            if let Some(n) = self.boxes[id].style.line_clamp {
                inline::clamp_lines(self, id, &mut ifc, n as usize, content_w);
            }
            content_h = ifc.height;
            self.boxes[id].ifc = Some(Box::new(ifc));
        } else {
            let can_collapse_top = !bfc && self.boxes[id].border[0] == 0.0 && self.boxes[id].padding[0] == 0.0 && self.boxes[id].parent.is_some();
            let can_collapse_bottom = !bfc && self.boxes[id].border[2] == 0.0 && self.boxes[id].padding[2] == 0.0 && spec_h.is_none();
            let mut y = 0.0f32;
            let mut pending: Option<Strut> = None; // adjoining margins awaiting the next in-flow box
            let mut first = true;
            let own_mt = Strut::of(self.boxes[id].margin[0]);
            for c in kids {
                if Some(c) == legend {
                    continue;
                }
                if self.boxes[c].out_of_flow {
                    let p = pending.map(|p| p.sum()).unwrap_or(0.0).max(0.0);
                    // In a right-to-left container the static position is the content box's right edge.
                    let rtl = self.boxes[id].style.direction_rtl;
                    self.register_oof(c, if rtl { cx + content_w } else { cx }, cy + y + p);
                    self.boxes[c].static_rtl = rtl;
                    continue;
                }
                if self.boxes[c].is_float {
                    self.resolve_box_metrics(c, content_w);
                    self.layout_float(c, content_w, own_cb_h);
                    let (fw, fh) = (self.boxes[c].outer_w(), self.boxes[c].outer_h());
                    let left = self.boxes[c].style.float == Float::Left;
                    let mut fy = off.1 + cy + y + pending.map(|p| p.sum()).unwrap_or(0.0).max(0.0);
                    let clear = self.boxes[c].style.clear;
                    if clear != Clear::None {
                        fy = fy.max(fctx.clear_y(clear));
                    }
                    let (fx, fy) = fctx.place(fw, fh, left, fy, off.0 + cx, off.0 + cx + content_w);
                    if std::env::var_os("OHB_FLOAT_DEBUG").is_some() {
                        eprintln!("[float] block {:?} {} fw={fw} fh={fh} at ({fx},{fy}) m={:?} w={} nfloats={}", self.boxes[c].node, self.boxes[c].node.map(|n| self.doc.tag(n).to_string()).unwrap_or_default(), self.boxes[c].margin, self.boxes[c].w, fctx.floats.len());
                    }
                    let shape = self.float_shape(c);
                    fctx.set_last_shape(shape);
                    let b = &mut self.boxes[c];
                    b.x = fx - off.0 + b.margin[3];
                    b.y = fy - off.1 + b.margin[0];
                    continue;
                }
                self.resolve_box_metrics(c, content_w);
                let ts0 = Strut::of(self.boxes[c].margin[0]);
                let mut top_gap = pending.unwrap_or_default().join(ts0).sum();
                let collapse_into_parent = first && can_collapse_top;
                if collapse_into_parent {
                    // Child's top margin collapses through us.
                    first_child_collapsed_top = Some(first_child_collapsed_top.unwrap_or(own_mt).join(ts0));
                    top_gap = 0.0;
                }
                let mut child_y = y + top_gap;
                // Clearance.
                let clear = self.boxes[c].style.clear;
                let mut cleared = false;
                if clear != Clear::None {
                    let cy_bfc = fctx.clear_y(clear) - off.1 - cy;
                    if cy_bfc > child_y {
                        child_y = cy_bfc;
                        cleared = true;
                        if collapse_into_parent {
                            first_child_collapsed_top = None;
                            child_y = cy_bfc.max(y + ts0.sum());
                        }
                    }
                }
                // BFC roots avoid floats.
                let child_bfc = self.is_bfc(c);
                let mut avail_w = content_w;
                let mut x_shift = 0.0;
                if child_bfc && !fctx.floats.is_empty() {
                    let band_y = off.1 + cy + child_y;
                    if fctx.has_floats_in(band_y, 1.0) {
                        let (x0, x1) = fctx.band(band_y, 1.0, off.0 + cx, off.0 + cx + content_w);
                        x_shift = x0 - (off.0 + cx);
                        avail_w = (x1 - x0).max(0.0);
                        // If the child has a fixed width that doesn't fit, push it below the floats.
                        if let Some(sw) = self.specified_w(c, Some(content_w)) {
                            if sw > avail_w + 0.5 {
                                let below = fctx.clear_y(Clear::Both) - off.1 - cy;
                                child_y = child_y.max(below);
                                x_shift = 0.0;
                                avail_w = content_w;
                            }
                        }
                    }
                }
                let nfloats = fctx.floats.len();
                let child_off = (off.0 + cx + x_shift, off.1 + cy + child_y);
                self.layout_box(c, avail_w, own_cb_h, Some(fctx), child_off);
                // The child's margins may have grown by collapsing with its descendants'.
                let ts = self.top_strut(c);
                if ts != ts0 && !cleared {
                    if collapse_into_parent {
                        first_child_collapsed_top = Some(first_child_collapsed_top.unwrap_or(own_mt).join(ts));
                    } else {
                        let ny = y + pending.unwrap_or_default().join(ts).sum();
                        if (ny - child_y).abs() > 0.01 {
                            child_y = ny;
                            if fctx.floats.len() > nfloats {
                                // Floats inside were placed at the estimated offset: redo.
                                fctx.floats.truncate(nfloats);
                                self.resolve_box_metrics(c, content_w);
                                self.layout_box(c, avail_w, own_cb_h, Some(fctx), (off.0 + cx + x_shift, off.1 + cy + child_y));
                            }
                        }
                    }
                }
                let ts = self.top_strut(c);
                let bs = self.bot_strut(c);
                let (h, ml) = (self.boxes[c].h, self.boxes[c].margin[3]);
                {
                    let b = &mut self.boxes[c];
                    b.x = cx + x_shift + ml;
                    b.y = cy + child_y;
                }
                // Empty block: its margins collapse through it (it stays at its
                // hypothetical position so floats inside keep their place).
                let empty = h == 0.0 && self.self_collapsing(c, 0);
                if empty && !cleared {
                    if collapse_into_parent {
                        first_child_collapsed_top = Some(first_child_collapsed_top.unwrap_or(own_mt).join(ts).join(bs));
                        pending = None;
                        // stays "first" for collapsing purposes
                        continue;
                    }
                    pending = Some(pending.unwrap_or_default().join(ts).join(bs));
                    continue;
                }
                y = child_y + h;
                pending = Some(bs);
                first = false;
            }
            // Bottom margin of last child.
            let pm = pending.unwrap_or_default();
            if can_collapse_bottom && self.boxes[id].parent.is_some() {
                last_mb_through = Some(pm);
            } else {
                y += pm.sum();
            }
            content_h = y.max(0.0);
        }
        if let Some(m) = first_child_collapsed_top {
            self.boxes[id].top_strut = Some(m);
            self.boxes[id].margin[0] = m.sum();
        }
        if let Some(t) = last_mb_through {
            if t != Strut::default() {
                let m = Strut::of(self.boxes[id].margin[2]).join(t);
                self.boxes[id].bot_strut = Some(m);
                self.boxes[id].margin[2] = m.sum();
            }
        }
        // BFC roots contain their floats.
        if bfc && spec_h.is_none() {
            let fb = own.bottom() - cy;
            content_h = content_h.max(fb);
        }
        // Table cells report their natural content height; the table applies the
        // specified height as a minimum and aligns content within the row.
        let is_cell = self.boxes[id].style.display == Display::TableCell;
        let mut h = match spec_h.filter(|_| !is_cell) {
            Some(h) => h,
            None => {
                // Size containment: the content doesn't contribute to the height.
                let natural = if self.boxes[id].style.contain & CONTAIN_SIZE != 0 {
                    self.boxes[id].style.contain_intrinsic.1.unwrap_or(0.0) + self.vpad(id)
                } else {
                    content_h + self.vpad(id)
                };
                // aspect-ratio on a non-replaced box with auto height (content can't shrink it).
                match self.boxes[id].style.aspect_ratio {
                    Some(r) if r > 0.0 => {
                        let bw = self.boxes[id].w;
                        let from_ratio = if self.boxes[id].style.box_sizing == BoxSizing::BorderBox {
                            bw / r
                        } else {
                            (bw - self.hpad(id)).max(0.0) / r + self.vpad(id)
                        };
                        let overflow_ok = !self.boxes[id].style.overflow_y.clips();
                        if overflow_ok { from_ratio.max(natural) } else { from_ratio }
                    }
                    _ => natural,
                }
            }
        };
        if let Some(f) = fill_h {
            // (The html element's margins may now include its children's collapsed ones.)
            let f = match self.boxes[id].parent {
                Some(p) if self.boxes[p].node.is_none() || self.boxes[p].parent.is_none() => (self.vh - self.boxes[id].margin[0] - self.boxes[id].margin[2]).max(0.0),
                _ => f,
            };
            h = h.max(f);
        }
        h = self.clamp_h(id, h, cb_h);
        self.boxes[id].h = h;
        // Buttons center their content vertically.
        if let Some(n) = self.boxes[id].node {
            if self.doc.is_tag(n, "button") {
                let inner = h - self.vpad(id);
                if inner > content_h + 0.5 {
                    super::table::shift_content(self, id, (inner - content_h) / 2.0);
                }
            }
        }
        self.finish_block_extras(id);
    }

    /// Markers, baselines, scroll extents, abs children.
    pub fn finish_block_extras(&mut self, id: BoxId) {
        // Overflow extent for scroll containers.
        let s = self.boxes[id].style.clone();
        if s.overflow_x.clips() || s.overflow_y.clips() {
            let mut mx: f32 = 0.0;
            let mut my: f32 = 0.0;
            if let Some(ifc) = &self.boxes[id].ifc {
                mx = mx.max(ifc.max_line_w + self.boxes[id].content_x());
                my = my.max(ifc.height + self.boxes[id].content_y());
            }
            for &c in &self.boxes[id].children {
                let b = &self.boxes[c];
                if b.out_of_flow {
                    continue;
                }
                mx = mx.max(b.x + b.w + b.margin[1]);
                my = my.max(b.y + b.h + b.margin[2]);
            }
            // (Child positions are relative to the border box; the extent is
            // the padding box's.)
            let b = &mut self.boxes[id];
            b.scroll_w = (mx - b.border[3] + b.padding[1]).max(b.w - b.border[1] - b.border[3]);
            b.scroll_h = (my - b.border[0] + b.padding[2]).max(b.h - b.border[0] - b.border[2]);
        }
        // List marker (outside).
        if s.display == Display::ListItem && !s.list_style_inside {
            if let Some(spec) = self.boxes[id].list_marker.clone() {
                let ms = spec.style.clone();
                let baseline = self.first_baseline(id).unwrap_or(self.boxes[id].content_y() + ms.font_size);
                let rtl = s.direction_rtl;
                let (cx0, cw) = (self.boxes[id].content_x(), self.boxes[id].content_w());
                let styled = if Rc::ptr_eq(&ms, &self.boxes[id].style) { None } else { Some(ms.clone()) };
                let marker = if let (Some(img), true) = (&spec.image, spec.text.is_empty()) {
                    // Images hang 7px before the content (Blink's kCMarkerPaddingPx).
                    let (iw, ih) = img.natural();
                    let x = if rtl { cx0 + cw + 7.0 } else { cx0 - iw - 7.0 };
                    Marker { text: String::new(), x, baseline, shaped: None, bullet: None, style: styled, image: Some((img.clone(), iw, ih)) }
                } else if let Some(bullet) = spec.bullet {
                    let width = ms.font_size * 0.35;
                    let gap = ms.font_size * 0.55;
                    let x = if rtl { cx0 + cw + gap } else { cx0 - width - gap };
                    Marker { text: spec.text.clone(), x, baseline, shaped: None, bullet: Some(bullet), style: styled, image: None }
                } else {
                    // Text markers end flush with the content start (their suffix
                    // supplies the spacing).
                    let shaped = self.fonts.shape(&spec.text, &ms.font_family, ms.font_weight, ms.font_style, ms.font_size, 0.0);
                    let x = if rtl { cx0 + cw } else { cx0 - shaped.width };
                    Marker { text: spec.text.clone(), x, baseline, shaped: Some(shaped), bullet: None, style: styled, image: None }
                };
                self.boxes[id].marker = Some(marker);
            }
        }
        if s.is_abs_cb() {
            self.layout_abs_children(id);
        }
        self.boxes[id].baseline = self.last_baseline(id);
    }
}

