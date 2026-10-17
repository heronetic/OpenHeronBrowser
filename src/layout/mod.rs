// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronLayout — box tree generation and layout.
//!
//! Boxes live in an arena (`Vec<LayoutBox>`). Each box stores its border-box
//! position relative to its parent box (or, for absolutely positioned boxes,
//! relative to its containing block); a final pass computes absolute document
//! coordinates used by painting and hit testing.

pub mod block;
pub mod flex;
pub mod gen;
pub mod grid;
pub mod inline;
pub mod multicol;
pub mod selection;
pub mod table;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::css::style::*;
use crate::css::values::Len;
use crate::css::Styles;
use crate::dom::{Document, Namespace, NodeData, NodeId};
use crate::paint::ImageData;
use crate::text::FontContext;

pub type BoxId = usize;

#[derive(Clone, Debug, PartialEq)]
pub enum ReplacedKind {
    Image,
    Svg,
    TextInput,
    Checkbox,
    Radio,
    Button,
    Select,
    TextArea,
    Range,
    Progress,
    Media,
    Frame,
    Canvas,
    /// date / time / datetime-local / month / week
    DateTime,
    /// input type=color
    ColorWell,
    /// input type=file
    File,
}

#[derive(Clone)]
pub enum BoxKind {
    Block,
    InlineBox,
    Text(Rc<str>),
    Replaced(ReplacedKind),
    LineBreak,
    /// Generated text (pseudo-element content / list marker inside).
    Generated(Rc<str>),
}

#[derive(Clone)]
pub struct Marker {
    pub text: String,
    pub x: f32,
    pub baseline: f32,
    pub shaped: Option<Rc<crate::text::ShapedText>>,
    pub bullet: Option<char>,
    /// The `::marker` style (color and font), when it differs from the item's.
    pub style: Option<Rc<ComputedStyle>>,
    /// `list-style-image`: the image and its size.
    pub image: Option<(Arc<ImageData>, f32, f32)>,
}

pub struct LayoutBox {
    /// Containing block size when it isn't the parent's content box (grid areas).
    pub cb_size: Option<(f32, f32)>,
    pub node: Option<NodeId>,
    pub style: Rc<ComputedStyle>,
    pub kind: BoxKind,
    pub children: Vec<BoxId>,
    pub parent: Option<BoxId>,
    pub inline_level: bool,
    pub out_of_flow: bool,
    pub is_float: bool,
    pub pseudo: u8, // 0 none, 1 before, 2 after, 3 anonymous
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub margin: [f32; 4],
    pub border: [f32; 4],
    pub padding: [f32; 4],
    pub ifc: Option<Box<inline::InlineLayout>>,
    pub abs_x: f32,
    pub abs_y: f32,
    pub scroll_w: f32,
    pub scroll_h: f32,
    pub baseline: Option<f32>,
    pub marker: Option<Marker>,
    pub abs_children: Vec<BoxId>,
    pub static_pos: (f32, f32),
    /// The static position is the inline-start (right) edge of an RTL container.
    pub static_rtl: bool,
    pub intrinsic: Cell<Option<(f32, f32)>>,
    pub image: Option<Arc<ImageData>>,
    pub ordinal: i32,
    pub fixed: bool,
    /// Block created from an inline element containing blocks: percentages in its
    /// children resolve against the enclosing containing block (inlines are never one).
    pub cb_passthrough: bool,
    /// Collapsed-through margin struts (positive max, negative min) when the
    /// box's top/bottom margin collapsed with descendants' margins.
    pub top_strut: Option<block::Strut>,
    pub bot_strut: Option<block::Strut>,
    /// Table painting flags (`table::TBL_*`).
    pub tbl: u8,
    /// Collapsing border model: border widths used for layout instead of the style's.
    pub border_override: Option<[f32; 4]>,
    /// Table cells: the column group, column, row group and row whose backgrounds
    /// paint behind the cell (CSS 2.1 §17.5.1).
    pub cell_bgs: Option<Rc<[Option<BoxId>; 4]>>,
    /// Tables in the collapsing border model: the resolved border segments.
    pub collapsed: Option<Rc<Vec<table::CollapsedSeg>>>,
    /// List items: the marker decided at box-tree time.
    pub list_marker: Option<Rc<gen::MarkerSpec>>,
    /// Fieldsets: the rendered legend and how far the border/background
    /// start below the border-box top (the border is centered on the legend).
    pub legend: Option<(BoxId, f32)>,
    /// Multi-column containers: column geometry (for rules).
    pub multicol: Option<Rc<multicol::MulticolInfo>>,
    /// Row subgrids: their items' (row, span, outer height), for the parent grid.
    pub sub_rows: Option<Rc<Vec<(usize, usize, f32)>>>,
    /// Row subgrids: row sizes imposed by the parent grid.
    pub forced_rows: Option<Rc<Vec<f32>>>,
    /// Vertical writing-mode root (its `writing-mode`; 0 = not one): the
    /// children and line boxes are laid out in a rotated frame (inline axis
    /// horizontal) that painting maps onto the physical content box.
    pub wm: u8,
    pub caption_space: (f32, f32),
}

impl LayoutBox {
    /// Heights of a table's top and bottom captions: they sit outside the
    /// table's border and padding, which paint around the grid only.
    pub fn caption_space(&self) -> (f32, f32) {
        self.caption_space
    }

    pub fn content_x(&self) -> f32 {
        self.border[3] + self.padding[3]
    }
    pub fn content_y(&self) -> f32 {
        self.border[0] + self.padding[0]
    }
    pub fn content_w(&self) -> f32 {
        (self.w - self.border[1] - self.border[3] - self.padding[1] - self.padding[3]).max(0.0)
    }
    pub fn content_h(&self) -> f32 {
        (self.h - self.border[0] - self.border[2] - self.padding[0] - self.padding[2]).max(0.0)
    }
    pub fn outer_w(&self) -> f32 {
        self.w + self.margin[1] + self.margin[3]
    }
    pub fn outer_h(&self) -> f32 {
        self.h + self.margin[0] + self.margin[2]
    }
    pub fn establishes_bfc(&self) -> bool {
        let s = &self.style;
        self.is_float
            || self.out_of_flow
            || self.inline_level
            || s.overflow_x.clips()
            || matches!(s.display, Display::FlowRoot | Display::Flex | Display::InlineFlex | Display::Grid | Display::InlineGrid | Display::Table | Display::InlineTable | Display::TableCell | Display::TableCaption | Display::InlineBlock)
            || self.parent.is_none()
            || s.columns.is_some()
    }
}

impl Layout<'_> {
    /// Whether a box establishes an independent block formatting context
    /// (including flex and grid items, which never collapse margins with their children).
    pub fn is_bfc(&self, id: BoxId) -> bool {
        let b = &self.boxes[id];
        if b.establishes_bfc() {
            return true;
        }
        match b.parent {
            Some(p) => matches!(self.boxes[p].style.display, Display::Flex | Display::InlineFlex | Display::Grid | Display::InlineGrid),
            None => true,
        }
    }
}

/// Information about loaded resources that layout needs.
pub trait ResourceView {
    fn image_for(&self, doc: &Document, node: NodeId) -> Option<Arc<ImageData>>;
    fn image_url(&self, url: &str) -> Option<Arc<ImageData>>;
    /// Rasterize the inline `<svg>` at `node` to `w`×`h`; `styles` supplies CSS
    /// `color`, `fill` and `stroke` for it and its descendants.
    fn svg_for(&self, doc: &Document, node: NodeId, w: f32, h: f32, styles: &Styles) -> Option<Arc<ImageData>>;
    fn canvas_for(&self, _node: NodeId) -> Option<Arc<ImageData>> {
        None
    }
    /// The rendered document of an `<iframe>`.
    fn frame_image(&self, _node: NodeId) -> Option<Arc<ImageData>> {
        None
    }
    /// Whether the image for this element failed to load (vs. still loading).
    fn image_failed(&self, _doc: &Document, _node: NodeId) -> bool {
        false
    }
    /// Whether an <object>'s data loaded as a document (shown as a nested frame).
    fn object_is_document(&self, _doc: &Document, _node: NodeId) -> bool {
        false
    }
}

pub struct LayoutTree {
    pub boxes: Vec<LayoutBox>,
    pub node_box: HashMap<NodeId, BoxId>,
    pub root: BoxId,
    pub doc_width: f32,
    pub doc_height: f32,
    pub viewport: (f32, f32),
    /// Scrollable overflow per box (absolute min x, min y, max x, max y of what
    /// it contributes to its ancestors), computed on first use.
    pub overflow: std::cell::RefCell<Option<Vec<[f32; 4]>>>,
    /// Sticky-positioned boxes (in tree order) with the offset currently applied.
    pub sticky: Vec<(BoxId, (f32, f32))>,
}

/// What a sticky box's offset depends on: its normal-flow border box, its
/// containing block's content box, resolved insets and margins (all in
/// document coordinates with ancestors' sticky offsets applied).
#[derive(Clone, Debug)]
pub struct StickyGeom {
    pub rect: [f32; 4],
    pub cb: [f32; 4],
    pub inset: [Option<f32>; 4],
    pub margin: [f32; 4],
}

impl StickyGeom {
    /// The offset for scrollport `view` (x0, y0, x1, y1).
    pub fn offset(&self, view: [f32; 4]) -> (f32, f32) {
        let [x0, y0, x1, y1] = self.rect;
        let axis = |start: Option<f32>, end: Option<f32>, lo: f32, hi: f32, v0: f32, v1: f32, cb0: f32, cb1: f32, m0: f32, m1: f32| -> f32 {
            let mut d = 0.0f32;
            if let Some(e) = end {
                // A view rectangle shorter than the box moves its end edge.
                let lim = match start {
                    Some(st) => (v1 - e).max(v0 + st + (hi - lo)),
                    None => v1 - e,
                };
                if hi > lim {
                    d = (lim - hi).max(cb0 - (lo - m0)).min(0.0);
                }
            }
            if let Some(st) = start {
                let lim = v0 + st;
                if lo < lim {
                    let up = (lim - lo).min(cb1 - (hi + m1)).max(0.0);
                    if up > 0.0 {
                        d = up;
                    }
                }
            }
            d
        };
        let dy = axis(self.inset[0], self.inset[2], y0, y1, view[1], view[3], self.cb[1], self.cb[3], self.margin[0], self.margin[2]);
        let dx = axis(self.inset[3], self.inset[1], x0, x1, view[0], view[2], self.cb[0], self.cb[2], self.margin[3], self.margin[1]);
        (dx, dy)
    }
}

impl LayoutTree {
    /// The box whose scrollport a sticky box sticks to (None: the viewport),
    /// and whether it is inside fixed-position content (which doesn't scroll).
    pub fn sticky_scroller(&self, b: BoxId, doc: &Document) -> (Option<BoxId>, bool) {
        let html = doc.document_element();
        let body = doc.body();
        let html_visible = html.and_then(|h| self.node_box.get(&h)).map(|&hb| !self.boxes[hb].style.overflow_y.clips() && !self.boxes[hb].style.overflow_x.clips()).unwrap_or(true);
        let mut p = self.boxes[b].parent;
        while let Some(id) = p {
            let bx = &self.boxes[id];
            let st = &bx.style;
            let scroll_ctr = (st.overflow_x.clips() && st.overflow_x != Overflow::Clip) || (st.overflow_y.clips() && st.overflow_y != Overflow::Clip);
            if scroll_ctr && bx.parent.is_some() {
                // The root's (or body's, when the root's is visible) overflow
                // belongs to the viewport.
                let viewport_owner = bx.node.is_some() && (bx.node == html || (html_visible && bx.node == body));
                if !viewport_owner {
                    return (Some(id), false);
                }
            }
            if bx.fixed {
                return (None, true);
            }
            p = bx.parent;
        }
        (None, false)
    }

    /// Geometry for sticky box `b` with its current offset `applied` removed
    /// (percentage insets against a scrollport of `view` size).
    pub fn sticky_geom(&self, b: BoxId, applied: (f32, f32), view: (f32, f32)) -> StickyGeom {
        let bx = &self.boxes[b];
        let (x, y) = (bx.abs_x - applied.0, bx.abs_y - applied.1);
        let cb = match bx.parent {
            Some(p) => {
                let pb = &self.boxes[p];
                let cx = pb.abs_x + pb.border[3] + pb.padding[3];
                let cy = pb.abs_y + pb.border[0] + pb.padding[0];
                let (mut w, mut h) = (pb.content_w(), pb.content_h().max(0.0));
                // A scroll container's content area extends over its
                // scrollable overflow.
                if pb.style.overflow_x.clips() || pb.style.overflow_y.clips() {
                    w = w.max(pb.scroll_w - pb.padding[3] - pb.padding[1]);
                    h = h.max(pb.scroll_h - pb.padding[0] - pb.padding[2]);
                }
                [cx, cy, cx + w, cy + h]
            }
            None => [f32::MIN, f32::MIN, f32::MAX, f32::MAX],
        };
        let res = |v: &Len, basis: f32| if v.is_auto() { None } else { Some(v.resolve(basis)) };
        let ins = &bx.style.inset;
        let inset = [res(&ins[0], view.1), res(&ins[1], view.0), res(&ins[2], view.1), res(&ins[3], view.0)];
        StickyGeom { rect: [x, y, x + bx.w, y + bx.h], cb, inset, margin: bx.margin }
    }

    /// Scrollport of `scroller` (None: the viewport) at the given scroll offsets.
    pub fn sticky_view(&self, scroller: Option<BoxId>, vscroll: (f32, f32), elem_scroll: &HashMap<NodeId, (f32, f32)>) -> [f32; 4] {
        match scroller {
            Some(s) => {
                let sb = &self.boxes[s];
                let (sx, sy) = sb.node.and_then(|n| elem_scroll.get(&n).copied()).unwrap_or((0.0, 0.0));
                let x = sb.abs_x + sb.border[3] + sx;
                let y = sb.abs_y + sb.border[0] + sy;
                [x, y, x + sb.w - sb.border[1] - sb.border[3], y + sb.h - sb.border[0] - sb.border[2]]
            }
            None => [vscroll.0, vscroll.1, vscroll.0 + self.viewport.0, vscroll.1 + self.viewport.1],
        }
    }

