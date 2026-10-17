// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Property values: a grammar-driven engine that validates declarations,
//! serializes specified values canonically (CSSOM), expands shorthands and
//! produces computed values for getComputedStyle — for every property in the
//! table in `db.rs`, whether or not layout uses it.

pub mod block;
pub mod db;
pub mod grammar;
pub mod hooks;
pub mod image;
pub mod interp;
pub mod path;
pub mod shorthand;
pub mod shorthand2;
pub mod table;
pub mod types;

use super::calc::{self, Node};
use super::color;
use super::tokenizer::{Tok, CV};

/// A parsed (or computed) value.
#[derive(Clone, Debug, PartialEq)]
pub enum V {
    /// An absent optional part.
    None,
    /// A keyword (lowercase).
    Kw(String),
    /// A custom identifier (case kept).
    Ident(String),
    Num(f64),
    /// A dimension with a lowercase unit; "%" for percentages.
    Dim(f64, String),
    Math(Box<MathV>),
    Str(String),
    Url(String),
    Color(Box<color::Spec>),
    /// A computed color.
    Rgba(Box<color::Abs>),
    Image(Box<image::Image>),
    Pos(Box<Pos>),
    /// A function from the grammar: name + arguments.
    Fn(String, Box<V>),
    /// Space-separated parts (juxtaposition, repetition).
    Seq(Vec<V>),
    /// Comma-separated list.
    List(Vec<V>),
    /// `||` / `&&` parts in grammar order (None for absent ones).
    Group(Vec<V>),
    /// A literal delimiter from the grammar (',' or '/').
    Delim(char),
    /// Text kept as written.
    Raw(String),
}

/// What a math function must resolve to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Len,
    LenPct,
    Pct,
    Num,
    Int,
    NumPct,
    Angle,
    AnglePct,
    Time,
    Res,
    Flex,
    Freq,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MathV {
    pub node: Node,
    pub kind: Kind,
    pub range: Option<(f64, f64)>,
}

/// A `<position>`: horizontal and vertical edges.
#[derive(Clone, Debug, PartialEq)]
pub struct Pos {
    pub h: Edge,
    pub v: Edge,
    /// The number of values as written (1–4).
    pub n: u8,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    /// left/right/top/bottom/center/x-start/… (None: just an offset).
    pub kw: Option<String>,
    pub off: Option<V>,
}

impl V {
    pub fn is_none(&self) -> bool {
        matches!(self, V::None)
    }
    pub fn is_empty_group(&self) -> bool {
        match self {
            V::None => true,
            V::Group(v) | V::Seq(v) => v.iter().all(|x| x.is_empty_group()),
            _ => false,
        }
    }
    pub fn kw(&self) -> Option<&str> {
        match self {
            V::Kw(k) => Some(k),
            _ => None,
        }
    }
    pub fn is_kw(&self, k: &str) -> bool {
        self.kw() == Some(k)
    }
    /// Items of a Seq/Group/List (or the value itself).
    pub fn parts(&self) -> Vec<&V> {
        match self {
            V::Seq(v) | V::Group(v) | V::List(v) => v.iter().collect(),
            _ => vec![self],
        }
    }
    /// Non-None parts, flattened through nested Seq/Group.
    pub fn flat(&self) -> Vec<&V> {
        let mut out = Vec::new();
        fn go<'a>(v: &'a V, out: &mut Vec<&'a V>) {
            match v {
                V::None => {}
                V::Seq(x) | V::Group(x) => x.iter().for_each(|y| go(y, out)),
                _ => out.push(v),
            }
        }
        go(self, &mut out);
        out
    }
    pub fn is_zero_len(&self) -> bool {
        matches!(self, V::Dim(v, u) if *v == 0.0 && u != "%") || matches!(self, V::Num(v) if *v == 0.0)
    }
}

// ------------------------------------------------------------------ serialization

pub fn fmt(v: f64) -> String {
    calc::fmt_num(v)
}

/// CSSOM "serialize a string".
pub fn ser_string(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '\0' => o.push('\u{FFFD}'),
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\u{1}'..='\u{1f}' | '\u{7f}' => o.push_str(&format!("\\{:x} ", c as u32)),
            _ => o.push(c),
        }
    }
    o.push('"');
    o
}

