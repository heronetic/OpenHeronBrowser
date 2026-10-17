// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Animation interpolation of computed values (CSS Values 4 §3 "Combining
//! Values"; each property's animation type: by computed value, repeatable
//! list, or discrete).

use super::super::calc::{self, Node};
use super::super::color;
use super::super::tokenizer;
use super::{db, ser, Edge, Env, Kind, MathV, Pos, V};

/// Properties that can't be animated at all (keyframe declarations of them
/// are ignored, and transitions don't run).
pub fn animatable(prop: &str) -> bool {
    !(prop.starts_with("animation")
        || prop.starts_with("transition")
        || prop.starts_with("scroll-timeline")
        || prop.starts_with("view-timeline")
        || matches!(prop, "direction" | "unicode-bidi" | "will-change" | "container" | "container-name" | "container-type" | "timeline-scope" | "writing-mode" | "text-orientation" | "contain" | "text-combine-upright"))
}

/// Properties whose numeric-looking values still animate discretely.
fn discrete_only(prop: &str) -> bool {
    matches!(
        prop,
        "font-feature-settings"
            | "counter-increment"
            | "counter-reset"
            | "counter-set"
            | "grid-template-areas"
            | "grid-row-start"
            | "grid-row-end"
            | "grid-column-start"
            | "grid-column-end"
            | "grid-auto-flow"
            | "column-span"
            | "initial-letter"
            | "quotes"
            | "content"
            | "font-language-override"
            | "hyphenate-limit-chars"
            | "list-style-type"
            | "anchor-name"
            | "position-anchor"
            | "position-try-fallbacks"
            | "view-transition-name"
            | "view-transition-class"
            | "font-family"
            | "white-space-collapse"
    )
}

fn lerp(a: f64, b: f64, p: f64) -> f64 {
    a + (b - a) * p
}

fn node_of(v: &V) -> Option<Node> {
    match v {
        V::Dim(n, u) => Some(Node::Dim(*n, u.clone())),
        V::Num(n) => Some(Node::Num(*n)),
        V::Math(m) => Some(m.node.clone()),
        _ => None,
    }
}

