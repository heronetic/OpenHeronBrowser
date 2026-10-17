// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Text selection over laid-out inline content (HeronLayout).
//!
//! A selection endpoint addresses a byte inside a text fragment of a line of an
//! inline formatting context; endpoints are ordered by a pre-order walk of the
//! box tree so selections can span blocks.

use super::inline::FragKind;
use super::{BoxId, LayoutTree};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelPoint {
    pub ifc: BoxId,
    pub line: usize,
    pub frag: usize,
    pub byte: usize,
}

/// A normalized selection (start ≤ end) with the document order it was computed for.
#[derive(Clone, Debug)]
pub struct SelSpan {
    pub order: std::rc::Rc<Vec<u32>>,
    pub start: SelPoint,
    pub end: SelPoint,
}

impl SelSpan {
    pub fn key(&self, p: &SelPoint) -> (u32, usize, usize, usize) {
        (self.order.get(p.ifc).copied().unwrap_or(u32::MAX), p.line, p.frag, p.byte)
    }
    /// Selected byte range of fragment (ifc, line, frag), if any (end is exclusive; usize::MAX = to end).
    pub fn frag_range(&self, ifc: BoxId, line: usize, frag: usize) -> Option<(usize, usize)> {
        let o = self.order.get(ifc).copied().unwrap_or(u32::MAX);
        let k = (o, line, frag);
        let s = self.key(&self.start);
        let e = self.key(&self.end);
        if k < (s.0, s.1, s.2) || k > (e.0, e.1, e.2) {
            return None;
        }
        let from = if k == (s.0, s.1, s.2) { s.3 } else { 0 };
        let to = if k == (e.0, e.1, e.2) { e.3 } else { usize::MAX };
        if from >= to {
            return None;
        }
        Some((from, to))
    }
}

impl LayoutTree {
    /// Pre-order index of every box (document order).
    pub fn preorder(&self) -> Vec<u32> {
        let mut order = vec![u32::MAX; self.boxes.len()];
        let mut n = 0u32;
        let mut stack = vec![self.root];
        while let Some(b) = stack.pop() {
            if b >= self.boxes.len() || order[b] != u32::MAX {
                continue;
            }
            order[b] = n;
            n += 1;
            for &c in self.boxes[b].children.iter().rev() {
                stack.push(c);
            }
        }
        for o in order.iter_mut() {
            if *o == u32::MAX {
                *o = n;
                n += 1;
            }
        }
        order
    }

    fn valid_point(&self, p: &SelPoint) -> bool {
        self.boxes.get(p.ifc).and_then(|b| b.ifc.as_ref()).and_then(|i| i.lines.get(p.line)).map(|l| p.frag < l.frags.len()).unwrap_or(false)
    }

    pub fn span(&self, a: SelPoint, b: SelPoint) -> Option<SelSpan> {
        if !self.valid_point(&a) || !self.valid_point(&b) {
            return None;
        }
        let order = std::rc::Rc::new(self.preorder());
        let mut sp = SelSpan { order, start: a, end: b };
        if sp.key(&b) < sp.key(&a) {
            sp.start = b;
            sp.end = a;
        }
        Some(sp)
    }

