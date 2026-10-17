// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Math functions (calc(), min(), clamp(), sign(), …) as expression trees:
//! parsed from component values, simplified per CSS Values 4 ("simplify a
//! calculation tree"), serialized like Chrome (sorted sums and products), and
//! evaluated with bindings for keywords such as relative-color channels.

use super::tokenizer::{Tok, CV};

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Num(f64),
    /// A dimension or percentage (`unit` lowercased; "%" for percentages).
    Dim(f64, String),
    /// A keyword: a channel name (`r`, `alpha`, …) or another identifier.
    Ident(String),
    Sum(Vec<Node>),
    Product(Vec<Node>),
    Negate(Box<Node>),
    Invert(Box<Node>),
    /// A math function other than calc() (min, max, clamp, sign, round, …).
    Func(String, Vec<Node>),
    /// Something kept as written (var(), sibling-index(), …).
    Opaque(String),
}

const MATH_FNS: &[&str] = &[
    "calc", "min", "max", "clamp", "round", "mod", "rem", "sin", "cos", "tan", "asin", "acos", "atan", "atan2", "pow", "sqrt", "hypot", "log", "exp", "abs", "sign", "-webkit-calc",
];

pub fn is_math_fn(name: &str) -> bool {
    MATH_FNS.contains(&name.to_ascii_lowercase().as_str())
}

/// Parse a math function (`name(args)`) into a tree. None if malformed.
pub fn parse_fn(name: &str, args: &[CV]) -> Option<Node> {
    let n = name.to_ascii_lowercase();
    if n == "calc" || n == "-webkit-calc" {
        return parse_sum(&tokens(args));
    }
    let parts: Vec<Vec<&CV>> = split_commas(args);
    let mut nodes = Vec::new();
    for p in &parts {
        if p.is_empty() {
            return None;
        }
        nodes.push(parse_sum(p)?);
    }
    let ok = match n.as_str() {
        "min" | "max" | "hypot" => !nodes.is_empty(),
        "clamp" => nodes.len() == 3,
        "round" => {
            // round(<strategy>?, A, B?)
            if let Some(Node::Ident(s)) = nodes.first() {
                if matches!(s.as_str(), "nearest" | "up" | "down" | "to-zero") {
                    let rest = nodes.len() - 1;
                    rest == 1 || rest == 2
                } else {
                    nodes.len() == 1 || nodes.len() == 2
                }
            } else {
                nodes.len() == 1 || nodes.len() == 2
            }
        }
        "mod" | "rem" | "atan2" | "pow" => nodes.len() == 2,
        "log" => nodes.len() == 1 || nodes.len() == 2,
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "sqrt" | "exp" | "abs" | "sign" => nodes.len() == 1,
        _ => false,
    };
    if !ok {
        return None;
    }
    Some(Node::Func(n, nodes))
}

