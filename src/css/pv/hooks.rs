// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Property-specific validation, canonicalization (specified values) and
//! computed-value adjustments on top of the generic grammar engine.

use super::super::calc::Node;
use super::super::color;
use super::{fmt, ser, Env, Kind, MathV, V};

fn pct_to_num(v: V) -> V {
    match v {
        V::Dim(p, u) if u == "%" => V::Num(p / 100.0),
        o => o,
    }
}

/// Collapse 1–4 side values (top right bottom left) like margin.
pub fn collapse4(v: &[String]) -> Vec<String> {
    let mut v: Vec<String> = match v.len() {
        1 => vec![v[0].clone(); 4],
        2 => vec![v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()],
        3 => vec![v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()],
        _ => v.to_vec(),
    };
    if v[3] == v[1] {
        v.pop();
        if v[2] == v[0] {
            v.pop();
            if v[1] == v[0] {
                v.pop();
            }
        }
    }
    v
}

/// 1–4 side values, expanded to four (top, right, bottom, left).
fn expand4_v(v: &V) -> Vec<V> {
    let it: Vec<V> = v.flat().into_iter().cloned().collect();
    match it.len() {
        1 => vec![it[0].clone(); 4],
        2 => vec![it[0].clone(), it[1].clone(), it[0].clone(), it[1].clone()],
        3 => vec![it[0].clone(), it[1].clone(), it[2].clone(), it[1].clone()],
        _ => it,
    }
}

/// Four side values in their shortest form.
fn collapse4_v(v: &V) -> V {
    let mut it = expand4_v(v);
    if it.len() == 4 && ser(&it[3]) == ser(&it[1]) {
        it.pop();
        if ser(&it[2]) == ser(&it[0]) {
            it.pop();
            if ser(&it[1]) == ser(&it[0]) {
                it.pop();
            }
        }
    }
    V::Seq(it)
}

fn is_side4(prop: &str) -> bool {
    matches!(prop, "border-image-width" | "border-image-outset" | "mask-border-width" | "mask-border-outset")
}
fn is_slice(prop: &str) -> bool {
    matches!(prop, "border-image-slice" | "mask-border-slice")
}

fn is_lit_zero_angle(v: &V) -> bool {
    matches!(v, V::Dim(n, _) if *n == 0.0)
}

/// Display: canonical short forms.
fn display_canon(v: &V) -> V {
    let words: Vec<String> = v.flat().iter().map(|x| ser(x)).collect();
    let has = |w: &str| words.iter().any(|x| x == w);
    if words.len() == 1 {
        return match words[0].as_str() {
            "flow" => V::Kw("block".into()),
            "flow-root" | "table" | "flex" | "grid" | "grid-lanes" | "ruby" | "math" if false => V::Kw(words[0].clone()),
            _ => v.flat()[0].clone(),
        };
    }
    if has("list-item") {
        let outside = words.iter().find(|w| matches!(w.as_str(), "block" | "inline" | "run-in")).cloned();
        let inside = words.iter().find(|w| matches!(w.as_str(), "flow" | "flow-root")).cloned();
        let mut out = Vec::new();
        if let Some(o) = outside.filter(|o| o != "block") {
            out.push(o);
        }
        if let Some(i) = inside.filter(|i| i != "flow") {
            out.push(i);
        }
        out.push("list-item".into());
        return V::Raw(out.join(" "));
    }
    let outside = words.iter().find(|w| matches!(w.as_str(), "block" | "inline" | "run-in")).cloned().unwrap_or_else(|| "block".into());
    let inside = words.iter().find(|w| !matches!(w.as_str(), "block" | "inline" | "run-in")).cloned().unwrap_or_else(|| "flow".into());
    let s = match (outside.as_str(), inside.as_str()) {
        ("block", "flow") => "block".to_string(),
        ("inline", "flow") => "inline".to_string(),
        ("run-in", "flow") => "run-in".to_string(),
        ("block", "flow-root") => "flow-root".to_string(),
        ("inline", "flow-root") => "inline-block".to_string(),
        ("block", "ruby") => "block ruby".to_string(),
        ("inline", "ruby") => "ruby".to_string(),
        ("block", i) => i.to_string(),
        ("inline", i @ ("table" | "flex" | "grid" | "grid-lanes")) => format!("inline-{i}"),
        ("inline", "math") => "math".to_string(),
        (o, i) => format!("{o} {i}"),
    };
    V::Raw(s)
}

