// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Inline formatting context: whitespace processing, line breaking,
//! vertical alignment and text alignment.

use super::block::FloatCtx;
use super::*;
use crate::text::ShapedText;

#[derive(Clone)]
pub enum FragKind {
    Text {
        pieces: Vec<(Rc<ShapedText>, f32)>,
        text: Rc<str>,
        /// Byte offset in `text` where each piece starts.
        starts: Vec<u32>,
    },
    Atomic,
    Ellipsis(Rc<ShapedText>),
}

#[derive(Clone)]
pub struct Frag {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub ascent: f32,
    pub box_id: BoxId,
    pub kind: FragKind,
}

pub struct Line {
    pub y: f32,
    pub h: f32,
    pub baseline: f32,
    pub frags: Vec<Frag>,
    pub width: f32,
}

#[derive(Clone, Debug)]
pub struct InlineDeco {
    pub box_id: BoxId,
    pub x0: f32,
    pub x1: f32,
    pub y: f32,
    pub h: f32,
    pub first: bool,
    pub last: bool,
}

impl Frag {
    /// x offset (from the fragment start) of byte `b` of a text fragment.
    pub fn x_of_byte(&self, b: usize) -> f32 {
        if let FragKind::Text { pieces, starts, text } = &self.kind {
            if b >= text.len() {
                return self.w;
            }
            let mut best = None;
            for (i, st) in starts.iter().enumerate() {
                if (*st as usize) <= b {
                    best = Some(i);
                }
            }
            match best {
                Some(i) => {
                    let (sh, off) = &pieces[i];
                    let local = b - starts[i] as usize;
                    let end = starts.get(i + 1).map(|v| *v as usize).unwrap_or(text.len());
                    if b >= end {
                        off + sh.width
                    } else {
                        off + sh.x_of_byte(local)
                    }
                }
                None => 0.0,
            }
        } else {
            0.0
        }
    }

    /// Byte index of a text fragment nearest to x (relative to the fragment start).
    pub fn byte_at_x(&self, x: f32) -> usize {
        if let FragKind::Text { pieces, starts, text } = &self.kind {
            if x <= 0.0 {
                return 0;
            }
            if x >= self.w {
                return text.len();
            }
            for (i, (sh, off)) in pieces.iter().enumerate() {
                let st = starts[i] as usize;
                let end = starts.get(i + 1).map(|v| *v as usize).unwrap_or(text.len());
                if x < *off {
                    return st;
                }
                if x < off + sh.width {
                    let seg = &text[st.min(text.len())..end.min(text.len()).max(st.min(text.len()))];
                    return st + sh.byte_at_x(x - off, seg).min(seg.len());
                }
            }
            text.len()
        } else {
            0
        }
    }
}

/// Truncate an inline layout to `n` lines, ending the last one with an ellipsis
/// (`-webkit-line-clamp`).
pub fn clamp_lines(l: &mut Layout, id: BoxId, ifc: &mut InlineLayout, n: usize, content_w: f32) {
    if n == 0 || ifc.lines.len() <= n {
        return;
    }
    ifc.lines.truncate(n);
    let last_y = ifc.lines[n - 1].y;
    let last_h = ifc.lines[n - 1].h;
    ifc.height = last_y + last_h;
    ifc.decos.retain(|d| d.y < last_y + last_h - 0.01);
    let bs = l.boxes[id].style.clone();
    let ell = l.fonts.shape("…", &bs.font_family, bs.font_weight, bs.font_style, bs.font_size, 0.0);
    let limit = (content_w - ell.width).max(0.0);
    let line = &mut ifc.lines[n - 1];
    // Drop or cut fragments that would collide with the ellipsis.
    let mut cut_at: Option<usize> = None;
    for (i, f) in line.frags.iter().enumerate() {
        if f.x + f.w > limit {
            cut_at = Some(i);
            break;
        }
    }
    let mut end_x = line.frags.iter().filter(|f| matches!(f.kind, FragKind::Text { .. } | FragKind::Atomic)).map(|f| f.x + f.w).fold(0.0f32, f32::max);
    if let Some(i) = cut_at {
        let f = line.frags[i].clone_shallow();
        line.frags.truncate(i);
        end_x = line.frags.iter().map(|f| f.x + f.w).fold(0.0f32, f32::max);
        if let (FragKind::Text { text, .. }, true) = (&f.kind, f.x < limit) {
            let b = f.byte_at_x(limit - f.x);
            let mut b = b.min(text.len());
            while b > 0 && !text.is_char_boundary(b) {
                b -= 1;
            }
            let head = text[..b].trim_end().to_string();
            if !head.is_empty() {
                let s = l.boxes[f.box_id].style.clone();
                let sh = l.fonts.shape_s(&head, &s);
                let w = sh.width;
                line.frags.push(Frag { x: f.x, y: f.y, w, h: f.h, ascent: f.ascent, box_id: f.box_id, kind: FragKind::Text { pieces: vec![(sh, 0.0)], text: Rc::from(head.as_str()), starts: vec![0] } });
                end_x = f.x + w;
            }
        }
    }
    // Ellipsis on the root's baseline.
    let base_frag = line.frags.iter().find(|f| matches!(f.kind, FragKind::Text { .. }));
    let (fy, fa, fh) = base_frag.map(|f| (f.y, f.ascent, f.h)).unwrap_or((line.y, line.baseline, line.h));
    line.frags.push(Frag { x: end_x, y: fy, w: ell.width, h: fh, ascent: fa, box_id: id, kind: FragKind::Ellipsis(ell) });
}

impl Frag {
    fn clone_shallow(&self) -> Frag {
        Frag {
            x: self.x,
            y: self.y,
            w: self.w,
            h: self.h,
            ascent: self.ascent,
            box_id: self.box_id,
            kind: match &self.kind {
                FragKind::Text { pieces, text, starts } => FragKind::Text { pieces: pieces.clone(), text: text.clone(), starts: starts.clone() },
                FragKind::Atomic => FragKind::Atomic,
                FragKind::Ellipsis(s) => FragKind::Ellipsis(s.clone()),
            },
        }
    }
}

pub struct InlineLayout {
    pub lines: Vec<Line>,
    pub decos: Vec<InlineDeco>,
    pub height: f32,
    pub max_line_w: f32,
}

#[derive(Clone)]
enum Item {
    Text { box_id: BoxId, text: Rc<str> },
    Atomic { box_id: BoxId },
    Open { box_id: BoxId },
    Close { box_id: BoxId },
    /// A forced break (the `<br>`'s box, for its `clear`).
    Break(Option<BoxId>),
    Float { box_id: BoxId },
    Abs { box_id: BoxId },
}

pub(crate) fn transform_text(t: &str, tt: TextTransform, prev_char: Option<char>) -> String {
    match tt {
        TextTransform::None => t.to_string(),
        TextTransform::Uppercase => t.to_uppercase(),
        TextTransform::Lowercase => t.to_lowercase(),
        TextTransform::MathAuto => {
            // Only a lone letter is italicised (MathML Core §6.1).
            let trimmed = t.trim();
            let mut it = trimmed.chars();
            match (it.next(), it.next()) {
                (Some(c), None) => t.replacen(c, &math_italic(c).to_string(), 1),
                _ => t.to_string(),
            }
        }
        TextTransform::Capitalize => {
            let mut out = String::with_capacity(t.len());
            let mut at_start = prev_char.map(|c| c.is_whitespace()).unwrap_or(true);
            for c in t.chars() {
                if at_start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    at_start = false;
                } else {
                    if c.is_whitespace() {
                        at_start = true;
                    }
                    out.push(c);
                }
            }
            out
        }
    }
}

/// The Mathematical Alphanumeric Symbols italic form of a letter.
fn math_italic(c: char) -> char {
    let u = c as u32;
    let m = match c {
        'h' => 0x210E,
        'A'..='Z' => 0x1D434 + (u - 'A' as u32),
        'a'..='z' => 0x1D44E + (u - 'a' as u32),
        '\u{131}' => 0x1D6A4,
        '\u{237}' => 0x1D6A5,
        '\u{391}'..='\u{3A9}' => 0x1D6E2 + (u - 0x391),
        '\u{3B1}'..='\u{3C9}' => 0x1D6FC + (u - 0x3B1),
        _ => return c,
    };
    char::from_u32(m).unwrap_or(c)
}

