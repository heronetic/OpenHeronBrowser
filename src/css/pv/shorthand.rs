// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Shorthands: their longhands, expansion of a value into longhand values,
//! and serialization back from longhands (CSSOM).

use std::rc::Rc;

use super::db::{self, with_db, Db};
use super::grammar::{self, G};
use super::hooks::collapse4;
use super::super::tokenizer::CV;
use super::{ser, V};

/// Legacy names that are plain aliases.
pub const ALIASES: &[(&str, &str)] = &[
    ("grid-row-gap", "row-gap"),
    ("grid-column-gap", "column-gap"),
    ("grid-gap", "gap"),
    ("-webkit-border-radius", "border-radius"),
    ("-webkit-transform", "transform"),
    ("-moz-transform", "transform"),
    ("-webkit-transform-origin", "transform-origin"),
    ("-webkit-perspective", "perspective"),
    ("-webkit-perspective-origin", "perspective-origin"),
    ("-webkit-transform-style", "transform-style"),
    ("-webkit-backface-visibility", "backface-visibility"),
    ("-webkit-border-top-left-radius", "border-top-left-radius"),
    ("-webkit-border-top-right-radius", "border-top-right-radius"),
    ("-webkit-border-bottom-left-radius", "border-bottom-left-radius"),
    ("-webkit-border-bottom-right-radius", "border-bottom-right-radius"),
    ("-webkit-box-shadow", "box-shadow"),
    ("-webkit-box-sizing", "box-sizing"),
    ("-webkit-filter", "filter"),
    ("-webkit-opacity", "opacity"),
    ("-webkit-clip-path", "clip-path"),
    ("-webkit-mask-image", "mask-image"),
    ("-webkit-mask-size", "mask-size"),
    ("-webkit-mask-repeat", "mask-repeat"),
    ("-webkit-mask-position", "mask-position"),
    ("-webkit-mask-origin", "mask-origin"),
    ("-webkit-mask-clip", "mask-clip"),
    ("-webkit-mask", "mask"),
    ("-webkit-animation", "animation"),
    ("-webkit-animation-name", "animation-name"),
    ("-webkit-animation-duration", "animation-duration"),
    ("-webkit-animation-delay", "animation-delay"),
    ("-webkit-animation-timing-function", "animation-timing-function"),
    ("-webkit-animation-iteration-count", "animation-iteration-count"),
    ("-webkit-animation-direction", "animation-direction"),
    ("-webkit-animation-fill-mode", "animation-fill-mode"),
    ("-webkit-animation-play-state", "animation-play-state"),
    ("-webkit-transition", "transition"),
    ("-webkit-transition-property", "transition-property"),
    ("-webkit-transition-duration", "transition-duration"),
    ("-webkit-transition-delay", "transition-delay"),
    ("-webkit-transition-timing-function", "transition-timing-function"),
    ("-webkit-flex", "flex"),
    ("-webkit-flex-direction", "flex-direction"),
    ("-webkit-flex-wrap", "flex-wrap"),
    ("-webkit-flex-flow", "flex-flow"),
    ("-webkit-flex-grow", "flex-grow"),
    ("-webkit-flex-shrink", "flex-shrink"),
    ("-webkit-flex-basis", "flex-basis"),
    ("-webkit-order", "order"),
    ("-webkit-align-items", "align-items"),
    ("-webkit-align-self", "align-self"),
    ("-webkit-align-content", "align-content"),
    ("-webkit-justify-content", "justify-content"),
    ("-webkit-background-clip", "background-clip"),
    ("-webkit-background-origin", "background-origin"),
    ("-webkit-background-size", "background-size"),
    ("-webkit-border-image", "border-image"),
    ("-webkit-column-count", "column-count"),
    ("-webkit-column-gap", "column-gap"),
    ("-webkit-column-width", "column-width"),
    ("-webkit-column-rule", "column-rule"),
    ("-webkit-columns", "columns"),
    ("-webkit-shape-outside", "shape-outside"),
    ("-webkit-text-emphasis", "text-emphasis"),
    ("-webkit-text-emphasis-color", "text-emphasis-color"),
    ("-webkit-text-emphasis-style", "text-emphasis-style"),
    ("-webkit-text-emphasis-position", "text-emphasis-position"),
    ("-webkit-writing-mode", "writing-mode"),
    ("-webkit-text-orientation", "text-orientation"),
    ("-webkit-font-feature-settings", "font-feature-settings"),
    ("-webkit-text-decoration-line", "text-decoration-line"),
    ("-webkit-text-decoration-style", "text-decoration-style"),
    ("-webkit-text-decoration-color", "text-decoration-color"),
    ("-webkit-text-decoration", "text-decoration"),
    ("-webkit-line-break", "line-break"),
    ("-webkit-ruby-position", "ruby-position"),
];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SK {
    /// 1–4 values: top right bottom left.
    Four,
    /// 1–2 values (start end / x y).
    Pair,
    /// `a || b || c`, initial values omitted.
    Combo,
    /// Hand-written expansion and serialization.
    Custom,
}

