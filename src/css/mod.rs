// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronStyle — CSS parsing, media queries and the cascade.

pub mod anim;
pub mod calc;
pub mod color;
pub mod counters;
pub mod mat4;
pub mod pv;
pub mod selector;
pub mod style;
pub mod tokenizer;
pub mod values;

use std::collections::HashMap;

/// A HashMap with the fast (non-DoS-resistant) Fx hash, for internal keys.
pub type FxMap<K, V> = HashMap<K, V, rustc_hash::FxBuildHasher>;
use std::rc::Rc;

use crate::dom::{Document, NodeData, NodeId};
use selector::{MatchState, PseudoElement, Selector};
use style::{ApplyCtx, ComputedStyle, Display, Float, FontStyle};
use tokenizer::{split_commas, trim, words, Tok, CV};
use values::Color;

#[derive(Clone, Debug)]
pub struct Declaration {
    pub name: Rc<str>,
    pub value: Rc<Vec<CV>>,
    pub important: bool,
    pub has_var: bool,
}

#[derive(Clone, Debug)]
pub struct StyleRule {
    pub selectors: Vec<Selector>,
    pub decls: Rc<Vec<Declaration>>,
    /// All media/container conditions that must hold (from enclosing at-rules).
    pub media: Vec<Rc<Vec<CV>>>,
    /// Cascade layer (dotted full name), None for unlayered rules.
    pub layer: Option<Rc<str>>,
    /// Enclosing `@scope` rules, outermost first.
    pub scope: Option<Rc<Scope>>,
    /// Enclosing `@container` queries (all must hold).
    pub containers: Vec<Rc<ContainerQuery>>,
}

/// An `@container [name] <condition>` query.
#[derive(Clone, Debug)]
pub struct ContainerQuery {
    pub name: Option<String>,
    pub cond: Vec<CV>,
}

/// An `@scope (<root>) to (<limit>)` rule.
#[derive(Clone, Debug)]
pub struct Scope {
    pub root: Vec<Selector>,
    pub limit: Vec<Selector>,
}

#[derive(Clone, Debug)]
pub struct FontFace {
    pub ranges: Vec<(u32, u32)>,
    pub family: String,
    pub sources: Vec<(String, String)>, // (url, format)
    pub weight: (u16, u16),
    pub style: FontStyle,
    pub base: url::Url,
    /// The raw font-style / font-stretch descriptors (oblique angles and
    /// widths aren't matched on, but faces differing in them are distinct).
    pub variant: String,
}

#[derive(Clone, Debug)]
pub struct Import {
    pub url: String,
    pub media: Option<Rc<Vec<CV>>>,
    /// `supports()` condition holds (or there is none).
    pub supports_ok: bool,
    /// `layer` / `layer(name)`: the imported rules go in that cascade layer
    /// (Some("") for an anonymous one).
    pub layer: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    UserAgent,
    /// User style sheets (extensions' content CSS): above the UA, below
    /// authors; their `!important` beats every author declaration.
    User,
    Author,
}

#[derive(Clone, Debug)]
pub struct Stylesheet {
    pub rules: Vec<StyleRule>,
    pub font_faces: Vec<FontFace>,
    pub imports: Vec<Import>,
    pub origin: Origin,
    pub base: url::Url,
    /// Layer names in order of first appearance (parents before children).
    pub layer_order: Vec<Rc<str>>,
    /// `@property` registrations: name -> (inherits, initial value).
    /// `@property` registrations: (name, inherits, initial value, syntax).
    pub properties: Vec<(String, bool, Option<Rc<Vec<CV>>>, Option<Rc<str>>)>,
    /// `@counter-style` rules.
    pub counter_styles: Vec<counters::CounterStyleRule>,
    /// `@keyframes` rules with the media they are nested in.
    pub keyframes: Vec<(String, Rc<anim::Keyframes>, Vec<Rc<Vec<CV>>>)>,
    /// Parsing state: the layer and scope rules are being added to.
    cur_layer: Option<Rc<str>>,
    cur_scope: Option<Rc<Scope>>,
    cur_containers: Vec<Rc<ContainerQuery>>,
    anon_layers: u32,
    /// How many of `layer_order` were declared before the last `@import`
    /// (they order ahead of imported layers).
    pub layers_before_imports: usize,
}

impl Stylesheet {
    /// A rule-less sheet declaring the layers this one declared ahead of its
    /// imports (placed before the imported sheets in the cascade).
    pub fn leading_layers(&self) -> Option<Stylesheet> {
        if self.layers_before_imports == 0 || self.imports.is_empty() {
            return None;
        }
        Some(Stylesheet {
            rules: Vec::new(),
            font_faces: Vec::new(),
            imports: Vec::new(),
            origin: self.origin,
            base: self.base.clone(),
            layer_order: self.layer_order[..self.layers_before_imports.min(self.layer_order.len())].to_vec(),
            properties: Vec::new(),
            counter_styles: Vec::new(),
            keyframes: Vec::new(),
            cur_layer: None,
            cur_scope: None,
            cur_containers: Vec::new(),
            anon_layers: 0,
            layers_before_imports: 0,
        })
    }

    /// Declare a layer (relative to the current one) and return its full name.
    fn declare_layer(&mut self, name: &str) -> Rc<str> {
        let full: Rc<str> = match &self.cur_layer {
            Some(c) => Rc::from(format!("{c}.{name}").as_str()),
            None => Rc::from(name),
        };
        let mut acc = String::new();
        for part in full.split('.') {
            if !acc.is_empty() {
                acc.push('.');
            }
            acc.push_str(part);
            if !self.layer_order.iter().any(|l| **l == *acc) {
                self.layer_order.push(Rc::from(acc.as_str()));
            }
        }
        full
    }
}

const MAX_RULES: usize = 200_000;

// --------------------------------------------------------------------------
// Parsing
// --------------------------------------------------------------------------

thread_local! {
    /// The source text of each `unicode-range` descriptor in the sheet being
    /// parsed, in order: tokens lose what the ranges need (`U+0000-00FF`
    /// tokenizes as numbers, `U+038E-03A1` even as an exponent).
    static RAW_RANGES: std::cell::RefCell<std::collections::VecDeque<String>> = const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

/// `unicode-range: …` values in source order (comments skipped).
fn raw_unicode_ranges(src: &str) -> std::collections::VecDeque<String> {
    let mut out = std::collections::VecDeque::new();
    if !src.contains("unicode-range") {
        return out;
    }
    let b = src.as_bytes();
    let mut i = 0;
    let mut clean = String::with_capacity(src.len());
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            match src[i + 2..].find("*/") {
                Some(e) => i = i + 2 + e + 2,
                None => break,
            }
            clean.push(' ');
            continue;
        }
        let ch = src[i..].chars().next().unwrap_or(' ');
        clean.push(ch);
        i += ch.len_utf8();
    }
    let mut rest = clean.as_str();
    while let Some(p) = rest.find("unicode-range") {
        let after = &rest[p + "unicode-range".len()..];
        let t = after.trim_start();
        if let Some(v) = t.strip_prefix(':') {
            let end = v.find([';', '}']).unwrap_or(v.len());
            out.push_back(v[..end].trim().to_string());
        }
        rest = after;
    }
    out
}

pub fn parse_stylesheet(src: &str, base: &url::Url, origin: Origin) -> Stylesheet {
    let saved = RAW_RANGES.with(|r| std::mem::replace(&mut *r.borrow_mut(), raw_unicode_ranges(src)));
    let sheet = parse_stylesheet_inner(src, base, origin);
    RAW_RANGES.with(|r| *r.borrow_mut() = saved);
    sheet
}

fn parse_stylesheet_inner(src: &str, base: &url::Url, origin: Origin) -> Stylesheet {
    let mut cvs = tokenizer::parse_cvs(src);
    tokenizer::absolutize_urls(&mut cvs, base);
    let mut sheet = Stylesheet {
        rules: Vec::new(),
        font_faces: Vec::new(),
        imports: Vec::new(),
        origin,
        base: base.clone(),
        layer_order: Vec::new(),
        properties: Vec::new(),
        counter_styles: Vec::new(),
        keyframes: Vec::new(),
        cur_layer: None,
        cur_scope: None,
        cur_containers: Vec::new(),
        anon_layers: 0,
        layers_before_imports: 0,
    };
    parse_rule_list(&cvs, &mut sheet, &[], None, true);
    sheet
}

fn parse_rule_list(cvs: &[CV], sheet: &mut Stylesheet, media: &[Rc<Vec<CV>>], parent: Option<&[Selector]>, top: bool) {
    let mut i = 0;
    while i < cvs.len() {
        if sheet.rules.len() > MAX_RULES {
            return;
        }
        match &cvs[i] {
            // A stray `;` is not skipped: it becomes part of the next rule's prelude (invalidating it), as in browsers.
            CV::T(Tok::Ws) | CV::T(Tok::Cdo) | CV::T(Tok::Cdc) => i += 1,
            CV::T(Tok::AtKeyword(name)) => {
                let name = name.to_ascii_lowercase();
                // prelude until ';' or '{}' block
                let start = i + 1;
                let mut j = start;
                let mut block = None;
                while j < cvs.len() {
                    match &cvs[j] {
                        CV::T(Tok::Semicolon) => break,
                        CV::Block('{', b) => {
                            block = Some(b);
                            break;
                        }
                        _ => j += 1,
                    }
                }
                let prelude = trim(&cvs[start..j.min(cvs.len())]);
                handle_at_rule(&name, prelude, block.map(|b| b.as_slice()), sheet, media, parent, top);
                i = j + 1;
            }
            _ => {
                // qualified rule: prelude until {} block
                let start = i;
                let mut j = i;
                while j < cvs.len() && !matches!(cvs[j], CV::Block('{', _)) {
                    j += 1;
                }
                if j >= cvs.len() {
                    return;
                }
                let prelude = trim(&cvs[start..j]);
                if let CV::Block('{', body) = &cvs[j] {
                    if let Some(sels) = selector::parse_selector_list(prelude, parent) {
                        parse_style_block(body, sels, sheet, media);
                    }
                }
                i = j + 1;
            }
        }
    }
}