/// CSSOM "serialize an identifier".
pub fn ser_ident(s: &str) -> String {
    let mut o = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        let esc_digit = c.is_ascii_digit() && (i == 0 || (i == 1 && chars[0] == '-'));
        if c == '\0' {
            o.push('\u{FFFD}');
        } else if ('\u{1}'..='\u{1f}').contains(&c) || c == '\u{7f}' || esc_digit {
            o.push_str(&format!("\\{:x} ", c as u32));
        } else if i == 0 && c == '-' && chars.len() == 1 {
            o.push_str("\\-");
        } else if c as u32 >= 0x80 || c == '-' || c == '_' || c.is_ascii_alphanumeric() {
            o.push(c);
        } else {
            o.push('\\');
            o.push(c);
        }
    }
    o
}

pub fn ser_url(u: &str) -> String {
    format!("url({})", ser_string(u))
}

pub fn join_parts(parts: &[String]) -> String {
    let mut o = String::new();
    for p in parts {
        if p.is_empty() {
            continue;
        }
        if p.starts_with(',') {
            o.push_str(p);
            continue;
        }
        if !o.is_empty() {
            o.push(' ');
        }
        o.push_str(p);
    }
    o
}

pub fn ser(v: &V) -> String {
    match v {
        V::None => String::new(),
        V::Kw(k) => k.clone(),
        V::Ident(s) => ser_ident(s),
        V::Num(n) => fmt(*n),
        V::Dim(n, u) => format!("{}{}", fmt(*n), u),
        V::Math(m) => ser_math(&m.node),
        V::Str(s) => ser_string(s),
        V::Url(u) => ser_url(u),
        V::Color(c) => color::ser_specified(c),
        V::Rgba(c) => color::ser_computed(c),
        V::Image(i) => image::ser_image(i),
        V::Pos(p) => ser_pos(p),
        V::Fn(name, args) => format!("{}({})", name, ser_args(args)),
        V::Seq(items) | V::Group(items) => join_parts(&items.iter().map(ser).collect::<Vec<_>>()),
        V::List(items) => items.iter().map(ser).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        V::Delim(c) => c.to_string(),
        V::Raw(s) => s.clone(),
    }
}

/// Function arguments: comma literals followed by a space.
pub fn ser_args(v: &V) -> String {
    ser(v)
}

pub fn ser_math(n: &Node) -> String {
    calc::serialize(n)
}

pub fn ser_edge(e: &Edge) -> String {
    match (&e.kw, &e.off) {
        (Some(k), Some(o)) => format!("{} {}", k, ser(o)),
        (Some(k), None) => k.clone(),
        (None, Some(o)) => ser(o),
        (None, None) => String::new(),
    }
}

/// Specified `<position>`: horizontal then vertical.
pub fn ser_pos(p: &Pos) -> String {
    format!("{} {}", ser_edge(&p.h), ser_edge(&p.v))
}

// ------------------------------------------------------------------ computed values

/// What computed values resolve against.
#[derive(Clone, Debug)]
pub struct Env {
    pub em: f64,
    pub rem: f64,
    pub ex: f64,
    pub ch: f64,
    pub lh: f64,
    pub rlh: f64,
    pub vw: f64,
    pub vh: f64,
    pub cqw: f64,
    pub cqh: f64,
    pub current: color::Abs,
    pub dark: bool,
    pub base: String,
    /// Zoom (effective) – lengths in computed values are unzoomed.
    pub zoom: f64,
}

impl Default for Env {
    fn default() -> Self {
        Env { em: 16.0, rem: 16.0, ex: 8.0, ch: 8.0, lh: 19.2, rlh: 19.2, vw: 800.0, vh: 600.0, cqw: 800.0, cqh: 600.0, current: color::Abs::srgb(0.0, 0.0, 0.0, 1.0), dark: false, base: String::new(), zoom: 1.0 }
    }
}

/// URL resolution hook (set by the browser; identity when unset).
pub static URL_RESOLVE: std::sync::OnceLock<fn(&str, &str) -> Option<String>> = std::sync::OnceLock::new();

pub fn resolve_url(base: &str, u: &str) -> String {
    match URL_RESOLVE.get() {
        Some(f) => f(base, u).unwrap_or_else(|| u.to_string()),
        None => u.to_string(),
    }
}

