// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronParse — HTML parsing into a HeronDOM `Document`.
//!
//! The tree builder follows the shape of the HTML5 algorithm (implied
//! html/head/body, auto-closing paragraphs and list items, implicit table
//! sections, foster parenting, foreign SVG/MathML content) while staying
//! small and forgiving.

pub mod encoding;
pub mod entities;
pub mod tokenizer;
pub mod xml;

use crate::dom::{Attr, Document, Namespace, NodeData, NodeId, DOCUMENT};
use tokenizer::{Token, Tokenizer};
use url::Url;

const MAX_DEPTH: usize = 384;

fn is_special(name: &str) -> bool {
    matches!(
        name,
        "address" | "applet" | "area" | "article" | "aside" | "base" | "basefont" | "bgsound" | "blockquote"
            | "body" | "br" | "button" | "caption" | "center" | "col" | "colgroup" | "dd" | "details" | "dir"
            | "div" | "dl" | "dt" | "embed" | "fieldset" | "figcaption" | "figure" | "footer" | "form" | "frame"
            | "frameset" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "head" | "header" | "hgroup" | "hr" | "html"
            | "iframe" | "img" | "input" | "li" | "link" | "listing" | "main" | "marquee" | "menu" | "meta" | "nav"
            | "noembed" | "noframes" | "noscript" | "object" | "ol" | "p" | "param" | "plaintext" | "pre"
            | "script" | "section" | "select" | "source" | "style" | "summary" | "table" | "tbody" | "td"
            | "template" | "textarea" | "tfoot" | "th" | "thead" | "title" | "tr" | "track" | "ul" | "wbr" | "xmp"
            | "search" | "dialog"
    )
}

fn closes_p(name: &str) -> bool {
    matches!(
        name,
        "address" | "article" | "aside" | "blockquote" | "center" | "details" | "dialog" | "dir" | "div" | "dl"
            | "fieldset" | "figcaption" | "figure" | "footer" | "header" | "hgroup" | "main" | "menu" | "nav"
            | "ol" | "p" | "search" | "section" | "summary" | "ul" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            | "pre" | "listing" | "form" | "plaintext" | "table" | "hr" | "xmp" | "dd" | "dt" | "li"
    )
}

fn is_heading(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

fn is_head_content(name: &str) -> bool {
    matches!(name, "base" | "basefont" | "bgsound" | "link" | "meta" | "title" | "style" | "script" | "noscript" | "template")
}

fn is_table_structure(name: &str) -> bool {
    matches!(name, "table" | "caption" | "colgroup" | "col" | "tbody" | "thead" | "tfoot" | "tr" | "td" | "th")
}

/// The camelCase spelling of a lowercased SVG attribute name.
fn svg_attr_case(name: &str) -> Option<&'static str> {
    const NAMES: &[&str] = &[
        "attributeName", "attributeType", "baseFrequency", "baseProfile", "calcMode", "clipPathUnits", "diffuseConstant", "edgeMode", "filterUnits",
        "glyphRef", "gradientTransform", "gradientUnits", "kernelMatrix", "kernelUnitLength", "keyPoints", "keySplines", "keyTimes", "lengthAdjust",
        "limitingConeAngle", "markerHeight", "markerUnits", "markerWidth", "maskContentUnits", "maskUnits", "numOctaves", "pathLength",
        "patternContentUnits", "patternTransform", "patternUnits", "pointsAtX", "pointsAtY", "pointsAtZ", "preserveAlpha", "preserveAspectRatio",
        "primitiveUnits", "refX", "refY", "repeatCount", "repeatDur", "requiredExtensions", "requiredFeatures", "specularConstant",
        "specularExponent", "spreadMethod", "startOffset", "stdDeviation", "stitchTiles", "surfaceScale", "systemLanguage", "tableValues",
        "targetX", "targetY", "textLength", "viewBox", "viewTarget", "xChannelSelector", "yChannelSelector", "zoomAndPan",
    ];
    NAMES.iter().find(|n| n.eq_ignore_ascii_case(name)).copied()
}

/// Elements that, appearing inside foreign (SVG/MathML) content, break out of it.
fn breaks_foreign(name: &str) -> bool {
    matches!(
        name,
        "b" | "big" | "blockquote" | "body" | "br" | "center" | "code" | "dd" | "div" | "dl" | "dt" | "em" | "embed"
            | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "head" | "hr" | "i" | "img" | "li" | "listing" | "menu"
            | "meta" | "nobr" | "ol" | "p" | "pre" | "ruby" | "s" | "small" | "span" | "strong" | "strike" | "sub"
            | "sup" | "table" | "tt" | "u" | "ul" | "var"
    )
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum Phase {
    BeforeHtml,
    InHead,
    InBody,
}

struct TreeBuilder<'d> {
    doc: &'d mut Document,
    stack: Vec<NodeId>,
    html: Option<NodeId>,
    head: Option<NodeId>,
    body: Option<NodeId>,
    form: Option<NodeId>,
    phase: Phase,
    fragment: bool,
    /// The list of active formatting elements (None = marker), HTML §13.2.4.3.
    afe: Vec<Option<NodeId>>,
    /// The scripting flag: off for DOMParser documents, where `<noscript>`
    /// content is markup rather than raw text.
    scripting: bool,
}

fn is_formatting(name: &str) -> bool {
    matches!(name, "a" | "b" | "big" | "code" | "em" | "font" | "i" | "nobr" | "s" | "small" | "strike" | "strong" | "tt" | "u")
}