    /// The sticky offset currently applied to box `b` (its own and its
    /// sticky ancestors').
    pub fn sticky_shift(&self, b: BoxId) -> (f32, f32) {
        if self.sticky.is_empty() {
            return (0.0, 0.0);
        }
        let (mut dx, mut dy) = (0.0, 0.0);
        let mut cur = Some(b);
        while let Some(id) = cur {
            if let Some(&(_, (x, y))) = self.sticky.iter().find(|(s, _)| *s == id) {
                dx += x;
                dy += y;
            }
            cur = self.boxes[id].parent;
        }
        (dx, dy)
    }

    /// Move box `b` and what it positions by (dx, dy).
    fn shift_subtree(&mut self, b: BoxId, dx: f32, dy: f32) {
        let mut stack = vec![b];
        while let Some(id) = stack.pop() {
            self.boxes[id].abs_x += dx;
            self.boxes[id].abs_y += dy;
            let bx = &self.boxes[id];
            for &c in &bx.children {
                if !self.boxes[c].out_of_flow {
                    stack.push(c);
                }
            }
            let bx = &self.boxes[id];
            for &a in &bx.abs_children {
                if !self.boxes[a].fixed {
                    stack.push(a);
                }
            }
        }
    }

    /// Apply sticky positioning for the current scroll offsets. Returns
    /// whether anything moved.
    pub fn apply_sticky(&mut self, doc: &Document, vscroll: (f32, f32), elem_scroll: &HashMap<NodeId, (f32, f32)>) -> bool {
        let mut moved = false;
        for i in 0..self.sticky.len() {
            let (b, applied) = self.sticky[i];
            let (scroller, in_fixed) = self.sticky_scroller(b, doc);
            let view = self.sticky_view(scroller, if in_fixed { (0.0, 0.0) } else { vscroll }, elem_scroll);
            let geom = self.sticky_geom(b, applied, (view[2] - view[0], view[3] - view[1]));
            let (dx, dy) = geom.offset(view);
            if std::env::var_os("OHB_DEBUG_STICKY").is_some() {
                eprintln!("[sticky] box {b} rect {:?} cb {:?} inset {:?} view {view:?} -> {dy}", geom.rect, geom.cb, geom.inset);
            }
            let (ddx, ddy) = (dx - applied.0, dy - applied.1);
            if ddx.abs() > 0.001 || ddy.abs() > 0.001 {
                self.shift_subtree(b, ddx, ddy);
                self.sticky[i].1 = (dx, dy);
                moved = true;
            }
        }
        if moved {
            *self.overflow.borrow_mut() = None;
        }
        moved
    }

    fn contribution(&self, b: BoxId, out: &mut Vec<[f32; 4]>, done: &mut Vec<bool>) -> [f32; 4] {
        if done[b] {
            return out[b];
        }
        let bx = &self.boxes[b];
        let mut r = [bx.abs_x, bx.abs_y, bx.abs_x + bx.w, bx.abs_y + bx.h];
        // Paint containment clips like overflow: clip, so the contents don't
        // add to ancestors' scrollable overflow either.
        let clips = bx.style.overflow_x.clips() || bx.style.overflow_y.clips() || bx.style.contain & crate::css::style::CONTAIN_PAINT != 0;
        let kids: Vec<BoxId> = bx.children.clone();
        if !clips {
            for c in kids {
                if self.boxes[c].fixed {
                    continue;
                }
                let k = self.contribution(c, out, done);
                r = [r[0].min(k[0]), r[1].min(k[1]), r[2].max(k[2]), r[3].max(k[3])];
            }
            if let Some(ifc) = &bx.ifc {
                let (cx, cy) = (bx.abs_x + bx.content_x(), bx.abs_y + bx.content_y());
                r[2] = r[2].max(cx + ifc.max_line_w);
                r[3] = r[3].max(cy + ifc.height);
            }
        } else {
            for c in kids {
                self.contribution(c, out, done);
            }
        }
        out[b] = r;
        done[b] = true;
        r
    }

    /// The scrollable overflow of box `b` relative to its padding box's
    /// top-left: (width, height) as scrollWidth/scrollHeight report it (the
    /// padding box, descendants' border boxes, and in-flow content plus the
    /// end padding).
    pub fn scroll_extent(&self, b: BoxId) -> (f32, f32) {
        if self.overflow.borrow().is_none() {
            let n = self.boxes.len();
            let mut out = vec![[0.0; 4]; n];
            let mut done = vec![false; n];
            self.contribution(self.root, &mut out, &mut done);
            for i in 0..n {
                if !done[i] {
                    self.contribution(i, &mut out, &mut done);
                }
            }
            *self.overflow.borrow_mut() = Some(out);
        }
        let ov = self.overflow.borrow();
        let ov = ov.as_ref().unwrap();
        let bx = &self.boxes[b];
        let px = bx.abs_x + bx.border[3];
        let py = bx.abs_y + bx.border[0];
        let pw = bx.w - bx.border[1] - bx.border[3];
        let ph = bx.h - bx.border[0] - bx.border[2];
        let (mut mx, mut my) = (px + pw, py + ph);
        let mut minx = px;
        let mut miny = py;
        // A child's block-end margin is inside unless it collapses through.
        let st = &bx.style;
        let bfc = st.overflow_x.clips() || st.overflow_y.clips() || !matches!(st.display, crate::css::style::Display::Block | crate::css::style::Display::ListItem) || st.is_positioned() || st.float != crate::css::style::Float::None;
        let keep_end_margin = bfc || bx.padding[2] > 0.0 || bx.border[2] > 0.0;
        for &c in &bx.children {
            let cb = &self.boxes[c];
            if cb.fixed {
                continue;
            }
            let k = ov[c];
            mx = mx.max(k[2]);
            my = my.max(k[3]);
            minx = minx.min(k[0]);
            miny = miny.min(k[1]);
            if !cb.out_of_flow {
                minx = minx.min(cb.abs_x - cb.margin[3] - bx.padding[3]);
                miny = miny.min(cb.abs_y - cb.margin[0] - bx.padding[0]);
                mx = mx.max(cb.abs_x + cb.w + cb.margin[1] + bx.padding[1]);
                my = my.max(cb.abs_y + cb.h + if keep_end_margin { cb.margin[2] } else { 0.0 } + bx.padding[2]);
            }
        }
        if let Some(ifc) = &bx.ifc {
            mx = mx.max(bx.abs_x + bx.content_x() + ifc.max_line_w + bx.padding[1]);
            my = my.max(bx.abs_y + bx.content_y() + ifc.height + bx.padding[2]);
        }
        // The scroll origin: boxes whose content starts at the right (rtl,
        // vertical-rl, reversed flex axes) or bottom scroll from that edge, so
        // overflow towards the left / top counts instead.
        let vertical = st.is_vertical();
        let mut from_bottom = vertical && st.direction_rtl;
        let mut from_right = (!vertical && st.direction_rtl) || matches!(st.writing_mode, 1 | 3);
        if st.display.is_flex() {
            let main_vertical = st.flex_direction.is_row() == vertical;
            if st.flex_direction.is_reverse() {
                if main_vertical {
                    from_bottom = !from_bottom;
                } else {
                    from_right = !from_right;
                }
            }
            if st.flex_wrap == crate::css::style::FlexWrap::WrapReverse {
                if main_vertical {
                    from_right = !from_right;
                } else {
                    from_bottom = !from_bottom;
                }
            }
        }
        let w = if from_right { px + pw - minx } else { mx - px };
        let h = if from_bottom { py + ph - miny } else { my - py };
        (w.max(pw), h.max(ph))
    }
}

pub struct Layout<'a> {
    pub doc: &'a Document,
    pub styles: &'a Styles,
    pub fonts: &'a mut FontContext,
    pub res: &'a dyn ResourceView,
    pub boxes: Vec<LayoutBox>,
    pub vw: f32,
    pub vh: f32,
    pub node_box: HashMap<NodeId, BoxId>,
    pub depth: u32,
    pub counters: gen::CounterState,
    /// Vertical writing-mode roots whose rotated content is being laid out.
    pub vroots: Vec<BoxId>,
    /// Original styles of text boxes restyled for `::first-line`.
    pub fl_orig: HashMap<BoxId, Rc<ComputedStyle>>,
}

const MAX_BOXES: usize = 400_000;

impl<'a> Layout<'a> {
    pub fn new(doc: &'a Document, styles: &'a Styles, fonts: &'a mut FontContext, res: &'a dyn ResourceView, vw: f32, vh: f32) -> Self {
        Layout { doc, styles, fonts, res, boxes: Vec::new(), vw, vh, node_box: HashMap::new(), depth: 0, counters: gen::CounterState::default(), vroots: Vec::new(), fl_orig: HashMap::new() }
    }

    pub fn new_box(&mut self, node: Option<NodeId>, style: Rc<ComputedStyle>, kind: BoxKind, parent: Option<BoxId>) -> BoxId {
        // Inside vertical writing-mode content, box properties are expressed in
        // the rotated frame the content is laid out in.
        let style = if style.is_vertical() && !matches!(kind, BoxKind::Text(_) | BoxKind::Generated(_) | BoxKind::LineBreak) && parent.map(|p| self.boxes[p].style.is_vertical()).unwrap_or(false) {
            Rc::new(style.to_vertical_frame())
        } else {
            style
        };
        let inline_level = match kind {
            BoxKind::Text(_) | BoxKind::InlineBox | BoxKind::LineBreak | BoxKind::Generated(_) => true,
            _ => style.display.is_inline_level(),
        };
        let is_textual = matches!(kind, BoxKind::Text(_) | BoxKind::Generated(_) | BoxKind::LineBreak);
        let out_of_flow = !is_textual && style.is_out_of_flow() && parent.is_some();
        let is_float = !is_textual && style.float != Float::None && !out_of_flow && parent.is_some();
        self.boxes.push(LayoutBox {
            node,
            style,
            kind,
            children: Vec::new(),
            parent,
            inline_level: inline_level && !out_of_flow && !is_float,
            out_of_flow,
            is_float,
            pseudo: 0,
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            margin: [0.0; 4],
            border: [0.0; 4],
            padding: [0.0; 4],
            ifc: None,
            abs_x: 0.0,
            abs_y: 0.0,
            scroll_w: 0.0,
            scroll_h: 0.0,
            baseline: None,
            marker: None,
            abs_children: Vec::new(),
            static_pos: (0.0, 0.0),
            static_rtl: false,
            intrinsic: Cell::new(None),
            image: None,
            ordinal: 0,
            fixed: false,
            cb_passthrough: false,
            top_strut: None,
            bot_strut: None,
            tbl: 0,
            border_override: None,
            cell_bgs: None,
            collapsed: None,
            list_marker: None,
            cb_size: None,
            legend: None,
            multicol: None,
            sub_rows: None,
            forced_rows: None,
            wm: 0,
            caption_space: (0.0, 0.0),
        });
        let id = self.boxes.len() - 1;
        if let Some(n) = node {
            self.node_box.entry(n).or_insert(id);
        }
        id
    }

    // ---------------------------------------------------------------- building

    fn replaced_kind(&self, node: NodeId) -> Option<ReplacedKind> {
        let el = self.doc.element(node)?;
        if el.ns == Namespace::Svg {
            return if el.name.eq_ignore_ascii_case("svg") { Some(ReplacedKind::Svg) } else { None };
        }
        if el.ns != Namespace::Html {
            return None;
        }
        Some(match el.name.as_str() {
            "img" => ReplacedKind::Image,
            "input" => match el.attr("type").unwrap_or("text").to_ascii_lowercase().as_str() {
                "checkbox" => ReplacedKind::Checkbox,
                "radio" => ReplacedKind::Radio,
                "submit" | "button" | "reset" => ReplacedKind::Button,
                "image" => ReplacedKind::Image,
                "range" => ReplacedKind::Range,
                "hidden" => return None,
                "color" => ReplacedKind::ColorWell,
                "file" => ReplacedKind::File,
                "date" | "time" | "datetime-local" | "month" | "week" => ReplacedKind::DateTime,
                _ => ReplacedKind::TextInput,
            },
            "select" => ReplacedKind::Select,
            "textarea" => ReplacedKind::TextArea,
            // <object>/<embed>: an image resource renders as an image; anything
            // else (unsupported type, failed load, still loading) uses fallback content.
            "object" => {
                if self.res.image_for(self.doc, node).is_some() {
                    ReplacedKind::Image
                } else if self.res.object_is_document(self.doc, node) {
                    ReplacedKind::Frame
                } else {
                    return None;
                }
            }
            "embed" => {
                if self.res.image_for(self.doc, node).is_some() {
                    ReplacedKind::Image
                } else {
                    ReplacedKind::Media
                }
            }
            "video" | "audio" => ReplacedKind::Media,
            "canvas" => ReplacedKind::Canvas,
            "iframe" | "frame" => ReplacedKind::Frame,
            "progress" | "meter" => ReplacedKind::Progress,
            _ => return None,
        })
    }

    /// Build the box tree for the document. Returns the root (viewport) box.
    pub fn build(&mut self) -> BoxId {
        let mut vs = ComputedStyle::initial();
        vs.display = Display::Block;
        let root = self.new_box(None, Rc::new(vs), BoxKind::Block, None);
        self.counters.clear();
        if let Some(html) = self.doc.document_element() {
            self.build_node(html, root);
        }
        // CSS Overflow 3 §3.3: the root element's overflow (else, in HTML, the
        // body's) applies to the viewport; the element itself is then
        // `overflow: visible` (it doesn't clip the page at the window height).
        let unclip = |l: &mut Self, n: Option<NodeId>| -> bool {
            let Some(b) = n.and_then(|n| l.node_box.get(&n).copied()) else { return false };
            let s = &l.boxes[b].style;
            if s.overflow_x == Overflow::Visible && s.overflow_y == Overflow::Visible {
                return false;
            }
            let mut st = (**s).clone();
            st.overflow_x = Overflow::Visible;
            st.overflow_y = Overflow::Visible;
            l.boxes[b].style = Rc::new(st);
            true
        };
        let html = self.doc.document_element();
        if !unclip(self, html) {
            let body = self.doc.body().filter(|&b| self.doc.parent(b) == html);
            unclip(self, body);
        }
        self.build_top_layer(root);
        self.fix_block_children(root);
        root
    }