/// Parse a style rule's block: declarations plus nested rules (CSS Nesting),
/// in order (declarations after a nested rule apply after it).
fn parse_style_block(body: &[CV], sels: Vec<Selector>, sheet: &mut Stylesheet, media: &[Rc<Vec<CV>>]) {
    let mut decls = Vec::new();
    let flush = |decls: &mut Vec<Declaration>, sheet: &mut Stylesheet| {
        if !decls.is_empty() {
            sheet.rules.push(StyleRule { selectors: sels.clone(), decls: Rc::new(std::mem::take(decls)), media: media.to_vec(), layer: sheet.cur_layer.clone(), scope: sheet.cur_scope.clone(), containers: sheet.cur_containers.clone() });
        }
    };
    let mut i = 0;
    let mut start = 0;
    while i < body.len() {
        match &body[i] {
            CV::T(Tok::Semicolon) => {
                if let Some(d) = parse_declaration(&body[start..i]) {
                    decls.push(d);
                }
                start = i + 1;
            }
            CV::Block('{', inner) => {
                // Nested rule or nested at-rule.
                let prelude = trim(&body[start..i]);
                if let Some(CV::T(Tok::AtKeyword(n))) = prelude.first() {
                    let name = n.to_ascii_lowercase();
                    let pre = trim(&prelude[1..]).to_vec();
                    if nested_at_ok(&name, &pre) {
                        flush(&mut decls, sheet);
                        parse_nested_at(&name, pre, inner, &sels, sheet, media);
                    }
                } else if !prelude.is_empty() {
                    if let Some(child) = selector::parse_selector_list(prelude, Some(&sels)) {
                        flush(&mut decls, sheet);
                        parse_style_block(inner, child, sheet, media);
                    }
                }
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if start < body.len() {
        if let Some(d) = parse_declaration(&body[start..]) {
            decls.push(d);
        }
    }
    flush(&mut decls, sheet);
}

/// Whether a nested group rule is valid (it then splits the declarations around it).
fn nested_at_ok(name: &str, prelude: &[CV]) -> bool {
    match name {
        "media" | "container" | "layer" | "scope" | "starting-style" => true,
        "supports" => supports_valid(&tokenizer::serialize(prelude)),
        _ => false,
    }
}

fn parse_nested_at(name: &str, prelude: Vec<CV>, inner: &[CV], sels: &[Selector], sheet: &mut Stylesheet, media: &[Rc<Vec<CV>>]) {
    match name {
        "media" => {
            let mut m = media.to_vec();
            m.push(Rc::new(prelude));
            parse_style_block(inner, sels.to_vec(), sheet, &m);
        }
        "container" => {
            sheet.cur_containers.push(Rc::new(container_query(&prelude)));
            parse_style_block(inner, sels.to_vec(), sheet, media);
            sheet.cur_containers.pop();
        }
        "supports" => {
            if supports(&prelude) {
                parse_style_block(inner, sels.to_vec(), sheet, media);
            }
        }
        "layer" => {
            let names = layer_names(&prelude);
            let n = names.into_iter().next().unwrap_or_else(|| {
                sheet.anon_layers += 1;
                format!("\u{1}anon{}", sheet.anon_layers)
            });
            let full = sheet.declare_layer(&n);
            let old = sheet.cur_layer.replace(full);
            parse_style_block(inner, sels.to_vec(), sheet, media);
            sheet.cur_layer = old;
        }
        _ => {}
    }
}

/// Names in a `@layer a.b, c` prelude.
fn layer_names(prelude: &[CV]) -> Vec<String> {
    tokenizer::split_commas(prelude)
        .iter()
        .map(|p| tokenizer::serialize(p).chars().filter(|c| !c.is_whitespace()).collect::<String>())
        .filter(|n| !n.is_empty())
        .collect()
}

/// Split an `@container` prelude into its optional name and condition.
fn container_query(prelude: &[CV]) -> ContainerQuery {
    let toks: Vec<CV> = trim(prelude).to_vec();
    match toks.first() {
        Some(CV::T(Tok::Ident(n))) if !matches!(n.to_ascii_lowercase().as_str(), "not" | "and" | "or") => {
            let rest = trim(&toks[1..]).to_vec();
            ContainerQuery { name: Some(n.clone()), cond: rest }
        }
        _ => ContainerQuery { name: None, cond: toks },
    }
}

/// An element's query-container context: the chain of ancestor containers.
pub struct CChain {
    pub node: NodeId,
    pub names: Option<Rc<Vec<String>>>,
    /// 0 none (style container only), 1 inline-size, 2 size.
    pub ctype: u8,
    pub size: Option<(f32, f32)>,
    pub style: Rc<ComputedStyle>,
    pub parent: Option<Rc<CChain>>,
}

impl CChain {
    fn iter(self: &Rc<CChain>) -> impl Iterator<Item = &CChain> {
        std::iter::successors(Some(&**self), |c| c.parent.as_deref())
    }
}

/// The nearest size container (for cq* units).
pub fn cq_size(chain: Option<&Rc<CChain>>) -> Option<(f32, f32)> {
    chain?.iter().find(|c| c.ctype != 0).and_then(|c| c.size.map(|(w, h)| (w, if c.ctype == 2 { h } else { f32::NAN })))
}

/// Evaluate a container query for an element whose ancestors are `chain`
/// and whose parent element's style is `parent` (CSS Conditional 5 §7).
fn container_matches(q: &ContainerQuery, chain: Option<&Rc<CChain>>, parent: &ComputedStyle, env: &MediaEnv) -> bool {
    let named = |c: &&CChain| match &q.name {
        Some(n) => c.names.as_ref().map(|v| v.iter().any(|x| x == n)).unwrap_or(false),
        None => true,
    };
    let toks: Vec<&CV> = q.cond.iter().filter(|c| !c.is_ws()).collect();
    container_cond(&toks, &|c: &CV| match c {
        CV::Func(f, args) if f.eq_ignore_ascii_case("style") => {
            // Style queries: every element is a style container.
            let custom = match (&q.name, chain) {
                (None, _) => Some(parent.custom.clone()),
                (Some(_), Some(ch)) => ch.iter().find(named).map(|c| c.style.custom.clone()),
                _ => None,
            };
            let custom = match custom {
                Some(c) => c,
                None => return false,
            };
            let w: Vec<&CV> = args.iter().filter(|c| !c.is_ws()).collect();
            let name = match w.first().and_then(|c| c.ident()) {
                Some(n) if n.starts_with("--") => n.to_string(),
                _ => return false,
            };
            let have = custom.get(&name).map(|v| tokenizer::serialize(v).trim().to_string());
            match w.iter().position(|c| matches!(c, CV::T(Tok::Colon))) {
                Some(i) => {
                    let want = tokenizer::serialize(&args[args.iter().position(|c| matches!(c, CV::T(Tok::Colon))).unwrap() + 1..]).trim().to_string();
                    let _ = i;
                    have.as_deref() == Some(want.as_str())
                }
                None => have.map(|h| !h.is_empty()).unwrap_or(false),
            }
        }
        CV::Block('(', _) => {
            let c2 = match chain.and_then(|ch| ch.iter().filter(named).find(|c| c.ctype != 0)) {
                Some(c) => c,
                None => return false,
            };
            let (w, h) = match c2.size {
                Some(s) => s,
                None => return false,
            };
            let e = MediaEnv { width: w, height: if c2.ctype == 2 { h } else { f32::NAN }, ..*env };
            cond_matches(c, &e)
        }
        _ => false,
    })
}

/// `<cond> [and|or <cond>]*` with `not` prefixes, terms evaluated by `term`.
fn container_cond(w: &[&CV], term: &dyn Fn(&CV) -> bool) -> bool {
    let mut i = 0;
    let mut result: Option<bool> = None;
    let mut op_or = false;
    while i < w.len() {
        let c = w[i];
        if c.is_ident("and") {
            op_or = false;
        } else if c.is_ident("or") {
            op_or = true;
        } else {
            let (v, step) = if c.is_ident("not") {
                (w.get(i + 1).map(|n| !term_or_group(n, term)).unwrap_or(false), 2)
            } else {
                (term_or_group(c, term), 1)
            };
            result = Some(match result {
                None => v,
                Some(r) => if op_or { r || v } else { r && v },
            });
            i += step;
            continue;
        }
        i += 1;
    }
    result.unwrap_or(false)
}

fn term_or_group(c: &CV, term: &dyn Fn(&CV) -> bool) -> bool {
    if let CV::Block('(', inner) = c {
        let w: Vec<&CV> = inner.iter().filter(|c| !c.is_ws()).collect();
        // A parenthesised sub-condition rather than a size feature.
        if w.first().map(|f| matches!(f, CV::Block('(', _) | CV::Func(..)) || f.is_ident("not")).unwrap_or(false) {
            return container_cond(&w, term);
        }
    }
    term(c)
}

pub fn parse_declaration(cvs: &[CV]) -> Option<Declaration> {
    let cvs = trim(cvs);
    let name = match cvs.first()? {
        CV::T(Tok::Ident(n)) => n.clone(),
        _ => return None,
    };
    let mut i = 1;
    while i < cvs.len() && cvs[i].is_ws() {
        i += 1;
    }
    if !matches!(cvs.get(i), Some(CV::T(Tok::Colon))) {
        return None;
    }
    let mut value: Vec<CV> = trim(&cvs[i + 1..]).to_vec();
    let mut important = false;
    // trailing "! important"
    let n = value.len();
    if n >= 2 {
        let last = &value[n - 1];
        let mut k = n - 2;
        while k > 0 && value[k].is_ws() {
            k -= 1;
        }
        if last.is_ident("important") && value[k].is_delim('!') {
            important = true;
            value.truncate(k);
            let t = trim(&value).to_vec();
            value = t;
        }
    }
    let custom = name.starts_with("--");
    let lname = if custom { name } else { name.to_ascii_lowercase() };
    if value.is_empty() && !custom {
        return None;
    }
    let has_var = contains_var(&value);
    Some(Declaration { name: Rc::from(lname.as_str()), value: Rc::new(value), important, has_var })
}

fn contains_var(v: &[CV]) -> bool {
    v.iter().any(|c| match c {
        CV::Func(n, a) => n == "var" || n == "env" || contains_var(a),
        CV::Block(_, a) => contains_var(a),
        _ => false,
    })
}

pub fn parse_declarations(src: &str) -> Vec<Declaration> {
    parse_declarations_in(src, None)
}

/// Parse a declaration list (a `style` attribute), resolving URLs against `base`.
pub fn parse_declarations_in(src: &str, base: Option<&url::Url>) -> Vec<Declaration> {
    let mut cvs = tokenizer::parse_cvs(src);
    if let Some(b) = base {
        tokenizer::absolutize_urls(&mut cvs, b);
    }
    let mut out = Vec::new();
    for part in cvs.split(|c| matches!(c, CV::T(Tok::Semicolon))) {
        if let Some(d) = parse_declaration(part) {
            out.push(d);
        }
    }
    out
}

fn url_from_cvs(prelude: &[CV]) -> Option<(String, usize)> {
    for (i, c) in prelude.iter().enumerate() {
        match c {
            CV::T(Tok::Url(u)) | CV::T(Tok::Str(u)) => return Some((u.clone(), i)),
            CV::Func(n, a) if n == "url" => {
                for x in a {
                    if let CV::T(Tok::Str(s)) = x {
                        return Some((s.clone(), i));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn handle_at_rule(name: &str, prelude: &[CV], block: Option<&[CV]>, sheet: &mut Stylesheet, media: &[Rc<Vec<CV>>], parent: Option<&[Selector]>, top: bool) {
    match name {
        "import" if top => {
            if let Some((u, idx)) = url_from_cvs(prelude) {
                let rest = trim(&prelude[idx + 1..]);
                let mut layer = None;
                let mut supports_ok = true;
                for c in rest.iter() {
                    match c {
                        CV::Func(n, a) if n.eq_ignore_ascii_case("supports") => {
                            let a = trim(a);
                            let decl = a.iter().any(|x| matches!(x, CV::T(Tok::Colon)));
                            supports_ok = !a.is_empty() && if decl { supports(&[CV::Block('(', a.to_vec())]) } else { supports(a) };
                        }
                        CV::Func(n, a) if n.eq_ignore_ascii_case("layer") => {
                            layer = Some(tokenizer::serialize(trim(a)).trim().to_string());
                        }
                        c if c.is_ident("layer") => layer = Some(String::new()),
                        _ => {}
                    }
                }
                // strip layer()/supports() clauses
                let rest: Vec<CV> = rest.iter().filter(|c| !matches!(c, CV::Func(n, _) if n == "layer" || n == "supports") && !c.is_ident("layer")).cloned().collect();
                let rest = trim(&rest).to_vec();
                sheet.imports.push(Import { url: u, media: if rest.is_empty() { None } else { Some(Rc::new(rest)) }, supports_ok, layer });
                sheet.layers_before_imports = sheet.layer_order.len();
            }
        }
        "media" => {
            if let Some(b) = block {
                let mut m = media.to_vec();
                m.push(Rc::new(prelude.to_vec()));
                parse_rule_list(b, sheet, &m, parent, false);
            }
        }
        "container" => {
            if let Some(b) = block {
                sheet.cur_containers.push(Rc::new(container_query(prelude)));
                parse_rule_list(b, sheet, media, parent, false);
                sheet.cur_containers.pop();
            }
        }
        "supports" => {
            if let Some(b) = block {
                if supports(prelude) {
                    parse_rule_list(b, sheet, media, parent, false);
                }
            }
        }
        "layer" => {
            let names = layer_names(prelude);
            match block {
                Some(b) => {
                    let n = names.into_iter().next().unwrap_or_else(|| {
                        sheet.anon_layers += 1;
                        format!("\u{1}anon{}", sheet.anon_layers)
                    });
                    let full = sheet.declare_layer(&n);
                    let old = sheet.cur_layer.replace(full);
                    parse_rule_list(b, sheet, media, parent, false);
                    sheet.cur_layer = old;
                }
                None => {
                    for n in names {
                        sheet.declare_layer(&n);
                    }
                }
            }
        }
        "scope" => {
            if let Some(b) = block {
                // @scope (<root>) [to (<limit>)]
                let blocks: Vec<&Vec<CV>> = prelude.iter().filter_map(|c| if let CV::Block('(', inner) = c { Some(inner) } else { None }).collect();
                let root = blocks.first().and_then(|r| selector::parse_selector_list(r, None));
                let has_to = prelude.iter().any(|c| c.is_ident("to"));
                let limit = if has_to { blocks.get(1).and_then(|l| selector::parse_selector_list(l, None)) } else { None };
                if let Some(root) = root {
                    let scope = Rc::new(Scope { root, limit: limit.unwrap_or_default() });
                    let old = sheet.cur_scope.replace(scope);
                    parse_rule_list(b, sheet, media, parent, false);
                    sheet.cur_scope = old;
                }
            }
        }
        "document" | "-moz-document" => {
            if let Some(b) = block {
                parse_rule_list(b, sheet, media, parent, false);
            }
        }
        "property" => {
            let name = prelude.iter().find_map(|c| c.ident()).filter(|n| n.starts_with("--")).map(|n| n.to_string());
            if let (Some(name), Some(b)) = (name, block) {
                let mut inherits = None;
                let mut initial = None;
                let mut syntax: Option<Rc<str>> = None;
                for part in b.split(|c| matches!(c, CV::T(Tok::Semicolon))) {
                    if let Some(d) = parse_declaration(part) {
                        match &*d.name {
                            "inherits" => inherits = d.value.iter().find_map(|c| c.ident()).map(|v| v.eq_ignore_ascii_case("true")),
                            "initial-value" => initial = Some(d.value.clone()),
                            "syntax" => {
                                syntax = d.value.iter().find_map(|c| if let CV::T(Tok::Str(t)) = c { Some(Rc::from(t.trim())) } else { None });
                            }
                            _ => {}
                        }
                    }
                }
                // `inherits` is required for a valid registration.
                if let Some(inh) = inherits {
                    sheet.properties.push((name, inh, initial, syntax.filter(|s| &**s != "*")));
                }
            }
        }
        "counter-style" => {
            let name = prelude.iter().find_map(|c| c.ident()).map(|n| n.to_string());
            if let (Some(name), Some(b)) = (name, block) {
                let decls: Vec<Declaration> = b.split(|c| matches!(c, CV::T(Tok::Semicolon))).filter_map(parse_declaration).collect();
                if let Some(r) = counters::parse_rule(&name, &decls) {
                    sheet.counter_styles.retain(|x| x.name != r.name);
                    sheet.counter_styles.push(r);
                }
            }
        }
        "font-face" => {
            if let Some(b) = block {
                if let Some(ff) = parse_font_face(b, &sheet.base) {
                    sheet.font_faces.push(ff);
                }
            }
        }
        "keyframes" | "-webkit-keyframes" | "-moz-keyframes" => {
            let name = prelude.iter().find_map(|c| match c {
                CV::T(Tok::Ident(i)) => Some(i.clone()),
                CV::T(Tok::Str(s)) => Some(s.clone()),
                _ => None,
            });
            if let (Some(name), Some(b)) = (name, block) {
                if !name.eq_ignore_ascii_case("none") {
                    sheet.keyframes.push((name, Rc::new(anim::parse_keyframes(b)), media.to_vec()));
                }
            }
        }
        _ => {} // page, namespace...
    }
}

fn parse_font_face(block: &[CV], base: &url::Url) -> Option<FontFace> {
    let mut family = None;
    let mut sources = Vec::new();
    let mut weight = (1, 1000);
    let mut explicit_weight = false;
    let mut style = FontStyle::Normal;
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    let mut variant = String::new();
    for part in block.split(|c| matches!(c, CV::T(Tok::Semicolon))) {
        let d = match parse_declaration(part) {
            Some(d) => d,
            None => continue,
        };
        match &*d.name {
            "font-family" => family = style::parse_font_family(&d.value).and_then(|f| f.into_iter().next()),
            "src" => {
                for s in split_commas(&d.value) {
                    let mut url = None;
                    let mut fmt = String::new();
                    for c in words(s) {
                        match c {
                            CV::T(Tok::Url(u)) => url = Some(u.clone()),
                            CV::Func(n, a) if n == "url" => {
                                if let Some(CV::T(Tok::Str(u))) = words(a).first() {
                                    url = Some(u.clone());
                                }
                            }
                            CV::Func(n, a) if n == "format" => {
                                if let Some(f) = words(a).first() {
                                    fmt = match f {
                                        CV::T(Tok::Str(s)) => s.clone(),
                                        CV::T(Tok::Ident(s)) => s.clone(),
                                        _ => String::new(),
                                    }
                                    .to_ascii_lowercase();
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(u) = url {
                        sources.push((u, fmt));
                    }
                }
            }
            "font-weight" => {
                let nums: Vec<u16> = words(&d.value)
                    .iter()
                    .filter_map(|c| match c {
                        CV::T(Tok::Number(n)) => Some(*n as u16),
                        c if c.is_ident("bold") => Some(700),
                        c if c.is_ident("normal") => Some(400),
                        _ => None,
                    })
                    .collect();
                explicit_weight = true;
                match nums.len() {
                    1 => weight = (nums[0], nums[0]),
                    2 => weight = (nums[0].min(nums[1]), nums[0].max(nums[1])),
                    _ => explicit_weight = false,
                }
            }
            "font-style" | "font-stretch" => {
                variant.push_str(&d.name);
                variant.push(':');
                variant.push_str(&tokenizer::serialize(&d.value));
                variant.push(';');
                if &*d.name == "font-style" && words(&d.value).first().map(|c| c.is_ident("italic") || c.is_ident("oblique")).unwrap_or(false) {
                    style = FontStyle::Italic;
                }
            }
            "unicode-range" => {
                let raw = RAW_RANGES.with(|r| r.borrow_mut().pop_front());
                ranges = parse_unicode_range(&raw.unwrap_or_else(|| tokenizer::serialize(&d.value)));
            }
            _ => {}
        }
    }
    if !explicit_weight {
        weight = (400, 400);
    }
    Some(FontFace { family: family?, sources, weight, style, base: base.clone(), ranges, variant })
}

/// Parse a unicode-range descriptor ("U+0000-00FF, U+0131, U+4??").
pub fn parse_unicode_range(s: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let p = part.trim().trim_start_matches(['U', 'u']).trim_start_matches('+').replace(' ', "");
        if p.is_empty() {
            continue;
        }
        if let Some((a, b)) = p.split_once('-') {
            if let (Ok(a), Ok(b)) = (u32::from_str_radix(a, 16), u32::from_str_radix(b, 16)) {
                out.push((a, b));
            }
        } else if p.contains('?') {
            let lo = u32::from_str_radix(&p.replace('?', "0"), 16);
            let hi = u32::from_str_radix(&p.replace('?', "F"), 16);
            if let (Ok(a), Ok(b)) = (lo, hi) {
                out.push((a, b));
            }
        } else if let Ok(a) = u32::from_str_radix(&p, 16) {
            out.push((a, a));
        }
    }
    out
}

pub fn known_property(name: &str) -> bool {
    const KNOWN: &[&str] = &[
        "display", "position", "float", "clear", "top", "right", "bottom", "left", "inset", "width", "height", "min-width",
        "min-height", "max-width", "max-height", "margin", "padding", "border", "border-radius", "box-sizing", "overflow",
        "z-index", "opacity", "background", "background-color", "background-image", "box-shadow", "outline", "transform",
        "content", "text-decoration", "vertical-align", "flex", "flex-direction", "flex-wrap", "justify-content",
        "align-items", "align-self", "align-content", "gap", "row-gap", "column-gap", "grid", "grid-template-columns",
        "grid-template-rows", "grid-area", "grid-column", "grid-row", "color", "font", "font-family", "font-size",
        "font-weight", "font-style", "line-height", "text-align", "text-indent", "text-transform", "white-space",
        "letter-spacing", "word-spacing", "list-style", "cursor", "visibility", "border-collapse", "border-spacing",
        "aspect-ratio", "object-fit", "text-overflow", "order", "place-items", "place-content", "inset-inline",
        "margin-inline", "padding-inline", "text-shadow", "pointer-events", "user-select", "word-break", "overflow-wrap",
        "translate", "rotate", "scale", "accent-color", "fill", "stroke", "container", "container-type", "container-name", "contain", "will-change", "unicode-bidi", "contain-intrinsic-size", "contain-intrinsic-width", "contain-intrinsic-height", "content-visibility", "text-wrap", "appearance", "-webkit-appearance",
        "overflow-x", "overflow-y", "overflow-inline", "overflow-block", "clip-path", "mask", "mask-image",
        "-webkit-mask", "-webkit-mask-image", "line-clamp", "-webkit-line-clamp", "-webkit-box-orient",
        "object-position", "inset-block", "inset-inline-start", "inset-inline-end", "inset-block-start",
        "inset-block-end", "inline-size", "block-size", "min-inline-size", "max-inline-size", "min-block-size",
        "max-block-size", "place-self", "justify-items", "justify-self", "caret-color", "tab-size", "direction",
        "list-style-type", "list-style-position", "list-style-image", "table-layout", "caption-side", "empty-cells", "quotes", "counter-reset", "counter-increment", "counter-set",
        "hyphens", "-webkit-hyphens", "white-space-collapse", "text-wrap-mode", "text-wrap-style", "columns", "column-count",
        "font-variant", "font-variant-caps", "font-feature-settings", "font-stretch", "outline-offset",
        "outline-width", "outline-style", "outline-color", "transform-origin", "-webkit-text-fill-color",
        "perspective", "perspective-origin", "transform-style", "backface-visibility", "-webkit-backface-visibility",
        "writing-mode", "text-orientation", "text-combine-upright", "shape-outside", "shape-margin", "image-rendering",
        "-webkit-background-clip", "background-clip", "visibility", "resize", "scroll-behavior", "touch-action",
        "overscroll-behavior", "overscroll-behavior-x", "overscroll-behavior-y", "overscroll-behavior-block", "overscroll-behavior-inline",
        "scroll-snap-type", "scroll-snap-align", "scroll-snap-stop", "scrollbar-gutter", "scrollbar-width", "break-before", "break-after",
        "break-inside", "view-transition-name", "field-sizing", "mix-blend-mode", "isolation",
    ];
    if name.starts_with("--") {
        return true;
    }
    // Properties OHB does not implement, so feature queries take the fallback.
    const UNSUPPORTED: &[&str] = &["margin-trim"];
    if UNSUPPORTED.contains(&name) {
        return false;
    }
    KNOWN.contains(&name)
        || name.starts_with("border-")
        || name.starts_with("margin-")
        || name.starts_with("padding-")
        || name.starts_with("grid-")
        || name.starts_with("flex-")
        || name.starts_with("background-")
        || name.starts_with("text-decoration")
}

/// `CSS.supports(condition)`.
/// Whether `q` parses as a `<supports-condition>` (an @supports prelude).
pub fn supports_valid(q: &str) -> bool {
    let cvs = tokenizer::parse_cvs(q);
    let w: Vec<&CV> = cvs.iter().filter(|c| !c.is_ws()).collect();
    sv_cond(&w)
}

fn sv_cond(w: &[&CV]) -> bool {
    if w.is_empty() {
        return false;
    }
    if w[0].is_ident("not") {
        return w.len() == 2 && sv_in_parens(w[1]);
    }
    if !sv_in_parens(w[0]) {
        return false;
    }
    let mut op: Option<bool> = None;
    let mut i = 1;
    while i < w.len() {
        let is_and = w[i].is_ident("and");
        if !is_and && !w[i].is_ident("or") {
            return false;
        }
        if op.is_some_and(|o| o != is_and) {
            return false;
        }
        op = Some(is_and);
        match w.get(i + 1) {
            Some(c) if sv_in_parens(c) => {}
            _ => return false,
        }
        i += 2;
    }
    true
}

fn sv_in_parens(c: &CV) -> bool {
    match c {
        CV::Block('(', inner) => mq_any_value_ok(inner),
        CV::Func(_, a) => mq_any_value_ok(a),
        _ => false,
    }
}

pub fn supports_query(q: &str) -> bool {
    let cvs = tokenizer::parse_cvs(q);
    supports(&cvs)
}

fn supports(prelude: &[CV]) -> bool {
    let w: Vec<&CV> = prelude.iter().filter(|c| !c.is_ws()).collect();
    supports_cond(&w)
}

fn supports_cond(w: &[&CV]) -> bool {
    if w.is_empty() {
        return false;
    }
    if w[0].is_ident("not") {
        return !supports_cond(&w[1..]);
    }
    // a and b / a or b
    let mut result = supports_atom(w[0]);
    let mut i = 1;
    while i + 1 < w.len() {
        let rhs = supports_atom(w[i + 1]);
        if w[i].is_ident("and") {
            result = result && rhs;
        } else if w[i].is_ident("or") {
            result = result || rhs;
        }
        i += 2;
    }
    result
}

fn supports_atom(c: &CV) -> bool {
    match c {
        CV::Block('(', inner) => {
            let iw: Vec<&CV> = inner.iter().filter(|c| !c.is_ws()).collect();
            if iw.len() >= 2 && matches!(iw[1], CV::T(Tok::Colon)) {
                if let Some(name) = iw[0].ident() {
                    let n = name.to_ascii_lowercase();
                    let engine = pv::is_known(&n);
                    if !known_property(&n) && !engine {
                        return false;
                    }
                    // Reject obviously unsupported values.
                    let val = tokenizer::serialize(inner);
                    let is_color = n == "color" || n.ends_with("-color") || n == "fill" || n == "stroke";
                    if (val.contains("subgrid") && n != "grid-template-rows") || val.contains("masonry") || val.contains("anchor") || (!is_color && val.contains("light-dark(")) {
                        return false;
                    }
                    if engine && !n.starts_with("--") {
                        let ci = inner.iter().position(|c| matches!(c, CV::T(Tok::Colon))).unwrap_or(0);
                        let text = tokenizer::serialize(&inner[ci + 1..]);
                        // (`!important` is allowed in a supports declaration.)
                        let (v, _imp) = pv::block::strip_important(&text);
                        return pv::block::parse_one(&n, &v, false, &|_| false).is_some();
                    }
                    // Colors must parse.
                    if is_color {
                        let vals: Vec<&CV> = iw[2..].iter().copied().collect();
                        if vals.len() == 1 && matches!(vals[0], CV::T(Tok::Ident(k)) if matches!(k.to_ascii_lowercase().as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer")) {
                            return true;
                        }
                        return vals.len() == 1 && (values::parse_color(vals[0]).is_some() || color::parse(vals[0]).is_some());
                    }
                    return true;
                }
                return false;
            }
            supports_cond(&iw)
        }
        CV::Func(n, args) if n == "selector" => selector::parse_selector_list(args, None).is_some(),
        CV::Func(n, args) if n.eq_ignore_ascii_case("font-format") || n.eq_ignore_ascii_case("font-tech") => {
            let a: Vec<&CV> = args.iter().filter(|c| !c.is_ws()).collect();
            let [CV::T(Tok::Ident(k))] = a.as_slice() else { return false };
            let k = k.to_ascii_lowercase();
            if n.eq_ignore_ascii_case("font-format") {
                matches!(k.as_str(), "collection" | "opentype" | "truetype" | "woff" | "woff2")
            } else {
                matches!(k.as_str(), "features-opentype" | "color-colrv0" | "color-colrv1" | "color-cbdt" | "color-sbix" | "variations" | "palettes")
            }
        }
        CV::Func(n, args) if n.eq_ignore_ascii_case("at-rule") => {
            let a: Vec<&CV> = args.iter().filter(|c| !c.is_ws()).collect();
            let [CV::T(Tok::AtKeyword(k))] = a.as_slice() else { return false };
            matches!(
                k.to_ascii_lowercase().as_str(),
                "import" | "media" | "supports" | "font-face" | "keyframes" | "layer" | "container" | "page" | "namespace" | "counter-style" | "property" | "font-feature-values" | "font-palette-values" | "scope" | "starting-style" | "view-transition" | "charset"
            )
        }
        _ => false,
    }
}

// --------------------------------------------------------------------------
// Media queries
// --------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub struct MediaEnv {
    pub width: f32,
    pub height: f32,
    pub dpr: f32,
    pub dark: bool,
    /// The screen ("device-width"/"device-height"): the top-level viewport.
    pub dev_w: f32,
    pub dev_h: f32,
}

pub fn media_matches(cvs: &[CV], env: &MediaEnv) -> bool {
    let cvs = trim(cvs);
    if cvs.is_empty() {
        return true;
    }
    // Media Queries 4: each query true / false / unknown; a query that does
    // not parse is "not all".
    split_commas(cvs).iter().any(|q| media_query(q, env) == Some(Kleene::T))
}

/// One `<media-query>`: a condition, or `[not|only]? <type> [and <condition-without-or>]?`.
fn media_query(q: &[CV], env: &MediaEnv) -> Option<Kleene> {
    let w: Vec<&CV> = q.iter().filter(|c| !c.is_ws()).collect();
    if w.is_empty() {
        return None;
    }
    if let Some(k) = mq_condition(&w, env, true) {
        return Some(k);
    }
    let mut i = 0;
    let mut negate = false;
    if w[0].is_ident("not") {
        negate = true;
        i = 1;
    } else if w[0].is_ident("only") {
        i = 1;
    }
    let t = w.get(i)?.ident()?.to_ascii_lowercase();
    if matches!(t.as_str(), "not" | "and" | "or" | "only" | "layer") {
        return None;
    }
    i += 1;
    let mut k = if t == "all" || t == "screen" { Kleene::T } else { Kleene::F };
    if i < w.len() {
        if !w[i].is_ident("and") {
            return None;
        }
        let c = mq_condition(&w[i + 1..], env, false)?;
        k = match (k, c) {
            (Kleene::F, _) | (_, Kleene::F) => Kleene::F,
            (Kleene::T, Kleene::T) => Kleene::T,
            _ => Kleene::U,
        };
    }
    Some(if negate { k_not(k) } else { k })
}

#[allow(dead_code)]
fn media_matches_legacy(cvs: &[CV], env: &MediaEnv) -> bool {
    let cvs = trim(cvs);
    if cvs.is_empty() {
        return true;
    }
    split_commas(cvs).iter().any(|q| query_matches(q, env))
}

fn query_matches(q: &[CV], env: &MediaEnv) -> bool {
    let w: Vec<&CV> = q.iter().filter(|c| !c.is_ws()).collect();
    if w.is_empty() {
        return false;
    }
    let mut i = 0;
    let mut negate = false;
    if w[0].is_ident("not") {
        negate = true;
        i = 1;
    } else if w[0].is_ident("only") {
        i = 1;
    }
    let mut result = true;
    // media type
    if let Some(t) = w.get(i).and_then(|c| c.ident()) {
        let t = t.to_ascii_lowercase();
        if t != "and" && t != "or" && t != "not" {
            result = matches!(t.as_str(), "all" | "screen");
            i += 1;
        }
    }
    let mut op_or = false;
    while i < w.len() {
        let c = w[i];
        if c.is_ident("and") {
            op_or = false;
        } else if c.is_ident("or") {
            op_or = true;
        } else if c.is_ident("not") {
            if let Some(next) = w.get(i + 1) {
                let v = !cond_matches(next, env);
                result = if op_or { result || v } else { result && v };
                i += 2;
                continue;
            }
        } else {
            let v = cond_matches(c, env);
            result = if op_or { result || v } else { result && v };
        }
        i += 1;
    }
    result != negate
}

fn cond_matches(c: &CV, env: &MediaEnv) -> bool {
    let inner = match c {
        CV::Block('(', inner) => inner,
        _ => return false,
    };
    let w: Vec<&CV> = inner.iter().filter(|c| !c.is_ws()).collect();
    if w.is_empty() {
        return false;
    }
    // Nested condition: (a) and (b)
    if matches!(w[0], CV::Block('(', _)) || w[0].is_ident("not") {
        return query_matches(inner, env);
    }
    let ctx = values::UnitCtx::new(16.0, 16.0, env.width, env.height);
    let num = |c: &CV| -> Option<f32> {
        match c {
            CV::T(Tok::Number(n)) => Some(*n as f32),
            CV::T(Tok::Dimension(n, u)) if u == "dppx" || u == "x" => Some(*n as f32),
            CV::T(Tok::Dimension(n, u)) if u == "dpi" => Some((n / 96.0) as f32),
            CV::T(Tok::Dimension(n, u)) if u == "dpcm" => Some((n * 2.54 / 96.0) as f32),
            _ => values::parse_len(c, &ctx).map(|l| l.resolve(0.0)),
        }
    };
    let feature_value = |name: &str| -> Option<f32> {
        Some(match name {
            "width" | "inline-size" => env.width,
            "height" | "block-size" => env.height,
            "device-width" => env.dev_w,
            "device-height" => env.dev_h,
            "resolution" | "device-pixel-ratio" | "-webkit-device-pixel-ratio" => env.dpr,
            "aspect-ratio" => env.width / env.height.max(1e-6),
            "device-aspect-ratio" => env.dev_w / env.dev_h.max(1e-6),
            "color" => 8.0,
            "monochrome" | "grid" | "color-index" => 0.0,
            _ => return None,
        })
    };
    // name: value
    if w.len() >= 3 && matches!(w[1], CV::T(Tok::Colon)) {
        let name = match w[0].ident() {
            Some(n) => n.to_ascii_lowercase(),
            None => return false,
        };
        let val = w[2];
        let ratio = || -> Option<f32> {
            // a / b
            let a = num(w[2])?;
            if w.len() >= 5 && w[3].is_delim('/') {
                Some(a / num(w[4])?.max(0.0001))
            } else {
                Some(a)
            }
        };
        let (prefix, base) = if let Some(b) = name.strip_prefix("min-") {
            ("min", b.to_string())
        } else if let Some(b) = name.strip_prefix("max-") {
            ("max", b.to_string())
        } else if let Some(b) = name.strip_prefix("-webkit-min-") {
            ("min", b.to_string())
        } else if let Some(b) = name.strip_prefix("-webkit-max-") {
            ("max", b.to_string())
        } else {
            ("", name.clone())
        };
        if let Some(fv) = feature_value(&base) {
            let target = if base.contains("aspect-ratio") { ratio() } else { num(val) };
            let target = match target {
                Some(t) => t,
                None => return false,
            };
            let eq = (fv - target).abs() <= 1e-5 * fv.abs().max(1.0);
            return match prefix {
                "min" => fv >= target || eq,
                "max" => fv <= target || eq,
                _ => eq,
            };
        }
        let v = val.ident().map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        return match base.as_str() {
            "orientation" => (v == "portrait") == (env.height >= env.width),
            "prefers-color-scheme" => (v == "dark") == env.dark,
            "prefers-reduced-motion" | "prefers-reduced-transparency" | "prefers-reduced-data" => v == "no-preference",
            "prefers-contrast" => v == "no-preference",
            "hover" | "any-hover" => v == "hover",
            "pointer" | "any-pointer" => v == "fine",
            "display-mode" => v == "browser",
            "forced-colors" | "inverted-colors" => v == "none",
            "scripting" => v == "enabled",
            "update" => v == "fast",
            "overflow-block" => v == "scroll",
            "color-gamut" => v == "srgb",
            "dynamic-range" => v == "standard",
            _ => false,
        };
    }
    // Boolean feature: (color), (hover)
    if w.len() == 1 {
        if let Some(n) = w[0].ident() {
            return matches!(n.to_ascii_lowercase().as_str(), "color" | "hover" | "pointer" | "any-hover" | "any-pointer" | "width" | "height");
        }
    }
    // Range syntax: (width >= 600px), (400px <= width <= 700px)
    let mut toks: Vec<String> = Vec::new();
    let mut vals: Vec<Option<f32>> = Vec::new();
    let mut i = 0;
    while i < w.len() {
        let c = w[i];
        if let CV::T(Tok::Delim(d)) = c {
            let mut op = d.to_string();
            if matches!(w.get(i + 1), Some(CV::T(Tok::Delim('=')))) {
                op.push('=');
                i += 1;
            }
            toks.push(op);
            vals.push(None);
        } else if let Some(n) = c.ident() {
            toks.push(format!("@{}", n.to_ascii_lowercase()));
            vals.push(None);
        } else {
            toks.push("#".into());
            vals.push(num(c));
        }
        i += 1;
    }
    let resolve = |idx: usize| -> Option<f32> {
        let t = &toks[idx];
        if let Some(name) = t.strip_prefix('@') {
            feature_value(name)
        } else {
            vals[idx]
        }
    };
    let cmp = |a: f32, op: &str, b: f32| {
        let eq = (a - b).abs() <= 1e-5 * a.abs().max(b.abs()).max(1.0);
        match op {
            "<" => a < b && !eq,
            "<=" => a <= b || eq,
            ">" => a > b && !eq,
            ">=" => a >= b || eq,
            "=" => eq,
            _ => false,
        }
    };
    if toks.len() == 3 {
        if let (Some(a), Some(b)) = (resolve(0), resolve(2)) {
            return cmp(a, &toks[1], b);
        }
    } else if toks.len() == 5 {
        if let (Some(a), Some(b), Some(c2)) = (resolve(0), resolve(2), resolve(4)) {
            return cmp(a, &toks[1], b) && cmp(b, &toks[3], c2);
        }
    }
    false
}

/// Three-valued media query logic (Media Queries 4): `Unknown` for
/// general-enclosed and unrecognized features.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kleene {
    T,
    F,
    U,
}

fn k_not(k: Kleene) -> Kleene {
    match k {
        Kleene::T => Kleene::F,
        Kleene::F => Kleene::T,
        Kleene::U => Kleene::U,
    }
}

const MQ_RANGE_FEATURES: &[&str] = &["width", "height", "aspect-ratio", "resolution", "color", "color-index", "monochrome", "device-width", "device-height", "device-aspect-ratio", "device-pixel-ratio", "-webkit-device-pixel-ratio", "inline-size", "block-size"];

fn mq_discrete_values(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "orientation" => &["portrait", "landscape"],
        "scan" => &["interlace", "progressive"],
        "grid" => &[],
        "update" => &["none", "slow", "fast"],
        "overflow-block" => &["none", "scroll", "paged"],
        "overflow-inline" => &["none", "scroll"],
        "color-gamut" => &["srgb", "p3", "rec2020"],
        "pointer" | "any-pointer" => &["none", "coarse", "fine"],
        "hover" | "any-hover" => &["none", "hover"],
        "display-mode" => &["browser", "fullscreen", "standalone", "minimal-ui", "picture-in-picture", "window-controls-overlay"],
        "prefers-reduced-motion" | "prefers-reduced-transparency" | "prefers-reduced-data" => &["no-preference", "reduce"],
        "prefers-contrast" => &["no-preference", "less", "more", "custom"],
        "prefers-color-scheme" => &["light", "dark"],
        "forced-colors" => &["none", "active"],
        "inverted-colors" => &["none", "inverted"],
        "scripting" => &["none", "initial-only", "enabled"],
        "dynamic-range" | "video-dynamic-range" => &["standard", "high"],
        _ => return None,
    })
}

/// Whether a component value list is an `<any-value>` (no unmatched closing
/// brackets anywhere, no bad strings or URLs).
fn mq_any_value_ok(cvs: &[CV]) -> bool {
    cvs.iter().all(|c| match c {
        CV::T(Tok::RParen | Tok::RBracket | Tok::RBrace | Tok::BadUrl) => false,
        CV::Func(_, a) | CV::Block(_, a) => mq_any_value_ok(a),
        _ => true,
    })
}

/// A `<media-feature>` inside parentheses: Some(result) when it is a valid
/// feature, None when it does not parse as one (then general-enclosed).
fn mq_feature(w: &[&CV], block: &CV, env: &MediaEnv) -> Option<Kleene> {
    let b = |v: bool| if v { Kleene::T } else { Kleene::F };
    // Plain: name ':' value
    if w.len() >= 3 && matches!(w[1], CV::T(Tok::Colon)) {
        let name = w[0].ident()?.to_ascii_lowercase();
        let base = name.strip_prefix("min-").or_else(|| name.strip_prefix("max-")).or_else(|| name.strip_prefix("-webkit-min-")).or_else(|| name.strip_prefix("-webkit-max-"));
        let prefixed = base.is_some();
        let base = base.unwrap_or(&name);
        if MQ_RANGE_FEATURES.contains(&base) {
            let ctx = values::UnitCtx::new(16.0, 16.0, env.width, env.height);
            let ok = match base {
                "aspect-ratio" | "device-aspect-ratio" => {
                    (w.len() == 3 && matches!(w[2], CV::T(Tok::Number(n)) if *n >= 0.0)) || (w.len() == 5 && matches!(w[2], CV::T(Tok::Number(n)) if *n >= 0.0) && w[3].is_delim('/') && matches!(w[4], CV::T(Tok::Number(n)) if *n >= 0.0))
                }
                "resolution" | "device-pixel-ratio" | "-webkit-device-pixel-ratio" => {
                    w.len() == 3 && matches!(w[2], CV::T(Tok::Dimension(n, u)) if *n >= 0.0 && matches!(u.to_ascii_lowercase().as_str(), "dpi" | "dpcm" | "dppx" | "x")) || (base != "resolution" && w.len() == 3 && matches!(w[2], CV::T(Tok::Number(n)) if *n >= 0.0))
                }
                "color" | "color-index" | "monochrome" => w.len() == 3 && matches!(w[2], CV::T(Tok::Number(n)) if n.fract() == 0.0),
                _ => w.len() == 3 && matches!(values::parse_len(w[2], &ctx), Some(values::Len::Px(_))),
            };
            return if ok { Some(b(cond_matches(block, env))) } else { None };
        }
        if prefixed {
            return None;
        }
        let allowed = mq_discrete_values(base)?;
        if w.len() != 3 {
            return None;
        }
        if base == "grid" {
            return match w[2] {
                CV::T(Tok::Number(n)) if *n == 0.0 || *n == 1.0 => Some(b(*n == 0.0)),
                _ => None,
            };
        }
        let v = w[2].ident()?.to_ascii_lowercase();
        if !allowed.contains(&v.as_str()) {
            return None;
        }
        return Some(b(cond_matches(block, env)));
    }
    // Boolean: name
    if w.len() == 1 {
        let name = w[0].ident()?.to_ascii_lowercase();
        if MQ_RANGE_FEATURES.contains(&name.as_str()) {
            // Boolean context: true unless the value would be zero.
            let v = match name.as_str() {
                "width" | "inline-size" => env.width,
                "height" | "block-size" => env.height,
                "device-width" => env.dev_w,
                "device-height" => env.dev_h,
                "monochrome" | "color-index" => 0.0,
                _ => 1.0,
            };
            return Some(b(v != 0.0));
        }
        mq_discrete_values(&name)?;
        return Some(b(matches!(name.as_str(), "hover" | "any-hover" | "pointer" | "any-pointer" | "update" | "overflow-block" | "overflow-inline" | "color-gamut" | "display-mode" | "scripting" | "dynamic-range" | "video-dynamic-range" | "orientation" | "prefers-color-scheme")));
    }
    // Range: (width >= 600px), (400px <= width <= 700px)
    if let CV::Block(_, inner) = block {
        if mq_range_valid(inner, env) {
            return Some(b(cond_matches(block, env)));
        }
    }
    None
}

/// `<mf-range>`: `name op value`, `value op name`, or `value lt name lt value`
/// (both comparisons the same direction), with values of the feature's type.
fn mq_range_valid(inner: &[CV], env: &MediaEnv) -> bool {
    enum It {
        Op(String),
        Name(String),
        Val(Vec<CV>),
    }
    let mut items: Vec<It> = Vec::new();
    let mut i = 0;
    while i < inner.len() {
        match &inner[i] {
            CV::T(Tok::Ws) => i += 1,
            CV::T(Tok::Delim(d)) if matches!(d, '<' | '>' | '=') => {
                let mut op = d.to_string();
                if *d != '=' && matches!(inner.get(i + 1), Some(CV::T(Tok::Delim('=')))) {
                    op.push('=');
                    i += 1;
                }
                items.push(It::Op(op));
                i += 1;
            }
            CV::T(Tok::Ident(n)) => {
                items.push(It::Name(n.to_ascii_lowercase()));
                i += 1;
            }
            c => {
                let mut v = vec![c.clone()];
                i += 1;
                // A ratio: <number> / <number>.
                let mut j = i;
                while matches!(inner.get(j), Some(CV::T(Tok::Ws))) {
                    j += 1;
                }
                if inner.get(j).is_some_and(|x| x.is_delim('/')) {
                    j += 1;
                    while matches!(inner.get(j), Some(CV::T(Tok::Ws))) {
                        j += 1;
                    }
                    if let Some(n @ CV::T(Tok::Number(_))) = inner.get(j) {
                        v.push(CV::T(Tok::Delim('/')));
                        v.push(n.clone());
                        i = j + 1;
                    } else {
                        return false;
                    }
                }
                items.push(It::Val(v));
            }
        }
    }
    let ctx = values::UnitCtx::new(16.0, 16.0, env.width, env.height);
    let val_ok = |name: &str, v: &[CV]| -> bool {
        match name {
            "aspect-ratio" | "device-aspect-ratio" => {
                matches!(v, [CV::T(Tok::Number(n))] if *n >= 0.0) || matches!(v, [CV::T(Tok::Number(a)), _, CV::T(Tok::Number(b))] if *a >= 0.0 && *b >= 0.0)
            }
            "resolution" => matches!(v, [CV::T(Tok::Dimension(n, u))] if *n >= 0.0 && matches!(u.to_ascii_lowercase().as_str(), "dpi" | "dpcm" | "dppx" | "x")),
            "color" | "color-index" | "monochrome" => matches!(v, [CV::T(Tok::Number(n))] if n.fract() == 0.0),
            _ => v.len() == 1 && matches!(values::parse_len(&v[0], &ctx), Some(values::Len::Px(_))),
        }
    };
    let feature = |n: &str| MQ_RANGE_FEATURES.contains(&n) && !n.starts_with("min-") && !n.starts_with("max-") && !n.contains("pixel-ratio");
    let lt = |o: &str| o == "<" || o == "<=";
    let gt = |o: &str| o == ">" || o == ">=";
    match items.as_slice() {
        [It::Name(n), It::Op(_), It::Val(v)] | [It::Val(v), It::Op(_), It::Name(n)] => feature(n) && val_ok(n, v),
        [It::Val(a), It::Op(o1), It::Name(n), It::Op(o2), It::Val(c)] => feature(n) && val_ok(n, a) && val_ok(n, c) && ((lt(o1) && lt(o2)) || (gt(o1) && gt(o2))),
        _ => false,
    }
}

fn mq_in_parens(c: &CV, env: &MediaEnv) -> Option<Kleene> {
    match c {
        CV::Block('(', inner) => {
            let w: Vec<&CV> = inner.iter().filter(|c| !c.is_ws()).collect();
            if let Some(k) = mq_condition(&w, env, true) {
                return Some(k);
            }
            if let Some(k) = mq_feature(&w, c, env) {
                return Some(k);
            }
            if mq_any_value_ok(inner) {
                Some(Kleene::U)
            } else {
                None
            }
        }
        CV::Func(_, args) if mq_any_value_ok(args) => Some(Kleene::U),
        _ => None,
    }
}

/// `<media-condition>` (or `<media-condition-without-or>`): None = invalid.
fn mq_condition(w: &[&CV], env: &MediaEnv, allow_or: bool) -> Option<Kleene> {
    let first = w.first()?;
    if first.is_ident("not") {
        if w.len() != 2 {
            return None;
        }
        return mq_in_parens(w[1], env).map(k_not);
    }
    let mut acc = mq_in_parens(first, env)?;
    let mut op: Option<bool> = None;
    let mut i = 1;
    while i < w.len() {
        let is_and = w[i].is_ident("and");
        if !is_and && !(allow_or && w[i].is_ident("or")) {
            return None;
        }
        if op.is_some_and(|o| o != is_and) {
            return None;
        }
        op = Some(is_and);
        let v = mq_in_parens(w.get(i + 1)?, env)?;
        acc = match (is_and, acc, v) {
            (true, Kleene::F, _) | (true, _, Kleene::F) => Kleene::F,
            (true, Kleene::T, Kleene::T) => Kleene::T,
            (true, _, _) => Kleene::U,
            (false, Kleene::T, _) | (false, _, Kleene::T) => Kleene::T,
            (false, Kleene::F, Kleene::F) => Kleene::F,
            (false, _, _) => Kleene::U,
        };
        i += 2;
    }
    Some(acc)
}

/// CSSOM "serialize a media query list" of `text`: each query in canonical
/// form, or "not all" where it does not parse. An empty list is empty.
pub fn media_canon(text: &str) -> Vec<String> {
    let cvs = tokenizer::parse_cvs(text);
    let t = trim(&cvs);
    if t.is_empty() {
        return Vec::new();
    }
    let env = MediaEnv { width: 1024.0, height: 768.0, dpr: 1.0, dark: false, dev_w: 1024.0, dev_h: 768.0 };
    split_commas(t)
        .iter()
        .map(|q| {
            let w: Vec<&CV> = q.iter().filter(|c| !c.is_ws()).collect();
            if w.is_empty() || media_query(q, &env).is_none() {
                "not all".to_string()
            } else {
                mq_ser_query(&w, &env)
            }
        })
        .collect()
}

fn mq_ser_query(w: &[&CV], env: &MediaEnv) -> String {
    if mq_condition(w, env, true).is_some() {
        return mq_ser_condition(w, env);
    }
    let mut out = String::new();
    let mut i = 0;
    if w[0].is_ident("not") {
        out.push_str("not ");
        i = 1;
    } else if w[0].is_ident("only") {
        out.push_str("only ");
        i = 1;
    }
    let t = w[i].ident().unwrap_or("").to_ascii_lowercase();
    i += 1;
    if i < w.len() {
        let cond = mq_ser_condition(&w[i + 1..], env);
        if out.is_empty() && t == "all" {
            return cond;
        }
        format!("{out}{} and {cond}", pv::ser_ident(&t))
    } else {
        format!("{out}{}", pv::ser_ident(&t))
    }
}

fn mq_ser_condition(w: &[&CV], env: &MediaEnv) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in w {
        if let Some(k) = c.ident() {
            parts.push(k.to_ascii_lowercase());
        } else {
            parts.push(mq_ser_in_parens(c, env));
        }
    }
    parts.join(" ")
}

fn mq_ser_in_parens(c: &CV, env: &MediaEnv) -> String {
    match c {
        CV::Block('(', inner) => {
            let w: Vec<&CV> = inner.iter().filter(|c| !c.is_ws()).collect();
            if !w.is_empty() && mq_condition(&w, env, true).is_some() {
                return format!("({})", mq_ser_condition(&w, env));
            }
            if mq_feature(&w, c, env).is_some() {
                return format!("({})", mq_ser_feature(&w));
            }
            format!("({})", mq_ser_raw(trim(inner)))
        }
        other => mq_ser_raw(std::slice::from_ref(other)),
    }
}

/// A valid `<media-feature>`: lowercase names, single spaces, `a / b` ratios.
fn mq_ser_feature(w: &[&CV]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < w.len() {
        let piece = match w[i] {
            CV::T(Tok::Colon) => {
                if let Some(l) = parts.last_mut() {
                    l.push(':');
                }
                i += 1;
                continue;
            }
            CV::T(Tok::Delim(d)) if matches!(d, '<' | '>' | '=') => {
                let mut op = d.to_string();
                if *d != '=' && matches!(w.get(i + 1), Some(CV::T(Tok::Delim('=')))) {
                    op.push('=');
                    i += 1;
                }
                op
            }
            CV::T(Tok::Ident(n)) => pv::ser_ident(&n.to_ascii_lowercase()),
            CV::T(Tok::Dimension(n, u)) => format!("{}{}", calc::fmt_num(*n), u.to_ascii_lowercase()),
            CV::T(Tok::Number(n)) => calc::fmt_num(*n),
            other => mq_ser_raw(std::slice::from_ref(other)),
        };
        parts.push(piece);
        i += 1;
    }
    parts.join(" ")
}

/// Component values as CSS text (identifiers and strings escaped, whitespace collapsed).
fn mq_ser_raw(cvs: &[CV]) -> String {
    let mut out = String::new();
    for c in cvs {
        match c {
            CV::T(Tok::Ws) => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            CV::T(Tok::Ident(n)) => out.push_str(&pv::ser_ident(n)),
            CV::T(Tok::Str(s)) => out.push_str(&pv::ser_string(s)),
            CV::T(Tok::Number(n)) => out.push_str(&calc::fmt_num(*n)),
            CV::T(Tok::Dimension(n, u)) => {
                out.push_str(&calc::fmt_num(*n));
                out.push_str(u);
            }
            CV::T(Tok::Percentage(n)) => {
                out.push_str(&calc::fmt_num(*n));
                out.push('%');
            }
            CV::Func(n, a) => {
                out.push_str(&pv::ser_ident(n));
                out.push('(');
                out.push_str(&mq_ser_raw(a));
                out.push(')');
            }
            CV::Block(b, a) => {
                out.push(*b);
                out.push_str(&mq_ser_raw(a));
                out.push(match b {
                    '(' => ')',
                    '[' => ']',
                    _ => '}',
                });
            }
            other => out.push_str(&tokenizer::serialize(std::slice::from_ref(other))),
        }
    }
    out.trim().to_string()
}

/// Evaluate a `<media-condition>` (no media types): Some(true) only when it
/// parses and is definitely true; None when it does not parse.
pub fn media_condition(cvs: &[CV], env: &MediaEnv) -> Option<bool> {
    let w: Vec<&CV> = cvs.iter().filter(|c| !c.is_ws()).collect();
    mq_condition(&w, env, true).map(|k| k == Kleene::T)
}

/// HTML "parse a sizes attribute": the source size in CSS px. `auto_w` is
/// the rendered width of an image that allows auto-sizes.
pub fn sizes_px(sizes: &str, env: &MediaEnv, auto_w: Option<f32>) -> f32 {
    let cvs = tokenizer::parse_cvs(sizes);
    let items = split_commas(&cvs);
    let ctx = values::UnitCtx::new(16.0, 16.0, env.width, env.height);
    for item in items.iter() {
        let mut item: &[CV] = item;
        while item.last().is_some_and(|c| c.is_ws()) {
            item = &item[..item.len() - 1];
        }
        let Some(last) = item.last() else { continue };
        let size: Option<f32> = match last {
            CV::T(Tok::Ident(i)) if i.eq_ignore_ascii_case("auto") => auto_w,
            CV::T(Tok::Number(v)) if *v == 0.0 => Some(0.0),
            CV::T(Tok::Dimension(v, _)) if *v >= 0.0 => match values::parse_len(last, &ctx) {
                Some(values::Len::Px(px)) if px.is_finite() => Some(px),
                _ => continue,
            },
            CV::Func(name, _) if values::is_math_fn(name) => match values::parse_len(last, &ctx) {
                Some(values::Len::Px(px)) if px.is_finite() => Some(px.max(0.0)),
                _ => continue,
            },
            _ => continue,
        };
        let mut rest: &[CV] = &item[..item.len() - 1];
        while rest.last().is_some_and(|c| c.is_ws()) {
            rest = &rest[..rest.len() - 1];
        }
        if rest.is_empty() {
            match size {
                Some(s) => return s,
                None => continue,
            }
        }
        if media_condition(rest, env) != Some(true) {
            continue;
        }
        if let Some(s) = size {
            return s;
        }
    }
    env.width
}

// --------------------------------------------------------------------------
// Cascade
// --------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct IndexEntry {
    sheet: u32,
    rule: u32,
    sel: u16,
    spec: u32,
    order: u32,
    /// Cascade-layer precedence: higher wins for normal declarations
    /// (unlayered rules are highest); reversed for !important.
    layer: u16,
    /// Hashes of ids, classes and tags some ancestor must carry (0 = unused),
    /// checked against the ancestor Bloom filter before matching.
    anc: [u32; 4],
}

/// A Bloom filter of the ids, classes and tag names on an element's ancestors
/// (fast rejection of descendant and child selectors, as in Blink and Servo).
#[derive(Clone, Copy, Default)]
pub struct Bloom([u64; 8]);

impl Bloom {
    fn insert(&mut self, h: u32) {
        let (a, b) = (h & 511, (h >> 9) & 511);
        self.0[(a >> 6) as usize] |= 1 << (a & 63);
        self.0[(b >> 6) as usize] |= 1 << (b & 63);
    }
    fn might_contain(&self, h: u32) -> bool {
        let (a, b) = (h & 511, (h >> 9) & 511);
        self.0[(a >> 6) as usize] & (1 << (a & 63)) != 0 && self.0[(b >> 6) as usize] & (1 << (b & 63)) != 0
    }
    /// Add an element's id, classes and tag name.
    fn add_element(&mut self, el: &crate::dom::ElementData) {
        if let Some(id) = &el.id {
            self.insert(key_hash(b'#', id));
        }
        for c in &el.classes {
            self.insert(key_hash(b'.', c));
        }
        self.insert(key_hash_lc(b'<', &el.name));
    }
}

fn key_hash(kind: u8, s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5 ^ kind as u32;
    h = h.wrapping_mul(0x01000193);
    for b in s.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h | 1
}

fn key_hash_lc(kind: u8, s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5 ^ kind as u32;
    h = h.wrapping_mul(0x01000193);
    for b in s.bytes() {
        h ^= b.to_ascii_lowercase() as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h | 1
}

/// Record where the classes, ids and attributes a selector tests influence
/// matching, relative to an element carrying them. `outer` is the relation of
/// the selector's subject (the element being styled, or the compound a nested
/// selector list belongs to). `:has()` arguments are skipped: incremental
/// restyles re-evaluate the `:has()` anchors a mutation can reach instead.
fn collect_inv(sel: &Selector, outer: u8, inv: &mut Invalidation) {
    use selector::{Combinator, Pseudo};
    let mut prev_kind = outer;
    for (j, c) in sel.parts.iter().enumerate() {
        // Left of a sibling combinator: a preceding sibling of the subject or
        // of one of its ancestors; left of a descendant/child one: an ancestor.
        let sib = matches!(sel.combinators.get(j.wrapping_sub(1)), Some(Combinator::NextSibling) | Some(Combinator::SubsequentSibling));
        let kind = if j == 0 {
            outer
        } else if sib {
            // The compound to the right depends on what precedes it.
            inv.sibcomb |= prev_kind;
            INV_SIB
        } else {
            INV_DESC
        };
        prev_kind = kind;
        for cl in &c.classes {
            *inv.class.entry(cl.clone()).or_insert(0) |= kind;
        }
        if let Some(id) = &c.id {
            *inv.id.entry(id.clone()).or_insert(0) |= kind;
        }
        for a in &c.attrs {
            *inv.attr.entry(a.name.to_ascii_lowercase()).or_insert(0) |= kind;
        }
        for p in &c.pseudos {
            match p {
                Pseudo::Not(l) | Pseudo::Is(l) | Pseudo::Where(l) | Pseudo::Host(Some(l)) => {
                    for s in l {
                        collect_inv(s, kind, inv);
                    }
                }
                // Tested against preceding siblings too.
                Pseudo::NthChild(_, _, l) => {
                    inv.structural_fwd |= kind;
                    for s in l.iter().flatten() {
                        collect_inv(s, kind | INV_SIB, inv);
                    }
                }
                Pseudo::FirstChild | Pseudo::FirstOfType | Pseudo::NthOfType(..) => inv.structural_fwd |= kind,
                Pseudo::LastChild | Pseudo::LastOfType | Pseudo::NthLastChild(..) | Pseudo::NthLastOfType(..) => inv.structural_bwd |= kind,
                Pseudo::OnlyChild | Pseudo::OnlyOfType => {
                    inv.structural_fwd |= kind;
                    inv.structural_bwd |= kind;
                }
                Pseudo::Empty => inv.empty |= kind,
                // The host or its ancestors, for elements in its shadow tree.
                Pseudo::HostContext(l) => {
                    for s in l {
                        collect_inv(s, kind | INV_DESC, inv);
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(a) = &sel.shadow_arg {
        if let selector::ShadowArg::Slotted(l) = &**a {
            for s in l {
                collect_inv(s, INV_SELF, inv);
            }
        }
    }
}

/// Rules bucketed by their subject's most specific key (id, first class,
/// tag, first attribute name; the rest scanned for every element).
#[derive(Default)]
struct Buckets {
    by_id: FxMap<String, Vec<IndexEntry>>,
    by_class: FxMap<String, Vec<IndexEntry>>,
    by_tag: FxMap<String, Vec<IndexEntry>>,
    by_attr: FxMap<String, Vec<IndexEntry>>,
    rest: Vec<IndexEntry>,
    len: usize,
}

impl Buckets {
    fn insert(&mut self, subj: &selector::Compound, e: IndexEntry) {
        self.len += 1;
        if let Some(id) = &subj.id {
            self.by_id.entry(id.clone()).or_default().push(e);
        } else if let Some(c) = subj.classes.first() {
            self.by_class.entry(c.clone()).or_default().push(e);
        } else if let Some(t) = &subj.tag {
            self.by_tag.entry(t.clone()).or_default().push(e);
        } else if let Some(a) = subj.attrs.first().filter(|a| !a.name.contains('|')) {
            self.by_attr.entry(a.name.to_ascii_lowercase()).or_default().push(e);
        } else {
            self.rest.push(e);
        }
    }

    /// Entries that may match `el`, in index order.
    fn candidates<'s>(&'s self, el: &crate::dom::ElementData, out: &mut Vec<&'s IndexEntry>) {
        if self.len == 0 {
            return;
        }
        if let Some(l) = el.id.as_ref().and_then(|i| self.by_id.get(i)) {
            out.extend(l.iter());
        }
        for c in &el.classes {
            if let Some(l) = self.by_class.get(c) {
                out.extend(l.iter());
            }
        }
        let tag = if el.ns == crate::dom::Namespace::Html { self.by_tag.get(&el.name) } else { self.by_tag.get(&el.name.to_ascii_lowercase()) };
        if let Some(l) = tag {
            out.extend(l.iter());
        }
        if !self.by_attr.is_empty() {
            for a in &el.attrs {
                if let Some(l) = self.by_attr.get(&a.name.to_ascii_lowercase()) {
                    out.extend(l.iter());
                }
            }
        }
        out.extend(self.rest.iter());
    }
}

/// Inclusive Bloom filters (an element's ancestors and itself), memoized per node.
struct BloomMemo {
    blooms: Vec<Bloom>,
    done: Vec<bool>,
}

/// The Bloom filter of an element's DOM element ancestors.
fn ancestor_bloom(doc: &Document, node: NodeId, memo: &mut BloomMemo) -> Bloom {
    if memo.done.len() < doc.nodes.len() {
        memo.blooms.resize(doc.nodes.len(), Bloom::default());
        memo.done.resize(doc.nodes.len(), false);
    }
    let Some(parent) = doc.parent(node).filter(|&p| doc.is_element(p)) else {
        return Bloom::default();
    };
    // Collect the not-yet-computed chain, then fill it top-down.
    let mut chain = Vec::new();
    let mut cur = Some(parent);
    while let Some(c) = cur {
        if memo.done[c] {
            break;
        }
        chain.push(c);
        cur = doc.parent(c).filter(|&p| doc.is_element(p));
    }
    let mut b = cur.map(|c| memo.blooms[c]).unwrap_or_default();
    for &c in chain.iter().rev() {
        if let Some(el) = doc.element(c) {
            b.add_element(el);
        }
        memo.blooms[c] = b;
        memo.done[c] = true;
    }
    memo.blooms[parent]
}

/// The ancestor hashes a selector requires: ids, classes and tags of the
/// compounds joined to the right by a descendant or child combinator.
fn ancestor_hashes(sel: &Selector) -> [u32; 4] {
    let mut ids = Vec::new();
    let mut classes = Vec::new();
    let mut tags = Vec::new();
    for j in 1..sel.parts.len() {
        if !matches!(sel.combinators.get(j - 1), Some(selector::Combinator::Descendant) | Some(selector::Combinator::Child)) {
            continue;
        }
        let p = &sel.parts[j];
        if let Some(id) = &p.id {
            ids.push(key_hash(b'#', id));
        }
        for c in &p.classes {
            classes.push(key_hash(b'.', c));
        }
        if let Some(t) = &p.tag {
            if t != "*" && !t.contains('|') {
                tags.push(key_hash_lc(b'<', t));
            }
        }
    }
    let mut out = [0u32; 4];
    for (k, h) in ids.into_iter().chain(classes).chain(tags).take(4).enumerate() {
        out[k] = h;
    }
    out
}

/// Rules from all stylesheets, indexed by the subject's most specific key.
pub struct RuleIndex {
    sheets: Vec<Rc<Stylesheet>>,
    by_id: FxMap<String, Vec<IndexEntry>>,
    by_class: FxMap<String, Vec<IndexEntry>>,
    by_tag: FxMap<String, Vec<IndexEntry>>,
    /// Subjects with only attribute selectors, by (lowercase) attribute name.
    by_attr: FxMap<String, Vec<IndexEntry>>,
    universal: Vec<IndexEntry>,
    /// `:root` rules (only the root element can match them).
    root_rules: Vec<IndexEntry>,
    /// Pseudo-element rules (`::before`…), keyed like the others.
    pseudo: Buckets,
    /// Shadow-tree rules whose subject is `:host` (matched only against the host).
    host_rules: Vec<IndexEntry>,
    /// `:host::before` and the like.
    host_pseudo: Vec<IndexEntry>,
    /// `::slotted()` rules (matched against nodes assigned to this tree's slots).
    slotted: Vec<IndexEntry>,
    /// `::part()` rules (matched against elements in the shadow trees of hosts in this tree).
    part: Vec<IndexEntry>,
    pub has_hover_rules: bool,
    /// Some selector uses `:has()` (incremental restyles must check anchors).
    pub has_has: bool,
    /// A `:has()` argument nests a complex selector (`:is(.a .b)`), which can
    /// look outside the anchor's subtree: replay every logged anchor.
    pub has_complex: bool,
    /// `:empty` outside a top-level subject (a child-list change can affect
    /// the element's following siblings).
    pub empty_dep: bool,
    /// An attribute selector on `style` (inline-style changes are not local).
    pub attr_style: bool,
    /// Where selector features influence matching (incremental restyles).
    pub inv: Invalidation,
    /// Some rule depends on a container query (styles need container sizes).
    pub has_container_rules: bool,
    /// Registered custom properties (`@property`).
    pub registered: Rc<HashMap<String, Registered>>,
    /// `@counter-style` rules of all sheets (later sheets win).
    pub counter_styles: Rc<counters::CounterStyles>,
    /// `@keyframes` by name, in cascade order (the last matching one wins).
    pub keyframes: HashMap<String, Vec<(Rc<anim::Keyframes>, Vec<Rc<Vec<CV>>>)>>,
}

impl RuleIndex {
    pub fn new(sheets: Vec<Rc<Stylesheet>>) -> RuleIndex {
        let mut idx = RuleIndex {
            sheets,
            by_id: Default::default(),
            by_class: Default::default(),
            by_tag: Default::default(),
            by_attr: Default::default(),
            universal: Vec::new(),
            root_rules: Vec::new(),
            pseudo: Buckets::default(),
            host_rules: Vec::new(),
            host_pseudo: Vec::new(),
            slotted: Vec::new(),
            part: Vec::new(),
            has_hover_rules: false,
            has_has: false,
            has_complex: false,
            empty_dep: false,
            attr_style: false,
            inv: Invalidation::default(),
            has_container_rules: false,
            registered: Rc::new(HashMap::new()),
            counter_styles: Rc::new(HashMap::new()),
            keyframes: HashMap::new(),
        };
        for sh in &idx.sheets {
            for (name, kf, media) in &sh.keyframes {
                idx.keyframes.entry(name.clone()).or_default().push((kf.clone(), media.clone()));
            }
        }
        let mut cs_rules: Vec<counters::CounterStyleRule> = Vec::new();
        for sh in &idx.sheets {
            for r in &sh.counter_styles {
                cs_rules.retain(|x| x.name != r.name);
                cs_rules.push(r.clone());
            }
        }
        if !cs_rules.is_empty() {
            idx.counter_styles = Rc::new(counters::build(&cs_rules));
        }
        let mut reg = HashMap::new();
        for sh in &idx.sheets {
            for (n, inh, init, syn) in &sh.properties {
                reg.entry(n.clone()).or_insert((*inh, init.clone(), syn.clone()));
            }
        }
        idx.registered = Rc::new(reg);
        let layer_rank = layer_ranks(&idx.sheets);
        let mut order = 0u32;
        for (si, sheet) in idx.sheets.iter().enumerate() {
            for (ri, rule) in sheet.rules.iter().enumerate() {
                let layer = rule.layer.as_ref().and_then(|l| layer_rank.get(&**l).copied()).unwrap_or(u16::MAX);
                // (Container query units also depend on container sizes.)
                if !rule.containers.is_empty() || (!idx.has_container_rules && rule.decls.iter().any(|d| uses_cq_units(&d.value))) {
                    idx.has_container_rules = true;
                }
                for (sel_i, sel) in rule.selectors.iter().enumerate() {
                    order += 1;
                    let e = IndexEntry { sheet: si as u32, rule: ri as u32, sel: sel_i as u16, spec: sel.specificity, order, layer, anc: ancestor_hashes(sel) };
                    if sel.parts.iter().any(|p| p.pseudos.iter().any(|ps| matches!(ps, selector::Pseudo::Hover | selector::Pseudo::Active))) {
                        idx.has_hover_rules = true;
                    }
                    let (mut hh, mut ed, mut ast, mut hc) = (false, false, false, false);
                    selector::visit_compounds(sel, true, &mut |c, top| {
                        for p in &c.pseudos {
                            match p {
                                selector::Pseudo::Has(rel) => {
                                    hh = true;
                                    for (_, rs) in rel {
                                        selector::visit_compounds(rs, false, &mut |rc, _| {
                                            for rp in &rc.pseudos {
                                                if let selector::Pseudo::Not(l) | selector::Pseudo::Is(l) | selector::Pseudo::Where(l) | selector::Pseudo::NthChild(_, _, Some(l)) = rp {
                                                    if l.iter().any(|x| x.parts.len() > 1) {
                                                        hc = true;
                                                    }
                                                }
                                            }
                                        });
                                    }
                                }
                                selector::Pseudo::Empty if !top => ed = true,
                                _ => {}
                            }
                        }
                        if c.attrs.iter().any(|a| a.name.eq_ignore_ascii_case("style")) {
                            ast = true;
                        }
                    });
                    idx.has_has |= hh;
                    idx.has_complex |= hc;
                    idx.empty_dep |= ed;
                    idx.attr_style |= ast;
                    let top = if matches!(sel.pseudo_element, Some(PseudoElement::Part) | Some(PseudoElement::Slotted)) { INV_SELF | INV_DESC } else { INV_SELF };
                    collect_inv(sel, top, &mut idx.inv);
                    let subj = sel.subject();
                    let host_subject = subj.pseudos.iter().any(|p| matches!(p, selector::Pseudo::Host(_) | selector::Pseudo::HostContext(_)));
                    match sel.pseudo_element {
                        Some(PseudoElement::Slotted) => {
                            idx.slotted.push(e);
                            continue;
                        }
                        Some(PseudoElement::Part) => {
                            idx.part.push(e);
                            continue;
                        }
                        Some(_) if host_subject => {
                            idx.host_pseudo.push(e);
                            continue;
                        }
                        Some(_) => {
                            idx.pseudo.insert(subj, e);
                            continue;
                        }
                        None => {}
                    }
                    if host_subject {
                        idx.host_rules.push(e);
                        continue;
                    }
                    // A subject without its own id/class/tag/attribute may still
                    // require one through a one-alternative :is()/:where().
                    let inner = if subj.id.is_none() && subj.classes.is_empty() && subj.tag.is_none() && subj.attrs.is_empty() {
                        subj.pseudos.iter().find_map(|p| match p {
                            selector::Pseudo::Is(l) | selector::Pseudo::Where(l) if l.len() == 1 && l[0].parts.len() == 1 && l[0].pseudo_element.is_none() => {
                                let c = &l[0].parts[0];
                                (c.id.is_some() || !c.classes.is_empty() || c.tag.is_some() || c.attrs.iter().any(|a| !a.name.contains('|'))).then_some(c)
                            }
                            _ => None,
                        })
                    } else {
                        None
                    };
                    let key = inner.unwrap_or(subj);
                    if let Some(id) = &key.id {
                        idx.by_id.entry(id.clone()).or_default().push(e);
                    } else if let Some(c) = key.classes.first() {
                        idx.by_class.entry(c.clone()).or_default().push(e);
                    } else if let Some(t) = &key.tag {
                        idx.by_tag.entry(t.clone()).or_default().push(e);
                    } else if let Some(a) = key.attrs.first().filter(|a| !a.name.contains('|')) {
                        idx.by_attr.entry(a.name.to_ascii_lowercase()).or_default().push(e);
                    } else if subj.pseudos.iter().any(|p| matches!(p, selector::Pseudo::Root)) {
                        idx.root_rules.push(e);
                    } else {
                        if std::env::var("OHB_STYLE_STATS").as_deref() == Ok("2") {
                            eprintln!("[universal] sheet={} {:?} | comb={:?} | anc={:?}", si, subj, sel.combinators, sel.parts.get(1).map(|p| (&p.tag, &p.classes, &p.id)));
                        }
                        idx.universal.push(e);
                    }
                }
            }
        }
        idx
    }

    pub fn universal_len(&self) -> usize {
        self.universal.len()
    }

    pub fn pseudo_len(&self) -> usize {
        self.pseudo.len
    }

    /// The `@keyframes` rule named `name` that applies in `env`.
    pub fn keyframes_for(&self, name: &str, env: &MediaEnv) -> Option<Rc<anim::Keyframes>> {
        self.keyframes.get(name)?.iter().rev().find(|(_, m)| m.iter().all(|q| media_matches(q, env))).map(|(k, _)| k.clone())
    }

    pub fn has_keyframes(&self) -> bool {
        !self.keyframes.is_empty()
    }

    pub fn font_faces(&self) -> Vec<FontFace> {
        self.sheets.iter().flat_map(|s| s.font_faces.iter().cloned()).collect()
    }
}

/// Precedence rank of every cascade layer across all sheets (CSS Cascade 5
/// §6.4): layers order by first declaration, sub-layers before their parent's
/// own rules; unlayered rules (u16::MAX) beat all layers.
fn layer_ranks(sheets: &[Rc<Stylesheet>]) -> HashMap<String, u16> {
    let mut names: Vec<String> = Vec::new();
    for sh in sheets {
        for l in &sh.layer_order {
            if !names.iter().any(|n| **n == **l) {
                names.push(l.to_string());
            }
        }
    }
    if names.is_empty() {
        return HashMap::new();
    }
    // Sibling index of each path component, in order of first appearance.
    let mut child_idx: HashMap<String, usize> = HashMap::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for n in &names {
        let parent = n.rsplit_once('.').map(|(p, _)| p.to_string()).unwrap_or_default();
        let c = counts.entry(parent).or_insert(0);
        child_idx.insert(n.clone(), *c);
        *c += 1;
    }
    let key = |n: &str| -> Vec<usize> {
        let mut k = Vec::new();
        let mut acc = String::new();
        for part in n.split('.') {
            if !acc.is_empty() {
                acc.push('.');
            }
            acc.push_str(part);
            k.push(child_idx.get(&acc).copied().unwrap_or(0));
        }
        // A layer's own rules come after its sub-layers.
        k.push(usize::MAX);
        k
    };
    let mut sorted: Vec<(Vec<usize>, String)> = names.iter().map(|n| (key(n), n.clone())).collect();
    sorted.sort();
    sorted.into_iter().enumerate().map(|(i, (_, n))| (n, i.min(u16::MAX as usize - 1) as u16)).collect()
}

/// Does `node` fall inside `scope` (CSS Cascade 6 §2.5)? Returns the nearest scoping root.
fn scope_root(scope: &Scope, doc: &Document, node: NodeId) -> Option<NodeId> {
    let st = MatchState { hover_chain: &[], focus: doc.focused, active_chain: &[], target: None, scope: None, host: None, shadow_root: None };
    let mut cur = Some(node);
    let mut path: Vec<NodeId> = Vec::new();
    while let Some(c) = cur {
        if doc.is_element(c) {
            path.push(c);
            if scope.root.iter().any(|s| selector::matches(s, doc, c, &st)) {
                // The node must not be at or below a scoping limit inside this root.
                let limited = !scope.limit.is_empty()
                    && path.iter().any(|&p| {
                        let lst = MatchState { hover_chain: &[], focus: doc.focused, active_chain: &[], target: None, scope: Some(c), host: None, shadow_root: None };
                        scope.limit.iter().any(|s| selector::matches(s, doc, p, &lst))
                    });
                if !limited {
                    return Some(c);
                }
            }
        }
        cur = doc.parent(c);
    }
    None
}

/// Result of styling a document.
pub struct Styles {
    pub map: Vec<Option<Rc<ComputedStyle>>>,
    pub before: HashMap<NodeId, Rc<ComputedStyle>>,
    pub after: HashMap<NodeId, Rc<ComputedStyle>>,
    pub placeholder: HashMap<NodeId, Rc<ComputedStyle>>,
    /// `::marker` styles of list items that have `::marker` rules.
    pub marker: HashMap<NodeId, Rc<ComputedStyle>>,
    /// `::first-letter` styles of block containers.
    pub first_letter: HashMap<NodeId, Rc<ComputedStyle>>,
    /// `::first-line` styles of block containers.
    pub first_line: HashMap<NodeId, Rc<ComputedStyle>>,
    /// `::backdrop` styles of top-layer elements.
    pub backdrop: HashMap<NodeId, Rc<ComputedStyle>>,
    pub counter_styles: Rc<counters::CounterStyles>,
    pub root_bg: Color,
}

impl Styles {
    pub fn get(&self, n: NodeId) -> Option<&Rc<ComputedStyle>> {
        self.map.get(n).and_then(|s| s.as_ref())
    }
}

thread_local! {
    /// When set, a style pass only descends into these nodes (see `push_kids`).
    pub static STYLE_PATH: std::cell::RefCell<Option<std::collections::HashSet<NodeId>>> = const { std::cell::RefCell::new(None) };
}

pub struct StyleEngine<'a> {
    pub doc: &'a Document,
    pub index: &'a RuleIndex,
    pub env: MediaEnv,
    pub hover_chain: Vec<NodeId>,
    pub active_chain: Vec<NodeId>,
    pub target: Option<NodeId>,
    pub inline_cache: &'a mut HashMap<String, Rc<Vec<Declaration>>>,
    /// Document node to style (DOCUMENT, or a sub-document in the same arena).
    pub root: NodeId,
    /// Also style descendants of `display: none` elements (getComputedStyle needs them).
    pub descend_hidden: bool,
    /// Style only the children of this node, given its computed style and the root font size.
    pub seed: Option<(NodeId, Rc<ComputedStyle>, f32)>,
    /// Content-box sizes of query containers from the previous layout.
    pub container_sizes: Option<&'a HashMap<NodeId, (f32, f32)>>,
    /// Animations and transitions (the page's engine); `None` styles statically.
    pub anim: Option<&'a mut anim::AnimEngine>,
    /// The previous pass's styles (before-change styles for transitions).
    pub prev: Option<&'a Styles>,
    /// Rule indexes of shadow trees, by shadow root.
    pub shadow_indexes: Option<&'a HashMap<NodeId, Rc<RuleIndex>>>,
    /// Incremental restyle: the nodes to restyle (`DIRTY_*` flags); every
    /// other element keeps its style from `prev`.
    pub dirty: Option<&'a HashMap<NodeId, u8>>,
}

/// After an incremental `compute`: did any restyled element's style (or
/// pseudo-element styles) differ from the previous pass?
pub fn styles_changed() -> bool {
    STYLE_CHANGED.with(|c| c.get())
}

thread_local! {
    static STYLE_CHANGED: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// Incremental restyle flags: restyle the node's following siblings and
/// their subtrees; the node (and its subtree only if its style changed); the
/// node and its subtree.
pub const DIRTY_SIBLINGS: u8 = 1;
pub const DIRTY_SELF: u8 = 2;
pub const DIRTY_SUBTREE: u8 = 4;

/// Where an element carrying a selector feature (class, id, attribute)
/// influences matching: itself, its descendants, its following siblings.
pub const INV_SELF: u8 = 1;
pub const INV_DESC: u8 = 2;
pub const INV_SIB: u8 = 4;

/// Invalidation summary of a rule index: `INV_*` relations by class, id and
/// (lowercase) attribute name; of structural pseudo-classes that depend on
/// preceding siblings (`:nth-child`…) and on following ones (`:last-child`…);
/// of `:empty`; and of the compounds right of sibling combinators.
#[derive(Default)]
pub struct Invalidation {
    pub class: HashMap<String, u8>,
    pub id: HashMap<String, u8>,
    pub attr: HashMap<String, u8>,
    pub structural_fwd: u8,
    pub structural_bwd: u8,
    pub empty: u8,
    pub sibcomb: u8,
}

/// Cascade ranks: the UA, then the author rules of an inner tree (a shadow
/// tree's `:host` / `::slotted()` rules), presentational hints, the element's
/// own tree, an outer tree (`::part()`), and the style attribute.
const RANK_UA: u8 = 0;
const RANK_USER: u8 = 1;
const RANK_INNER: u8 = 2;
const RANK_HINTS: u8 = 3;
const RANK_AUTHOR: u8 = 4;
const RANK_OUTER: u8 = 5;
const RANK_INLINE: u8 = 6;

/// Precedence of `!important` declarations by rank: inner trees win over
/// outer ones, user important over all author ones, and UA important wins
/// over everything.
fn important_rank(r: u8) -> u8 {
    match r {
        RANK_UA => 12,
        RANK_USER => 11,
        RANK_INNER => 10,
        RANK_OUTER => 1,
        r => r,
    }
}

/// The cascade rank of a rule from `origin`, `author` for author sheets.
fn origin_rank(origin: Origin, author: u8) -> u8 {
    match origin {
        Origin::UserAgent => RANK_UA,
        Origin::User => RANK_USER,
        Origin::Author => author,
    }
}

/// Animation keys of `::before` / `::after` boxes.
pub const ANIM_BEFORE: NodeId = 1 << 40;
pub const ANIM_AFTER: NodeId = 1 << 41;

struct Matched<'r> {
    rank: u8,
    layer: u16,
    spec: u32,
    order: u32,
    decls: &'r Rc<Vec<Declaration>>,
}

fn var_refs<'a>(v: &'a [CV], out: &mut Vec<&'a str>) {
    for c in v {
        match c {
            CV::Func(n, args) => {
                if n == "var" {
                    if let Some(name) = args.iter().find_map(|a| a.ident()) {
                        out.push(name);
                    }
                }
                var_refs(args, out);
            }
            CV::Block(_, args) => var_refs(args, out),
            _ => {}
        }
    }
}

fn resolve_custom<'a>(name: &'a str, winners: &HashMap<&'a str, &'a Declaration>, map: &mut CustomOverlay, state: &mut HashMap<&'a str, u8>) {
    match state.get(name) {
        Some(2) => return,
        Some(1) => {
            // Dependency cycle: the property becomes guaranteed-invalid.
            map.remove(name);
            return;
        }
        _ => {}
    }
    let d = match winners.get(name) {
        Some(d) if d.has_var => *d,
        _ => return,
    };
    state.insert(name, 1);
    let mut deps = Vec::new();
    var_refs(&d.value, &mut deps);
    for dep in deps {
        if winners.get(dep).map(|w| w.has_var).unwrap_or(false) {
            resolve_custom(dep, winners, map, state);
        }
    }
    if state.get(name) == Some(&1) {
        match substitute(&d.value, map, 0) {
            Some(v) => {
                map.insert(name.to_string(), Rc::new(v));
            }
            None => {
                map.remove(name);
            }
        }
    }
    state.insert(name, 2);
}

/// Where var() looks custom properties up.
pub trait VarLookup {
    fn lookup(&self, name: &str) -> Option<&Rc<Vec<CV>>>;
}

impl VarLookup for style::CustomMap {
    fn lookup(&self, name: &str) -> Option<&Rc<Vec<CV>>> {
        self.get(name)
    }
}

impl VarLookup for Rc<style::CustomMap> {
    fn lookup(&self, name: &str) -> Option<&Rc<Vec<CV>>> {
        self.get(name)
    }
}

/// An element's custom properties while its own declarations resolve: its
/// changes over the inherited map.
struct CustomOverlay<'a> {
    base: &'a style::CustomMap,
    changes: style::CustomChanges,
}

impl CustomOverlay<'_> {
    fn insert(&mut self, k: String, v: Rc<Vec<CV>>) {
        self.changes.insert(k, Some(v));
    }
    fn remove(&mut self, k: &str) {
        if self.base.contains_key(k) {
            self.changes.insert(k.to_string(), None);
        } else {
            self.changes.remove(k);
        }
    }
}

impl VarLookup for CustomOverlay<'_> {
    fn lookup(&self, name: &str) -> Option<&Rc<Vec<CV>>> {
        match self.changes.get(name) {
            Some(v) => v.as_ref(),
            None => self.base.get(name),
        }
    }
}

fn substitute<M: VarLookup + ?Sized>(v: &[CV], custom: &M, depth: u32) -> Option<Vec<CV>> {
    if depth > 16 {
        return None;
    }
    let mut out = Vec::with_capacity(v.len());
    for c in v {
        match c {
            CV::Func(n, args) if n == "var" => {
                let parts = split_commas(args);
                let name = words(parts[0]).first()?.ident()?.to_string();
                match custom.lookup(&name) {
                    // An empty value is valid and substitutes nothing
                    // (Tailwind's `--tw-ring-inset: ;` idiom).
                    Some(val) => out.extend(substitute(val, custom, depth + 1)?),
                    None => {
                        // The fallback is everything after the first comma,
                        // and may itself be empty: `var(--x,)`.
                        let pos = args.iter().position(|c| c.is_comma())?;
                        out.extend(substitute(trim(&args[pos + 1..]), custom, depth + 1)?);
                    }
                }
            }
            CV::Func(n, args) if n == "env" => {
                let pos = args.iter().position(|c| c.is_comma());
                match pos {
                    Some(p) => out.extend(substitute(trim(&args[p + 1..]), custom, depth + 1)?),
                    None => out.push(CV::T(Tok::Dimension(0.0, "px".into()))),
                }
            }
            CV::Func(n, args) => out.push(CV::Func(n.clone(), substitute(args, custom, depth)?)),
            CV::Block(b, args) => out.push(CV::Block(*b, substitute(args, custom, depth)?)),
            other => out.push(other.clone()),
        }
    }
    Some(out)
}

thread_local! {
    static UA_SHEET: Rc<Stylesheet> = Rc::new(parse_stylesheet(&ua_css(), &url::Url::parse("about:blank").unwrap(), Origin::UserAgent));
}

/// The UA stylesheet for this platform: Chrome draws form controls in the
/// system UI font on macOS and in Arial elsewhere.
fn ua_css() -> String {
    if cfg!(target_os = "macos") {
        UA_CSS.to_string()
    } else {
        UA_CSS.replace("font: 13.333px system-ui, sans-serif;", "font: 13.333px Arial, sans-serif;")
    }
}

pub fn ua_sheet() -> Rc<Stylesheet> {
    UA_SHEET.with(|s| s.clone())
}

/// Extra UA rules for quirks-mode documents (Chromium's quirks.css).
pub const QUIRKS_CSS: &str = r#"
table { white-space: normal; line-height: normal; font-weight: normal; font-size: medium; font-variant: normal; font-style: normal; text-align: start; }
img[align=left i] { margin-right: 3px; }
img[align=right i] { margin-left: 3px; }
input:not([type=image i]), textarea { box-sizing: border-box; }
form { margin-block-end: 1em; }
li:not(:defined) {}
"#;

thread_local! {
    static QUIRKS_SHEET: Rc<Stylesheet> = Rc::new(parse_stylesheet(QUIRKS_CSS, &url::Url::parse("about:blank").unwrap(), Origin::UserAgent));
}

pub fn quirks_sheet() -> Rc<Stylesheet> {
    QUIRKS_SHEET.with(|s| s.clone())
}

impl<'a> StyleEngine<'a> {
    pub fn compute(&mut self) -> Styles {
        let doc = self.doc;
        let mut styles = Styles {
            map: vec![None; doc.nodes.len()],
            before: HashMap::new(),
            after: HashMap::new(),
            placeholder: HashMap::new(),
            marker: HashMap::new(),
            first_letter: HashMap::new(),
            first_line: HashMap::new(),
            backdrop: HashMap::new(),
            counter_styles: self.index.counter_styles.clone(),
            root_bg: Color::WHITE,
        };
        let root_parent = Rc::new(ComputedStyle::initial());
        let mut share: HashMap<(usize, usize, Vec<u32>), Rc<ComputedStyle>> = HashMap::new();
        let st0 = MatchState {
            hover_chain: &self.hover_chain,
            focus: doc.focused,
            active_chain: &self.active_chain,
            target: self.target,
            scope: None,
            host: None,
            shadow_root: None,
        };
        let mut media_cache: HashMap<usize, bool> = HashMap::new();
        let mut bloom_memo = BloomMemo { blooms: Vec::new(), done: Vec::new() };
        let mut root_font = 16.0;
        // The rule index of a tree scope (the document, or a shadow root).
        let doc_root = self.root;
        let main_index: &'a RuleIndex = self.index;
        let shadow_indexes = self.shadow_indexes;
        let index_for = |scope: NodeId| -> Option<&'a RuleIndex> {
            if scope == doc_root {
                Some(main_index)
            } else {
                shadow_indexes.and_then(|m| m.get(&scope)).map(|r| &**r)
            }
        };
        // Iterative DFS over the flat tree:
        // (node, parent style, is_root, display of the layout parent — skips
        // display:contents, container chain, tree scope)
        // ... and the restyle state: 0 keep the previous style, 1 restyle the
        // element (its children only if its style changed), 2 restyle it and
        // its subtree.
        type Entry = (NodeId, Rc<ComputedStyle>, bool, Display, Option<Rc<CChain>>, NodeId, u8);
        let all_dirty = self.dirty.is_none() || self.prev.is_none();
        let dirty_map = if all_dirty { None } else { self.dirty };
        let mut changed = all_dirty;
        let flags_of = |n: NodeId| dirty_map.and_then(|m| m.get(&n).copied()).unwrap_or(0);
        // The states of a child list (a DIRTY_SIBLINGS node fully dirties the
        // siblings after it).
        // Styling only the way to some elements (getComputedStyle in a
        // display:none subtree): other children are skipped.
        let path: Option<std::collections::HashSet<NodeId>> = STYLE_PATH.with(|p| p.borrow().clone());
        let push_kids = |stack: &mut Vec<Entry>, kids: &[NodeId], parent: &Rc<ComputedStyle>, lp: Display, chain: &Option<Rc<CChain>>, scope: NodeId, base: u8, is_root: bool| {
            let filtered: Vec<NodeId>;
            let kids = match &path {
                Some(p) => {
                    filtered = kids.iter().copied().filter(|k| p.contains(k)).collect();
                    &filtered[..]
                }
                None => kids,
            };
            let mut states: Vec<u8> = Vec::with_capacity(kids.len());
            let mut sib = false;
            for &c in kids {
                let f = flags_of(c);
                states.push(if base == 2 || sib || f & DIRTY_SUBTREE != 0 {
                    2
                } else if f & DIRTY_SELF != 0 {
                    1
                } else {
                    0
                });
                if f & DIRTY_SIBLINGS != 0 {
                    sib = true;
                }
            }
            for (i, &c) in kids.iter().enumerate().rev() {
                stack.push((c, parent.clone(), is_root, lp, chain.clone(), scope, states[i]));
            }
        };
        let mut stack: Vec<Entry> = Vec::new();
        if let Some((seed, ps, rf)) = self.seed.clone() {
            root_font = rf;
            // The seed's flat-tree children (as in the main loop below).
            let scope = doc.root_of(seed);
            let in_shadow = doc.is_shadow_root(scope);
            let (kids, kscope): (Vec<NodeId>, NodeId) = if let Some(&sr) = doc.shadow_roots.get(&seed) {
                (doc.children(sr).to_vec(), sr)
            } else if in_shadow && doc.is_tag(seed, "slot") && doc.slot_map().assigned.contains_key(&seed) {
                let hscope = doc.host_of(scope).map(|h| doc.root_of(h)).unwrap_or(doc_root);
                (doc.slot_map().assigned.get(&seed).cloned().unwrap_or_default(), hscope)
            } else {
                (doc.children(seed).to_vec(), scope)
            };
            for &c in kids.iter().rev() {
                if path.as_ref().is_some_and(|p| !p.contains(&c)) {
                    continue;
                }
                stack.push((c, ps.clone(), false, ps.display, None, kscope, 2));
            }
        } else {
            let base = if all_dirty || flags_of(self.root) & DIRTY_SUBTREE != 0 { 2 } else { 0 };
            push_kids(&mut stack, doc.children(self.root), &root_parent, Display::Block, &None, doc_root, base, true);
        }
        while let Some((node, parent, is_root, layout_parent, chain, scope, state_in)) = stack.pop() {
            let cq = CqCtx { chain: chain.as_ref(), parent: &parent };
            // Rules of this node's own tree (UA rules only for a shadow tree without styles).
            let Some(idx) = index_for(scope) else { continue };
            let st = if scope == doc_root || !doc.is_shadow_root(scope) { MatchState { ..st0 } } else { MatchState { host: doc.host_of(scope), shadow_root: Some(scope), ..st0 } };
            let el = match doc.element(node) {
                Some(e) => e,
                None => {
                    if let NodeData::Text(_) = doc.node(node).data {
                        styles.map[node] = Some(parent.clone());
                    }
                    continue;
                }
            };
            // Incremental restyle: an element outside every dirty subtree keeps
            // its previous style (its ancestors did too, so inheritance holds),
            // unless an animation or transition needs to see it this pass.
            let mut state = state_in;
            let old: Option<Rc<ComputedStyle>> = if all_dirty { None } else { self.prev.and_then(|p| p.map.get(node)).and_then(|s| s.clone()) };
            if state < 2 {
                let animated = match self.anim.as_deref_mut() {
                    Some(a) => {
                        let keys = [node, node | ANIM_BEFORE, node | ANIM_AFTER];
                        let tracked = keys.iter().any(|&k| a.tracks(k));
                        // Settled animations (finished with a fill, paused…)
                        // don't change: an untouched element keeps its style.
                        tracked && (old.is_none() || state != 0 || !keys.iter().all(|&k| a.keep_if_idle(k)))
                    }
                    None => false,
                };
                if old.is_none() || animated {
                    static WHY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                    if *WHY.get_or_init(|| std::env::var("OHB_STYLE_STATS").as_deref() == Ok("4")) {
                        eprintln!("[style-why] {} <{} class={:?}> old_none={} animated={} state_in={} {}", node, el.name, el.attr("class").map(|c| c.chars().take(40).collect::<String>()), old.is_none(), animated, state_in, self.anim.as_deref().map(|a| a.describe(node)).unwrap_or_default());
                    }
                    state = 2;
                }
            }
            let reuse = if state == 0 { old.clone() } else { None };
            let computed = if let Some(s) = reuse {
                for (dst, src) in [
                    (&mut styles.before, &self.prev.unwrap().before),
                    (&mut styles.after, &self.prev.unwrap().after),
                    (&mut styles.placeholder, &self.prev.unwrap().placeholder),
                    (&mut styles.marker, &self.prev.unwrap().marker),
                    (&mut styles.first_letter, &self.prev.unwrap().first_letter),
                    (&mut styles.first_line, &self.prev.unwrap().first_line),
                    (&mut styles.backdrop, &self.prev.unwrap().backdrop),
                ] {
                    if let Some(v) = src.get(&node) {
                        dst.insert(node, v.clone());
                    }
                }
                if is_root {
                    root_font = s.font_size;
                }
                s
            } else {
            let hints = presentational_hints(doc, node);
            let inline: Option<Rc<Vec<Declaration>>> = el.attr("style").filter(|s| !s.trim().is_empty()).map(|s| {
                if let Some(d) = self.inline_cache.get(s) {
                    d.clone()
                } else {
                    // Values set through CSSOM keep their full precision.
                    let d = match pv::block::precise_text(s) {
                        Some(p) => Rc::new(parse_declarations_in(&p, Some(&doc.base_url))),
                        None => Rc::new(parse_declarations_in(s, Some(&doc.base_url))),
                    };
                    if self.inline_cache.len() < 20000 {
                        self.inline_cache.insert(s.to_string(), d.clone());
                    }
                    d
                }
            });
            let hints_rc = if hints.is_empty() { None } else { Some(Rc::new(hints)) };
            // Gather matching rules.
            let mut matched: Vec<Matched> = Vec::new();
            let mut pseudo_matched: Vec<(PseudoElement, Matched)> = Vec::new();
            let env = self.env;
            // Rules from other trees: the element's own shadow tree (`:host`),
            // the trees of the slots it renders in (`::slotted()`), and the
            // tree of the host whose shadow tree it is in (`::part()`).
            let mut cross_tree = false;
            if !doc.shadow_roots.is_empty() {
                if let Some(&sr) = doc.shadow_roots.get(&node) {
                    if let Some(si) = index_for(sr) {
                        let hst = MatchState { host: Some(node), shadow_root: Some(sr), ..st0 };
                        for e in &si.host_rules {
                            let before = matched.len();
                            consider_entry_ranked(si, e, &mut media_cache, &env, doc, node, &hst, &cq, &mut matched, RANK_INNER);
                            cross_tree |= matched.len() > before;
                        }
                        for e in &si.host_pseudo {
                            if !entry_applies(si, e, &mut media_cache, &env, &cq) {
                                continue;
                            }
                            let rule = &si.sheets[e.sheet as usize].rules[e.rule as usize];
                            let sel = &rule.selectors[e.sel as usize];
                            if selector::matches(sel, doc, node, &hst) {
                                let rank = origin_rank(si.sheets[e.sheet as usize].origin, RANK_INNER);
                                pseudo_matched.push((sel.pseudo_element.unwrap(), Matched { rank, layer: e.layer, spec: e.spec, order: e.order, decls: &rule.decls }));
                            }
                        }
                    }
                }
                let mut cur = node;
                let mut hops = 0;
                while let Some(slot) = doc.assigned_slot(cur) {
                    hops += 1;
                    if hops > 32 {
                        break;
                    }
                    if let Some(sr) = doc.containing_shadow_root(slot) {
                        if let Some(si) = index_for(sr) {
                            let sst = MatchState { host: doc.host_of(sr), shadow_root: Some(sr), ..st0 };
                            for e in &si.slotted {
                                if !entry_applies(si, e, &mut media_cache, &env, &cq) {
                                    continue;
                                }
                                let rule = &si.sheets[e.sheet as usize].rules[e.rule as usize];
                                let sel = &rule.selectors[e.sel as usize];
                                if selector::matches_slotted(sel, doc, node, slot, &sst) {
                                    let rank = origin_rank(si.sheets[e.sheet as usize].origin, RANK_INNER);
                                    matched.push(Matched { rank, layer: e.layer, spec: e.spec, order: e.order, decls: &rule.decls });
                                    cross_tree = true;
                                }
                            }
                        }
                    }
                    cur = slot;
                }
                if st.shadow_root.is_some() && el.has_attr("part") {
                    if let Some(host) = st.host {
                        let hscope = doc.root_of(host);
                        if let Some(hi) = index_for(hscope) {
                            let hst = if doc.is_shadow_root(hscope) { MatchState { host: doc.host_of(hscope), shadow_root: Some(hscope), ..st0 } } else { MatchState { ..st0 } };
                            for e in &hi.part {
                                if !entry_applies(hi, e, &mut media_cache, &env, &cq) {
                                    continue;
                                }
                                let rule = &hi.sheets[e.sheet as usize].rules[e.rule as usize];
                                let sel = &rule.selectors[e.sel as usize];
                                if selector::matches_part(sel, doc, node, host, &hst) {
                                    matched.push(Matched { rank: RANK_OUTER, layer: e.layer, spec: e.spec, order: e.order, decls: &rule.decls });
                                    cross_tree = true;
                                }
                            }
                        }
                    }
                }
            }
            let stats_on = STATS_ON.with(|v| *v);
            if stats_on {
                stat(0, 1);
                stat(1, el.id.as_ref().and_then(|i| idx.by_id.get(i)).map(|l| l.len()).unwrap_or(0) as u64);
                stat(2, el.classes.iter().map(|c| idx.by_class.get(c).map(|l| l.len()).unwrap_or(0)).sum::<usize>() as u64);
                let tk = if el.ns == crate::dom::Namespace::Html { el.name.clone() } else { el.name.to_ascii_lowercase() };
                stat(3, idx.by_tag.get(&tk).map(|l| l.len()).unwrap_or(0) as u64);
                stat(4, idx.universal.len() as u64);
            }
            // Ancestor Bloom filter (document tree only; quirks mode matches
            // classes and ids case-insensitively).
            let anc_bloom = if scope == doc_root && !doc.quirks { Some(ancestor_bloom(doc, node, &mut bloom_memo)) } else { None };
            let rejected = |e: &IndexEntry| match &anc_bloom {
                Some(b) => e.anc[0] != 0 && e.anc.iter().any(|&h| h != 0 && !b.might_contain(h)),
                None => false,
            };
            if let Some(id) = &el.id {
                if let Some(list) = idx.by_id.get(id) {
                    for e in list {
                        if !rejected(e) {
                            consider_entry(idx, e, &mut media_cache, &env, doc, node, &st, &cq, &mut matched);
                        }
                    }
                }
            }
            for c in &el.classes {
                if let Some(list) = idx.by_class.get(c) {
                    for e in list {
                        if !rejected(e) {
                            consider_entry(idx, e, &mut media_cache, &env, doc, node, &st, &cq, &mut matched);
                        }
                    }
                }
            }
            let tag_key = if el.ns == crate::dom::Namespace::Html { el.name.clone() } else { el.name.to_ascii_lowercase() };
            if let Some(list) = idx.by_tag.get(&tag_key) {
                for e in list {
                    if !rejected(e) {
                        consider_entry(idx, e, &mut media_cache, &env, doc, node, &st, &cq, &mut matched);
                    }
                }
            }
            if !idx.by_attr.is_empty() {
                for a in &el.attrs {
                    let list = if a.name.bytes().any(|b| b.is_ascii_uppercase()) { idx.by_attr.get(&a.name.to_ascii_lowercase()) } else { idx.by_attr.get(&a.name) };
                    if let Some(list) = list {
                        stat(7, list.len() as u64);
                        for e in list {
                            if !rejected(e) {
                                consider_entry(idx, e, &mut media_cache, &env, doc, node, &st, &cq, &mut matched);
                            }
                        }
                    }
                }
            }
            for e in &idx.universal {
                if !rejected(e) {
                    consider_entry(idx, e, &mut media_cache, &env, doc, node, &st, &cq, &mut matched);
                }
            }
            if !idx.root_rules.is_empty() && doc.parent(node).map(|p| doc.is_document(p)).unwrap_or(false) {
                for e in &idx.root_rules {
                    consider_entry(idx, e, &mut media_cache, &env, doc, node, &st, &cq, &mut matched);
                }
            }
            // Pseudo-element rules (rarely many; scan linearly).
            let mut pcands: Vec<&IndexEntry> = Vec::new();
            idx.pseudo.candidates(el, &mut pcands);
            for e in pcands {
                if rejected(e) {
                    continue;
                }
                let sheet = &idx.sheets[e.sheet as usize];
                let rule = &sheet.rules[e.rule as usize];
                let sel = &rule.selectors[e.sel as usize];
                if !rule.media.iter().all(|m| *media_cache.entry(Rc::as_ptr(m) as usize).or_insert_with(|| media_matches(m, &self.env))) {
                    continue;
                }
                if !rule.containers.iter().all(|q| container_matches(q, cq.chain, cq.parent, &self.env)) {
                    continue;
                }
                if selector::matches(sel, doc, node, &st) {
                    let rank = origin_rank(sheet.origin, RANK_AUTHOR);
                    pseudo_matched.push((sel.pseudo_element.unwrap(), Matched { rank, layer: e.layer, spec: e.spec, order: e.order, decls: &rule.decls }));
                }
            }

            if let Some(h) = &hints_rc {
                matched.push(Matched { rank: RANK_HINTS, layer: u16::MAX, spec: 0, order: 0, decls: h });
            }
            if let Some(i) = &inline {
                matched.push(Matched { rank: RANK_INLINE, layer: u16::MAX, spec: 0, order: 0, decls: i });
            }
            matched.sort_by(|a, b| (a.rank, a.layer, a.spec, a.order).cmp(&(b.rank, b.layer, b.spec, b.order)));

            let shareable = hints_rc.is_none() && inline.is_none() && !is_root && !cross_tree;
            let share_key = if shareable {
                Some((Rc::as_ptr(&parent) as usize, idx as *const RuleIndex as usize, matched.iter().map(|m| m.order).chain(std::iter::once(u32::MAX - layout_parent as u32)).collect::<Vec<u32>>()))
            } else {
                None
            };
            let computed = match share_key.as_ref().and_then(|k| share.get(k)) {
                Some(s) => s.clone(),
                None => {
                    let cx = ApplyCtx { root_font_size: root_font, vw: self.env.width, vh: self.env.height, root_line_height: 0.0, cq: cq_size(chain.as_ref()) };
                    let mut s = cascade(&matched, &parent, &cx, &main_index.registered);
                    fixup(&mut s, &parent, is_root, el.name.as_str(), layout_parent);
                    if is_root {
                        root_font = s.font_size;
                    }
                    let rc = Rc::new(s);
                    if let Some(k) = share_key {
                        if share.len() < 50_000 {
                            share.insert(k, rc.clone());
                        }
                    }
                    rc
                }
            };
            // Animations and transitions turn the base style into the animated one.
            let computed = match self.anim.as_deref_mut() {
                Some(a) if a.has_work(node, &computed) => {
                    let prev = self.prev.and_then(|p| p.get(node)).map(|r| &**r);
                    let kf = |name: &str| idx.keyframes_for(name, &env);
                    let cx = ApplyCtx { root_font_size: root_font, vw: self.env.width, vh: self.env.height, root_line_height: 0.0, cq: cq_size(chain.as_ref()) };
                    let k = anim::KfCtx { parent: &parent, cx: &cx, keyframes: &kf };
                    a.remember(node, computed.clone(), parent.clone(), &cx);
                    match a.apply(node, &computed, prev, &k) {
                        Some(s) => Rc::new(s),
                        None => computed,
                    }
                }
                _ => computed,
            };
            // Pseudo-elements.
            if !pseudo_matched.is_empty() {
                for kind in [PseudoElement::Before, PseudoElement::After, PseudoElement::Placeholder, PseudoElement::Marker, PseudoElement::FirstLetter, PseudoElement::FirstLine, PseudoElement::Backdrop] {
                    if kind == PseudoElement::Backdrop && !doc.top_layer.contains(&node) {
                        continue;
                    }
                    let mut list: Vec<Matched> = pseudo_matched
                        .iter()
                        .filter(|(k, _)| *k == kind)
                        .map(|(_, m)| Matched { rank: m.rank, layer: m.layer, spec: m.spec, order: m.order, decls: m.decls })
                        .collect();
                    if list.is_empty() {
                        continue;
                    }
                    list.sort_by(|a, b| (a.rank, a.layer, a.spec, a.order).cmp(&(b.rank, b.layer, b.spec, b.order)));
                    let cx = ApplyCtx { root_font_size: root_font, vw: self.env.width, vh: self.env.height, root_line_height: 0.0, cq: cq_size(chain.as_ref()) };
                    let mut ps = cascade(&list, &computed, &cx, &main_index.registered);
                    match kind {
                        PseudoElement::Placeholder => {
                            styles.placeholder.insert(node, Rc::new(ps));
                        }
                        PseudoElement::Marker => {
                            styles.marker.insert(node, Rc::new(ps));
                        }
                        PseudoElement::FirstLetter => {
                            fixup(&mut ps, &computed, false, "", Display::Block);
                            styles.first_letter.insert(node, Rc::new(ps));
                        }
                        PseudoElement::FirstLine => {
                            styles.first_line.insert(node, Rc::new(ps));
                        }
                        PseudoElement::Backdrop => {
                            fixup(&mut ps, &computed, false, "", Display::Block);
                            styles.backdrop.insert(node, Rc::new(ps));
                        }
                        _ => {
                            if ps.content.is_some() && ps.display != Display::None {
                                fixup(&mut ps, &computed, false, "", if computed.display == Display::Contents { layout_parent } else { computed.display });
                                let key = node | if kind == PseudoElement::Before { ANIM_BEFORE } else { ANIM_AFTER };
                                if let Some(a) = self.anim.as_deref_mut() {
                                    if a.has_work(key, &ps) {
                                        let prev = self.prev.and_then(|p| if kind == PseudoElement::Before { p.before.get(&node) } else { p.after.get(&node) }).map(|r| &**r);
                                        let kf = |name: &str| idx.keyframes_for(name, &env);
                                        let k = anim::KfCtx { parent: &computed, cx: &cx, keyframes: &kf };
                                        a.remember(key, Rc::new(ps.clone()), computed.clone(), &cx);
                                        if let Some(s2) = a.apply(key, &ps, prev, &k) {
                                            ps = s2;
                                        }
                                    }
                                }
                                if kind == PseudoElement::Before {
                                    styles.before.insert(node, Rc::new(ps));
                                } else {
                                    styles.after.insert(node, Rc::new(ps));
                                }
                            }
                        }
                    }
                }
            }
            computed
            };
            // A restyled element whose style didn't change leaves its children alone.
            let (computed, kid_state) = match state {
                1 => match &old {
                    Some(o) if **o == *computed => (o.clone(), 0),
                    _ => (computed, 2),
                },
                s => (computed, s),
            };
            if state > 0 && !changed {
                let p = self.prev.unwrap();
                let same = |a: Option<&Rc<ComputedStyle>>, b: Option<&Rc<ComputedStyle>>| match (a, b) {
                    (Some(x), Some(y)) => Rc::ptr_eq(x, y) || **x == **y,
                    (None, None) => true,
                    _ => false,
                };
                changed = !(same(old.as_ref(), Some(&computed))
                    && same(p.before.get(&node), styles.before.get(&node))
                    && same(p.after.get(&node), styles.after.get(&node))
                    && same(p.marker.get(&node), styles.marker.get(&node))
                    && same(p.placeholder.get(&node), styles.placeholder.get(&node))
                    && same(p.first_letter.get(&node), styles.first_letter.get(&node))
                    && same(p.first_line.get(&node), styles.first_line.get(&node))
                    && same(p.backdrop.get(&node), styles.backdrop.get(&node)));
            }
            if is_root {
                styles.root_bg = computed.background_color.resolve(computed.color);
            }
            styles.map[node] = Some(computed.clone());
            if computed.display == Display::None && !self.descend_hidden {
                continue; // descendants are not rendered; skip styling them
            }
            // <details> distributes its children through UA shadow-DOM slots, so they
            // inherit from an anonymous slot (inherited properties only), as in Chrome.
            let child_lp = if computed.display == Display::Contents { layout_parent } else { computed.display };
            let child_parent = if el.name == "details" && el.ns == crate::dom::Namespace::Html { Rc::new(ComputedStyle::inherit(&computed)) } else { computed.clone() };
            // Containers (and named style containers) extend the chain for descendants.
            let child_chain = if computed.container_type != 0 || computed.container_names.is_some() {
                Some(Rc::new(CChain {
                    node,
                    names: computed.container_names.clone(),
                    ctype: computed.container_type,
                    size: self.container_sizes.and_then(|m| m.get(&node).copied()),
                    style: computed.clone(),
                    parent: chain.clone(),
                }))
            } else {
                chain.clone()
            };
            // Flat-tree children: a host's shadow tree; a slot's assigned nodes
            // (from the host's tree) or its fallback content; else DOM children.
            if let Some(&sr) = doc.shadow_roots.get(&node) {
                let base = if flags_of(sr) & DIRTY_SUBTREE != 0 { 2 } else { kid_state };
                push_kids(&mut stack, doc.children(sr), &child_parent, child_lp, &child_chain, sr, base, false);
                continue;
            }
            if st.shadow_root.is_some() && el.name == "slot" && el.ns == crate::dom::Namespace::Html {
                if let Some(assigned) = doc.slot_map().assigned.get(&node) {
                    let hscope = st.host.map(|h| doc.root_of(h)).unwrap_or(doc_root);
                    push_kids(&mut stack, assigned, &child_parent, child_lp, &child_chain, hscope, kid_state, false);
                    continue;
                }
            }
            push_kids(&mut stack, doc.children(node), &child_parent, child_lp, &child_chain, scope, kid_state, false);
        }
        STYLE_CHANGED.with(|c| c.set(changed));
        // Canvas background: root, or body if root is transparent.
        if styles.root_bg.a == 0 {
            if let Some(b) = doc.body() {
                if let Some(s) = styles.get(b) {
                    styles.root_bg = s.background_color.resolve(s.color);
                }
            }
        }
        styles
    }
}

/// Container-query context of the element being styled.
struct CqCtx<'a> {
    chain: Option<&'a Rc<CChain>>,
    parent: &'a ComputedStyle,
}

thread_local! {
    /// Debug counters (OHB_STYLE_STATS): elements, candidates per bucket, matches.
    pub static STYLE_STATS: std::cell::RefCell<[u64; 8]> = const { std::cell::RefCell::new([0; 8]) };
    static STATS_ON: bool = std::env::var_os("OHB_STYLE_STATS").is_some();
}
#[inline]
fn stat(i: usize, n: u64) {
    if STATS_ON.with(|v| *v) {
        STYLE_STATS.with(|s| s.borrow_mut()[i] += n);
    }
}

#[allow(clippy::too_many_arguments)]
/// Whether a declaration value uses container query length units.
fn uses_cq_units(v: &[CV]) -> bool {
    v.iter().any(|c| match c {
        CV::T(tokenizer::Tok::Dimension(_, u)) => u.len() >= 3 && u.as_bytes()[..2].eq_ignore_ascii_case(b"cq"),
        CV::Func(_, a) | CV::Block(_, a) => uses_cq_units(a),
        _ => false,
    })
}

fn consider_entry<'r>(idx: &'r RuleIndex, e: &IndexEntry, media_cache: &mut HashMap<usize, bool>, env: &MediaEnv, doc: &Document, node: NodeId, st: &MatchState, cq: &CqCtx, matched: &mut Vec<Matched<'r>>) {
    consider_entry_ranked(idx, e, media_cache, env, doc, node, st, cq, matched, RANK_AUTHOR)
}

/// Does the media / container condition of an indexed rule hold?
fn entry_applies(idx: &RuleIndex, e: &IndexEntry, media_cache: &mut HashMap<usize, bool>, env: &MediaEnv, cq: &CqCtx) -> bool {
    let rule = &idx.sheets[e.sheet as usize].rules[e.rule as usize];
    rule.media.iter().all(|m| *media_cache.entry(Rc::as_ptr(m) as usize).or_insert_with(|| media_matches(m, env))) && rule.containers.iter().all(|q| container_matches(q, cq.chain, cq.parent, env))
}

#[allow(clippy::too_many_arguments)]
fn consider_entry_ranked<'r>(idx: &'r RuleIndex, e: &IndexEntry, media_cache: &mut HashMap<usize, bool>, env: &MediaEnv, doc: &Document, node: NodeId, st: &MatchState, cq: &CqCtx, matched: &mut Vec<Matched<'r>>, author_rank: u8) {
    let sheet = &idx.sheets[e.sheet as usize];
    let rule = &sheet.rules[e.rule as usize];
    for m in &rule.media {
        let key = Rc::as_ptr(m) as usize;
        let ok = *media_cache.entry(key).or_insert_with(|| media_matches(m, env));
        if !ok {
            return;
        }
    }
    if !rule.containers.iter().all(|q| container_matches(q, cq.chain, cq.parent, env)) {
        return;
    }
    let sel = &rule.selectors[e.sel as usize];
    stat(6, 1);
    let ok = match &rule.scope {
        None => selector::matches(sel, doc, node, st),
        Some(scope) => match scope_root(scope, doc, node) {
            Some(root) => {
                let sst = MatchState { scope: Some(root), ..*st };
                selector::matches(sel, doc, node, &sst)
            }
            None => false,
        },
    };
    if ok {
        stat(5, 1);
        let rank = origin_rank(sheet.origin, author_rank);
        matched.push(Matched { rank, layer: e.layer, spec: e.spec, order: e.order, decls: &rule.decls });
    }
}

/// A registered custom property: (inherits, initial value, syntax other than `*`).
pub type Registered = (bool, Option<Rc<Vec<CV>>>, Option<Rc<str>>);

fn cascade(matched: &[Matched], parent: &ComputedStyle, cx: &ApplyCtx, reg: &HashMap<String, Registered>) -> ComputedStyle {
    let mut s = ComputedStyle::inherit(parent);
    // Registered custom properties: non-inherited ones start from their initial
    // value; inherited ones without a value get it too (CSS Properties & Values §2).
    if !reg.is_empty() {
        let mut changes: style::CustomChanges = Default::default();
        for (name, (inherits, init, _)) in reg.iter() {
            let has = s.custom.contains_key(name);
            if !*inherits || !has {
                match init {
                    Some(v) => {
                        if s.custom.get(name) != Some(v) {
                            changes.insert(name.clone(), Some(v.clone()));
                        }
                    }
                    None => {
                        if has {
                            changes.insert(name.clone(), None);
                        }
                    }
                }
            }
        }
        if !changes.is_empty() {
            s.custom = style::CustomMap::derive(&s.custom, changes);
        }
    }
    // Order: normal declarations by (rank, spec, order), then important ones
    // (author important < inline important < UA important).
    let mut ordered: Vec<&Declaration> = Vec::new();
    for m in matched {
        for d in m.decls.iter() {
            if !d.important {
                ordered.push(d);
            }
        }
    }
    let mut imp: Vec<(u8, u16, &Declaration)> = Vec::new();
    for m in matched {
        for d in m.decls.iter() {
            if d.important {
                let r = important_rank(m.rank);
                // Important declarations in earlier layers win (layer order reverses).
                imp.push((r, u16::MAX - m.layer, d));
            }
        }
    }
    imp.sort_by_key(|(r, l, _)| (*r, *l));
    ordered.extend(imp.into_iter().map(|(_, _, d)| d));

    // `revert` in an author (or user) declaration rolls the property back to
    // the user-agent (and user) origins: it becomes `unset` followed by those
    // origins' declarations of the property, in cascade order.
    // `revert` rolls a property back to the user-agent (and user) origins;
    // `revert-layer` to the cascade layers (and origins) below its own. The
    // declaration becomes `unset` followed by those declarations of the
    // property, in cascade order.
    let rev_kind = |d: &Declaration| -> u8 {
        if d.value.len() > 3 {
            0
        } else if style::ident_is(&d.value, "revert") {
            1
        } else if style::ident_is(&d.value, "revert-layer") {
            2
        } else {
            0
        }
    };
    let unsets: Vec<Declaration> = ordered
        .iter()
        .filter(|d| rev_kind(d) != 0)
        .map(|d| Declaration { name: d.name.clone(), value: Rc::new(vec![CV::T(Tok::Ident("unset".into()))]), important: false, has_var: false })
        .collect();
    if !unsets.is_empty() {
        let mut pos: HashMap<*const Declaration, (u8, u16)> = HashMap::new();
        for m in matched {
            for d in m.decls.iter() {
                pos.insert(d as *const Declaration, (m.rank, m.layer));
            }
        }
        let all_normal: Vec<(&Declaration, (u8, u16))> = matched.iter().flat_map(|m| m.decls.iter().filter(|d| !d.important).map(move |d| (d, (m.rank, m.layer)))).collect();
        let affects = |l: &str, p: &str| -> bool {
            if p == "all" || l == "all" || l == p {
                return true;
            }
            let ll = pv::shorthand::longhands(l).unwrap_or_default();
            let pl = pv::shorthand::longhands(p).unwrap_or_default();
            ll.iter().any(|x| *x == p) || pl.iter().any(|x| *x == l) || ll.iter().any(|x| pl.contains(x))
        };
        let mut out: Vec<&Declaration> = Vec::with_capacity(ordered.len() + 16);
        let mut ui = 0;
        for d in ordered.iter() {
            let k = rev_kind(d);
            if k == 0 {
                out.push(d);
                continue;
            }
            let at = pos.get(&(*d as *const Declaration)).copied().unwrap_or((RANK_INLINE, u16::MAX));
            // A user-agent `revert` keeps its own (there is nothing below it).
            if at.0 == RANK_UA {
                out.push(d);
                ui += 1;
                continue;
            }
            out.push(&unsets[ui]);
            ui += 1;
            for (l, lp) in &all_normal {
                let below = if k == 1 { lp.0 <= RANK_USER } else { *lp < at };
                if below && !std::ptr::eq(*l, *d) && rev_kind(l) == 0 && affects(&l.name, &d.name) {
                    out.push(l);
                }
            }
        }
        ordered = out;
    }

    // Pass 0: custom properties. The winning declaration of each custom property is
    // found first; var() references are then resolved against this element's own
    // (winning) custom properties, recursively, with cycle detection.
    if ordered.iter().any(|d| d.name.starts_with("--")) {
        let mut winners: HashMap<&str, &Declaration> = HashMap::new();
        for d in &ordered {
            if d.name.starts_with("--") {
                winners.insert(&d.name, d);
            }
        }
        let base = s.custom.clone();
        let mut map = CustomOverlay { base: &base, changes: Default::default() };
        let mut pending = Vec::new();
        // A registered property whose value doesn't match its syntax is
        // invalid at computed-value time: it keeps the inherited (or initial)
        // value (CSS Properties & Values §2.4).
        let fits = |n: &str, v: &[CV]| -> bool {
            match reg.get(n).and_then(|r| r.2.as_ref()) {
                Some(syn) if !v.iter().all(|c| c.is_ws()) => pv::matches_syntax(syn, v),
                _ => true,
            }
        };
        for (n, d) in &winners {
            if d.has_var {
                pending.push(*n);
            } else if d.value.iter().any(|c| c.is_ident("initial")) && d.value.len() == 1 {
                map.remove(*n);
            } else if !fits(n, &d.value) {
                map.changes.remove(*n);
            } else {
                map.insert(n.to_string(), Rc::new((*d.value).clone()));
            }
        }
        if !pending.is_empty() {
            let mut state: HashMap<&str, u8> = HashMap::new(); // 1 = visiting, 2 = done
            for n in pending.iter() {
                resolve_custom(n, &winners, &mut map, &mut state);
            }
            for n in pending {
                if let Some(Some(v)) = map.changes.get(n) {
                    if !fits(n, v) {
                        map.changes.remove(n);
                    }
                }
            }
        }
        let changes = map.changes;
        if !changes.is_empty() {
            s.custom = style::CustomMap::derive(&base, changes);
        }
    }
    let custom = s.custom.clone();
    let apply_one = |s: &mut ComputedStyle, d: &Declaration| {
        if d.has_var {
            if let Some(v) = substitute(&d.value, &custom, 0) {
                style::apply(s, parent, &d.name, &v, cx);
            }
        } else {
            style::apply(s, parent, &d.name, &d.value, cx);
        }
    };
    // `all` overrides every declaration before it in cascade order (except
    // custom properties, direction and unicode-bidi): only the last one and
    // what follows it apply.
    let first = ordered.iter().rposition(|d| &*d.name == "all").unwrap_or(0);
    let mut ordered: Vec<&Declaration> = if first > 0 {
        ordered.iter().enumerate().filter(|(i, d)| *i >= first || d.name.starts_with("--") || matches!(&*d.name, "direction" | "unicode-bidi")).map(|(_, d)| *d).collect()
    } else {
        ordered
    };
    // direction / unicode-bidi from before it still apply, after the reset.
    if first > 0 {
        let pre: Vec<&Declaration> = ordered.iter().filter(|d| matches!(&*d.name, "direction" | "unicode-bidi")).copied().collect();
        ordered.retain(|d| !matches!(&*d.name, "direction" | "unicode-bidi"));
        if let Some(p) = ordered.iter().position(|d| &*d.name == "all") {
            for (k, d) in pre.into_iter().enumerate() {
                ordered.insert(p + 1 + k, d);
            }
        }
    }
    // Pass 1: font-size / font / font-family (em base), direction (logical
    // properties) and color (currentColor base).
    for d in &ordered {
        if matches!(&*d.name, "all" | "font-size" | "font" | "font-family" | "direction" | "color-scheme") {
            apply_one(&mut s, d);
        }
    }
    // System colors and light-dark() follow this element's color scheme.
    values::set_dark_scheme(s.dark_scheme);
    // A keyword-derived size depends on the family (`monospace` alone uses 13px).
    if let Some(kw) = s.font_kw {
        s.font_size = style::keyword_font_size(kw, &s.font_family);
    }
    // line-height next: the `lh` unit in other properties depends on it. A
    // `font` shorthand later in the cascade resets it.
    let last_font = ordered.iter().rposition(|d| &*d.name == "font");
    for (i, d) in ordered.iter().enumerate() {
        if &*d.name == "line-height" && !last_font.map(|lf| i < lf).unwrap_or(false) {
            apply_one(&mut s, d);
        }
    }
    for d in &ordered {
        if &*d.name == "color" {
            apply_one(&mut s, d);
        }
    }
    // Pass 2: everything else. Font longhands that precede a later `font`
    // shorthand lose to it (the shorthand was applied in pass 1).
    for (i, d) in ordered.iter().enumerate() {
        if !d.name.starts_with("--") && !matches!(&*d.name, "all" | "font-size" | "font" | "font-family" | "color" | "line-height" | "direction") {
            if last_font.map(|lf| i < lf).unwrap_or(false) && matches!(&*d.name, "font-weight" | "font-style" | "font-variant" | "font-stretch" | "font-variant-caps") {
                continue;
            }
            apply_one(&mut s, d);
        }
    }
    values::set_dark_scheme(false);
    s
}

fn fixup(s: &mut ComputedStyle, _parent: &ComputedStyle, is_root: bool, tag: &str, layout_parent: Display) {
    // An inline whose writing mode is orthogonal to its parent's becomes an
    // inline-block (css-writing-modes-4 §3.1).
    if s.is_vertical() != _parent.is_vertical() && s.display == Display::Inline && !is_root {
        s.display = Display::InlineBlock;
    }
    // container-type implies size (or inline-size) containment plus style containment.
    match s.container_type {
        1 => s.contain |= style::CONTAIN_INLINE_SIZE | style::CONTAIN_STYLE,
        2 => s.contain |= style::CONTAIN_SIZE | style::CONTAIN_STYLE,
        _ => {}
    }
    // translate, rotate, scale, then transform (CSS Transforms 2 §4).
    if s.translate_prop.is_some() || s.rotate_prop.is_some() || s.scale_prop.is_some() {
        use style::TransformOp;
        let mut ops = Vec::new();
        if let Some((x, y, z)) = &s.translate_prop {
            ops.push(if *z == 0.0 { TransformOp::Translate(x.clone(), y.clone()) } else { TransformOp::Translate3d(x.clone(), y.clone(), *z) });
        }
        if let Some((x, y, z, a)) = s.rotate_prop {
            ops.push(if x == 0.0 && y == 0.0 && z > 0.0 { TransformOp::Rotate(a) } else { TransformOp::Rotate3d(x, y, z, a) });
        }
        if let Some((x, y, z)) = s.scale_prop {
            ops.push(if z == 1.0 { TransformOp::Scale(x, y) } else { TransformOp::Scale3d(x, y, z) });
        }
        if let Some(t) = &s.transform {
            ops.extend(t.iter().cloned());
        }
        s.transform = Some(Rc::new(ops));
    }
    // `display:-webkit-box` + `-webkit-line-clamp` behaves as a clamped block container.
    if s.legacy_box && s.line_clamp.is_some() {
        s.display = if s.display == Display::InlineFlex { Display::InlineBlock } else { Display::FlowRoot };
    }
    if !s.legacy_box && !matches!(s.display, Display::Block | Display::FlowRoot | Display::ListItem | Display::InlineBlock) {
        s.line_clamp = None;
    }
    if s.is_out_of_flow() {
        s.float = Float::None;
    }
    if is_root || s.float != Float::None || s.is_out_of_flow() || layout_parent.is_flex() || layout_parent.is_grid() {
        if !(s.display == Display::Contents && !is_root) {
            s.display = s.display.blockify();
        }
    }
    if s.display == Display::Contents && is_root {
        s.display = Display::Block;
    }
    // Tables never take the -webkit-* text-align values (Blink's style adjuster).
    if matches!(s.display, Display::Table | Display::InlineTable) && s.text_align == style::TextAlign::WebkitCenter {
        s.text_align = style::TextAlign::Start;
    }
    // Text decorations do not propagate into atomic inlines / out-of-flow boxes.
    if matches!(s.display, Display::InlineBlock | Display::InlineFlex | Display::InlineGrid | Display::InlineTable) || s.is_out_of_flow() || s.float != Float::None {
        s.deco_propagated = 0;
    }
    if tag == "img" || tag == "input" || tag == "button" || tag == "select" || tag == "textarea" {
        s.deco_propagated = 0;
    }
    // visible/clip compute to auto/hidden when the other axis scrolls (neither visible nor clip).
    {
        use style::Overflow as O;
        let scrolls = |o: O| !matches!(o, O::Visible | O::Clip);
        if scrolls(s.overflow_x) || scrolls(s.overflow_y) {
            let fix = |o: O| match o {
                O::Visible => O::Auto,
                O::Clip => O::Hidden,
                o => o,
            };
            s.overflow_x = fix(s.overflow_x);
            s.overflow_y = fix(s.overflow_y);
        }
    }
}

fn legacy_color(v: &str) -> Option<String> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    if values::named_color(&v.to_ascii_lowercase()).is_some() {
        return Some(v.to_string());
    }
    let hex = v.trim_start_matches('#');
    if hex.chars().all(|c| c.is_ascii_hexdigit()) && matches!(hex.len(), 3 | 6) {
        return Some(format!("#{hex}"));
    }
    None
}

fn legacy_len(v: &str) -> Option<String> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    if let Some(p) = v.strip_suffix('%') {
        let n: f32 = p.trim().parse().ok()?;
        return Some(format!("{n}%"));
    }
    let digits: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    let n: f32 = digits.parse().ok()?;
    Some(format!("{n}px"))
}

