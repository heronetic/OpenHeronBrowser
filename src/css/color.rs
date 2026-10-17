// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! CSS Color 4/5 values for the CSSOM: parsing with full precision,
//! specified-value and computed-value serialization (as Chrome), color
//! conversion between the predefined spaces, relative colors, color-mix(),
//! light-dark(), alpha() and contrast-color().

use super::calc::{self, Node};
use super::tokenizer::{Tok, CV};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Space {
    Srgb,
    SrgbLinear,
    DisplayP3,
    DisplayP3Linear,
    A98,
    ProPhoto,
    Rec2020,
    XyzD50,
    XyzD65,
    Lab,
    Lch,
    Oklab,
    Oklch,
    Hsl,
    Hwb,
}

impl Space {
    fn from_color_fn(n: &str) -> Option<Space> {
        Some(match n {
            "srgb" => Space::Srgb,
            "srgb-linear" => Space::SrgbLinear,
            "display-p3" => Space::DisplayP3,
            "display-p3-linear" => Space::DisplayP3Linear,
            "a98-rgb" => Space::A98,
            "prophoto-rgb" => Space::ProPhoto,
            "rec2020" => Space::Rec2020,
            "xyz-d50" => Space::XyzD50,
            "xyz-d65" | "xyz" => Space::XyzD65,
            _ => return None,
        })
    }
    pub fn mix_space(n: &str) -> Option<Space> {
        Some(match n {
            "lab" => Space::Lab,
            "lch" => Space::Lch,
            "oklab" => Space::Oklab,
            "oklch" => Space::Oklch,
            "hsl" => Space::Hsl,
            "hwb" => Space::Hwb,
            _ => return Space::from_color_fn(n),
        })
    }
    pub fn name(self) -> &'static str {
        match self {
            Space::Srgb => "srgb",
            Space::SrgbLinear => "srgb-linear",
            Space::DisplayP3 => "display-p3",
            Space::DisplayP3Linear => "display-p3-linear",
            Space::A98 => "a98-rgb",
            Space::ProPhoto => "prophoto-rgb",
            Space::Rec2020 => "rec2020",
            Space::XyzD50 => "xyz-d50",
            Space::XyzD65 => "xyz-d65",
            Space::Lab => "lab",
            Space::Lch => "lch",
            Space::Oklab => "oklab",
            Space::Oklch => "oklch",
            Space::Hsl => "hsl",
            Space::Hwb => "hwb",
        }
    }
    fn is_rgbish(self) -> bool {
        matches!(self, Space::Srgb | Space::SrgbLinear | Space::DisplayP3 | Space::DisplayP3Linear | Space::A98 | Space::ProPhoto | Space::Rec2020)
    }
    fn is_xyz(self) -> bool {
        matches!(self, Space::XyzD50 | Space::XyzD65)
    }
    pub fn is_polar(self) -> bool {
        matches!(self, Space::Lch | Space::Oklch | Space::Hsl | Space::Hwb)
    }
    /// Index of the hue channel, if any.
    fn hue_index(self) -> Option<usize> {
        match self {
            Space::Lch | Space::Oklch => Some(2),
            Space::Hsl | Space::Hwb => Some(0),
            _ => None,
        }
    }
}

/// An absolute color: channels in the space's own units (rgb-ish 0..1,
/// hsl hue degrees with s/l in percent, lab L 0..100, …); None is `none`.
#[derive(Clone, Debug, PartialEq)]
pub struct Abs {
    pub space: Space,
    pub c: [Option<f64>; 3],
    pub a: Option<f64>,
    /// From a legacy syntax (named, hex, rgb(), hsl(), hwb()): serialized as rgb()/rgba().
    pub legacy: bool,
}

impl Abs {
    pub fn srgb(r: f64, g: f64, b: f64, a: f64) -> Abs {
        Abs { space: Space::Srgb, c: [Some(r), Some(g), Some(b)], a: Some(a), legacy: true }
    }
}