/// px per unit for lengths (absolute and relative).
pub fn len_factor(u: &str, env: &Env) -> Option<f64> {
    Some(match u {
        "px" => 1.0,
        "in" => 96.0,
        "cm" => 96.0 / 2.54,
        "mm" => 96.0 / 25.4,
        "q" => 96.0 / 101.6,
        "pt" => 96.0 / 72.0,
        "pc" => 16.0,
        "em" => env.em,
        "rem" => env.rem,
        "ex" => env.ex,
        "rex" => env.rem * 0.5,
        "ch" => env.ch,
        "rch" => env.rem * 0.5,
        "cap" => env.em * 0.7,
        "rcap" => env.rem * 0.7,
        "ic" => env.em,
        "ric" => env.rem,
        "lh" => env.lh,
        "rlh" => env.rlh,
        "vw" | "svw" | "lvw" | "dvw" | "vi" | "svi" | "lvi" | "dvi" => env.vw / 100.0,
        "vh" | "svh" | "lvh" | "dvh" | "vb" | "svb" | "lvb" | "dvb" => env.vh / 100.0,
        "vmin" | "svmin" | "lvmin" | "dvmin" => env.vw.min(env.vh) / 100.0,
        "vmax" | "svmax" | "lvmax" | "dvmax" => env.vw.max(env.vh) / 100.0,
        "cqw" | "cqi" => env.cqw / 100.0,
        "cqh" | "cqb" => env.cqh / 100.0,
        "cqmin" => env.cqw.min(env.cqh) / 100.0,
        "cqmax" => env.cqw.max(env.cqh) / 100.0,
        _ => return None,
    })
}

/// Canonical unit of a non-length dimension (deg, s, hz, dppx).
pub fn canon_dim(v: f64, u: &str) -> (f64, &'static str) {
    match u {
        "deg" => (v, "deg"),
        "grad" => (v * 0.9, "deg"),
        "rad" => (v.to_degrees(), "deg"),
        "turn" => (v * 360.0, "deg"),
        "s" => (v, "s"),
        "ms" => (v / 1000.0, "s"),
        "hz" => (v, "hz"),
        "khz" => (v * 1000.0, "hz"),
        "dppx" | "x" => (v, "dppx"),
        "dpi" => (v / 96.0, "dppx"),
        "dpcm" => (v * 2.54 / 96.0, "dppx"),
        "fr" => (v, "fr"),
        "%" => (v, "%"),
        _ => (v, "px"),
    }
}

/// A dimension's computed form.
pub fn compute_dim(v: f64, u: &str, env: &Env) -> V {
    if u == "%" {
        return V::Dim(v, "%".into());
    }
    if let Some(k) = len_factor(u, env) {
        return V::Dim(v * k, "px".into());
    }
    let (cv, cu) = canon_dim(v, u);
    V::Dim(cv, cu.into())
}

/// Resolve relative units in a calculation and simplify it.
pub fn resolve_node(n: &Node, env: &Env) -> Node {
    let r = calc::map_dims(n, &|v, u| len_factor(u, env).map(|k| Node::Dim(v * k, "px".into())));
    calc::simplify(&r, &|_| None)
}

fn clamp_range(v: f64, range: Option<(f64, f64)>) -> f64 {
    match range {
        Some((lo, hi)) => v.max(lo).min(hi),
        None => v,
    }
}

pub fn compute_math(m: &MathV, env: &Env) -> V {
    let n = resolve_node(&m.node, env);
    match &n {
        Node::Num(v) => {
            let mut v = *v;
            if v.is_nan() {
                v = 0.0;
            }
            if m.kind == Kind::Int {
                v = (v + 0.5).floor();
            }
            V::Num(clamp_range(v, m.range))
        }
        Node::Dim(v, u) => {
            let mut v = *v;
            if v.is_nan() {
                v = 0.0;
            }
            let v = clamp_range(v, m.range);
            if u == "px" || u == "%" {
                V::Dim(v, u.clone())
            } else {
                let (cv, cu) = canon_dim(v, u);
                V::Dim(cv, cu.into())
            }
        }
        _ => {
            // A percentage mixed with a length stays a calculation (clamped when used).
            let mut mv = m.clone();
            mv.node = n;
            V::Math(Box::new(mv))
        }
    }
}

