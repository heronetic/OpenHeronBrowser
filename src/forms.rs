// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Form-control state shared by selectors (`:valid`, `:in-range`,
//! `:indeterminate`, `:dir()`) and the DOM's constraint-validation API
//! (HTML §4.10.20 / §4.10.5).

use crate::dom::{Document, NodeData, NodeId};

/// The ValidityState flags of a control.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub struct Validity {
    pub value_missing: bool,
    pub type_mismatch: bool,
    pub pattern_mismatch: bool,
    pub too_long: bool,
    pub too_short: bool,
    pub range_underflow: bool,
    pub range_overflow: bool,
    pub step_mismatch: bool,
    pub bad_input: bool,
    pub custom_error: bool,
}

impl Validity {
    pub fn valid(&self) -> bool {
        !(self.value_missing
            || self.type_mismatch
            || self.pattern_mismatch
            || self.too_long
            || self.too_short
            || self.range_underflow
            || self.range_overflow
            || self.step_mismatch
            || self.bad_input
            || self.custom_error)
    }

    /// Flags in ValidityState attribute order, for the script bridge.
    pub fn flags(&self) -> [bool; 10] {
        [
            self.value_missing,
            self.type_mismatch,
            self.pattern_mismatch,
            self.too_long,
            self.too_short,
            self.range_underflow,
            self.range_overflow,
            self.step_mismatch,
            self.bad_input,
            self.custom_error,
        ]
    }
}

fn input_type(doc: &Document, n: NodeId) -> String {
    doc.attr(n, "type").map(|t| t.trim().to_ascii_lowercase()).unwrap_or_else(|| "text".into())
}

/// The control's current value (live value, else the markup's).
pub fn value_of(doc: &Document, n: NodeId) -> String {
    match doc.element(n) {
        Some(e) if e.name == "textarea" => e.value.clone().unwrap_or_else(|| doc.text_content(n)),
        Some(e) if e.name == "select" => {
            let opts: Vec<NodeId> = doc.descendants(n).filter(|&d| doc.element(d).map(|o| o.name == "option").unwrap_or(false)).collect();
            let sel = opts.iter().copied().find(|&o| doc.element(o).map(|e| e.selected.unwrap_or_else(|| e.has_attr("selected"))).unwrap_or(false));
            let chosen = sel.or_else(|| if doc.has_attr(n, "multiple") { None } else { opts.first().copied() });
            chosen.map(|o| doc.attr(o, "value").map(|v| v.to_string()).unwrap_or_else(|| doc.text_content(o).trim().to_string())).unwrap_or_default()
        }
        Some(e) => e.value.clone().or_else(|| e.attr("value").map(|s| s.to_string())).unwrap_or_default(),
        None => String::new(),
    }
}

fn is_checked(doc: &Document, n: NodeId) -> bool {
    doc.element(n).map(|e| e.checked.unwrap_or_else(|| e.has_attr("checked"))).unwrap_or(false)
}

/// Inside a `<datalist>` or disabled (itself or by a disabled fieldset): barred from validation.
fn barred(doc: &Document, n: NodeId) -> bool {
    if doc.has_attr(n, "disabled") || doc.has_attr(n, "readonly") && doc.element(n).map(|e| e.name != "select").unwrap_or(false) {
        return true;
    }
    let mut cur = doc.parent_element(n);
    while let Some(p) = cur {
        match doc.element(p).map(|e| e.name.as_str()) {
            Some("datalist") => return true,
            Some("fieldset") if doc.has_attr(p, "disabled") => return true,
            _ => {}
        }
        cur = doc.parent_element(p);
    }
    false
}

/// Radio buttons in the same group as `n` (same name, same form owner).
fn radio_group(doc: &Document, n: NodeId) -> Vec<NodeId> {
    let name = match doc.attr(n, "name") {
        Some(nm) if !nm.is_empty() => nm.to_string(),
        _ => return vec![n],
    };
    let form = doc.ancestors(n).find(|&a| doc.element(a).map(|e| e.name == "form").unwrap_or(false));
    let root = form.unwrap_or_else(|| doc.root_of(n));
    doc.descendants(root)
        .filter(|&d| {
            doc.element(d).map(|e| e.name == "input").unwrap_or(false)
                && input_type(doc, d) == "radio"
                && doc.attr(d, "name") == Some(name.as_str())
                && (form.is_some() || !doc.ancestors(d).any(|a| doc.element(a).map(|e| e.name == "form").unwrap_or(false)))
        })
        .collect()
}

