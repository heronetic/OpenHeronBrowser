// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Hand-written shorthands: expansion and serialization.

use super::shorthand::{initial_of, lh_text, parse_src};
use super::super::tokenizer::CV;
use super::{ser, V};

type Out = Vec<(&'static str, String)>;

fn split_list(s: &str) -> Vec<String> {
    // Split a serialized comma list at top level.
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    let mut q: Option<char> = None;
    for c in s.chars() {
        if let Some(qc) = q {
            cur.push(c);
            if c == qc {
                q = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                q = Some(c);
                cur.push(c)
            }
            '(' => {
                depth += 1;
                cur.push(c)
            }
            ')' => {
                depth -= 1;
                cur.push(c)
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

fn group(v: V) -> Option<Vec<V>> {
    match v {
        V::Group(g) => Some(g),
        _ => None,
    }
}

/// Longhand values from a `||` group (initial for absent parts).
fn combo_out(l: &[&'static str], g: &[V]) -> Option<Out> {
    let mut out = Vec::new();
    for (i, n) in l.iter().enumerate() {
        out.push((*n, if g[i].is_none() { initial_of(n) } else { lh_text(n, &g[i])? }));
    }
    Some(out)
}

/// Combo-style serialization: non-initial values in order; the first value when all are initial.
fn combo_ser(l: &[&'static str], vals: &[String]) -> String {
    let mut parts = Vec::new();
    for (i, n) in l.iter().enumerate() {
        if vals[i] != initial_of(n) {
            parts.push(vals[i].clone());
        }
    }
    if parts.is_empty() {
        parts.push(vals[0].clone());
    }
    parts.join(" ")
}

fn is_kw(v: &V, k: &str) -> bool {
    v.is_kw(k)
}

fn place_single(prop: &str, v: &V) -> Option<String> {
    lh_text(prop, v)
}

pub fn expand(name: &str, l: &[&'static str], v: &[CV]) -> Option<Out> {
    match name {
        "text-wrap" | "text-decoration" | "font-synthesis" if false => None,
        "text-wrap" => {
            let g = group(parse_src("<'text-wrap-mode'> || <'text-wrap-style'>", v)?)?;
            combo_out(l, &g)
        }
        "text-decoration" => {
            let g = group(parse_src("<'text-decoration-line'> || <'text-decoration-thickness'> || <'text-decoration-style'> || <'text-decoration-color'>", v)?)?;
            combo_out(l, &g)
        }
        "list-style" => {
            let g = group(parse_src("<'list-style-position'> || <'list-style-image'> || <'list-style-type'>", v)?)?;
            let mut g = g;
            let none_count = super::grammar::items(v).iter().filter(|c| c.is_ident("none")).count();
            // `none` sets whichever of image/type isn't otherwise given.
            if is_kw(&g[1], "none") && g[2].is_none() {
                g[2] = V::Kw("none".into());
            } else if is_kw(&g[2], "none") && g[1].is_none() && none_count >= 1 {
                g[1] = V::Kw("none".into());
            }
            if none_count > 2 {
                return None;
            }
            combo_out(l, &g)
        }
        "columns" => {
            let val = parse_src("[ <'column-width'> || <'column-count'> ] [ / <'column-height'> ]?", v)?;
            let p = val.parts();
            let g = group(p[0].clone())?;
            let mut out = combo_out(&l[..2], &g)?;
            let h = match p.get(1) {
                Some(V::Seq(q)) if q.len() == 2 => lh_text("column-height", &q[1])?,
                _ => "auto".into(),
            };
            out.push(("column-height", h));
            Some(out)
        }
        "flex" => {
            if super::grammar::items(v).len() == 1 && v.iter().any(|c| c.is_ident("none")) {
                return Some(vec![(l[0], "0".into()), (l[1], "0".into()), (l[2], "auto".into())]);
            }
            let g = group(parse_src("[ <'flex-grow'> <'flex-shrink'>? ] || <'flex-basis'>", v)?)?;
            let (grow, shrink) = match &g[0] {
                V::Seq(p) => (lh_text("flex-grow", &p[0])?, if p[1].is_none() { "1".into() } else { lh_text("flex-shrink", &p[1])? }),
                _ => ("1".into(), "1".into()),
            };
            let basis = if g[1].is_none() { "0%".into() } else { lh_text("flex-basis", &g[1])? };
            Some(vec![(l[0], grow), (l[1], shrink), (l[2], basis)])
        }
        "place-content" | "place-items" | "place-self" => {
            let (a, j) = (l[0], l[1]);
            let val = parse_src(&format!("<'{a}'> <'{j}'>?"), v)?;
            let p = val.parts();
            let av = place_single(a, p[0])?;
            let jv = if p[1].is_none() {
                // One value: used for both, except baseline alignment in justify-content.
                let first = ser(p[0]);
                if name == "place-content" && first.ends_with("baseline") {
                    "start".to_string()
                } else {
                    // Must be valid for the justify property too.
                    let cvs = super::super::tokenizer::parse_cvs(&first);
                    let jv = super::db::parse_value(j, &cvs)?;
                    ser(&jv)
                }
            } else {
                place_single(j, p[1])?
            };
            Some(vec![(a, av), (j, jv)])
        }
        "border-radius" => {
            let val = parse_src("<length-percentage [0,∞]>{1,4} [ / <length-percentage [0,∞]>{1,4} ]?", v)?;
            let p = val.parts();
            let h: Vec<String> = p[0].parts().iter().map(|x| ser(x)).collect();
            let vv: Vec<String> = match p.get(1) {
                Some(V::Seq(s)) if s.len() == 2 && !s[1].is_none() => s[1].parts().iter().map(|x| ser(x)).collect(),
                _ => h.clone(),
            };
            let four = |x: &[String]| -> Vec<String> {
                match x.len() {
                    1 => vec![x[0].clone(); 4],
                    2 => vec![x[0].clone(), x[1].clone(), x[0].clone(), x[1].clone()],
                    3 => vec![x[0].clone(), x[1].clone(), x[2].clone(), x[1].clone()],
                    _ => x.to_vec(),
                }
            };
            let (h4, v4) = (four(&h), four(&vv));
            Some((0..4).map(|i| (l[i], if h4[i] == v4[i] { h4[i].clone() } else { format!("{} {}", h4[i], v4[i]) })).collect())
        }
        "border-block" | "border-inline" => {
            let g = group(parse_src("<line-width> || <line-style> || <color>", v)?)?;
            let side = combo_out(&l[..3], &g)?;
            let mut out = side.clone();
            for (i, (_, val)) in side.into_iter().enumerate() {
                out.push((l[3 + i], val));
            }
            Some(out)
        }
        "border" => {
            let g = group(parse_src("<line-width> || <line-style> || <color>", v)?)?;
            let w = if g[0].is_none() { "medium".to_string() } else { lh_text("border-top-width", &g[0])? };
            let s = if g[1].is_none() { "none".to_string() } else { lh_text("border-top-style", &g[1])? };
            let c = if g[2].is_none() { "currentcolor".to_string() } else { lh_text("border-top-color", &g[2])? };
            let mut out: Out = Vec::new();
            for i in 0..4 {
                out.push((l[i], w.clone()));
            }
            for i in 4..8 {
                out.push((l[i], s.clone()));
            }
            for i in 8..12 {
                out.push((l[i], c.clone()));
            }
            for n in &l[12..] {
                out.push((*n, initial_of(n)));
            }
            Some(out)
        }
        "border-image" | "mask-border" => {
            let g = group(parse_src(
                if name == "border-image" {
                    "<'border-image-source'> || [ <'border-image-slice'> [ / <'border-image-width'> | / <'border-image-width'>? / <'border-image-outset'> ]? ] || <'border-image-repeat'>"
                } else {
                    "<'mask-border-source'> || [ <'mask-border-slice'> [ / <'mask-border-width'> | / <'mask-border-width'>? / <'mask-border-outset'> ]? ] || <'mask-border-repeat'> || <'mask-border-mode'>"
                },
                v,
            )?)?;
            let mut src = initial_of(l[0]);
            let mut slice = initial_of(l[1]);
            let mut width = initial_of(l[2]);
            let mut outset = initial_of(l[3]);
            let mut rep = initial_of(l[4]);
            if !g[0].is_none() {
                src = lh_text(l[0], &g[0])?;
            }
            if let V::Seq(p) = &g[1] {
                slice = lh_text(l[1], &p[0])?;
                // p[1]: None, or Seq([/, width]) or Seq([/, width?, /, outset])
                if let V::Seq(q) = &p[1] {
                    if q.len() == 2 {
                        width = lh_text(l[2], &q[1])?;
                    } else if q.len() == 4 {
                        if !q[1].is_none() {
                            width = lh_text(l[2], &q[1])?;
                        }
                        outset = lh_text(l[3], &q[3])?;
                    }
                }
            }
            if !g[2].is_none() {
                rep = lh_text(l[4], &g[2])?;
            }
            let mut out = vec![(l[0], src), (l[1], slice), (l[2], width), (l[3], outset), (l[4], rep)];
            if l.len() > 5 {
                out.push((l[5], if g.len() > 3 && !g[3].is_none() { lh_text(l[5], &g[3])? } else { initial_of(l[5]) }));
            }
            Some(out)
        }
        "white-space" => {
            let it = super::grammar::items(v);
            if it.len() == 1 {
                if let Some(k) = it[0].ident() {
                    let m = match k.to_ascii_lowercase().as_str() {
                        "normal" => Some(("collapse", "wrap")),
                        "pre" => Some(("preserve", "nowrap")),
                        "nowrap" => Some(("collapse", "nowrap")),
                        "pre-wrap" => Some(("preserve", "wrap")),
                        "break-spaces" => Some(("break-spaces", "wrap")),
                        "pre-line" => Some(("preserve-breaks", "wrap")),
                        _ => None,
                    };
                    if let Some((c, w)) = m {
                        return Some(vec![(l[0], c.into()), (l[1], w.into())]);
                    }
                }
            }
            let g = group(parse_src("<'white-space-collapse'> || <'text-wrap-mode'>", v)?)?;
            combo_out(l, &g)
        }
        "font-synthesis" => {
            let it = super::grammar::items(v);
            if it.len() == 1 && it[0].is_ident("none") {
                return Some(l.iter().map(|n| (*n, "none".to_string())).collect());
            }
            let g = group(parse_src("weight || [ style | oblique-only ] || small-caps || position", v)?)?;
            let style = match &g[1] {
                V::Kw(k) if k == "oblique-only" => "oblique-only",
                V::None => "none",
                _ => "auto",
            };
            Some(vec![
                (l[0], if g[0].is_none() { "none" } else { "auto" }.into()),
                (l[1], style.into()),
                (l[2], if g[2].is_none() { "none" } else { "auto" }.into()),
                (l[3], if g[3].is_none() { "none" } else { "auto" }.into()),
            ])
        }
        "background" => background_expand(l, v),
        "font" => font_expand(l, v),
        "font-variant" => font_variant_expand(l, v),
        "grid-row" | "grid-column" => {
            let val = parse_src("<grid-line> [ / <grid-line> ]?", v)?;
            let p = val.parts();
            let a = lh_text(l[0], p[0])?;
            let b = match p.get(1) {
                Some(V::Seq(q)) => lh_text(l[1], &q[1])?,
                _ => grid_default(&a),
            };
            Some(vec![(l[0], a), (l[1], b)])
        }
        "grid-area" => {
            let val = parse_src("<grid-line> [ / <grid-line> ]{0,3}", v)?;
            let p = val.parts();
            let mut vals: Vec<String> = vec![lh_text(l[0], p[0])?];
            if let Some(V::Seq(rest)) = p.get(1) {
                for x in rest {
                    if let V::Seq(q) = x {
                        vals.push(lh_text(l[0], &q[1])?);
                    }
                }
            }
            let rs = vals[0].clone();
            let cs = vals.get(1).cloned().unwrap_or_else(|| grid_default(&rs));
            let re = vals.get(2).cloned().unwrap_or_else(|| grid_default(&rs));
            let ce = vals.get(3).cloned().unwrap_or_else(|| grid_default(&cs));
            Some(vec![(l[0], rs), (l[1], cs), (l[2], re), (l[3], ce)])
        }
        "mask" => mask_expand(l, v),
        "text-spacing" => {
            let it = super::grammar::items(v);
            if it.len() == 1 && it[0].is_ident("none") {
                return Some(vec![(l[0], "space-all".into()), (l[1], "no-autospace".into())]);
            }
            if it.len() == 1 && it[0].is_ident("auto") {
                return Some(vec![(l[0], "auto".into()), (l[1], "auto".into())]);
            }
            let g = group(parse_src("[ space-first | trim-start | trim-both | space-all | normal | trim-all ] || [ normal | no-autospace | [ [ ideograph-alpha || ideograph-numeric || punctuation ] || [ insert | replace ] ] ]", v)?)?;
            Some(vec![
                (l[0], if g[0].is_none() { "normal".into() } else { ser(&g[0]) }),
                (l[1], if g[1].is_none() { "normal".into() } else { lh_text(l[1], &g[1])? }),
            ])
        }
        "animation-range" => {
            let mut st = Vec::new();
            let mut en = Vec::new();
            for layer in split_cv(v) {
                let val = parse_src("[ normal | <length-percentage> | <timeline-range-name> <length-percentage>? ] [ normal | <length-percentage> | <timeline-range-name> <length-percentage>? ]?", &layer)?;
                let p = val.parts();
                let s0 = lh_text(l[0], &V::List(vec![p[0].clone()]))?;
                let e0 = if p[1].is_none() {
                    // A range name alone covers that whole range.
                    let name = p[0].flat().first().and_then(|x| x.kw()).filter(|k| *k != "normal").map(|k| k.to_string());
                    match name {
                        Some(n) => n,
                        None => "normal".into(),
                    }
                } else {
                    lh_text(l[1], &V::List(vec![p[1].clone()]))?
                };
                st.push(s0);
                en.push(e0);
            }
            Some(vec![(l[0], st.join(", ")), (l[1], en.join(", "))])
        }
        "grid-template" => {
            let (r, c, a) = grid_template_expand(v)?;
            Some(vec![(l[0], r), (l[1], c), (l[2], a)])
        }
        "grid" => {
            if let Some((r, c, a)) = grid_template_expand(v) {
                return Some(vec![(l[0], r), (l[1], c), (l[2], a), (l[3], "auto".into()), (l[4], "auto".into()), (l[5], "row".into())]);
            }
            if let Some(val) = parse_src("<'grid-template-rows'> / [ auto-flow && dense? ] <'grid-auto-columns'>?", v) {
                let p = val.parts();
                let dense = matches!(p[2], V::Group(g) if !g[1].is_none());
                let ac = if p[3].is_none() { "auto".into() } else { lh_text("grid-auto-columns", p[3])? };
                return Some(vec![(l[0], lh_text(l[0], p[0])?), (l[1], "none".into()), (l[2], "none".into()), (l[3], "auto".into()), (l[4], ac), (l[5], if dense { "column dense" } else { "column" }.into())]);
            }
            let val = parse_src("[ auto-flow && dense? ] <'grid-auto-rows'>? / <'grid-template-columns'>", v)?;
            let p = val.parts();
            let dense = matches!(p[0], V::Group(g) if !g[1].is_none());
            let ar = if p[1].is_none() { "auto".into() } else { lh_text("grid-auto-rows", p[1])? };
            Some(vec![(l[0], "none".into()), (l[1], lh_text(l[1], p[3])?), (l[2], "none".into()), (l[3], ar), (l[4], "auto".into()), (l[5], if dense { "dense" } else { "row" }.into())])
        }
        "page-break-before" | "page-break-after" | "page-break-inside" => {
            let it = super::grammar::items(v);
            if it.len() != 1 {
                return None;
            }
            let k = it[0].ident()?.to_ascii_lowercase();
            let inside = name == "page-break-inside";
            let m = match k.as_str() {
                "auto" | "avoid" => k.clone(),
                "always" if !inside => "page".into(),
                "left" | "right" if !inside => k.clone(),
                _ => return None,
            };
            Some(vec![(l[0], m)])
        }
        "offset" => offset_expand(l, v),
        "text-box" => {
            let it = super::grammar::items(v);
            if it.len() == 1 && it[0].is_ident("normal") {
                return Some(vec![(l[0], "none".into()), (l[1], "auto".into())]);
            }
            let g = group(parse_src("<'text-box-trim'> || <'text-box-edge'>", v)?)?;
            Some(vec![
                (l[0], if g[0].is_none() { "trim-both".into() } else { lh_text(l[0], &g[0])? }),
                (l[1], if g[1].is_none() { "auto".into() } else { lh_text(l[1], &g[1])? }),
            ])
        }
        "position-try" => {
            let val = parse_src("<'position-try-order'>? <'position-try-fallbacks'>", v)?;
            let p = val.parts();
            Some(vec![(l[0], if p[0].is_none() { "normal".into() } else { lh_text(l[0], p[0])? }), (l[1], lh_text(l[1], p[1])?)])
        }
        "transition" => layered_expand(name, l, v),
        "animation" => layered_expand(name, l, v),
        _ => None,
    }
}

pub fn serialize(name: &str, l: &[&'static str], vals: &[String]) -> Option<String> {
    match name {
        "text-wrap" | "text-decoration" | "list-style" => {
            if name == "text-wrap" && vals[0] == "wrap" && vals[1] != "auto" {
                return Some(vals[1].clone());
            }
            Some(combo_ser(l, vals))
        }
        "columns" => {
            let mut s = combo_ser(&l[..2], &vals[..2]);
            if vals.len() > 2 && vals[2] != "auto" {
                s.push_str(" / ");
                s.push_str(&vals[2]);
            }
            Some(s)
        }
        "flex" => Some(vals.join(" ")),
        "place-content" | "place-items" | "place-self" => {
            if vals[0] == vals[1] {
                Some(vals[0].clone())
            } else {
                Some(format!("{} {}", vals[0], vals[1]))
            }
        }
        "border-radius" => {
            let mut h = Vec::new();
            let mut v = Vec::new();
            for x in vals {
                let mut it = x.splitn(2, ' ');
                let a = it.next().unwrap_or("").to_string();
                let b = it.next().map(|s| s.to_string()).unwrap_or_else(|| a.clone());
                h.push(a);
                v.push(b);
            }
            let hs = super::hooks::collapse4(&h).join(" ");
            let vs = super::hooks::collapse4(&v).join(" ");
            Some(if hs == vs { hs } else { format!("{hs} / {vs}") })
        }
        "border-block" | "border-inline" => {
            if vals[..3] != vals[3..] {
                return None;
            }
            Some(combo_ser(&l[..3], &vals[..3]))
        }
        "border" => {
            for k in 0..3 {
                let s = &vals[k * 4..k * 4 + 4];
                if s.iter().any(|x| x != &s[0]) {
                    return None;
                }
            }
            for (i, n) in l[12..].iter().enumerate() {
                if vals[12 + i] != initial_of(n) {
                    return None;
                }
            }
            Some(combo_ser(&["border-top-width", "border-top-style", "border-top-color"], &[vals[0].clone(), vals[4].clone(), vals[8].clone()]))
        }
        "border-image" | "mask-border" => {
            let init: Vec<String> = l.iter().map(|n| initial_of(n)).collect();
            let mut parts = Vec::new();
            if vals[0] != init[0] {
                parts.push(vals[0].clone());
            }
            let w = vals[2] != init[2];
            let o = vals[3] != init[3];
            if vals[1] != init[1] || w || o {
                let mut s = vals[1].clone();
                if w || o {
                    s.push_str(" /");
                    if w {
                        s.push(' ');
                        s.push_str(&vals[2]);
                    }
                    if o {
                        s.push_str(" / ");
                        s.push_str(&vals[3]);
                    }
                }
                parts.push(s);
            }
            if vals[4] != init[4] {
                parts.push(vals[4].clone());
            }
            if vals.len() > 5 && vals[5] != init[5] {
                parts.push(vals[5].clone());
            }
            if parts.is_empty() {
                parts.push("none".into());
            }
            Some(parts.join(" "))
        }
        "white-space" => {
            let kw = match (vals[0].as_str(), vals[1].as_str()) {
                ("collapse", "wrap") => Some("normal"),
                ("preserve", "nowrap") => Some("pre"),
                ("collapse", "nowrap") => Some("nowrap"),
                ("preserve", "wrap") => Some("pre-wrap"),
                ("break-spaces", "wrap") => Some("break-spaces"),
                ("preserve-breaks", "wrap") => Some("pre-line"),
                _ => None,
            };
            Some(match kw {
                Some(k) => k.to_string(),
                None => combo_ser(l, vals),
            })
        }
        "font-synthesis" => {
            let mut parts = Vec::new();
            if vals[0] == "auto" {
                parts.push("weight");
            }
            if vals[1] == "auto" {
                parts.push("style");
            } else if vals[1] == "oblique-only" {
                parts.push("oblique-only");
            }
            if vals[2] == "auto" {
                parts.push("small-caps");
            }
            if vals[3] == "auto" {
                parts.push("position");
            }
            Some(if parts.is_empty() { "none".into() } else { parts.join(" ") })
        }
        "background" => background_ser(l, vals),
        "font" => font_ser(l, vals),
        "font-variant" => {
            if vals.iter().all(|v| v == "normal") {
                return Some("normal".into());
            }
            if vals[0] == "none" {
                return if vals[1..].iter().all(|v| v == "normal") { Some("none".into()) } else { None };
            }
            Some(vals.iter().filter(|v| *v != "normal").cloned().collect::<Vec<_>>().join(" "))
        }
        "grid-row" | "grid-column" => Some(if vals[1] == grid_default(&vals[0]) { vals[0].clone() } else { format!("{} / {}", vals[0], vals[1]) }),
        "grid-area" => {
            let mut parts = vals.to_vec();
            if parts[3] == grid_default(&parts[1]) {
                parts.pop();
                if parts[2] == grid_default(&parts[0]) {
                    parts.pop();
                    if parts[1] == grid_default(&parts[0]) {
                        parts.pop();
                    }
                }
            }
            Some(parts.join(" / "))
        }
        "mask" => mask_ser(l, vals),
        "text-spacing" => Some(match (vals[0].as_str(), vals[1].as_str()) {
            ("space-all", "no-autospace") => "none".into(),
            ("auto", "auto") => "auto".into(),
            ("normal", "normal") => "normal".into(),
            ("normal", b) => b.to_string(),
            (a, "normal") => a.to_string(),
            (a, b) => format!("{a} {b}"),
        }),
        "animation-range" => {
            let (a, b) = (split_list(&vals[0]), split_list(&vals[1]));
            if a.len() != b.len() {
                return None;
            }
            let mut out = Vec::new();
            for (s0, e0) in a.iter().zip(b.iter()) {
                let name = s0.split(' ').next().unwrap_or("");
                let is_name = matches!(name, "cover" | "contain" | "entry" | "exit" | "entry-crossing" | "exit-crossing" | "scroll");
                let dflt = if is_name { name.to_string() } else { "normal".to_string() };
                if *e0 == dflt || (!is_name && e0 == "100%") {
                    out.push(s0.clone());
                } else {
                    out.push(format!("{s0} {e0}"));
                }
            }
            Some(out.join(", "))
        }
        "grid-template" => grid_template_ser(&vals[0], &vals[1], &vals[2]),
        "grid" => {
            let (r, c, a, ar, ac, fl) = (&vals[0], &vals[1], &vals[2], &vals[3], &vals[4], &vals[5]);
            if ar == "auto" && ac == "auto" && fl == "row" {
                return grid_template_ser(r, c, a);
            }
            if fl.starts_with("column") {
                if c != "none" || a != "none" || ar != "auto" {
                    return None;
                }
                let mut s = format!("{r} / auto-flow");
                if fl.ends_with("dense") {
                    s.push_str(" dense");
                }
                if ac != "auto" {
                    s.push(' ');
                    s.push_str(ac);
                }
                return Some(s);
            }
            if r != "none" || a != "none" || ac != "auto" {
                return None;
            }
            let mut s = "auto-flow".to_string();
            if fl == "dense" {
                s.push_str(" dense");
            }
            if ar != "auto" {
                s.push(' ');
                s.push_str(ar);
            }
            Some(format!("{s} / {c}"))
        }
        "page-break-before" | "page-break-after" | "page-break-inside" => match vals[0].as_str() {
            "page" if name != "page-break-inside" => Some("always".into()),
            "auto" | "avoid" => Some(vals[0].clone()),
            "left" | "right" if name != "page-break-inside" => Some(vals[0].clone()),
            _ => None,
        },
        "offset" => offset_ser(vals),
        "text-box" => Some(match (vals[0].as_str(), if vals[1] == "text alphabetic" { "alphabetic" } else { vals[1].as_str() }) {
            ("none", "auto") => "normal".into(),
            ("trim-both", "auto") => "trim-both".into(),
            ("trim-both", e) => e.to_string(),
            (t, "auto") => t.to_string(),
            (t, e) => format!("{t} {e}"),
        }),
        "position-try" => Some(if vals[0] == "normal" { vals[1].clone() } else { format!("{} {}", vals[0], vals[1]) }),
        "transition" | "animation" => layered_ser(name, l, vals),
        _ => None,
    }
}

// ------------------------------------------------------------------ background

const BG_LAYER: &str = "<bg-image> || [ <bg-position> [ / <bg-size> ]? ] || <repeat-style> || <attachment> || <visual-box> || <bg-clip>";

fn background_expand(l: &[&'static str], v: &[CV]) -> Option<Out> {
    let layers: Vec<Vec<CV>> = split_cv(v);
    let n = layers.len();
    let mut cols: Vec<Vec<String>> = vec![Vec::new(); l.len()];
    let mut color = initial_of("background-color");
    for (i, layer) in layers.iter().enumerate() {
        let last = i == n - 1;
        let src = if last { format!("{BG_LAYER} || <color>") } else { BG_LAYER.to_string() };
        let g = group(parse_src(&src, layer)?)?;
        let img = if g[0].is_none() { "none".into() } else { lh_text("background-image", &g[0])? };
        let (pos, size) = match &g[1] {
            V::Seq(p) => {
                let pos = lh_text("background-position", &p[0])?;
                let size = match &p[1] {
                    V::Seq(q) => lh_text("background-size", &V::List(vec![q[1].clone()]))?,
                    _ => "auto".into(),
                };
                (pos, size)
            }
            _ => ("0% 0%".into(), "auto".into()),
        };
        let rep = if g[2].is_none() { "repeat".into() } else { lh_text("background-repeat", &V::List(vec![g[2].clone()]))? };
        let att = if g[3].is_none() { "scroll".into() } else { ser(&g[3]) };
        // One box: origin and clip; two: origin then clip.
        let (origin, clip) = match (&g[4], &g[5]) {
            (V::None, V::None) => ("padding-box".to_string(), "border-box".to_string()),
            (o, V::None) => (ser(o), ser(o)),
            (V::None, c) => {
                let cs = ser(c);
                let o = if cs.contains("border-area") || cs == "text" { "border-box".to_string() } else { cs.clone() };
                (if cs == "text" { "padding-box".to_string() } else { o }, cs)
            }
            (o, c) => (ser(o), ser(c)),
        };
        if last && g.len() > 6 && !g[6].is_none() {
            color = lh_text("background-color", &g[6])?;
        }
        for (k, val) in [img, pos, size, rep, att, origin, clip].into_iter().enumerate() {
            cols[k].push(val);
        }
    }
    let mut out = Vec::new();
    for (k, n) in l.iter().enumerate() {
        if *n == "background-color" {
            out.push((*n, color.clone()));
        } else {
            out.push((*n, cols[k].join(", ")));
        }
    }
    Some(out)
}

fn background_ser(l: &[&'static str], vals: &[String]) -> Option<String> {
    let lists: Vec<Vec<String>> = vals[..7].iter().map(|s| split_list(s)).collect();
    let n = lists[0].len();
    if lists.iter().any(|x| x.len() != n) {
        return None;
    }
    let _ = l;
    let mut layers = Vec::new();
    for i in 0..n {
        let mut parts = Vec::new();
        if i == n - 1 && vals[7] != "transparent" {
            parts.push(vals[7].clone());
        }
        let (img, pos, size, rep, att, origin, clip) = (&lists[0][i], &lists[1][i], &lists[2][i], &lists[3][i], &lists[4][i], &lists[5][i], &lists[6][i]);
        if img != "none" {
            parts.push(img.clone());
        }
        if size != "auto" {
            parts.push(format!("{pos} / {size}"));
        } else if pos != "0% 0%" {
            parts.push(pos.clone());
        }
        if rep != "repeat" {
            parts.push(rep.clone());
        }
        if att != "scroll" {
            parts.push(att.clone());
        }
        if !(origin == "padding-box" && clip == "border-box") {
            if origin == clip {
                parts.push(origin.clone());
            } else {
                parts.push(origin.clone());
                parts.push(clip.clone());
            }
        }
        if parts.is_empty() {
            parts.push(if i == n - 1 { "none".into() } else { "none".into() });
        }
        layers.push(parts.join(" "));
    }
    Some(layers.join(", "))
}

fn split_cv(v: &[CV]) -> Vec<Vec<CV>> {
    let mut out = vec![Vec::new()];
    for c in v {
        if c.is_comma() {
            out.push(Vec::new());
        } else {
            out.last_mut().unwrap().push(c.clone());
        }
    }
    out
}

// ------------------------------------------------------------------ transition / animation

fn layer_src(name: &str) -> &'static str {
    match name {
        "transition" => "<'transition-property'> || <time [0,∞]> || <easing> || <time> || [ normal | allow-discrete ]",
        _ => "<time [0,∞]> || <easing> || <time> || <single-animation-iteration-count> || <single-animation-direction> || <single-animation-fill-mode> || <single-animation-play-state> || [ none | <keyframes-name> ]",
    }
}

fn layered_expand(name: &str, l: &[&'static str], v: &[CV]) -> Option<Out> {
    let layers = split_cv(v);
    let mut cols: Vec<Vec<String>> = vec![Vec::new(); l.len()];
    for layer in &layers {
        if layer.iter().all(|c| c.is_ws()) {
            return None;
        }
        let g = group(parse_src(layer_src(name), layer)?)?;
        for (k, n) in l.iter().enumerate() {
            let one = |x: &V| -> Option<String> {
                let s = lh_text(n, &V::List(vec![x.clone()]))?;
                Some(s)
            };
            let val = if g[k].is_none() { initial_of(n) } else { one(&g[k])? };
            cols[k].push(val);
        }
        if name == "transition" {
            // `none` only as the sole transition.
            if cols[0].last().map(|s| s == "none").unwrap_or(false) && layers.len() > 1 {
                return None;
            }
        }
    }
    let mut out: Out = l.iter().enumerate().map(|(k, n)| (*n, cols[k].join(", "))).collect();
    if name == "animation" {
        out.push(("animation-timeline", "auto".into()));
        out.push(("animation-range-start", "normal".into()));
        out.push(("animation-range-end", "normal".into()));
    }
    Some(out)
}

fn layered_ser(name: &str, l: &[&'static str], vals: &[String]) -> Option<String> {
    let lists: Vec<Vec<String>> = vals.iter().map(|s| split_list(s)).collect();
    let n = lists[0].len();
    if lists.iter().any(|x| x.len() != n) {
        return None;
    }
    let mut layers = Vec::new();
    for i in 0..n {
        let get = |k: usize| lists[k][i].clone();
        let mut parts: Vec<String> = Vec::new();
        if name == "transition" {
            let (prop, dur, tf, delay, beh) = (get(0), get(1), get(2), get(3), get(4));
            if prop != "all" {
                parts.push(prop);
            }
            if dur != "0s" || delay != "0s" {
                parts.push(dur);
            }
            if tf != "ease" {
                parts.push(tf);
            }
            if delay != "0s" {
                parts.push(delay);
            }
            if beh != "normal" {
                parts.push(beh);
            }
            if parts.is_empty() {
                parts.push("all".into());
            }
        } else {
            let inits = ["auto", "ease", "0s", "1", "normal", "none", "running", "none"];
            for (k, init) in inits.iter().enumerate() {
                let v = get(k);
                let need_dur = k == 0 && get(2) != "0s";
                if v != *init || need_dur {
                    parts.push(v);
                }
            }
            if parts.is_empty() {
                parts.push("none".into());
            }
        }
        layers.push(parts.join(" "));
    }
    let _ = l;
    Some(layers.join(", "))
}

// ------------------------------------------------------------------ fonts

fn font_expand(l: &[&'static str], v: &[CV]) -> Option<Out> {
    let it = super::grammar::items(v);
    // System fonts.
    if it.len() == 1 {
        if let Some(k) = it[0].ident() {
            if matches!(k.to_ascii_lowercase().as_str(), "caption" | "icon" | "menu" | "message-box" | "small-caption" | "status-bar") {
                let mut out: Out = l.iter().map(|n| (*n, initial_of(n))).collect();
                // As the style system: the UI font on macOS, Arial elsewhere.
                let mac = cfg!(target_os = "macos");
                for e in out.iter_mut() {
                    match e.0 {
                        "font-size" => e.1 = if mac { "13px" } else { "16px" }.into(),
                        "font-family" => e.1 = if mac { "system-ui" } else { "Arial" }.into(),
                        _ => {}
                    }
                }
                return Some(out);
            }
        }
    }
    let val = parse_src("[ <'font-style'> || [ normal | small-caps ] || <'font-weight'> || [ normal | ultra-condensed | extra-condensed | condensed | semi-condensed | semi-expanded | expanded | extra-expanded | ultra-expanded ] ]? <'font-size'> [ / <'line-height'> ]? <'font-family'>", v)?;
    let p = val.parts();
    let g = match p[0] {
        V::Group(g) => g.clone(),
        _ => vec![V::None; 4],
    };
    let mut out: Out = l.iter().map(|n| (*n, initial_of(n))).collect();
    let mut set = |n: &str, val: String| {
        if let Some(e) = out.iter_mut().find(|e| e.0 == n) {
            e.1 = val;
        }
    };
    if !g[0].is_none() {
        set("font-style", lh_text("font-style", &g[0])?);
    }
    if !g[1].is_none() {
        set("font-variant-caps", ser(&g[1]));
    }
    if !g[2].is_none() {
        set("font-weight", lh_text("font-weight", &g[2])?);
    }
    if !g[3].is_none() {
        set("font-stretch", ser(&g[3]));
    }
    set("font-size", lh_text("font-size", p[1])?);
    if let V::Seq(q) = p[2] {
        set("line-height", lh_text("line-height", &q[1])?);
    }
    set("font-family", lh_text("font-family", p[3])?);
    Some(out)
}

fn font_ser(l: &[&'static str], vals: &[String]) -> Option<String> {
    let get = |n: &str| l.iter().position(|x| *x == n).map(|i| vals[i].clone()).unwrap_or_default();
    for n in ["font-variant-ligatures", "font-variant-numeric", "font-variant-east-asian", "font-variant-alternates", "font-variant-position", "font-variant-emoji", "font-optical-sizing", "font-size-adjust", "font-kerning", "font-feature-settings", "font-variation-settings", "font-language-override"] {
        if get(n) != initial_of(n) {
            return None;
        }
    }
    let caps = get("font-variant-caps");
    if caps != "normal" && caps != "small-caps" {
        return None;
    }
    let mut stretch = get("font-stretch");
    if stretch.ends_with('%') {
        // Computed widths: only the keyword ones fit in the shorthand.
        stretch = match stretch.as_str() {
            "50%" => "ultra-condensed",
            "62.5%" => "extra-condensed",
            "75%" => "condensed",
            "87.5%" => "semi-condensed",
            "100%" => "normal",
            "112.5%" => "semi-expanded",
            "125%" => "expanded",
            "150%" => "extra-expanded",
            "200%" => "ultra-expanded",
            _ => return None,
        }
        .to_string();
    }
    let mut parts = Vec::new();
    let style = get("font-style");
    if style != "normal" {
        parts.push(style);
    }
    if caps != "normal" {
        parts.push(caps);
    }
    let weight = get("font-weight");
    if weight != "normal" && !(weight == "400" && super::shorthand::computed_mode()) {
        parts.push(weight);
    }
    if stretch != "normal" {
        parts.push(stretch);
    }
    let lh = get("line-height");
    if lh != "normal" {
        parts.push(format!("{} / {}", get("font-size"), lh));
    } else {
        parts.push(get("font-size"));
    }
    parts.push(get("font-family"));
    Some(parts.join(" "))
}

fn font_variant_expand(l: &[&'static str], v: &[CV]) -> Option<Out> {
    let it = super::grammar::items(v);
    if it.len() == 1 && (it[0].is_ident("normal") || it[0].is_ident("none")) {
        let lig = if it[0].is_ident("none") { "none" } else { "normal" };
        let mut out: Out = l.iter().map(|n| (*n, "normal".to_string())).collect();
        out[0].1 = lig.into();
        return Some(out);
    }
    let src = "<common-lig-values> || <discretionary-lig-values> || <historical-lig-values> || <contextual-alt-values> || [ small-caps | all-small-caps | petite-caps | all-petite-caps | unicase | titling-caps ] || [ stylistic( <custom-ident> ) || historical-forms || styleset( <custom-ident># ) || character-variant( <custom-ident># ) || swash( <custom-ident> ) || ornaments( <custom-ident> ) || annotation( <custom-ident> ) ] || <numeric-figure-values> || <numeric-spacing-values> || <numeric-fraction-values> || ordinal || slashed-zero || <east-asian-variant-values> || <east-asian-width-values> || ruby || [ sub | super ] || [ text | emoji | unicode ]";
    let g = group(parse_src(src, v)?)?;
    let join = |r: std::ops::Range<usize>| -> String {
        let parts: Vec<String> = g[r].iter().filter(|x| !x.is_none()).map(ser).collect();
        if parts.is_empty() {
            "normal".into()
        } else {
            parts.join(" ")
        }
    };
    let canon = |n: &str, t: String| -> String { super::db::specified_text(n, &t).unwrap_or(t) };
    Some(vec![
        (l[0], canon(l[0], join(0..4))),
        (l[1], join(4..5)),
        (l[2], canon(l[2], join(5..6))),
        (l[3], canon(l[3], join(6..11))),
        (l[4], canon(l[4], join(11..14))),
        (l[5], join(14..15)),
        (l[6], join(15..16)),
    ])
}

fn grid_default(v: &str) -> String {
    // A lone custom-ident repeats; anything else defaults to auto.
    if !v.is_empty() && !v.contains(' ') && v != "auto" && !v.starts_with(|c: char| c.is_ascii_digit() || c == '-' && v[1..].starts_with(|d: char| d.is_ascii_digit())) && v != "span" {
        v.to_string()
    } else {
        "auto".into()
    }
}

// ------------------------------------------------------------------ mask

const MASK_LAYER: &str = "<mask-reference> || [ <position> [ / <bg-size> ]? ] || <repeat-style> || <coord-box> || [ <coord-box> | no-clip ] || <compositing-operator> || <masking-mode>";

fn mask_expand(l: &[&'static str], v: &[CV]) -> Option<Out> {
    let layers = split_cv(v);
    let mut cols: Vec<Vec<String>> = vec![Vec::new(); l.len()];
    for layer in &layers {
        let g = group(parse_src(MASK_LAYER, layer)?)?;
        let img = if g[0].is_none() { "none".into() } else { lh_text("mask-image", &V::List(vec![g[0].clone()]))? };
        let (pos, size) = match &g[1] {
            V::Seq(p) => {
                let pos = lh_text("mask-position", &V::List(vec![p[0].clone()]))?;
                let size = match &p[1] {
                    V::Seq(q) => lh_text("mask-size", &V::List(vec![q[1].clone()]))?,
                    _ => "auto".into(),
                };
                (pos, size)
            }
            _ => ("0% 0%".into(), "auto".into()),
        };
        let rep = if g[2].is_none() { "repeat".into() } else { lh_text("mask-repeat", &V::List(vec![g[2].clone()]))? };
        let (origin, clip) = match (&g[3], &g[4]) {
            (V::None, V::None) => ("border-box".to_string(), "border-box".to_string()),
            (o, V::None) => (ser(o), ser(o)),
            (V::None, c) => ("border-box".to_string(), ser(c)),
            (o, c) => (ser(o), ser(c)),
        };
        let comp = if g[5].is_none() { "add".into() } else { ser(&g[5]) };
        let mode = if g[6].is_none() { "match-source".into() } else { ser(&g[6]) };
        for (k, val) in [img, pos, size, rep, origin, clip, comp, mode].into_iter().enumerate() {
            cols[k].push(val);
        }
    }
    Some(l.iter().enumerate().map(|(k, n)| (*n, cols[k].join(", "))).collect())
}

fn mask_ser(_l: &[&'static str], vals: &[String]) -> Option<String> {
    let lists: Vec<Vec<String>> = vals.iter().map(|s| split_list(s)).collect();
    let n = lists[0].len();
    if lists.iter().any(|x| x.len() != n) {
        return None;
    }
    let mut layers = Vec::new();
    for i in 0..n {
        let g = |k: usize| lists[k][i].clone();
        let mut parts = Vec::new();
        if g(0) != "none" {
            parts.push(g(0));
        }
        if g(2) != "auto" {
            parts.push(format!("{} / {}", g(1), g(2)));
        } else if g(1) != "0% 0%" {
            parts.push(g(1));
        }
        if g(3) != "repeat" {
            parts.push(g(3));
        }
        let (o, c) = (g(4), g(5));
        if !(o == "border-box" && c == "border-box") {
            if o == c {
                parts.push(o);
            } else {
                parts.push(o);
                parts.push(c);
            }
        }
        if g(6) != "add" {
            parts.push(g(6));
        }
        if g(7) != "match-source" {
            parts.push(g(7));
        }
        if parts.is_empty() {
            parts.push("none".into());
        }
        layers.push(parts.join(" "));
    }
    Some(layers.join(", "))
}

// ------------------------------------------------------------------ offset

fn offset_expand(l: &[&'static str], v: &[CV]) -> Option<Out> {
    let val = parse_src("[ <'offset-position'>? [ <'offset-path'> [ <'offset-distance'> || <'offset-rotate'> ]? ]? ]! [ / <'offset-anchor'> ]?", v)?;
    let p = val.parts();
    let main = p[0].parts();
    let mut out: Out = l.iter().map(|n| (*n, initial_of(n))).collect();
    if !main[0].is_none() {
        out[0].1 = lh_text(l[0], main[0])?;
    }
    if let Some(V::Seq(q)) = main.get(1) {
        out[1].1 = lh_text(l[1], &q[0])?;
        if let V::Group(dr) = &q[1] {
            if !dr[0].is_none() {
                out[2].1 = lh_text(l[2], &dr[0])?;
            }
            if !dr[1].is_none() {
                out[3].1 = lh_text(l[3], &dr[1])?;
            }
        }
    }
    if let Some(V::Seq(q)) = p.get(1) {
        out[4].1 = lh_text(l[4], &q[1])?;
    }
    Some(out)
}

fn offset_ser(vals: &[String]) -> Option<String> {
    let (pos, path, dist, rot, anchor) = (&vals[0], &vals[1], &vals[2], &vals[3], &vals[4]);
    let mut parts = Vec::new();
    if pos != "normal" {
        parts.push(pos.clone());
    }
    let tail = dist != "0px" || rot != "auto";
    if path != "none" || tail || parts.is_empty() {
        parts.push(path.clone());
    }
    if dist != "0px" {
        parts.push(dist.clone());
    }
    if rot != "auto" {
        parts.push(rot.clone());
    }
    let mut s = parts.join(" ");
    if anchor != "auto" {
        s.push_str(" / ");
        s.push_str(anchor);
    }
    Some(s)
}

// ------------------------------------------------------------------ grid-template

fn cv_text(t: &[&CV]) -> String {
    t.iter().map(|c| super::super::tokenizer::serialize(std::slice::from_ref(*c))).collect::<Vec<_>>().join(" ")
}

fn names_of(c: &CV) -> Option<Vec<String>> {
    match c {
        CV::Block('[', inner) => {
            let mut out = Vec::new();
            for x in super::grammar::items(inner) {
                let n = x.ident()?;
                if n.eq_ignore_ascii_case("span") || n.eq_ignore_ascii_case("auto") || super::is_css_wide(n) || n.eq_ignore_ascii_case("default") {
                    return None;
                }
                out.push(super::ser_ident(n));
            }
            Some(out)
        }
        _ => None,
    }
}

/// grid-template: (rows, columns, areas) specified texts.
fn grid_template_expand(v: &[CV]) -> Option<(String, String, String)> {
    use super::super::tokenizer::Tok;
    let it = super::grammar::items(v);
    if it.len() == 1 && it[0].is_ident("none") {
        return Some(("none".into(), "none".into(), "none".into()));
    }
    let slash = it.iter().position(|c| c.is_delim('/'));
    let (a, b) = match slash {
        Some(i) => (&it[..i], Some(&it[i + 1..])),
        None => (&it[..], None),
    };
    let has_str = a.iter().any(|c| matches!(c, CV::T(Tok::Str(_))));
    if !has_str {
        let b = b?;
        if a.is_empty() || b.is_empty() {
            return None;
        }
        let r = super::db::specified_text("grid-template-rows", &cv_text(a))?;
        let c = super::db::specified_text("grid-template-columns", &cv_text(b))?;
        return Some((r, c, "none".into()));
    }
    // [ <line-names>? <string> <track-size>? <line-names>? ]+ [ / <explicit-track-list> ]?
    let mut parts: Vec<String> = Vec::new();
    let mut strings: Vec<String> = Vec::new();
    let mut pending: Vec<String> = Vec::new();
    let mut i = 0;
    let mut groups = 0;
    while i < a.len() {
        // One group before a row (plus the previous row's trailing group).
        if let Some(n) = a.get(i).and_then(|c| names_of(c)) {
            pending.extend(n);
            i += 1;
            groups += 1;
        }
        if groups > 2 || (groups == 2 && strings.is_empty()) {
            return None;
        }
        groups = 0;
        let CV::T(Tok::Str(sv)) = a.get(i)? else { return None };
        if !pending.is_empty() {
            parts.push(format!("[{}]", pending.join(" ")));
            pending.clear();
        }
        strings.push(sv.clone());
        i += 1;
        let mut size = "auto".to_string();
        if let Some(c) = a.get(i) {
            if !matches!(c, CV::Block('[', _) | CV::T(Tok::Str(_))) {
                let one = [(*c).clone()];
                let val = parse_src("<track-size>", &one)?;
                size = lh_text("grid-auto-rows", &V::Seq(vec![val]))?;
                i += 1;
            }
        }
        parts.push(size);
        if let Some(n) = a.get(i).and_then(|c| names_of(c)) {
            pending.extend(n);
            i += 1;
            groups = 1;
        }
        if i == a.len() {
            break;
        }
    }
    if i < a.len() {
        return None;
    }
    if !pending.is_empty() {
        parts.push(format!("[{}]", pending.join(" ")));
    }
    let areas_text = strings.iter().map(|s| super::ser_string(s)).collect::<Vec<_>>().join(" ");
    let areas = super::db::specified_text("grid-template-areas", &areas_text)?;
    let cols = match b {
        Some(b) => {
            if b.is_empty() {
                return None;
            }
            let t = cv_text(b);
            if t.contains("auto-fill") || t.contains("auto-fit") || (b.len() == 1 && b[0].is_ident("none")) {
                return None;
            }
            super::db::specified_text("grid-template-columns", &t)?
        }
        None => "none".into(),
    };
    Some((parts.join(" "), cols, areas))
}

fn grid_template_ser(rows: &str, cols: &str, areas: &str) -> Option<String> {
    if areas == "none" {
        if rows == "none" && cols == "none" {
            return Some("none".into());
        }
        return Some(format!("{rows} / {cols}"));
    }
    let strs: Vec<String> = {
        let cvs = super::super::tokenizer::parse_cvs(areas);
        super::grammar::items(&cvs).iter().filter_map(|c| if let CV::T(super::super::tokenizer::Tok::Str(s)) = c { Some(super::ser_string(s)) } else { None }).collect()
    };
    if rows.contains("repeat(") {
        return None;
    }
    let cvs = super::super::tokenizer::parse_cvs(rows);
    let toks = super::grammar::items(&cvs);
    let mut out: Vec<String> = Vec::new();
    let mut k = 0;
    for t in &toks {
        match t {
            CV::Block('[', _) => out.push(super::super::tokenizer::serialize(std::slice::from_ref(*t))),
            _ => {
                let s = strs.get(k)?;
                out.push(s.clone());
                let sz = super::super::tokenizer::serialize(std::slice::from_ref(*t));
                if sz != "auto" {
                    out.push(sz);
                }
                k += 1;
            }
        }
    }
    if k != strs.len() {
        return None;
    }
    let mut s = out.join(" ");
    if cols != "none" {
        s.push_str(" / ");
        s.push_str(cols);
    }
    Some(s)
}