/// A channel (or alpha) as written: kept for specified serialization of
/// relative colors and unresolvable math.
#[derive(Clone, Debug, PartialEq)]
pub enum Comp {
    None,
    Num(f64),
    Pct(f64),
    /// A hue angle in degrees, with the unit it was written in.
    Angle(f64, String),
    Ident(String),
    Calc(Node),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Spec {
    Abs(Abs),
    /// A named color (kept as the keyword in specified values).
    Named(String, Abs),
    Transparent,
    Current,
    System(String),
    /// A color function with unresolved parts (relative color, or math that
    /// cannot be resolved at parse time).
    Func(Box<FuncSpec>),
    Mix(Box<MixSpec>),
    LightDark(Box<Spec>, Box<Spec>),
    Alpha(Box<Spec>, Option<Comp>),
    Contrast(Box<Spec>),
    /// Contains var()/env(): kept as written.
    Raw(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FuncSpec {
    pub name: String,
    pub space: Space,
    pub from: Option<Spec>,
    pub c: [Comp; 3],
    pub alpha: Option<Comp>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MixSpec {
    pub space: Space,
    /// Whether `in <space>` was given (oklab is the default).
    pub explicit: bool,
    pub hue: Option<String>,
    pub items: Vec<(Spec, Option<Comp>)>,
}

// ------------------------------------------------------------------ parsing

fn toks(args: &[CV]) -> Vec<&CV> {
    args.iter().filter(|c| !c.is_ws()).collect()
}

fn has_var(args: &[CV]) -> bool {
    args.iter().any(|c| match c {
        CV::Func(n, a) => {
            let l = n.to_ascii_lowercase();
            l == "var" || l == "env" || l == "attr" || has_var(a)
        }
        CV::Block(_, a) => has_var(a),
        _ => false,
    })
}

/// Parse a `<color>` (one component value). None if invalid.
pub fn parse(cv: &CV) -> Option<Spec> {
    match cv {
        CV::T(Tok::Hash(h, _)) => parse_hex(h).map(Spec::Abs),
        CV::T(Tok::Ident(name)) => {
            let n = name.to_ascii_lowercase();
            match n.as_str() {
                "currentcolor" => Some(Spec::Current),
                "transparent" => Some(Spec::Transparent),
                _ => {
                    if let Some(rgb) = named(&n) {
                        return Some(Spec::Named(n, rgb));
                    }
                    if is_system(&n) {
                        return Some(Spec::System(n));
                    }
                    None
                }
            }
        }
        CV::Func(name, args) => {
            let n = name.to_ascii_lowercase();
            if has_var(args) {
                return Some(Spec::Raw(super::tokenizer::serialize(std::slice::from_ref(cv))));
            }
            match n.as_str() {
                "color-mix" => parse_mix(args),
                "light-dark" => {
                    let parts = split_commas(args);
                    if parts.len() != 2 || parts[0].len() != 1 || parts[1].len() != 1 {
                        return None;
                    }
                    Some(Spec::LightDark(Box::new(parse(parts[0][0])?), Box::new(parse(parts[1][0])?)))
                }
                "alpha" => parse_alpha_fn(args),
                "contrast-color" => {
                    let t = toks(args);
                    if t.len() != 1 {
                        return None;
                    }
                    Some(Spec::Contrast(Box::new(parse(t[0])?)))
                }
                "rgb" | "rgba" | "hsl" | "hsla" | "hwb" | "lab" | "lch" | "oklab" | "oklch" | "color" => parse_fn(&n, args),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Parse a color from text.
pub fn parse_str(s: &str) -> Option<Spec> {
    let cvs = super::tokenizer::parse_cvs(s);
    let t = toks(&cvs);
    if t.len() != 1 {
        return None;
    }
    parse(t[0])
}

fn split_commas(args: &[CV]) -> Vec<Vec<&CV>> {
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

fn parse_hex(h: &str) -> Option<Abs> {
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let d = |c: char| c.to_digit(16).unwrap() as f64;
    let ch: Vec<char> = h.chars().collect();
    let (r, g, b, a) = match ch.len() {
        3 => (d(ch[0]) * 17.0, d(ch[1]) * 17.0, d(ch[2]) * 17.0, 255.0),
        4 => (d(ch[0]) * 17.0, d(ch[1]) * 17.0, d(ch[2]) * 17.0, d(ch[3]) * 17.0),
        6 => (d(ch[0]) * 16.0 + d(ch[1]), d(ch[2]) * 16.0 + d(ch[3]), d(ch[4]) * 16.0 + d(ch[5]), 255.0),
        8 => (d(ch[0]) * 16.0 + d(ch[1]), d(ch[2]) * 16.0 + d(ch[3]), d(ch[4]) * 16.0 + d(ch[5]), d(ch[6]) * 16.0 + d(ch[7])),
        _ => return None,
    };
    Some(Abs::srgb(r / 255.0, g / 255.0, b / 255.0, a / 255.0))
}

/// A raw component token: number, percentage, angle, none, ident or math.
fn comp_of(c: &CV) -> Option<Comp> {
    Some(match c {
        CV::T(Tok::Number(n)) => Comp::Num(*n as f64),
        CV::T(Tok::Percentage(p)) => Comp::Pct(*p as f64),
        CV::T(Tok::Dimension(n, u)) => {
            let u = u.to_ascii_lowercase();
            let k = match u.as_str() {
                "deg" => 1.0,
                "grad" => 0.9,
                "rad" => 180.0 / std::f64::consts::PI,
                "turn" => 360.0,
                _ => return None,
            };
            Comp::Angle(*n as f64 * k, u)
        }
        CV::T(Tok::Ident(s)) => {
            let l = s.to_ascii_lowercase();
            if l == "none" {
                Comp::None
            } else {
                Comp::Ident(l)
            }
        }
        CV::Func(name, args) if calc::is_math_fn(name) => Comp::Calc(calc::parse_fn(name, args)?),
        _ => return None,
    })
}

/// The type a component resolves to: number, percentage or angle.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Ty {
    Num,
    Pct,
    Angle,
    Unknown,
}

fn calc_type(n: &Node, idents_are_numbers: bool) -> Option<Ty> {
    match n {
        Node::Num(_) => Some(Ty::Num),
        Node::Dim(_, u) if u == "%" => Some(Ty::Pct),
        Node::Dim(_, u) => match u.as_str() {
            "deg" | "grad" | "rad" | "turn" => Some(Ty::Angle),
            _ => Some(Ty::Unknown),
        },
        Node::Ident(_) => Some(if idents_are_numbers { Ty::Num } else { Ty::Unknown }),
        Node::Opaque(_) => Some(Ty::Unknown),
        Node::Negate(x) | Node::Invert(x) => calc_type(x, idents_are_numbers),
        Node::Sum(items) => {
            let mut t: Option<Ty> = None;
            let mut unknown = false;
            for it in items {
                match calc_type(it, idents_are_numbers)? {
                    Ty::Unknown => unknown = true,
                    x => {
                        if let Some(t0) = t {
                            if t0 != x {
                                return None;
                            }
                        }
                        t = Some(x)
                    }
                }
            }
            if unknown {
                Some(t.unwrap_or(Ty::Unknown))
            } else {
                t
            }
        }
        Node::Product(items) => {
            let mut t = Ty::Num;
            for it in items {
                let x = match it {
                    Node::Invert(inner) => {
                        let ti = calc_type(inner, idents_are_numbers)?;
                        if ti != Ty::Num && ti != Ty::Unknown {
                            return None;
                        }
                        ti
                    }
                    _ => calc_type(it, idents_are_numbers)?,
                };
                match (t, x) {
                    (Ty::Num, x) => t = x,
                    (_, Ty::Num) => {}
                    (_, Ty::Unknown) | (Ty::Unknown, _) => t = Ty::Unknown,
                    _ => return None,
                }
            }
            Some(t)
        }
        Node::Func(name, args) => match name.as_str() {
            "sign" => Some(Ty::Num),
            "sin" | "cos" | "tan" | "exp" | "log" | "pow" | "sqrt" => Some(Ty::Num),
            "asin" | "acos" | "atan" | "atan2" => Some(Ty::Angle),
            _ => {
                let mut t: Option<Ty> = None;
                for a in args {
                    if matches!(a, Node::Ident(s) if matches!(s.as_str(), "nearest" | "up" | "down" | "to-zero")) {
                        continue;
                    }
                    let x = calc_type(a, idents_are_numbers)?;
                    if x == Ty::Unknown {
                        continue;
                    }
                    if let Some(t0) = t {
                        if t0 != x {
                            return None;
                        }
                    }
                    t = Some(x);
                }
                Some(t.unwrap_or(Ty::Unknown))
            }
        },
    }
}

/// Channel kinds per function: what each component accepts.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// number or percentage (rgb channels, lab/lch L, etc.)
    NumPct,
    /// hue: number or angle
    Hue,
    /// alpha: number or percentage
    Alpha,
}

fn kinds(name: &str) -> [Kind; 3] {
    match name {
        "hsl" | "hsla" | "hwb" => [Kind::Hue, Kind::NumPct, Kind::NumPct],
        "lch" | "oklch" => [Kind::NumPct, Kind::NumPct, Kind::Hue],
        _ => [Kind::NumPct, Kind::NumPct, Kind::NumPct],
    }
}

/// Check a component against its kind (relative: channel keywords allowed).
fn comp_ok(c: &Comp, k: Kind, rel: bool, channels: &[&str]) -> bool {
    match c {
        Comp::None => true,
        Comp::Num(_) => true,
        Comp::Pct(_) => k != Kind::Hue,
        Comp::Angle(..) => k == Kind::Hue,
        Comp::Ident(s) => rel && channels.contains(&s.as_str()),
        Comp::Calc(n) => {
            if !rel && calc_has_unknown_ident(n, &[]) {
                return false;
            }
            if rel && calc_has_unknown_ident(n, channels) {
                return false;
            }
            match calc_type(n, rel) {
                Some(Ty::Num) | Some(Ty::Unknown) => true,
                Some(Ty::Pct) => k != Kind::Hue,
                Some(Ty::Angle) => k == Kind::Hue,
                None => false,
            }
        }
    }
}

fn calc_has_unknown_ident(n: &Node, allowed: &[&str]) -> bool {
    match n {
        Node::Ident(s) => !matches!(s.as_str(), "e" | "pi" | "infinity" | "-infinity" | "NaN" | "nearest" | "up" | "down" | "to-zero") && !allowed.contains(&s.as_str()),
        Node::Negate(x) | Node::Invert(x) => calc_has_unknown_ident(x, allowed),
        Node::Sum(v) | Node::Product(v) | Node::Func(_, v) => v.iter().any(|x| calc_has_unknown_ident(x, allowed)),
        _ => false,
    }
}

fn calc_has_dims(n: &Node) -> bool {
    match n {
        Node::Dim(_, u) => !matches!(u.as_str(), "%" | "deg" | "grad" | "rad" | "turn"),
        Node::Opaque(_) => true,
        Node::Negate(x) | Node::Invert(x) => calc_has_dims(x),
        Node::Sum(v) | Node::Product(v) | Node::Func(_, v) => v.iter().any(calc_has_dims),
        _ => false,
    }
}

fn channels_of(name: &str, space: Space) -> &'static [&'static str] {
    match name {
        "rgb" | "rgba" => &["r", "g", "b", "alpha"],
        "hsl" | "hsla" => &["h", "s", "l", "alpha"],
        "hwb" => &["h", "w", "b", "alpha"],
        "lab" | "oklab" => &["l", "a", "b", "alpha"],
        "lch" | "oklch" => &["l", "c", "h", "alpha"],
        _ => {
            if space.is_xyz() {
                &["x", "y", "z", "alpha"]
            } else {
                &["r", "g", "b", "alpha"]
            }
        }
    }
}

fn fn_space(name: &str) -> Space {
    match name {
        "rgb" | "rgba" => Space::Srgb,
        "hsl" | "hsla" => Space::Hsl,
        "hwb" => Space::Hwb,
        "lab" => Space::Lab,
        "lch" => Space::Lch,
        "oklab" => Space::Oklab,
        "oklch" => Space::Oklch,
        _ => Space::Srgb,
    }
}

fn parse_fn(name: &str, args: &[CV]) -> Option<Spec> {
    let t = toks(args);
    if t.is_empty() {
        return None;
    }
    // Relative color: <fn>(from <color> …)
    if t[0].is_ident("from") {
        return parse_relative(name, &t[1..]);
    }
    let legacy_fn = matches!(name, "rgb" | "rgba" | "hsl" | "hsla");
    if t.iter().any(|c| c.is_comma()) {
        if !legacy_fn {
            return None;
        }
        return parse_legacy(name, args);
    }
    // Modern syntax.
    let (space, rest) = if name == "color" {
        let sp = Space::from_color_fn(&t[0].ident()?.to_ascii_lowercase())?;
        (sp, &t[1..])
    } else {
        (fn_space(name), &t[..])
    };
    let (chs, alpha) = split_alpha(rest)?;
    if chs.len() != 3 {
        return None;
    }
    let ks = kinds(name);
    let mut c: Vec<Comp> = Vec::new();
    for (i, cv) in chs.iter().enumerate() {
        let comp = comp_of(cv)?;
        if !comp_ok(&comp, ks[i], false, &[]) {
            return None;
        }
        c.push(comp);
    }
    let alpha = match alpha {
        Some(a) => {
            let comp = comp_of(a)?;
            if !comp_ok(&comp, Kind::Alpha, false, &[]) {
                return None;
            }
            Some(comp)
        }
        None => None,
    };
    let fs = FuncSpec { name: canonical_fn_name(name).to_string(), space, from: None, c: [c[0].clone(), c[1].clone(), c[2].clone()], alpha };
    // Resolve now unless some math depends on unknown units.
    let unresolvable = fs.c.iter().chain(fs.alpha.iter()).any(|c| matches!(c, Comp::Calc(n) if calc_has_dims(n)));
    if unresolvable {
        return Some(Spec::Func(Box::new(fs)));
    }
    // Non-legacy functions keep calc() as written in specified values.
    let has_calc = fs.c.iter().chain(fs.alpha.iter()).any(|c| matches!(c, Comp::Calc(_)));
    if has_calc && !legacy_fn && name != "hwb" {
        return Some(Spec::Func(Box::new(fs)));
    }
    let abs = resolve_func(&fs, None)?;
    Some(Spec::Abs(abs))
}

fn canonical_fn_name(n: &str) -> &str {
    match n {
        "rgba" => "rgb",
        "hsla" => "hsl",
        x => x,
    }
}

/// Split modern channels from `/ alpha`.
fn split_alpha<'a>(t: &[&'a CV]) -> Option<(Vec<&'a CV>, Option<&'a CV>)> {
    match t.iter().position(|c| c.is_delim('/')) {
        Some(i) => {
            if i + 2 != t.len() {
                return None;
            }
            Some((t[..i].to_vec(), Some(t[i + 1])))
        }
        None => Some((t.to_vec(), None)),
    }
}

fn parse_legacy(name: &str, args: &[CV]) -> Option<Spec> {
    let parts = split_commas(args);
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    if parts.iter().any(|p| p.len() != 1) {
        return None;
    }
    let comps: Vec<Comp> = parts.iter().map(|p| comp_of(p[0])).collect::<Option<Vec<_>>>()?;
    if comps.iter().any(|c| matches!(c, Comp::None | Comp::Ident(_))) {
        return None;
    }
    let ty = |c: &Comp| -> Option<Ty> {
        match c {
            Comp::Num(_) => Some(Ty::Num),
            Comp::Pct(_) => Some(Ty::Pct),
            Comp::Angle(..) => Some(Ty::Angle),
            Comp::Calc(n) => {
                if calc_has_unknown_ident(n, &[]) {
                    return None;
                }
                calc_type(n, false)
            }
            _ => None,
        }
    };
    let tys: Vec<Ty> = comps.iter().map(ty).collect::<Option<Vec<_>>>()?;
    if name.starts_with("rgb") {
        let t0 = if tys[0] == Ty::Unknown { tys.iter().take(3).find(|t| **t != Ty::Unknown).copied().unwrap_or(Ty::Num) } else { tys[0] };
        if !matches!(t0, Ty::Num | Ty::Pct) || tys[..3].iter().any(|t| *t != t0 && *t != Ty::Unknown) {
            return None;
        }
    } else {
        if !matches!(tys[0], Ty::Num | Ty::Angle | Ty::Unknown) || !matches!(tys[1], Ty::Pct | Ty::Unknown) || !matches!(tys[2], Ty::Pct | Ty::Unknown) {
            return None;
        }
    }
    if let Some(t) = tys.get(3) {
        if !matches!(t, Ty::Num | Ty::Pct | Ty::Unknown) {
            return None;
        }
    }
    let fs = FuncSpec {
        name: canonical_fn_name(name).to_string(),
        space: fn_space(name),
        from: None,
        c: [comps[0].clone(), comps[1].clone(), comps[2].clone()],
        alpha: comps.get(3).cloned(),
    };
    let unresolvable = fs.c.iter().chain(fs.alpha.iter()).any(|c| matches!(c, Comp::Calc(n) if calc_has_dims(n)));
    if unresolvable {
        return Some(Spec::Func(Box::new(fs)));
    }
    Some(Spec::Abs(resolve_func(&fs, None)?))
}

fn parse_relative(name: &str, t: &[&CV]) -> Option<Spec> {
    let origin = parse(t.first()?)?;
    let mut rest = &t[1..];
    let space = if name == "color" {
        let sp = Space::from_color_fn(&rest.first()?.ident()?.to_ascii_lowercase())?;
        rest = &rest[1..];
        sp
    } else {
        fn_space(name)
    };
    let (chs, alpha) = split_alpha(rest)?;
    if chs.len() != 3 {
        return None;
    }
    let names = channels_of(name, space);
    let ks = kinds(name);
    let mut c: Vec<Comp> = Vec::new();
    for (i, cv) in chs.iter().enumerate() {
        let comp = comp_of(cv)?;
        if !comp_ok(&comp, ks[i], true, names) {
            return None;
        }
        c.push(comp);
    }
    let alpha = match alpha {
        Some(a) => {
            let comp = comp_of(a)?;
            if !comp_ok(&comp, Kind::Alpha, true, names) {
                return None;
            }
            Some(comp)
        }
        None => None,
    };
    Some(Spec::Func(Box::new(FuncSpec { name: canonical_fn_name(name).to_string(), space, from: Some(origin), c: [c[0].clone(), c[1].clone(), c[2].clone()], alpha })))
}

fn parse_alpha_fn(args: &[CV]) -> Option<Spec> {
    let t = toks(args);
    if t.len() < 2 || !t[0].is_ident("from") {
        return None;
    }
    let origin = parse(t[1])?;
    let rest = &t[2..];
    if rest.len() != 2 || !rest[0].is_delim('/') {
        return None;
    }
    let comp = match rest[1] {
        CV::Func(n, _) if matches!(n.to_ascii_lowercase().as_str(), "sibling-index" | "sibling-count") => Comp::Calc(Node::Opaque(super::tokenizer::serialize(std::slice::from_ref(rest[1])))),
        cv => comp_of(cv)?,
    };
    if !comp_ok(&comp, Kind::Alpha, true, &["alpha"]) && !matches!(&comp, Comp::Calc(Node::Opaque(_))) {
        return None;
    }
    Some(Spec::Alpha(Box::new(origin), Some(comp)))
}

fn parse_mix(args: &[CV]) -> Option<Spec> {
    let parts = split_commas(args);
    let head = &parts[0];
    let (space, hue, explicit, rest) = if !head.is_empty() && head[0].is_ident("in") {
        let space = Space::mix_space(&head.get(1)?.ident()?.to_ascii_lowercase())?;
        let mut hue = None;
        match head.len() {
            2 => {}
            4 => {
                if !space.is_polar() || !head[3].is_ident("hue") {
                    return None;
                }
                let h = head[2].ident()?.to_ascii_lowercase();
                if !matches!(h.as_str(), "shorter" | "longer" | "increasing" | "decreasing") {
                    return None;
                }
                hue = Some(h);
            }
            _ => return None,
        }
        (space, hue, true, &parts[1..])
    } else {
        (Space::Oklab, None, false, &parts[..])
    };
    if rest.is_empty() {
        return None;
    }
    let item = |p: &[&CV]| -> Option<(Spec, Option<Comp>)> {
        let mut col = None;
        let mut pct = None;
        if p.is_empty() || p.len() > 2 {
            return None;
        }
        for c in p {
            let as_pct = match c {
                CV::T(Tok::Percentage(x)) => Some(Comp::Pct(*x as f64)),
                CV::Func(n, a) if calc::is_math_fn(n) => {
                    let node = calc::parse_fn(n, a)?;
                    match calc_type(&node, false)? {
                        Ty::Pct | Ty::Unknown => Some(Comp::Calc(node)),
                        _ => return None,
                    }
                }
                _ => None,
            };
            match as_pct {
                Some(pc) => {
                    if pct.is_some() {
                        return None;
                    }
                    pct = Some(pc)
                }
                None => {
                    if col.is_some() {
                        return None;
                    }
                    col = Some(parse(c)?)
                }
            }
        }
        Some((col?, pct))
    };
    let mut items = Vec::new();
    for p in rest {
        let it = item(p)?;
        if let Some(Comp::Pct(v)) = &it.1 {
            if *v < 0.0 || *v > 100.0 {
                return None;
            }
        }
        items.push(it);
    }
    Some(Spec::Mix(Box::new(MixSpec { space, explicit, hue, items })))
}

// ------------------------------------------------------------------ resolution

/// Lengths for math in colors at computed time (font-relative and viewport units).
#[derive(Clone, Copy, Debug)]
pub struct UnitEnv {
    pub em: f64,
    pub rem: f64,
    pub vw: f64,
    pub vh: f64,
    pub cqw: f64,
    pub cqh: f64,
}

thread_local! {
    static UNIT_ENV: std::cell::Cell<UnitEnv> = const { std::cell::Cell::new(UnitEnv { em: 16.0, rem: 16.0, vw: 0.0, vh: 0.0, cqw: 0.0, cqh: 0.0 }) };
}

/// Set the unit environment used by `compute` (per element).
pub fn set_unit_env(e: UnitEnv) {
    UNIT_ENV.with(|c| c.set(e));
}

/// Replace relative lengths with px (computed time).
fn absolutize(n: &Node) -> Node {
    match n {
        Node::Dim(v, u) => {
            let e = UNIT_ENV.with(|c| c.get());
            let k = match u.as_str() {
                "em" => e.em,
                "rem" => e.rem,
                "ex" | "ch" => e.em * 0.5,
                "lh" => e.em * 1.2,
                "rlh" => e.rem * 1.2,
                "vw" | "svw" | "lvw" | "dvw" | "vi" => e.vw / 100.0,
                "vh" | "svh" | "lvh" | "dvh" | "vb" => e.vh / 100.0,
                "vmin" => e.vw.min(e.vh) / 100.0,
                "vmax" => e.vw.max(e.vh) / 100.0,
                "cqw" | "cqi" => e.cqw / 100.0,
                "cqh" | "cqb" => e.cqh / 100.0,
                "cqmin" => e.cqw.min(e.cqh) / 100.0,
                "cqmax" => e.cqw.max(e.cqh) / 100.0,
                _ => return n.clone(),
            };
            Node::Dim(v * k, "px".into())
        }
        Node::Negate(x) => Node::Negate(Box::new(absolutize(x))),
        Node::Invert(x) => Node::Invert(Box::new(absolutize(x))),
        Node::Sum(v) => Node::Sum(v.iter().map(absolutize).collect()),
        Node::Product(v) => Node::Product(v.iter().map(absolutize).collect()),
        Node::Func(f, v) => Node::Func(f.clone(), v.iter().map(absolutize).collect()),
        _ => n.clone(),
    }
}

/// Resolve a component to its value in the channel's reference range.
/// `pct_ref`: what 100% maps to. Hue angles to degrees.
fn comp_value(c: &Comp, pct_ref: f64, bind: &dyn Fn(&str) -> Option<f64>) -> Option<Option<f64>> {
    Some(match c {
        Comp::None => None,
        Comp::Num(v) => Some(*v),
        Comp::Pct(p) => Some(p / 100.0 * pct_ref),
        Comp::Angle(d, _) => Some(*d),
        Comp::Ident(s) => Some(bind(s)?),
        Comp::Calc(n) => {
            let b = |s: &str| bind(s).map(Node::Num);
            let r = calc::simplify(&absolutize(n), &b);
            match r {
                Node::Num(v) => Some(v),
                Node::Dim(v, u) => Some(match u.as_str() {
                    "%" => v / 100.0 * pct_ref,
                    "deg" => v,
                    "grad" => v * 0.9,
                    "rad" => v.to_degrees(),
                    "turn" => v * 360.0,
                    _ => return None,
                }),
                _ => return None,
            }
        }
    })
}

/// Channel percentage references per function.
fn pct_refs(name: &str, space: Space) -> [f64; 3] {
    match name {
        "rgb" | "rgba" => [255.0, 255.0, 255.0],
        "hsl" | "hsla" | "hwb" => [0.0, 100.0, 100.0],
        "lab" => [100.0, 125.0, 125.0],
        "lch" => [100.0, 150.0, 0.0],
        "oklab" => [1.0, 0.4, 0.4],
        "oklch" => [1.0, 0.4, 0.0],
        _ => {
            let _ = space;
            [1.0, 1.0, 1.0]
        }
    }
}

fn nan0(v: f64) -> f64 {
    if v.is_nan() {
        0.0
    } else {
        v
    }
}

fn norm_hue(h: f64) -> f64 {
    if !h.is_finite() {
        return 0.0;
    }
    let r = h.rem_euclid(360.0);
    if r >= 360.0 {
        0.0
    } else {
        r
    }
}

/// Resolve a function (absolute or relative, given the origin) to an absolute color.
fn resolve_func(fs: &FuncSpec, origin: Option<&Abs>) -> Option<Abs> {
    let name = fs.name.as_str();
    let space = if name == "color" { fs.space } else { fn_space(name) };
    // Channel bindings for relative colors: the origin converted into this function's space.
    let origin_vals: Option<([Option<f64>; 3], Option<f64>)> = origin.map(|o| {
        let conv = convert(o, space);
        let mut c = conv.c;
        // rgb() channels are referenced in 0..255.
        if name == "rgb" {
            for v in c.iter_mut() {
                *v = v.map(|x| x * 255.0);
            }
        }
        (c, conv.a)
    });
    let names = channels_of(name, space);
    let bind = |s: &str| -> Option<f64> {
        let (c, a) = origin_vals.as_ref()?;
        let i = names.iter().position(|n| *n == s)?;
        // `none` channels resolve to 0 in math.
        Some(if i == 3 { a.unwrap_or(0.0) } else { c[i].unwrap_or(0.0) })
    };
    let refs = pct_refs(name, space);
    let mut c: [Option<f64>; 3] = [None; 3];
    for i in 0..3 {
        // A bare channel keyword keeps `none` from the origin.
        if let (Comp::Ident(s), Some((oc, oa))) = (&fs.c[i], origin_vals.as_ref()) {
            let j = names.iter().position(|n| *n == s.as_str())?;
            c[i] = if j == 3 { *oa } else { oc[j] };
            continue;
        }
        c[i] = comp_value(&fs.c[i], refs[i], &bind)?;
    }
    let alpha = match &fs.alpha {
        // A relative color without alpha keeps the origin's.
        None => match origin_vals.as_ref() {
            Some((_, oa)) => *oa,
            None => Some(1.0),
        },
        Some(Comp::Ident(s)) if origin_vals.is_some() => {
            let (oc, oa) = origin_vals.as_ref().unwrap();
            let j = names.iter().position(|n| *n == s.as_str())?;
            if j == 3 {
                *oa
            } else {
                oc[j]
            }
        }
        Some(a) => comp_value(a, 1.0, &bind)?,
    };
    let alpha = alpha.map(|a| nan0(a).clamp(0.0, 1.0));
    let legacy = matches!(name, "rgb" | "hsl" | "hwb") && origin.is_none();
    let mut out = Abs { space, c, a: alpha, legacy };
    // Clamp/normalize per space.
    match space {
        Space::Srgb if name == "rgb" => {
            for v in out.c.iter_mut() {
                *v = v.map(|x| {
                    let x = if x.is_nan() { 0.0 } else { x };
                    if legacy {
                        x.clamp(0.0, 255.0) / 255.0
                    } else {
                        x / 255.0
                    }
                });
            }
        }
        Space::Hsl | Space::Hwb => {
            if let Some(h) = out.c[0] {
                out.c[0] = Some(norm_hue(h));
            }
            for i in 1..3 {
                out.c[i] = out.c[i].map(nan0);
            }
            if space == Space::Hsl {
                out.c[1] = out.c[1].map(|s| s.max(0.0));
            }
        }
        Space::Lab | Space::Oklab => {
            let max_l = if space == Space::Lab { 100.0 } else { 1.0 };
            out.c[0] = out.c[0].map(|l| nan0(l).clamp(0.0, max_l));
            for i in 1..3 {
                out.c[i] = out.c[i].map(nan0);
            }
        }
        Space::Lch | Space::Oklch => {
            let max_l = if space == Space::Lch { 100.0 } else { 1.0 };
            out.c[0] = out.c[0].map(|l| nan0(l).clamp(0.0, max_l));
            out.c[1] = out.c[1].map(|c| nan0(c).max(0.0));
            out.c[2] = out.c[2].map(norm_hue);
        }
        _ => {
            for v in out.c.iter_mut() {
                *v = v.map(nan0);
            }
        }
    }
    if legacy && space == Space::Srgb && out.c.iter().any(|v| v.is_none()) {
        out.legacy = false;
    }
    if legacy && matches!(space, Space::Hsl | Space::Hwb) && out.a.is_none() && out.c.iter().all(|v| v.is_some()) {
        // hsl(… / none): keep the function form.
    }
    Some(out)
}

// ------------------------------------------------------------------ conversions

type V3 = [f64; 3];

fn mat(m: &[[f64; 3]; 3], v: V3) -> V3 {
    [m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2], m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2], m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2]]
}

fn srgb_to_lin(c: f64) -> f64 {
    let a = c.abs();
    if a <= 0.04045 {
        c / 12.92
    } else {
        c.signum() * ((a + 0.055) / 1.055).powf(2.4)
    }
}
fn lin_to_srgb(c: f64) -> f64 {
    let a = c.abs();
    if a > 0.0031308 {
        c.signum() * (1.055 * a.powf(1.0 / 2.4) - 0.055)
    } else {
        12.92 * c
    }
}

const SRGB_TO_XYZ: [[f64; 3]; 3] = [
    [506752.0 / 1228815.0, 87881.0 / 245763.0, 12673.0 / 70218.0],
    [87098.0 / 409605.0, 175762.0 / 245763.0, 12673.0 / 175545.0],
    [7918.0 / 409605.0, 87881.0 / 737289.0, 1001167.0 / 1053270.0],
];
const XYZ_TO_SRGB: [[f64; 3]; 3] = [
    [12831.0 / 3959.0, -329.0 / 214.0, -1974.0 / 3959.0],
    [-851781.0 / 878810.0, 1648619.0 / 878810.0, 36519.0 / 878810.0],
    [705.0 / 12673.0, -2585.0 / 12673.0, 705.0 / 667.0],
];
const P3_TO_XYZ: [[f64; 3]; 3] = [
    [608311.0 / 1250200.0, 189793.0 / 714400.0, 198249.0 / 1000160.0],
    [35783.0 / 156275.0, 247089.0 / 357200.0, 198249.0 / 2500400.0],
    [0.0 / 1.0, 32229.0 / 714400.0, 5220557.0 / 5000800.0],
];
const XYZ_TO_P3: [[f64; 3]; 3] = [
    [446124.0 / 178915.0, -333277.0 / 357830.0, -72051.0 / 178915.0],
    [-14852.0 / 17905.0, 63121.0 / 35810.0, 423.0 / 17905.0],
    [11844.0 / 330415.0, -50337.0 / 660830.0, 316169.0 / 330415.0],
];
const A98_TO_XYZ: [[f64; 3]; 3] = [
    [573536.0 / 994567.0, 263643.0 / 1420810.0, 187206.0 / 994567.0],
    [591459.0 / 1989134.0, 6239551.0 / 9945670.0, 374412.0 / 4972835.0],
    [53769.0 / 1989134.0, 351524.0 / 4972835.0, 4929758.0 / 4972835.0],
];
const XYZ_TO_A98: [[f64; 3]; 3] = [
    [1829569.0 / 896150.0, -506331.0 / 896150.0, -308931.0 / 896150.0],
    [-851781.0 / 878810.0, 1648619.0 / 878810.0, 36519.0 / 878810.0],
    [16779.0 / 1248040.0, -147721.0 / 1248040.0, 1266979.0 / 1248040.0],
];
const REC2020_TO_XYZ: [[f64; 3]; 3] = [
    [63426534.0 / 99577255.0, 20160776.0 / 139408157.0, 47086771.0 / 278816314.0],
    [26158966.0 / 99577255.0, 472592308.0 / 697040785.0, 8267143.0 / 139408157.0],
    [0.0, 19567812.0 / 697040785.0, 295819943.0 / 278816314.0],
];
const XYZ_TO_REC2020: [[f64; 3]; 3] = [
    [30757411.0 / 17917100.0, -6372589.0 / 17917100.0, -4539589.0 / 17917100.0],
    [-19765991.0 / 29648200.0, 47925759.0 / 29648200.0, 467509.0 / 29648200.0],
    [792561.0 / 44930125.0, -1921689.0 / 44930125.0, 42328811.0 / 44930125.0],
];
const PROPHOTO_TO_XYZ50: [[f64; 3]; 3] = [
    [0.7977666449006423, 0.13518129740053308, 0.0313477341283922],
    [0.2880748288194013, 0.711835234241873, 0.00008993693872564],
    [0.0, 0.0, 0.8251046025104602],
];
const XYZ50_TO_PROPHOTO: [[f64; 3]; 3] = [
    [1.3457868816471583, -0.25557208737979464, -0.05110186497554526],
    [-0.5446307051249019, 1.5082477428451468, 0.02052744743642139],
    [0.0, 0.0, 1.2119675456389452],
];
const D65_TO_D50: [[f64; 3]; 3] = [
    [1.0479297925449969, 0.022946870601609652, -0.05019226628920524],
    [0.02962780877005599, 0.9904344267538799, -0.017073799063418826],
    [-0.009243040646204504, 0.015055191490298152, 0.7518742814281371],
];
const D50_TO_D65: [[f64; 3]; 3] = [
    [0.955473421488075, -0.02309845494876471, 0.06325924320057072],
    [-0.0283697093338637, 1.0099953980813041, 0.021041441191917323],
    [0.012314014864481998, -0.020507649298898964, 1.330365926242124],
];
const D50: V3 = [0.3457 / 0.3585, 1.0, (1.0 - 0.3457 - 0.3585) / 0.3585];
const XYZ_TO_LMS: [[f64; 3]; 3] = [
    [0.8190224379967030, 0.3619062600528904, -0.1288737815209879],
    [0.0329836539323885, 0.9292868615863434, 0.0361446663506424],
    [0.0481771893596242, 0.2642395317527308, 0.6335478284694309],
];
const LMS_TO_OKLAB: [[f64; 3]; 3] = [
    [0.2104542683093140, 0.7936177747023054, -0.0040720430116193],
    [1.9779985324311684, -2.4285922420485799, 0.4505937096174110],
    [0.0259040424655478, 0.7827717124575296, -0.8086757548808609],
];
const OKLAB_TO_LMS: [[f64; 3]; 3] = [
    [1.0000000000000000, 0.3963377773761749, 0.2158037573099136],
    [1.0000000000000000, -0.1055613458156586, -0.0638541728258133],
    [1.0000000000000000, -0.0894841775298119, -1.2914855480194092],
];
const LMS_TO_XYZ: [[f64; 3]; 3] = [
    [1.2268798758459243, -0.5578149944602171, 0.2813910456659647],
    [-0.0405757452148008, 1.1122868032803170, -0.0717110580655164],
    [-0.0763729366746601, -0.4214933324022432, 1.5869240198367816],
];

fn a98_lin(c: f64) -> f64 {
    c.signum() * c.abs().powf(563.0 / 256.0)
}
fn a98_gam(c: f64) -> f64 {
    c.signum() * c.abs().powf(256.0 / 563.0)
}
fn pp_lin(c: f64) -> f64 {
    let a = c.abs();
    if a <= 16.0 / 512.0 {
        c / 16.0
    } else {
        c.signum() * a.powf(1.8)
    }
}
fn pp_gam(c: f64) -> f64 {
    let a = c.abs();
    if a >= 1.0 / 512.0 {
        c.signum() * a.powf(1.0 / 1.8)
    } else {
        16.0 * c
    }
}
fn r2020_lin(c: f64) -> f64 {
    let a = 1.09929682680944;
    let b = 0.018053968510807;
    let ab = c.abs();
    if ab < b * 4.5 {
        c / 4.5
    } else {
        c.signum() * ((ab + a - 1.0) / a).powf(1.0 / 0.45)
    }
}
fn r2020_gam(c: f64) -> f64 {
    let a = 1.09929682680944;
    let b = 0.018053968510807;
    let ab = c.abs();
    if ab > b {
        c.signum() * (a * ab.powf(0.45) - (a - 1.0))
    } else {
        4.5 * c
    }
}

fn hsl_to_srgb(h: f64, s: f64, l: f64) -> V3 {
    let s = s / 100.0;
    let l = l / 100.0;
    let f = |n: f64| {
        let k = (n + h / 30.0).rem_euclid(12.0);
        let a = s * l.min(1.0 - l);
        l - a * (k - 3.0).min(9.0 - k).min(1.0).max(-1.0)
    };
    [f(0.0), f(8.0), f(4.0)]
}

fn srgb_to_hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (min + max) / 2.0;
    let d = max - min;
    let mut h = f64::NAN;
    let mut s = 0.0;
    if d != 0.0 {
        s = if l == 0.0 || l == 1.0 { 0.0 } else { (max - l) / l.min(1.0 - l) };
        h = if max == r {
            (g - b) / d + if g < b { 6.0 } else { 0.0 }
        } else if max == g {
            (b - r) / d + 2.0
        } else {
            (r - g) / d + 4.0
        };
        h *= 60.0;
    }
    // Negative saturation (out-of-gamut colors): flip the hue.
    if s < 0.0 {
        h += 180.0;
        s = s.abs();
    }
    // Achromatic (as Chrome, below a tiny saturation): the hue is powerless.
    if s.abs() <= 1.05e-5 {
        s = 0.0;
        h = f64::NAN;
    }
    if h >= 360.0 {
        h -= 360.0;
    }
    (h, s * 100.0, l * 100.0)
}