/// Collect inline items for the inline-level children of `id`.
fn collect(l: &Layout, id: BoxId, items: &mut Vec<Item>, prev_space: &mut bool, include_self: bool) {
    let b = &l.boxes[id];
    if include_self {
        visit(l, id, items, prev_space);
        return;
    }
    for &c in &b.children {
        visit(l, c, items, prev_space);
    }
}

fn visit(l: &Layout, c: BoxId, items: &mut Vec<Item>, prev_space: &mut bool) {
    let cb = &l.boxes[c];
    if cb.out_of_flow {
        items.push(Item::Abs { box_id: c });
        return;
    }
    if cb.is_float {
        items.push(Item::Float { box_id: c });
        return;
    }
    match &cb.kind {
        BoxKind::Text(t) | BoxKind::Generated(t) => {
            let s = &cb.style;
            if s.visibility == Visibility::Collapse {
                return;
            }
            let processed = process_ws(t, s.white_space, prev_space);
            let processed = if s.text_transform != TextTransform::None { transform_text(&processed, s.text_transform, None) } else { processed };
            // unicode-bidi: bidi-override with direction: rtl lays characters out right to left.
            let processed = if s.bidi_override && s.direction_rtl { processed.chars().rev().collect::<String>() } else { processed };
            if s.white_space.preserves_newlines() && processed.contains('\n') {
                let parts: Vec<&str> = processed.split('\n').collect();
                for (i, p) in parts.iter().enumerate() {
                    if !p.is_empty() {
                        items.push(Item::Text { box_id: c, text: Rc::from(*p) });
                    }
                    if i + 1 < parts.len() {
                        items.push(Item::Break(None));
                    }
                }
            } else if !processed.is_empty() {
                items.push(Item::Text { box_id: c, text: Rc::from(processed.as_str()) });
            }
        }
        BoxKind::InlineBox => {
            items.push(Item::Open { box_id: c });
            // Inline list items (display: list-item inside) marker.
            for &k in &cb.children {
                visit(l, k, items, prev_space);
            }
            items.push(Item::Close { box_id: c });
        }
        BoxKind::LineBreak => {
            items.push(Item::Break(Some(c)));
            *prev_space = true;
        }
        _ => {
            items.push(Item::Atomic { box_id: c });
            *prev_space = false;
        }
    }
}

fn process_ws(t: &str, ws: WhiteSpace, prev_space: &mut bool) -> String {
    let mut out = String::with_capacity(t.len());
    match ws {
        WhiteSpace::Normal | WhiteSpace::NoWrap => {
            for c in t.chars() {
                if matches!(c, ' ' | '\n' | '\t' | '\r' | '\x0c') {
                    if !*prev_space {
                        out.push(' ');
                        *prev_space = true;
                    }
                } else {
                    out.push(c);
                    *prev_space = false;
                }
            }
        }
        WhiteSpace::PreLine => {
            let mut pending_space = false;
            for c in t.chars() {
                match c {
                    '\n' => {
                        // spaces before a newline are removed
                        pending_space = false;
                        out.push('\n');
                        *prev_space = true;
                    }
                    ' ' | '\t' | '\r' | '\x0c' => {
                        if !*prev_space {
                            pending_space = true;
                        }
                    }
                    _ => {
                        if pending_space {
                            out.push(' ');
                            pending_space = false;
                        }
                        out.push(c);
                        *prev_space = false;
                    }
                }
            }
            if pending_space {
                out.push(' ');
                *prev_space = true;
            }
        }
        _ => {
            for c in t.chars() {
                match c {
                    // Tabs stay: they advance to the next tab stop at layout time.
                    '\r' => {}
                    _ => out.push(c),
                }
            }
            *prev_space = out.ends_with(' ') || out.ends_with('\n');
        }
    }
    out
}

#[derive(Clone)]
struct Piece {
    item: usize,
    w: f32,
    space: bool,
    /// May the line break after this piece?
    brk: bool,
    shaped: Option<Rc<ShapedText>>,
    text: Option<Rc<str>>,
}

fn is_cjk(c: char) -> bool {
    let u = c as u32;
    (0x2E80..=0x9FFF).contains(&u) || (0xAC00..=0xD7AF).contains(&u) || (0xF900..=0xFAFF).contains(&u) || (0xFF00..=0xFFEF).contains(&u) || (0x20000..=0x2FA1F).contains(&u)
}

/// Synthesized small caps (as Blink): lowercase letters become capitals at
/// 70% of the size, other characters keep theirs.
fn shape_small_caps(l: &mut Layout, s: &ComputedStyle, seg: &str) -> Rc<ShapedText> {
    let mut out = ShapedText { runs: Vec::new(), width: 0.0 };
    let mut groups: Vec<(usize, usize, bool)> = Vec::new();
    for (i, c) in seg.char_indices() {
        let lower = c.is_lowercase();
        match groups.last_mut() {
            Some(g) if g.2 == lower => g.1 = i + c.len_utf8(),
            _ => groups.push((i, i + c.len_utf8(), lower)),
        }
    }
    for (a, b, lower) in groups {
        let (t, size) = if lower { (seg[a..b].to_uppercase(), s.font_size * 0.7) } else { (seg[a..b].to_string(), s.font_size) };
        let sh = l.fonts.shape(&t, &s.font_family, s.font_weight, s.font_style, size, s.letter_spacing);
        for r in &sh.runs {
            let glyphs = r.glyphs.iter().map(|g| crate::text::ShapedGlyph { cluster: g.cluster + a as u32, ..g.clone() }).collect();
            out.runs.push(crate::text::ShapedRun { font: r.font.clone(), size: r.size, glyphs, width: r.width });
        }
        out.width += sh.width;
    }
    Rc::new(out)
}

/// Split text into words / spaces / break opportunities.
fn split_words(t: &str, can_wrap: bool) -> Vec<(&str, bool, bool)> {
    // (segment, is_space, break_after)
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = t.as_bytes();
    let mut chars = t.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == ' ' || c == '\u{200b}' || c == '\t' {
            if i > start {
                out.push((&t[start..i], false, false));
            }
            let end = i + c.len_utf8();
            out.push((&t[i..end], c != '\u{200b}', can_wrap));
            start = end;
        } else if c == '\u{00ad}' && can_wrap {
            // Soft hyphen: a break opportunity that shows a hyphen when taken.
            let end = i + c.len_utf8();
            out.push((&t[start..end], false, true));
            start = end;
        } else if can_wrap && (is_cjk(c) || ((c == '-' || c == '—' || c == '–') && i > start) || ((c == '—' || c == '–') && i == start)) {
            let end = i + c.len_utf8();
            // keep hyphen attached to the preceding word
            let next_is_cjk = chars.peek().map(|(_, n)| is_cjk(*n)).unwrap_or(false);
            let next_is_digit = chars.peek().map(|(_, n)| n.is_ascii_digit()).unwrap_or(false);
            if is_cjk(c) && i > start {
                out.push((&t[start..i], false, true));
                start = i;
            }
            // Dashes allow a break after them (UAX #14 BA/B2), also where the
            // next word is in another inline element; not "-" before a digit.
            let dash_at_end = end >= bytes.len() && c != '-';
            if is_cjk(c) || (end < bytes.len() && !next_is_cjk && !(c == '-' && next_is_digit)) || dash_at_end {
                out.push((&t[start..end], false, true));
                start = end;
            }
        }
    }
    if start < t.len() {
        out.push((&t[start..], false, false));
    }
    // Split words that switch between right-to-left and left-to-right letters
    // (no break opportunity between the parts) so each is shaped in its own direction.
    let mut split = Vec::with_capacity(out.len());
    for (seg, sp, brk) in out {
        if sp || !has_rtl(seg) || !seg.chars().any(|c| c.is_alphabetic() && !is_rtl_char(c)) {
            split.push((seg, sp, brk));
            continue;
        }
        let mut s0 = 0;
        let mut cur: Option<bool> = None;
        for (i, c) in seg.char_indices() {
            let d = if is_rtl_char(c) { Some(true) } else if c.is_alphabetic() { Some(false) } else { None };
            if let (Some(dn), Some(dc)) = (d, cur) {
                if dn != dc && i > s0 {
                    split.push((&seg[s0..i], false, false));
                    s0 = i;
                }
            }
            if d.is_some() {
                cur = d;
            }
        }
        split.push((&seg[s0..], false, brk));
    }
    split
}

struct Builder<'l, 'a> {
    l: &'l mut Layout<'a>,
    items: Vec<Item>,
    pieces: Vec<Piece>,
    /// Definite content height of the block container (for percentage heights of atomics).
    cb_h: Option<f32>,
}