impl<'d> TreeBuilder<'d> {
    fn same_element_token(&self, a: NodeId, b: NodeId) -> bool {
        match (self.doc.element(a), self.doc.element(b)) {
            (Some(x), Some(y)) => {
                x.name == y.name
                    && x.ns == y.ns
                    && x.attrs.len() == y.attrs.len()
                    && x.attrs.iter().all(|at| y.attrs.iter().any(|bt| bt.name == at.name && bt.value == at.value))
            }
            _ => false,
        }
    }

    /// Push a formatting element, applying the "Noah's Ark" limit of three.
    fn push_afe(&mut self, el: NodeId) {
        let mut same = Vec::new();
        for (i, e) in self.afe.iter().enumerate().rev() {
            match e {
                None => break,
                Some(n) if self.same_element_token(*n, el) => same.push(i),
                _ => {}
            }
        }
        if same.len() >= 3 {
            self.afe.remove(*same.last().unwrap());
        }
        self.afe.push(Some(el));
    }

    fn push_marker(&mut self) {
        self.afe.push(None);
    }

    fn clear_afe_to_marker(&mut self) {
        while let Some(e) = self.afe.pop() {
            if e.is_none() {
                break;
            }
        }
    }

    fn clone_element(&mut self, n: NodeId) -> NodeId {
        let (name, ns, attrs) = match self.doc.element(n) {
            Some(e) => (e.name.clone(), e.ns, e.attrs.clone()),
            None => ("span".into(), Namespace::Html, Vec::new()),
        };
        self.doc.create_element(&name, ns, attrs)
    }

    /// Reconstruct the active formatting elements (HTML §13.2.4.3).
    fn reconstruct_afe(&mut self) {
        let last = match self.afe.last() {
            Some(Some(n)) => *n,
            _ => return,
        };
        if self.stack.contains(&last) {
            return;
        }
        let mut i = self.afe.len() - 1;
        while i > 0 {
            match self.afe[i - 1] {
                None => break,
                Some(n) if self.stack.contains(&n) => break,
                _ => i -= 1,
            }
        }
        for j in i..self.afe.len() {
            let old = match self.afe[j] {
                Some(n) => n,
                None => continue,
            };
            let el = self.clone_element(old);
            let (parent, before) = self.insertion_point(None);
            self.doc.insert_before(parent, el, before);
            if self.stack.len() < MAX_DEPTH {
                self.stack.push(el);
            }
            self.afe[j] = Some(el);
        }
    }

    /// The adoption agency algorithm (HTML §13.2.6.4.7). Returns false when the
    /// end tag should be handled as "any other end tag".
    fn adoption_agency(&mut self, subject: &str) -> bool {
        let cur = self.current();
        if self.doc.tag(cur) == subject && self.ns_of(cur) == Namespace::Html && !self.afe.contains(&Some(cur)) {
            self.stack.pop();
            self.ensure_nonempty();
            return true;
        }
        for _ in 0..8 {
            // The last formatting element with this tag after the last marker.
            let mut fe_idx = None;
            for (i, e) in self.afe.iter().enumerate().rev() {
                match e {
                    None => break,
                    Some(n) if self.doc.tag(*n) == subject => {
                        fe_idx = Some(i);
                        break;
                    }
                    _ => {}
                }
            }
            let fe_idx = match fe_idx {
                Some(i) => i,
                None => return false,
            };
            let fe = self.afe[fe_idx].unwrap();
            let fe_pos = match self.stack.iter().position(|&n| n == fe) {
                Some(p) => p,
                None => {
                    self.afe.remove(fe_idx);
                    return true;
                }
            };
            if !self.has_in_scope(subject, &[]) {
                return true;
            }
            let fb_pos = (fe_pos + 1..self.stack.len()).find(|&i| is_special(self.doc.tag(self.stack[i])) || self.ns_of(self.stack[i]) != Namespace::Html && false);
            let fb_pos = match fb_pos {
                Some(p) => p,
                None => {
                    self.stack.truncate(fe_pos);
                    self.ensure_nonempty();
                    if let Some(i) = self.afe.iter().position(|e| *e == Some(fe)) {
                        self.afe.remove(i);
                    }
                    return true;
                }
            };
            let furthest = self.stack[fb_pos];
            let common = if fe_pos > 0 { self.stack[fe_pos - 1] } else { DOCUMENT };
            let mut bookmark = fe_idx;
            let mut node_pos = fb_pos;
            let mut last_node = furthest;
            let mut counter = 0;
            loop {
                counter += 1;
                node_pos -= 1;
                let node = self.stack[node_pos];
                if node == fe {
                    break;
                }
                let mut in_afe = self.afe.iter().position(|e| *e == Some(node));
                if counter > 3 {
                    if let Some(i) = in_afe {
                        self.afe.remove(i);
                        if i < bookmark {
                            bookmark -= 1;
                        }
                        in_afe = None;
                    }
                }
                let ai = match in_afe {
                    Some(i) => i,
                    None => {
                        self.stack.remove(node_pos);
                        continue;
                    }
                };
                let new = self.clone_element(node);
                self.afe[ai] = Some(new);
                self.stack[node_pos] = new;
                if last_node == furthest {
                    bookmark = ai + 1;
                }
                self.doc.append_child(new, last_node);
                last_node = new;
            }
            // Insert last_node at the appropriate place for the common ancestor.
            if matches!(self.doc.tag(common), "table" | "tbody" | "tfoot" | "thead" | "tr") {
                if let Some(&table) = self.stack.iter().rev().find(|&&n| self.doc.tag(n) == "table") {
                    if let Some(p) = self.doc.parent(table) {
                        self.doc.insert_before(p, last_node, Some(table));
                    } else {
                        self.doc.append_child(common, last_node);
                    }
                } else {
                    self.doc.append_child(common, last_node);
                }
            } else {
                self.doc.append_child(common, last_node);
            }
            let e = self.clone_element(fe);
            let kids: Vec<NodeId> = self.doc.children(furthest).to_vec();
            for k in kids {
                self.doc.append_child(e, k);
            }
            self.doc.append_child(furthest, e);
            if let Some(i) = self.afe.iter().position(|x| *x == Some(fe)) {
                self.afe.remove(i);
                if i < bookmark {
                    bookmark -= 1;
                }
            }
            let bm = bookmark.min(self.afe.len());
            self.afe.insert(bm, Some(e));
            if let Some(i) = self.stack.iter().position(|&n| n == fe) {
                self.stack.remove(i);
            }
            if let Some(i) = self.stack.iter().position(|&n| n == furthest) {
                self.stack.insert(i + 1, e);
            }
        }
        true
    }