/// The hue of an sRGB color for HWB (no flip for out-of-gamut colors).
fn hwb_hue(r: f64, g: f64, b: f64) -> f64 {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let l = (max + min) / 2.0;
    let sat = if l == 0.0 || l == 1.0 { 0.0 } else { (max - l) / l.min(1.0 - l) };
    if d.abs() <= 1e-12 || sat.abs() <= 1.05e-5 {
        return f64::NAN;
    }
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    norm_hue(h * 60.0)
}

fn hwb_to_srgb(h: f64, w: f64, b: f64) -> V3 {
    let w = w / 100.0;
    let b = b / 100.0;
    if w + b >= 1.0 {
        let g = w / (w + b);
        return [g, g, g];
    }
    let rgb = hsl_to_srgb(h, 100.0, 50.0);
    rgb.map(|c| c * (1.0 - w - b) + w)
}

/// To linear-light XYZ (D65) from an absolute color's channels (none = 0).
fn to_xyz65(space: Space, c: V3) -> V3 {
    match space {
        Space::Srgb => mat(&SRGB_TO_XYZ, c.map(srgb_to_lin)),
        Space::SrgbLinear => mat(&SRGB_TO_XYZ, c),
        Space::DisplayP3 => mat(&P3_TO_XYZ, c.map(srgb_to_lin)),
        Space::DisplayP3Linear => mat(&P3_TO_XYZ, c),
        Space::A98 => mat(&A98_TO_XYZ, c.map(a98_lin)),
        Space::ProPhoto => mat(&D50_TO_D65, mat(&PROPHOTO_TO_XYZ50, c.map(pp_lin))),
        Space::Rec2020 => mat(&REC2020_TO_XYZ, c.map(r2020_lin)),
        Space::XyzD65 => c,
        Space::XyzD50 => mat(&D50_TO_D65, c),
        Space::Lab => mat(&D50_TO_D65, lab_to_xyz50(c)),
        Space::Lch => mat(&D50_TO_D65, lab_to_xyz50(lch_to_lab(c))),
        Space::Oklab => oklab_to_xyz(c),
        Space::Oklch => oklab_to_xyz(lch_to_lab(c)),
        Space::Hsl => to_xyz65(Space::Srgb, hsl_to_srgb(c[0], c[1], c[2])),
        Space::Hwb => to_xyz65(Space::Srgb, hwb_to_srgb(c[0], c[1], c[2])),
    }
}