/// An SVG `width`/`height` attribute as a CSS length, or None if invalid/auto.
fn svg_len(v: &str) -> Option<String> {
    let v = v.trim();
    let num_end = v.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+' || c == 'e' || c == 'E')).unwrap_or(v.len());
    // Keep an exponent only if digits follow it (so "1em" is 1 + "em").
    let (mut num, mut unit) = v.split_at(num_end);
    if let Some(i) = num.find(['e', 'E']) {
        num = &v[..i];
        unit = &v[i..];
    }
    let n: f32 = num.parse().ok()?;
    if n < 0.0 {
        return None;
    }
    let unit = unit.trim().to_ascii_lowercase();
    match unit.as_str() {
        "" => Some(format!("{n}px")),
        "%" | "px" | "em" | "rem" | "ex" | "ch" | "pt" | "pc" | "in" | "cm" | "mm" | "q" | "vw" | "vh" | "vmin" | "vmax" => Some(format!("{n}{unit}")),
        _ => None,
    }
}

/// Map legacy presentational attributes (bgcolor, width, align, ...) to CSS.
fn presentational_hints(doc: &Document, node: NodeId) -> Vec<Declaration> {
    let el = match doc.element(node) {
        Some(e) => e,
        None => return Vec::new(),
    };
    let tag = el.name.as_str();
    // Cells take hints from their table (border, cellpadding); <bdi> defaults to dir=auto.
    if el.attrs.is_empty() && !matches!(tag, "td" | "th" | "bdi" | "mo") {
        return Vec::new();
    }
    let mut css = String::new();
    let attr = |n: &str| el.attr(n);
    let mut push = |s: String| {
        css.push_str(&s);
        css.push(';');
    };
    // dir=auto (and <bdi> without dir): direction from the first strong character.
    if attr("dir").map(|d| d.trim().eq_ignore_ascii_case("auto")).unwrap_or(false) || (tag == "bdi" && attr("dir").is_none()) {
        if let Some(rtl) = crate::forms::first_strong_rtl(doc, node) {
            push(format!("direction:{}", if rtl { "rtl" } else { "ltr" }));
        }
    }
    if el.ns == crate::dom::Namespace::Svg {
        if tag == "svg" {
            // SVG width/height are presentation attributes: any CSS length
            // (1em, 2rem, 50%) or a unitless number (user units = px).
            if let Some(w) = attr("width").and_then(svg_len) {
                push(format!("width:{w}"));
            }
            if let Some(h) = attr("height").and_then(svg_len) {
                push(format!("height:{h}"));
            }
            if let Some(f) = attr("fill").filter(|f| !f.contains("url(")) {
                if let Some(c) = legacy_color(f) {
                    push(format!("--svg-fill:{c}"));
                }
            }
        }
        return parse_declarations(&css);
    }
    if let Some(c) = attr("bgcolor").and_then(legacy_color) {
        push(format!("background-color:{c}"));
    }
    // <br clear=left|right|all|both>
    if tag == "br" {
        if let Some(c) = attr("clear") {
            match c.trim().to_ascii_lowercase().as_str() {
                "left" => push("clear:left".to_string()),
                "right" => push("clear:right".to_string()),
                "all" | "both" => push("clear:both".to_string()),
                _ => {}
            }
        }
    }
    if tag == "iframe" {
        // Blink: frameborder only ever turns the border off (when it isn't a nonzero integer).
        if let Some(fb) = attr("frameborder") {
            let t = fb.trim();
            let digits: String = t.trim_start_matches(['+', '-']).chars().take_while(|c| c.is_ascii_digit()).collect();
            if digits.parse::<i64>().map(|n| n == 0).unwrap_or(true) {
                push("border-width:0px".to_string());
            }
        }
    }
    if let Some(b) = attr("background") {
        if matches!(tag, "body" | "table" | "td" | "th" | "tr") {
            push(format!("background-image:url(\"{}\")", b.replace('"', "%22")));
        }
    }
    match tag {
        "body" => {
            if let Some(c) = attr("text").and_then(legacy_color) {
                push(format!("color:{c}"));
            }
            for (a, side) in [("marginwidth", "left"), ("marginwidth", "right"), ("leftmargin", "left"), ("rightmargin", "right"), ("topmargin", "top"), ("bottommargin", "bottom"), ("marginheight", "top"), ("marginheight", "bottom")] {
                if let Some(v) = attr(a).and_then(legacy_len) {
                    push(format!("margin-{side}:{v}"));
                }
            }
        }
        "table" => {
            if let Some(w) = attr("width").and_then(legacy_len) {
                push(format!("width:{w}"));
            }
            if let Some(h) = attr("height").and_then(legacy_len) {
                push(format!("height:{h}"));
            }
            if let Some(cs) = attr("cellspacing").and_then(legacy_len) {
                push(format!("border-spacing:{cs}"));
            }
            if let Some(b) = attr("border") {
                let n: f32 = b.trim().parse().unwrap_or(1.0);
                if n > 0.0 {
                    push(format!("border:{n}px outset #808080"));
                }
            }
            match attr("align").map(|a| a.to_ascii_lowercase()).as_deref() {
                Some("center") => push("margin-left:auto;margin-right:auto".into()),
                Some("left") => push("float:left".into()),
                Some("right") => push("float:right".into()),
                _ => {}
            }
        }
        "td" | "th" | "tr" | "tbody" | "thead" | "tfoot" | "col" => {
            if tag != "tr" {
                if let Some(w) = attr("width").and_then(legacy_len) {
                    push(format!("width:{w}"));
                }
            }
            if let Some(h) = attr("height").and_then(legacy_len) {
                push(format!("height:{h}"));
            }
            if let Some(a) = attr("align") {
                // HTML rendering §15.3.8: cell alignment also aligns block children.
                match a.to_ascii_lowercase().as_str() {
                    "center" | "middle" => push("text-align:-webkit-center".into()),
                    "left" => push("text-align:-webkit-left".into()),
                    "right" => push("text-align:-webkit-right".into()),
                    "justify" => push("text-align:justify".into()),
                    _ => {}
                }
            }
            if let Some(v) = attr("valign") {
                let v = v.to_ascii_lowercase();
                if matches!(v.as_str(), "top" | "middle" | "bottom" | "baseline") {
                    push(format!("vertical-align:{v}"));
                }
            }
            if attr("nowrap").is_some() {
                push("white-space:nowrap".into());
            }
            if matches!(tag, "td" | "th") {
                // Inherit cellpadding / border from the table.
                let mut t = doc.parent_element(node);
                while let Some(p) = t {
                    if doc.is_tag(p, "table") {
                        break;
                    }
                    t = doc.parent_element(p);
                }
                if let Some(table) = t {
                    if let Some(cp) = doc.attr(table, "cellpadding").and_then(legacy_len) {
                        push(format!("padding:{cp}"));
                    }
                    if let Some(b) = doc.attr(table, "border") {
                        let n: f32 = b.trim().parse().unwrap_or(1.0);
                        if n > 0.0 {
                            push("border:1px inset #808080".into());
                        }
                    }
                }
            }
        }
        "mo" | "mi" | "mn" | "mtext" | "mspace" | "math" | "mstyle" | "mrow" | "mfrac" | "msqrt" | "mtd" | "ms" if el.ns == crate::dom::Namespace::MathMl => {
            if let Some(c) = attr("mathcolor") {
                push(format!("color:{c}"));
            }
            if let Some(c) = attr("mathbackground") {
                push(format!("background-color:{c}"));
            }
            if let Some(sz) = attr("mathsize") {
                push(format!("font-size:{sz}"));
            }
            match attr("mathvariant").map(|v| v.to_ascii_lowercase()).as_deref() {
                Some("normal") => push("text-transform:none".into()),
                Some("bold") => push("font-weight:bold;text-transform:none".into()),
                Some("italic") => push("font-style:italic;text-transform:none".into()),
                Some("bold-italic") => push("font-weight:bold;font-style:italic;text-transform:none".into()),
                Some("monospace") => push("font-family:monospace;text-transform:none".into()),
                Some("sans-serif") => push("font-family:sans-serif;text-transform:none".into()),
                _ => {}
            }
            if tag == "mspace" {
                if let Some(w) = attr("width") {
                    push(format!("width:{w}"));
                }
                if let Some(h) = attr("height") {
                    push(format!("height:{h}"));
                }
            }
            if tag == "mo" {
                // Operator spacing from the operator dictionary (MathML Core §3.2.4):
                // relations get thick space, binary operators medium, fences none;
                // a leading (prefix) operator has no space before it.
                let text: String = doc.text_content(node).trim().to_string();
                let parent = doc.parent_element(node);
                let siblings: Vec<NodeId> = parent.map(|p| doc.children(p).iter().copied().filter(|&c| doc.element(c).is_some()).collect()).unwrap_or_default();
                let first = siblings.first() == Some(&node) && siblings.len() > 1;
                let last = siblings.last() == Some(&node) && siblings.len() > 1;
                let (mut l, mut r) = match text.as_str() {
                    "(" | ")" | "[" | "]" | "{" | "}" | "|" | "\u{2016}" | "\u{27E8}" | "\u{27E9}" | "\u{2308}" | "\u{2309}" | "\u{230A}" | "\u{230B}" | "." | "\u{2061}" | "\u{2062}" | "\u{2063}" => (0.0, 0.0),
                    "," | ";" => (0.0, 0.1667),
                    ":" => (0.2778, 0.2778),
                    "!" | "'" | "\u{2032}" => (0.0, 0.0),
                    "=" | "<" | ">" | "\u{2264}" | "\u{2265}" | "\u{2260}" | "\u{2248}" | "\u{2261}" | "\u{2192}" | "\u{2190}" | "\u{2194}" | "\u{21D2}" | "\u{21D4}" | "\u{2208}" | "\u{2209}" | "\u{2282}" | "\u{2283}" | "\u{2286}" | "\u{2287}" | "\u{223C}" | "\u{2245}" | "\u{221D}" | "\u{226A}" | "\u{226B}" | "\u{21A6}" | ":=" | "\u{2254}" => (0.2778, 0.2778),
                    "+" | "-" | "\u{2212}" | "\u{00B1}" | "\u{2213}" | "\u{00D7}" | "\u{00F7}" | "\u{22C5}" | "\u{00B7}" | "\u{2217}" | "*" | "\u{2218}" | "\u{222A}" | "\u{2229}" | "\u{2227}" | "\u{2228}" | "\u{2295}" | "\u{2297}" | "/" => (0.2222, 0.2222),
                    "\u{222B}" | "\u{222C}" | "\u{222D}" | "\u{222E}" | "\u{2211}" | "\u{220F}" | "\u{2210}" | "\u{22C3}" | "\u{22C2}" | "lim" | "max" | "min" | "sup" | "inf" => (0.1667, 0.1667),
                    _ => (0.2778, 0.2778),
                };
                let fence = l == 0.0 && r == 0.0;
                if !fence && first && !matches!(text.as_str(), "\u{222B}" | "\u{2211}" | "\u{220F}") {
                    // prefix form: e.g. unary minus
                    l = 0.0;
                    r = 0.0;
                } else if !fence && last && matches!(text.as_str(), "!" | "'") {
                    l = 0.0;
                }
                if let Some(v) = attr("lspace") {
                    push(format!("padding-left:{v}"));
                } else if l > 0.0 {
                    push(format!("padding-left:{l}em"));
                }
                if let Some(v) = attr("rspace") {
                    push(format!("padding-right:{v}"));
                } else if r > 0.0 {
                    push(format!("padding-right:{r}em"));
                }
            }
        }
        "img" | "video" | "canvas" | "iframe" | "embed" | "object" | "input" => {
            // Canvas width/height are its bitmap size (its natural size), not CSS hints.
            // An <img> in a <picture> takes its dimensions from the selected
            // <source> when that has any (HTML "dimension attribute source").
            let src_dims = if tag == "img" {
                crate::page::resources::picture_source(doc, node).filter(|&s| doc.attr(s, "width").is_some() || doc.attr(s, "height").is_some())
            } else {
                None
            };
            let attr = |n: &str| -> Option<&str> { match src_dims { Some(s) if n == "width" || n == "height" => doc.attr(s, n), _ => el.attr(n) } };
            if tag != "canvas" && (tag != "input" || attr("type").map(|t| t.eq_ignore_ascii_case("image")).unwrap_or(false)) {
                if let Some(w) = attr("width").and_then(legacy_len) {
                    push(format!("width:{w}"));
                }
                if let Some(h) = attr("height").and_then(legacy_len) {
                    push(format!("height:{h}"));
                }
                if let (Some(w), Some(h)) = (attr("width").and_then(|v| v.trim().parse::<f32>().ok()), attr("height").and_then(|v| v.trim().parse::<f32>().ok())) {
                    if w > 0.0 && h > 0.0 {
                        push(format!("aspect-ratio:auto {w}/{h}"));
                    }
                }
            }
            if tag == "img" {
                if let Some(b) = attr("border").and_then(legacy_len) {
                    push(format!("border:{b} solid"));
                }
                match attr("align").map(|a| a.to_ascii_lowercase()).as_deref() {
                    Some("left") => push("float:left".into()),
                    Some("right") => push("float:right".into()),
                    Some("middle") | Some("absmiddle") | Some("center") => push("vertical-align:middle".into()),
                    Some("top") => push("vertical-align:top".into()),
                    _ => {}
                }
                if let Some(h) = attr("hspace").and_then(legacy_len) {
                    push(format!("margin-left:{h};margin-right:{h}"));
                }
                if let Some(v) = attr("vspace").and_then(legacy_len) {
                    push(format!("margin-top:{v};margin-bottom:{v}"));
                }
            }
        }
        "select" => {
            // A list box (size > 1) looks like a field rather than a button.
            if attr("multiple").is_none() && attr("size").and_then(|v| v.trim().parse::<i32>().ok()).map(|n| n > 1).unwrap_or(false) {
                push("border-radius:2px;vertical-align:text-bottom;white-space:nowrap".into());
            }
        }
        "font" => {
            if let Some(c) = attr("color").and_then(legacy_color) {
                push(format!("color:{c}"));
            }
            if let Some(f) = attr("face") {
                push(format!("font-family:{}", f.replace(';', "")));
            }
            if let Some(sz) = attr("size") {
                let sz = sz.trim();
                let n: i32 = if let Some(r) = sz.strip_prefix('+') {
                    3 + r.parse::<i32>().unwrap_or(0)
                } else if let Some(r) = sz.strip_prefix('-') {
                    3 - r.parse::<i32>().unwrap_or(0)
                } else {
                    sz.parse().unwrap_or(3)
                };
                let kw = ["x-small", "x-small", "small", "medium", "large", "x-large", "xx-large", "xxx-large"][n.clamp(1, 7) as usize];
                push(format!("font-size:{kw}"));
            }
        }
        "hr" => {
            if let Some(w) = attr("width").and_then(legacy_len) {
                push(format!("width:{w}"));
            }
            if let Some(sz) = attr("size").and_then(|s| s.trim().parse::<f32>().ok()) {
                push(format!("height:{}px;border-width:0;background-color:gray", (sz - 2.0).max(0.0)));
                if sz <= 1.0 {
                    push("border-bottom-width:0;height:0;border-top-width:1px".into());
                }
            }
            if let Some(c) = attr("color").and_then(legacy_color) {
                push(format!("border-color:{c};background-color:{c}"));
            }
            if attr("noshade").is_some() {
                push("border-style:solid;background-color:gray".into());
            }
            match attr("align").map(|a| a.to_ascii_lowercase()).as_deref() {
                Some("left") => push("margin-left:0;margin-right:auto".into()),
                Some("right") => push("margin-left:auto;margin-right:0".into()),
                _ => {}
            }
        }
        "div" | "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "caption" | "legend" => {
            if let Some(a) = attr("align") {
                let a = a.to_ascii_lowercase();
                if matches!(tag, "div" | "p") && matches!(a.as_str(), "center" | "middle") {
                    push("text-align:-webkit-center".into());
                } else if matches!(a.as_str(), "left" | "right" | "center" | "justify") {
                    push(format!("text-align:{a}"));
                }
            }
        }
        "ol" | "ul" | "li" => {
            if let Some(t) = attr("type") {
                let lst = match t {
                    "1" => "decimal",
                    "a" => "lower-alpha",
                    "A" => "upper-alpha",
                    "i" => "lower-roman",
                    "I" => "upper-roman",
                    "disc" => "disc",
                    "circle" => "circle",
                    "square" => "square",
                    _ => "",
                };
                if !lst.is_empty() {
                    push(format!("list-style-type:{lst}"));
                }
            }
        }
        _ => {}
    }
    if let Some(d) = attr("dir") {
        if d.eq_ignore_ascii_case("rtl") {
            push("direction:rtl".into());
        }
    }
    if css.is_empty() {
        return Vec::new();
    }
    parse_declarations_in(&css, Some(&doc.base_url))
}