/// `a·(1−p) + b·p` for mixed dimensions (a length and a percentage, or calc()).
fn mix_math(a: &V, b: &V, p: f64) -> Option<V> {
    let (na, nb) = (node_of(a)?, node_of(b)?);
    let kind = match (a, b) {
        (V::Math(m), _) | (_, V::Math(m)) => m.kind,
        _ => Kind::LenPct,
    };
    // Only lengths and percentages mix.
    let lp = |v: &V| match v {
        V::Dim(_, u) => u == "%" || u == "px",
        V::Math(m) => matches!(m.kind, Kind::Len | Kind::LenPct | Kind::Pct),
        _ => false,
    };
    if !lp(a) || !lp(b) {
        return None;
    }
    let n = Node::Sum(vec![Node::Product(vec![na, Node::Num(1.0 - p)]), Node::Product(vec![nb, Node::Num(p)])]);
    let mut s = calc::simplify(&n, &|_| None);
    // A zero length term is dropped (a zero percentage stays: `calc(0% + 10px)`).
    if let Node::Sum(t) = &s {
        if t.len() == 2 && t.iter().any(|x| matches!(x, Node::Dim(v, u) if u == "px" && *v == 0.0)) {
            if let Some(pct) = t.iter().find(|x| matches!(x, Node::Dim(_, u) if u == "%")) {
                s = pct.clone();
            }
        }
    }
    Some(match s {
        Node::Dim(v, u) => V::Dim(v, u),
        other => V::Math(Box::new(MathV { node: other, kind: if kind == Kind::Len { Kind::LenPct } else { kind }, range: None })),
    })
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

fn each(a: &[V], b: &[V], p: f64) -> Option<Vec<V>> {
    if a.len() != b.len() {
        return None;
    }
    a.iter().zip(b).map(|(x, y)| lerp_v(x, y, p)).collect()
}

/// Structural interpolation of two computed values; None when they can't be
/// combined (the caller then flips discretely).
pub fn lerp_v(a: &V, b: &V, p: f64) -> Option<V> {
    if a == b {
        return Some(a.clone());
    }
    Some(match (a, b) {
        (V::Num(x), V::Num(y)) => V::Num(lerp(*x, *y, p)),
        (V::Dim(x, u), V::Dim(y, w)) if u == w => V::Dim(lerp(*x, *y, p), u.clone()),
        (V::Dim(..) | V::Math(..), V::Dim(..) | V::Math(..)) => mix_math(a, b, p)?,
        (V::Rgba(x), V::Rgba(y)) => V::Rgba(Box::new(color::interpolate(x, y, p))),
        (V::Pos(x), V::Pos(y)) => {
            let e = |a: &Edge, b: &Edge| -> Option<Edge> {
                if a.kw != b.kw {
                    return None;
                }
                let off = match (&a.off, &b.off) {
                    (Some(x), Some(y)) => Some(lerp_v(x, y, p)?),
                    (None, None) => None,
                    _ => return None,
                };
                Some(Edge { kw: a.kw.clone(), off })
            };
            V::Pos(Box::new(Pos { h: e(&x.h, &y.h)?, v: e(&x.v, &y.v)?, n: x.n.max(y.n) }))
        }
        (V::Fn(n, x), V::Fn(m, y)) if n == "shape" && m == "shape" => match (&**x, &**y) {
            (V::List(a), V::List(b)) if a.len() == b.len() => {
                let mut out = Vec::with_capacity(a.len());
                for (i, j) in a.iter().zip(b) {
                    out.push(if i.parts().first().map(|k| k.is_kw("arc")).unwrap_or(false) { lerp_arc(i, j, p)? } else { lerp_v(i, j, p)? });
                }
                V::Fn(n.clone(), Box::new(V::List(out)))
            }
            _ => return None,
        },
        (V::Fn(n, x), V::Fn(m, y)) if n.eq_ignore_ascii_case(m) => {
            // Keyword radii don't interpolate.
            if (n == "circle" || n == "ellipse") && [x, y].iter().any(|v| v.flat().iter().any(|k| matches!(k, V::Kw(k) if k.starts_with("closest") || k.starts_with("farthest")))) {
                return None;
            }
            if n == "path" {
                return lerp_path(x, y, p);
            }
            // polygon() vertices pair up one to one.
            if n == "polygon" {
                let len = |v: &V| v.parts().iter().find_map(|q| if let V::List(l) = q { Some(l.len()) } else { None });
                if len(x) != len(y) {
                    return None;
                }
            }
            V::Fn(n.clone(), Box::new(lerp_v(x, y, p)?))
        }
        (V::Seq(x), V::Seq(y)) => V::Seq(each(x, y, p)?),
        (V::Group(x), V::Group(y)) => V::Group(each(x, y, p)?),
        (V::List(x), V::List(y)) => {
            // Repeatable lists: both repeated to their least common multiple length.
            if x.is_empty() || y.is_empty() {
                return None;
            }
            let n = x.len() / gcd(x.len(), y.len()) * y.len();
            if n > 1000 {
                return None;
            }
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                out.push(lerp_v(&x[i % x.len()], &y[i % y.len()], p)?);
            }
            V::List(out)
        }
        _ => return None,
    })
}

/// shape() arc commands: radii and rotation interpolate; the cw/large flags
/// interpolate as 0/1 numbers (set while positive).
fn lerp_arc(a: &V, b: &V, p: f64) -> Option<V> {
    fn norm(v: &V) -> Option<(Vec<V>, f64, f64, V)> {
        let V::Seq(x) = v else { return None };
        let mut head = Vec::new();
        let (mut cw, mut large, mut rot) = (0.0, 0.0, V::Dim(0.0, "deg".into()));
        let mut i = 0;
        while i < x.len() {
            match &x[i] {
                V::Kw(k) if k == "cw" => cw = 1.0,
                V::Kw(k) if k == "large" => large = 1.0,
                V::Kw(k) if k == "rotate" => {
                    rot = x.get(i + 1)?.clone();
                    i += 1;
                }
                o => head.push(o.clone()),
            }
            i += 1;
        }
        Some((head, cw, large, rot))
    }
    let (ha, ca, la, ra) = norm(a)?;
    let (hb, cb, lb, rb) = norm(b)?;
    let mut out = each(&ha, &hb, p)?;
    if lerp(ca, cb, p) > 0.0 {
        out.push(V::Kw("cw".into()));
    }
    if lerp(la, lb, p) > 0.0 {
        out.push(V::Kw("large".into()));
    }
    let r = lerp_v(&ra, &rb, p)?;
    if !matches!(r, V::Dim(v, _) if v == 0.0) {
        out.push(V::Kw("rotate".into()));
        out.push(r);
    }
    Some(V::Seq(out))
}