    /// Inside a `<template>` opened in the head (before the body exists):
    /// its contents parse like body content, into the template.
    fn in_head_template(&self) -> bool {
        self.phase != Phase::InBody && self.stack.iter().any(|&n| self.doc.tag(n) == "template" && self.ns_of(n) == Namespace::Html)
    }

    fn current(&self) -> NodeId {
        *self.stack.last().unwrap_or(&DOCUMENT)
    }

    fn current_name(&self) -> &str {
        self.doc.tag(self.current())
    }

    fn ns_of(&self, id: NodeId) -> Namespace {
        self.doc.element(id).map(|e| e.ns).unwrap_or(Namespace::Html)
    }

    fn in_foreign(&self) -> bool {
        let c = self.current();
        match self.doc.element(c) {
            Some(e) => e.ns != Namespace::Html && !(e.ns == Namespace::Svg && e.name.eq_ignore_ascii_case("foreignobject")),
            None => false,
        }
    }

    fn ensure_html(&mut self) {
        if self.html.is_none() && !self.fragment {
            let h = self.doc.create_element("html", Namespace::Html, Vec::new());
            self.doc.append_child(DOCUMENT, h);
            self.html = Some(h);
            self.stack.push(h);
        }
    }

    fn ensure_head(&mut self) {
        self.ensure_html();
        if self.head.is_none() && !self.fragment {
            let h = self.doc.create_element("head", Namespace::Html, Vec::new());
            self.doc.append_child(self.html.unwrap(), h);
            self.head = Some(h);
        }
    }

    fn ensure_body(&mut self, attrs: Vec<Attr>) {
        if self.fragment {
            self.phase = Phase::InBody;
            return;
        }
        self.ensure_head();
        if self.body.is_none() {
            let b = self.doc.create_element("body", Namespace::Html, attrs);
            self.doc.append_child(self.html.unwrap(), b);
            self.body = Some(b);
            // Stack: html, body
            self.stack.truncate(1);
            self.stack.push(b);
        }
        self.phase = Phase::InBody;
    }

    fn has_in_scope(&self, name: &str, extra: &[&str]) -> bool {
        for &n in self.stack.iter().rev() {
            let t = self.doc.tag(n);
            if t == name && self.ns_of(n) == Namespace::Html {
                return true;
            }
            if matches!(t, "applet" | "caption" | "html" | "table" | "td" | "th" | "marquee" | "object" | "template")
                || extra.contains(&t)
                || (self.ns_of(n) == Namespace::Svg && (t.eq_ignore_ascii_case("foreignobject") || matches!(t, "desc" | "title")))
            {
                return false;
            }
        }
        false
    }

    /// "Has an element in table scope": only html/table/template bound it.
    fn has_in_table_scope(&self, name: &str) -> bool {
        for &n in self.stack.iter().rev() {
            if self.ns_of(n) != Namespace::Html {
                continue;
            }
            let t = self.doc.tag(n);
            if t == name {
                return true;
            }
            if matches!(t, "html" | "table" | "template") {
                return false;
            }
        }
        false
    }

    fn pop_until(&mut self, name: &str) {
        while let Some(n) = self.stack.pop() {
            if self.doc.tag(n) == name {
                break;
            }
            if self.stack.is_empty() {
                break;
            }
        }
        self.ensure_nonempty();
    }

    fn ensure_nonempty(&mut self) {
        if self.stack.is_empty() {
            if let Some(h) = self.html {
                self.stack.push(h);
            }
        }
    }

    fn close_p(&mut self) {
        if self.has_in_scope("p", &["button"]) {
            self.pop_until("p");
        }
    }

    fn generate_implied_end_tags(&mut self, except: &str) {
        loop {
            let t = self.current_name().to_string();
            if t != except
                && matches!(t.as_str(), "dd" | "dt" | "li" | "optgroup" | "option" | "p" | "rb" | "rp" | "rt" | "rtc")
            {
                self.stack.pop();
            } else {
                break;
            }
        }
    }

