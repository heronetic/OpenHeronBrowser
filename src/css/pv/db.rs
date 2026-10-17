// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! The property database (built from `table.rs`), the grammar resolver, and
//! the public parsing / serialization / computation API.

use std::collections::HashMap;
use std::rc::Rc;

use super::grammar::{self, Resolver, G};
use super::super::tokenizer::{self, Tok, CV};
use super::{compute, ser, Env, V};

pub struct Prop {
    pub name: &'static str,
    pub grammar: Rc<G>,
    pub initial: &'static str,
    pub inherited: bool,
}

pub struct Db {
    pub props: HashMap<&'static str, Prop>,
    pub types: HashMap<&'static str, Rc<G>>,
    pub aliases: HashMap<&'static str, &'static str>,
    /// Property names in table order (for getComputedStyle enumeration).
    pub order: Vec<&'static str>,
}

impl Db {
    fn build() -> Db {
        let mut db = Db { props: HashMap::new(), types: HashMap::new(), aliases: HashMap::new(), order: Vec::new() };
        for line in super::table::TABLE.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("//") {
                continue;
            }
            if line.starts_with('<') {
                let (name, g) = line.split_once(":=").expect("type line");
                let name = name.trim().trim_start_matches('<').trim_end_matches('>');
                db.types.insert(name, Rc::new(grammar::parse(g.trim())));
                continue;
            }
            let (names, rest) = line.split_once(" = ").unwrap_or_else(|| panic!("bad table line: {line}"));
            let mut parts = rest.split(" ; ");
            let g = Rc::new(grammar::parse(parts.next().unwrap().trim()));
            let initial = parts.next().unwrap_or("").trim();
            let flags = parts.next().unwrap_or("").trim();
            let mut first: Option<&'static str> = None;
            for n in names.split_whitespace() {
                // -webkit- names after a standard one are aliases of it.
                if let Some(f) = first {
                    if n.starts_with("-webkit-") && !f.starts_with("-webkit-") || n == "word-wrap" {
                        db.aliases.insert(n, f);
                        continue;
                    }
                }
                if first.is_none() {
                    first = Some(n);
                }
                db.order.push(n);
                db.props.insert(n, Prop { name: n, grammar: g.clone(), initial, inherited: flags.contains('i') });
            }
        }
        for (a, b) in super::shorthand::ALIASES {
            db.aliases.insert(a, b);
        }
        db
    }
}

thread_local! {
    pub static DB: Db = Db::build();
}

pub fn with_db<R>(f: impl FnOnce(&Db) -> R) -> R {
    DB.with(|d| f(d))
}

impl Resolver for Db {
    fn leaf(&self, name: &str, range: Option<(f64, f64)>, inp: &[&CV], pos: usize) -> Vec<(usize, V)> {
        match name {
            "line-names" => match inp.get(pos) {
                Some(CV::Block('[', inner)) => {
                    let it = grammar::items(inner);
                    let mut names = Vec::new();
                    for c in it {
                        match c {
                            CV::T(Tok::Ident(s)) if !s.eq_ignore_ascii_case("span") && !s.eq_ignore_ascii_case("auto") && !super::is_css_wide(s) && !s.eq_ignore_ascii_case("default") => names.push(super::ser_ident(s)),
                            _ => return vec![],
                        }
                    }
                    vec![(pos + 1, V::Raw(format!("[{}]", names.join(" "))))]
                }
                _ => vec![],
            },
            _ => super::types::leaf(name, range, inp, pos),
        }
    }
    fn grammar(&self, name: &str, prop: bool) -> Option<Rc<G>> {
        if prop {
            return self.props.get(name).map(|p| p.grammar.clone()).or_else(|| super::shorthand::grammar_of(self, name));
        }
        self.types.get(name).cloned()
    }
}

pub fn canonical_name(name: &str) -> String {
    let l = if name.starts_with("--") { name.to_string() } else { name.to_ascii_lowercase() };
    with_db(|db| db.aliases.get(l.as_str()).map(|s| s.to_string())).unwrap_or(l)
}

pub fn is_known(name: &str) -> bool {
    let n = canonical_name(name);
    with_db(|db| db.props.contains_key(n.as_str())) || super::shorthand::longhands(&n).is_some()
}

pub fn is_inherited(name: &str) -> bool {
    with_db(|db| db.props.get(name).map(|p| p.inherited).unwrap_or(false))
}