pub struct SH {
    pub name: &'static str,
    pub longhands: &'static [&'static str],
    pub kind: SK,
}

macro_rules! sh {
    ($n:expr, $k:ident, [$($l:expr),*]) => {
        SH { name: $n, longhands: &[$($l),*], kind: SK::$k }
    };
}

pub const SHORTHANDS: &[SH] = &[
    sh!("margin", Four, ["margin-top", "margin-right", "margin-bottom", "margin-left"]),
    sh!("padding", Four, ["padding-top", "padding-right", "padding-bottom", "padding-left"]),
    sh!("inset", Four, ["top", "right", "bottom", "left"]),
    sh!("border-width", Four, ["border-top-width", "border-right-width", "border-bottom-width", "border-left-width"]),
    sh!("border-style", Four, ["border-top-style", "border-right-style", "border-bottom-style", "border-left-style"]),
    sh!("border-color", Four, ["border-top-color", "border-right-color", "border-bottom-color", "border-left-color"]),
    sh!("scroll-margin", Four, ["scroll-margin-top", "scroll-margin-right", "scroll-margin-bottom", "scroll-margin-left"]),
    sh!("scroll-padding", Four, ["scroll-padding-top", "scroll-padding-right", "scroll-padding-bottom", "scroll-padding-left"]),
    sh!("margin-block", Pair, ["margin-block-start", "margin-block-end"]),
    sh!("margin-inline", Pair, ["margin-inline-start", "margin-inline-end"]),
    sh!("padding-block", Pair, ["padding-block-start", "padding-block-end"]),
    sh!("padding-inline", Pair, ["padding-inline-start", "padding-inline-end"]),
    sh!("inset-block", Pair, ["inset-block-start", "inset-block-end"]),
    sh!("inset-inline", Pair, ["inset-inline-start", "inset-inline-end"]),
    sh!("scroll-margin-block", Pair, ["scroll-margin-block-start", "scroll-margin-block-end"]),
    sh!("scroll-margin-inline", Pair, ["scroll-margin-inline-start", "scroll-margin-inline-end"]),
    sh!("scroll-padding-block", Pair, ["scroll-padding-block-start", "scroll-padding-block-end"]),
    sh!("scroll-padding-inline", Pair, ["scroll-padding-inline-start", "scroll-padding-inline-end"]),
    sh!("border-block-width", Pair, ["border-block-start-width", "border-block-end-width"]),
    sh!("border-block-style", Pair, ["border-block-start-style", "border-block-end-style"]),
    sh!("border-block-color", Pair, ["border-block-start-color", "border-block-end-color"]),
    sh!("border-inline-width", Pair, ["border-inline-start-width", "border-inline-end-width"]),
    sh!("border-inline-style", Pair, ["border-inline-start-style", "border-inline-end-style"]),
    sh!("border-inline-color", Pair, ["border-inline-start-color", "border-inline-end-color"]),
    sh!("overflow", Pair, ["overflow-x", "overflow-y"]),
    sh!("overscroll-behavior", Pair, ["overscroll-behavior-x", "overscroll-behavior-y"]),
    sh!("gap", Pair, ["row-gap", "column-gap"]),
    sh!("contain-intrinsic-size", Pair, ["contain-intrinsic-width", "contain-intrinsic-height"]),
    sh!("border-top", Combo, ["border-top-width", "border-top-style", "border-top-color"]),
    sh!("border-right", Combo, ["border-right-width", "border-right-style", "border-right-color"]),
    sh!("border-bottom", Combo, ["border-bottom-width", "border-bottom-style", "border-bottom-color"]),
    sh!("border-left", Combo, ["border-left-width", "border-left-style", "border-left-color"]),
    sh!("border-block-start", Combo, ["border-block-start-width", "border-block-start-style", "border-block-start-color"]),
    sh!("border-block-end", Combo, ["border-block-end-width", "border-block-end-style", "border-block-end-color"]),
    sh!("border-inline-start", Combo, ["border-inline-start-width", "border-inline-start-style", "border-inline-start-color"]),
    sh!("border-inline-end", Combo, ["border-inline-end-width", "border-inline-end-style", "border-inline-end-color"]),
    sh!("outline", Combo, ["outline-color", "outline-style", "outline-width"]),
    sh!("column-rule", Combo, ["column-rule-width", "column-rule-style", "column-rule-color"]),
    sh!("flex-flow", Combo, ["flex-direction", "flex-wrap"]),
    sh!("text-emphasis", Combo, ["text-emphasis-style", "text-emphasis-color"]),
    sh!("-webkit-text-stroke", Combo, ["-webkit-text-stroke-width", "-webkit-text-stroke-color"]),
    sh!("text-wrap", Custom, ["text-wrap-mode", "text-wrap-style"]),
    sh!("text-decoration", Custom, ["text-decoration-line", "text-decoration-thickness", "text-decoration-style", "text-decoration-color"]),
    sh!("list-style", Custom, ["list-style-position", "list-style-image", "list-style-type"]),
    sh!("columns", Custom, ["column-width", "column-count", "column-height"]),
    sh!("flex", Custom, ["flex-grow", "flex-shrink", "flex-basis"]),
    sh!("place-content", Custom, ["align-content", "justify-content"]),
    sh!("place-items", Custom, ["align-items", "justify-items"]),
    sh!("place-self", Custom, ["align-self", "justify-self"]),
    sh!("border-radius", Custom, ["border-top-left-radius", "border-top-right-radius", "border-bottom-right-radius", "border-bottom-left-radius"]),
    sh!("border-block", Custom, ["border-block-start-width", "border-block-start-style", "border-block-start-color", "border-block-end-width", "border-block-end-style", "border-block-end-color"]),
    sh!("border-inline", Custom, ["border-inline-start-width", "border-inline-start-style", "border-inline-start-color", "border-inline-end-width", "border-inline-end-style", "border-inline-end-color"]),
    sh!(
        "border",
        Custom,
        [
            "border-top-width",
            "border-right-width",
            "border-bottom-width",
            "border-left-width",
            "border-top-style",
            "border-right-style",
            "border-bottom-style",
            "border-left-style",
            "border-top-color",
            "border-right-color",
            "border-bottom-color",
            "border-left-color",
            "border-image-source",
            "border-image-slice",
            "border-image-width",
            "border-image-outset",
            "border-image-repeat"
        ]
    ),
    sh!("border-image", Custom, ["border-image-source", "border-image-slice", "border-image-width", "border-image-outset", "border-image-repeat"]),
    sh!("mask-border", Custom, ["mask-border-source", "mask-border-slice", "mask-border-width", "mask-border-outset", "mask-border-repeat", "mask-border-mode"]),
    sh!("white-space", Custom, ["white-space-collapse", "text-wrap-mode"]),
    sh!("font-synthesis", Custom, ["font-synthesis-weight", "font-synthesis-style", "font-synthesis-small-caps", "font-synthesis-position"]),
    sh!("background", Custom, ["background-image", "background-position", "background-size", "background-repeat", "background-attachment", "background-origin", "background-clip", "background-color"]),
    sh!(
        "font",
        Custom,
        [
            "font-style",
            "font-variant-ligatures",
            "font-variant-caps",
            "font-variant-numeric",
            "font-variant-east-asian",
            "font-variant-alternates",
            "font-variant-position",
            "font-variant-emoji",
            "font-weight",
            "font-stretch",
            "font-size",
            "line-height",
            "font-family",
            "font-optical-sizing",
            "font-size-adjust",
            "font-kerning",
            "font-feature-settings",
            "font-variation-settings",
            "font-language-override"
        ]
    ),
    sh!("font-variant", Custom, ["font-variant-ligatures", "font-variant-caps", "font-variant-alternates", "font-variant-numeric", "font-variant-east-asian", "font-variant-position", "font-variant-emoji"]),
    sh!("grid-row", Custom, ["grid-row-start", "grid-row-end"]),
    sh!("grid-column", Custom, ["grid-column-start", "grid-column-end"]),
    sh!("grid-area", Custom, ["grid-row-start", "grid-column-start", "grid-row-end", "grid-column-end"]),
    sh!("mask", Custom, ["mask-image", "mask-position", "mask-size", "mask-repeat", "mask-origin", "mask-clip", "mask-composite", "mask-mode"]),
    sh!("offset", Custom, ["offset-position", "offset-path", "offset-distance", "offset-rotate", "offset-anchor"]),
    sh!("text-box", Custom, ["text-box-trim", "text-box-edge"]),
    sh!("position-try", Custom, ["position-try-order", "position-try-fallbacks"]),
    sh!("page-break-before", Custom, ["break-before"]),
    sh!("page-break-after", Custom, ["break-after"]),
    sh!("page-break-inside", Custom, ["break-inside"]),
    sh!("grid-template", Custom, ["grid-template-rows", "grid-template-columns", "grid-template-areas"]),
    sh!("grid", Custom, ["grid-template-rows", "grid-template-columns", "grid-template-areas", "grid-auto-rows", "grid-auto-columns", "grid-auto-flow"]),
    sh!("animation-range", Custom, ["animation-range-start", "animation-range-end"]),
    sh!("block-step", Combo, ["block-step-size", "block-step-insert", "block-step-align", "block-step-round"]),
    sh!("text-spacing", Custom, ["text-spacing-trim", "text-autospace"]),
    sh!("transition", Custom, ["transition-property", "transition-duration", "transition-timing-function", "transition-delay", "transition-behavior"]),
    sh!("animation", Custom, ["animation-duration", "animation-timing-function", "animation-delay", "animation-iteration-count", "animation-direction", "animation-fill-mode", "animation-play-state", "animation-name"]),
];