fn tokens(args: &[CV]) -> Vec<&CV> {
    args.iter().filter(|c| !c.is_ws()).collect()
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

// sum := product (('+'|'-') product)*
fn parse_sum(t: &[&CV]) -> Option<Node> {
    let mut i = 0;
    let first = parse_product(t, &mut i)?;
    let mut terms = vec![first];
    while i < t.len() {
        let neg = if t[i].is_delim('+') {
            false
        } else if t[i].is_delim('-') {
            true
        } else {
            return None;
        };
        i += 1;
        let p = parse_product(t, &mut i)?;
        terms.push(if neg { Node::Negate(Box::new(p)) } else { p });
    }
    Some(if terms.len() == 1 { terms.pop().unwrap() } else { Node::Sum(terms) })
}

fn parse_product(t: &[&CV], i: &mut usize) -> Option<Node> {
    let mut factors = vec![parse_value(t, i)?];
    while *i < t.len() && (t[*i].is_delim('*') || t[*i].is_delim('/')) {
        let inv = t[*i].is_delim('/');
        *i += 1;
        let v = parse_value(t, i)?;
        factors.push(if inv { Node::Invert(Box::new(v)) } else { v });
    }
    Some(if factors.len() == 1 { factors.pop().unwrap() } else { Node::Product(factors) })
}

fn parse_value(t: &[&CV], i: &mut usize) -> Option<Node> {
    let c = *t.get(*i)?;
    *i += 1;
    Some(match c {
        CV::T(Tok::Number(n)) => Node::Num(*n as f64),
        CV::T(Tok::Percentage(p)) => Node::Dim(*p as f64, "%".into()),
        CV::T(Tok::Dimension(n, u)) => Node::Dim(*n as f64, u.to_ascii_lowercase()),
        CV::T(Tok::Ident(s)) => {
            let l = s.to_ascii_lowercase();
            match l.as_str() {
                "e" => Node::Ident("e".into()),
                "pi" => Node::Ident("pi".into()),
                "infinity" => Node::Ident("infinity".into()),
                "-infinity" => Node::Ident("-infinity".into()),
                "nan" => Node::Ident("NaN".into()),
                _ => Node::Ident(l),
            }
        }
        CV::Block('(', inner) => parse_sum(&tokens(inner))?,
        CV::Func(name, args) if is_math_fn(name) => parse_fn(name, args)?,
        CV::Func(name, _) => {
            let l = name.to_ascii_lowercase();
            if matches!(l.as_str(), "var" | "env" | "attr" | "sibling-index" | "sibling-count" | "if" | "inherit") {
                Node::Opaque(super::tokenizer::serialize(std::slice::from_ref(c)))
            } else {
                return None;
            }
        }
        _ => return None,
    })
}

/// Unit families for combining.
fn canon_unit(u: &str) -> Option<(&'static str, f64)> {
    Some(match u {
        "deg" => ("deg", 1.0),
        "grad" => ("deg", 0.9),
        "rad" => ("deg", 180.0 / std::f64::consts::PI),
        "turn" => ("deg", 360.0),
        "px" => ("px", 1.0),
        "in" => ("px", 96.0),
        "cm" => ("px", 96.0 / 2.54),
        "mm" => ("px", 96.0 / 25.4),
        "q" => ("px", 96.0 / 101.6),
        "pt" => ("px", 96.0 / 72.0),
        "pc" => ("px", 16.0),
        "s" => ("s", 1.0),
        "ms" => ("s", 0.001),
        "hz" => ("hz", 1.0),
        "khz" => ("hz", 1000.0),
        "dppx" | "x" => ("dppx", 1.0),
        "dpi" => ("dppx", 1.0 / 96.0),
        "dpcm" => ("dppx", 2.54 / 96.0),
        _ => return None,
    })
}

fn const_value(s: &str) -> Option<f64> {
    Some(match s {
        "e" => std::f64::consts::E,
        "pi" => std::f64::consts::PI,
        "infinity" => f64::INFINITY,
        "-infinity" => f64::NEG_INFINITY,
        "NaN" => f64::NAN,
        _ => return None,
    })
}

/// Simplify a tree (CSS Values 4 §10.10 "simplify a calculation tree"),
/// resolving keywords with `bind` (channel values), and converting absolute
/// units to their canonical unit only when `canon` (computed time).
pub fn simplify(n: &Node, bind: &dyn Fn(&str) -> Option<Node>) -> Node {
    match n {
        // Absolute units become their canonical unit (1in → 96px, 0.5turn → 180deg).
        Node::Dim(v, u) => match canon_unit(u) {
            Some((cu, k)) if cu != "dppx" || u == "dppx" || u == "x" => Node::Dim(v * k, cu.to_string()),
            _ => n.clone(),
        },
        Node::Num(_) | Node::Opaque(_) => n.clone(),
        Node::Ident(s) => {
            if let Some(v) = bind(s) {
                return v;
            }
            if let Some(v) = const_value(s) {
                return Node::Num(v);
            }
            n.clone()
        }
        Node::Negate(x) => match simplify(x, bind) {
            Node::Num(v) => Node::Num(-v),
            Node::Dim(v, u) => Node::Dim(-v, u),
            Node::Negate(y) => *y,
            other => Node::Negate(Box::new(other)),
        },
        Node::Invert(x) => match simplify(x, bind) {
            Node::Num(v) => Node::Num(1.0 / v),
            Node::Invert(y) => *y,
            other => Node::Invert(Box::new(other)),
        },
        Node::Sum(items) => {
            let mut flat: Vec<Node> = Vec::new();
            for it in items {
                match simplify(it, bind) {
                    Node::Sum(inner) => flat.extend(inner),
                    other => flat.push(other),
                }
            }
            // Combine numbers, and dimensions of the same unit.
            let mut out: Vec<Node> = Vec::new();
            for it in flat {
                let merged = match &it {
                    Node::Num(v) => out.iter_mut().find_map(|o| if let Node::Num(w) = o { *w += v; Some(()) } else { None }).is_some(),
                    Node::Dim(v, u) => out.iter_mut().find_map(|o| if let Node::Dim(w, u2) = o { if u2 == u { *w += v; Some(()) } else { None } } else { None }).is_some(),
                    _ => false,
                };
                if !merged {
                    out.push(it);
                }
            }
            if out.len() == 1 {
                out.pop().unwrap()
            } else {
                Node::Sum(out)
            }
        }
        Node::Product(items) => {
            let mut flat: Vec<Node> = Vec::new();
            for it in items {
                match simplify(it, bind) {
                    Node::Product(inner) => flat.extend(inner),
                    other => flat.push(other),
                }
            }
            // Multiply numbers together; cancel a dimension against an
            // inverted one of the same unit; fold into a single dimension when possible.
            let mut num = 1.0;
            let mut has_num = false;
            let mut rest: Vec<Node> = Vec::new();
            let mut dims: Vec<(f64, String)> = Vec::new();
            let mut inv_dims: Vec<(f64, String)> = Vec::new();
            for it in flat {
                match it {
                    Node::Num(v) => {
                        num *= v;
                        has_num = true;
                    }
                    Node::Dim(v, u) => dims.push((v, u)),
                    Node::Invert(b) if matches!(*b, Node::Num(_) | Node::Dim(..)) => match *b {
                        Node::Num(v) => {
                            num /= v;
                            has_num = true;
                        }
                        Node::Dim(v, u) => inv_dims.push((v, u)),
                        _ => unreachable!(),
                    },
                    other => rest.push(other),
                }
            }
            let mut kept_inv = Vec::new();
            for (v, u) in inv_dims {
                if let Some(i) = dims.iter().position(|d| d.1 == u) {
                    let (dv, _) = dims.remove(i);
                    num *= dv / v;
                    has_num = true;
                } else {
                    kept_inv.push(Node::Invert(Box::new(Node::Dim(v, u))));
                }
            }
            let mut front: Vec<Node> = dims.into_iter().map(|(v, u)| Node::Dim(v, u)).collect();
            front.extend(rest);
            front.extend(kept_inv);
            let mut rest = front;
            if rest.is_empty() {
                return Node::Num(num);
            }
            if rest.len() == 1 {
                if let Node::Dim(v, u) = &rest[0] {
                    return Node::Dim(v * num, u.clone());
                }
                if !has_num || num == 1.0 {
                    return rest.pop().unwrap();
                }
                // A number times a sum distributes over its numbers/dimensions only.
                if let Node::Sum(terms) = &rest[0] {
                    if terms.iter().all(|t| matches!(t, Node::Num(_) | Node::Dim(..))) {
                        let terms = terms
                            .iter()
                            .map(|t| match t {
                                Node::Num(v) => Node::Num(v * num),
                                Node::Dim(v, u) => Node::Dim(v * num, u.clone()),
                                _ => unreachable!(),
                            })
                            .collect();
                        return Node::Sum(terms);
                    }
                }
            }
            let mut out = Vec::new();
            if has_num && num != 1.0 {
                out.push(Node::Num(num));
            }
            out.extend(rest);
            if out.len() == 1 {
                out.pop().unwrap()
            } else {
                Node::Product(out)
            }
        }
        Node::Func(name, args) => {
            let args: Vec<Node> = args.iter().map(|a| simplify(a, bind)).collect();
            match eval_fn(name, &args) {
                Some(v) => v,
                None => Node::Func(name.clone(), args),
            }
        }
    }
}

/// A numeric value with a canonical unit ("" for numbers).
fn as_value(n: &Node) -> Option<(f64, String)> {
    match n {
        Node::Num(v) => Some((*v, String::new())),
        Node::Dim(v, u) => {
            if u == "%" {
                return Some((*v, "%".into()));
            }
            let (cu, k) = canon_unit(u)?;
            Some((v * k, cu.to_string()))
        }
        _ => None,
    }
}

fn mk(v: f64, unit: &str) -> Node {
    if unit.is_empty() {
        Node::Num(v)
    } else {
        Node::Dim(v, unit.to_string())
    }
}

fn eval_fn(name: &str, args: &[Node]) -> Option<Node> {
    let vals: Vec<(f64, String)> = args
        .iter()
        .filter(|a| !matches!(a, Node::Ident(s) if matches!(s.as_str(), "nearest" | "up" | "down" | "to-zero")))
        .map(as_value)
        .collect::<Option<Vec<_>>>()?;
    let same = |vs: &[(f64, String)]| vs.windows(2).all(|w| w[0].1 == w[1].1);
    let deg = std::f64::consts::PI / 180.0;
    let angle_in = |(v, u): &(f64, String)| -> Option<f64> {
        match u.as_str() {
            "" => Some(*v),
            "deg" => Some(v * deg),
            _ => None,
        }
    };
    Some(match name {
        "min" | "max" => {
            if !same(&vals) {
                return None;
            }
            let mut r = vals[0].0;
            for (v, _) in &vals[1..] {
                if v.is_nan() || r.is_nan() {
                    r = f64::NAN;
                } else if name == "min" {
                    r = r.min(*v);
                } else {
                    r = r.max(*v);
                }
            }
            mk(r, &vals[0].1)
        }
        "clamp" => {
            if !same(&vals) {
                return None;
            }
            let (lo, v, hi) = (vals[0].0, vals[1].0, vals[2].0);
            let r = if v.is_nan() || lo.is_nan() || hi.is_nan() { f64::NAN } else { v.min(hi).max(lo) };
            mk(r, &vals[0].1)
        }
        "abs" => mk(vals[0].0.abs(), &vals[0].1),
        "sign" => {
            let v = vals[0].0;
            Node::Num(if v.is_nan() { f64::NAN } else if v > 0.0 { 1.0 } else if v < 0.0 { -1.0 } else { v })
        }
        "sin" => Node::Num(angle_in(&vals[0])?.sin()),
        "cos" => Node::Num(angle_in(&vals[0])?.cos()),
        "tan" => Node::Num(angle_in(&vals[0])?.tan()),
        "asin" | "acos" | "atan" => {
            if !vals[0].1.is_empty() {
                return None;
            }
            let v = vals[0].0;
            Node::Dim(match name { "asin" => v.asin(), "acos" => v.acos(), _ => v.atan() } / deg, "deg".into())
        }
        "atan2" => {
            if !same(&vals) {
                return None;
            }
            Node::Dim(vals[0].0.atan2(vals[1].0) / deg, "deg".into())
        }
        "pow" => {
            if !vals[0].1.is_empty() || !vals[1].1.is_empty() {
                return None;
            }
            Node::Num(vals[0].0.powf(vals[1].0))
        }
        "sqrt" => {
            if !vals[0].1.is_empty() {
                return None;
            }
            Node::Num(vals[0].0.sqrt())
        }
        "exp" => {
            if !vals[0].1.is_empty() {
                return None;
            }
            Node::Num(vals[0].0.exp())
        }
        "log" => {
            if vals.iter().any(|v| !v.1.is_empty()) {
                return None;
            }
            Node::Num(if vals.len() == 2 { vals[0].0.ln() / vals[1].0.ln() } else { vals[0].0.ln() })
        }
        "hypot" => {
            if !same(&vals) {
                return None;
            }
            mk(vals.iter().map(|v| v.0 * v.0).sum::<f64>().sqrt(), &vals[0].1)
        }
        "mod" | "rem" => {
            if !same(&vals) {
                return None;
            }
            let (a, b) = (vals[0].0, vals[1].0);
            let r = if name == "mod" { a - b * (a / b).floor() } else { a % b };
            mk(r, &vals[0].1)
        }
        "round" => {
            let strategy = match &args[0] {
                Node::Ident(s) => s.as_str(),
                _ => "nearest",
            };
            let (a, step) = if vals.len() == 2 {
                if !same(&vals) {
                    return None;
                }
                (vals[0].0, vals[1].0)
            } else {
                if !vals[0].1.is_empty() {
                    return None;
                }
                (vals[0].0, 1.0)
            };
            let q = a / step;
            let r = match strategy {
                "up" => q.ceil(),
                "down" => q.floor(),
                "to-zero" => q.trunc(),
                _ => (q + 0.5).floor(),
            } * step;
            mk(r, &vals[0].1)
        }
        _ => return None,
    })
}

/// Evaluate to a value with a canonical unit, if fully resolved.
pub fn value(n: &Node) -> Option<(f64, String)> {
    as_value(n)
}

thread_local! {
    /// Full-precision number formatting (values kept for computing, not for CSSOM).
    static PRECISE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` formatting numbers at full precision.
pub fn with_precise<R>(f: impl FnOnce() -> R) -> R {
    let old = PRECISE.with(|c| c.replace(true));
    let r = f();
    PRECISE.with(|c| c.set(old));
    r
}

/// Whether text has a number literal with more than six significant digits
/// (which the six-digit serialization would round).
pub fn has_long_number(s: &str) -> bool {
    let mut sig = 0;
    let mut leading = true;
    for c in s.chars() {
        if c.is_ascii_digit() {
            if c == '0' && leading {
                continue;
            }
            leading = false;
            sig += 1;
            if sig > 6 {
                return true;
            }
        } else if c == '.' {
            continue;
        } else {
            sig = 0;
            leading = true;
        }
    }
    false
}

/// Format a number like Chrome (six significant digits, no exponent for
/// ordinary magnitudes).
pub fn fmt_num(v: f64) -> String {
    if PRECISE.with(|c| c.get()) && v.is_finite() && v != 0.0 {
        let mag = v.abs().log10().floor() as i32;
        if (-7..21).contains(&mag) {
            let s = format!("{}", v);
            return if s == "-0" { "0".into() } else { s };
        }
    }
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "infinity".into() } else { "-infinity".into() };
    }
    if v == 0.0 {
        return "0".into();
    }
    let mag = v.abs().log10().floor() as i32;
    if !(-7..21).contains(&mag) {
        let s = format!("{:e}", v);
        return s;
    }
    let decimals = (5 - mag).max(0) as usize;
    let mut s = format!("{:.*}", decimals, v);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

/// Sort key for the children of sums and products (CSS Values 4 "sort a
/// calculation's children"): numbers, percentages, dimensions by unit, rest.
fn sort_key(n: &Node) -> (u8, String) {
    match n {
        Node::Num(_) => (0, String::new()),
        Node::Dim(_, u) if u == "%" => (1, String::new()),
        Node::Dim(_, u) => (2, u.clone()),
        _ => (3, String::new()),
    }
}

fn sorted(items: &[Node]) -> Vec<Node> {
    let mut v: Vec<Node> = items.to_vec();
    v.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    v
}

fn ser_inner(n: &Node, nested: bool) -> String {
    match n {
        Node::Num(v) => fmt_num(*v),
        Node::Dim(v, u) => {
            if v.is_infinite() || v.is_nan() {
                format!("{} * 1{}", fmt_num(*v), u)
            } else {
                format!("{}{}", fmt_num(*v), u)
            }
        }
        Node::Ident(s) => s.clone(),
        Node::Opaque(s) => s.clone(),
        Node::Negate(x) => {
            let inner = ser_inner(x, true);
            format!("(-1 * {})", inner)
        }
        Node::Invert(x) => {
            if nested {
                format!("(1 / {})", ser_inner(x, true))
            } else {
                format!("1 / {}", ser_inner(x, true))
            }
        }
        Node::Sum(items) => {
            let items = sorted(items);
            let mut s = String::new();
            for (i, it) in items.iter().enumerate() {
                let (neg, body) = match it {
                    Node::Num(v) if *v < 0.0 && i > 0 => (true, fmt_num(-v)),
                    Node::Dim(v, u) if *v < 0.0 && i > 0 => (true, format!("{}{}", fmt_num(-v), u)),
                    Node::Negate(x) if i > 0 => (true, ser_inner(x, true)),
                    _ => (false, ser_inner(it, true)),
                };
                if i > 0 {
                    s.push_str(if neg { " - " } else { " + " });
                }
                s.push_str(&body);
            }
            if nested {
                format!("({})", s)
            } else {
                s
            }
        }
        Node::Product(items) => {
            let items = sorted(items);
            let mut s = String::new();
            for (i, it) in items.iter().enumerate() {
                match it {
                    Node::Invert(x) if i > 0 => {
                        s.push_str(" / ");
                        s.push_str(&ser_inner(x, true));
                    }
                    _ => {
                        if i > 0 {
                            s.push_str(" * ");
                        }
                        s.push_str(&ser_inner(it, true));
                    }
                }
            }
            if nested {
                format!("({})", s)
            } else {
                s
            }
        }
        Node::Func(name, args) => format!("{}({})", name, args.iter().map(|a| ser_inner(a, false)).collect::<Vec<_>>().join(", ")),
    }
}

/// Serialize a simplified tree as a CSS value: `calc(…)` around anything
/// that is not itself a math function.
pub fn serialize(n: &Node) -> String {
    match n {
        Node::Func(..) => ser_inner(n, false),
        _ => format!("calc({})", ser_inner(n, false)),
    }
}

// ---------------------------------------------------------------- typed arithmetic

/// Base types of CSS typed arithmetic (indices into a `CType`).
pub const LEN: usize = 0;
pub const ANGLE: usize = 1;
pub const TIME: usize = 2;
pub const FREQ: usize = 3;
pub const RES: usize = 4;
pub const FLEX: usize = 5;
pub const PCT: usize = 6;
/// Exponents of each base type (all zero: a <number>).
pub type CType = [i8; 7];
pub const NUMBER: CType = [0; 7];

pub fn base(t: usize) -> CType {
    let mut c = NUMBER;
    c[t] = 1;
    c
}

/// The base type a unit belongs to ("%" → PCT).
pub fn unit_base(u: &str) -> Option<usize> {
    Some(match u {
        "%" => PCT,
        "px" | "cm" | "mm" | "q" | "in" | "pt" | "pc" | "em" | "rem" | "ex" | "rex" | "cap" | "rcap" | "ch" | "rch" | "ic" | "ric" | "lh" | "rlh" | "vw" | "vh" | "vi" | "vb" | "vmin" | "vmax" | "svw" | "svh"
        | "svi" | "svb" | "svmin" | "svmax" | "lvw" | "lvh" | "lvi" | "lvb" | "lvmin" | "lvmax" | "dvw" | "dvh" | "dvi" | "dvb" | "dvmin" | "dvmax" | "cqw" | "cqh" | "cqi" | "cqb" | "cqmin" | "cqmax" => LEN,
        "deg" | "grad" | "rad" | "turn" => ANGLE,
        "s" | "ms" => TIME,
        "hz" | "khz" => FREQ,
        "dpi" | "dpcm" | "dppx" | "x" => RES,
        "fr" => FLEX,
        _ => return None,
    })
}

fn add_t(a: CType, b: CType) -> CType {
    let mut r = a;
    for i in 0..7 {
        r[i] += b[i];
    }
    r
}

/// Map percentages to `pct_as` (the type they resolve against), if any.
fn pct_fold(t: CType, pct_as: Option<usize>) -> CType {
    match pct_as {
        Some(b) if t[PCT] != 0 => {
            let mut r = t;
            r[b] += r[PCT];
            r[PCT] = 0;
            r
        }
        _ => t,
    }
}

/// The type of a calculation (None when invalid: e.g. adding a length to a
/// number). Percentages resolve against `pct_as` where given.
pub fn node_type(n: &Node, pct_as: Option<usize>) -> Option<CType> {
    let t = match n {
        Node::Num(_) => NUMBER,
        Node::Dim(_, u) => base(unit_base(u)?),
        Node::Ident(s) => match s.as_str() {
            "e" | "pi" | "infinity" | "-infinity" | "NaN" => NUMBER,
            _ => return None,
        },
        Node::Opaque(s) => {
            let l = s.to_ascii_lowercase();
            if l.starts_with("sibling-index(") || l.starts_with("sibling-count(") {
                NUMBER
            } else {
                return None;
            }
        }
        Node::Negate(x) => node_type(x, pct_as)?,
        Node::Invert(x) => {
            let mut t = node_type(x, pct_as)?;
            for v in t.iter_mut() {
                *v = -*v;
            }
            t
        }
        Node::Sum(items) => {
            let mut t: Option<CType> = None;
            for it in items {
                let x = pct_fold(node_type(it, pct_as)?, pct_as);
                match t {
                    None => t = Some(x),
                    Some(t0) if t0 == x => {}
                    _ => return None,
                }
            }
            t?
        }
        Node::Product(items) => {
            let mut t = NUMBER;
            for it in items {
                t = add_t(t, pct_fold(node_type(it, pct_as)?, pct_as));
            }
            t
        }
        Node::Func(name, args) => {
            let strategy = |a: &Node| matches!(a, Node::Ident(s) if matches!(s.as_str(), "nearest" | "up" | "down" | "to-zero"));
            let typed: Vec<CType> = args.iter().filter(|a| !strategy(a)).map(|a| node_type(a, pct_as).map(|t| pct_fold(t, pct_as))).collect::<Option<Vec<_>>>()?;
            let same = || typed.windows(2).all(|w| w[0] == w[1]);
            match name.as_str() {
                "sign" => NUMBER,
                "sin" | "cos" | "tan" => {
                    if typed[0] != NUMBER && typed[0] != base(ANGLE) {
                        return None;
                    }
                    NUMBER
                }
                "asin" | "acos" | "atan" => {
                    if typed[0] != NUMBER {
                        return None;
                    }
                    base(ANGLE)
                }
                "atan2" => {
                    if !same() {
                        return None;
                    }
                    base(ANGLE)
                }
                "exp" | "log" | "pow" | "sqrt" => {
                    if typed.iter().any(|t| *t != NUMBER) {
                        return None;
                    }
                    NUMBER
                }
                "round" => {
                    if !same() || (typed.len() == 1 && typed[0] != NUMBER) {
                        return None;
                    }
                    typed[0]
                }
                _ => {
                    if typed.is_empty() || !same() {
                        return None;
                    }
                    typed[0]
                }
            }
        }
    };
    Some(pct_fold(t, pct_as))
}

/// Whether a calculation (as parsed) contains a percentage anywhere.
pub fn has_pct(n: &Node) -> bool {
    match n {
        Node::Dim(_, u) => u == "%",
        Node::Negate(x) | Node::Invert(x) => has_pct(x),
        Node::Sum(v) | Node::Product(v) | Node::Func(_, v) => v.iter().any(has_pct),
        _ => false,
    }
}

/// Apply `f` to every dimension leaf (e.g. to resolve relative units).
pub fn map_dims(n: &Node, f: &dyn Fn(f64, &str) -> Option<Node>) -> Node {
    match n {
        Node::Dim(v, u) => f(*v, u).unwrap_or_else(|| n.clone()),
        Node::Negate(x) => Node::Negate(Box::new(map_dims(x, f))),
        Node::Invert(x) => Node::Invert(Box::new(map_dims(x, f))),
        Node::Sum(v) => Node::Sum(v.iter().map(|x| map_dims(x, f)).collect()),
        Node::Product(v) => Node::Product(v.iter().map(|x| map_dims(x, f)).collect()),
        Node::Func(name, v) => Node::Func(name.clone(), v.iter().map(|x| map_dims(x, f)).collect()),
        _ => n.clone(),
    }
}
