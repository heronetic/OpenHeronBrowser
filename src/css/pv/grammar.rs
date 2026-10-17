// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! CSS Value Definition Syntax: a parser for grammar strings
//! (`<length> | auto`, `[ a || b ]#`, `fn( <number>{1,3} )`, …) and a
//! backtracking matcher over component values.

use super::super::tokenizer::{Tok, CV};
use super::V;

#[derive(Debug, Clone)]
pub enum G {
    Kw(String),
    /// A literal delimiter: ',' or '/'.
    Lit(char),
    /// A type (`<length>`), with an optional numeric range (`<length [0,∞]>`).
    Type(String, Option<(f64, f64)>),
    /// The grammar of another property (`<'margin-top'>`).
    Prop(String),
    Func(String, Box<G>),
    Seq(Vec<G>),
    /// `&&`: all, any order.
    All(Vec<G>),
    /// `||`: one or more, any order.
    Any(Vec<G>),
    /// `|`: exactly one.
    One(Vec<G>),
    /// Repetition: min, max (u32::MAX = unbounded), comma-separated.
    Mult(Box<G>, u32, u32, bool),
    /// `?`
    Opt(Box<G>),
    /// `[ … ]!`: the group must produce at least one value.
    Req(Box<G>),
}

// ------------------------------------------------------------------ grammar text parsing

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, t: &str) -> bool {
        self.ws();
        if self.s[self.i..].starts_with(t.as_bytes()) {
            self.i += t.len();
            true
        } else {
            false
        }
    }
    fn one(&mut self) -> G {
        let mut alts = vec![self.any()];
        loop {
            self.ws();
            if self.s.get(self.i) == Some(&b'|') && self.s.get(self.i + 1) != Some(&b'|') {
                self.i += 1;
                alts.push(self.any());
            } else {
                break;
            }
        }
        if alts.len() == 1 {
            alts.pop().unwrap()
        } else {
            G::One(alts)
        }
    }
    fn any(&mut self) -> G {
        let mut v = vec![self.all()];
        while self.eat("||") {
            v.push(self.all());
        }
        if v.len() == 1 {
            v.pop().unwrap()
        } else {
            G::Any(v)
        }
    }
    fn all(&mut self) -> G {
        let mut v = vec![self.seq()];
        while self.eat("&&") {
            v.push(self.seq());
        }
        if v.len() == 1 {
            v.pop().unwrap()
        } else {
            G::All(v)
        }
    }
    fn seq(&mut self) -> G {
        let mut v = Vec::new();
        loop {
            match self.peek() {
                None | Some(b'|') | Some(b']') | Some(b')') => break,
                Some(b'&') if self.s.get(self.i + 1) == Some(&b'&') => break,
                _ => {}
            }
            v.push(self.term());
        }
        if v.len() == 1 {
            v.pop().unwrap()
        } else {
            G::Seq(v)
        }
    }
    fn num(&mut self) -> u32 {
        self.ws();
        let st = self.i;
        while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[st..self.i]).unwrap().parse().unwrap_or(0)
    }
    fn braces(&mut self) -> (u32, u32) {
        let a = self.num();
        let b = if self.eat(",") {
            self.ws();
            if self.s.get(self.i) == Some(&b'}') {
                u32::MAX
            } else {
                self.num()
            }
        } else {
            a
        };
        self.eat("}");
        (a, b)
    }
    fn term(&mut self) -> G {
        let mut g = self.atom();
        loop {
            // Multipliers bind tightly (no whitespace before them).
            match self.s.get(self.i) {
                Some(b'?') => {
                    self.i += 1;
                    g = G::Opt(Box::new(g));
                }
                Some(b'*') => {
                    self.i += 1;
                    g = G::Mult(Box::new(g), 0, u32::MAX, false);
                }
                Some(b'+') => {
                    self.i += 1;
                    g = G::Mult(Box::new(g), 1, u32::MAX, false);
                }
                Some(b'!') => {
                    self.i += 1;
                    g = G::Req(Box::new(g));
                }
                Some(b'#') => {
                    self.i += 1;
                    if self.s.get(self.i) == Some(&b'{') {
                        self.i += 1;
                        let (a, b) = self.braces();
                        g = G::Mult(Box::new(g), a, b, true);
                    } else {
                        g = G::Mult(Box::new(g), 1, u32::MAX, true);
                    }
                }
                Some(b'{') => {
                    self.i += 1;
                    let (a, b) = self.braces();
                    g = G::Mult(Box::new(g), a, b, false);
                }
                _ => break,
            }
        }
        g
    }
    fn atom(&mut self) -> G {
        self.ws();
        let c = self.s[self.i];
        match c {
            b'[' => {
                self.i += 1;
                let g = self.one();
                self.eat("]");
                g
            }
            b'<' => {
                self.i += 1;
                let st = self.i;
                while self.s[self.i] != b'>' {
                    self.i += 1;
                }
                let inner = std::str::from_utf8(&self.s[st..self.i]).unwrap().trim().to_string();
                self.i += 1;
                if let Some(p) = inner.strip_prefix('\'') {
                    return G::Prop(p.trim_end_matches('\'').to_string());
                }
                if let Some(sp) = inner.find(" [") {
                    let name = inner[..sp].to_string();
                    let r = inner[sp + 2..].trim_end_matches(']');
                    let mut it = r.split(',');
                    let f = |s: &str| match s.trim() {
                        "∞" | "inf" => f64::INFINITY,
                        "-∞" | "-inf" => f64::NEG_INFINITY,
                        x => x.parse().unwrap_or(0.0),
                    };
                    let lo = f(it.next().unwrap_or("-inf"));
                    let hi = f(it.next().unwrap_or("inf"));
                    return G::Type(name, Some((lo, hi)));
                }
                G::Type(inner, None)
            }
            b',' => {
                self.i += 1;
                G::Lit(',')
            }
            b'/' => {
                self.i += 1;
                G::Lit('/')
            }
            b'\'' => {
                self.i += 1;
                let ch = self.s[self.i] as char;
                self.i += 2;
                G::Lit(ch)
            }
            _ => {
                let st = self.i;
                while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'-' || self.s[self.i] == b'_' || self.s[self.i] >= 0x80) {
                    self.i += 1;
                }
                let name = std::str::from_utf8(&self.s[st..self.i]).unwrap().to_string();
                if name.is_empty() {
                    panic!("bad grammar at {}: {}", self.i, String::from_utf8_lossy(self.s));
                }
                if self.s.get(self.i) == Some(&b'(') {
                    self.i += 1;
                    let inner = if self.peek() == Some(b')') { G::Seq(vec![]) } else { self.one() };
                    self.eat(")");
                    return G::Func(name, Box::new(inner));
                }
                G::Kw(name)
            }
        }
    }
}