    /// Find the text position at document point (x, y). With `strict`, the point
    /// must lie on a line; otherwise the nearest line of a horizontally
    /// overlapping inline context is used (for drag-selection).
    pub fn text_hit(&self, x: f32, y: f32, strict: bool) -> Option<SelPoint> {
        let mut best: Option<(f32, f32, SelPoint)> = None; // (vertical distance, width, point)
        for (id, b) in self.boxes.iter().enumerate() {
            let ifc = match &b.ifc {
                Some(i) => i,
                None => continue,
            };
            if b.fixed {
                continue;
            }
            let ox = b.abs_x + b.content_x();
            let oy = b.abs_y + b.content_y();
            let cw = b.content_w().max(ifc.max_line_w);
            if x < ox - 1.0 || x > ox + cw + 1.0 {
                if strict {
                    continue;
                }
                // Allow drags that leave the box sideways only when vertically inside it.
                if y < oy || y > oy + ifc.height {
                    continue;
                }
            }
            for (li, line) in ifc.lines.iter().enumerate() {
                let ly0 = oy + line.y;
                let ly1 = ly0 + line.h;
                let dist = if y < ly0 { ly0 - y } else if y >= ly1 { y - ly1 } else { 0.0 };
                if strict && dist > 0.0 {
                    continue;
                }
                // Pick the text fragment under / nearest to x.
                let mut pick: Option<(f32, usize, usize)> = None; // (dx, frag, byte)
                for (fi, f) in line.frags.iter().enumerate() {
                    if !matches!(f.kind, FragKind::Text { .. }) {
                        continue;
                    }
                    let fx0 = ox + f.x;
                    let fx1 = fx0 + f.w;
                    let (dx, byte) = if x < fx0 {
                        (fx0 - x, 0)
                    } else if x > fx1 {
                        (x - fx1, f.byte_at_x(f.w + 1.0))
                    } else {
                        (0.0, f.byte_at_x(x - fx0))
                    };
                    if pick.map(|p| dx < p.0).unwrap_or(true) {
                        pick = Some((dx, fi, byte));
                    }
                }
                let (dx, fi, byte) = match pick {
                    Some(p) => p,
                    None => continue,
                };
                if strict && dx > 0.0 {
                    // On the line but beside the text: still a hit (selection starts at the edge).
                }
                let p = SelPoint { ifc: id, line: li, frag: fi, byte };
                let w = b.content_w();
                let better = match &best {
                    None => true,
                    Some((bd, bw, _)) => dist < *bd - 0.01 || ((dist - *bd).abs() <= 0.01 && w < *bw),
                };
                if better {
                    best = Some((dist, w, p));
                }
            }
        }
        best.map(|b| b.2)
    }

    fn frag_text(&self, ifc: BoxId, line: usize, frag: usize) -> Option<&str> {
        let f = self.boxes.get(ifc)?.ifc.as_ref()?.lines.get(line)?.frags.get(frag)?;
        match &f.kind {
            FragKind::Text { text, .. } => Some(text),
            _ => None,
        }
    }