    /// Top-layer elements (modal dialogs, open popovers) render as children
    /// of the root after everything else, each above its `::backdrop`, with
    /// z-indexes above any author z-index.
    fn build_top_layer(&mut self, root: BoxId) {
        // A subtree laid out on its own (a foreignObject's) has no top layer.
        if self.doc.is_element(self.doc.render_root.get()) {
            return;
        }
        let layer: Vec<NodeId> = self.doc.top_layer.clone();
        for (i, n) in layer.into_iter().enumerate() {
            if !self.doc.is_element(n) || !self.doc.is_connected(n) || self.doc.owner_doc(n) != crate::dom::DOCUMENT {
                continue;
            }
            // Hidden by a display:none ancestor: no box (as in Chrome, the
            // top layer only lifts elements that render).
            if self.doc.ancestors(n).any(|a| self.style_of(a).map(|s| s.display == Display::None).unwrap_or(true)) {
                continue;
            }
            let Some(st) = self.style_of(n) else { continue };
            if st.display == Display::None {
                continue;
            }
            let z = i32::MAX - 4096 + 2 * (i as i32).min(2000);
            if let Some(bs) = self.styles.backdrop.get(&n).cloned() {
                if bs.display != Display::None {
                    let mut b = (*bs).clone();
                    b.z_index = Some(z);
                    if !b.is_out_of_flow() {
                        b.position = crate::css::style::Position::Fixed;
                    }
                    let bx = self.new_box(None, Rc::new(b), BoxKind::Block, Some(root));
                    self.boxes[bx].pseudo = 5;
                    self.boxes[root].children.push(bx);
                }
            }
            let mut s = (*st).clone();
            s.z_index = Some(z + 1);
            if !s.is_out_of_flow() {
                s.position = crate::css::style::Position::Absolute;
            }
            let s = Rc::new(s);
            self.counters_enter(Some(n), &s);
            self.build_element_inner(n, root, s);
            self.counters_leave();
        }
    }

    fn style_of(&self, node: NodeId) -> Option<Rc<ComputedStyle>> {
        self.styles.get(node).cloned()
    }

    fn build_node(&mut self, node: NodeId, parent: BoxId) {
        if self.boxes.len() > MAX_BOXES {
            return;
        }
        self.depth += 1;
        if self.depth > 300 {
            self.depth -= 1;
            return;
        }
        match &self.doc.node(node).data {
            NodeData::Text(t) => {
                if !t.is_empty() {
                    let st = self.boxes[parent].style.clone();
                    let st = if self.boxes[parent].node.is_some() { self.style_of(node).unwrap_or(st) } else { st };
                    let b = self.new_box(Some(node), st, BoxKind::Text(Rc::from(t.as_str())), Some(parent));
                    self.boxes[parent].children.push(b);
                }
            }
            NodeData::Element(_) => self.build_element(node, parent),
            _ => {}
        }
        self.depth -= 1;
    }

    fn build_element(&mut self, node: NodeId, parent: BoxId) {
        let style = match self.style_of(node) {
            Some(s) => s,
            None => return,
        };
        // <noscript> content is raw text when scripting is on; it never renders (as in Chrome).
        if style.display == Display::None || self.doc.is_tag(node, "noscript") {
            return;
        }
        // Top-layer elements are built at the end, under the root.
        if !self.doc.top_layer.is_empty() && self.doc.top_layer.contains(&node) && self.doc.owner_doc(node) == crate::dom::DOCUMENT {
            return;
        }
        self.counters_enter(Some(node), &style);
        self.build_element_inner(node, parent, style);
        self.counters_leave();
    }

    fn build_element_inner(&mut self, node: NodeId, parent: BoxId, style: Rc<ComputedStyle>) {
        if style.display == Display::Contents {
            self.add_pseudo(node, parent, true);
            let kids: Vec<NodeId> = self.doc.flat_children(node).to_vec();
            for c in kids {
                self.build_node(c, parent);
            }
            self.add_pseudo(node, parent, false);
            return;
        }
        let tag = self.doc.tag(node).to_string();
        if tag == "wbr" && self.doc.element(node).map(|e| e.ns == Namespace::Html).unwrap_or(false) {
            // <wbr>: a line-break opportunity.
            let b = self.new_box(None, style, BoxKind::Generated(Rc::from("\u{200b}")), Some(parent));
            self.boxes[parent].children.push(b);
            return;
        }
        if tag == "br" && self.doc.element(node).map(|e| e.ns == Namespace::Html).unwrap_or(false) {
            let b = self.new_box(Some(node), style, BoxKind::LineBreak, Some(parent));
            self.boxes[parent].children.push(b);
            return;
        }
        if let Some(rk) = self.replaced_kind(node) {
            // <img> without a loaded image but with alt text renders the alt text inline.
            if rk == ReplacedKind::Image {
                let img = self.res.image_for(self.doc, node);
                let has_size = !style.width.is_auto() || !style.height.is_auto();
                if img.is_none() && !has_size {
                    if let Some(alt) = self.doc.attr(node, "alt").filter(|a| !a.trim().is_empty()) {
                        let ib = self.new_box(Some(node), style.clone(), BoxKind::InlineBox, Some(parent));
                        self.boxes[ib].inline_level = !style.display.blockify().eq(&style.display) || style.display.is_inline_level();
                        let t = self.new_box(None, style, BoxKind::Generated(Rc::from(alt)), Some(ib));
                        self.boxes[ib].children.push(t);
                        self.boxes[parent].children.push(ib);
                        return;
                    }
                }
                let b = self.new_box(Some(node), style, BoxKind::Replaced(rk), Some(parent));
                self.boxes[b].image = img;
                self.boxes[parent].children.push(b);
                return;
            }
            let is_media = rk == ReplacedKind::Media;
            let b = self.new_box(Some(node), style, BoxKind::Replaced(rk), Some(parent));
            if is_media && tag == "video" {
                // The poster frame stands in for the video.
                self.boxes[b].image = self.doc.attr(node, "poster").and_then(|p| self.doc.resolve_url(p)).and_then(|u| self.res.image_url(u.as_str()));
            }
            self.boxes[parent].children.push(b);
            return;
        }
        // <button> and similar behave as inline-block containers.
        let mut style = style;
        if tag == "button" && style.display == Display::Inline {
            let mut s = (*style).clone();
            s.display = Display::InlineBlock;
            style = Rc::new(s);
        }
        let kind = if style.display == Display::Inline { BoxKind::InlineBox } else { BoxKind::Block };
        let b = self.new_box(Some(node), style.clone(), kind, Some(parent));
        self.boxes[parent].children.push(b);
        if tag == "ruby" && matches!(self.boxes[b].kind, BoxKind::InlineBox) && self.doc.element(node).map(|e| e.ns == Namespace::Html).unwrap_or(false) {
            self.build_ruby(node, b, &style);
            return;
        }
        // List item ordinal and marker.
        if style.display == Display::ListItem {
            self.boxes[b].ordinal = self.counters.value("list-item").clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            self.boxes[b].list_marker = self.make_marker(node, &style).map(Rc::new);
        }
        self.add_pseudo(node, b, true);
        let kids: Vec<NodeId> = self.doc.flat_children(node).to_vec();
        for c in kids {
            self.build_node(c, b);
        }
        self.add_pseudo(node, b, false);
        if !matches!(self.boxes[b].kind, BoxKind::InlineBox) && self.styles.first_letter.contains_key(&node) {
            self.apply_first_letter(b, node);
        }
        // Block-level content inside an inline box (CSS 2.1 §9.2.1.1): the
        // inline is split around it, so the content before the block joins the
        // preceding line and the content after it the following one.
        if matches!(self.boxes[b].kind, BoxKind::InlineBox) && self.has_block_child(b) {
            self.split_block_in_inline(b, parent);
            return;
        }
        if matches!(self.boxes[b].kind, BoxKind::Block) {
            self.fix_block_children(b);
        }
    }

    /// `<ruby>`: each run of base content and the `<rt>` after it become an
    /// inline-block column with the annotation centered above the base.
    fn build_ruby(&mut self, node: NodeId, b: BoxId, style: &Rc<ComputedStyle>) {
        self.add_pseudo(node, b, true);
        let kids: Vec<NodeId> = self.doc.flat_children(node).to_vec();
        let mut base: Vec<NodeId> = Vec::new();
        let anon = |display: Display, center: bool| {
            let mut s = ComputedStyle::inherit(style);
            s.display = display;
            if center {
                s.text_align = TextAlign::Center;
            }
            s.white_space = style.white_space;
            Rc::new(s)
        };
        for c in kids {
            let is_rt = self.doc.is_tag(c, "rt");
            if !is_rt {
                if !self.doc.is_tag(c, "rp") {
                    base.push(c);
                }
                continue;
            }
            let Some(rt_style) = self.style_of(c) else { continue };
            if rt_style.display == Display::None {
                continue;
            }
            let col = self.new_box(None, anon(Display::InlineBlock, true), BoxKind::Block, Some(b));
            self.boxes[b].children.push(col);
            let mut rs = (*rt_style).clone();
            rs.display = Display::Block;
            self.build_element_inner(c, col, Rc::new(rs));
            let bb = self.new_box(None, anon(Display::Block, true), BoxKind::Block, Some(col));
            self.boxes[col].children.push(bb);
            for n in base.drain(..) {
                self.build_node(n, bb);
            }
            self.fix_block_children(bb);
            self.fix_block_children(col);
        }
        for n in base.drain(..) {
            self.build_node(n, b);
        }
        self.add_pseudo(node, b, false);
    }

    /// Replace inline box `b` (last child of `parent`) by its fragments: runs of
    /// its inline content (copies of `b`, with the inline-start decoration on
    /// the first and the inline-end one on the last) and its block-level
    /// children in between. A parent inline then splits the same way.
    fn split_block_in_inline(&mut self, b: BoxId, parent: BoxId) {
        let kids = std::mem::take(&mut self.boxes[b].children);
        // Group into runs of inline content and single blocks.
        let mut groups: Vec<Result<Vec<BoxId>, BoxId>> = Vec::new();
        for c in kids {
            let cb = &self.boxes[c];
            if !cb.inline_level && !cb.out_of_flow && !cb.is_float {
                groups.push(Err(c));
            } else {
                match groups.last_mut() {
                    Some(Ok(run)) => run.push(c),
                    _ => groups.push(Ok(vec![c])),
                }
            }
        }
        let n_runs = groups.iter().filter(|g| g.is_ok()).count();
        let mut pieces = Vec::new();
        let mut run_i = 0;
        let mut used_b = false;
        let base = self.boxes[b].style.clone();
        let node = self.boxes[b].node;
        for g in groups {
            match g {
                Err(block) => {
                    self.boxes[block].parent = Some(parent);
                    pieces.push(block);
                }
                Ok(run) => {
                    let (first, last) = (run_i == 0, run_i + 1 == n_runs);
                    run_i += 1;
                    let mut st = (*base).clone();
                    if !first {
                        st.margin[3] = Len::Px(0.0);
                        st.padding[3] = Len::Px(0.0);
                        st.border_width[3] = 0.0;
                    }
                    if !last {
                        st.margin[1] = Len::Px(0.0);
                        st.padding[1] = Len::Px(0.0);
                        st.border_width[1] = 0.0;
                    }
                    let frag = if !used_b {
                        used_b = true;
                        self.boxes[b].style = Rc::new(st);
                        b
                    } else {
                        let f = self.new_box(node, Rc::new(st), BoxKind::InlineBox, Some(parent));
                        self.boxes[f].pseudo = self.boxes[b].pseudo;
                        f
                    };
                    for &c in &run {
                        self.boxes[c].parent = Some(frag);
                    }
                    self.boxes[frag].children = run;
                    pieces.push(frag);
                }
            }
        }
        if !used_b {
            // Only blocks inside: the element's geometry is its first block's.
            if let (Some(n), Some(&first)) = (node, pieces.first()) {
                if self.node_box.get(&n) == Some(&b) {
                    self.node_box.insert(n, first);
                }
            }
        }
        let pc = &mut self.boxes[parent].children;
        if let Some(pos) = pc.iter().rposition(|&c| c == b) {
            pc.splice(pos..pos + 1, pieces);
        } else {
            pc.extend(pieces);
        }
    }

    fn has_block_child(&self, b: BoxId) -> bool {
        self.boxes[b].children.iter().any(|&c| {
            let cb = &self.boxes[c];
            !cb.inline_level && !cb.out_of_flow && !cb.is_float
        })
    }