pub fn find(name: &str) -> Option<&'static SH> {
    SHORTHANDS.iter().find(|s| s.name == name)
}

pub fn longhands(name: &str) -> Option<Vec<&'static str>> {
    find(name).map(|s| s.longhands.to_vec())
}

/// The grammar of a shorthand referenced as `<'name'>` in another grammar.
pub fn grammar_of(db: &Db, name: &str) -> Option<Rc<G>> {
    thread_local! {
        static CACHE: std::cell::RefCell<std::collections::HashMap<String, Rc<G>>> = Default::default();
    }
    if let Some(g) = CACHE.with(|c| c.borrow().get(name).cloned()) {
        return Some(g);
    }
    let _ = db;
    let src = match name {
        "border-radius" => "<length-percentage [0,∞]>{1,4} [ / <length-percentage [0,∞]>{1,4} ]?",
        _ => return None,
    };
    let g = Rc::new(grammar::parse(src));
    CACHE.with(|c| c.borrow_mut().insert(name.to_string(), g.clone()));
    Some(g)
}

fn parse_with(src: &str, v: &[CV]) -> Option<V> {
    thread_local! {
        static CACHE: std::cell::RefCell<std::collections::HashMap<String, Rc<G>>> = Default::default();
    }
    let g = CACHE.with(|c| c.borrow_mut().entry(src.to_string()).or_insert_with(|| Rc::new(grammar::parse(src))).clone());
    let it = grammar::items(v);
    with_db(|db| grammar::match_all(db, &g, &it))
}

