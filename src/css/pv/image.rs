// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! `<image>`: url(), gradients (linear/radial/conic, repeating and
//! -webkit- prefixed), image(), image-set(), cross-fade(), light-dark().

use super::super::color;
use super::super::tokenizer::{Tok, CV};
use super::types::{self, numeric};
use super::{compute, compute_pos, ser, ser_pos, Env, Kind, Pos, V};

#[derive(Clone, Debug, PartialEq)]
pub enum Image {
    Url(String),
    Grad(Box<Grad>),
    /// image(<color>)
    Color(V),
    /// image-set() / -webkit-image-set(): (image, resolution, type)
    Set(String, Vec<(Image, Option<V>, Option<String>)>),
    /// cross-fade(): (image, percentage)
    Cross(Vec<(Image, Option<V>)>),
    LightDark(Box<Image>, Box<Image>),
    None,
    Raw(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GK {
    Linear,
    Radial,
    Conic,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Grad {
    /// Function name, lowercase (e.g. "repeating-linear-gradient").
    pub name: String,
    pub kind: GK,
    pub legacy: bool,
    /// linear: angle; conic: `from` angle.
    pub angle: Option<V>,
    /// linear `to` sides (horizontal, vertical).
    pub to: Option<(Option<String>, Option<String>)>,
    pub shape: Option<String>,
    pub size: Vec<V>,
    pub pos: Option<Pos>,
    pub interp: Option<(String, Option<String>)>,
    pub stops: Vec<Stop>,
    /// A computed value (defaults omitted).
    pub computed: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stop {
    /// None for a color hint.
    pub color: Option<V>,
    pub pos: Vec<V>,
}

fn items(v: &[CV]) -> Vec<&CV> {
    v.iter().filter(|c| !c.is_ws()).collect()
}

fn split(args: &[CV]) -> Vec<Vec<&CV>> {
    let mut out = vec![Vec::new()];
    for c in args {
        if c.is_comma() {
            out.push(Vec::new());
        } else if !c.is_ws() {
            out.last_mut().unwrap().push(c);
        }
    }
    out
}

fn kw(c: &CV) -> Option<String> {
    c.ident().map(|s| s.to_ascii_lowercase())
}

/// `in <space> [<hue-method> hue]?` at the start of `t`; returns the method and tokens used.
fn interp(t: &[&CV]) -> Option<((String, Option<String>), usize)> {
    if t.is_empty() || !t[0].is_ident("in") {
        return None;
    }
    let sp = kw(t.get(1)?)?;
    let space = color::Space::mix_space(&sp)?;
    let name = space.name().to_string();
    if t.len() >= 4 && t[3].is_ident("hue") {
        let h = kw(t[2])?;
        if !space.is_polar() || !matches!(h.as_str(), "shorter" | "longer" | "increasing" | "decreasing") {
            return None;
        }
        return Some(((name, Some(h)), 4));
    }
    Some(((name, None), 2))
}

/// Split off an interpolation method at the start or end of a prelude.
fn take_interp<'a>(t: &'a [&'a CV]) -> Option<(Option<(String, Option<String>)>, Vec<&'a CV>)> {
    match t.iter().position(|c| c.is_ident("in")) {
        None => Some((None, t.to_vec())),
        Some(0) => {
            let (m, n) = interp(t)?;
            Some((Some(m), t[n..].to_vec()))
        }
        Some(k) => {
            let (m, n) = interp(&t[k..])?;
            if k + n != t.len() {
                return None;
            }
            Some((Some(m), t[..k].to_vec()))
        }
    }
}

fn whole_pos(t: &[&CV]) -> Option<Pos> {
    types::position(t, 0, false).into_iter().find(|(e, _)| *e == t.len()).and_then(|(_, v)| if let V::Pos(p) = v { Some(*p) } else { None })
}

fn stop_list(parts: &[Vec<&CV>], conic: bool) -> Option<Vec<Stop>> {
    let kind = if conic { Kind::AnglePct } else { Kind::LenPct };
    let mut stops = Vec::new();
    for p in parts {
        if p.is_empty() {
            return None;
        }
        let pos_of = |c: &CV| numeric(c, kind, None, conic);
        if let Some(col) = color::parse(p[0]).filter(|c| !matches!(c, color::Spec::Raw(_))) {
            if p.len() > 3 {
                return None;
            }
            let mut pos = Vec::new();
            for c in &p[1..] {
                pos.push(pos_of(c)?);
            }
            stops.push(Stop { color: Some(V::Color(Box::new(col))), pos });
        } else if p.len() == 1 {
            stops.push(Stop { color: None, pos: vec![pos_of(p[0])?] });
        } else {
            return None;
        }
    }
    // At least two parts; first and last are color stops; no adjacent hints.
    if stops.len() < 2 && !(stops.len() == 1 && stops[0].color.is_some()) {
        return None;
    }
    if stops.first()?.color.is_none() || stops.last()?.color.is_none() {
        return None;
    }
    for w in stops.windows(2) {
        if w[0].color.is_none() && w[1].color.is_none() {
            return None;
        }
    }
    Some(stops)
}

fn parse_grad(name: &str, args: &[CV]) -> Option<Grad> {
    let lname = name.to_ascii_lowercase();
    let legacy = lname.starts_with("-webkit-");
    let base = lname.trim_start_matches("-webkit-").trim_start_matches("repeating-");
    let kind = match base {
        "linear-gradient" => GK::Linear,
        "radial-gradient" => GK::Radial,
        "conic-gradient" if !legacy => GK::Conic,
        _ => return None,
    };
    let parts = split(args);
    let mut g = Grad { name: lname.clone(), kind, legacy, angle: None, to: None, shape: None, size: vec![], pos: None, interp: None, stops: vec![], computed: false };
    // Is the first part a prelude (not a color stop)?
    let first = &parts[0];
    if first.is_empty() {
        return None;
    }
    let is_prelude = color::parse(first[0]).is_none() && !(first.len() == 1 && numeric(first[0], if kind == GK::Conic { Kind::AnglePct } else { Kind::LenPct }, None, false).is_some() && kind != GK::Linear);
    let stop_parts: &[Vec<&CV>] = if is_prelude { &parts[1..] } else { &parts[..] };
    if is_prelude {
        if legacy {
            match kind {
                GK::Linear => {
                    if first.len() == 1 && numeric(first[0], Kind::Angle, None, true).is_some() {
                        g.angle = numeric(first[0], Kind::Angle, None, true);
                    } else {
                        let mut h = None;
                        let mut v = None;
                        for c in first {
                            match kw(c)?.as_str() {
                                k @ ("left" | "right") if h.is_none() => h = Some(k.to_string()),
                                k @ ("top" | "bottom") if v.is_none() => v = Some(k.to_string()),
                                "center" => {}
                                _ => return None,
                            }
                        }
                        g.to = Some((h, v));
                    }
                }
                _ => {
                    g.pos = Some(whole_pos(first)?);
                    // A second prelude part: shape/size.
                    if parts.len() > 2 && color::parse(parts[1][0]).is_none() {
                        return None;
                    }
                }
            }
        } else {
            let (m, rest) = take_interp(first)?;
            g.interp = m;
            match kind {
                GK::Linear => {
                    if rest.is_empty() {
                        if g.interp.is_none() {
                            return None;
                        }
                    } else if rest[0].is_ident("to") {
                        let mut h = None;
                        let mut v = None;
                        if rest.len() < 2 || rest.len() > 3 {
                            return None;
                        }
                        for c in &rest[1..] {
                            match kw(c)?.as_str() {
                                k @ ("left" | "right") if h.is_none() => h = Some(k.to_string()),
                                k @ ("top" | "bottom") if v.is_none() => v = Some(k.to_string()),
                                _ => return None,
                            }
                        }
                        g.to = Some((h, v));
                    } else if rest.len() == 1 {
                        g.angle = Some(numeric(rest[0], Kind::Angle, None, true)?);
                    } else {
                        return None;
                    }
                }
                GK::Radial => {
                    let at = rest.iter().position(|c| c.is_ident("at"));
                    let (ss, pp) = match at {
                        Some(k) => (&rest[..k], Some(&rest[k + 1..])),
                        None => (&rest[..], None),
                    };
                    if let Some(pp) = pp {
                        g.pos = Some(whole_pos(pp)?);
                    }
                    let mut shape = None;
                    let mut size: Vec<V> = Vec::new();
                    let mut extent = false;
                    for c in ss {
                        match kw(c).as_deref() {
                            Some(k @ ("circle" | "ellipse")) if shape.is_none() => shape = Some(k.to_string()),
                            Some(k @ ("closest-corner" | "closest-side" | "farthest-corner" | "farthest-side")) if size.is_empty() => {
                                size.push(V::Kw(k.to_string()));
                                extent = true;
                            }
                            _ if !extent && size.len() < 2 => size.push(numeric(c, Kind::LenPct, Some((0.0, f64::INFINITY)), false)?),
                            _ => return None,
                        }
                    }
                    // Lengths must be contiguous ("circle 10px" / "10px circle", not split).
                    if !extent && !size.is_empty() {
                        let idx: Vec<usize> = ss.iter().enumerate().filter(|(_, c)| kw(c).map(|k| k != "circle" && k != "ellipse").unwrap_or(true)).map(|(i, _)| i).collect();
                        if idx.windows(2).any(|w| w[1] != w[0] + 1) {
                            return None;
                        }
                    }
                    let is_pct = |v: &V| matches!(v, V::Dim(_, u) if u == "%") || matches!(v, V::Math(m) if super::super::calc::has_pct(&m.node));
                    match (shape.as_deref(), size.len(), extent) {
                        (Some("circle"), 2, _) => return None,
                        (Some("circle"), 1, false) if is_pct(&size[0]) => return None,
                        (Some("ellipse"), 1, false) => return None,
                        (None, 1, false) if is_pct(&size[0]) => return None,
                        _ => {}
                    }
                    if ss.is_empty() && pp.is_none() && g.interp.is_none() {
                        return None;
                    }
                    g.shape = shape;
                    g.size = size;
                }
                GK::Conic => {
                    let mut i = 0;
                    if rest.first().map(|c| c.is_ident("from")).unwrap_or(false) {
                        g.angle = Some(numeric(rest.get(1)?, Kind::Angle, None, true)?);
                        i = 2;
                    }
                    if i < rest.len() {
                        if !rest[i].is_ident("at") {
                            return None;
                        }
                        g.pos = Some(whole_pos(&rest[i + 1..])?);
                    } else if i == 0 && g.interp.is_none() {
                        return None;
                    }
                }
            }
        }
    }
    g.stops = stop_list(stop_parts, kind == GK::Conic)?;
    if g.stops.len() < 2 && !legacy {
        // A single color stop is allowed (css-images-4) only without hints.
        if g.stops.len() != 1 {
            return None;
        }
    }
    Some(g)
}

fn image_set_option(p: &[&CV]) -> Option<(Image, Option<V>, Option<String>)> {
    let img = match p.first()? {
        CV::T(Tok::Str(s)) => Image::Url(s.clone()),
        c => {
            let i = parse(c)?;
            if matches!(i, Image::Set(..)) {
                return None;
            }
            i
        }
    };
    let mut res = None;
    let mut ty = None;
    for c in &p[1..] {
        match c {
            CV::Func(n, a) if n.eq_ignore_ascii_case("type") && ty.is_none() => match items(a).as_slice() {
                [CV::T(Tok::Str(s))] => ty = Some(s.clone()),
                _ => return None,
            },
            _ if res.is_none() => res = Some(numeric(c, Kind::Res, Some((0.0, f64::INFINITY)), false)?),
            _ => return None,
        }
    }
    Some((img, res, ty))
}

/// Parse one `<image>` component value.
pub fn parse(cv: &CV) -> Option<Image> {
    if let Some(u) = types::url_of(cv) {
        return Some(Image::Url(u));
    }
    let CV::Func(name, args) = cv else { return None };
    let l = name.to_ascii_lowercase();
    match l.as_str() {
        "image" => {
            let t = items(args);
            if t.len() != 1 {
                return None;
            }
            let c = color::parse(t[0])?;
            Some(Image::Color(V::Color(Box::new(c))))
        }
        "image-set" | "-webkit-image-set" => {
            let mut opts = Vec::new();
            for p in split(args) {
                opts.push(image_set_option(&p)?);
            }
            Some(Image::Set(l, opts))
        }
        "cross-fade" => {
            let mut out = Vec::new();
            for p in split(args) {
                let (mut img, mut pct) = (None, None);
                for c in &p {
                    if pct.is_none() {
                        if let Some(v) = numeric(c, Kind::Pct, Some((0.0, 100.0)), false) {
                            pct = Some(v);
                            continue;
                        }
                    }
                    if img.is_some() {
                        return None;
                    }
                    img = Some(match parse(c) {
                        Some(i) => i,
                        None => Image::Color(V::Color(Box::new(color::parse(c)?))),
                    });
                }
                out.push((img?, pct));
            }
            Some(Image::Cross(out))
        }
        "light-dark" => {
            let p = split(args);
            if p.len() != 2 || p[0].len() != 1 || p[1].len() != 1 {
                return None;
            }
            let one = |c: &CV| if c.is_ident("none") { Some(Image::None) } else { parse(c) };
            Some(Image::LightDark(Box::new(one(p[0][0])?), Box::new(one(p[1][0])?)))
        }
        "paint" | "element" | "-moz-element" | "-webkit-gradient" | "-webkit-cross-fade" => Some(Image::Raw(super::super::tokenizer::serialize(std::slice::from_ref(cv)))),
        _ => parse_grad(name, args).map(|g| Image::Grad(Box::new(g))),
    }
}

// ------------------------------------------------------------------ serialization

fn is_legacy_color(v: &V) -> bool {
    match v {
        V::Color(c) => match &**c {
            color::Spec::Abs(a) => a.legacy,
            color::Spec::Named(..) | color::Spec::Transparent | color::Spec::Current | color::Spec::System(_) => true,
            _ => false,
        },
        V::Rgba(a) => a.legacy || matches!(a.space, color::Space::Hsl | color::Space::Hwb),
        _ => true,
    }
}

fn ser_interp(g: &Grad) -> Option<String> {
    let (sp, hue) = g.interp.as_ref()?;
    let legacy = g.stops.iter().all(|s| s.color.as_ref().map(is_legacy_color).unwrap_or(true));
    let default = if legacy { "srgb" } else { "oklab" };
    let hue = hue.as_deref().filter(|h| *h != "shorter");
    if sp == default && hue.is_none() {
        return None;
    }
    Some(match hue {
        Some(h) => format!("in {sp} {h} hue"),
        None => format!("in {sp}"),
    })
}

fn is_center_pos(p: &Pos) -> bool {
    let c = |e: &super::Edge| matches!(&e.off, Some(V::Dim(v, u)) if *v == 50.0 && u == "%") && e.kw.is_none();
    c(&p.h) && c(&p.v)
}

fn is_zero_angle(v: &V) -> bool {
    matches!(v, V::Dim(n, _) if *n == 0.0)
}

fn ser_grad(g: &Grad, computed: bool) -> String {
    let mut pre: Vec<String> = Vec::new();
    match g.kind {
        GK::Linear => {
            if g.legacy {
                if let Some(a) = &g.angle {
                    pre.push(ser(a));
                } else if let Some((h, v)) = &g.to {
                    let s: Vec<String> = [h.clone(), v.clone()].into_iter().flatten().collect();
                    pre.push(if s.is_empty() { "center".into() } else { s.join(" ") });
                }
            } else if let Some(a) = &g.angle {
                let is180 = matches!(a, V::Dim(n, u) if (*n == 180.0 && u == "deg") || (*n == 0.5 && u == "turn") || (*n == 200.0 && u == "grad"));
                if !is180 {
                    pre.push(ser(a));
                }
            } else if let Some((h, v)) = &g.to {
                if !(h.is_none() && v.as_deref() == Some("bottom")) {
                    let s: Vec<String> = [h.clone(), v.clone()].into_iter().flatten().collect();
                    pre.push(format!("to {}", s.join(" ")));
                }
            }
        }
        GK::Radial => {
            if g.legacy {
                if let Some(p) = &g.pos {
                    pre.push(ser_pos(p));
                }
            } else {
                let extent = g.size.first().and_then(|v| v.kw().map(|s| s.to_string()));
                let mut sh = Vec::new();
                if g.shape.as_deref() == Some("circle") && (extent.is_some() || g.size.is_empty()) {
                    sh.push("circle".to_string());
                }
                match &extent {
                    Some(e) if e != "farthest-corner" => sh.push(e.clone()),
                    Some(_) => {}
                    None => sh.extend(g.size.iter().map(ser)),
                }
                if let Some(p) = &g.pos {
                    if !(computed && is_center_pos(p)) {
                        sh.push(format!("at {}", ser_pos(p)));
                    }
                }
                if !sh.is_empty() {
                    pre.push(sh.join(" "));
                }
            }
        }
        GK::Conic => {
            let mut sh = Vec::new();
            if let Some(a) = &g.angle {
                if !(computed && is_zero_angle(a)) {
                    sh.push(format!("from {}", ser(a)));
                }
            }
            if let Some(p) = &g.pos {
                if !(computed && is_center_pos(p)) {
                    sh.push(format!("at {}", ser_pos(p)));
                }
            }
            if !sh.is_empty() {
                pre.push(sh.join(" "));
            }
        }
    }
    if let Some(i) = ser_interp(g) {
        match pre.last_mut() {
            Some(p) => {
                p.push(' ');
                p.push_str(&i);
            }
            None => pre.push(i),
        }
    }
    for s in &g.stops {
        let mut parts = Vec::new();
        if let Some(c) = &s.color {
            parts.push(ser(c));
        }
        parts.extend(s.pos.iter().map(ser));
        pre.push(parts.join(" "));
    }
    format!("{}({})", g.name, pre.join(", "))
}

fn ser_img(i: &Image, computed: bool) -> String {
    match i {
        Image::Url(u) => super::ser_url(u),
        Image::Grad(g) => ser_grad(g, computed || g.computed),
        Image::Color(c) => format!("image({})", ser(c)),
        Image::Set(n, opts) => format!(
            "{}({})",
            n,
            opts.iter()
                .map(|(img, r, t)| {
                    let mut s = ser_img(img, computed);
                    if let Some(r) = r {
                        s.push(' ');
                        s.push_str(&ser(r));
                    }
                    if let Some(t) = t {
                        s.push_str(&format!(" type({})", super::ser_string(t)));
                    }
                    s
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Image::Cross(v) => format!(
            "cross-fade({})",
            v.iter()
                .map(|(img, p)| match p {
                    Some(p) => format!("{} {}", ser_img(img, computed), ser(p)),
                    None => ser_img(img, computed),
                })
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Image::LightDark(a, b) => format!("light-dark({}, {})", ser_img(a, computed), ser_img(b, computed)),
        Image::None => "none".into(),
        Image::Raw(s) => s.clone(),
    }
}

pub fn ser_image(i: &Image) -> String {
    ser_img(i, false)
}

pub fn ser_computed(i: &Image) -> String {
    ser_img(i, true)
}

// ------------------------------------------------------------------ computed values

pub fn compute_image(i: &Image, env: &Env) -> Image {
    match i {
        Image::Url(u) => Image::Url(super::resolve_url(&env.base, u)),
        Image::Grad(g) => {
            let mut g = (**g).clone();
            g.angle = g.angle.as_ref().map(|a| compute(a, env));
            g.size = g
                .size
                .iter()
                .map(|s| match compute(s, env) {
                    V::Dim(v, u) if v < 0.0 => V::Dim(0.0, u),
                    o => o,
                })
                .collect();
            g.computed = true;
            g.pos = g.pos.as_ref().map(|p| compute_pos(p, env));
            g.stops = g.stops.iter().map(|s| Stop { color: s.color.as_ref().map(|c| compute(c, env)), pos: s.pos.iter().map(|p| compute(p, env)).collect() }).collect();
            Image::Grad(Box::new(g))
        }
        Image::Color(c) => Image::Color(compute(c, env)),
        Image::Set(n, v) => Image::Set(n.clone(), v.iter().map(|(img, r, t)| (compute_img(img, env), r.as_ref().map(|r| compute(r, env)), t.clone())).collect()),
        Image::Cross(v) => Image::Cross(v.iter().map(|(img, p)| (compute_img(img, env), p.clone())).collect()),
        Image::LightDark(a, b) => {
            let pick = if env.dark { b } else { a };
            match &**pick {
                Image::None => Image::Color(V::Rgba(Box::new(color::Abs::srgb(0.0, 0.0, 0.0, 0.0)))),
                other => compute_img(other, env),
            }
        }
        Image::None => Image::None,
        Image::Raw(s) => Image::Raw(s.clone()),
    }
}

fn compute_img(i: &Image, env: &Env) -> Image {
    compute_image(i, env)
}