/// Generic computed value: absolute lengths, canonical units, computed colors,
/// absolute URLs.
pub fn compute(v: &V, env: &Env) -> V {
    match v {
        V::Dim(n, u) => compute_dim(*n, u, env),
        V::Math(m) => compute_math(m, env),
        V::Color(c) => match color::compute(c, &env.current, env.dark) {
            Some(a) => V::Rgba(Box::new(a)),
            None => v.clone(),
        },
        V::Url(u) => V::Url(resolve_url(&env.base, u)),
        V::Image(i) => V::Image(Box::new(image::compute_image(i, env))),
        V::Pos(p) => V::Pos(Box::new(compute_pos(p, env))),
        V::Fn(n, a) => V::Fn(n.clone(), Box::new(compute(a, env))),
        V::Seq(x) => V::Seq(x.iter().map(|y| compute(y, env)).collect()),
        V::List(x) => V::List(x.iter().map(|y| compute(y, env)).collect()),
        V::Group(x) => V::Group(x.iter().map(|y| compute(y, env)).collect()),
        _ => v.clone(),
    }
}

/// A computed `<position>`: two length-percentages (keywords as percentages,
/// right/bottom offsets as `calc(100% - x)`).
pub fn compute_pos(p: &Pos, env: &Env) -> Pos {
    let side = |e: &Edge| -> V {
        let off = e.off.as_ref().map(|o| compute(o, env));
        let kw = e.kw.as_deref();
        let far = matches!(kw, Some("right") | Some("bottom") | Some("x-end") | Some("y-end"));
        match (kw, off) {
            (Some("center"), _) => V::Dim(50.0, "%".into()),
            (Some(_), None) => V::Dim(if far { 100.0 } else { 0.0 }, "%".into()),
            (_, Some(o)) if !far => o,
            (_, Some(o)) => sub_from_100(&o),
            (None, None) => V::Dim(50.0, "%".into()),
        }
    };
    Pos { h: Edge { kw: None, off: Some(side(&p.h)) }, v: Edge { kw: None, off: Some(side(&p.v)) }, n: 2 }
}

/// `100% - x` for a computed length-percentage.
pub fn sub_from_100(o: &V) -> V {
    match o {
        V::Dim(v, u) if u == "%" => V::Dim(100.0 - v, "%".into()),
        V::Dim(v, u) => V::Math(Box::new(MathV { node: Node::Sum(vec![Node::Dim(100.0, "%".into()), Node::Dim(-v, u.clone())]), kind: Kind::LenPct, range: None })),
        V::Math(m) => {
            let n = calc::simplify(&Node::Sum(vec![Node::Dim(100.0, "%".into()), Node::Negate(Box::new(m.node.clone()))]), &|_| None);
            V::Math(Box::new(MathV { node: n, kind: Kind::LenPct, range: None }))
        }
        _ => o.clone(),
    }
}

// ------------------------------------------------------------------ public API

pub fn is_css_wide(s: &str) -> bool {
    matches!(s.to_ascii_lowercase().as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")
}

/// Whether component values contain var()/env()/attr() (pending substitution).
pub fn has_var(v: &[CV]) -> bool {
    v.iter().any(|c| match c {
        CV::Func(n, a) => {
            let l = n.to_ascii_lowercase();
            l == "var" || l == "env" || l == "attr" || l == "if" || has_var(a)
        }
        CV::Block(_, a) => has_var(a),
        _ => false,
    })
}

/// Trim and collapse a raw value for pending-substitution serialization.
pub fn raw_text(v: &[CV]) -> String {
    let v = super::tokenizer::trim(v);
    super::tokenizer::serialize(v)
}

/// A css-wide keyword value (alone), lowercased.
pub fn css_wide_of(v: &[CV]) -> Option<String> {
    let it = grammar::items(v);
    if it.len() == 1 {
        if let CV::T(Tok::Ident(s)) = it[0] {
            if is_css_wide(s) {
                return Some(s.to_ascii_lowercase());
            }
        }
    }
    None
}

#[allow(unused_imports)]
pub use db::{computed_text, initial_text, is_inherited, is_known, longhands_of, parse_decl, parse_value, specified_text, supports};

/// Properties whose getComputedStyle value stays with the native style code
/// (resolved/used values from layout, or element-dependent fixups).
pub const KEEP_NATIVE: &[&str] = &[
    "display", "position", "float", "width", "height", "min-width", "min-height", "max-width", "max-height", "margin", "margin-top", "margin-right", "margin-bottom",
    "margin-left", "padding", "padding-top", "padding-right", "padding-bottom", "padding-left", "top", "right", "bottom", "left", "inset", "transform", "color",
    "background-color", "font-size", "line-height", "font-weight", "border-top-width", "border-right-width", "border-bottom-width", "border-left-width", "border-width",
    "border-top-color", "border-right-color", "border-bottom-color", "border-left-color", "border-color", "outline-color", "outline-width", "visibility", "overflow",
    "overflow-x", "overflow-y", "direction", "writing-mode", "caret-color", "fill", "stroke", "font-family", ];

thread_local! {
    static ROUTED: std::cell::RefCell<std::collections::HashMap<String, Option<&'static str>>> = Default::default();
}

/// The engine key of a property whose computed value the engine provides
/// (a longhand or a shorthand), or None for natively handled ones.
pub fn engine_routed(name: &str) -> Option<&'static str> {
    if let Some(r) = ROUTED.with(|c| c.borrow().get(name).copied()) {
        return r;
    }
    let r = (|| {
        if name.starts_with("--") || KEEP_NATIVE.contains(&name) {
            return None;
        }
        let n = db::canonical_name(name);
        if KEEP_NATIVE.contains(&n.as_str()) {
            return None;
        }
        if let Some(sh) = shorthand::find(&n) {
            return Some(sh.name);
        }
        db::with_db(|d| d.props.get_key_value(n.as_str()).map(|(k, _)| *k))
    })();
    ROUTED.with(|c| c.borrow_mut().insert(name.to_string(), r));
    r
}