fn from_xyz65(space: Space, x: V3) -> V3 {
    match space {
        Space::Srgb => mat(&XYZ_TO_SRGB, x).map(lin_to_srgb),
        Space::SrgbLinear => mat(&XYZ_TO_SRGB, x),
        Space::DisplayP3 => mat(&XYZ_TO_P3, x).map(lin_to_srgb),
        Space::DisplayP3Linear => mat(&XYZ_TO_P3, x),
        Space::A98 => mat(&XYZ_TO_A98, x).map(a98_gam),
        Space::ProPhoto => mat(&XYZ50_TO_PROPHOTO, mat(&D65_TO_D50, x)).map(pp_gam),
        Space::Rec2020 => mat(&XYZ_TO_REC2020, x).map(r2020_gam),
        Space::XyzD65 => x,
        Space::XyzD50 => mat(&D65_TO_D50, x),
        Space::Lab => xyz50_to_lab(mat(&D65_TO_D50, x)),
        Space::Lch => lab_to_lch(xyz50_to_lab(mat(&D65_TO_D50, x)), 0.0015),
        Space::Oklab => xyz_to_oklab(x),
        Space::Oklch => lab_to_lch(xyz_to_oklab(x), 0.0000039),
        Space::Hsl => {
            let s = from_xyz65(Space::Srgb, x);
            let (h, sa, l) = srgb_to_hsl(s[0], s[1], s[2]);
            [h, sa, l]
        }
        Space::Hwb => {
            let s = from_xyz65(Space::Srgb, x);
            let h = hwb_hue(s[0], s[1], s[2]);
            let w = s[0].min(s[1]).min(s[2]);
            let b = 1.0 - s[0].max(s[1]).max(s[2]);
            [if w + b >= 1.0 - 1e-12 { f64::NAN } else { h }, w * 100.0, b * 100.0]
        }
    }
}