impl<'l, 'a> Builder<'l, 'a> {
    fn style(&self, b: BoxId) -> Rc<ComputedStyle> {
        self.l.boxes[b].style.clone()
    }

    fn make_pieces(&mut self, content_w: f32, measure_only: bool) {
        let items = self.items.clone();
        // Running x since the last forced break, for tab stops.
        let mut line_x = 0.0f32;
        let mut counted = 0usize;
        for (idx, it) in items.iter().enumerate() {
            for p in &self.pieces[counted..] {
                line_x += p.w;
            }
            counted = self.pieces.len();
            if matches!(it, Item::Break(_)) {
                line_x = 0.0;
            }
            match it {
                Item::Text { box_id, text } => {
                    let s = self.style(*box_id);
                    let can_wrap = s.white_space.wraps();
                    let space_w = {
                        let font = self.l.fonts.primary(&s.font_family, s.font_weight, s.font_style);
                        self.l.fonts.space_width(&font, s.font_size) + s.word_spacing + s.letter_spacing
                    };
                    let mut x = line_x;
                    let segs = split_words(text, can_wrap);
                    for (si, &(seg, is_space, brk)) in segs.iter().enumerate() {
                        if seg == "\t" {
                            // Advance to the next multiple of tab-size spaces (CSS Text §4.2).
                            let stop = (s.tab_size.max(0.0) * space_w).max(0.0);
                            let w = if stop > 0.0 {
                                let mut w = stop - (x % stop);
                                if w < space_w * 0.5 {
                                    w += stop;
                                }
                                w
                            } else {
                                0.0
                            };
                            x += w;
                            self.pieces.push(Piece { item: idx, w, space: true, brk, shaped: None, text: None });
                        } else if is_space {
                            // Shaped in context, a space takes the kerning with its
                            // neighbors (as when the whole run is shaped at once).
                            let prev = si.checked_sub(1).and_then(|i| segs.get(i)).filter(|g| !g.1).and_then(|g| g.0.chars().last());
                            let next = segs.get(si + 1).filter(|g| !g.1).and_then(|g| g.0.chars().next());
                            let w = if seg == " " && !s.font_variant_small_caps && (prev.is_some() || next.is_some()) {
                                let fam = s.font_family.clone();
                                let sh = |l: &mut Layout, t: &str| l.fonts.shape(t, &fam, s.font_weight, s.font_style, s.font_size, s.letter_spacing).width;
                                let (p, n) = (prev.map(String::from).unwrap_or_default(), next.map(String::from).unwrap_or_default());
                                let whole = sh(self.l, &format!("{p} {n}"));
                                let parts = if p.is_empty() { 0.0 } else { sh(self.l, &p) } + if n.is_empty() { 0.0 } else { sh(self.l, &n) };
                                let kerned = whole - parts + s.word_spacing;
                                if (kerned - space_w).abs() < s.font_size * 0.25 { kerned } else { space_w }
                            } else {
                                space_w
                            };
                            x += w;
                            self.pieces.push(Piece { item: idx, w, space: true, brk, shaped: None, text: None });
                        } else {
                            let shaped = if s.font_variant_small_caps && seg.chars().any(|c| c.is_lowercase()) {
                                shape_small_caps(self.l, &s, seg)
                            } else {
                                self.l.fonts.shape_s(seg, &s)
                            };
                            let w = shaped.width;
                            x += w;
                            self.pieces.push(Piece { item: idx, w, space: false, brk, shaped: Some(shaped), text: Some(Rc::from(seg)) });
                        }
                    }
                }
                Item::Atomic { box_id } => {
                    let w = if measure_only {
                        0.0
                    } else {
                        let cb_h = self.cb_h;
                        self.l.layout_box(*box_id, content_w, cb_h, None, (0.0, 0.0));
                        self.l.boxes[*box_id].outer_w()
                    };
                    // Soft wrap opportunities around atomic inlines follow the
                    // white-space of the content they sit in (the parent's, not
                    // the atomic's own): none inside nowrap/pre.
                    let wraps = atomic_wraps(self.l, *box_id);
                    self.pieces.push(Piece { item: idx, w, space: false, brk: wraps, shaped: None, text: None });
                    // Break opportunity before an atomic: mark previous piece breakable.
                    let n = self.pieces.len();
                    // (CSS Text: a soft wrap opportunity before and after
                    // each atomic inline, even next to text.)
                    if wraps && n >= 2 && !self.pieces[n - 2].space {
                        let prev_item = self.pieces[n - 2].item;
                        if !matches!(self.items[prev_item], Item::Open { .. }) {
                            self.pieces[n - 2].brk = true;
                        }
                    }
                }
                Item::Open { box_id } | Item::Close { box_id } => {
                    let open = matches!(it, Item::Open { .. });
                    let w = if !measure_only {
                        self.l.resolve_box_metrics(*box_id, content_w);
                        let b = &self.l.boxes[*box_id];
                        if open { b.margin[3] + b.border[3] + b.padding[3] } else { b.margin[1] + b.border[1] + b.padding[1] }
                    } else {
                        // Intrinsic sizing: resolve from style without touching laid-out metrics.
                        let st = &self.l.boxes[*box_id].style;
                        let side = if open { 3 } else { 1 };
                        let m = if st.margin[side].is_auto() { 0.0 } else { st.margin[side].resolve(0.0) };
                        m + st.bw(side) + st.padding[side].resolve(0.0).max(0.0)
                    };
                    self.pieces.push(Piece { item: idx, w, space: false, brk: false, shaped: None, text: None });
                }
                Item::Break(_) | Item::Float { .. } | Item::Abs { .. } => {
                    self.pieces.push(Piece { item: idx, w: 0.0, space: false, brk: true, shaped: None, text: None });
                }
            }
        }
    }
}

/// Whether the line may wrap around atomic inline `id` (its parent's white-space).
fn atomic_wraps(l: &Layout, id: BoxId) -> bool {
    match l.boxes[id].parent {
        Some(p) => l.boxes[p].style.white_space.wraps(),
        None => l.boxes[id].style.white_space.wraps(),
    }
}

/// min-content / max-content of inline content in box `id`.
pub fn intrinsic_inline(l: &mut Layout, id: BoxId) -> (f32, f32) {
    let mut items = Vec::new();
    let mut prev_space = true;
    let include_self = !matches!(l.boxes[id].kind, BoxKind::Block);
    collect(l, id, &mut items, &mut prev_space, include_self);
    let mut bld = Builder { l, items, pieces: Vec::new(), cb_h: None };
    bld.make_pieces(0.0, true);
    let mut min_w: f32 = 0.0;
    let mut max_w: f32 = 0.0;
    let mut word: f32 = 0.0;
    let mut line: f32 = 0.0;
    // Width of collapsible spaces at the current end of the line: they are
    // removed at the end of a line, so they don't count towards max-content.
    let mut trail: f32 = 0.0;
    let pieces = std::mem::take(&mut bld.pieces);
    let items = bld.items.clone();
    for p in &pieces {
        match &items[p.item] {
            Item::Break(_) => {
                max_w = max_w.max(line - trail);
                min_w = min_w.max(word - trail);
                line = 0.0;
                word = 0.0;
                trail = 0.0;
                continue;
            }
            Item::Atomic { box_id } => {
                let (mn, mx) = bld.l.intrinsic_widths(*box_id);
                let m = bld.l.boxes[*box_id].margin[1] + bld.l.boxes[*box_id].margin[3];
                if atomic_wraps(bld.l, *box_id) {
                    min_w = min_w.max(word);
                    word = 0.0;
                    min_w = min_w.max(mn + m);
                } else {
                    // No wrap opportunity around it: it joins the word.
                    word += mx + m;
                }
                line += mx + m;
                trail = 0.0;
                continue;
            }
            Item::Float { box_id } => {
                let (mn, mx) = bld.l.intrinsic_widths(*box_id);
                let m = bld.l.boxes[*box_id].margin[1] + bld.l.boxes[*box_id].margin[3];
                min_w = min_w.max(mn + m);
                line += mx + m;
                continue;
            }
            Item::Abs { .. } => continue,
            _ => {}
        }
        if p.space {
            line += p.w;
            if bs_collapses(bld.l, &items, p) {
                trail += p.w;
            } else {
                trail = 0.0;
            }
            if p.brk {
                min_w = min_w.max(word);
                word = 0.0;
            } else {
                // No break opportunity (nowrap / pre): the space joins the word.
                word += p.w;
            }
            continue;
        }
        line += p.w;
        word += p.w;
        // Inline box boundaries don't stop trailing spaces from hanging.
        if !matches!(items[p.item], Item::Open { .. } | Item::Close { .. }) {
            trail = 0.0;
        }
        if p.brk {
            min_w = min_w.max(word);
            word = 0.0;
        }
    }
    min_w = min_w.max(word - trail);
    max_w = max_w.max(line - trail);
    // Like LayoutNG: round up to a LayoutUnit (1/64 px), not to whole pixels.
    ((min_w * 64.0).ceil() / 64.0, (max_w * 64.0).ceil() / 64.0)
}

