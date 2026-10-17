// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Selectors Level 4 (practical subset): parsing, specificity and matching.

use super::tokenizer::{parse_cvs, trim, Tok, CV};
use crate::dom::{Document, Namespace, NodeData, NodeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combinator {
    Descendant,
    Child,
    NextSibling,
    SubsequentSibling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PseudoElement {
    Before,
    After,
    Marker,
    Placeholder,
    FirstLetter,
    FirstLine,
    /// `::slotted(compound)`: elements assigned to the subject slot.
    Slotted,
    /// `::part(name…)`: elements exported from the subject host's shadow tree.
    Part,
    /// `::backdrop` of a top-layer element.
    Backdrop,
    /// Valid pseudo-element we do not render (::selection, …).
    Ignored,
}

/// Argument of `::slotted()` / `::part()`.
#[derive(Clone, Debug)]
pub enum ShadowArg {
    Slotted(Vec<Selector>),
    /// Part names, and pseudo-classes that follow (`::part(x):hover`).
    Part(Vec<String>, Compound),
}

#[derive(Clone, Debug)]
pub enum AttrOp {
    Exists,
    Eq(String),
    Includes(String),
    DashMatch(String),
    Prefix(String),
    Suffix(String),
    Substring(String),
}

#[derive(Clone, Debug)]
pub struct AttrSel {
    pub name: String,
    pub op: AttrOp,
    pub ci: bool,
    /// An explicit `s` flag: case-sensitive even for HTML's legacy attributes.
    pub cs: bool,
}

#[derive(Clone, Debug)]
pub enum Pseudo {
    Hover,
    Active,
    Focus,
    FocusWithin,
    FocusVisible,
    FirstChild,
    LastChild,
    OnlyChild,
    FirstOfType,
    LastOfType,
    OnlyOfType,
    NthChild(i32, i32, Option<Vec<Selector>>),
    NthLastChild(i32, i32),
    NthOfType(i32, i32),
    NthLastOfType(i32, i32),
    Not(Vec<Selector>),
    Is(Vec<Selector>),
    Where(Vec<Selector>),
    Has(Vec<(Combinator, Selector)>),
    Root,
    Empty,
    Link,
    Visited,
    AnyLink,
    Checked,
    Disabled,
    Enabled,
    Required,
    Optional,
    ReadOnly,
    ReadWrite,
    PlaceholderShown,
    Target,
    Lang(String),
    Defined,
    Scope,
    Open,
    Default,
    Indeterminate,
    Valid,
    Invalid,
    InRange,
    OutOfRange,
    /// :dir(rtl) when true, :dir(ltr) when false.
    Dir(bool),
    PopoverOpen,
    /// `:modal`: a dialog opened with showModal().
    Modal,
    /// `:host` / `:host(compound)`: the shadow host, from inside its shadow tree.
    Host(Option<Vec<Selector>>),
    /// `:host-context(compound)`: the host when it or an ancestor matches.
    HostContext(Vec<Selector>),
}

#[derive(Clone, Debug, Default)]
pub struct Compound {
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<AttrSel>,
    pub pseudos: Vec<Pseudo>,
}

#[derive(Clone, Debug)]
pub struct Selector {
    /// Compounds right-to-left: parts[0] is the subject.
    pub parts: Vec<Compound>,
    /// combinators[i] joins parts[i] (right) with parts[i+1] (left).
    pub combinators: Vec<Combinator>,
    pub specificity: u32,
    pub pseudo_element: Option<PseudoElement>,
    pub shadow_arg: Option<std::rc::Rc<ShadowArg>>,
}

impl Selector {
    pub fn subject(&self) -> &Compound {
        &self.parts[0]
    }

    /// Serialize (for DevTools; nesting is already resolved).
    pub fn to_css(&self) -> String {
        let mut out = String::new();
        for i in (0..self.parts.len()).rev() {
            out.push_str(&self.parts[i].to_css());
            if i > 0 {
                out.push_str(match self.combinators[i - 1] {
                    Combinator::Descendant => " ",
                    Combinator::Child => " > ",
                    Combinator::NextSibling => " + ",
                    Combinator::SubsequentSibling => " ~ ",
                });
            }
        }
        if out.is_empty() && self.pseudo_element.is_some() {
            out.push('*');
        }
        match self.pseudo_element {
            Some(PseudoElement::Before) => out.push_str("::before"),
            Some(PseudoElement::After) => out.push_str("::after"),
            Some(PseudoElement::Marker) => out.push_str("::marker"),
            Some(PseudoElement::Placeholder) => out.push_str("::placeholder"),
            Some(PseudoElement::FirstLetter) => out.push_str("::first-letter"),
            Some(PseudoElement::FirstLine) => out.push_str("::first-line"),
            Some(PseudoElement::Backdrop) => out.push_str("::backdrop"),
            Some(PseudoElement::Slotted) => out.push_str("::slotted(…)"),
            Some(PseudoElement::Part) => out.push_str("::part(…)"),
            Some(PseudoElement::Ignored) => out.push_str("::…"),
            None => {}
        }
        out
    }
}

fn list_css(l: &[Selector]) -> String {
    l.iter().map(|s| s.to_css()).collect::<Vec<_>>().join(", ")
}

fn nth_css(a: i32, b: i32) -> String {
    match (a, b) {
        (0, b) => b.to_string(),
        (2, 1) => "odd".into(),
        (2, 0) => "even".into(),
        (a, 0) => format!("{a}n"),
        (a, b) if b < 0 => format!("{a}n{b}"),
        (a, b) => format!("{a}n+{b}"),
    }
}

impl Compound {
    pub fn to_css(&self) -> String {
        let mut s = String::new();
        if let Some(t) = &self.tag {
            s.push_str(t);
        }
        if let Some(i) = &self.id {
            s.push('#');
            s.push_str(i);
        }
        for c in &self.classes {
            s.push('.');
            s.push_str(c);
        }
        for a in &self.attrs {
            let (op, v) = match &a.op {
                AttrOp::Exists => ("", None),
                AttrOp::Eq(v) => ("=", Some(v)),
                AttrOp::Includes(v) => ("~=", Some(v)),
                AttrOp::DashMatch(v) => ("|=", Some(v)),
                AttrOp::Prefix(v) => ("^=", Some(v)),
                AttrOp::Suffix(v) => ("$=", Some(v)),
                AttrOp::Substring(v) => ("*=", Some(v)),
            };
            match v {
                Some(v) => s.push_str(&format!("[{}{}\"{}\"{}]", a.name, op, v.replace('"', "\\\""), if a.ci { " i" } else if a.cs { " s" } else { "" })),
                None => s.push_str(&format!("[{}]", a.name)),
            }
        }
        for p in &self.pseudos {
            let t: String = match p {
                Pseudo::Hover => ":hover".into(),
                Pseudo::Active => ":active".into(),
                Pseudo::Focus => ":focus".into(),
                Pseudo::FocusWithin => ":focus-within".into(),
                Pseudo::FocusVisible => ":focus-visible".into(),
                Pseudo::FirstChild => ":first-child".into(),
                Pseudo::LastChild => ":last-child".into(),
                Pseudo::OnlyChild => ":only-child".into(),
                Pseudo::FirstOfType => ":first-of-type".into(),
                Pseudo::LastOfType => ":last-of-type".into(),
                Pseudo::OnlyOfType => ":only-of-type".into(),
                Pseudo::NthChild(a, b, of) => match of {
                    Some(l) => format!(":nth-child({} of {})", nth_css(*a, *b), list_css(l)),
                    None => format!(":nth-child({})", nth_css(*a, *b)),
                },
                Pseudo::NthLastChild(a, b) => format!(":nth-last-child({})", nth_css(*a, *b)),
                Pseudo::NthOfType(a, b) => format!(":nth-of-type({})", nth_css(*a, *b)),
                Pseudo::NthLastOfType(a, b) => format!(":nth-last-of-type({})", nth_css(*a, *b)),
                Pseudo::Not(l) => format!(":not({})", list_css(l)),
                Pseudo::Is(l) => format!(":is({})", list_css(l)),
                Pseudo::Where(l) => format!(":where({})", list_css(l)),
                Pseudo::Has(l) => format!(
                    ":has({})",
                    l.iter()
                        .map(|(c, s)| format!(
                            "{}{}",
                            match c {
                                Combinator::Descendant => "",
                                Combinator::Child => "> ",
                                Combinator::NextSibling => "+ ",
                                Combinator::SubsequentSibling => "~ ",
                            },
                            s.to_css()
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Pseudo::Root => ":root".into(),
                Pseudo::Empty => ":empty".into(),
                Pseudo::Link => ":link".into(),
                Pseudo::Visited => ":visited".into(),
                Pseudo::AnyLink => ":any-link".into(),
                Pseudo::Checked => ":checked".into(),
                Pseudo::Disabled => ":disabled".into(),
                Pseudo::Enabled => ":enabled".into(),
                Pseudo::Required => ":required".into(),
                Pseudo::Optional => ":optional".into(),
                Pseudo::ReadOnly => ":read-only".into(),
                Pseudo::ReadWrite => ":read-write".into(),
                Pseudo::PlaceholderShown => ":placeholder-shown".into(),
                Pseudo::Target => ":target".into(),
                Pseudo::Lang(l) => format!(":lang({l})"),
                Pseudo::Defined => ":defined".into(),
                Pseudo::Scope => ":scope".into(),
                Pseudo::Open => ":open".into(),
                Pseudo::Default => ":default".into(),
                Pseudo::Indeterminate => ":indeterminate".into(),
                Pseudo::Valid => ":valid".into(),
                Pseudo::Invalid => ":invalid".into(),
                Pseudo::InRange => ":in-range".into(),
                Pseudo::OutOfRange => ":out-of-range".into(),
                Pseudo::Dir(rtl) => (if *rtl { ":dir(rtl)" } else { ":dir(ltr)" }).into(),
                Pseudo::PopoverOpen => ":popover-open".into(),
                Pseudo::Modal => ":modal".into(),
                Pseudo::Host(None) => ":host".into(),
                Pseudo::Host(Some(l)) => format!(":host({})", list_css(l)),
                Pseudo::HostContext(l) => format!(":host-context({})", list_css(l)),
            };
            s.push_str(&t);
        }
        s
    }
}

// --------------------------------------------------------------------------
// Parsing
// --------------------------------------------------------------------------

pub fn parse_selector_list_str(s: &str) -> Option<Vec<Selector>> {
    parse_selector_list(&parse_cvs(s), None)
}

/// Parse a comma-separated selector list. `parent` resolves the nesting
/// selector `&`. Returns None if any selector is invalid (whole rule dropped).
pub fn parse_selector_list(cvs: &[CV], parent: Option<&[Selector]>) -> Option<Vec<Selector>> {
    parse_list(cvs, parent, true)
}

/// `implicit_nest`: a nested selector without `&` is relative to the parent
/// (true for style rules; false for arguments of :is()/:where()/:not(), where
/// `&` may appear but nothing is implied).
fn parse_list(cvs: &[CV], parent: Option<&[Selector]>, implicit_nest: bool) -> Option<Vec<Selector>> {
    let mut out = Vec::new();
    for part in super::tokenizer::split_commas(cvs) {
        if part.is_empty() {
            return None;
        }
        out.push(parse_selector_ex(part, parent, implicit_nest)?);
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Like parse_selector_list but drops invalid entries (forgiving lists for :is/:where).
fn parse_forgiving(cvs: &[CV], parent: Option<&[Selector]>) -> Vec<Selector> {
    super::tokenizer::split_commas(cvs)
        .into_iter()
        .filter_map(|p| if p.is_empty() { None } else { parse_selector_ex(p, parent, false) })
        .collect()
}

pub fn parse_selector(cvs: &[CV], parent: Option<&[Selector]>) -> Option<Selector> {
    parse_selector_ex(cvs, parent, true)
}

// The final flush!() leaves a dead store to `cur_empty`.
#[allow(unused_assignments)]
fn parse_selector_ex(cvs: &[CV], parent: Option<&[Selector]>, implicit_nest: bool) -> Option<Selector> {
    let cvs = trim(cvs);
    let mut compounds: Vec<Compound> = Vec::new(); // left-to-right
    let mut combs: Vec<Combinator> = Vec::new();
    let mut cur = Compound::default();
    let mut cur_empty = true;
    let mut pending: Option<Combinator> = None;
    let mut pseudo_el: Option<PseudoElement> = None;
    let mut shadow_arg: Option<ShadowArg> = None;
    let mut uses_nesting = false;
    let mut i = 0;

    macro_rules! flush {
        () => {
            if !cur_empty {
                if !compounds.is_empty() {
                    combs.push(pending.take().unwrap_or(Combinator::Descendant));
                } else if pending.is_some() {
                    // Leading combinator (relative selector) only valid in nesting.
                    if parent.is_some() {
                        let comb = pending.take().unwrap();
                        compounds.push(amp_compound(parent.unwrap()));
                        uses_nesting = true;
                        combs.push(comb);
                    } else {
                        return None;
                    }
                }
                compounds.push(std::mem::take(&mut cur));
                cur_empty = true;
            }
        };
    }

    while i < cvs.len() {
        let c = &cvs[i];
        if pseudo_el.is_some() && !c.is_ws() && !matches!(c, CV::T(Tok::Colon)) {
            return None; // nothing may follow a pseudo-element (except pseudo-classes we ignore)
        }
        match c {
            CV::T(Tok::Ws) => {
                flush!();
                // whitespace may be a descendant combinator
                if !compounds.is_empty() && pending.is_none() {
                    pending = Some(Combinator::Descendant);
                }
            }
            CV::T(Tok::Delim('>')) | CV::T(Tok::Delim('+')) | CV::T(Tok::Delim('~')) => {
                flush!();
                let comb = match c {
                    CV::T(Tok::Delim('>')) => Combinator::Child,
                    CV::T(Tok::Delim('+')) => Combinator::NextSibling,
                    _ => Combinator::SubsequentSibling,
                };
                if compounds.is_empty() && parent.is_none() {
                    return None;
                }
                // Two combinators in a row ("div ++ p") are invalid.
                if matches!(pending, Some(p) if p != Combinator::Descendant) {
                    return None;
                }
                pending = Some(comb);
            }
            CV::T(Tok::Ident(name)) => {
                if !cur_empty && (cur.tag.is_some() || !cur.classes.is_empty() || cur.id.is_some()) {
                    return None;
                }
                // namespace prefix "svg|rect"
                if matches!(cvs.get(i + 1), Some(CV::T(Tok::Delim('|')))) {
                    i += 2;
                    match cvs.get(i) {
                        Some(CV::T(Tok::Ident(n))) => cur.tag = Some(n.to_ascii_lowercase()),
                        Some(CV::T(Tok::Delim('*'))) => {}
                        _ => return None,
                    }
                    cur_empty = false;
                    i += 1;
                    continue;
                }
                cur.tag = Some(name.to_ascii_lowercase());
                cur_empty = false;
            }
            CV::T(Tok::Delim('*')) => {
                if matches!(cvs.get(i + 1), Some(CV::T(Tok::Delim('|')))) {
                    i += 1;
                }
                cur_empty = false;
            }
            CV::T(Tok::Delim('|')) => {}
            CV::T(Tok::Hash(h, _)) => {
                cur.id = Some(h.clone());
                cur_empty = false;
            }
            CV::T(Tok::Delim('.')) => match cvs.get(i + 1) {
                Some(CV::T(Tok::Ident(cls))) => {
                    cur.classes.push(cls.clone());
                    cur_empty = false;
                    i += 1;
                }
                _ => return None,
            },
            CV::T(Tok::Delim('&')) => {
                let p = parent?;
                cur.pseudos.push(Pseudo::Is(p.to_vec()));
                cur_empty = false;
                uses_nesting = true;
            }
            CV::Block('[', inner) => {
                cur.attrs.push(parse_attr(inner)?);
                cur_empty = false;
            }
            CV::T(Tok::Colon) => {
                // pseudo-element?
                if matches!(cvs.get(i + 1), Some(CV::T(Tok::Colon))) {
                    i += 2;
                    let name = match cvs.get(i) {
                        Some(CV::T(Tok::Ident(n))) => n.to_ascii_lowercase(),
                        Some(CV::Func(n, _)) => n.to_ascii_lowercase(),
                        _ => return None,
                    };
                    if pseudo_el.is_some() {
                        return None; // only one pseudo-element (we do not chain ::part()::before)
                    }
                    if let (Some(CV::Func(_, args)), "slotted" | "part") = (cvs.get(i), name.as_str()) {
                        if name == "slotted" {
                            let list = parse_list(args, None, false)?;
                            // A compound selector only.
                            if list.len() != 1 || list[0].parts.len() != 1 || list[0].pseudo_element.is_some() {
                                return None;
                            }
                            shadow_arg = Some(ShadowArg::Slotted(list));
                            pseudo_el = Some(PseudoElement::Slotted);
                        } else {
                            let names: Vec<String> = args.iter().filter(|c| !c.is_ws()).map(|c| c.ident().map(|s| s.to_string())).collect::<Option<Vec<_>>>()?;
                            if names.is_empty() {
                                return None;
                            }
                            shadow_arg = Some(ShadowArg::Part(names, Compound::default()));
                            pseudo_el = Some(PseudoElement::Part);
                        }
                        cur_empty = false;
                        i += 1;
                        continue;
                    }
                    pseudo_el = Some(match name.as_str() {
                        "before" => PseudoElement::Before,
                        "after" => PseudoElement::After,
                        "marker" => PseudoElement::Marker,
                        "placeholder" | "-webkit-input-placeholder" => PseudoElement::Placeholder,
                        "first-letter" => PseudoElement::FirstLetter,
                        "first-line" => PseudoElement::FirstLine,
                        "backdrop" => PseudoElement::Backdrop,
                        "selection" | "file-selector-button" | "cue" | "slotted" | "part" | "highlight" | "target-text"
                        | "spelling-error" | "grammar-error" | "details-content" | "picker" | "picker-icon" | "checkmark" | "column" | "scroll-marker" | "scroll-marker-group" | "search-text" => PseudoElement::Ignored,
                        n if n.starts_with("view-transition") || n.starts_with("-webkit-") => PseudoElement::Ignored,
                        _ => return None,
                    });
                    cur_empty = false;
                } else {
                    i += 1;
                    match cvs.get(i) {
                        Some(CV::T(Tok::Ident(n))) if matches!(shadow_arg, Some(ShadowArg::Part(..))) => {
                            // `::part(x):hover` — state of the part element.
                            let ps = simple_pseudo(&n.to_ascii_lowercase())?;
                            if let Some(ShadowArg::Part(_, c)) = &mut shadow_arg {
                                c.pseudos.push(ps);
                            }
                        }
                        Some(CV::T(Tok::Ident(n))) => {
                            let n = n.to_ascii_lowercase();
                            // Legacy single-colon pseudo-elements.
                            match n.as_str() {
                                "before" => pseudo_el = Some(PseudoElement::Before),
                                "after" => pseudo_el = Some(PseudoElement::After),
                                "first-letter" => pseudo_el = Some(PseudoElement::FirstLetter),
                                "first-line" => pseudo_el = Some(PseudoElement::FirstLine),
                                _ => cur.pseudos.push(simple_pseudo(&n)?),
                            }
                        }
                        Some(CV::Func(n, args)) => {
                            // `&` inside :is()/:where()/:not()/:has() also makes
                            // this selector explicitly nested (no implicit `& `).
                            if parent.is_some() && contains_amp(args) {
                                uses_nesting = true;
                            }
                            cur.pseudos.push(functional_pseudo(&n.to_ascii_lowercase(), args, parent)?)
                        }
                        _ => return None,
                    }
                    cur_empty = false;
                }
            }
            _ => return None,
        }
        i += 1;
    }
    flush!();
    if compounds.is_empty() {
        return None;
    }
    if pending.is_some() && pending != Some(Combinator::Descendant) {
        return None; // trailing combinator
    }
    // Nested selector without '&' is implicitly a descendant of the parent.
    if let (Some(p), false, true) = (parent, uses_nesting, implicit_nest) {
        compounds.insert(0, amp_compound(p));
        combs.insert(0, Combinator::Descendant);
    }
    compounds.reverse();
    combs.reverse();
    let mut sel = Selector { parts: compounds, combinators: combs, specificity: 0, pseudo_element: pseudo_el, shadow_arg: shadow_arg.map(std::rc::Rc::new) };
    sel.specificity = specificity(&sel);
    Some(sel)
}

fn amp_compound(parent: &[Selector]) -> Compound {
    Compound { pseudos: vec![Pseudo::Is(parent.to_vec())], ..Default::default() }
}

fn parse_attr(inner: &[CV]) -> Option<AttrSel> {
    // [name], [*|name], [|name], then optionally an operator (its delims
    // adjacent), an ident or string value and an `i`/`s` flag; nothing else.
    let toks: Vec<(usize, &CV)> = inner.iter().enumerate().filter(|(_, c)| !c.is_ws()).collect();
    let mut i = 0;
    let delim = |k: usize, ch: char| matches!(toks.get(k), Some((_, CV::T(Tok::Delim(d)))) if *d == ch);
    let adjacent = |k: usize| toks.get(k).zip(toks.get(k + 1)).map(|(a, b)| b.0 == a.0 + 1).unwrap_or(false);
    let name = match toks.first()?.1 {
        CV::T(Tok::Ident(n)) => {
            if delim(1, '|') && !delim(2, '=') {
                // A namespace prefix: only declared ones are valid (none are).
                return None;
            }
            n.to_ascii_lowercase()
        }
        CV::T(Tok::Delim('*')) if delim(1, '|') && adjacent(0) && adjacent(1) => {
            i = 2;
            toks.get(i)?.1.ident()?.to_ascii_lowercase()
        }
        CV::T(Tok::Delim('|')) if adjacent(0) => {
            i = 1;
            toks.get(i)?.1.ident()?.to_ascii_lowercase()
        }
        _ => return None,
    };
    i += 1;
    if i >= toks.len() {
        return Some(AttrSel { name, op: AttrOp::Exists, ci: false, cs: false });
    }
    let op = match toks.get(i)?.1 {
        CV::T(Tok::Delim('=')) => {
            i += 1;
            "="
        }
        CV::T(Tok::Delim(d)) if matches!(d, '~' | '|' | '^' | '$' | '*') => {
            if !delim(i + 1, '=') || !adjacent(i) {
                return None;
            }
            i += 2;
            match d {
                '~' => "~=",
                '|' => "|=",
                '^' => "^=",
                '$' => "$=",
                _ => "*=",
            }
        }
        _ => return None,
    };
    let val = match toks.get(i)?.1 {
        CV::T(Tok::Ident(v)) | CV::T(Tok::Str(v)) => v.clone(),
        _ => return None,
    };
    i += 1;
    let (mut ci, mut cs) = (false, false);
    if let Some((_, c)) = toks.get(i) {
        if c.is_ident("i") {
            ci = true;
        } else if c.is_ident("s") {
            cs = true;
        } else {
            return None;
        }
        i += 1;
    }
    if i < toks.len() {
        return None;
    }
    let op = match op {
        "=" => AttrOp::Eq(val),
        "~=" => AttrOp::Includes(val),
        "|=" => AttrOp::DashMatch(val),
        "^=" => AttrOp::Prefix(val),
        "$=" => AttrOp::Suffix(val),
        _ => AttrOp::Substring(val),
    };
    Some(AttrSel { name, op, ci, cs })
}

fn simple_pseudo(n: &str) -> Option<Pseudo> {
    Some(match n {
        "hover" => Pseudo::Hover,
        "active" => Pseudo::Active,
        "focus" => Pseudo::Focus,
        "focus-within" => Pseudo::FocusWithin,
        "focus-visible" => Pseudo::FocusVisible,
        "first-child" => Pseudo::FirstChild,
        "last-child" => Pseudo::LastChild,
        "only-child" => Pseudo::OnlyChild,
        "first-of-type" => Pseudo::FirstOfType,
        "last-of-type" => Pseudo::LastOfType,
        "only-of-type" => Pseudo::OnlyOfType,
        "root" => Pseudo::Root,
        "empty" => Pseudo::Empty,
        "link" => Pseudo::Link,
        "visited" => Pseudo::Visited,
        "any-link" => Pseudo::AnyLink,
        "checked" => Pseudo::Checked,
        "disabled" => Pseudo::Disabled,
        "enabled" => Pseudo::Enabled,
        "required" => Pseudo::Required,
        "optional" => Pseudo::Optional,
        "read-only" => Pseudo::ReadOnly,
        "read-write" => Pseudo::ReadWrite,
        "placeholder-shown" => Pseudo::PlaceholderShown,
        "target" => Pseudo::Target,
        "defined" => Pseudo::Defined,
        "scope" => Pseudo::Scope,
        "open" => Pseudo::Open,
        "popover-open" => Pseudo::PopoverOpen,
        "modal" => Pseudo::Modal,
        "default" => Pseudo::Default,
        "indeterminate" => Pseudo::Indeterminate,
        "valid" | "user-valid" => Pseudo::Valid,
        "invalid" | "user-invalid" => Pseudo::Invalid,
        "in-range" => Pseudo::InRange,
        "out-of-range" => Pseudo::OutOfRange,
        // Valid selectors that never match in this engine (shadow DOM, media state, …).
        "host" => Pseudo::Host(None),
        "autofill" | "-webkit-autofill" | "fullscreen" | "modal" | "picture-in-picture" | "playing" | "paused" | "seeking" | "buffering" | "stalled" | "muted"
        | "volume-locked" | "local-link" | "target-within" | "blank" | "current" | "past" | "future" | "popover-closed" | "closed" | "-webkit-full-screen" | "xr-overlay" | "active-view-transition" => {
            Pseudo::Is(Vec::new())
        }
        _ => return None,
    })
}

fn contains_amp(args: &[CV]) -> bool {
    args.iter().any(|c| match c {
        CV::T(Tok::Delim('&')) => true,
        CV::Func(_, a) | CV::Block(_, a) => contains_amp(a),
        _ => false,
    })
}

fn functional_pseudo(n: &str, args: &[CV], parent: Option<&[Selector]>) -> Option<Pseudo> {
    let parent = if contains_amp(args) { parent } else { None };
    Some(match n {
        "not" => Pseudo::Not(parse_list(args, parent, false)?),
        "is" | "matches" | "-webkit-any" | "-moz-any" => Pseudo::Is(parse_forgiving(args, parent)),
        "where" => Pseudo::Where(parse_forgiving(args, parent)),
        "has" => {
            let mut rel = Vec::new();
            for part in super::tokenizer::split_commas(args) {
                let part = trim(part);
                let (comb, rest) = match part.first() {
                    Some(CV::T(Tok::Delim('>'))) => (Combinator::Child, &part[1..]),
                    Some(CV::T(Tok::Delim('+'))) => (Combinator::NextSibling, &part[1..]),
                    Some(CV::T(Tok::Delim('~'))) => (Combinator::SubsequentSibling, &part[1..]),
                    _ => (Combinator::Descendant, part),
                };
                rel.push((comb, parse_selector(rest, None)?));
            }
            Pseudo::Has(rel)
        }
        "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type" => {
            // Split "An+B of S"
            let mut of_idx = None;
            for (i, c) in args.iter().enumerate() {
                if c.is_ident("of") {
                    of_idx = Some(i);
                }
            }
            let (anb_part, of_sel) = match of_idx {
                Some(i) => (&args[..i], Some(parse_selector_list(&args[i + 1..], parent)?)),
                None => (args, None),
            };
            let (a, b) = parse_anb(anb_part)?;
            match n {
                "nth-child" => Pseudo::NthChild(a, b, of_sel),
                "nth-last-child" => Pseudo::NthLastChild(a, b),
                "nth-of-type" => Pseudo::NthOfType(a, b),
                _ => Pseudo::NthLastOfType(a, b),
            }
        }
        "lang" => Pseudo::Lang(super::tokenizer::serialize(args).trim().trim_matches('"').to_ascii_lowercase()),
        "dir" => match super::tokenizer::serialize(args).trim().to_ascii_lowercase().as_str() {
            "rtl" => Pseudo::Dir(true),
            "ltr" => Pseudo::Dir(false),
            _ => Pseudo::Is(Vec::new()),
        },
        "host" => {
            let l = parse_list(args, None, false)?;
            if l.iter().any(|s| s.parts.len() != 1 || s.pseudo_element.is_some()) {
                return None;
            }
            Pseudo::Host(Some(l))
        }
        "host-context" => {
            let l = parse_list(args, None, false)?;
            if l.iter().any(|s| s.parts.len() != 1 || s.pseudo_element.is_some()) {
                return None;
            }
            Pseudo::HostContext(l)
        }
        "state" | "nth-col" | "nth-last-col" | "current" | "active-view-transition-type" | "heading" => Pseudo::Is(Vec::new()),
        _ => return None,
    })
}

/// Parse An+B microsyntax by serializing the tokens and parsing the text.
fn parse_anb(cvs: &[CV]) -> Option<(i32, i32)> {
    // The An+B microsyntax: whitespace only around the sign that joins B.
    let s = super::tokenizer::serialize(cvs).to_ascii_lowercase();
    let t = s.trim_matches(|c: char| c.is_ascii_whitespace());
    match t {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    let b = t.as_bytes();
    let digits = |from: usize| -> usize {
        let mut k = from;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        k
    };
    // A part: [+-]?\d*n (contiguous)
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    let d_end = digits(i);
    if d_end < b.len() && b[d_end] == b'n' {
        let a_str = &t[..d_end];
        let a: i32 = match a_str {
            "" | "+" => 1,
            "-" => -1,
            x => x.parse().ok()?,
        };
        let mut k = d_end + 1;
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        if k == b.len() {
            return Some((a, 0));
        }
        // "n-1" also arrives here; a sign, optional whitespace, then digits.
        // (Number tokens serialize without their '+': "2n+1" reads "2n1".)
        let neg = match b[k] {
            b'+' => false,
            b'-' => true,
            c if c.is_ascii_digit() => {
                let e = digits(k);
                if e != b.len() {
                    return None;
                }
                return Some((a, t[k..e].parse().ok()?));
            }
            _ => return None,
        };
        k += 1;
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        let e = digits(k);
        if e == k || e != b.len() {
            return None;
        }
        let v: i32 = t[k..e].parse().ok()?;
        return Some((a, if neg { -v } else { v }));
    }
    // B alone: [+-]?\d+
    if d_end == i || d_end != b.len() {
        return None;
    }
    Some((0, t.trim_start_matches('+').parse().ok()?))
}

fn specificity(sel: &Selector) -> u32 {
    let mut a = 0u32;
    let mut b = 0u32;
    let mut c = 0u32;
    for comp in &sel.parts {
        let (x, y, z) = compound_spec(comp);
        a += x;
        b += y;
        c += z;
    }
    if sel.pseudo_element.is_some() {
        c += 1;
    }
    if let Some(ShadowArg::Slotted(l)) = sel.shadow_arg.as_deref() {
        let (x, y, z) = max_spec(l);
        a += x;
        b += y;
        c += z;
    }
    (a.min(255) << 16) | (b.min(255) << 8) | c.min(255)
}

fn max_spec(list: &[Selector]) -> (u32, u32, u32) {
    let m = list.iter().map(|s| s.specificity).max().unwrap_or(0);
    (m >> 16, (m >> 8) & 255, m & 255)
}

fn compound_spec(comp: &Compound) -> (u32, u32, u32) {
    let mut a = comp.id.is_some() as u32;
    let mut b = (comp.classes.len() + comp.attrs.len()) as u32;
    let mut c = comp.tag.is_some() as u32;
    for p in &comp.pseudos {
        match p {
            Pseudo::Where(_) => {}
            Pseudo::Host(Some(l)) | Pseudo::HostContext(l) => {
                let (x, y, z) = max_spec(l);
                a += x;
                b += y + 1;
                c += z;
            }
            Pseudo::Is(l) | Pseudo::Not(l) => {
                let (x, y, z) = max_spec(l);
                a += x;
                b += y;
                c += z;
            }
            Pseudo::Has(l) => {
                let m = l.iter().map(|(_, s)| s.specificity).max().unwrap_or(0);
                a += m >> 16;
                b += (m >> 8) & 255;
                c += m & 255;
            }
            Pseudo::NthChild(_, _, Some(l)) => {
                let (x, y, z) = max_spec(l);
                a += x;
                b += y + 1;
                c += z;
            }
            _ => b += 1,
        }
    }
    (a, b, c)
}

// --------------------------------------------------------------------------
// Matching
// --------------------------------------------------------------------------

/// Dynamic state needed to evaluate pseudo-classes.
pub struct MatchState<'a> {
    pub hover_chain: &'a [NodeId],
    pub focus: Option<NodeId>,
    pub active_chain: &'a [NodeId],
    pub target: Option<NodeId>,
    pub scope: Option<NodeId>,
    /// Host and root of the shadow tree whose rules are being matched (None
    /// for document rules). Inside that context the host is featureless.
    pub host: Option<NodeId>,
    pub shadow_root: Option<NodeId>,
}

impl MatchState<'_> {
    /// State for matching outside any shadow tree (keeps the dynamic state).
    fn outer(&self) -> MatchState<'_> {
        MatchState { hover_chain: self.hover_chain, focus: self.focus, active_chain: self.active_chain, target: self.target, scope: None, host: None, shadow_root: None }
    }
}

/// Parent for combinator matching: stops at the shadow root, except that the
/// host is the parent of its shadow tree's top-level elements.
fn match_parent(doc: &Document, node: NodeId, st: &MatchState) -> Option<NodeId> {
    if st.host == Some(node) {
        return None;
    }
    match doc.parent(node) {
        Some(p) if doc.is_element(p) => Some(p),
        Some(p) if st.shadow_root == Some(p) => st.host,
        _ => None,
    }
}

thread_local! {
    /// While set (during a style pass), every `:has()` evaluation in document
    /// scope: (anchor, address of the relative selector list) -> (result, list length).
    /// Incremental restyles replay these to find anchors whose result changed.
    pub static HAS_LOG: std::cell::RefCell<Option<std::collections::HashMap<(NodeId, usize), (bool, u32)>>> = const { std::cell::RefCell::new(None) };
}

/// Categories of [`STRUCT_LOG`] (bit shifts of 4-bit fields): the node's
/// children were matched by selectors that depend on their preceding
/// siblings / following siblings / sibling combinators; the node was tested
/// with `:empty`. Each field holds the `INV_*` relations of the compounds
/// that did, relative to the element being styled.
pub const STRUCT_FWD: u32 = 0;
pub const STRUCT_BWD: u32 = 4;
pub const STRUCT_SIB: u32 = 8;
pub const STRUCT_EMPTY: u32 = 12;

thread_local! {
    /// While set (during a style pass), which nodes' child lists matching
    /// depended on (incremental restyles of child-list changes read it).
    pub static STRUCT_LOG: std::cell::RefCell<Option<std::collections::HashMap<NodeId, u16>>> = const { std::cell::RefCell::new(None) };
    /// `INV_*` relation of the compound being matched (to the styled element).
    static CUR_KIND: std::cell::Cell<u8> = const { std::cell::Cell::new(super::INV_SELF) };
}

/// Record a [`STRUCT_LOG`] dependency of the compound being matched.
#[inline]
fn log_struct(node: Option<NodeId>, cat: u32) {
    if let Some(n) = node {
        STRUCT_LOG.with(|l| {
            if let Some(m) = l.borrow_mut().as_mut() {
                let k = CUR_KIND.with(|k| k.get()) as u16;
                *m.entry(n).or_insert(0) |= k << cat;
            }
        });
    }
}

thread_local! {
    /// The relative selector of the `:has()` being evaluated, its anchor and
    /// leading combinator: its leftmost compound must relate to the anchor.
    static HAS_REL: std::cell::Cell<Option<(*const Selector, NodeId, Combinator)>> = const { std::cell::Cell::new(None) };
}

/// Does `node` (matched by the leftmost compound of a relative selector)
/// relate to the `:has()` anchor through the leading combinator?
fn rel_anchored(doc: &Document, node: NodeId, anchor: NodeId, comb: Combinator) -> bool {
    match comb {
        Combinator::Child => doc.parent(node) == Some(anchor),
        Combinator::Descendant => {
            let mut cur = doc.parent(node);
            while let Some(p) = cur {
                if p == anchor {
                    return true;
                }
                cur = doc.parent(p);
            }
            false
        }
        Combinator::NextSibling => doc.prev_element_sibling(node) == Some(anchor),
        Combinator::SubsequentSibling => {
            let mut cur = doc.prev_element_sibling(node);
            while let Some(p) = cur {
                if p == anchor {
                    return true;
                }
                cur = doc.prev_element_sibling(p);
            }
            false
        }
    }
}

/// `:has()`: some element in the anchor's scope matches a relative selector
/// whose leftmost compound relates to the anchor by the leading combinator.
fn has_matches(rel: &[(Combinator, Selector)], doc: &Document, node: NodeId, st: &MatchState, depth: u32) -> bool {
    rel.iter().any(|(comb, s)| {
        let simple = s.parts.len() == 1;
        let saved = HAS_REL.with(|h| h.replace(if simple { None } else { Some((s as *const Selector, node, *comb)) }));
        let m = |d: NodeId| doc.is_element(d) && match_from(s, 0, doc, d, st, depth + 1);
        let r = match comb {
            Combinator::Descendant => doc.descendants(node).any(m),
            Combinator::Child if simple => doc.element_children(node).any(m),
            Combinator::Child => doc.descendants(node).any(m),
            Combinator::NextSibling if simple => doc.next_element_sibling(node).map(m).unwrap_or(false),
            Combinator::NextSibling => doc.next_element_sibling(node).map(|d| m(d) || doc.descendants(d).any(m)).unwrap_or(false),
            Combinator::SubsequentSibling => {
                let mut cur = doc.next_element_sibling(node);
                let mut found = false;
                while let Some(d) = cur {
                    if m(d) || (!simple && doc.descendants(d).any(m)) {
                        found = true;
                        break;
                    }
                    cur = doc.next_element_sibling(d);
                }
                found
            }
        };
        HAS_REL.with(|h| h.set(saved));
        r
    })
}

/// Re-evaluate a `:has()` recorded in [`HAS_LOG`].
///
/// # Safety
/// `ptr`/`len` must come from `HAS_LOG` entries of a pass over rule indexes
/// that are still alive and unchanged.
/// `HAS_LOG` key marking a shadow host whose tree's rules use `:has()`.
pub const HAS_HOST_MARK: usize = 1;

pub unsafe fn replay_has(ptr: usize, len: u32, doc: &Document, node: NodeId, st: &MatchState) -> bool {
    let rel = unsafe { std::slice::from_raw_parts(ptr as *const (Combinator, Selector), len as usize) };
    has_matches(rel, doc, node, st, 0)
}

/// Visit every compound of a selector, including those nested in pseudo-class
/// arguments; `top` is true for the subject compound of a top-level selector.
pub fn visit_compounds(sel: &Selector, top: bool, f: &mut dyn FnMut(&Compound, bool)) {
    for (i, c) in sel.parts.iter().enumerate() {
        f(c, top && i == 0);
        for p in &c.pseudos {
            match p {
                Pseudo::Not(l) | Pseudo::Is(l) | Pseudo::Where(l) | Pseudo::HostContext(l) | Pseudo::NthChild(_, _, Some(l)) | Pseudo::Host(Some(l)) => {
                    for s in l {
                        visit_compounds(s, false, f);
                    }
                }
                Pseudo::Has(rel) => {
                    for (_, s) in rel {
                        visit_compounds(s, false, f);
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(a) = &sel.shadow_arg {
        match &**a {
            ShadowArg::Slotted(l) => {
                for s in l {
                    visit_compounds(s, false, f);
                }
            }
            ShadowArg::Part(_, c) => f(c, false),
        }
    }
}

pub fn matches(sel: &Selector, doc: &Document, node: NodeId, st: &MatchState) -> bool {
    match_from(sel, 0, doc, node, st, 0)
}

pub fn matches_any(list: &[Selector], doc: &Document, node: NodeId, st: &MatchState) -> bool {
    list.iter().any(|s| s.pseudo_element.is_none() && matches(s, doc, node, st))
}

fn match_from(sel: &Selector, idx: usize, doc: &Document, node: NodeId, st: &MatchState, depth: u32) -> bool {
    if depth > 32 {
        return false;
    }
    // The compound's relation to the element being styled (for STRUCT_LOG);
    // a nested selector's subject inherits that of the enclosing compound.
    let kind = if idx > 0 {
        if matches!(sel.combinators[idx - 1], Combinator::NextSibling | Combinator::SubsequentSibling) {
            super::INV_SIB
        } else {
            super::INV_DESC
        }
    } else if depth == 0 {
        super::INV_SELF
    } else {
        CUR_KIND.with(|k| k.get())
    };
    let saved = CUR_KIND.with(|k| k.replace(kind));
    let ok = match_compound(&sel.parts[idx], doc, node, st, depth);
    if ok && idx + 1 < sel.parts.len() && matches!(sel.combinators[idx], Combinator::NextSibling | Combinator::SubsequentSibling) && st.host != Some(node) {
        log_struct(doc.parent(node), STRUCT_SIB);
    }
    CUR_KIND.with(|k| k.set(saved));
    if !ok {
        return false;
    }
    if idx + 1 >= sel.parts.len() {
        // The leftmost compound of a `:has()` relative selector.
        if let Some((ptr, anchor, comb)) = HAS_REL.with(|h| h.get()) {
            if std::ptr::eq(ptr, sel) {
                return rel_anchored(doc, node, anchor, comb);
            }
        }
        return true;
    }
    match sel.combinators[idx] {
        Combinator::Child => match match_parent(doc, node, st) {
            Some(p) => match_from(sel, idx + 1, doc, p, st, depth),
            None => false,
        },
        Combinator::Descendant => {
            let mut cur = match_parent(doc, node, st);
            while let Some(p) = cur {
                if match_from(sel, idx + 1, doc, p, st, depth) {
                    return true;
                }
                cur = match_parent(doc, p, st);
            }
            false
        }
        _ if st.host == Some(node) => false,
        Combinator::NextSibling => match doc.prev_element_sibling(node) {
            Some(s) => match_from(sel, idx + 1, doc, s, st, depth),
            None => false,
        },
        Combinator::SubsequentSibling => {
            let mut cur = doc.prev_element_sibling(node);
            while let Some(s) = cur {
                if match_from(sel, idx + 1, doc, s, st, depth) {
                    return true;
                }
                cur = doc.prev_element_sibling(s);
            }
            false
        }
    }
}

fn nth(a: i32, b: i32, pos: i32) -> bool {
    if a == 0 {
        pos == b
    } else {
        let d = pos - b;
        d % a == 0 && d / a >= 0
    }
}

fn is_form_control(name: &str) -> bool {
    matches!(name, "input" | "button" | "select" | "textarea" | "option" | "optgroup" | "fieldset")
}

fn match_compound(c: &Compound, doc: &Document, node: NodeId, st: &MatchState, depth: u32) -> bool {
    let el = match doc.element(node) {
        Some(e) => e,
        None => return false,
    };
    if st.host == Some(node) {
        return match_featureless_host(c, doc, node, st, depth);
    }
    if let Some(t) = &c.tag {
        let ok = if el.ns == Namespace::Html { &el.name == t } else { el.name.eq_ignore_ascii_case(t) };
        if !ok {
            return false;
        }
    }
    if let Some(id) = &c.id {
        if el.id.as_deref() != Some(id.as_str()) {
            return false;
        }
    }
    for cls in &c.classes {
        if !el.has_class(cls) {
            return false;
        }
    }
    for a in &c.attrs {
        let v = match el.attr(&a.name) {
            Some(v) => v,
            None => return false,
        };
        // HTML §4.16.2: in HTML documents these null-namespace attributes'
        // values match case-insensitively on HTML elements (unless `s`).
        let legacy_ci = !a.ci
            && !a.cs
            && el.ns == Namespace::Html
            && !doc.is_xml
            && el.attrs.iter().any(|x| x.name == a.name && x.ns.is_none())
            && matches!(
                a.name.as_str(),
                "accept" | "accept-charset" | "align" | "alink" | "axis" | "bgcolor" | "charset" | "checked" | "clear" | "codetype" | "color" | "compact" | "declare" | "defer" | "dir" | "direction" | "disabled" | "enctype" | "face" | "frame" | "hreflang" | "http-equiv" | "lang" | "language" | "link" | "media" | "method" | "multiple" | "nohref" | "noresize" | "noshade" | "nowrap" | "readonly" | "rel" | "rev" | "rules" | "scope" | "scrolling" | "selected" | "shape" | "target" | "text" | "type" | "valign" | "valuetype" | "vlink"
            );
        let (v, cmp): (std::borrow::Cow<str>, bool) = (std::borrow::Cow::Borrowed(v), a.ci || legacy_ci);
        let eq = |x: &str, y: &str| if cmp { x.eq_ignore_ascii_case(y) } else { x == y };
        let ok = match &a.op {
            AttrOp::Exists => true,
            AttrOp::Eq(x) => eq(&v, x),
            AttrOp::Includes(x) => v.split_ascii_whitespace().any(|w| eq(w, x)),
            AttrOp::DashMatch(x) => eq(&v, x) || (v.len() > x.len() && v.get(..x.len()).map(|p| eq(p, x)).unwrap_or(false) && v.as_bytes()[x.len()] == b'-'),
            AttrOp::Prefix(x) => !x.is_empty() && if cmp { v.to_ascii_lowercase().starts_with(&x.to_ascii_lowercase()) } else { v.starts_with(x.as_str()) },
            AttrOp::Suffix(x) => !x.is_empty() && if cmp { v.to_ascii_lowercase().ends_with(&x.to_ascii_lowercase()) } else { v.ends_with(x.as_str()) },
            AttrOp::Substring(x) => !x.is_empty() && if cmp { v.to_ascii_lowercase().contains(&x.to_ascii_lowercase()) } else { v.contains(x.as_str()) },
        };
        if !ok {
            return false;
        }
    }
    for p in &c.pseudos {
        match p {
            Pseudo::FirstChild | Pseudo::FirstOfType | Pseudo::NthOfType(..) | Pseudo::NthChild(..) => {
                log_struct(doc.parent(node), STRUCT_FWD);
            }
            Pseudo::LastChild | Pseudo::LastOfType | Pseudo::NthLastChild(..) | Pseudo::NthLastOfType(..) => {
                log_struct(doc.parent(node), STRUCT_BWD);
            }
            Pseudo::OnlyChild | Pseudo::OnlyOfType => {
                log_struct(doc.parent(node), STRUCT_FWD);
                log_struct(doc.parent(node), STRUCT_BWD);
            }
            Pseudo::Empty => {
                log_struct(Some(node), STRUCT_EMPTY);
            }
            _ => {}
        }
        let ok = match p {
            Pseudo::Hover => st.hover_chain.contains(&node),
            Pseudo::Active => st.active_chain.contains(&node),
            // A host with delegatesFocus also matches while focus is inside its shadow tree.
            Pseudo::Focus | Pseudo::FocusVisible => {
                st.focus == Some(node)
                    || doc.shadow_roots.get(&node).and_then(|r| doc.shadow_info.get(r)).map(|i| i.delegates_focus).unwrap_or(false) && st.focus.map(|f| f != node && doc.shadow_including_contains(node, f)).unwrap_or(false)
            }
            Pseudo::FocusWithin => st.focus.map(|f| doc.shadow_including_contains(node, f)).unwrap_or(false),
            // (Selectors 4: an element without a parent element, such as the
            // root or a detached element, is its own first and last child.)
            Pseudo::FirstChild => doc.prev_element_sibling(node).is_none(),
            Pseudo::LastChild => doc.next_element_sibling(node).is_none(),
            Pseudo::OnlyChild => doc.prev_element_sibling(node).is_none() && doc.next_element_sibling(node).is_none(),
            Pseudo::FirstOfType | Pseudo::LastOfType | Pseudo::OnlyOfType | Pseudo::NthOfType(..) | Pseudo::NthLastOfType(..) => {
                let sibs: Vec<NodeId> = match doc.parent(node) {
                    Some(parent) => doc.element_children(parent).filter(|&s| doc.tag(s) == el.name).collect(),
                    None => vec![node],
                };
                let pos = sibs.iter().position(|&s| s == node).unwrap_or(0) as i32;
                let len = sibs.len() as i32;
                match p {
                    Pseudo::FirstOfType => pos == 0,
                    Pseudo::LastOfType => pos == len - 1,
                    Pseudo::OnlyOfType => len == 1,
                    Pseudo::NthOfType(a, b) => nth(*a, *b, pos + 1),
                    Pseudo::NthLastOfType(a, b) => nth(*a, *b, len - pos),
                    _ => false,
                }
            }
            Pseudo::NthChild(a, b, of) => {
                let sibs: Vec<NodeId> = match doc.parent(node) {
                    Some(parent) => doc.element_children(parent).collect(),
                    None => vec![node],
                };
                let mut pos = 0;
                for s in sibs {
                    let counts = match of {
                        Some(l) => matches_any(l, doc, s, st),
                        None => true,
                    };
                    if counts {
                        pos += 1;
                    }
                    if s == node {
                        break;
                    }
                }
                if let Some(l) = of {
                    if !matches_any(l, doc, node, st) {
                        return false;
                    }
                }
                nth(*a, *b, pos)
            }
            Pseudo::NthLastChild(a, b) => {
                let sibs: Vec<NodeId> = match doc.parent(node) {
                    Some(parent) => doc.element_children(parent).collect(),
                    None => vec![node],
                };
                let pos = sibs.iter().position(|&s| s == node).unwrap_or(0) as i32;
                nth(*a, *b, sibs.len() as i32 - pos)
            }
            Pseudo::Not(list) => !list.iter().any(|s| match_from(s, 0, doc, node, st, depth + 1)),
            Pseudo::Is(list) | Pseudo::Where(list) => list.iter().any(|s| match_from(s, 0, doc, node, st, depth + 1)),
            Pseudo::Has(rel) => {
                let r = has_matches(rel, doc, node, st, depth);
                HAS_LOG.with(|l| {
                    if let Some(m) = l.borrow_mut().as_mut() {
                        if st.scope.is_none() && st.shadow_root.is_none() && st.host.is_none() {
                            m.insert((node, rel.as_ptr() as usize), (r, rel.len() as u32));
                        } else if let (None, Some(h)) = (st.scope, st.host) {
                            // Shadow tree rules: not replayed; any mutation
                            // under the host restyles the host's subtree.
                            m.insert((h, HAS_HOST_MARK), (false, 0));
                        } else {
                            m.insert((usize::MAX, 0), (false, 0)); // not replayable
                        }
                    }
                });
                r
            }
            Pseudo::Root => doc.parent(node).map(|p| doc.is_document(p)).unwrap_or(false),
            Pseudo::Empty => doc.children(node).iter().all(|&ch| match &doc.node(ch).data {
                NodeData::Comment(_) => true,
                NodeData::Text(t) => t.is_empty(),
                _ => false,
            }),
            Pseudo::Link | Pseudo::AnyLink => matches!(el.name.as_str(), "a" | "area") && el.has_attr("href"),
            Pseudo::Visited => false, // privacy: never expose history via styling
            Pseudo::Checked => match el.name.as_str() {
                "input" => {
                    matches!(el.attr("type").map(|t| t.to_ascii_lowercase()).as_deref(), Some("checkbox") | Some("radio"))
                        && el.checked.unwrap_or_else(|| el.has_attr("checked"))
                }
                "option" => el.selected.unwrap_or_else(|| el.has_attr("selected")),
                _ => false,
            },
            Pseudo::Disabled => is_form_control(&el.name) && actually_disabled(doc, node),
            Pseudo::Enabled => is_form_control(&el.name) && !actually_disabled(doc, node),
            Pseudo::Required => el.has_attr("required"),
            Pseudo::Optional => is_form_control(&el.name) && !el.has_attr("required"),
            Pseudo::ReadOnly => !(matches!(el.name.as_str(), "input" | "textarea") && !el.has_attr("readonly") && !el.has_attr("disabled")) && !el.has_attr("contenteditable"),
            Pseudo::ReadWrite => matches!(el.name.as_str(), "input" | "textarea") && !el.has_attr("readonly") && !el.has_attr("disabled"),
            Pseudo::PlaceholderShown => {
                matches!(el.name.as_str(), "input" | "textarea")
                    && el.has_attr("placeholder")
                    && el.value.as_deref().unwrap_or_else(|| el.attr("value").unwrap_or("")).is_empty()
            }
            Pseudo::Target => st.target == Some(node),
            Pseudo::Lang(l) => {
                let mut cur = Some(node);
                let mut found = None;
                while let Some(n) = cur {
                    if let Some(v) = doc.attr(n, "lang") {
                        found = Some(v.to_ascii_lowercase());
                        break;
                    }
                    cur = doc.parent_element(n);
                }
                found.map(|f| f == *l || f.starts_with(&format!("{l}-"))).unwrap_or(false)
            }
            Pseudo::Defined => {
                // Built-in elements are always defined; custom elements once registered.
                match doc.element(node) {
                    Some(e) if e.name.contains('-') && e.ns == Namespace::Html => e.ce_defined,
                    _ => true,
                }
            }
            Pseudo::Scope => match st.scope {
                Some(s) => s == node,
                None => doc.parent(node).map(|p| doc.is_document(p)).unwrap_or(false),
            },
            Pseudo::Open => el.has_attr("open"),
            Pseudo::Default => default_button(doc, node) || el.has_attr("checked") || el.has_attr("selected"),
            Pseudo::Indeterminate => crate::forms::indeterminate(doc, node),
            Pseudo::Valid => crate::forms::validity(doc, node).map(|v| v.valid()).unwrap_or(false),
            Pseudo::Invalid => crate::forms::validity(doc, node).map(|v| !v.valid()).unwrap_or(false),
            Pseudo::InRange => crate::forms::in_range(doc, node) == Some(true),
            Pseudo::OutOfRange => crate::forms::in_range(doc, node) == Some(false),
            Pseudo::Dir(rtl) => crate::forms::is_rtl(doc, node) == *rtl,
            Pseudo::PopoverOpen => el.popover_open,
            Pseudo::Modal => el.modal,
            Pseudo::Host(_) | Pseudo::HostContext(_) => false,
        };
        if !ok {
            return false;
        }
    }
    true
}

/// The shadow host seen from its own shadow tree matches only `:host`,
/// `:host(compound)` and `:host-context(compound)`.
fn match_featureless_host(c: &Compound, doc: &Document, node: NodeId, st: &MatchState, depth: u32) -> bool {
    if c.tag.is_some() || c.id.is_some() || !c.classes.is_empty() || !c.attrs.is_empty() {
        return false;
    }
    let mut hostish = false;
    for p in &c.pseudos {
        let ok = match p {
            Pseudo::Host(None) => true,
            Pseudo::Host(Some(l)) => {
                let o = st.outer();
                l.iter().any(|s| match_from(s, 0, doc, node, &o, depth + 1))
            }
            Pseudo::HostContext(l) => {
                let o = st.outer();
                let mut cur = Some(node);
                let mut found = false;
                while let Some(n) = cur {
                    if doc.is_element(n) && l.iter().any(|s| match_from(s, 0, doc, n, &o, depth + 1)) {
                        found = true;
                        break;
                    }
                    cur = doc.parent(n).or_else(|| doc.host_of(n));
                }
                found
            }
            Pseudo::Is(l) | Pseudo::Where(l) => l.iter().any(|s| s.parts.len() == 1 && match_compound(&s.parts[0], doc, node, st, depth + 1)),
            Pseudo::Not(l) => !l.iter().any(|s| s.parts.len() == 1 && match_compound(&s.parts[0], doc, node, st, depth + 1)),
            _ => return false,
        };
        if !ok {
            return false;
        }
        if matches!(p, Pseudo::Host(_) | Pseudo::HostContext(_)) {
            hostish = true;
        }
    }
    hostish
}

/// Does `sel` (a `::slotted()` rule of the shadow tree `root` of `host`)
/// apply to `node`, which renders in `slot`?
pub fn matches_slotted(sel: &Selector, doc: &Document, node: NodeId, slot: NodeId, st: &MatchState) -> bool {
    let Some(ShadowArg::Slotted(arg)) = sel.shadow_arg.as_deref() else { return false };
    let o = st.outer();
    let light = MatchState { host: None, shadow_root: None, ..o };
    if !arg.iter().any(|s| match_compound(&s.parts[0], doc, node, &light, 0)) {
        return false;
    }
    match_from(sel, 0, doc, slot, st, 0)
}

/// Does `sel` (a `::part()` rule) apply to `node`, an element of the shadow
/// tree of `host`? `st` is the context of the tree containing the host.
pub fn matches_part(sel: &Selector, doc: &Document, node: NodeId, host: NodeId, st: &MatchState) -> bool {
    let Some(ShadowArg::Part(names, after)) = sel.shadow_arg.as_deref() else { return false };
    let Some(parts) = doc.attr(node, "part") else { return false };
    if !names.iter().all(|n| parts.split_ascii_whitespace().any(|p| p == n)) {
        return false;
    }
    if !after.pseudos.is_empty() {
        let inner = MatchState { host: None, shadow_root: None, ..st.outer() };
        if !match_compound(after, doc, node, &inner, 0) {
            return false;
        }
    }
    match_from(sel, 0, doc, host, st, 0)
}

/// A form control is disabled by its own attribute, a disabled ancestor
/// `<fieldset>` (unless it sits in that fieldset's first `<legend>`), or for
/// options a disabled `<optgroup>`.
pub fn actually_disabled(doc: &Document, node: NodeId) -> bool {
    let Some(el) = doc.element(node) else { return false };
    if el.has_attr("disabled") {
        return matches!(el.name.as_str(), "input" | "button" | "select" | "textarea" | "option" | "optgroup" | "fieldset");
    }
    if el.name == "option" {
        return doc.parent_element(node).map(|p| doc.is_tag(p, "optgroup") && doc.has_attr(p, "disabled")).unwrap_or(false);
    }
    if !matches!(el.name.as_str(), "input" | "button" | "select" | "textarea" | "fieldset") {
        return false;
    }
    let mut child = node;
    let mut cur = doc.parent_element(node);
    while let Some(a) = cur {
        if doc.is_tag(a, "fieldset") && doc.has_attr(a, "disabled") {
            // Exempt: inside the fieldset's first legend child.
            let first_legend = doc.element_children(a).find(|&c| doc.is_tag(c, "legend"));
            if first_legend != Some(child) {
                return true;
            }
        }
        child = a;
        cur = doc.parent_element(a);
    }
    false
}

/// `:default` for buttons: the first submit button of its form.
fn default_button(doc: &Document, node: NodeId) -> bool {
    let is_submit = |n: NodeId| match doc.element(n) {
        Some(e) if e.name == "button" => e.attr("type").map(|t| t.eq_ignore_ascii_case("submit")).unwrap_or(true),
        Some(e) if e.name == "input" => e.attr("type").map(|t| t.eq_ignore_ascii_case("submit") || t.eq_ignore_ascii_case("image")).unwrap_or(false),
        _ => false,
    };
    if !is_submit(node) {
        return false;
    }
    match doc.ancestors(node).find(|&a| doc.element(a).map(|e| e.name == "form").unwrap_or(false)) {
        Some(f) => doc.descendants(f).find(|&d| is_submit(d)) == Some(node),
        None => false,
    }
}

/// querySelectorAll over a subtree (excluding root unless `include_root`).
pub fn query_all(doc: &Document, root: NodeId, list: &[Selector], first_only: bool) -> Vec<NodeId> {
    let st = MatchState { hover_chain: &[], focus: doc.focused, active_chain: &[], target: None, scope: Some(root), host: None, shadow_root: None };
    let mut out = Vec::new();
    for n in doc.descendants(root) {
        if doc.is_element(n) && matches_any(list, doc, n, &st) {
            out.push(n);
            if first_only {
                break;
            }
        }
    }
    out
}
