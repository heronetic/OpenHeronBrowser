// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! CSS Syntax Level 3 tokenizer + component-value parser.

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Function(String),
    AtKeyword(String),
    Hash(String, bool),
    Str(String),
    Url(String),
    BadUrl,
    Delim(char),
    Number(f64),
    Percentage(f64),
    Dimension(f64, String),
    Ws,
    Cdo,
    Cdc,
    Colon,
    Semicolon,
    Comma,
    LBracket,
    RBracket,
    LParen,
    RParen,
    LBrace,
    RBrace,
}

/// Component value: a token, a function with arguments, or a bracketed block.
#[derive(Clone, Debug, PartialEq)]
pub enum CV {
    T(Tok),
    Func(String, Vec<CV>),
    Block(char, Vec<CV>),
}

impl CV {
    pub fn is_ws(&self) -> bool {
        matches!(self, CV::T(Tok::Ws))
    }
    pub fn ident(&self) -> Option<&str> {
        match self {
            CV::T(Tok::Ident(s)) => Some(s),
            _ => None,
        }
    }
    pub fn is_ident(&self, s: &str) -> bool {
        self.ident().map(|i| i.eq_ignore_ascii_case(s)).unwrap_or(false)
    }
    pub fn is_delim(&self, c: char) -> bool {
        matches!(self, CV::T(Tok::Delim(d)) if *d == c)
    }
    pub fn is_comma(&self) -> bool {
        matches!(self, CV::T(Tok::Comma))
    }
}

pub struct Tokenizer<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
}

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}
fn is_name(c: u8) -> bool {
    is_name_start(c) || c.is_ascii_digit() || c == b'-'
}
fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
}

impl<'a> Tokenizer<'a> {
    pub fn new(src: &'a str) -> Self {
        Tokenizer { s: src.as_bytes(), src, i: 0 }
    }

    fn peek(&self, o: usize) -> u8 {
        *self.s.get(self.i + o).unwrap_or(&0)
    }

    fn valid_escape(&self, o: usize) -> bool {
        self.peek(o) == b'\\' && self.peek(o + 1) != b'\n' && self.i + o + 1 < self.s.len()
    }

    fn starts_ident(&self, o: usize) -> bool {
        let c = self.peek(o);
        if c == b'-' {
            let n = self.peek(o + 1);
            is_name_start(n) || n == b'-' || self.valid_escape(o + 1)
        } else if is_name_start(c) {
            true
        } else {
            self.valid_escape(o)
        }
    }

    fn starts_number(&self) -> bool {
        let c = self.peek(0);
        if c.is_ascii_digit() {
            return true;
        }
        if c == b'+' || c == b'-' {
            let n = self.peek(1);
            return n.is_ascii_digit() || (n == b'.' && self.peek(2).is_ascii_digit());
        }
        c == b'.' && self.peek(1).is_ascii_digit()
    }

    fn escape(&mut self) -> char {
        // assumes '\\' already consumed
        let mut hex = String::new();
        while hex.len() < 6 && self.peek(0).is_ascii_hexdigit() {
            hex.push(self.peek(0) as char);
            self.i += 1;
        }
        if !hex.is_empty() {
            if is_ws(self.peek(0)) {
                self.i += 1;
            }
            let cp = u32::from_str_radix(&hex, 16).unwrap_or(0xFFFD);
            return if cp == 0 || cp > 0x10FFFF || (0xD800..=0xDFFF).contains(&cp) {
                '\u{FFFD}'
            } else {
                char::from_u32(cp).unwrap_or('\u{FFFD}')
            };
        }
        if self.i >= self.s.len() {
            return '\u{FFFD}';
        }
        let ch = self.src[self.i..].chars().next().unwrap_or('\u{FFFD}');
        self.i += ch.len_utf8();
        ch
    }

    fn name(&mut self) -> String {
        let mut out = String::new();
        loop {
            let c = self.peek(0);
            if self.i < self.s.len() && is_name(c) {
                let ch = self.src[self.i..].chars().next().unwrap();
                out.push(ch);
                self.i += ch.len_utf8();
            } else if self.valid_escape(0) {
                self.i += 1;
                out.push(self.escape());
            } else {
                return out;
            }
        }
    }