struct LineState {
    start: usize, // piece index
    end: usize,
}

pub fn layout_inline(l: &mut Layout, id: BoxId, content_w: f32, cb_h: Option<f32>, fctx: &mut FloatCtx, origin: (f32, f32)) -> InlineLayout {
    let mut items = Vec::new();
    let mut prev_space = true;
    collect(l, id, &mut items, &mut prev_space, false);
    // Inside list marker.
    let bs = l.boxes[id].style.clone();
    let mut bld = Builder { l, items, pieces: Vec::new(), cb_h };
    if bs.display == Display::ListItem && bs.list_style_inside {
        if let Some(spec) = bld.l.boxes[id].list_marker.clone() {
            // Inside markers are inline content (Blink's ListMarker::InlineMarginsForInside:
            // symbols get -1px/6px margins, images a 7px end margin).
            let mut ms = (*spec.style).clone();
            ms.display = Display::Inline;
            ms.margin = [Len::Px(0.0), Len::Px(0.0), Len::Px(0.0), Len::Px(0.0)];
            ms.padding = [Len::Px(0.0), Len::Px(0.0), Len::Px(0.0), Len::Px(0.0)];
            ms.border_width = [0.0; 4];
            ms.background = None;
            ms.background_color = crate::css::values::ColorValue::Rgba(crate::css::values::Color::TRANSPARENT);
            if spec.bullet.is_some() {
                ms.margin[3] = Len::Px(-1.0);
                ms.margin[1] = Len::Px(6.0);
            } else if spec.image.is_some() && spec.text.is_empty() {
                ms.margin[1] = Len::Px(7.0);
            }
            let ms = Rc::new(ms);
            let ib = bld.l.new_box(None, ms.clone(), BoxKind::InlineBox, Some(id));
            bld.l.boxes[ib].inline_level = true;
            let mut marker_items = vec![Item::Open { box_id: ib }];
            if let (Some(img), true) = (&spec.image, spec.text.is_empty()) {
                let rb = bld.l.new_box(None, ms.clone(), BoxKind::Replaced(ReplacedKind::Image), Some(ib));
                bld.l.boxes[rb].image = Some(img.clone());
                bld.l.boxes[rb].inline_level = true;
                bld.l.boxes[ib].children.push(rb);
                marker_items.push(Item::Atomic { box_id: rb });
            } else {
                // The suffix's trailing space doesn't collapse.
                let t = if spec.bullet.is_some() { format!("{} ", spec.text) } else { spec.text.clone() };
                let trimmed = t.trim_end_matches(' ');
                let t = if trimmed.len() < t.len() { format!("{trimmed}\u{a0}") } else { t.clone() };
                if !t.is_empty() {
                    let gb = bld.l.new_box(None, ms.clone(), BoxKind::Generated(Rc::from(t.as_str())), Some(ib));
                    bld.l.boxes[gb].inline_level = true;
                    bld.l.boxes[ib].children.push(gb);
                    marker_items.push(Item::Text { box_id: gb, text: Rc::from(t.as_str()) });
                }
            }
            marker_items.push(Item::Close { box_id: ib });
            bld.items.splice(0..0, marker_items);
        }
    }
    bld.make_pieces(content_w, false);
    let pieces = std::mem::take(&mut bld.pieces);
    let items = bld.items.clone();
    let l = bld.l;

    // Strut metrics of the container.
    let strut = metrics_for(l, &bs);
    let indent = bs.text_indent.resolve(content_w);
    let can_wrap = bs.white_space.wraps();
    let ellipsis = bs.text_overflow_ellipsis && bs.overflow_x.clips() && !can_wrap;

    // `text-wrap: balance`: wrap at the narrowest width that keeps the line
    // count (for up to 6 lines, like Blink), aligning within the full width.
    let wrap_cap = if bs.wrap_balance && can_wrap && content_w.is_finite() { balanced_width(&items, &pieces, content_w, indent) } else { f32::INFINITY };

    let mut lines: Vec<Line> = Vec::new();
    let mut decos: Vec<InlineDeco> = Vec::new();
    let mut y = 0.0f32;
    let mut i = 0;
    let n = pieces.len();
    let mut open_stack: Vec<BoxId> = Vec::new();
    let mut max_line_w: f32 = 0.0;
    let mut first_line = true;

    // Pre-pass: handle the case of no pieces.
    if n == 0 {
        return InlineLayout { lines, decos, height: 0.0, max_line_w: 0.0 };
    }

    while i < n {
        // Determine available width at this y.
        let line_h_guess = strut.2;
        let (x0, x1) = fctx.band_lines(origin.1 + y, line_h_guess, origin.0, origin.0 + content_w);
        if std::env::var_os("OHB_FLOAT_DEBUG").is_some() && !fctx.floats.is_empty() {
            eprintln!("[band] {:?} y={} origin={:?} cw={} -> ({x0},{x1}) floats={:?}", l.boxes[id].node, origin.1 + y, origin, content_w, fctx.floats.iter().map(|f| (f.x as i32, f.y as i32, f.w as i32, f.h as i32)).collect::<Vec<_>>());
        }
        let mut avail = x1 - x0;
        let mut left_off = x0 - origin.0;
        let mut x = if first_line { indent } else { 0.0 };
        // Skip leading collapsible spaces.
        while i < n && pieces[i].space && bs_collapses(l, &items, &pieces[i]) {
            i += 1;
        }
        if i >= n {
            break;
        }
        let start = i;
        let mut last_break: Option<usize> = None; // index of piece after which we may break
        let mut forced = false;
        let mut break_clear: Option<Clear> = None;
        let mut has_content = false;
        let mut trail_w: f32 = 0.0;
        while i < n {
            let p = &pieces[i];
            match &items[p.item] {
                Item::Break(br) => {
                    i += 1;
                    forced = true;
                    break_clear = br.map(|b| l.boxes[b].style.clear).filter(|c| *c != Clear::None);
                    break;
                }
                Item::Float { box_id } => {
                    let fb = *box_id;
                    l.resolve_box_metrics(fb, content_w);
                    l.layout_float(fb, content_w, None);
                    let (fw, fh) = (l.boxes[fb].outer_w(), l.boxes[fb].outer_h());
                    let left = l.boxes[fb].style.float == Float::Left;
                    let fits_now = x + fw <= avail || !has_content;
                    let mut fy = if fits_now { origin.1 + y } else { origin.1 + y + line_h_guess };
                    // A float's own `clear` puts it below earlier floats on that side.
                    let clear = l.boxes[fb].style.clear;
                    if clear != Clear::None {
                        fy = fy.max(fctx.clear_y(clear));
                    }
                    let (fx, fy) = fctx.place(fw, fh, left, fy, origin.0, origin.0 + content_w);
                    if std::env::var_os("OHB_FLOAT_DEBUG").is_some() {
                        eprintln!("[float] inline {:?} fw={fw} fh={fh} at ({fx},{fy}) m={:?} w={}", l.boxes[fb].node, l.boxes[fb].margin, l.boxes[fb].w);
                    }
                    let shape = l.float_shape(fb);
                    fctx.set_last_shape(shape);
                    let cxo = l.boxes[id].content_x();
                    let cyo = l.boxes[id].content_y();
                    let b = &mut l.boxes[fb];
                    b.x = fx - origin.0 + cxo + b.margin[3];
                    b.y = fy - origin.1 + cyo + b.margin[0];
                    if fits_now {
                        // The float narrows the current line; content already on it shifts with left_off.
                        let (nx0, nx1) = fctx.band_lines(origin.1 + y, line_h_guess, origin.0, origin.0 + content_w);
                        avail = nx1 - nx0;
                        left_off = nx0 - origin.0;
                    }
                    i += 1;
                    continue;
                }
                Item::Abs { box_id } => {
                    let cxo = l.boxes[id].content_x();
                    let cyo = l.boxes[id].content_y();
                    l.register_oof(*box_id, cxo + left_off + x, cyo + y);
                    i += 1;
                    continue;
                }
                _ => {}
            }
            // Inline box boundaries after collapsible spaces: the spaces hang.
            let boundary = matches!(items[p.item], Item::Open { .. } | Item::Close { .. });
            let wrap_w = avail.min(wrap_cap);
            let fits = if boundary { x - trail_w + p.w <= wrap_w + 0.01 } else { x + p.w <= wrap_w + 0.01 };
            if !fits && can_wrap && has_content && !p.space {
                if let Some(b) = last_break {
                    // Break after `b`.
                    i = b + 1;
                    break;
                }
                // No earlier opportunity: overflow-wrap anywhere?
            }
            if !fits && can_wrap && !has_content && !p.space && l.boxes[id].style.break_anywhere {
                // Split a long word by characters.
                if let (Some(text), Item::Text { box_id, .. }) = (&p.text, &items[p.item]) {
                    let s = l.boxes[*box_id].style.clone();
                    let mut cut = 0;
                    let mut acc = String::new();
                    for (bi, ch) in text.char_indices() {
                        acc.push(ch);
                        let w = l.fonts.shape_s(&acc, &s).width;
                        if x + w > avail + 0.01 && bi > 0 {
                            break;
                        }
                        cut = bi + ch.len_utf8();
                    }
                    if cut > 0 && cut < text.len() {
                        // Replace the piece in place by two pieces (rebuild slice).
                        let head = &text[..cut];
                        let tail = &text[cut..];
                        let hs = l.fonts.shape_s(head, &s);
                        let ts = l.fonts.shape_s(tail, &s);
                        let mut np = pieces.clone();
                        np[i] = Piece { item: p.item, w: hs.width, space: false, brk: true, shaped: Some(hs), text: Some(Rc::from(head)) };
                        np.insert(i + 1, Piece { item: p.item, w: ts.width, space: false, brk: p.brk, shaped: Some(ts), text: Some(Rc::from(tail)) });
                        // Restart with the new piece list.
                        let mut rest = InlineRestart { pieces: np, i: start };
                        return relayout_with(l, id, content_w, fctx, origin, items, &mut rest);
                    }
                }
            }
            x += p.w;
            if p.space && bs_collapses(l, &items, p) {
                trail_w += p.w;
            } else if !boundary {
                trail_w = 0.0;
            }
            if !matches!(items[p.item], Item::Open { .. } | Item::Close { .. }) || p.w > 0.0 {
                if !p.space {
                    has_content = true;
                }
            }
            if matches!(items[p.item], Item::Atomic { .. }) {
                has_content = true;
            }
            if p.brk && can_wrap {
                last_break = Some(i);
            }
            if p.space && can_wrap {
                last_break = Some(i);
            }
            i += 1;
        }
        let end = i;
        let st = LineState { start, end };
        let line = build_line(l, id, &items, &pieces, &st, y, left_off, avail, first_line, forced || end >= n, &strut, &mut open_stack, &mut decos, ellipsis);
        let lh = line.h;
        max_line_w = max_line_w.max(line.width + left_off);
        // Skip empty lines that contain nothing (e.g. only Open/Close of empty spans) unless forced.
        let visible = !line.frags.is_empty() || forced || line.width > 0.0;
        if visible {
            y += lh;
            lines.push(line);
        }
        // `<br style="clear: ...">` (or clear="all"): the next line starts below the floats.
        if let Some(c) = break_clear {
            y = y.max(fctx.clear_y(c) - origin.1);
        }
        first_line = false;
        if end == start {
            i += 1; // safety
        }
    }
    InlineLayout { lines, decos, height: y, max_line_w }
}