    /// `::first-letter`: split the first letter (with surrounding punctuation)
    /// of the block's first text into its own box, floated or inline.
    fn apply_first_letter(&mut self, block: BoxId, node: NodeId) {
        let Some(fl) = self.styles.first_letter.get(&node).cloned() else { return };
        // The first in-flow text, through inline boxes and first block children.
        fn find(l: &Layout, b: BoxId) -> Option<(BoxId, usize)> {
            for (i, &c) in l.boxes[b].children.iter().enumerate() {
                let cb = &l.boxes[c];
                if cb.out_of_flow || cb.is_float {
                    continue;
                }
                match &cb.kind {
                    BoxKind::Text(t) => {
                        if t.chars().any(|ch| !ch.is_whitespace()) {
                            return Some((b, i));
                        }
                    }
                    BoxKind::InlineBox => {
                        if let Some(r) = find(l, c) {
                            return Some(r);
                        }
                    }
                    BoxKind::Block if !cb.inline_level => return find(l, c),
                    BoxKind::LineBreak => {}
                    // Atomic inlines, replaced content and generated text come first.
                    _ => return None,
                }
            }
            None
        }
        let Some((parent, idx)) = find(self, block) else { return };
        let tb = self.boxes[parent].children[idx];
        let BoxKind::Text(text) = self.boxes[tb].kind.clone() else { return };
        let is_punct = |c: char| matches!(c, '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '«' | '»' | '‘' | '’' | '“' | '”' | '„' | '‹' | '›' | '¡' | '¿' | '.' | ',' | ':' | ';' | '!' | '?' | '*' | '-' | '—' | '–');
        let start = text.char_indices().find(|(_, c)| !c.is_whitespace()).map(|(i, _)| i).unwrap_or(0);
        let rest = &text[start..];
        let mut end = start;
        let mut it = unicode_segmentation::UnicodeSegmentation::grapheme_indices(rest, true).peekable();
        // Leading punctuation, the letter, then trailing punctuation.
        while let Some(&(i, g)) = it.peek() {
            if g.chars().all(is_punct) {
                end = start + i + g.len();
                it.next();
            } else {
                break;
            }
        }
        if let Some((i, g)) = it.next() {
            if g.chars().all(char::is_whitespace) {
                return;
            }
            end = start + i + g.len();
        }
        while let Some(&(i, g)) = it.peek() {
            if g.chars().all(is_punct) {
                end = start + i + g.len();
                it.next();
            } else {
                break;
            }
        }
        if end <= start {
            return;
        }
        let (head, letter, tail) = (&text[..start], &text[start..end], &text[end..]);
        let tstyle = self.boxes[tb].style.clone();
        let floated = fl.float != Float::None;
        let lb = self.new_box(None, fl.clone(), if floated { BoxKind::Block } else { BoxKind::InlineBox }, Some(parent));
        self.boxes[lb].pseudo = 4;
        let lt = self.new_box(None, fl.clone(), BoxKind::Generated(Rc::from(letter)), Some(lb));
        self.boxes[lb].children.push(lt);
        let mut new_kids: Vec<BoxId> = Vec::new();
        if !head.is_empty() {
            self.boxes[tb].kind = BoxKind::Text(Rc::from(head));
            new_kids.push(tb);
        }
        new_kids.push(lb);
        if !tail.is_empty() {
            let rest_box = self.new_box(self.boxes[tb].node, tstyle, BoxKind::Text(Rc::from(tail)), Some(parent));
            new_kids.push(rest_box);
        }
        if head.is_empty() {
            // The original text box keeps its node: reuse it for the tail when possible.
            if let Some(&last) = new_kids.last() {
                if last != lb {
                    let tail_text = self.boxes[last].kind.clone();
                    self.boxes[tb].kind = tail_text;
                    let n = new_kids.len();
                    new_kids[n - 1] = tb;
                }
            }
        }
        let kids = &mut self.boxes[parent].children;
        kids.splice(idx..idx + 1, new_kids);
    }

    fn add_pseudo(&mut self, node: NodeId, parent: BoxId, before: bool) {
        let ps = if before { self.styles.before.get(&node) } else { self.styles.after.get(&node) };
        let ps = match ps {
            Some(p) => p.clone(),
            None => return,
        };
        let content = match &ps.content {
            Some(c) => c.clone(),
            None => return,
        };
        // The pseudo-element is a child of its element for counters.
        self.counters_enter(None, &ps);
        let text = self.content_text(&content, Some(node), &ps);
        self.counters_leave();
        let image = content.iter().find_map(|i| if let ContentItem::Url(u) = i { self.res.image_url(u) } else { None });
        let kind = if ps.display == Display::Inline { BoxKind::InlineBox } else { BoxKind::Block };
        let b = self.new_box(None, ps.clone(), kind, Some(parent));
        self.boxes[b].pseudo = if before { 1 } else { 2 };
        self.boxes[b].node = None;
        if let Some(img) = image {
            // An anonymous replaced box: it inherits, but the pseudo-element's
            // own box properties (size, transform, margins…) aren't repeated.
            let mut is = ComputedStyle::inherit(&ps);
            is.display = Display::Inline;
            let r = self.new_box(None, Rc::new(is), BoxKind::Replaced(ReplacedKind::Image), Some(b));
            self.boxes[r].image = Some(img);
            self.boxes[r].inline_level = true;
            self.boxes[b].children.push(r);
        }
        if !text.is_empty() {
            let t = self.new_box(None, ps.clone(), BoxKind::Generated(Rc::from(text.as_str())), Some(b));
            self.boxes[b].children.push(t);
        }
        // Attribute the pseudo box to its element for hit testing.
        self.boxes[b].node = None;
        self.boxes[parent].children.push(b);
        let _ = node;
    }

    /// Ensure a block container has either all block-level or all inline-level
    /// in-flow children, wrapping inline runs in anonymous blocks.
    fn fix_block_children(&mut self, b: BoxId) {
        let disp = self.boxes[b].style.display;
        let kids = self.boxes[b].children.clone();
        if kids.is_empty() {
            return;
        }
        let is_flex_grid = disp.is_flex() || disp.is_grid();
        let is_table = matches!(disp, Display::Table | Display::InlineTable | Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup | Display::TableRow);
        // CSS 2.1 §17.2.1: table parts outside a table (a `display: block`
        // <table>'s rows, say) get an anonymous table around each run.
        let kids = if !is_table && !matches!(disp, Display::TableCell | Display::TableCaption | Display::TableColumnGroup | Display::TableColumn) { self.wrap_misparented_table_parts(b, kids) } else { kids };
        let has_block = kids.iter().any(|&c| {
            let cb = &self.boxes[c];
            !cb.inline_level && !cb.out_of_flow && !cb.is_float
        });
        if !has_block && !is_flex_grid && !is_table {
            return; // pure inline formatting context
        }
        let mut new_kids = Vec::new();
        let mut run: Vec<BoxId> = Vec::new();
        let flush = |this: &mut Self, run: &mut Vec<BoxId>, out: &mut Vec<BoxId>| {
            if run.is_empty() {
                return;
            }
            // Drop whitespace-only runs (unless whitespace is preserved).
            let all_ws = run.iter().all(|&c| match &this.boxes[c].kind {
                BoxKind::Text(t) => t.trim_matches(|ch: char| ch.is_ascii_whitespace()).is_empty() && !this.boxes[c].style.white_space.preserves_newlines(),
                _ => this.boxes[c].out_of_flow,
            });
            if all_ws {
                // keep out-of-flow boxes
                for &c in run.iter() {
                    if this.boxes[c].out_of_flow {
                        out.push(c);
                    }
                }
                run.clear();
                return;
            }
            if run.len() == 1 && (is_flex_grid) && !matches!(this.boxes[run[0]].kind, BoxKind::Text(_) | BoxKind::InlineBox | BoxKind::LineBreak | BoxKind::Generated(_)) {
                // Atomic inline-level child of a flex/grid container is itself an item.
                out.push(run[0]);
                run.clear();
                return;
            }
            let parent_style = this.boxes[b].style.clone();
            let mut st = ComputedStyle::inherit(&parent_style);
            st.display = Display::Block;
            let anon = this.new_box(None, Rc::new(st), BoxKind::Block, Some(b));
            this.boxes[anon].pseudo = 3;
            // An anonymous block wrapper is transparent for percentage heights.
            this.boxes[anon].cb_passthrough = !is_flex_grid;
            for &c in run.iter() {
                this.boxes[c].parent = Some(anon);
            }
            this.boxes[anon].children = std::mem::take(run);
            out.push(anon);
        };
        for c in kids {
            let cb = &self.boxes[c];
            let is_inline = cb.inline_level || ((cb.out_of_flow || cb.is_float) && !is_flex_grid && !is_table);
            let item_atomic = is_flex_grid && !matches!(cb.kind, BoxKind::Text(_) | BoxKind::InlineBox | BoxKind::LineBreak | BoxKind::Generated(_));
            if is_inline && !item_atomic {
                run.push(c);
            } else {
                flush(self, &mut run, &mut new_kids);
                new_kids.push(c);
            }
        }
        flush(self, &mut run, &mut new_kids);
        self.boxes[b].children = new_kids;
        if is_table {
            self.wrap_table_loose(b);
        }
    }

    fn wrap_misparented_table_parts(&mut self, b: BoxId, kids: Vec<BoxId>) -> Vec<BoxId> {
        let is_part = |d: Display| {
            matches!(d, Display::TableCell | Display::TableRow | Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup | Display::TableCaption | Display::TableColumn | Display::TableColumnGroup)
        };
        if !kids.iter().any(|&c| !self.boxes[c].out_of_flow && is_part(self.boxes[c].style.display)) {
            return kids;
        }
        let ws = |this: &Self, c: BoxId| matches!(&this.boxes[c].kind, BoxKind::Text(t) if t.trim_matches(|ch: char| ch.is_ascii_whitespace()).is_empty());
        let mut out = Vec::new();
        let mut run: Vec<BoxId> = Vec::new();
        let mut pending_ws: Vec<BoxId> = Vec::new();
        let flush = |this: &mut Self, run: &mut Vec<BoxId>, out: &mut Vec<BoxId>| {
            if run.is_empty() {
                return;
            }
            let mut st = ComputedStyle::inherit(&this.boxes[b].style);
            st.display = Display::Table;
            let anon = this.new_box(None, Rc::new(st), BoxKind::Block, Some(b));
            this.boxes[anon].pseudo = 3;
            for &c in run.iter() {
                this.boxes[c].parent = Some(anon);
            }
            this.boxes[anon].children = std::mem::take(run);
            this.fix_block_children(anon);
            out.push(anon);
        };
        for c in kids {
            let d = self.boxes[c].style.display;
            if !self.boxes[c].out_of_flow && is_part(d) {
                // Whitespace between table parts is dropped.
                pending_ws.clear();
                run.push(c);
            } else if !run.is_empty() && ws(self, c) {
                pending_ws.push(c);
            } else {
                flush(self, &mut run, &mut out);
                out.append(&mut pending_ws);
                out.push(c);
            }
        }
        flush(self, &mut run, &mut out);
        out.append(&mut pending_ws);
        self.boxes[b].children = out.clone();
        out
    }

    /// CSS 2.1 §17.2.1: consecutive children of a table, row group or row
    /// that aren't table parts are wrapped in one anonymous table cell (the
    /// table code then puts loose cells in anonymous rows).
    fn wrap_table_loose(&mut self, b: BoxId) {
        let disp = self.boxes[b].style.display;
        let is_part = |d: Display| {
            matches!(d, Display::TableCell | Display::TableRow | Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup | Display::TableCaption | Display::TableColumn | Display::TableColumnGroup)
        };
        let kids = self.boxes[b].children.clone();
        if !kids.iter().any(|&c| !self.boxes[c].out_of_flow && !is_part(self.boxes[c].style.display)) {
            return;
        }
        let in_row = disp == Display::TableRow;
        let mut out = Vec::new();
        let mut run: Vec<BoxId> = Vec::new();
        let flush = |this: &mut Self, run: &mut Vec<BoxId>, out: &mut Vec<BoxId>| {
            if run.is_empty() {
                return;
            }
            let mut st = ComputedStyle::inherit(&this.boxes[b].style);
            st.display = Display::TableCell;
            let anon = this.new_box(None, Rc::new(st), BoxKind::Block, Some(b));
            this.boxes[anon].pseudo = 3;
            for &c in run.iter() {
                this.boxes[c].parent = Some(anon);
            }
            this.boxes[anon].children = std::mem::take(run);
            this.fix_block_children(anon);
            out.push(anon);
        };
        for c in kids {
            let (oof, d) = (self.boxes[c].out_of_flow, self.boxes[c].style.display);
            let part = is_part(d) && (!in_row || d == Display::TableCell);
            if oof || part {
                // Out-of-flow boxes stay where they are (between runs).
                if !oof {
                    flush(self, &mut run, &mut out);
                }
                if oof && !run.is_empty() {
                    run.push(c);
                } else {
                    out.push(c);
                }
            } else {
                run.push(c);
            }
        }
        flush(self, &mut run, &mut out);
        self.boxes[b].children = out;
    }

    // ------------------------------------------------------------- utilities

    pub fn resolve_box_metrics(&mut self, id: BoxId, cb_w: f32) {
        let s = self.boxes[id].style.clone();
        let mut border = [0.0; 4];
        let mut padding = [0.0; 4];
        let mut margin = [0.0; 4];
        let inline_box = matches!(self.boxes[id].kind, BoxKind::InlineBox);
        for i in 0..4 {
            border[i] = s.bw(i);
            padding[i] = s.padding[i].resolve(cb_w).max(0.0);
            margin[i] = if s.margin[i].is_auto() { 0.0 } else { s.margin[i].resolve(cb_w) };
        }
        if let Some(o) = self.boxes[id].border_override {
            border = o;
        }
        if inline_box {
            // vertical margins don't apply to inline boxes
            margin[0] = 0.0;
            margin[2] = 0.0;
        }
        // Native checkboxes/radios ignore author padding and borders (like Chrome).
        if matches!(self.boxes[id].kind, BoxKind::Replaced(ReplacedKind::Checkbox) | BoxKind::Replaced(ReplacedKind::Radio)) && !s.appearance_none {
            border = [0.0; 4];
            padding = [0.0; 4];
        }
        let b = &mut self.boxes[id];
        b.border = border;
        b.padding = padding;
        b.margin = margin;
        b.top_strut = None;
        b.bot_strut = None;
    }

    /// Convert a specified width/height to border-box size.
    pub fn to_border_box_w(&self, id: BoxId, content_or_border: f32) -> f32 {
        let b = &self.boxes[id];
        if b.style.box_sizing == BoxSizing::BorderBox {
            content_or_border.max(b.border[1] + b.border[3] + b.padding[1] + b.padding[3])
        } else {
            content_or_border + b.border[1] + b.border[3] + b.padding[1] + b.padding[3]
        }
    }

    pub fn to_border_box_h(&self, id: BoxId, v: f32) -> f32 {
        let b = &self.boxes[id];
        if b.style.box_sizing == BoxSizing::BorderBox {
            v.max(b.border[0] + b.border[2] + b.padding[0] + b.padding[2])
        } else {
            v + b.border[0] + b.border[2] + b.padding[0] + b.padding[2]
        }
    }

    pub fn hpad(&self, id: BoxId) -> f32 {
        let b = &self.boxes[id];
        b.border[1] + b.border[3] + b.padding[1] + b.padding[3]
    }

    pub fn vpad(&self, id: BoxId) -> f32 {
        let b = &self.boxes[id];
        b.border[0] + b.border[2] + b.padding[0] + b.padding[2]
    }

    /// Clamp a border-box width by min/max-width.
    pub fn clamp_w(&self, id: BoxId, w: f32, cb_w: f32) -> f32 {
        let s = &self.boxes[id].style;
        let mut w = w;
        if let Some(mx) = s.max_width.definite(Some(cb_w)) {
            w = w.min(self.to_border_box_w(id, mx));
        }
        if let Some(mn) = s.min_width.definite(Some(cb_w)) {
            w = w.max(self.to_border_box_w(id, mn));
        }
        w.max(self.hpad(id))
    }

    pub fn clamp_h(&self, id: BoxId, h: f32, cb_h: Option<f32>) -> f32 {
        let s = &self.boxes[id].style;
        let mut h = h;
        if let Some(mx) = s.max_height.definite(cb_h) {
            h = h.min(self.to_border_box_h(id, mx));
        }
        if let Some(mn) = s.min_height.definite(cb_h) {
            h = h.max(self.to_border_box_h(id, mn));
        }
        h.max(self.vpad(id))
    }

    /// Specified border-box width if definite.
    pub fn specified_w(&self, id: BoxId, cb_w: Option<f32>) -> Option<f32> {
        let s = &self.boxes[id].style;
        if s.width.is_intrinsic() {
            return None;
        }
        if s.width.is_auto() && !matches!(self.boxes[id].kind, BoxKind::Replaced(_)) {
            // aspect-ratio: an auto width follows a definite height (CSS Sizing 4 §5.1).
            if let (Some(r), Some(h)) = (s.aspect_ratio.filter(|r| *r > 0.0), s.height.definite(None)) {
                let bb_h = self.to_border_box_h(id, h.max(0.0));
                return Some(if s.box_sizing == BoxSizing::BorderBox { bb_h * r } else { (bb_h - self.vpad(id)).max(0.0) * r + self.hpad(id) });
            }
        }
        s.width.definite(cb_w).map(|w| self.to_border_box_w(id, w.max(0.0)))
    }

    /// Content-box height of the parent when it is definite without layout.
    fn parent_def_h(&self, id: BoxId) -> Option<f32> {
        let p = self.boxes[id].parent?;
        self.def_content_h(p, 0)
    }

    /// Content-box height of `id` when definite without layout: a fixed
    /// height, or a percentage of an ancestor chain that ends in one.
    fn def_content_h(&self, id: BoxId, depth: u32) -> Option<f32> {
        let hl = &self.boxes[id].style.height;
        let h = match hl.definite(None) {
            Some(h) => self.to_border_box_h(id, h.max(0.0)),
            None if hl.has_pct() && depth < 12 && !self.boxes[id].out_of_flow => {
                let ph = self.def_content_h(self.boxes[id].parent?, depth + 1)?;
                self.to_border_box_h(id, hl.definite(Some(ph))?.max(0.0))
            }
            None => return None,
        };
        // Definite min/max heights clamp it.
        let s = &self.boxes[id].style;
        let h = match s.max_height.definite(None) {
            Some(mx) => h.min(self.to_border_box_h(id, mx.max(0.0))),
            None => h,
        };
        let h = match s.min_height.definite(None) {
            Some(mn) => h.max(self.to_border_box_h(id, mn.max(0.0))),
            None => h,
        };
        Some((h - self.vpad(id)).max(0.0))
    }

    pub fn specified_h(&self, id: BoxId, cb_h: Option<f32>) -> Option<f32> {
        let s = &self.boxes[id].style;
        s.height.definite(cb_h).map(|h| self.to_border_box_h(id, h.max(0.0)))
    }

    // --------------------------------------------------------- intrinsic size

    /// (min-content, max-content) border-box widths, excluding margins.
    /// Forget the cached intrinsic widths of a box and its descendants.
    pub fn clear_intrinsic(&mut self, id: BoxId) {
        let mut stack = vec![id];
        while let Some(b) = stack.pop() {
            self.boxes[b].intrinsic.set(None);
            stack.extend(self.boxes[b].children.iter().copied());
        }
    }

    pub fn intrinsic_widths(&mut self, id: BoxId) -> (f32, f32) {
        if let Some(v) = self.boxes[id].intrinsic.get() {
            return v;
        }
        self.depth += 1;
        let r = if self.depth > 200 { (0.0, 0.0) } else { self.compute_intrinsic(id) };
        self.depth -= 1;
        self.boxes[id].intrinsic.set(Some(r));
        r
    }

    fn compute_intrinsic(&mut self, id: BoxId) -> (f32, f32) {
        lstat(3);
        self.resolve_box_metrics(id, 0.0);
        let pad = self.hpad(id);
        // Table cells: the column algorithm combines the specified width with the
        // content's min-content width, so report the content here.
        let is_cell = self.boxes[id].style.display == Display::TableCell;
        if let Some(w) = self.specified_w(id, None).filter(|_| !is_cell) {
            if !self.boxes[id].style.width.has_pct() {
                // Percentage min/max can't be resolved here: ignore them.
                let s = &self.boxes[id].style;
                let mut w = w;
                if let Some(mx) = s.max_width.definite(None) {
                    w = w.min(self.to_border_box_w(id, mx));
                }
                if let Some(mn) = s.min_width.definite(None) {
                    w = w.max(self.to_border_box_w(id, mn));
                }
                let w = w.max(pad);
                // Compressible replaced elements (css-sizing-3 §5.2.2): a
                // percentage max-width makes the min-content contribution zero.
                if matches!(self.boxes[id].kind, BoxKind::Replaced(_)) && s.max_width.has_pct() {
                    return (pad, w);
                }
                return (w, w);
            }
        }
        // A vertical writing-mode root is as wide as its stacked lines.
        if self.is_vertical_root(id) {
            let (sx, sy, sw, sh) = { let b = &self.boxes[id]; (b.x, b.y, b.w, b.h) };
            self.layout_vertical_root(id, 0.0, None);
            let w = self.boxes[id].w;
            let b = &mut self.boxes[id];
            (b.x, b.y, b.w, b.h) = (sx, sy, sw, sh);
            return (w, w);
        }
        // Size / inline-size containment: sized as if empty (CSS Contain §3.1).
        let ct = self.boxes[id].style.contain;
        if ct & (CONTAIN_SIZE | CONTAIN_INLINE_SIZE) != 0 {
            let w = self.boxes[id].style.contain_intrinsic.0.unwrap_or(0.0) + pad;
            return (w, w);
        }
        // aspect-ratio with a percentage height against a parent of definite height.
        {
            let s = &self.boxes[id].style;
            if s.width.is_auto() && s.height.has_pct() && !matches!(self.boxes[id].kind, BoxKind::Replaced(_)) {
                if let (Some(r), Some(ph)) = (s.aspect_ratio.filter(|r| *r > 0.0), self.parent_def_h(id)) {
                    if let Some(h) = self.boxes[id].style.height.definite(Some(ph)) {
                        let bb_h = self.to_border_box_h(id, h.max(0.0));
                        let w = if self.boxes[id].style.box_sizing == BoxSizing::BorderBox { bb_h * r } else { (bb_h - self.vpad(id)).max(0.0) * r + pad };
                        return (w, w);
                    }
                }
            }
        }
        let kind = self.boxes[id].kind.clone();
        let (mut mn, mut mx) = match kind {
            BoxKind::Replaced(_) => {
                let ph = if self.boxes[id].style.height.has_pct() { self.parent_def_h(id) } else { None };
                let (w, _) = self.replaced_size(id, None, ph);
                let s = &self.boxes[id].style;
                if s.width.has_pct() || s.max_width.has_pct() {
                    (pad, w)
                } else {
                    (w, w)
                }
            }
            BoxKind::Block => {
                let s = self.boxes[id].style.clone();
                let kids = self.boxes[id].children.clone();
                let has_inline = kids.iter().any(|&c| self.boxes[c].inline_level);
                if has_inline && !s.display.is_flex() && !s.display.is_grid() {
                    let (a, b) = inline::intrinsic_inline(self, id);
                    (a + pad, b + pad)
                } else if matches!(s.display, Display::Table | Display::InlineTable) {
                    let (a, b) = table::table_intrinsic(self, id);
                    (a + pad, b + pad)
                } else if s.display.is_grid() {
                    let (a, b) = grid::grid_intrinsic(self, id);
                    (a + pad, b + pad)
                } else if self.is_multicol(id) {
                    let (a, b) = self.multicol_intrinsic(id);
                    (a + pad, b + pad)
                } else {
                    let row_flex = s.display.is_flex() && s.flex_direction.is_row();
                    let grid = s.display.is_grid();
                    let mut a: f32 = 0.0;
                    let mut b: f32 = 0.0;
                    let gap = s.column_gap.resolve(0.0).max(0.0);
                    let mut n = 0;
                    // Adjacent floats sit side by side: their max-content widths
                    // add up (per side) until a `clear`.
                    let (mut fl, mut fr) = (0.0f32, 0.0f32);
                    for c in kids {
                        if self.boxes[c].out_of_flow {
                            continue;
                        }
                        let (ci, cx) = self.intrinsic_widths(c);
                        let m = self.boxes[c].margin[1] + self.boxes[c].margin[3];
                        let (cf, cc) = (self.boxes[c].style.float, self.boxes[c].style.clear);
                        if !row_flex && !grid {
                            match cc {
                                Clear::Left => fl = 0.0,
                                Clear::Right => fr = 0.0,
                                Clear::Both => {
                                    fl = 0.0;
                                    fr = 0.0;
                                }
                                _ => {}
                            }
                        }
                        if !row_flex && !grid && cf != Float::None {
                            if cf == Float::Left {
                                fl += cx + m;
                            } else {
                                fr += cx + m;
                            }
                            a = a.max(ci + m);
                            b = b.max(fl + fr);
                            continue;
                        }
                        if row_flex {
                            if s.flex_wrap == FlexWrap::NoWrap {
                                a += ci + m;
                            } else {
                                a = a.max(ci + m);
                            }
                            b += cx + m;
                            n += 1;
                        } else if grid {
                            a = a.max(ci + m);
                            b += cx + m;
                            n += 1;
                        } else {
                            a = a.max(ci + m);
                            b = b.max(cx + m);
                        }
                    }
                    if (row_flex || grid) && n > 1 {
                        b += gap * (n - 1) as f32;
                        if row_flex && s.flex_wrap == FlexWrap::NoWrap {
                            a += gap * (n - 1) as f32;
                        }
                    }
                    if grid {
                        // Use explicit column count to estimate max-content.
                        if let Some(cols) = &s.grid_template_columns {
                            let fixed: f32 = cols.iter().map(|t| match t {
                                Track::Len(l) => l.resolve(0.0),
                                _ => 0.0,
                            }).sum();
                            b = b.max(fixed);
                        }
                    }
                    (a + pad, b + pad)
                }
            }
            BoxKind::InlineBox | BoxKind::Text(_) | BoxKind::Generated(_) => {
                let (a, b) = inline::intrinsic_inline(self, id);
                (a, b)
            }
            BoxKind::LineBreak => (0.0, 0.0),
        };
        let s = &self.boxes[id].style;
        if let Some(mxw) = s.max_width.definite(None) {
            let v = self.to_border_box_w(id, mxw);
            mn = mn.min(v);
            mx = mx.min(v);
        }
        if let Some(mnw) = s.min_width.definite(None) {
            let v = self.to_border_box_w(id, mnw);
            mn = mn.max(v);
            mx = mx.max(v);
        }
        (mn, mx.max(mn))
    }

    /// Min-content width of a box's content, ignoring its own `width` (the
    /// flex "content size suggestion"; max-width still clamps it).
    pub fn content_min_width(&mut self, id: BoxId) -> f32 {
        let s = self.boxes[id].style.clone();
        if s.width.is_auto() && s.min_width.is_auto() {
            return self.intrinsic_widths(id).0;
        }
        let mut st = (*s).clone();
        st.width = Len::Auto;
        st.min_width = Len::Auto;
        let saved = self.boxes[id].intrinsic.get();
        self.boxes[id].style = Rc::new(st);
        self.boxes[id].intrinsic.set(None);
        let v = self.intrinsic_widths(id).0;
        self.boxes[id].style = s;
        self.boxes[id].intrinsic.set(saved);
        v
    }

    /// Shrink-to-fit border-box width.
    pub fn shrink_to_fit(&mut self, id: BoxId, avail: f32) -> f32 {
        let (mn, mx) = self.intrinsic_widths(id);
        mx.min(avail.max(mn))
    }

    // ---------------------------------------------------------- replaced size

    /// Natural/used border-box size of a replaced element.
    pub fn replaced_size(&mut self, id: BoxId, cb_w: Option<f32>, cb_h: Option<f32>) -> (f32, f32) {
        let b = &self.boxes[id];
        let s = b.style.clone();
        let kind = match &b.kind {
            BoxKind::Replaced(k) => k.clone(),
            _ => return (0.0, 0.0),
        };
        let node = b.node;
        let font = s.font_size;
        // Intrinsic dimensions.
        let (iw, ih, ratio): (Option<f32>, Option<f32>, Option<f32>) = match kind {
            // Size containment (sizes=auto images): no natural size; contain-intrinsic-size instead.
            ReplacedKind::Image if s.contain & CONTAIN_SIZE != 0 => (Some(s.contain_intrinsic.0.unwrap_or(0.0)), Some(s.contain_intrinsic.1.unwrap_or(0.0)), None),
            ReplacedKind::Image => match &b.image {
                Some(img) => {
                    let (w, h) = img.natural();
                    (Some(w), Some(h), if h > 0.0 { Some(w / h) } else { None })
                }
                None => (None, None, None),
            },
            ReplacedKind::Svg => {
                let doc = self.doc;
                let n = node.unwrap_or(0);
                let wa = doc.attr(n, "width").and_then(|v| parse_svg_len_em(v, s.font_size));
                let ha = doc.attr(n, "height").and_then(|v| parse_svg_len_em(v, s.font_size));
                let vb = doc.attr(n, "viewbox").and_then(|v| {
                    let p: Vec<f32> = v.split(|c: char| c == ',' || c.is_whitespace()).filter_map(|x| x.parse().ok()).collect();
                    if p.len() == 4 && p[2] > 0.0 && p[3] > 0.0 {
                        Some(p[2] / p[3])
                    } else {
                        None
                    }
                });
                match (wa, ha, vb) {
                    (Some(w), Some(h), _) => (Some(w), Some(h), Some(w / h.max(0.001))),
                    (Some(w), None, Some(r)) => (Some(w), Some(w / r), Some(r)),
                    (None, Some(h), Some(r)) => (Some(h * r), Some(h), Some(r)),
                    (None, None, Some(r)) => (None, None, Some(r)),
                    _ => (Some(300.0), Some(150.0), Some(2.0)),
                }
            }
            ReplacedKind::Checkbox | ReplacedKind::Radio => (Some(13.0), Some(13.0), Some(1.0)),
            ReplacedKind::TextInput => {
                let size = node.and_then(|n| self.doc.attr(n, "size")).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| *v > 0.0).unwrap_or(20.0);
                let (avg, max) = self.fonts.primary(&s.font_family, s.font_weight, s.font_style).char_widths(font);
                // Blink (TextFieldIntrinsicInlineSize): size × avg + (max − avg), ceiled.
                let w = (avg * size + (max - avg).max(0.0)).ceil();
                (Some(w), Some(self.used_line_height(&s)), None)
            }
            ReplacedKind::DateTime => {
                // Chrome's datetime edit (monospace fields + picker icon), per em.
                let t = node.and_then(|n| self.doc.attr(n, "type")).unwrap_or("date").to_ascii_lowercase();
                let (kw, kh) = match t.as_str() {
                    "time" => (7.425, 1.5),
                    "datetime-local" => (15.475, 1.3),
                    "month" => (7.075, 1.3),
                    "week" => (10.675, 1.3),
                    _ => (9.025, 1.3),
                };
                let lh = self.used_line_height(&s);
                (Some((kw * font * 64.0).round() / 64.0), Some((kh * font).max(lh)), None)
            }
            ReplacedKind::ColorWell => (Some(44.0), Some(23.0), None),
            ReplacedKind::File => {
                // "Choose File" button + 4px + "No file chosen", at least 34 zeros wide.
                let zero = self.fonts.shape("0", &s.font_family, s.font_weight, s.font_style, font, 0.0).width;
                let bw = self.fonts.shape("Choose File", &s.font_family, s.font_weight, s.font_style, font, 0.0).width + 16.0;
                let label = self.fonts.shape("No file chosen", &s.font_family, s.font_weight, s.font_style, font, 0.0).width;
                let lh = self.used_line_height(&s);
                (Some((zero * 34.0).max(bw + 4.0 + label).ceil()), Some(lh + 6.0), None)
            }
            ReplacedKind::TextArea => {
                let attr_n = |name: &str, d: f32| node.and_then(|n| self.doc.attr(n, name)).and_then(|v| v.trim().parse::<f32>().ok()).filter(|v| *v > 0.0).unwrap_or(d);
                let (cols, rows) = (attr_n("cols", 20.0), attr_n("rows", 2.0));
                let (avg, _) = self.fonts.primary(&s.font_family, s.font_weight, s.font_style).char_widths(font);
                let sb = if matches!(s.overflow_y, Overflow::Auto | Overflow::Scroll) { 15.0 } else { 0.0 };
                (Some((cols * avg).ceil() + sb), Some(rows * self.used_line_height(&s)), None)
            }
            ReplacedKind::Select => {
                // Sized by the widest option (Chrome), plus the menulist's
                // internal padding (4px start, 4px + arrow end, 1px top/bottom),
                // or as a list box showing `size` rows.
                let n = node.unwrap_or(0);
                let opts: Vec<String> = self.doc.descendants(n).filter(|&o| self.doc.is_tag(o, "option")).map(|o| crate::dom::collapse_ws(&self.doc.text_content(o))).collect();
                let mut wmax: f32 = 0.0;
                for o in &opts {
                    wmax = wmax.max(self.fonts.shape_s(o, &s).width);
                }
                let lh = self.used_line_height(&s);
                let multiple = self.doc.attr(n, "multiple").is_some();
                let size = self.doc.attr(n, "size").and_then(|v| v.trim().parse::<i32>().ok()).filter(|v| *v > 0);
                let listbox = multiple || size.map(|v| v > 1).unwrap_or(false);
                if listbox {
                    let rows = size.unwrap_or(4) as f32;
                    (Some(((wmax + 4.0) * 64.0).ceil() / 64.0), Some(rows * (lh + 2.0)), None)
                } else if s.appearance_none {
                    (Some(wmax.ceil()), Some(lh), None)
                } else {
                    (Some(wmax.ceil() + 20.0), Some(lh + 2.0), None)
                }
            }
            ReplacedKind::Button => {
                let label = self.control_label(id);
                let shaped = self.fonts.shape_s(&label, &s);
                (Some((shaped.width * 64.0).ceil() / 64.0), Some(self.used_line_height(&s)), None)
            }
            ReplacedKind::Range => (Some(130.0), Some(16.0), None),
            ReplacedKind::Progress => (Some(160.0), Some(10.0), None),
            ReplacedKind::Media if node.map(|n| self.doc.is_tag(n, "audio")).unwrap_or(false) => (Some(300.0), Some(54.0), None),
            ReplacedKind::Media if b.image.is_some() => {
                let (w, h) = b.image.as_ref().map(|i| i.natural()).unwrap_or((300.0, 150.0));
                (Some(w), Some(h), Some(w / h.max(1.0)))
            }
            ReplacedKind::Media | ReplacedKind::Frame | ReplacedKind::Canvas => {
                let n = node.unwrap_or(0);
                let wa = self.doc.attr(n, "width").and_then(parse_svg_len);
                let ha = self.doc.attr(n, "height").and_then(parse_svg_len);
                (Some(wa.unwrap_or(300.0)), Some(ha.unwrap_or(150.0)), Some(wa.unwrap_or(300.0) / ha.unwrap_or(150.0).max(1.0)))
            }
        };
        // `aspect-ratio: auto <r>` (e.g. from width/height attributes) yields
        // to the natural ratio once the image has one.
        // A CSS ratio that is used (not yielding to a natural one) applies to
        // the box named by box-sizing; a natural ratio to the content box.
        let css_ratio = s.aspect_ratio.is_some() && !(s.aspect_auto && ratio.is_some());
        let ratio = if s.aspect_auto { ratio.or(s.aspect_ratio) } else { s.aspect_ratio.or(ratio) };
        let pad_w = self.hpad(id);
        let pad_h = self.vpad(id);
        let sw = if s.width.is_intrinsic() { None } else { s.width.definite(cb_w) };
        let sh = s.height.definite(cb_h);
        let border_box = s.box_sizing == BoxSizing::BorderBox;
        let to_content_w = |v: f32| if border_box { (v - pad_w).max(0.0) } else { v };
        let to_content_h = |v: f32| if border_box { (v - pad_h).max(0.0) } else { v };
        let bb_ratio = css_ratio && border_box;
        let (mut w, mut h) = match (sw.map(to_content_w), sh.map(to_content_h)) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, ratio.map(|r| if bb_ratio { ((w + pad_w) / r - pad_h).max(0.0) } else { w / r }).or(ih).unwrap_or(150.0)),
            (None, Some(h)) => (ratio.map(|r| if bb_ratio { ((h + pad_h) * r - pad_w).max(0.0) } else { h * r }).or(iw).unwrap_or(300.0), h),
            (None, None) => match (iw, ih) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, ratio.map(|r| w / r).unwrap_or(150.0)),
                (None, Some(h)) => (ratio.map(|r| h * r).unwrap_or(300.0), h),
                (None, None) => match ratio {
                    Some(r) => {
                        let w = cb_w.unwrap_or(300.0);
                        (w, w / r)
                    }
                    None => (0.0, 0.0),
                },
            },
        };
        // min/max constraints (keeping aspect ratio for images when only one side is constrained).
        let maxw = s.max_width.definite(cb_w).map(to_content_w);
        let maxh = s.max_height.definite(cb_h).map(to_content_h);
        let minw = s.min_width.definite(cb_w).map(to_content_w);
        let minh = s.min_height.definite(cb_h).map(to_content_h);
        let keep_ratio = matches!(kind, ReplacedKind::Image | ReplacedKind::Svg | ReplacedKind::Media | ReplacedKind::Canvas) && ratio.is_some();
        if let Some(mx) = maxw {
            if w > mx {
                if keep_ratio && sh.is_none() {
                    h = mx / ratio.unwrap();
                }
                w = mx;
            }
        }
        if let Some(mx) = maxh {
            if h > mx {
                if keep_ratio && sw.is_none() {
                    w = mx * ratio.unwrap();
                }
                h = mx;
            }
        }
        if let Some(mn) = minw {
            w = w.max(mn);
        }
        if let Some(mn) = minh {
            h = h.max(mn);
        }
        (w + pad_w, h + pad_h)
    }

    /// The used line height: `normal` from the primary font's rounded metrics.
    pub fn used_line_height(&mut self, s: &ComputedStyle) -> f32 {
        match s.line_height {
            LineHeight::Normal => {
                let f = self.fonts.primary(&s.font_family, s.font_weight, s.font_style);
                let sc = f.scale(s.font_size);
                (f.ascent * sc).round() + (f.descent * sc).round() + (f.line_gap * sc).round()
            }
            _ => s.line_height_px(),
        }
    }

    pub fn control_label(&self, id: BoxId) -> String {
        let node = match self.boxes[id].node {
            Some(n) => n,
            None => return String::new(),
        };
        let doc = self.doc;
        let el = match doc.element(node) {
            Some(e) => e,
            None => return String::new(),
        };
        match el.name.as_str() {
            "select" => {
                let opts: Vec<NodeId> = doc.descendants(node).filter(|&n| doc.is_tag(n, "option")).collect();
                let sel = opts
                    .iter()
                    .copied()
                    .find(|&o| doc.element(o).map(|e| e.selected.unwrap_or_else(|| e.has_attr("selected"))).unwrap_or(false))
                    .or(opts.first().copied());
                // Width should fit the longest option; label shows the selected one.
                sel.map(|o| crate::dom::collapse_ws(&doc.text_content(o))).unwrap_or_default()
            }
            _ => {
                let t = el.attr("type").unwrap_or("").to_ascii_lowercase();
                if let Some(v) = el.value.clone().or_else(|| el.attr("value").map(|s| s.to_string())) {
                    if !v.is_empty() || el.has_attr("value") {
                        return v;
                    }
                }
                match t.as_str() {
                    "submit" => "Submit".into(),
                    "reset" => "Reset".into(),
                    "file" => "Choose File".into(),
                    "color" => "      ".into(),
                    _ => String::new(),
                }
            }
        }
    }

    // ------------------------------------------------------------------ entry

    /// Lay out any box given its containing block. Sets w/h and margins; the
    /// caller positions it.
    pub fn layout_box(&mut self, id: BoxId, cb_w: f32, cb_h: Option<f32>, fctx: Option<&mut block::FloatCtx>, foff: (f32, f32)) {
        lstat(0);
        self.depth += 1;
        if self.depth > 250 {
            self.depth -= 1;
            return;
        }
        let kind = self.boxes[id].kind.clone();
        if self.is_vertical_root(id) {
            self.layout_vertical_root(id, cb_w, cb_h);
            self.depth -= 1;
            return;
        }
        match kind {
            BoxKind::Replaced(_) => {
                self.resolve_box_metrics(id, cb_w);
                let (w, h) = self.replaced_size(id, Some(cb_w), cb_h);
                let b = &mut self.boxes[id];
                b.w = w;
                b.h = h;
                // auto margins centering for block-level replaced elements
                if !b.inline_level && !b.is_float && !b.out_of_flow {
                    let s = b.style.clone();
                    let free = cb_w - w - b.margin[1] - b.margin[3];
                    if s.margin[1].is_auto() && s.margin[3].is_auto() {
                        b.margin[3] = (free / 2.0).max(0.0);
                        b.margin[1] = b.margin[3];
                    } else if s.margin[3].is_auto() {
                        b.margin[3] = free.max(0.0);
                    }
                }
            }
            BoxKind::Block => {
                let disp = self.boxes[id].style.display;
                if disp.is_flex() {
                    flex::layout_flex(self, id, cb_w, cb_h);
                } else if disp.is_grid() {
                    grid::layout_grid(self, id, cb_w, cb_h);
                } else if matches!(disp, Display::Table | Display::InlineTable) {
                    table::layout_table(self, id, cb_w, cb_h);
                } else if self.is_multicol(id) {
                    self.layout_multicol(id, cb_w, cb_h);
                } else {
                    self.layout_block(id, cb_w, cb_h, fctx, foff);
                }
            }
            _ => {}
        }
        self.depth -= 1;
    }

    /// A block container with a vertical writing mode inside horizontal
    /// content: an orthogonal flow root laid out in a rotated frame.
    pub fn is_vertical_root(&self, id: BoxId) -> bool {
        let b = &self.boxes[id];
        if !b.style.is_vertical() || !matches!(b.kind, BoxKind::Block) || self.vroots.contains(&id) {
            return false;
        }
        match b.parent {
            Some(p) => !self.boxes[p].style.is_vertical() && self.boxes[p].parent.is_some(),
            None => false,
        }
    }

    /// Lay out a vertical writing-mode root: its own box stays physical; its
    /// content is laid out horizontally with the physical content height as
    /// the line length, then the frame is rotated onto the content box
    /// (the box's width is the stacked lines' extent).
    fn layout_vertical_root(&mut self, id: BoxId, cb_w: f32, cb_h: Option<f32>) {
        let phys = self.boxes[id].style.clone();
        self.resolve_box_metrics(id, cb_w);
        let (border, padding, margin) = {
            let b = &self.boxes[id];
            (b.border, b.padding, b.margin)
        };
        let (hpad, vpad) = (self.hpad(id), self.vpad(id));
        let spec_w = self.specified_w(id, Some(cb_w)).map(|w| self.clamp_w(id, w, cb_w));
        let spec_h = self.specified_h(id, cb_h).map(|h| self.clamp_h(id, h, cb_h));
        // The line length: the content height, else what the containing
        // block (or the viewport) offers.
        let avail = match spec_h {
            Some(h) => (h - vpad).max(0.0),
            None => {
                let mut a = (cb_h.unwrap_or(self.vh) - margin[0] - margin[2] - vpad).max(0.0);
                if let Some(mx) = phys.max_height.definite(cb_h) {
                    a = a.min((self.to_border_box_h(id, mx) - vpad).max(0.0));
                }
                a
            }
        };
        let block_def = spec_w.map(|w| (w - hpad).max(0.0));
        let mut ls = (*phys).clone();
        for i in 0..4 {
            ls.margin[i] = Len::zero();
            ls.padding[i] = Len::zero();
            ls.border_width[i] = 0.0;
        }
        ls.min_width = Len::Auto;
        ls.max_width = Len::None;
        ls.min_height = Len::Auto;
        ls.max_height = Len::None;
        ls.box_sizing = BoxSizing::ContentBox;
        ls.height = block_def.map(Len::Px).unwrap_or(Len::Auto);
        ls.width = Len::Auto;
        ls.aspect_ratio = None;
        self.vroots.push(id);
        self.boxes[id].style = Rc::new(ls.clone());
        self.boxes[id].intrinsic.set(None);
        let inline = if spec_h.is_some() {
            avail
        } else {
            let (mn, mx) = self.intrinsic_widths(id);
            let mut v = mx.min(avail.max(mn));
            if let Some(mn) = phys.min_height.definite(cb_h) {
                v = v.max(self.to_border_box_h(id, mn) - vpad);
            }
            v
        };
        ls.width = Len::Px(inline);
        self.boxes[id].style = Rc::new(ls);
        self.boxes[id].intrinsic.set(None);
        let disp = phys.display;
        if disp.is_flex() {
            flex::layout_flex(self, id, inline, block_def);
        } else if disp.is_grid() {
            grid::layout_grid(self, id, inline, block_def);
        } else if matches!(disp, Display::Table | Display::InlineTable) {
            table::layout_table(self, id, inline, block_def);
        } else if self.is_multicol(id) {
            self.layout_multicol(id, inline, block_def);
        } else {
            self.layout_block(id, inline, block_def, None, (0.0, 0.0));
        }
        self.vroots.pop();
        let (lw, lh) = (self.boxes[id].w, self.boxes[id].h);
        self.boxes[id].style = phys.clone();
        self.boxes[id].intrinsic.set(None);
        {
            let b = &mut self.boxes[id];
            b.border = border;
            b.padding = padding;
            b.margin = margin;
        }
        let pw = spec_w.unwrap_or_else(|| self.clamp_w(id, lh + hpad, cb_w));
        let ph = spec_h.unwrap_or_else(|| self.clamp_h(id, lw + vpad, cb_h));
        // Block-level auto margins center the box.
        let b = &self.boxes[id];
        if !b.inline_level && !b.is_float && !b.out_of_flow {
            let free = cb_w - pw - margin[1] - margin[3];
            if phys.margin[1].is_auto() && phys.margin[3].is_auto() {
                self.boxes[id].margin[3] = (free / 2.0).max(0.0);
                self.boxes[id].margin[1] = self.boxes[id].margin[3];
            } else if phys.margin[3].is_auto() {
                self.boxes[id].margin[3] = free.max(0.0);
            } else if phys.margin[1].is_auto() {
                self.boxes[id].margin[1] = free.max(0.0);
            }
        }
        // The rotated frame's origin is the physical content box's corner.
        let (ox, oy) = (border[3] + padding[3], border[0] + padding[0]);
        // Block-level children and floats are placed relative to this box;
        // atomic inlines are placed relative to it by the inline layout.
        let mut moved: Vec<BoxId> = self.boxes[id].children.iter().copied().filter(|&c| !self.boxes[c].out_of_flow && !self.boxes[c].inline_level).collect();
        if let Some(ifc) = &self.boxes[id].ifc {
            for line in &ifc.lines {
                for f in &line.frags {
                    if matches!(f.kind, inline::FragKind::Atomic) {
                        moved.push(f.box_id);
                    }
                }
            }
        }
        for c in moved {
            self.boxes[c].x += ox;
            self.boxes[c].y += oy;
        }
        let b = &mut self.boxes[id];
        b.w = pw;
        b.h = ph;
        b.wm = phys.writing_mode;
        b.baseline = None;
        b.scroll_w = pw;
        b.scroll_h = ph;
    }

    /// Lay out absolutely positioned descendants registered on `cb`.
    pub fn layout_abs_children(&mut self, cb: BoxId) {
        // Laying out a positioned box can register more (a fixed box inside
        // an absolutely positioned one joins the viewport's list): repeat
        // until none are new.
        let mut done: Vec<BoxId> = Vec::new();
        let mut seen: std::collections::HashSet<BoxId> = std::collections::HashSet::new();
        for _ in 0..64 {
            let list = std::mem::take(&mut self.boxes[cb].abs_children);
            let fresh: Vec<BoxId> = list.into_iter().filter(|a| seen.insert(*a)).collect();
            if fresh.is_empty() {
                break;
            }
            for &a in &fresh {
                self.layout_abs(a, cb);
            }
            done.extend(fresh);
        }
        self.boxes[cb].abs_children = done;
    }

    fn layout_abs(&mut self, id: BoxId, cb: BoxId) {
        let (cbw, cbh, cb_off_x, cb_off_y) = if self.boxes[id].fixed {
            (self.vw, self.vh, 0.0, 0.0)
        } else {
            let c = &self.boxes[cb];
            // containing block is the padding box
            (c.w - c.border[1] - c.border[3], c.h - c.border[0] - c.border[2], c.border[3], c.border[0])
        };
        let s = self.boxes[id].style.clone();
        self.resolve_box_metrics(id, cbw);
        // Static position relative to the containing block.
        let (mut sx, mut sy) = self.boxes[id].static_pos;
        let mut p = self.boxes[id].parent;
        while let Some(pp) = p {
            if pp == cb {
                break;
            }
            sx += self.boxes[pp].x;
            sy += self.boxes[pp].y;
            p = self.boxes[pp].parent;
            if self.boxes.get(pp).map(|b| b.out_of_flow).unwrap_or(false) {
                break;
            }
        }
        if self.boxes[id].fixed {
            // static position of fixed boxes is approximated via their abs coords later
        }
        let left = s.inset[3].definite(Some(cbw));
        let right = s.inset[1].definite(Some(cbw));
        let top = s.inset[0].definite(Some(cbh));
        let bottom = s.inset[2].definite(Some(cbh));
        let (ml, mr) = (self.boxes[id].margin[3], self.boxes[id].margin[1]);
        let (mt, mb) = (self.boxes[id].margin[0], self.boxes[id].margin[2]);
        // Width.
        let is_replaced = matches!(self.boxes[id].kind, BoxKind::Replaced(_));
        let spec_w = if is_replaced { Some(self.replaced_size(id, Some(cbw), Some(cbh)).0) } else { self.specified_w(id, Some(cbw)) };
        let avail_lr = (cbw - left.unwrap_or(0.0) - right.unwrap_or(0.0) - ml - mr).max(0.0);
        // top+bottom make an auto height definite: expose it while measuring
        // the content so percentage heights (and aspect-ratio) resolve.
        let pre_style = if spec_w.is_none() && s.height.is_auto() && !is_replaced && top.is_some() && bottom.is_some() && self.boxes[id].children.iter().any(|&c| self.boxes[c].style.height.has_pct()) {
            let h = self.clamp_h(id, (cbh - top.unwrap() - bottom.unwrap() - mt - mb).max(0.0), Some(cbh));
            let mut st = (*s).clone();
            st.height = Len::Px(if s.box_sizing == BoxSizing::BorderBox { h } else { (h - self.vpad(id)).max(0.0) });
            let o = self.boxes[id].style.clone();
            self.boxes[id].style = Rc::new(st);
            self.boxes[id].intrinsic.set(None);
            for i in 0..self.boxes[id].children.len() {
                let c = self.boxes[id].children[i];
                self.boxes[c].intrinsic.set(None);
            }
            Some(o)
        } else {
            None
        };
        let w = match (spec_w, left, right) {
            (Some(w), _, _) => w,
            // Intrinsic widths win over the left/right stretch.
            (None, _, _) if matches!(s.width, Len::MinContent) => self.intrinsic_widths(id).0,
            (None, _, _) if matches!(s.width, Len::MaxContent) => self.intrinsic_widths(id).1,
            (None, _, _) if matches!(s.width, Len::FitContent) => self.shrink_to_fit(id, avail_lr),
            (None, Some(l), Some(r)) => (cbw - l - r - ml - mr).max(0.0),
            (None, _, _) => {
                let avail = (cbw - left.unwrap_or(0.0) - right.unwrap_or(0.0) - ml - mr).max(0.0);
                self.shrink_to_fit(id, avail)
            }
        };
        if let Some(o) = pre_style {
            self.boxes[id].style = o;
        }
        let w = self.clamp_w(id, w, cbw);
        // Lay out content with that width.
        let mut st = (*s).clone();
        st.width = Len::Px(if s.box_sizing == BoxSizing::BorderBox { w } else { (w - self.hpad(id)).max(0.0) });
        // With both top and bottom set, an auto height is definite: lay out
        // with it so flex/grid containers can align and stretch inside it.
        if s.height.is_auto() && !is_replaced {
            if let (Some(t), Some(bm)) = (top, bottom) {
                let h = self.clamp_h(id, (cbh - t - bm - mt - mb).max(0.0), Some(cbh));
                st.height = Len::Px(if s.box_sizing == BoxSizing::BorderBox { h } else { (h - self.vpad(id)).max(0.0) });
            }
        }
        let orig = self.boxes[id].style.clone();
        self.boxes[id].style = Rc::new(st);
        let cb_h_for_child = if top.is_some() && bottom.is_some() && s.height.is_auto() {
            Some((cbh - top.unwrap() - bottom.unwrap() - mt - mb - self.vpad(id)).max(0.0))
        } else {
            Some(cbh)
        };
        self.layout_box(id, cbw, cb_h_for_child.or(Some(cbh)), None, (0.0, 0.0));
        self.boxes[id].style = orig;
        let mut h = self.boxes[id].h;
        if s.height.is_auto() && !is_replaced {
            if let (Some(t), Some(bm)) = (top, bottom) {
                h = (cbh - t - bm - mt - mb).max(0.0);
                h = self.clamp_h(id, h, Some(cbh));
                self.boxes[id].h = h;
            }
        }
        // The static position of an absolutely positioned child of a flex container is
        // where it would sit as the container's sole flex item (css-flexbox-1 §4.1), so
        // justify-content and align-self/align-items move it.
        let (mut fdx, mut fdy) = (0.0, 0.0);
        if let Some(par) = self.boxes[id].parent {
            let ps = self.boxes[par].style.clone();
            if ps.display.is_flex() && (left.is_none() && right.is_none() || top.is_none() && bottom.is_none()) {
                let pcw = (self.boxes[par].w - self.hpad(par)).max(0.0);
                let pch = (self.boxes[par].h - self.vpad(par)).max(0.0);
                let row = ps.flex_direction.is_row();
                let (ow, oh) = (w + ml + mr, h + mt + mb);
                let (main_free, cross_free) = if row { (pcw - ow, pch - oh) } else { (pch - oh, pcw - ow) };
                let jf = match ps.justify_content {
                    Align::Center | Align::SpaceAround | Align::SpaceEvenly => 0.5,
                    Align::End => 1.0,
                    Align::Right if row => 1.0,
                    _ => 0.0,
                };
                let jf = if ps.flex_direction.is_reverse() && !matches!(ps.justify_content, Align::Left | Align::Right) { 1.0 - jf } else { jf };
                let al = if s.align_self == Align::Auto { ps.align_items } else { s.align_self };
                let af = match al {
                    Align::Center => 0.5,
                    Align::End => 1.0,
                    _ => 0.0,
                };
                let af = if ps.flex_wrap == FlexWrap::WrapReverse { 1.0 - af } else { af };
                let (dm, dc) = (main_free * jf, cross_free * af);
                (fdx, fdy) = if row { (dm, dc) } else { (dc, dm) };
            }
        }
        let b = &mut self.boxes[id];
        b.w = w;
        // auto margins centering
        let mut ml2 = ml;
        if let (Some(l), Some(r)) = (left, right) {
            if s.margin[3].is_auto() && s.margin[1].is_auto() {
                let free = cbw - l - r - w;
                ml2 = (free / 2.0).max(0.0);
            } else if s.margin[3].is_auto() {
                // A single auto margin takes all the remaining space.
                ml2 = cbw - l - r - w - mr;
            }
        }
        let mut mt2 = mt;
        if let (Some(t), Some(bm)) = (top, bottom) {
            if s.margin[0].is_auto() && s.margin[2].is_auto() {
                let free = cbh - t - bm - h;
                mt2 = (free / 2.0).max(0.0);
            } else if s.margin[0].is_auto() {
                mt2 = cbh - t - bm - h - mb;
            }
        }
        let x = match (left, right) {
            (Some(l), _) => l + ml2,
            (None, Some(r)) => cbw - r - mr - w,
            (None, None) if b.static_rtl => sx - cb_off_x - mr - w - fdx,
            (None, None) => sx - cb_off_x + ml + fdx,
        };
        let y = match (top, bottom) {
            (Some(t), _) => t + mt2,
            (None, Some(bm)) => cbh - bm - mb - h,
            (None, None) => sy - cb_off_y + mt + fdy,
        };
        b.x = x + cb_off_x;
        b.y = y + cb_off_y;
        if b.fixed && left.is_none() && right.is_none() {
            // keep static x for fixed elements (relative to viewport approximates document)
            b.x = if b.static_rtl { sx - mr - w } else { sx + ml };
        }
    }

    // ----------------------------------------------------------- final pass

    pub fn finish(mut self, root: BoxId) -> LayoutTree {
        self.assign_abs(root, 0.0, 0.0);
        let (mut dw, mut dh) = (self.vw, self.vh);
        // Scrollable overflow of the page: every box not clipped by an
        // ancestor with overflow other than visible — for in-flow boxes any
        // ancestor, for absolutely positioned ones their containing-block
        // chain. The root element and body propagate overflow to the
        // viewport, so they don't clip.
        let n = self.boxes.len();
        let html = self.doc.document_element();
        let body = self.doc.body();
        let self_clip = |b: &LayoutBox| -> u8 {
            if b.node.is_some() && (b.node == html || b.node == body) && b.pseudo == 0 {
                return 0;
            }
            if b.style.contain & crate::css::style::CONTAIN_PAINT != 0 {
                return 3;
            }
            (b.style.overflow_x != Overflow::Visible) as u8 | ((b.style.overflow_y != Overflow::Visible) as u8) << 1
        };
        // Clip bits that apply to a box's in-flow descendants (itself and its ancestors).
        let mut memo: Vec<u8> = vec![255; n];
        let anc_clip = |id: BoxId, boxes: &Vec<LayoutBox>, memo: &mut Vec<u8>| -> u8 {
            let mut chain = Vec::new();
            let mut cur = Some(id);
            let mut base = 0u8;
            while let Some(c) = cur {
                if c == root {
                    break;
                }
                if memo[c] != 255 {
                    base = memo[c];
                    break;
                }
                chain.push(c);
                cur = boxes[c].parent;
            }
            for &c in chain.iter().rev() {
                base |= self_clip(&boxes[c]);
                memo[c] = base;
            }
            if memo[id] == 255 { 0 } else { memo[id] }
        };
        // Boxes inside fixed-position ones move with the viewport: they don't
        // extend the document (boxes come after their parents).
        let mut in_fixed: Vec<u8> = vec![2; n];
        for i in 0..n {
            let mut chain = Vec::new();
            let mut cur = Some(i);
            let mut v = 0u8;
            while let Some(c) = cur {
                if in_fixed[c] != 2 {
                    v = in_fixed[c];
                    break;
                }
                if self.boxes[c].fixed {
                    v = 1;
                    in_fixed[c] = 1;
                    break;
                }
                chain.push(c);
                cur = self.boxes[c].parent;
            }
            for c in chain {
                in_fixed[c] = v;
            }
        }
        let in_fixed: Vec<bool> = in_fixed.into_iter().map(|v| v == 1).collect();
        for i in 0..n {
            let b = &self.boxes[i];
            if i == root || in_fixed[i] || b.style.visibility != Visibility::Visible && b.children.is_empty() {
                continue;
            }
            let clip = match b.parent {
                Some(p) if p != root => {
                    if b.out_of_flow {
                        // The containing block: the nearest abspos-containing ancestor.
                        let mut cb = Some(p);
                        while let Some(c) = cb {
                            if c == root || self.boxes[c].style.is_abs_cb() {
                                break;
                            }
                            cb = self.boxes[c].parent;
                        }
                        match cb {
                            Some(c) if c != root => anc_clip(c, &self.boxes, &mut memo),
                            _ => 0,
                        }
                    } else {
                        anc_clip(p, &self.boxes, &mut memo)
                    }
                }
                _ => 0,
            };
            // The box's extent, after its own transform (centering with
            // `left: 50%; transform: translateX(-50%)` doesn't overflow).
            let (right, bottom) = match b.style.transform.as_ref() {
                Some(ops) => {
                    let origin = (b.style.transform_origin.0.resolve(b.w), b.style.transform_origin.1.resolve(b.h), b.style.transform_origin_z);
                    let m = crate::css::mat4::box_matrix(ops, b.abs_x, b.abs_y, b.w, b.h, origin);
                    let corners = [(b.abs_x, b.abs_y), (b.abs_x + b.w, b.abs_y), (b.abs_x, b.abs_y + b.h), (b.abs_x + b.w, b.abs_y + b.h)];
                    let pts: Vec<(f64, f64)> = if m.is_finite() { corners.iter().filter_map(|&(x, y)| m.project(x as f64, y as f64)).collect() } else { Vec::new() };
                    if pts.len() == 4 {
                        (pts.iter().map(|p| p.0).fold(f64::MIN, f64::max) as f32, pts.iter().map(|p| p.1).fold(f64::MIN, f64::max) as f32)
                    } else {
                        (b.abs_x + b.w, b.abs_y + b.h)
                    }
                }
                None => (b.abs_x + b.w, b.abs_y + b.h),
            };
            if clip & 2 == 0 {
                dh = dh.max(bottom + if b.parent == Some(root) { b.margin[2] } else { 0.0 });
            }
            if clip & 1 == 0 {
                dw = dw.max(right);
            }
        }
        if let Some(&r) = self.boxes[root].children.first() {
            let b = &self.boxes[r];
            dh = dh.max(b.abs_y + b.h + b.margin[2]);
        }
        let sticky = self.boxes.iter().enumerate().filter(|(_, b)| b.style.position == Position::Sticky && b.node.is_some() && b.parent.is_some()).map(|(i, _)| (i, (0.0, 0.0))).collect();
        LayoutTree { boxes: self.boxes, node_box: self.node_box, root, doc_width: dw, doc_height: dh.ceil(), viewport: (self.vw, self.vh), overflow: Default::default(), sticky }
    }

    fn assign_abs(&mut self, id: BoxId, px: f32, py: f32) {
        let mut stack = vec![(id, px, py)];
        while let Some((id, px, py)) = stack.pop() {
            let (mut x, mut y) = (px + self.boxes[id].x, py + self.boxes[id].y);
            // relative positioning offsets (sticky ones depend on scrolling:
            // LayoutTree::apply_sticky)
            let s = self.boxes[id].style.clone();
            if s.position == Position::Relative {
                let (cbw, cbh) = match (self.boxes[id].cb_size, self.boxes[id].parent) {
                    (Some((w, h)), _) => (w, Some(h)),
                    // Percentage top/bottom need a definite containing-block height
                    // (CSS 2.1 §9.4.3: otherwise they compute to auto).
                    (None, Some(p)) => {
                        let ps = &self.boxes[p].style;
                        let item = self.boxes[p].parent.map(|gp| self.boxes[gp].style.display.is_flex() || self.boxes[gp].style.display.is_grid()).unwrap_or(true);
                        let abs_stretch = ps.is_out_of_flow() && !ps.inset[0].is_auto() && !ps.inset[2].is_auto();
                        let definite = !ps.height.is_auto() || item || abs_stretch;
                        (self.boxes[p].content_w(), if definite { Some(self.boxes[p].content_h()) } else { None })
                    }
                    (None, None) => (self.vw, Some(self.vh)),
                };
                if let Some(l) = s.inset[3].definite(Some(cbw)) {
                    x += l;
                } else if let Some(r) = s.inset[1].definite(Some(cbw)) {
                    x -= r;
                }
                if let Some(t) = s.inset[0].definite(cbh) {
                    y += t;
                } else if let Some(b) = s.inset[2].definite(cbh) {
                    y -= b;
                }
            }
            self.boxes[id].abs_x = x;
            self.boxes[id].abs_y = y;
            let b = &self.boxes[id];
            for &c in b.children.iter().rev() {
                if !self.boxes[c].out_of_flow {
                    stack.push((c, x, y));
                }
            }
            for &a in b.abs_children.iter().rev() {
                if self.boxes[a].fixed {
                    stack.push((a, 0.0, 0.0));
                } else {
                    stack.push((a, x, y));
                }
            }
        }
    }
}