fn path_parts(v: &V) -> Option<(bool, Vec<super::path::Seg>)> {
    let flat = v.flat();
    let evenodd = flat.iter().any(|x| x.is_kw("evenodd"));
    let text = flat.iter().find_map(|x| if let V::Str(t) = x { Some(t.clone()) } else { None })?;
    Some((evenodd, super::path::parse(&text)?))
}

fn path_v(evenodd: bool, segs: &[super::path::Seg]) -> V {
    let s = V::Str(super::path::serialize(segs));
    if evenodd {
        V::Seq(vec![V::Kw("evenodd".into()), V::Delim(','), s])
    } else {
        s
    }
}

/// path() arguments: same fill rule, matching commands.
fn lerp_path(x: &V, y: &V, p: f64) -> Option<V> {
    let (fa, sa) = path_parts(x)?;
    let (fb, sb) = path_parts(y)?;
    if fa != fb {
        return None;
    }
    Some(V::Fn("path".into(), Box::new(path_v(fa, &super::path::interpolate(&sa, &sb, p)?))))
}

/// Interpolate a property's computed values; None if not interpolable.
pub fn interpolate(prop: &str, a: &V, b: &V, p: f64) -> Option<V> {
    if !animatable(prop) || discrete_only(prop) {
        return None;
    }
    if a == b {
        return Some(a.clone());
    }
    match prop {
        "aspect-ratio" => return ratio(a, b, p),
        "box-shadow" | "text-shadow" => return lerp_shadows(a, b, p),
        "filter" | "backdrop-filter" => return lerp_filters(a, b, p),
        "grid-template-columns" | "grid-template-rows" => return lerp_tracks(a, b, p),
        _ => {}
    }
    lerp_v(a, b, p)
}

fn shadow_list(v: &V) -> Option<Vec<V>> {
    match v {
        V::Kw(k) if k == "none" => Some(vec![]),
        V::List(x) => Some(x.clone()),
        V::Seq(_) => Some(vec![v.clone()]),
        _ => None,
    }
}

/// A transparent zero shadow shaped like `like` (same inset-ness).
fn zero_shadow(like: &V) -> V {
    let V::Seq(p) = like else { return like.clone() };
    V::Seq(
        p.iter()
            .map(|x| match x {
                V::Rgba(_) | V::Color(_) => V::Rgba(Box::new(color::Abs::srgb(0.0, 0.0, 0.0, 0.0))),
                V::Dim(_, u) => V::Dim(0.0, u.clone()),
                V::Math(_) => V::Dim(0.0, "px".into()),
                o => o.clone(),
            })
            .collect(),
    )
}

/// Shadow lists: the shorter is padded with transparent zero shadows; inset
/// and outer shadows don't interpolate with each other.
fn lerp_shadows(a: &V, b: &V, p: f64) -> Option<V> {
    let (x, y) = (shadow_list(a)?, shadow_list(b)?);
    let n = x.len().max(y.len());
    if n == 0 {
        return Some(V::Kw("none".into()));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let sa = x.get(i).cloned().unwrap_or_else(|| zero_shadow(&y[i]));
        let sb = y.get(i).cloned().unwrap_or_else(|| zero_shadow(&x[i]));
        let inset = |s: &V| s.parts().iter().any(|k| k.is_kw("inset"));
        if inset(&sa) != inset(&sb) {
            return None;
        }
        out.push(lerp_v(&sa, &sb, p)?);
    }
    Some(V::List(out))
}

