// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Generated content: CSS counters (css-lists-3 §4), quotes, `content`
//! evaluation and list markers.

use super::*;
use crate::css::counters;

#[derive(Clone)]
struct Inst {
    name: Rc<str>,
    value: i64,
    reversed: bool,
}

/// Counter scopes, one frame per open element (counters created by an element
/// live in its parent's frame, so they reach the element's following siblings).
#[derive(Default)]
pub struct CounterState {
    frames: Vec<Vec<Inst>>,
    pub quote_depth: i32,
}

impl CounterState {
    pub fn clear(&mut self) {
        self.frames.clear();
        self.frames.push(Vec::new());
        self.quote_depth = 0;
    }
    pub fn push(&mut self) {
        self.frames.push(Vec::new());
    }
    pub fn pop(&mut self) {
        if self.frames.len() > 1 {
            self.frames.pop();
        }
    }
    fn reset(&mut self, name: &str, value: i64, reversed: bool) {
        if self.frames.is_empty() {
            self.frames.push(Vec::new());
        }
        let top = self.frames.last_mut().unwrap();
        if let Some(i) = top.iter_mut().find(|i| &*i.name == name) {
            i.value = value;
            i.reversed = reversed;
        } else {
            top.push(Inst { name: Rc::from(name), value, reversed });
        }
    }
    fn find(&mut self, name: &str) -> Option<&mut Inst> {
        self.frames.iter_mut().rev().find_map(|f| f.iter_mut().find(|i| &*i.name == name))
    }
    fn increment(&mut self, name: &str, by: i64) {
        if self.find(name).is_none() {
            self.reset(name, 0, false);
        }
        if let Some(i) = self.find(name) {
            i.value = i.value.saturating_add(by);
        }
    }
    fn set(&mut self, name: &str, v: i64) {
        match self.find(name) {
            Some(i) => i.value = v,
            None => self.reset(name, v, false),
        }
    }
    pub fn value(&self, name: &str) -> i64 {
        self.frames.iter().rev().find_map(|f| f.iter().find(|i| &*i.name == name)).map(|i| i.value).unwrap_or(0)
    }
    pub fn values(&self, name: &str) -> Vec<i64> {
        let v: Vec<i64> = self.frames.iter().filter_map(|f| f.iter().find(|i| &*i.name == name)).map(|i| i.value).collect();
        if v.is_empty() {
            vec![0]
        } else {
            v
        }
    }
    fn reversed(&self, name: &str) -> bool {
        self.frames.iter().rev().find_map(|f| f.iter().find(|i| &*i.name == name)).map(|i| i.reversed).unwrap_or(false)
    }
}

/// Counter-style name for a `list-style-type` value.
pub fn style_name(t: &ListStyleType) -> Option<&str> {
    Some(match t {
        ListStyleType::None => return None,
        ListStyleType::Disc => "disc",
        ListStyleType::Circle => "circle",
        ListStyleType::Square => "square",
        ListStyleType::Decimal => "decimal",
        ListStyleType::DecimalLeadingZero => "decimal-leading-zero",
        ListStyleType::LowerAlpha => "lower-alpha",
        ListStyleType::UpperAlpha => "upper-alpha",
        ListStyleType::LowerRoman => "lower-roman",
        ListStyleType::UpperRoman => "upper-roman",
        ListStyleType::LowerGreek => "lower-greek",
        ListStyleType::DisclosureClosed => "disclosure-closed",
        ListStyleType::DisclosureOpen => "disclosure-open",
        ListStyleType::Str(_) => return None,
        ListStyleType::Named(n) => n,
    })
}

/// A list item's marker, decided when the box tree is built (counters are known then).
pub struct MarkerSpec {
    pub text: String,
    pub bullet: Option<char>,
    pub image: Option<Arc<ImageData>>,
    pub style: Rc<ComputedStyle>,
    /// `::marker { content: ... }` or a string `list-style-type`: placed flush
    /// against the content (no symbol spacing).
    pub plain: bool,
}