pub fn parse_svg_len(s: &str) -> Option<f32> {
    parse_svg_len_em(s, 16.0)
}

/// An SVG length attribute in px, with `em` relative to `em` px.
pub fn parse_svg_len_em(s: &str, em: f32) -> Option<f32> {
    let s = s.trim();
    if s.ends_with('%') {
        return None;
    }
    let num: String = s.chars().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
    let v: f32 = num.parse().ok()?;
    let unit = &s[num.len()..];
    let px = match unit.trim() {
        "" | "px" => v,
        "em" => v * em,
        "pt" => v * 4.0 / 3.0,
        "rem" => v * 16.0,
        "in" => v * 96.0,
        "cm" => v * 96.0 / 2.54,
        "mm" => v * 96.0 / 25.4,
        _ => v,
    };
    if px > 0.0 {
        Some(px)
    } else {
        None
    }
}

thread_local! {
    /// Debug counters (OHB_LAYOUT_STATS): layout_box calls, layout_sized calls, (unused), intrinsic computations.
    pub static LAYOUT_STATS: std::cell::RefCell<[u64; 4]> = const { std::cell::RefCell::new([0; 4]) };
    static LSTATS_ON: bool = std::env::var_os("OHB_LAYOUT_STATS").is_some();
}
#[inline]
pub fn lstat(i: usize) {
    if LSTATS_ON.with(|v| *v) {
        LAYOUT_STATS.with(|s| s.borrow_mut()[i] += 1);
    }
}

