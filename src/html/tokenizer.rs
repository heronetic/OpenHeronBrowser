// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HTML tokenizer: a forgiving state machine over UTF-8 input.

use super::entities;
use crate::dom::Attr;

#[derive(Debug, Clone)]
pub enum Token {
    Doctype(String),
    StartTag { name: String, attrs: Vec<Attr>, self_closing: bool },
    EndTag { name: String },
    Text(String),
    Comment(String),
    Eof,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RawKind {
    RawText,
    RcData,
    PlainText,
}

pub struct Tokenizer<'a> {
    src: &'a str,
    pos: usize,
    raw: Option<(RawKind, String)>,
    pub cdata_allowed: bool,
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
}

impl<'a> Tokenizer<'a> {
    pub fn new(src: &'a str) -> Self {
        Tokenizer { src, pos: 0, raw: None, cdata_allowed: false }
    }

    /// Switch to raw text mode until `</name>` (used for script, style...).
    pub fn set_rawtext(&mut self, name: &str) {
        self.raw = Some((RawKind::RawText, name.to_string()));
    }
    pub fn set_rcdata(&mut self, name: &str) {
        self.raw = Some((RawKind::RcData, name.to_string()));
    }
    pub fn set_plaintext(&mut self) {
        self.raw = Some((RawKind::PlainText, String::new()));
    }

