// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! SVG path data in `path()`: parsing, the canonical serialization (one
//! command letter per segment, space-separated numbers) and interpolation.

#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    /// The command as written (`Z` for both closepath forms).
    pub cmd: char,
    pub args: Vec<f64>,
}

fn arity(c: char) -> Option<usize> {
    Some(match c.to_ascii_uppercase() {
        'M' | 'L' | 'T' => 2,
        'H' | 'V' => 1,
        'C' => 6,
        'S' | 'Q' => 4,
        'A' => 7,
        'Z' => 0,
        _ => return None,
    })
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Lexer<'a> {
    fn skip_ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r' | b'\x0c') {
            self.i += 1;
        }
    }
    fn skip_sep(&mut self) {
        self.skip_ws();
        if self.i < self.s.len() && self.s[self.i] == b',' {
            self.i += 1;
            self.skip_ws();
        }
    }
    fn number(&mut self) -> Option<f64> {
        let st = self.i;
        let s = self.s;
        if self.i < s.len() && (s[self.i] == b'+' || s[self.i] == b'-') {
            self.i += 1;
        }
        let mut digits = 0;
        while self.i < s.len() && s[self.i].is_ascii_digit() {
            self.i += 1;
            digits += 1;
        }
        if self.i < s.len() && s[self.i] == b'.' {
            self.i += 1;
            while self.i < s.len() && s[self.i].is_ascii_digit() {
                self.i += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            self.i = st;
            return None;
        }
        if self.i < s.len() && (s[self.i] == b'e' || s[self.i] == b'E') {
            let save = self.i;
            self.i += 1;
            if self.i < s.len() && (s[self.i] == b'+' || s[self.i] == b'-') {
                self.i += 1;
            }
            let ds = self.i;
            while self.i < s.len() && s[self.i].is_ascii_digit() {
                self.i += 1;
            }
            if self.i == ds {
                self.i = save;
            }
        }
        std::str::from_utf8(&s[st..self.i]).ok()?.parse().ok()
    }
    fn flag(&mut self) -> Option<f64> {
        match self.s.get(self.i) {
            Some(b'0') => {
                self.i += 1;
                Some(0.0)
            }
            Some(b'1') => {
                self.i += 1;
                Some(1.0)
            }
            _ => None,
        }
    }
}

/// Parse path data; None if it isn't valid (an empty string is an empty path).
pub fn parse(text: &str) -> Option<Vec<Seg>> {
    let mut lx = Lexer { s: text.as_bytes(), i: 0 };
    let mut out: Vec<Seg> = Vec::new();
    lx.skip_ws();
    let mut prev: Option<char> = None;
    while lx.i < lx.s.len() {
        let c = lx.s[lx.i] as char;
        let cmd = if arity(c).is_some() {
            lx.i += 1;
            c
        } else {
            // Repeated arguments: the previous command again (lineto after moveto).
            match prev {
                Some('Z') | Some('z') | None => return None,
                Some('M') => 'L',
                Some('m') => 'l',
                Some(p) => p,
            }
        };
        if out.is_empty() && cmd.to_ascii_uppercase() != 'M' {
            return None;
        }
        let n = arity(cmd)?;
        let mut args = Vec::with_capacity(n);
        lx.skip_ws();
        for k in 0..n {
            if k > 0 {
                lx.skip_sep();
            }
            let v = if cmd.to_ascii_uppercase() == 'A' && (k == 3 || k == 4) { lx.flag() } else { lx.number() };
            args.push(v?);
        }
        let cmd = if cmd == 'z' { 'Z' } else { cmd };
        out.push(Seg { cmd, args });
        prev = Some(cmd);
        lx.skip_sep();
        // A comma right after closepath / before a command letter is an error only when trailing.
    }
    Some(out)
}

pub fn serialize(segs: &[Seg]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for s in segs {
        let mut p = s.cmd.to_string();
        for a in &s.args {
            p.push(' ');
            p.push_str(&super::fmt(*a));
        }
        parts.push(p);
    }
    parts.join(" ")
}

/// Canonical form of path data text; None if invalid.
pub fn canonical(text: &str) -> Option<String> {
    parse(text).map(|s| serialize(&s))
}