    fn number(&mut self) -> f64 {
        let start = self.i;
        if matches!(self.peek(0), b'+' | b'-') {
            self.i += 1;
        }
        while self.peek(0).is_ascii_digit() {
            self.i += 1;
        }
        if self.peek(0) == b'.' && self.peek(1).is_ascii_digit() {
            self.i += 1;
            while self.peek(0).is_ascii_digit() {
                self.i += 1;
            }
        }
        if matches!(self.peek(0), b'e' | b'E') {
            let n = self.peek(1);
            if n.is_ascii_digit() || (matches!(n, b'+' | b'-') && self.peek(2).is_ascii_digit()) {
                self.i += 2;
                while self.peek(0).is_ascii_digit() {
                    self.i += 1;
                }
            }
        }
        self.src[start..self.i].parse::<f64>().unwrap_or(0.0)
    }

    fn string(&mut self, q: u8) -> Tok {
        self.i += 1;
        let mut out = String::new();
        loop {
            if self.i >= self.s.len() {
                return Tok::Str(out);
            }
            let c = self.peek(0);
            if c == q {
                self.i += 1;
                return Tok::Str(out);
            }
            if c == b'\n' {
                return Tok::Str(out); // bad string; be lenient
            }
            if c == b'\\' {
                if self.peek(1) == b'\n' {
                    self.i += 2;
                    continue;
                }
                self.i += 1;
                out.push(self.escape());
                continue;
            }
            let ch = self.src[self.i..].chars().next().unwrap();
            out.push(ch);
            self.i += ch.len_utf8();
        }
    }

    fn url(&mut self) -> Tok {
        while is_ws(self.peek(0)) {
            self.i += 1;
        }
        let mut out = String::new();
        loop {
            if self.i >= self.s.len() {
                return Tok::Url(out);
            }
            let c = self.peek(0);
            match c {
                b')' => {
                    self.i += 1;
                    return Tok::Url(out);
                }
                _ if is_ws(c) => {
                    while is_ws(self.peek(0)) {
                        self.i += 1;
                    }
                    if self.peek(0) == b')' || self.i >= self.s.len() {
                        self.i += 1;
                        return Tok::Url(out);
                    }
                    return self.bad_url();
                }
                b'"' | b'\'' | b'(' => return self.bad_url(),
                b'\\' => {
                    self.i += 1;
                    out.push(self.escape());
                }
                _ => {
                    let ch = self.src[self.i..].chars().next().unwrap();
                    out.push(ch);
                    self.i += ch.len_utf8();
                }
            }
        }
    }

    fn bad_url(&mut self) -> Tok {
        while self.i < self.s.len() {
            if self.peek(0) == b')' {
                self.i += 1;
                break;
            }
            if self.valid_escape(0) {
                self.i += 1;
                self.escape();
            } else {
                self.i += 1;
            }
        }
        Tok::BadUrl
    }

