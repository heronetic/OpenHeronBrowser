// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! A namespace-aware, non-validating XML parser (XML 1.0 + Namespaces) for
//! XML and XHTML documents, `DOMParser` and `responseXML`.
//!
//! Well-formedness errors stop the parse; the caller decides how to report
//! them (browsers show a `<parsererror>` with what was parsed so far).

use crate::dom::{Attr, Document, Namespace, NodeId, DOCUMENT, XML_NS, XMLNS_NS};

pub const HTML_NS: &str = "http://www.w3.org/1999/xhtml";
pub const SVG_NS: &str = "http://www.w3.org/2000/svg";
pub const MATHML_NS: &str = "http://www.w3.org/1998/Math/MathML";

/// A well-formedness error: 1-based line and column, and a message.
#[derive(Debug, Clone)]
pub struct XmlError {
    pub line: usize,
    pub col: usize,
    pub msg: String,
}

/// Namespaces in XML: a QName is an NCName, or two joined by one colon.
fn valid_qname(n: &str) -> bool {
    match n.split_once(':') {
        None => true,
        Some((p, l)) => !p.is_empty() && !l.is_empty() && !l.contains(':') && l.chars().next().map(|c| is_name_start(c)).unwrap_or(false),
    }
}

struct Parser<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
    doc: &'a mut Document,
    /// Open elements: (node, qualified name, namespace scope length).
    stack: Vec<(NodeId, String, usize)>,
    /// In-scope namespace bindings (prefix, uri); "" is the default namespace.
    scope: Vec<(String, String)>,
    /// Entities declared in the internal DTD subset.
    entities: Vec<(String, String)>,
    /// XHTML doctypes make HTML's named character references available.
    html_entities: bool,
    seen_root: bool,
    /// The document node being filled (DOCUMENT, or a DOMParser document).
    root: NodeId,
}

fn is_name_start(c: char) -> bool {
    c == ':' || c == '_' || c.is_ascii_alphabetic() || (c as u32 >= 0xC0 && !matches!(c as u32, 0xD7 | 0xF7 | 0x300..=0x36F | 0x2000..=0x200B | 0x200E..=0x206F | 0x2190..=0x2BFF | 0x2FF0..=0x3000 | 0xD800..=0xF8FF | 0xFDD0..=0xFDEF | 0xFFFE..=0xFFFF))
}

fn is_name_char(c: char) -> bool {
    is_name_start(c) || c == '-' || c == '.' || c.is_ascii_digit() || c == '\u{B7}' || ('\u{300}'..='\u{36F}').contains(&c) || c == '\u{203F}' || c == '\u{2040}'
}