fn filter_list(v: &V) -> Option<Vec<V>> {
    match v {
        V::Kw(k) if k == "none" => Some(vec![]),
        V::Seq(x) => Some(x.clone()),
        V::Fn(..) => Some(vec![v.clone()]),
        _ => None,
    }
}

/// The identity (no-op) form of a filter function.
fn filter_identity(f: &V) -> Option<V> {
    let V::Fn(name, _) = f else { return None };
    let arg = match name.as_str() {
        "blur" => V::Dim(0.0, "px".into()),
        "brightness" | "contrast" | "opacity" | "saturate" => V::Num(1.0),
        "grayscale" | "invert" | "sepia" => V::Num(0.0),
        "hue-rotate" => V::Dim(0.0, "deg".into()),
        "drop-shadow" => {
            let V::Fn(_, args) = f else { return None };
            zero_shadow(args)
        }
        _ => return None,
    };
    Some(V::Fn(name.clone(), Box::new(arg)))
}

/// Filter lists interpolate function by function when one is a prefix of
/// the other (the rest pad with identity functions); else discretely.
fn lerp_filters(a: &V, b: &V, p: f64) -> Option<V> {
    let (x, y) = (filter_list(a)?, filter_list(b)?);
    let name = |v: &V| match v {
        V::Fn(n, _) => Some(n.clone()),
        _ => None,
    };
    for i in 0..x.len().min(y.len()) {
        if name(&x[i])? != name(&y[i])? {
            return None;
        }
    }
    let n = x.len().max(y.len());
    if n == 0 {
        return Some(V::Kw("none".into()));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let fa = match x.get(i) {
            Some(f) => f.clone(),
            None => filter_identity(&y[i])?,
        };
        let fb = match y.get(i) {
            Some(f) => f.clone(),
            None => filter_identity(&x[i])?,
        };
        out.push(lerp_v(&fa, &fb, p)?);
    }
    Some(V::Seq(out))
}

/// Track lists of the same length interpolate track by track; a pair of
/// tracks that can't interpolate flips on its own.
fn lerp_tracks(a: &V, b: &V, p: f64) -> Option<V> {
    match (a, b) {
        (V::Seq(x), V::Seq(y)) if x.len() == y.len() => {
            // repeat() must line up with repeat().
            let rep = |v: &V| matches!(v, V::Fn(n, _) if n == "repeat");
            if x.iter().zip(y).any(|(i, j)| rep(i) != rep(j)) {
                return None;
            }
            Some(V::Seq(x.iter().zip(y).map(|(i, j)| lerp_tracks(i, j, p)).collect::<Option<Vec<_>>>()?))
        }
        (V::Seq(_), V::Seq(_)) => None,
        (V::Fn(n, x), V::Fn(m, y)) if n == m => {
            // repeat(): the same count (or auto-fill/auto-fit) is required.
            if n == "repeat" && x.parts().first() != y.parts().first() {
                return None;
            }
            Some(V::Fn(n.clone(), Box::new(lerp_tracks(x, y, p)?)))
        }
        _ if a.is_kw("none") || b.is_kw("none") || a.is_kw("subgrid") || b.is_kw("subgrid") || a.is_kw("masonry") || b.is_kw("masonry") => (a == b).then(|| a.clone()),
        _ => Some(lerp_v(a, b, p).unwrap_or_else(|| if p < 0.5 { a.clone() } else { b.clone() })),
    }
}

fn add_tracks(a: &V, b: &V) -> Option<V> {
    match (a, b) {
        (V::Seq(x), V::Seq(y)) if x.len() == y.len() => Some(V::Seq(x.iter().zip(y).map(|(i, j)| add_tracks(i, j)).collect::<Option<Vec<_>>>()?)),
        (V::Seq(_), V::Seq(_)) => None,
        _ => Some(add_v(a, b).unwrap_or_else(|| b.clone())),
    }
}

