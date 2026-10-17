// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! CSS Animations, CSS Transitions and Web Animations (`element.animate()`).
//!
//! Animation specs (`animation-*`, `transition-*`) are parsed into the computed
//! style; `@keyframes` live in the rule index. The [`AnimEngine`] keeps the
//! running animations and transitions per element and, during the cascade,
//! turns an element's base style into its animated style for the current time
//! of the document timeline. It also produces the animation and transition
//! events for script.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::pv;
use super::style::{self, ApplyCtx, ComputedStyle, FilterOp, GenInterp, GenVal, LineHeight, Shadow, SvgPaint, TransformOp, Visibility};
use super::tokenizer::{split_commas, Tok, CV};
use super::values::{Color, ColorValue, Len};
use super::Declaration;
use crate::dom::NodeId;

// ------------------------------------------------------------------ timing functions

#[derive(Clone, Debug, PartialEq)]
pub enum Timing {
    Linear,
    Bezier(f64, f64, f64, f64),
    /// steps(n, position): 0 jump-end, 1 jump-start, 2 jump-none, 3 jump-both.
    Steps(u32, u8),
    /// linear(<point>#): (input, output) pairs.
    Points(Rc<Vec<(f32, f32)>>),
}

impl Timing {
    pub const EASE: Timing = Timing::Bezier(0.25, 0.1, 0.25, 1.0);

    pub fn eval(&self, x: f64) -> f64 {
        self.eval_flag(x, false)
    }

    /// With the "before flag" (CSS Easing §2.3: a step function at a step
    /// boundary reached from before takes the previous step).
    pub fn eval_flag(&self, x: f64, before: bool) -> f64 {
        match self {
            Timing::Linear => x,
            Timing::Bezier(x1, y1, x2, y2) => bezier(*x1, *y1, *x2, *y2, x),
            Timing::Steps(n, pos) => {
                let n = (*n).max(1) as f64;
                let (jumps, start) = match pos {
                    1 => (n, 1.0),
                    2 => ((n - 1.0).max(1.0), 0.0),
                    3 => (n + 1.0, 1.0),
                    _ => (n, 0.0),
                };
                let mut step = (x * n).floor() + start;
                if before && (x * n).fract() == 0.0 {
                    step -= 1.0;
                }
                if x >= 0.0 && step < 0.0 {
                    step = 0.0;
                }
                if x <= 1.0 && step > jumps {
                    step = jumps;
                }
                step / jumps
            }
            Timing::Points(p) => {
                if p.is_empty() {
                    return x;
                }
                let first = p[0];
                if x <= first.0 as f64 {
                    return first.1 as f64;
                }
                for w in p.windows(2) {
                    let (a, b) = (w[0], w[1]);
                    if x <= b.0 as f64 {
                        let span = (b.0 - a.0) as f64;
                        return if span <= 0.0 { b.1 as f64 } else { a.1 as f64 + (b.1 - a.1) as f64 * (x - a.0 as f64) / span };
                    }
                }
                p[p.len() - 1].1 as f64
            }
        }
    }
}

/// Cubic Bézier easing (Newton–Raphson with bisection fallback), as in Blink.
fn bezier(x1: f64, y1: f64, x2: f64, y2: f64, x: f64) -> f64 {
    if x <= 0.0 {
        // Extrapolate with the start tangent.
        let g = if x1 > 0.0 { y1 / x1 } else if y1 == 0.0 && x2 > 0.0 { y2 / x2 } else { 0.0 };
        return g * x;
    }
    if x >= 1.0 {
        let g = if x2 < 1.0 { (y2 - 1.0) / (x2 - 1.0) } else if y2 == 1.0 && x1 < 1.0 { (y1 - 1.0) / (x1 - 1.0) } else { 0.0 };
        return 1.0 + g * (x - 1.0);
    }
    let (cx, cy) = (3.0 * x1, 3.0 * y1);
    let (bx, by) = (3.0 * (x2 - x1) - cx, 3.0 * (y2 - y1) - cy);
    let (ax, ay) = (1.0 - cx - bx, 1.0 - cy - by);
    let sx = |t: f64| ((ax * t + bx) * t + cx) * t;
    let sy = |t: f64| ((ay * t + by) * t + cy) * t;
    let dx = |t: f64| (3.0 * ax * t + 2.0 * bx) * t + cx;
    let mut t = x;
    for _ in 0..16 {
        let e = sx(t) - x;
        if e.abs() < 1e-12 {
            return sy(t);
        }
        let d = dx(t);
        if d.abs() < 1e-6 {
            break;
        }
        t -= e / d;
    }
    let (mut lo, mut hi) = (0.0, 1.0);
    t = x;
    for _ in 0..60 {
        let v = sx(t);
        if (v - x).abs() < 1e-12 {
            break;
        }
        if v < x {
            lo = t;
        } else {
            hi = t;
        }
        t = (lo + hi) / 2.0;
    }
    sy(t)
}