    /// Where to insert a node: normally the current node, but content that
    /// lands directly inside table structure is foster-parented before the table.
    fn insertion_point(&self, for_name: Option<&str>) -> (NodeId, Option<NodeId>) {
        let cur = self.current();
        let cur_name = self.doc.tag(cur);
        if matches!(cur_name, "table" | "tbody" | "thead" | "tfoot" | "tr") && self.ns_of(cur) == Namespace::Html {
            let allowed = match for_name {
                Some(n) => is_table_structure(n) || matches!(n, "script" | "style" | "template" | "form" | "input:hidden"),
                None => false,
            };
            if !allowed {
                // Find the table on the stack and insert before it.
                if let Some(&table) = self.stack.iter().rev().find(|&&n| self.doc.tag(n) == "table") {
                    if let Some(p) = self.doc.parent(table) {
                        return (p, Some(table));
                    }
                }
            }
        }
        (cur, None)
    }

    fn insert_text(&mut self, text: &str) {
        let (parent, before) = if text.trim().is_empty() {
            (self.current(), None)
        } else {
            self.insertion_point(None)
        };
        // Merge with a preceding text node.
        let prev = match before {
            Some(b) => self.doc.prev_sibling(b),
            None => self.doc.children(parent).last().copied(),
        };
        if let Some(p) = prev {
            if let NodeData::Text(t) = &mut self.doc.nodes[p].data {
                t.push_str(text);
                return;
            }
        }
        let t = self.doc.create_text(text);
        self.doc.insert_before(parent, t, before);
    }

    fn insert_element(&mut self, name: &str, ns: Namespace, mut attrs: Vec<Attr>, push: bool) -> NodeId {
        if ns == Namespace::Svg {
            // HTML §13.2.6.5 "adjust SVG attributes": restore camelCase names.
            for a in attrs.iter_mut() {
                if let Some(fixed) = svg_attr_case(&a.name) {
                    a.name = fixed.to_string();
                }
            }
        }
        if ns != Namespace::Html {
            // "adjust foreign attributes": xlink:*, xml:*, xmlns(:xlink).
            for a in attrs.iter_mut() {
                let n = a.name.as_str();
                let uri = if matches!(n, "xlink:actuate" | "xlink:arcrole" | "xlink:href" | "xlink:role" | "xlink:show" | "xlink:title" | "xlink:type") {
                    Some(crate::dom::XLINK_NS)
                } else if matches!(n, "xml:lang" | "xml:space") {
                    Some(crate::dom::XML_NS)
                } else if matches!(n, "xmlns" | "xmlns:xlink") {
                    Some(crate::dom::XMLNS_NS)
                } else {
                    None
                };
                a.ns = uri.map(|u| u.into());
            }
        }
        // "Adjust SVG tag names": foreign elements keep their camelCase names
        // (linearGradient, clipPath, foreignObject...).
        let name = if ns == Namespace::Svg { crate::dom::svg_tag_case(name) } else { name };
        let hidden_input = name == "input" && attrs.iter().any(|a| a.name == "type" && a.value.eq_ignore_ascii_case("hidden"));
        let (parent, before) = self.insertion_point(Some(if hidden_input { "input:hidden" } else { name }));
        let el = self.doc.create_element(name, ns, attrs);
        self.doc.insert_before(parent, el, before);
        if push && self.stack.len() < MAX_DEPTH {
            self.stack.push(el);
        }
        el
    }

    fn insert_into_head(&mut self, name: &str, attrs: Vec<Attr>, tok: &mut Tokenizer) -> NodeId {
        self.ensure_head();
        let head = self.head.unwrap_or(self.current());
        let el = self.doc.create_element(name, Namespace::Html, attrs);
        self.doc.append_child(head, el);
        // Raw-text head elements need their content collected.
        match name {
            "title" => {
                tok.set_rcdata(name);
                self.collect_raw(el, tok, name);
            }
            "style" | "script" | "noscript" | "noframes" => {
                tok.set_rawtext(name);
                self.collect_raw(el, tok, name);
            }
            _ => {}
        }
        el
    }

    /// Read raw text content into `el` and consume its end tag.
    fn collect_raw(&mut self, el: NodeId, tok: &mut Tokenizer, name: &str) {
        loop {
            match tok.next_token() {
                Token::Text(t) => {
                    let n = self.doc.create_text(&t);
                    self.doc.append_child(el, n);
                }
                Token::EndTag { name: n } if n == name => break,
                Token::Eof => break,
                _ => break,
            }
        }
    }