fn is_xml_char(c: char) -> bool {
    matches!(c as u32, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

impl<'a> Parser<'a> {
    fn err<T>(&self, msg: impl Into<String>) -> Result<T, XmlError> {
        let before = &self.s[..self.i.min(self.s.len())];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0) + 1;
        Err(XmlError { line, col, msg: msg.into() })
    }

    fn peek(&self) -> Option<char> {
        self.s[self.i..].chars().next()
    }

    fn starts(&self, p: &str) -> bool {
        self.s[self.i..].starts_with(p)
    }

    fn skip_ws(&mut self) -> bool {
        let st = self.i;
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
        self.i > st
    }

    fn name(&mut self) -> Result<String, XmlError> {
        let st = self.i;
        match self.peek() {
            Some(c) if is_name_start(c) => self.i += c.len_utf8(),
            _ => return self.err("expected a name"),
        }
        while let Some(c) = self.peek() {
            if !is_name_char(c) {
                break;
            }
            self.i += c.len_utf8();
        }
        Ok(self.s[st..self.i].to_string())
    }

    fn expect(&mut self, p: &str) -> Result<(), XmlError> {
        if self.starts(p) {
            self.i += p.len();
            Ok(())
        } else {
            self.err(format!("expected '{p}'"))
        }
    }

    fn lookup_ns(&self, prefix: &str) -> Option<String> {
        if prefix == "xml" {
            return Some(XML_NS.into());
        }
        if prefix == "xmlns" {
            return Some(XMLNS_NS.into());
        }
        self.scope.iter().rev().find(|(p, _)| p == prefix).map(|(_, u)| u.clone()).filter(|u| !u.is_empty() || prefix.is_empty())
    }

    /// A character or entity reference (after '&').
    fn reference(&mut self, out: &mut String) -> Result<(), XmlError> {
        self.i += 1;
        if self.starts("#") {
            self.i += 1;
            let hex = self.starts("x");
            if hex {
                self.i += 1;
            }
            let st = self.i;
            while self.i < self.b.len() && (self.b[self.i] as char).is_ascii_alphanumeric() {
                self.i += 1;
            }
            let digits = &self.s[st..self.i];
            let v = if hex { u32::from_str_radix(digits, 16) } else { digits.parse::<u32>() };
            let c = v.ok().and_then(char::from_u32).filter(|c| is_xml_char(*c));
            let Some(c) = c else { return self.err("invalid character reference") };
            self.expect(";")?;
            out.push(c);
            return Ok(());
        }
        let n = self.name()?;
        self.expect(";")?;
        match n.as_str() {
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "amp" => out.push('&'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                if let Some((_, v)) = self.entities.iter().find(|(k, _)| *k == n) {
                    out.push_str(&v.clone());
                } else if let Some(v) = self.html_entities.then(|| crate::html::entities::lookup(&n)).flatten() {
                    out.push_str(v);
                } else {
                    return self.err(format!("Entity '{n}' not defined"));
                }
            }
        }
        Ok(())
    }

    fn attr_value(&mut self) -> Result<String, XmlError> {
        let q = match self.peek() {
            Some(c @ ('"' | '\'')) => c,
            _ => return self.err("AttValue: \" or ' expected"),
        };
        self.i += 1;
        let mut v = String::new();
        loop {
            let Some(c) = self.peek() else { return self.err("AttValue: ' expected") };
            if c == q {
                self.i += 1;
                break;
            }
            match c {
                '<' => return self.err("Unescaped '<' not allowed in attributes values"),
                '&' => self.reference(&mut v)?,
                '\t' | '\n' | '\r' => {
                    v.push(' ');
                    self.i += 1;
                }
                c => {
                    v.push(c);
                    self.i += c.len_utf8();
                }
            }
        }
        Ok(v)
    }

    fn append(&mut self, n: NodeId) {
        let parent = self.stack.last().map(|s| s.0).unwrap_or(self.root);
        self.doc.append_child(parent, n);
    }

    fn text(&mut self, t: &str) {
        let parent = self.stack.last().map(|s| s.0).unwrap_or(self.root);
        // Merge with a preceding text node.
        if let Some(&last) = self.doc.children(parent).last() {
            if let (crate::dom::NodeData::Text(prev), false) = (&self.doc.node(last).data, self.doc.cdata.contains(&last)) {
                let merged = format!("{prev}{t}");
                self.doc.set_text(last, &merged);
                return;
            }
        }
        let n = self.doc.create_text(t);
        self.doc.append_child(parent, n);
    }

    fn doctype(&mut self) -> Result<(), XmlError> {
        self.i += "<!DOCTYPE".len();
        if !self.skip_ws() {
            return self.err("Space required after '<!DOCTYPE'");
        }
        let name = self.name()?;
        self.skip_ws();
        let mut public = String::new();
        let mut system = String::new();
        let quoted = |p: &mut Parser| -> Result<String, XmlError> {
            let q = match p.peek() {
                Some(c @ ('"' | '\'')) => c,
                _ => return p.err("quoted literal expected"),
            };
            p.i += 1;
            let st = p.i;
            while p.peek().map(|c| c != q).unwrap_or(false) {
                p.i += p.peek().unwrap().len_utf8();
            }
            let v = p.s[st..p.i].to_string();
            p.expect(&q.to_string())?;
            Ok(v)
        };
        if self.starts("PUBLIC") {
            self.i += 6;
            self.skip_ws();
            public = quoted(self)?;
            self.skip_ws();
            if matches!(self.peek(), Some('"' | '\'')) {
                system = quoted(self)?;
            }
        } else if self.starts("SYSTEM") {
            self.i += 6;
            self.skip_ws();
            system = quoted(self)?;
        }
        self.skip_ws();
        if self.starts("[") {
            // Internal subset: keep general entity declarations, skip the rest.
            self.i += 1;
            loop {
                self.skip_ws();
                if self.starts("]") {
                    self.i += 1;
                    break;
                }
                if self.i >= self.b.len() {
                    return self.err("DOCTYPE improperly terminated");
                }
                if self.starts("<!ENTITY") {
                    self.i += 8;
                    self.skip_ws();
                    let param = self.starts("%");
                    if param {
                        self.i += 1;
                        self.skip_ws();
                    }
                    let en = self.name()?;
                    self.skip_ws();
                    if matches!(self.peek(), Some('"' | '\'')) {
                        let q = self.peek().unwrap();
                        self.i += 1;
                        let mut v = String::new();
                        while let Some(c) = self.peek() {
                            if c == q {
                                break;
                            }
                            if c == '&' && !self.starts("&#") {
                                // Entity references in entity values stay as text until used.
                                v.push('&');
                                self.i += 1;
                                continue;
                            }
                            if c == '&' {
                                self.reference(&mut v)?;
                                continue;
                            }
                            v.push(c);
                            self.i += c.len_utf8();
                        }
                        self.expect(&q.to_string())?;
                        if !param && !self.entities.iter().any(|(k, _)| *k == en) {
                            self.entities.push((en, v));
                        }
                    }
                    while self.i < self.b.len() && self.b[self.i] != b'>' {
                        self.i += 1;
                    }
                    self.i += 1;
                } else if self.starts("<!--") {
                    let e = self.s[self.i + 4..].find("-->").ok_or_else(|| self.err::<()>("Comment not terminated").unwrap_err())?;
                    self.i += 4 + e + 3;
                } else if self.starts("<?") {
                    let e = self.s[self.i..].find("?>").ok_or_else(|| self.err::<()>("PI not terminated").unwrap_err())?;
                    self.i += e + 2;
                } else if self.starts("<!") {
                    // ELEMENT / ATTLIST / NOTATION: skip to the matching '>' (outside quotes).
                    let mut q: Option<u8> = None;
                    while self.i < self.b.len() {
                        let c = self.b[self.i];
                        self.i += 1;
                        match q {
                            Some(x) if c == x => q = None,
                            Some(_) => {}
                            None if c == b'"' || c == b'\'' => q = Some(c),
                            None if c == b'>' => break,
                            None => {}
                        }
                    }
                } else if self.starts("%") {
                    while self.i < self.b.len() && self.b[self.i] != b';' {
                        self.i += 1;
                    }
                    self.i += 1;
                } else {
                    return self.err("internal subset: unexpected content");
                }
            }
            self.skip_ws();
        }
        self.expect(">")?;
        if public.contains("XHTML") || system.contains("xhtml") {
            self.html_entities = true;
        }
        let dt = self.doc.create_doctype(&format!("{name}\0{public}\0{system}"));
        self.append(dt);
        Ok(())
    }

    fn start_tag(&mut self) -> Result<(), XmlError> {
        self.i += 1;
        let qname = self.name()?;
        if !valid_qname(&qname) {
            return self.err(format!("Failed to parse QName '{qname}'"));
        }
        let mut raw: Vec<(String, String)> = Vec::new();
        let mut empty = false;
        loop {
            let had_ws = self.skip_ws();
            if self.starts("/>") {
                self.i += 2;
                empty = true;
                break;
            }
            if self.starts(">") {
                self.i += 1;
                break;
            }
            if self.i >= self.b.len() {
                return self.err(format!("Couldn't find end of Start Tag {qname}"));
            }
            if !had_ws {
                return self.err("attributes construct error");
            }
            let an = self.name()?;
            if !valid_qname(&an) {
                return self.err(format!("Failed to parse QName '{an}'"));
            }
            self.skip_ws();
            self.expect("=").or_else(|_| self.err(format!("Specification mandates value for attribute {an}")))?;
            self.skip_ws();
            let av = self.attr_value()?;
            if raw.iter().any(|(n, _)| *n == an) {
                return self.err(format!("Attribute {an} redefined"));
            }
            raw.push((an, av));
        }
        if self.stack.is_empty() {
            if self.seen_root {
                return self.err("Extra content at the end of the document");
            }
            self.seen_root = true;
        }
        let scope_len = self.scope.len();
        for (n, v) in &raw {
            if n == "xmlns" {
                if v == XML_NS || v == XMLNS_NS {
                    return self.err("xmlns: reserved namespace URI");
                }
                self.scope.push((String::new(), v.clone()));
            } else if let Some(p) = n.strip_prefix("xmlns:") {
                if v.is_empty() {
                    return self.err(format!("xmlns:{p}: Empty XML namespace is not allowed"));
                }
                if p == "xmlns" || (p == "xml" && v != XML_NS) || (p != "xml" && (v == XML_NS || v == XMLNS_NS)) {
                    return self.err(format!("xmlns:{p}: reserved prefix or namespace"));
                }
                self.scope.push((p.to_string(), v.clone()));
            }
        }
        let (prefix, local) = match qname.split_once(':') {
            Some((p, l)) => (p.to_string(), l.to_string()),
            None => (String::new(), qname.clone()),
        };
        let ns = match self.lookup_ns(&prefix) {
            Some(u) => u,
            None if prefix.is_empty() => String::new(),
            None => return self.err(format!("Namespace prefix {prefix} on {local} is not defined")),
        };
        let mut attrs = Vec::with_capacity(raw.len());
        for (n, v) in raw {
            let ans = if n == "xmlns" || n.starts_with("xmlns:") {
                Some(XMLNS_NS.to_string())
            } else if let Some((p, _)) = n.split_once(':') {
                match self.lookup_ns(p) {
                    Some(u) => Some(u),
                    None => return self.err(format!("Namespace prefix {p} for {} on {local} is not defined", n.split_once(':').map(|x| x.1).unwrap_or(""))),
                }
            } else {
                None
            };
            attrs.push(Attr { name: n, value: v, ns: ans.map(|s| s.into_boxed_str()) });
        }
        let (kind, name) = match ns.as_str() {
            HTML_NS => (Namespace::Html, local.clone()),
            SVG_NS => (Namespace::Svg, local.clone()),
            MATHML_NS => (Namespace::MathMl, local.clone()),
            _ => (Namespace::Other, qname.clone()),
        };
        let el = self.doc.create_element(&name, kind, attrs);
        if kind == Namespace::Other {
            self.doc.ns_uris.insert(el, ns);
        }
        self.append(el);
        if empty {
            self.scope.truncate(scope_len);
        } else {
            self.stack.push((el, qname, scope_len));
        }
        Ok(())
    }

    fn run(&mut self) -> Result<(), XmlError> {
        if self.starts("\u{feff}") {
            self.i += 3;
        }
        if self.starts("<?xml") && self.b.get(5).map(|c| c.is_ascii_whitespace()).unwrap_or(false) {
            let e = self.s[self.i..].find("?>").ok_or_else(|| self.err::<()>("XML declaration not terminated").unwrap_err())?;
            self.i += e + 2;
        }
        let mut text = String::new();
        while self.i < self.b.len() {
            let c = self.b[self.i];
            if c == b'<' {
                if !text.is_empty() {
                    if self.stack.is_empty() {
                        if !text.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r')) {
                            return self.err(if self.seen_root { "Extra content at the end of the document" } else { "Start tag expected, '<' not found" });
                        }
                    } else {
                        let t = std::mem::take(&mut text);
                        self.text(&t);
                    }
                    text.clear();
                }
                if self.starts("<!--") {
                    let rest = &self.s[self.i + 4..];
                    let Some(e) = rest.find("--") else { return self.err("Comment not terminated") };
                    if !rest[e..].starts_with("-->") {
                        self.i += 4 + e;
                        return self.err("Double hyphen within comment");
                    }
                    let body = rest[..e].to_string();
                    self.i += 4 + e + 3;
                    let n = self.doc.create_comment(&body);
                    self.append(n);
                } else if self.starts("<![CDATA[") {
                    if self.stack.is_empty() {
                        return self.err("CDATA section outside the root element");
                    }
                    let Some(e) = self.s[self.i + 9..].find("]]>") else { return self.err("CData section not finished") };
                    let body = self.s[self.i + 9..self.i + 9 + e].to_string();
                    self.i += 9 + e + 3;
                    let n = self.doc.create_cdata(&body);
                    self.append(n);
                } else if self.starts("<!DOCTYPE") {
                    if self.seen_root {
                        return self.err("DOCTYPE after the root element");
                    }
                    self.doctype()?;
                } else if self.starts("<?") {
                    self.i += 2;
                    let target = self.name()?;
                    if target.eq_ignore_ascii_case("xml") {
                        return self.err("XML declaration allowed only at the start of the document");
                    }
                    let had_ws = self.skip_ws();
                    let Some(e) = self.s[self.i..].find("?>") else { return self.err("PI not terminated") };
                    if !had_ws && e > 0 {
                        return self.err(format!("ParsePI: PI {target} space expected"));
                    }
                    let data = self.s[self.i..self.i + e].to_string();
                    self.i += e + 2;
                    let n = self.doc.create_pi(&target, &data);
                    self.append(n);
                } else if self.starts("</") {
                    self.i += 2;
                    let n = self.name()?;
                    self.skip_ws();
                    self.expect(">")?;
                    match self.stack.pop() {
                        Some((_, open, scope_len)) if open == n => self.scope.truncate(scope_len),
                        Some((_, open, _)) => return self.err(format!("Opening and ending tag mismatch: {open} and {n}")),
                        None => return self.err("Extra content at the end of the document"),
                    }
                } else {
                    self.start_tag()?;
                }
            } else if c == b'&' {
                if self.stack.is_empty() {
                    return self.err("Extra content at the end of the document");
                }
                self.reference(&mut text)?;
            } else {
                if self.s[self.i..].starts_with("]]>") {
                    return self.err("Sequence ']]>' not allowed in content");
                }
                let ch = self.peek().unwrap();
                if !is_xml_char(ch) {
                    return self.err("Char 0x0 out of allowed range");
                }
                if ch == '\r' {
                    // End-of-line normalization.
                    text.push('\n');
                    self.i += 1;
                    if self.starts("\n") {
                        self.i += 1;
                    }
                    continue;
                }
                text.push(ch);
                self.i += ch.len_utf8();
            }
        }
        if !text.is_empty() && !self.stack.is_empty() {
            let t = std::mem::take(&mut text);
            self.text(&t);
        }
        if let Some((_, open, _)) = self.stack.last() {
            let open = open.clone();
            return self.err(format!("Premature end of data in tag {open}"));
        }
        if !self.seen_root {
            return self.err("Document is empty");
        }
        Ok(())
    }
}