/// Parse a grammar string.
pub fn parse(src: &str) -> G {
    let mut p = P { s: src.as_bytes(), i: 0 };
    let g = p.one();
    p.ws();
    assert!(p.i >= p.s.len(), "trailing grammar text: {src}");
    g
}

// ------------------------------------------------------------------ matching

/// Resolves types and property references while matching.
pub trait Resolver {
    /// Match a type at `pos`: candidate (end, value) pairs, preferred first.
    fn leaf(&self, name: &str, range: Option<(f64, f64)>, inp: &[&CV], pos: usize) -> Vec<(usize, V)>;
    /// The grammar of a named non-terminal or property.
    fn grammar(&self, name: &str, prop: bool) -> Option<std::rc::Rc<G>>;
}

pub type Res = Vec<(usize, V)>;

fn dedupe(mut v: Res) -> Res {
    let mut seen: Vec<usize> = Vec::new();
    v.retain(|(p, _)| {
        if seen.contains(p) {
            false
        } else {
            seen.push(*p);
            true
        }
    });
    v
}

/// Non-whitespace component values.
pub fn items(v: &[CV]) -> Vec<&CV> {
    v.iter().filter(|c| !c.is_ws()).collect()
}

pub fn m(r: &dyn Resolver, g: &G, inp: &[&CV], pos: usize) -> Res {
    match g {
        G::Kw(k) => match inp.get(pos) {
            Some(CV::T(Tok::Ident(s))) if s.eq_ignore_ascii_case(k) => vec![(pos + 1, V::Kw(k.clone()))],
            _ => vec![],
        },
        G::Lit(c) => match inp.get(pos) {
            Some(CV::T(Tok::Comma)) if *c == ',' => vec![(pos + 1, V::Delim(','))],
            Some(CV::T(Tok::Delim(d))) if d == c => vec![(pos + 1, V::Delim(*c))],
            Some(CV::T(Tok::Colon)) if *c == ':' => vec![(pos + 1, V::Delim(':'))],
            _ => vec![],
        },
        G::Type(name, range) => {
            if let Some(gr) = r.grammar(name, false) {
                return m(r, &gr, inp, pos);
            }
            r.leaf(name, *range, inp, pos)
        }
        G::Prop(name) => match r.grammar(name, true) {
            Some(gr) => m(r, &gr, inp, pos),
            None => vec![],
        },
        G::Func(name, inner) => match inp.get(pos) {
            Some(CV::Func(n, args)) if n.eq_ignore_ascii_case(name) => {
                let a = items(args);
                let res = m(r, inner, &a, 0);
                match res.into_iter().find(|(p, _)| *p == a.len()) {
                    Some((_, v)) => vec![(pos + 1, V::Fn(name.clone(), Box::new(v)))],
                    None => vec![],
                }
            }
            _ => vec![],
        },
        G::Seq(parts) => {
            let mut cur: Vec<(usize, Vec<V>)> = vec![(pos, Vec::new())];
            for p in parts {
                let mut next = Vec::new();
                for (at, vals) in &cur {
                    for (e, v) in m(r, p, inp, *at) {
                        let mut nv = vals.clone();
                        nv.push(v);
                        next.push((e, nv));
                    }
                }
                // Dedupe by end position (first = preferred).
                let mut seen: Vec<usize> = Vec::new();
                next.retain(|(e, _)| {
                    if seen.contains(e) {
                        false
                    } else {
                        seen.push(*e);
                        true
                    }
                });
                if next.is_empty() {
                    return vec![];
                }
                cur = next;
            }
            cur.into_iter().map(|(e, v)| (e, V::Seq(v))).collect()
        }
        G::One(alts) => {
            let mut out = Vec::new();
            for a in alts {
                out.extend(m(r, a, inp, pos));
            }
            // Longest first: a later alternative may consume more.
            out.sort_by(|a, b| b.0.cmp(&a.0));
            dedupe(out)
        }
        G::All(parts) | G::Any(parts) => {
            let all = matches!(g, G::All(_));
            let mut out: Vec<(usize, Vec<V>)> = Vec::new();
            let mut slots: Vec<V> = vec![V::None; parts.len()];
            combo(r, parts, inp, pos, 0, all, &mut slots, &mut out);
            out.sort_by(|a, b| b.0.cmp(&a.0));
            let mut seen: Vec<usize> = Vec::new();
            out.retain(|(e, _)| {
                if seen.contains(e) {
                    false
                } else {
                    seen.push(*e);
                    true
                }
            });
            out.into_iter().map(|(e, v)| (e, V::Group(v))).collect()
        }
        G::Opt(inner) => {
            let mut out: Res = m(r, inner, inp, pos).into_iter().filter(|(e, _)| *e > pos).collect();
            out.push((pos, V::None));
            dedupe(out)
        }
        G::Req(inner) => m(r, inner, inp, pos).into_iter().filter(|(_, v)| !v.is_empty_group()).collect(),
        G::Mult(inner, min, max, comma) => {
            let mut levels: Vec<Vec<(usize, Vec<V>)>> = vec![vec![(pos, Vec::new())]];
            let mut k = 0u32;
            while k < *max {
                let mut next = Vec::new();
                for (at, vals) in levels.last().unwrap() {
                    let mut start = *at;
                    if *comma && k > 0 {
                        match inp.get(start) {
                            Some(CV::T(Tok::Comma)) => start += 1,
                            _ => continue,
                        }
                    }
                    for (e, v) in m(r, inner, inp, start) {
                        if e > start {
                            let mut nv = vals.clone();
                            nv.push(v);
                            next.push((e, nv));
                        }
                    }
                }
                let mut seen: Vec<usize> = Vec::new();
                next.retain(|(e, _)| {
                    if seen.contains(e) {
                        false
                    } else {
                        seen.push(*e);
                        true
                    }
                });
                if next.is_empty() {
                    break;
                }
                levels.push(next);
                k += 1;
            }
            let mut out = Vec::new();
            for (n, lv) in levels.into_iter().enumerate().rev() {
                if (n as u32) < *min {
                    continue;
                }
                for (e, v) in lv {
                    out.push((e, if *comma { V::List(v) } else { V::Seq(v) }));
                }
            }
            dedupe(out)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn combo(r: &dyn Resolver, parts: &[G], inp: &[&CV], pos: usize, used: u64, all: bool, slots: &mut Vec<V>, out: &mut Vec<(usize, Vec<V>)>) {
    let n = parts.len();
    for i in 0..n {
        if used & (1 << i) != 0 {
            continue;
        }
        for (e, v) in m(r, &parts[i], inp, pos) {
            if e == pos {
                continue;
            }
            slots[i] = v;
            combo(r, parts, inp, e, used | (1 << i), all, slots, out);
            slots[i] = V::None;
        }
    }
    let done = if all {
        // Every part present (parts that can match nothing count as present).
        (0..n).all(|i| used & (1 << i) != 0 || m(r, &parts[i], inp, pos).iter().any(|(e, _)| *e == pos))
    } else {
        used != 0
    };
    if done {
        let mut vals = slots.clone();
        if all {
            for i in 0..n {
                if used & (1 << i) == 0 {
                    if let Some((_, v)) = m(r, &parts[i], inp, pos).into_iter().find(|(e, _)| *e == pos) {
                        vals[i] = v;
                    }
                }
            }
        }
        out.push((pos, vals));
    }
}

/// Match a whole value; None unless the grammar consumes every item.
pub fn match_all(r: &dyn Resolver, g: &G, inp: &[&CV]) -> Option<V> {
    m(r, g, inp, 0).into_iter().find(|(e, _)| *e == inp.len()).map(|(_, v)| v)
}
