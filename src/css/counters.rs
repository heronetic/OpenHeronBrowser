// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! CSS Counter Styles Level 3: the predefined counter styles, `@counter-style`
//! rules and the algorithms that turn a counter value into text.

use std::collections::HashMap;
use std::rc::Rc;

use super::tokenizer::{words, Tok, CV};

#[derive(Clone, Debug, PartialEq)]
pub enum System {
    Cyclic,
    Numeric,
    Alphabetic,
    Symbolic,
    Additive,
    Fixed(i64),
}

#[derive(Clone, Debug)]
pub struct CounterStyle {
    pub system: System,
    pub symbols: Vec<String>,
    pub additive: Vec<(i64, String)>,
    pub prefix: String,
    pub suffix: String,
    pub negative: (String, String),
    pub pad: Option<(usize, String)>,
    /// Inclusive ranges; `None` is `auto`.
    pub range: Option<Vec<(i64, i64)>>,
    pub fallback: String,
}

/// `@counter-style` rules by name.
pub type CounterStyles = HashMap<String, Rc<CounterStyle>>;

impl CounterStyle {
    fn new(system: System, symbols: &[&str], suffix: &str) -> CounterStyle {
        CounterStyle {
            system,
            symbols: symbols.iter().map(|s| s.to_string()).collect(),
            additive: Vec::new(),
            prefix: String::new(),
            suffix: suffix.to_string(),
            negative: ("-".into(), String::new()),
            pad: None,
            range: None,
            fallback: "decimal".into(),
        }
    }
    fn additive(table: &[(i64, &str)], suffix: &str, range: Option<(i64, i64)>) -> CounterStyle {
        let mut s = CounterStyle::new(System::Additive, &[], suffix);
        s.additive = table.iter().map(|(v, t)| (*v, t.to_string())).collect();
        s.range = range.map(|r| vec![r]);
        s
    }
    fn chars(system: System, digits: &str, suffix: &str) -> CounterStyle {
        let syms: Vec<String> = digits.chars().map(|c| c.to_string()).collect();
        let mut s = CounterStyle::new(system, &[], suffix);
        s.symbols = syms;
        s
    }
}