    fn start_tag(&mut self, name: String, attrs: Vec<Attr>, self_closing: bool, tok: &mut Tokenizer) {
        // ---- foreign content ------------------------------------------------
        if self.in_foreign() {
            if breaks_foreign(&name) || (name == "font" && attrs.iter().any(|a| matches!(a.name.as_str(), "color" | "face" | "size"))) {
                while self.in_foreign() && self.stack.len() > 1 {
                    self.stack.pop();
                }
            } else {
                let ns = self.ns_of(self.current());
                let el = self.insert_element(&name, ns, attrs, !self_closing);
                if !self_closing && (name == "style" || name == "script") && ns == Namespace::Svg {
                    tok.set_rawtext(&name);
                    self.collect_raw(el, tok, &name);
                    self.stack.pop();
                }
                tok.cdata_allowed = self.in_foreign();
                return;
            }
        }

        let head_tpl = self.in_head_template();
        match self.phase {
            _ if head_tpl => {}
            Phase::BeforeHtml | Phase::InHead => {
                if name == "template" && !self.fragment {
                    self.ensure_head();
                    let head = self.head.unwrap_or(self.current());
                    let el = self.doc.create_element(&name, Namespace::Html, attrs);
                    self.doc.append_child(head, el);
                    if self.stack.len() < MAX_DEPTH {
                        self.stack.push(el);
                        self.push_marker();
                    }
                    self.phase = Phase::InHead;
                    return;
                }
                if name == "html" {
                    self.ensure_html();
                    if let Some(h) = self.html {
                        for a in attrs {
                            if self.doc.attr(h, &a.name).is_none() {
                                self.doc.set_attr(h, &a.name, &a.value);
                            }
                        }
                    }
                    return;
                }
                if name == "head" {
                    self.ensure_head();
                    if let Some(h) = self.head {
                        for a in attrs {
                            self.doc.set_attr(h, &a.name, &a.value);
                        }
                    }
                    self.phase = Phase::InHead;
                    return;
                }
                if is_head_content(&name) && !self.fragment {
                    self.insert_into_head(&name, attrs, tok);
                    self.phase = Phase::InHead;
                    return;
                }
                if name == "body" {
                    self.ensure_body(attrs);
                    return;
                }
                if name == "frameset" {
                    self.ensure_head();
                    let f = self.doc.create_element("frameset", Namespace::Html, attrs);
                    self.doc.append_child(self.html.unwrap(), f);
                    self.body = Some(f);
                    self.stack.truncate(1);
                    self.stack.push(f);
                    self.phase = Phase::InBody;
                    return;
                }
                self.ensure_body(Vec::new());
            }
            Phase::InBody => {}
        }

        // ---- in body ----------------------------------------------------------
        match name.as_str() {
            "html" => {
                if let Some(h) = self.html {
                    for a in attrs {
                        if self.doc.attr(h, &a.name).is_none() {
                            self.doc.set_attr(h, &a.name, &a.value);
                        }
                    }
                }
                return;
            }
            "body" => {
                if let Some(b) = self.body {
                    for a in attrs {
                        if self.doc.attr(b, &a.name).is_none() {
                            self.doc.set_attr(b, &a.name, &a.value);
                        }
                    }
                }
                return;
            }
            "head" => return,
            "frameset" | "frame" => {}
            _ => {}
        }

        // Implicit end tags.
        if closes_p(&name) {
            self.close_p();
        }
        if is_heading(&name) && is_heading(self.current_name()) {
            self.stack.pop();
        }
        match name.as_str() {
            "li" => {
                for i in (0..self.stack.len()).rev() {
                    let t = self.doc.tag(self.stack[i]).to_string();
                    if t == "li" {
                        self.generate_implied_end_tags("li");
                        self.pop_until("li");
                        break;
                    }
                    if is_special(&t) && !matches!(t.as_str(), "address" | "div" | "p") {
                        break;
                    }
                }
            }
            "dd" | "dt" => {
                for i in (0..self.stack.len()).rev() {
                    let t = self.doc.tag(self.stack[i]).to_string();
                    if t == "dd" || t == "dt" {
                        self.generate_implied_end_tags(&t);
                        self.pop_until(&t);
                        break;
                    }
                    if is_special(&t) && !matches!(t.as_str(), "address" | "div" | "p") {
                        break;
                    }
                }
            }
            "a" => {
                // An open <a> in the formatting list is closed by the adoption agency.
                let open_a = self.afe.iter().rev().take_while(|e| e.is_some()).flatten().copied().find(|&n| self.doc.tag(n) == "a");
                if let Some(a) = open_a {
                    self.adoption_agency("a");
                    if let Some(i) = self.afe.iter().position(|e| *e == Some(a)) {
                        self.afe.remove(i);
                    }
                    if let Some(i) = self.stack.iter().position(|&n| n == a) {
                        self.stack.remove(i);
                        self.ensure_nonempty();
                    }
                }
            }
            "option" => {
                if self.current_name() == "option" {
                    self.stack.pop();
                }
            }
            "optgroup" => {
                if self.current_name() == "option" {
                    self.stack.pop();
                }
                if self.current_name() == "optgroup" {
                    self.stack.pop();
                }
            }
            "button" => {
                if self.has_in_scope("button", &[]) {
                    self.pop_until("button");
                }
            }
            "nobr" => {
                self.reconstruct_afe();
                if self.has_in_scope("nobr", &[]) {
                    self.adoption_agency("nobr");
                }
            }
            "form" => {
                if self.form.is_some() && !self.stack.iter().any(|&n| self.doc.tag(n) == "template") {
                    return; // nested forms are ignored
                }
            }
            "select" => {
                if self.current_name() == "select" {
                    self.stack.pop();
                }
            }
            "input" | "keygen" | "textarea" if self.has_in_scope("select", &[]) => {
                self.pop_until("select");
            }
            "caption" | "colgroup" | "tbody" | "thead" | "tfoot" => {
                if self.has_in_table_scope("table") {
                    while !matches!(self.current_name(), "table" | "html" | "template") {
                        self.stack.pop();
                    }
                }
            }
            "col" => {
                if self.has_in_table_scope("table") {
                    while !matches!(self.current_name(), "table" | "colgroup" | "html" | "template") {
                        self.stack.pop();
                    }
                    // A <col> directly in a table gets an implied <colgroup>.
                    if self.current_name() == "table" {
                        self.insert_element("colgroup", Namespace::Html, Vec::new(), true);
                    }
                }
            }
            "tr" => {
                if self.has_in_table_scope("table") {
                    while !matches!(self.current_name(), "table" | "tbody" | "thead" | "tfoot" | "html" | "template") {
                        self.stack.pop();
                    }
                    if self.current_name() == "table" {
                        self.insert_element("tbody", Namespace::Html, Vec::new(), true);
                    }
                }
            }
            "td" | "th" => {
                if self.has_in_table_scope("table") {
                    while !matches!(self.current_name(), "table" | "tbody" | "thead" | "tfoot" | "tr" | "html" | "template") {
                        if matches!(self.current_name(), "td" | "th" | "caption") {
                            self.clear_afe_to_marker();
                        }
                        self.stack.pop();
                    }
                    if self.current_name() == "table" {
                        self.insert_element("tbody", Namespace::Html, Vec::new(), true);
                    }
                    if self.current_name() != "tr" {
                        self.insert_element("tr", Namespace::Html, Vec::new(), true);
                    }
                }
            }
            "table" => {}
            _ => {}
        }

        // Insert.
        match name.as_str() {
            "svg" | "math" => {
                self.reconstruct_afe();
                let ns = if name == "svg" { Namespace::Svg } else { Namespace::MathMl };
                self.insert_element(&name, ns, attrs, !self_closing);
                tok.cdata_allowed = !self_closing;
            }
            "image" => {
                self.insert_element("img", Namespace::Html, attrs, false);
            }
            "br" | "area" | "embed" | "img" | "input" | "keygen" | "wbr" => {
                self.reconstruct_afe();
                self.insert_element(&name, Namespace::Html, attrs, false);
            }
            "base" | "basefont" | "bgsound" | "col" | "hr" | "link" | "meta" | "param" | "source" | "track" | "frame" => {
                self.insert_element(&name, Namespace::Html, attrs, false);
            }
            n if is_formatting(n) => {
                self.reconstruct_afe();
                let el = self.insert_element(&name, Namespace::Html, attrs, true);
                self.push_afe(el);
            }
            "applet" | "marquee" | "object" => {
                self.reconstruct_afe();
                self.insert_element(&name, Namespace::Html, attrs, true);
                self.push_marker();
            }
            "td" | "th" | "caption" | "template" => {
                self.insert_element(&name, Namespace::Html, attrs, true);
                self.push_marker();
            }
            "noscript" if !self.scripting => {
                self.reconstruct_afe();
                self.insert_element(&name, Namespace::Html, attrs, true);
            }
            "script" | "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => {
                let el = self.insert_element(&name, Namespace::Html, attrs, false);
                tok.set_rawtext(&name);
                self.collect_raw(el, tok, &name);
            }
            "textarea" | "title" => {
                let el = self.insert_element(&name, Namespace::Html, attrs, false);
                tok.set_rcdata(&name);
                self.collect_raw(el, tok, &name);
                // A leading newline in textarea is dropped.
                if name == "textarea" {
                    if let Some(&first) = self.doc.children(el).first() {
                        if let NodeData::Text(t) = &mut self.doc.nodes[first].data {
                            if t.starts_with('\n') {
                                t.remove(0);
                            }
                        }
                    }
                }
            }
            "plaintext" => {
                self.insert_element(&name, Namespace::Html, attrs, true);
                tok.set_plaintext();
            }
            "form" => {
                // In table context the form is inserted and closed at once.
                let in_table = matches!(self.current_name(), "table" | "tbody" | "thead" | "tfoot" | "tr");
                let f = self.insert_element(&name, Namespace::Html, attrs, true);
                self.form = Some(f);
                if in_table && self.current() == f {
                    self.stack.pop();
                }
            }
            _ => {
                if !is_special(&name) || matches!(name.as_str(), "button" | "select" | "textarea") {
                    self.reconstruct_afe();
                }
                self.insert_element(&name, Namespace::Html, attrs, true);
            }
        }
    }

    fn end_tag(&mut self, name: String) {
        if self.in_foreign() {
            // Pop to the matching foreign element if present.
            for i in (0..self.stack.len()).rev() {
                let n = self.stack[i];
                if self.doc.tag(n).eq_ignore_ascii_case(&name) {
                    self.stack.truncate(i);
                    self.ensure_nonempty();
                    break;
                }
                if self.ns_of(n) == Namespace::Html {
                    break;
                }
            }
            if !self.in_foreign() {
                return;
            }
            return;
        }
        match name.as_str() {
            "head" => {
                if self.phase == Phase::InHead || self.phase == Phase::BeforeHtml {
                    self.phase = Phase::InHead;
                }
                return;
            }
            "body" | "html" => return,
            "br" => {
                if self.phase != Phase::InBody {
                    self.ensure_body(Vec::new());
                }
                self.insert_element("br", Namespace::Html, Vec::new(), false);
                return;
            }
            "p" => {
                if self.phase != Phase::InBody {
                    self.ensure_body(Vec::new());
                }
                if !self.has_in_scope("p", &["button"]) {
                    self.insert_element("p", Namespace::Html, Vec::new(), false);
                    return;
                }
                self.generate_implied_end_tags("p");
                self.pop_until("p");
                return;
            }
            "form" => {
                self.form = None;
                if self.has_in_scope("form", &[]) {
                    self.pop_until("form");
                }
                return;
            }
            // Table structure end tags use table scope, so `</table>` inside an
            // open cell closes the cell, row, section and table.
            "table" | "tbody" | "thead" | "tfoot" | "tr" | "td" | "th" | "caption" | "colgroup" => {
                if self.has_in_table_scope(&name) {
                    self.generate_implied_end_tags("");
                    // Closing cells (directly or via the table) drops their formatting entries.
                    let cells = self.stack.iter().rev().take_while(|&&n| self.doc.tag(n) != name).filter(|&&n| matches!(self.doc.tag(n), "td" | "th" | "caption")).count()
                        + usize::from(matches!(name.as_str(), "td" | "th" | "caption"));
                    for _ in 0..cells {
                        self.clear_afe_to_marker();
                    }
                    self.pop_until(&name);
                } else if matches!(name.as_str(), "tbody" | "thead" | "tfoot" | "tr") {
                    // Ignored.
                }
                return;
            }
            _ => {}
        }
        if self.phase != Phase::InBody && !self.in_head_template() {
            return;
        }
        if is_formatting(&name) {
            if self.adoption_agency(&name) {
                return;
            }
        }
        if matches!(name.as_str(), "applet" | "marquee" | "object" | "template") {
            if self.has_in_scope(&name, &[]) {
                self.generate_implied_end_tags("");
                self.pop_until(&name);
                self.clear_afe_to_marker();
            }
            return;
        }
        if is_special(&name) {
            let extra: &[&str] = if name == "li" { &["ol", "ul"] } else { &[] };
            if is_heading(&name) {
                // Any open heading closes.
                if let Some(pos) = self.stack.iter().rposition(|&n| is_heading(self.doc.tag(n))) {
                    if self.stack[pos..].iter().all(|&n| !matches!(self.doc.tag(n), "table" | "td" | "th")) {
                        self.stack.truncate(pos);
                        self.ensure_nonempty();
                    }
                }
                return;
            }
            if self.has_in_scope(&name, extra) {
                self.generate_implied_end_tags(&name);
                self.pop_until(&name);
            }
            return;
        }
        // Any other end tag.
        for i in (0..self.stack.len()).rev() {
            let n = self.stack[i];
            let t = self.doc.tag(n);
            // Foreign element names keep their case (foreignObject); end tags
            // arrive lowercased.
            if t == name || (self.ns_of(n) != Namespace::Html && t.eq_ignore_ascii_case(&name)) {
                self.stack.truncate(i);
                self.ensure_nonempty();
                return;
            }
            if is_special(t) {
                return;
            }
        }
    }

    fn run(&mut self, tok: &mut Tokenizer) {
        loop {
            match tok.next_token() {
                Token::Eof => break,
                Token::Doctype(full) => {
                    if self.html.is_none() && !self.fragment {
                        let mut parts = full.split('\0');
                        let name = parts.next().unwrap_or("");
                        let pubid = parts.next().unwrap_or("");
                        let sysid = parts.next().unwrap_or("");
                        self.doc.limited_quirks = name.ends_with(" limited");
                        self.doc.quirks = name.is_empty() || (name != "html" && !self.doc.limited_quirks);
                        let n = name.split(' ').next().unwrap_or("html");
                        let d = if pubid.is_empty() && sysid.is_empty() { self.doc.create_doctype(n) } else { self.doc.create_doctype(&format!("{n}\0{pubid}\0{sysid}")) };
                        self.doc.append_child(DOCUMENT, d);
                    }
                }
                Token::Comment(c) => {
                    let n = self.doc.create_comment(&c);
                    let parent = if self.stack.is_empty() { DOCUMENT } else { self.current() };
                    self.doc.append_child(parent, n);
                }
                Token::Text(t) => {
                    if self.phase != Phase::InBody && !self.in_head_template() {
                        let trimmed = t.trim_start_matches(|c: char| c.is_ascii_whitespace());
                        if trimmed.is_empty() {
                            continue;
                        }
                        self.ensure_body(Vec::new());
                        self.insert_text(trimmed);
                    } else {
                        if !self.in_foreign() {
                            self.reconstruct_afe();
                        }
                        // Drop NULs.
                        if t.contains('\0') {
                            self.insert_text(&t.replace('\0', ""));
                        } else {
                            self.insert_text(&t);
                        }
                    }
                }
                Token::StartTag { name, attrs, self_closing } => {
                    if !self.fragment {
                        self.ensure_html();
                    }
                    self.start_tag(name, attrs, self_closing, tok);
                }
                Token::EndTag { name } => self.end_tag(name),
            }
        }
        if !self.fragment {
            self.ensure_body(Vec::new());
        }
    }
}

