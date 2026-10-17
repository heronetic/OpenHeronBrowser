// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Computed style model and per-property value application.

use super::tokenizer::{split_commas, trim, words, Tok, CV};
use super::values::*;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
    ListItem,
    Table,
    InlineTable,
    TableRowGroup,
    TableHeaderGroup,
    TableFooterGroup,
    TableRow,
    TableCell,
    TableCaption,
    TableColumn,
    TableColumnGroup,
    Contents,
    FlowRoot,
}

impl Display {
    pub fn is_inline_level(self) -> bool {
        matches!(self, Display::Inline | Display::InlineBlock | Display::InlineFlex | Display::InlineGrid | Display::InlineTable)
    }
    pub fn is_flex(self) -> bool {
        matches!(self, Display::Flex | Display::InlineFlex)
    }
    pub fn is_grid(self) -> bool {
        matches!(self, Display::Grid | Display::InlineGrid)
    }
    pub fn is_table_part(self) -> bool {
        matches!(
            self,
            Display::TableRowGroup | Display::TableHeaderGroup | Display::TableFooterGroup | Display::TableRow | Display::TableCell | Display::TableCaption | Display::TableColumn | Display::TableColumnGroup
        )
    }
    /// Blockify (for floats, absolutely positioned boxes, flex/grid items).
    pub fn blockify(self) -> Display {
        match self {
            Display::Inline | Display::InlineBlock => Display::Block,
            Display::InlineFlex => Display::Flex,
            Display::InlineGrid => Display::Grid,
            Display::InlineTable => Display::Table,
            d if d.is_table_part() => Display::Block,
            d => d,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Float {
    None,
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Clear {
    None,
    Left,
    Right,
    Both,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BorderStyle {
    None,
    Hidden,
    Solid,
    Dashed,
    Dotted,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
}

impl BorderStyle {
    pub fn visible(self) -> bool {
        !matches!(self, BorderStyle::None | BorderStyle::Hidden)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BoxSizing {
    ContentBox,
    BorderBox,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overflow {
    Visible,
    Hidden,
    Clip,
    Scroll,
    Auto,
}

impl Overflow {
    pub fn clips(self) -> bool {
        !matches!(self, Overflow::Visible)
    }
    pub fn scrollable(self) -> bool {
        matches!(self, Overflow::Scroll | Overflow::Auto)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Visibility {
    Visible,
    Hidden,
    Collapse,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum FontStyle {
    Normal,
    Italic,
    Oblique,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum LineHeight {
    Normal,
    Number(f32),
    Px(f32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextAlign {
    /// `start` / `end`: left or right depending on the direction.
    Start,
    End,
    Left,
    Right,
    Center,
    Justify,
    /// `-webkit-center` (used by `<center>`): centers text and block children.
    WebkitCenter,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WhiteSpace {
    Normal,
    Pre,
    NoWrap,
    PreWrap,
    PreLine,
    BreakSpaces,
}

impl WhiteSpace {
    pub fn collapses_spaces(self) -> bool {
        matches!(self, WhiteSpace::Normal | WhiteSpace::NoWrap | WhiteSpace::PreLine)
    }
    pub fn preserves_newlines(self) -> bool {
        matches!(self, WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine | WhiteSpace::BreakSpaces)
    }
    pub fn wraps(self) -> bool {
        !matches!(self, WhiteSpace::Pre | WhiteSpace::NoWrap)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextTransform {
    None,
    Uppercase,
    Lowercase,
    Capitalize,
    /// MathML `mi`: a single letter becomes its mathematical italic form.
    MathAuto,
}

#[derive(Clone, PartialEq, Debug)]
pub enum VerticalAlign {
    Baseline,
    Top,
    Middle,
    Bottom,
    TextTop,
    TextBottom,
    Sub,
    Super,
    Len(Len),
}

#[derive(Clone, PartialEq, Debug)]
pub enum ListStyleType {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    DecimalLeadingZero,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
    LowerGreek,
    DisclosureClosed,
    DisclosureOpen,
    Str(Rc<str>),
    /// Any other counter style (predefined or `@counter-style`).
    Named(Rc<str>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cursor {
    Auto,
    Default,
    None,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    EResize,
    NResize,
    NeResize,
    NwResize,
    SResize,
    SeResize,
    SwResize,
    WResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ColResize,
    RowResize,
    AllScroll,
    ZoomIn,
    ZoomOut,
}

impl Cursor {
    pub fn as_str(self) -> &'static str {
        match self {
            Cursor::Auto => "auto",
            Cursor::Default => "default",
            Cursor::None => "none",
            Cursor::ContextMenu => "context-menu",
            Cursor::Help => "help",
            Cursor::Pointer => "pointer",
            Cursor::Progress => "progress",
            Cursor::Wait => "wait",
            Cursor::Cell => "cell",
            Cursor::Crosshair => "crosshair",
            Cursor::Text => "text",
            Cursor::VerticalText => "vertical-text",
            Cursor::Alias => "alias",
            Cursor::Copy => "copy",
            Cursor::Move => "move",
            Cursor::NoDrop => "no-drop",
            Cursor::NotAllowed => "not-allowed",
            Cursor::Grab => "grab",
            Cursor::Grabbing => "grabbing",
            Cursor::EResize => "e-resize",
            Cursor::NResize => "n-resize",
            Cursor::NeResize => "ne-resize",
            Cursor::NwResize => "nw-resize",
            Cursor::SResize => "s-resize",
            Cursor::SeResize => "se-resize",
            Cursor::SwResize => "sw-resize",
            Cursor::WResize => "w-resize",
            Cursor::EwResize => "ew-resize",
            Cursor::NsResize => "ns-resize",
            Cursor::NeswResize => "nesw-resize",
            Cursor::NwseResize => "nwse-resize",
            Cursor::ColResize => "col-resize",
            Cursor::RowResize => "row-resize",
            Cursor::AllScroll => "all-scroll",
            Cursor::ZoomIn => "zoom-in",
            Cursor::ZoomOut => "zoom-out",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlexDirection {
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

impl FlexDirection {
    pub fn is_row(self) -> bool {
        matches!(self, FlexDirection::Row | FlexDirection::RowReverse)
    }
    pub fn is_reverse(self) -> bool {
        matches!(self, FlexDirection::RowReverse | FlexDirection::ColumnReverse)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FlexWrap {
    NoWrap,
    Wrap,
    WrapReverse,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Auto,
    Normal,
    Stretch,
    Start,
    End,
    Center,
    Baseline,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ObjectFit {
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    pub radial: bool,
    pub repeating: bool,
    pub angle: f32,
    pub stops: Vec<(Color, Option<Len>)>,
    pub circle: bool,
    pub center: (Len, Len),
    pub conic: bool,
    /// Color interpolation space (`in oklab` …); None = sRGB.
    pub space: Option<MixSpace>,
    /// Hue interpolation method for polar spaces (0 shorter, 1 longer, 2 increasing, 3 decreasing).
    pub hue: u8,
    /// Radial ending-shape extent: 0 farthest-corner, 1 closest-side, 2 closest-corner, 3 farthest-side.
    pub extent: u8,
    /// Explicit radial size (`10px`, `40% 20%`); overrides `extent`.
    pub size: Option<(Len, Len)>,
    /// Color hints: (index of the stop the hint precedes, position).
    pub hints: Vec<(usize, Len)>,
}

/// `border-image`: source, slices (number px or %), fill, widths, outsets and
/// repeat styles (0 stretch, 1 repeat, 2 round, 3 space).
#[derive(Clone, Debug, PartialEq)]
pub struct BorderImage {
    pub source: BgImage,
    /// (value, is_percentage) per side: top, right, bottom, left.
    pub slice: [(f32, bool); 4],
    pub fill: bool,
    /// Per side: Some(Len) for lengths/percentages, number multiplier as Px with
    /// `width_mult`, None = auto.
    pub width: [Option<Len>; 4],
    pub width_mult: [Option<f32>; 4],
    pub outset: [f32; 4],
    pub outset_mult: [Option<f32>; 4],
    pub repeat: (u8, u8),
}

impl Default for BorderImage {
    fn default() -> Self {
        BorderImage { source: BgImage::None, slice: [(100.0, true); 4], fill: false, width: [None, None, None, None], width_mult: [Some(1.0); 4], outset: [0.0; 4], outset_mult: [None; 4], repeat: (0, 0) }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum BgImage {
    None,
    Url(Rc<str>),
    Gradient(Rc<Gradient>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum BgSize {
    Auto,
    Cover,
    Contain,
    Explicit(Len, Len),
}

#[derive(Clone, Debug, PartialEq)]
pub struct BgLayer {
    pub image: BgImage,
    pub size: BgSize,
    pub pos: (Len, Len),
    pub repeat: (bool, bool),
    /// Repeat style per axis when repeating: 0 repeat, 1 space, 2 round.
    pub rmode: (u8, u8),
    pub clip_text: bool,
    /// background-attachment: fixed (positioned against the viewport).
    pub fixed: bool,
    /// background-origin / background-clip box: 0 border, 1 padding, 2 content.
    pub origin: u8,
    pub clip: u8,
}

impl Default for BgLayer {
    fn default() -> Self {
        BgLayer { image: BgImage::None, size: BgSize::Auto, pos: (Len::Pct(0.0), Len::Pct(0.0)), repeat: (true, true), rmode: (0, 0), clip_text: false, fixed: false, origin: 1, clip: 0 }
    }
}

fn bg_box(i: &str) -> Option<u8> {
    match i {
        "border-box" => Some(0),
        "padding-box" => Some(1),
        "content-box" => Some(2),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shadow {
    pub inset: bool,
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: ColorValue,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransformOp {
    Translate(Len, Len),
    Scale(f32, f32),
    Rotate(f32),
    Skew(f32, f32),
    Matrix([f32; 6]),
    /// translate3d() / translateZ() (z in px).
    Translate3d(Len, Len, f32),
    Scale3d(f32, f32, f32),
    /// rotate3d(x, y, z, angle) (also rotateX / rotateY).
    Rotate3d(f32, f32, f32, f32),
    /// perspective(d) (None: `perspective(none)`).
    Perspective(Option<f32>),
    /// matrix3d() values in column-major order.
    Matrix3d([f32; 16]),
}

/// One CSS `filter` function. Amounts are fractions (1 = 100%).
#[derive(Clone, Debug, PartialEq)]
pub enum FilterOp {
    Blur(f32),
    Brightness(f32),
    Contrast(f32),
    Grayscale(f32),
    HueRotate(f32),
    Invert(f32),
    Opacity(f32),
    Saturate(f32),
    Sepia(f32),
    DropShadow { x: f32, y: f32, blur: f32, color: ColorValue },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Track {
    Len(Len),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    MinMax(Box<Track>, Box<Track>),
    FitContent(Len),
    Repeat(RepeatCount, Vec<Track>),
    /// Line names (`[a b]`) attached to the line at this position.
    Names(Rc<Vec<String>>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RepeatCount {
    Count(u32),
    AutoFill,
    AutoFit,
}

#[derive(Clone, Debug, PartialEq)]
pub enum GridLine {
    Auto,
    Line(i32),
    Span(u32),
    Name(Rc<str>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ContentItem {
    Str(String),
    Attr(String),
    OpenQuote,
    CloseQuote,
    NoOpenQuote,
    NoCloseQuote,
    Counter(String, ListStyleType),
    Counters(String, String, ListStyleType),
    Url(String),
}

pub const DECO_UNDERLINE: u8 = 1;
pub const DECO_OVERLINE: u8 = 2;
pub const DECO_LINE_THROUGH: u8 = 4;

/// Rectangular clipping from `clip-path: inset()` or the legacy `clip: rect()`.
#[derive(Clone, Debug, PartialEq)]
pub enum ClipShape {
    /// top, right, bottom, left insets from the border box.
    Inset([Len; 4]),
    /// top, right, bottom, left edges (None = auto), relative to the border box origin.
    Rect([Option<Len>; 4]),
    /// A `clip-path` basic shape and its reference box (0 border, 1 padding, 2 content, 3 margin).
    Shape(BasicShape, u8),
}

/// `shape-outside` of a float.
#[derive(Clone, Debug, PartialEq)]
pub enum ShapeOutside {
    /// A basic shape and its reference box (0 border, 1 padding, 2 content, 3 margin).
    Shape(BasicShape, u8),
    /// A reference box alone (with its rounded corners).
    Box(u8),
}

/// A radius of `circle()` / `ellipse()`.
#[derive(Clone, Debug, PartialEq)]
pub enum ShapeRadius {
    Len(Len),
    ClosestSide,
    FarthestSide,
}

/// CSS Shapes basic shapes (`clip-path`).
#[derive(Clone, Debug, PartialEq)]
pub enum BasicShape {
    /// Insets (t r b l) with optional corner radii (tl tr br bl).
    Inset([Len; 4], Option<[(Len, Len); 4]>),
    /// x y width height, with optional radii.
    Xywh([Len; 4], Option<[(Len, Len); 4]>),
    Circle(ShapeRadius, (Len, Len)),
    Ellipse(ShapeRadius, ShapeRadius, (Len, Len)),
    Polygon(Vec<(Len, Len)>, bool),
    /// SVG path data in px, and whether it uses the even-odd rule.
    Path(Rc<str>, bool),
}

/// A declaration kept for the property-value engine (see ComputedStyle::generic).
#[derive(Clone, Debug, PartialEq)]
pub struct GenDecl {
    pub name: &'static str,
    /// The declared value (empty: the initial value).
    pub value: Rc<Vec<CV>>,
    pub em: f32,
    pub lh: f32,
    /// Whether the property (all longhands of a shorthand) is inherited.
    pub inherited: bool,
    /// An animated longhand: its value interpolates between two others.
    pub interp: Option<Rc<GenInterp>>,
}

/// A longhand's value for the property-value engine.
#[derive(Clone, Debug, PartialEq)]
pub enum GenVal {
    /// Declared tokens (empty: the initial value) with the font size and
    /// line height they resolve against.
    Decl(Rc<Vec<CV>>, f32, f32),
    /// Mid-animation.
    Interp(Rc<GenInterp>),
    /// Animated natively (getComputedStyle reads the typed fields).
    Native,
    /// A keyframe value composited onto the underlying value.
    Comp(Rc<GenComp>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenComp {
    pub under: GenVal,
    pub val: GenVal,
    /// `accumulate` (else `add`).
    pub acc: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GenInterp {
    pub a: GenVal,
    pub b: GenVal,
    pub p: f64,
    /// Also animated natively: when the engine can't interpolate, report the native value.
    pub native: bool,
}


/// Custom properties of an element: the ones it sets over its parent's
/// (shared, not copied — pages declare thousands at the root), flattened
/// once the chain gets deep. Equal (base, changes) pairs share one map, so
/// restyles keep pointer-equal maps and compare them cheaply.
pub type CustomChanges = std::collections::HashMap<String, Option<Rc<Vec<CV>>>, rustc_hash::FxBuildHasher>;

#[derive(Debug, Default)]
pub struct CustomMap {
    parent: Option<Rc<CustomMap>>,
    /// This level's values (None: removed here).
    own: CustomChanges,
    depth: u32,
}

thread_local! {
    /// (base map, changed names) -> maps derived that way.
    static CUSTOM_INTERN: std::cell::RefCell<std::collections::HashMap<(usize, String), Vec<Rc<CustomMap>>, rustc_hash::FxBuildHasher>> = Default::default();
}

impl CustomMap {
    pub fn get(&self, name: &str) -> Option<&Rc<Vec<CV>>> {
        let mut cur = self;
        loop {
            if let Some(v) = cur.own.get(name) {
                return v.as_ref();
            }
            match &cur.parent {
                Some(p) => cur = p,
                None => return None,
            }
        }
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// All (name, value) pairs.
    pub fn flatten(&self) -> std::collections::HashMap<String, Rc<Vec<CV>>, rustc_hash::FxBuildHasher> {
        let mut levels: Vec<&CustomMap> = Vec::new();
        let mut cur = Some(self);
        while let Some(c) = cur {
            levels.push(c);
            cur = c.parent.as_deref();
        }
        let mut out: std::collections::HashMap<String, Rc<Vec<CV>>, rustc_hash::FxBuildHasher> = Default::default();
        for l in levels.iter().rev() {
            for (k, v) in &l.own {
                match v {
                    Some(v) => {
                        out.insert(k.clone(), v.clone());
                    }
                    None => {
                        out.remove(k);
                    }
                }
            }
        }
        out
    }

    /// `base` with `changes` applied (None removes a property).
    pub fn derive(base: &Rc<CustomMap>, changes: CustomChanges) -> Rc<CustomMap> {
        if changes.is_empty() {
            return base.clone();
        }
        let empty_base = base.parent.is_none() && base.own.is_empty();
        let mut names: Vec<&String> = changes.keys().collect();
        names.sort_unstable();
        let mut key_names = String::new();
        for n in names {
            key_names.push_str(n);
            key_names.push('\u{1}');
        }
        let key = (if empty_base { 0 } else { Rc::as_ptr(base) as usize }, key_names);
        let hit = CUSTOM_INTERN.with(|c| {
            c.borrow().get(&key).and_then(|list| {
                list.iter()
                    .find(|m| {
                        let same_base = match &m.parent {
                            Some(p) => !empty_base && Rc::ptr_eq(p, base),
                            None => empty_base,
                        };
                        same_base && m.depth == if empty_base { 0 } else { base.depth + 1 } && m.own == changes
                    })
                    .cloned()
            })
        });
        if let Some(m) = hit {
            return m;
        }
        let made = if base.depth >= 12 {
            let mut flat: CustomChanges = base.flatten().into_iter().map(|(k, v)| (k, Some(v))).collect();
            for (k, v) in changes {
                match v {
                    Some(v) => {
                        flat.insert(k, Some(v));
                    }
                    None => {
                        flat.remove(&k);
                    }
                }
            }
            return Rc::new(CustomMap { parent: None, own: flat, depth: 0 });
        } else {
            Rc::new(CustomMap { parent: if empty_base { None } else { Some(base.clone()) }, own: changes, depth: if empty_base { 0 } else { base.depth + 1 } })
        };
        CUSTOM_INTERN.with(|c| {
            let mut c = c.borrow_mut();
            if c.len() > 20_000 {
                c.clear();
            }
            let list = c.entry(key).or_default();
            if list.len() < 8 {
                list.push(made.clone());
            }
        });
        made
    }
}

impl PartialEq for CustomMap {
    fn eq(&self, other: &Self) -> bool {
        if std::ptr::eq(self, other) {
            return true;
        }
        match (&self.parent, &other.parent) {
            (Some(a), Some(b)) if Rc::ptr_eq(a, b) => return self.own == other.own,
            (None, None) if self.depth == 0 && other.depth == 0 => return self.own.iter().all(|(k, v)| v.is_some() == other.own.get(k).map(|o| o.is_some()).unwrap_or(false) && (v.is_none() || other.own.get(k) == Some(v))) && other.own.values().filter(|v| v.is_some()).count() == self.own.values().filter(|v| v.is_some()).count(),
            _ => {}
        }
        self.flatten() == other.flatten()
    }
}

impl Eq for CustomMap {}

#[derive(Clone, Debug, PartialEq)]
pub struct ComputedStyle {
    pub display: Display,
    pub position: Position,
    pub float: Float,
    pub clear: Clear,
    pub inset: [Len; 4], // top right bottom left
    pub width: Len,
    pub height: Len,
    pub min_width: Len,
    pub min_height: Len,
    pub max_width: Len,
    pub max_height: Len,
    pub margin: [Len; 4],
    pub padding: [Len; 4],
    pub border_width: [f32; 4],
    pub border_style: [BorderStyle; 4],
    pub border_color: [ColorValue; 4],
    pub border_radius: [(Len, Len); 4], // tl tr br bl
    pub box_sizing: BoxSizing,
    pub overflow_x: Overflow,
    pub overflow_y: Overflow,
    pub z_index: Option<i32>,
    pub opacity: f32,
    /// mix-blend-mode (0 normal; see `blend_mode`).
    pub mix_blend: u8,
    /// isolation: isolate
    pub isolate: bool,
    /// `resize` other than none (textarea grip).
    pub resize: bool,
    pub border_image: Option<Rc<BorderImage>>,
    /// Used color scheme is dark (`color-scheme: dark`; inherited).
    pub dark_scheme: bool,
    /// background-blend-mode list (per background layer, cycling).
    pub bg_blend: Option<Rc<Vec<u8>>>,
    pub background_color: ColorValue,
    pub background: Option<Rc<Vec<BgLayer>>>,
    pub box_shadow: Option<Rc<Vec<Shadow>>>,
    pub outline_width: f32,
    pub outline_style: BorderStyle,
    pub outline_color: ColorValue,
    pub outline_offset: f32,
    pub transform: Option<Rc<Vec<TransformOp>>>,
    pub transform_origin: (Len, Len),
    /// Third (z) component of transform-origin, in px.
    pub transform_origin_z: f32,
    /// `perspective` (None = none), in px.
    pub perspective: Option<f32>,
    pub perspective_origin: (Len, Len),
    /// `transform-style: preserve-3d`.
    pub preserve_3d: bool,
    /// `backface-visibility: hidden`.
    pub backface_hidden: bool,
    /// `image-rendering: pixelated / crisp-edges` (inherited): nearest-neighbor scaling.
    pub pixelated: bool,
    /// `shape-outside` (floats) and `shape-margin` (px).
    pub shape_outside: Option<Rc<ShapeOutside>>,
    pub shape_margin: Len,
    /// `writing-mode`: 0 horizontal-tb, 1 vertical-rl, 2 vertical-lr,
    /// 3 sideways-rl, 4 sideways-lr (inherited).
    pub writing_mode: u8,
    /// `text-orientation`: 0 mixed, 1 upright, 2 sideways (inherited).
    pub text_orientation: u8,
    /// `text-combine-upright: all`.
    pub text_combine: bool,
    /// `filter` functions (None = no filter).
    pub filter: Option<Rc<Vec<FilterOp>>>,
    /// `backdrop-filter`.
    pub backdrop_filter: Option<Rc<Vec<FilterOp>>>,
    /// Individual transform properties (`translate`, `rotate`, `scale`); they
    /// are composed with `transform` after the cascade (CSS Transforms 2).
    /// `translate`: x, y and z (px).
    pub translate_prop: Option<(Len, Len, f32)>,
    /// `rotate`: axis (x, y, z) and angle in degrees.
    pub rotate_prop: Option<(f32, f32, f32, f32)>,
    /// `scale`: x, y and z factors.
    pub scale_prop: Option<(f32, f32, f32)>,
    pub clip_shape: Option<Rc<ClipShape>>,
    /// `appearance: none` — form controls drop their native look.
    pub appearance_none: bool,
    /// First `mask` layer (used to tint icon masks with the background color).
    pub mask: Option<Rc<BgLayer>>,
    /// Further mask layers (`mask: a, b`), combined with `mask-composite: add`.
    pub mask_more: Option<Rc<Vec<BgLayer>>>,
    /// `-webkit-line-clamp` / `line-clamp`: maximum number of lines.
    pub line_clamp: Option<u32>,
    /// display was the legacy `-webkit-box` / `-webkit-inline-box`.
    pub legacy_box: bool,
    pub aspect_ratio: Option<f32>,
    /// `aspect-ratio: auto <ratio>`: a replaced element's natural ratio wins.
    pub aspect_auto: bool,
    pub object_fit: ObjectFit,
    pub object_position: (Len, Len),
    pub content: Option<Rc<Vec<ContentItem>>>,
    pub counter_reset: Option<Rc<Vec<(Rc<str>, i64)>>>,
    pub counter_increment: Option<Rc<Vec<(Rc<str>, i64)>>>,
    pub counter_set: Option<Rc<Vec<(Rc<str>, i64)>>>,
    /// `quotes`: `None` is `auto`; an empty list is `none`.
    pub quotes: Option<Rc<Vec<(String, String)>>>,
    pub text_decoration_line: u8,
    pub text_decoration_color: ColorValue,
    pub text_decoration_style: BorderStyle,
    pub text_decoration_thickness: Option<f32>,
    pub text_overflow_ellipsis: bool,
    pub vertical_align: VerticalAlign,
    // flex
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    pub justify_content: Align,
    pub align_items: Align,
    pub align_self: Align,
    pub align_content: Align,
    pub justify_items: Align,
    pub justify_self: Align,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Len,
    pub order: i32,
    pub row_gap: Len,
    pub column_gap: Len,
    // grid
    pub grid_template_columns: Option<Rc<Vec<Track>>>,
    pub grid_template_rows: Option<Rc<Vec<Track>>>,
    /// `grid-template-rows: subgrid`: the rows are the parent grid's.
    pub subgrid_rows: bool,
    pub grid_auto_columns: Option<Rc<Vec<Track>>>,
    pub grid_auto_rows: Option<Rc<Vec<Track>>>,
    pub grid_template_areas: Option<Rc<Vec<Vec<String>>>>,
    pub grid_auto_flow_column: bool,
    pub grid_auto_flow_dense: bool,
    pub grid_column: (GridLine, GridLine),
    pub grid_row: (GridLine, GridLine),
    pub columns: Option<u32>,
    /// Multi-column: column-width (px), rule (width, style, color), span/fill/break.
    pub column_width: Option<f32>,
    pub column_rule: (f32, BorderStyle, ColorValue),
    pub column_span_all: bool,
    pub column_fill_auto: bool,
    pub break_inside_avoid: bool,
    // table
    pub table_layout_fixed: bool,
    // ---- inherited ----
    pub color: Color,
    pub font_family: Rc<Vec<String>>,
    pub font_size: f32,
    pub font_weight: u16,
    pub font_style: FontStyle,
    pub font_variant_small_caps: bool,
    /// `font-feature-settings` (tag, value) pairs.
    pub font_feature_settings: Option<Rc<Vec<(u32, u32)>>>,
    /// font-variant-numeric / -ligatures / -position and font-kerning (FV_* bits).
    pub font_variant_bits: u32,
    /// `font-optical-sizing: none`.
    pub font_optical_sizing_none: bool,
    /// `font-variation-settings` (axis tag, value).
    pub font_variation_settings: Option<Rc<Vec<(u32, f32)>>>,
    pub line_height: LineHeight,
    pub text_align: TextAlign,
    pub text_indent: Len,
    pub text_transform: TextTransform,
    pub white_space: WhiteSpace,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub list_style_type: ListStyleType,
    pub list_style_inside: bool,
    pub list_style_image: Option<Rc<str>>,
    pub cursor: Cursor,
    pub visibility: Visibility,
    pub border_collapse: bool,
    pub border_spacing: f32,
    /// Vertical border spacing (the second `border-spacing` value).
    pub border_spacing_v: f32,
    pub empty_cells_hide: bool,
    pub caption_bottom: bool,
    pub break_anywhere: bool,
    /// `text-wrap-style: balance`.
    pub wrap_balance: bool,
    pub text_shadow: Option<Rc<Vec<Shadow>>>,
    pub pointer_events: bool,
    pub user_select: bool,
    pub deco_propagated: u8,
    pub deco_propagated_color: Color,
    pub direction_rtl: bool,
    pub custom: Rc<CustomMap>,
    pub tab_size: f32,
    /// Specified text of color properties whose computed value is not a plain
    /// legacy sRGB color (lab(), color(), color-mix(), currentcolor, …), for
    /// getComputedStyle's full-precision serialization.
    pub color_hp: Option<Rc<Vec<(&'static str, Rc<super::color::Spec>)>>>,
    /// Specified values of non-inherited keyword properties that only
    /// getComputedStyle reads (see PASSTHRU), as serialized text.
    pub passthru: Option<Rc<Vec<(&'static str, Rc<str>)>>>,
    /// Declarations of properties whose getComputedStyle value comes from
    /// the property-value engine (css::pv), in cascade order, with the
    /// font-relative bases they resolve against.
    pub generic: Option<Rc<Vec<GenDecl>>>,
    pub accent_color: Option<Color>,
    pub caret_color: Option<Color>,
    /// SVG `fill` / `stroke` set by CSS (None = not specified, use the markup's own).
    pub fill: Option<SvgPaint>,
    pub stroke: Option<SvgPaint>,
    /// Other SVG presentation properties set by CSS (`stroke-width`, `stroke-dasharray`, …),
    /// as (index into [`SVG_PROPS`], specified value text), sorted by index.
    pub svg_extra: Option<Rc<Vec<(u8, Rc<str>)>>>,
    /// When the font size derives from a keyword (`medium` by default): the
    /// keyword's index and a multiplier from em/% sizes on the way. The used
    /// size then depends on the family: `monospace` alone uses the 13px table,
    /// as in Chromium and Firefox (see [`keyword_font_size`]).
    pub font_kw: Option<(u8, f32)>,
    /// container-type: 0 normal, 1 inline-size, 2 size (not inherited).
    pub container_type: u8,
    pub container_names: Option<Rc<Vec<String>>>,
    /// `contain` bits (CONTAIN_*), including what container-type implies.
    pub contain: u8,
    /// `will-change` names a property that creates a containing block (transform…).
    pub will_change_cb: bool,
    /// `will-change` names a property that makes a Backdrop Root.
    pub will_change_backdrop_root: bool,
    /// `contain-intrinsic-size` (width, height) used with size containment.
    pub contain_intrinsic: (Option<f32>, Option<f32>),
    /// `unicode-bidi: bidi-override | isolate-override` (characters laid out in `direction`).
    pub bidi_override: bool,
    /// `unicode-bidi: isolate | isolate-override | plaintext`.
    pub bidi_isolate: bool,
    /// `animation-*` and `transition-*` (not inherited).
    pub animation: Option<Rc<super::anim::AnimSpec>>,
    pub transition: Option<Rc<super::anim::TransSpec>>,
}

pub const CONTAIN_SIZE: u8 = 1;
pub const CONTAIN_INLINE_SIZE: u8 = 2;
pub const CONTAIN_LAYOUT: u8 = 4;
pub const CONTAIN_PAINT: u8 = 8;
pub const CONTAIN_STYLE: u8 = 16;

/// Chromium's font-size keyword sizes (xx-small .. xxx-large) for the default
/// proportional (16px) and monospace (13px) fonts.
const KW_SIZES: [[f32; 8]; 2] = [[9.0, 10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0], [9.0, 10.0, 12.0, 13.0, 16.0, 20.0, 26.0, 39.0]];

/// The used font size for a keyword-derived size, given the family list.
pub fn keyword_font_size(kw: (u8, f32), family: &[String]) -> f32 {
    let mono = family.len() == 1 && family[0].eq_ignore_ascii_case("monospace");
    KW_SIZES[mono as usize][(kw.0 as usize).min(7)] * kw.1
}

/// A CSS `fill`/`stroke` value for inline SVG.
#[derive(Clone, Debug, PartialEq)]
pub enum SvgPaint {
    None,
    Color(ColorValue),
    /// Anything else (`url(#id) fallback`, `context-fill`), passed to the SVG renderer as written.
    Raw(Rc<str>),
}

impl SvgPaint {
    /// The value as an SVG presentation attribute, with currentColor resolved against `color`.
    pub fn to_attr(&self, color: Color) -> String {
        match self {
            SvgPaint::None => "none".into(),
            SvgPaint::Color(c) => {
                let c = c.resolve(color);
                if c.a == 255 {
                    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
                } else {
                    format!("rgba({},{},{},{:.3})", c.r, c.g, c.b, c.a as f32 / 255.0)
                }
            }
            SvgPaint::Raw(r) => r.to_string(),
        }
    }
}

thread_local! {
    static INITIAL: ComputedStyle = ComputedStyle::build_initial();
}

impl ComputedStyle {
    /// overflow other than visible/clip on either axis.
    pub fn is_scroll_container(&self) -> bool {
        let sc = |o: Overflow| !matches!(o, Overflow::Visible | Overflow::Clip);
        sc(self.overflow_x) || sc(self.overflow_y)
    }
    fn build_initial() -> ComputedStyle {
        ComputedStyle {
            display: Display::Inline,
            position: Position::Static,
            float: Float::None,
            clear: Clear::None,
            inset: [Len::Auto, Len::Auto, Len::Auto, Len::Auto],
            width: Len::Auto,
            height: Len::Auto,
            min_width: Len::Auto,
            min_height: Len::Auto,
            max_width: Len::None,
            max_height: Len::None,
            margin: [Len::zero(), Len::zero(), Len::zero(), Len::zero()],
            padding: [Len::zero(), Len::zero(), Len::zero(), Len::zero()],
            border_width: [3.0; 4],
            border_style: [BorderStyle::None; 4],
            border_color: [ColorValue::CurrentColor; 4],
            border_radius: [(Len::zero(), Len::zero()), (Len::zero(), Len::zero()), (Len::zero(), Len::zero()), (Len::zero(), Len::zero())],
            box_sizing: BoxSizing::ContentBox,
            overflow_x: Overflow::Visible,
            overflow_y: Overflow::Visible,
            z_index: None,
            opacity: 1.0,
            mix_blend: 0,
            isolate: false,
            resize: false,
            border_image: None,
            dark_scheme: false,
            bg_blend: None,
            background_color: ColorValue::Rgba(Color::TRANSPARENT),
            background: None,
            box_shadow: None,
            outline_width: 3.0,
            outline_style: BorderStyle::None,
            outline_color: ColorValue::CurrentColor,
            outline_offset: 0.0,
            transform: None,
            filter: None,
            backdrop_filter: None,
            translate_prop: None,
            rotate_prop: None,
            scale_prop: None,
            transform_origin: (Len::Pct(50.0), Len::Pct(50.0)),
            transform_origin_z: 0.0,
            perspective: None,
            perspective_origin: (Len::Pct(50.0), Len::Pct(50.0)),
            preserve_3d: false,
            backface_hidden: false,
            pixelated: false,
            shape_outside: None,
            shape_margin: Len::zero(),
            writing_mode: 0,
            text_orientation: 0,
            text_combine: false,
            clip_shape: None,
            appearance_none: false,
            mask: None,
            mask_more: None,
            line_clamp: None,
            legacy_box: false,
            aspect_ratio: None,
            aspect_auto: false,
            object_fit: ObjectFit::Fill,
            object_position: (Len::Pct(50.0), Len::Pct(50.0)),
            content: None,
            counter_reset: None,
            counter_increment: None,
            counter_set: None,
            quotes: None,
            text_decoration_line: 0,
            text_decoration_color: ColorValue::CurrentColor,
            text_decoration_style: BorderStyle::Solid,
            text_decoration_thickness: None,
            text_overflow_ellipsis: false,
            vertical_align: VerticalAlign::Baseline,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            justify_content: Align::Normal,
            align_items: Align::Normal,
            align_self: Align::Auto,
            align_content: Align::Normal,
            justify_items: Align::Normal,
            justify_self: Align::Auto,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Len::Auto,
            order: 0,
            row_gap: Len::Auto,
            column_gap: Len::Auto,
            grid_template_columns: None,
            grid_template_rows: None,
            subgrid_rows: false,
            grid_auto_columns: None,
            grid_auto_rows: None,
            grid_template_areas: None,
            grid_auto_flow_column: false,
            grid_auto_flow_dense: false,
            grid_column: (GridLine::Auto, GridLine::Auto),
            grid_row: (GridLine::Auto, GridLine::Auto),
            columns: None,
            column_width: None,
            column_rule: (3.0, BorderStyle::None, ColorValue::CurrentColor),
            column_span_all: false,
            column_fill_auto: false,
            break_inside_avoid: false,
            table_layout_fixed: false,
            color: Color::BLACK,
            font_family: Rc::new(vec!["serif".to_string()]),
            font_size: 16.0,
            font_weight: 400,
            font_style: FontStyle::Normal,
            font_variant_small_caps: false,
            font_feature_settings: None,
            font_variant_bits: 0,
            font_optical_sizing_none: false,
            font_variation_settings: None,
            line_height: LineHeight::Normal,
            text_align: TextAlign::Start,
            text_indent: Len::zero(),
            text_transform: TextTransform::None,
            white_space: WhiteSpace::Normal,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            list_style_type: ListStyleType::Disc,
            list_style_inside: false,
            list_style_image: None,
            cursor: Cursor::Auto,
            visibility: Visibility::Visible,
            border_collapse: false,
            border_spacing: 0.0,
            border_spacing_v: 0.0,
            empty_cells_hide: false,
            caption_bottom: false,
            break_anywhere: false,
            wrap_balance: false,
            text_shadow: None,
            pointer_events: true,
            user_select: true,
            deco_propagated: 0,
            deco_propagated_color: Color::BLACK,
            direction_rtl: false,
            custom: Rc::new(CustomMap::default()),
            tab_size: 8.0,
            color_hp: None,
            passthru: None,
            generic: None,
            accent_color: None,
            caret_color: None,
            fill: None,
            stroke: None,
            svg_extra: None,
            font_kw: Some((3, 1.0)),
            container_type: 0,
            container_names: None,
            contain: 0,
            will_change_cb: false,
            will_change_backdrop_root: false,
            contain_intrinsic: (None, None),
            bidi_override: false,
            bidi_isolate: false,
            animation: None,
            transition: None,
        }
    }

    pub fn initial() -> ComputedStyle {
        INITIAL.with(|i| i.clone())
    }

    /// Start a child style: initial values with inherited properties copied.
    pub fn inherit(parent: &ComputedStyle) -> ComputedStyle {
        let mut s = ComputedStyle::initial();
        s.color = parent.color;
        s.font_family = parent.font_family.clone();
        s.font_size = parent.font_size;
        s.font_weight = parent.font_weight;
        s.font_style = parent.font_style;
        s.font_variant_small_caps = parent.font_variant_small_caps;
        s.font_feature_settings = parent.font_feature_settings.clone();
        s.font_variant_bits = parent.font_variant_bits;
        s.font_optical_sizing_none = parent.font_optical_sizing_none;
        s.font_variation_settings = parent.font_variation_settings.clone();
        s.line_height = parent.line_height;
        s.text_align = parent.text_align;
        s.text_indent = parent.text_indent.clone();
        s.text_transform = parent.text_transform;
        s.white_space = parent.white_space;
        s.letter_spacing = parent.letter_spacing;
        s.word_spacing = parent.word_spacing;
        s.list_style_type = parent.list_style_type.clone();
        s.list_style_inside = parent.list_style_inside;
        s.list_style_image = parent.list_style_image.clone();
        s.cursor = parent.cursor;
        s.visibility = parent.visibility;
        s.border_collapse = parent.border_collapse;
        s.border_spacing = parent.border_spacing;
        s.border_spacing_v = parent.border_spacing_v;
        s.quotes = parent.quotes.clone();
        s.empty_cells_hide = parent.empty_cells_hide;
        s.caption_bottom = parent.caption_bottom;
        s.break_anywhere = parent.break_anywhere;
        s.wrap_balance = parent.wrap_balance;
        s.text_shadow = parent.text_shadow.clone();
        s.pointer_events = parent.pointer_events;
        s.user_select = parent.user_select;
        s.direction_rtl = parent.direction_rtl;
        s.writing_mode = parent.writing_mode;
        s.pixelated = parent.pixelated;
        s.text_orientation = parent.text_orientation;
        s.dark_scheme = parent.dark_scheme;
        s.custom = parent.custom.clone();
        s.tab_size = parent.tab_size;
        if let Some(h) = &parent.color_hp {
            let v: Vec<(&'static str, Rc<super::color::Spec>)> = h.iter().filter(|(n, _)| is_inherited(n)).cloned().collect();
            s.color_hp = if v.is_empty() { None } else { Some(Rc::new(v)) };
        }
        if let Some(g) = &parent.generic {
            if g.iter().all(|d| d.inherited) {
                s.generic = Some(g.clone());
            } else {
                let v: Vec<GenDecl> = g.iter().filter(|d| d.inherited).cloned().collect();
                s.generic = if v.is_empty() { None } else { Some(Rc::new(v)) };
            }
        }
        s.accent_color = parent.accent_color;
        s.caret_color = parent.caret_color;
        s.fill = parent.fill.clone();
        s.stroke = parent.stroke.clone();
        if let Some(x) = &parent.svg_extra {
            if x.iter().all(|(i, _)| SVG_PROPS[*i as usize].1) {
                s.svg_extra = Some(x.clone());
            } else {
                let v: Vec<(u8, Rc<str>)> = x.iter().filter(|(i, _)| SVG_PROPS[*i as usize].1).cloned().collect();
                s.svg_extra = if v.is_empty() { None } else { Some(Rc::new(v)) };
            }
        }
        s.font_kw = parent.font_kw;
        // Text decorations propagate to in-flow descendants.
        s.deco_propagated = parent.deco_propagated | parent.text_decoration_line;
        s.deco_propagated_color = if parent.text_decoration_line != 0 {
            parent.text_decoration_color.resolve(parent.color)
        } else {
            parent.deco_propagated_color
        };
        s
    }

    pub fn line_height_px(&self) -> f32 {
        let v = match self.line_height {
            LineHeight::Normal => self.font_size * 1.2,
            LineHeight::Number(n) => self.font_size * n,
            LineHeight::Px(p) => p,
        };
        // Blink keeps line heights as LayoutUnits (1/64 px, truncated).
        (v * 64.0).trunc() / 64.0
    }

    pub fn border_color_at(&self, i: usize) -> Color {
        self.border_color[i].resolve(self.color)
    }

    pub fn bw(&self, i: usize) -> f32 {
        if self.border_style[i].visible() {
            self.border_width[i]
        } else {
            0.0
        }
    }

    pub fn is_positioned(&self) -> bool {
        self.position != Position::Static
    }

    /// Vertical writing mode (any of the vertical-* / sideways-* values).
    pub fn is_vertical(&self) -> bool {
        self.writing_mode != 0
    }

    /// This style with its physical box properties re-expressed in the
    /// rotated frame a vertical writing mode is laid out in (inline axis
    /// horizontal): sides, corners, width/height and overflow axes move with
    /// the rotation, so painting the frame rotated puts them back in place.
    pub fn to_vertical_frame(&self) -> ComputedStyle {
        let mut s = self.clone();
        // logical side i takes physical side map[i] (sides: top right bottom left)
        let map: [usize; 4] = match self.writing_mode {
            2 => [3, 2, 1, 0], // vertical-lr: transposed
            4 => [3, 0, 1, 2], // sideways-lr: rotated counter-clockwise
            _ => [1, 2, 3, 0], // vertical-rl / sideways-rl: rotated clockwise
        };
        // logical corner i (tl tr br bl) takes physical corner cmap[i]
        let cmap: [usize; 4] = match self.writing_mode {
            2 => [0, 3, 2, 1],
            4 => [3, 0, 1, 2],
            _ => [1, 2, 3, 0],
        };
        for i in 0..4 {
            s.margin[i] = self.margin[map[i]].clone();
            s.padding[i] = self.padding[map[i]].clone();
            s.inset[i] = self.inset[map[i]].clone();
            s.border_width[i] = self.border_width[map[i]];
            s.border_style[i] = self.border_style[map[i]];
            s.border_color[i] = self.border_color[map[i]];
            let (rx, ry) = self.border_radius[cmap[i]].clone();
            s.border_radius[i] = (ry, rx);
        }
        s.width = self.height.clone();
        s.height = self.width.clone();
        s.min_width = self.min_height.clone();
        s.min_height = self.min_width.clone();
        s.max_width = self.max_height.clone();
        s.max_height = self.max_width.clone();
        s.overflow_x = self.overflow_y;
        s.overflow_y = self.overflow_x;
        s
    }

    /// Establishes a containing block for fixed-position descendants
    /// (transforms, filters, will-change, layout/paint containment).
    pub fn is_fixed_cb(&self) -> bool {
        self.transform.is_some() || self.perspective.is_some() || self.preserve_3d || self.filter.is_some() || self.will_change_cb || self.contain & (CONTAIN_LAYOUT | CONTAIN_PAINT) != 0
    }

    /// Establishes a containing block for absolutely positioned descendants.
    pub fn is_abs_cb(&self) -> bool {
        self.position != Position::Static || self.is_fixed_cb()
    }

    pub fn is_out_of_flow(&self) -> bool {
        matches!(self.position, Position::Absolute | Position::Fixed)
    }

}

/// Context for applying declarations.
#[derive(Clone)]
pub struct ApplyCtx {
    pub root_font_size: f32,
    pub vw: f32,
    pub vh: f32,
    /// The root element's line height in px (`rlh`); 0 = derive from the font size.
    pub root_line_height: f32,
    /// Nearest query container's content box (cq* units).
    pub cq: Option<(f32, f32)>,
}

impl ApplyCtx {
    fn units(&self, s: &ComputedStyle) -> UnitCtx {
        let mut u = UnitCtx::new(s.font_size, self.root_font_size, self.vw, self.vh);
        u.cq = self.cq;
        u.font = Some((s.font_family.clone(), s.font_weight, s.font_style != FontStyle::Normal));
        u.line_height = match s.line_height {
            LineHeight::Normal => None,
            _ => Some(s.line_height_px()),
        };
        u.rlh = self.root_line_height;
        u
    }

}

fn ident_lc(v: &[CV]) -> Option<String> {
    let w = words(v);
    if w.len() == 1 {
        w[0].ident().map(|s| s.to_ascii_lowercase())
    } else {
        None
    }
}

/// Whether a value is exactly this keyword (ASCII case-insensitive).
pub fn ident_is(v: &[CV], kw: &str) -> bool {
    ident_lc(v).as_deref() == Some(kw)
}

/// Is this a CSS-wide keyword? Returns the keyword.
pub fn css_wide(v: &[CV]) -> Option<&'static str> {
    match ident_lc(v)?.as_str() {
        "inherit" => Some("inherit"),
        "initial" => Some("initial"),
        "unset" | "revert" | "revert-layer" => Some("unset"),
        _ => None,
    }
}

/// SVG presentation properties OHB passes through to its SVG renderer:
/// (name, inherited).
pub const SVG_PROPS: &[(&str, bool)] = &[
    ("stroke-width", true),
    ("stroke-opacity", true),
    ("stroke-linecap", true),
    ("stroke-linejoin", true),
    ("stroke-miterlimit", true),
    ("stroke-dasharray", true),
    ("stroke-dashoffset", true),
    ("fill-opacity", true),
    ("fill-rule", true),
    ("clip-rule", true),
    ("paint-order", true),
    ("marker", true),
    ("marker-start", true),
    ("marker-mid", true),
    ("marker-end", true),
    ("shape-rendering", true),
    ("text-anchor", true),
    ("dominant-baseline", true),
    ("vector-effect", false),
    ("stop-color", false),
    ("stop-opacity", false),
    ("flood-color", false),
    ("flood-opacity", false),
    ("lighting-color", false),
    ("cx", false),
    ("cy", false),
    ("r", false),
    ("rx", false),
    ("ry", false),
    ("x", false),
    ("y", false),
    ("d", false),
    ("color-interpolation-filters", true),
];

pub fn svg_prop_index(name: &str) -> Option<u8> {
    SVG_PROPS.iter().position(|(n, _)| *n == name).map(|i| i as u8)
}

impl ComputedStyle {
    pub fn svg_value(&self, i: u8) -> Option<&Rc<str>> {
        self.svg_extra.as_ref().and_then(|v| v.iter().find(|(k, _)| *k == i).map(|(_, v)| v))
    }
    pub fn set_svg_value(&mut self, i: u8, val: Option<Rc<str>>) {
        let mut v: Vec<(u8, Rc<str>)> = self.svg_extra.as_ref().map(|v| (**v).clone()).unwrap_or_default();
        v.retain(|(k, _)| *k != i);
        if let Some(val) = val {
            let pos = v.iter().position(|(k, _)| *k > i).unwrap_or(v.len());
            v.insert(pos, (i, val));
        }
        self.svg_extra = if v.is_empty() { None } else { Some(Rc::new(v)) };
    }
}

// font-variant-* bits (ComputedStyle::font_variant_bits).
pub const FV_LNUM: u32 = 1;
pub const FV_ONUM: u32 = 1 << 1;
pub const FV_PNUM: u32 = 1 << 2;
pub const FV_TNUM: u32 = 1 << 3;
pub const FV_FRAC: u32 = 1 << 4;
pub const FV_AFRC: u32 = 1 << 5;
pub const FV_ORDN: u32 = 1 << 6;
pub const FV_ZERO: u32 = 1 << 7;
pub const FV_NUMERIC_MASK: u32 = 0xff;
pub const FV_LIG_NONE: u32 = 1 << 8;
pub const FV_NO_COMMON: u32 = 1 << 9;
pub const FV_COMMON: u32 = 1 << 10;
pub const FV_DLIG: u32 = 1 << 11;
pub const FV_NO_DLIG: u32 = 1 << 12;
pub const FV_HLIG: u32 = 1 << 13;
pub const FV_NO_HLIG: u32 = 1 << 14;
pub const FV_NO_CALT: u32 = 1 << 15;
pub const FV_CALT: u32 = 1 << 16;
pub const FV_LIGATURES_MASK: u32 = 0x1ff00;
pub const FV_KERN_NONE: u32 = 1 << 17;
pub const FV_SUB: u32 = 1 << 18;
pub const FV_SUPER: u32 = 1 << 19;

pub fn font_variant_keyword(k: &str) -> u32 {
    match k {
        "lining-nums" => FV_LNUM,
        "oldstyle-nums" => FV_ONUM,
        "proportional-nums" => FV_PNUM,
        "tabular-nums" => FV_TNUM,
        "diagonal-fractions" => FV_FRAC,
        "stacked-fractions" => FV_AFRC,
        "ordinal" => FV_ORDN,
        "slashed-zero" => FV_ZERO,
        "none" => FV_LIG_NONE,
        "no-common-ligatures" => FV_NO_COMMON,
        "common-ligatures" => FV_COMMON,
        "discretionary-ligatures" => FV_DLIG,
        "no-discretionary-ligatures" => FV_NO_DLIG,
        "historical-ligatures" => FV_HLIG,
        "no-historical-ligatures" => FV_NO_HLIG,
        "no-contextual" => FV_NO_CALT,
        "contextual" => FV_CALT,
        "sub" => FV_SUB,
        "super" => FV_SUPER,
        _ => 0,
    }
}

impl ComputedStyle {
    /// OpenType features for shaping: font-variant-* first, then
    /// font-feature-settings (which wins).
    pub fn font_features(&self) -> Vec<(u32, u32)> {
        let b = self.font_variant_bits;
        if b == 0 && self.font_feature_settings.is_none() {
            return Vec::new();
        }
        let t = |s: &[u8; 4]| u32::from_be_bytes(*s);
        let mut f = Vec::new();
        for (bit, tag) in [(FV_LNUM, b"lnum"), (FV_ONUM, b"onum"), (FV_PNUM, b"pnum"), (FV_TNUM, b"tnum"), (FV_FRAC, b"frac"), (FV_AFRC, b"afrc"), (FV_ORDN, b"ordn"), (FV_ZERO, b"zero"), (FV_DLIG, b"dlig"), (FV_HLIG, b"hlig"), (FV_SUB, b"subs"), (FV_SUPER, b"sups")] {
            if b & bit != 0 {
                f.push((t(tag), 1));
            }
        }
        if b & FV_LIG_NONE != 0 {
            for tag in [b"liga", b"clig", b"dlig", b"hlig", b"calt"] {
                f.push((t(tag), 0));
            }
        }
        if b & FV_NO_COMMON != 0 {
            f.push((t(b"liga"), 0));
            f.push((t(b"clig"), 0));
        }
        if b & FV_NO_CALT != 0 {
            f.push((t(b"calt"), 0));
        }
        if b & FV_KERN_NONE != 0 {
            f.push((t(b"kern"), 0));
        }
        if let Some(fs) = &self.font_feature_settings {
            f.extend(fs.iter().copied());
        }
        f
    }
}

pub fn is_inherited(name: &str) -> bool {
    matches!(
        name,
        "color" | "font" | "font-family" | "font-size" | "font-weight" | "font-style" | "font-variant" | "font-feature-settings" | "font-variant-numeric" | "font-variant-ligatures" | "font-variant-position" | "font-kerning" | "font-optical-sizing" | "font-variation-settings" | "line-height"
            | "text-align" | "text-indent" | "text-transform" | "white-space" | "letter-spacing" | "word-spacing" | "text-wrap-style"
            | "list-style" | "list-style-type" | "list-style-position" | "list-style-image" | "cursor" | "visibility"
            | "border-collapse" | "border-spacing" | "empty-cells" | "caption-side" | "word-break" | "overflow-wrap" | "word-wrap" | "text-shadow"
            | "pointer-events" | "user-select" | "direction" | "tab-size" | "accent-color" | "caret-color"
            | "white-space-collapse" | "text-wrap" | "text-wrap-mode" | "fill" | "stroke" | "quotes"
    ) || SVG_PROPS.iter().any(|(n, inh)| *inh && *n == name)
}

/// `<blend-mode>` keyword → code (0 normal … 16 plus-lighter).
pub fn blend_mode(name: &str) -> Option<u8> {
    Some(match name {
        "normal" => 0,
        "multiply" => 1,
        "screen" => 2,
        "overlay" => 3,
        "darken" => 4,
        "lighten" => 5,
        "color-dodge" => 6,
        "color-burn" => 7,
        "hard-light" => 8,
        "soft-light" => 9,
        "difference" => 10,
        "exclusion" => 11,
        "hue" => 12,
        "saturation" => 13,
        "color" => 14,
        "luminosity" => 15,
        "plus-lighter" => 16,
        _ => return None,
    })
}

/// Apply a CSS-wide keyword by copying from `src` (parent or initial).
pub fn copy_prop(s: &mut ComputedStyle, src: &ComputedStyle, name: &str) {
    if let Some(&key) = COLOR_HP_PROPS.iter().find(|n| **n == name) {
        let mut l: Vec<(&'static str, Rc<super::color::Spec>)> = s.color_hp.as_ref().map(|l| l.iter().filter(|(n, _)| *n != key).cloned().collect()).unwrap_or_default();
        if let Some(e) = src.color_hp.as_ref().and_then(|l| l.iter().find(|(n, _)| *n == key)) {
            l.push(e.clone());
        }
        s.color_hp = if l.is_empty() { None } else { Some(Rc::new(l)) };
    }
    macro_rules! cp {
        ($($f:ident),*) => {{ $( s.$f = src.$f.clone(); )* }};
    }
    match name {
        "display" => cp!(display),
        "position" => cp!(position),
        "fill" => cp!(fill),
        "stroke" => cp!(stroke),
        "font-feature-settings" => cp!(font_feature_settings),
        "font-optical-sizing" => cp!(font_optical_sizing_none),
        "font-variation-settings" => cp!(font_variation_settings),
        "font-variant-numeric" | "font-variant-ligatures" | "font-variant-position" | "font-kerning" | "font-variant" => cp!(font_variant_bits),
        n if svg_prop_index(n).is_some() => {
            let i = svg_prop_index(n).unwrap();
            s.set_svg_value(i, src.svg_value(i).cloned());
        }
        "container-type" => cp!(container_type),
        "container-name" => cp!(container_names),
        "container" => cp!(container_type, container_names),
        "contain" => cp!(contain),
        "unicode-bidi" => cp!(bidi_override, bidi_isolate),
        "will-change" => cp!(will_change_cb, will_change_backdrop_root),
        "contain-intrinsic-size" => cp!(contain_intrinsic),
        "float" => cp!(float),
        "border-spacing" => cp!(border_spacing, border_spacing_v),
        "border-collapse" => cp!(border_collapse),
        "empty-cells" => cp!(empty_cells_hide),
        "caption-side" => cp!(caption_bottom),
        "table-layout" => cp!(table_layout_fixed),
        "clear" => cp!(clear),
        "top" => s.inset[0] = src.inset[0].clone(),
        "right" => s.inset[1] = src.inset[1].clone(),
        "bottom" => s.inset[2] = src.inset[2].clone(),
        "left" => s.inset[3] = src.inset[3].clone(),
        "inset" => cp!(inset),
        "width" => cp!(width),
        "height" => cp!(height),
        "min-width" => cp!(min_width),
        "min-height" => cp!(min_height),
        "max-width" => cp!(max_width),
        "max-height" => cp!(max_height),
        "margin" => cp!(margin),
        "margin-top" => s.margin[0] = src.margin[0].clone(),
        "margin-right" => s.margin[1] = src.margin[1].clone(),
        "margin-bottom" => s.margin[2] = src.margin[2].clone(),
        "margin-left" => s.margin[3] = src.margin[3].clone(),
        "padding" => cp!(padding),
        "padding-top" => s.padding[0] = src.padding[0].clone(),
        "padding-right" => s.padding[1] = src.padding[1].clone(),
        "padding-bottom" => s.padding[2] = src.padding[2].clone(),
        "padding-left" => s.padding[3] = src.padding[3].clone(),
        "border" => cp!(border_width, border_style, border_color),
        "border-width" => cp!(border_width),
        "border-style" => cp!(border_style),
        "border-color" => cp!(border_color),
        "border-radius" => cp!(border_radius),
        "caret-color" => cp!(caret_color),
        "accent-color" => cp!(accent_color),
        // Per-side longhands (and the per-side shorthands).
        n if n.starts_with("border-") && !n.contains("radius") && !n.contains("image") && !n.contains("spacing") && !n.contains("collapse") => {
            let rest = &n[7..];
            let (side, part) = match rest.rsplit_once('-') {
                Some((sd, pt)) if matches!(pt, "width" | "style" | "color") => (sd, Some(pt)),
                _ => (rest, None),
            };
            let idx: &[usize] = match side {
                "top" | "block-start" => &[0],
                "right" | "inline-end" => &[1],
                "bottom" | "block-end" => &[2],
                "left" | "inline-start" => &[3],
                "block" => &[0, 2],
                "inline" => &[1, 3],
                _ => &[],
            };
            for &i in idx {
                if part.is_none() || part == Some("width") {
                    s.border_width[i] = src.border_width[i];
                }
                if part.is_none() || part == Some("style") {
                    s.border_style[i] = src.border_style[i];
                }
                if part.is_none() || part == Some("color") {
                    s.border_color[i] = src.border_color[i].clone();
                }
            }
        }
        "box-sizing" => cp!(box_sizing),
        "overflow" => cp!(overflow_x, overflow_y),
        "overflow-x" => cp!(overflow_x),
        "overflow-y" => cp!(overflow_y),
        "z-index" => cp!(z_index),
        "opacity" => cp!(opacity),
        "mix-blend-mode" => cp!(mix_blend),
        "isolation" => cp!(isolate),
        "columns" => cp!(columns, column_width),
        "column-count" => cp!(columns),
        "column-width" => cp!(column_width),
        "column-rule" => cp!(column_rule),
        "column-rule-width" => s.column_rule.0 = src.column_rule.0,
        "column-rule-style" => s.column_rule.1 = src.column_rule.1,
        "column-rule-color" => s.column_rule.2 = src.column_rule.2,
        "column-span" => cp!(column_span_all),
        "column-fill" => cp!(column_fill_auto),
        "break-inside" | "page-break-inside" => cp!(break_inside_avoid),
        "resize" => cp!(resize),
        "border-image" | "border-image-source" | "border-image-slice" | "border-image-width" | "border-image-outset" | "border-image-repeat" => cp!(border_image),
        "color-scheme" => cp!(dark_scheme),
        "background-blend-mode" => cp!(bg_blend),
        "background" => cp!(background_color, background),
        "background-color" => cp!(background_color),
        "background-image" => cp!(background),
        "box-shadow" => cp!(box_shadow),
        "outline" => cp!(outline_width, outline_style, outline_color),
        "outline-width" => cp!(outline_width),
        "outline-style" => cp!(outline_style),
        "outline-color" => cp!(outline_color),
        "transform" => cp!(transform),
        "transform-origin" => cp!(transform_origin, transform_origin_z),
        "perspective" | "-webkit-perspective" => cp!(perspective),
        "perspective-origin" | "-webkit-perspective-origin" => cp!(perspective_origin),
        "transform-style" | "-webkit-transform-style" => cp!(preserve_3d),
        "backface-visibility" | "-webkit-backface-visibility" => cp!(backface_hidden),
        "writing-mode" | "-webkit-writing-mode" | "-ms-writing-mode" => cp!(writing_mode),
        "shape-outside" | "-webkit-shape-outside" => cp!(shape_outside),
        "image-rendering" | "-ms-interpolation-mode" => cp!(pixelated),
        "shape-margin" | "-webkit-shape-margin" => cp!(shape_margin),
        "text-orientation" | "-webkit-text-orientation" => cp!(text_orientation),
        "text-combine-upright" | "-webkit-text-combine" => cp!(text_combine),
        "filter" | "-webkit-filter" => cp!(filter),
        "backdrop-filter" | "-webkit-backdrop-filter" => cp!(backdrop_filter),
        "translate" => cp!(translate_prop),
        "rotate" => cp!(rotate_prop),
        "scale" => cp!(scale_prop),
        "content" => cp!(content),
        "counter-reset" => cp!(counter_reset),
        "counter-increment" => cp!(counter_increment),
        "counter-set" => cp!(counter_set),
        "quotes" => cp!(quotes),
        "text-decoration" | "text-decoration-line" => cp!(text_decoration_line),
        "vertical-align" => cp!(vertical_align),
        "flex-direction" => cp!(flex_direction),
        "flex-wrap" => cp!(flex_wrap),
        "justify-content" => cp!(justify_content),
        "align-items" => cp!(align_items),
        "align-self" => cp!(align_self),
        "align-content" => cp!(align_content),
        "flex" => cp!(flex_grow, flex_shrink, flex_basis),
        "flex-grow" => cp!(flex_grow),
        "flex-shrink" => cp!(flex_shrink),
        "flex-basis" => cp!(flex_basis),
        "order" => cp!(order),
        "gap" => cp!(row_gap, column_gap),
        "row-gap" => cp!(row_gap),
        "column-gap" => cp!(column_gap),
        "color" => cp!(color),
        "font" => cp!(font_family, font_size, font_weight, font_style, line_height),
        "font-family" => cp!(font_family),
        "font-size" => cp!(font_size, font_kw),
        "font-weight" => cp!(font_weight),
        "font-style" => cp!(font_style),
        "line-height" => cp!(line_height),
        "text-align" => cp!(text_align),
        "text-indent" => cp!(text_indent),
        "text-transform" => cp!(text_transform),
        "white-space" => cp!(white_space),
        "text-wrap-style" => cp!(wrap_balance),
        "letter-spacing" => cp!(letter_spacing),
        "word-spacing" => cp!(word_spacing),
        "list-style" => cp!(list_style_type, list_style_inside, list_style_image),
        "list-style-type" => cp!(list_style_type),
        "cursor" => cp!(cursor),
        "visibility" => cp!(visibility),
        "pointer-events" => cp!(pointer_events),
        "text-shadow" => cp!(text_shadow),
        _ => {}
    }
}

fn parse_border_style(v: &str) -> Option<BorderStyle> {
    Some(match v {
        "none" => BorderStyle::None,
        "hidden" => BorderStyle::Hidden,
        "solid" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        "groove" => BorderStyle::Groove,
        "ridge" => BorderStyle::Ridge,
        "inset" => BorderStyle::Inset,
        "outset" => BorderStyle::Outset,
        _ => return None,
    })
}

fn parse_border_width(cv: &CV, u: &UnitCtx) -> Option<f32> {
    if let Some(i) = cv.ident() {
        return match i.to_ascii_lowercase().as_str() {
            "thin" => Some(1.0),
            "medium" => Some(3.0),
            "thick" => Some(5.0),
            _ => None,
        };
    }
    match parse_len(cv, u)? {
        Len::Pct(_) => None,
        l => Some(snap_border_width(l.resolve(0.0).max(0.0))),
    }
}

/// CSS Values "snap a length as a border width" (one device pixel per CSS
/// pixel): below 1px rounds up to 1px, above it rounds down to whole pixels.
pub fn snap_border_width(v: f32) -> f32 {
    if v <= 0.0 || !v.is_finite() {
        v.max(0.0)
    } else if v < 1.0 {
        1.0
    } else {
        (v + 1e-4).floor()
    }
}

/// Expand 1-4 values into [top, right, bottom, left].
fn four<T: Clone>(v: Vec<T>) -> Option<[T; 4]> {
    match v.len() {
        1 => Some([v[0].clone(), v[0].clone(), v[0].clone(), v[0].clone()]),
        2 => Some([v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()]),
        3 => Some([v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()]),
        4 => Some([v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone()]),
        _ => None,
    }
}

/// Physical side (top, right, bottom, left) a property name refers to;
/// inline-start/end follow the direction (horizontal writing mode).
fn side_index(name: &str, rtl: bool) -> Option<usize> {
    if name.ends_with("-top") || name.ends_with("-block-start") {
        Some(0)
    } else if name.ends_with("-right") {
        Some(1)
    } else if name.ends_with("-inline-end") {
        Some(if rtl { 3 } else { 1 })
    } else if name.ends_with("-bottom") || name.ends_with("-block-end") {
        Some(2)
    } else if name.ends_with("-left") {
        Some(3)
    } else if name.ends_with("-inline-start") {
        Some(if rtl { 1 } else { 3 })
    } else {
        None
    }
}

pub fn parse_font_family(v: &[CV]) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for part in split_commas(v) {
        let w = words(part);
        if w.is_empty() {
            continue;
        }
        if let CV::T(Tok::Str(s)) = w[0] {
            out.push(s.clone());
        } else {
            let names: Vec<&str> = w.iter().filter_map(|c| c.ident()).collect();
            if names.is_empty() {
                continue;
            }
            out.push(names.join(" "));
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// How a font-size value relates to the keyword sizes (see `ComputedStyle::font_kw`).
fn font_kw_of(cv: &CV, parent: &ComputedStyle) -> Option<(u8, f32)> {
    if let Some(i) = cv.ident() {
        let idx = match i.to_ascii_lowercase().as_str() {
            "xx-small" => 0,
            "x-small" => 1,
            "small" => 2,
            "medium" => 3,
            "large" => 4,
            "x-large" => 5,
            "xx-large" => 6,
            "xxx-large" => 7,
            "smaller" => return parent.font_kw.map(|(k, m)| (k, m / 1.2)),
            "larger" => return parent.font_kw.map(|(k, m)| (k, m * 1.2)),
            _ => return parent.font_kw,
        };
        return Some((idx, 1.0));
    }
    match cv {
        CV::T(Tok::Dimension(n, u)) if u.eq_ignore_ascii_case("em") => parent.font_kw.map(|(k, m)| (k, m * *n as f32)),
        CV::T(Tok::Percentage(p)) => parent.font_kw.map(|(k, m)| (k, m * *p as f32 / 100.0)),
        _ => None,
    }
}

fn font_size_px(cv: &CV, s: &ComputedStyle, parent: &ComputedStyle, cx: &ApplyCtx) -> Option<f32> {
    if let Some(i) = cv.ident() {
        return Some(match i.to_ascii_lowercase().as_str() {
            "xx-small" => 9.0,
            "x-small" => 10.0,
            "small" => 13.0,
            "medium" => 16.0,
            "large" => 18.0,
            "x-large" => 24.0,
            "xx-large" => 32.0,
            "xxx-large" => 48.0,
            "smaller" => parent.font_size / 1.2,
            "larger" => parent.font_size * 1.2,
            "math" => parent.font_size,
            _ => return None,
        });
    }
    // em and % are relative to the parent's font size.
    let mut u = UnitCtx::new(parent.font_size, cx.root_font_size, cx.vw, cx.vh);
    // (Container query units resolve against the element's query container.)
    u.cq = cx.cq;
    let _ = s;
    let l = parse_len(cv, &u)?;
    let px = l.resolve(parent.font_size);
    if px < 0.0 {
        return None;
    }
    Some(px)
}

fn parse_weight(cv: &CV, parent: &ComputedStyle) -> Option<u16> {
    match cv {
        CV::T(Tok::Number(n)) => Some(n.clamp(1.0, 1000.0) as u16),
        CV::T(Tok::Ident(i)) => Some(match i.to_ascii_lowercase().as_str() {
            "normal" => 400,
            "bold" => 700,
            "bolder" => {
                if parent.font_weight < 350 {
                    400
                } else if parent.font_weight < 550 {
                    700
                } else {
                    900
                }
            }
            "lighter" => {
                if parent.font_weight < 550 {
                    100
                } else if parent.font_weight < 750 {
                    400
                } else {
                    700
                }
            }
            _ => return None,
        }),
        _ => parse_number(cv).map(|n| n.clamp(1.0, 1000.0) as u16),
    }
}

fn parse_align(v: &str) -> Option<Align> {
    Some(match v {
        "auto" => Align::Auto,
        "normal" => Align::Normal,
        "stretch" => Align::Stretch,
        "start" | "flex-start" | "self-start" => Align::Start,
        "end" | "flex-end" | "self-end" => Align::End,
        "center" => Align::Center,
        "baseline" | "first" | "last" => Align::Baseline,
        "space-between" => Align::SpaceBetween,
        "space-around" => Align::SpaceAround,
        "space-evenly" => Align::SpaceEvenly,
        "left" => Align::Left,
        "right" => Align::Right,
        _ => return None,
    })
}

fn parse_align_value(v: &[CV]) -> Option<Align> {
    // Skip "safe"/"unsafe"/"legacy" modifiers.
    let w = words(v);
    let mut out = None;
    for c in w {
        let i = c.ident()?.to_ascii_lowercase();
        if matches!(i.as_str(), "safe" | "unsafe" | "legacy") {
            continue;
        }
        out = Some(parse_align(&i)?);
    }
    out
}

pub fn parse_list_style_type(i: &str) -> Option<ListStyleType> {
    Some(match i {
        "none" => ListStyleType::None,
        "disc" => ListStyleType::Disc,
        "circle" => ListStyleType::Circle,
        "square" => ListStyleType::Square,
        "decimal" => ListStyleType::Decimal,
        "decimal-leading-zero" => ListStyleType::DecimalLeadingZero,
        "lower-alpha" | "lower-latin" => ListStyleType::LowerAlpha,
        "upper-alpha" | "upper-latin" => ListStyleType::UpperAlpha,
        "lower-roman" => ListStyleType::LowerRoman,
        "upper-roman" => ListStyleType::UpperRoman,
        "lower-greek" => ListStyleType::LowerGreek,
        "disclosure-closed" => ListStyleType::DisclosureClosed,
        "disclosure-open" => ListStyleType::DisclosureOpen,
        "inherit" | "initial" | "unset" | "revert" | "revert-layer" | "default" | "inside" | "outside" => return None,
        other => ListStyleType::Named(Rc::from(other)),
    })
}

fn parse_shadow_list(v: &[CV], u: &UnitCtx) -> Option<Option<Rc<Vec<Shadow>>>> {
    if ident_lc(v).as_deref() == Some("none") {
        return Some(None);
    }
    let mut out = Vec::new();
    for part in split_commas(v) {
        let mut inset = false;
        let mut lens = Vec::new();
        let mut color = None;
        for c in words(part) {
            if c.is_ident("inset") {
                inset = true;
            } else if let Some(l) = parse_len(c, u) {
                lens.push(l.resolve(0.0));
            } else if let Some(col) = parse_color(c) {
                color = Some(col);
            } else {
                return None;
            }
        }
        if lens.len() < 2 {
            return None;
        }
        out.push(Shadow {
            inset,
            x: lens[0],
            y: lens[1],
            blur: lens.get(2).copied().unwrap_or(0.0).max(0.0),
            spread: lens.get(3).copied().unwrap_or(0.0),
            color: color.unwrap_or(ColorValue::CurrentColor),
        });
    }
    Some(Some(Rc::new(out)))
}

/// `round <border-radius>` of inset()/xywh()/rect().
fn parse_shape_radii(w: &[&CV], u: &UnitCtx) -> Option<[(Len, Len); 4]> {
    let slash = w.iter().position(|c| c.is_delim('/'));
    let (a, b) = match slash {
        Some(k) => (&w[..k], &w[k + 1..]),
        None => (w, w),
    };
    let la: Vec<Len> = a.iter().filter_map(|c| parse_len(c, u)).collect();
    let lb: Vec<Len> = b.iter().filter_map(|c| parse_len(c, u)).collect();
    let fa = four(la)?;
    let fb = four(lb)?;
    Some([(fa[0].clone(), fb[0].clone()), (fa[1].clone(), fb[1].clone()), (fa[2].clone(), fb[2].clone()), (fa[3].clone(), fb[3].clone())])
}

fn parse_basic_shape(name: &str, args: &[CV], u: &UnitCtx) -> Option<BasicShape> {
    let w = words(args);
    let radius = |c: &CV| -> Option<ShapeRadius> {
        match c.ident().map(|i| i.to_ascii_lowercase()).as_deref() {
            Some("closest-side") => Some(ShapeRadius::ClosestSide),
            Some("farthest-side") => Some(ShapeRadius::FarthestSide),
            Some(_) => None,
            None => parse_len(c, u).map(ShapeRadius::Len),
        }
    };
    let center = Len::Pct(50.0);
    match name {
        "inset" | "xywh" | "rect" => {
            let end = w.iter().position(|c| c.is_ident("round")).unwrap_or(w.len());
            let radii = if end < w.len() { Some(parse_shape_radii(&w[end + 1..], u)?) } else { None };
            let parts = &w[..end];
            match name {
                "inset" => {
                    let l: Vec<Len> = parts.iter().filter_map(|c| parse_len(c, u)).collect();
                    if l.len() != parts.len() {
                        return None;
                    }
                    Some(BasicShape::Inset(four(l)?, radii))
                }
                "xywh" => {
                    let l: Vec<Len> = parts.iter().filter_map(|c| parse_len(c, u)).collect();
                    if l.len() != 4 {
                        return None;
                    }
                    Some(BasicShape::Xywh([l[0].clone(), l[1].clone(), l[2].clone(), l[3].clone()], radii))
                }
                _ => {
                    // rect(top right bottom left): edges from the reference box's top/left; auto = the box edge.
                    if parts.len() != 4 {
                        return None;
                    }
                    let e: Vec<Option<Len>> = parts.iter().map(|c| if c.is_ident("auto") { None } else { parse_len(c, u) }).collect();
                    let (t, r, b, l) = (e[0].clone().unwrap_or(Len::Pct(0.0)), e[1].clone().unwrap_or(Len::Pct(100.0)), e[2].clone().unwrap_or(Len::Pct(100.0)), e[3].clone().unwrap_or(Len::Pct(0.0)));
                    // Convert to insets: right/bottom edges measured from the far side.
                    let inv = |x: Len| match x {
                        Len::Px(v) => Len::Calc(-v, 100.0),
                        Len::Pct(p) => Len::Pct(100.0 - p),
                        Len::Calc(px, p) => Len::Calc(-px, 100.0 - p),
                        o => o,
                    };
                    Some(BasicShape::Inset([t, inv(r), inv(b), l], radii))
                }
            }
        }
        "circle" => {
            let at = w.iter().position(|c| c.is_ident("at"));
            let head = &w[..at.unwrap_or(w.len())];
            let r = match head {
                [] => ShapeRadius::ClosestSide,
                [c] => radius(c)?,
                _ => return None,
            };
            let pos = match at {
                Some(k) => parse_position_pair(&w[k + 1..], u)?,
                None => (center.clone(), center),
            };
            Some(BasicShape::Circle(r, pos))
        }
        "ellipse" => {
            let at = w.iter().position(|c| c.is_ident("at"));
            let head = &w[..at.unwrap_or(w.len())];
            let (rx, ry) = match head {
                [] => (ShapeRadius::ClosestSide, ShapeRadius::ClosestSide),
                [a, b] => (radius(a)?, radius(b)?),
                _ => return None,
            };
            let pos = match at {
                Some(k) => parse_position_pair(&w[k + 1..], u)?,
                None => (center.clone(), center),
            };
            Some(BasicShape::Ellipse(rx, ry, pos))
        }
        "polygon" => {
            let mut evenodd = false;
            let mut pts = Vec::new();
            for (k, part) in split_commas(args).into_iter().enumerate() {
                let pw = words(part);
                if k == 0 && pw.len() == 1 && pw[0].ident().is_some() {
                    evenodd = pw[0].is_ident("evenodd");
                    continue;
                }
                let (lw, rest): (Vec<&CV>, Vec<&CV>) = (pw.clone(), Vec::new());
                let _ = rest;
                let mut it = lw.iter();
                let (x, y) = match (it.next(), it.next()) {
                    (Some(a), Some(b)) => (parse_len(a, u)?, parse_len(b, u)?),
                    (Some(a), None) if k == 0 && a.ident().is_some() => continue,
                    _ => return None,
                };
                // The fill rule may precede the first point in the same comma part.
                pts.push((x, y));
            }
            if pts.len() < 3 {
                return None;
            }
            Some(BasicShape::Polygon(pts, evenodd))
        }
        "path" => {
            let mut evenodd = false;
            let mut d = None;
            for c in args.iter().filter(|c| !c.is_ws() && !matches!(c, CV::T(Tok::Comma))) {
                match c {
                    CV::T(Tok::Str(sv)) => d = Some(sv.clone()),
                    c if c.is_ident("evenodd") => evenodd = true,
                    c if c.is_ident("nonzero") => evenodd = false,
                    _ => return None,
                }
            }
            Some(BasicShape::Path(Rc::from(d?.as_str()), evenodd))
        }
        _ => None,
    }
}

fn parse_position_pair(w: &[&CV], u: &UnitCtx) -> Option<(Len, Len)> {
    // Handles 1-4 value <position> syntax (keywords and lengths).
    let kw = |c: &CV| c.ident().map(|s| s.to_ascii_lowercase());
    let to_len = |c: &CV| -> Option<Len> {
        match kw(c).as_deref() {
            Some("left") | Some("top") => Some(Len::Pct(0.0)),
            Some("right") | Some("bottom") => Some(Len::Pct(100.0)),
            Some("center") => Some(Len::Pct(50.0)),
            _ => parse_len(c, u),
        }
    };
    match w.len() {
        0 => None,
        1 => {
            let k = kw(w[0]);
            match k.as_deref() {
                Some("top") | Some("bottom") => Some((Len::Pct(50.0), to_len(w[0])?)),
                _ => Some((to_len(w[0])?, Len::Pct(50.0))),
            }
        }
        2 => {
            let (a, b) = (kw(w[0]), kw(w[1]));
            if matches!(a.as_deref(), Some("top") | Some("bottom")) || matches!(b.as_deref(), Some("left") | Some("right")) {
                Some((to_len(w[1])?, to_len(w[0])?))
            } else {
                Some((to_len(w[0])?, to_len(w[1])?))
            }
        }
        _ => {
            // "right 10px bottom 20px" style.
            let mut x = Len::Pct(50.0);
            let mut y = Len::Pct(50.0);
            let mut i = 0;
            while i < w.len() {
                let k = kw(w[i]);
                let off = w.get(i + 1).and_then(|c| if kw(c).is_none() { parse_len(c, u) } else { None });
                match k.as_deref() {
                    Some("left") => x = off.clone().unwrap_or(Len::Pct(0.0)),
                    Some("right") => {
                        x = match &off {
                            Some(Len::Px(p)) => Len::Calc(-p, 100.0),
                            Some(Len::Pct(p)) => Len::Pct(100.0 - p),
                            _ => Len::Pct(100.0),
                        }
                    }
                    Some("top") => y = off.clone().unwrap_or(Len::Pct(0.0)),
                    Some("bottom") => {
                        y = match &off {
                            Some(Len::Px(p)) => Len::Calc(-p, 100.0),
                            Some(Len::Pct(p)) => Len::Pct(100.0 - p),
                            _ => Len::Pct(100.0),
                        }
                    }
                    Some("center") => {}
                    _ => return None,
                }
                i += if off.is_some() { 2 } else { 1 };
            }
            Some((x, y))
        }
    }
}

pub fn parse_gradient(name: &str, args: &[CV], u: &UnitCtx) -> Option<Gradient> {
    let repeating = name.starts_with("repeating-");
    let base = name.trim_start_matches("repeating-").trim_start_matches("-webkit-").trim_start_matches("-moz-");
    let radial = base == "radial-gradient";
    let conic = base == "conic-gradient";
    if !matches!(base, "linear-gradient" | "radial-gradient" | "conic-gradient") {
        return None;
    }
    let parts = split_commas(args);
    let mut angle = if conic { 0.0 } else { 180.0 };
    let mut circle = false;
    let mut center = (Len::Pct(50.0), Len::Pct(50.0));
    let mut first_stop = 0;
    let mut space = None;
    let mut hue = 0u8;
    let mut extent = 0u8;
    let mut size: Option<(Len, Len)> = None;
    let mut hints: Vec<(usize, Len)> = Vec::new();
    if let Some(first) = parts.first() {
        let mut w = words(first);
        // `in <colorspace> [<hue-method> hue]` may appear anywhere in the prelude.
        if let Some(i) = w.iter().position(|c| c.is_ident("in")) {
            space = w.get(i + 1).and_then(|c| c.ident()).and_then(|n| mix_space(&n.to_ascii_lowercase()));
            if space.is_none() {
                return None;
            }
            let mut end = i + 2;
            while end < w.len() && w[end].ident().map(|x| matches!(x.to_ascii_lowercase().as_str(), "shorter" | "longer" | "increasing" | "decreasing" | "hue")).unwrap_or(false) {
                if let Some(h) = w[end].ident().and_then(|x| super::values::hue_method(&x.to_ascii_lowercase())) {
                    hue = h;
                }
                end += 1;
            }
            w.drain(i..end.min(w.len()));
            if w.is_empty() {
                first_stop = 1;
            }
        }
        if w.is_empty() {
        } else if !radial && !conic {
            if let Some(a) = w.first().and_then(|c| angle_deg(c)) {
                angle = a;
                first_stop = 1;
            } else if w.first().map(|c| c.is_ident("to")).unwrap_or(false) {
                let dirs: Vec<String> = w[1..].iter().filter_map(|c| c.ident().map(|s| s.to_ascii_lowercase())).collect();
                let has = |d: &str| dirs.iter().any(|x| x == d);
                angle = match (has("top"), has("bottom"), has("left"), has("right")) {
                    (true, _, true, _) => 315.0,
                    (true, _, _, true) => 45.0,
                    (_, true, true, _) => 225.0,
                    (_, true, _, true) => 135.0,
                    (true, _, _, _) => 0.0,
                    (_, true, _, _) => 180.0,
                    (_, _, true, _) => 270.0,
                    (_, _, _, true) => 90.0,
                    _ => 180.0,
                };
                first_stop = 1;
            }
        } else if parse_color(w.first()?).is_none() || w.iter().any(|c| c.is_ident("at") || c.is_ident("circle") || c.is_ident("ellipse") || c.is_ident("from")) {
            // radial/conic shape/position prelude
            let mut i = 0;
            let mut sizes: Vec<Len> = Vec::new();
            while i < w.len() {
                if w[i].is_ident("circle") {
                    circle = true;
                } else if w[i].is_ident("at") {
                    if let Some(p) = parse_position_pair(&w[i + 1..], u) {
                        center = p;
                    }
                    break;
                } else if w[i].is_ident("from") {
                    if let Some(a) = w.get(i + 1).and_then(|c| angle_deg(c)) {
                        angle = a;
                    }
                    i += 1;
                } else if let Some(k) = w[i].ident().map(|x| x.to_ascii_lowercase()) {
                    extent = match k.as_str() {
                        "closest-side" => 1,
                        "closest-corner" => 2,
                        "farthest-side" => 3,
                        _ => extent,
                    };
                } else if radial {
                    if let Some(l) = parse_len(w[i], u) {
                        sizes.push(l);
                    }
                }
                i += 1;
            }
            match sizes.len() {
                1 => {
                    circle = true;
                    size = Some((sizes[0].clone(), sizes[0].clone()));
                }
                2 => size = Some((sizes[0].clone(), sizes[1].clone())),
                _ => {}
            }
            first_stop = 1;
        }
    }
    let mut stops = Vec::new();
    for p in &parts[first_stop..] {
        let w = words(p);
        if w.is_empty() {
            continue;
        }
        match parse_color(w[0]) {
            Some(c) => {
                let col = c.resolve(Color::BLACK);
                let at = |c: &CV| parse_len(c, u).or_else(|| angle_deg(c).map(|d| Len::Pct(d / 3.6)));
                let pos = w.get(1).and_then(|c| at(c));
                stops.push((col, pos));
                if let Some(p2) = w.get(2).and_then(|c| at(c)) {
                    stops.push((col, Some(p2)));
                }
            }
            None => {
                // Color hint: a bare position between two color stops.
                if w.len() == 1 && !stops.is_empty() {
                    if let Some(h) = parse_len(w[0], u).or_else(|| angle_deg(w[0]).map(|d| Len::Pct(d / 3.6))) {
                        hints.push((stops.len(), h));
                    }
                }
            }
        }
    }
    if stops.is_empty() {
        return None;
    }
    if stops.len() == 1 {
        stops.push(stops[0].clone());
    }
    hints.retain(|(i, _)| *i < stops.len());
    Some(Gradient { radial, repeating, angle, stops, circle, center, conic, space, hue, extent, size, hints })
}

fn bi_none(b: &Option<Rc<BorderImage>>) -> bool {
    b.is_none()
}

/// Parse `border-image` or one of its longhands into `bi`.
fn parse_border_image(name: &str, w: &[&CV], u: &UnitCtx, bi: &mut BorderImage) -> Option<()> {
    let sides4 = |v: &[f32]| -> [f32; 4] {
        match v.len() {
            1 => [v[0]; 4],
            2 => [v[0], v[1], v[0], v[1]],
            3 => [v[0], v[1], v[2], v[1]],
            _ => [v[0], v[1], v[2], v[3]],
        }
    };
    let repeat_kw = |c: &CV| c.ident().and_then(|i| match i.to_ascii_lowercase().as_str() {
        "stretch" => Some(0u8),
        "repeat" => Some(1),
        "round" => Some(2),
        "space" => Some(3),
        _ => None,
    });
    // Split on '/' into groups (slice / width / outset).
    let mut groups: Vec<Vec<&CV>> = vec![Vec::new()];
    for c in w.iter() {
        if c.is_delim('/') {
            groups.push(Vec::new());
        } else {
            groups.last_mut().unwrap().push(c);
        }
    }
    let parse_slice = |toks: &[&CV], bi: &mut BorderImage| -> bool {
        let mut vals: Vec<(f32, bool)> = Vec::new();
        for c in toks {
            match c {
                CV::T(Tok::Number(n)) => vals.push((n.max(0.0) as f32, false)),
                CV::T(Tok::Percentage(p)) => vals.push((p.max(0.0) as f32, true)),
                _ if c.is_ident("fill") => bi.fill = true,
                _ => {}
            }
        }
        if vals.is_empty() {
            return false;
        }
        let v: Vec<f32> = (0..vals.len()).collect::<Vec<_>>().iter().map(|&i| i as f32).collect();
        let idx = sides4(&v);
        for (k, i) in idx.iter().enumerate() {
            bi.slice[k] = vals[*i as usize];
        }
        true
    };
    let parse_widths = |toks: &[&CV], lens: &mut [Option<Len>; 4], mults: &mut [Option<f32>; 4], outset: bool| {
        let mut items: Vec<(Option<Len>, Option<f32>)> = Vec::new();
        for c in toks {
            match c {
                CV::T(Tok::Number(n)) => items.push((None, Some(n.max(0.0) as f32))),
                _ if c.is_ident("auto") && !outset => items.push((None, None)),
                other => {
                    if let Some(l) = parse_len(other, u) {
                        items.push((Some(l), None));
                    }
                }
            }
        }
        if items.is_empty() {
            return;
        }
        let v: Vec<f32> = (0..items.len()).map(|i| i as f32).collect();
        let idx = sides4(&v);
        for (k, i) in idx.iter().enumerate() {
            lens[k] = items[*i as usize].0.clone();
            mults[k] = items[*i as usize].1;
        }
    };
    match name {
        "border-image-source" => {
            bi.source = w.first().and_then(|c| parse_image(c, u)).unwrap_or(BgImage::None);
        }
        "border-image-slice" => {
            bi.fill = false;
            if !parse_slice(w, bi) {
                return None;
            }
        }
        "border-image-width" => {
            let (mut l, mut m) = (bi.width.clone(), bi.width_mult);
            parse_widths(w, &mut l, &mut m, false);
            bi.width = l;
            bi.width_mult = m;
        }
        "border-image-outset" => {
            let (mut l, mut m) = ([None, None, None, None], [None; 4]);
            parse_widths(w, &mut l, &mut m, true);
            for k in 0..4 {
                bi.outset[k] = l[k].as_ref().map(|x| x.resolve(0.0)).unwrap_or(0.0);
                bi.outset_mult[k] = m[k];
            }
        }
        "border-image-repeat" => {
            let r: Vec<u8> = w.iter().filter_map(|c| repeat_kw(c)).collect();
            match r.len() {
                1 => bi.repeat = (r[0], r[0]),
                2 => bi.repeat = (r[0], r[1]),
                _ => return None,
            }
        }
        _ => {
            // Shorthand: source, slice [/ width [/ outset]], repeat in any order.
            let mut slice_toks: Vec<&CV> = Vec::new();
            let mut reps: Vec<u8> = Vec::new();
            for c in &groups[0] {
                if let Some(img) = parse_image(c, u) {
                    bi.source = img;
                } else if let Some(r) = repeat_kw(c) {
                    reps.push(r);
                } else if c.is_ident("none") {
                    bi.source = BgImage::None;
                } else {
                    slice_toks.push(c);
                }
            }
            if !slice_toks.is_empty() {
                parse_slice(&slice_toks, bi);
            }
            for (gi, g) in groups.iter().enumerate().skip(1).take(2) {
                let mut rest: Vec<&CV> = Vec::new();
                for c in g {
                    if let Some(r) = repeat_kw(c) {
                        reps.push(r);
                    } else if let Some(img) = parse_image(c, u) {
                        bi.source = img;
                    } else {
                        rest.push(*c);
                    }
                }
                if gi == 1 {
                    let (mut l, mut m) = (bi.width.clone(), bi.width_mult);
                    parse_widths(&rest, &mut l, &mut m, false);
                    bi.width = l;
                    bi.width_mult = m;
                } else {
                    let (mut l, mut m) = ([None, None, None, None], [None; 4]);
                    parse_widths(&rest, &mut l, &mut m, true);
                    for k in 0..4 {
                        bi.outset[k] = l[k].as_ref().map(|x| x.resolve(0.0)).unwrap_or(0.0);
                        bi.outset_mult[k] = m[k];
                    }
                }
            }
            match reps.len() {
                0 => {}
                1 => bi.repeat = (reps[0], reps[0]),
                _ => bi.repeat = (reps[0], reps[1]),
            }
        }
    }
    Some(())
}

fn parse_image(cv: &CV, u: &UnitCtx) -> Option<BgImage> {
    match cv {
        CV::T(Tok::Url(s)) => Some(BgImage::Url(Rc::from(s.as_str()))),
        CV::Func(n, args) if n == "url" => {
            let s = words(args).first().and_then(|c| match c {
                CV::T(Tok::Str(s)) => Some(s.clone()),
                _ => None,
            })?;
            Some(BgImage::Url(Rc::from(s.as_str())))
        }
        CV::Func(n, args) if n.contains("gradient") => parse_gradient(n, args, u).map(|g| BgImage::Gradient(Rc::new(g))),
        CV::Func(n, args) if n == "image-set" || n == "-webkit-image-set" => {
            // Pick the first candidate.
            let first = split_commas(args).into_iter().next()?;
            let w = words(first);
            match w.first()? {
                CV::T(Tok::Str(s)) => Some(BgImage::Url(Rc::from(s.as_str()))),
                c => parse_image(c, u),
            }
        }
        CV::T(Tok::Ident(i)) if i.eq_ignore_ascii_case("none") => Some(BgImage::None),
        _ => None,
    }
}

fn parse_bg_size(w: &[&CV], u: &UnitCtx) -> Option<BgSize> {
    match w.len() {
        1 => match w[0].ident().map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("cover") => Some(BgSize::Cover),
            Some("contain") => Some(BgSize::Contain),
            Some("auto") => Some(BgSize::Auto),
            _ => Some(BgSize::Explicit(parse_len(w[0], u)?, Len::Auto)),
        },
        2 => Some(BgSize::Explicit(parse_len(w[0], u)?, parse_len(w[1], u)?)),
        _ => None,
    }
}

/// A `background-repeat` keyword: ((repeat-x, repeat-y), (style-x, style-y)),
/// with styles 0 repeat, 1 space, 2 round.
fn parse_repeat(i: &str) -> Option<((bool, bool), (u8, u8))> {
    Some(match i {
        "repeat" => ((true, true), (0, 0)),
        "space" => ((true, true), (1, 1)),
        "round" => ((true, true), (2, 2)),
        "no-repeat" => ((false, false), (0, 0)),
        "repeat-x" => ((true, false), (0, 0)),
        "repeat-y" => ((false, true), (0, 0)),
        _ => return None,
    })
}

/// Combine one or two `background-repeat` keywords (two = x then y).
fn repeat_pair(r: &[((bool, bool), (u8, u8))]) -> Option<((bool, bool), (u8, u8))> {
    match r.len() {
        1 => Some(r[0]),
        2 => Some(((r[0].0 .0, r[1].0 .1), (r[0].1 .0, r[1].1 .1))),
        _ => None,
    }
}

/// Parse the `background` shorthand. Returns (color, layers).
fn parse_background(v: &[CV], u: &UnitCtx) -> Option<(Option<ColorValue>, Vec<BgLayer>)> {
    let mut layers = Vec::new();
    let mut color = None;
    for part in split_commas(v) {
        let w = words(part);
        let mut layer = BgLayer::default();
        let mut pos_tokens: Vec<&CV> = Vec::new();
        let mut i = 0;
        let mut rep: Option<((bool, bool), (u8, u8))> = None;
        let mut seen_box = false;
        while i < w.len() {
            let c = w[i];
            if let Some(img) = parse_image(c, u) {
                layer.image = img;
            } else if let Some(r) = c.ident().and_then(|s| parse_repeat(&s.to_ascii_lowercase())) {
                rep = Some(match rep {
                    // two-value repeat syntax "repeat no-repeat"
                    Some(x) => repeat_pair(&[x, r]).unwrap_or(r),
                    None => r,
                });
            } else if c.is_delim('/') {
                // size follows
                let mut j = i + 1;
                let mut size_w = Vec::new();
                while j < w.len() && size_w.len() < 2 {
                    if parse_len(w[j], u).is_some() || w[j].is_ident("cover") || w[j].is_ident("contain") || w[j].is_ident("auto") {
                        size_w.push(w[j]);
                        j += 1;
                    } else {
                        break;
                    }
                }
                if let Some(sz) = parse_bg_size(&size_w, u) {
                    layer.size = sz;
                }
                i = j;
                continue;
            } else if c.ident().map(|s| matches!(s.to_ascii_lowercase().as_str(), "left" | "right" | "top" | "bottom" | "center")).unwrap_or(false)
                || (parse_len(c, u).is_some() && !c.is_ident("auto"))
            {
                pos_tokens.push(c);
            } else if c.is_ident("text") {
                layer.clip_text = true;
            } else if c.is_ident("fixed") {
                layer.fixed = true;
            } else if c.is_ident("scroll") || c.is_ident("local") {
            } else if let Some(bx) = c.ident().and_then(|s| bg_box(&s.to_ascii_lowercase())) {
                if seen_box {
                    layer.clip = bx;
                } else {
                    layer.origin = bx;
                    layer.clip = bx;
                    seen_box = true;
                }
            } else if let Some(col) = parse_color(c) {
                // Only one color, in the final layer.
                if color.is_some() {
                    return None;
                }
                color = Some(col);
            } else {
                return None;
            }
            i += 1;
        }
        if let Some(r) = rep {
            layer.repeat = r.0;
            layer.rmode = r.1;
        }
        if !pos_tokens.is_empty() {
            if let Some(p) = parse_position_pair(&pos_tokens, u) {
                layer.pos = p;
            }
        }
        layers.push(layer);
    }
    Some((color, layers))
}

/// Parse a `filter` value: `none` → Some(None); invalid (or `url()`) → None.
fn parse_filter(v: &[CV], u: &UnitCtx) -> Option<Option<Rc<Vec<FilterOp>>>> {
    if ident_lc(v).as_deref() == Some("none") {
        return Some(None);
    }
    let mut ops = Vec::new();
    for c in words(v) {
        let (name, args) = match c {
            CV::Func(n, a) => (n.as_str(), a),
            _ => return None,
        };
        let a: Vec<&CV> = args.iter().filter(|c| !c.is_ws()).collect();
        // Amount: number or percentage, default 1 (0 for blur/hue-rotate).
        let amount = |default: f32| -> Option<f32> {
            match a.first() {
                None => Some(default),
                Some(CV::T(Tok::Percentage(p))) => Some((p / 100.0) as f32),
                Some(c) => parse_number(c),
            }
        };
        let op = match name {
            "blur" => FilterOp::Blur(match a.first() {
                None => 0.0,
                Some(c) => parse_len(c, u)?.resolve(0.0).max(0.0),
            }),
            "brightness" => FilterOp::Brightness(amount(1.0)?.max(0.0)),
            "contrast" => FilterOp::Contrast(amount(1.0)?.max(0.0)),
            "grayscale" => FilterOp::Grayscale(amount(1.0)?.clamp(0.0, 1.0)),
            "invert" => FilterOp::Invert(amount(1.0)?.clamp(0.0, 1.0)),
            "opacity" => FilterOp::Opacity(amount(1.0)?.clamp(0.0, 1.0)),
            "saturate" => FilterOp::Saturate(amount(1.0)?.max(0.0)),
            "sepia" => FilterOp::Sepia(amount(1.0)?.clamp(0.0, 1.0)),
            "hue-rotate" => FilterOp::HueRotate(match a.first() {
                None => 0.0,
                Some(CV::T(Tok::Number(n))) if *n == 0.0 => 0.0,
                Some(c) => angle_deg(c)?,
            }),
            "drop-shadow" => {
                let mut lens = Vec::new();
                let mut color = ColorValue::CurrentColor;
                for c in &a {
                    if let Some(l) = parse_len(c, u) {
                        lens.push(l.resolve(0.0));
                    } else if let Some(col) = parse_color(c) {
                        color = col;
                    } else {
                        return None;
                    }
                }
                if lens.len() < 2 || lens.len() > 3 {
                    return None;
                }
                FilterOp::DropShadow { x: lens[0], y: lens[1], blur: lens.get(2).copied().unwrap_or(0.0).max(0.0), color }
            }
            _ => return None,
        };
        ops.push(op);
    }
    if ops.is_empty() {
        return None;
    }
    Some(Some(Rc::new(ops)))
}

fn parse_transform(v: &[CV], u: &UnitCtx) -> Option<Option<Rc<Vec<TransformOp>>>> {
    if ident_lc(v).as_deref() == Some("none") {
        return Some(None);
    }
    let mut ops = Vec::new();
    for c in words(v) {
        let (name, args) = match c {
            CV::Func(n, a) => (n.as_str(), a),
            _ => return None,
        };
        let a: Vec<&CV> = args.iter().filter(|c| !c.is_ws() && !c.is_comma()).collect();
        let len = |i: usize| a.get(i).and_then(|c| parse_len(c, u));
        let num = |i: usize| a.get(i).and_then(|c| parse_number(c).or_else(|| match c {
            CV::T(Tok::Percentage(p)) => Some((p / 100.0) as f32),
            _ => None,
        }));
        let ang = |i: usize| a.get(i).and_then(|c| angle_deg(c));
        let op = match name {
            "translate" => TransformOp::Translate(len(0)?, len(1).unwrap_or(Len::zero())),
            "translatex" => TransformOp::Translate(len(0)?, Len::zero()),
            "translatey" => TransformOp::Translate(Len::zero(), len(0)?),
            "translate3d" => {
                if a.len() != 3 {
                    return None;
                }
                let z = len(2)?.definite(None)?;
                if z == 0.0 {
                    TransformOp::Translate(len(0)?, len(1)?)
                } else {
                    TransformOp::Translate3d(len(0)?, len(1)?, z)
                }
            }
            "translatez" => {
                let z = len(0)?.definite(None)?;
                if z == 0.0 {
                    TransformOp::Translate(Len::zero(), Len::zero())
                } else {
                    TransformOp::Translate3d(Len::zero(), Len::zero(), z)
                }
            }
            "perspective" => {
                if a.first().map(|c| c.is_ident("none")).unwrap_or(false) {
                    TransformOp::Perspective(None)
                } else {
                    let d = len(0)?.definite(None)?;
                    if d < 0.0 {
                        return None;
                    }
                    TransformOp::Perspective(Some(d))
                }
            }
            "rotatex" => TransformOp::Rotate3d(1.0, 0.0, 0.0, ang(0)?),
            "rotatey" => TransformOp::Rotate3d(0.0, 1.0, 0.0, ang(0)?),
            "scale" => {
                let x = num(0)?;
                TransformOp::Scale(x, num(1).unwrap_or(x))
            }
            "scale3d" => {
                if a.len() != 3 {
                    return None;
                }
                let z = num(2)?;
                if z == 1.0 {
                    TransformOp::Scale(num(0)?, num(1)?)
                } else {
                    TransformOp::Scale3d(num(0)?, num(1)?, z)
                }
            }
            "scalez" => TransformOp::Scale3d(1.0, 1.0, num(0)?),
            "scalex" => TransformOp::Scale(num(0)?, 1.0),
            "scaley" => TransformOp::Scale(1.0, num(0)?),
            "rotate" | "rotatez" => TransformOp::Rotate(ang(0)?),
            "rotate3d" => {
                let (x, y, z) = (num(0)?, num(1)?, num(2)?);
                let d = ang(3)?;
                if x == 0.0 && y == 0.0 && z > 0.0 {
                    TransformOp::Rotate(d)
                } else {
                    TransformOp::Rotate3d(x, y, z, d)
                }
            }
            "skew" => TransformOp::Skew(ang(0)?, ang(1).unwrap_or(0.0)),
            "skewx" => TransformOp::Skew(ang(0)?, 0.0),
            "skewy" => TransformOp::Skew(0.0, ang(0)?),
            "matrix" => TransformOp::Matrix([num(0)?, num(1)?, num(2)?, num(3)?, num(4)?, num(5)?]),
            "matrix3d" => {
                if a.len() != 16 {
                    return None;
                }
                let mut v = [0.0f32; 16];
                for (i, slot) in v.iter_mut().enumerate() {
                    *slot = parse_number(a[i])?;
                }
                // A 2D matrix written as matrix3d stays 2D.
                let flat = [2usize, 3, 6, 7, 8, 9, 11, 14].iter().all(|&i| v[i] == 0.0) && v[10] == 1.0 && v[15] == 1.0;
                if flat {
                    TransformOp::Matrix([v[0], v[1], v[4], v[5], v[12], v[13]])
                } else {
                    TransformOp::Matrix3d(v)
                }
            }
            _ => return None,
        };
        ops.push(op);
    }
    Some(Some(Rc::new(ops)))
}

fn parse_track(c: &CV, u: &UnitCtx) -> Option<Track> {
    match c {
        CV::T(Tok::Dimension(n, unit)) if unit == "fr" => Some(Track::Fr(*n as f32)),
        CV::T(Tok::Ident(i)) => match i.to_ascii_lowercase().as_str() {
            "auto" => Some(Track::Auto),
            "min-content" => Some(Track::MinContent),
            "max-content" => Some(Track::MaxContent),
            _ => None,
        },
        CV::Func(n, args) if n == "minmax" => {
            let parts = split_commas(args);
            if parts.len() != 2 {
                return None;
            }
            let a = parse_track(words(parts[0]).first()?, u)?;
            let b = parse_track(words(parts[1]).first()?, u)?;
            Some(Track::MinMax(Box::new(a), Box::new(b)))
        }
        CV::Func(n, args) if n == "fit-content" => Some(Track::FitContent(parse_len(words(args).first()?, u)?)),
        CV::Func(n, args) if n == "repeat" => {
            let parts = split_commas(args);
            if parts.len() < 2 {
                return None;
            }
            let cnt = match words(parts[0]).first()? {
                CV::T(Tok::Number(n)) => RepeatCount::Count((*n as u32).clamp(1, 1000)),
                c if c.is_ident("auto-fill") => RepeatCount::AutoFill,
                c if c.is_ident("auto-fit") => RepeatCount::AutoFit,
                _ => return None,
            };
            let mut tracks = Vec::new();
            for p in &parts[1..] {
                for c in words(p) {
                    if let CV::Block('[', inner) = c {
                        tracks.push(Track::Names(Rc::new(line_names(inner))));
                        continue;
                    }
                    tracks.push(parse_track(c, u)?);
                }
            }
            Some(Track::Repeat(cnt, tracks))
        }
        _ => parse_len(c, u).map(Track::Len),
    }
}

fn parse_track_list(v: &[CV], u: &UnitCtx) -> Option<Option<Rc<Vec<Track>>>> {
    if ident_lc(v).as_deref() == Some("none") {
        return Some(None);
    }
    let mut out = Vec::new();
    for c in words(v) {
        if let CV::Block('[', inner) = c {
            out.push(Track::Names(Rc::new(line_names(inner))));
            continue;
        }
        if c.is_ident("subgrid") || c.is_ident("masonry") {
            continue;
        }
        out.push(parse_track(c, u)?);
    }
    Some(Some(Rc::new(out)))
}

fn line_names(inner: &[CV]) -> Vec<String> {
    words(inner)
        .iter()
        .filter_map(|c| match c {
            CV::T(Tok::Ident(i)) => Some(i.clone()),
            _ => None,
        })
        .collect()
}

fn parse_grid_line(v: &[CV]) -> Option<GridLine> {
    let w = words(v);
    let mut span = false;
    let mut num: Option<i32> = None;
    let mut name: Option<String> = None;
    for c in &w {
        match c {
            CV::T(Tok::Ident(i)) if i.eq_ignore_ascii_case("span") => span = true,
            CV::T(Tok::Ident(i)) if i.eq_ignore_ascii_case("auto") => return Some(GridLine::Auto),
            CV::T(Tok::Ident(i)) => name = Some(i.clone()),
            CV::T(Tok::Number(n)) => num = Some(*n as i32),
            _ => return None,
        }
    }
    if span {
        return Some(GridLine::Span(num.unwrap_or(1).max(1) as u32));
    }
    if let Some(n) = num {
        return Some(GridLine::Line(n));
    }
    name.map(|n| GridLine::Name(Rc::from(n.as_str())))
}

fn parse_grid_pair(v: &[CV]) -> Option<(GridLine, GridLine)> {
    let mut parts = v.splitn(2, |c| c.is_delim('/'));
    let a = parse_grid_line(parts.next()?)?;
    let b = match parts.next() {
        Some(p) => parse_grid_line(p)?,
        None => match &a {
            GridLine::Name(n) => GridLine::Name(n.clone()),
            _ => GridLine::Auto,
        },
    };
    Some((a, b))
}

fn parse_content(v: &[CV]) -> Option<Option<Rc<Vec<ContentItem>>>> {
    match ident_lc(v).as_deref() {
        Some("none") | Some("normal") => return Some(None),
        _ => {}
    }
    let mut out = Vec::new();
    for c in words(v) {
        match c {
            CV::T(Tok::Str(s)) => out.push(ContentItem::Str(s.clone())),
            CV::Func(n, args) if n == "attr" => out.push(ContentItem::Attr(words(args).first()?.ident()?.to_ascii_lowercase())),
            CV::Func(n, args) if n == "counter" => {
                let parts = split_commas(args);
                let name = words(parts.first()?).first()?.ident()?.to_string();
                let style = match parts.get(1).and_then(|p| words(p).first().and_then(|c| c.ident()).map(|i| i.to_ascii_lowercase())) {
                    Some(i) => parse_list_style_type(&i).unwrap_or(ListStyleType::Decimal),
                    None => ListStyleType::Decimal,
                };
                out.push(ContentItem::Counter(name, style));
            }
            CV::Func(n, args) if n == "counters" => {
                let parts = split_commas(args);
                let name = words(parts.first()?).first()?.ident()?.to_string();
                let sep = match parts.get(1).and_then(|p| words(p).first().copied()) {
                    Some(CV::T(Tok::Str(s))) => s.clone(),
                    _ => return None,
                };
                let style = match parts.get(2).and_then(|p| words(p).first().and_then(|c| c.ident()).map(|i| i.to_ascii_lowercase())) {
                    Some(i) => parse_list_style_type(&i).unwrap_or(ListStyleType::Decimal),
                    None => ListStyleType::Decimal,
                };
                out.push(ContentItem::Counters(name, sep, style));
            }
            CV::T(Tok::Url(u)) => out.push(ContentItem::Url(u.clone())),
            CV::Func(n, args) if n.eq_ignore_ascii_case("url") => match words(args).first() {
                Some(CV::T(Tok::Str(u))) => out.push(ContentItem::Url(u.clone())),
                _ => return None,
            },
            CV::T(Tok::Ident(i)) => match i.to_ascii_lowercase().as_str() {
                "open-quote" => out.push(ContentItem::OpenQuote),
                "close-quote" => out.push(ContentItem::CloseQuote),
                "no-open-quote" => out.push(ContentItem::NoOpenQuote),
                "no-close-quote" => out.push(ContentItem::NoCloseQuote),
                _ => return None,
            },
            CV::T(Tok::Delim('/')) => break, // alt text follows
            _ => {}
        }
    }
    Some(Some(Rc::new(out)))
}

fn parse_cursor(v: &[CV]) -> Option<Cursor> {
    // url() fallbacks followed by a keyword; an unknown keyword invalidates the declaration.
    let w = words(v);
    let kw = w.iter().rev().find_map(|c| c.ident())?.to_ascii_lowercase();
    Some(match kw.as_str() {
        "auto" => Cursor::Auto,
        "default" => Cursor::Default,
        "none" => Cursor::None,
        "context-menu" => Cursor::ContextMenu,
        "help" => Cursor::Help,
        "pointer" => Cursor::Pointer,
        "progress" => Cursor::Progress,
        "wait" => Cursor::Wait,
        "cell" => Cursor::Cell,
        "crosshair" => Cursor::Crosshair,
        "text" => Cursor::Text,
        "vertical-text" => Cursor::VerticalText,
        "alias" => Cursor::Alias,
        "copy" => Cursor::Copy,
        "move" => Cursor::Move,
        "no-drop" => Cursor::NoDrop,
        "not-allowed" => Cursor::NotAllowed,
        "grab" => Cursor::Grab,
        "grabbing" => Cursor::Grabbing,
        "e-resize" => Cursor::EResize,
        "n-resize" => Cursor::NResize,
        "ne-resize" => Cursor::NeResize,
        "nw-resize" => Cursor::NwResize,
        "s-resize" => Cursor::SResize,
        "se-resize" => Cursor::SeResize,
        "sw-resize" => Cursor::SwResize,
        "w-resize" => Cursor::WResize,
        "ew-resize" => Cursor::EwResize,
        "ns-resize" => Cursor::NsResize,
        "nesw-resize" => Cursor::NeswResize,
        "nwse-resize" => Cursor::NwseResize,
        "col-resize" => Cursor::ColResize,
        "row-resize" => Cursor::RowResize,
        "all-scroll" => Cursor::AllScroll,
        "zoom-in" => Cursor::ZoomIn,
        "zoom-out" => Cursor::ZoomOut,
        "-webkit-grab" => Cursor::Grab,
        "-webkit-grabbing" => Cursor::Grabbing,
        "-webkit-zoom-in" => Cursor::ZoomIn,
        "-webkit-zoom-out" => Cursor::ZoomOut,
        _ => return None,
    })
}

/// Apply a single (var-substituted) declaration to `s`.
/// Non-inherited keyword properties whose computed value is their specified
/// value (for getComputedStyle), with their initial values.
pub const PASSTHRU: &[(&str, &str)] = &[
    ("scroll-behavior", "auto"),
    ("content-visibility", "visible"),
    ("contain", "none"),
    ("will-change", "auto"),
    ("overscroll-behavior-x", "auto"),
    ("overscroll-behavior-y", "auto"),
    ("overscroll-behavior-block", "auto"),
    ("overscroll-behavior-inline", "auto"),
    ("scroll-snap-type", "none"),
    ("scroll-snap-align", "none"),
    ("scroll-snap-stop", "normal"),
    ("touch-action", "auto"),
    ("resize", "none"),
    ("isolation", "auto"),
    ("container-type", "normal"),
    ("container-name", "none"),
    ("scrollbar-gutter", "auto"),
    ("scrollbar-width", "auto"),
    ("break-before", "auto"),
    ("break-after", "auto"),
    ("break-inside", "auto"),
    ("view-transition-name", "none"),
    ("field-sizing", "fixed"),
    ("mix-blend-mode", "normal"),
];

fn is_passthru(name: &str) -> bool {
    matches!(
        name,
        "scroll-behavior" | "content-visibility" | "contain" | "will-change" | "overscroll-behavior" | "overscroll-behavior-x" | "overscroll-behavior-y" | "overscroll-behavior-block"
            | "overscroll-behavior-inline" | "scroll-snap-type" | "scroll-snap-align" | "scroll-snap-stop" | "touch-action" | "resize" | "isolation" | "container-type"
            | "container-name" | "scrollbar-gutter" | "scrollbar-width" | "break-before" | "break-after" | "break-inside" | "view-transition-name" | "field-sizing" | "mix-blend-mode"
    )
}

/// The computed value of a PASSTHRU property (None for other properties).
pub fn passthru_value(s: &ComputedStyle, name: &str) -> Option<String> {
    let init = PASSTHRU.iter().find(|(n, _)| *n == name)?.1;
    Some(s.passthru.as_ref().and_then(|l| l.iter().find(|(n, _)| *n == name).map(|(_, v)| v.to_string())).unwrap_or_else(|| init.to_string()))
}

fn set_passthru(s: &mut ComputedStyle, name: &str, value: Option<String>) {
    let Some(&(key, _)) = PASSTHRU.iter().find(|(n, _)| *n == name) else { return };
    let mut l: Vec<(&'static str, Rc<str>)> = s.passthru.as_ref().map(|l| l.iter().filter(|(n, _)| *n != key).cloned().collect()).unwrap_or_default();
    if let Some(v) = value {
        l.push((key, Rc::from(v)));
    }
    s.passthru = if l.is_empty() { None } else { Some(Rc::new(l)) };
}

/// Keyword values serialized the way computed values are (lowercase
/// identifiers, single spaces; will-change and container-name keep commas).
fn passthru_text(v: &[CV]) -> String {
    let t = super::tokenizer::serialize(v);
    let mut out = String::new();
    for part in t.split(',') {
        let words: Vec<String> = part.split_whitespace().map(|w| if w.starts_with('"') { w.to_string() } else { w.to_ascii_lowercase() }).collect();
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(&words.join(" "));
    }
    out
}

/// The latest declared value of `longhand` among engine declarations:
/// (tokens, em, lh); empty tokens mean the initial value.
pub fn generic_find(list: &[GenDecl], longhand: &str) -> Option<GenVal> {
    for d in list.iter().rev() {
        if let Some(i) = &d.interp {
            if d.name == longhand {
                return Some(GenVal::Interp(i.clone()));
            }
            continue;
        }
        if d.value.is_empty() {
            // Initial value, or (em < 0) an animated value held natively.
            if d.name == longhand {
                return Some(if d.em < 0.0 { GenVal::Native } else { GenVal::Decl(d.value.clone(), d.em, d.lh) });
            }
            continue;
        }
        if d.name == longhand {
            return Some(GenVal::Decl(d.value.clone(), d.em, d.lh));
        }
        if let Some(v) = super::pv::longhand_value(d.name, longhand, &d.value) {
            return Some(GenVal::Decl(Rc::new(v), d.em, d.lh));
        }
    }
    None
}

/// A longhand's value in a style (the initial value when not declared).
pub fn gen_val(s: &ComputedStyle, longhand: &str) -> GenVal {
    s.generic.as_ref().and_then(|g| generic_find(g, longhand)).unwrap_or_else(|| GenVal::Decl(Rc::new(Vec::new()), s.font_size, s.line_height_px()))
}

/// The computed value of a GenVal (None: held natively).
pub fn gen_eval(prop: &str, v: &GenVal, env: &super::pv::Env) -> Option<super::pv::V> {
    use super::pv;
    match v {
        GenVal::Native => None,
        GenVal::Decl(t, em, lh) => {
            let mut e = env.clone();
            e.em = *em as f64;
            e.ex = *em as f64 * 0.5;
            e.ch = *em as f64 * 0.5;
            e.lh = *lh as f64;
            if !t.is_empty() {
                if let Some(c) = pv::db::computed_value(prop, t, &e) {
                    return Some(c);
                }
            }
            let init = pv::db::initial_specified(prop)?;
            pv::db::computed_value(prop, &super::tokenizer::parse_cvs(init), &e)
        }
        GenVal::Comp(c) => {
            let v = gen_eval(prop, &c.val, env)?;
            let Some(u) = gen_eval(prop, &c.under, env) else { return Some(v) };
            match pv::interp::add(prop, &u, &v, c.acc) {
                // (Colors stay unclamped until interpolated and serialized.)
                Some(r) if has_rgba(&r) => Some(r),
                Some(r) => Some(pv::interp::finalize(prop, r, env)),
                None => Some(v),
            }
        }
        GenVal::Interp(i) => {
            let a = gen_eval(prop, &i.a, env);
            let b = gen_eval(prop, &i.b, env);
            let (a, b) = match (a, b) {
                (Some(a), Some(b)) => (a, b),
                _ => return None,
            };
            if i.native && pv::interp::interpolate(prop, &a, &b, i.p).is_none() {
                return None;
            }
            Some(pv::interp::blend(prop, &a, &b, i.p, env))
        }
    }
}

fn has_rgba(v: &super::pv::V) -> bool {
    use super::pv::V;
    match v {
        V::Rgba(_) => true,
        V::Seq(x) | V::List(x) | V::Group(x) => x.iter().any(has_rgba),
        V::Fn(_, a) => has_rgba(a),
        _ => false,
    }
}

/// The engine environment for a style (used by animations; getComputedStyle
/// builds its own with the document's base URL).
pub fn gen_env(s: &ComputedStyle, cx: &ApplyCtx) -> super::pv::Env {
    let em = s.font_size as f64;
    let rem = cx.root_font_size as f64;
    super::pv::Env {
        em,
        rem,
        ex: em * 0.5,
        ch: em * 0.5,
        lh: s.line_height_px() as f64,
        rlh: if cx.root_line_height > 0.0 { cx.root_line_height as f64 } else { rem * 1.2 },
        vw: cx.vw as f64,
        vh: cx.vh as f64,
        cqw: cx.cq.map(|c| c.0).unwrap_or(cx.vw) as f64,
        cqh: cx.cq.map(|c| c.1).unwrap_or(cx.vh) as f64,
        current: super::color::Abs::srgb(s.color.r as f64 / 255.0, s.color.g as f64 / 255.0, s.color.b as f64 / 255.0, s.color.a as f64 / 255.0),
        dark: s.dark_scheme,
        base: String::new(),
        zoom: 1.0,
    }
}

/// Record an animated longhand's value.
pub fn push_gen_interp(s: &mut ComputedStyle, name: &'static str, i: GenInterp) {
    let inherited = super::pv::is_inherited(name);
    let mut l: Vec<GenDecl> = s.generic.as_ref().map(|g| (**g).clone()).unwrap_or_default();
    l.push(GenDecl { name, value: Rc::new(Vec::new()), em: s.font_size, lh: s.line_height_px(), inherited, interp: Some(Rc::new(i)) });
    s.generic = Some(Rc::new(l));
}

/// Record a longhand's value (a GenVal from another style).
pub fn push_gen_val(s: &mut ComputedStyle, name: &'static str, v: GenVal) {
    match v {
        GenVal::Interp(i) => push_gen_interp(s, name, (*i).clone()),
        GenVal::Decl(t, em, lh) => {
            let inherited = super::pv::is_inherited(name);
            let mut l: Vec<GenDecl> = s.generic.as_ref().map(|g| (**g).clone()).unwrap_or_default();
            l.push(GenDecl { name, value: t, em, lh, inherited, interp: None });
            s.generic = Some(Rc::new(l));
        }
        GenVal::Comp(c) => {
            // Kept as a (degenerate) interpolation at its own value.
            push_gen_interp(s, name, GenInterp { a: GenVal::Comp(c.clone()), b: GenVal::Comp(c), p: 0.0, native: false });
        }
        GenVal::Native => {}
    }
}

/// Mark a property as holding an animated value (getComputedStyle then
/// reads the native typed fields).
pub fn mark_generic_native(s: &mut ComputedStyle, name: &str) {
    if let Some(key) = super::pv::engine_routed(name) {
        if super::pv::shorthand::longhands(key).is_some() {
            return;
        }
        if let Some(g) = &s.generic {
            if g.last().map(|d| d.name == key && d.em < 0.0).unwrap_or(false) {
                return;
            }
        }
        let inherited = super::pv::all_inherited(key);
        let mut l: Vec<GenDecl> = s.generic.as_ref().map(|g| (**g).clone()).unwrap_or_default();
        l.push(GenDecl { name: key, value: Rc::new(Vec::new()), em: -1.0, lh: 0.0, inherited, interp: None });
        s.generic = Some(Rc::new(l));
    }
}

/// Record a declaration for the property-value engine.
fn note_generic(s: &mut ComputedStyle, parent: &ComputedStyle, key: &'static str, v: &[CV]) {
    let inherited = super::pv::all_inherited(key);
    let lh = match s.line_height {
        LineHeight::Normal => s.font_size * 1.2,
        LineHeight::Number(n) => s.font_size * n,
        LineHeight::Px(p) => p,
    };
    let mut push = |s: &mut ComputedStyle, d: GenDecl| {
        let mut l: Vec<GenDecl> = s.generic.as_ref().map(|g| (**g).clone()).unwrap_or_default();
        l.push(d);
        s.generic = Some(Rc::new(l));
    };
    match css_wide(v) {
        Some(kw) => {
            let longs: Vec<&'static str> = super::pv::shorthand::longhands(key).unwrap_or_else(|| vec![key]);
            for l in longs {
                let inh = super::pv::is_inherited(l);
                let use_parent = kw == "inherit" || ((kw == "unset" || kw == "revert" || kw == "revert-layer") && inh);
                let found = if use_parent { generic_find(parent.generic.as_deref().map(|v| &v[..]).unwrap_or(&[]), l) } else { None };
                match found {
                    Some(GenVal::Decl(val, em, lh)) => push(s, GenDecl { name: l, value: val, em, lh, inherited: inh, interp: None }),
                    Some(GenVal::Interp(i)) => push(s, GenDecl { name: l, value: Rc::new(Vec::new()), em: s.font_size, lh, inherited: inh, interp: Some(i) }),
                    Some(c @ GenVal::Comp(_)) => push(s, GenDecl { name: l, value: Rc::new(Vec::new()), em: s.font_size, lh, inherited: inh, interp: Some(Rc::new(GenInterp { a: c.clone(), b: c, p: 0.0, native: false })) }),
                    Some(GenVal::Native) => push(s, GenDecl { name: l, value: Rc::new(Vec::new()), em: -1.0, lh, inherited: inh, interp: None }),
                    None => push(s, GenDecl { name: l, value: Rc::new(Vec::new()), em: s.font_size, lh, inherited: inh, interp: None }),
                }
            }
        }
        None => {
            let em = s.font_size;
            push(s, GenDecl { name: key, value: Rc::new(v.to_vec()), em, lh, inherited, interp: None });
        }
    }
}

/// Color properties whose specified color text is kept for getComputedStyle.
pub const COLOR_HP_PROPS: &[&str] = &[
    "color", "background-color", "border-top-color", "border-right-color", "border-bottom-color", "border-left-color", "outline-color", "text-decoration-color",
    "caret-color", "column-rule-color", "accent-color", "-webkit-text-fill-color", "-webkit-text-stroke-color", "text-emphasis-color", "fill", "stroke", "stop-color",
    "flood-color", "lighting-color",
];

/// Remember (or forget) a color property's specified text when it is not a plain legacy color.
fn note_color_hp(s: &mut ComputedStyle, name: &str, v: &[CV]) {
    let Some(&key) = COLOR_HP_PROPS.iter().find(|n| **n == name) else { return };
    let w: Vec<&CV> = v.iter().filter(|c| !c.is_ws()).collect();
    let keep = if w.len() == 1 {
        match super::color::parse(w[0]) {
            Some(super::color::Spec::Abs(a)) if a.legacy && a.space == super::color::Space::Srgb && a.c.iter().all(|c| c.is_some()) && a.a.is_some() => None,
            Some(super::color::Spec::Named(..)) | Some(super::color::Spec::Transparent) | None => None,
            Some(sp) => Some(sp),
        }
    } else {
        None
    };
    let mut l: Vec<(&'static str, Rc<super::color::Spec>)> = s.color_hp.as_ref().map(|l| l.iter().filter(|(n, _)| *n != key).cloned().collect()).unwrap_or_default();
    if let Some(sp) = keep {
        l.push((key, Rc::new(sp)));
    }
    s.color_hp = if l.is_empty() { None } else { Some(Rc::new(l)) };
}

pub fn apply(s: &mut ComputedStyle, parent: &ComputedStyle, name: &str, v: &[CV], cx: &ApplyCtx) {
    let v = trim(v);
    if v.is_empty() {
        return;
    }
    if let Some(key) = super::pv::engine_routed(name) {
        note_generic(s, parent, key, v);
    }
    if is_passthru(name) {
        match css_wide(v) {
            Some("inherit") => {
                for n in if name == "overscroll-behavior" { &["overscroll-behavior-x", "overscroll-behavior-y"][..] } else { std::slice::from_ref(&name) } {
                    let pv = passthru_value(parent, n).filter(|pv| Some(pv.as_str()) != PASSTHRU.iter().find(|(k, _)| k == n).map(|x| x.1));
                    set_passthru(s, n, pv);
                }
            }
            Some(_) => {
                for n in if name == "overscroll-behavior" { &["overscroll-behavior-x", "overscroll-behavior-y"][..] } else { std::slice::from_ref(&name) } {
                    set_passthru(s, n, None);
                }
            }
            None => {
                let text = passthru_text(v);
                if name == "overscroll-behavior" {
                    let parts: Vec<&str> = text.split(' ').collect();
                    let (x, y) = (parts[0].to_string(), parts.get(1).unwrap_or(&parts[0]).to_string());
                    set_passthru(s, "overscroll-behavior-x", Some(x));
                    set_passthru(s, "overscroll-behavior-y", Some(y));
                } else {
                    set_passthru(s, name, Some(text));
                }
            }
        }
    }
    if name.contains("animation") && super::anim::apply_animation_prop(s, name, v) {
        return;
    }
    if name.contains("transition") && super::anim::apply_transition_prop(s, name, v) {
        return;
    }
    if name == "all" {
        // `all` resets every property except direction/unicode-bidi and custom properties.
        if let Some(kw) = css_wide(v) {
            let (dir, custom, bidi) = (s.direction_rtl, s.custom.clone(), s.bidi_override);
            *s = match kw {
                "inherit" => parent.clone(),
                "initial" => {
                    let mut i = INITIAL.with(|i| i.clone());
                    // Chromium's initial font-family is its default fixed font.
                    i.font_family = Rc::new(vec!["monospace".to_string()]);
                    i
                }
                _ => ComputedStyle::inherit(parent),
            };
            s.direction_rtl = dir;
            s.custom = custom;
            s.bidi_override = bidi;
        }
        return;
    }
    if let Some(kw) = css_wide(v) {
        match kw {
            "inherit" => copy_prop(s, parent, name),
            "initial" => INITIAL.with(|i| copy_prop(s, i, name)),
            _ => {
                if is_inherited(name) {
                    copy_prop(s, parent, name)
                } else {
                    INITIAL.with(|i| copy_prop(s, i, name))
                }
            }
        }
        return;
    }
    if COLOR_HP_PROPS.contains(&name) {
        note_color_hp(s, name, v);
    }
    let u = cx.units(s);
    let w = words(v);
    let first = w[0];
    let id = ident_lc(v);
    let id = id.as_deref();
    let len1 = || parse_len(first, &u);

    match name {
        "display" => {
            let kws: Vec<String> = w.iter().filter_map(|c| c.ident().map(|s| s.to_ascii_lowercase())).collect();
            let has = |k: &str| kws.iter().any(|x| x == k);
            let d = if has("none") {
                Display::None
            } else if has("contents") {
                Display::Contents
            } else if has("list-item") {
                Display::ListItem
            } else if has("flow-root") {
                if has("inline") {
                    Display::InlineBlock
                } else {
                    Display::FlowRoot
                }
            } else if has("inline-block") {
                Display::InlineBlock
            } else if has("inline-flex") || has("-webkit-inline-box") || has("-webkit-inline-flex") || (has("inline") && has("flex")) {
                Display::InlineFlex
            } else if has("flex") || has("-webkit-box") || has("-webkit-flex") || has("-ms-flexbox") {
                Display::Flex
            } else if has("inline-grid") || (has("inline") && has("grid")) {
                Display::InlineGrid
            } else if has("grid") || has("-ms-grid") {
                Display::Grid
            } else if has("inline-table") {
                Display::InlineTable
            } else if has("table") {
                Display::Table
            } else if has("table-row-group") {
                Display::TableRowGroup
            } else if has("table-header-group") {
                Display::TableHeaderGroup
            } else if has("table-footer-group") {
                Display::TableFooterGroup
            } else if has("table-row") {
                Display::TableRow
            } else if has("table-cell") {
                Display::TableCell
            } else if has("table-caption") {
                Display::TableCaption
            } else if has("table-column") {
                Display::TableColumn
            } else if has("table-column-group") {
                Display::TableColumnGroup
            } else if has("inline") || has("ruby") || has("ruby-text") || has("run-in") {
                Display::Inline
            } else if has("block") {
                Display::Block
            } else {
                return;
            };
            s.display = d;
            s.legacy_box = has("-webkit-box") || has("-webkit-inline-box");
        }
        "position" => {
            s.position = match id {
                Some("static") => Position::Static,
                Some("relative") => Position::Relative,
                Some("absolute") => Position::Absolute,
                Some("fixed") => Position::Fixed,
                Some("sticky") | Some("-webkit-sticky") => Position::Sticky,
                _ => return,
            }
        }
        "float" => {
            s.float = match id {
                Some("left") | Some("inline-start") => Float::Left,
                Some("right") | Some("inline-end") => Float::Right,
                Some("none") => Float::None,
                _ => return,
            }
        }
        "clear" => {
            s.clear = match id {
                Some("left") | Some("inline-start") => Clear::Left,
                Some("right") | Some("inline-end") => Clear::Right,
                Some("both") => Clear::Both,
                Some("none") => Clear::None,
                _ => return,
            }
        }
        "top" | "right" | "bottom" | "left" | "inset-block-start" | "inset-block-end" | "inset-inline-start" | "inset-inline-end" => {
            if let (Some(l), Some(i)) = (len1(), side_index(name, s.direction_rtl).or(match name {
                "top" => Some(0),
                "right" => Some(1),
                "bottom" => Some(2),
                "left" => Some(3),
                _ => None,
            })) {
                s.inset[i] = l;
            }
        }
        "inset" | "inset-block" | "inset-inline" => {
            let ls: Option<Vec<Len>> = w.iter().map(|c| parse_len(c, &u)).collect();
            if let Some(ls) = ls {
                match name {
                    "inset" => {
                        if let Some(f) = four(ls) {
                            s.inset = f;
                        }
                    }
                    "inset-block" => {
                        s.inset[0] = ls[0].clone();
                        s.inset[2] = ls.get(1).cloned().unwrap_or(ls[0].clone());
                    }
                    _ => {
                        let (a, b) = (ls[0].clone(), ls.get(1).cloned().unwrap_or(ls[0].clone()));
                        if s.direction_rtl {
                            s.inset[1] = a;
                            s.inset[3] = b;
                        } else {
                            s.inset[3] = a;
                            s.inset[1] = b;
                        }
                    }
                }
            }
        }
        "width" | "inline-size" => {
            if let Some(l) = len1() {
                if !l.is_none() {
                    s.width = l
                }
            }
        }
        "height" | "block-size" => {
            if let Some(l) = len1() {
                if !l.is_none() {
                    s.height = l
                }
            }
        }
        "min-width" | "min-inline-size" => {
            if let Some(l) = len1() {
                s.min_width = l
            }
        }
        "min-height" | "min-block-size" => {
            if let Some(l) = len1() {
                s.min_height = l
            }
        }
        "max-width" | "max-inline-size" => {
            if let Some(l) = len1() {
                s.max_width = if l.is_auto() { Len::None } else { l }
            }
        }
        "max-height" | "max-block-size" => {
            if let Some(l) = len1() {
                s.max_height = if l.is_auto() { Len::None } else { l }
            }
        }
        "margin" | "padding" => {
            let ls: Option<Vec<Len>> = w.iter().map(|c| parse_len(c, &u)).collect();
            if let Some(f) = ls.and_then(four) {
                if name == "margin" {
                    s.margin = f;
                } else {
                    s.padding = f;
                }
            }
        }
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" | "margin-block-start" | "margin-block-end"
        | "margin-inline-start" | "margin-inline-end" => {
            if let (Some(l), Some(i)) = (len1(), side_index(name, s.direction_rtl)) {
                s.margin[i] = l;
            }
        }
        "margin-block" | "margin-inline" | "padding-block" | "padding-inline" => {
            let ls: Option<Vec<Len>> = w.iter().map(|c| parse_len(c, &u)).collect();
            if let Some(ls) = ls {
                let a = ls[0].clone();
                let b = ls.get(1).cloned().unwrap_or(a.clone());
                let rtl = s.direction_rtl;
                let arr = if name.starts_with("margin") { &mut s.margin } else { &mut s.padding };
                if name.ends_with("block") {
                    arr[0] = a;
                    arr[2] = b;
                } else if rtl {
                    arr[1] = a;
                    arr[3] = b;
                } else {
                    arr[3] = a;
                    arr[1] = b;
                }
            }
        }
        "padding-top" | "padding-right" | "padding-bottom" | "padding-left" | "padding-block-start" | "padding-block-end"
        | "padding-inline-start" | "padding-inline-end" => {
            if let (Some(l), Some(i)) = (len1(), side_index(name, s.direction_rtl)) {
                if !l.is_auto() {
                    s.padding[i] = l;
                }
            }
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "border-block" | "border-inline"
        | "border-block-start" | "border-block-end" | "border-inline-start" | "border-inline-end" => {
            let mut width = 3.0;
            let mut style = BorderStyle::None;
            let mut color = ColorValue::CurrentColor;
            for c in &w {
                if let Some(bs) = c.ident().and_then(|i| parse_border_style(&i.to_ascii_lowercase())) {
                    style = bs;
                } else if let Some(bw) = parse_border_width(c, &u) {
                    width = bw;
                } else if let Some(col) = parse_color(c) {
                    color = col;
                } else {
                    return;
                }
            }
            let sides: Vec<usize> = match name {
                "border" => vec![0, 1, 2, 3],
                "border-block" => vec![0, 2],
                "border-inline" => vec![1, 3],
                n => vec![side_index(n, s.direction_rtl).unwrap_or(0)],
            };
            for i in sides {
                s.border_width[i] = width;
                s.border_style[i] = style;
                s.border_color[i] = color;
            }
        }
        "border-width" => {
            let ws: Option<Vec<f32>> = w.iter().map(|c| parse_border_width(c, &u)).collect();
            if let Some(f) = ws.and_then(four) {
                s.border_width = f;
            }
        }
        "border-style" => {
            let ws: Option<Vec<BorderStyle>> = w.iter().map(|c| c.ident().and_then(|i| parse_border_style(&i.to_ascii_lowercase()))).collect();
            if let Some(f) = ws.and_then(four) {
                s.border_style = f;
            }
        }
        "border-color" => {
            let ws: Option<Vec<ColorValue>> = w.iter().map(|c| parse_color(c)).collect();
            if let Some(f) = ws.and_then(four) {
                s.border_color = f;
            }
        }
        _ if name.starts_with("border-") && (name.ends_with("-width") || name.ends_with("-style") || name.ends_with("-color")) && !name.contains("radius") => {
            let side = &name[7..name.rfind('-').unwrap_or(name.len())];
            let i = match side {
                "top" | "block-start" => 0,
                "right" | "inline-end" => 1,
                "bottom" | "block-end" => 2,
                "left" | "inline-start" => 3,
                "block" | "inline" => {
                    let idx: [usize; 2] = if side == "block" { [0, 2] } else { [1, 3] };
                    for i in idx {
                        if name.ends_with("-width") {
                            if let Some(bw) = parse_border_width(first, &u) {
                                s.border_width[i] = bw;
                            }
                        } else if name.ends_with("-style") {
                            if let Some(bs) = id.and_then(parse_border_style) {
                                s.border_style[i] = bs;
                            }
                        } else if let Some(c) = parse_color(first) {
                            s.border_color[i] = c;
                        }
                    }
                    return;
                }
                _ => return,
            };
            if name.ends_with("-width") {
                if let Some(bw) = parse_border_width(first, &u) {
                    s.border_width[i] = bw;
                }
            } else if name.ends_with("-style") {
                if let Some(bs) = id.and_then(parse_border_style) {
                    s.border_style[i] = bs;
                }
            } else if let Some(c) = parse_color(first) {
                s.border_color[i] = c;
            }
        }
        "border-radius" => {
            let mut halves = v.splitn(2, |c| c.is_delim('/'));
            let h: Option<Vec<Len>> = words(halves.next().unwrap_or(&[])).iter().map(|c| parse_len(c, &u)).collect();
            let vv: Option<Option<Vec<Len>>> = halves.next().map(|p| words(p).iter().map(|c| parse_len(c, &u)).collect());
            let h = match h.and_then(four) {
                Some(h) => h,
                None => return,
            };
            let vv = match vv {
                Some(Some(x)) => four(x).unwrap_or(h.clone()),
                Some(None) => return,
                None => h.clone(),
            };
            // four() yields [tl, tr, br, bl] ordering for radius too.
            for i in 0..4 {
                s.border_radius[i] = (h[i].clone(), vv[i].clone());
            }
        }
        "border-top-left-radius" | "border-top-right-radius" | "border-bottom-right-radius" | "border-bottom-left-radius"
        | "border-start-start-radius" | "border-start-end-radius" | "border-end-end-radius" | "border-end-start-radius" => {
            let rtl = s.direction_rtl;
            let i = match name {
                "border-top-left-radius" => 0,
                "border-top-right-radius" => 1,
                "border-bottom-right-radius" => 2,
                "border-bottom-left-radius" => 3,
                "border-start-start-radius" => if rtl { 1 } else { 0 },
                "border-start-end-radius" => if rtl { 0 } else { 1 },
                "border-end-end-radius" => if rtl { 3 } else { 2 },
                _ => if rtl { 2 } else { 3 },
            };
            let a = parse_len(first, &u);
            let b = w.get(1).and_then(|c| parse_len(c, &u));
            if let Some(a) = a {
                s.border_radius[i] = (a.clone(), b.unwrap_or(a));
            }
        }
        "box-sizing" | "-webkit-box-sizing" | "-moz-box-sizing" => {
            s.box_sizing = match id {
                Some("border-box") => BoxSizing::BorderBox,
                Some("content-box") => BoxSizing::ContentBox,
                _ => return,
            }
        }
        "overflow" | "overflow-x" | "overflow-y" | "overflow-block" | "overflow-inline" => {
            let parse = |c: &CV| -> Option<Overflow> {
                Some(match c.ident()?.to_ascii_lowercase().as_str() {
                    "visible" => Overflow::Visible,
                    "hidden" => Overflow::Hidden,
                    "clip" => Overflow::Clip,
                    "scroll" => Overflow::Scroll,
                    "auto" | "overlay" => Overflow::Auto,
                    _ => return None,
                })
            };
            let a = match parse(first) {
                Some(a) => a,
                None => return,
            };
            match name {
                "overflow" => {
                    s.overflow_x = a;
                    s.overflow_y = w.get(1).and_then(|c| parse(c)).unwrap_or(a);
                }
                "overflow-x" | "overflow-inline" => s.overflow_x = a,
                _ => s.overflow_y = a,
            }
        }
        "z-index" => {
            // calc() included: Tailwind writes `-z-[1]` as `calc(1 * -1)`.
            s.z_index = parse_number(first).map(|n| n.round() as i32)
        }
        "mix-blend-mode" => {
            if let Some(m) = first.ident().and_then(|i| blend_mode(&i.to_ascii_lowercase())) {
                s.mix_blend = m;
            }
        }
        "resize" => {
            if let Some(i) = first.ident() {
                s.resize = !i.eq_ignore_ascii_case("none");
            }
        }
        "border-image" | "border-image-source" | "border-image-slice" | "border-image-width" | "border-image-outset" | "border-image-repeat" => {
            let mut bi = if name == "border-image" { BorderImage::default() } else { s.border_image.as_deref().cloned().unwrap_or_default() };
            if parse_border_image(name, &w, &u, &mut bi).is_some() {
                s.border_image = if bi.source == BgImage::None { None } else { Some(Rc::new(bi)) };
                if name != "border-image" && name != "border-image-source" && bi_none(&s.border_image) {
                    // Longhands other than the source keep their values for a later source.
                }
            }
        }
        "color-scheme" => {
            // Dark only when dark is offered and light is not (the user's
            // preference, light here, decides between both).
            let ids: Vec<String> = v.iter().filter_map(|c| c.ident().map(|i| i.to_ascii_lowercase())).collect();
            if !ids.is_empty() {
                s.dark_scheme = ids.iter().any(|i| i == "dark") && !ids.iter().any(|i| i == "light");
            }
        }
        "isolation" => {
            if let Some(i) = first.ident() {
                s.isolate = i.eq_ignore_ascii_case("isolate");
            }
        }
        "background-blend-mode" => {
            let modes: Option<Vec<u8>> = split_commas(v).iter().map(|p| words(p).first().and_then(|c| c.ident()).and_then(|i| blend_mode(&i.to_ascii_lowercase()))).collect();
            if let Some(m) = modes {
                s.bg_blend = if m.iter().all(|&x| x == 0) { None } else { Some(Rc::new(m)) };
            }
        }
        "opacity" => {
            if let Some(n) = parse_number(first).or(match first {
                CV::T(Tok::Percentage(p)) => Some((p / 100.0) as f32),
                _ => None,
            }) {
                s.opacity = n.clamp(0.0, 1.0);
            }
        }
        "background-color" => {
            if let Some(c) = parse_color(first) {
                s.background_color = c;
            }
        }
        "background" => {
            if let Some((color, layers)) = parse_background(v, &u) {
                s.background_color = color.unwrap_or(ColorValue::Rgba(Color::TRANSPARENT));
                let keep = layers.iter().any(|l| l.image != BgImage::None || l.clip != 0);
                s.background = if keep { Some(Rc::new(layers)) } else { None };
            }
        }
        "background-image" => {
            let mut layers: Vec<BgLayer> = s.background.as_ref().map(|b| (**b).clone()).unwrap_or_default();
            let parts = split_commas(v);
            let mut imgs = Vec::new();
            for p in &parts {
                match words(p).first().and_then(|c| parse_image(c, &u)) {
                    Some(i) => imgs.push(i),
                    None => return,
                }
            }
            // New layers repeat the existing longhand values cyclically
            // (lists shorter than the image list repeat).
            let n = layers.len();
            for j in n..imgs.len() {
                let l = if n > 0 { layers[j % n].clone() } else { BgLayer::default() };
                layers.push(l);
            }
            layers.truncate(imgs.len());
            for (l, i) in layers.iter_mut().zip(imgs) {
                l.image = i;
            }
            s.background = if layers.iter().any(|l| l.image != BgImage::None) { Some(Rc::new(layers)) } else { None };
        }
        "background-size" | "background-position" | "background-repeat" | "background-position-x" | "background-position-y" | "background-clip" | "-webkit-background-clip" | "background-origin" | "background-attachment" => {
            let mut layers: Vec<BgLayer> = match &s.background {
                Some(b) => (**b).clone(),
                None => vec![BgLayer::default()],
            };
            let parts = split_commas(v);
            if parts.is_empty() {
                return;
            }
            // A list shorter than the number of layers repeats.
            for i in 0..layers.len() {
                let wv = words(&parts[i % parts.len()]);
                let l = &mut layers[i];
                match name {
                    "background-size" => {
                        if let Some(sz) = parse_bg_size(&wv, &u) {
                            l.size = sz;
                        }
                    }
                    "background-position" => {
                        if let Some(pp) = parse_position_pair(&wv, &u) {
                            l.pos = pp;
                        }
                    }
                    "background-position-x" => {
                        if let Some(pp) = parse_position_pair(&wv, &u) {
                            l.pos.0 = pp.0;
                        }
                    }
                    "background-position-y" => {
                        if let Some(pp) = parse_position_pair(&wv, &u) {
                            l.pos.1 = if wv.len() == 1 && wv[0].ident().is_none() { pp.0 } else { pp.1 };
                        }
                    }
                    "background-repeat" => {
                        let r: Vec<((bool, bool), (u8, u8))> = wv.iter().filter_map(|c| c.ident().and_then(|s| parse_repeat(&s.to_ascii_lowercase()))).collect();
                        if let Some(r) = repeat_pair(&r) {
                            l.repeat = r.0;
                            l.rmode = r.1;
                        }
                    }
                    "background-attachment" => {
                        l.fixed = wv.first().map(|c| c.is_ident("fixed")).unwrap_or(false);
                    }
                    "background-origin" => {
                        if let Some(bx) = wv.first().and_then(|c| c.ident()).and_then(|i| bg_box(&i.to_ascii_lowercase())) {
                            l.origin = bx;
                        }
                    }
                    _ => {
                        l.clip_text = wv.first().map(|c| c.is_ident("text")).unwrap_or(false);
                        if let Some(bx) = wv.first().and_then(|c| c.ident()).and_then(|i| bg_box(&i.to_ascii_lowercase())) {
                            l.clip = bx;
                        }
                    }
                }
            }
            s.background = Some(Rc::new(layers));
        }
        "box-shadow" | "-webkit-box-shadow" => {
            if let Some(sh) = parse_shadow_list(v, &u) {
                s.box_shadow = sh;
            }
        }
        "text-shadow" => {
            if let Some(sh) = parse_shadow_list(v, &u) {
                s.text_shadow = sh;
            }
        }
        "outline" => {
            let mut width = 3.0;
            let mut style = BorderStyle::None;
            let mut color = ColorValue::CurrentColor;
            for c in &w {
                if c.is_ident("auto") {
                    style = BorderStyle::Solid;
                    width = 2.0;
                } else if let Some(bs) = c.ident().and_then(|i| parse_border_style(&i.to_ascii_lowercase())) {
                    style = bs;
                } else if let Some(bw) = parse_border_width(c, &u) {
                    width = bw;
                } else if let Some(col) = parse_color(c) {
                    color = col;
                } else {
                    return;
                }
            }
            s.outline_width = width;
            s.outline_style = style;
            s.outline_color = color;
        }
        "outline-width" => {
            if let Some(bw) = parse_border_width(first, &u) {
                s.outline_width = bw;
            }
        }
        "outline-style" => {
            if id == Some("auto") {
                s.outline_style = BorderStyle::Solid;
            } else if let Some(bs) = id.and_then(parse_border_style) {
                s.outline_style = bs;
            }
        }
        "outline-color" => {
            if let Some(c) = parse_color(first) {
                s.outline_color = c;
            }
        }
        "outline-offset" => {
            if let Some(l) = len1() {
                s.outline_offset = l.resolve(0.0);
            }
        }
        "filter" | "-webkit-filter" => {
            if let Some(f) = parse_filter(v, &u) {
                s.filter = f;
            }
        }
        "backdrop-filter" | "-webkit-backdrop-filter" => {
            if let Some(f) = parse_filter(v, &u) {
                s.backdrop_filter = f;
            }
        }
        "transform" | "-webkit-transform" | "-ms-transform" => {
            if let Some(t) = parse_transform(v, &u) {
                s.transform = t;
            }
        }
        // Individual transforms: each is a single property (later declarations
        // replace earlier ones); they are composed with `transform` in fixup.
        "translate" => {
            if id == Some("none") {
                s.translate_prop = None;
                return;
            }
            let a = parse_len(first, &u);
            let b = w.get(1).and_then(|c| parse_len(c, &u)).unwrap_or(Len::zero());
            let z = match w.get(2) {
                Some(c) => match parse_len(c, &u).and_then(|l| l.definite(None)) {
                    Some(z) => z,
                    None => return,
                },
                None => 0.0,
            };
            if let Some(a) = a {
                s.translate_prop = Some((a, b, z));
            }
        }
        "rotate" => {
            if id == Some("none") {
                s.rotate_prop = None;
                return;
            }
            // `rotate: <angle>`, `rotate: x|y|z <angle>` or `<number>{3} <angle>` (either order).
            // The angle: a dimension (a unitless 0 is an axis component when three numbers precede it).
            let ai = w.iter().position(|c| !matches!(c, CV::T(Tok::Number(_))) && angle_deg(c).is_some()).or_else(|| w.iter().rposition(|c| angle_deg(c).is_some()));
            let Some(ai) = ai else { return };
            let Some(a) = angle_deg(&w[ai]) else { return };
            let nums: Vec<f32> = w.iter().enumerate().filter(|(i, _)| *i != ai).filter_map(|(_, c)| parse_number(c)).collect();
            let axis = match w.iter().find_map(|c| c.ident()).map(|i| i.to_ascii_lowercase()).as_deref() {
                Some("x") => (1.0, 0.0, 0.0),
                Some("y") => (0.0, 1.0, 0.0),
                Some("z") | None if nums.len() != 3 => (0.0, 0.0, 1.0),
                None => (nums[0], nums[1], nums[2]),
                _ => return,
            };
            s.rotate_prop = Some((axis.0, axis.1, axis.2, a));
        }
        "scale" => {
            if id == Some("none") {
                s.scale_prop = None;
                return;
            }
            let num = |c: &CV| match c {
                CV::T(Tok::Percentage(p)) => Some((p / 100.0) as f32),
                c => parse_number(c),
            };
            if let Some(x) = num(first) {
                let y = w.get(1).and_then(|c| num(c)).unwrap_or(x);
                let z = w.get(2).and_then(|c| num(c)).unwrap_or(1.0);
                s.scale_prop = Some((x, y, z));
            }
        }
        "mask" | "-webkit-mask" => {
            if id == Some("none") {
                s.mask = None;
                s.mask_more = None;
            } else if let Some((_, layers)) = parse_background(v, &u) {
                let mut it = layers.into_iter();
                s.mask = it.next().filter(|l| l.image != BgImage::None).map(Rc::new);
                let more: Vec<BgLayer> = it.filter(|l| l.image != BgImage::None).collect();
                s.mask_more = if more.is_empty() || s.mask.is_none() { None } else { Some(Rc::new(more)) };
            }
        }
        "mask-image" | "-webkit-mask-image" => {
            if id == Some("none") {
                s.mask = None;
                s.mask_more = None;
            } else if let Some(img) = split_commas(v).first().and_then(|p| words(p).first().and_then(|c| parse_image(c, &u))) {
                let mut l = s.mask.as_ref().map(|m| (**m).clone()).unwrap_or_default();
                l.image = img;
                s.mask = Some(Rc::new(l));
                // Further images: more layers (sizes etc. come from their longhands).
                let rest: Vec<BgImage> = split_commas(v).iter().skip(1).filter_map(|p| words(p).first().and_then(|c| parse_image(c, &u))).collect();
                if rest.is_empty() {
                    s.mask_more = None;
                } else {
                    let mut more: Vec<BgLayer> = s.mask_more.as_ref().map(|m| (**m).clone()).unwrap_or_default();
                    more.resize_with(rest.len(), BgLayer::default);
                    for (l, img) in more.iter_mut().zip(rest) {
                        l.image = img;
                    }
                    s.mask_more = Some(Rc::new(more));
                }
            }
        }
        "mask-size" | "-webkit-mask-size" | "mask-position" | "-webkit-mask-position" | "mask-repeat" | "-webkit-mask-repeat" => {
            let parts = split_commas(v);
            let apply = |l: &mut BgLayer, part: &[CV]| {
                let wv = words(part);
                match name.trim_start_matches("-webkit-") {
                    "mask-size" => {
                        if let Some(sz) = parse_bg_size(&wv, &u) {
                            l.size = sz;
                        }
                    }
                    "mask-position" => {
                        if let Some(pp) = parse_position_pair(&wv, &u) {
                            l.pos = pp;
                        }
                    }
                    _ => {
                        let r: Vec<((bool, bool), (u8, u8))> = wv.iter().filter_map(|c| c.ident().and_then(|s| parse_repeat(&s.to_ascii_lowercase()))).collect();
                        if let Some(r) = repeat_pair(&r) {
                            l.repeat = r.0;
                            l.rmode = r.1;
                        }
                    }
                }
            };
            let mut l = s.mask.as_ref().map(|m| (**m).clone()).unwrap_or_default();
            apply(&mut l, parts.first().copied().unwrap_or(&[]));
            s.mask = Some(Rc::new(l));
            // Values repeat over the further layers.
            if let (Some(more), false) = (s.mask_more.as_ref(), parts.is_empty()) {
                let mut more = (**more).clone();
                for (i, ml) in more.iter_mut().enumerate() {
                    apply(ml, parts[(i + 1) % parts.len()]);
                }
                s.mask_more = Some(Rc::new(more));
            }
        }
        "appearance" | "-webkit-appearance" | "-moz-appearance" => {
            if let Some(i) = id {
                s.appearance_none = i.eq_ignore_ascii_case("none");
            }
        }
        "shape-outside" | "-webkit-shape-outside" => {
            if id == Some("none") {
                s.shape_outside = None;
                return;
            }
            let mut shape = None;
            let mut refbox = None;
            for c in &w {
                match c {
                    CV::Func(n, args) => match parse_basic_shape(&n.to_ascii_lowercase(), args, &u) {
                        Some(sh) => shape = Some(sh),
                        None => return,
                    },
                    c => match c.ident().map(|i| i.to_ascii_lowercase()).as_deref() {
                        Some("border-box") => refbox = Some(0),
                        Some("padding-box") => refbox = Some(1),
                        Some("content-box") => refbox = Some(2),
                        Some("margin-box") => refbox = Some(3),
                        _ => return,
                    },
                }
            }
            s.shape_outside = match (shape, refbox) {
                (Some(sh), r) => Some(Rc::new(ShapeOutside::Shape(sh, r.unwrap_or(3)))),
                (None, Some(r)) => Some(Rc::new(ShapeOutside::Box(r))),
                (None, None) => return,
            };
        }
        "image-rendering" | "-ms-interpolation-mode" => {
            s.pixelated = match id {
                Some("pixelated") | Some("crisp-edges") | Some("-moz-crisp-edges") | Some("-webkit-optimize-contrast") | Some("optimizespeed") | Some("optimize-contrast") | Some("nearest-neighbor") => true,
                Some("auto") | Some("smooth") | Some("high-quality") | Some("optimizequality") | Some("bicubic") => false,
                _ => return,
            }
        }
        "shape-margin" | "-webkit-shape-margin" => {
            if let Some(l) = parse_len(first, &u) {
                s.shape_margin = l;
            }
        }
        "clip-path" | "-webkit-clip-path" => {
            if id == Some("none") {
                s.clip_shape = None;
                return;
            }
            // <basic-shape> || <geometry-box>
            let mut shape = None;
            let mut refbox = 0u8;
            for c in &w {
                match c {
                    CV::Func(n, args) => shape = parse_basic_shape(&n.to_ascii_lowercase(), args, &u),
                    c => match c.ident().map(|i| i.to_ascii_lowercase()).as_deref() {
                        Some("border-box") | Some("stroke-box") | Some("view-box") => refbox = 0,
                        Some("padding-box") | Some("fill-box") => refbox = 1,
                        Some("content-box") => refbox = 2,
                        Some("margin-box") => refbox = 3,
                        _ => return,
                    },
                }
            }
            match shape {
                Some(sh) => s.clip_shape = Some(Rc::new(ClipShape::Shape(sh, refbox))),
                // A box alone clips to that box.
                None => s.clip_shape = Some(Rc::new(ClipShape::Shape(BasicShape::Inset([Len::zero(), Len::zero(), Len::zero(), Len::zero()], None), refbox))),
            }
        }
        "clip" => {
            if id == Some("auto") {
                s.clip_shape = None;
            } else if let CV::Func(n, args) = first {
                if n.eq_ignore_ascii_case("rect") {
                    let vals: Vec<&CV> = words(args).into_iter().filter(|c| !c.is_comma()).collect();
                    if vals.len() == 4 {
                        let e: Vec<Option<Len>> = vals.iter().map(|c| if c.is_ident("auto") { None } else { parse_len(c, &u) }).collect();
                        s.clip_shape = Some(Rc::new(ClipShape::Rect([e[0].clone(), e[1].clone(), e[2].clone(), e[3].clone()])));
                    }
                }
            }
        }
        "transform-origin" | "-webkit-transform-origin" => {
            if let Some(p) = parse_position_pair(&w[..w.len().min(2)], &u) {
                s.transform_origin = p;
                s.transform_origin_z = w.get(2).and_then(|c| parse_len(c, &u)).and_then(|l| l.definite(None)).unwrap_or(0.0);
            }
        }
        "perspective" | "-webkit-perspective" => {
            if id == Some("none") {
                s.perspective = None;
            } else if let Some(d) = parse_len(first, &u).and_then(|l| l.definite(None)) {
                if d >= 0.0 {
                    s.perspective = Some(d.max(1.0));
                }
            }
        }
        "perspective-origin" | "-webkit-perspective-origin" => {
            if let Some(p) = parse_position_pair(&w[..w.len().min(2)], &u) {
                s.perspective_origin = p;
            }
        }
        "transform-style" | "-webkit-transform-style" => match id {
            Some("preserve-3d") => s.preserve_3d = true,
            Some("flat") => s.preserve_3d = false,
            _ => {}
        },
        "backface-visibility" | "-webkit-backface-visibility" => match id {
            Some("hidden") => s.backface_hidden = true,
            Some("visible") => s.backface_hidden = false,
            _ => {}
        },
        "writing-mode" | "-webkit-writing-mode" | "-ms-writing-mode" => {
            s.writing_mode = match id {
                Some("horizontal-tb") | Some("lr") | Some("lr-tb") | Some("rl") | Some("rl-tb") => 0,
                Some("vertical-rl") | Some("tb") | Some("tb-rl") => 1,
                Some("vertical-lr") => 2,
                Some("sideways-rl") => 3,
                Some("sideways-lr") => 4,
                _ => return,
            }
        }
        "text-orientation" | "-webkit-text-orientation" => {
            s.text_orientation = match id {
                Some("mixed") => 0,
                Some("upright") => 1,
                Some("sideways") | Some("sideways-right") => 2,
                _ => return,
            }
        }
        "text-combine-upright" | "-webkit-text-combine" => match id {
            Some("all") | Some("horizontal") => s.text_combine = true,
            Some("none") => s.text_combine = false,
            _ => {}
        },
        "aspect-ratio" => {
            let nums: Vec<f32> = w.iter().filter_map(|c| parse_number(c)).collect();
            s.aspect_auto = w.iter().any(|c| c.ident().is_some_and(|i| i.eq_ignore_ascii_case("auto")));
            s.aspect_ratio = match nums.len() {
                1 if nums[0] > 0.0 => Some(nums[0]),
                2 if nums[1] > 0.0 => Some(nums[0] / nums[1]),
                _ => if id == Some("auto") { None } else { s.aspect_ratio },
            };
        }
        "object-fit" => {
            s.object_fit = match id {
                Some("fill") => ObjectFit::Fill,
                Some("contain") => ObjectFit::Contain,
                Some("cover") => ObjectFit::Cover,
                Some("none") => ObjectFit::None,
                Some("scale-down") => ObjectFit::ScaleDown,
                _ => return,
            }
        }
        "object-position" => {
            if let Some(p) = parse_position_pair(&w, &u) {
                s.object_position = p;
            }
        }
        "content" => {
            if let Some(c) = parse_content(v) {
                s.content = c;
            }
        }
        "text-decoration" | "text-decoration-line" | "-webkit-text-decoration" | "-webkit-text-decoration-line" => {
            let mut lines = 0u8;
            for c in &w {
                match c.ident().map(|s| s.to_ascii_lowercase()).as_deref() {
                    Some("underline") => lines |= DECO_UNDERLINE,
                    Some("overline") => lines |= DECO_OVERLINE,
                    Some("line-through") => lines |= DECO_LINE_THROUGH,
                    Some("none") => lines = 0,
                    Some("blink") => {}
                    Some(st) if name.starts_with("text-decoration") && parse_border_style(st).is_some() => {
                        s.text_decoration_style = parse_border_style(st).unwrap();
                    }
                    Some("wavy") => s.text_decoration_style = BorderStyle::Dashed,
                    _ => {
                        if let Some(col) = parse_color(c) {
                            s.text_decoration_color = col;
                        } else if let Some(l) = parse_len(c, &u) {
                            s.text_decoration_thickness = Some(l.resolve(s.font_size));
                        }
                    }
                }
            }
            s.text_decoration_line = lines;
        }
        "text-decoration-color" | "-webkit-text-decoration-color" => {
            if let Some(c) = parse_color(first) {
                s.text_decoration_color = c;
            }
        }
        "text-decoration-style" => {
            if let Some(bs) = id.and_then(parse_border_style) {
                s.text_decoration_style = bs;
            }
        }
        "text-decoration-thickness" => {
            if let Some(l) = len1() {
                s.text_decoration_thickness = if l.is_auto() { None } else { Some(l.resolve(s.font_size)) };
            }
        }
        "text-overflow" => s.text_overflow_ellipsis = id == Some("ellipsis"),
        "vertical-align" => {
            s.vertical_align = match id {
                Some("baseline") => VerticalAlign::Baseline,
                Some("top") => VerticalAlign::Top,
                Some("middle") => VerticalAlign::Middle,
                Some("bottom") => VerticalAlign::Bottom,
                Some("text-top") => VerticalAlign::TextTop,
                Some("text-bottom") => VerticalAlign::TextBottom,
                Some("sub") => VerticalAlign::Sub,
                Some("super") => VerticalAlign::Super,
                _ => match len1() {
                    Some(Len::Pct(p)) => VerticalAlign::Len(Len::Px(p / 100.0 * s.line_height_px())),
                    Some(l) => VerticalAlign::Len(l),
                    None => return,
                },
            }
        }
        "flex-direction" | "-webkit-flex-direction" => {
            s.flex_direction = match id {
                Some("row") => FlexDirection::Row,
                Some("row-reverse") => FlexDirection::RowReverse,
                Some("column") => FlexDirection::Column,
                Some("column-reverse") => FlexDirection::ColumnReverse,
                _ => return,
            }
        }
        "flex-wrap" | "-webkit-flex-wrap" => {
            s.flex_wrap = match id {
                Some("nowrap") => FlexWrap::NoWrap,
                Some("wrap") => FlexWrap::Wrap,
                Some("wrap-reverse") => FlexWrap::WrapReverse,
                _ => return,
            }
        }
        "flex-flow" | "-webkit-flex-flow" => {
            // Omitted longhands reset to their initial values; anything invalid drops the declaration.
            let (mut dir, mut wrap) = (None, None);
            for c in &w {
                match c.ident().map(|s| s.to_ascii_lowercase()).as_deref() {
                    Some("row") if dir.is_none() => dir = Some(FlexDirection::Row),
                    Some("row-reverse") if dir.is_none() => dir = Some(FlexDirection::RowReverse),
                    Some("column") if dir.is_none() => dir = Some(FlexDirection::Column),
                    Some("column-reverse") if dir.is_none() => dir = Some(FlexDirection::ColumnReverse),
                    Some("nowrap") if wrap.is_none() => wrap = Some(FlexWrap::NoWrap),
                    Some("wrap") if wrap.is_none() => wrap = Some(FlexWrap::Wrap),
                    Some("wrap-reverse") if wrap.is_none() => wrap = Some(FlexWrap::WrapReverse),
                    _ => return,
                }
            }
            if dir.is_none() && wrap.is_none() {
                return;
            }
            s.flex_direction = dir.unwrap_or(FlexDirection::Row);
            s.flex_wrap = wrap.unwrap_or(FlexWrap::NoWrap);
        }
        "justify-content" | "-webkit-justify-content" => {
            if let Some(a) = parse_align_value(v) {
                s.justify_content = a;
            }
        }
        "align-items" | "-webkit-align-items" => {
            if let Some(a) = parse_align_value(v) {
                s.align_items = a;
            }
        }
        "align-self" | "-webkit-align-self" => {
            if let Some(a) = parse_align_value(v) {
                s.align_self = a;
            }
        }
        "align-content" => {
            if let Some(a) = parse_align_value(v) {
                s.align_content = a;
            }
        }
        "justify-items" => {
            if let Some(a) = parse_align_value(v) {
                s.justify_items = a;
            }
        }
        "justify-self" => {
            if let Some(a) = parse_align_value(v) {
                s.justify_self = a;
            }
        }
        "place-items" | "place-content" | "place-self" => {
            let a = first.ident().and_then(|i| parse_align(&i.to_ascii_lowercase()));
            let b = w.get(1).and_then(|c| c.ident()).and_then(|i| parse_align(&i.to_ascii_lowercase())).or(a);
            if let (Some(a), Some(b)) = (a, b) {
                match name {
                    "place-items" => {
                        s.align_items = a;
                        s.justify_items = b;
                    }
                    "place-content" => {
                        s.align_content = a;
                        s.justify_content = b;
                    }
                    _ => {
                        s.align_self = a;
                        s.justify_self = b;
                    }
                }
            }
        }
        "flex" | "-webkit-flex" | "-ms-flex" => {
            match id {
                Some("none") => {
                    s.flex_grow = 0.0;
                    s.flex_shrink = 0.0;
                    s.flex_basis = Len::Auto;
                    return;
                }
                Some("auto") => {
                    s.flex_grow = 1.0;
                    s.flex_shrink = 1.0;
                    s.flex_basis = Len::Auto;
                    return;
                }
                Some("initial") => {
                    s.flex_grow = 0.0;
                    s.flex_shrink = 1.0;
                    s.flex_basis = Len::Auto;
                    return;
                }
                _ => {}
            }
            let mut nums = Vec::new();
            let mut basis = None;
            for c in &w {
                if let CV::T(Tok::Number(n)) = c {
                    nums.push(*n as f32);
                } else if c.is_ident("content") {
                    basis = Some(Len::MaxContent);
                } else if let Some(l) = parse_len(c, &u) {
                    basis = Some(l);
                } else if let Some(n) = parse_number(c) {
                    nums.push(n);
                }
            }
            s.flex_grow = nums.first().copied().unwrap_or(1.0).max(0.0);
            s.flex_shrink = nums.get(1).copied().unwrap_or(1.0).max(0.0);
            s.flex_basis = basis.unwrap_or(if nums.is_empty() { Len::Auto } else { Len::Pct(0.0) });
            if nums.is_empty() && w.len() == 1 {
                s.flex_grow = 1.0;
            }
        }
        "flex-grow" | "-webkit-flex-grow" | "-webkit-box-flex" => {
            if let Some(n) = parse_number(first) {
                s.flex_grow = n.max(0.0);
            }
        }
        "flex-shrink" | "-webkit-flex-shrink" => {
            if let Some(n) = parse_number(first) {
                s.flex_shrink = n.max(0.0);
            }
        }
        "flex-basis" | "-webkit-flex-basis" => {
            if first.is_ident("content") {
                s.flex_basis = Len::MaxContent;
            } else if let Some(l) = len1() {
                s.flex_basis = l;
            }
        }
        "order" | "-webkit-order" => {
            if let Some(n) = parse_number(first) {
                s.order = n.round() as i32;
            }
        }
        "gap" | "grid-gap" => {
            let a = parse_len(first, &u);
            let b = w.get(1).and_then(|c| parse_len(c, &u));
            if let Some(a) = a {
                let a = if id == Some("normal") { Len::Auto } else { a };
                s.row_gap = a.clone();
                s.column_gap = b.unwrap_or(a);
            } else if id == Some("normal") {
                s.row_gap = Len::Auto;
                s.column_gap = Len::Auto;
            }
        }
        "row-gap" | "grid-row-gap" => {
            if let Some(l) = len1() {
                s.row_gap = l;
            }
        }
        "column-gap" | "grid-column-gap" => {
            if let Some(l) = len1() {
                s.column_gap = l;
            }
        }
        "grid-template-columns" => {
            if let Some(t) = parse_track_list(v, &u) {
                s.grid_template_columns = t;
            }
        }
        "grid-template-rows" => {
            if id == Some("subgrid") || w.first().map(|c| c.is_ident("subgrid")).unwrap_or(false) {
                s.subgrid_rows = true;
                s.grid_template_rows = None;
            } else if let Some(t) = parse_track_list(v, &u) {
                s.grid_template_rows = t;
                s.subgrid_rows = false;
            }
        }
        "grid-auto-columns" => {
            if let Some(t) = parse_track_list(v, &u) {
                s.grid_auto_columns = t;
            }
        }
        "grid-auto-rows" => {
            if let Some(t) = parse_track_list(v, &u) {
                s.grid_auto_rows = t;
            }
        }
        "grid-template-areas" => {
            if id == Some("none") {
                s.grid_template_areas = None;
                return;
            }
            let rows: Vec<Vec<String>> = w
                .iter()
                .filter_map(|c| match c {
                    CV::T(Tok::Str(st)) => Some(area_tokens(st)),
                    _ => None,
                })
                .collect();
            if !rows.is_empty() {
                s.grid_template_areas = Some(Rc::new(rows));
            }
        }
        "grid-template" | "grid" => {
            // "rows / columns" form (areas strings ignored except as rows)
            let mut halves = v.splitn(2, |c| c.is_delim('/'));
            let rows = halves.next().unwrap_or(&[]);
            if let Some(cols) = halves.next() {
                if let Some(t) = parse_track_list(cols, &u) {
                    s.grid_template_columns = t;
                }
            }
            let strs: Vec<Vec<String>> = words(rows)
                .iter()
                .filter_map(|c| match c {
                    CV::T(Tok::Str(st)) => Some(area_tokens(st)),
                    _ => None,
                })
                .collect();
            if !strs.is_empty() {
                s.grid_template_areas = Some(Rc::new(strs));
            } else if let Some(t) = parse_track_list(rows, &u) {
                s.grid_template_rows = t;
            }
        }
        "grid-auto-flow" => {
            s.grid_auto_flow_column = w.iter().any(|c| c.is_ident("column"));
            s.grid_auto_flow_dense = w.iter().any(|c| c.is_ident("dense"));
        }
        "grid-column" => {
            if let Some(p) = parse_grid_pair(v) {
                s.grid_column = p;
            }
        }
        "grid-row" => {
            if let Some(p) = parse_grid_pair(v) {
                s.grid_row = p;
            }
        }
        "grid-column-start" => {
            if let Some(l) = parse_grid_line(v) {
                s.grid_column.0 = l;
            }
        }
        "grid-column-end" => {
            if let Some(l) = parse_grid_line(v) {
                s.grid_column.1 = l;
            }
        }
        "grid-row-start" => {
            if let Some(l) = parse_grid_line(v) {
                s.grid_row.0 = l;
            }
        }
        "grid-row-end" => {
            if let Some(l) = parse_grid_line(v) {
                s.grid_row.1 = l;
            }
        }
        "grid-area" => {
            let parts: Vec<&[CV]> = v.split(|c| c.is_delim('/')).collect();
            let lines: Option<Vec<GridLine>> = parts.iter().map(|p| parse_grid_line(p)).collect();
            if let Some(l) = lines {
                match l.len() {
                    1 => {
                        s.grid_row = (l[0].clone(), l[0].clone());
                        s.grid_column = (l[0].clone(), l[0].clone());
                    }
                    2 => {
                        s.grid_row.0 = l[0].clone();
                        s.grid_column.0 = l[1].clone();
                    }
                    3 => {
                        s.grid_row = (l[0].clone(), l[2].clone());
                        s.grid_column.0 = l[1].clone();
                    }
                    4 => {
                        s.grid_row = (l[0].clone(), l[2].clone());
                        s.grid_column = (l[1].clone(), l[3].clone());
                    }
                    _ => {}
                }
            }
        }
        "columns" | "column-count" | "column-width" => {
            // `columns` sets both (either may be omitted or auto).
            if name != "column-width" {
                s.columns = w.iter().find_map(|c| match c {
                    CV::T(Tok::Number(n)) if *n >= 1.0 => Some(*n as u32),
                    _ => None,
                });
            }
            if name != "column-count" {
                s.column_width = w.iter().find_map(|c| match c {
                    CV::T(Tok::Number(_)) => None,
                    other => parse_len(other, &u).and_then(|l| l.definite(None)).filter(|v| *v > 0.0),
                });
            }
        }
        "column-rule" | "column-rule-width" | "column-rule-style" | "column-rule-color" => {
            for c in w.iter() {
                if name != "column-rule-width" && name != "column-rule-color" {
                    if let Some(st) = c.ident().and_then(|i| parse_border_style(&i.to_ascii_lowercase())) {
                        s.column_rule.1 = st;
                        continue;
                    }
                }
                if name != "column-rule-style" && name != "column-rule-color" {
                    if let Some(bw) = parse_border_width(c, &u) {
                        s.column_rule.0 = bw;
                        continue;
                    }
                }
                if name != "column-rule-style" && name != "column-rule-width" {
                    if let Some(col) = parse_color(c) {
                        s.column_rule.2 = col;
                    }
                }
            }
        }
        "column-span" => s.column_span_all = id == Some("all"),
        "column-fill" => s.column_fill_auto = id == Some("auto"),
        "break-inside" | "page-break-inside" => s.break_inside_avoid = matches!(id, Some("avoid") | Some("avoid-column") | Some("avoid-page")),
        "table-layout" => s.table_layout_fixed = id == Some("fixed"),
        "color" => {
            if let Some(c) = parse_color(first) {
                s.color = c.resolve(parent.color);
            }
        }
        "font-family" => {
            if let Some(f) = parse_font_family(v) {
                s.font_family = Rc::new(f);
            }
        }
        "font-size" => {
            if let Some(px) = font_size_px(first, s, parent, cx) {
                s.font_size = px.min(4096.0);
                s.font_kw = font_kw_of(first, parent);
            }
        }
        "font-weight" => {
            if let Some(wt) = parse_weight(first, parent) {
                s.font_weight = wt;
            }
        }
        "font-style" => {
            s.font_style = match id {
                Some("normal") => FontStyle::Normal,
                Some("italic") => FontStyle::Italic,
                _ if first.is_ident("oblique") => FontStyle::Oblique,
                _ => return,
            }
        }
        "font-variant-caps" => s.font_variant_small_caps = w.iter().any(|c| c.is_ident("small-caps")),
        "font-variant" => {
            // Shorthand: caps, numeric, ligatures and position keywords.
            s.font_variant_small_caps = w.iter().any(|c| c.is_ident("small-caps"));
            let mut bits = 0;
            for c in &w {
                if let Some(k) = c.ident() {
                    bits |= font_variant_keyword(&k.to_ascii_lowercase());
                }
            }
            s.font_variant_bits = bits;
        }
        "font-variant-numeric" | "font-variant-ligatures" | "font-variant-position" | "font-kerning" => {
            let mask = match name {
                "font-variant-numeric" => FV_NUMERIC_MASK,
                "font-variant-ligatures" => FV_LIGATURES_MASK,
                "font-variant-position" => FV_SUB | FV_SUPER,
                _ => FV_KERN_NONE,
            };
            let mut bits = 0;
            for c in &w {
                if let Some(k) = c.ident() {
                    let k = k.to_ascii_lowercase();
                    bits |= if name == "font-kerning" { if k == "none" { FV_KERN_NONE } else { 0 } } else { font_variant_keyword(&k) };
                }
            }
            s.font_variant_bits = (s.font_variant_bits & !mask) | (bits & mask);
        }
        "font-optical-sizing" => s.font_optical_sizing_none = id == Some("none"),
        "font-variation-settings" => {
            if id == Some("normal") {
                s.font_variation_settings = None;
            } else {
                let mut out = Vec::new();
                for part in super::tokenizer::split_commas(v) {
                    let toks: Vec<&CV> = part.iter().filter(|c| !c.is_ws()).collect();
                    match (toks.first(), toks.get(1)) {
                        (Some(CV::T(Tok::Str(t))), Some(CV::T(Tok::Number(n)))) if t.len() == 4 && t.is_ascii() => {
                            let b = t.as_bytes();
                            out.push((u32::from_be_bytes([b[0], b[1], b[2], b[3]]), *n as f32));
                        }
                        _ => {
                            out.clear();
                            break;
                        }
                    }
                }
                s.font_variation_settings = if out.is_empty() { None } else { Some(Rc::new(out)) };
            }
        }
        "font-feature-settings" => {
            if id == Some("normal") {
                s.font_feature_settings = None;
            } else {
                let mut out = Vec::new();
                for part in super::tokenizer::split_commas(v) {
                    let toks: Vec<&CV> = part.iter().filter(|c| !c.is_ws()).collect();
                    let tag = match toks.first() {
                        Some(CV::T(Tok::Str(t))) if t.len() == 4 && t.is_ascii() => t.as_bytes().to_vec(),
                        _ => {
                            out.clear();
                            break;
                        }
                    };
                    let val = match toks.get(1) {
                        None => 1,
                        Some(CV::T(Tok::Number(n))) if *n >= 0.0 => *n as u32,
                        Some(c) if c.is_ident("on") => 1,
                        Some(c) if c.is_ident("off") => 0,
                        _ => {
                            out.clear();
                            break;
                        }
                    };
                    out.push((u32::from_be_bytes([tag[0], tag[1], tag[2], tag[3]]), val));
                }
                s.font_feature_settings = if out.is_empty() { None } else { Some(Rc::new(out)) };
            }
        }
        "font" => {
            // [style || variant || weight || stretch] size [/ line-height] family
            match id {
                // System fonts (and the form-control keywords) reset every font
                // longhand: Chrome uses the UI font on macOS, Arial elsewhere.
                Some("caption") | Some("icon") | Some("menu") | Some("message-box") | Some("small-caption") | Some("status-bar") | Some("-apple-system-body") | Some("-webkit-small-control") | Some("-webkit-control") | Some("-webkit-mini-control") => {
                    let control = id.map(|k| k.starts_with("-webkit-")).unwrap_or(false);
                    let mac = cfg!(target_os = "macos");
                    s.font_family = Rc::new(vec![if mac { "system-ui".into() } else { "Arial".into() }]);
                    s.font_size = if control { 13.333 } else if mac { 13.0 } else { 16.0 };
                    s.font_kw = None;
                    s.font_style = FontStyle::Normal;
                    s.font_weight = 400;
                    s.font_variant_small_caps = false;
                    s.line_height = LineHeight::Normal;
                    return;
                }
                _ => {}
            }
            let mut i = 0;
            let mut style = FontStyle::Normal;
            let mut weight = 400;
            let mut small_caps = false;
            while i < v.len() {
                let c = &v[i];
                if c.is_ws() {
                    i += 1;
                    continue;
                }
                if c.is_ident("italic") {
                    style = FontStyle::Italic;
                } else if c.is_ident("oblique") {
                    style = FontStyle::Oblique;
                } else if c.is_ident("small-caps") {
                    small_caps = true;
                } else if c.is_ident("normal") || c.ident().map(|x| x.ends_with("condensed") || x.ends_with("expanded")).unwrap_or(false) {
                } else if matches!(c, CV::T(Tok::Number(n)) if !(1.0..=1000.0).contains(n)) {
                    // `font: 0/0 Arial`: a number outside 1–1000 isn't a weight
                    // (unitless 0 is the font-size).
                    break;
                } else if let (Some(wt), true) = (parse_weight(c, parent), matches!(c, CV::T(Tok::Number(_))) || c.is_ident("bold") || c.is_ident("bolder") || c.is_ident("lighter")) {
                    weight = wt;
                } else {
                    break;
                }
                i += 1;
            }
            let size = match v.get(i).and_then(|c| font_size_px(c, s, parent, cx)) {
                Some(sz) => sz,
                None => return,
            };
            let kw = font_kw_of(&v[i], parent);
            i += 1;
            let mut lh = LineHeight::Normal;
            let rest = &v[i..];
            let mut j = 0;
            while j < rest.len() && rest[j].is_ws() {
                j += 1;
            }
            if j < rest.len() && rest[j].is_delim('/') {
                j += 1;
                while j < rest.len() && rest[j].is_ws() {
                    j += 1;
                }
                if let Some(c) = rest.get(j) {
                    lh = match c {
                        CV::T(Tok::Number(n)) => LineHeight::Number(*n as f32),
                        c if c.is_ident("normal") => LineHeight::Normal,
                        c => match parse_len(c, &UnitCtx::new(size, cx.root_font_size, cx.vw, cx.vh)) {
                            Some(l) => LineHeight::Px(l.resolve(size)),
                            None => LineHeight::Normal,
                        },
                    };
                    j += 1;
                }
            }
            if let Some(f) = parse_font_family(&rest[j..]) {
                s.font_family = Rc::new(f);
            } else {
                return;
            }
            s.font_style = style;
            s.font_weight = weight;
            s.font_size = size;
            s.font_kw = kw;
            s.line_height = lh;
            s.font_variant_small_caps = small_caps;
        }
        "line-height" => {
            s.line_height = match first {
                CV::T(Tok::Number(n)) => LineHeight::Number(n.max(0.0) as f32),
                c if c.is_ident("normal") => LineHeight::Normal,
                // A unitless math function (`calc(1.25 / .875)`) is a number,
                // not a length: it scales with each element's font size.
                c @ CV::Func(..) if parse_number(c).is_some() => LineHeight::Number(parse_number(c).unwrap_or(1.0).max(0.0)),
                c => match parse_len(c, &u) {
                    Some(l) => LineHeight::Px(l.resolve(s.font_size).max(0.0)),
                    None => match parse_number(c) {
                        Some(n) => LineHeight::Number(n),
                        None => return,
                    },
                },
            }
        }
        "text-align" => {
            s.text_align = match id {
                Some("start") => TextAlign::Start,
                Some("end") => TextAlign::End,
                Some("left") | Some("-webkit-left") => TextAlign::Left,
                Some("right") | Some("-webkit-right") => TextAlign::Right,
                Some("center") => TextAlign::Center,
                Some("-webkit-center") | Some("-moz-center") => TextAlign::WebkitCenter,
                Some("justify") => TextAlign::Justify,
                Some("match-parent") => parent.text_align,
                _ => return,
            }
        }
        "text-indent" => {
            if let Some(l) = len1() {
                s.text_indent = l;
            }
        }
        "text-transform" => {
            s.text_transform = match id {
                Some("uppercase") => TextTransform::Uppercase,
                Some("lowercase") => TextTransform::Lowercase,
                Some("capitalize") => TextTransform::Capitalize,
                Some("math-auto") => TextTransform::MathAuto,
                Some("none") => TextTransform::None,
                _ => return,
            }
        }
        "white-space" => {
            s.white_space = match id {
                Some("normal") => WhiteSpace::Normal,
                Some("pre") => WhiteSpace::Pre,
                Some("nowrap") => WhiteSpace::NoWrap,
                Some("pre-wrap") => WhiteSpace::PreWrap,
                Some("pre-line") => WhiteSpace::PreLine,
                Some("break-spaces") => WhiteSpace::BreakSpaces,
                _ => {
                    if w.iter().any(|c| c.is_ident("nowrap")) {
                        WhiteSpace::NoWrap
                    } else {
                        return;
                    }
                }
            }
        }
        "text-wrap-style" => {
            s.wrap_balance = id == Some("balance");
        }
        "text-wrap" | "text-wrap-mode" => {
            if name == "text-wrap" {
                let w = words(v);
                if w.iter().any(|c| c.is_ident("balance")) {
                    s.wrap_balance = true;
                } else if w.iter().any(|c| c.is_ident("auto") || c.is_ident("pretty") || c.is_ident("stable")) || id == Some("wrap") || id == Some("nowrap") {
                    s.wrap_balance = false;
                }
            }
            if id == Some("nowrap") {
                s.white_space = if s.white_space == WhiteSpace::Pre || s.white_space == WhiteSpace::PreWrap { WhiteSpace::Pre } else { WhiteSpace::NoWrap };
            } else if id == Some("wrap") && s.white_space == WhiteSpace::NoWrap {
                s.white_space = WhiteSpace::Normal;
            }
        }
        "white-space-collapse" => {
            if id == Some("preserve") || id == Some("break-spaces") {
                s.white_space = if s.white_space.wraps() { WhiteSpace::PreWrap } else { WhiteSpace::Pre };
            }
        }
        "letter-spacing" => {
            if id == Some("normal") {
                s.letter_spacing = 0.0;
            } else if let Some(l) = len1() {
                s.letter_spacing = l.resolve(s.font_size);
            }
        }
        "word-spacing" => {
            if id == Some("normal") {
                s.word_spacing = 0.0;
            } else if let Some(l) = len1() {
                s.word_spacing = l.resolve(s.font_size);
            }
        }
        "word-break" | "overflow-wrap" | "word-wrap" => {
            s.break_anywhere = matches!(id, Some("break-all") | Some("break-word") | Some("anywhere"));
        }
        "counter-reset" | "counter-increment" | "counter-set" => {
            let dflt = if name == "counter-increment" { 1 } else { 0 };
            let v = if id == Some("none") {
                None
            } else {
                let mut list: Vec<(Rc<str>, i64)> = Vec::new();
                for c in &w {
                    match c {
                        CV::T(Tok::Ident(i)) => list.push((Rc::from(i.as_str()), dflt)),
                        CV::Func(f, args) if f == "reversed" && name == "counter-reset" => {
                            // reversed(name): counts down; the start value is found at layout.
                            if let Some(i) = words(args).first().and_then(|c| c.ident()) {
                                list.push((Rc::from(format!("reversed({i})").as_str()), i64::MIN));
                            }
                        }
                        CV::T(Tok::Number(n)) if n.fract() == 0.0 => match list.last_mut() {
                            Some(last) => last.1 = *n as i64,
                            None => return,
                        },
                        _ => return,
                    }
                }
                if list.is_empty() {
                    return;
                }
                Some(Rc::new(list))
            };
            match name {
                "counter-reset" => s.counter_reset = v,
                "counter-increment" => s.counter_increment = v,
                _ => s.counter_set = v,
            }
        }
        "quotes" => {
            if id == Some("none") {
                s.quotes = Some(Rc::new(Vec::new()));
            } else if id == Some("auto") {
                s.quotes = None;
            } else {
                let strs: Vec<String> = w.iter().filter_map(|c| if let CV::T(Tok::Str(t)) = c { Some(t.clone()) } else { None }).collect();
                if strs.len() == w.len() && !strs.is_empty() && strs.len() % 2 == 0 {
                    s.quotes = Some(Rc::new(strs.chunks(2).map(|p| (p[0].clone(), p[1].clone())).collect()));
                }
            }
        }
        "list-style-type" => {
            if let Some(t) = id.and_then(parse_list_style_type) {
                s.list_style_type = t;
            } else if let CV::T(Tok::Str(st)) = first {
                s.list_style_type = ListStyleType::Str(Rc::from(st.as_str()));
            }
        }
        "list-style-position" => s.list_style_inside = id == Some("inside"),
        "list-style-image" => {
            s.list_style_image = match parse_image(first, &u) {
                Some(BgImage::Url(u)) => Some(u),
                _ => None,
            }
        }
        "list-style" => {
            let mut set_type = false;
            for c in &w {
                if let Some(i) = c.ident().map(|s| s.to_ascii_lowercase()) {
                    if i == "inside" {
                        s.list_style_inside = true;
                    } else if i == "outside" {
                        s.list_style_inside = false;
                    } else if let Some(t) = parse_list_style_type(&i) {
                        s.list_style_type = t;
                        set_type = true;
                    }
                } else if let CV::T(Tok::Str(st)) = c {
                    s.list_style_type = ListStyleType::Str(Rc::from(st.as_str()));
                    set_type = true;
                } else if let Some(BgImage::Url(u)) = parse_image(c, &u) {
                    s.list_style_image = Some(u);
                }
            }
            let _ = set_type;
        }
        "cursor" => {
            if let Some(c) = parse_cursor(v) {
                s.cursor = c;
            }
        }
        "visibility" => {
            s.visibility = match id {
                Some("visible") => Visibility::Visible,
                Some("hidden") => Visibility::Hidden,
                Some("collapse") => Visibility::Collapse,
                _ => return,
            }
        }
        "border-collapse" => s.border_collapse = id == Some("collapse"),
        "border-spacing" => {
            let w = words(v);
            let ls: Vec<f32> = w.iter().filter_map(|c| parse_len(c, &u)).map(|l| l.resolve(0.0).max(0.0)).collect();
            if ls.len() == w.len() && (1..=2).contains(&ls.len()) {
                s.border_spacing = ls[0];
                s.border_spacing_v = *ls.get(1).unwrap_or(&ls[0]);
            }
        }
        "empty-cells" => s.empty_cells_hide = id == Some("hide"),
        "caption-side" => s.caption_bottom = id == Some("bottom"),
        "pointer-events" => s.pointer_events = id != Some("none"),
        "user-select" | "-webkit-user-select" | "-moz-user-select" | "-ms-user-select" => s.user_select = id != Some("none"),
        "direction" => s.direction_rtl = id == Some("rtl"),
        "unicode-bidi" => {
            s.bidi_override = matches!(id, Some("bidi-override") | Some("isolate-override"));
            s.bidi_isolate = matches!(id, Some("isolate") | Some("isolate-override") | Some("plaintext"));
        }
        "tab-size" | "-moz-tab-size" => {
            if let CV::T(Tok::Number(n)) = first {
                s.tab_size = *n as f32;
            }
        }
        "accent-color" => s.accent_color = parse_color(first).map(|c| c.resolve(s.color)),
        "caret-color" => s.caret_color = parse_color(first).map(|c| c.resolve(s.color)),
        "contain" => {
            let mut c = 0u8;
            for t in w.iter().filter_map(|c| c.ident()).map(|x| x.to_ascii_lowercase()) {
                c |= match t.as_str() {
                    "size" => CONTAIN_SIZE,
                    "inline-size" => CONTAIN_INLINE_SIZE,
                    "layout" => CONTAIN_LAYOUT,
                    "paint" => CONTAIN_PAINT,
                    "style" => CONTAIN_STYLE,
                    "strict" => CONTAIN_SIZE | CONTAIN_LAYOUT | CONTAIN_PAINT | CONTAIN_STYLE,
                    "content" => CONTAIN_LAYOUT | CONTAIN_PAINT | CONTAIN_STYLE,
                    _ => 0,
                };
            }
            s.contain = c;
        }
        "will-change" => {
            s.will_change_cb = w.iter().filter_map(|c| c.ident()).any(|t| {
                matches!(t.to_ascii_lowercase().as_str(), "transform" | "translate" | "rotate" | "scale" | "perspective" | "filter" | "backdrop-filter" | "contain")
            });
            s.will_change_backdrop_root = w.iter().filter_map(|c| c.ident()).any(|t| {
                matches!(t.to_ascii_lowercase().as_str(), "opacity" | "filter" | "backdrop-filter" | "mask" | "mask-image" | "clip-path" | "mix-blend-mode")
            });
        }
        "contain-intrinsic-size" => {
            // [auto]? <length> | none, for width and height.
            let lens: Vec<Option<f32>> = w.iter().filter(|c| !c.is_ident("auto")).map(|c| parse_len(c, &u).and_then(|l| l.definite(None))).collect();
            s.contain_intrinsic = match lens.as_slice() {
                [a] => (*a, *a),
                [a, b, ..] => (*a, *b),
                _ => (None, None),
            };
        }
        "contain-intrinsic-width" => s.contain_intrinsic.0 = len1().and_then(|l| l.definite(None)),
        "contain-intrinsic-height" => s.contain_intrinsic.1 = len1().and_then(|l| l.definite(None)),
        "container-type" => {
            s.container_type = match id {
                Some("inline-size") => 1,
                Some("size") => 2,
                _ => 0,
            }
        }
        "container-name" => {
            let names: Vec<String> = w.iter().filter_map(|c| c.ident()).filter(|n| !n.eq_ignore_ascii_case("none")).map(|n| n.to_string()).collect();
            s.container_names = if names.is_empty() { None } else { Some(Rc::new(names)) };
        }
        "container" => {
            // <name>+ [ / <type> ]?
            let slash = v.iter().position(|c| c.is_delim('/'));
            let (names_part, type_part) = match slash {
                Some(i) => (&v[..i], Some(&v[i + 1..])),
                None => (v, None),
            };
            let names: Vec<String> = words(names_part).iter().filter_map(|c| c.ident()).filter(|n| !n.eq_ignore_ascii_case("none")).map(|n| n.to_string()).collect();
            s.container_names = if names.is_empty() { None } else { Some(Rc::new(names)) };
            s.container_type = match type_part.and_then(|t| words(t).first().and_then(|c| c.ident()).map(|x| x.to_ascii_lowercase())).as_deref() {
                Some("inline-size") => 1,
                Some("size") => 2,
                _ => 0,
            };
        }
        "fill" | "stroke" => {
            let p = if id == Some("none") {
                Some(SvgPaint::None)
            } else if w.len() == 1 {
                parse_color(first).map(SvgPaint::Color)
            } else {
                None
            };
            let p = p.or_else(|| Some(SvgPaint::Raw(Rc::from(super::tokenizer::serialize(v).trim()))));
            if name == "fill" {
                s.fill = p;
            } else {
                s.stroke = p;
            }
        }
        n if svg_prop_index(n).is_some() => {
            let text = super::tokenizer::serialize(v);
            s.set_svg_value(svg_prop_index(n).unwrap(), Some(Rc::from(text.trim())));
        }
        "-webkit-line-clamp" | "line-clamp" => {
            s.line_clamp = match first {
                CV::T(Tok::Number(n)) if *n >= 1.0 => Some(*n as u32),
                _ => None,
            };
        }
        "-webkit-box-orient" | "box-orient" => {
            if id == Some("vertical") || id == Some("block-axis") {
                s.flex_direction = FlexDirection::Column;
            } else if id == Some("horizontal") || id == Some("inline-axis") {
                s.flex_direction = FlexDirection::Row;
            }
        }
        _ => {}
    }
}


/// Parse a canvas `font` string ("italic bold 16px Arial, sans-serif").
pub fn parse_canvas_font(src: &str) -> Option<(Rc<Vec<String>>, u16, FontStyle, f32)> {
    let cvs = super::tokenizer::parse_cvs(src);
    if cvs.is_empty() {
        return None;
    }
    let parent = ComputedStyle::initial();
    let mut s = ComputedStyle::initial();
    s.font_size = -1.0;
    let cx = ApplyCtx { root_font_size: 16.0, vw: 1280.0, vh: 800.0, root_line_height: 0.0, cq: None };
    apply(&mut s, &parent, "font", &cvs, &cx);
    if s.font_size <= 0.0 {
        return None;
    }
    Some((s.font_family.clone(), s.font_weight, s.font_style, s.font_size))
}

#[cfg(test)]
mod tests {
    use super::keyword_font_size;

    #[test]
    fn monospace_keyword_sizes() {
        let mono = vec!["monospace".to_string()];
        let sans = vec!["sans-serif".to_string()];
        let both = vec!["monospace".to_string(), "monospace".to_string()];
        assert_eq!(keyword_font_size((3, 1.0), &mono), 13.0);
        assert_eq!(keyword_font_size((3, 1.0), &sans), 16.0);
        assert_eq!(keyword_font_size((3, 1.0), &both), 16.0);
        assert_eq!(keyword_font_size((2, 1.0), &mono), 12.0);
        assert_eq!(keyword_font_size((3, 1.5), &mono), 19.5);
    }
}

/// The cells of a `grid-template-areas` row string: runs of "." are one null
/// cell token each and names are separate even without spaces (". .main. .").
fn area_tokens(st: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut dots = false;
    for c in st.chars() {
        if c.is_whitespace() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            dots = false;
        } else if c == '.' {
            if !dots && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            if !dots {
                cur.push('.');
            }
            dots = true;
        } else {
            if dots {
                out.push(std::mem::take(&mut cur));
                dots = false;
            }
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