/// Lines a greedy wrap of `pieces` at `width` makes (no floats).
fn greedy_line_count(items: &[Item], pieces: &[Piece], width: f32, indent: f32) -> usize {
    let n = pieces.len();
    let mut lines = 0;
    let mut i = 0;
    let mut first = true;
    while i < n {
        while i < n && pieces[i].space {
            i += 1;
        }
        if i >= n {
            break;
        }
        let mut x = if first { indent } else { 0.0 };
        first = false;
        let mut last_break: Option<usize> = None;
        let mut has_content = false;
        let start = i;
        while i < n {
            let p = &pieces[i];
            match items[p.item] {
                Item::Break(_) => {
                    i += 1;
                    break;
                }
                Item::Float { .. } | Item::Abs { .. } => {
                    i += 1;
                    continue;
                }
                _ => {}
            }
            if x + p.w > width + 0.01 && has_content && !p.space {
                if let Some(b) = last_break {
                    i = b + 1;
                    break;
                }
            }
            x += p.w;
            if !p.space && (p.w > 0.0 || matches!(items[p.item], Item::Atomic { .. })) {
                has_content = true;
            }
            if p.brk || p.space {
                last_break = Some(i);
            }
            i += 1;
        }
        if i == start {
            i += 1;
        }
        lines += 1;
        if lines > 64 {
            break;
        }
    }
    lines
}

/// The narrowest wrap width (at most `width`) whose greedy wrap has no more
/// lines than at `width`; unbounded when balancing doesn't apply.
fn balanced_width(items: &[Item], pieces: &[Piece], width: f32, indent: f32) -> f32 {
    if items.iter().any(|it| matches!(it, Item::Float { .. })) {
        return f32::INFINITY;
    }
    let lines = greedy_line_count(items, pieces, width, indent);
    if !(2..=6).contains(&lines) {
        return f32::INFINITY;
    }
    let widest = pieces.iter().filter(|p| !p.space).map(|p| p.w).fold(0.0f32, f32::max);
    let (mut lo, mut hi) = (widest.max(width / lines as f32 * 0.9).min(width), width);
    if greedy_line_count(items, pieces, lo, indent) <= lines {
        return lo;
    }
    for _ in 0..14 {
        let mid = (lo + hi) / 2.0;
        if greedy_line_count(items, pieces, mid, indent) <= lines {
            hi = mid;
        } else {
            lo = mid;
        }
        if hi - lo < 0.5 {
            break;
        }
    }
    hi
}

struct InlineRestart {
    pieces: Vec<Piece>,
    i: usize,
}

/// Re-run line layout with a modified piece list (after splitting a word).
fn relayout_with(l: &mut Layout, id: BoxId, content_w: f32, fctx: &mut FloatCtx, origin: (f32, f32), items: Vec<Item>, rest: &mut InlineRestart) -> InlineLayout {
    // Simple approach: split every overlong word up-front and lay out again.
    let bs = l.boxes[id].style.clone();
    let mut pieces = Vec::new();
    for p in rest.pieces.drain(..) {
        if p.w > content_w && !p.space {
            if let (Some(text), Item::Text { box_id, .. }) = (&p.text, &items[p.item]) {
                let s = l.boxes[*box_id].style.clone();
                let mut acc = String::new();
                for ch in text.chars() {
                    let mut trial = acc.clone();
                    trial.push(ch);
                    let w = l.fonts.shape_s(&trial, &s).width;
                    if w > content_w && !acc.is_empty() {
                        let sh = l.fonts.shape_s(&acc, &s);
                        pieces.push(Piece { item: p.item, w: sh.width, space: false, brk: true, shaped: Some(sh), text: Some(Rc::from(acc.as_str())) });
                        acc.clear();
                    }
                    acc.push(ch);
                }
                if !acc.is_empty() {
                    let sh = l.fonts.shape_s(&acc, &s);
                    pieces.push(Piece { item: p.item, w: sh.width, space: false, brk: p.brk, shaped: Some(sh), text: Some(Rc::from(acc.as_str())) });
                }
                continue;
            }
        }
        pieces.push(p);
    }
    let _ = rest.i;
    let strut = metrics_for(l, &bs);
    let mut lines = Vec::new();
    let mut decos = Vec::new();
    let mut open_stack = Vec::new();
    let mut y = 0.0;
    let mut i = 0;
    let n = pieces.len();
    let mut max_line_w: f32 = 0.0;
    let mut first = true;
    while i < n {
        let (x0, x1) = fctx.band_lines(origin.1 + y, strut.2, origin.0, origin.0 + content_w);
        let avail = x1 - x0;
        while i < n && pieces[i].space {
            i += 1;
        }
        if i >= n {
            break;
        }
        let start = i;
        let mut x = 0.0;
        let mut last_break = None;
        let mut forced = false;
        let mut has = false;
        while i < n {
            let p = &pieces[i];
            if matches!(items[p.item], Item::Break(_)) {
                i += 1;
                forced = true;
                break;
            }
            if x + p.w > avail + 0.01 && has && !p.space {
                if let Some(b) = last_break {
                    i = b + 1;
                    break;
                }
            }
            x += p.w;
            if !p.space {
                has = true;
            }
            if p.brk || p.space {
                last_break = Some(i);
            }
            i += 1;
        }
        let st = LineState { start, end: i };
        let line = build_line(l, id, &items, &pieces, &st, y, x0 - origin.0, avail, first, forced || i >= n, &strut, &mut open_stack, &mut decos, false);
        y += line.h;
        max_line_w = max_line_w.max(line.width);
        lines.push(line);
        first = false;
    }
    InlineLayout { lines, decos, height: y, max_line_w }
}