/// Parse a complete HTML document.
pub fn parse_document(src: &str, url: Url) -> Document {
    parse_document_with(src, url, true)
}

/// Parse a complete HTML document with the scripting flag on or off.
pub fn parse_document_with(src: &str, url: Url, scripting: bool) -> Document {
    // Input stream preprocessing: CR and CRLF become LF.
    let norm;
    let src = if src.contains('\r') {
        norm = src.replace("\r\n", "\n").replace('\r', "\n");
        norm.as_str()
    } else {
        src
    };
    let mut doc = Document::new(url);
    // No DOCTYPE → quirks mode (the DOCTYPE token, if any, resets this).
    doc.quirks = !scripting || !src.trim().is_empty();
    {
        let mut tb = TreeBuilder {
            doc: &mut doc,
            stack: Vec::new(),
            html: None,
            head: None,
            body: None,
            form: None,
            phase: Phase::BeforeHtml,
            fragment: false,
            afe: Vec::new(),
            scripting,
        };
        let mut tok = Tokenizer::new(src);
        tb.run(&mut tok);
    }
    doc.hoist_template_contents(DOCUMENT);
    doc.attach_declarative_shadows(DOCUMENT);
    // <base href> support.
    if let Some(head) = doc.head() {
        let base = doc
            .element_children(head)
            .find(|&c| doc.is_tag(c, "base") && doc.attr(c, "href").is_some())
            .and_then(|b| doc.attr(b, "href").map(|s| s.to_string()));
        if let Some(b) = base {
            if let Ok(u) = doc.url.join(&b) {
                doc.base_url = u;
            }
        }
    }
    doc
}