/// Parse `src` as an XML document into `doc` (appending to its document node).
/// On a well-formedness error, what was parsed so far stays in `doc`.
pub fn parse_into(doc: &mut Document, src: &str) -> Result<(), XmlError> {
    parse_into_node(doc, DOCUMENT, src)
}

/// Parse `src` as an XML document into the (empty) document node `root` of
/// `doc`; created nodes are owned by `root`.
pub fn parse_into_node(doc: &mut Document, root: NodeId, src: &str) -> Result<(), XmlError> {
    let src = if src.contains('\r') { src.replace("\r\n", "\n") } else { src.to_string() };
    let first = doc.nodes.len();
    let mut p = Parser { s: &src, b: src.as_bytes(), i: 0, doc, stack: Vec::new(), scope: Vec::new(), entities: Vec::new(), html_entities: false, seen_root: false, root };
    let r = p.run();
    if root != DOCUMENT {
        for n in first..doc.nodes.len() {
            doc.nodes[n].owner = root;
        }
    }
    r
}

/// Insert a browser-style `<parsererror>` (XHTML namespace) for `e`: as the
/// root's first child, or as the root of an empty document.
pub fn insert_parser_error(doc: &mut Document, e: &XmlError) {
    insert_parser_error_at(doc, DOCUMENT, e)
}