/// `:indeterminate`: a checkbox whose IDL flag is set, a radio group with
/// nothing checked, or a `<progress>` without a value.
pub fn indeterminate(doc: &Document, n: NodeId) -> bool {
    let e = match doc.element(n) {
        Some(e) => e,
        None => return false,
    };
    match e.name.as_str() {
        "input" => match input_type(doc, n).as_str() {
            "checkbox" => e.indeterminate,
            "radio" => !radio_group(doc, n).iter().any(|&r| is_checked(doc, r)),
            _ => false,
        },
        "progress" => !e.has_attr("value"),
        _ => false,
    }
}

/// Parse a control value of a date/time/number type to a comparable number.
fn parse_typed(ty: &str, v: &str) -> Option<f64> {
    let v = v.trim();
    let num = |s: &str| s.parse::<f64>().ok();
    let ymd = |s: &str| -> Option<f64> {
        let p: Vec<&str> = s.split('-').collect();
        if p.len() != 3 {
            return None;
        }
        let (y, m, d) = (p[0].parse::<i64>().ok()?, p[1].parse::<i64>().ok()?, p[2].parse::<i64>().ok()?);
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        // Days since epoch (proleptic Gregorian).
        let (yy, mm) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
        Some((365 * yy + yy / 4 - yy / 100 + yy / 400 + (153 * (mm - 3) + 2) / 5 + d) as f64)
    };
    let hms = |s: &str| -> Option<f64> {
        let p: Vec<&str> = s.split(':').collect();
        if p.len() < 2 {
            return None;
        }
        let h = p[0].parse::<f64>().ok()?;
        let m = p[1].parse::<f64>().ok()?;
        let sec = p.get(2).and_then(|x| x.parse::<f64>().ok()).unwrap_or(0.0);
        if h >= 24.0 || m >= 60.0 {
            return None;
        }
        Some(h * 3600.0 + m * 60.0 + sec)
    };
    match ty {
        "number" | "range" => num(v).filter(|x| x.is_finite()),
        "date" => ymd(v),
        "month" => {
            let (y, m) = v.split_once('-')?;
            let (y, m) = (y.parse::<f64>().ok()?, m.parse::<f64>().ok()?);
            (1.0..=12.0).contains(&m).then_some(y * 12.0 + m)
        }
        "week" => {
            let (y, w) = v.split_once("-W").or_else(|| v.split_once("-w"))?;
            Some(y.parse::<f64>().ok()? * 53.0 + w.parse::<f64>().ok()?)
        }
        "time" => hms(v),
        "datetime-local" => {
            let (d, t) = v.split_once('T').or_else(|| v.split_once(' '))?;
            Some(ymd(d)? * 86400.0 + hms(t)?)
        }
        _ => None,
    }
}

fn has_range(ty: &str) -> bool {
    matches!(ty, "number" | "range" | "date" | "month" | "week" | "time" | "datetime-local")
}

/// `:in-range` (Some(true)) / `:out-of-range` (Some(false)); None when the
/// control has no range limitation.
pub fn in_range(doc: &Document, n: NodeId) -> Option<bool> {
    let e = doc.element(n)?;
    if e.name != "input" {
        return None;
    }
    let ty = input_type(doc, n);
    if !has_range(&ty) {
        return None;
    }
    let min = doc.attr(n, "min").and_then(|m| parse_typed(&ty, m));
    let max = doc.attr(n, "max").and_then(|m| parse_typed(&ty, m));
    if min.is_none() && max.is_none() && ty != "range" {
        return None;
    }
    if ty == "range" {
        return Some(true);
    }
    let v = match parse_typed(&ty, &value_of(doc, n)) {
        Some(v) => v,
        None => return Some(true),
    };
    Some(!(min.map(|m| v < m).unwrap_or(false) || max.map(|m| v > m).unwrap_or(false)))
}