/// Parse an HTML fragment (innerHTML) into a new fragment node of `doc`.
pub fn parse_fragment(doc: &mut Document, context: NodeId, src: &str) -> NodeId {
    // Input stream preprocessing: CR and CRLF become LF.
    let norm;
    let src = if src.contains('\r') {
        norm = src.replace("\r\n", "\n").replace('\r', "\n");
        norm.as_str()
    } else {
        src
    };
    let frag = doc.create_fragment();
    let ctx_name = doc.tag(context).to_string();
    let ctx_ns = doc.element(context).map(|e| e.ns).unwrap_or(Namespace::Html);
    if ctx_name == "html" && ctx_ns == Namespace::Html {
        // An <html> context starts "before head": parse as a document and
        // take the root's children (head and body).
        let tmp = parse_document(src, doc.url.clone());
        if let Some(root) = tmp.element_children(DOCUMENT).next() {
            let kids: Vec<NodeId> = tmp.children(root).to_vec();
            for k in kids {
                let n = doc.import_from(&tmp, k, doc.owner_doc(context));
                doc.append_child(frag, n);
            }
        }
        return frag;
    }
    {
        // Use a synthetic container mirroring the context element so table
        // and foreign-content rules behave correctly.
        // Below it sits a stand-in for the spec's root html element: content
        // that closes the context (a <p> in a p context, say) continues
        // there, after the holder, which is what the fragment should hold.
        let marker = doc.create_element("html", Namespace::Html, Vec::new());
        doc.append_child(frag, marker);
        let holder = doc.create_element(if ctx_name.is_empty() { "div" } else { &ctx_name }, ctx_ns, Vec::new());
        doc.append_child(marker, holder);
        let mut tb = TreeBuilder {
            doc,
            stack: vec![marker, holder],
            html: Some(marker),
            head: None,
            body: Some(holder),
            form: None,
            phase: Phase::InBody,
            fragment: true,
            afe: Vec::new(),
            scripting: true,
        };
        let mut tok = Tokenizer::new(src);
        if matches!(ctx_name.as_str(), "script" | "style" | "textarea" | "title" | "xmp") {
            let t = tb.doc.create_text(src);
            tb.doc.append_child(holder, t);
        } else {
            tok.cdata_allowed = ctx_ns != Namespace::Html;
            tb.run(&mut tok);
        }
        let top: Vec<NodeId> = doc.children(marker).to_vec();
        for k in top {
            if k == holder {
                let kids: Vec<NodeId> = doc.children(holder).to_vec();
                for c in kids {
                    doc.append_child(frag, c);
                }
            } else {
                doc.append_child(frag, k);
            }
        }
        doc.detach(holder);
        doc.detach(marker);
    }
    doc.hoist_template_contents(frag);
    frag
}

