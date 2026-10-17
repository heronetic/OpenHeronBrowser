// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Leaf value types: numbers and dimensions (with math functions), strings,
//! URLs, identifiers, colors, images and positions.

use super::super::calc::{self, Node};
use super::super::color;
use super::super::tokenizer::{Tok, CV};
use super::{Edge, Kind, MathV, Pos, V};

pub type Res = Vec<(usize, V)>;

fn one(pos: usize, v: Option<V>) -> Res {
    match v {
        Some(v) => vec![(pos + 1, v)],
        None => vec![],
    }
}

fn in_range(v: f64, range: Option<(f64, f64)>) -> bool {
    match range {
        Some((lo, hi)) => v >= lo && v <= hi,
        None => true,
    }
}

thread_local! {
    /// Clamp mode (re-parsing interpolated values): out-of-range numbers are
    /// clamped and non-integers rounded instead of rejected.
    static CLAMP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with clamp mode on.
pub fn with_clamp<R>(f: impl FnOnce() -> R) -> R {
    let old = CLAMP.with(|c| c.replace(true));
    let r = f();
    CLAMP.with(|c| c.set(old));
    r
}

fn clamping() -> bool {
    CLAMP.with(|c| c.get())
}

fn clampv(v: f64, range: Option<(f64, f64)>) -> f64 {
    match range {
        Some((lo, hi)) => v.max(lo).min(hi),
        None => v,
    }
}

fn kind_of(name: &str) -> Option<Kind> {
    Some(match name {
        "length" => Kind::Len,
        "length-percentage" => Kind::LenPct,
        "percentage" => Kind::Pct,
        "number" => Kind::Num,
        "integer" => Kind::Int,
        "number-percentage" => Kind::NumPct,
        "angle" | "angle-zero" => Kind::Angle,
        "angle-percentage" => Kind::AnglePct,
        "time" => Kind::Time,
        "resolution" => Kind::Res,
        "flex" => Kind::Flex,
        "frequency" => Kind::Freq,
        _ => return None,
    })
}

/// The calc type a kind requires, and what percentages resolve against.
fn kind_type(k: Kind) -> (Vec<calc::CType>, Option<usize>) {
    use calc::*;
    match k {
        Kind::Len => (vec![base(LEN)], None),
        Kind::LenPct => (vec![base(LEN)], Some(LEN)),
        Kind::Pct => (vec![base(PCT)], None),
        Kind::Num | Kind::Int => (vec![NUMBER], None),
        Kind::NumPct => (vec![NUMBER, base(PCT)], None),
        Kind::Angle => (vec![base(ANGLE)], None),
        Kind::AnglePct => (vec![base(ANGLE)], Some(ANGLE)),
        Kind::Time => (vec![base(TIME)], None),
        Kind::Res => (vec![base(RES)], None),
        Kind::Flex => (vec![base(FLEX)], None),
        Kind::Freq => (vec![base(FREQ)], None),
    }
}

/// Parse a math function as a value of `kind`.
pub fn math(cv: &CV, kind: Kind, range: Option<(f64, f64)>) -> Option<V> {
    let CV::Func(name, args) = cv else { return None };
    if !calc::is_math_fn(name) {
        return None;
    }
    let node = calc::parse_fn(name, args)?;
    let (types, pct_as) = kind_type(kind);
    let t = calc::node_type(&node, pct_as)?;
    // A sum of a percentage and a length in a length-percentage context is fine.
    if !types.contains(&t) {
        return None;
    }
    let simplified = calc::simplify(&node, &|_| None);
    Some(V::Math(Box::new(MathV { node: simplified, kind, range })))
}

fn unit_ok(kind: Kind, u: &str) -> bool {
    let b = calc::unit_base(u);
    match kind {
        Kind::Len | Kind::LenPct => b == Some(calc::LEN),
        Kind::Angle | Kind::AnglePct => b == Some(calc::ANGLE),
        Kind::Time => b == Some(calc::TIME),
        Kind::Res => b == Some(calc::RES),
        Kind::Flex => b == Some(calc::FLEX),
        Kind::Freq => b == Some(calc::FREQ),
        _ => false,
    }
}

/// A numeric leaf of `kind` (one component value).
pub fn numeric(cv: &CV, kind: Kind, range: Option<(f64, f64)>, zero_angle: bool) -> Option<V> {
    if clamping() {
        match cv {
            CV::T(Tok::Number(n)) if matches!(kind, Kind::Num | Kind::NumPct | Kind::Int) => {
                // (Halves round up even when interpolation lands a hair below them.)
                let n = if kind == Kind::Int { (*n + 0.5 + 1e-7).floor() } else { *n };
                return Some(V::Num(clampv(n, range)));
            }
            CV::T(Tok::Percentage(p)) if matches!(kind, Kind::Pct | Kind::LenPct | Kind::NumPct | Kind::AnglePct) => return Some(V::Dim(clampv(*p, range), "%".into())),
            CV::T(Tok::Dimension(n, u)) => {
                let u = u.to_ascii_lowercase();
                return unit_ok(kind, &u).then(|| V::Dim(clampv(*n, range), u));
            }
            _ => {}
        }
    }
    match cv {
        CV::T(Tok::Number(n)) => {
            let n = *n;
            match kind {
                Kind::Num | Kind::NumPct => in_range(n, range).then_some(V::Num(n)),
                Kind::Int => (n.fract() == 0.0 && in_range(n, range)).then_some(V::Num(n)),
                Kind::Len | Kind::LenPct if n == 0.0 => Some(V::Dim(0.0, "px".into())),
                Kind::Angle | Kind::AnglePct if n == 0.0 && zero_angle => Some(V::Dim(0.0, "deg".into())),
                _ => None,
            }
        }
        CV::T(Tok::Percentage(p)) => match kind {
            Kind::Pct | Kind::LenPct | Kind::NumPct | Kind::AnglePct => in_range(*p, range).then(|| V::Dim(*p, "%".into())),
            _ => None,
        },
        CV::T(Tok::Dimension(n, u)) => {
            let u = u.to_ascii_lowercase();
            if !unit_ok(kind, &u) || !in_range(*n, range) {
                return None;
            }
            Some(V::Dim(*n, u))
        }
        CV::Func(..) => math(cv, kind, range),
        _ => None,
    }
}

pub fn is_css_wide(s: &str) -> bool {
    super::is_css_wide(s)
}

/// Identifier leaf types: custom-ident (with exclusions), dashed-ident, ident.
fn ident_leaf(name: &str, cv: &CV) -> Option<V> {
    let CV::T(Tok::Ident(s)) = cv else { return None };
    if name == "ident" {
        return Some(V::Ident(s.clone()));
    }
    if name == "dashed-ident" {
        return s.starts_with("--").then(|| V::Ident(s.clone()));
    }
    // custom-ident [except a b c]
    if is_css_wide(s) || s.eq_ignore_ascii_case("default") {
        return None;
    }
    if let Some(ex) = name.strip_prefix("custom-ident except ") {
        if ex.split_whitespace().any(|e| e.eq_ignore_ascii_case(s)) {
            return None;
        }
    }
    Some(V::Ident(s.clone()))
}

pub fn url_of(cv: &CV) -> Option<String> {
    match cv {
        CV::T(Tok::Url(u)) => Some(u.clone()),
        CV::Func(n, args) if n.eq_ignore_ascii_case("url") || n.eq_ignore_ascii_case("src") => {
            let it: Vec<&CV> = args.iter().filter(|c| !c.is_ws()).collect();
            match it.as_slice() {
                [CV::T(Tok::Str(s))] => Some(s.clone()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Match a leaf type at `pos`.
pub fn leaf(name: &str, range: Option<(f64, f64)>, inp: &[&CV], pos: usize) -> Res {
    if let Some(k) = kind_of(name) {
        return match inp.get(pos) {
            Some(cv) => one(pos, numeric(cv, k, range, name == "angle-zero")),
            None => vec![],
        };
    }
    let Some(cv) = inp.get(pos) else { return vec![] };
    match name {
        "string" => one(pos, if let CV::T(Tok::Str(s)) = cv { Some(V::Str(s.clone())) } else { None }),
        "url" => one(pos, url_of(cv).map(V::Url)),
        "color" => one(pos, color::parse(cv).filter(|c| !matches!(c, color::Spec::Raw(_))).map(|c| V::Color(Box::new(c)))),
        "image" => one(pos, super::image::parse(cv).map(|i| V::Image(Box::new(i)))),
        "position" => position(inp, pos, false),
        "bg-position" => position(inp, pos, true),
        "zero" => one(pos, matches!(cv, CV::T(Tok::Number(n)) if *n == 0.0).then_some(V::Num(0.0))),
        "shape-fn" => one(pos, shape_fn(cv)),
        "any-value" | "declaration-value" => {
            let rest = &inp[pos..];
            let text = rest.iter().map(|c| super::super::tokenizer::serialize(std::slice::from_ref(*c))).collect::<Vec<_>>().join(" ");
            vec![(inp.len(), V::Raw(text))]
        }
        _ if name == "ident" || name == "dashed-ident" || name.starts_with("custom-ident") => one(pos, ident_leaf(name, cv)),
        _ => vec![],
    }
}

// ------------------------------------------------------------------ <position>

fn lp(cv: &CV) -> Option<V> {
    numeric(cv, Kind::LenPct, None, false)
}

fn kw_of(cv: &CV) -> Option<String> {
    match cv {
        CV::T(Tok::Ident(s)) => {
            let l = s.to_ascii_lowercase();
            matches!(l.as_str(), "left" | "right" | "top" | "bottom" | "center").then_some(l)
        }
        _ => None,
    }
}

fn is_h(k: &str) -> bool {
    matches!(k, "left" | "right" | "center")
}
fn is_v(k: &str) -> bool {
    matches!(k, "top" | "bottom" | "center")
}

fn edge(kw: Option<&str>, off: Option<V>) -> Edge {
    Edge { kw: kw.map(|s| s.to_string()), off }
}

/// Parse exactly `n` values at `pos` as a position.
fn position_n(t: &[&CV], bg: bool) -> Option<Pos> {
    let k: Vec<Option<String>> = t.iter().map(|c| kw_of(c)).collect();
    let l: Vec<Option<V>> = t.iter().map(|c| if kw_of(c).is_some() { None } else { lp(c) }).collect();
    for i in 0..t.len() {
        if k[i].is_none() && l[i].is_none() {
            return None;
        }
    }
    let center = || edge(Some("center"), None);
    match t.len() {
        1 => {
            if let Some(kw) = &k[0] {
                if kw == "top" || kw == "bottom" {
                    return Some(Pos { h: center(), v: edge(Some(kw), None), n: 1 });
                }
                return Some(Pos { h: edge(Some(kw), None), v: center(), n: 1 });
            }
            Some(Pos { h: edge(None, l[0].clone()), v: center(), n: 1 })
        }
        2 => {
            match (&k[0], &k[1]) {
                (Some(a), Some(b)) => {
                    if is_h(a) && is_v(b) {
                        Some(Pos { h: edge(Some(a), None), v: edge(Some(b), None), n: 2 })
                    } else if is_v(a) && is_h(b) {
                        Some(Pos { h: edge(Some(b), None), v: edge(Some(a), None), n: 2 })
                    } else {
                        None
                    }
                }
                (Some(a), None) => is_h(a).then(|| Pos { h: edge(Some(a), None), v: edge(None, l[1].clone()), n: 2 }),
                (None, Some(b)) => is_v(b).then(|| Pos { h: edge(None, l[0].clone()), v: edge(Some(b), None), n: 2 }),
                (None, None) => Some(Pos { h: edge(None, l[0].clone()), v: edge(None, l[1].clone()), n: 2 }),
            }
        }
        3 if bg => {
            // [ center | [left|right] <lp>? ] && [ center | [top|bottom] <lp>? ]
            let groups: Vec<(String, Option<V>)> = if k[0].is_some() && k[1].is_none() && k[2].is_some() {
                vec![(k[0].clone()?, l[1].clone()), (k[2].clone()?, None)]
            } else if k[0].is_some() && k[1].is_some() && k[2].is_none() {
                vec![(k[0].clone()?, None), (k[1].clone()?, l[2].clone())]
            } else {
                return None;
            };
            side_pair(&groups, 3)
        }
        4 => {
            if k[0].is_some() && k[1].is_none() && k[2].is_some() && k[3].is_none() {
                side_pair(&[(k[0].clone()?, l[1].clone()), (k[2].clone()?, l[3].clone())], 4)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Two keyword(+offset) groups, one per axis; `center` takes no offset.
fn side_pair(g: &[(String, Option<V>)], n: u8) -> Option<Pos> {
    let (a, b) = (&g[0], &g[1]);
    for (k, o) in g {
        if k == "center" && o.is_some() {
            return None;
        }
    }
    let h_only = |k: &str| matches!(k, "left" | "right");
    let v_only = |k: &str| matches!(k, "top" | "bottom");
    let (h, v) = if h_only(&a.0) {
        if h_only(&b.0) {
            return None;
        }
        (a, b)
    } else if v_only(&a.0) {
        if v_only(&b.0) {
            return None;
        }
        (b, a)
    } else if h_only(&b.0) {
        (b, a)
    } else {
        (a, b)
    };
    Some(Pos { h: edge(Some(&h.0), h.1.clone()), v: edge(Some(&v.0), v.1.clone()), n })
}

/// `<position>` (or `<bg-position>` with its three-value forms) at `pos`.
pub fn position(inp: &[&CV], pos: usize, bg: bool) -> Res {
    let mut out = Vec::new();
    for n in (1..=4).rev() {
        if pos + n > inp.len() {
            continue;
        }
        if let Some(p) = position_n(&inp[pos..pos + n], bg) {
            out.push((pos + n, V::Pos(Box::new(p))));
        }
    }
    out
}

/// Numeric literal value of a V (for range checks in hooks).
pub fn num_of(v: &V) -> Option<f64> {
    match v {
        V::Num(n) => Some(*n),
        V::Dim(n, _) => Some(*n),
        V::Math(m) => match &m.node {
            Node::Num(n) => Some(*n),
            Node::Dim(n, _) => Some(*n),
            _ => None,
        },
        _ => None,
    }
}

// ------------------------------------------------------------------ shape()

fn ident_is(cv: Option<&&CV>, k: &str) -> bool {
    matches!(cv, Some(CV::T(Tok::Ident(s))) if s.eq_ignore_ascii_case(k))
}

fn ident_lc(cv: Option<&&CV>) -> Option<String> {
    match cv {
        Some(CV::T(Tok::Ident(s))) => Some(s.to_ascii_lowercase()),
        _ => None,
    }
}

/// The longest `<position>` at `i` (the whole rest when `to_end`).
fn shape_pos(t: &[&CV], i: usize, to_end: bool) -> Option<(usize, V)> {
    let c = position(t, i, false);
    if to_end {
        c.into_iter().find(|(e, _)| *e == t.len())
    } else {
        c.into_iter().next()
    }
}

fn coord_pair(t: &[&CV], i: usize) -> Option<(usize, V)> {
    let a = lp(t.get(i)?)?;
    let b = lp(t.get(i + 1)?)?;
    Some((i + 2, V::Seq(vec![a, b])))
}

/// `to <position>` | `by <coordinate-pair>`.
fn end_point(t: &[&CV], i: usize) -> Option<(usize, bool, V)> {
    if ident_is(t.get(i), "to") {
        let (e, p) = shape_pos(t, i + 1, false)?;
        return Some((e, false, p));
    }
    if ident_is(t.get(i), "by") {
        let (e, p) = coord_pair(t, i + 1)?;
        return Some((e, true, p));
    }
    None
}

/// A control point; `rel_default` is the anchor omitted when serializing.
fn control_point(t: &[&CV], i: usize, by: bool) -> Option<(usize, V)> {
    // A relative control point: a coordinate pair with an optional anchor.
    if let Some((e, pair)) = coord_pair(t, i) {
        if ident_is(t.get(e), "from") {
            let a = ident_lc(t.get(e + 1)).filter(|a| matches!(a.as_str(), "start" | "end" | "origin"))?;
            let default = if by { "start" } else { "origin" };
            if a == default {
                return Some((e + 2, pair));
            }
            let V::Seq(mut p) = pair else { return None };
            p.push(V::Kw("from".into()));
            p.push(V::Kw(a));
            return Some((e + 2, V::Seq(p)));
        }
        if e == t.len() || ident_is(t.get(e), "with") || matches!(t.get(e), Some(CV::T(Tok::Delim('/')))) {
            return Some((e, pair));
        }
    }
    // An absolute position.
    let c = position(t, i, false);
    c.into_iter().find(|(e, _)| *e == t.len() || matches!(t.get(*e), Some(CV::T(Tok::Delim('/')))))
}

fn shape_command(t: &[&CV]) -> Option<V> {
    let cmd = ident_lc(t.first())?;
    let kw = |s: &str| V::Kw(s.into());
    match cmd.as_str() {
        "close" if t.len() == 1 => Some(kw("close")),
        "move" | "line" => {
            let (e, by, p) = end_point(t, 1)?;
            (e == t.len()).then(|| V::Seq(vec![kw(&cmd), kw(if by { "by" } else { "to" }), p]))
        }
        "hline" | "vline" => {
            if t.len() != 3 {
                return None;
            }
            if ident_is(t.get(1), "by") {
                return Some(V::Seq(vec![kw(&cmd), kw("by"), lp(t[2])?]));
            }
            if !ident_is(t.get(1), "to") {
                return None;
            }
            if let Some(k) = ident_lc(t.get(2)) {
                let ok = if cmd == "hline" { matches!(k.as_str(), "left" | "center" | "right" | "x-start" | "x-end") } else { matches!(k.as_str(), "top" | "center" | "bottom" | "y-start" | "y-end") };
                return ok.then(|| V::Seq(vec![kw(&cmd), kw("to"), V::Kw(k)]));
            }
            Some(V::Seq(vec![kw(&cmd), kw("to"), lp(t[2])?]))
        }
        "curve" | "smooth" => {
            let (mut e, by, p) = end_point(t, 1)?;
            let mut out = vec![kw(&cmd), kw(if by { "by" } else { "to" }), p];
            if ident_is(t.get(e), "with") {
                out.push(kw("with"));
                let (e1, c1) = control_point(&t[..], e + 1, by)?;
                out.push(c1);
                e = e1;
                if cmd == "curve" && matches!(t.get(e), Some(CV::T(Tok::Delim('/')))) {
                    let (e2, c2) = control_point(&t[..], e + 1, by)?;
                    out.push(V::Delim('/'));
                    out.push(c2);
                    e = e2;
                }
            } else if cmd == "curve" {
                return None;
            }
            (e == t.len()).then_some(V::Seq(out))
        }
        "arc" => {
            let (mut e, by, p) = end_point(t, 1)?;
            let mut radius: Option<Vec<V>> = None;
            let mut sweep: Option<String> = None;
            let mut size: Option<String> = None;
            let mut rot: Option<V> = None;
            while e < t.len() {
                match ident_lc(t.get(e)).as_deref() {
                    Some("of") if radius.is_none() => {
                        let a = lp(t.get(e + 1)?)?;
                        e += 2;
                        let mut r = vec![a];
                        if let Some(b) = t.get(e).and_then(|c| lp(c)) {
                            r.push(b);
                            e += 1;
                        }
                        radius = Some(r);
                    }
                    Some(k @ ("cw" | "ccw")) if sweep.is_none() => {
                        sweep = Some(k.to_string());
                        e += 1;
                    }
                    Some(k @ ("large" | "small")) if size.is_none() => {
                        size = Some(k.to_string());
                        e += 1;
                    }
                    Some("rotate") if rot.is_none() => {
                        rot = Some(numeric(t.get(e + 1)?, Kind::Angle, None, true)?);
                        e += 2;
                    }
                    _ => return None,
                }
            }
            let radius = radius?;
            let mut out = vec![kw("arc"), kw(if by { "by" } else { "to" }), p, kw("of")];
            out.extend(radius);
            if let Some(s) = sweep.filter(|s| s != "ccw") {
                out.push(V::Kw(s));
            }
            if let Some(s) = size.filter(|s| s != "small") {
                out.push(V::Kw(s));
            }
            if let Some(r) = rot.filter(|r| !matches!(r, V::Dim(v, _) if *v == 0.0)) {
                out.push(kw("rotate"));
                out.push(r);
            }
            Some(V::Seq(out))
        }
        _ => None,
    }
}

/// `shape( <fill-rule>? from <position> , <shape-command># )`.
fn shape_fn(cv: &CV) -> Option<V> {
    let CV::Func(name, args) = cv else { return None };
    if !name.eq_ignore_ascii_case("shape") {
        return None;
    }
    let segs = super::super::tokenizer::split_commas(args);
    if segs.len() < 2 {
        return None;
    }
    let mut items = Vec::new();
    let head: Vec<&CV> = segs[0].iter().filter(|c| !c.is_ws()).collect();
    let mut i = 0;
    let mut first = Vec::new();
    if let Some(r) = ident_lc(head.first()).filter(|r| r == "nonzero" || r == "evenodd") {
        if r == "evenodd" {
            first.push(V::Kw(r));
        }
        i = 1;
    }
    if !ident_is(head.get(i), "from") {
        return None;
    }
    let (_, p) = shape_pos(&head, i + 1, true)?;
    first.push(V::Kw("from".into()));
    first.push(p);
    items.push(V::Seq(first));
    for seg in &segs[1..] {
        let t: Vec<&CV> = seg.iter().filter(|c| !c.is_ws()).collect();
        items.push(shape_command(&t)?);
    }
    Some(V::Fn("shape".into(), Box::new(V::List(items))))
}