/// Whether a property (every longhand of a shorthand) is inherited.
pub fn all_inherited(key: &str) -> bool {
    match shorthand::longhands(key) {
        Some(l) => l.iter().all(|n| db::is_inherited(n)),
        None => db::is_inherited(key),
    }
}


/// A longhand's declared value from a declaration of `key` (itself or a
/// shorthand containing it): Some(tokens), or None if `key` doesn't set it.
pub fn longhand_value(key: &str, longhand: &str, v: &[CV]) -> Option<Vec<CV>> {
    if key == longhand {
        return Some(v.to_vec());
    }
    let l = shorthand::longhands(key)?;
    if !l.contains(&longhand) {
        return None;
    }
    let p = db::parse_decl(key, v)?;
    let text = p.longhands.iter().find(|(n, _)| *n == longhand).map(|(_, t)| t.clone())?;
    Some(super::tokenizer::parse_cvs(&text))
}

/// Whether `v` matches a registered custom property's syntax string (as in
/// `@property { syntax: "<length> | auto" }`). Syntaxes with data types this
/// engine can't check are treated as matching.
pub fn matches_syntax(syntax: &str, v: &[CV]) -> bool {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    thread_local!(static CACHE: RefCell<HashMap<String, Option<Rc<grammar::G>>>> = RefCell::new(HashMap::new()));
    const CHECKED: &[&str] = &["length", "number", "percentage", "length-percentage", "color", "integer", "angle", "time", "resolution", "custom-ident", "image", "url", "string", "transform-function", "transform-list"];
    let g = CACHE.with(|c| {
        c.borrow_mut()
            .entry(syntax.to_string())
            .or_insert_with(|| {
                // Only simple syntaxes: `|`-separated terms of known types or idents,
                // optionally multiplied by `+` or `#`.
                for term in syntax.split('|') {
                    let t = term.trim().trim_end_matches(['+', '#']);
                    if let Some(name) = t.strip_prefix('<').and_then(|x| x.strip_suffix('>')) {
                        if !CHECKED.contains(&name) {
                            return None;
                        }
                    } else if t.is_empty() || !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                        return None;
                    }
                }
                Some(Rc::new(grammar::parse(&syntax.replace("<transform-list>", "<transform-function>+"))))
            })
            .clone()
    });
    let Some(g) = g else { return true };
    let items = grammar::items(v);
    if items.is_empty() {
        return true;
    }
    // Unsubstituted references can't be checked here.
    if items.iter().any(|c| matches!(c, CV::Func(f, _) if f.eq_ignore_ascii_case("var") || f.eq_ignore_ascii_case("env") || f.eq_ignore_ascii_case("attr"))) {
        return true;
    }
    db::with_db(|d| grammar::match_all(d, &g, &items).is_some())
}