impl Layout<'_> {
    /// Apply an element's counter-reset, counter-increment and counter-set
    /// (in that order), including the implicit `list-item` counter.
    pub(super) fn counters_enter(&mut self, node: Option<NodeId>, s: &ComputedStyle) {
        let html = node.and_then(|n| self.doc.element(n)).map(|e| e.ns == Namespace::Html).unwrap_or(false);
        let tag = node.map(|n| self.doc.tag(n).to_string()).unwrap_or_default();
        let mut reset_list_item = false;
        if let Some(r) = &s.counter_reset {
            for (name, v) in r.iter() {
                if let Some(inner) = name.strip_prefix("reversed(").and_then(|x| x.strip_suffix(')')) {
                    let start = if *v == i64::MIN { node.map(|n| self.count_list_items(n) as i64 + 1).unwrap_or(1) } else { *v };
                    self.counters.reset(inner, start, true);
                    reset_list_item |= inner == "list-item";
                } else {
                    self.counters.reset(name, *v, false);
                    reset_list_item |= &**name == "list-item";
                }
            }
        }
        // HTML lists reset `list-item` (honoring start and reversed).
        if html && !reset_list_item && matches!(tag.as_str(), "ol" | "ul" | "menu") {
            let n = node.unwrap();
            let reversed = tag == "ol" && self.doc.attr(n, "reversed").is_some();
            let start: Option<i64> = if tag == "ol" { self.doc.attr(n, "start").and_then(|v| v.trim().parse().ok()) } else { None };
            if reversed {
                let st = start.unwrap_or_else(|| self.count_list_items(n) as i64);
                self.counters.reset("list-item", st + 1, true);
            } else {
                self.counters.reset("list-item", start.unwrap_or(1) - 1, false);
            }
        }
        let mut inc_list_item = false;
        if let Some(r) = &s.counter_increment {
            for (name, v) in r.iter() {
                self.counters.increment(name, *v);
                inc_list_item |= &**name == "list-item";
            }
        }
        if s.display == Display::ListItem && !inc_list_item {
            let by = if self.counters.reversed("list-item") { -1 } else { 1 };
            self.counters.increment("list-item", by);
        }
        if let Some(r) = &s.counter_set {
            for (name, v) in r.iter() {
                self.counters.set(name, *v);
            }
        }
        if html && tag == "li" {
            if let Some(v) = node.and_then(|n| self.doc.attr(n, "value")).and_then(|v| v.trim().parse::<i64>().ok()) {
                self.counters.set("list-item", v);
            }
        }
        self.counters.push();
    }

    pub(super) fn counters_leave(&mut self) {
        self.counters.pop();
    }

    /// Number of list items that an `ol reversed` numbers (its own items, not nested lists').
    fn count_list_items(&self, list: NodeId) -> usize {
        let mut n = 0;
        let mut stack: Vec<NodeId> = self.doc.element_children(list).collect();
        stack.reverse();
        while let Some(c) = stack.pop() {
            let Some(st) = self.styles.get(c) else { continue };
            if st.display == Display::None {
                continue;
            }
            if st.display == Display::ListItem {
                n += 1;
            }
            let tag = self.doc.tag(c);
            if !matches!(tag, "ol" | "ul" | "menu") && st.display != Display::ListItem {
                let mut kids: Vec<NodeId> = self.doc.element_children(c).collect();
                kids.reverse();
                stack.extend(kids);
            }
        }
        n
    }

    fn quote_pairs(s: &ComputedStyle) -> Vec<(String, String)> {
        match &s.quotes {
            Some(q) => q.as_ref().clone(),
            None => vec![("\u{201c}".into(), "\u{201d}".into()), ("\u{2018}".into(), "\u{2019}".into())],
        }
    }

    /// Text of a `content` value (strings, attributes, counters and quotes).
    pub(super) fn content_text(&mut self, items: &[ContentItem], node: Option<NodeId>, s: &ComputedStyle) -> String {
        let mut text = String::new();
        for item in items {
            match item {
                ContentItem::Str(t) => text.push_str(t),
                ContentItem::Attr(a) => text.push_str(node.and_then(|n| self.doc.attr(n, a)).unwrap_or("")),
                ContentItem::OpenQuote => {
                    let q = Self::quote_pairs(s);
                    if !q.is_empty() {
                        let d = (self.counters.quote_depth.max(0) as usize).min(q.len() - 1);
                        text.push_str(&q[d].0);
                    }
                    self.counters.quote_depth += 1;
                }
                ContentItem::CloseQuote => {
                    if self.counters.quote_depth > 0 {
                        self.counters.quote_depth -= 1;
                        let q = Self::quote_pairs(s);
                        if !q.is_empty() {
                            let d = (self.counters.quote_depth as usize).min(q.len() - 1);
                            text.push_str(&q[d].1);
                        }
                    }
                }
                ContentItem::NoOpenQuote => self.counters.quote_depth += 1,
                ContentItem::NoCloseQuote => self.counters.quote_depth = (self.counters.quote_depth - 1).max(0),
                ContentItem::Counter(name, st) => {
                    let v = self.counters.value(name);
                    text.push_str(&self.counter_repr(st, v));
                }
                ContentItem::Counters(name, sep, st) => {
                    let vals = self.counters.values(name);
                    let parts: Vec<String> = vals.iter().map(|&v| self.counter_repr(st, v)).collect();
                    text.push_str(&parts.join(sep));
                }
                ContentItem::Url(_) => {}
            }
        }
        text
    }

    fn counter_repr(&self, st: &ListStyleType, v: i64) -> String {
        match st {
            ListStyleType::None => String::new(),
            ListStyleType::Str(s) => s.to_string(),
            other => {
                let name = style_name(other).unwrap_or("decimal");
                let cs = counters::lookup(name, &self.styles.counter_styles);
                counters::represent(&cs, v, &self.styles.counter_styles)
            }
        }
    }

    /// Decide a list item's marker (after its counters were applied).
    pub(super) fn make_marker(&mut self, node: NodeId, s: &Rc<ComputedStyle>) -> Option<MarkerSpec> {
        let ms = self.styles.marker.get(&node).cloned();
        let mstyle = ms.clone().unwrap_or_else(|| s.clone());
        if let Some(items) = ms.as_ref().and_then(|m| m.content.clone()) {
            let text = self.content_text(&items, Some(node), &mstyle);
            let image = items.iter().find_map(|i| if let ContentItem::Url(u) = i { self.res.image_url(u) } else { None });
            if text.is_empty() && image.is_none() {
                return None;
            }
            return Some(MarkerSpec { text, bullet: None, image, style: mstyle, plain: true });
        }
        if let Some(url) = &s.list_style_image {
            if let Some(img) = self.res.image_url(url) {
                return Some(MarkerSpec { text: String::new(), bullet: None, image: Some(img), style: mstyle, plain: false });
            }
        }
        let v = self.counters.value("list-item");
        let (text, bullet, plain) = match &s.list_style_type {
            ListStyleType::None => return None,
            ListStyleType::Disc => ("•".to_string(), Some('•'), false),
            ListStyleType::Circle => ("◦".to_string(), Some('◦'), false),
            ListStyleType::Square => ("▪".to_string(), Some('▪'), false),
            ListStyleType::Str(t) => (t.to_string(), None, true),
            other => {
                let name = style_name(other).unwrap_or("decimal");
                (counters::marker_string(name, v, &self.styles.counter_styles), None, false)
            }
        };
        Some(MarkerSpec { text, bullet, image: None, style: mstyle, plain })
    }
}