/// Run the full layout pipeline for a document.
pub fn layout_document(doc: &Document, styles: &Styles, fonts: &mut FontContext, res: &dyn ResourceView, vw: f32, vh: f32) -> LayoutTree {
    let t0 = std::time::Instant::now();
    let mut l = Layout::new(doc, styles, fonts, res, vw, vh);
    let root = l.build();
    let t_build = t0.elapsed();
    {
        let b = &mut l.boxes[root];
        b.w = vw;
        b.h = vh;
    }
    let kids = l.boxes[root].children.clone();
    let mut fctx = block::FloatCtx::default();
    let mut y = 0.0;
    for c in kids {
        if l.boxes[c].out_of_flow {
            l.boxes[root].abs_children.push(c);
            continue;
        }
        l.layout_box(c, vw, Some(vh), Some(&mut fctx), (0.0, y));
        let b = &mut l.boxes[c];
        b.x = b.margin[3];
        b.y = y + b.margin[0];
        y += b.outer_h();
    }
    l.layout_abs_children(root);
    let t_layout = t0.elapsed();
    let n = l.boxes.len();
    let tree = l.finish(root);
    if LSTATS_ON.with(|v| *v) {
        let st = LAYOUT_STATS.with(|s| std::mem::take(&mut *s.borrow_mut()));
        eprintln!("[layout] boxes={} build={:.1}ms layout={:.1}ms total={:.1}ms layout_box={} sized={} intrinsic={}", n, t_build.as_secs_f64() * 1e3, (t_layout - t_build).as_secs_f64() * 1e3, t0.elapsed().as_secs_f64() * 1e3, st[0], st[1], st[3]);
    }
    tree
}