fn bs_collapses(l: &Layout, items: &[Item], p: &Piece) -> bool {
    match &items[p.item] {
        Item::Text { box_id, .. } => l.boxes[*box_id].style.white_space.collapses_spaces() || l.boxes[*box_id].style.white_space == WhiteSpace::PreWrap,
        _ => true,
    }
}

/// Split the leading (line-height minus content height) above and below: the
/// part above is floored to whole pixels, as in Blink's LayoutNG.
fn leading(lh: f32, a: f32, d: f32) -> (f32, f32) {
    let total = lh - (a + d);
    let above = (total / 2.0).floor();
    (above, total - above)
}

/// (ascent, descent, line-height) for a style's primary font. Like Blink,
/// ascent, descent and line gap are each rounded to whole pixels, and
/// `line-height: normal` is their sum.
fn metrics_for(l: &mut Layout, s: &ComputedStyle) -> (f32, f32, f32) {
    let font = l.fonts.primary(&s.font_family, s.font_weight, s.font_style);
    let sc = font.scale(s.font_size);
    let a = (font.ascent * sc).round();
    let d = (font.descent * sc).round();
    let lh = match s.line_height {
        LineHeight::Normal => a + d + (font.line_gap * sc).round(),
        _ => s.line_height_px(),
    };
    (a, d, lh)
}