fn lab_to_xyz50(lab: V3) -> V3 {
    let k = 24389.0 / 27.0;
    let e = 216.0 / 24389.0;
    let f1 = (lab[0] + 16.0) / 116.0;
    let f0 = lab[1] / 500.0 + f1;
    let f2 = f1 - lab[2] / 200.0;
    let x = if f0.powi(3) > e { f0.powi(3) } else { (116.0 * f0 - 16.0) / k };
    let y = if lab[0] > k * e { ((lab[0] + 16.0) / 116.0).powi(3) } else { lab[0] / k };
    let z = if f2.powi(3) > e { f2.powi(3) } else { (116.0 * f2 - 16.0) / k };
    [x * D50[0], y * D50[1], z * D50[2]]
}

fn xyz50_to_lab(x: V3) -> V3 {
    let k = 24389.0 / 27.0;
    let e = 216.0 / 24389.0;
    let xs = [x[0] / D50[0], x[1] / D50[1], x[2] / D50[2]];
    let f = xs.map(|v| if v > e { v.cbrt() } else { (k * v + 16.0) / 116.0 });
    [116.0 * f[1] - 16.0, 500.0 * (f[0] - f[1]), 200.0 * (f[1] - f[2])]
}

fn lch_to_lab(c: V3) -> V3 {
    let h = c[2].to_radians();
    [c[0], c[1] * h.cos(), c[1] * h.sin()]
}

/// Lab to LCH; below `eps` chroma the color is achromatic (chroma 0, hue powerless).
fn lab_to_lch(l: V3, eps: f64) -> V3 {
    let c = (l[1] * l[1] + l[2] * l[2]).sqrt();
    if c < eps {
        return [l[0], 0.0, f64::NAN];
    }
    [l[0], c, norm_hue(l[2].atan2(l[1]).to_degrees())]
}

fn xyz_to_oklab(x: V3) -> V3 {
    let lms = mat(&XYZ_TO_LMS, x).map(|v| v.cbrt());
    mat(&LMS_TO_OKLAB, lms)
}

fn oklab_to_xyz(l: V3) -> V3 {
    let lms = mat(&OKLAB_TO_LMS, l).map(|v| v * v * v);
    mat(&LMS_TO_XYZ, lms)
}

/// Convert an absolute color to `to`; missing (none) channels convert as 0,
/// then carry over to analogous channels; powerless hues become none.
pub fn convert(c: &Abs, to: Space) -> Abs {
    if c.space == to {
        let mut o = c.clone();
        o.legacy = false;
        return o;
    }
    let mut vals: V3 = [c.c[0].unwrap_or(0.0), c.c[1].unwrap_or(0.0), c.c[2].unwrap_or(0.0)];
    // A powerless hue (chroma at most Chrome's threshold) converts as gray.
    let mut src_powerless = false;
    if (c.space == Space::Lch && vals[1] <= 0.0015 + 1e-9) || (c.space == Space::Oklch && vals[1] <= 0.000004 + 1e-12) {
        vals[1] = 0.0;
        src_powerless = true;
    }
    // Hue none counts as 0.
    let x = to_xyz65(c.space, vals);
    let mut out = from_xyz65(to, x);
    // Shortcuts that avoid round trips through XYZ for sRGB-family conversions.
    if c.space == Space::Hsl && to == Space::Srgb {
        out = hsl_to_srgb(vals[0], vals[1], vals[2]);
    } else if c.space == Space::Hwb && to == Space::Srgb {
        out = hwb_to_srgb(vals[0], vals[1], vals[2]);
    } else if c.space == Space::Srgb && to == Space::Hsl {
        let (h, s, l) = srgb_to_hsl(vals[0], vals[1], vals[2]);
        out = [h, s, l];
    } else if c.space == Space::Srgb && to == Space::Hwb {
        let h = hwb_hue(vals[0], vals[1], vals[2]);
        let w = vals[0].min(vals[1]).min(vals[2]);
        let b = 1.0 - vals[0].max(vals[1]).max(vals[2]);
        out = [if w + b >= 1.0 - 1e-12 { f64::NAN } else { h }, w * 100.0, b * 100.0];
    } else if (c.space == Space::Lab && to == Space::Lch) || (c.space == Space::Oklab && to == Space::Oklch) {
        out = lab_to_lch(vals, if to == Space::Lch { 0.0015 } else { 0.0000039 });
    } else if (c.space == Space::Lch && to == Space::Lab) || (c.space == Space::Oklch && to == Space::Oklab) {
        out = lch_to_lab(vals);
    }
    let mut res = Abs { space: to, c: [Some(out[0]), Some(out[1]), Some(out[2])], a: c.a, legacy: false };
    if let Some(hi) = to.hue_index() {
        if out[hi].is_nan() || src_powerless {
            res.c[hi] = None;
        }
    }
    // Carry missing components to analogous ones.
    if c.c.iter().all(|v| v.is_none()) {
        res.c = [None; 3];
        return res;
    }
    for i in 0..3 {
        if c.c[i].is_none() {
            if let Some(j) = analogous(c.space, i, to) {
                res.c[j] = None;
            }
        }
    }
    // As Chrome: lab/oklab with both a and b missing has missing chroma;
    // hwb with both whiteness and blackness missing has missing s and l.
    if matches!(c.space, Space::Lab | Space::Oklab) && c.c[1].is_none() && c.c[2].is_none() {
        match to {
            Space::Hsl => res.c[1] = None,
            Space::Lch | Space::Oklch => res.c[1] = None,
            _ => {}
        }
    }
    // Both channels of the colorfulness/lightness pair missing (hsl s+l,
    // hwb w+b, lch c+l) carry over as a pair.
    let pair = |sp: Space| match sp {
        Space::Hsl | Space::Hwb => Some((1, 2)),
        Space::Lch | Space::Oklch => Some((1, 0)),
        _ => None,
    };
    if let (Some((a1, b1)), Some((a2, b2))) = (pair(c.space), pair(to)) {
        if c.c[a1].is_none() && c.c[b1].is_none() {
            res.c[a2] = None;
            res.c[b2] = None;
        }
    }
    res
}