/// Canonical specified text of a longhand component value.
fn lh(name: &str, v: &V) -> Option<String> {
    let v = super::hooks::specified(name, v.clone())?;
    Some(ser(&v))
}

thread_local! {
    static INIT_OVERRIDE: std::cell::RefCell<Option<Box<dyn Fn(&str) -> Option<String>>>> = const { std::cell::RefCell::new(None) };
}

/// Serialize a shorthand from computed longhands (initial values compared in
/// computed form, as `init` gives them).
pub fn serialize_computed(name: &str, get: &dyn Fn(&str) -> Option<String>, init: Box<dyn Fn(&str) -> Option<String>>) -> Option<String> {
    INIT_OVERRIDE.with(|o| *o.borrow_mut() = Some(init));
    let r = serialize(name, get);
    INIT_OVERRIDE.with(|o| *o.borrow_mut() = None);
    r
}

/// Whether a computed-value serialization is in progress.
pub fn computed_mode() -> bool {
    INIT_OVERRIDE.with(|o| o.borrow().is_some())
}

fn initial(name: &str) -> String {
    if let Some(v) = INIT_OVERRIDE.with(|o| o.borrow().as_ref().and_then(|f| f(name))) {
        return v;
    }
    let init = db::initial_specified(name).unwrap_or("");
    db::specified_text(name, init).unwrap_or_else(|| init.to_string())
}