/// The OpenHeronBrowser user-agent stylesheet.
pub const UA_CSS: &str = r#"
html, address, blockquote, body, center, dialog, div, figure, figcaption, footer, form, header, hr, legend,
listing, main, p, plaintext, pre, search, xmp, article, aside, h1, h2, h3, h4, h5, h6, hgroup, nav, section,
dir, dd, dl, dt, menu, ol, ul, fieldset, details, summary, optgroup, frameset, frame { display: block; }
slot { display: contents; }
head, link, meta, script, style, title, base, basefont, datalist, noembed, noframes, param, rp, template, area,
[hidden]:not([hidden=until-found]), noscript, dialog:not([open]), embed[hidden], source, track { display: none; }
input[type=hidden i] { display: none !important; }
html { color: canvastext; background: transparent; font-family: serif; line-height: normal; }
[dir=rtl i] { direction: rtl; }
[dir=ltr i] { direction: ltr; }
bdi, [dir] { unicode-bidi: isolate; }
bdo { unicode-bidi: isolate-override; }
body { margin: 8px; }
li { display: list-item; }
table { display: table; border-spacing: 2px; border-collapse: separate; box-sizing: border-box; text-indent: initial; }
caption { display: table-caption; text-align: center; }
colgroup { display: table-column-group; }
col { display: table-column; }
thead { display: table-header-group; vertical-align: middle; }
tbody { display: table-row-group; vertical-align: middle; }
tfoot { display: table-footer-group; vertical-align: middle; }
tr { display: table-row; vertical-align: inherit; }
td, th { display: table-cell; vertical-align: inherit; padding: 1px; }
th { font-weight: bold; text-align: center; }
p, blockquote, figure, dl, pre, listing, xmp, plaintext { margin-top: 1em; margin-bottom: 1em; }
blockquote, figure { margin-left: 40px; margin-right: 40px; }
dd { margin-left: 40px; }
address, em, i, cite, var, dfn { font-style: italic; }
b, strong { font-weight: bolder; }
h1 { font-size: 2em; margin-top: 0.67em; margin-bottom: 0.67em; font-weight: bold; }
h2 { font-size: 1.5em; margin-top: 0.83em; margin-bottom: 0.83em; font-weight: bold; }
h3 { font-size: 1.17em; margin-top: 1em; margin-bottom: 1em; font-weight: bold; }
h4 { margin-top: 1.33em; margin-bottom: 1.33em; font-weight: bold; }
h5 { font-size: 0.83em; margin-top: 1.67em; margin-bottom: 1.67em; font-weight: bold; }
h6 { font-size: 0.67em; margin-top: 2.33em; margin-bottom: 2.33em; font-weight: bold; }
ul, menu, dir { list-style-type: disc; margin-top: 1em; margin-bottom: 1em; padding-left: 40px; }
ol { list-style-type: decimal; margin-top: 1em; margin-bottom: 1em; padding-left: 40px; }
ul ul, ol ul, ul ol, ol ol, ul menu, menu ul { margin-top: 0; margin-bottom: 0; }
ul ul, ol ul, menu ul { list-style-type: circle; }
ul ul ul, ul ol ul, ol ul ul, ol ol ul { list-style-type: square; }
pre, code, kbd, samp, tt, listing, xmp, plaintext { font-family: monospace; }
pre, listing, xmp, plaintext { white-space: pre; }
textarea { white-space: pre-wrap; }
mark { background-color: mark; color: marktext; }
big { font-size: larger; }
small { font-size: smaller; }
sub { vertical-align: sub; font-size: smaller; }
sup { vertical-align: super; font-size: smaller; }
s, strike, del { text-decoration: line-through; }
u, ins { text-decoration: underline; }
abbr[title], acronym[title] { text-decoration: underline dotted; }
center { display: block; text-align: -webkit-center; }
nobr { white-space: nowrap; }
a:any-link { color: #0000ee; text-decoration: underline; cursor: pointer; }
hr { color: gray; border-style: inset; border-width: 1px; margin: 0.5em auto; overflow: hidden; }
fieldset { margin-left: 2px; margin-right: 2px; padding: 0.35em 0.75em 0.625em; border: 2px groove threedface; min-width: min-content; }
legend { padding-left: 2px; padding-right: 2px; }
img, video, canvas, svg, iframe, embed, object { display: inline; }
img:is([sizes="auto" i], [sizes^="auto," i]) { contain: size !important; contain-intrinsic-size: 300px 150px; }
iframe { border: 2px inset; width: 300px; height: 150px; }
audio:not([controls]) { display: none !important; }
video { object-fit: contain; }
summary { display: list-item; list-style: disclosure-closed inside; cursor: pointer; }
details[open] > summary { list-style-type: disclosure-open; }
details:not([open]) > :not(summary) { display: none; }
q::before { content: open-quote; }
q::after { content: close-quote; }
ruby { display: ruby; }
math { direction: ltr; text-indent: 0; letter-spacing: normal; word-spacing: normal; line-height: 1; font-family: math; font-style: normal; font-weight: normal; white-space: nowrap; text-transform: none; }
math[display="block" i] { display: block; text-align: center; margin: 0.5em 0; }
mi { text-transform: math-auto; }
mtable { display: inline-table; vertical-align: middle; border-spacing: 0; }
mtr { display: table-row; }
mtd { display: table-cell; text-align: center; padding: 0.5ex 0.4em; vertical-align: baseline; line-height: 0.75; }
mfrac { display: inline-flex; flex-direction: column; vertical-align: middle; text-align: center; padding: 0 1px; }
mfrac > * { display: block; font-size: 71%; padding: 0 0.1em; }
math[display="block" i] > mfrac > *, math[display="block" i] > mrow > mfrac > *, math[display="block" i] > mstyle > mfrac > * { font-size: 100%; }
mfrac > :first-child { border-bottom: 0.06em solid; padding-bottom: 0.1em; }
mfrac > :nth-child(2) { padding-top: 0.12em; }
mfrac[linethickness="0"] > :first-child, mfrac[linethickness="0px"] > :first-child { border-bottom: none; }
msqrt, mroot { display: inline-block; position: relative; border-top: 0.06em solid; margin-left: 0.8em; padding: 0.1em 0.08em 0 0.06em; }
msqrt::before, mroot::before { content: "\221A"; position: absolute; right: 100%; top: -0.26em; line-height: 1; font-size: 1em; }
mroot > :not(:first-child) { position: absolute; right: 100%; top: -0.2em; margin-right: 0.45em; font-size: 50%; }
msup > :not(:first-child), msub > :not(:first-child), msubsup > :not(:first-child), munder > :not(:first-child), mover > :not(:first-child), munderover > :not(:first-child), mmultiscripts > :not(:first-child) { font-size: 71%; }
msup > :nth-child(2) { vertical-align: super; }
msub > :nth-child(2) { vertical-align: sub; }
msubsup { display: inline-grid; grid-template-columns: auto auto; grid-template-rows: auto auto; vertical-align: middle; }
msubsup > :first-child { grid-row: 1 / span 2; grid-column: 1; align-self: center; }
msubsup > :nth-child(2) { grid-row: 2; grid-column: 2; align-self: end; }
msubsup > :nth-child(3) { grid-row: 1; grid-column: 2; align-self: start; }
munder, mover, munderover { display: inline-flex; flex-direction: column; align-items: center; vertical-align: middle; }
mover { flex-direction: column-reverse; }
munderover > :first-child { order: 2; } munderover > :nth-child(2) { order: 3; } munderover > :nth-child(3) { order: 1; }
semantics > :not(:first-child), maction > :not(:first-child), annotation, annotation-xml, none, mprescripts { display: none; }
merror { border: 1px solid red; background-color: lightyellow; }
mphantom { visibility: hidden; }
menclose { display: inline-block; border: 1px solid; padding: 0 0.2em; }
mspace { display: inline-block; }
ms::before, ms::after { content: "\""; }
rt { font-size: 50%; line-height: 1; }
rp { display: none; }
input, select, textarea, button { font: 13.333px system-ui, sans-serif; color: fieldtext; letter-spacing: normal; word-spacing: normal; line-height: normal; text-transform: none; text-indent: 0; text-shadow: none; text-align: start; display: inline-block; }
input { border: 2px inset light-dark(#767676, #858585); padding: 1px 2px; background-color: field; cursor: text; }
input[type=search] { box-sizing: border-box; }
input[type=date], input[type=time], input[type=datetime-local], input[type=month], input[type=week] { font-family: monospace; padding: 0 0 0 1px; cursor: default; }
textarea { border: 1px solid light-dark(#767676, #858585); padding: 2px; background-color: field; overflow: auto; font-family: monospace; resize: auto; white-space: pre-wrap; overflow-wrap: break-word; cursor: text; }
select { border: 1px solid light-dark(#767676, #858585); padding: 0; background-color: field; box-sizing: border-box; cursor: default; white-space: pre; }
select[multiple] { border-radius: 2px; vertical-align: text-bottom; white-space: nowrap; }
input[type=checkbox], input[type=radio] { width: 13px; height: 13px; padding: 0; border: none; margin: 3px 3px 3px 4px; cursor: default; box-sizing: border-box; background-color: transparent; }
input[type=radio] { margin: 3px 3px 0 5px; }
input[type=range] { width: 129px; height: 16px; margin: 2px; padding: 0; border: none; background: transparent; cursor: default; color: #9d968e; }
input[type=color] { width: 50px; height: 27px; padding: 1px 2px; border: 1px solid buttonborder; background-color: buttonface; box-sizing: border-box; cursor: default; }
input[type=file] { border: none; padding: 0; background: transparent; cursor: default; white-space: pre; }
input[type=submit], input[type=button], input[type=reset], button { background-color: buttonface; color: buttontext; border: 2px outset light-dark(#767676, #6b6b6b); padding: 1px 6px; text-align: center; box-sizing: border-box; cursor: default; }
input[type=image] { border: none; padding: 0; background: transparent; cursor: pointer; }
input:disabled, textarea:disabled, select:disabled { color: #545454; border-color: rgba(118, 118, 118, 0.3); background-color: rgba(239, 239, 239, 0.3); cursor: default; }
input[type=submit]:disabled, input[type=button]:disabled, input[type=reset]:disabled, input[type=color]:disabled, button:disabled { color: rgba(16, 16, 16, 0.3); border-color: rgba(118, 118, 118, 0.3); background-color: rgba(239, 239, 239, 0.3); }
input[type=checkbox]:disabled, input[type=radio]:disabled, input[type=file]:disabled, input[type=range]:disabled, input[type=image]:disabled { background-color: transparent; }
option { display: block; }
select option, select optgroup { display: none; }
progress { display: inline-block; width: 10em; height: 1em; vertical-align: -0.2em; box-sizing: border-box; }
meter { display: inline-block; width: 5em; height: 1em; vertical-align: -0.2em; box-sizing: border-box; }
dialog { position: absolute; left: 0; right: 0; width: fit-content; height: fit-content; margin: auto; border: solid; padding: 1em; background-color: canvas; color: canvastext; }
dialog:modal { overflow: auto; position: fixed; top: 0; bottom: 0; max-width: calc(100% - 6px - 2em); max-height: calc(100% - 6px - 2em); }
::backdrop { position: fixed; top: 0; right: 0; bottom: 0; left: 0; }
dialog::backdrop { background: rgba(0, 0, 0, 0.1); }
[popover]::backdrop { pointer-events: none; }
[popover]:not(:popover-open):not(dialog[open]) { display: none; }
[popover] { position: fixed; inset: 0; width: fit-content; height: fit-content; margin: auto; border: solid; padding: 0.25em; overflow: auto; color: canvastext; background-color: canvas; }
:focus-visible { outline: 2px solid #0b57d0; outline-offset: 1px; }
input:focus, textarea:focus, select:focus { outline: 2px solid #0b57d0; outline-offset: 0; }
::placeholder { color: #757575; }
marquee { display: inline-block; width: -webkit-fill-available; }
slot { display: contents; }

/* Element fullscreen (the element fills the viewport). */
[__ohbfs] { position: fixed !important; inset: 0 !important; width: 100% !important; height: 100% !important; max-width: none !important; max-height: none !important; min-width: 0 !important; min-height: 0 !important; margin: 0 !important; box-sizing: border-box !important; z-index: 2147483647 !important; transform: none !important; object-fit: contain; background-color: black; }
"#;

/// A style rule that matches an element, for DevTools.
pub struct MatchedRuleInfo {
    pub selector: String,
    pub origin: Origin,
    /// The sheet's URL (its base).
    pub source: String,
    pub media: Vec<String>,
    pub decls: Vec<(String, String, bool)>,
    pub specificity: u32,
}

/// The rules of `sheets` that match `node` (document-tree rules, no
/// pseudo-elements), lowest precedence first. For DevTools: a plain scan of
/// every rule, so it is slow but simple and exact.
pub fn matched_rules_debug(sheets: &[Rc<Stylesheet>], doc: &Document, node: NodeId, env: &MediaEnv, hover: &[NodeId], focus: Option<NodeId>) -> Vec<MatchedRuleInfo> {
    let st = selector::MatchState { hover_chain: hover, focus, active_chain: &[], target: None, scope: None, host: None, shadow_root: None };
    let mut out: Vec<(u8, u32, usize, MatchedRuleInfo)> = Vec::new();
    let mut order = 0usize;
    for sheet in sheets {
        for rule in &sheet.rules {
            order += 1;
            if !rule.media.iter().all(|m| media_matches(m, env)) {
                continue;
            }
            let mut best: Option<u32> = None;
            let mut texts = Vec::new();
            for sel in &rule.selectors {
                if sel.pseudo_element.is_some() || sel.shadow_arg.is_some() {
                    continue;
                }
                let ok = match &rule.scope {
                    None => selector::matches(sel, doc, node, &st),
                    Some(scope) => match scope_root(scope, doc, node) {
                        Some(root) => selector::matches(sel, doc, node, &selector::MatchState { scope: Some(root), ..st }),
                        None => false,
                    },
                };
                if ok {
                    best = Some(best.map_or(sel.specificity, |b| b.max(sel.specificity)));
                    texts.push(sel.to_css());
                }
            }
            let Some(spec) = best else { continue };
            let decls = rule.decls.iter().map(|d| (d.name.to_string(), tokenizer::serialize(&d.value), d.important)).collect();
            let media = rule.media.iter().map(|m| tokenizer::serialize(m)).collect();
            let rank = match sheet.origin {
                Origin::UserAgent => 0,
                Origin::User => 1,
                Origin::Author => 2,
            };
            out.push((rank, spec, order, MatchedRuleInfo { selector: texts.join(", "), origin: sheet.origin, source: sheet.base.to_string(), media, decls, specificity: spec }));
        }
    }
    out.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    out.into_iter().map(|x| x.3).collect()
}