/// Debug dump of the box tree (`OHB_DUMP_LAYOUT=<file>` writes it on every layout).
pub fn dump(tree: &LayoutTree, doc: &Document) -> String {
    let mut out = String::new();
    fn rec(t: &LayoutTree, doc: &Document, id: BoxId, depth: usize, out: &mut String) {
        if depth > 60 {
            return;
        }
        let b = &t.boxes[id];
        let name = match (&b.kind, b.node) {
            (BoxKind::Text(s), _) => format!("#text {:?}", s.chars().take(30).collect::<String>()),
            (BoxKind::Generated(s), _) => format!("#gen {:?}", s.chars().take(30).collect::<String>()),
            (_, Some(n)) => {
                let e = doc.element(n);
                let mut s = format!("<{}", doc.tag(n));
                if let Some(e) = e {
                    if let Some(i) = &e.id {
                        s.push_str(&format!(" #{i}"));
                    }
                    if !e.classes.is_empty() {
                        s.push_str(&format!(" .{}", e.classes.join(".")));
                    }
                }
                s.push('>');
                s
            }
            (_, None) => if b.pseudo == 3 { "anon".into() } else { format!("pseudo{}", b.pseudo) },
        };
        let kind = match &b.kind {
            BoxKind::Block => format!("{:?}", b.style.display),
            BoxKind::InlineBox => "inline".into(),
            BoxKind::Replaced(k) => format!("{k:?}"),
            BoxKind::LineBreak => "br".into(),
            _ => String::new(),
        };
        let flags = format!("{}{}{}", if b.out_of_flow { " OOF" } else { "" }, if b.is_float { " FLOAT" } else { "" }, if b.inline_level { " IL" } else { "" });
        out.push_str(&format!(
            "{}{} {} w={} [{:.0},{:.0} {:.0}x{:.0}] m{:?}{}\n",
            "  ".repeat(depth),
            name,
            kind,
            b.style.width.to_css(),
            b.abs_x,
            b.abs_y,
            b.w,
            b.h,
            b.margin.map(|v| v.round() as i32),
            flags
        ));
        if let Some(ifc) = &b.ifc {
            for (i, l) in ifc.lines.iter().enumerate().take(6) {
                out.push_str(&format!("{}  line{} y={:.0} h={:.0} w={:.0} frags={}\n", "  ".repeat(depth), i, l.y, l.h, l.width, l.frags.len()));
            }
        }
        for &c in &b.children {
            rec(t, doc, c, depth + 1, out);
        }
    }
    rec(tree, doc, tree.root, 0, &mut out);
    out
}