/// `aspect-ratio`: ratios interpolate in log space; `auto` must match.
fn ratio(a: &V, b: &V, p: f64) -> Option<V> {
    fn split(v: &V) -> Option<(Option<String>, f64)> {
        let mut kw = None;
        let mut nums = Vec::new();
        for x in v.flat() {
            match x {
                V::Kw(k) => kw = Some(k.clone()),
                V::Num(n) => nums.push(*n),
                V::Delim('/') => {}
                _ => return None,
            }
        }
        let r = match nums.as_slice() {
            [w] => *w,
            [w, h] if *h != 0.0 => w / h,
            _ => return None,
        };
        Some((kw, r))
    }
    let (ka, ra) = split(a)?;
    let (kb, rb) = split(b)?;
    if ka != kb || ra <= 0.0 || rb <= 0.0 {
        return None;
    }
    let r = (lerp(ra.ln(), rb.ln(), p)).exp();
    let ratio = V::Seq(vec![V::Num(r), V::Delim('/'), V::Num(1.0)]);
    Some(match ka {
        Some(k) => V::Seq(vec![V::Kw(k), ratio]),
        None => ratio,
    })
}

/// Re-validate an interpolated value: clamp to the property's ranges (and
/// round integers), then recompute.
pub fn finalize(prop: &str, v: V, env: &Env) -> V {
    let text = ser(&v);
    let cvs = tokenizer::parse_cvs(&text);
    super::types::with_clamp(|| db::computed_value(prop, &cvs, env)).unwrap_or(v)
}

/// The value at progress `p`: interpolated, or the nearer endpoint (flip at 0.5).
pub fn blend(prop: &str, a: &V, b: &V, p: f64, env: &Env) -> V {
    match interpolate(prop, a, b, p) {
        Some(v) if &v == a || &v == b => v,
        Some(v) => finalize(prop, v, env),
        None => {
            if p < 0.5 {
                a.clone()
            } else {
                b.clone()
            }
        }
    }
}

// ------------------------------------------------------------------ addition

/// `a + b` for mixed dimensions.
fn sum_math(a: &V, b: &V) -> Option<V> {
    mix_weighted(a, b, 1.0, 1.0)
}

fn mix_weighted(a: &V, b: &V, wa: f64, wb: f64) -> Option<V> {
    let (na, nb) = (node_of(a)?, node_of(b)?);
    let lp = |v: &V| match v {
        V::Dim(_, u) => u == "%" || u == "px",
        V::Math(m) => matches!(m.kind, Kind::Len | Kind::LenPct | Kind::Pct),
        _ => false,
    };
    if !lp(a) || !lp(b) {
        return None;
    }
    let n = Node::Sum(vec![Node::Product(vec![na, Node::Num(wa)]), Node::Product(vec![nb, Node::Num(wb)])]);
    let s = calc::simplify(&n, &|_| None);
    Some(match s {
        Node::Dim(v, u) => V::Dim(v, u),
        other => V::Math(Box::new(MathV { node: other, kind: Kind::LenPct, range: None })),
    })
}

