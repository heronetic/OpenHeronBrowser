// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! CSS Multi-column Layout: the content is laid out as one column of the
//! column width, then cut between lines and unbreakable boxes into columns
//! (balanced, or filled in order with `column-fill: auto` and a height), and
//! each piece is moved into its column. `column-span: all` elements split the
//! flow into independently balanced column sets.

use super::*;

/// Column geometry of a multi-column container, for painting rules.
#[derive(Clone, Debug)]
pub struct MulticolInfo {
    pub col_w: f32,
    pub gap: f32,
    /// Column sets: (top, height, columns used), relative to the content box.
    pub sets: Vec<(f32, f32, usize)>,
}

/// A piece of the flow that cannot be split: a line box or a whole box.
#[derive(Clone, Copy)]
enum AtomRef {
    Line(BoxId, usize),
    Box(BoxId),
}

#[derive(Clone, Copy)]
struct Atom {
    top: f32,
    bottom: f32,
    _what: AtomRef,
}

impl Layout<'_> {
    pub fn is_multicol(&self, id: BoxId) -> bool {
        let s = &self.boxes[id].style;
        (s.columns.is_some() || s.column_width.is_some())
            && matches!(s.display, Display::Block | Display::FlowRoot | Display::ListItem | Display::InlineBlock)
            && !matches!(self.boxes[id].kind, BoxKind::Replaced(_))
    }

    /// Column count and width for an available content width.
    fn column_geometry(&self, id: BoxId, avail: f32) -> (usize, f32, f32) {
        let s = &self.boxes[id].style;
        let gap = match s.column_gap {
            Len::Auto => s.font_size,
            ref l => l.definite(Some(avail)).unwrap_or(s.font_size).max(0.0),
        };
        let n = match (s.columns, s.column_width) {
            (Some(n), None) => n as usize,
            (None, Some(w)) => (((avail + gap) / (w + gap)).floor() as usize).max(1),
            (Some(n), Some(w)) => (n as usize).min((((avail + gap) / (w + gap)).floor() as usize).max(1)),
            (None, None) => 1,
        }
        .clamp(1, 1000);
        let cw = ((avail - gap * (n - 1) as f32) / n as f32).max(0.0);
        (n, cw, gap)
    }

    /// Intrinsic widths of a multi-column container's content.
    pub fn multicol_intrinsic(&mut self, id: BoxId) -> (f32, f32) {
        let kids = self.boxes[id].children.clone();
        let (mut mn, mut mx) = (0.0f32, 0.0f32);
        for c in kids {
            if self.boxes[c].out_of_flow {
                continue;
            }
            let (a, b) = self.intrinsic_widths(c);
            let m = self.boxes[c].margin[1] + self.boxes[c].margin[3];
            mn = mn.max(a + m);
            mx = mx.max(b + m);
        }
        let s = self.boxes[id].style.clone();
        let gap = match s.column_gap {
            Len::Auto => s.font_size,
            ref l => l.definite(None).unwrap_or(s.font_size),
        };
        if let Some(w) = s.column_width {
            mn = mn.min(w).max(0.0);
            mx = mx.max(w);
        }
        let n = s.columns.unwrap_or(1).max(1) as f32;
        (mn, mx * n + gap * (n - 1.0))
    }

    pub fn layout_multicol(&mut self, id: BoxId, cb_w: f32, cb_h: Option<f32>) {
        self.resolve_box_metrics(id, cb_w);
        let w = self.compute_block_width(id, cb_w);
        self.boxes[id].w = w;
        let avail = (w - self.hpad(id)).max(0.0);
        let (n, cw, gap) = self.column_geometry(id, avail);
        let spec_h = self.specified_h(id, cb_h).map(|h| (self.clamp_h(id, h, cb_h) - self.vpad(id)).max(0.0));
        let fill_auto = self.boxes[id].style.column_fill_auto && spec_h.is_some();
        let (cx, cy) = (self.boxes[id].content_x(), self.boxes[id].content_y());
        let kids = self.boxes[id].children.clone();
        let mut y = 0.0f32;
        let mut sets: Vec<(f32, f32, usize)> = Vec::new();
        let mut i = 0;
        while i < kids.len() {
            let c = kids[i];
            if self.boxes[c].out_of_flow {
                self.register_oof(c, cx, cy + y);
                i += 1;
                continue;
            }
            if self.boxes[c].style.column_span_all && !self.boxes[c].inline_level {
                // A spanner: full width, between column sets.
                self.resolve_box_metrics(c, avail);
                self.layout_box(c, avail, spec_h, None, (0.0, 0.0));
                let (mt, mb, ml) = (self.boxes[c].margin[0], self.boxes[c].margin[2], self.boxes[c].margin[3]);
                self.boxes[c].x = cx + ml;
                self.boxes[c].y = cy + y + mt;
                y += mt + self.boxes[c].h + mb;
                i += 1;
                continue;
            }
            // A run of column content.
            let mut run: Vec<BoxId> = Vec::new();
            while i < kids.len() && !(self.boxes[kids[i]].style.column_span_all && !self.boxes[kids[i]].inline_level) {
                if self.boxes[kids[i]].out_of_flow {
                    self.register_oof(kids[i], cx, cy + y);
                } else {
                    run.push(kids[i]);
                }
                i += 1;
            }
            let run_top = y;
            let mut ry = 0.0f32;
            let mut prev_mb: Option<f32> = None;
            // Floats among the column content: lay the content out as one
            // block flow at the column width (text wraps around the floats),
            // then break that into columns.
            let whole = run.len() == kids.iter().filter(|&&k| !self.boxes[k].out_of_flow).count() && run.iter().any(|&c| self.mc_has_float(c, 0));
            if whole {
                let saved = self.boxes[id].style.clone();
                let (sx, sy) = (self.boxes[id].x, self.boxes[id].y);
                let mut st = (*saved).clone();
                st.columns = None;
                st.column_width = None;
                st.box_sizing = BoxSizing::ContentBox;
                st.width = Len::Px(cw);
                st.min_width = Len::Auto;
                st.max_width = Len::None;
                st.height = Len::Auto;
                st.min_height = Len::Auto;
                st.max_height = Len::None;
                self.boxes[id].style = Rc::new(st);
                self.layout_block(id, cb_w, None, None, (0.0, 0.0));
                self.boxes[id].style = saved;
                self.boxes[id].w = w;
                self.boxes[id].x = sx;
                self.boxes[id].y = sy;
                // Children keep their positions from that flow (relative to the box).
                let dx = self.boxes[id].content_x() - cx;
                if dx.abs() > 0.01 {
                    for &c in &run {
                        self.boxes[c].x -= dx;
                    }
                }
            }
            for &c in run.iter().filter(|_| !whole) {
                self.resolve_box_metrics(c, cw);
                self.layout_box(c, cw, None, None, (0.0, 0.0));
                let (mt, mb, ml) = (self.boxes[c].margin[0], self.boxes[c].margin[2], self.boxes[c].margin[3]);
                // Adjacent sibling margins collapse; the first one is kept
                // (the column set is a formatting context).
                let top_gap = match prev_mb {
                    None => mt,
                    Some(p) => {
                        if p >= 0.0 && mt >= 0.0 {
                            p.max(mt)
                        } else if p < 0.0 && mt < 0.0 {
                            p.min(mt)
                        } else {
                            p + mt
                        }
                    }
                };
                self.boxes[c].x = cx + ml;
                self.boxes[c].y = cy + run_top + ry + top_gap;
                ry += top_gap + self.boxes[c].h;
                prev_mb = Some(mb);
            }
            // Atoms of the run, relative to its top.
            let mut atoms: Vec<Atom> = Vec::new();
            for &c in &run {
                let top = self.boxes[c].y - (cy + run_top);
                self.mc_atoms(c, top, &mut atoms);
            }
            atoms.sort_by(|a, b| a.top.total_cmp(&b.top));
            let total = atoms.iter().map(|a| a.bottom).fold(0.0f32, f32::max);
            let starts = if fill_auto {
                let h = (spec_h.unwrap_or(total) - run_top).max(1.0);
                breaks(&atoms, h)
            } else {
                balance(&atoms, n, total)
            };
            let used = starts.len().max(1);
            // Column heights and the moves.
            let mut set_h = 0.0f32;
            for (k, &b0) in starts.iter().enumerate() {
                let b1 = starts.get(k + 1).copied().unwrap_or(f32::INFINITY);
                let bottom = atoms.iter().filter(|a| a.top >= b0 - 0.01 && a.top < b1 - 0.01).map(|a| a.bottom).fold(b0, f32::max);
                set_h = set_h.max(bottom - b0);
            }
            if fill_auto {
                set_h = (spec_h.unwrap_or(set_h) - run_top).max(set_h.min(spec_h.unwrap_or(set_h)));
            }
            let pitch = cw + gap;
            for &c in &run {
                let top = self.boxes[c].y - (cy + run_top);
                self.mc_place(c, top, &starts, pitch, (0.0, 0.0));
            }
            sets.push((run_top, set_h, used));
            y += set_h;
        }
        let content_h = spec_h.unwrap_or(y);
        let h = self.clamp_h(id, content_h + self.vpad(id), cb_h);
        self.boxes[id].h = h;
        self.boxes[id].multicol = Some(Rc::new(MulticolInfo { col_w: cw, gap, sets }));
        self.finish_block_extras(id);
        self.boxes[id].baseline = self.first_baseline(id);
    }

    /// A float among a box's in-flow content (anonymous wrappers included).
    fn mc_has_float(&self, b: BoxId, depth: u32) -> bool {
        let bx = &self.boxes[b];
        if bx.is_float || bx.style.float != Float::None {
            return true;
        }
        depth < 2 && bx.node.is_none() && bx.children.iter().any(|&c| self.mc_has_float(c, depth + 1))
    }

    /// Whether a box must stay in one piece.
    fn mc_unbreakable(&self, b: BoxId) -> bool {
        let bx = &self.boxes[b];
        let s = &bx.style;
        matches!(bx.kind, BoxKind::Replaced(_))
            || bx.inline_level
            || bx.is_float
            || s.break_inside_avoid
            || s.display.is_flex()
            || s.display.is_grid()
            || matches!(s.display, Display::Table | Display::InlineTable)
            || (bx.ifc.is_none() && bx.children.iter().all(|&c| self.boxes[c].out_of_flow))
    }

    fn mc_atoms(&self, b: BoxId, top: f32, out: &mut Vec<Atom>) {
        if self.mc_unbreakable(b) {
            out.push(Atom { top, bottom: top + self.boxes[b].h, _what: AtomRef::Box(b) });
            return;
        }
        let bx = &self.boxes[b];
        if let Some(ifc) = &bx.ifc {
            // orphans: 2 and widows: 2 (the initial values): the first two and
            // the last two lines stay together; shorter paragraphs never split.
            const ORPHANS: usize = 2;
            const WIDOWS: usize = 2;
            let base = top + bx.content_y();
            let n = ifc.lines.len();
            if n < ORPHANS + WIDOWS {
                if let (Some(f), Some(l)) = (ifc.lines.first(), ifc.lines.last()) {
                    out.push(Atom { top: base + f.y, bottom: base + l.y + l.h, _what: AtomRef::Box(b) });
                }
                return;
            }
            let mut i = 0;
            while i < n {
                let j = if i == 0 { ORPHANS } else if i + WIDOWS >= n { n } else if i + 1 + WIDOWS > n { n } else { i + 1 };
                let j = if j < n && j + WIDOWS > n { n - WIDOWS } else { j }.max(i + 1);
                let (f, l) = (&ifc.lines[i], &ifc.lines[j - 1]);
                out.push(Atom { top: base + f.y, bottom: base + l.y + l.h, _what: AtomRef::Line(b, i) });
                i = j;
            }
            return;
        }
        let before = out.len();
        for &c in &bx.children {
            if self.boxes[c].out_of_flow {
                continue;
            }
            self.mc_atoms(c, top + self.boxes[c].y, out);
        }
        if out.len() == before {
            out.push(Atom { top, bottom: top + bx.h, _what: AtomRef::Box(b) });
        }
    }

    /// Move a box (or its lines / children) into the columns its atoms fall in.
    /// `applied` is the move already applied to its ancestors.
    fn mc_place(&mut self, b: BoxId, top: f32, starts: &[f32], pitch: f32, applied: (f32, f32)) {
        let col = |y: f32| starts.iter().rposition(|&s| s <= y + 0.01).unwrap_or(0);
        let delta = |k: usize| (k as f32 * pitch, -starts[k]);
        let mut atoms = Vec::new();
        self.mc_atoms(b, top, &mut atoms);
        let first = atoms.first().map(|a| col(a.top)).unwrap_or(0);
        let single = atoms.iter().all(|a| col(a.top) == first);
        if single || self.mc_unbreakable(b) {
            let (dx, dy) = delta(first);
            let bx = &mut self.boxes[b];
            bx.x += dx - applied.0;
            bx.y += dy - applied.1;
            return;
        }
        if self.boxes[b].ifc.is_some() {
            // Lines move on their own; the block stays where it is.
            let base = top + self.boxes[b].content_y();
            let mut ifc = self.boxes[b].ifc.take().unwrap();
            let mut moved_boxes: Vec<(BoxId, f32, f32)> = Vec::new();
            for line in ifc.lines.iter_mut() {
                let (dx, dy) = delta(col(base + line.y));
                let (rx, ry) = (dx - applied.0, dy - applied.1);
                if rx == 0.0 && ry == 0.0 {
                    continue;
                }
                let (ly0, ly1) = (line.y, line.y + line.h);
                line.y += ry;
                for f in line.frags.iter_mut() {
                    f.x += rx;
                    f.y += ry;
                    if matches!(f.kind, inline::FragKind::Atomic) {
                        moved_boxes.push((f.box_id, rx, ry));
                    }
                }
                for d in ifc.decos.iter_mut() {
                    if d.y >= ly0 - 0.01 && d.y < ly1 - 0.01 {
                        d.x0 += rx;
                        d.x1 += rx;
                        d.y += ry;
                    }
                }
            }
            self.boxes[b].ifc = Some(ifc);
            for (c, rx, ry) in moved_boxes {
                self.boxes[c].x += rx;
                self.boxes[c].y += ry;
            }
            return;
        }
        let kids = self.boxes[b].children.clone();
        for c in kids {
            if self.boxes[c].out_of_flow {
                continue;
            }
            let ct = top + self.boxes[c].y;
            self.mc_place(c, ct, starts, pitch, applied);
        }
    }
}

/// Column starts when filling columns of height `h` in order.
fn breaks(atoms: &[Atom], h: f32) -> Vec<f32> {
    let mut starts = vec![0.0f32];
    let mut cur = 0.0f32;
    for a in atoms {
        if a.bottom - cur > h + 0.01 && a.top > cur + 0.01 {
            cur = a.top;
            starts.push(cur);
        }
    }
    starts
}

/// The smallest column height that fits the atoms in `n` columns.
fn balance(atoms: &[Atom], n: usize, total: f32) -> Vec<f32> {
    if n <= 1 || atoms.is_empty() {
        return vec![0.0];
    }
    let tallest = atoms.iter().map(|a| a.bottom - a.top).fold(0.0f32, f32::max);
    let (mut lo, mut hi) = ((total / n as f32).max(tallest), total.max(1.0));
    if breaks(atoms, lo).len() <= n {
        return breaks(atoms, lo);
    }
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if breaks(atoms, mid).len() <= n {
            hi = mid;
        } else {
            lo = mid;
        }
        if hi - lo < 0.25 {
            break;
        }
    }
    breaks(atoms, hi)
}
