// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! CSS declaration blocks (CSSOM): parsing a declaration list into longhand
//! declarations, get/set/remove, and serialization with shorthands.

use super::db;
use super::shorthand;
use super::super::tokenizer;

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    /// Specified value ("" when pending substitution from a shorthand).
    pub value: String,
    pub important: bool,
    /// (shorthand, raw value) for longhands set by a shorthand with var().
    pub pending: Option<(String, String)>,
    /// The value at full numeric precision, when the serialization rounds it.
    pub precise: Option<String>,
}

thread_local! {
    /// Serialized declaration blocks whose values were rounded → the text
    /// at full precision (what styles are computed from).
    static PRECISE_TEXT: std::cell::RefCell<std::collections::HashMap<String, std::rc::Rc<str>>> = Default::default();
}

/// Remember the full-precision form of a serialized block (if it differs).
pub fn remember_precise(serialized: &str, entries: &[Entry]) {
    if !entries.iter().any(|e| e.precise.is_some()) {
        return;
    }
    let t = native_text(entries);
    PRECISE_TEXT.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 1024 {
            c.clear();
        }
        c.insert(serialized.to_string(), std::rc::Rc::from(t.as_str()));
    });
}

/// The full-precision text of a serialized block, if one was remembered.
pub fn precise_text(serialized: &str) -> Option<std::rc::Rc<str>> {
    PRECISE_TEXT.with(|c| c.borrow().get(serialized).cloned())
}

/// The declarations as longhands at full precision (for computing styles).
pub fn native_text(entries: &[Entry]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut pend: Vec<&str> = Vec::new();
    for e in entries {
        let imp = if e.important { " !important" } else { "" };
        if let Some((sh, raw)) = &e.pending {
            if !pend.contains(&sh.as_str()) {
                pend.push(sh);
                out.push(format!("{}: {}{};", sh, raw, imp));
            }
            continue;
        }
        out.push(format!("{}: {}{};", e.name, e.precise.as_deref().unwrap_or(&e.value), imp));
    }
    out.join(" ")
}