fn add_v(a: &V, b: &V) -> Option<V> {
    Some(match (a, b) {
        (V::Num(x), V::Num(y)) => V::Num(x + y),
        (V::Dim(x, u), V::Dim(y, w)) if u == w => V::Dim(x + y, u.clone()),
        (V::Dim(..) | V::Math(..), V::Dim(..) | V::Math(..)) => sum_math(a, b)?,
        (V::Rgba(x), V::Rgba(y)) => V::Rgba(Box::new(color::add(x, y))),
        (V::Pos(x), V::Pos(y)) => {
            let e = |a: &Edge, b: &Edge| -> Option<Edge> {
                let off = match (&a.off, &b.off) {
                    (Some(x), Some(y)) => Some(add_v(x, y)?),
                    _ => return None,
                };
                Some(Edge { kw: None, off })
            };
            V::Pos(Box::new(Pos { h: e(&x.h, &y.h)?, v: e(&x.v, &y.v)?, n: 2 }))
        }
        (V::Fn(n, x), V::Fn(m, y)) if n.eq_ignore_ascii_case(m) => {
            if (n == "circle" || n == "ellipse") && [x, y].iter().any(|v| v.flat().iter().any(|k| matches!(k, V::Kw(k) if k.starts_with("closest") || k.starts_with("farthest")))) {
                return None;
            }
            if n == "path" {
                let (fa, sa) = path_parts(x)?;
                let (fb, sb) = path_parts(y)?;
                if fa != fb {
                    return None;
                }
                return Some(V::Fn(n.clone(), Box::new(path_v(fb, &super::path::add(&sa, &sb)?))));
            }
            V::Fn(n.clone(), Box::new(add_v(x, y)?))
        }
        (V::Seq(x), V::Seq(y)) if x.len() == y.len() => V::Seq(x.iter().zip(y).map(|(p, q)| add_v(p, q)).collect::<Option<Vec<_>>>()?),
        (V::Group(x), V::Group(y)) if x.len() == y.len() => V::Group(x.iter().zip(y).map(|(p, q)| add_v(p, q)).collect::<Option<Vec<_>>>()?),
        (V::List(x), V::List(y)) if !x.is_empty() && !y.is_empty() => {
            let n = x.len() / gcd(x.len(), y.len()) * y.len();
            if n > 1000 {
                return None;
            }
            V::List((0..n).map(|i| add_v(&x[i % x.len()], &y[i % y.len()])).collect::<Option<Vec<_>>>()?)
        }
        _ if a == b && !matches!(a, V::Num(_) | V::Dim(..) | V::Math(..)) => b.clone(),
        (V::None, V::None) => V::None,
        _ => return None,
    })
}

/// Value addition for `composite: add` (`acc`: accumulate); None when the
/// values don't add (the effect value replaces).
pub fn add(prop: &str, u: &V, v: &V, acc: bool) -> Option<V> {
    if !animatable(prop) || discrete_only(prop) {
        return None;
    }
    match prop {
        // Shadow and filter lists are appended (accumulate adds item-wise).
        "box-shadow" | "text-shadow" | "filter" | "backdrop-filter" if !acc => {
            if u.is_kw("none") {
                return Some(v.clone());
            }
            if v.is_kw("none") {
                return Some(u.clone());
            }
            let mut out: Vec<V> = u.parts().into_iter().cloned().collect();
            out.extend(v.parts().into_iter().cloned());
            return Some(if prop.ends_with("filter") { V::Seq(out) } else { V::List(out) });
        }
        "grid-template-columns" | "grid-template-rows" => return add_tracks(u, v),
        // Implicit track sizes don't add (the effect value replaces).
        "grid-auto-columns" | "grid-auto-rows" => return None,
        "box-shadow" | "text-shadow" => {
            let (x, y) = (shadow_list(u)?, shadow_list(v)?);
            let n = x.len().max(y.len());
            let mut out = Vec::new();
            for i in 0..n {
                let sa = x.get(i).cloned().unwrap_or_else(|| zero_shadow(&y[i]));
                let sb = y.get(i).cloned().unwrap_or_else(|| zero_shadow(&x[i]));
                out.push(add_v(&sa, &sb)?);
            }
            return Some(if out.is_empty() { V::Kw("none".into()) } else { V::List(out) });
        }
        "filter" | "backdrop-filter" => {
            let (x, y) = (filter_list(u)?, filter_list(v)?);
            let mut out = Vec::new();
            for i in 0..x.len().max(y.len()) {
                match (x.get(i), y.get(i)) {
                    // Functions whose identity is 1 accumulate as a + b − 1.
                    (Some(V::Fn(n, a)), Some(V::Fn(m, b))) if n == m && matches!(n.as_str(), "brightness" | "contrast" | "opacity" | "saturate") => match (&**a, &**b) {
                        (V::Num(p), V::Num(q)) => out.push(V::Fn(n.clone(), Box::new(V::Num(p + q - 1.0)))),
                        _ => return None,
                    },
                    (Some(a), Some(b)) => out.push(add_v(a, b)?),
                    (Some(a), None) => out.push(a.clone()),
                    (None, Some(b)) => out.push(b.clone()),
                    _ => {}
                }
            }
            return Some(if out.is_empty() { V::Kw("none".into()) } else { V::Seq(out) });
        }
        _ => {}
    }
    add_v(u, v)
}
