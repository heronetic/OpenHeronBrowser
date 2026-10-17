// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! CSS value types and parsers: colors, lengths, calc(), angles.

use super::tokenizer::{Tok, CV};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color { r: 0, g: 0, b: 0, a: 0 };
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0, a: 255 };
    pub const WHITE: Color = Color { r: 255, g: 255, b: 255, a: 255 };
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color { r, g, b, a }
    }
    pub fn from_hex(h: u32) -> Color {
        Color::rgb((h >> 16) as u8, (h >> 8) as u8, h as u8)
    }
    pub fn with_alpha_mul(self, f: f32) -> Color {
        Color { a: (self.a as f32 * f).round().clamp(0.0, 255.0) as u8, ..self }
    }
    pub fn to_css(&self) -> String {
        if self.a == 255 {
            format!("rgb({}, {}, {})", self.r, self.g, self.b)
        } else {
            // Like Blink: the shortest of 2 or 3 decimals that maps back to the same byte.
            let a2 = (self.a as f32 / 255.0 * 100.0).round() / 100.0;
            let a = if (a2 * 255.0).round() as u8 == self.a { a2 } else { (self.a as f32 / 255.0 * 1000.0).round() / 1000.0 };
            format!("rgba({}, {}, {}, {})", self.r, self.g, self.b, a)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorValue {
    Rgba(Color),
    CurrentColor,
    /// A `color-mix()` that involves currentColor, resolved at use time.
    /// The index points into the thread's mix table (see `intern_mix`).
    Mix(u32),
}

impl ColorValue {
    pub fn resolve(&self, current: Color) -> Color {
        match self {
            ColorValue::Rgba(c) => *c,
            ColorValue::CurrentColor => current,
            ColorValue::Mix(i) => match MIXES.with(|t| t.borrow().0.get(*i as usize).copied()) {
                Some(m) => mix_colors_hue(m.space, m.hue, m.c1.resolve(current), m.w1, m.c2.resolve(current), m.w2, m.alpha_mul),
                None => current,
            },
        }
    }
}

/// Interpolation color space of `color-mix()`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MixSpace {
    Srgb,
    SrgbLinear,
    Oklab,
    Oklch,
    Lab,
    Lch,
    Hsl,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ColorMix {
    space: MixSpace,
    /// Hue interpolation: 0 shorter, 1 longer, 2 increasing, 3 decreasing.
    hue: u8,
    c1: ColorValue,
    w1: f32,
    c2: ColorValue,
    w2: f32,
    alpha_mul: f32,
}

thread_local! {
    static MIXES: std::cell::RefCell<(Vec<ColorMix>, std::collections::HashMap<String, u32>)> = Default::default();
}

fn intern_mix(m: ColorMix) -> ColorValue {
    MIXES.with(|t| {
        let mut t = t.borrow_mut();
        let key = format!("{m:?}");
        if let Some(&i) = t.1.get(&key) {
            return ColorValue::Mix(i);
        }
        if t.0.len() >= 100_000 {
            return ColorValue::Rgba(mix_colors_hue(m.space, m.hue, m.c1.resolve(Color::BLACK), m.w1, m.c2.resolve(Color::BLACK), m.w2, m.alpha_mul));
        }
        let i = t.0.len() as u32;
        t.0.push(m);
        t.1.insert(key, i);
        ColorValue::Mix(i)
    })
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn rgb_to_oklab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (r, g, b) = (srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    (
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    )
}

fn rgb_to_lab(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (r, g, b) = (srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b));
    // linear sRGB -> XYZ (D65) -> Bradford D50.
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175 * b;
    let z = 0.019_333_9 * r + 0.119_192 * g + 0.950_304_1 * b;
    let x5 = 1.047_811_2 * x + 0.022_886_6 * y - 0.050_127 * z;
    let y5 = 0.029_542_4 * x + 0.990_484_4 * y - 0.017_049_1 * z;
    let z5 = -0.009_234_5 * x + 0.015_043_6 * y + 0.752_131_6 * z;
    let e = 216.0 / 24389.0;
    let k = 24389.0 / 27.0;
    let f = |t: f32| if t > e { t.cbrt() } else { (k * t + 16.0) / 116.0 };
    let (fx, fy, fz) = (f(x5 / 0.964_22), f(y5), f(z5 / 0.825_21));
    (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let l = (mx + mn) / 2.0;
    let d = mx - mn;
    if d < 1e-6 {
        return (f32::NAN, 0.0, l);
    }
    let s = if l > 0.5 { d / (2.0 - mx - mn) } else { d / (mx + mn) };
    let h = if mx == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

/// Mix two colors as CSS Color 5 `color-mix()` does: convert to the
/// interpolation space, premultiply alpha (hue is never premultiplied),
/// interpolate (hues along the shorter arc; a powerless hue takes the other
/// color's), un-premultiply and convert back to sRGB.
fn mix_colors(space: MixSpace, a: Color, wa: f32, b: Color, wb: f32, alpha_mul: f32) -> Color {
    mix_colors_hue(space, 0, a, wa, b, wb, alpha_mul)
}

fn mix_colors_hue(space: MixSpace, hue_method: u8, a: Color, wa: f32, b: Color, wb: f32, alpha_mul: f32) -> Color {
    let f = |c: Color| (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0, c.a as f32 / 255.0);
    let (ar, ag, ab, aa) = f(a);
    let (br, bg, bb, ba) = f(b);
    // (components, hue index if polar)
    let to = |r: f32, g: f32, b: f32| -> ([f32; 3], Option<usize>) {
        match space {
            MixSpace::Srgb => ([r, g, b], None),
            MixSpace::SrgbLinear => ([srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b)], None),
            MixSpace::Oklab => {
                let (l, a, b) = rgb_to_oklab(r, g, b);
                ([l, a, b], None)
            }
            MixSpace::Lab => {
                let (l, a, b) = rgb_to_lab(r, g, b);
                ([l, a, b], None)
            }
            MixSpace::Oklch | MixSpace::Lch => {
                let (l, a, b) = if space == MixSpace::Oklch { rgb_to_oklab(r, g, b) } else { rgb_to_lab(r, g, b) };
                let c = (a * a + b * b).sqrt();
                let powerless = if space == MixSpace::Oklch { c < 2e-4 } else { c < 0.015 };
                let h = if powerless { f32::NAN } else { b.atan2(a).to_degrees().rem_euclid(360.0) };
                ([l, c, h], Some(2))
            }
            MixSpace::Hsl => {
                let (h, s, l) = rgb_to_hsl(r, g, b);
                ([h, s, l], Some(0))
            }
        }
    };
    let (mut ca, hue) = to(ar, ag, ab);
    let (mut cb, _) = to(br, bg, bb);
    if let Some(h) = hue {
        if ca[h].is_nan() {
            ca[h] = if cb[h].is_nan() { 0.0 } else { cb[h] };
        }
        if cb[h].is_nan() {
            cb[h] = ca[h];
        }
        let d = cb[h] - ca[h];
        match hue_method {
            1 => {
                if d > 0.0 && d < 180.0 {
                    ca[h] += 360.0;
                } else if d > -180.0 && d <= 0.0 {
                    cb[h] += 360.0;
                }
            }
            2 => {
                if d < 0.0 {
                    cb[h] += 360.0;
                }
            }
            3 => {
                if d > 0.0 {
                    ca[h] += 360.0;
                }
            }
            _ => {
                if d > 180.0 {
                    ca[h] += 360.0;
                } else if d < -180.0 {
                    cb[h] += 360.0;
                }
            }
        }
    }
    let alpha = aa * wa + ba * wb;
    let mut out = [0.0f32; 3];
    for i in 0..3 {
        out[i] = if Some(i) == hue {
            (ca[i] * wa + cb[i] * wb).rem_euclid(360.0)
        } else if alpha > 0.0 {
            (ca[i] * aa * wa + cb[i] * ba * wb) / alpha
        } else {
            ca[i] * wa + cb[i] * wb
        };
    }
    let (r, g, b) = match space {
        MixSpace::Srgb => (out[0], out[1], out[2]),
        MixSpace::SrgbLinear => (linear_to_srgb(out[0]), linear_to_srgb(out[1]), linear_to_srgb(out[2])),
        MixSpace::Oklab => oklab_to_rgb(out[0], out[1], out[2]),
        MixSpace::Lab => lab_to_rgb(out[0], out[1], out[2]),
        MixSpace::Oklch | MixSpace::Lch => {
            let (h, c) = (out[2].to_radians(), out[1]);
            if space == MixSpace::Oklch {
                oklab_to_rgb(out[0], c * h.cos(), c * h.sin())
            } else {
                lab_to_rgb(out[0], c * h.cos(), c * h.sin())
            }
        }
        MixSpace::Hsl => hsl_to_rgb(out[0], out[1], out[2]),
    };
    Color::rgba(to_u8(r * 255.0), to_u8(g * 255.0), to_u8(b * 255.0), to_u8(alpha * alpha_mul * 255.0))
}

/// A color space keyword (as used by `color-mix()` and gradients' `in …`).
pub fn mix_space(name: &str) -> Option<MixSpace> {
    Some(match name {
        "srgb" | "display-p3" | "a98-rgb" | "prophoto-rgb" | "rec2020" => MixSpace::Srgb,
        "srgb-linear" | "xyz" | "xyz-d65" | "xyz-d50" => MixSpace::SrgbLinear,
        "oklab" => MixSpace::Oklab,
        "oklch" => MixSpace::Oklch,
        "lab" => MixSpace::Lab,
        "lch" => MixSpace::Lch,
        "hsl" | "hwb" => MixSpace::Hsl,
        _ => return None,
    })
}

/// Color at `t` between `a` and `b`, interpolated with premultiplied alpha in `space`.
pub fn interpolate_color(space: MixSpace, a: Color, b: Color, t: f32) -> Color {
    mix_colors(space, a, 1.0 - t, b, t, 1.0)
}

/// As `interpolate_color`, with a hue interpolation method (0 shorter, 1 longer, 2 increasing, 3 decreasing).
pub fn interpolate_color_hue(space: MixSpace, hue: u8, a: Color, b: Color, t: f32) -> Color {
    mix_colors_hue(space, hue, a, 1.0 - t, b, t, 1.0)
}

/// `<hue-interpolation-method>` keyword (before `hue`).
pub fn hue_method(name: &str) -> Option<u8> {
    Some(match name {
        "shorter" => 0,
        "longer" => 1,
        "increasing" => 2,
        "decreasing" => 3,
        _ => return None,
    })
}

/// Parse `color-mix(in <space> [<hue-method>], <color> [<pct>], <color> [<pct>])`.
fn parse_color_mix(args: &[CV]) -> Option<ColorValue> {
    let parts = super::tokenizer::split_commas(args);
    let mut hue = 0u8;
    let (space, colors) = match parts.len() {
        3 => {
            let w: Vec<&CV> = parts[0].iter().filter(|c| !c.is_ws()).collect();
            if !w.first()?.is_ident("in") {
                return None;
            }
            let space = mix_space(&w.get(1)?.ident()?.to_ascii_lowercase())?;
            hue = w.get(2).and_then(|c| c.ident()).and_then(|n| hue_method(&n.to_ascii_lowercase())).unwrap_or(0);
            (space, [parts[1], parts[2]])
        }
        2 => (MixSpace::Oklab, [parts[0], parts[1]]),
        _ => return None,
    };
    let part = |p: &[CV]| -> Option<(ColorValue, Option<f32>)> {
        let mut col = None;
        let mut pct = None;
        for c in p.iter().filter(|c| !c.is_ws()) {
            match c {
                CV::T(Tok::Percentage(x)) => pct = Some((*x / 100.0) as f32),
                CV::Func(n, _) if is_math_fn(n) => pct = Some(parse_calc_fn(n, match c { CV::Func(_, a) => a, _ => &[] }, &UnitCtx::new(16.0, 16.0, 0.0, 0.0))?.eval(100.0) / 100.0),
                _ => col = Some(parse_color(c)?),
            }
        }
        Some((col?, pct))
    };
    let (c1, p1) = part(colors[0])?;
    let (c2, p2) = part(colors[1])?;
    let (w1, w2) = match (p1, p2) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => (a, 1.0 - a),
        (None, Some(b)) => (1.0 - b, b),
        (None, None) => (0.5, 0.5),
    };
    if w1 < 0.0 || w2 < 0.0 || w1 + w2 <= 0.0 {
        return None;
    }
    let sum = w1 + w2;
    let m = ColorMix { space, hue, c1, w1: w1 / sum, c2, w2: w2 / sum, alpha_mul: sum.min(1.0) };
    match (c1, c2) {
        (ColorValue::Rgba(a), ColorValue::Rgba(b)) => Some(ColorValue::Rgba(mix_colors_hue(space, m.hue, a, m.w1, b, m.w2, m.alpha_mul))),
        _ => Some(intern_mix(m)),
    }
}

/// Context for resolving relative units.
#[derive(Clone, Debug)]
pub struct UnitCtx {
    pub em: f32,
    pub rem: f32,
    pub vw: f32,
    pub vh: f32,
    /// The element's font (family list, weight, italic) for ex/ch/cap/ic/lh,
    /// resolved lazily against the system fonts; None = ratios of `em`.
    pub font: Option<(Rc<Vec<String>>, u16, bool)>,
    /// The element's line height in px if not `normal`.
    pub line_height: Option<f32>,
    /// The root element's line height in px (`rlh`), 0 = 1.2rem.
    pub rlh: f32,
    /// Query container's content size for cq* units (None = the viewport).
    pub cq: Option<(f32, f32)>,
}

impl UnitCtx {
    pub fn new(em: f32, rem: f32, vw: f32, vh: f32) -> UnitCtx {
        UnitCtx { em, rem, vw, vh, font: None, line_height: None, rlh: 0.0, cq: None }
    }
    /// ex, ch, cap, ic and normal line height for `em`, in px.
    fn font_metrics(&self) -> Option<(f32, f32, f32, f32, f32)> {
        let (fam, w, it) = self.font.as_ref()?;
        let st = if *it { crate::css::style::FontStyle::Italic } else { crate::css::style::FontStyle::Normal };
        let f = crate::text::system_primary(fam, *w, st);
        let (ex, ch, cap, ic) = f.unit_ratios();
        Some((ex * self.em, ch * self.em, cap * self.em, ic * self.em, f.normal_line_height(self.em)))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CalcNode {
    /// px + pct% (+ number for unitless parts)
    Lin { px: f32, pct: f32, num: f32 },
    Min(Vec<CalcNode>),
    Max(Vec<CalcNode>),
    Clamp(Box<CalcNode>, Box<CalcNode>, Box<CalcNode>),
    Sum(Vec<CalcNode>),
    Scale(Box<CalcNode>, f32),
}

impl CalcNode {
    pub fn eval(&self, basis: f32) -> f32 {
        match self {
            CalcNode::Lin { px, pct, num } => px + pct * basis / 100.0 + num,
            CalcNode::Min(v) => v.iter().map(|n| n.eval(basis)).fold(f32::INFINITY, f32::min),
            CalcNode::Max(v) => v.iter().map(|n| n.eval(basis)).fold(f32::NEG_INFINITY, f32::max),
            CalcNode::Clamp(a, b, c) => {
                let lo = a.eval(basis);
                let v = b.eval(basis);
                let hi = c.eval(basis);
                v.min(hi).max(lo)
            }
            CalcNode::Sum(v) => v.iter().map(|n| n.eval(basis)).sum(),
            CalcNode::Scale(n, f) => n.eval(basis) * f,
        }
    }
    fn has_pct(&self) -> bool {
        match self {
            CalcNode::Lin { pct, .. } => *pct != 0.0,
            CalcNode::Min(v) | CalcNode::Max(v) | CalcNode::Sum(v) => v.iter().any(|n| n.has_pct()),
            CalcNode::Clamp(a, b, c) => a.has_pct() || b.has_pct() || c.has_pct(),
            CalcNode::Scale(n, _) => n.has_pct(),
        }
    }
    fn as_lin(&self) -> Option<(f32, f32, f32)> {
        match self {
            CalcNode::Lin { px, pct, num } => Some((*px, *pct, *num)),
            _ => None,
        }
    }
    fn is_pure_number(&self) -> bool {
        matches!(self, CalcNode::Lin { px, pct, .. } if *px == 0.0 && *pct == 0.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Len {
    Auto,
    Px(f32),
    Pct(f32),
    Calc(f32, f32),
    Expr(Rc<CalcNode>),
    None,
    MinContent,
    MaxContent,
    FitContent,
}

impl Default for Len {
    fn default() -> Self {
        Len::Auto
    }
}

impl Len {
    pub fn zero() -> Len {
        Len::Px(0.0)
    }
    pub fn is_auto(&self) -> bool {
        matches!(self, Len::Auto)
    }
    pub fn is_none(&self) -> bool {
        matches!(self, Len::None)
    }
    pub fn is_intrinsic(&self) -> bool {
        matches!(self, Len::MinContent | Len::MaxContent | Len::FitContent)
    }
    pub fn has_pct(&self) -> bool {
        match self {
            Len::Pct(_) => true,
            Len::Calc(_, p) => *p != 0.0,
            Len::Expr(e) => e.has_pct(),
            _ => false,
        }
    }
    /// Resolve against a basis; auto/none resolve to 0.
    pub fn resolve(&self, basis: f32) -> f32 {
        match self {
            Len::Px(v) => *v,
            Len::Pct(p) => p * basis / 100.0,
            Len::Calc(px, p) => px + p * basis / 100.0,
            Len::Expr(e) => e.eval(basis),
            _ => 0.0,
        }
    }
    /// Resolve where percentages against an indefinite basis mean "auto".
    pub fn definite(&self, basis: Option<f32>) -> Option<f32> {
        match self {
            Len::Px(v) => Some(*v),
            Len::Auto | Len::None | Len::MinContent | Len::MaxContent | Len::FitContent => None,
            _ => {
                if self.has_pct() {
                    basis.map(|b| self.resolve(b))
                } else {
                    Some(self.resolve(0.0))
                }
            }
        }
    }
    pub fn to_css(&self) -> String {
        match self {
            Len::Auto => "auto".into(),
            Len::Px(v) => format!("{}px", fmt(*v)),
            Len::Pct(p) => format!("{}%", fmt(*p)),
            Len::Calc(px, p) if *px < 0.0 => format!("calc({}% - {}px)", fmt(*p), fmt(-*px)),
            Len::Calc(px, p) => format!("calc({}% + {}px)", fmt(*p), fmt(*px)),
            Len::Expr(_) => "calc()".into(),
            Len::None => "none".into(),
            Len::MinContent => "min-content".into(),
            Len::MaxContent => "max-content".into(),
            Len::FitContent => "fit-content".into(),
        }
    }
}

pub fn fmt(v: f32) -> String {
    if (v - v.round()).abs() < 0.005 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{:.2}", v);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

pub fn unit_to_px(n: f32, unit: &str, ctx: &UnitCtx) -> Option<f32> {
    Some(match unit {
        "px" => n,
        "em" => n * ctx.em,
        "rem" => n * ctx.rem,
        "ex" => n * ctx.font_metrics().map(|m| m.0).unwrap_or(ctx.em * 0.5),
        "ch" => n * ctx.font_metrics().map(|m| m.1).unwrap_or(ctx.em * 0.5),
        "cap" => n * ctx.font_metrics().map(|m| m.2).unwrap_or(ctx.em * 0.7),
        "ic" => n * ctx.font_metrics().map(|m| m.3).unwrap_or(ctx.em),
        "lh" => n * ctx.line_height.or_else(|| ctx.font_metrics().map(|m| m.4)).unwrap_or(ctx.em * 1.2),
        "rlh" => n * if ctx.rlh > 0.0 { ctx.rlh } else { ctx.rem * 1.2 },
        "rex" => n * ctx.rem * 0.5,
        "rch" => n * ctx.rem * 0.5,
        "rcap" => n * ctx.rem * 0.7,
        "ric" => n * ctx.rem,
        "vw" | "svw" | "lvw" | "dvw" | "vi" | "svi" | "lvi" | "dvi" => n * ctx.vw / 100.0,
        "vh" | "svh" | "lvh" | "dvh" | "vb" | "svb" | "lvb" | "dvb" => n * ctx.vh / 100.0,
        "vmin" | "svmin" | "lvmin" | "dvmin" => n * ctx.vw.min(ctx.vh) / 100.0,
        "vmax" | "svmax" | "lvmax" | "dvmax" => n * ctx.vw.max(ctx.vh) / 100.0,
        "cqw" | "cqi" => n * ctx.cq.map(|c| c.0).unwrap_or(ctx.vw) / 100.0,
        "cqh" | "cqb" => n * ctx.cq.map(|c| c.1).unwrap_or(ctx.vh) / 100.0,
        "cqmin" => n * ctx.cq.map(|c| c.0.min(c.1)).unwrap_or(ctx.vw.min(ctx.vh)) / 100.0,
        "cqmax" => n * ctx.cq.map(|c| c.0.max(c.1)).unwrap_or(ctx.vw.max(ctx.vh)) / 100.0,
        "pt" => n * 96.0 / 72.0,
        "pc" => n * 16.0,
        "in" => n * 96.0,
        "cm" => n * 96.0 / 2.54,
        "mm" => n * 96.0 / 25.4,
        "q" => n * 96.0 / 101.6,
        _ => return None,
    })
}

/// Parse a single component value as a length (or percentage).
pub fn parse_len(cv: &CV, ctx: &UnitCtx) -> Option<Len> {
    match cv {
        CV::T(Tok::Dimension(n, u)) => unit_to_px(*n as f32, u, ctx).map(Len::Px),
        CV::T(Tok::Percentage(p)) => Some(Len::Pct(*p as f32)),
        CV::T(Tok::Number(n)) if *n == 0.0 => Some(Len::Px(0.0)),
        CV::T(Tok::Ident(i)) => match i.to_ascii_lowercase().as_str() {
            "auto" => Some(Len::Auto),
            "none" => Some(Len::None),
            "min-content" => Some(Len::MinContent),
            "max-content" => Some(Len::MaxContent),
            "fit-content" | "-webkit-fill-available" | "-moz-available" | "stretch" | "-webkit-fit-content" => Some(Len::FitContent),
            _ => None,
        },
        CV::Func(name, args) if is_math_fn(name) => {
            let node = parse_calc_fn(name, args, ctx)?;
            Some(node_to_len(node))
        }
        CV::Func(name, _) if name == "fit-content" => Some(Len::FitContent),
        _ => None,
    }
}

fn node_to_len(node: CalcNode) -> Len {
    if let Some((px, pct, num)) = node.as_lin() {
        let px = px + num;
        if pct == 0.0 {
            return Len::Px(px);
        }
        if px == 0.0 {
            return Len::Pct(pct);
        }
        return Len::Calc(px, pct);
    }
    if !node.has_pct() {
        return Len::Px(node.eval(0.0));
    }
    Len::Expr(Rc::new(node))
}

pub fn is_math_fn(name: &str) -> bool {
    matches!(
        name,
        "calc" | "min" | "max" | "clamp" | "-webkit-calc" | "-moz-calc" | "round" | "mod" | "rem" | "abs" | "sign" | "sin" | "cos"
            | "tan" | "asin" | "acos" | "atan" | "atan2" | "pow" | "sqrt" | "hypot" | "log" | "exp"
    )
}

/// Evaluate the comma-separated arguments of a math function to resolved
/// values; `None` if any argument depends on a percentage basis. The flag
/// says whether every argument is a plain number.
fn math_args(args: &[CV], ctx: &UnitCtx) -> Option<(Vec<f32>, bool)> {
    let mut out = Vec::new();
    let mut all_num = true;
    for part in super::tokenizer::split_commas(args) {
        let toks: Vec<&CV> = part.iter().filter(|c| !c.is_ws()).collect();
        let mut i = 0;
        let n = calc_sum(&toks, &mut i, ctx)?;
        if i != toks.len() || n.has_pct() {
            return None;
        }
        all_num &= n.is_pure_number();
        out.push(n.eval(0.0));
    }
    Some((out, all_num))
}

/// CSS Values 4 math functions other than calc/min/max/clamp. Angles live in
/// the px slot as degrees (see `calc_value`), so trig takes and returns degrees
/// there, and a plain number argument is in radians.
fn math_fn(name: &str, args: &[CV], ctx: &UnitCtx) -> Option<CalcNode> {
    let number = |v: f32| CalcNode::Lin { px: 0.0, pct: 0.0, num: v };
    let typed = |v: f32, num: bool| if num { number(v) } else { CalcNode::Lin { px: v, pct: 0.0, num: 0.0 } };
    if name == "round" {
        // round(<strategy>?, A, B?)
        let parts = super::tokenizer::split_commas(args);
        let (strategy, rest): (&str, Vec<CV>) = match parts.first().map(|p| p.iter().filter(|c| !c.is_ws()).collect::<Vec<_>>()) {
            Some(first) if first.len() == 1 && first[0].ident().is_some() && !first[0].is_ident("pi") && !first[0].is_ident("e") => {
                let s = first[0].ident().unwrap().to_ascii_lowercase();
                let strat: &str = match s.as_str() {
                    "up" => "up",
                    "down" => "down",
                    "to-zero" => "to-zero",
                    "nearest" => "nearest",
                    _ => return None,
                };
                let mut rest = Vec::new();
                for (k, p) in parts.iter().enumerate().skip(1) {
                    if k > 1 {
                        rest.push(CV::T(Tok::Comma));
                    }
                    rest.extend(p.iter().cloned());
                }
                (strat, rest)
            }
            _ => ("nearest", args.to_vec()),
        };
        let (v, num) = math_args(&rest, ctx)?;
        let a = *v.first()?;
        let b = v.get(1).copied().unwrap_or(if num { 1.0 } else { 1.0 });
        if b == 0.0 {
            return None;
        }
        let q = a / b;
        let r = match strategy {
            "up" => q.ceil(),
            "down" => q.floor(),
            "to-zero" => q.trunc(),
            _ => {
                // Ties round up (towards +infinity), as in CSS.
                (q + 0.5).floor()
            }
        } * b;
        return Some(typed(r, num));
    }
    let (v, num) = math_args(args, ctx)?;
    let a = *v.first()?;
    let rad = |x: f32| if num { x } else { x.to_radians() };
    Some(match name {
        "mod" | "rem" => {
            let b = *v.get(1)?;
            if b == 0.0 {
                return None;
            }
            let r = if name == "mod" { a - b * (a / b).floor() } else { a - b * (a / b).trunc() };
            typed(r, num)
        }
        "abs" => typed(a.abs(), num),
        "sign" => number(if a > 0.0 { 1.0 } else if a < 0.0 { -1.0 } else { 0.0 }),
        "sin" => number(rad(a).sin()),
        "cos" => number(rad(a).cos()),
        "tan" => number(rad(a).tan()),
        "asin" => typed(a.asin().to_degrees(), false),
        "acos" => typed(a.acos().to_degrees(), false),
        "atan" => typed(a.atan().to_degrees(), false),
        "atan2" => typed(a.atan2(*v.get(1)?).to_degrees(), false),
        "pow" => number(a.powf(*v.get(1)?)),
        "sqrt" => number(a.sqrt()),
        "hypot" => typed(v.iter().map(|x| x * x).sum::<f32>().sqrt(), num),
        "log" => number(match v.get(1) {
            Some(b) => a.ln() / b.ln(),
            None => a.ln(),
        }),
        "exp" => number(a.exp()),
        _ => return None,
    })
}

pub fn parse_calc_fn(name: &str, args: &[CV], ctx: &UnitCtx) -> Option<CalcNode> {
    match name {
        "calc" | "-webkit-calc" | "-moz-calc" => {
            let toks: Vec<&CV> = args.iter().filter(|c| !c.is_ws()).collect();
            let mut i = 0;
            let n = calc_sum(&toks, &mut i, ctx)?;
            if i != toks.len() {
                return None;
            }
            Some(n)
        }
        "min" | "max" => {
            let mut v = Vec::new();
            for part in super::tokenizer::split_commas(args) {
                let toks: Vec<&CV> = part.iter().filter(|c| !c.is_ws()).collect();
                let mut i = 0;
                v.push(calc_sum(&toks, &mut i, ctx)?);
            }
            if v.is_empty() {
                return None;
            }
            // Fold to linear if possible.
            if v.iter().all(|n| !n.has_pct()) {
                let vals = v.iter().map(|n| n.eval(0.0));
                let r = if name == "min" { vals.fold(f32::INFINITY, f32::min) } else { vals.fold(f32::NEG_INFINITY, f32::max) };
                return Some(CalcNode::Lin { px: r, pct: 0.0, num: 0.0 });
            }
            Some(if name == "min" { CalcNode::Min(v) } else { CalcNode::Max(v) })
        }
        "clamp" => {
            let parts = super::tokenizer::split_commas(args);
            if parts.len() != 3 {
                return None;
            }
            let mut v = Vec::new();
            for part in parts {
                let toks: Vec<&CV> = part.iter().filter(|c| !c.is_ws()).collect();
                let mut i = 0;
                if toks.len() == 1 && toks[0].is_ident("none") {
                    v.push(CalcNode::Lin { px: f32::NEG_INFINITY, pct: 0.0, num: 0.0 });
                    continue;
                }
                v.push(calc_sum(&toks, &mut i, ctx)?);
            }
            let c = v.pop()?;
            let b = v.pop()?;
            let a = v.pop()?;
            if !a.has_pct() && !b.has_pct() && !c.has_pct() {
                let r = b.eval(0.0).min(c.eval(0.0)).max(a.eval(0.0));
                return Some(CalcNode::Lin { px: r, pct: 0.0, num: 0.0 });
            }
            Some(CalcNode::Clamp(Box::new(a), Box::new(b), Box::new(c)))
        }
        other => math_fn(other, args, ctx),
    }
}

fn add(a: CalcNode, b: CalcNode, sign: f32) -> CalcNode {
    match (a.as_lin(), b.as_lin()) {
        (Some((ax, ap, an)), Some((bx, bp, bn))) => CalcNode::Lin { px: ax + sign * bx, pct: ap + sign * bp, num: an + sign * bn },
        _ => {
            let b = if sign < 0.0 { CalcNode::Scale(Box::new(b), -1.0) } else { b };
            CalcNode::Sum(vec![a, b])
        }
    }
}

fn scale(a: CalcNode, f: f32) -> CalcNode {
    match a.as_lin() {
        Some((x, p, n)) => CalcNode::Lin { px: x * f, pct: p * f, num: n * f },
        None => CalcNode::Scale(Box::new(a), f),
    }
}

fn calc_sum(t: &[&CV], i: &mut usize, ctx: &UnitCtx) -> Option<CalcNode> {
    let mut acc = calc_product(t, i, ctx)?;
    while *i < t.len() {
        let sign = match t[*i] {
            CV::T(Tok::Delim('+')) => 1.0,
            CV::T(Tok::Delim('-')) => -1.0,
            // "-5px" tokenizes as a negative dimension: treat as subtraction.
            CV::T(Tok::Number(n)) if *n < 0.0 => {
                let rhs = calc_product(t, i, ctx)?;
                acc = add(acc, rhs, 1.0);
                continue;
            }
            CV::T(Tok::Dimension(n, _)) | CV::T(Tok::Percentage(n)) if *n < 0.0 => {
                let rhs = calc_product(t, i, ctx)?;
                acc = add(acc, rhs, 1.0);
                continue;
            }
            _ => break,
        };
        *i += 1;
        let rhs = calc_product(t, i, ctx)?;
        acc = add(acc, rhs, sign);
    }
    Some(acc)
}

fn calc_product(t: &[&CV], i: &mut usize, ctx: &UnitCtx) -> Option<CalcNode> {
    let mut acc = calc_value(t, i, ctx)?;
    while *i < t.len() {
        match t[*i] {
            CV::T(Tok::Delim('*')) => {
                *i += 1;
                let rhs = calc_value(t, i, ctx)?;
                if rhs.is_pure_number() {
                    let f = rhs.eval(0.0);
                    acc = scale(acc, f);
                } else if acc.is_pure_number() {
                    let f = acc.eval(0.0);
                    acc = scale(rhs, f);
                } else {
                    return None;
                }
            }
            CV::T(Tok::Delim('/')) => {
                *i += 1;
                let rhs = calc_value(t, i, ctx)?;
                if !rhs.is_pure_number() {
                    return None;
                }
                let f = rhs.eval(0.0);
                if f == 0.0 {
                    return None;
                }
                acc = scale(acc, 1.0 / f);
            }
            _ => break,
        }
    }
    Some(acc)
}

fn calc_value(t: &[&CV], i: &mut usize, ctx: &UnitCtx) -> Option<CalcNode> {
    let cv = *t.get(*i)?;
    *i += 1;
    Some(match cv {
        CV::T(Tok::Number(n)) => CalcNode::Lin { px: 0.0, pct: 0.0, num: *n as f32 },
        CV::T(Tok::Dimension(n, u)) => CalcNode::Lin { px: unit_to_px(*n as f32, u, ctx).or_else(|| angle_deg(cv))?, pct: 0.0, num: 0.0 },
        CV::T(Tok::Percentage(p)) => CalcNode::Lin { px: 0.0, pct: *p as f32, num: 0.0 },
        CV::T(Tok::Ident(id)) => match id.to_ascii_lowercase().as_str() {
            "pi" => CalcNode::Lin { px: 0.0, pct: 0.0, num: std::f32::consts::PI },
            "e" => CalcNode::Lin { px: 0.0, pct: 0.0, num: std::f32::consts::E },
            "infinity" => CalcNode::Lin { px: 0.0, pct: 0.0, num: 1e9 },
            "-infinity" => CalcNode::Lin { px: 0.0, pct: 0.0, num: -1e9 },
            _ => return None,
        },
        CV::Block('(', inner) => {
            let toks: Vec<&CV> = inner.iter().filter(|c| !c.is_ws()).collect();
            let mut j = 0;
            calc_sum(&toks, &mut j, ctx)?
        }
        CV::Func(name, args) if is_math_fn(name) => parse_calc_fn(name, args, ctx)?,
        _ => return None,
    })
}

pub fn parse_number(cv: &CV) -> Option<f32> {
    match cv {
        CV::T(Tok::Number(n)) => Some(*n as f32),
        CV::Func(name, args) if is_math_fn(name) => {
            // Every unit must resolve to a non-zero length here, or a length
            // (`calc(10vw * .45)` with no viewport) would pass for a number.
            let mut ctx = UnitCtx::new(16.0, 16.0, 997.0, 991.0);
            ctx.rlh = 19.0;
            let n = parse_calc_fn(name, args, &ctx)?;
            if n.is_pure_number() {
                Some(n.eval(0.0))
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn angle_deg(cv: &CV) -> Option<f32> {
    match cv {
        CV::T(Tok::Dimension(n, u)) => Some((match u.as_str() {
            "deg" => *n,
            "rad" => n.to_degrees(),
            "grad" => n * 0.9,
            "turn" => n * 360.0,
            _ => return None,
        }) as f32),
        CV::T(Tok::Number(n)) if *n == 0.0 => Some(0.0),
        CV::Func(name, args) if is_math_fn(name) => {
            // Angles evaluate in the px slot (degrees); a bare 0 is also an angle.
            let ctx = UnitCtx::new(16.0, 16.0, 0.0, 0.0);
            let n = parse_calc_fn(name, args, &ctx)?;
            if n.has_pct() {
                return None;
            }
            Some(n.eval(0.0))
        }
        _ => None,
    }
}

// --------------------------------------------------------------------------
// Colors
// --------------------------------------------------------------------------

thread_local! {
    /// The used color scheme of the element being styled is dark: system
    /// colors and `light-dark()` pick their dark values.
    static DARK_SCHEME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Set the color scheme used while resolving colors (see `DARK_SCHEME`).
pub fn set_dark_scheme(dark: bool) {
    DARK_SCHEME.with(|d| d.set(dark));
}

pub fn dark_scheme() -> bool {
    DARK_SCHEME.with(|d| d.get())
}

pub fn parse_color(cv: &CV) -> Option<ColorValue> {
    // Color functions through the full color module (exact hwb()/lab()/color() math),
    // unless they depend on currentcolor (resolved at use time below).
    if let CV::Func(name, _) = cv {
        if !name.eq_ignore_ascii_case("light-dark") {
            if let Some(spec) = super::color::parse(cv) {
                if !super::color::depends_on_current(&spec) {
                    let black = super::color::Abs::srgb(0.0, 0.0, 0.0, 1.0);
                    if let Some(c) = super::color::compute(&spec, &black, dark_scheme()) {
                        let [r, g, b, a] = super::color::to_rgba8(&c);
                        return Some(ColorValue::Rgba(Color::rgba(r, g, b, a)));
                    }
                }
            }
        }
    }
    parse_color_basic(cv).or_else(|| {
        // Syntaxes only the full color module knows (none, N-ary color-mix(), alpha(), …).
        let spec = super::color::parse(cv)?;
        let black = super::color::Abs::srgb(0.0, 0.0, 0.0, 1.0);
        let c = super::color::compute(&spec, &black, dark_scheme())?;
        let [r, g, b, a] = super::color::to_rgba8(&c);
        Some(ColorValue::Rgba(Color::rgba(r, g, b, a)))
    })
}

fn parse_color_basic(cv: &CV) -> Option<ColorValue> {
    match cv {
        CV::T(Tok::Hash(h, _)) => parse_hex(h).map(ColorValue::Rgba),
        CV::T(Tok::Ident(name)) => {
            let n = name.to_ascii_lowercase();
            match n.as_str() {
                "currentcolor" => Some(ColorValue::CurrentColor),
                "transparent" => Some(ColorValue::Rgba(Color::TRANSPARENT)),
                _ => named_color(&n).map(ColorValue::Rgba),
            }
        }
        CV::Func(name, args) if name.eq_ignore_ascii_case("color-mix") => parse_color_mix(args),
        CV::Func(name, args) if name.eq_ignore_ascii_case("light-dark") => {
            let parts: Vec<&CV> = args.iter().filter(|c| !c.is_ws()).collect();
            let comma = parts.iter().position(|c| c.is_comma())?;
            let (a, b) = (&parts[..comma], &parts[comma + 1..]);
            if a.len() != 1 || b.len() != 1 {
                return None;
            }
            let (l, d) = (parse_color(a[0])?, parse_color(b[0])?);
            Some(if dark_scheme() { d } else { l })
        }
        CV::Func(name, args) => {
            let n = name.to_ascii_lowercase();
            if args.iter().find(|c| !c.is_ws()).map(|c| c.is_ident("from")).unwrap_or(false) {
                return relative_color(&n, args).map(ColorValue::Rgba);
            }
            parse_color_fn(&n, args).map(ColorValue::Rgba)
        }
        _ => None,
    }
}

/// CSS Color 5 relative color syntax: `rgb(from <color> r g b / alpha)` and
/// friends. The origin's channels (in the function's space) replace the
/// channel keywords, then the function is parsed as usual.
fn relative_color(name: &str, args: &[CV]) -> Option<Color> {
    let toks: Vec<&CV> = args.iter().filter(|c| !c.is_ws()).collect();
    let origin = parse_color(toks.get(1)?)?.resolve(Color::BLACK);
    let (r, g, b, a) = (origin.r as f32 / 255.0, origin.g as f32 / 255.0, origin.b as f32 / 255.0, origin.a as f32 / 255.0);
    let nz = |v: f32| if v.is_nan() { 0.0 } else { v };
    let mut rest: Vec<CV> = toks[2..].iter().map(|c| (*c).clone()).collect();
    let chans: Vec<(&str, f32)> = match name {
        "rgb" | "rgba" => vec![("r", r * 255.0), ("g", g * 255.0), ("b", b * 255.0)],
        "hsl" | "hsla" => {
            let (h, s, l) = rgb_to_hsl(r, g, b);
            vec![("h", nz(h)), ("s", s * 100.0), ("l", l * 100.0)]
        }
        "hwb" => {
            let (h, _, _) = rgb_to_hsl(r, g, b);
            vec![("h", nz(h)), ("w", r.min(g).min(b) * 100.0), ("b", (1.0 - r.max(g).max(b)) * 100.0)]
        }
        "lab" | "oklab" => {
            let (l, x, y) = if name == "lab" { rgb_to_lab(r, g, b) } else { rgb_to_oklab(r, g, b) };
            vec![("l", l), ("a", x), ("b", y)]
        }
        "lch" | "oklch" => {
            let (l, x, y) = if name == "lch" { rgb_to_lab(r, g, b) } else { rgb_to_oklab(r, g, b) };
            let c = (x * x + y * y).sqrt();
            let h = if c < 1e-4 { 0.0 } else { y.atan2(x).to_degrees().rem_euclid(360.0) };
            vec![("l", l), ("c", c), ("h", h)]
        }
        "color" => {
            let space = rest.first()?.ident()?.to_ascii_lowercase();
            let (c1, c2, c3) = srgb_to_space(&space, r, g, b)?;
            if space.starts_with("xyz") {
                vec![("x", c1), ("y", c2), ("z", c3)]
            } else {
                vec![("r", c1), ("g", c2), ("b", c3)]
            }
        }
        _ => return None,
    };
    fn subst(v: &mut [CV], chans: &[(&str, f32)], alpha: f32) {
        for c in v.iter_mut() {
            match c {
                CV::T(Tok::Ident(i)) => {
                    let li = i.to_ascii_lowercase();
                    if li == "alpha" {
                        *c = CV::T(Tok::Number(alpha as f64));
                    } else if let Some((_, val)) = chans.iter().find(|(n, _)| *n == li) {
                        *c = CV::T(Tok::Number(*val as f64));
                    }
                }
                CV::Func(_, a) | CV::Block(_, a) => subst(a, chans, alpha),
                _ => {}
            }
        }
    }
    // The color space name of color() stays an identifier.
    let skip = if name == "color" { 1 } else { 0 };
    subst(&mut rest[skip..], &chans, a);
    if !rest.iter().any(|c| c.is_delim('/')) {
        rest.push(CV::T(Tok::Delim('/')));
        rest.push(CV::T(Tok::Number(a as f64)));
    }
    // Re-insert separators so the plain parser sees whitespace-separated values.
    let mut spaced: Vec<CV> = Vec::with_capacity(rest.len() * 2);
    for (i, c) in rest.into_iter().enumerate() {
        if i > 0 {
            spaced.push(CV::T(Tok::Ws));
        }
        spaced.push(c);
    }
    parse_color_fn(name, &spaced)
}

/// sRGB (0–1) to the channels of a `color()` space.
fn srgb_to_space(space: &str, r: f32, g: f32, b: f32) -> Option<(f32, f32, f32)> {
    let (lr, lg, lb) = (srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b));
    let xyz = |m: [[f32; 3]; 3]| (m[0][0] * lr + m[0][1] * lg + m[0][2] * lb, m[1][0] * lr + m[1][1] * lg + m[1][2] * lb, m[2][0] * lr + m[2][1] * lg + m[2][2] * lb);
    const TO_XYZ: [[f32; 3]; 3] = [[0.412_390_8, 0.357_584_33, 0.180_480_79], [0.212_639, 0.715_168_7, 0.072_192_32], [0.019_330_818, 0.119_194_78, 0.950_532_14]];
    Some(match space {
        "srgb" => (r, g, b),
        "srgb-linear" => (lr, lg, lb),
        "display-p3" => {
            let (pr, pg, pb) = xyz([[0.822_462_1, 0.177_538, 0.0], [0.033_194_1, 0.966_805_8, 0.0], [0.017_082_7, 0.072_397_4, 0.910_519_9]]);
            let enc = |c: f32| if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
            (enc(pr), enc(pg), enc(pb))
        }
        "xyz" | "xyz-d65" => xyz(TO_XYZ),
        _ => return None,
    })
}

/// A `color()` space's channels to sRGB (0–1, clipped by the caller).
fn space_to_srgb(space: &str, c: [f32; 3]) -> Option<(f32, f32, f32)> {
    let mul = |m: [[f32; 3]; 3], v: [f32; 3]| [m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2], m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2], m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2]];
    const XYZ_TO_LIN: [[f32; 3]; 3] = [[3.240_97, -1.537_383_2, -0.498_610_76], [-0.969_243_6, 1.875_967_5, 0.041_555_06], [0.055_630_08, -0.203_976_96, 1.056_971_5]];
    const D50_TO_D65: [[f32; 3]; 3] = [[0.955_473_4, -0.023_098_455, 0.063_259_24], [-0.028_369_71, 1.009_995_4, 0.021_041_44], [0.012_314_015, -0.020_507_65, 1.330_365_9]];
    let srgb_tf = |v: f32| v.signum() * if v.abs() <= 0.040_45 { v.abs() / 12.92 } else { ((v.abs() + 0.055) / 1.055).powf(2.4) };
    let lin = match space {
        "srgb" => return Some((c[0], c[1], c[2])),
        "srgb-linear" => c,
        "display-p3" => mul([[1.224_940_2, -0.224_940_4, 0.0], [-0.042_056_955, 1.042_057_1, 0.0], [-0.019_637_555, -0.078_636_05, 1.098_273_6]], c.map(srgb_tf)),
        "a98-rgb" => {
            let v = c.map(|x| x.signum() * x.abs().powf(563.0 / 256.0));
            mul(XYZ_TO_LIN, mul([[0.576_669, 0.185_558_24, 0.188_228_65], [0.297_345, 0.627_363_57, 0.075_291_46], [0.027_031_36, 0.070_688_85, 0.991_337_5]], v))
        }
        "prophoto-rgb" => {
            let v = c.map(|x| x.signum() * if x.abs() <= 16.0 / 512.0 { x.abs() / 16.0 } else { x.abs().powf(1.8) });
            mul(XYZ_TO_LIN, mul(D50_TO_D65, mul([[0.797_760_5, 0.135_185_84, 0.031_349_35], [0.288_071_13, 0.711_843_2, 0.000_085_653_96], [0.0, 0.0, 0.825_104_6]], v)))
        }
        "rec2020" => {
            let (al, be) = (1.099_296_8, 0.018_053_97);
            let v = c.map(|x| x.signum() * if x.abs() < be * 4.5 { x.abs() / 4.5 } else { ((x.abs() + al - 1.0) / al).powf(1.0 / 0.45) });
            mul(XYZ_TO_LIN, mul([[0.636_958, 0.144_616_9, 0.168_880_98], [0.262_700_2, 0.677_998_1, 0.059_301_716], [0.0, 0.028_072_693, 1.060_985_1]], v))
        }
        "xyz" | "xyz-d65" => mul(XYZ_TO_LIN, c),
        "xyz-d50" => mul(XYZ_TO_LIN, mul(D50_TO_D65, c)),
        _ => return None,
    };
    Some((linear_to_srgb(lin[0]), linear_to_srgb(lin[1]), linear_to_srgb(lin[2])))
}

pub fn parse_hex(h: &str) -> Option<Color> {
    let d: Vec<u8> = h.chars().map(|c| c.to_digit(16).map(|v| v as u8)).collect::<Option<Vec<u8>>>()?;
    Some(match d.len() {
        3 => Color::rgb(d[0] * 17, d[1] * 17, d[2] * 17),
        4 => Color::rgba(d[0] * 17, d[1] * 17, d[2] * 17, d[3] * 17),
        6 => Color::rgb(d[0] * 16 + d[1], d[2] * 16 + d[3], d[4] * 16 + d[5]),
        8 => Color::rgba(d[0] * 16 + d[1], d[2] * 16 + d[3], d[4] * 16 + d[5], d[6] * 16 + d[7]),
        _ => return None,
    })
}

fn color_args(args: &[CV]) -> Vec<&CV> {
    args.iter().filter(|c| !c.is_ws() && !c.is_comma() && !c.is_delim('/')).collect()
}

fn num_or_pct(cv: &CV, pct_scale: f32) -> Option<f32> {
    match cv {
        CV::T(Tok::Number(n)) => Some(*n as f32),
        CV::T(Tok::Percentage(p)) => Some((p / 100.0) as f32 * pct_scale),
        CV::T(Tok::Ident(i)) if i.eq_ignore_ascii_case("none") => Some(0.0),
        CV::Func(n, _) if is_math_fn(n) => parse_number(cv),
        _ => None,
    }
}

fn alpha_of(cv: Option<&&CV>) -> Option<f32> {
    match cv {
        None => Some(1.0),
        Some(c) => num_or_pct(c, 1.0),
    }
}

fn to_u8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

fn hue(cv: &CV) -> Option<f32> {
    match cv {
        CV::T(Tok::Number(n)) => Some(*n as f32),
        CV::T(Tok::Ident(i)) if i.eq_ignore_ascii_case("none") => Some(0.0),
        CV::Func(n, _) if is_math_fn(n) => parse_number(cv).or_else(|| angle_deg(cv)),
        _ => angle_deg(cv),
    }
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = ((h % 360.0) + 360.0) % 360.0 / 360.0;
    let f = |n: f32| {
        let k = (n + h * 12.0) % 12.0;
        let a = s * l.min(1.0 - l);
        l - a * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
    };
    (f(0.0), f(8.0), f(4.0))
}

fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

fn oklab_to_rgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3;
    let g = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3;
    let bb = -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;
    (linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(bb))
}

fn lab_to_rgb(l: f32, a: f32, b: f32) -> (f32, f32, f32) {
    // CIE Lab (D50) -> XYZ -> linear sRGB (D65, Bradford-adapted).
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let e = 216.0 / 24389.0;
    let k = 24389.0 / 27.0;
    let finv = |f: f32| if f * f * f > e { f * f * f } else { (116.0 * f - 16.0) / k };
    let x = finv(fx) * 0.964_22;
    let y = if l > k * e { fy * fy * fy } else { l / k };
    let z = finv(fz) * 0.825_21;
    let r = 3.134_136 * x - 1.617_386 * y - 0.490_662 * z;
    let g = -0.978_795 * x + 1.916_254 * y + 0.033_44 * z;
    let bb = 0.071_955 * x - 0.228_977 * y + 1.405_386 * z;
    (linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(bb))
}

pub fn parse_color_fn(name: &str, args: &[CV]) -> Option<Color> {
    let a = color_args(args);
    match name {
        "rgb" | "rgba" => {
            if a.len() < 3 {
                return None;
            }
            let r = num_or_pct(a[0], 255.0)?;
            let g = num_or_pct(a[1], 255.0)?;
            let b = num_or_pct(a[2], 255.0)?;
            let al = alpha_of(a.get(3))?;
            Some(Color::rgba(to_u8(r), to_u8(g), to_u8(b), to_u8(al * 255.0)))
        }
        "hsl" | "hsla" => {
            if a.len() < 3 {
                return None;
            }
            let h = hue(a[0])?;
            // Legacy comma syntax requires percentages for saturation and lightness.
            let legacy = args.iter().any(|c| c.is_comma());
            if legacy && !(matches!(a[1], CV::T(Tok::Percentage(_)) | CV::Func(..)) && matches!(a[2], CV::T(Tok::Percentage(_)) | CV::Func(..))) {
                return None;
            }
            let s = num_or_pct(a[1], 100.0)? / 100.0;
            let l = num_or_pct(a[2], 100.0)? / 100.0;
            let al = alpha_of(a.get(3))?;
            let (r, g, b) = hsl_to_rgb(h, s.clamp(0.0, 1.0), l.clamp(0.0, 1.0));
            Some(Color::rgba(to_u8(r * 255.0), to_u8(g * 255.0), to_u8(b * 255.0), to_u8(al * 255.0)))
        }
        "hwb" => {
            if a.len() < 3 {
                return None;
            }
            let h = hue(a[0])?;
            let w = num_or_pct(a[1], 100.0)? / 100.0;
            let bl = num_or_pct(a[2], 100.0)? / 100.0;
            let al = alpha_of(a.get(3))?;
            let (mut r, mut g, mut b) = hsl_to_rgb(h, 1.0, 0.5);
            let f = 1.0 - w - bl;
            r = r * f + w;
            g = g * f + w;
            b = b * f + w;
            Some(Color::rgba(to_u8(r * 255.0), to_u8(g * 255.0), to_u8(b * 255.0), to_u8(al * 255.0)))
        }
        "oklab" | "oklch" | "lab" | "lch" => {
            if a.len() < 3 {
                return None;
            }
            let (lscale, cscale) = if name.starts_with("ok") { (1.0, 0.4) } else { (100.0, 150.0) };
            let l = num_or_pct(a[0], lscale)?;
            let al = alpha_of(a.get(3))?;
            let (x, y) = if name.ends_with("ch") {
                let c = num_or_pct(a[1], cscale)?;
                let h = hue(a[2])?.to_radians();
                (c * h.cos(), c * h.sin())
            } else {
                (num_or_pct(a[1], cscale)?, num_or_pct(a[2], cscale)?)
            };
            let (r, g, b) = if name.starts_with("ok") { oklab_to_rgb(l, x, y) } else { lab_to_rgb(l, x, y) };
            Some(Color::rgba(to_u8(r * 255.0), to_u8(g * 255.0), to_u8(b * 255.0), to_u8(al * 255.0)))
        }
        "color" => {
            // color(<space> c1 c2 c3 / a), converted to sRGB and clipped.
            if a.len() < 4 {
                return None;
            }
            let space = a[0].ident()?.to_ascii_lowercase();
            let r = num_or_pct(a[1], 1.0)?;
            let g = num_or_pct(a[2], 1.0)?;
            let b = num_or_pct(a[3], 1.0)?;
            let al = alpha_of(a.get(4))?;
            let (r, g, b) = space_to_srgb(&space, [r, g, b])?;
            Some(Color::rgba(to_u8(r * 255.0), to_u8(g * 255.0), to_u8(b * 255.0), to_u8(al * 255.0)))
        }
        "color-mix" => parse_color_mix(args).map(|c| c.resolve(Color::BLACK)),
        _ => None,
    }
}

pub fn named_color(n: &str) -> Option<Color> {
    let hex = match n {
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
        // CSS system colors, dark scheme (Chrome's dark form controls).
        "canvas" | "window" | "background" if dark_scheme() => 0x121212,
        "field" if dark_scheme() => 0x3b3b3b,
        "canvastext" | "windowtext" | "fieldtext" | "buttontext" | "captiontext" | "infotext" | "menutext" if dark_scheme() => 0xffffff,
        "buttonface" | "buttonhighlight" | "threedface" | "menu" if dark_scheme() => 0x6b6b6b,
        "buttonborder" if dark_scheme() => 0x6b6b6b,
        "linktext" if dark_scheme() => 0x9e9eff,
        "visitedtext" if dark_scheme() => 0xd0adf0,
        "graytext" if dark_scheme() => 0x8e8e8e,
        // CSS system colors (light scheme).
        "canvas" | "window" | "field" | "background" => 0xffffff,
        "canvastext" | "windowtext" | "fieldtext" | "buttontext" | "captiontext" | "infotext" | "menutext" => 0x000000,
        "linktext" => 0x0000ee, "visitedtext" => 0x551a8b, "activetext" => 0xff0000,
        "buttonface" | "buttonhighlight" | "threedface" | "menu" => 0xefefef, "buttonborder" => 0x000000,
        "graytext" => 0x808080, "selecteditem" => 0x1967d2, "highlighttext" | "selecteditemtext" => 0xffffff,
        "highlight" => return Some(Color::rgba(0, 65, 198, 204)),
        "mark" => 0xffff00, "marktext" => 0x000000,
        _ => return None,
    };
    Some(Color::from_hex(hex))
}