/// Property-specific checks and canonical forms for specified values.
/// None rejects the value.
/// polygon(): the default fill rule and a zero rounding are omitted.
fn polygon_canon(v: V) -> V {
    match v {
        V::Fn(n, a) if n == "polygon" => {
            let parts: Vec<V> = a.parts().into_iter().cloned().collect();
            let Some(points) = parts.iter().rev().find(|x| matches!(x, V::List(_) | V::Seq(_))).cloned() else { return V::Fn(n, a) };
            let mut head: Vec<V> = Vec::new();
            let flat: Vec<V> = parts.iter().filter(|x| !std::ptr::eq(*x, parts.last().unwrap()) || !matches!(x, V::List(_))).flat_map(|x| x.flat().into_iter().cloned().collect::<Vec<_>>()).collect();
            let mut i = 0;
            while i < flat.len() {
                match &flat[i] {
                    V::Kw(k) if k == "evenodd" => head.push(flat[i].clone()),
                    V::Kw(k) if k == "round" => {
                        if let Some(r) = flat.get(i + 1) {
                            if !matches!(r, V::Dim(z, _) if *z == 0.0) && !matches!(r, V::Num(z) if *z == 0.0) {
                                head.push(V::Kw("round".into()));
                                head.push(r.clone());
                            }
                            i += 1;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            if !matches!(parts.last(), Some(V::List(_))) {
                return V::Fn(n, a);
            }
            if head.is_empty() {
                return V::Fn(n, Box::new(points));
            }
            head.push(V::Delim(','));
            head.push(points);
            V::Fn(n, Box::new(V::Seq(head)))
        }
        V::Group(g) => V::Group(g.into_iter().map(polygon_canon).collect()),
        V::Seq(g) => V::Seq(g.into_iter().map(polygon_canon).collect()),
        o => o,
    }
}

/// Computed polygon(): fill rule and rounding always present (so values of
/// the same shape line up for interpolation); serialization omits defaults.
fn polygon_full(v: V) -> V {
    match v {
        V::Fn(n, a) if n == "polygon" => {
            let (mut head, points): (Vec<V>, V) = match *a {
                V::Seq(p) if matches!(p.last(), Some(V::List(_))) => {
                    let pts = p.last().unwrap().clone();
                    (p[..p.len() - 1].iter().filter(|x| !matches!(x, V::Delim(_))).cloned().collect(), pts)
                }
                o => (vec![], o),
            };
            if !head.first().map(|k| k.is_kw("evenodd")).unwrap_or(false) {
                head.insert(0, V::Kw("nonzero".into()));
            }
            if head.len() < 3 {
                head.push(V::Kw("round".into()));
                head.push(V::Dim(0.0, "px".into()));
            }
            head.push(V::Delim(','));
            head.push(points);
            V::Fn(n, Box::new(V::Seq(head)))
        }
        V::Group(g) => V::Group(g.into_iter().map(polygon_full).collect()),
        V::Seq(g) => V::Seq(g.into_iter().map(polygon_full).collect()),
        o => o,
    }
}

/// The serialized form of a computed polygon(): defaults dropped.
fn polygon_short(v: &V) -> V {
    match v {
        V::Fn(n, a) if n == "polygon" => {
            let V::Seq(p) = &**a else { return v.clone() };
            let mut head: Vec<V> = Vec::new();
            for x in p.iter() {
                if matches!(x, V::List(_)) {
                    break;
                }
                head.push(x.clone());
            }
            let pts = p.iter().find(|x| matches!(x, V::List(_))).cloned().unwrap_or(V::None);
            let mut out: Vec<V> = Vec::new();
            if head.first().map(|k| k.is_kw("evenodd")).unwrap_or(false) {
                out.push(V::Kw("evenodd".into()));
            }
            if let Some(i) = head.iter().position(|k| k.is_kw("round")) {
                if let Some(r) = head.get(i + 1) {
                    if !matches!(r, V::Dim(z, _) if *z == 0.0) {
                        out.push(V::Kw("round".into()));
                        out.push(r.clone());
                    }
                }
            }
            if out.is_empty() {
                return V::Fn(n.clone(), Box::new(pts));
            }
            out.push(V::Delim(','));
            out.push(pts);
            V::Fn(n.clone(), Box::new(V::Seq(out)))
        }
        V::Group(g) => V::Group(g.iter().map(polygon_short).collect()),
        V::Seq(g) => V::Seq(g.iter().map(polygon_short).collect()),
        o => o.clone(),
    }
}

/// path(): canonical path data (None if invalid); the default fill rule omitted.
pub fn path_canon(v: V) -> Option<V> {
    Some(match v {
        V::Fn(n, a) if n == "path" => {
            let flat: Vec<V> = a.flat().into_iter().cloned().collect();
            let evenodd = flat.iter().any(|x| x.is_kw("evenodd"));
            let text = flat.iter().find_map(|x| if let V::Str(t) = x { Some(t.clone()) } else { None })?;
            let canon = V::Str(super::path::canonical(&text)?);
            V::Fn(n, Box::new(if evenodd { V::Seq(vec![V::Kw("evenodd".into()), V::Delim(','), canon]) } else { canon }))
        }
        V::Group(g) => V::Group(g.into_iter().map(path_canon).collect::<Option<Vec<_>>>()?),
        V::Seq(g) => V::Seq(g.into_iter().map(path_canon).collect::<Option<Vec<_>>>()?),
        o => o,
    })
}

pub fn specified(prop: &str, v: V) -> Option<V> {
    Some(match prop {
        // cursor: image-set() strings are URLs.
        "cursor" => {
            fn urls(v: V, inside: bool) -> V {
                match v {
                    V::Str(s) if inside => V::Url(s),
                    V::Fn(n, a) => {
                        let ins = (inside || n.ends_with("image-set")) && n != "type";
                        V::Fn(n, Box::new(urls(*a, ins)))
                    }
                    V::Seq(x) => V::Seq(x.into_iter().map(|y| urls(y, inside)).collect()),
                    V::List(x) => V::List(x.into_iter().map(|y| urls(y, inside)).collect()),
                    V::Group(x) => V::Group(x.into_iter().map(|y| urls(y, inside)).collect()),
                    o => o,
                }
            }
            urls(v, false)
        }
        // offset-rotate: a zero angle beside auto/reverse is omitted.
        "offset-rotate" => {
            let f = v.flat();
            if f.len() == 2 && f.iter().any(|x| x.is_kw("auto") || x.is_kw("reverse")) && f.iter().any(|x| matches!(x, V::Dim(z, _) if *z == 0.0)) {
                f.into_iter().find(|x| matches!(x, V::Kw(_))).cloned().unwrap()
            } else {
                v
            }
        }
        // text-box-edge: a second edge implied by the first is omitted.
        "text-box-edge" => match &v {
            V::Seq(p) if p.len() == 2 => match (p[0].kw(), p[1].kw()) {
                (Some(a), Some(b)) if a == b || ((a == "cap" || a == "ex") && b == "text") => p[0].clone(),
                _ => v,
            },
            _ => v,
        },
        "clip-path" | "shape-outside" | "offset-path" => path_canon(polygon_canon(v))?,
        "d" => path_canon(v)?,
        "opacity" | "fill-opacity" | "stroke-opacity" | "stop-opacity" | "flood-opacity" | "shape-image-threshold" => pct_to_num(v),
        "display" => display_canon(&v),
        "align-content" | "justify-content" | "align-items" | "align-self" | "justify-items" | "justify-self" => first_baseline(v),
        "transition-timing-function" | "animation-timing-function" => V::List(v.parts().into_iter().map(easing_canon).collect::<Option<Vec<_>>>()?),
        "translate" => translate_canon(v),
        "vertical-align" => vertical_align_canon(v),
        "animation-name" => V::List(v.parts().into_iter().map(anim_name).collect::<Option<Vec<_>>>()?),
        "content" | "list-style-type" => counter_canon(v)?,
        "aspect-ratio" => computed(prop, v, &Env::default()),
        "font-palette" => palette_canon(v),
        "scroll-snap-type" => match &v {
            V::Seq(p) if p.len() == 2 && p[1].is_kw("proximity") => p[0].clone(),
            _ => v,
        },
        "grid-template-areas" => areas_canon(v)?,
        "grid-template-columns" | "grid-template-rows" => drop_empty_names(v),
        "overflow-clip-margin" => clip_margin_canon(v),
        "animation-range-start" | "animation-range-end" => {
            let dflt = if prop == "animation-range-start" { 0.0 } else { 100.0 };
            V::List(
                v.parts()
                    .into_iter()
                    .map(|x| match x {
                        V::Seq(p) if p.len() == 2 && matches!(&p[1], V::Dim(n, u) if u == "%" && *n == dflt) => p[0].clone(),
                        o => o.clone(),
                    })
                    .collect(),
            )
        }
        "grid-auto-flow" => match &v {
            V::Group(g) => {
                let dense = !g[1].is_none();
                match g[0].kw() {
                    Some("column") => V::Raw(if dense { "column dense".into() } else { "column".into() }),
                    _ => V::Raw(if dense { "dense".into() } else { "row".into() }),
                }
            }
            _ => v,
        },
        "font-family" => V::List(
            v.parts()
                .into_iter()
                .map(|f| match f {
                    V::Str(s) if family_unquotable(s) => V::Raw(s.split(' ').map(super::ser_ident).collect::<Vec<_>>().join(" ")),
                    o => o.clone(),
                })
                .collect(),
        ),
        "counter-reset" | "counter-set" | "counter-increment" => {
            let dflt = if prop == "counter-increment" { 1.0 } else { 0.0 };
            match &v {
                V::Seq(items) => V::Seq(
                    items
                        .iter()
                        .map(|it| match it {
                            V::Seq(p) if p.len() == 2 && p[1].is_none() => V::Seq(vec![p[0].clone(), V::Num(dflt)]),
                            o => o.clone(),
                        })
                        .collect(),
                ),
                _ => v,
            }
        }
        "font-variation-settings" | "font-feature-settings" => {
            for it in v.parts() {
                if let Some(V::Str(tag)) = it.flat().first() {
                    if tag.chars().count() != 4 || tag.chars().any(|c| !(' '..='~').contains(&c)) {
                        return None;
                    }
                }
            }
            v
        }
        "background-size" | "mask-size" => V::List(
            v.parts()
                .into_iter()
                .map(|x| match x {
                    V::Seq(p) if p.len() == 2 && p[0].is_kw("auto") && p[1].is_kw("auto") => V::Kw("auto".into()),
                    V::Seq(p) if p.len() == 1 && !p[0].is_kw("auto") => V::Seq(vec![p[0].clone(), V::Kw("auto".into())]),
                    o => o.clone(),
                })
                .collect(),
        ),
        "background-repeat" | "mask-repeat" => V::List(
            v.parts()
                .into_iter()
                .map(|x| match x {
                    V::Seq(p) if p.len() == 2 && p[0] == p[1] => p[0].clone(),
                    V::Seq(p) if p.len() == 2 && p[0].is_kw("repeat") && p[1].is_kw("no-repeat") => V::Kw("repeat-x".into()),
                    V::Seq(p) if p.len() == 2 && p[0].is_kw("no-repeat") && p[1].is_kw("repeat") => V::Kw("repeat-y".into()),
                    o => o.clone(),
                })
                .collect(),
        ),
        "border-image-repeat" | "mask-border-repeat" => match &v {
            V::Seq(p) if p.len() == 2 && p[0] == p[1] => p[0].clone(),
            _ => v,
        },
        _ if is_side4(prop) => collapse4_v(&v),
        _ if is_slice(prop) => {
            let V::Group(g) = &v else { return Some(v) };
            V::Group(vec![collapse4_v(&g[0]), g[1].clone()])
        }
        "font-style" => match &v {
            V::Seq(p) if p.len() == 2 && p[0].is_kw("oblique") && is_lit_zero_angle(&p[1]) => V::Kw("normal".into()),
            _ => v,
        },
        "border-spacing" => match &v {
            V::Seq(p) if p.len() == 2 && p[0] == p[1] => p[0].clone(),
            _ => v,
        },
        "border-top-left-radius" | "border-top-right-radius" | "border-bottom-right-radius" | "border-bottom-left-radius" | "border-start-start-radius" | "border-start-end-radius" | "border-end-start-radius" | "border-end-end-radius" => match &v {
            V::Seq(p) if p.len() == 2 && p[0] == p[1] => p[0].clone(),
            _ => v,
        },
        _ => v,
    })
}

fn px(v: f64) -> V {
    V::Dim(v, "px".into())
}

fn line_width(v: &V) -> V {
    match v.kw() {
        Some("thin") => px(1.0),
        Some("medium") => px(3.0),
        Some("thick") => px(5.0),
        _ => v.clone(),
    }
}

fn clamp_num(v: V, lo: f64, hi: f64) -> V {
    match v {
        V::Num(n) => V::Num(n.max(lo).min(hi)),
        o => o,
    }
}

fn font_size_kw(k: &str) -> Option<f64> {
    Some(match k {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 48.0,
        _ => return None,
    })
}

fn current(env: &Env) -> V {
    V::Rgba(Box::new(env.current.clone()))
}

/// Property-specific computed values (after the generic computation).
/// A length-percentage as unit → coefficient terms (None for other math).
fn lin_terms(v: &V) -> Option<Vec<(f64, String)>> {
    use super::super::calc::Node;
    fn walk(n: &Node, sign: f64, out: &mut Vec<(f64, String)>) -> Option<()> {
        match n {
            Node::Dim(v, u) => {
                match out.iter_mut().find(|(_, w)| w == u) {
                    Some(t) => t.0 += sign * v,
                    None => out.push((sign * v, u.clone())),
                }
                Some(())
            }
            Node::Sum(items) => items.iter().try_for_each(|i| walk(i, sign, out)),
            Node::Negate(i) => walk(i, -sign, out),
            _ => None,
        }
    }
    let mut out = Vec::new();
    match v {
        V::Dim(n, u) => out.push((*n, u.clone())),
        V::Math(m) => walk(&m.node, 1.0, &mut out)?,
        _ => return None,
    }
    Some(out)
}

/// Terms back to a value: zero lengths dropped (a zero percentage stays beside a length).
fn from_terms(mut t: Vec<(f64, String)>) -> V {
    use super::super::calc::Node;
    t.retain(|(v, u)| *v != 0.0 || u == "%");
    if t.len() > 1 {
        t.retain(|(v, u)| !(u != "%" && *v == 0.0));
    }
    match t.len() {
        0 => V::Dim(0.0, "px".into()),
        1 => V::Dim(t[0].0, t[0].1.clone()),
        _ => {
            t.sort_by(|a, b| (a.1 != "%").cmp(&(b.1 != "%")).then(a.1.cmp(&b.1)));
            V::Math(Box::new(super::MathV { node: Node::Sum(t.into_iter().map(|(v, u)| Node::Dim(v, u)).collect()), kind: super::Kind::LenPct, range: None }))
        }
    }
}

/// `100% - x` (as Blink: a zero length term is dropped).
fn from_100(x: &V) -> V {
    if x.is_kw("auto") {
        return V::Dim(0.0, "%".into());
    }
    match lin_terms(x) {
        Some(t) => {
            let mut r: Vec<(f64, String)> = vec![(100.0, "%".into())];
            for (v, u) in t {
                match r.iter_mut().find(|(_, w)| *w == u) {
                    Some(e) => e.0 -= v,
                    None => r.push((-v, u)),
                }
            }
            from_terms(r)
        }
        None => super::sub_from_100(x),
    }
}

/// Computed inset()/rect()/xywh(): an inset() with all four offsets.
fn inset_full(name: &str, args: &V) -> Option<V> {
    let items: Vec<V> = args.flat().into_iter().cloned().collect();
    let r = items.iter().position(|x| x.is_kw("round")).unwrap_or(items.len());
    let (edges, radius) = (&items[..r], &items[(r + 1).min(items.len())..]);
    let auto0 = |x: &V| if x.is_kw("auto") { V::Dim(0.0, "%".into()) } else { x.clone() };
    let sides: Vec<V> = match name {
        "inset" => {
            let e = edges.to_vec();
            match e.len() {
                1 => vec![e[0].clone(), e[0].clone(), e[0].clone(), e[0].clone()],
                2 => vec![e[0].clone(), e[1].clone(), e[0].clone(), e[1].clone()],
                3 => vec![e[0].clone(), e[1].clone(), e[2].clone(), e[1].clone()],
                4 => e,
                _ => return None,
            }
        }
        "rect" if edges.len() == 4 => vec![auto0(&edges[0]), from_100(&edges[1]), from_100(&edges[2]), auto0(&edges[3])],
        "xywh" if edges.len() == 4 => {
            let sum = |a: &V, b: &V| -> V {
                match (lin_terms(a), lin_terms(b)) {
                    (Some(mut x), Some(y)) => {
                        for (v, u) in y {
                            match x.iter_mut().find(|(_, w)| *w == u) {
                                Some(e) => e.0 += v,
                                None => x.push((v, u)),
                            }
                        }
                        V::Math(Box::new(super::MathV { node: super::super::calc::Node::Sum(x.into_iter().map(|(v, u)| super::super::calc::Node::Dim(v, u)).collect()), kind: super::Kind::LenPct, range: None }))
                    }
                    _ => a.clone(),
                }
            };
            vec![edges[1].clone(), from_100(&sum(&edges[0], &edges[2])), from_100(&sum(&edges[1], &edges[3])), edges[0].clone()]
        }
        _ => return None,
    };
    // Radii in full (four horizontal / four vertical) so shapes line up for interpolation.
    let slash = radius.iter().position(|x| matches!(x, V::Delim('/')));
    let (h, vr) = match slash {
        Some(i) => (radius[..i].to_vec(), radius[i + 1..].to_vec()),
        None => (radius.to_vec(), radius.to_vec()),
    };
    let zero = V::Dim(0.0, "px".into());
    let full = |r: Vec<V>| -> Vec<V> {
        if r.is_empty() {
            return vec![zero.clone(); 4];
        }
        expand4_v(&V::Seq(r))
    };
    let mut out = sides;
    out.push(V::Kw("round".into()));
    out.extend(full(h));
    out.push(V::Delim('/'));
    out.extend(full(vr));
    Some(V::Fn("inset".into(), Box::new(V::Seq(out))))
}

/// The serialized form of a computed inset(): offsets collapsed.
fn shape_short(v: &V) -> V {
    match v {
        V::Fn(n, a) if n == "inset" => {
            let items: Vec<V> = a.flat().into_iter().cloned().collect();
            if items.len() < 4 || items[..4].iter().any(|x| matches!(x, V::Kw(_))) {
                return v.clone();
            }
            let V::Seq(mut out) = collapse4_v(&V::Seq(items[..4].to_vec())) else { return v.clone() };
            let rest = &items[4..];
            if rest.len() == 10 && rest[0].is_kw("round") && matches!(rest[5], V::Delim('/')) {
                let (h, vr) = (&rest[1..5], &rest[6..10]);
                let is0 = |x: &V| matches!(x, V::Dim(z, _) if *z == 0.0);
                if !(h.iter().all(is0) && vr.iter().all(is0)) {
                    out.push(V::Kw("round".into()));
                    let V::Seq(hc) = collapse4_v(&V::Seq(h.to_vec())) else { return v.clone() };
                    out.extend(hc);
                    if h.iter().map(ser).collect::<Vec<_>>() != vr.iter().map(ser).collect::<Vec<_>>() {
                        out.push(V::Delim('/'));
                        let V::Seq(vc) = collapse4_v(&V::Seq(vr.to_vec())) else { return v.clone() };
                        out.extend(vc);
                    }
                }
            } else {
                out.extend(rest.iter().cloned());
            }
            V::Fn(n.clone(), Box::new(V::Seq(out)))
        }
        V::Group(g) => V::Group(g.iter().map(shape_short).collect()),
        V::Seq(g) => V::Seq(g.iter().map(shape_short).collect()),
        o => o.clone(),
    }
}

/// Computed shape(): hline/vline keywords become percentages.
fn shape_computed(v: V) -> V {
    match v {
        V::Fn(n, a) if n == "inset" || n == "rect" || n == "xywh" => match inset_full(&n, &a) {
            Some(x) => x,
            None => V::Fn(n, a),
        },
        V::Fn(n, a) if n == "shape" => match *a {
            V::List(items) => V::Fn(
                n,
                Box::new(V::List(
                    items
                        .into_iter()
                        .map(|c| match c {
                            V::Seq(mut p) if p.len() == 3 && (p[0].is_kw("hline") || p[0].is_kw("vline")) && p[1].is_kw("to") => {
                                if let V::Kw(k) = &p[2] {
                                    let pct = match k.as_str() {
                                        "center" => 50.0,
                                        "right" | "x-end" | "bottom" | "y-end" => 100.0,
                                        _ => 0.0,
                                    };
                                    p[2] = V::Dim(pct, "%".into());
                                }
                                V::Seq(p)
                            }
                            o => o,
                        })
                        .collect(),
                )),
            ),
            o => V::Fn(n, Box::new(o)),
        },
        V::Group(g) => V::Group(g.into_iter().map(shape_computed).collect()),
        V::Seq(g) => V::Seq(g.into_iter().map(shape_computed).collect()),
        o => o,
    }
}

/// Computed `transform-origin`: x y [z] with keywords as percentages.
fn origin_computed(v: V) -> V {
    let items: Vec<V> = v.flat().into_iter().cloned().collect();
    let pct = |k: &str| -> Option<(V, Option<bool>)> {
        // (value, Some(true) horizontal / Some(false) vertical / None either)
        Some(match k {
            "left" => (V::Dim(0.0, "%".into()), Some(true)),
            "right" => (V::Dim(100.0, "%".into()), Some(true)),
            "top" => (V::Dim(0.0, "%".into()), Some(false)),
            "bottom" => (V::Dim(100.0, "%".into()), Some(false)),
            "center" => (V::Dim(50.0, "%".into()), None),
            _ => return None,
        })
    };
    let conv: Vec<(V, Option<bool>)> = items
        .iter()
        .map(|x| match x {
            V::Kw(k) => pct(k).unwrap_or((x.clone(), None)),
            o => (o.clone(), None),
        })
        .collect();
    let center = V::Dim(50.0, "%".into());
    let (mut x, mut y, z) = match conv.len() {
        1 => match conv[0].1 {
            Some(false) => (center.clone(), conv[0].0.clone(), None),
            _ => (conv[0].0.clone(), center.clone(), None),
        },
        2 | 3 => (conv[0].0.clone(), conv[1].0.clone(), conv.get(2).map(|c| c.0.clone())),
        _ => return v,
    };
    if conv.len() >= 2 && (conv[0].1 == Some(false) || conv[1].1 == Some(true)) {
        std::mem::swap(&mut x, &mut y);
    }
    V::Seq(vec![x, y, z.unwrap_or(V::Dim(0.0, "px".into()))])
}

fn is_radius(prop: &str) -> bool {
    matches!(prop, "border-spacing" | "border-top-left-radius" | "border-top-right-radius" | "border-bottom-right-radius" | "border-bottom-left-radius" | "border-start-start-radius" | "border-start-end-radius" | "border-end-start-radius" | "border-end-end-radius")
}

pub fn computed(prop: &str, v: V, env: &Env) -> V {
    match prop {
        // SVG lengths: unitless numbers are pixels.
        "stroke-dasharray" | "stroke-width" | "stroke-dashoffset" => {
            fn num_px(v: V) -> V {
                match v {
                    V::Num(n) => V::Dim(n, "px".into()),
                    V::Seq(x) => V::Seq(x.into_iter().map(num_px).collect()),
                    V::List(x) => V::List(x.into_iter().map(num_px).collect()),
                    o => o,
                }
            }
            num_px(v)
        }
        // Corner radii: both values (they interpolate pairwise); serialization collapses.
        _ if is_radius(prop) => {
            let it: Vec<V> = v.flat().into_iter().cloned().collect();
            match it.len() {
                1 => V::Seq(vec![it[0].clone(), it[0].clone()]),
                2 => V::Seq(it),
                _ => v,
            }
        }
        "letter-spacing" if v.is_kw("normal") => px(0.0),
        "transform-origin" => origin_computed(v),
        "clip-path" | "shape-outside" | "offset-path" | "object-view-box" => polygon_full(polygon_canon(shape_computed(v))),
        _ if is_side4(prop) => V::Seq(expand4_v(&v)),
        _ if is_slice(prop) => match &v {
            V::Group(g) => V::Group(vec![V::Seq(expand4_v(&g[0])), g[1].clone()]),
            _ => v,
        },
        "opacity" | "fill-opacity" | "stroke-opacity" | "stop-opacity" | "flood-opacity" => clamp_num(pct_to_num(v), 0.0, 1.0),
        "shape-image-threshold" => clamp_num(pct_to_num(v), 0.0, 1.0),
        "align-content" | "justify-content" | "align-items" | "align-self" | "justify-items" | "justify-self" => flow_start(v),
        "transition-timing-function" | "animation-timing-function" => V::List(v.parts().into_iter().map(|x| easing_canon(x).unwrap_or_else(|| x.clone())).collect()),
        "scale" => scale_computed(v),
        "font-palette" => palette_canon(v),
        "overflow-clip-margin" => clip_margin_canon(v),
        "animation-range-start" | "animation-range-end" => specified(prop, v.clone()).unwrap_or(v),
        "font-stretch" | "font-width" => match v.kw() {
            Some(k) => {
                let p = match k {
                    "ultra-condensed" => 50.0,
                    "extra-condensed" => 62.5,
                    "condensed" => 75.0,
                    "semi-condensed" => 87.5,
                    "semi-expanded" => 112.5,
                    "expanded" => 125.0,
                    "extra-expanded" => 150.0,
                    "ultra-expanded" => 200.0,
                    _ => 100.0,
                };
                V::Dim(p, "%".into())
            }
            None => v,
        },
        "font-variation-settings" => {
            // Later duplicates win; sorted by tag.
            let mut items: Vec<(String, V)> = Vec::new();
            for it in v.parts() {
                let f = it.flat();
                if let Some(V::Str(tag)) = f.first() {
                    items.retain(|(t, _)| t != tag);
                    items.push((tag.clone(), it.clone()));
                }
            }
            if items.is_empty() {
                return v;
            }
            items.sort_by(|a, b| a.0.cmp(&b.0));
            V::List(items.into_iter().map(|(_, x)| x).collect())
        }
        "rotate" => rotate_computed(v),
        "offset-rotate" => offset_rotate_computed(v),
        "filter" | "backdrop-filter" => filter_computed(v, env),
        "background-position-x" | "background-position-y" => V::List(v.parts().into_iter().map(axis_pos_computed).collect()),
        "caret-color" => {
            let p = v.flat();
            let mut cols: Vec<V> = Vec::new();
            for (i, c) in p.iter().enumerate() {
                if i == 1 && c.is_kw("auto") {
                    break;
                }
                cols.push(if c.is_kw("auto") { current(env) } else { (*c).clone() });
            }
            if cols.len() == 1 {
                cols.pop().unwrap()
            } else {
                V::Seq(cols)
            }
        }
        "aspect-ratio" => match &v {
            V::Group(g) => {
                let ratio = match &g[1] {
                    V::Seq(r) if r.len() == 2 && r[1].is_none() => V::Seq(vec![r[0].clone(), V::Seq(vec![V::Delim('/'), V::Num(1.0)])]),
                    o => o.clone(),
                };
                V::Group(vec![g[0].clone(), ratio])
            }
            _ => v,
        },
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" | "border-block-start-width" | "border-block-end-width" | "border-inline-start-width" | "border-inline-end-width" | "outline-width" | "column-rule-width" | "-webkit-text-stroke-width" => line_width(&v),
        "word-spacing" => {
            if v.is_kw("normal") {
                px(0.0)
            } else {
                v
            }
        }
        "font-weight" => match v.kw() {
            Some("normal") => V::Num(400.0),
            Some("bold") => V::Num(700.0),
            Some("bolder") => V::Num(700.0),
            Some("lighter") => V::Num(100.0),
            _ => v,
        },
        "font-size" => match v.kw().and_then(font_size_kw) {
            Some(s) => px(s),
            None => match v.kw() {
                Some("larger") => px(env.em * 1.2),
                Some("smaller") => px(env.em / 1.2),
                _ => match &v {
                    V::Dim(p, u) if u == "%" => px(env.em * p / 100.0),
                    _ => v,
                },
            },
        },
        "line-height" => match &v {
            V::Dim(p, u) if u == "%" => px(env.em * p / 100.0),
            _ => v,
        },
        "box-shadow" | "text-shadow" => match &v {
            V::List(items) => V::List(
                items
                    .iter()
                    .map(|s| {
                        let V::Group(g) = s else { return s.clone() };
                        let col = if g[0].is_none() { current(env) } else { g[0].clone() };
                        let lens: Vec<V> = g[1].flat().into_iter().cloned().collect();
                        let n = if prop == "box-shadow" { 4 } else { 3 };
                        let mut l = lens;
                        while l.len() < n {
                            l.push(px(0.0));
                        }
                        let mut parts = vec![col];
                        parts.extend(l);
                        if prop == "box-shadow" && !g[2].is_none() {
                            parts.push(V::Kw("inset".into()));
                        }
                        V::Seq(parts)
                    })
                    .collect(),
            ),
            _ => v,
        },
        _ => v,
    }
}

/// Serialization of a computed value (some properties differ from specified).
pub fn ser_computed(prop: &str, v: &V) -> String {
    if is_radius(prop) {
        if let V::Seq(p) = v {
            if p.len() == 2 && ser_c(&p[0]) == ser_c(&p[1]) {
                return ser_c(&p[0]);
            }
        }
    }
    if prop == "letter-spacing" && matches!(v, V::Dim(z, u) if *z == 0.0 && u != "%") {
        return "normal".into();
    }
    if matches!(prop, "clip-path" | "shape-outside" | "offset-path" | "object-view-box") {
        return ser_c(&shape_short(&polygon_short(v)));
    }
    if prop == "transform-origin" {
        if let V::Seq(p) = v {
            if p.len() == 3 && matches!(&p[2], V::Dim(n, _) if *n == 0.0) {
                return ser_c(&V::Seq(p[..2].to_vec()));
            }
        }
    }
    if is_side4(prop) {
        return ser_c(&collapse4_v(v));
    }
    if is_slice(prop) {
        if let V::Group(g) = v {
            return ser_c(&V::Group(vec![collapse4_v(&g[0]), g[1].clone()]));
        }
    }
    ser_c(v)
}

fn ser_c(v: &V) -> String {
    match v {
        V::Image(i) => super::image::ser_computed(i),
        V::Seq(x) | V::Group(x) => super::join_parts(&x.iter().map(ser_c).collect::<Vec<_>>()),
        V::List(x) => x.iter().map(ser_c).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        V::Fn(n, a) => format!("{}({})", n, ser_c(a)),
        _ => ser(v),
    }
}

/// palette-mix(): default method omitted, percentages completed.
fn palette_canon(v: V) -> V {
    match v {
        V::Fn(name, args) if name == "palette-mix" => {
            let p = args.parts();
            let method: Option<&V> = match p.first() {
                Some(V::Seq(x)) if !x.is_empty() => Some(&x[0]),
                _ => None,
            };
            let items: Vec<&V> = p.get(1).map(|x| vec![*x]).unwrap_or_default();
            let list: Vec<&V> = items.iter().flat_map(|x| match x {
                V::List(l) => l.iter().collect::<Vec<_>>(),
                o => vec![*o],
            }).collect();
            let mut out: Vec<String> = Vec::new();
            if let Some(m) = method {
                let words: Vec<String> = m.flat().into_iter().filter(|x| !matches!(x, V::Delim(_))).map(ser).collect();
                let mut sp = words.get(1).cloned().unwrap_or_default();
                if sp == "xyz" {
                    sp = "xyz-d65".into();
                }
                let hue = if words.len() >= 4 && words[2] != "shorter" { Some(words[2].clone()) } else { None };
                if sp != "oklab" || hue.is_some() {
                    out.push(match hue {
                        Some(h) => format!("in {sp} {h} hue"),
                        None => format!("in {sp}"),
                    });
                }
            }
            let mut ents: Vec<(String, Option<f64>, bool)> = Vec::new();
            for it in &list {
                let (mut pal, mut pct) = (String::new(), None);
                let mut pct_text: Option<String> = None;
                for x in it.flat() {
                    match x {
                        V::Dim(n, u) if u == "%" => pct = Some(*n),
                        V::Math(m) => match &m.node {
                            Node::Dim(n, u) if u == "%" => pct = Some(*n),
                            _ => pct_text = Some(ser(x)),
                        },
                        V::Fn(..) => pal = ser(&palette_canon(x.clone())),
                        o => pal = ser(o),
                    }
                }
                let calc = pct_text.is_some();
                if let Some(t) = pct_text {
                    pal = format!("{pal} {t}");
                }
                ents.push((pal, pct, calc));
            }
            if ents.len() == 2 {
                match (ents[0].1, ents[1].1) {
                    (Some(a), None) if !ents[1].2 => ents[1].1 = Some(100.0 - a),
                    (None, Some(b)) if !ents[0].2 => ents[0].1 = Some(100.0 - b),
                    _ => {}
                }
                if ents[0].1 == Some(50.0) && ents[1].1 == Some(50.0) {
                    ents[0].1 = None;
                    ents[1].1 = None;
                }
            }
            for (pal, pct, _) in ents {
                out.push(match pct {
                    Some(p) => format!("{pal} {}%", super::fmt(p)),
                    None => pal,
                });
            }
            V::Raw(format!("palette-mix({})", out.join(", ")))
        }
        o => o,
    }
}

fn clip_margin_canon(v: V) -> V {
    let V::Group(g) = &v else { return v };
    let bx = g[0].kw().filter(|k| *k != "padding-box").map(|k| V::Kw(k.to_string()));
    let len = if g[1].is_none() || (is_zero(&g[1]) && bx.is_some()) { None } else { Some(g[1].clone()) };
    match (bx, len) {
        (Some(b), Some(l)) => V::Seq(vec![b, l]),
        (Some(b), None) => b,
        (None, Some(l)) => l,
        (None, None) => V::Dim(0.0, "px".into()),
    }
}

/// Track lists: empty line-name sets are dropped.
fn drop_empty_names(v: V) -> V {
    match v {
        V::Raw(r) if r == "[]" => V::None,
        V::Seq(x) => V::Seq(x.into_iter().map(drop_empty_names).collect()),
        V::Group(x) => V::Group(x.into_iter().map(drop_empty_names).collect()),
        V::Fn(n, a) => V::Fn(n, Box::new(drop_empty_names(*a))),
        o => o,
    }
}

/// A quoted family name that serializes as identifiers.
fn family_unquotable(s: &str) -> bool {
    !s.is_empty()
        && s.split(' ').all(|w| {
            let l = w.to_ascii_lowercase();
            !w.is_empty()
                && !w.starts_with(|c: char| c.is_ascii_digit())
                && !(w.starts_with('-') && (w.len() == 1 || w[1..].starts_with(|c: char| c.is_ascii_digit())))
                && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c as u32 >= 0x80)
                && !matches!(l.as_str(), "serif" | "sans-serif" | "cursive" | "fantasy" | "monospace" | "system-ui" | "emoji" | "math" | "fangsong" | "ui-serif" | "ui-sans-serif" | "ui-monospace" | "ui-rounded" | "inherit" | "initial" | "unset" | "default" | "revert" | "revert-layer")
        })
}

fn first_baseline(v: V) -> V {
    match v {
        V::Seq(p) if p.len() == 2 && p[0].is_kw("first") && p[1].is_kw("baseline") => V::Kw("baseline".into()),
        V::Seq(p) => V::Seq(p.into_iter().map(first_baseline).collect()),
        V::Group(p) => V::Group(p.into_iter().map(first_baseline).collect()),
        o => o,
    }
}

fn flow_start(v: V) -> V {
    match v {
        V::Kw(k) if k == "flex-start" => V::Kw("flow-start".into()),
        V::Kw(k) if k == "flex-end" => V::Kw("flow-end".into()),
        V::Seq(p) => V::Seq(p.into_iter().map(flow_start).collect()),
        V::Group(p) => V::Group(p.into_iter().map(flow_start).collect()),
        o => o,
    }
}

/// steps()/step-start/step-end canonical forms; None for invalid steps().
pub fn easing_canon(v: &V) -> Option<V> {
    let steps = |n: V, pos: Option<&str>| {
        let mut args = vec![n];
        if let Some(p) = pos {
            args.push(V::Seq(vec![V::Delim(','), V::Kw(p.into())]));
        }
        V::Fn("steps".into(), Box::new(V::Seq(args)))
    };
    Some(match v {
        V::Kw(k) if k == "step-start" => steps(V::Num(1.0), Some("start")),
        V::Kw(k) if k == "step-end" => steps(V::Num(1.0), None),
        V::Fn(name, args) if name == "steps" => {
            let p = args.parts();
            let n = p[0].clone();
            let pos = p.get(1).and_then(|x| x.flat().into_iter().find_map(|y| y.kw().map(|s| s.to_string())));
            if pos.as_deref() == Some("jump-none") {
                if let V::Num(k) = n {
                    if k < 2.0 {
                        return None;
                    }
                }
            }
            match pos.as_deref() {
                None | Some("end") | Some("jump-end") => steps(n, None),
                Some(p) => steps(n, Some(p)),
            }
        }
        o => o.clone(),
    })
}

fn is_zero(v: &V) -> bool {
    matches!(v, V::Dim(n, _) if *n == 0.0) || matches!(v, V::Num(n) if *n == 0.0)
}

fn translate_canon(v: V) -> V {
    // Seq[x, (Seq[y, z?])?]
    let V::Seq(p) = &v else { return v };
    let x = p[0].clone();
    let (mut y, mut z) = (None, None);
    if let Some(V::Seq(q)) = p.get(1) {
        y = Some(q[0].clone());
        if !q[1].is_none() {
            z = Some(q[1].clone());
        }
    }
    if z.as_ref().map(is_zero).unwrap_or(false) {
        z = None;
    }
    if z.is_none() && y.as_ref().map(is_zero).unwrap_or(false) {
        y = None;
    }
    let mut out = vec![x];
    if let Some(y) = y {
        out.push(y);
    }
    if let Some(z) = z {
        if out.len() == 1 {
            out.push(V::Dim(0.0, "px".into()));
        }
        out.push(z);
    }
    V::Seq(out)
}

fn vertical_align_canon(v: V) -> V {
    let V::Group(mut g) = v else { return v };
    if is_zero(&g[2]) && !matches!(g[2], V::Dim(_, ref u) if u == "%") {
        g[2] = V::None;
    }
    if g[2].is_none() && g[0].is_none() && (g[1].is_none() || g[1].is_kw("baseline")) {
        return V::Kw("baseline".into());
    }
    if g[1].is_kw("baseline") && (!g[0].is_none() || !g[2].is_none()) {
        g[1] = V::None;
    }
    V::Group(g)
}

fn anim_name(v: &V) -> Option<V> {
    match v {
        V::Str(s) => {
            if s.is_empty() {
                return None;
            }
            let l = s.to_ascii_lowercase();
            if matches!(l.as_str(), "none" | "initial" | "inherit" | "unset" | "default" | "revert" | "revert-layer") {
                Some(v.clone())
            } else {
                Some(V::Ident(s.clone()))
            }
        }
        o => Some(o.clone()),
    }
}

/// Counter styles: `decimal` omitted from counter()/counters(); symbols()
/// drops the default `symbolic` and checks symbol counts.
fn counter_canon(v: V) -> Option<V> {
    Some(match v {
        V::Fn(name, args) if name == "symbols" => {
            let p = args.flat();
            let ty = p.first().and_then(|x| x.kw()).map(|s| s.to_string());
            let n = p.iter().filter(|x| !matches!(x, V::Kw(_))).count();
            if matches!(ty.as_deref(), Some("numeric") | Some("alphabetic")) && n < 2 {
                return None;
            }
            if ty.as_deref() == Some("symbolic") {
                V::Fn(name, Box::new(V::Seq(p.into_iter().skip(1).cloned().collect())))
            } else {
                V::Fn(name, args)
            }
        }
        V::Fn(name, args) if name == "counter" || name == "counters" => {
            let mut p: Vec<V> = match *args {
                V::Seq(x) => x,
                o => vec![o],
            };
            // ..., <counter-style>: drop decimal
            if let Some(V::Ident(st)) = p.last() {
                if st.eq_ignore_ascii_case("decimal") && p.len() >= 3 {
                    p.truncate(p.len() - 2);
                }
            }
            let p: Vec<V> = p.into_iter().map(|x| counter_canon(x)).collect::<Option<Vec<_>>>()?;
            V::Fn(name, Box::new(V::Seq(p)))
        }
        V::Seq(x) => V::Seq(x.into_iter().map(counter_canon).collect::<Option<Vec<_>>>()?),
        V::Group(x) => V::Group(x.into_iter().map(counter_canon).collect::<Option<Vec<_>>>()?),
        V::List(x) => V::List(x.into_iter().map(counter_canon).collect::<Option<Vec<_>>>()?),
        V::Ident(s) if s.eq_ignore_ascii_case("decimal") || s.eq_ignore_ascii_case("disc") || s.eq_ignore_ascii_case("none") => V::Ident(s.to_ascii_lowercase()),
        o => o,
    })
}

/// grid-template-areas: normalized rows; rectangular named areas.
fn areas_canon(v: V) -> Option<V> {
    let rows: Vec<String> = v.flat().into_iter().filter_map(|x| if let V::Str(s) = x { Some(s.clone()) } else { None }).collect();
    if rows.is_empty() {
        return Some(v);
    }
    let mut grid: Vec<Vec<String>> = Vec::new();
    for r in &rows {
        let mut cells = Vec::new();
        let mut cur = String::new();
        let mut dots = false;
        for c in r.chars() {
            if c.is_whitespace() {
                if !cur.is_empty() {
                    cells.push(std::mem::take(&mut cur));
                }
                dots = false;
            } else if c == '.' {
                if !dots && !cur.is_empty() {
                    cells.push(std::mem::take(&mut cur));
                }
                if !dots {
                    cur.push('.');
                }
                dots = true;
            } else {
                if dots {
                    cells.push(std::mem::take(&mut cur));
                    dots = false;
                }
                cur.push(c);
            }
        }
        if !cur.is_empty() {
            cells.push(cur);
        }
        if cells.is_empty() {
            return None;
        }
        grid.push(cells);
    }
    let w = grid[0].len();
    if grid.iter().any(|r| r.len() != w) {
        return None;
    }
    // Each name must form a filled rectangle.
    let mut names: Vec<&String> = grid.iter().flatten().filter(|c| *c != ".").collect();
    names.sort();
    names.dedup();
    for n in names {
        let cells: Vec<(usize, usize)> = grid.iter().enumerate().flat_map(|(i, r)| r.iter().enumerate().filter(|(_, c)| *c == n).map(move |(j, _)| (i, j))).collect();
        let (r0, r1) = (cells.iter().map(|c| c.0).min()?, cells.iter().map(|c| c.0).max()?);
        let (c0, c1) = (cells.iter().map(|c| c.1).min()?, cells.iter().map(|c| c.1).max()?);
        if (r1 - r0 + 1) * (c1 - c0 + 1) != cells.len() {
            return None;
        }
    }
    Some(V::Seq(grid.into_iter().map(|r| V::Str(r.join(" "))).collect()))
}

fn scale_computed(v: V) -> V {
    if v.is_kw("none") {
        return v;
    }
    let mut n: Vec<f64> = v.flat().into_iter().map(|x| match x {
        V::Dim(p, u) if u == "%" => p / 100.0,
        V::Num(k) => *k,
        _ => 1.0,
    }).collect();
    if n.len() == 3 && n[2] == 1.0 {
        n.pop();
    }
    if n.len() == 2 && n[0] == n[1] {
        n.pop();
    }
    V::Seq(n.into_iter().map(V::Num).collect())
}

fn rotate_computed(v: V) -> V {
    match &v {
        V::Group(g) => {
            let ang = match &g[1] {
                V::Dim(a, _) => *a,
                _ => return v,
            };
            let axis: Vec<f64> = match &g[0] {
                V::Kw(k) if k == "x" => vec![1.0, 0.0, 0.0],
                V::Kw(k) if k == "y" => vec![0.0, 1.0, 0.0],
                V::Kw(k) if k == "z" => vec![0.0, 0.0, 1.0],
                other => other.flat().into_iter().map(|x| if let V::Num(k) = x { *k } else { 0.0 }).collect(),
            };
            if axis.len() != 3 {
                return v;
            }
            let deg = |a: f64| V::Dim(a, "deg".into());
            match (axis[0], axis[1], axis[2]) {
                (x, y, z) if y == 0.0 && z == 0.0 && x != 0.0 => V::Seq(vec![V::Kw("x".into()), deg(ang * x.signum())]),
                (x, y, z) if x == 0.0 && z == 0.0 && y != 0.0 => V::Seq(vec![V::Kw("y".into()), deg(ang * y.signum())]),
                (x, y, z) if x == 0.0 && y == 0.0 && z != 0.0 => deg(ang * z.signum()),
                (x, y, z) => {
                    let len = (x * x + y * y + z * z).sqrt();
                    if len == 0.0 {
                        return deg(ang);
                    }
                    V::Seq(vec![V::Num(x / len), V::Num(y / len), V::Num(z / len), deg(ang)])
                }
            }
        }
        _ => v,
    }
}

fn offset_rotate_computed(v: V) -> V {
    let V::Group(g) = &v else {
        return match &v {
            V::Dim(..) => v.clone(),
            _ => v,
        };
    };
    let ang = match &g[1] {
        V::Dim(a, _) => *a,
        _ => 0.0,
    };
    match g[0].kw() {
        Some("reverse") => V::Seq(vec![V::Kw("auto".into()), V::Dim(ang + 180.0, "deg".into())]),
        Some("auto") => {
            if g[1].is_none() {
                V::Kw("auto".into())
            } else {
                V::Seq(vec![V::Kw("auto".into()), g[1].clone()])
            }
        }
        _ => g[1].clone(),
    }
}

fn filter_computed(v: V, env: &Env) -> V {
    let f = |x: &V| -> V {
        let V::Fn(name, args) = x else { return x.clone() };
        let a = args.flat();
        let num = |d: f64, max: Option<f64>| -> V {
            let mut k = match a.first() {
                Some(V::Num(k)) => *k,
                Some(V::Dim(p, u)) if u == "%" => p / 100.0,
                _ => d,
            };
            if k < 0.0 {
                k = 0.0;
            }
            if let Some(m) = max {
                k = k.min(m);
            }
            V::Num(k)
        };
        let l = name.to_ascii_lowercase();
        let arg = match l.as_str() {
            "brightness" | "contrast" | "saturate" => num(1.0, None),
            "grayscale" | "invert" | "opacity" | "sepia" => num(1.0, Some(1.0)),
            "blur" => a.first().map(|x| (*x).clone()).map(|x| match x {
                V::Dim(k, u) if k < 0.0 => V::Dim(0.0, u),
                o => o,
            }).unwrap_or(V::Dim(0.0, "px".into())),
            "hue-rotate" => a.first().map(|x| (*x).clone()).unwrap_or(V::Dim(0.0, "deg".into())),
            "drop-shadow" => {
                let mut parts: Vec<V> = Vec::new();
                let col = a.iter().find(|x| matches!(x, V::Rgba(_) | V::Color(_))).map(|x| (*x).clone()).unwrap_or_else(|| current(env));
                parts.push(col);
                let lens: Vec<V> = a.iter().filter(|x| matches!(x, V::Dim(..) | V::Math(_))).map(|x| (*x).clone()).collect();
                parts.extend(lens.iter().cloned());
                if lens.len() == 2 {
                    parts.push(px(0.0));
                }
                return V::Fn(name.clone(), Box::new(V::Seq(parts)));
            }
            _ => return x.clone(),
        };
        V::Fn(name.clone(), Box::new(arg))
    };
    match &v {
        V::Seq(items) => V::Seq(items.iter().map(f).collect()),
        V::Fn(..) => f(&v),
        _ => v,
    }
}

fn axis_pos_computed(v: &V) -> V {
    let p = v.flat();
    let kw = p.iter().find_map(|x| x.kw()).map(|s| s.to_string());
    let off = p.iter().find(|x| !matches!(x, V::Kw(_))).map(|x| (*x).clone());
    let far = matches!(kw.as_deref(), Some("right") | Some("bottom") | Some("x-end") | Some("y-end"));
    match (kw.as_deref(), off) {
        (Some("center"), _) => V::Dim(50.0, "%".into()),
        (Some(_), None) => V::Dim(if far { 100.0 } else { 0.0 }, "%".into()),
        (_, Some(o)) if far => super::sub_from_100(&o),
        (_, Some(o)) => o,
        (None, None) => V::Dim(0.0, "%".into()),
    }
}

#[allow(dead_code)]
fn unused(_: &MathV, _: Kind, _: &Node, _: &color::Abs) -> String {
    fmt(0.0)
}