/// The counter styles every UA predefines (css-counter-styles-3 §6, §7).
pub fn predefined(name: &str) -> Option<CounterStyle> {
    use System::*;
    let dot = ". ";
    Some(match name {
        "decimal" => CounterStyle::chars(Numeric, "0123456789", dot),
        "decimal-leading-zero" => {
            let mut s = CounterStyle::chars(Numeric, "0123456789", dot);
            s.pad = Some((2, "0".into()));
            s
        }
        "arabic-indic" => CounterStyle::chars(Numeric, "٠١٢٣٤٥٦٧٨٩", dot),
        "bengali" => CounterStyle::chars(Numeric, "০১২৩৪৫৬৭৮৯", dot),
        "cambodian" | "khmer" => CounterStyle::chars(Numeric, "០១២៣៤៥៦៧៨៩", dot),
        "cjk-decimal" => CounterStyle::chars(Numeric, "〇一二三四五六七八九", "、"),
        "devanagari" => CounterStyle::chars(Numeric, "०१२३४५६७८९", dot),
        "gujarati" => CounterStyle::chars(Numeric, "૦૧૨૩૪૫૬૭૮૯", dot),
        "gurmukhi" => CounterStyle::chars(Numeric, "੦੧੨੩੪੫੬੭੮੯", dot),
        "kannada" => CounterStyle::chars(Numeric, "೦೧೨೩೪೫೬೭೮೯", dot),
        "lao" => CounterStyle::chars(Numeric, "໐໑໒໓໔໕໖໗໘໙", dot),
        "malayalam" => CounterStyle::chars(Numeric, "൦൧൨൩൪൫൬൭൮൯", dot),
        "mongolian" => CounterStyle::chars(Numeric, "᠐᠑᠒᠓᠔᠕᠖᠗᠘᠙", dot),
        "myanmar" => CounterStyle::chars(Numeric, "၀၁၂၃၄၅၆၇၈၉", dot),
        "oriya" => CounterStyle::chars(Numeric, "୦୧୨୩୪୫୬୭୮୯", dot),
        "persian" => CounterStyle::chars(Numeric, "۰۱۲۳۴۵۶۷۸۹", dot),
        "tamil" => CounterStyle::chars(Numeric, "௦௧௨௩௪௫௬௭௮௯", dot),
        "telugu" => CounterStyle::chars(Numeric, "౦౧౨౩౪౫౬౭౮౯", dot),
        "thai" => CounterStyle::chars(Numeric, "๐๑๒๓๔๕๖๗๘๙", dot),
        "tibetan" => CounterStyle::chars(Numeric, "༠༡༢༣༤༥༦༧༨༩", dot),
        "lower-alpha" | "lower-latin" => CounterStyle::chars(Alphabetic, "abcdefghijklmnopqrstuvwxyz", dot),
        "upper-alpha" | "upper-latin" => CounterStyle::chars(Alphabetic, "ABCDEFGHIJKLMNOPQRSTUVWXYZ", dot),
        "lower-greek" => CounterStyle::chars(Alphabetic, "αβγδεζηθικλμνξοπρστυφχψω", dot),
        "hiragana" => CounterStyle::chars(Alphabetic, "あいうえおかきくけこさしすせそたちつてとなにぬねのはひふへほまみむめもやゆよらりるれろわゐゑをん", "、"),
        "hiragana-iroha" => CounterStyle::chars(Alphabetic, "いろはにほへとちりぬるをわかよたれそつねならむうゐのおくやまけふこえてあさきゆめみしゑひもせす", "、"),
        "katakana" => CounterStyle::chars(Alphabetic, "アイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホマミムメモヤユヨラリルレロワヰヱヲン", "、"),
        "katakana-iroha" => CounterStyle::chars(Alphabetic, "イロハニホヘトチリヌルヲワカヨタレソツネナラムウヰノオクヤマケフコエテアサキユメミシヱヒモセス", "、"),
        "cjk-earthly-branch" => CounterStyle::chars(Alphabetic, "子丑寅卯辰巳午未申酉戌亥", "、"),
        "cjk-heavenly-stem" => CounterStyle::chars(Alphabetic, "甲乙丙丁戊己庚辛壬癸", "、"),
        "disc" => CounterStyle::new(Cyclic, &["•"], " "),
        "circle" => CounterStyle::new(Cyclic, &["◦"], " "),
        "square" => CounterStyle::new(Cyclic, &["▪"], " "),
        "disclosure-open" => CounterStyle::new(Cyclic, &["▾"], " "),
        "disclosure-closed" => CounterStyle::new(Cyclic, &["▸"], " "),
        "lower-roman" => CounterStyle::additive(
            &[(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")],
            dot,
            Some((1, 3999)),
        ),
        "upper-roman" => CounterStyle::additive(
            &[(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"), (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")],
            dot,
            Some((1, 3999)),
        ),
        "armenian" | "upper-armenian" => CounterStyle::additive(
            &[
                (9000, "Ք"), (8000, "Փ"), (7000, "Ւ"), (6000, "Ց"), (5000, "Ր"), (4000, "Տ"), (3000, "Վ"), (2000, "Ս"), (1000, "Ռ"),
                (900, "Ջ"), (800, "Պ"), (700, "Չ"), (600, "Ո"), (500, "Շ"), (400, "Ն"), (300, "Յ"), (200, "Մ"), (100, "Ճ"),
                (90, "Ղ"), (80, "Ձ"), (70, "Հ"), (60, "Կ"), (50, "Ծ"), (40, "Խ"), (30, "Լ"), (20, "Ի"), (10, "Ժ"),
                (9, "Թ"), (8, "Ը"), (7, "Է"), (6, "Զ"), (5, "Ե"), (4, "Դ"), (3, "Գ"), (2, "Բ"), (1, "Ա"),
            ],
            dot,
            Some((1, 9999)),
        ),
        "lower-armenian" => CounterStyle::additive(
            &[
                (9000, "ք"), (8000, "փ"), (7000, "ւ"), (6000, "ց"), (5000, "ր"), (4000, "տ"), (3000, "վ"), (2000, "ս"), (1000, "ռ"),
                (900, "ջ"), (800, "պ"), (700, "չ"), (600, "ո"), (500, "շ"), (400, "ն"), (300, "յ"), (200, "մ"), (100, "ճ"),
                (90, "ղ"), (80, "ձ"), (70, "հ"), (60, "կ"), (50, "ծ"), (40, "խ"), (30, "լ"), (20, "ի"), (10, "ժ"),
                (9, "թ"), (8, "ը"), (7, "է"), (6, "զ"), (5, "ե"), (4, "դ"), (3, "գ"), (2, "բ"), (1, "ա"),
            ],
            dot,
            Some((1, 9999)),
        ),
        "georgian" => CounterStyle::additive(
            &[
                (10000, "ჵ"), (9000, "ჰ"), (8000, "ჯ"), (7000, "ჴ"), (6000, "ხ"), (5000, "ჭ"), (4000, "წ"), (3000, "ძ"), (2000, "ც"), (1000, "ჩ"),
                (900, "შ"), (800, "ყ"), (700, "ღ"), (600, "ქ"), (500, "ფ"), (400, "ჳ"), (300, "ტ"), (200, "ს"), (100, "რ"),
                (90, "ჟ"), (80, "პ"), (70, "ო"), (60, "ჲ"), (50, "ნ"), (40, "მ"), (30, "ლ"), (20, "კ"), (10, "ი"),
                (9, "თ"), (8, "ჱ"), (7, "ზ"), (6, "ვ"), (5, "ე"), (4, "დ"), (3, "გ"), (2, "ბ"), (1, "ა"),
            ],
            dot,
            Some((1, 19999)),
        ),
        "hebrew" => CounterStyle::additive(
            &[
                (10000, "י׳"), (9000, "ט׳"), (8000, "ח׳"), (7000, "ז׳"), (6000, "ו׳"), (5000, "ה׳"), (4000, "ד׳"), (3000, "ג׳"), (2000, "ב׳"), (1000, "א׳"),
                (400, "ת"), (300, "ש"), (200, "ר"), (100, "ק"), (90, "צ"), (80, "פ"), (70, "ע"), (60, "ס"), (50, "נ"), (40, "מ"), (30, "ל"), (20, "כ"),
                (19, "יט"), (18, "יח"), (17, "יז"), (16, "טז"), (15, "טו"), (10, "י"), (9, "ט"), (8, "ח"), (7, "ז"), (6, "ו"), (5, "ה"), (4, "ד"), (3, "ג"), (2, "ב"), (1, "א"),
            ],
            dot,
            Some((1, 10999)),
        ),
        _ => return None,
    })
}

fn string_of(c: &CV) -> Option<String> {
    match c {
        CV::T(Tok::Str(s)) => Some(s.clone()),
        CV::T(Tok::Ident(i)) => Some(i.clone()),
        CV::T(Tok::Number(n)) => Some(format!("{n}")),
        _ => None,
    }
}

fn int_of(c: &CV) -> Option<i64> {
    match c {
        CV::T(Tok::Number(n)) if n.fract() == 0.0 => Some(*n as i64),
        _ => None,
    }
}

/// A parsed `@counter-style` rule, before `extends` is resolved.
#[derive(Clone, Debug, Default)]
pub struct CounterStyleRule {
    pub name: String,
    pub system: Option<System>,
    pub extends: Option<String>,
    pub symbols: Option<Vec<String>>,
    pub additive: Option<Vec<(i64, String)>>,
    pub prefix: Option<String>,
    pub suffix: Option<String>,
    pub negative: Option<(String, String)>,
    pub pad: Option<(usize, String)>,
    pub range: Option<Option<Vec<(i64, i64)>>>,
    pub fallback: Option<String>,
}

/// Parse the body of `@counter-style <name> { ... }`.
pub fn parse_rule(name: &str, decls: &[super::Declaration]) -> Option<CounterStyleRule> {
    let lname = name.to_ascii_lowercase();
    if matches!(lname.as_str(), "none" | "inherit" | "initial" | "unset" | "default" | "decimal" | "disc" | "square" | "circle" | "disclosure-open" | "disclosure-closed") {
        return None;
    }
    let mut r = CounterStyleRule { name: name.to_string(), ..Default::default() };
    for d in decls {
        let w = words(&d.value);
        match &*d.name {
            "system" => {
                let kw = w.first().and_then(|c| c.ident()).map(|s| s.to_ascii_lowercase());
                r.system = match kw.as_deref() {
                    Some("cyclic") => Some(System::Cyclic),
                    Some("numeric") => Some(System::Numeric),
                    Some("alphabetic") => Some(System::Alphabetic),
                    Some("symbolic") => Some(System::Symbolic),
                    Some("additive") => Some(System::Additive),
                    Some("fixed") => Some(System::Fixed(w.get(1).and_then(|c| int_of(c)).unwrap_or(1))),
                    Some("extends") => {
                        r.extends = w.get(1).and_then(|c| c.ident()).map(|s| s.to_ascii_lowercase());
                        None
                    }
                    _ => None,
                };
            }
            "symbols" => {
                let syms: Vec<String> = w.iter().filter_map(|c| string_of(c)).collect();
                if !syms.is_empty() {
                    r.symbols = Some(syms);
                }
            }
            "additive-symbols" => {
                let mut tab = Vec::new();
                for part in super::tokenizer::split_commas(&d.value) {
                    let pw = words(part);
                    let v = pw.iter().find_map(|c| int_of(c));
                    let s = pw.iter().find_map(|c| if let CV::T(Tok::Str(s)) = c { Some(s.clone()) } else { None });
                    if let (Some(v), Some(s)) = (v, s) {
                        tab.push((v, s));
                    }
                }
                if !tab.is_empty() {
                    r.additive = Some(tab);
                }
            }
            "prefix" => r.prefix = w.first().and_then(|c| string_of(c)),
            "suffix" => r.suffix = w.first().and_then(|c| string_of(c)),
            "negative" => {
                if let Some(a) = w.first().and_then(|c| string_of(c)) {
                    r.negative = Some((a, w.get(1).and_then(|c| string_of(c)).unwrap_or_default()));
                }
            }
            "pad" => {
                let n = w.iter().find_map(|c| int_of(c));
                let s = w.iter().find_map(|c| if let CV::T(Tok::Str(s)) = c { Some(s.clone()) } else { None });
                if let (Some(n), Some(s)) = (n, s) {
                    r.pad = Some((n.clamp(0, 100) as usize, s));
                }
            }
            "range" => {
                if w.first().map(|c| c.is_ident("auto")).unwrap_or(false) {
                    r.range = Some(None);
                } else {
                    let mut rs = Vec::new();
                    for part in super::tokenizer::split_commas(&d.value) {
                        let pw = words(part);
                        let b = |c: Option<&CV>| -> Option<i64> {
                            match c {
                                Some(c) if c.is_ident("infinite") => Some(i64::MAX),
                                Some(c) => int_of(c),
                                None => None,
                            }
                        };
                        let lo = if pw.first().map(|c| c.is_ident("infinite")).unwrap_or(false) { Some(i64::MIN) } else { b(pw.first().copied()) };
                        if let (Some(lo), Some(hi)) = (lo, b(pw.get(1).copied())) {
                            rs.push((lo, hi));
                        }
                    }
                    if !rs.is_empty() {
                        r.range = Some(Some(rs));
                    }
                }
            }
            "fallback" => r.fallback = w.first().and_then(|c| c.ident()).map(|s| s.to_ascii_lowercase()),
            _ => {}
        }
    }
    Some(r)
}

/// Resolve rules (including `extends` chains) into counter styles.
pub fn build(rules: &[CounterStyleRule]) -> CounterStyles {
    let by_name: HashMap<String, &CounterStyleRule> = rules.iter().map(|r| (r.name.clone(), r)).collect();
    let mut out = CounterStyles::new();
    for r in rules {
        if let Some(cs) = resolve_rule(r, &by_name, 0) {
            out.insert(r.name.clone(), Rc::new(cs));
        }
    }
    out
}

fn resolve_rule(r: &CounterStyleRule, by_name: &HashMap<String, &CounterStyleRule>, depth: u32) -> Option<CounterStyle> {
    let mut base = if let Some(ext) = &r.extends {
        if depth > 16 {
            predefined("decimal")?
        } else if let Some(p) = by_name.get(ext).filter(|p| p.name != r.name) {
            resolve_rule(p, by_name, depth + 1)?
        } else {
            predefined(ext).unwrap_or_else(|| predefined("decimal").unwrap())
        }
    } else {
        let system = r.system.clone().unwrap_or(System::Symbolic);
        let mut s = CounterStyle::new(system, &[], ". ");
        s.symbols.clear();
        s
    };
    if r.extends.is_none() {
        if let Some(sym) = &r.symbols {
            base.symbols = sym.clone();
        }
        if let Some(a) = &r.additive {
            base.additive = a.clone();
        }
        // Each system needs its symbols.
        let ok = match base.system {
            System::Additive => !base.additive.is_empty(),
            System::Alphabetic | System::Numeric => base.symbols.len() >= 2,
            _ => !base.symbols.is_empty(),
        };
        if !ok {
            return None;
        }
    }
    if let Some(p) = &r.prefix {
        base.prefix = p.clone();
    }
    if let Some(s) = &r.suffix {
        base.suffix = s.clone();
    }
    if let Some(n) = &r.negative {
        base.negative = n.clone();
    }
    if let Some(p) = &r.pad {
        base.pad = Some(p.clone());
    }
    if let Some(rg) = &r.range {
        base.range = rg.clone();
    }
    if let Some(f) = &r.fallback {
        base.fallback = f.clone();
    }
    Some(base)
}

/// Look up a counter style by name: author rules first, then the predefined ones.
pub fn lookup(name: &str, author: &CounterStyles) -> Rc<CounterStyle> {
    if let Some(s) = author.get(name) {
        return s.clone();
    }
    let lname = name.to_ascii_lowercase();
    if let Some(s) = author.get(&lname) {
        return s.clone();
    }
    Rc::new(predefined(&lname).unwrap_or_else(|| predefined("decimal").unwrap()))
}

fn in_range(s: &CounterStyle, v: i64) -> bool {
    match &s.range {
        Some(rs) => rs.iter().any(|&(a, b)| v >= a && v <= b),
        None => match s.system {
            System::Alphabetic | System::Symbolic => v >= 1,
            System::Additive => v >= 0,
            _ => true,
        },
    }
}

fn uses_negative(s: &CounterStyle) -> bool {
    matches!(s.system, System::Symbolic | System::Alphabetic | System::Numeric | System::Additive)
}

/// The representation of `v` (no prefix or suffix), following fallbacks.
pub fn represent(s: &CounterStyle, v: i64, author: &CounterStyles) -> String {
    represent_depth(s, v, author, 0)
}

fn represent_depth(s: &CounterStyle, v: i64, author: &CounterStyles, depth: u32) -> String {
    let fallback = |author: &CounterStyles| -> String {
        if depth > 8 {
            return v.to_string();
        }
        let f = lookup(&s.fallback, author);
        represent_depth(&f, v, author, depth + 1)
    };
    if !in_range(s, v) {
        return fallback(author);
    }
    let neg = v < 0 && uses_negative(s);
    let mag = if neg { v.unsigned_abs() as i64 } else { v };
    let body = match algorithm(s, mag) {
        Some(b) => b,
        None => return fallback(author),
    };
    let mut body = body;
    if let Some((n, sym)) = &s.pad {
        let len = body.chars().count() + if neg { s.negative.0.chars().count() + s.negative.1.chars().count() } else { 0 };
        if len < *n {
            body = sym.repeat(n - len) + &body;
        }
    }
    if neg {
        format!("{}{}{}", s.negative.0, body, s.negative.1)
    } else {
        body
    }
}

fn algorithm(s: &CounterStyle, v: i64) -> Option<String> {
    let syms = &s.symbols;
    let n = syms.len() as i64;
    match &s.system {
        System::Cyclic => {
            if n == 0 {
                return None;
            }
            Some(syms[((v - 1).rem_euclid(n)) as usize].clone())
        }
        System::Fixed(first) => {
            let i = v - first;
            if i >= 0 && i < n {
                Some(syms[i as usize].clone())
            } else {
                None
            }
        }
        System::Symbolic => {
            if v < 1 || n == 0 {
                return None;
            }
            let reps = ((v - 1) / n + 1).min(60) as usize;
            Some(syms[((v - 1) % n) as usize].repeat(reps))
        }
        System::Alphabetic => {
            if v < 1 || n < 2 {
                return None;
            }
            let mut v = v;
            let mut out = Vec::new();
            while v > 0 {
                v -= 1;
                out.push(syms[(v % n) as usize].clone());
                v /= n;
            }
            out.reverse();
            Some(out.concat())
        }
        System::Numeric => {
            if n < 2 {
                return None;
            }
            if v == 0 {
                return Some(syms[0].clone());
            }
            let mut v = v;
            let mut out = Vec::new();
            while v > 0 {
                out.push(syms[(v % n) as usize].clone());
                v /= n;
            }
            out.reverse();
            Some(out.concat())
        }
        System::Additive => {
            let tab = &s.additive;
            if v == 0 {
                return tab.iter().find(|(w, _)| *w == 0).map(|(_, s)| s.clone());
            }
            let mut v = v;
            let mut out = String::new();
            for (w, sym) in tab {
                if *w <= 0 {
                    continue;
                }
                let k = v / w;
                if k > 0 {
                    if k > 60 {
                        return None;
                    }
                    out.push_str(&sym.repeat(k as usize));
                    v -= k * w;
                }
                if v == 0 {
                    break;
                }
            }
            if v != 0 {
                None
            } else {
                Some(out)
            }
        }
    }
}

/// Marker text for a list item: prefix, representation and suffix (of the
/// fallback style when the value can't be represented).
pub fn marker_string(name: &str, v: i64, author: &CounterStyles) -> String {
    let mut s = lookup(name, author);
    for _ in 0..8 {
        let mag = if v < 0 && uses_negative(&s) { v.unsigned_abs() as i64 } else { v };
        if in_range(&s, v) && algorithm(&s, mag).is_some() {
            break;
        }
        s = lookup(&s.fallback.clone(), author);
    }
    format!("{}{}{}", s.prefix, represent(&s, v, author), s.suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predefined_styles() {
        let a = CounterStyles::new();
        assert_eq!(marker_string("decimal", 9, &a), "9. ");
        assert_eq!(marker_string("decimal-leading-zero", 1, &a), "01. ");
        assert_eq!(marker_string("lower-roman", 4, &a), "iv. ");
        assert_eq!(marker_string("upper-roman", 3999, &a), "MMMCMXCIX. ");
        assert_eq!(marker_string("lower-alpha", 27, &a), "aa. ");
        assert_eq!(marker_string("lower-greek", 2, &a), "β. ");
        assert_eq!(marker_string("cjk-decimal", 10, &a), "一〇、");
        assert_eq!(marker_string("upper-roman", 4000, &a), "4000. ");
        assert_eq!(represent(&lookup("decimal", &a), -5, &a), "-5");
    }
}