    /// Word boundaries around a point (double-click selection).
    pub fn word_at(&self, p: SelPoint) -> (SelPoint, SelPoint) {
        let text = match self.frag_text(p.ifc, p.line, p.frag) {
            Some(t) => t,
            None => return (p, p),
        };
        let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '\'' || c == '-';
        let b = p.byte.min(text.len());
        let mut s = b;
        while s > 0 {
            let prev = text[..s].chars().next_back().unwrap();
            if !is_word(prev) {
                break;
            }
            s -= prev.len_utf8();
        }
        let mut e = b;
        while e < text.len() {
            let c = text[e..].chars().next().unwrap();
            if !is_word(c) {
                break;
            }
            e += c.len_utf8();
        }
        if s == e && e < text.len() {
            e += text[e..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        }
        (SelPoint { byte: s, ..p }, SelPoint { byte: e, ..p })
    }

    /// First and last text positions of the document (select all).
    pub fn text_extent(&self) -> Option<(SelPoint, SelPoint)> {
        let order = self.preorder();
        let mut first: Option<(u32, SelPoint)> = None;
        let mut last: Option<(u32, SelPoint)> = None;
        for (id, b) in self.boxes.iter().enumerate() {
            let ifc = match &b.ifc {
                Some(i) => i,
                None => continue,
            };
            for (li, line) in ifc.lines.iter().enumerate() {
                for (fi, f) in line.frags.iter().enumerate() {
                    if let FragKind::Text { text, .. } = &f.kind {
                        let o = order[id];
                        let a = SelPoint { ifc: id, line: li, frag: fi, byte: 0 };
                        let z = SelPoint { ifc: id, line: li, frag: fi, byte: text.len() };
                        if first.map(|(fo, fp)| (o, li, fi) < (fo, fp.line, fp.frag)).unwrap_or(true) {
                            first = Some((o, a));
                        }
                        if last.map(|(lo, lp)| (o, li, fi) > (lo, lp.line, lp.frag)).unwrap_or(true) {
                            last = Some((o, z));
                        }
                    }
                }
            }
        }
        Some((first?.1, last?.1))
    }

    /// Plain text of a selection: fragments on a line are concatenated, soft line
    /// breaks become spaces and separate inline contexts (blocks) become newlines.
    pub fn selected_text(&self, sp: &SelSpan) -> String {
        let mut ifcs: Vec<BoxId> = self.boxes.iter().enumerate().filter(|(_, b)| b.ifc.is_some()).map(|(i, _)| i).collect();
        ifcs.sort_by_key(|&i| sp.order.get(i).copied().unwrap_or(u32::MAX));
        let mut out = String::new();
        for id in ifcs {
            let ifc = self.boxes[id].ifc.as_ref().unwrap();
            let mut block = String::new();
            for (li, line) in ifc.lines.iter().enumerate() {
                let mut line_txt = String::new();
                for (fi, f) in line.frags.iter().enumerate() {
                    if let FragKind::Text { text, .. } = &f.kind {
                        if let Some((a, b)) = sp.frag_range(id, li, fi) {
                            let a = a.min(text.len());
                            let b = b.min(text.len());
                            if a < b && text.is_char_boundary(a) && text.is_char_boundary(b) {
                                line_txt.push_str(&text[a..b]);
                            }
                        }
                    }
                }
                if !line_txt.is_empty() {
                    if !block.is_empty() && !block.ends_with(char::is_whitespace) {
                        block.push(' ');
                    }
                    block.push_str(&line_txt);
                }
            }
            let block = block.trim_end().to_string();
            if !block.is_empty() {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&block);
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Layout positions <-> DOM boundary points (for the DOM Selection, carets in
// editing hosts and mouse selection).

fn css_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

/// For each char boundary of `laid` (a text node's laid-out text: collapsed,
/// maybe transformed, split over lines), the UTF-16 offset in `data`.
fn align(data: &str, laid: &str) -> Vec<(usize, usize)> {
    let d: Vec<(usize, char)> = {
        let mut v = Vec::with_capacity(data.len());
        let mut u = 0usize;
        for c in data.chars() {
            v.push((u, c));
            u += c.len_utf16();
        }
        v.push((u, '\0'));
        v
    };
    let end = d.len() - 1;
    let mut out = Vec::with_capacity(laid.len() + 1);
    let mut i = 0usize;
    for (bi, ch) in laid.char_indices() {
        if css_ws(ch) {
            out.push((bi, d[i.min(end)].0));
            if i < end && css_ws(d[i].1) {
                i += 1;
            }
        } else {
            while i < end && css_ws(d[i].1) {
                i += 1;
            }
            out.push((bi, d[i.min(end)].0));
            if i < end {
                i += 1;
            }
        }
    }
    out.push((laid.len(), d[i.min(end)].0));
    out
}

impl LayoutTree {
    /// The text frags of text box `tb` in inline context `ifc` (line, frag,
    /// start byte in their concatenation) and the concatenated text.
    fn box_frags(&self, ifc: BoxId, tb: BoxId) -> (Vec<(usize, usize, usize)>, String) {
        let mut frags = Vec::new();
        let mut text = String::new();
        if let Some(i) = self.boxes.get(ifc).and_then(|b| b.ifc.as_ref()) {
            for (li, line) in i.lines.iter().enumerate() {
                for (fi, f) in line.frags.iter().enumerate() {
                    if f.box_id != tb {
                        continue;
                    }
                    if let FragKind::Text { text: t, .. } = &f.kind {
                        frags.push((li, fi, text.len()));
                        text.push_str(t);
                    }
                }
            }
        }
        (frags, text)
    }

    /// The DOM boundary point (Text node, UTF-16 offset) of a layout position.
    pub fn point_to_dom(&self, doc: &crate::dom::Document, p: SelPoint) -> Option<(crate::dom::NodeId, usize)> {
        let f = self.boxes.get(p.ifc)?.ifc.as_ref()?.lines.get(p.line)?.frags.get(p.frag)?;
        let tb = f.box_id;
        let node = self.boxes.get(tb)?.node?;
        let data = match &doc.nodes.get(node)?.data {
            crate::dom::NodeData::Text(t) => t.as_str(),
            _ => return None,
        };
        let (frags, laid) = self.box_frags(p.ifc, tb);
        let start = frags.iter().find(|(l, fi, _)| *l == p.line && *fi == p.frag)?.2;
        let flen = match &f.kind {
            FragKind::Text { text, .. } => text.len(),
            _ => 0,
        };
        let b = start + p.byte.min(flen);
        let map = align(data, &laid);
        let off = map.iter().take_while(|(bi, _)| *bi <= b).last().map(|x| x.1).unwrap_or(0);
        Some((node, off))
    }

    /// The layout position of a DOM boundary point in a Text node (None when
    /// the node has no laid-out text).
    pub fn dom_text_to_point(&self, doc: &crate::dom::Document, node: crate::dom::NodeId, off: usize) -> Option<SelPoint> {
        let data = match &doc.nodes.get(node)?.data {
            crate::dom::NodeData::Text(t) => t.as_str(),
            _ => return None,
        };
        for tb in self.boxes_of(node) {
            let b = &self.boxes[tb];
            // The inline formatting context holding this text box.
            let mut ifc = b.parent;
            while let Some(i) = ifc {
                if self.boxes[i].ifc.is_some() {
                    break;
                }
                ifc = self.boxes[i].parent;
            }
            let Some(ifc) = ifc else { continue };
            let (frags, laid) = self.box_frags(ifc, tb);
            if frags.is_empty() {
                continue;
            }
            let map = align(data, &laid);
            // The last laid-out position at or before the offset.
            let b = map.iter().take_while(|(_, u)| *u <= off).last().map(|x| x.0).unwrap_or(0);
            let (li, fi, st) = frags.iter().rev().find(|(_, _, st)| *st <= b).copied().unwrap_or(frags[0]);
            return Some(SelPoint { ifc, line: li, frag: fi, byte: b - st });
        }
        None
    }

    /// The layout position of any DOM boundary point: a Text node directly,
    /// an element position through the nearest text at or around it.
    pub fn dom_to_point(&self, doc: &crate::dom::Document, node: crate::dom::NodeId, off: usize) -> Option<SelPoint> {
        if let Some(crate::dom::NodeData::Text(_)) = doc.nodes.get(node).map(|n| &n.data) {
            return self.dom_text_to_point(doc, node, off);
        }
        let kids = doc.children(node);
        // The first text at or after the point inside `node`, else the last before it.
        let first_text = |n: crate::dom::NodeId| -> Option<crate::dom::NodeId> {
            if matches!(doc.nodes[n].data, crate::dom::NodeData::Text(_)) {
                return Some(n);
            }
            doc.descendants(n).find(|&d| matches!(doc.nodes[d].data, crate::dom::NodeData::Text(_)) && self.dom_text_to_point(doc, d, 0).is_some())
        };
        for &k in kids.iter().skip(off) {
            if let Some(t) = first_text(k) {
                if let Some(p) = self.dom_text_to_point(doc, t, 0) {
                    return Some(p);
                }
            }
        }
        for &k in kids.iter().take(off).rev() {
            let last = if matches!(doc.nodes[k].data, crate::dom::NodeData::Text(_)) { Some(k) } else { doc.descendants(k).filter(|&d| matches!(doc.nodes[d].data, crate::dom::NodeData::Text(_))).last() };
            if let Some(t) = last {
                let len = match &doc.nodes[t].data {
                    crate::dom::NodeData::Text(s) => s.encode_utf16().count(),
                    _ => 0,
                };
                if let Some(p) = self.dom_text_to_point(doc, t, len) {
                    return Some(p);
                }
            }
        }
        None
    }

    /// Client rectangles (document coordinates: x, y, w, h) of the laid-out
    /// text of Text node `node` between UTF-16 offsets `s..e`, one per line
    /// fragment it covers. A collapsed range gives a zero-width rectangle at
    /// the position (two at a line wrap: line end and next line start).
    pub fn text_range_rects(&self, doc: &crate::dom::Document, node: crate::dom::NodeId, s: usize, e: usize) -> Vec<(f32, f32, f32, f32)> {
        let mut out = Vec::new();
        let data = match doc.nodes.get(node).map(|n| &n.data) {
            Some(crate::dom::NodeData::Text(t)) => t.as_str(),
            _ => return out,
        };
        for tb in self.boxes_of(node) {
            let b = &self.boxes[tb];
            let mut ifc = b.parent;
            while let Some(i) = ifc {
                if self.boxes[i].ifc.is_some() {
                    break;
                }
                ifc = self.boxes[i].parent;
            }
            let Some(ifc) = ifc else { continue };
            let (frags, laid) = self.box_frags(ifc, tb);
            if frags.is_empty() {
                continue;
            }
            let map = align(data, &laid);
            // An offset at collapsed (unlaid) text maps to the next laid
            // character: the end of the previous one.
            let to_byte = |u: usize| map.iter().find(|(_, x)| *x >= u).or(map.last()).map(|x| x.0).unwrap_or(0);
            let (bs, be) = (to_byte(s), to_byte(e.max(s)));
            let bx = &self.boxes[ifc];
            let Some(info) = bx.ifc.as_ref() else { continue };
            let (ox, oy) = (bx.abs_x + bx.content_x(), bx.abs_y + bx.content_y());
            for &(li, fi, st) in &frags {
                let Some(line) = info.lines.get(li) else { continue };
                let Some(f) = line.frags.get(fi) else { continue };
                let flen = match &f.kind {
                    FragKind::Text { text, .. } => text.len(),
                    _ => 0,
                };
                let (fs, fe) = (st, st + flen);
                let hit = if bs == be { bs >= fs && bs <= fe } else { bs < fe && be > fs };
                if !hit {
                    continue;
                }
                let a = bs.max(fs) - fs;
                let z = be.min(fe) - fs;
                let x0 = ox + f.x + f.x_of_byte(a);
                let x1 = ox + f.x + f.x_of_byte(z);
                // The font's content box around the baseline (no leading).
                let baseline = oy + f.y + f.ascent;
                let (asc, desc) = match &f.kind {
                    FragKind::Text { pieces, .. } => pieces.first().and_then(|(sh, _)| sh.runs.first()).map(|r| r.font.ascent_descent(r.size)).unwrap_or((f.ascent, f.h - f.ascent)),
                    _ => (f.ascent, f.h - f.ascent),
                };
                let _ = line;
                out.push((x0.min(x1), baseline - asc, (x1 - x0).abs(), asc + desc));
            }
            if !out.is_empty() {
                break;
            }
        }
        out
    }

    /// Boxes generated by `node`: its registered first box (and any later
    /// ones), else a full scan.
    fn boxes_of(&self, node: crate::dom::NodeId) -> Vec<BoxId> {
        if let Some(&b) = self.node_box.get(&node) {
            let mut v = vec![b];
            // A text split into several boxes: later siblings with the same node.
            let mut i = b + 1;
            while i < self.boxes.len() && i < b + 64 {
                if self.boxes[i].node == Some(node) {
                    v.push(i);
                }
                i += 1;
            }
            return v;
        }
        self.boxes.iter().enumerate().filter(|(_, b)| b.node == Some(node)).map(|(i, _)| i).collect()
    }

    /// Caret geometry of a layout position: (x, y, height) in document coordinates.
    pub fn caret_rect(&self, p: SelPoint) -> Option<(f32, f32, f32)> {
        let b = self.boxes.get(p.ifc)?;
        let ifc = b.ifc.as_ref()?;
        let line = ifc.lines.get(p.line)?;
        let f = line.frags.get(p.frag)?;
        let x = b.abs_x + b.content_x() + f.x + f.x_of_byte(p.byte);
        let y = b.abs_y + b.content_y() + line.y;
        Some((x, y, line.h))
    }
}

#[cfg(test)]
mod align_tests {
    use super::align;

    #[test]
    fn collapsed_whitespace() {
        // "  a  b " laid out as "a b": a at 2, space at 3, b at 5, end at 7.
        let m = align("  a  b ", "a b");
        assert_eq!(m, vec![(0, 2), (1, 3), (2, 5), (3, 6)]);
        let m = align("x\u{e9}y", "x\u{e9}y");
        assert_eq!(m.last().copied(), Some((4, 3)));
        // Preserved spaces map one to one.
        let m = align("a  b", "a  b");
        assert_eq!(m, vec![(0, 0), (1, 1), (2, 2), (3, 3), (4, 4)]);
    }
}