    pub fn next(&mut self) -> Option<Tok> {
        // Comments.
        loop {
            if self.peek(0) == b'/' && self.peek(1) == b'*' {
                match self.src[self.i + 2..].find("*/") {
                    Some(e) => self.i += 2 + e + 2,
                    None => self.i = self.s.len(),
                }
            } else {
                break;
            }
        }
        if self.i >= self.s.len() {
            return None;
        }
        let c = self.peek(0);
        if is_ws(c) {
            while is_ws(self.peek(0)) {
                self.i += 1;
            }
            return Some(Tok::Ws);
        }
        match c {
            b'"' | b'\'' => return Some(self.string(c)),
            b'#' => {
                if is_name(self.peek(1)) || self.valid_escape(1) {
                    self.i += 1;
                    let is_id = self.starts_ident(0);
                    let n = self.name();
                    return Some(Tok::Hash(n, is_id));
                }
                self.i += 1;
                return Some(Tok::Delim('#'));
            }
            b'(' => {
                self.i += 1;
                return Some(Tok::LParen);
            }
            b')' => {
                self.i += 1;
                return Some(Tok::RParen);
            }
            b'[' => {
                self.i += 1;
                return Some(Tok::LBracket);
            }
            b']' => {
                self.i += 1;
                return Some(Tok::RBracket);
            }
            b'{' => {
                self.i += 1;
                return Some(Tok::LBrace);
            }
            b'}' => {
                self.i += 1;
                return Some(Tok::RBrace);
            }
            b',' => {
                self.i += 1;
                return Some(Tok::Comma);
            }
            b':' => {
                self.i += 1;
                return Some(Tok::Colon);
            }
            b';' => {
                self.i += 1;
                return Some(Tok::Semicolon);
            }
            b'<' if self.src[self.i..].starts_with("<!--") => {
                self.i += 4;
                return Some(Tok::Cdo);
            }
            b'-' if self.src[self.i..].starts_with("-->") => {
                self.i += 3;
                return Some(Tok::Cdc);
            }
            b'@' => {
                if self.starts_ident(1) {
                    self.i += 1;
                    return Some(Tok::AtKeyword(self.name()));
                }
                self.i += 1;
                return Some(Tok::Delim('@'));
            }
            _ => {}
        }
        if self.starts_number() {
            let n = self.number();
            if self.starts_ident(0) {
                let unit = self.name();
                return Some(Tok::Dimension(n, unit.to_ascii_lowercase()));
            }
            if self.peek(0) == b'%' {
                self.i += 1;
                return Some(Tok::Percentage(n));
            }
            return Some(Tok::Number(n));
        }
        if self.starts_ident(0) {
            let name = self.name();
            if self.peek(0) == b'(' {
                self.i += 1;
                if name.eq_ignore_ascii_case("url") {
                    // url( unquoted ) is a single token; quoted becomes a function.
                    let save = self.i;
                    while is_ws(self.peek(0)) {
                        self.i += 1;
                    }
                    if matches!(self.peek(0), b'"' | b'\'') {
                        self.i = save;
                        return Some(Tok::Function(name.to_ascii_lowercase()));
                    }
                    self.i = save;
                    return Some(self.url());
                }
                return Some(Tok::Function(name.to_ascii_lowercase()));
            }
            return Some(Tok::Ident(name));
        }
        if c == b'\\' {
            self.i += 1;
            return Some(Tok::Delim('\\'));
        }
        let ch = self.src[self.i..].chars().next().unwrap();
        self.i += ch.len_utf8();
        Some(Tok::Delim(ch))
    }
}

pub fn tokenize(src: &str) -> Vec<Tok> {
    let mut t = Tokenizer::new(src);
    let mut v = Vec::with_capacity(src.len() / 4);
    while let Some(tok) = t.next() {
        v.push(tok);
    }
    v
}

/// Group tokens into component values (functions & blocks nested).
pub fn component_values(toks: &[Tok]) -> Vec<CV> {
    let mut i = 0;
    parse_list(toks, &mut i, None, 0)
}

fn parse_list(toks: &[Tok], i: &mut usize, close: Option<&Tok>, depth: usize) -> Vec<CV> {
    let mut out = Vec::new();
    while *i < toks.len() {
        let t = &toks[*i];
        *i += 1;
        if Some(t) == close {
            return out;
        }
        if depth > 64 {
            continue; // pathological nesting
        }
        match t {
            Tok::Function(name) => {
                let args = parse_list(toks, i, Some(&Tok::RParen), depth + 1);
                out.push(CV::Func(name.clone(), args));
            }
            Tok::LParen => out.push(CV::Block('(', parse_list(toks, i, Some(&Tok::RParen), depth + 1))),
            Tok::LBracket => out.push(CV::Block('[', parse_list(toks, i, Some(&Tok::RBracket), depth + 1))),
            Tok::LBrace => out.push(CV::Block('{', parse_list(toks, i, Some(&Tok::RBrace), depth + 1))),
            other => out.push(CV::T(other.clone())),
        }
    }
    out
}

pub fn parse_cvs(src: &str) -> Vec<CV> {
    component_values(&tokenize(src))
}