/// `insert_parser_error` for the document node `droot`.
pub fn insert_parser_error_at(doc: &mut Document, droot: NodeId, e: &XmlError) {
    let first = doc.nodes.len();
    let style = "display: block; white-space: pre; border: 2px solid #c77; padding: 0 1em 0 1em; margin: 1em; background-color: #fdd; color: black";
    let pe = doc.create_element("parsererror", Namespace::Html, vec![Attr::new("style", style)]);
    let h3 = doc.create_element("h3", Namespace::Html, Vec::new());
    let t = doc.create_text("This page contains the following errors:");
    doc.append_child(h3, t);
    doc.append_child(pe, h3);
    let div = doc.create_element("div", Namespace::Html, vec![Attr::new("style", "font-family:monospace;font-size:12px")]);
    let t = doc.create_text(&format!("error on line {} at column {}: {}\n", e.line, e.col, e.msg));
    doc.append_child(div, t);
    doc.append_child(pe, div);
    let h3 = doc.create_element("h3", Namespace::Html, Vec::new());
    let t = doc.create_text("Below is a rendering of the page up to the first error.");
    doc.append_child(h3, t);
    doc.append_child(pe, h3);
    let root = doc.children(droot).iter().copied().find(|&c| doc.is_element(c));
    match root {
        Some(r) => {
            let first = doc.children(r).first().copied();
            doc.insert_before(r, pe, first);
        }
        None => {
            let html = doc.create_element("html", Namespace::Html, Vec::new());
            let body = doc.create_element("body", Namespace::Html, Vec::new());
            doc.append_child(droot, html);
            doc.append_child(html, body);
            doc.append_child(body, pe);
        }
    }
    if droot != DOCUMENT {
        for n in first..doc.nodes.len() {
            doc.nodes[n].owner = droot;
        }
    }
}