/// Segments in absolute coordinates (commands uppercased).
fn absolute(segs: &[Seg]) -> Vec<Seg> {
    let (mut cx, mut cy, mut sx, mut sy) = (0.0, 0.0, 0.0, 0.0);
    let mut out = Vec::with_capacity(segs.len());
    for s in segs {
        let rel = s.cmd.is_ascii_lowercase();
        let up = s.cmd.to_ascii_uppercase();
        let mut a = s.args.clone();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        match up {
            'M' => {
                a[0] += ox;
                a[1] += oy;
                cx = a[0];
                cy = a[1];
                sx = cx;
                sy = cy;
            }
            'L' | 'T' => {
                a[0] += ox;
                a[1] += oy;
                cx = a[0];
                cy = a[1];
            }
            'H' => {
                a[0] += ox;
                cx = a[0];
            }
            'V' => {
                a[0] += oy;
                cy = a[0];
            }
            'C' | 'S' | 'Q' => {
                for k in (0..a.len()).step_by(2) {
                    a[k] += ox;
                    a[k + 1] += oy;
                }
                cx = a[a.len() - 2];
                cy = a[a.len() - 1];
            }
            'A' => {
                a[5] += ox;
                a[6] += oy;
                cx = a[5];
                cy = a[6];
            }
            _ => {
                cx = sx;
                cy = sy;
            }
        }
        out.push(Seg { cmd: up, args: a });
    }
    out
}

/// Absolute segments back to the commands of `like` (relative where it is).
#[allow(dead_code)]
fn relative_like(abs: &[Seg], like: &[Seg]) -> Vec<Seg> {
    let (mut cx, mut cy, mut sx, mut sy) = (0.0, 0.0, 0.0, 0.0);
    let mut out = Vec::with_capacity(abs.len());
    for (s, l) in abs.iter().zip(like) {
        let rel = l.cmd.is_ascii_lowercase();
        let mut a = s.args.clone();
        let (ox, oy) = if rel { (cx, cy) } else { (0.0, 0.0) };
        match s.cmd {
            'M' => {
                cx = a[0];
                cy = a[1];
                sx = cx;
                sy = cy;
                a[0] -= ox;
                a[1] -= oy;
            }
            'L' | 'T' => {
                cx = a[0];
                cy = a[1];
                a[0] -= ox;
                a[1] -= oy;
            }
            'H' => {
                cx = a[0];
                a[0] -= ox;
            }
            'V' => {
                cy = a[0];
                a[0] -= oy;
            }
            'C' | 'S' | 'Q' => {
                cx = a[a.len() - 2];
                cy = a[a.len() - 1];
                for k in (0..a.len()).step_by(2) {
                    a[k] -= ox;
                    a[k + 1] -= oy;
                }
            }
            'A' => {
                cx = a[5];
                cy = a[6];
                a[5] -= ox;
                a[6] -= oy;
            }
            _ => {
                cx = sx;
                cy = sy;
            }
        }
        out.push(Seg { cmd: l.cmd, args: a });
    }
    out
}

/// Arc flags: each endpoint's own at or beyond it, either one set in between (as Blink).
fn arc_flag(u: f64, v: f64, p: f64) -> f64 {
    if p <= 0.0 {
        u
    } else if p >= 1.0 {
        v
    } else if u != 0.0 || v != 0.0 {
        1.0
    } else {
        0.0
    }
}

/// Interpolate two paths with matching commands (absolute and relative forms
/// pair up); the result is in absolute commands.
pub fn interpolate(a: &[Seg], b: &[Seg], p: f64) -> Option<Vec<Seg>> {
    if a.len() != b.len() || a.iter().zip(b).any(|(x, y)| x.cmd.to_ascii_uppercase() != y.cmd.to_ascii_uppercase()) {
        return None;
    }
    let (aa, ba) = (absolute(a), absolute(b));
    let mut mix = Vec::with_capacity(a.len());
    for (x, y) in aa.iter().zip(&ba) {
        let args = x
            .args
            .iter()
            .zip(&y.args)
            .enumerate()
            .map(|(k, (u, v))| if x.cmd == 'A' && (k == 3 || k == 4) { arc_flag(*u, *v, p) } else { u + (v - u) * p })
            .collect();
        mix.push(Seg { cmd: x.cmd, args });
    }
    Some(mix)
}

/// Path addition (composite add): coordinates summed, in absolute terms.
pub fn add(a: &[Seg], b: &[Seg]) -> Option<Vec<Seg>> {
    if a.len() != b.len() || a.iter().zip(b).any(|(x, y)| x.cmd.to_ascii_uppercase() != y.cmd.to_ascii_uppercase()) {
        return None;
    }
    let (aa, ba) = (absolute(a), absolute(b));
    let sum: Vec<Seg> = aa
        .iter()
        .zip(&ba)
        .map(|(x, y)| Seg { cmd: x.cmd, args: x.args.iter().zip(&y.args).enumerate().map(|(k, (u, v))| if x.cmd == 'A' && (k == 3 || k == 4) { *v } else { u + v }).collect() })
        .collect();
    Some(sum)
}