/// Split a declaration list at top-level semicolons (strings, blocks and
/// comments respected).
pub fn split_decls(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut q: Option<char> = None;
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if let Some(qc) = q {
            cur.push(c);
            if c == '\\' {
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            } else if c == qc {
                q = None;
            }
            continue;
        }
        match c {
            '/' if it.peek() == Some(&'*') => {
                it.next();
                let mut prev = ' ';
                for d in it.by_ref() {
                    if prev == '*' && d == '/' {
                        break;
                    }
                    prev = d;
                }
                cur.push(' ');
            }
            '"' | '\'' => {
                q = Some(c);
                cur.push(c);
            }
            '\\' => {
                cur.push(c);
                if let Some(n) = it.next() {
                    cur.push(n);
                }
            }
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(c);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(c);
            }
            ';' if depth <= 0 => {
                out.push(std::mem::take(&mut cur));
                depth = 0;
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

/// Strip a trailing `!important`; returns (value, important).
pub fn strip_important(v: &str) -> (String, bool) {
    let t = v.trim_end();
    let lower = t.to_ascii_lowercase();
    if lower.ends_with("important") {
        let head = &t[..t.len() - "important".len()];
        let h = head.trim_end();
        if let Some(rest) = h.strip_suffix('!') {
            return (rest.to_string(), true);
        }
    }
    (v.to_string(), false)
}

/// Parse one declaration's value into entries. `native` accepts properties
/// the engine doesn't model (kept as written).
pub fn parse_one(prop: &str, value: &str, important: bool, native: &dyn Fn(&str) -> bool) -> Option<Vec<Entry>> {
    let name = if prop.starts_with("--") { prop.to_string() } else { db::canonical_name(prop) };
    if name.starts_with("--") {
        // Custom properties: the value as written, trimmed.
        let v = value.trim_matches(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\x0c');
        return Some(vec![Entry { name, value: v.to_string(), important, pending: None, precise: None }]);
    }
    let cvs = tokenizer::parse_cvs(value);
    if !db::is_known(&name) {
        if !native(&name) || tokenizer::trim(&cvs).is_empty() {
            return None;
        }
        return Some(vec![Entry { name, value: super::raw_text(&cvs), important, pending: None, precise: None }]);
    }
    let p = db::parse_decl(&name, &cvs)?;
    // Values with long numbers also keep a full-precision form.
    let precise: Option<Vec<(&'static str, String)>> = (p.pending.is_none() && super::super::calc::has_long_number(value)).then(|| super::super::calc::with_precise(|| db::parse_decl(&name, &cvs)).map(|x| x.longhands)).flatten();
    let pending = p.pending.map(|raw| (name.clone(), raw));
    Some(
        p.longhands
            .into_iter()
            .map(|(n, v)| {
                let pr = precise.as_ref().and_then(|l| l.iter().find(|(m, _)| *m == n)).map(|(_, t)| t.clone()).filter(|t| *t != v);
                Entry { name: n.to_string(), value: v, important, pending: pending.clone(), precise: pr }
            })
            .collect(),
    )
}

/// Parse a declaration list (a style attribute or rule body).
pub fn parse_block(text: &str, native: &dyn Fn(&str) -> bool) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for d in split_decls(text) {
        let Some(i) = d.find(':') else { continue };
        let name = d[..i].trim();
        if name.is_empty() || name.contains(char::is_whitespace) {
            continue;
        }
        let (v, imp) = strip_important(&d[i + 1..]);
        if let Some(es) = parse_one(name, &v, imp, native) {
            set(&mut out, es, true);
        }
    }
    out
}

/// Set entries; with `reorder` (parsing), a redeclared property moves to the end.
pub fn set(entries: &mut Vec<Entry>, new: Vec<Entry>, reorder: bool) -> bool {
    let mut changed = false;
    for e in new {
        if let Some(i) = entries.iter().position(|x| x.name == e.name) {
            if reorder {
                entries.remove(i);
                entries.push(e);
                changed = true;
            } else if entries[i] != e {
                entries[i] = e;
                changed = true;
            }
        } else {
            entries.push(e);
            changed = true;
        }
    }
    changed
}

fn names_for(prop: &str) -> Vec<String> {
    if prop.starts_with("--") {
        return vec![prop.to_string()];
    }
    let n = db::canonical_name(prop);
    match shorthand::longhands(&n) {
        Some(l) => l.into_iter().map(|s| s.to_string()).collect(),
        None => vec![n],
    }
}

pub fn remove(entries: &mut Vec<Entry>, prop: &str) -> bool {
    let names = names_for(prop);
    let before = entries.len();
    entries.retain(|e| !names.contains(&e.name));
    entries.len() != before
}

/// Shorthand value from entries for its longhands; "" when not expressible.
fn shorthand_value(entries: &[Entry], sh: &str, longhands: &[&str]) -> Option<(String, bool)> {
    let es: Vec<&Entry> = longhands.iter().map(|l| entries.iter().find(|e| e.name == *l)).collect::<Option<Vec<_>>>()?;
    let imp = es[0].important;
    if es.iter().any(|e| e.important != imp) {
        return None;
    }
    // Pending substitution: all from this shorthand with the same raw value.
    if es.iter().any(|e| e.pending.is_some()) {
        let p0 = es[0].pending.clone()?;
        if p0.0 == sh && es.iter().all(|e| e.pending.as_ref() == Some(&p0)) {
            return Some((p0.1, imp));
        }
        return None;
    }
    let v = shorthand::serialize(sh, &|n| es.iter().find(|e| e.name == n).map(|e| e.value.clone()))?;
    if v.is_empty() {
        return None;
    }
    Some((v, imp))
}

pub fn get(entries: &[Entry], prop: &str) -> String {
    let n = if prop.starts_with("--") { prop.to_string() } else { db::canonical_name(prop) };
    if let Some(l) = shorthand::longhands(&n) {
        return shorthand_value(entries, &n, &l).map(|x| x.0).unwrap_or_default();
    }
    entries.iter().find(|e| e.name == n).map(|e| if e.pending.is_some() { String::new() } else { e.value.clone() }).unwrap_or_default()
}

pub fn priority(entries: &[Entry], prop: &str) -> bool {
    let names = names_for(prop);
    let es: Vec<&Entry> = entries.iter().filter(|e| names.contains(&e.name)).collect();
    !es.is_empty() && es.len() == names.len() && es.iter().all(|e| e.important)
}

/// Shorthands containing `longhand`, most longhands first.
fn shorthands_for(longhand: &str) -> Vec<&'static shorthand::SH> {
    let mut v: Vec<&'static shorthand::SH> = shorthand::SHORTHANDS.iter().filter(|s| s.longhands.contains(&longhand)).collect();
    v.sort_by(|a, b| b.longhands.len().cmp(&a.longhands.len()));
    v
}

/// CSSOM "serialize a CSS declaration block".
pub fn serialize(entries: &[Entry]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut done: Vec<&str> = Vec::new();
    let mut pending_out: Vec<&str> = Vec::new();
    'outer: for e in entries {
        if done.contains(&e.name.as_str()) {
            continue;
        }
        for sh in shorthands_for(&e.name) {
            if sh.longhands.iter().any(|l| done.contains(l)) {
                continue;
            }
            if let Some((v, imp)) = shorthand_value(entries, sh.name, sh.longhands) {
                out.push(format!("{}: {}{};", sh.name, v, if imp { " !important" } else { "" }));
                done.extend(sh.longhands.iter().copied());
                continue 'outer;
            }
        }
        if let Some((sh, raw)) = &e.pending {
            // A longhand of a pending shorthand that can't be grouped (one of
            // its longhands was overridden): the shorthand is kept as written,
            // ahead of the overrides, so the cascade still sees it.
            done.push(&e.name);
            if !pending_out.contains(&sh.as_str()) {
                pending_out.push(sh.as_str());
                out.push(format!("{}: {}{};", sh, raw, if e.important { " !important" } else { "" }));
            }
            continue;
        }
        out.push(format!("{}: {}{};", e.name, e.value, if e.important { " !important" } else { "" }));
        done.push(&e.name);
    }
    out.join(" ")
}