#[allow(clippy::too_many_arguments)]
fn build_line(
    l: &mut Layout,
    id: BoxId,
    items: &[Item],
    pieces: &[Piece],
    st: &LineState,
    y: f32,
    left_off: f32,
    avail: f32,
    first_line: bool,
    last_line: bool,
    strut: &(f32, f32, f32),
    open_stack: &mut Vec<BoxId>,
    decos: &mut Vec<InlineDeco>,
    ellipsis: bool,
) -> Line {
    let bs = l.boxes[id].style.clone();
    // Trim trailing collapsible spaces, also when inline box boundaries follow them.
    let mut drop: Vec<usize> = Vec::new();
    let mut k = st.end;
    while k > st.start {
        let p = &pieces[k - 1];
        if matches!(items[p.item], Item::Open { .. } | Item::Close { .. }) {
            k -= 1;
            continue;
        }
        if p.space && bs_collapses(l, items, p) {
            drop.push(k - 1);
            k -= 1;
        } else {
            break;
        }
    }
    let mut seq: Vec<usize> = (st.start..st.end).filter(|i| !drop.contains(i)).collect();
    // Justification info.
    let mut x = if first_line { bs.text_indent.resolve(avail) } else { 0.0 };
    struct Placed {
        piece: usize,
        x: f32,
        w: f32,
    }
    let mut placed: Vec<Placed> = Vec::new();
    let x0 = x;
    for &pi in &seq {
        let p = &pieces[pi];
        placed.push(Placed { piece: pi, x, w: p.w });
        x += p.w;
    }
    // Bidirectional text: lay the pieces out in visual order (UAX #9 L2 at
    // word granularity), keeping `placed` in logical order.
    if bs.direction_rtl || seq.iter().any(|&pi| pieces[pi].text.as_deref().map(has_rtl).unwrap_or(false)) {
        // Content of isolates (bdi, [dir] inline boxes) is neutral to the surrounding text.
        let isolated = |b: BoxId| {
            let mut cur = l.boxes[b].parent;
            while let Some(c) = cur {
                if c == id {
                    return false;
                }
                if l.boxes[c].style.bidi_isolate {
                    return true;
                }
                cur = l.boxes[c].parent;
            }
            false
        };
        let classes: Vec<u8> = placed
            .iter()
            .map(|p| {
                let pc = &pieces[p.piece];
                match &items[pc.item] {
                    Item::Text { box_id, .. } if isolated(*box_id) => 0,
                    _ => piece_class(pc),
                }
            })
            .collect();
        let levels = bidi_levels(&classes, bs.direction_rtl);
        let order = visual_order(&levels);
        let mut vx = x0;
        for &i in &order {
            placed[i].x = vx;
            vx += placed[i].w;
        }
    }
    let mut line_w = x;
    // Ellipsis truncation.
    let mut ellipsis_frag: Option<(f32, Rc<ShapedText>)> = None;
    let mut cut_piece: Option<(usize, Rc<ShapedText>, Rc<str>)> = None;
    if ellipsis && line_w > avail {
        let es = l.fonts.shape("…", &bs.font_family, bs.font_weight, bs.font_style, bs.font_size, 0.0);
        let limit = (avail - es.width).max(0.0);
        let mut keep = Vec::new();
        for pl in placed.drain(..) {
            if pl.x + pl.w <= limit {
                keep.push(pl);
            } else {
                // Like Blink, keep the characters of the crossing word that fit.
                let pc = &pieces[pl.piece];
                if let (Some(sh), Some(t), Item::Text { box_id, .. }) = (&pc.shaped, &pc.text, &items[pc.item]) {
                    let room = limit - pl.x;
                    let mut best = 0;
                    for (b, _) in t.char_indices().skip(1) {
                        if sh.x_of_byte(b) <= room + 0.01 {
                            best = b;
                        } else {
                            break;
                        }
                    }
                    if best > 0 {
                        let head = &t[..best];
                        let s = l.boxes[*box_id].style.clone();
                        let hs = l.fonts.shape_s(head, &s);
                        let w = hs.width;
                        cut_piece = Some((pl.piece, hs, Rc::from(head)));
                        keep.push(Placed { piece: pl.piece, x: pl.x, w });
                    }
                }
                break;
            }
        }
        let ex = keep.last().map(|p| p.x + p.w).unwrap_or(0.0);
        placed = keep;
        seq = placed.iter().map(|p| p.piece).collect();
        ellipsis_frag = Some((ex, es));
        line_w = avail;
    }
    // Horizontal alignment.
    let free = (avail - line_w).max(0.0);
    let align = bs.text_align;
    let rtl = bs.direction_rtl;
    let shift = match align {
        TextAlign::Left => 0.0,
        TextAlign::Right => free,
        TextAlign::Start | TextAlign::Justify => if rtl { free } else { 0.0 },
        TextAlign::End => if rtl { 0.0 } else { free },
        TextAlign::Center | TextAlign::WebkitCenter => free / 2.0,
    };
    // The last line of justified text is start-aligned.
    let shift = if align == TextAlign::Justify && (last_line || free <= 0.0) && rtl { free } else { shift };
    if align == TextAlign::Justify && !last_line && free > 0.0 {
        let spaces = placed.iter().filter(|p| pieces[p.piece].space).count();
        if spaces > 0 {
            let extra = free / spaces as f32;
            let mut add = 0.0;
            for p in placed.iter_mut() {
                p.x += add;
                if pieces[p.piece].space {
                    p.w += extra;
                    add += extra;
                }
            }
            line_w = avail;
        }
    }
    let _ = seq;

    // Build fragments, merging consecutive text pieces of the same item.
    let mut frags: Vec<Frag> = Vec::new();
    let (sa, sd, slh) = *strut;
    let (ha, hd) = leading(slh, sa, sd);
    let mut above = sa + ha; // relative to baseline
    let mut below = sd + hd;
    // Vertical metrics for open inline boxes on this line.
    let mut deco_open: Vec<(BoxId, f32, bool)> = open_stack.iter().map(|&b| (b, shift + left_off, false)).collect();
    let mut deco_out: Vec<(BoxId, f32, f32, bool, bool)> = Vec::new();
    type CurText = Option<(usize, f32, Vec<(Rc<ShapedText>, f32)>, f32, String, Vec<u32>)>; // (item, x, pieces, w, text, starts)
    let mut cur_text: CurText = None;
    let finish_text = |cur: &mut CurText, frags: &mut Vec<Frag>| {
        if let Some((item, fx, ps, w, text, starts)) = cur.take() {
            if let Item::Text { box_id, .. } = &items[item] {
                frags.push(Frag { x: fx, y: 0.0, w, h: 0.0, ascent: 0.0, box_id: *box_id, kind: FragKind::Text { pieces: ps, text: Rc::from(text.as_str()), starts } });
            }
        }
    };
    for p in &placed {
        let pc = &pieces[p.piece];
        let px = p.x + shift + left_off;
        match &items[pc.item] {
            Item::Text { .. } => {
                let same = matches!(&cur_text, Some((it, fx, _, w, _, _)) if *it == pc.item && px >= *fx + *w - 0.01);
                if !same {
                    finish_text(&mut cur_text, &mut frags);
                    cur_text = Some((pc.item, px, Vec::new(), 0.0, String::new(), Vec::new()));
                }
                if let Some((_, fx, ps, w, text, starts)) = cur_text.as_mut() {
                    let cut = cut_piece.as_ref().filter(|c| c.0 == p.piece);
                    if let Some((_, sh, t)) = cut {
                        ps.push((sh.clone(), px - *fx));
                        starts.push(text.len() as u32);
                        text.push_str(t);
                    } else if let Some(sh) = &pc.shaped {
                        ps.push((sh.clone(), px - *fx));
                        starts.push(text.len() as u32);
                        if let Some(t) = &pc.text {
                            text.push_str(t);
                        }
                    } else if pc.space {
                        text.push(' ');
                    }
                    *w = px + p.w - *fx;
                }
            }
            Item::Atomic { box_id } => {
                finish_text(&mut cur_text, &mut frags);
                frags.push(Frag { x: px, y: 0.0, w: p.w, h: 0.0, ascent: 0.0, box_id: *box_id, kind: FragKind::Atomic });
            }
            Item::Open { box_id } => {
                finish_text(&mut cur_text, &mut frags);
                let m = l.boxes[*box_id].margin[3];
                deco_open.push((*box_id, px + m, true));
                open_stack.push(*box_id);
            }
            Item::Close { box_id } => {
                finish_text(&mut cur_text, &mut frags);
                let m = l.boxes[*box_id].margin[1];
                if let Some(pos) = deco_open.iter().rposition(|(b, _, _)| b == box_id) {
                    let (b, sx, first) = deco_open.remove(pos);
                    deco_out.push((b, sx, px + p.w - m, first, true));
                }
                if let Some(pos) = open_stack.iter().rposition(|b| b == box_id) {
                    open_stack.remove(pos);
                }
            }
            _ => {}
        }
    }
    finish_text(&mut cur_text, &mut frags);
    let line_end_x = line_w + shift + left_off;
    for (b, sx, first) in deco_open {
        deco_out.push((b, sx, line_end_x, first, false));
    }
    if let Some((ex, es)) = ellipsis_frag {
        frags.push(Frag { x: ex + shift + left_off, y: 0.0, w: es.width, h: 0.0, ascent: 0.0, box_id: id, kind: FragKind::Ellipsis(es) });
    } else if !last_line {
        // A line broken at a soft hyphen ends with a visible hyphen.
        if let Some(lp) = placed.last() {
            let pc = &pieces[lp.piece];
            if let (Some(t), Item::Text { box_id, .. }) = (&pc.text, &items[pc.item]) {
                if t.ends_with('\u{00ad}') {
                    let s = l.boxes[*box_id].style.clone();
                    let hs = l.fonts.shape_s("-", &s);
                    frags.push(Frag { x: lp.x + lp.w + shift + left_off, y: 0.0, w: hs.width, h: 0.0, ascent: 0.0, box_id: *box_id, kind: FragKind::Ellipsis(hs) });
                }
            }
        }
    }

    // Vertical alignment: compute each fragment's ascent/descent relative to the baseline.
    struct VInfo {
        a: f32,
        d: f32,
        shift: f32,
        top_bottom: i8,
    }
    let mut vinfo: Vec<VInfo> = Vec::with_capacity(frags.len());
    for f in &frags {
        let fb = f.box_id;
        let s = l.boxes[fb].style.clone();
        match &f.kind {
            FragKind::Text { .. } | FragKind::Ellipsis(_) => {
                let (a, d, lh) = metrics_for(l, &s);
                let (ha, hd) = leading(lh, a, d);
                // vertical-align of the text's parent inline box chain.
                let va_shift = inline_va_shift(l, fb, id);
                vinfo.push(VInfo { a: a + ha, d: d + hd, shift: va_shift, top_bottom: 0 });
            }
            FragKind::Atomic => {
                let (ca, cd, _) = metrics_for(l, &s);
                let b = &l.boxes[fb];
                let h = b.outer_h();
                let base = match &b.kind {
                    // A color well sits on the bottom of its swatch (4px above the content bottom).
                    BoxKind::Replaced(ReplacedKind::ColorWell) if !b.style.appearance_none => Some(b.h - b.border[2] - b.padding[2] - 4.0),
                    BoxKind::Replaced(k) if matches!(k, ReplacedKind::TextInput | ReplacedKind::Button | ReplacedKind::Select | ReplacedKind::DateTime | ReplacedKind::File | ReplacedKind::ColorWell) => {
                        // Text baseline of the vertically centered inner line.
                        let inner = b.h - b.border[0] - b.border[2] - b.padding[0] - b.padding[2];
                        Some(b.border[0] + b.padding[0] + (inner - (ca + cd)) / 2.0 + ca)
                    }
                    // Native checkboxes and radios sit on the bottom of their border box.
                    BoxKind::Replaced(ReplacedKind::Checkbox | ReplacedKind::Radio | ReplacedKind::Range) if !b.style.appearance_none => Some(b.h),
                    BoxKind::Replaced(_) => None,
                    _ => {
                        // CSS 2.1: an inline-block that clips its overflow uses its
                        // bottom margin edge; flex/grid containers keep their item
                        // baseline whatever their overflow.
                        let bs = &b.style;
                        if bs.is_scroll_container() && !bs.display.is_flex() && !bs.display.is_grid() {
                            None
                        } else {
                            b.baseline
                        }
                    }
                };
                let a = match base {
                    Some(bl) => b.margin[0] + bl,
                    None => h,
                };
                let (pa, ppd, _) = metrics_for(l, &l.boxes[l.boxes[fb].parent.unwrap_or(id)].style.clone());
                let mut sh = inline_va_shift(l, fb, id);
                let mut tb = 0;
                match &s.vertical_align {
                    VerticalAlign::Middle => {
                        // center at baseline + x-height/2 of parent
                        let font = l.fonts.primary(&bs.font_family, bs.font_weight, bs.font_style);
                        let xh = font.x_height * font.scale(bs.font_size);
                        // Midpoint at baseline + x-height/2 (positive shift raises the box).
                        sh += xh / 2.0 + h / 2.0 - a;
                    }
                    VerticalAlign::Top => tb = 1,
                    VerticalAlign::Bottom => tb = 2,
                    // Align with the parent's content area (its font's ascent/descent).
                    VerticalAlign::TextTop => sh += pa - a,
                    VerticalAlign::TextBottom => sh += h - a - ppd,
                    _ => {}
                }
                vinfo.push(VInfo { a, d: h - a, shift: sh, top_bottom: tb });
            }
        }
    }
    // Quirks mode "line height calculation quirk": inline boxes (including the
    // root inline box) without text of their own don't contribute a strut.
    let quirks = l.doc.quirks || l.doc.limited_quirks;
    if quirks && !frags.is_empty() {
        let root_text = frags.iter().any(|f| matches!(f.kind, FragKind::Text { .. }) && (f.box_id == id || l.boxes[f.box_id].parent == Some(id)));
        if !root_text {
            above = 0.0;
            below = 0.0;
        }
    }
    for v in &vinfo {
        if v.top_bottom == 0 {
            above = above.max(v.a + v.shift);
            below = below.max(v.d - v.shift);
        }
    }
    // Open inline boxes contribute their strut.
    for (b, _, _, _, _) in &deco_out {
        if quirks {
            let bx = &l.boxes[*b];
            let has_vertical = bx.border[0] + bx.border[2] + bx.padding[0] + bx.padding[2] > 0.0;
            let has_text = frags.iter().any(|f| matches!(f.kind, FragKind::Text { .. }) && l.boxes[f.box_id].parent == Some(*b));
            if !has_vertical && !has_text {
                continue;
            }
        }
        let s = l.boxes[*b].style.clone();
        let (a, d, lh) = metrics_for(l, &s);
        let (ha, hd) = leading(lh, a, d);
        let sh = inline_va_shift(l, *b, id);
        above = above.max(a + ha + sh);
        below = below.max(d + hd - sh);
    }
    // An outside list marker takes part in its item's first line (LayoutNG).
    if first_line && l.boxes[id].style.display == Display::ListItem && !l.boxes[id].style.list_style_inside {
        if let Some(spec) = l.boxes[id].list_marker.clone() {
            // The marker's baseline aligns with the line's; a marker taller above
            // the baseline pushes the content down by whole pixels, and its
            // descent never grows the line (NGUnpositionedListMarker::AddToBox).
            let m_above = if let (Some(img), true) = (&spec.image, spec.text.is_empty()) {
                img.natural().1
            } else {
                let (a, d, lh) = metrics_for(l, &spec.style);
                a + ((lh - a - d) / 2.0).floor()
            };
            let push = (m_above - above).ceil();
            if push > 0.0 {
                above += push;
            }
        }
    }
    let mut line_h = above + below;
    for v in &vinfo {
        if v.top_bottom != 0 {
            line_h = line_h.max(v.a + v.d);
        }
    }
    let baseline = above;
    for (f, v) in frags.iter_mut().zip(&vinfo) {
        match v.top_bottom {
            1 => {
                f.y = y;
                f.ascent = v.a;
            }
            2 => {
                f.y = y + line_h - (v.a + v.d);
                f.ascent = v.a;
            }
            _ => {
                f.y = y + baseline - v.shift - v.a;
                f.ascent = v.a;
            }
        }
        f.h = v.a + v.d;
    }
    // Position atomic boxes (relative to the block's border box).
    let cx = l.boxes[id].content_x();
    let cy = l.boxes[id].content_y();
    for f in &frags {
        if let FragKind::Atomic = f.kind {
            let b = &mut l.boxes[f.box_id];
            b.x = cx + f.x + b.margin[3];
            b.y = cy + f.y + b.margin[0];
        }
    }
    // Inline box decorations (backgrounds/borders), vertical extent from own font.
    for (b, x0, x1, first, last) in deco_out {
        let s = l.boxes[b].style.clone();
        let (a, d, _) = metrics_for(l, &s);
        let sh = inline_va_shift(l, b, id);
        let bx = &l.boxes[b];
        let top = y + baseline - sh - a - bx.padding[0] - bx.border[0];
        let h = a + d + bx.padding[0] + bx.padding[2] + bx.border[0] + bx.border[2];
        decos.push(InlineDeco { box_id: b, x0, x1, y: top, h, first, last });
    }
    Line { y, h: line_h, baseline, frags, width: line_w }
}