/// Analogous component categories (CSS Color 4 §12.2): reds, greens, blues,
/// lightness, colorfulness, hue, opposing a/b.
fn category(space: Space, i: usize) -> Option<u8> {
    Some(match space {
        s if s.is_rgbish() => [0, 1, 2][i],
        s if s.is_xyz() => [0, 1, 2][i],
        Space::Lab | Space::Oklab => [3, 6, 7][i],
        Space::Lch | Space::Oklch => [3, 4, 5][i],
        Space::Hsl => [5, 4, 3][i],
        Space::Hwb => return if i == 0 { Some(5) } else { None },
        _ => return None,
    })
}

fn analogous(from: Space, i: usize, to: Space) -> Option<usize> {
    let cat = category(from, i)?;
    (0..3).find(|&j| category(to, j) == Some(cat))
}

// ------------------------------------------------------------------ serialization

fn fmt(v: f64) -> String {
    if v.is_infinite() {
        return if v > 0.0 { "calc(infinity)".into() } else { "calc(-infinity)".into() };
    }
    calc::fmt_num(v)
}

fn round8(v: f64) -> u8 {
    // A tiny bias so 127.4999999… (from 0.5 computed in floating point) rounds like 127.5.
    (v.clamp(0.0, 1.0) * 255.0 + 1e-7).round() as u8
}

/// Legacy alpha: as Blink, the shortest of two or three decimals that maps
/// back to the same 8-bit value.
fn legacy_alpha(a: f64) -> String {
    let b = round8(a);
    let a2 = (b as f64 / 255.0 * 100.0).round() / 100.0;
    let v = if round8(a2) == b { a2 } else { (b as f64 / 255.0 * 1000.0).round() / 1000.0 };
    fmt(v)
}

fn ser_legacy_rgb(c: &Abs) -> String {
    let r = round8(c.c[0].unwrap_or(0.0));
    let g = round8(c.c[1].unwrap_or(0.0));
    let b = round8(c.c[2].unwrap_or(0.0));
    let a = c.a.unwrap_or(0.0);
    if round8(a) == 255 {
        format!("rgb({}, {}, {})", r, g, b)
    } else {
        format!("rgba({}, {}, {}, {})", r, g, b, legacy_alpha(a))
    }
}

fn opt(v: Option<f64>) -> String {
    match v {
        None => "none".into(),
        Some(x) => fmt(x),
    }
}

fn alpha_suffix(a: Option<f64>) -> String {
    match a {
        None => " / none".into(),
        Some(x) if x >= 1.0 => String::new(),
        Some(x) => format!(" / {}", fmt(x)),
    }
}

/// Serialize an absolute color.
pub fn ser_abs(c: &Abs) -> String {
    match c.space {
        Space::Srgb if c.legacy && c.c.iter().all(|v| v.is_some()) && c.a.is_some() => ser_legacy_rgb(c),
        Space::Hsl => {
            if c.c.iter().all(|v| v.is_some()) && c.a.is_some() && c.legacy {
                return ser_legacy_rgb(&convert(c, Space::Srgb));
            }
            format!("hsl({} {} {}{})", opt(c.c[0]), pct(c.c[1]), pct(c.c[2]), alpha_suffix(c.a))
        }
        Space::Hwb => {
            if c.c.iter().all(|v| v.is_some()) && c.a.is_some() && c.legacy {
                return ser_legacy_rgb(&convert(c, Space::Srgb));
            }
            format!("hwb({} {} {}{})", opt(c.c[0]), pct(c.c[1]), pct(c.c[2]), alpha_suffix(c.a))
        }
        Space::Lab | Space::Lch | Space::Oklab | Space::Oklch => format!("{}({} {} {}{})", c.space.name(), opt(c.c[0]), opt(c.c[1]), opt(c.c[2]), alpha_suffix(c.a)),
        sp => format!("color({} {} {} {}{})", sp.name(), opt(c.c[0]), opt(c.c[1]), opt(c.c[2]), alpha_suffix(c.a)),
    }
}

fn pct(v: Option<f64>) -> String {
    match v {
        None => "none".into(),
        Some(x) => format!("{}%", fmt(x)),
    }
}

/// Serialize a component as written (relative colors, unresolved math).
fn ser_comp(c: &Comp) -> String {
    match c {
        Comp::None => "none".into(),
        Comp::Num(v) => fmt(*v),
        Comp::Pct(v) => format!("{}%", fmt(*v)),
        Comp::Angle(d, u) => {
            let k = match u.as_str() {
                "grad" => 0.9,
                "rad" => 180.0 / std::f64::consts::PI,
                "turn" => 360.0,
                _ => 1.0,
            };
            format!("{}{}", fmt(d / k), u)
        }
        Comp::Ident(s) => s.clone(),
        Comp::Calc(n) => calc::serialize(&calc::simplify(n, &|_| None)),
    }
}

/// Specified-value serialization.
pub fn ser_specified(s: &Spec) -> String {
    match s {
        Spec::Abs(a) => ser_abs(a),
        Spec::Named(n, _) => n.clone(),
        Spec::Transparent => "transparent".into(),
        Spec::Current => "currentcolor".into(),
        Spec::System(n) => n.clone(),
        Spec::Raw(t) => t.clone(),
        Spec::LightDark(a, b) => format!("light-dark({}, {})", ser_specified(a), ser_specified(b)),
        Spec::Contrast(a) => format!("contrast-color({})", ser_specified(a)),
        Spec::Alpha(o, a) => match a {
            None => format!("alpha(from {})", ser_specified(o)),
            Some(a) => format!("alpha(from {} / {})", ser_specified(o), ser_comp(a)),
        },
        Spec::Func(f) => ser_func_spec(f),
        Spec::Mix(m) => {
            let pcts: Vec<Option<String>> = if m.items.len() == 2 {
                let (pa, pb) = mix_pcts_specified(&m.items[0].1, &m.items[1].1);
                vec![pa, pb]
            } else {
                let lits: Vec<Option<f64>> = m.items.iter().map(|(_, p)| if let Some(Comp::Pct(v)) = p { Some(*v) } else { None }).collect();
                let all_lit_or_none = m.items.iter().all(|(_, p)| p.is_none() || matches!(p, Some(Comp::Pct(_))));
                if m.items.len() == 1 && lits[0] == Some(100.0) {
                    vec![None]
                } else if all_lit_or_none && lits.iter().any(|p| p.is_some()) && lits.iter().any(|p| p.is_none()) {
                    // The omitted percentages share what is left.
                    let given: f64 = lits.iter().flatten().sum();
                    let missing = lits.iter().filter(|p| p.is_none()).count() as f64;
                    let fill = ((100.0 - given) / missing).max(0.0);
                    lits.iter().map(|p| Some(format!("{}%", fmt(p.unwrap_or(fill))))).collect()
                } else {
                    m.items.iter().map(|(_, p)| p.as_ref().map(ser_comp)).collect()
                }
            };
            let hue = match &m.hue {
                Some(h) if h != "shorter" => format!(" {} hue", h),
                _ => String::new(),
            };
            let items: Vec<String> = m
                .items
                .iter()
                .zip(pcts.iter())
                .map(|((c, _), p)| match p {
                    Some(p) => format!("{} {}", ser_specified(c), p),
                    None => ser_specified(c),
                })
                .collect();
            // `in oklab` is the default and is omitted.
            let head = if m.explicit && (m.space != Space::Oklab || !hue.is_empty()) { format!("in {}{}, ", m.space.name(), hue) } else { String::new() };
            format!("color-mix({}{})", head, items.join(", "))
        }
    }
}

/// color-mix() percentages as serialized: omitted when 50/50, the complement
/// filled in when only one is given.
fn mix_pcts_specified(pa: &Option<Comp>, pb: &Option<Comp>) -> (Option<String>, Option<String>) {
    let lit = |p: &Option<Comp>| match p {
        Some(Comp::Pct(v)) => Some(*v),
        _ => None,
    };
    match (pa, pb) {
        (None, None) => (None, None),
        (Some(a), None) => match lit(pa) {
            Some(50.0) => (None, None),
            Some(v) => (Some(format!("{}%", fmt(v))), Some(format!("{}%", fmt(100.0 - v)))),
            None => (Some(ser_comp(a)), None),
        },
        (None, Some(b)) => match lit(pb) {
            Some(50.0) => (None, None),
            Some(v) => (Some(format!("{}%", fmt(100.0 - v))), Some(format!("{}%", fmt(v)))),
            None => (None, Some(ser_comp(b))),
        },
        (Some(a), Some(b)) => {
            if lit(pa) == Some(50.0) && lit(pb) == Some(50.0) {
                (None, None)
            } else {
                (Some(ser_comp(a)), Some(ser_comp(b)))
            }
        }
    }
}

fn ser_func_spec(f: &FuncSpec) -> String {
    let name = f.name.as_str();
    let head = match &f.from {
        Some(o) => {
            if name == "color" {
                format!("color(from {} {} ", ser_specified(o), f.space.name())
            } else {
                format!("{}(from {} ", name, ser_specified(o))
            }
        }
        None => {
            if name == "color" {
                format!("color({} ", f.space.name())
            } else {
                format!("{}(", name)
            }
        }
    };
    let relative = f.from.is_some();
    let refs = pct_refs(name, f.space);
    let comps: Vec<String> = (0..3)
        .map(|i| {
            if relative {
                return ser_comp(&f.c[i]);
            }
            // Absolute with unresolved math: literal components canonicalized.
            canon_literal(name, i, &f.c[i], refs[i])
        })
        .collect();
    let alpha = match &f.alpha {
        None => String::new(),
        Some(a) => {
            if relative {
                format!(" / {}", ser_comp(a))
            } else {
                match a {
                    Comp::Num(v) => {
                        if nan0(*v) >= 1.0 {
                            String::new()
                        } else {
                            format!(" / {}", fmt(nan0(*v).clamp(0.0, 1.0)))
                        }
                    }
                    Comp::Pct(v) => {
                        let x = (v / 100.0).clamp(0.0, 1.0);
                        if x >= 1.0 {
                            String::new()
                        } else {
                            format!(" / {}", fmt(x))
                        }
                    }
                    other => format!(" / {}", ser_comp(other)),
                }
            }
        }
    };
    format!("{}{} {} {}{})", head, comps[0], comps[1], comps[2], alpha)
}