pub fn longhands_of(name: &str) -> Option<Vec<&'static str>> {
    super::shorthand::longhands(name)
}

/// Parse a longhand's value with its grammar plus property-specific checks.
pub fn parse_value(prop: &str, v: &[CV]) -> Option<V> {
    let it = grammar::items(v);
    if it.is_empty() {
        return None;
    }
    let val = with_db(|db| {
        let p = db.props.get(prop)?;
        grammar::match_all(db, &p.grammar, &it)
    })?;
    super::hooks::specified(prop, val)
}

/// One parsed declaration: longhand → specified text ("" = pending
/// substitution), and the raw text for a pending shorthand.
#[derive(Clone, Debug)]
pub struct Parsed {
    pub longhands: Vec<(&'static str, String)>,
    pub pending: Option<String>,
}

/// Parse a declaration (`prop: value`) into longhand values. None if invalid.
pub fn parse_decl(prop: &str, v: &[CV]) -> Option<Parsed> {
    let name = canonical_name(prop);
    let v = tokenizer::trim(v);
    if v.is_empty() {
        return None;
    }
    let short = super::shorthand::longhands(&name);
    let key: &'static str = match &short {
        Some(_) => "",
        None => with_db(|db| db.props.get_key_value(name.as_str()).map(|(k, _)| *k))?,
    };
    if let Some(kw) = super::css_wide_of(v) {
        return Some(match short {
            Some(l) => Parsed { longhands: l.into_iter().map(|n| (n, kw.clone())).collect(), pending: None },
            None => Parsed { longhands: vec![(key, kw)], pending: None },
        });
    }
    if super::has_var(v) {
        let raw = super::raw_text(v);
        return Some(match short {
            Some(l) => Parsed { longhands: l.into_iter().map(|n| (n, String::new())).collect(), pending: Some(raw) },
            None => Parsed { longhands: vec![(key, raw)], pending: None },
        });
    }
    // A css-wide keyword inside a larger value is invalid.
    if grammar::items(v).iter().any(|c| matches!(c, CV::T(Tok::Ident(s)) if super::is_css_wide(s))) && name != "will-change" {
        return None;
    }
    match short {
        Some(_) => {
            let vals = super::shorthand::expand(&name, v)?;
            Some(Parsed { longhands: vals, pending: None })
        }
        None => {
            let val = parse_value(key, v)?;
            Some(Parsed { longhands: vec![(key, ser(&val))], pending: None })
        }
    }
}

/// Specified serialization of a single property value (None if invalid).
pub fn specified_text(prop: &str, text: &str) -> Option<String> {
    let cvs = tokenizer::parse_cvs(text);
    let p = parse_decl(prop, &cvs)?;
    if let Some(raw) = p.pending {
        return Some(raw);
    }
    if p.longhands.len() == 1 && super::shorthand::longhands(&canonical_name(prop)).is_none() {
        return Some(p.longhands[0].1.clone());
    }
    let name = canonical_name(prop);
    let map: HashMap<&str, String> = p.longhands.iter().map(|(k, v)| (*k, v.clone())).collect();
    super::shorthand::serialize(&name, &|n| map.get(n).cloned())
}

/// Whether `prop: value` is valid (CSS.supports).
pub fn supports(prop: &str, text: &str) -> bool {
    let cvs = tokenizer::parse_cvs(text);
    parse_decl(prop, &cvs).is_some()
}

/// The computed value of a longhand's specified tokens.
pub fn computed_value(prop: &str, v: &[CV], env: &Env) -> Option<V> {
    let val = parse_value(prop, v)?;
    let c = compute(&val, env);
    Some(super::hooks::computed(prop, c, env))
}

pub fn computed_text(prop: &str, text: &str, env: &Env) -> Option<String> {
    let cvs = tokenizer::parse_cvs(text);
    computed_value(prop, &cvs, env).map(|v| super::hooks::ser_computed(prop, &v))
}

/// The computed initial value.
pub fn initial_text(prop: &str, env: &Env) -> Option<String> {
    let init = with_db(|db| db.props.get(prop).map(|p| p.initial))?;
    computed_text(prop, init, env)
}

pub fn initial_specified(prop: &str) -> Option<&'static str> {
    with_db(|db| db.props.get(prop).map(|p| p.initial))
}