/// Parse a whole XML document (with a `<parsererror>` on failure).
pub fn parse_document(src: &str, url: url::Url) -> (Document, Option<XmlError>) {
    let mut doc = Document::new(url);
    doc.quirks = false;
    let r = parse_into(&mut doc, src);
    let err = r.err();
    if let Some(e) = &err {
        insert_parser_error(&mut doc, e);
    }
    (doc, err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> (Document, Option<XmlError>) {
        parse_document(s, url::Url::parse("about:blank").unwrap())
    }

    #[test]
    fn basic_namespaces() {
        let (d, e) = parse("<?xml version='1.0'?><root xmlns='urn:a' xmlns:h='http://www.w3.org/1999/xhtml'><h:p class='x'>hi &amp; bye</h:p><b/></root>");
        assert!(e.is_none(), "{e:?}");
        let root = d.children(DOCUMENT)[0];
        assert_eq!(d.ns_uris.get(&root).map(|s| s.as_str()), Some("urn:a"));
        let p = d.children(root)[0];
        assert_eq!(d.element(p).unwrap().ns, Namespace::Html);
        assert_eq!(d.text_content(p), "hi & bye");
    }

    #[test]
    fn errors() {
        assert!(parse("<a><b></a>").1.is_some());
        assert!(parse("<a>&foo;</a>").1.is_some());
        assert!(parse("<a x=1/>").1.is_some());
        assert!(parse("").1.is_some());
        assert!(parse("<a/><b/>").1.is_some());
        assert!(parse("<p:a/>").1.is_some());
        assert!(parse("<!DOCTYPE html PUBLIC \"-//W3C//DTD XHTML 1.0 Strict//EN\" \"x\"><html>&nbsp;</html>").1.is_none());
    }
}