/// A literal component of an absolute function in specified form: percentages
/// converted to numbers, clamped as the function clamps.
fn canon_literal(name: &str, i: usize, c: &Comp, pct_ref: f64) -> String {
    match c {
        Comp::Calc(_) | Comp::Ident(_) | Comp::None => ser_comp(c),
        Comp::Angle(d, _) => fmt(norm_hue(*d)),
        Comp::Num(v) | Comp::Pct(v) => {
            let mut x = if matches!(c, Comp::Pct(_)) { v / 100.0 * pct_ref } else { *v };
            match (name, i) {
                ("rgb", _) => x = x.clamp(0.0, 255.0),
                ("lab", 0) | ("lch", 0) => x = x.clamp(0.0, 100.0),
                ("oklab", 0) | ("oklch", 0) => x = x.clamp(0.0, 1.0),
                ("lch", 1) | ("oklch", 1) => x = x.max(0.0),
                ("lch", 2) | ("oklch", 2) | ("hsl", 0) | ("hwb", 0) => x = norm_hue(x),
                ("hsl", _) | ("hwb", _) => return fmt(if matches!(c, Comp::Pct(_)) { *v } else { x }),
                _ => {}
            }
            fmt(x)
        }
    }
}

// ------------------------------------------------------------------ computed values

/// The computed color: absolute, or currentcolor-dependent (resolved with `current`).
pub fn compute(s: &Spec, current: &Abs, dark: bool) -> Option<Abs> {
    Some(match s {
        // Legacy hsl()/hwb() keep their channels (for mixing) and serialize as rgb().
        Spec::Abs(a) => a.clone(),
        Spec::Named(_, a) => a.clone(),
        Spec::Transparent => Abs::srgb(0.0, 0.0, 0.0, 0.0),
        Spec::Current => current.clone(),
        Spec::System(n) => system_color(n, dark),
        Spec::Raw(_) => return None,
        Spec::LightDark(a, b) => compute(if dark { b } else { a }, current, dark)?,
        Spec::Contrast(c) => {
            let base = compute(c, current, dark)?;
            let x = to_xyz65(base.space, [base.c[0].unwrap_or(0.0), base.c[1].unwrap_or(0.0), base.c[2].unwrap_or(0.0)]);
            let y = x[1].max(0.0);
            // WCAG contrast against white and black.
            let cw = 1.05 / (y + 0.05);
            let cb = (y + 0.05) / 0.05;
            if cw >= cb {
                Abs::srgb(1.0, 1.0, 1.0, 1.0)
            } else {
                Abs::srgb(0.0, 0.0, 0.0, 1.0)
            }
        }
        Spec::Alpha(o, a) => {
            let base = compute(o, current, dark)?;
            let mut out = base.clone();
            out.legacy = false;
            if out.space == Space::Hsl || out.space == Space::Hwb {
                out = convert(&out, Space::Srgb);
            }
            if let Some(a) = a {
                let ba = base.a;
                let bind = |s: &str| if s == "alpha" { Some(ba.unwrap_or(0.0)) } else { None };
                out.a = match a {
                    Comp::Ident(s) if s == "alpha" => ba,
                    Comp::Calc(Node::Opaque(_)) => return None,
                    _ => comp_value(a, 1.0, &bind)?.map(|v| nan0(v).clamp(0.0, 1.0)),
                };
            }
            out
        }
        Spec::Func(f) => {
            let origin = match &f.from {
                Some(o) => Some(compute(o, current, dark)?),
                None => None,
            };
            let mut r = resolve_func(f, origin.as_ref())?;
            if f.from.is_some() {
                // Relative rgb()/hsl()/hwb() compute to color(srgb …) (hsl()/hwb() with `none` keep their form).
                if matches!(r.space, Space::Hsl | Space::Hwb) && r.c.iter().all(|v| v.is_some()) && r.a.is_some() {
                    r = convert(&r, Space::Srgb);
                }
                r.legacy = false;
            }
            r
        }
        Spec::Mix(m) => mix(m, current, dark)?,
    })
}

fn system_color(n: &str, dark: bool) -> Abs {
    let hex: u32 = match n {
        "canvas" if dark => 0x121212,
        "field" if dark => 0x3b3b3b,
        "canvastext" | "fieldtext" | "buttontext" if dark => 0xffffff,
        "buttonface" if dark => 0x6b6b6b,
        "buttonborder" if dark => 0x6b6b6b,
        "linktext" if dark => 0x9e9eff,
        "visitedtext" if dark => 0xd0adf0,
        "graytext" if dark => 0x8e8e8e,
        "canvas" | "field" => 0xffffff,
        "canvastext" | "fieldtext" | "buttontext" => 0x000000,
        "linktext" => 0x0000ee,
        "visitedtext" => 0x551a8b,
        "activetext" => 0xff0000,
        "buttonface" => 0xefefef,
        "buttonborder" => 0x767676,
        "graytext" => 0x808080,
        "selecteditem" | "accentcolor" => 0x0075ff,
        "highlighttext" | "selecteditemtext" | "accentcolortext" => 0xffffff,
        "highlight" => 0xb4d5fe,
        "mark" => 0xffff00,
        "marktext" => 0x000000,
        _ => 0x000000,
    };
    Abs::srgb(((hex >> 16) & 255) as f64 / 255.0, ((hex >> 8) & 255) as f64 / 255.0, (hex & 255) as f64 / 255.0, 1.0)
}

fn mix(m: &MixSpec, current: &Abs, dark: bool) -> Option<Abs> {
    let lit = |p: &Option<Comp>| -> Option<Option<f64>> {
        Some(match p {
            None => None,
            Some(Comp::Pct(v)) => Some(*v / 100.0),
            Some(Comp::Calc(n)) => {
                let s = calc::simplify(n, &|_| None);
                match s {
                    Node::Dim(v, u) if u == "%" => Some((v / 100.0).clamp(0.0, 1.0)),
                    _ => return None,
                }
            }
            _ => return None,
        })
    };
    let mut colors = Vec::new();
    let mut ps = Vec::new();
    for (c, p) in &m.items {
        colors.push(compute(c, current, dark)?);
        ps.push(lit(p)?);
    }
    // Normalize the percentages (omitted ones share what is left).
    let given: f64 = ps.iter().flatten().sum();
    let missing = ps.iter().filter(|p| p.is_none()).count();
    let fill = if missing > 0 { ((1.0 - given) / missing as f64).max(0.0) } else { 0.0 };
    let mut w: Vec<f64> = ps.iter().map(|p| p.unwrap_or(fill)).collect();
    let total: f64 = w.iter().sum();
    let alpha_mult = if total < 1.0 { total } else { 1.0 };
    if total <= 0.0 {
        let n = w.len() as f64;
        w = w.iter().map(|_| 1.0 / n).collect();
    } else {
        w = w.iter().map(|x| x / total).collect();
    }
    // Successive pairwise mixing.
    let mut acc = convert(&colors[0], m.space);
    let mut acc_w = w[0];
    for i in 1..colors.len() {
        let next = convert(&colors[i], m.space);
        let t = if acc_w + w[i] > 0.0 { w[i] / (acc_w + w[i]) } else { 0.5 };
        acc = mix2(m, acc, next, 1.0 - t, t);
        acc_w += w[i];
    }
    if colors.len() == 1 {
        acc = convert(&colors[0], m.space);
    }
    if let Some(a) = acc.a {
        acc.a = Some(a * alpha_mult);
    }
    if let Some(hi) = m.space.hue_index() {
        acc.c[hi] = acc.c[hi].map(norm_hue);
    }
    // Legacy spaces: the result is reported in sRGB (unless something is missing).
    if matches!(m.space, Space::Hsl | Space::Hwb) && acc.c.iter().all(|v| v.is_some()) && acc.a.is_some() {
        let mut r = convert(&acc, Space::Srgb);
        r.legacy = false;
        return Some(r);
    }
    acc.legacy = false;
    Some(acc)
}

/// Mix two colors already in the interpolation space (premultiplied alpha).
fn mix2(m: &MixSpec, mut ca: Abs, mut cb: Abs, w1: f64, w2: f64) -> Abs {
    let space = m.space;
    for i in 0..3 {
        match (ca.c[i], cb.c[i]) {
            (None, Some(v)) => ca.c[i] = Some(v),
            (Some(v), None) => cb.c[i] = Some(v),
            _ => {}
        }
    }
    match (ca.a, cb.a) {
        (None, Some(v)) => ca.a = Some(v),
        (Some(v), None) => cb.a = Some(v),
        _ => {}
    }
    let hue_i = space.hue_index();
    if let Some(hi) = hue_i {
        if let (Some(h1), Some(h2)) = (ca.c[hi], cb.c[hi]) {
            let (mut h1, mut h2) = (norm_hue(h1), norm_hue(h2));
            let d = h2 - h1;
            match m.hue.as_deref().unwrap_or("shorter") {
                "shorter" => {
                    if d > 180.0 {
                        h1 += 360.0
                    } else if d < -180.0 {
                        h2 += 360.0
                    }
                }
                "longer" => {
                    if 0.0 < d && d < 180.0 {
                        h1 += 360.0
                    } else if -180.0 < d && d <= 0.0 {
                        h2 += 360.0
                    }
                }
                "increasing" => {
                    if d < 0.0 {
                        h2 += 360.0
                    }
                }
                _ => {
                    if d > 0.0 {
                        h1 += 360.0
                    }
                }
            }
            ca.c[hi] = Some(h1);
            cb.c[hi] = Some(h2);
        }
    }
    let a1 = ca.a.unwrap_or(1.0);
    let a2 = cb.a.unwrap_or(1.0);
    let alpha = a1 * w1 + a2 * w2;
    let mut out = Abs { space, c: [None; 3], a: if ca.a.is_none() && cb.a.is_none() { None } else { Some(alpha) }, legacy: false };
    for i in 0..3 {
        if let (Some(x), Some(y)) = (ca.c[i], cb.c[i]) {
            out.c[i] = Some(if Some(i) == hue_i {
                x * w1 + y * w2
            } else if alpha == 0.0 {
                0.0
            } else {
                (x * a1 * w1 + y * a2 * w2) / alpha
            });
        }
    }
    out
}

/// Computed-value serialization.
pub fn ser_computed(c: &Abs) -> String {
    let mut c = c.clone();
    if matches!(c.space, Space::Hsl | Space::Hwb) && c.c.iter().all(|v| v.is_some()) && c.a.is_some() {
        c = convert(&c, Space::Srgb);
        c.legacy = true;
    }
    ser_abs(&c)
}

/// Whether a color needs the element's currentcolor (or the color scheme) to resolve.
pub fn depends_on_current(s: &Spec) -> bool {
    match s {
        Spec::Current | Spec::System(_) | Spec::Raw(_) | Spec::LightDark(..) => true,
        Spec::Abs(_) | Spec::Named(..) | Spec::Transparent => false,
        Spec::Func(f) => f.from.as_ref().map(depends_on_current).unwrap_or(false),
        Spec::Mix(m) => m.items.iter().any(|(c, _)| depends_on_current(c)),
        Spec::Alpha(o, _) | Spec::Contrast(o) => depends_on_current(o),
    }
}