fn email_ok(v: &str) -> bool {
    let (local, domain) = match v.split_once('@') {
        Some(p) => p,
        None => return false,
    };
    !local.is_empty()
        && local.chars().all(|c| c.is_ascii_alphanumeric() || ".!#$%&'*+/=?^_`{|}~-".contains(c))
        && !domain.is_empty()
        && domain.split('.').all(|l| {
            !l.is_empty() && l.len() <= 63 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !l.starts_with('-') && !l.ends_with('-')
        })
}

/// The constraint-validation state of a submittable control; None for elements
/// that are not candidates for constraint validation.
pub fn validity(doc: &Document, n: NodeId) -> Option<Validity> {
    let e = doc.element(n)?;
    let mut v = Validity { custom_error: e.custom_validity.as_ref().map(|m| !m.is_empty()).unwrap_or(false), ..Validity::default() };
    match e.name.as_str() {
        "input" => {
            let ty = input_type(doc, n);
            if matches!(ty.as_str(), "hidden" | "button" | "reset" | "submit" | "image") || barred(doc, n) {
                return if matches!(ty.as_str(), "submit") { Some(v) } else { None };
            }
            let value = value_of(doc, n);
            let required = e.has_attr("required");
            match ty.as_str() {
                "checkbox" => v.value_missing = required && !is_checked(doc, n),
                "radio" => {
                    let group = radio_group(doc, n);
                    let req = group.iter().any(|&r| doc.has_attr(r, "required"));
                    v.value_missing = req && !group.iter().any(|&r| is_checked(doc, r));
                }
                "file" => v.value_missing = required,
                _ => {
                    v.value_missing = required && value.is_empty();
                    if !value.is_empty() {
                        match ty.as_str() {
                            "email" => {
                                let parts: Vec<&str> = if e.has_attr("multiple") { value.split(',').map(|s| s.trim()).collect() } else { vec![value.as_str()] };
                                v.type_mismatch = parts.iter().any(|p| !email_ok(p));
                            }
                            "url" => v.type_mismatch = url::Url::parse(&value).is_err(),
                            _ => {}
                        }
                        if has_range(&ty) {
                            match parse_typed(&ty, &value) {
                                None => v.bad_input = ty == "number",
                                Some(x) => {
                                    if let Some(m) = doc.attr(n, "min").and_then(|m| parse_typed(&ty, m)) {
                                        v.range_underflow = x < m;
                                    }
                                    if let Some(m) = doc.attr(n, "max").and_then(|m| parse_typed(&ty, m)) {
                                        v.range_overflow = x > m;
                                    }
                                    if ty == "number" {
                                        let step = doc.attr(n, "step").map(|s| s.trim().to_ascii_lowercase());
                                        if step.as_deref() != Some("any") {
                                            let st = step.and_then(|s| s.parse::<f64>().ok()).filter(|s| *s > 0.0).unwrap_or(1.0);
                                            // Step base: min, else the value content attribute, else 0.
                                            let base = doc.attr(n, "min").and_then(|m| parse_typed(&ty, m)).or_else(|| doc.attr(n, "value").and_then(|m| parse_typed(&ty, m))).unwrap_or(0.0);
                                            let q = (x - base) / st;
                                            v.step_mismatch = (q - q.round()).abs() > 1e-7;
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(pat) = doc.attr(n, "pattern") {
                            if matches!(ty.as_str(), "text" | "search" | "url" | "tel" | "email" | "password") {
                                if let Ok(re) = regress::Regex::with_flags(&format!("^(?:{pat})$"), "v").or_else(|_| regress::Regex::with_flags(&format!("^(?:{pat})$"), "u")) {
                                    v.pattern_mismatch = re.find(&value).is_none();
                                }
                            }
                        }
                        let len = value.encode_utf16().count();
                        // Length constraints apply only to user edits (dirty values).
                        if e.value.is_some() {
                            if let Some(mx) = doc.attr(n, "maxlength").and_then(|m| m.trim().parse::<usize>().ok()) {
                                v.too_long = len > mx;
                            }
                            if let Some(mn) = doc.attr(n, "minlength").and_then(|m| m.trim().parse::<usize>().ok()) {
                                v.too_short = len < mn;
                            }
                        }
                    }
                }
            }
            Some(v)
        }
        "textarea" => {
            if barred(doc, n) {
                return None;
            }
            let value = value_of(doc, n);
            v.value_missing = e.has_attr("required") && value.is_empty();
            if e.value.is_some() {
                let len = value.encode_utf16().count();
                if let Some(mx) = doc.attr(n, "maxlength").and_then(|m| m.trim().parse::<usize>().ok()) {
                    v.too_long = len > mx;
                }
                if let Some(mn) = doc.attr(n, "minlength").and_then(|m| m.trim().parse::<usize>().ok()) {
                    v.too_short = len > 0 && len < mn;
                }
            }
            Some(v)
        }
        "select" => {
            if barred(doc, n) {
                return None;
            }
            v.value_missing = e.has_attr("required") && value_of(doc, n).is_empty();
            Some(v)
        }
        "button" => {
            let ty = doc.attr(n, "type").map(|t| t.to_ascii_lowercase());
            if matches!(ty.as_deref(), Some("button") | Some("reset")) || barred(doc, n) {
                None
            } else {
                Some(v)
            }
        }
        "form" => {
            let invalid = doc.descendants(n).any(|d| d != n && validity(doc, d).map(|x| !x.valid()).unwrap_or(false));
            v.custom_error = invalid;
            Some(v)
        }
        "fieldset" => {
            let invalid = doc.descendants(n).any(|d| d != n && doc.element(d).map(|e| e.name != "form" && e.name != "fieldset").unwrap_or(false) && validity(doc, d).map(|x| !x.valid()).unwrap_or(false));
            v.custom_error = invalid;
            Some(v)
        }
        _ => None,
    }
}

/// True for right-to-left directionality (HTML §3.2.6.4, `dir` attribute,
/// with `dir=auto` taking the first strong character of the text).
pub fn is_rtl(doc: &Document, n: NodeId) -> bool {
    let mut cur = Some(n);
    while let Some(c) = cur {
        if let Some(d) = doc.attr(c, "dir") {
            match d.trim().to_ascii_lowercase().as_str() {
                "rtl" => return true,
                "ltr" => return false,
                "auto" => return first_strong_rtl(doc, c).unwrap_or(false),
                _ => {}
            }
        }
        if doc.element(c).map(|e| e.name == "bdi").unwrap_or(false) && c != n {
            // <bdi> without dir behaves as dir=auto for itself only.
        }
        cur = doc.parent_element(c);
    }
    false
}

/// Direction of the first strong character in the text of `n` (skipping
/// script, style and elements with their own `dir`).
pub fn first_strong_rtl(doc: &Document, n: NodeId) -> Option<bool> {
    if let Some(e) = doc.element(n) {
        if matches!(e.name.as_str(), "input" | "textarea") {
            return strong_dir(&value_of(doc, n));
        }
    }
    for &c in doc.children(n) {
        match &doc.node(c).data {
            NodeData::Text(t) => {
                if let Some(d) = strong_dir(t) {
                    return Some(d);
                }
            }
            NodeData::Element(e) => {
                if matches!(e.name.as_str(), "script" | "style" | "textarea" | "bdi") || e.has_attr("dir") {
                    continue;
                }
                if let Some(d) = first_strong_rtl(doc, c) {
                    return Some(d);
                }
            }
            _ => {}
        }
    }
    None
}

/// The direction of the first strongly directional character in `s`.
pub fn strong_dir(s: &str) -> Option<bool> {
    for ch in s.chars() {
        let c = ch as u32;
        // Hebrew, Arabic, Syriac, Thaana, NKo, Samaritan, Mandaic, Arabic ext., presentation forms.
        let rtl = (0x0590..=0x08FF).contains(&c) || (0xFB1D..=0xFDFF).contains(&c) || (0xFE70..=0xFEFF).contains(&c) || (0x10800..=0x10FFF).contains(&c) || (0x1E800..=0x1EFFF).contains(&c);
        if rtl {
            return Some(true);
        }
        if ch.is_alphabetic() {
            return Some(false);
        }
    }
    None
}