type Out = Vec<(&'static str, String)>;

/// Expand a shorthand value (no css-wide keywords or var()) into longhands.
pub fn expand(name: &str, v: &[CV]) -> Option<Out> {
    let sh = find(name)?;
    let l = sh.longhands;
    match sh.kind {
        SK::Four => {
            let val = parse_with(&format!("<'{}'>{{1,4}}", l[0]), v)?;
            let p: Vec<String> = val.parts().iter().map(|x| lh(l[0], x)).collect::<Option<Vec<_>>>()?;
            let (t, r, b, lf) = match p.len() {
                1 => (0, 0, 0, 0),
                2 => (0, 1, 0, 1),
                3 => (0, 1, 2, 1),
                _ => (0, 1, 2, 3),
            };
            Some(vec![(l[0], p[t].clone()), (l[1], p[r].clone()), (l[2], p[b].clone()), (l[3], p[lf].clone())])
        }
        SK::Pair => {
            let val = parse_with(&format!("<'{}'>{{1,2}}", l[0]), v)?;
            let p: Vec<String> = val.parts().iter().map(|x| lh(l[0], x)).collect::<Option<Vec<_>>>()?;
            let second = p.get(1).cloned().unwrap_or_else(|| p[0].clone());
            Some(vec![(l[0], p[0].clone()), (l[1], second)])
        }
        SK::Combo => {
            let src = l.iter().map(|n| format!("<'{n}'>")).collect::<Vec<_>>().join(" || ");
            let val = parse_with(&src, v)?;
            let V::Group(g) = val else { return None };
            let mut out = Vec::new();
            for (i, n) in l.iter().enumerate() {
                out.push((*n, if g[i].is_none() { initial(n) } else { lh(n, &g[i])? }));
            }
            Some(out)
        }
        SK::Custom => super::shorthand2::expand(name, l, v),
    }
}

/// Serialize a shorthand from its longhands' specified values. None when a
/// longhand is missing or the values can't be expressed by the shorthand.
pub fn serialize(name: &str, get: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let sh = find(name)?;
    let vals: Vec<String> = sh.longhands.iter().map(|n| get(n)).collect::<Option<Vec<_>>>()?;
    // css-wide keywords: all the same, or not serializable.
    let wide: Vec<bool> = vals.iter().map(|v| super::is_css_wide(v)).collect();
    if wide.iter().any(|w| *w) {
        if wide.iter().all(|w| *w) && vals.iter().all(|v| v == &vals[0]) {
            return Some(vals[0].clone());
        }
        return None;
    }
    if vals.iter().any(|v| v.is_empty()) {
        return None;
    }
    match sh.kind {
        SK::Four => Some(collapse4(&vals).join(" ")),
        SK::Pair => Some(if vals[0] == vals[1] { vals[0].clone() } else { format!("{} {}", vals[0], vals[1]) }),
        SK::Combo if name == "text-emphasis" && INIT_OVERRIDE.with(|o| o.borrow().is_some()) => Some(vals.join(" ")),
        SK::Combo => {
            let mut parts = Vec::new();
            for (i, n) in sh.longhands.iter().enumerate() {
                if vals[i] != initial(n) {
                    parts.push(vals[i].clone());
                }
            }
            if parts.is_empty() {
                // All initial: `outline: none` (its color's initial `auto` would read back as a style).
                parts.push(if name == "outline" { vals[1].clone() } else { vals[0].clone() });
            }
            Some(parts.join(" "))
        }
        SK::Custom => super::shorthand2::serialize(name, sh.longhands, &vals),
    }
}

pub fn initial_of(name: &str) -> String {
    initial(name)
}

pub fn lh_text(name: &str, v: &V) -> Option<String> {
    lh(name, v)
}

pub fn parse_src(src: &str, v: &[CV]) -> Option<V> {
    parse_with(src, v)
}