    fn bytes(&self) -> &'a [u8] {
        self.src.as_bytes()
    }

    pub fn next_token(&mut self) -> Token {
        if let Some((kind, name)) = self.raw.take() {
            return self.raw_text(kind, &name);
        }
        let b = self.bytes();
        if self.pos >= b.len() {
            return Token::Eof;
        }
        if b[self.pos] == b'<' {
            if let Some(t) = self.tag() {
                return t;
            }
            // Not a tag: emit '<' literally plus following text.
            let start = self.pos;
            self.pos += 1;
            let end = self.find_lt(self.pos);
            self.pos = end;
            return Token::Text(entities::decode(&self.src[start..end], false));
        }
        let start = self.pos;
        let end = self.find_lt(start);
        self.pos = end;
        Token::Text(entities::decode(&self.src[start..end], false))
    }

    fn find_lt(&self, from: usize) -> usize {
        match self.src[from..].find('<') {
            Some(i) => from + i,
            None => self.src.len(),
        }
    }

    fn raw_text(&mut self, kind: RawKind, name: &str) -> Token {
        let start = self.pos;
        if kind == RawKind::PlainText {
            self.pos = self.src.len();
            return Token::Text(self.src[start..].to_string());
        }
        let b = self.bytes();
        let mut i = start;
        let end = loop {
            match self.src[i..].find("</") {
                None => break self.src.len(),
                Some(off) => {
                    let p = i + off;
                    let n = p + 2;
                    if n + name.len() <= b.len() && b[n..n + name.len()].eq_ignore_ascii_case(name.as_bytes()) {
                        let after = b.get(n + name.len()).copied();
                        if matches!(after, None | Some(b'>') | Some(b'/')) || after.map(is_ws).unwrap_or(false) {
                            break p;
                        }
                    }
                    i = p + 2;
                }
            }
        };
        self.pos = end;
        // Queue the end tag by leaving pos at "</name"; the normal tag path handles it.
        let text = &self.src[start..end];
        if text.is_empty() {
            return self.next_token();
        }
        Token::Text(if kind == RawKind::RcData { entities::decode(text, false) } else { text.to_string() })
    }

    fn tag(&mut self) -> Option<Token> {
        let b = self.bytes();
        let p = self.pos;
        let next = *b.get(p + 1)?;
        match next {
            b'!' => {
                if self.src[p..].starts_with("<!--") {
                    let body_start = p + 4;
                    // "<!-->" and "<!--->" are empty comments.
                    if self.src[body_start..].starts_with('>') {
                        self.pos = body_start + 1;
                        return Some(Token::Comment(String::new()));
                    }
                    if self.src[body_start..].starts_with("->") {
                        self.pos = body_start + 2;
                        return Some(Token::Comment(String::new()));
                    }
                    let (end, skip) = match self.src[body_start..].find("-->") {
                        Some(i) => {
                            let alt = self.src[body_start..].find("--!>");
                            match alt {
                                Some(j) if j < i => (body_start + j, 4),
                                _ => (body_start + i, 3),
                            }
                        }
                        None => (self.src.len(), 0),
                    };
                    let text = self.src[body_start..end].to_string();
                    self.pos = (end + skip).min(self.src.len());
                    return Some(Token::Comment(text));
                }
                if self.src.len() >= p + 9 && b[p + 2..p + 9].eq_ignore_ascii_case(b"doctype") {
                    let end = self.src[p..].find('>').map(|i| p + i).unwrap_or(self.src.len());
                    let inner = self.src[p + 9..end].trim();
                    let name = inner.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
                    self.pos = (end + 1).min(self.src.len());
                    let _ = inner;
                    let low = inner.to_ascii_lowercase();
                    let has_system = low.contains("\"http") || low.contains("'http") || low.matches('"').count() >= 4;
                    let quirks = low.contains("//dtd html 3")
                        || low.contains("//dtd html 2")
                        || low.contains("-//ietf//dtd html")
                        || low.contains("-//w3c//dtd html 4.0 transitional")
                        || low.contains("-//w3c//dtd html 4.0 frameset")
                        || low.contains("-//w3o//dtd w3 html")
                        || low.contains("-//netscape comm. corp.//dtd")
                        || low.contains("-//microsoft//dtd internet explorer")
                        || ((low.contains("-//w3c//dtd html 4.01 transitional") || low.contains("-//w3c//dtd html 4.01 frameset")) && !has_system);
                    let limited = low.contains("-//w3c//dtd xhtml 1.0 transitional")
                        || low.contains("-//w3c//dtd xhtml 1.0 frameset")
                        || ((low.contains("-//w3c//dtd html 4.01 transitional") || low.contains("-//w3c//dtd html 4.01 frameset")) && has_system);
                    // Public / system identifiers (carried after NUL separators).
                    let after = inner.get(inner.find(char::is_whitespace).unwrap_or(inner.len())..).unwrap_or("").trim_start();
                    let quoted = |s: &str| -> Vec<String> {
                        let mut out = Vec::new();
                        let mut it = s.char_indices().peekable();
                        while let Some((_, ch)) = it.next() {
                            if ch == '"' || ch == '\'' {
                                let mut v = String::new();
                                for (_, c2) in it.by_ref() {
                                    if c2 == ch {
                                        break;
                                    }
                                    v.push(c2);
                                }
                                out.push(v);
                            }
                        }
                        out
                    };
                    let al = after.to_ascii_lowercase();
                    let (pubid, sysid) = if al.starts_with("public") {
                        let q = quoted(&after[6..]);
                        (q.first().cloned().unwrap_or_default(), q.get(1).cloned().unwrap_or_default())
                    } else if al.starts_with("system") {
                        (String::new(), quoted(&after[6..]).first().cloned().unwrap_or_default())
                    } else {
                        (String::new(), String::new())
                    };
                    let head = if quirks {
                        format!("{name} quirks")
                    } else if limited {
                        format!("{name} limited")
                    } else {
                        name
                    };
                    return Some(Token::Doctype(format!("{head}\0{pubid}\0{sysid}")));
                }
                if self.cdata_allowed && self.src[p..].starts_with("<![CDATA[") {
                    let s = p + 9;
                    let end = self.src[s..].find("]]>").map(|i| s + i).unwrap_or(self.src.len());
                    self.pos = (end + 3).min(self.src.len());
                    return Some(Token::Text(self.src[s..end].to_string()));
                }
                // Bogus comment.
                let end = self.src[p..].find('>').map(|i| p + i).unwrap_or(self.src.len());
                let text = self.src[(p + 2).min(end)..end].to_string();
                self.pos = (end + 1).min(self.src.len());
                Some(Token::Comment(text))
            }
            b'?' => {
                let end = self.src[p..].find('>').map(|i| p + i).unwrap_or(self.src.len());
                let text = self.src[p + 1..end].to_string();
                self.pos = (end + 1).min(self.src.len());
                Some(Token::Comment(text))
            }
            b'/' => {
                let c = *b.get(p + 2)?;
                if c == b'>' {
                    self.pos = p + 3; // "</>" is ignored
                    return Some(Token::Comment(String::new()));
                }
                if !c.is_ascii_alphabetic() {
                    // Bogus comment.
                    let end = self.src[p..].find('>').map(|i| p + i).unwrap_or(self.src.len());
                    let text = self.src[p + 2..end].to_string();
                    self.pos = (end + 1).min(self.src.len());
                    return Some(Token::Comment(text));
                }
                let mut i = p + 2;
                while i < b.len() && !is_ws(b[i]) && b[i] != b'>' && b[i] != b'/' {
                    i += 1;
                }
                let name = self.src[p + 2..i].to_ascii_lowercase();
                // Skip anything (including attributes) until '>'.
                let end = self.skip_to_tag_end(i);
                self.pos = end;
                Some(Token::EndTag { name })
            }
            c if c.is_ascii_alphabetic() => {
                let mut i = p + 1;
                while i < b.len() && !is_ws(b[i]) && b[i] != b'>' && b[i] != b'/' {
                    i += 1;
                }
                let name = self.src[p + 1..i].to_ascii_lowercase();
                let (attrs, self_closing, end) = self.attributes(i);
                self.pos = end;
                Some(Token::StartTag { name, attrs, self_closing })
            }
            _ => None,
        }
    }

    fn skip_to_tag_end(&self, mut i: usize) -> usize {
        let b = self.bytes();
        let mut quote: Option<u8> = None;
        while i < b.len() {
            let c = b[i];
            match quote {
                Some(q) if c == q => quote = None,
                Some(_) => {}
                None if c == b'"' || c == b'\'' => quote = Some(c),
                None if c == b'>' => return i + 1,
                None => {}
            }
            i += 1;
        }
        b.len()
    }

    fn attributes(&self, mut i: usize) -> (Vec<Attr>, bool, usize) {
        let b = self.bytes();
        let mut attrs: Vec<Attr> = Vec::new();
        let mut self_closing = false;
        loop {
            while i < b.len() && is_ws(b[i]) {
                i += 1;
            }
            if i >= b.len() {
                return (attrs, self_closing, b.len());
            }
            match b[i] {
                b'>' => return (attrs, self_closing, i + 1),
                b'/' => {
                    i += 1;
                    if i < b.len() && b[i] == b'>' {
                        self_closing = true;
                    }
                    continue;
                }
                _ => {}
            }
            // Attribute name (first char may be '=' per spec quirk).
            let ns = i;
            i += 1;
            while i < b.len() && !is_ws(b[i]) && b[i] != b'=' && b[i] != b'>' && b[i] != b'/' {
                i += 1;
            }
            let name = self.src[ns..i].to_ascii_lowercase();
            while i < b.len() && is_ws(b[i]) {
                i += 1;
            }
            let mut value = String::new();
            if i < b.len() && b[i] == b'=' {
                i += 1;
                while i < b.len() && is_ws(b[i]) {
                    i += 1;
                }
                if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                    let q = b[i];
                    let vs = i + 1;
                    let mut j = vs;
                    while j < b.len() && b[j] != q {
                        j += 1;
                    }
                    value = entities::decode(&self.src[vs..j], true);
                    i = (j + 1).min(b.len());
                } else {
                    let vs = i;
                    while i < b.len() && !is_ws(b[i]) && b[i] != b'>' {
                        i += 1;
                    }
                    value = entities::decode(&self.src[vs..i], true);
                }
            }
            if !name.is_empty() && !attrs.iter().any(|a| a.name == name) && attrs.len() < 256 {
                attrs.push(Attr { name, value, ns: None });
            }
        }
    }
}