/// The box whose `perspective` and `transform-style` apply to box `id`: its
/// containing block for out-of-flow boxes, otherwise its parent element's box.
pub fn parent_3d(tree: &LayoutTree, id: BoxId) -> Option<BoxId> {
    let b = &tree.boxes[id];
    let fixed = matches!(b.style.position, crate::css::style::Position::Fixed);
    let abs = b.out_of_flow && matches!(b.style.position, crate::css::style::Position::Absolute | crate::css::style::Position::Fixed);
    let mut cur = b.parent;
    while let Some(p) = cur {
        let pb = &tree.boxes[p];
        if pb.parent.is_none() {
            return None;
        }
        if pb.node.is_some() {
            let ok = if fixed {
                pb.style.is_fixed_cb()
            } else if abs {
                pb.style.is_abs_cb()
            } else {
                true
            };
            if ok {
                return Some(p);
            }
        }
        cur = pb.parent;
    }
    None
}

/// The map `[a, b, c, d, e, f]` from a vertical writing-mode root's rotated
/// frame onto its physical content box, for the root's border box at (x, y).
pub fn wm_matrix(b: &LayoutBox, x: f32, y: f32) -> Option<[f32; 6]> {
    if b.wm == 0 {
        return None;
    }
    let cx = x + b.border[3] + b.padding[3];
    let cy = y + b.border[0] + b.padding[0];
    let cw = (b.w - b.border[1] - b.border[3] - b.padding[1] - b.padding[3]).max(0.0);
    let ch = (b.h - b.border[0] - b.border[2] - b.padding[0] - b.padding[2]).max(0.0);
    Some(match b.wm {
        // vertical-lr: transposed (lines left to right, text downwards)
        2 => [0.0, 1.0, 1.0, 0.0, cx - cy, cy - cx],
        // sideways-lr: rotated counter-clockwise (text upwards)
        4 => [0.0, -1.0, 1.0, 0.0, cx - cy, cy + ch + cx],
        // vertical-rl / sideways-rl: rotated clockwise (lines right to left)
        _ => [0.0, 1.0, -1.0, 0.0, cx + cw + cy, cy - cx],
    })
}

/// The perspective matrix box `p` applies to its children (document coordinates).
pub fn perspective_of(b: &LayoutBox) -> Option<crate::css::mat4::Mat4> {
    let d = b.style.perspective?;
    let px = b.abs_x + b.style.perspective_origin.0.resolve(b.w);
    let py = b.abs_y + b.style.perspective_origin.1.resolve(b.h);
    Some(crate::css::mat4::perspective_matrix(d, px, py))
}

/// The CSS transform of a laid-out box in document coordinates (rotations
/// etc. happen around `transform-origin`), including the perspective of its
/// 3D parent; None if untransformed.
pub fn box_matrix(tree: &LayoutTree, id: BoxId) -> Option<crate::css::mat4::Mat4> {
    let b = &tree.boxes[id];
    let persp = parent_3d(tree, id).and_then(|p| perspective_of(&tree.boxes[p]));
    let t = b.style.transform.as_ref();
    if t.is_none() && persp.is_none() {
        return None;
    }
    let mut m = match t {
        Some(ops) => {
            let origin = (b.style.transform_origin.0.resolve(b.w), b.style.transform_origin.1.resolve(b.h), b.style.transform_origin_z);
            crate::css::mat4::box_matrix(ops, b.abs_x, b.abs_y, b.w, b.h, origin)
        }
        None => crate::css::mat4::Mat4::IDENTITY,
    };
    if let Some(p) = persp {
        m = p.mul(&m);
    }
    (m.is_finite() && !m.is_identity()).then_some(m)
}