fn va_own(s: &ComputedStyle, parent: &ComputedStyle) -> f32 {
    match &s.vertical_align {
        VerticalAlign::Sub => -parent.font_size * 0.2,
        VerticalAlign::Super => parent.font_size * 0.35,
        VerticalAlign::Len(l) => l.resolve(s.line_height_px()),
        _ => 0.0,
    }
}

/// Cumulative baseline shift (upwards positive) from vertical-align on the
/// inline ancestors of `b` up to the IFC root.
fn inline_va_shift(l: &Layout, b: BoxId, root: BoxId) -> f32 {
    let mut shift = 0.0;
    let mut cur = Some(b);
    let mut guard = 0;
    while let Some(c) = cur {
        if c == root || guard > 64 {
            break;
        }
        guard += 1;
        let s = &l.boxes[c].style;
        let parent = l.boxes[c].parent.unwrap_or(root);
        let ps = &l.boxes[parent].style;
        if matches!(l.boxes[c].kind, BoxKind::InlineBox) || matches!(l.boxes[c].kind, BoxKind::Text(_) | BoxKind::Generated(_)) && c != b {
            shift += va_own(s, ps);
        } else if c == b && !matches!(l.boxes[c].kind, BoxKind::Text(_) | BoxKind::Generated(_)) {
            shift += va_own(s, ps);
        }
        cur = l.boxes[c].parent;
    }
    shift
}

/// Any right-to-left character (Hebrew, Arabic and related scripts)?
fn has_rtl(t: &str) -> bool {
    t.chars().any(is_rtl_char)
}

fn is_rtl_char(ch: char) -> bool {
    let c = ch as u32;
    (0x0590..=0x08FF).contains(&c) || (0xFB1D..=0xFDFF).contains(&c) || (0xFE70..=0xFEFF).contains(&c) || (0x10800..=0x10FFF).contains(&c) || (0x1E800..=0x1EFFF).contains(&c)
}

/// Bidi class of a piece: 0 neutral, 1 left-to-right, 2 right-to-left, 3 number.
fn piece_class(p: &Piece) -> u8 {
    let t = match &p.text {
        Some(t) => t,
        None => return 0,
    };
    for ch in t.chars() {
        if is_rtl_char(ch) {
            return 2;
        }
        if ch.is_alphabetic() {
            return 1;
        }
    }
    if t.chars().any(|c| c.is_ascii_digit() || ('\u{0660}'..='\u{0669}').contains(&c)) {
        3
    } else {
        0
    }
}

/// Embedding levels for a line's pieces (simplified UAX #9: strong types,
/// numbers as left-to-right runs, neutrals resolved from their neighbors).
fn bidi_levels(classes: &[u8], rtl: bool) -> Vec<u8> {
    let para = if rtl { 2u8 } else { 1u8 };
    // Direction each piece resolves to (1 L, 2 R); numbers count as R for neutrals in R context.
    let strong: Vec<Option<u8>> = classes.iter().map(|&c| match c { 1 => Some(1), 2 => Some(2), _ => None }).collect();
    let mut levels = vec![if rtl { 1u8 } else { 0u8 }; classes.len()];
    for i in 0..classes.len() {
        let dir = match classes[i] {
            1 => 1,
            2 => 2,
            3 => {
                // European numbers: left-to-right runs (level 2 inside R text).
                let prev = (0..i).rev().find_map(|j| strong[j]).unwrap_or(para);
                if prev == 2 || rtl { 3 } else { 1 }
            }
            _ => {
                let prev = (0..i).rev().find_map(|j| match classes[j] { 3 => Some(2), c => strong[j].or(if c == 3 { Some(2) } else { None }) }).unwrap_or(para);
                let next = (i + 1..classes.len()).find_map(|j| match classes[j] { 3 => Some(2), _ => strong[j] }).unwrap_or(para);
                if prev == next { prev } else { para }
            }
        };
        levels[i] = match (rtl, dir) {
            (false, 1) => 0,
            (false, 2) => 1,
            (false, _) => 2,
            (true, 2) => 1,
            (true, _) => 2,
        };
    }
    levels
}

/// Visual order of items with the given levels (UAX #9 rule L2).
fn visual_order(levels: &[u8]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..levels.len()).collect();
    let max = levels.iter().copied().max().unwrap_or(0);
    let min_odd = levels.iter().copied().filter(|l| l % 2 == 1).min().unwrap_or(max + 1);
    let mut lvl = max;
    while lvl >= min_odd && lvl > 0 {
        let mut i = 0;
        while i < order.len() {
            if levels[order[i]] >= lvl {
                let start = i;
                while i < order.len() && levels[order[i]] >= lvl {
                    i += 1;
                }
                order[start..i].reverse();
            } else {
                i += 1;
            }
        }
        lvl -= 1;
    }
    order
}

#[cfg(test)]
mod tests {
    use super::leading;

    #[test]
    fn bidi_reorders_words() {
        // RTL paragraph: R R R (with spaces) is fully reversed.
        assert_eq!(super::visual_order(&super::bidi_levels(&[2, 0, 2, 0, 2], true)), vec![4, 3, 2, 1, 0]);
        // LTR paragraph with an embedded RTL phrase: only the phrase reverses.
        assert_eq!(super::visual_order(&super::bidi_levels(&[1, 0, 2, 0, 2, 0, 1], false)), vec![0, 1, 4, 3, 2, 5, 6]);
        // English word inside an RTL paragraph keeps its own order.
        assert_eq!(super::visual_order(&super::bidi_levels(&[2, 0, 1, 0, 1], true)), vec![2, 3, 4, 1, 0]);
    }

    #[test]
    fn half_leading_floors_above() {
        assert_eq!(leading(20.0, 16.0, 4.0), (0.0, 0.0));
        assert_eq!(leading(24.0, 15.0, 4.0), (2.0, 3.0));
        assert_eq!(leading(19.5, 12.0, 4.0), (1.0, 2.5));
        assert_eq!(leading(10.0, 12.0, 3.0), (-3.0, -2.0));
    }
}