#[cfg(test)]
mod tests {
    use super::*;
    fn body(src: &str) -> String {
        let d = parse_document(src, Url::parse("http://x/").unwrap());
        d.inner_html(d.body().unwrap())
    }
    #[test]
    fn table_end_in_cell() {
        assert_eq!(body("<p><table><tr><td></table><p class=bad>x"), "<p></p><table><tbody><tr><td></td></tr></tbody></table><p class=\"bad\">x</p>");
    }
    #[test]
    fn cells_close() {
        assert_eq!(body("<table><tr><td>a<td>b<tr><td>c</table>"), "<table><tbody><tr><td>a</td><td>b</td></tr><tr><td>c</td></tr></tbody></table>");
    }
    #[test]
    fn adoption_agency_paragraph() {
        assert_eq!(body("<b>1<p>2</b>3</p>"), "<b>1</b><p><b>2</b>3</p>");
    }
    #[test]
    fn adoption_agency_misnested() {
        assert_eq!(body("<b>b<i>bi</b>x</i>"), "<b>b<i>bi</i></b><i>x</i>");
        assert_eq!(body("<a><p></a></p>"), "<a></a><p><a></a></p>");
    }
    #[test]
    fn reconstruct_after_paragraph() {
        assert_eq!(body("<p><b><i>x</p>y"), "<p><b><i>x</i></b></p><b><i>y</i></b>");
    }
    #[test]
    fn nested_anchors_close() {
        assert_eq!(body("<a href=1>a<a href=2>b"), "<a href=\"1\">a</a><a href=\"2\">b</a>");
    }
    #[test]
    fn cell_formatting_does_not_leak() {
        assert_eq!(body("<table><tr><td><b>x</td><td>y</td></tr></table>z"), "<table><tbody><tr><td><b>x</b></td><td>y</td></tr></tbody></table>z");
    }
    #[test]
    fn nested_tables() {
        assert_eq!(body("<table><tr><td><table><tr><td>x</table>y</table>"), "<table><tbody><tr><td><table><tbody><tr><td>x</td></tr></tbody></table>y</td></tr></tbody></table>");
    }
}