/// Resolve every `url(…)` (and `src("…")`, `image-set("…")` string) against
/// `base`, the URL of the stylesheet or document the CSS came from, so later
/// stages never see relative URLs. Fragment-only references (`url(#clip)`)
/// stay as written because they point into the document using them.
pub fn absolutize_urls(cvs: &mut [CV], base: &url::Url) {
    fn fix(u: &mut String, base: &url::Url) {
        let t = u.trim();
        if t.is_empty() || t.starts_with('#') || t.starts_with("data:") {
            return;
        }
        if let Ok(abs) = base.join(t) {
            *u = abs.to_string();
        }
    }
    for cv in cvs.iter_mut() {
        match cv {
            CV::T(Tok::Url(u)) => fix(u, base),
            CV::Func(name, args) => {
                let n = name.to_ascii_lowercase();
                if n == "url" || n == "src" || n == "image-set" || n == "-webkit-image-set" {
                    for a in args.iter_mut() {
                        if let CV::T(Tok::Str(u)) = a {
                            fix(u, base);
                        }
                    }
                }
                absolutize_urls(args, base);
            }
            CV::Block(_, inner) => absolutize_urls(inner, base),
            _ => {}
        }
    }
}

/// Trim leading/trailing whitespace component values.
pub fn trim(v: &[CV]) -> &[CV] {
    let mut s = 0;
    let mut e = v.len();
    while s < e && v[s].is_ws() {
        s += 1;
    }
    while e > s && v[e - 1].is_ws() {
        e -= 1;
    }
    &v[s..e]
}

/// Split on top-level commas.
pub fn split_commas(v: &[CV]) -> Vec<&[CV]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in v.iter().enumerate() {
        if c.is_comma() {
            out.push(trim(&v[start..i]));
            start = i + 1;
        }
    }
    out.push(trim(&v[start..]));
    out
}

/// Non-whitespace component values.
pub fn words(v: &[CV]) -> Vec<&CV> {
    v.iter().filter(|c| !c.is_ws()).collect()
}

/// Serialize component values back to CSS text (used for custom properties,
/// getComputedStyle and @supports).
pub fn serialize(v: &[CV]) -> String {
    let mut s = String::new();
    for c in v {
        ser_cv(c, &mut s);
    }
    s
}

fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e9 {
        format!("{}", n as i64)
    } else {
        let s = format!("{:.4}", n);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn ser_cv(c: &CV, s: &mut String) {
    match c {
        CV::T(t) => match t {
            Tok::Ident(i) => s.push_str(i),
            Tok::Function(f) => {
                s.push_str(f);
                s.push('(')
            }
            Tok::AtKeyword(a) => {
                s.push('@');
                s.push_str(a)
            }
            Tok::Hash(h, _) => {
                s.push('#');
                s.push_str(h)
            }
            Tok::Str(x) => {
                s.push('"');
                s.push_str(&x.replace('"', "\\\""));
                s.push('"')
            }
            Tok::Url(u) => {
                s.push_str("url(");
                s.push_str(u);
                s.push(')')
            }
            Tok::BadUrl => s.push_str("url()"),
            Tok::Delim(d) => s.push(*d),
            Tok::Number(n) => s.push_str(&fmt_num(*n)),
            Tok::Percentage(n) => {
                s.push_str(&fmt_num(*n));
                s.push('%')
            }
            Tok::Dimension(n, u) => {
                s.push_str(&fmt_num(*n));
                s.push_str(u)
            }
            Tok::Ws => s.push(' '),
            Tok::Cdo => s.push_str("<!--"),
            Tok::Cdc => s.push_str("-->"),
            Tok::Colon => s.push(':'),
            Tok::Semicolon => s.push(';'),
            Tok::Comma => s.push(','),
            Tok::LBracket => s.push('['),
            Tok::RBracket => s.push(']'),
            Tok::LParen => s.push('('),
            Tok::RParen => s.push(')'),
            Tok::LBrace => s.push('{'),
            Tok::RBrace => s.push('}'),
        },
        CV::Func(n, args) => {
            s.push_str(n);
            s.push('(');
            for a in args {
                ser_cv(a, s);
            }
            s.push(')');
        }
        CV::Block(b, inner) => {
            let close = match b {
                '(' => ')',
                '[' => ']',
                _ => '}',
            };
            s.push(*b);
            for a in inner {
                ser_cv(a, s);
            }
            s.push(close);
        }
    }
}