/// Convenience: specified serialization of a text value (None if invalid).
pub fn specified_text(s: &str) -> Option<String> {
    parse_str(s).map(|c| ser_specified(&c))
}

/// Convenience: computed serialization of a text value given currentcolor.
pub fn computed_text(s: &str, current: &Abs) -> Option<String> {
    let sp = parse_str(s)?;
    compute(&sp, current, false).map(|c| ser_computed(&c))
}

/// To 8-bit sRGB (clipped), for rendering.
pub fn to_rgba8(c: &Abs) -> [u8; 4] {
    let s = if c.space == Space::Srgb { c.clone() } else { convert(c, Space::Srgb) };
    [round8(s.c[0].unwrap_or(0.0)), round8(s.c[1].unwrap_or(0.0)), round8(s.c[2].unwrap_or(0.0)), round8(s.a.unwrap_or(0.0))]
}

fn is_system(n: &str) -> bool {
    matches!(
        n,
        "accentcolor" | "accentcolortext" | "activetext" | "buttonborder" | "buttonface" | "buttontext" | "canvas" | "canvastext" | "field" | "fieldtext" | "graytext" | "highlight" | "highlighttext" | "linktext" | "mark" | "marktext" | "selecteditem" | "selecteditemtext" | "visitedtext"
    )
}

fn named(n: &str) -> Option<Abs> {
    let hex: u32 = match n {
        "aliceblue" => 0xf0f8ff, "antiquewhite" => 0xfaebd7, "aqua" => 0x00ffff, "aquamarine" => 0x7fffd4,
        "azure" => 0xf0ffff, "beige" => 0xf5f5dc, "bisque" => 0xffe4c4, "black" => 0x000000,
        "blanchedalmond" => 0xffebcd, "blue" => 0x0000ff, "blueviolet" => 0x8a2be2, "brown" => 0xa52a2a,
        "burlywood" => 0xdeb887, "cadetblue" => 0x5f9ea0, "chartreuse" => 0x7fff00, "chocolate" => 0xd2691e,
        "coral" => 0xff7f50, "cornflowerblue" => 0x6495ed, "cornsilk" => 0xfff8dc, "crimson" => 0xdc143c,
        "cyan" => 0x00ffff, "darkblue" => 0x00008b, "darkcyan" => 0x008b8b, "darkgoldenrod" => 0xb8860b,
        "darkgray" | "darkgrey" => 0xa9a9a9, "darkgreen" => 0x006400, "darkkhaki" => 0xbdb76b,
        "darkmagenta" => 0x8b008b, "darkolivegreen" => 0x556b2f, "darkorange" => 0xff8c00, "darkorchid" => 0x9932cc,
        "darkred" => 0x8b0000, "darksalmon" => 0xe9967a, "darkseagreen" => 0x8fbc8f, "darkslateblue" => 0x483d8b,
        "darkslategray" | "darkslategrey" => 0x2f4f4f, "darkturquoise" => 0x00ced1, "darkviolet" => 0x9400d3,
        "deeppink" => 0xff1493, "deepskyblue" => 0x00bfff, "dimgray" | "dimgrey" => 0x696969,
        "dodgerblue" => 0x1e90ff, "firebrick" => 0xb22222, "floralwhite" => 0xfffaf0, "forestgreen" => 0x228b22,
        "fuchsia" => 0xff00ff, "gainsboro" => 0xdcdcdc, "ghostwhite" => 0xf8f8ff, "gold" => 0xffd700,
        "goldenrod" => 0xdaa520, "gray" | "grey" => 0x808080, "green" => 0x008000, "greenyellow" => 0xadff2f,
        "honeydew" => 0xf0fff0, "hotpink" => 0xff69b4, "indianred" => 0xcd5c5c, "indigo" => 0x4b0082,
        "ivory" => 0xfffff0, "khaki" => 0xf0e68c, "lavender" => 0xe6e6fa, "lavenderblush" => 0xfff0f5,
        "lawngreen" => 0x7cfc00, "lemonchiffon" => 0xfffacd, "lightblue" => 0xadd8e6, "lightcoral" => 0xf08080,
        "lightcyan" => 0xe0ffff, "lightgoldenrodyellow" => 0xfafad2, "lightgray" | "lightgrey" => 0xd3d3d3,
        "lightgreen" => 0x90ee90, "lightpink" => 0xffb6c1, "lightsalmon" => 0xffa07a, "lightseagreen" => 0x20b2aa,
        "lightskyblue" => 0x87cefa, "lightslategray" | "lightslategrey" => 0x778899, "lightsteelblue" => 0xb0c4de,
        "lightyellow" => 0xffffe0, "lime" => 0x00ff00, "limegreen" => 0x32cd32, "linen" => 0xfaf0e6,
        "magenta" => 0xff00ff, "maroon" => 0x800000, "mediumaquamarine" => 0x66cdaa, "mediumblue" => 0x0000cd,
        "mediumorchid" => 0xba55d3, "mediumpurple" => 0x9370db, "mediumseagreen" => 0x3cb371,
        "mediumslateblue" => 0x7b68ee, "mediumspringgreen" => 0x00fa9a, "mediumturquoise" => 0x48d1cc,
        "mediumvioletred" => 0xc71585, "midnightblue" => 0x191970, "mintcream" => 0xf5fffa, "mistyrose" => 0xffe4e1,
        "moccasin" => 0xffe4b5, "navajowhite" => 0xffdead, "navy" => 0x000080, "oldlace" => 0xfdf5e6,
        "olive" => 0x808000, "olivedrab" => 0x6b8e23, "orange" => 0xffa500, "orangered" => 0xff4500,
        "orchid" => 0xda70d6, "palegoldenrod" => 0xeee8aa, "palegreen" => 0x98fb98, "paleturquoise" => 0xafeeee,
        "palevioletred" => 0xdb7093, "papayawhip" => 0xffefd5, "peachpuff" => 0xffdab9, "peru" => 0xcd853f,
        "pink" => 0xffc0cb, "plum" => 0xdda0dd, "powderblue" => 0xb0e0e6, "purple" => 0x800080,
        "rebeccapurple" => 0x663399, "red" => 0xff0000, "rosybrown" => 0xbc8f8f, "royalblue" => 0x4169e1,
        "saddlebrown" => 0x8b4513, "salmon" => 0xfa8072, "sandybrown" => 0xf4a460, "seagreen" => 0x2e8b57,
        "seashell" => 0xfff5ee, "sienna" => 0xa0522d, "silver" => 0xc0c0c0, "skyblue" => 0x87ceeb,
        "slateblue" => 0x6a5acd, "slategray" | "slategrey" => 0x708090, "snow" => 0xfffafa,
        "springgreen" => 0x00ff7f, "steelblue" => 0x4682b4, "tan" => 0xd2b48c, "teal" => 0x008080,
        "thistle" => 0xd8bfd8, "tomato" => 0xff6347, "turquoise" => 0x40e0d0, "violet" => 0xee82ee,
        "wheat" => 0xf5deb3, "white" => 0xffffff, "whitesmoke" => 0xf5f5f5, "yellow" => 0xffff00,
        "yellowgreen" => 0x9acd32,
        _ => return None,
    };
    Some(Abs::srgb(((hex >> 16) & 255) as f64 / 255.0, ((hex >> 8) & 255) as f64 / 255.0, (hex & 255) as f64 / 255.0, 1.0))
}

/// Interpolate two computed colors for animation (CSS Color 4 §12): legacy
/// sRGB colors in sRGB (result legacy), others in Oklab; premultiplied alpha.
pub fn interpolate(a: &Abs, b: &Abs, p: f64) -> Abs {
    let legacy = a.legacy && b.legacy;
    let space = if legacy { Space::Srgb } else { Space::Oklab };
    let m = MixSpec { space, explicit: false, hue: None, items: vec![] };
    let (ca, cb) = (convert(a, space), convert(b, space));
    let mut r = mix2(&m, ca.clone(), cb.clone(), 1.0 - p, p);
    // Extrapolated alpha is clamped before un-premultiplying.
    let (a1, a2) = (ca.a.unwrap_or(1.0), cb.a.unwrap_or(1.0));
    let raw_alpha = a1 * (1.0 - p) + a2 * p;
    let alpha = raw_alpha.clamp(0.0, 1.0);
    if raw_alpha != alpha {
        let hue_i = if space.is_polar() { space.hue_index() } else { None };
        for i in 0..3 {
            if Some(i) == hue_i {
                continue;
            }
            if let (Some(x), Some(y)) = (ca.c[i].or(cb.c[i]), cb.c[i].or(ca.c[i])) {
                r.c[i] = Some(if alpha == 0.0 { 0.0 } else { (x * a1 * (1.0 - p) + y * a2 * p) / alpha });
            }
        }
    }
    // (Snap values a rounding error away from fully opaque / transparent.)
    let alpha = if (alpha - 1.0).abs() < 1e-9 { 1.0 } else if alpha.abs() < 1e-9 { 0.0 } else { alpha };
    r.a = Some(alpha);
    // Extrapolated lightness and chroma stay in range.
    match space {
        Space::Oklab | Space::Oklch => r.c[0] = r.c[0].map(|v| v.clamp(0.0, 1.0)),
        Space::Lab | Space::Lch => r.c[0] = r.c[0].map(|v| v.clamp(0.0, 100.0)),
        _ => {}
    }
    if space == Space::Oklch || space == Space::Lch {
        r.c[1] = r.c[1].map(|v| v.max(0.0));
    }
    if legacy {
        for c in r.c.iter_mut() {
            *c = c.map(|v| v.clamp(0.0, 1.0));
        }
        r.legacy = true;
    }
    r
}

/// Color addition (composite add): premultiplied sRGB components summed,
/// alpha clamped; channels are left unclamped (for further interpolation).
pub fn add(u: &Abs, v: &Abs) -> Abs {
    let (x, y) = (convert(u, Space::Srgb), convert(v, Space::Srgb));
    let (a1, a2) = (x.a.unwrap_or(1.0), y.a.unwrap_or(1.0));
    let alpha = (a1 + a2).min(1.0);
    let mut r = Abs { space: Space::Srgb, c: [None; 3], a: Some(alpha), legacy: u.legacy && v.legacy };
    for i in 0..3 {
        let s = x.c[i].unwrap_or(0.0) * a1 + y.c[i].unwrap_or(0.0) * a2;
        r.c[i] = Some(if alpha > 0.0 { s / alpha } else { 0.0 });
    }
    r
}