pub fn parse_timing(v: &[CV]) -> Option<Timing> {
    let v: Vec<&CV> = v.iter().filter(|c| !c.is_ws()).collect();
    if v.len() != 1 {
        return None;
    }
    match v[0] {
        CV::T(Tok::Ident(i)) => Some(match i.to_ascii_lowercase().as_str() {
            "linear" => Timing::Linear,
            "ease" => Timing::EASE,
            "ease-in" => Timing::Bezier(0.42, 0.0, 1.0, 1.0),
            "ease-out" => Timing::Bezier(0.0, 0.0, 0.58, 1.0),
            "ease-in-out" => Timing::Bezier(0.42, 0.0, 0.58, 1.0),
            "step-start" => Timing::Steps(1, 1),
            "step-end" => Timing::Steps(1, 0),
            _ => return None,
        }),
        CV::Func(name, args) => {
            let parts: Vec<&[CV]> = split_commas(args);
            let num = |p: &[CV]| -> Option<f32> {
                let w: Vec<&CV> = p.iter().filter(|c| !c.is_ws()).collect();
                match w.as_slice() {
                    [CV::T(Tok::Number(n))] => Some(*n as f32),
                    _ => None,
                }
            };
            match name.to_ascii_lowercase().as_str() {
                "cubic-bezier" if parts.len() == 4 => {
                    let num64 = |p: &[CV]| -> Option<f64> {
                        let w: Vec<&CV> = p.iter().filter(|c| !c.is_ws()).collect();
                        match w.as_slice() {
                            [CV::T(Tok::Number(n))] => Some(*n as f64),
                            _ => None,
                        }
                    };
                    let n: Vec<f64> = parts.iter().filter_map(|p| num64(p)).collect();
                    if n.len() != 4 || !(0.0..=1.0).contains(&n[0]) || !(0.0..=1.0).contains(&n[2]) {
                        return None;
                    }
                    Some(Timing::Bezier(n[0], n[1], n[2], n[3]))
                }
                "steps" if !parts.is_empty() && parts.len() <= 2 => {
                    let n = num(parts[0])?;
                    if n < 1.0 || n.fract() != 0.0 {
                        return None;
                    }
                    let pos = match parts.get(1).and_then(|p| p.iter().find_map(|c| c.ident())).map(|s| s.to_ascii_lowercase()) {
                        None => 0,
                        Some(s) => match s.as_str() {
                            "jump-end" | "end" => 0,
                            "jump-start" | "start" => 1,
                            "jump-none" => 2,
                            "jump-both" => 3,
                            _ => return None,
                        },
                    };
                    if pos == 2 && n < 2.0 {
                        return None;
                    }
                    Some(Timing::Steps(n as u32, pos))
                }
                "linear" => {
                    // linear(0, 0.25 75%, 1): outputs with optional input percentages.
                    let mut pts: Vec<(Option<f32>, f32)> = Vec::new();
                    for p in &parts {
                        let w: Vec<&CV> = p.iter().filter(|c| !c.is_ws()).collect();
                        let out = match w.first() {
                            Some(CV::T(Tok::Number(n))) => *n as f32,
                            _ => return None,
                        };
                        let ins: Vec<f32> = w[1..].iter().filter_map(|c| if let CV::T(Tok::Percentage(p)) = c { Some((*p / 100.0) as f32) } else { None }).collect();
                        if ins.is_empty() {
                            pts.push((None, out));
                        }
                        for i in ins {
                            pts.push((Some(i), out));
                        }
                    }
                    if pts.len() < 2 {
                        return None;
                    }
                    // Fill in missing inputs: first 0, last 1, others spaced evenly.
                    if pts[0].0.is_none() {
                        pts[0].0 = Some(0.0);
                    }
                    let last = pts.len() - 1;
                    if pts[last].0.is_none() {
                        pts[last].0 = Some(1.0);
                    }
                    let mut i = 0;
                    let mut maxin = 0.0f32;
                    while i < pts.len() {
                        if let Some(x) = pts[i].0 {
                            maxin = maxin.max(x);
                            pts[i].0 = Some(maxin);
                            i += 1;
                            continue;
                        }
                        let start = i - 1;
                        let mut end = i;
                        while pts[end].0.is_none() {
                            end += 1;
                        }
                        let (a, b) = (pts[start].0.unwrap(), pts[end].0.unwrap().max(maxin));
                        for k in start + 1..end {
                            pts[k].0 = Some(a + (b - a) * (k - start) as f32 / (end - start) as f32);
                        }
                        i = end;
                    }
                    Some(Timing::Points(Rc::new(pts.into_iter().map(|(i, o)| (i.unwrap_or(0.0), o)).collect())))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn parse_time(c: &CV) -> Option<f64> {
    match c {
        CV::T(Tok::Dimension(v, u)) => match u.to_ascii_lowercase().as_str() {
            "s" => Some(*v as f64 * 1000.0),
            "ms" => Some(*v as f64),
            _ => None,
        },
        CV::T(Tok::Number(n)) if *n == 0.0 => Some(0.0),
        CV::Func(name, args) if matches!(name.to_ascii_lowercase().as_str(), "calc" | "min" | "max" | "clamp") => match calc_time(&name.to_ascii_lowercase(), args)? {
            (v, true) => Some(v),
            _ => None,
        },
        _ => None,
    }
}

/// A math function over times: (value in ms or plain number, is a time).
fn calc_time(name: &str, args: &[CV]) -> Option<(f64, bool)> {
    let toks: Vec<&CV> = args.iter().filter(|c| !c.is_ws()).collect();
    if name != "calc" {
        let parts: Vec<Vec<CV>> = args.split(|c| c.is_comma()).map(|p| p.to_vec()).collect();
        let vals: Vec<(f64, bool)> = parts.iter().map(|p| calc_time("calc", p)).collect::<Option<_>>()?;
        let t = vals.first()?.1;
        if vals.iter().any(|v| v.1 != t) {
            return None;
        }
        let v = match name {
            "min" => vals.iter().map(|v| v.0).fold(f64::INFINITY, f64::min),
            "max" => vals.iter().map(|v| v.0).fold(f64::NEG_INFINITY, f64::max),
            _ if vals.len() == 3 => vals[1].0.min(vals[2].0).max(vals[0].0),
            _ => return None,
        };
        return Some((v, t));
    }
    // sum := product (('+'|'-') product)*; product := value (('*'|'/') value)*
    fn value(t: &[&CV], i: &mut usize) -> Option<(f64, bool)> {
        let c = *t.get(*i)?;
        *i += 1;
        match c {
            CV::T(Tok::Number(n)) => Some((*n, false)),
            CV::T(Tok::Dimension(v, u)) => match u.to_ascii_lowercase().as_str() {
                "s" => Some((*v * 1000.0, true)),
                "ms" => Some((*v, true)),
                _ => None,
            },
            CV::Block('(', inner) => calc_time("calc", inner),
            CV::Func(n, a) => calc_time(&n.to_ascii_lowercase(), a),
            _ => None,
        }
    }
    fn product(t: &[&CV], i: &mut usize) -> Option<(f64, bool)> {
        let mut acc = value(t, i)?;
        while let Some(c) = t.get(*i) {
            if c.is_delim('*') {
                *i += 1;
                let b = value(t, i)?;
                if acc.1 && b.1 {
                    return None;
                }
                acc = (acc.0 * b.0, acc.1 || b.1);
            } else if c.is_delim('/') {
                *i += 1;
                let b = value(t, i)?;
                if b.1 || b.0 == 0.0 {
                    return None;
                }
                acc = (acc.0 / b.0, acc.1);
            } else {
                break;
            }
        }
        Some(acc)
    }
    let mut i = 0;
    let mut acc = product(&toks, &mut i)?;
    while let Some(c) = toks.get(i) {
        let sign = if c.is_delim('+') { 1.0 } else if c.is_delim('-') { -1.0 } else { return None };
        i += 1;
        let b = product(&toks, &mut i)?;
        if b.1 != acc.1 {
            return None;
        }
        acc = (acc.0 + sign * b.0, acc.1);
    }
    Some(acc)
}

// ------------------------------------------------------------------ specs

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Direction {
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fill {
    None,
    Forwards,
    Backwards,
    Both,
}

/// The `animation-*` longhands as lists (cycled to the length of the name list).
#[derive(Clone, Debug, PartialEq)]
pub struct AnimSpec {
    pub names: Vec<Rc<str>>,
    pub durations: Vec<f64>,
    pub timings: Vec<Timing>,
    pub delays: Vec<f64>,
    pub iterations: Vec<f64>,
    pub directions: Vec<Direction>,
    pub fills: Vec<Fill>,
    pub paused: Vec<bool>,
    pub compositions: Vec<Composite>,
}

/// How an effect's value combines with the underlying value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Composite {
    Replace,
    Add,
    Accumulate,
}

impl Composite {
    pub fn parse(s: &str) -> Option<Composite> {
        Some(match s.to_ascii_lowercase().as_str() {
            "replace" => Composite::Replace,
            "add" => Composite::Add,
            "accumulate" => Composite::Accumulate,
            _ => return None,
        })
    }
}

impl Default for AnimSpec {
    fn default() -> Self {
        AnimSpec { names: vec![], durations: vec![0.0], timings: vec![Timing::EASE], delays: vec![0.0], iterations: vec![1.0], directions: vec![Direction::Normal], fills: vec![Fill::None], paused: vec![false], compositions: vec![Composite::Replace] }
    }
}

/// One resolved animation of the list.
#[derive(Clone, Debug, PartialEq)]
pub struct AnimTiming {
    pub duration: f64,
    pub delay: f64,
    pub end_delay: f64,
    pub iterations: f64,
    pub iteration_start: f64,
    pub direction: Direction,
    pub fill: Fill,
    pub timing: Timing,
}

impl AnimSpec {
    pub fn composite(&self, i: usize) -> Composite {
        if self.compositions.is_empty() {
            return Composite::Replace;
        }
        self.compositions[i % self.compositions.len()]
    }
    pub fn get(&self, i: usize) -> (AnimTiming, bool) {
        let c = |n: usize| if n == 0 { 0 } else { i % n };
        (
            AnimTiming {
                duration: self.durations.get(c(self.durations.len())).copied().unwrap_or(0.0),
                delay: self.delays.get(c(self.delays.len())).copied().unwrap_or(0.0),
                end_delay: 0.0,
                iterations: self.iterations.get(c(self.iterations.len())).copied().unwrap_or(1.0),
                iteration_start: 0.0,
                direction: self.directions.get(c(self.directions.len())).copied().unwrap_or(Direction::Normal),
                fill: self.fills.get(c(self.fills.len())).copied().unwrap_or(Fill::None),
                timing: self.timings.get(c(self.timings.len())).cloned().unwrap_or(Timing::EASE),
            },
            self.paused.get(c(self.paused.len())).copied().unwrap_or(false),
        )
    }
}

/// The `transition-*` longhands.
#[derive(Clone, Debug, PartialEq)]
pub struct TransSpec {
    /// Property names; "all", or "none" (empty list).
    pub props: Vec<Rc<str>>,
    pub durations: Vec<f64>,
    pub timings: Vec<Timing>,
    pub delays: Vec<f64>,
    pub allow_discrete: Vec<bool>,
}

impl Default for TransSpec {
    fn default() -> Self {
        TransSpec { props: vec![Rc::from("all")], durations: vec![0.0], timings: vec![Timing::EASE], delays: vec![0.0], allow_discrete: vec![false] }
    }
}

fn idents(v: &[CV]) -> Vec<String> {
    v.iter().filter_map(|c| c.ident()).map(|s| s.to_string()).collect()
}

/// Apply an `animation*` property to the style. Returns false if not one.
pub fn apply_animation_prop(s: &mut ComputedStyle, name: &str, v: &[CV]) -> bool {
    let lists: Vec<&[CV]> = split_commas(v);
    let mut spec = s.animation.as_deref().cloned().unwrap_or_default();
    macro_rules! each {
        ($f:expr) => {{
            let mut out = Vec::new();
            for l in &lists {
                match $f(*l) {
                    Some(x) => out.push(x),
                    None => return true,
                }
            }
            out
        }};
    }
    fn one(l: &[CV]) -> Option<&CV> {
        let w: Vec<&CV> = l.iter().filter(|c| !c.is_ws()).collect();
        if w.len() == 1 {
            Some(w[0])
        } else {
            None
        }
    }
    match name {
        "animation-name" | "-webkit-animation-name" => {
            spec.names = each!(|l: &[CV]| match one(l) {
                Some(CV::T(Tok::Ident(i))) => Some(Rc::from(i.as_str())),
                Some(CV::T(Tok::Str(i))) => Some(Rc::from(i.as_str())),
                _ => None,
            });
        }
        "animation-duration" | "-webkit-animation-duration" => spec.durations = each!(|l: &[CV]| one(l).and_then(parse_time).filter(|d| *d >= 0.0).or_else(|| one(l).filter(|c| c.is_ident("auto")).map(|_| 0.0))),
        "animation-delay" | "-webkit-animation-delay" => spec.delays = each!(|l: &[CV]| one(l).and_then(parse_time)),
        "animation-timing-function" | "-webkit-animation-timing-function" => spec.timings = each!(|l: &[CV]| parse_timing(l)),
        "animation-iteration-count" | "-webkit-animation-iteration-count" => {
            spec.iterations = each!(|l: &[CV]| match one(l) {
                Some(CV::T(Tok::Number(n))) if *n >= 0.0 => Some(*n as f64),
                Some(c) if c.is_ident("infinite") => Some(f64::INFINITY),
                _ => None,
            })
        }
        "animation-direction" | "-webkit-animation-direction" => {
            spec.directions = each!(|l: &[CV]| one(l).and_then(|c| c.ident()).and_then(|i| match i.to_ascii_lowercase().as_str() {
                "normal" => Some(Direction::Normal),
                "reverse" => Some(Direction::Reverse),
                "alternate" => Some(Direction::Alternate),
                "alternate-reverse" => Some(Direction::AlternateReverse),
                _ => None,
            }))
        }
        "animation-fill-mode" | "-webkit-animation-fill-mode" => {
            spec.fills = each!(|l: &[CV]| one(l).and_then(|c| c.ident()).and_then(|i| match i.to_ascii_lowercase().as_str() {
                "none" => Some(Fill::None),
                "forwards" => Some(Fill::Forwards),
                "backwards" => Some(Fill::Backwards),
                "both" => Some(Fill::Both),
                _ => None,
            }))
        }
        "animation-composition" => spec.compositions = each!(|l: &[CV]| one(l).and_then(|c| c.ident()).and_then(Composite::parse)),
        "animation-play-state" | "-webkit-animation-play-state" => {
            spec.paused = each!(|l: &[CV]| one(l).and_then(|c| c.ident()).and_then(|i| match i.to_ascii_lowercase().as_str() {
                "running" => Some(false),
                "paused" => Some(true),
                _ => None,
            }))
        }
        "animation" | "-webkit-animation" => {
            let mut n = AnimSpec { names: vec![], durations: vec![], timings: vec![], delays: vec![], iterations: vec![], directions: vec![], fills: vec![], paused: vec![], compositions: spec.compositions.clone() };
            for l in &lists {
                let (mut name, mut dur, mut delay, mut timing, mut iters, mut dir, mut fill, mut paused) = (None, None, None, None, None, None, None, None);
                for c in l.iter().filter(|c| !c.is_ws()) {
                    if let Some(t) = parse_time(c) {
                        if dur.is_none() {
                            dur = Some(t.max(0.0));
                        } else if delay.is_none() {
                            delay = Some(t);
                        } else {
                            return true;
                        }
                        continue;
                    }
                    if timing.is_none() {
                        if let Some(t) = parse_timing(std::slice::from_ref(c)) {
                            timing = Some(t);
                            continue;
                        }
                    }
                    match c {
                        CV::T(Tok::Number(x)) if iters.is_none() && *x >= 0.0 => iters = Some(*x as f64),
                        CV::T(Tok::Ident(i)) => {
                            let l = i.to_ascii_lowercase();
                            match l.as_str() {
                                "infinite" if iters.is_none() => iters = Some(f64::INFINITY),
                                "normal" | "reverse" | "alternate" | "alternate-reverse" if dir.is_none() => {
                                    dir = Some(match l.as_str() {
                                        "reverse" => Direction::Reverse,
                                        "alternate" => Direction::Alternate,
                                        "alternate-reverse" => Direction::AlternateReverse,
                                        _ => Direction::Normal,
                                    })
                                }
                                "forwards" | "backwards" | "both" if fill.is_none() => {
                                    fill = Some(match l.as_str() {
                                        "forwards" => Fill::Forwards,
                                        "backwards" => Fill::Backwards,
                                        _ => Fill::Both,
                                    })
                                }
                                "none" if name.is_none() => name = Some(Rc::from("none")),
                                "none" if fill.is_none() => fill = Some(Fill::None),
                                "running" | "paused" if paused.is_none() => paused = Some(l == "paused"),
                                _ if name.is_none() => name = Some(Rc::from(i.as_str())),
                                _ => return true,
                            }
                        }
                        CV::T(Tok::Str(i)) if name.is_none() => name = Some(Rc::from(i.as_str())),
                        _ => return true,
                    }
                }
                n.names.push(name.unwrap_or_else(|| Rc::from("none")));
                n.durations.push(dur.unwrap_or(0.0));
                n.delays.push(delay.unwrap_or(0.0));
                n.timings.push(timing.unwrap_or(Timing::EASE));
                n.iterations.push(iters.unwrap_or(1.0));
                n.directions.push(dir.unwrap_or(Direction::Normal));
                n.fills.push(fill.unwrap_or(Fill::None));
                n.paused.push(paused.unwrap_or(false));
            }
            spec = n;
        }
        _ => return false,
    }
    // Kept even without names: longhands can arrive in any order.
    s.animation = Some(Rc::new(spec));
    true
}

/// Apply a `transition*` property. Returns false if not one.
pub fn apply_transition_prop(s: &mut ComputedStyle, name: &str, v: &[CV]) -> bool {
    let lists: Vec<&[CV]> = split_commas(v);
    let mut spec = s.transition.as_deref().cloned().unwrap_or_default();
    let one = |l: &[CV]| -> Option<CV> {
        let w: Vec<&CV> = l.iter().filter(|c| !c.is_ws()).collect();
        if w.len() == 1 {
            Some(w[0].clone())
        } else {
            None
        }
    };
    match name {
        "transition-property" | "-webkit-transition-property" => {
            let mut out = Vec::new();
            for l in &lists {
                match one(l).and_then(|c| c.ident().map(|s| s.to_ascii_lowercase())) {
                    Some(p) => out.push(Rc::from(p.as_str())),
                    None => return true,
                }
            }
            if out.len() == 1 && &*out[0] == "none" {
                out.clear();
            }
            spec.props = out;
        }
        "transition-duration" | "-webkit-transition-duration" => {
            let mut out = Vec::new();
            for l in &lists {
                match one(l).as_ref().and_then(parse_time) {
                    Some(t) if t >= 0.0 => out.push(t),
                    _ => return true,
                }
            }
            spec.durations = out;
        }
        "transition-delay" | "-webkit-transition-delay" => {
            let mut out = Vec::new();
            for l in &lists {
                match one(l).as_ref().and_then(parse_time) {
                    Some(t) => out.push(t),
                    None => return true,
                }
            }
            spec.delays = out;
        }
        "transition-timing-function" | "-webkit-transition-timing-function" => {
            let mut out = Vec::new();
            for l in &lists {
                match parse_timing(l) {
                    Some(t) => out.push(t),
                    None => return true,
                }
            }
            spec.timings = out;
        }
        "transition-behavior" => {
            spec.allow_discrete = idents(v).iter().map(|i| i.eq_ignore_ascii_case("allow-discrete")).collect();
        }
        "transition" | "-webkit-transition" => {
            let mut n = TransSpec { props: vec![], durations: vec![], timings: vec![], delays: vec![], allow_discrete: vec![] };
            for l in &lists {
                let (mut prop, mut dur, mut delay, mut timing, mut discrete) = (None, None, None, None, false);
                for c in l.iter().filter(|c| !c.is_ws()) {
                    if let Some(t) = parse_time(c) {
                        if dur.is_none() {
                            if t < 0.0 {
                                return true;
                            }
                            dur = Some(t);
                        } else if delay.is_none() {
                            delay = Some(t);
                        } else {
                            return true;
                        }
                        continue;
                    }
                    if timing.is_none() {
                        if let Some(t) = parse_timing(std::slice::from_ref(c)) {
                            timing = Some(t);
                            continue;
                        }
                    }
                    match c.ident().map(|s| s.to_ascii_lowercase()) {
                        Some(i) if i == "allow-discrete" => discrete = true,
                        Some(i) if i == "normal" => {}
                        Some(i) if prop.is_none() => prop = Some(Rc::from(i.as_str())),
                        _ => return true,
                    }
                }
                n.props.push(prop.unwrap_or_else(|| Rc::from("all")));
                n.durations.push(dur.unwrap_or(0.0));
                n.delays.push(delay.unwrap_or(0.0));
                n.timings.push(timing.unwrap_or(Timing::EASE));
                n.allow_discrete.push(discrete);
            }
            if n.props.len() == 1 && &*n.props[0] == "none" {
                n.props.clear();
            }
            spec = n;
        }
        _ => return false,
    }
    s.transition = Some(Rc::new(spec));
    true
}

// ------------------------------------------------------------------ keyframes

#[derive(Clone, Debug)]
pub struct Keyframe {
    pub offset: f64,
    pub timing: Option<Timing>,
    pub decls: Vec<Declaration>,
    /// The keyframe's own composite operation (else the effect's).
    pub composite: Option<Composite>,
}

#[derive(Clone, Debug, Default)]
pub struct Keyframes {
    /// Sorted by offset.
    pub frames: Vec<Keyframe>,
    /// The effect's composite operation (script animations).
    pub composite: Option<Composite>,
}

/// Parse the block of `@keyframes`.
pub fn parse_keyframes(block: &[CV]) -> Keyframes {
    let mut frames = Vec::new();
    let mut i = 0;
    let mut start = 0;
    while i < block.len() {
        if let CV::Block('{', body) = &block[i] {
            let prelude = &block[start..i];
            let mut offsets = Vec::new();
            let mut ok = true;
            for sel in split_commas(prelude) {
                let w: Vec<&CV> = sel.iter().filter(|c| !c.is_ws()).collect();
                match w.as_slice() {
                    [c] if c.is_ident("from") => offsets.push(0.0),
                    [c] if c.is_ident("to") => offsets.push(1.0),
                    [CV::T(Tok::Percentage(p))] if (0.0..=100.0).contains(p) => offsets.push(*p as f64 / 100.0),
                    _ => ok = false,
                }
            }
            if ok && !offsets.is_empty() {
                let mut decls: Vec<Declaration> = body.split(|c| matches!(c, CV::T(Tok::Semicolon))).filter_map(super::parse_declaration).filter(|d| !d.important).collect();
                let timing = decls.iter().rposition(|d| &*d.name == "animation-timing-function").and_then(|k| parse_timing(&decls[k].value));
                let composite = decls.iter().rposition(|d| &*d.name == "animation-composition").and_then(|k| decls[k].value.iter().find_map(|c| c.ident()).and_then(Composite::parse));
                decls.retain(|d| !d.name.starts_with("animation") && !d.name.starts_with("transition"));
                for o in offsets {
                    frames.push(Keyframe { offset: o, timing: timing.clone(), decls: decls.clone(), composite });
                }
            }
            start = i + 1;
        }
        i += 1;
    }
    // Stable sort keeps source order for equal offsets (later ones win).
    frames.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    Keyframes { frames, composite: None }
}

// ------------------------------------------------------------------ animatable values

/// An interpolable property of the computed style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prop {
    Opacity,
    Transform,
    Translate,
    Rotate,
    Scale,
    TransformOrigin,
    Color,
    BackgroundColor,
    BorderColor(u8),
    OutlineColor,
    TextDecorationColor,
    CaretColor,
    Fill,
    Stroke,
    Width,
    Height,
    MinWidth,
    MinHeight,
    MaxWidth,
    MaxHeight,
    Inset(u8),
    Margin(u8),
    Padding(u8),
    BorderWidth(u8),
    BorderRadius(u8),
    OutlineWidth,
    OutlineOffset,
    FontSize,
    FontWeight,
    LetterSpacing,
    WordSpacing,
    LineHeight,
    TextIndent,
    BoxShadow,
    TextShadow,
    Filter,
    ZIndex,
    Visibility,
    FlexGrow,
    FlexShrink,
    FlexBasis,
    RowGap,
    ColumnGap,
    ObjectPosition,
    BackgroundPosition,
    /// An SVG presentation property (index into `style::SVG_PROPS`).
    Svg(u8),
}

const ALL_PROPS: &[Prop] = &[
    Prop::Opacity,
    Prop::Transform,
    Prop::Translate,
    Prop::Rotate,
    Prop::Scale,
    Prop::TransformOrigin,
    Prop::Color,
    Prop::BackgroundColor,
    Prop::BorderColor(0),
    Prop::BorderColor(1),
    Prop::BorderColor(2),
    Prop::BorderColor(3),
    Prop::OutlineColor,
    Prop::TextDecorationColor,
    Prop::CaretColor,
    Prop::Fill,
    Prop::Stroke,
    Prop::Width,
    Prop::Height,
    Prop::MinWidth,
    Prop::MinHeight,
    Prop::MaxWidth,
    Prop::MaxHeight,
    Prop::Inset(0),
    Prop::Inset(1),
    Prop::Inset(2),
    Prop::Inset(3),
    Prop::Margin(0),
    Prop::Margin(1),
    Prop::Margin(2),
    Prop::Margin(3),
    Prop::Padding(0),
    Prop::Padding(1),
    Prop::Padding(2),
    Prop::Padding(3),
    Prop::BorderWidth(0),
    Prop::BorderWidth(1),
    Prop::BorderWidth(2),
    Prop::BorderWidth(3),
    Prop::BorderRadius(0),
    Prop::BorderRadius(1),
    Prop::BorderRadius(2),
    Prop::BorderRadius(3),
    Prop::OutlineWidth,
    Prop::OutlineOffset,
    Prop::FontSize,
    Prop::FontWeight,
    Prop::LetterSpacing,
    Prop::WordSpacing,
    Prop::LineHeight,
    Prop::TextIndent,
    Prop::BoxShadow,
    Prop::TextShadow,
    Prop::Filter,
    Prop::ZIndex,
    Prop::Visibility,
    Prop::FlexGrow,
    Prop::FlexShrink,
    Prop::FlexBasis,
    Prop::RowGap,
    Prop::ColumnGap,
    Prop::ObjectPosition,
    Prop::BackgroundPosition,
];

/// The animatable properties a property name (longhand or shorthand) covers.
pub fn props_of(name: &str) -> Vec<Prop> {
    use Prop::*;
    let sides = |f: fn(u8) -> Prop| (0..4).map(f).collect::<Vec<_>>();
    let side = |n: &str| -> u8 {
        if n.contains("top") {
            0
        } else if n.contains("right") {
            1
        } else if n.contains("bottom") {
            2
        } else {
            3
        }
    };
    let n = name.strip_prefix("-webkit-").unwrap_or(name);
    match n {
        "all" => ALL_PROPS.to_vec(),
        "opacity" => vec![Opacity],
        "transform" => vec![Transform],
        "translate" => vec![Translate],
        "rotate" => vec![Rotate],
        "scale" => vec![Scale],
        "transform-origin" => vec![TransformOrigin],
        "color" => vec![Color],
        "background-color" => vec![BackgroundColor],
        "background" => vec![BackgroundColor, BackgroundPosition],
        "background-position" => vec![BackgroundPosition],
        "border-color" => sides(BorderColor),
        "border-width" => sides(BorderWidth),
        "border" => sides(BorderColor).into_iter().chain(sides(BorderWidth)).collect(),
        "border-top" | "border-right" | "border-bottom" | "border-left" => vec![BorderColor(side(n)), BorderWidth(side(n))],
        "border-top-color" | "border-right-color" | "border-bottom-color" | "border-left-color" => vec![BorderColor(side(n))],
        "border-top-width" | "border-right-width" | "border-bottom-width" | "border-left-width" => vec![BorderWidth(side(n))],
        "border-radius" => sides(BorderRadius),
        "border-top-left-radius" => vec![BorderRadius(0)],
        "border-top-right-radius" => vec![BorderRadius(1)],
        "border-bottom-right-radius" => vec![BorderRadius(2)],
        "border-bottom-left-radius" => vec![BorderRadius(3)],
        "outline" => vec![OutlineColor, OutlineWidth],
        "outline-color" => vec![OutlineColor],
        "outline-width" => vec![OutlineWidth],
        "outline-offset" => vec![OutlineOffset],
        "text-decoration-color" => vec![TextDecorationColor],
        "caret-color" => vec![CaretColor],
        "fill" => vec![Fill],
        "stroke" => vec![Stroke],
        "width" | "inline-size" => vec![Width],
        "height" | "block-size" => vec![Height],
        "min-width" => vec![MinWidth],
        "min-height" => vec![MinHeight],
        "max-width" => vec![MaxWidth],
        "max-height" => vec![MaxHeight],
        "inset" => sides(Inset),
        "top" | "right" | "bottom" | "left" => vec![Inset(side(n))],
        "margin" => sides(Margin),
        "margin-top" | "margin-right" | "margin-bottom" | "margin-left" => vec![Margin(side(n))],
        "padding" => sides(Padding),
        "padding-top" | "padding-right" | "padding-bottom" | "padding-left" => vec![Padding(side(n))],
        "font-size" => vec![FontSize],
        "font-weight" => vec![FontWeight],
        "font" => vec![FontSize, FontWeight, LineHeight],
        "letter-spacing" => vec![LetterSpacing],
        "word-spacing" => vec![WordSpacing],
        "line-height" => vec![LineHeight],
        "text-indent" => vec![TextIndent],
        "box-shadow" => vec![BoxShadow],
        "text-shadow" => vec![TextShadow],
        "filter" => vec![Filter],
        "z-index" => vec![ZIndex],
        "visibility" => vec![Visibility],
        "flex" => vec![FlexGrow, FlexShrink, FlexBasis],
        "flex-grow" => vec![FlexGrow],
        "flex-shrink" => vec![FlexShrink],
        "flex-basis" => vec![FlexBasis],
        "gap" | "grid-gap" => vec![RowGap, ColumnGap],
        "row-gap" | "grid-row-gap" => vec![RowGap],
        "column-gap" | "grid-column-gap" => vec![ColumnGap],
        "object-position" => vec![ObjectPosition],
        _ => match style::svg_prop_index(n) {
            Some(i) => vec![Svg(i)],
            None => vec![],
        },
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AV {
    Num(f32),
    Len(Len),
    Color(Color),
    Pair(Len, Len),
    Transform(Option<Rc<Vec<TransformOp>>>),
    Shadows(Option<Rc<Vec<Shadow>>>),
    Filter(Option<Rc<Vec<FilterOp>>>),
    Vis(Visibility),
    Z(Option<i32>),
    Lh(LineHeight),
    Translate(Option<(Len, Len, f32)>),
    Rotate(Option<(f32, f32, f32, f32)>),
    Scale(Option<(f32, f32, f32)>),
    Paint(Option<SvgPaint>),
    BgPos(Vec<(Len, Len)>),
    Raw(Option<Rc<str>>),
}

fn get(s: &ComputedStyle, p: Prop) -> AV {
    let col = |c: &ColorValue| AV::Color(c.resolve(s.color));
    match p {
        Prop::Opacity => AV::Num(s.opacity),
        Prop::Transform => AV::Transform(s.transform.clone()),
        Prop::Translate => AV::Translate(s.translate_prop.clone()),
        Prop::Rotate => AV::Rotate(s.rotate_prop),
        Prop::Scale => AV::Scale(s.scale_prop),
        Prop::TransformOrigin => AV::Pair(s.transform_origin.0.clone(), s.transform_origin.1.clone()),
        Prop::Color => AV::Color(s.color),
        Prop::BackgroundColor => col(&s.background_color),
        Prop::BorderColor(i) => col(&s.border_color[i as usize]),
        Prop::OutlineColor => col(&s.outline_color),
        Prop::TextDecorationColor => col(&s.text_decoration_color),
        // `auto` doesn't interpolate with colors.
        Prop::CaretColor => s.caret_color.map(AV::Color).unwrap_or(AV::Raw(None)),
        Prop::Fill => AV::Paint(s.fill.clone()),
        Prop::Stroke => AV::Paint(s.stroke.clone()),
        Prop::Width => AV::Len(s.width.clone()),
        Prop::Height => AV::Len(s.height.clone()),
        Prop::MinWidth => AV::Len(s.min_width.clone()),
        Prop::MinHeight => AV::Len(s.min_height.clone()),
        Prop::MaxWidth => AV::Len(s.max_width.clone()),
        Prop::MaxHeight => AV::Len(s.max_height.clone()),
        Prop::Inset(i) => AV::Len(s.inset[i as usize].clone()),
        Prop::Margin(i) => AV::Len(s.margin[i as usize].clone()),
        Prop::Padding(i) => AV::Len(s.padding[i as usize].clone()),
        Prop::BorderWidth(i) => AV::Num(s.border_width[i as usize]),
        Prop::BorderRadius(i) => AV::Pair(s.border_radius[i as usize].0.clone(), s.border_radius[i as usize].1.clone()),
        Prop::OutlineWidth => AV::Num(s.outline_width),
        Prop::OutlineOffset => AV::Num(s.outline_offset),
        Prop::FontSize => AV::Num(s.font_size),
        Prop::FontWeight => AV::Num(s.font_weight as f32),
        Prop::LetterSpacing => AV::Num(s.letter_spacing),
        Prop::WordSpacing => AV::Num(s.word_spacing),
        Prop::LineHeight => AV::Lh(s.line_height),
        Prop::TextIndent => AV::Len(s.text_indent.clone()),
        Prop::BoxShadow => AV::Shadows(s.box_shadow.clone()),
        Prop::TextShadow => AV::Shadows(s.text_shadow.clone()),
        Prop::Filter => AV::Filter(s.filter.clone()),
        Prop::ZIndex => AV::Z(s.z_index),
        Prop::Visibility => AV::Vis(s.visibility),
        Prop::FlexGrow => AV::Num(s.flex_grow),
        Prop::FlexShrink => AV::Num(s.flex_shrink),
        Prop::FlexBasis => AV::Len(s.flex_basis.clone()),
        Prop::RowGap => AV::Len(s.row_gap.clone()),
        Prop::ColumnGap => AV::Len(s.column_gap.clone()),
        Prop::ObjectPosition => AV::Pair(s.object_position.0.clone(), s.object_position.1.clone()),
        Prop::BackgroundPosition => AV::BgPos(s.background.as_ref().map(|l| l.iter().map(|x| x.pos.clone()).collect()).unwrap_or_default()),
        Prop::Svg(i) => AV::Raw(s.svg_value(i).cloned()),
    }
}

fn set(s: &mut ComputedStyle, p: Prop, v: AV) {
    style::mark_generic_native(s, prop_name(p));
    match (p, v) {
        (Prop::Opacity, AV::Num(x)) => s.opacity = x.clamp(0.0, 1.0),
        (Prop::Transform, AV::Transform(t)) => s.transform = t,
        (Prop::Translate, AV::Translate(t)) => s.translate_prop = t,
        (Prop::Rotate, AV::Rotate(t)) => s.rotate_prop = t,
        (Prop::Scale, AV::Scale(t)) => s.scale_prop = t,
        (Prop::TransformOrigin, AV::Pair(a, b)) => s.transform_origin = (a, b),
        (Prop::Color, AV::Color(c)) => s.color = c,
        (Prop::BackgroundColor, AV::Color(c)) => s.background_color = ColorValue::Rgba(c),
        (Prop::BorderColor(i), AV::Color(c)) => s.border_color[i as usize] = ColorValue::Rgba(c),
        (Prop::OutlineColor, AV::Color(c)) => s.outline_color = ColorValue::Rgba(c),
        (Prop::TextDecorationColor, AV::Color(c)) => s.text_decoration_color = ColorValue::Rgba(c),
        (Prop::CaretColor, AV::Color(c)) => s.caret_color = Some(c),
        (Prop::CaretColor, AV::Raw(None)) => s.caret_color = None,
        (Prop::Fill, AV::Paint(c)) => s.fill = c,
        (Prop::Stroke, AV::Paint(c)) => s.stroke = c,
        (Prop::Width, AV::Len(l)) => s.width = l,
        (Prop::Height, AV::Len(l)) => s.height = l,
        (Prop::MinWidth, AV::Len(l)) => s.min_width = l,
        (Prop::MinHeight, AV::Len(l)) => s.min_height = l,
        (Prop::MaxWidth, AV::Len(l)) => s.max_width = l,
        (Prop::MaxHeight, AV::Len(l)) => s.max_height = l,
        (Prop::Inset(i), AV::Len(l)) => s.inset[i as usize] = l,
        (Prop::Margin(i), AV::Len(l)) => s.margin[i as usize] = l,
        (Prop::Padding(i), AV::Len(l)) => s.padding[i as usize] = l,
        (Prop::BorderWidth(i), AV::Num(x)) => s.border_width[i as usize] = x.max(0.0),
        (Prop::BorderRadius(i), AV::Pair(a, b)) => s.border_radius[i as usize] = (a, b),
        (Prop::OutlineWidth, AV::Num(x)) => s.outline_width = x.max(0.0),
        (Prop::OutlineOffset, AV::Num(x)) => s.outline_offset = x,
        (Prop::FontSize, AV::Num(x)) => {
            s.font_size = x.max(0.0);
            s.font_kw = None;
        }
        (Prop::FontWeight, AV::Num(x)) => s.font_weight = x.round().clamp(1.0, 1000.0) as u16,
        (Prop::LetterSpacing, AV::Num(x)) => s.letter_spacing = x,
        (Prop::WordSpacing, AV::Num(x)) => s.word_spacing = x,
        (Prop::LineHeight, AV::Lh(l)) => s.line_height = l,
        (Prop::TextIndent, AV::Len(l)) => s.text_indent = l,
        (Prop::BoxShadow, AV::Shadows(v)) => s.box_shadow = v,
        (Prop::TextShadow, AV::Shadows(v)) => s.text_shadow = v,
        (Prop::Filter, AV::Filter(v)) => s.filter = v,
        (Prop::ZIndex, AV::Z(z)) => s.z_index = z,
        (Prop::Visibility, AV::Vis(v)) => s.visibility = v,
        (Prop::FlexGrow, AV::Num(x)) => s.flex_grow = x.max(0.0),
        (Prop::FlexShrink, AV::Num(x)) => s.flex_shrink = x.max(0.0),
        (Prop::FlexBasis, AV::Len(l)) => s.flex_basis = l,
        (Prop::RowGap, AV::Len(l)) => s.row_gap = l,
        (Prop::ColumnGap, AV::Len(l)) => s.column_gap = l,
        (Prop::ObjectPosition, AV::Pair(a, b)) => s.object_position = (a, b),
        (Prop::Svg(i), AV::Raw(v)) => s.set_svg_value(i, v),
        (Prop::BackgroundPosition, AV::BgPos(v)) => {
            if let Some(layers) = s.background.as_mut() {
                let l = Rc::make_mut(layers);
                for (layer, pos) in l.iter_mut().zip(v) {
                    layer.pos = pos;
                }
            }
        }
        _ => {}
    }
}

fn lerp(a: f32, b: f32, p: f64) -> f32 {
    a + (b - a) * p as f32
}

fn lerp_len(a: &Len, b: &Len, p: f64) -> Option<Len> {
    let parts = |l: &Len| match l {
        Len::Px(v) => Some((*v, 0.0, 0u8)),
        Len::Pct(v) => Some((0.0, *v, 1u8)),
        Len::Calc(px, pc) => Some((*px, *pc, 2u8)),
        _ => None,
    };
    let (a, b) = (parts(a)?, parts(b)?);
    let px = lerp(a.0, b.0, p);
    let pc = lerp(a.1, b.1, p);
    Some(match (a.2, b.2) {
        (0, 0) => Len::Px(px),
        (1, 1) => Len::Pct(pc),
        _ => Len::Calc(px, pc),
    })
}

fn lerp_color(a: Color, b: Color, p: f64) -> Color {
    // Premultiplied sRGB, as for legacy colors in Chrome.
    let (aa, ba) = (a.a as f64 / 255.0, b.a as f64 / 255.0);
    // Extrapolated alpha is clamped before un-premultiplying.
    let alpha = (aa + (ba - aa) * p).clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| -> u8 {
        if alpha <= 0.0 {
            return 0;
        }
        let v = (x as f64 * aa + (y as f64 * ba - x as f64 * aa) * p) / alpha;
        // (Halves round up even when the product lands a hair below them.)
        (v + 1e-6).round().clamp(0.0, 255.0) as u8
    };
    Color { r: ch(a.r, b.r), g: ch(a.g, b.g), b: ch(a.b, b.b), a: (alpha * 255.0 + 1e-6).round().clamp(0.0, 255.0) as u8 }
}

fn identity_op(op: &TransformOp) -> TransformOp {
    match op {
        TransformOp::Translate(..) => TransformOp::Translate(Len::Px(0.0), Len::Px(0.0)),
        TransformOp::Scale(..) => TransformOp::Scale(1.0, 1.0),
        TransformOp::Rotate(_) => TransformOp::Rotate(0.0),
        TransformOp::Skew(..) => TransformOp::Skew(0.0, 0.0),
        TransformOp::Matrix(_) => TransformOp::Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
        TransformOp::Translate3d(..) => TransformOp::Translate3d(Len::Px(0.0), Len::Px(0.0), 0.0),
        TransformOp::Scale3d(..) => TransformOp::Scale3d(1.0, 1.0, 1.0),
        TransformOp::Rotate3d(x, y, z, _) => TransformOp::Rotate3d(*x, *y, *z, 0.0),
        TransformOp::Perspective(_) => TransformOp::Perspective(None),
        TransformOp::Matrix3d(_) => TransformOp::Matrix3d([1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0]),
    }
}

/// The 3D form of a 2D function, so 2D and 3D variants can interpolate.
fn promote_3d(op: &TransformOp) -> TransformOp {
    match op {
        TransformOp::Translate(x, y) => TransformOp::Translate3d(x.clone(), y.clone(), 0.0),
        TransformOp::Scale(x, y) => TransformOp::Scale3d(*x, *y, 1.0),
        TransformOp::Rotate(d) => TransformOp::Rotate3d(0.0, 0.0, 1.0, *d),
        o => o.clone(),
    }
}

fn lerp_op(a: &TransformOp, b: &TransformOp, p: f64) -> Option<TransformOp> {
    use TransformOp::*;
    Some(match (a, b) {
        (Translate(ax, ay), Translate(bx, by)) => Translate(lerp_len(ax, bx, p)?, lerp_len(ay, by, p)?),
        (Scale(ax, ay), Scale(bx, by)) => Scale(lerp(*ax, *bx, p), lerp(*ay, *by, p)),
        (Rotate(x), Rotate(y)) => Rotate(lerp(*x, *y, p)),
        (Skew(ax, ay), Skew(bx, by)) => Skew(lerp(*ax, *bx, p), lerp(*ay, *by, p)),
        (Translate3d(ax, ay, az), Translate3d(bx, by, bz)) => Translate3d(lerp_len(ax, bx, p)?, lerp_len(ay, by, p)?, lerp(*az, *bz, p)),
        (Scale3d(ax, ay, az), Scale3d(bx, by, bz)) => Scale3d(lerp(*ax, *bx, p), lerp(*ay, *by, p), lerp(*az, *bz, p)),
        (Rotate3d(ax, ay, az, ad), Rotate3d(bx, by, bz, bd)) => {
            // Same axis (or one side at zero angle): interpolate the angle.
            let norm = |x: f32, y: f32, z: f32| {
                let l = (x * x + y * y + z * z).sqrt().max(1e-12);
                (x / l, y / l, z / l)
            };
            let (na, nb) = (norm(*ax, *ay, *az), norm(*bx, *by, *bz));
            let same = (na.0 - nb.0).abs() < 1e-5 && (na.1 - nb.1).abs() < 1e-5 && (na.2 - nb.2).abs() < 1e-5;
            if same || *ad == 0.0 {
                Rotate3d(nb.0, nb.1, nb.2, lerp(*ad, *bd, p))
            } else if *bd == 0.0 {
                Rotate3d(na.0, na.1, na.2, lerp(*ad, *bd, p))
            } else {
                return None;
            }
        }
        (Perspective(a), Perspective(b)) => {
            // Interpolate 1/d (none = infinite distance).
            let inv = |d: &Option<f32>| d.map(|d| 1.0 / d.max(1.0)).unwrap_or(0.0);
            let v = lerp(inv(a), inv(b), p);
            Perspective(if v <= 0.0 { None } else { Some(1.0 / v) })
        }
        (x, y) if std::mem::discriminant(x) != std::mem::discriminant(y) => {
            let (px, py) = (promote_3d(x), promote_3d(y));
            if std::mem::discriminant(&px) == std::mem::discriminant(&py) && (&px, &py) != (x, y) {
                return lerp_op(&px, &py, p);
            }
            return None;
        }
        _ => return None,
    })
}

/// 2D affine matrix of a transform list (translations must be in px).
fn op_matrix(ops: &[TransformOp]) -> Option<[f64; 6]> {
    let mut m = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    let mul = |m: [f64; 6], n: [f64; 6]| [m[0] * n[0] + m[2] * n[1], m[1] * n[0] + m[3] * n[1], m[0] * n[2] + m[2] * n[3], m[1] * n[2] + m[3] * n[3], m[0] * n[4] + m[2] * n[5] + m[4], m[1] * n[4] + m[3] * n[5] + m[5]];
    for op in ops {
        let n = match op {
            TransformOp::Translate(Len::Px(x), Len::Px(y)) => [1.0, 0.0, 0.0, 1.0, *x as f64, *y as f64],
            TransformOp::Translate(..) => return None,
            TransformOp::Scale(x, y) => [*x as f64, 0.0, 0.0, *y as f64, 0.0, 0.0],
            TransformOp::Rotate(d) => {
                let r = (*d as f64).to_radians();
                [r.cos(), r.sin(), -r.sin(), r.cos(), 0.0, 0.0]
            }
            TransformOp::Skew(x, y) => [1.0, (*y as f64).to_radians().tan(), (*x as f64).to_radians().tan(), 1.0, 0.0, 0.0],
            TransformOp::Matrix(v) => [v[0] as f64, v[1] as f64, v[2] as f64, v[3] as f64, v[4] as f64, v[5] as f64],
            TransformOp::Translate3d(..) | TransformOp::Scale3d(..) | TransformOp::Rotate3d(..) | TransformOp::Perspective(_) | TransformOp::Matrix3d(_) => return None,
        };
        m = mul(m, n);
    }
    Some(m)
}

/// CSS Transforms 1 §decomposing-a-2d-matrix: translate, rotate (deg), scale, skew, and back.
fn decompose(m: [f64; 6]) -> [f64; 6] {
    let (mut a, mut b, mut c, mut d) = (m[0], m[1], m[2], m[3]);
    let mut sx = (a * a + b * b).sqrt();
    let mut sy = (c * c + d * d).sqrt();
    if a * d - b * c < 0.0 {
        if a < d {
            sx = -sx;
        } else {
            sy = -sy;
        }
    }
    if sx != 0.0 {
        a /= sx;
        b /= sx;
    }
    if sy != 0.0 {
        c /= sy;
        d /= sy;
    }
    let mut skew = a * c + b * d;
    c -= a * skew;
    d -= b * skew;
    let angle = b.atan2(a).to_degrees();
    let _ = (c, d);
    if sy != 0.0 {
        skew /= sy;
    }
    [m[4], m[5], angle, sx, sy, skew]
}

fn recompose(v: [f64; 6]) -> [f32; 6] {
    let r = v[2].to_radians();
    let (cs, sn) = (r.cos(), r.sin());
    // translate · rotate · skew(xy) · scale
    let (a, b, c, d) = (cs, sn, -sn, cs);
    let (c, d) = (c + a * v[5], d + b * v[5]);
    [(a * v[3]) as f32, (b * v[3]) as f32, (c * v[4]) as f32, (d * v[4]) as f32, v[0] as f32, v[1] as f32]
}

fn lerp_transform(a: &Option<Rc<Vec<TransformOp>>>, b: &Option<Rc<Vec<TransformOp>>>, p: f64) -> Option<Rc<Vec<TransformOp>>> {
    let empty = Vec::new();
    let la: &Vec<TransformOp> = a.as_deref().unwrap_or(&empty);
    let lb: &Vec<TransformOp> = b.as_deref().unwrap_or(&empty);
    if la.is_empty() && lb.is_empty() {
        return None;
    }
    // Same function list (or none against a list): interpolate function by function.
    // Lists sharing their leading functions: the shorter one is padded with identities.
    let same_kind = |x: &TransformOp, y: &TransformOp| std::mem::discriminant(&promote_3d(x)) == std::mem::discriminant(&promote_3d(y));
    let (fa, fb): (Vec<TransformOp>, Vec<TransformOp>) = if la.len() != lb.len() && la.iter().zip(lb.iter()).all(|(x, y)| same_kind(x, y)) {
        let (mut a2, mut b2) = (la.clone(), lb.clone());
        if a2.len() < b2.len() {
            a2.extend(lb[la.len()..].iter().map(identity_op));
        } else {
            b2.extend(la[lb.len()..].iter().map(identity_op));
        }
        (a2, b2)
    } else {
        (la.clone(), lb.clone())
    };
    if fa.len() == fb.len() {
        let ops: Option<Vec<TransformOp>> = fa.iter().zip(&fb).map(|(x, y)| lerp_op(x, y, p)).collect();
        if let Some(ops) = ops {
            return Some(Rc::new(ops));
        }
    }
    // Otherwise interpolate the decomposed matrices.
    match (op_matrix(la), op_matrix(lb)) {
        (Some(ma), Some(mb)) => {
            let (mut da, db) = (decompose(ma), decompose(mb));
            // Rotate the short way round.
            if (da[2] - db[2]).abs() > 180.0 {
                if da[2] > db[2] {
                    da[2] -= 360.0;
                } else {
                    da[2] += 360.0;
                }
            }
            let mut v = [0.0; 6];
            for i in 0..6 {
                v[i] = da[i] + (db[i] - da[i]) * p;
            }
            Some(Rc::new(vec![TransformOp::Matrix(recompose(v))]))
        }
        _ => if p < 0.5 { a.clone() } else { b.clone() },
    }
}

fn lerp_shadows(a: &Option<Rc<Vec<Shadow>>>, b: &Option<Rc<Vec<Shadow>>>, p: f64, current: Color) -> Option<Option<Rc<Vec<Shadow>>>> {
    let empty = Vec::new();
    let la = a.as_deref().unwrap_or(&empty);
    let lb = b.as_deref().unwrap_or(&empty);
    let n = la.len().max(lb.len());
    if n == 0 {
        return Some(None);
    }
    let blank = |s: &Shadow| Shadow { inset: s.inset, x: 0.0, y: 0.0, blur: 0.0, spread: 0.0, color: ColorValue::Rgba(Color { r: 0, g: 0, b: 0, a: 0 }) };
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let (sa, sb) = match (la.get(i), lb.get(i)) {
            (Some(x), Some(y)) => (x.clone(), y.clone()),
            (Some(x), None) => (x.clone(), blank(x)),
            (None, Some(y)) => (blank(y), y.clone()),
            _ => unreachable!(),
        };
        if sa.inset != sb.inset {
            return None;
        }
        out.push(Shadow {
            inset: sa.inset,
            x: lerp(sa.x, sb.x, p),
            y: lerp(sa.y, sb.y, p),
            blur: lerp(sa.blur, sb.blur, p).max(0.0),
            spread: lerp(sa.spread, sb.spread, p),
            color: ColorValue::Rgba(lerp_color(sa.color.resolve(current), sb.color.resolve(current), p)),
        });
    }
    Some(Some(Rc::new(out)))
}

fn filter_identity(f: &FilterOp) -> FilterOp {
    use FilterOp::*;
    match f {
        Blur(_) => Blur(0.0),
        Brightness(_) => Brightness(1.0),
        Contrast(_) => Contrast(1.0),
        Grayscale(_) => Grayscale(0.0),
        HueRotate(_) => HueRotate(0.0),
        Invert(_) => Invert(0.0),
        Opacity(_) => Opacity(1.0),
        Saturate(_) => Saturate(1.0),
        Sepia(_) => Sepia(0.0),
        DropShadow { .. } => DropShadow { x: 0.0, y: 0.0, blur: 0.0, color: ColorValue::Rgba(Color { r: 0, g: 0, b: 0, a: 0 }) },
    }
}

fn lerp_filters(a: &Option<Rc<Vec<FilterOp>>>, b: &Option<Rc<Vec<FilterOp>>>, p: f64, current: Color) -> Option<Option<Rc<Vec<FilterOp>>>> {
    use FilterOp::*;
    let empty = Vec::new();
    let la = a.as_deref().unwrap_or(&empty);
    let lb = b.as_deref().unwrap_or(&empty);
    let same_kind = |x: &FilterOp, y: &FilterOp| std::mem::discriminant(x) == std::mem::discriminant(y);
    let (fa, fb): (Vec<FilterOp>, Vec<FilterOp>) = if la.iter().zip(lb.iter()).all(|(x, y)| same_kind(x, y)) {
        let (mut a2, mut b2) = (la.clone(), lb.clone());
        if a2.len() < b2.len() {
            a2.extend(lb[la.len()..].iter().map(filter_identity));
        } else {
            b2.extend(la[lb.len()..].iter().map(filter_identity));
        }
        (a2, b2)
    } else {
        (la.clone(), lb.clone())
    };
    if fa.len() != fb.len() {
        return None;
    }
    let mut out = Vec::new();
    for (x, y) in fa.iter().zip(&fb) {
        out.push(match (x, y) {
            (Blur(a), Blur(b)) => Blur(lerp(*a, *b, p).max(0.0)),
            (Brightness(a), Brightness(b)) => Brightness(lerp(*a, *b, p).max(0.0)),
            (Contrast(a), Contrast(b)) => Contrast(lerp(*a, *b, p).max(0.0)),
            (Grayscale(a), Grayscale(b)) => Grayscale(lerp(*a, *b, p).clamp(0.0, 1.0)),
            (HueRotate(a), HueRotate(b)) => HueRotate(lerp(*a, *b, p)),
            (Invert(a), Invert(b)) => Invert(lerp(*a, *b, p).clamp(0.0, 1.0)),
            (Opacity(a), Opacity(b)) => Opacity(lerp(*a, *b, p).clamp(0.0, 1.0)),
            (Saturate(a), Saturate(b)) => Saturate(lerp(*a, *b, p).max(0.0)),
            (Sepia(a), Sepia(b)) => Sepia(lerp(*a, *b, p).clamp(0.0, 1.0)),
            (DropShadow { x: ax, y: ay, blur: ab, color: ac }, DropShadow { x: bx, y: by, blur: bb, color: bc }) => DropShadow {
                x: lerp(*ax, *bx, p),
                y: lerp(*ay, *by, p),
                blur: lerp(*ab, *bb, p).max(0.0),
                color: ColorValue::Rgba(lerp_color(ac.resolve(current), bc.resolve(current), p)),
            },
            _ => return None,
        });
    }
    Some(Some(Rc::new(out)))
}

/// Interpolate two values of a property; `None` when they can only flip discretely.
fn interpolate(a: &AV, b: &AV, p: f64, current: Color) -> Option<AV> {
    Some(match (a, b) {
        (AV::Num(x), AV::Num(y)) => AV::Num(lerp(*x, *y, p)),
        (AV::Len(x), AV::Len(y)) => AV::Len(lerp_len(x, y, p)?),
        (AV::Color(x), AV::Color(y)) => AV::Color(lerp_color(*x, *y, p)),
        (AV::Pair(a1, a2), AV::Pair(b1, b2)) => AV::Pair(lerp_len(a1, b1, p)?, lerp_len(a2, b2, p)?),
        (AV::Transform(x), AV::Transform(y)) => AV::Transform(lerp_transform(x, y, p)),
        (AV::Shadows(x), AV::Shadows(y)) => AV::Shadows(lerp_shadows(x, y, p, current)?),
        (AV::Filter(x), AV::Filter(y)) => AV::Filter(lerp_filters(x, y, p, current)?),
        (AV::Vis(x), AV::Vis(y)) => {
            // Visible throughout the interpolation if either end is visible.
            if *x == Visibility::Visible || *y == Visibility::Visible {
                AV::Vis(if p <= 0.0 { *x } else if p >= 1.0 { *y } else { Visibility::Visible })
            } else {
                return None;
            }
        }
        (AV::Z(Some(x)), AV::Z(Some(y))) => AV::Z(Some(lerp(*x as f32, *y as f32, p).round() as i32)),
        (AV::Lh(LineHeight::Px(x)), AV::Lh(LineHeight::Px(y))) => AV::Lh(LineHeight::Px(lerp(*x, *y, p))),
        (AV::Lh(LineHeight::Number(x)), AV::Lh(LineHeight::Number(y))) => AV::Lh(LineHeight::Number(lerp(*x, *y, p))),
        (AV::Translate(x), AV::Translate(y)) => {
            let z = (Len::Px(0.0), Len::Px(0.0), 0.0);
            let (x, y) = (x.clone().unwrap_or(z.clone()), y.clone().unwrap_or(z));
            AV::Translate(Some((lerp_len(&x.0, &y.0, p)?, lerp_len(&x.1, &y.1, p)?, lerp(x.2, y.2, p))))
        }
        (AV::Rotate(Some(x)), AV::Rotate(Some(y))) if !same_axis(x, y) && x.3 != 0.0 && y.3 != 0.0 => AV::Rotate(Some(slerp_rotate(x, y, p))),
        (AV::Rotate(x), AV::Rotate(y)) => {
            // Interpolate the angle about the (destination's, else source's) axis.
            let ax = match (x, y) {
                (_, Some(b)) if b.3 != 0.0 || x.is_none() => (b.0, b.1, b.2),
                (Some(a), _) => (a.0, a.1, a.2),
                (None, Some(b)) => (b.0, b.1, b.2),
                (None, None) => (0.0, 0.0, 1.0),
            };
            let n = norm3(ax.0 as f64, ax.1 as f64, ax.2 as f64);
            AV::Rotate(Some((n.0 as f32, n.1 as f32, n.2 as f32, lerp(x.map(|v| v.3).unwrap_or(0.0), y.map(|v| v.3).unwrap_or(0.0), p))))
        }
        (AV::Scale(x), AV::Scale(y)) => {
            let (x, y) = (x.unwrap_or((1.0, 1.0, 1.0)), y.unwrap_or((1.0, 1.0, 1.0)));
            AV::Scale(Some((lerp(x.0, y.0, p), lerp(x.1, y.1, p), lerp(x.2, y.2, p))))
        }
        (AV::Paint(Some(SvgPaint::Color(x))), AV::Paint(Some(SvgPaint::Color(y)))) => AV::Paint(Some(SvgPaint::Color(ColorValue::Rgba(lerp_color(x.resolve(current), y.resolve(current), p))))),
        (AV::Raw(Some(x)), AV::Raw(Some(y))) => AV::Raw(Some(lerp_raw(x, y, p)?)),
        (AV::BgPos(x), AV::BgPos(y)) if x.len() == y.len() && !x.is_empty() => {
            let v: Option<Vec<(Len, Len)>> = x.iter().zip(y).map(|(a, b)| Some((lerp_len(&a.0, &b.0, p)?, lerp_len(&a.1, &b.1, p)?))).collect();
            AV::BgPos(v?)
        }
        _ => return None,
    })
}

fn norm3(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let l = (x * x + y * y + z * z).sqrt();
    if l == 0.0 {
        (0.0, 0.0, 1.0)
    } else {
        (x / l, y / l, z / l)
    }
}

fn same_axis(a: &(f32, f32, f32, f32), b: &(f32, f32, f32, f32)) -> bool {
    let (p, q) = (norm3(a.0 as f64, a.1 as f64, a.2 as f64), norm3(b.0 as f64, b.1 as f64, b.2 as f64));
    (p.0 - q.0).abs() < 1e-6 && (p.1 - q.1).abs() < 1e-6 && (p.2 - q.2).abs() < 1e-6
}

/// Rotations about different axes: quaternion slerp (as Blink), back to an
/// axis (normalized) and angle.
fn slerp_rotate(a: &(f32, f32, f32, f32), b: &(f32, f32, f32, f32), t: f64) -> (f32, f32, f32, f32) {
    let quat = |r: &(f32, f32, f32, f32)| {
        let (x, y, z) = norm3(r.0 as f64, r.1 as f64, r.2 as f64);
        let h = (r.3 as f64).to_radians() / 2.0;
        (x * h.sin(), y * h.sin(), z * h.sin(), h.cos())
    };
    let (qa, qb) = (quat(a), quat(b));
    let dot = (qa.0 * qb.0 + qa.1 * qb.1 + qa.2 * qb.2 + qa.3 * qb.3).clamp(-1.0, 1.0);
    let q = if (dot - 1.0).abs() < 1e-5 || (dot + 1.0).abs() < 1e-5 {
        qa
    } else {
        let denom = (1.0 - dot * dot).sqrt();
        let theta = dot.acos();
        let w = (t * theta).sin() / denom;
        let s1 = (t * theta).cos() - dot * w;
        (s1 * qa.0 + w * qb.0, s1 * qa.1 + w * qb.1, s1 * qa.2 + w * qb.2, s1 * qa.3 + w * qb.3)
    };
    let angle = 2.0 * q.3.clamp(-1.0, 1.0).acos();
    let s = (angle / 2.0).sin();
    let (x, y, z) = if s.abs() < 1e-9 { (0.0, 0.0, 1.0) } else { norm3(q.0 / s, q.1 / s, q.2 / s) };
    (x as f32, y as f32, z as f32, angle.to_degrees() as f32)
}

/// Interpolate SVG property text: numbers (with an optional shared unit), or
/// equal-length lists of them (`stroke-dasharray`).
fn lerp_raw(a: &str, b: &str, p: f64) -> Option<Rc<str>> {
    fn parts(s: &str) -> Option<Vec<(f32, String)>> {
        s.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .map(|t| {
                let end = t.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E')).unwrap_or(t.len());
                let (n, u) = t.split_at(end);
                n.parse::<f32>().ok().map(|n| (n, u.to_ascii_lowercase()))
            })
            .collect()
    }
    let (x, y) = (parts(a)?, parts(b)?);
    if x.is_empty() || x.len() != y.len() {
        return None;
    }
    let mut out = Vec::with_capacity(x.len());
    for ((xn, xu), (yn, yu)) in x.iter().zip(&y) {
        let unit = match (xu.as_str(), yu.as_str()) {
            (u, v) if u == v => u,
            ("", "px") | ("px", "") => "px",
            _ => return None,
        };
        out.push(format!("{}{}", lerp(*xn, *yn, p), unit));
    }
    Some(Rc::from(out.join(" ")))
}

fn add_len(a: &Len, b: &Len) -> Option<Len> {
    let parts = |l: &Len| match l {
        Len::Px(v) => Some((*v, 0.0, 0u8)),
        Len::Pct(v) => Some((0.0, *v, 1u8)),
        Len::Calc(px, pc) => Some((*px, *pc, 2u8)),
        _ => None,
    };
    let (a, b) = (parts(a)?, parts(b)?);
    Some(match (a.2, b.2) {
        (0, 0) => Len::Px(a.0 + b.0),
        (1, 1) => Len::Pct(a.1 + b.1),
        _ => Len::Calc(a.0 + b.0, a.1 + b.1),
    })
}

/// Value addition (`composite: add` / `accumulate`) of an underlying value
/// `u` and an effect value `v`; None when the type doesn't add (the effect
/// value replaces).
fn add_av(u: &AV, v: &AV, acc: bool) -> Option<AV> {
    Some(match (u, v) {
        (AV::Num(a), AV::Num(b)) => AV::Num(a + b),
        (AV::Len(a), AV::Len(b)) => AV::Len(add_len(a, b)?),
        (AV::Pair(a1, a2), AV::Pair(b1, b2)) => AV::Pair(add_len(a1, b1)?, add_len(a2, b2)?),
        (AV::Color(a), AV::Color(b)) => {
            // Premultiplied sum, alpha clamped.
            let (aa, ba) = (a.a as f64 / 255.0, b.a as f64 / 255.0);
            let alpha = (aa + ba).min(1.0);
            let ch = |x: u8, y: u8| if alpha > 0.0 { ((x as f64 * aa + y as f64 * ba) / alpha).round().clamp(0.0, 255.0) as u8 } else { 0 };
            AV::Color(Color { r: ch(a.r, b.r), g: ch(a.g, b.g), b: ch(a.b, b.b), a: (alpha * 255.0).round() as u8 })
        }
        (AV::Z(Some(a)), AV::Z(Some(b))) => AV::Z(Some(a + b)),
        (AV::Lh(LineHeight::Px(a)), AV::Lh(LineHeight::Px(b))) => AV::Lh(LineHeight::Px(a + b)),
        (AV::Lh(LineHeight::Number(a)), AV::Lh(LineHeight::Number(b))) => AV::Lh(LineHeight::Number(a + b)),
        (AV::Transform(a), AV::Transform(b)) => match (a, b) {
            (None, _) => v.clone(),
            (_, None) => u.clone(),
            // Accumulate: matching functions combine argument-wise.
            (Some(x), Some(y)) if acc && x.len() == y.len() => {
                let mut out = Vec::with_capacity(x.len());
                for (p, q) in x.iter().zip(y.iter()) {
                    out.push(match (p, q) {
                        (TransformOp::Translate(a1, a2), TransformOp::Translate(b1, b2)) => TransformOp::Translate(add_len(a1, b1)?, add_len(a2, b2)?),
                        (TransformOp::Translate3d(a1, a2, a3), TransformOp::Translate3d(b1, b2, b3)) => TransformOp::Translate3d(add_len(a1, b1)?, add_len(a2, b2)?, a3 + b3),
                        (TransformOp::Scale(a1, a2), TransformOp::Scale(b1, b2)) => TransformOp::Scale(a1 + b1 - 1.0, a2 + b2 - 1.0),
                        (TransformOp::Scale3d(a1, a2, a3), TransformOp::Scale3d(b1, b2, b3)) => TransformOp::Scale3d(a1 + b1 - 1.0, a2 + b2 - 1.0, a3 + b3 - 1.0),
                        (TransformOp::Rotate(a1), TransformOp::Rotate(b1)) => TransformOp::Rotate(a1 + b1),
                        (TransformOp::Skew(a1, a2), TransformOp::Skew(b1, b2)) => TransformOp::Skew(a1 + b1, a2 + b2),
                        (TransformOp::Rotate3d(x1, y1, z1, a1), TransformOp::Rotate3d(x2, y2, z2, a2)) if (x1, y1, z1) == (x2, y2, z2) => TransformOp::Rotate3d(*x1, *y1, *z1, a1 + a2),
                        _ => return Some(accumulate_matrices(x, y, v)),
                    });
                }
                AV::Transform(Some(Rc::new(out)))
            }
            (Some(x), Some(y)) if acc => accumulate_matrices(x, y, v),
            (Some(x), Some(y)) => AV::Transform(Some(Rc::new(x.iter().chain(y.iter()).cloned().collect()))),
        },
        (AV::Shadows(a), AV::Shadows(b)) if !acc => match (a, b) {
            (None, _) => v.clone(),
            (_, None) => u.clone(),
            (Some(x), Some(y)) => AV::Shadows(Some(Rc::new(x.iter().chain(y.iter()).cloned().collect()))),
        },
        (AV::Filter(a), AV::Filter(b)) if !acc => match (a, b) {
            (None, _) => v.clone(),
            (_, None) => u.clone(),
            (Some(x), Some(y)) => AV::Filter(Some(Rc::new(x.iter().chain(y.iter()).cloned().collect()))),
        },
        (AV::BgPos(a), AV::BgPos(b)) if !a.is_empty() && !b.is_empty() => {
            let n = b.len().max(a.len());
            let mut out = Vec::with_capacity(n);
            for i in 0..n {
                let (x, y) = (&a[i % a.len()], &b[i % b.len()]);
                out.push((add_len(&x.0, &y.0)?, add_len(&x.1, &y.1)?));
            }
            AV::BgPos(out)
        }
        (AV::Translate(a), AV::Translate(b)) => match (a, b) {
            (None, _) => v.clone(),
            (_, None) => u.clone(),
            (Some(x), Some(y)) => AV::Translate(Some((add_len(&x.0, &y.0)?, add_len(&x.1, &y.1)?, x.2 + y.2))),
        },
        (AV::Scale(a), AV::Scale(b)) => match (a, b) {
            (None, _) => v.clone(),
            (_, None) => u.clone(),
            (Some(x), Some(y)) if acc => AV::Scale(Some((x.0 + y.0 - 1.0, x.1 + y.1 - 1.0, x.2 + y.2 - 1.0))),
            (Some(x), Some(y)) => AV::Scale(Some((x.0 * y.0, x.1 * y.1, x.2 * y.2))),
        },
        (AV::Rotate(a), AV::Rotate(b)) => match (a, b) {
            (None, _) => v.clone(),
            (_, None) => u.clone(),
            (Some(x), Some(y)) if (x.0, x.1, x.2) == (y.0, y.1, y.2) => AV::Rotate(Some((x.0, x.1, x.2, x.3 + y.3))),
            _ => return None,
        },
        _ => return None,
    })
}

/// Accumulation of transform lists that don't combine function by function:
/// their 2D matrices decomposed, the components accumulated and recomposed.
/// A non-invertible matrix can't be decomposed: the effect value replaces.
fn accumulate_matrices(x: &[TransformOp], y: &[TransformOp], v: &AV) -> AV {
    match (op_matrix(x), op_matrix(y)) {
        (Some(mx), Some(my)) => {
            let det = |m: [f64; 6]| m[0] * m[3] - m[1] * m[2];
            if det(mx).abs() < 1e-12 || det(my).abs() < 1e-12 {
                return v.clone();
            }
            let (dx, dy) = (decompose(mx), decompose(my));
            let r = [dx[0] + dy[0], dx[1] + dy[1], dx[2] + dy[2], dx[3] + dy[3] - 1.0, dx[4] + dy[4] - 1.0, dx[5] + dy[5]];
            AV::Transform(Some(Rc::new(vec![TransformOp::Matrix(recompose(r))])))
        }
        _ => AV::Transform(Some(Rc::new(x.iter().chain(y.iter()).cloned().collect()))),
    }
}

fn discrete(a: &AV, b: &AV, p: f64) -> AV {
    if p < 0.5 {
        a.clone()
    } else {
        b.clone()
    }
}

// ------------------------------------------------------------------ timing model

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Before,
    Active,
    After,
}

/// Web Animations timing: (phase, current iteration, directed progress or None
/// when the effect doesn't apply).
pub fn progress(t: &AnimTiming, local: f64) -> (Phase, f64, Option<f64>) {
    let dur = t.duration.max(0.0);
    let iters = t.iterations.max(0.0);
    let active = if dur == 0.0 || iters == 0.0 { 0.0 } else { dur * iters };
    let end = (t.delay + active + t.end_delay).max(0.0);
    let before_end = t.delay.max(0.0).min(end);
    let active_after = (t.delay + active).max(0.0).min(end);
    let phase = if local < before_end || (local == before_end && t.delay > 0.0 && false) {
        Phase::Before
    } else if local >= active_after && !(active.is_infinite()) && local >= t.delay + active {
        Phase::After
    } else {
        Phase::Active
    };
    let active_time = match phase {
        Phase::Before => {
            if matches!(t.fill, Fill::Backwards | Fill::Both) {
                Some((local - t.delay).max(0.0))
            } else {
                None
            }
        }
        Phase::Active => Some(local - t.delay),
        Phase::After => {
            if matches!(t.fill, Fill::Forwards | Fill::Both) {
                Some((local - t.delay).max(0.0).min(active))
            } else {
                None
            }
        }
    };
    let Some(at) = active_time else { return (phase, 0.0, None) };
    let overall = if dur == 0.0 { if phase == Phase::Before { 0.0 } else { iters } } else { at / dur } + t.iteration_start;
    let mut simple = if overall.is_infinite() { t.iteration_start % 1.0 } else { overall % 1.0 };
    if simple == 0.0 && (phase == Phase::After || (dur == 0.0 && phase != Phase::Before)) && iters != 0.0 && overall != 0.0 {
        simple = 1.0;
    }
    let mut iteration = if phase == Phase::After && iters.is_infinite() { f64::INFINITY } else if simple == 1.0 { overall.floor() - 1.0 } else { overall.floor() };
    if iteration < 0.0 {
        iteration = 0.0;
    }
    let forwards = match t.direction {
        Direction::Normal => true,
        Direction::Reverse => false,
        Direction::Alternate => iteration % 2.0 == 0.0 || iteration.is_infinite(),
        Direction::AlternateReverse => !(iteration % 2.0 == 0.0 || iteration.is_infinite()),
    };
    let directed = if forwards { simple } else { 1.0 - simple };
    (phase, iteration, Some(directed))
}

// ------------------------------------------------------------------ engine

/// A Web Animations (`element.animate()`) animation.
#[derive(Clone, Debug)]
pub struct ScriptAnim {
    pub id: u64,
    pub pseudo: u8,
    pub keyframes: Rc<Keyframes>,
    pub timing: AnimTiming,
    /// Timeline time of local 0; `None` while paused/held.
    pub start: Option<f64>,
    /// Local time held while paused.
    pub hold: Option<f64>,
    pub rate: f64,
}

#[derive(Clone, Debug)]
struct RunningCss {
    id: u64,
    name: Rc<str>,
    start: f64,
    paused_at: Option<f64>,
    phase: Option<Phase>,
    iteration: f64,
    timing: Option<AnimTiming>,
    /// Script's play()/pause() (overrides animation-play-state from then on).
    play_override: Option<bool>,
    /// Canceled by script: no effect until the name stops applying.
    canceled: bool,
}

/// A running CSS animation or transition as script sees it (CSSAnimation / CSSTransition).
#[derive(Clone, Debug)]
pub struct CssAnimInfo {
    pub id: u64,
    pub node: NodeId,
    /// false: CSS animation; true: CSS transition.
    pub transition: bool,
    pub name: String,
    pub start: f64,
    pub paused_at: Option<f64>,
    pub delay: f64,
    pub duration: f64,
    pub iterations: f64,
    pub direction: &'static str,
    pub fill: &'static str,
    pub easing: String,
}

#[derive(Clone, Debug)]
struct RunningTransition {
    id: u64,
    prop: Prop,
    name: Rc<str>,
    from: AV,
    to: AV,
    start: f64,
    delay: f64,
    duration: f64,
    timing: Timing,
    started: bool,
    /// The engine longhand and its endpoints (getComputedStyle).
    gen: Option<(&'static str, GenVal, GenVal)>,
    /// Full-precision endpoints of a color property (when either isn't a legacy color).
    hp: Option<(super::color::Abs, super::color::Abs)>,
}

/// The color property a native animated field reports.
fn color_prop_name(p: Prop) -> Option<&'static str> {
    Some(match p {
        Prop::Color | Prop::BackgroundColor | Prop::BorderColor(_) | Prop::OutlineColor | Prop::TextDecorationColor | Prop::CaretColor => prop_name(p),
        _ => return None,
    })
}

fn legacy_abs(c: Color) -> super::color::Abs {
    super::color::Abs::srgb(c.r as f64 / 255.0, c.g as f64 / 255.0, c.b as f64 / 255.0, c.a as f64 / 255.0)
}

/// A color property's full-precision value, when it isn't a plain legacy color.
fn hp_abs(s: &ComputedStyle, name: &str) -> Option<super::color::Abs> {
    let h = s.color_hp.as_ref()?;
    let (_, spec) = h.iter().find(|(n, _)| *n == name)?;
    let current = match h.iter().find(|(n, _)| *n == "color") {
        Some((_, c)) if name != "color" => super::color::compute(c, &legacy_abs(s.color), s.dark_scheme).unwrap_or_else(|| legacy_abs(s.color)),
        _ => legacy_abs(s.color),
    };
    super::color::compute(spec, &current, s.dark_scheme)
}

/// Endpoints for full-precision color interpolation (None: both legacy).
fn hp_pair(sa: &ComputedStyle, sb: &ComputedStyle, p: Prop) -> Option<(super::color::Abs, super::color::Abs)> {
    let name = color_prop_name(p)?;
    if !matches!(get(sa, p), AV::Color(_)) || !matches!(get(sb, p), AV::Color(_)) {
        return None;
    }
    let (ha, hb) = (hp_abs(sa, name), hp_abs(sb, name));
    if ha.is_none() && hb.is_none() {
        return None;
    }
    let av = |s: &ComputedStyle| match get(s, p) {
        AV::Color(c) => legacy_abs(c),
        _ => legacy_abs(Color { r: 0, g: 0, b: 0, a: 0 }),
    };
    Some((ha.unwrap_or_else(|| av(sa)), hb.unwrap_or_else(|| av(sb))))
}

/// Drop a color property's specified high-precision color (the value engine reports it).
fn clear_hp(s: &mut ComputedStyle, name: &str) {
    if s.color_hp.as_ref().map(|h| h.iter().any(|(n, _)| *n == name)).unwrap_or(false) {
        let l: Vec<(&'static str, Rc<super::color::Spec>)> = s.color_hp.as_ref().map(|h| h.iter().filter(|(n, _)| *n != name).cloned().collect()).unwrap_or_default();
        s.color_hp = if l.is_empty() { None } else { Some(Rc::new(l)) };
    }
}

/// Record (or clear) the full-precision animated value of a color property.
fn set_hp(s: &mut ComputedStyle, p: Prop, v: Option<super::color::Abs>) {
    let Some(name) = color_prop_name(p) else { return };
    let mut l: Vec<(&'static str, Rc<super::color::Spec>)> = s.color_hp.as_ref().map(|h| (**h).clone()).unwrap_or_default();
    l.retain(|(n, _)| *n != name);
    if let Some(a) = v {
        l.push((name, Rc::new(super::color::Spec::Abs(a))));
    }
    s.color_hp = if l.is_empty() { None } else { Some(Rc::new(l)) };
}

/// A transition of a property only the value engine models.
#[derive(Clone, Debug)]
struct GenTransition {
    id: u64,
    prop: &'static str,
    name: Rc<str>,
    from: GenVal,
    to: GenVal,
    /// The computed end value (to detect a changed target).
    to_v: Option<pv::V>,
    start: f64,
    delay: f64,
    duration: f64,
    timing: Timing,
    started: bool,
}

#[derive(Default, Debug)]
struct NodeAnims {
    /// Timeline time of the pass that last applied these (its style holds
    /// the animated values for that instant).
    applied_at: Option<f64>,
    css: Vec<RunningCss>,
    trans: Vec<RunningTransition>,
    gtrans: Vec<GenTransition>,
}

/// The engine longhand mirroring a native animated property, unless its
/// native interpolation is the better one to report.
fn gen_key(p: Prop) -> Option<&'static str> {
    let n = prop_name(p);
    if matches!(n, "translate" | "rotate" | "scale") {
        return None;
    }
    let k = pv::engine_routed(n)?;
    pv::shorthand::longhands(k).is_none().then_some(k)
}

/// Longhands of a keyframe/transition property name that the engine
/// animates on its own (no native animated field).
fn gen_longhands(name: &str) -> Vec<&'static str> {
    let ls = match pv::engine_routed(name) {
        Some(k) => pv::shorthand::longhands(k).unwrap_or_else(|| vec![k]),
        // A natively handled shorthand (font, border, …): its engine longhands.
        None => match pv::shorthand::longhands(&pv::db::canonical_name(name)) {
            Some(l) => l.into_iter().filter(|x| pv::engine_routed(x).is_some()).collect(),
            None => return vec![],
        },
    };
    ls.into_iter().filter(|l| props_of(l).is_empty() && pv::interp::animatable(l) && pv::engine_routed(l).is_some()).collect()
}

#[derive(Clone, Debug)]
pub struct AnimEvent {
    pub node: NodeId,
    pub kind: &'static str,
    pub name: String,
    pub elapsed: f64,
}

#[derive(Default)]
pub struct AnimEngine {
    /// Document timeline time (ms).
    pub now: f64,
    nodes: HashMap<NodeId, NodeAnims>,
    pub script: HashMap<NodeId, Vec<ScriptAnim>>,
    seen: HashSet<NodeId>,
    /// Script seeked/played/paused/canceled a CSS animation since the last pass.
    touched: bool,
    pub events: Vec<AnimEvent>,
    /// Some animation or transition is in progress: produce another frame.
    pub running: bool,
    /// Earliest future time a delayed effect starts.
    pub next_wake: Option<f64>,
    /// Animated properties of the last pass only affect painting.
    pub paint_only: bool,
    /// Base styles of the animated elements of the last full pass.
    pub bases: HashMap<NodeId, AnimBase>,
    /// Ids for running CSS animations and transitions (script identity).
    next_id: u64,
}

/// Properties whose animation doesn't change layout.
fn paint_only(p: Prop) -> bool {
    matches!(
        p,
        Prop::Opacity
            | Prop::Transform
            | Prop::Translate
            | Prop::Rotate
            | Prop::Scale
            | Prop::TransformOrigin
            | Prop::BackgroundColor
            | Prop::BorderColor(_)
            | Prop::OutlineColor
            | Prop::TextDecorationColor
            | Prop::CaretColor
            | Prop::Fill
            | Prop::Stroke
            | Prop::BoxShadow
            | Prop::Filter
            | Prop::BackgroundPosition
            | Prop::Svg(_)
    )
}

/// What a frame needs to re-animate an element without a full restyle.
#[derive(Clone)]
pub struct AnimBase {
    pub base: Rc<ComputedStyle>,
    pub parent: Rc<ComputedStyle>,
    pub cx: ApplyCtx,
}

/// Style-application context for keyframes.
pub struct KfCtx<'a> {
    pub parent: &'a ComputedStyle,
    pub cx: &'a ApplyCtx,
    pub keyframes: &'a dyn Fn(&str) -> Option<Rc<Keyframes>>,
}

impl AnimEngine {
    pub fn begin(&mut self, now: f64) {
        self.begin_frame(now);
    }

    /// Start a frame that only re-animates the elements in `bases`.
    pub fn begin_frame(&mut self, now: f64) {
        self.now = now;
        self.seen.clear();
        self.running = false;
        self.next_wake = None;
        self.paint_only = true;
    }

    pub fn remember(&mut self, node: NodeId, base: Rc<ComputedStyle>, parent: Rc<ComputedStyle>, cx: &ApplyCtx) {
        self.bases.insert(node, AnimBase { base, parent, cx: cx.clone() });
    }

    pub fn has_work(&self, node: NodeId, s: &ComputedStyle) -> bool {
        s.animation.as_ref().map(|a| a.names.iter().any(|n| &**n != "none")).unwrap_or(false)
            || s.transition.as_ref().map(|t| !t.props.is_empty() && t.durations.iter().any(|d| *d > 0.0)).unwrap_or(false)
            || self.nodes.contains_key(&node)
            || self.script.contains_key(&node)
    }

    /// Does an animation, transition or script animation run on `key` (an
    /// element, or one with `ANIM_BEFORE`/`ANIM_AFTER` set)?
    pub fn tracks(&self, key: NodeId) -> bool {
        self.nodes.contains_key(&key) || self.script.contains_key(&key)
    }

    /// Is everything tracked on `key` settled (finished, paused or canceled
    /// CSS animations; no transitions or script animations)? Its previous
    /// style then still holds: mark it seen so it isn't treated as removed.
    pub fn keep_if_idle(&mut self, key: NodeId) -> bool {
        if self.touched || self.script.contains_key(&key) {
            return false;
        }
        let Some(e) = self.nodes.get(&key) else { return true };
        // Applied at this very instant already (a forced restyle within one
        // task), or settled for good.
        let idle = e.applied_at == Some(self.now) || (e.trans.is_empty() && e.gtrans.is_empty() && e.css.iter().all(|r| r.phase == Some(Phase::After) && r.paused_at.is_none() && r.play_override.is_none()));
        if idle {
            self.seen.insert(key);
        }
        idle
    }

    pub fn describe(&self, key: NodeId) -> String {
        match self.nodes.get(&key) {
            Some(e) => format!("now={} css={:?} trans={} gtrans={} script={}", self.now, e.css.iter().map(|r| (r.name.to_string(), r.phase, r.paused_at, r.canceled, r.start, r.timing.as_ref().map(|t| (t.delay, t.duration, t.iterations)))).collect::<Vec<_>>(), e.trans.len(), e.gtrans.len(), self.script.contains_key(&key)),
            None => format!("none script={}", self.script.contains_key(&key)),
        }
    }

    fn wake_at(&mut self, t: f64) {
        if t > self.now {
            self.next_wake = Some(self.next_wake.map(|w| w.min(t)).unwrap_or(t));
        }
    }

    /// The animated style of `node` given its base (cascaded) style and its
    /// style from the previous pass. `None` when nothing applies.
    pub fn apply(&mut self, node: NodeId, base: &ComputedStyle, prev: Option<&ComputedStyle>, k: &KfCtx) -> Option<ComputedStyle> {
        self.seen.insert(node);
        let now = self.now;
        let mut out: Option<ComputedStyle> = None;
        let mut entry = self.nodes.remove(&node).unwrap_or_default();
        // display:none cancels everything running on the element.
        if base.display == style::Display::None {
            for r in entry.css {
                if r.phase == Some(Phase::Active) {
                    self.events.push(AnimEvent { node, kind: "animationcancel", name: r.name.to_string(), elapsed: 0.0 });
                }
            }
            for t in entry.trans {
                self.events.push(AnimEvent { node, kind: "transitioncancel", name: t.name.to_string(), elapsed: ((now - t.start - t.delay).max(0.0)) / 1000.0 });
            }
            for t in entry.gtrans {
                self.events.push(AnimEvent { node, kind: "transitioncancel", name: t.name.to_string(), elapsed: ((now - t.start - t.delay).max(0.0)) / 1000.0 });
            }
            return None;
        }

        // CSS animations.
        let mut keep: Vec<RunningCss> = Vec::new();
        if let Some(spec) = base.animation.clone() {
            for (i, name) in spec.names.iter().enumerate() {
                if &**name == "none" {
                    continue;
                }
                let Some(kf) = (k.keyframes)(name) else { continue };
                let (timing, paused) = spec.get(i);
                let mut run = match entry.css.iter().position(|r| r.name == *name) {
                    Some(p) => entry.css.remove(p),
                    None => {
                        self.next_id += 1;
                        RunningCss { id: self.next_id, name: name.clone(), start: now, paused_at: None, phase: None, iteration: 0.0, timing: None, play_override: None, canceled: false }
                    }
                };
                run.timing = Some(timing.clone());
                if run.canceled {
                    keep.push(run);
                    continue;
                }
                let paused = run.play_override.map(|p| !p).unwrap_or(paused);
                if paused && run.paused_at.is_none() {
                    run.paused_at = Some(now);
                } else if !paused {
                    if let Some(pa) = run.paused_at.take() {
                        run.start += now - pa;
                    }
                }
                let local = run.paused_at.unwrap_or(now) - run.start;
                let (phase, iteration, prog) = progress(&timing, local);
                self.css_events(node, &mut run, &timing, phase, iteration);
                if phase == Phase::Before {
                    self.wake_at(run.start + timing.delay);
                }
                if !paused && phase == Phase::Active {
                    self.running = true;
                }
                if let Some(p) = prog {
                    let s = out.get_or_insert_with(|| base.clone());
                    self.apply_keyframes(s, base, &kf, p, &timing.timing, k, spec.composite(i));
                }
                keep.push(run);
            }
        }
        // Animations no longer listed are canceled.
        for r in entry.css.drain(..) {
            if r.phase == Some(Phase::Active) {
                self.events.push(AnimEvent { node, kind: "animationcancel", name: r.name.to_string(), elapsed: 0.0 });
            }
        }
        entry.css = keep;

        // Web Animations (composited above CSS animations).
        if let Some(list) = self.script.get(&node).cloned() {
            for a in list.iter().filter(|a| a.pseudo == 0) {
                let local = match (a.hold, a.start) {
                    (Some(h), _) => h,
                    (None, Some(st)) => (now - st) * a.rate,
                    _ => continue,
                };
                let (phase, _, prog) = progress(&a.timing, local);
                if a.hold.is_none() && (phase == Phase::Active || (phase == Phase::Before && a.rate > 0.0)) {
                    self.running = true;
                }
                if let Some(p) = prog {
                    let s = out.get_or_insert_with(|| base.clone());
                    self.apply_keyframes(s, base, &a.keyframes, p, &a.timing.timing, k, a.keyframes.composite.unwrap_or(Composite::Replace));
                }
            }
        }

        // CSS transitions (above animations).
        if base.transition.is_some() || !entry.trans.is_empty() || !entry.gtrans.is_empty() {
            let current = out.as_ref().unwrap_or(base).color;
            if let (Some(spec), Some(prev)) = (base.transition.clone(), prev.filter(|p| p.display != style::Display::None)) {
                for (i, pname) in spec.props.iter().enumerate() {
                    // A property listed again later uses the later entry.
                    if spec.props[i + 1..].iter().any(|q| q == pname) {
                        continue;
                    }
                    let c = |n: usize| if n == 0 { 0 } else { i % n };
                    let dur = spec.durations.get(c(spec.durations.len())).copied().unwrap_or(0.0);
                    let delay = spec.delays.get(c(spec.delays.len())).copied().unwrap_or(0.0);
                    let timing = spec.timings.get(c(spec.timings.len())).cloned().unwrap_or(Timing::EASE);
                    let allow_discrete = spec.allow_discrete.get(c(spec.allow_discrete.len())).copied().unwrap_or(false);
                    if dur + delay.max(0.0) <= 0.0 && dur <= 0.0 {
                        continue;
                    }
                    for prop in props_of(pname) {
                        // Later explicit entries win over `all`.
                        if &**pname == "all" && spec.props[i + 1..].iter().any(|q| props_of(q).contains(&prop)) {
                            continue;
                        }
                        // Values tracking currentColor follow the `color` transition.
                        if follows_current_color(prev, prop) && follows_current_color(base, prop) {
                            continue;
                        }
                        let to = get(base, prop);
                        let gkey = gen_key(prop);
                        if let Some(r) = entry.trans.iter().position(|t| t.prop == prop) {
                            if entry.trans[r].to == to {
                                continue;
                            }
                            // Retarget from the current value.
                            let old = entry.trans.remove(r);
                            let from = transition_value(&old, now, current);
                            self.events.push(AnimEvent { node, kind: "transitioncancel", name: old.name.to_string(), elapsed: ((now - old.start - old.delay).max(0.0)) / 1000.0 });
                            if from != to && (allow_discrete || interpolate(&from, &to, 0.5, current).is_some()) {
                                let name = trans_name(prop);
                                self.events.push(AnimEvent { node, kind: "transitionrun", name: name.to_string(), elapsed: (-delay).max(0.0) / 1000.0 });
                                let gen = match (gkey, &old.gen) {
                                    (Some(k), Some((_, ga, gb))) => Some((k, GenVal::Interp(Rc::new(GenInterp { a: ga.clone(), b: gb.clone(), p: trans_progress(&old, now), native: false })), style::gen_val(base, k))),
                                    _ => None,
                                };
                                let hp = old.hp.as_ref().map(|(x, y)| super::color::interpolate(x, y, trans_progress(&old, now))).map(|c| (c, hp_abs(base, color_prop_name(prop).unwrap_or("")).unwrap_or_else(|| match &to { AV::Color(c) => legacy_abs(*c), _ => legacy_abs(Color { r: 0, g: 0, b: 0, a: 0 }) })));
                                let hp = hp.or_else(|| hp_pair(prev, base, prop));
                                self.next_id += 1;
                                entry.trans.push(RunningTransition { id: self.next_id, prop, name, from, to, start: now, delay, duration: dur, timing: timing.clone(), started: false, gen, hp });
                            }
                            continue;
                        }
                        let from = get(prev, prop);
                        if from == to || (!allow_discrete && interpolate(&from, &to, 0.5, current).is_none()) {
                            continue;
                        }
                        let gen = gkey.map(|k| (k, style::gen_val(prev, k), style::gen_val(base, k)));
                        // The value engine decides interpolability (keywords the native value ignores).
                        if let (Some((gk, ga, gb)), false) = (&gen, allow_discrete) {
                            if ga != gb {
                                let (ea, eb) = (style::gen_env(prev, k.cx), style::gen_env(base, k.cx));
                                if let (Some(va), Some(vb)) = (style::gen_eval(gk, ga, &ea), style::gen_eval(gk, gb, &eb)) {
                                    if pv::interp::interpolate(gk, &va, &vb, 0.5).is_none() {
                                        continue;
                                    }
                                }
                            }
                        }
                        let name = trans_name(prop);
                        self.events.push(AnimEvent { node, kind: "transitionrun", name: name.to_string(), elapsed: (-delay).max(0.0) / 1000.0 });
                        let hp = hp_pair(prev, base, prop);
                        self.next_id += 1;
                        entry.trans.push(RunningTransition { id: self.next_id, prop, name, from, to, start: now, delay, duration: dur, timing: timing.clone(), started: false, gen, hp });
                    }
                    // Properties only the value engine animates.
                    let same_generic = match (&prev.generic, &base.generic) {
                        (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    };
                    if same_generic && entry.gtrans.is_empty() {
                        continue;
                    }
                    let cands: Vec<&'static str> = if &**pname == "all" {
                        let mut v: Vec<&'static str> = Vec::new();
                        for g in [&prev.generic, &base.generic].into_iter().flatten() {
                            for d in g.iter() {
                                for l in gen_longhands(d.name) {
                                    if !v.contains(&l) {
                                        v.push(l);
                                    }
                                }
                            }
                        }
                        for t in &entry.gtrans {
                            if !v.contains(&t.prop) {
                                v.push(t.prop);
                            }
                        }
                        v.retain(|l| !spec.props[i + 1..].iter().any(|q| gen_longhands(q).contains(l)));
                        v
                    } else {
                        gen_longhands(pname)
                    };
                    if cands.is_empty() {
                        continue;
                    }
                    let (env_prev, env_base) = (style::gen_env(prev, k.cx), style::gen_env(base, k.cx));
                    for l in cands {
                        let (fa, fb) = (style::gen_val(prev, l), style::gen_val(base, l));
                        let running = entry.gtrans.iter().position(|t| t.prop == l);
                        if fa == fb && running.is_none() {
                            continue;
                        }
                        let Some(vb) = style::gen_eval(l, &fb, &env_base) else { continue };
                        let from = match running {
                            Some(r) => {
                                if entry.gtrans[r].to_v.as_ref() == Some(&vb) {
                                    continue;
                                }
                                let old = entry.gtrans.remove(r);
                                self.events.push(AnimEvent { node, kind: "transitioncancel", name: old.name.to_string(), elapsed: ((now - old.start - old.delay).max(0.0)) / 1000.0 });
                                GenVal::Interp(Rc::new(GenInterp { a: old.from.clone(), b: old.to.clone(), p: gen_progress(&old, now), native: false }))
                            }
                            None => fa,
                        };
                        let Some(va) = style::gen_eval(l, &from, &env_prev) else { continue };
                        if va == vb {
                            continue;
                        }
                        if !allow_discrete && pv::interp::interpolate(l, &va, &vb, 0.5).is_none() {
                            continue;
                        }
                        let name: Rc<str> = Rc::from(l);
                        self.events.push(AnimEvent { node, kind: "transitionrun", name: name.to_string(), elapsed: (-delay).max(0.0) / 1000.0 });
                        self.next_id += 1;
                        entry.gtrans.push(GenTransition { id: self.next_id, prop: l, name, from, to: fb, to_v: Some(vb), start: now, delay, duration: dur, timing: timing.clone(), started: false });
                    }
                }
            }
            // Advance running transitions.
            let mut live = Vec::new();
            for mut t in entry.trans.drain(..) {
                let still = base.transition.as_ref().map(|s| s.props.iter().any(|p| &**p == "all" || props_of(p).contains(&t.prop))).unwrap_or(false);
                if !still {
                    self.events.push(AnimEvent { node, kind: "transitioncancel", name: t.name.to_string(), elapsed: 0.0 });
                    continue;
                }
                let local = now - t.start - t.delay;
                if local >= 0.0 && !t.started {
                    t.started = true;
                    self.events.push(AnimEvent { node, kind: "transitionstart", name: t.name.to_string(), elapsed: (-t.delay).max(0.0) / 1000.0 });
                }
                if local >= t.duration {
                    if !t.started {
                        self.events.push(AnimEvent { node, kind: "transitionstart", name: t.name.to_string(), elapsed: (-t.delay).max(0.0) / 1000.0 });
                    }
                    self.events.push(AnimEvent { node, kind: "transitionend", name: t.name.to_string(), elapsed: t.duration / 1000.0 });
                    continue;
                }
                if local < 0.0 {
                    self.wake_at(t.start + t.delay);
                } else {
                    self.running = true;
                }
                let v = transition_value(&t, now, current);
                if !paint_only(t.prop) {
                    self.paint_only = false;
                }
                let s = out.get_or_insert_with(|| base.clone());
                set(s, t.prop, v);
                if color_prop_name(t.prop).is_some() {
                    set_hp(s, t.prop, t.hp.as_ref().map(|(x, y)| super::color::interpolate(x, y, trans_progress(&t, now))));
                }
                if let Some((k, a, b)) = &t.gen {
                    if a != b {
                        style::push_gen_interp(s, k, GenInterp { a: a.clone(), b: b.clone(), p: trans_progress(&t, now), native: false });
                    }
                }
                live.push(t);
            }
            entry.trans = live;
            let mut glive = Vec::new();
            for mut t in entry.gtrans.drain(..) {
                let still = base.transition.as_ref().map(|s| s.props.iter().any(|p| &**p == "all" || gen_longhands(p).contains(&t.prop))).unwrap_or(false);
                if !still {
                    self.events.push(AnimEvent { node, kind: "transitioncancel", name: t.name.to_string(), elapsed: 0.0 });
                    continue;
                }
                let local = now - t.start - t.delay;
                if local >= 0.0 && !t.started {
                    t.started = true;
                    self.events.push(AnimEvent { node, kind: "transitionstart", name: t.name.to_string(), elapsed: (-t.delay).max(0.0) / 1000.0 });
                }
                if local >= t.duration {
                    if !t.started {
                        self.events.push(AnimEvent { node, kind: "transitionstart", name: t.name.to_string(), elapsed: (-t.delay).max(0.0) / 1000.0 });
                    }
                    self.events.push(AnimEvent { node, kind: "transitionend", name: t.name.to_string(), elapsed: t.duration / 1000.0 });
                    continue;
                }
                if local < 0.0 {
                    self.wake_at(t.start + t.delay);
                } else {
                    self.running = true;
                }
                self.paint_only = false;
                let s = out.get_or_insert_with(|| base.clone());
                style::push_gen_interp(s, t.prop, GenInterp { a: t.from.clone(), b: t.to.clone(), p: gen_progress(&t, now), native: false });
                clear_hp(s, t.prop);
                glive.push(t);
            }
            entry.gtrans = glive;
        }
        if !entry.css.is_empty() || !entry.trans.is_empty() || !entry.gtrans.is_empty() {
            entry.applied_at = Some(now);
            self.nodes.insert(node, entry);
        }
        out
    }

    fn css_events(&mut self, node: NodeId, run: &mut RunningCss, t: &AnimTiming, phase: Phase, iteration: f64) {
        let name = run.name.to_string();
        let prev = run.phase;
        let active = t.duration * t.iterations;
        match (prev, phase) {
            (None | Some(Phase::Before), Phase::Active) => {
                self.events.push(AnimEvent { node, kind: "animationstart", name, elapsed: (-t.delay).max(0.0) / 1000.0 });
            }
            (None | Some(Phase::Before), Phase::After) => {
                self.events.push(AnimEvent { node, kind: "animationstart", name: name.clone(), elapsed: (-t.delay).max(0.0) / 1000.0 });
                self.events.push(AnimEvent { node, kind: "animationend", name, elapsed: active / 1000.0 });
            }
            (Some(Phase::Active), Phase::Active) if iteration > run.iteration && iteration.is_finite() => {
                self.events.push(AnimEvent { node, kind: "animationiteration", name, elapsed: iteration * t.duration / 1000.0 });
            }
            (Some(Phase::Active), Phase::After) => {
                self.events.push(AnimEvent { node, kind: "animationend", name, elapsed: active / 1000.0 });
            }
            _ => {}
        }
        run.phase = Some(phase);
        run.iteration = iteration;
    }

    /// Apply keyframes at iteration progress `p` onto `s` (which starts as the base style).
    fn apply_keyframes(&mut self, s: &mut ComputedStyle, base: &ComputedStyle, kf: &Keyframes, p: f64, timing: &Timing, k: &KfCtx, effect_comp: Composite) {
        // Property names in the keyframes, in first-seen order.
        let mut names: Vec<&str> = Vec::new();
        for f in &kf.frames {
            for d in &f.decls {
                if !names.contains(&&*d.name) {
                    names.push(&d.name);
                }
            }
        }
        let frame_style = |f: &Keyframe| -> ComputedStyle {
            let mut st = base.clone();
            // Font size first: em-based values depend on it.
            for d in f.decls.iter().filter(|d| &*d.name == "font-size" || &*d.name == "font") {
                apply_decl(&mut st, k, d);
            }
            for d in f.decls.iter().filter(|d| &*d.name != "font-size" && &*d.name != "font") {
                apply_decl(&mut st, k, d);
            }
            st
        };
        let mut cache: HashMap<usize, ComputedStyle> = HashMap::new();
        for name in names {
            // Keyframes specifying this property, with implicit 0% and 100% from the base style.
            let with: Vec<usize> = kf.frames.iter().enumerate().filter(|(_, f)| f.decls.iter().any(|d| &*d.name == name)).map(|(i, _)| i).collect();
            let off = |i: Option<usize>, edge: f64| i.map(|i| kf.frames[i].offset).unwrap_or(edge);
            let mut a: Option<usize> = None;
            let mut b: Option<usize> = None;
            for &i in &with {
                if kf.frames[i].offset <= p {
                    a = Some(i);
                }
            }
            for &i in with.iter().rev() {
                if kf.frames[i].offset > p || (p >= 1.0 && kf.frames[i].offset >= 1.0) {
                    b = Some(i);
                }
            }
            // At or past the end: the last keyframe holds.
            if p >= 1.0 {
                if let Some(&last) = with.last() {
                    if kf.frames[last].offset >= 1.0 {
                        a = Some(last);
                        b = Some(last);
                    }
                }
            }
            let (oa, ob) = (off(a, 0.0), off(b, 1.0));
            let local = if ob > oa { ((p - oa) / (ob - oa)).clamp(-1.0, 2.0) } else { 1.0 };
            let ft = a.and_then(|i| kf.frames[i].timing.clone()).unwrap_or_else(|| timing.clone());
            let eased = if ob > oa { ft.eval(local) } else { 1.0 };
            for i in [a, b].into_iter().flatten() {
                if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(i) {
                    e.insert(frame_style(&kf.frames[i]));
                }
            }
            let sa = a.map(|i| &cache[&i]).unwrap_or(base);
            let sb = b.map(|i| &cache[&i]).unwrap_or(base);
            if !name.starts_with("--") && !pv::interp::animatable(name) {
                continue;
            }
            // Composite operations of the two keyframes (a neutral keyframe is the underlying value).
            let comp_of = |i: Option<usize>| i.map(|i| kf.frames[i].composite.unwrap_or(effect_comp)).unwrap_or(Composite::Replace);
            let (ca, cb) = (comp_of(a), comp_of(b));
            let under: Option<ComputedStyle> = (ca != Composite::Replace || cb != Composite::Replace).then(|| s.clone());
            let gcomp = |c: Composite, v: GenVal, l: &str| -> GenVal {
                match (&under, c) {
                    (Some(u), Composite::Add | Composite::Accumulate) => GenVal::Comp(Rc::new(style::GenComp { under: style::gen_val(u, l), val: v, acc: c == Composite::Accumulate })),
                    _ => v,
                }
            };
            let props = props_of(name);
            if props.is_empty() {
                // Natively discrete: the declaration of the nearer keyframe
                // (the value engine interpolates what getComputedStyle reports).
                let pick = if eased < 0.5 { a } else { b };
                if let Some(i) = pick {
                    for d in kf.frames[i].decls.iter().filter(|d| &*d.name == name) {
                        apply_decl(s, k, d);
                    }
                }
                for l in gen_longhands(name) {
                    let (ga, gb) = (gcomp(ca, style::gen_val(sa, l), l), gcomp(cb, style::gen_val(sb, l), l));
                    if ga != gb || under.is_some() {
                        style::push_gen_interp(s, l, GenInterp { a: ga, b: gb, p: eased, native: false });
                        clear_hp(s, l);
                    }
                }
                self.paint_only = false;
                continue;
            }
            // Longhands of the property the native fields don't cover: discrete
            // natively (applied first, then overridden by the native ones), interpolated by the engine.
            let extra = gen_longhands(name);
            if !extra.is_empty() {
                let pick = if eased < 0.5 { a } else { b };
                if let Some(i) = pick {
                    for d in kf.frames[i].decls.iter().filter(|d| &*d.name == name) {
                        apply_decl(s, k, d);
                    }
                }
                for l in extra {
                    let (ga, gb) = (gcomp(ca, style::gen_val(sa, l), l), gcomp(cb, style::gen_val(sb, l), l));
                    if ga != gb || under.is_some() {
                        style::push_gen_interp(s, l, GenInterp { a: ga, b: gb, p: eased, native: false });
                        clear_hp(s, l);
                    }
                }
                self.paint_only = false;
            }
            for prop in props {
                let (mut va, mut vb) = (get(sa, prop), get(sb, prop));
                if let Some(u) = &under {
                    let uv = get(u, prop);
                    if ca != Composite::Replace {
                        va = add_av(&uv, &va, ca == Composite::Accumulate).unwrap_or(va);
                    }
                    if cb != Composite::Replace {
                        vb = add_av(&uv, &vb, cb == Composite::Accumulate).unwrap_or(vb);
                    }
                }
                let v = if va == vb { va } else { interpolate(&va, &vb, eased, s.color).unwrap_or_else(|| discrete(&va, &vb, eased)) };
                if !paint_only(prop) {
                    self.paint_only = false;
                }
                set(s, prop, v);
                if let Some(cn) = color_prop_name(prop) {
                    let colors = |st: &ComputedStyle| matches!(get(st, prop), AV::Color(_));
                    let hp = match &under {
                        // Composited colors: sums kept unclamped through interpolation.
                        Some(u) if colors(u) && colors(sa) && colors(sb) => {
                            let abs = |st: &ComputedStyle| hp_abs(st, cn).unwrap_or_else(|| match get(st, prop) {
                                AV::Color(c) => legacy_abs(c),
                                _ => legacy_abs(Color { r: 0, g: 0, b: 0, a: 0 }),
                            });
                            let ua = abs(u);
                            let fa = if ca != Composite::Replace { super::color::add(&ua, &abs(sa)) } else { abs(sa) };
                            let fb = if cb != Composite::Replace { super::color::add(&ua, &abs(sb)) } else { abs(sb) };
                            Some(super::color::interpolate(&fa, &fb, eased))
                        }
                        _ => hp_pair(sa, sb, prop).map(|(x, y)| super::color::interpolate(&x, &y, eased)),
                    };
                    set_hp(s, prop, hp);
                }
                if let Some(gk) = gen_key(prop) {
                    let (ga, gb) = (gcomp(ca, style::gen_val(sa, gk), gk), gcomp(cb, style::gen_val(sb, gk), gk));
                    if ga != gb || under.is_some() {
                        style::push_gen_interp(s, gk, GenInterp { a: ga, b: gb, p: eased, native: false });
                    }
                }
            }
        }
    }

    /// The running CSS animations and transitions (for document.getAnimations()).
    pub fn css_list(&self) -> Vec<CssAnimInfo> {
        let dir = |d: Direction| match d {
            Direction::Normal => "normal",
            Direction::Reverse => "reverse",
            Direction::Alternate => "alternate",
            Direction::AlternateReverse => "alternate-reverse",
        };
        let fill = |f: Fill| match f {
            Fill::None => "none",
            Fill::Forwards => "forwards",
            Fill::Backwards => "backwards",
            Fill::Both => "both",
        };
        let mut out = Vec::new();
        for (&node, e) in &self.nodes {
            for t in e.trans.iter().filter(|t| !t.name.is_empty()) {
                out.push(CssAnimInfo { id: t.id, node, transition: true, name: t.name.to_string(), start: t.start, paused_at: None, delay: t.delay, duration: t.duration, iterations: 1.0, direction: "normal", fill: "backwards", easing: timing_text(&t.timing) });
            }
            for t in &e.gtrans {
                out.push(CssAnimInfo { id: t.id, node, transition: true, name: t.name.to_string(), start: t.start, paused_at: None, delay: t.delay, duration: t.duration, iterations: 1.0, direction: "normal", fill: "backwards", easing: timing_text(&t.timing) });
            }
            for r in e.css.iter().filter(|r| !r.canceled) {
                let tm = r.timing.clone().unwrap_or(AnimTiming { duration: 0.0, delay: 0.0, end_delay: 0.0, iterations: 1.0, iteration_start: 0.0, direction: Direction::Normal, fill: Fill::None, timing: Timing::EASE });
                out.push(CssAnimInfo { id: r.id, node, transition: false, name: r.name.to_string(), start: r.start, paused_at: r.paused_at, delay: tm.delay, duration: tm.duration, iterations: tm.iterations, direction: dir(tm.direction), fill: fill(tm.fill), easing: timing_text(&tm.timing) });
            }
        }
        out.sort_by_key(|i| (!i.transition, i.id));
        out
    }

    /// Seek a CSS animation or transition (by id) to `time` (ms) at timeline time `now`.
    pub fn css_seek(&mut self, id: u64, time: f64, now: f64) -> bool {
        self.touched = true;
        for e in self.nodes.values_mut() {
            if let Some(t) = e.trans.iter_mut().find(|t| t.id == id) {
                t.start = now - time;
                return true;
            }
            if let Some(t) = e.gtrans.iter_mut().find(|t| t.id == id) {
                t.start = now - time;
                return true;
            }
            if let Some(r) = e.css.iter_mut().find(|r| r.id == id) {
                r.start = r.paused_at.unwrap_or(now) - time;
                return true;
            }
        }
        false
    }

    /// Script play() (`play` true) or pause() of a CSS animation, at timeline time `now`.
    pub fn css_play(&mut self, id: u64, play: bool, now: f64) {
        self.touched = true;
        for e in self.nodes.values_mut() {
            if let Some(r) = e.css.iter_mut().find(|r| r.id == id) {
                r.play_override = Some(play);
                if !play {
                    if r.paused_at.is_none() {
                        r.paused_at = Some(now);
                    }
                    return;
                }
                if let Some(pa) = r.paused_at.take() {
                    r.start += now - pa;
                }
                // Playing a finished (or not yet started) animation restarts it.
                if let Some(t) = &r.timing {
                    let end = t.delay + t.duration * t.iterations + t.end_delay;
                    let cur = now - r.start;
                    if end.is_finite() && (cur >= end || cur < 0.0) {
                        r.start = now;
                        r.phase = None;
                    }
                }
                return;
            }
        }
    }

    /// Cancel a CSS transition or animation (by id).
    pub fn css_cancel(&mut self, id: u64) {
        self.touched = true;
        for (&node, e) in self.nodes.iter_mut() {
            if let Some(r) = e.css.iter_mut().find(|r| r.id == id) {
                if r.phase == Some(Phase::Active) {
                    self.events.push(AnimEvent { node, kind: "animationcancel", name: r.name.to_string(), elapsed: 0.0 });
                }
                r.canceled = true;
                return;
            }
            if let Some(i) = e.trans.iter().position(|t| t.id == id) {
                let t = e.trans.remove(i);
                self.events.push(AnimEvent { node, kind: "transitioncancel", name: t.name.to_string(), elapsed: 0.0 });
                return;
            }
            if let Some(i) = e.gtrans.iter().position(|t| t.id == id) {
                let t = e.gtrans.remove(i);
                self.events.push(AnimEvent { node, kind: "transitioncancel", name: t.name.to_string(), elapsed: 0.0 });
                return;
            }
        }
    }

    /// Nodes not styled in this pass lost their animations (removed, or display:none).
    pub fn finish(&mut self) {
        self.touched = false;
        // Bases of elements this pass kept (untouched, settled) stay valid.
        let (seen, nodes, script) = (&self.seen, &self.nodes, &self.script);
        self.bases.retain(|k, _| seen.contains(k) && (nodes.contains_key(k) || script.contains_key(k)));
        let gone: Vec<NodeId> = self.nodes.keys().filter(|n| !self.seen.contains(n)).copied().collect();
        for n in gone {
            if let Some(e) = self.nodes.remove(&n) {
                for r in e.css {
                    if r.phase == Some(Phase::Active) {
                        self.events.push(AnimEvent { node: n, kind: "animationcancel", name: r.name.to_string(), elapsed: 0.0 });
                    }
                }
                for t in e.trans {
                    self.events.push(AnimEvent { node: n, kind: "transitioncancel", name: t.name.to_string(), elapsed: 0.0 });
                }
                for t in e.gtrans {
                    self.events.push(AnimEvent { node: n, kind: "transitioncancel", name: t.name.to_string(), elapsed: 0.0 });
                }
            }
        }
    }

    pub fn clear(&mut self) {
        self.bases.clear();
        self.nodes.clear();
        self.script.clear();
        self.events.clear();
        self.running = false;
        self.next_wake = None;
    }
}

/// The property's computed value is currentColor (or `auto` caret color).
fn follows_current_color(s: &ComputedStyle, p: Prop) -> bool {
    let cc = |c: &ColorValue| matches!(c, ColorValue::CurrentColor);
    match p {
        Prop::BorderColor(i) => cc(&s.border_color[i as usize]),
        Prop::OutlineColor => cc(&s.outline_color),
        Prop::TextDecorationColor => cc(&s.text_decoration_color),
        Prop::BackgroundColor => cc(&s.background_color),
        Prop::CaretColor => s.caret_color.is_none(),
        _ => false,
    }
}

fn transition_value(t: &RunningTransition, now: f64, current: Color) -> AV {
    let local = now - t.start - t.delay;
    if local <= 0.0 {
        return t.from.clone();
    }
    if t.duration <= 0.0 || local >= t.duration {
        return t.to.clone();
    }
    let p = t.timing.eval(local / t.duration);
    interpolate(&t.from, &t.to, p, current).unwrap_or_else(|| discrete(&t.from, &t.to, p))
}

/// An easing function's CSS text.
pub fn timing_text(t: &Timing) -> String {
    let f = |v: f64| super::calc::fmt_num(v);
    match t {
        Timing::Linear => "linear".into(),
        Timing::Bezier(a, b, c, d) => match (*a, *b, *c, *d) {
            (0.25, 0.1, 0.25, 1.0) => "ease".into(),
            (0.42, 0.0, 1.0, 1.0) => "ease-in".into(),
            (0.0, 0.0, 0.58, 1.0) => "ease-out".into(),
            (0.42, 0.0, 0.58, 1.0) => "ease-in-out".into(),
            _ => format!("cubic-bezier({}, {}, {}, {})", f(*a), f(*b), f(*c), f(*d)),
        },
        Timing::Steps(n, pos) => match pos {
            0 => format!("steps({n})"),
            1 => format!("steps({n}, start)"),
            2 => format!("steps({n}, jump-none)"),
            _ => format!("steps({n}, jump-both)"),
        },
        Timing::Points(p) => format!("linear({})", p.iter().map(|(x, y)| format!("{} {}%", f(*y as f64), f(*x as f64 * 100.0))).collect::<Vec<_>>().join(", ")),
    }
}

/// A transition's eased progress at `now` (0 before it starts, 1 after).
fn trans_progress(t: &RunningTransition, now: f64) -> f64 {
    ease_at(now - t.start - t.delay, t.duration, &t.timing)
}

fn gen_progress(t: &GenTransition, now: f64) -> f64 {
    ease_at(now - t.start - t.delay, t.duration, &t.timing)
}

fn ease_at(local: f64, duration: f64, timing: &Timing) -> f64 {
    if local <= 0.0 {
        return 0.0;
    }
    if duration <= 0.0 || local >= duration {
        return 1.0;
    }
    timing.eval(local / duration)
}

/// A transition's event name: its longhand. The native background-position
/// transition is silent (empty name): the engine's background-position(-x/-y)
/// transitions report it.
fn trans_name(p: Prop) -> Rc<str> {
    let n = prop_name(p);
    if p == Prop::BackgroundPosition || pv::shorthand::longhands(n).is_some() {
        Rc::from("")
    } else {
        Rc::from(n)
    }
}

fn prop_name(p: Prop) -> &'static str {
    match p {
        Prop::Opacity => "opacity",
        Prop::Transform => "transform",
        Prop::Translate => "translate",
        Prop::Rotate => "rotate",
        Prop::Scale => "scale",
        Prop::TransformOrigin => "transform-origin",
        Prop::Color => "color",
        Prop::BackgroundColor => "background-color",
        Prop::BorderColor(0) => "border-top-color",
        Prop::BorderColor(1) => "border-right-color",
        Prop::BorderColor(2) => "border-bottom-color",
        Prop::BorderColor(_) => "border-left-color",
        Prop::OutlineColor => "outline-color",
        Prop::TextDecorationColor => "text-decoration-color",
        Prop::CaretColor => "caret-color",
        Prop::Fill => "fill",
        Prop::Stroke => "stroke",
        Prop::Width => "width",
        Prop::Height => "height",
        Prop::MinWidth => "min-width",
        Prop::MinHeight => "min-height",
        Prop::MaxWidth => "max-width",
        Prop::MaxHeight => "max-height",
        Prop::Inset(0) => "top",
        Prop::Inset(1) => "right",
        Prop::Inset(2) => "bottom",
        Prop::Inset(_) => "left",
        Prop::Margin(0) => "margin-top",
        Prop::Margin(1) => "margin-right",
        Prop::Margin(2) => "margin-bottom",
        Prop::Margin(_) => "margin-left",
        Prop::Padding(0) => "padding-top",
        Prop::Padding(1) => "padding-right",
        Prop::Padding(2) => "padding-bottom",
        Prop::Padding(_) => "padding-left",
        Prop::BorderWidth(0) => "border-top-width",
        Prop::BorderWidth(1) => "border-right-width",
        Prop::BorderWidth(2) => "border-bottom-width",
        Prop::BorderWidth(_) => "border-left-width",
        Prop::BorderRadius(0) => "border-top-left-radius",
        Prop::BorderRadius(1) => "border-top-right-radius",
        Prop::BorderRadius(2) => "border-bottom-right-radius",
        Prop::BorderRadius(_) => "border-bottom-left-radius",
        Prop::OutlineWidth => "outline-width",
        Prop::OutlineOffset => "outline-offset",
        Prop::FontSize => "font-size",
        Prop::FontWeight => "font-weight",
        Prop::LetterSpacing => "letter-spacing",
        Prop::WordSpacing => "word-spacing",
        Prop::LineHeight => "line-height",
        Prop::TextIndent => "text-indent",
        Prop::BoxShadow => "box-shadow",
        Prop::TextShadow => "text-shadow",
        Prop::Filter => "filter",
        Prop::ZIndex => "z-index",
        Prop::Visibility => "visibility",
        Prop::FlexGrow => "flex-grow",
        Prop::FlexShrink => "flex-shrink",
        Prop::FlexBasis => "flex-basis",
        Prop::RowGap => "row-gap",
        Prop::ColumnGap => "column-gap",
        Prop::ObjectPosition => "object-position",
        Prop::BackgroundPosition => "background-position",
        Prop::Svg(i) => style::SVG_PROPS[i as usize].0,
    }
}

fn apply_decl(s: &mut ComputedStyle, k: &KfCtx, d: &Declaration) {
    if d.has_var {
        if let Some(v) = super::substitute(&d.value, &s.custom.clone(), 0) {
            style::apply(s, k.parent, &d.name, &v, k.cx);
        }
    } else {
        style::apply(s, k.parent, &d.name, &d.value, k.cx);
    }
}

/// Parse `prop: value; ...` keyframe declarations (Web Animations).
pub fn keyframes_from_script(frames: &[(f64, Option<String>, String, Option<String>)], composite: Option<&str>) -> Keyframes {
    let mut out = Vec::new();
    for (off, easing, css, comp) in frames {
        let decls = super::parse_declarations(css).into_iter().filter(|d| !d.important).collect();
        let timing = easing.as_deref().and_then(|e| parse_timing(&super::tokenizer::parse_cvs(e)));
        out.push(Keyframe { offset: *off, timing, decls, composite: comp.as_deref().and_then(Composite::parse) });
    }
    out.sort_by(|a, b| a.offset.total_cmp(&b.offset));
    Keyframes { frames: out, composite: composite.and_then(Composite::parse) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing() {
        assert!((Timing::EASE.eval(0.5) - 0.8024).abs() < 1e-3);
        assert!((Timing::Linear.eval(0.3) - 0.3).abs() < 1e-9);
        assert_eq!(Timing::Steps(4, 0).eval(0.3), 0.25);
        assert_eq!(Timing::Steps(4, 1).eval(0.3), 0.5);
        let t = parse_timing(&crate::css::tokenizer::parse_cvs("linear(0, 0.25 75%, 1)")).unwrap();
        assert!((t.eval(0.75) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn timing_model() {
        let t = AnimTiming { duration: 1000.0, delay: 500.0, end_delay: 0.0, iterations: 2.0, iteration_start: 0.0, direction: Direction::Alternate, fill: Fill::Forwards, timing: Timing::Linear };
        assert_eq!(progress(&t, 0.0).2, None);
        assert_eq!(progress(&t, 750.0), (Phase::Active, 0.0, Some(0.25)));
        assert_eq!(progress(&t, 1750.0), (Phase::Active, 1.0, Some(0.75)));
        assert_eq!(progress(&t, 9000.0), (Phase::After, 1.0, Some(0.0)));
    }

    #[test]
    fn colors_and_lengths() {
        let c = lerp_color(Color { r: 255, g: 0, b: 0, a: 255 }, Color { r: 0, g: 0, b: 255, a: 255 }, 0.5);
        assert_eq!((c.r, c.b), (128, 128));
        assert_eq!(lerp_len(&Len::Px(0.0), &Len::Pct(100.0), 0.5), Some(Len::Calc(0.0, 50.0)));
    }
}
