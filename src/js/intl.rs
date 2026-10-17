// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Locale services behind the built-in `Intl` (intl.js): time zones from the
//! tz database, collation and text segmentation. They are only used when the
//! JavaScript engine has no Intl of its own (an ICU-backed engine like V8
//! brings its own).

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;

use jiff::tz::{Offset, TimeZone};
use jiff::Timestamp;
use unicode_segmentation::UnicodeSegmentation;

use super::host::*;

// ------------------------------------------------------------- time zones

const UTC_ALIASES: &[&str] = &[
    "UTC", "ETC/UTC", "ETC/UCT", "UCT", "GMT", "ETC/GMT", "ETC/GMT0", "ETC/GMT+0", "ETC/GMT-0", "GMT0", "GMT+0", "GMT-0", "ETC/UNIVERSAL", "UNIVERSAL", "ETC/ZULU", "ZULU",
    "ETC/GREENWICH", "GREENWICH", "Z",
];

thread_local! {
    static ZONES: RefCell<HashMap<String, Option<(String, TimeZone)>>> = RefCell::new(HashMap::new());
}

/// "+05:30", "-08", "+0530" -> seconds east of UTC.
fn parse_offset(s: &str) -> Option<i32> {
    let b = s.as_bytes();
    let sign = match b.first()? {
        b'+' => 1,
        b'-' | 0xe2 => -1,
        _ => return None,
    };
    let rest: String = s[1..].chars().filter(|c| *c != ':').collect();
    if !(rest.len() == 2 || rest.len() == 4) || !rest.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let h: i32 = rest[..2].parse().ok()?;
    let m: i32 = if rest.len() == 4 { rest[2..].parse().ok()? } else { 0 };
    if h > 23 || m > 59 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60))
}

/// A time zone by (case-insensitive) IANA name or UTC offset, with its
/// canonical spelling.
fn zone(name: &str) -> Option<(String, TimeZone)> {
    let key = name.trim().to_string();
    if let Some(z) = ZONES.with(|z| z.borrow().get(&key).cloned()) {
        return z;
    }
    let up = key.to_ascii_uppercase();
    let found = if UTC_ALIASES.contains(&up.as_str()) {
        Some(("UTC".to_string(), TimeZone::UTC))
    } else if let Some(secs) = parse_offset(&key) {
        let a = secs.abs();
        let canon = format!("{}{:02}:{:02}", if secs < 0 { '-' } else { '+' }, a / 3600, a / 60 % 60);
        Offset::from_seconds(secs).ok().map(|o| (canon, TimeZone::fixed(o)))
    } else if key.is_empty() || key.contains("..") {
        None
    } else {
        jiff::tz::db().get(&key).ok().map(|tz| (tz.iana_name().unwrap_or(&key).to_string(), tz))
    };
    ZONES.with(|z| z.borrow_mut().insert(key, found.clone()));
    found
}

/// The system time zone's IANA name ("UTC" when it can't be named).
pub fn system_zone_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| match TimeZone::try_system() {
        Ok(tz) => match tz.iana_name() {
            Some(n) => zone(n).map(|z| z.0).unwrap_or_else(|| n.to_string()),
            None => "UTC".to_string(),
        },
        Err(_) => "UTC".to_string(),
    })
    .clone()
}

/// Offset of the system time zone from UTC at `unix_seconds` (for `Date`).
pub fn local_offset_seconds(unix_seconds: i64) -> i32 {
    thread_local! {
        static SYS: Option<TimeZone> = TimeZone::try_system().ok();
    }
    SYS.with(|tz| match (tz, Timestamp::from_second(unix_seconds)) {
        (Some(tz), Ok(ts)) => tz.to_offset_info(ts).offset().seconds(),
        _ => 0,
    })
}

/// tzResolve(name) -> canonical name, or null when unknown. Without a name,
/// the system time zone.
fn n_tz_resolve(a: &mut dyn HostArgs) -> HostResult {
    Ok(match opt_st(a, 0) {
        None => HostValue::String(system_zone_name()),
        Some(n) => jopt(zone(&n).map(|z| z.0)),
    })
}

/// tzOffset(name, epochMs) -> [offset seconds east of UTC, abbreviation, isDst].
fn n_tz_offset(a: &mut dyn HostArgs) -> HostResult {
    let name = st(a, 0);
    let ms = num(a, 1);
    let Some((_, tz)) = zone(&name) else { return Ok(HostValue::Null) };
    if !ms.is_finite() {
        return Ok(HostValue::Null);
    }
    let Ok(ts) = Timestamp::from_millisecond(ms.floor() as i64) else { return Ok(HostValue::Null) };
    let info = tz.to_offset_info(ts);
    Ok(HostValue::Array(vec![HostValue::from(info.offset().seconds()), HostValue::String(info.abbreviation().to_string()), HostValue::Bool(info.dst().is_dst())]))
}

/// tzList() -> the canonical zone names, sorted.
fn n_tz_list(_a: &mut dyn HostArgs) -> HostResult {
    let mut v: Vec<String> = jiff::tz::db()
        .available()
        .map(|n| n.as_str().to_string())
        .filter(|n| n.contains('/') && !n.starts_with("Etc/") && !n.starts_with("posix") && !n.starts_with("right") && !n.starts_with("SystemV") && !n.starts_with("US/"))
        .collect();
    v.push("UTC".into());
    v.sort();
    v.dedup();
    Ok(jstrs(v))
}

// -------------------------------------------------------------- collation

/// Collation flags passed from script.
const SENS_MASK: u32 = 3; // 0 base, 1 accent, 2 case, 3 variant
const NUMERIC: u32 = 4;
const IGNORE_PUNCT: u32 = 8;
const UPPER_FIRST: u32 = 16;

/// Primary groups, in root collation order.
const G_SPACE: u32 = 0x0100_0000;
const G_PUNCT: u32 = 0x0200_0000;
const G_SYMBOL: u32 = 0x0300_0000;
const G_CURRENCY: u32 = 0x0380_0000;
const G_DIGIT: u32 = 0x0400_0000;
const G_LATIN: u32 = 0x0500_0000;
const G_LETTER: u32 = 0x0600_0000;

/// ASCII punctuation and symbols in root collation order.
const ASCII_ORDER: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

#[derive(Default)]
struct Keys {
    primary: Vec<u32>,
    secondary: Vec<u32>,
    tertiary: Vec<u8>,
}

fn is_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC | 0x0900..=0x0903 | 0x093A..=0x094F | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0x3099..=0x309A | 0xFE20..=0xFE2F)
}

/// Letters that sort as another letter (or two) at the primary level.
fn latin_fold(c: char) -> Option<&'static str> {
    Some(match c {
        'ø' | 'Ø' => "o",
        'đ' | 'Đ' | 'ð' | 'Ð' => "d",
        'ł' | 'Ł' => "l",
        'ħ' | 'Ħ' => "h",
        'ŧ' | 'Ŧ' => "t",
        'ı' => "i",
        'æ' | 'Æ' => "ae",
        'œ' | 'Œ' => "oe",
        'ß' | 'ẞ' => "ss",
        'ĳ' | 'Ĳ' => "ij",
        _ => return None,
    })
}

fn collation_keys(s: &str, flags: u32) -> Keys {
    let nfd = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd().normalize(s);
    let ignore_punct = flags & IGNORE_PUNCT != 0;
    let numeric = flags & NUMERIC != 0;
    let mut k = Keys::default();
    let chars: Vec<char> = nfd.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if is_mark(c) {
            k.secondary.push(0x100 + (c as u32 & 0xFFFF));
            continue;
        }
        let upper = c.is_uppercase() as u8;
        if c.is_whitespace() {
            if !ignore_punct {
                k.primary.push(G_SPACE + c as u32);
                k.secondary.push(1);
                k.tertiary.push(0);
            }
            continue;
        }
        if c.is_control() || matches!(c, '\u{200B}'..='\u{200F}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}') {
            continue;
        }
        if c.is_ascii_digit() {
            if numeric {
                let start = i - 1;
                let mut end = i;
                while end < chars.len() && chars[end].is_ascii_digit() {
                    end += 1;
                }
                let mut digits = &chars[start..end];
                while digits.len() > 1 && digits[0] == '0' {
                    digits = &digits[1..];
                }
                k.primary.push(G_DIGIT + 0x100 + digits.len() as u32);
                for d in digits {
                    k.primary.push(G_DIGIT + (*d as u32 - '0' as u32));
                }
                k.secondary.push(1);
                k.tertiary.push(0);
                i = end;
            } else {
                k.primary.push(G_DIGIT + (c as u32 - '0' as u32));
                k.secondary.push(1);
                k.tertiary.push(0);
            }
            continue;
        }
        if c.is_ascii() && !c.is_ascii_alphanumeric() {
            if !ignore_punct {
                let pos = ASCII_ORDER.find(c).unwrap_or(ASCII_ORDER.len()) as u32;
                let group = if c == '$' { G_CURRENCY } else if pos < ASCII_ORDER.find('@').unwrap_or(0) as u32 { G_PUNCT } else { G_SYMBOL };
                k.primary.push(group + pos);
                k.secondary.push(1);
                k.tertiary.push(0);
            }
            continue;
        }
        if c.is_alphabetic() {
            let folded: Option<&str> = latin_fold(c);
            let lower: Vec<char> = match folded {
                Some(f) => f.chars().collect(),
                None => c.to_lowercase().collect(),
            };
            for (j, l) in lower.iter().enumerate() {
                let p = if l.is_ascii_lowercase() { G_LATIN + (*l as u32 - 'a' as u32) * 4 } else { G_LETTER + *l as u32 };
                k.primary.push(p);
                k.secondary.push(if folded.is_some() && j == 0 { 2 } else { 1 });
                k.tertiary.push(upper);
            }
            continue;
        }
        if c.is_numeric() {
            k.primary.push(G_DIGIT + 0x1000 + c as u32);
            k.secondary.push(1);
            k.tertiary.push(0);
            continue;
        }
        // Other punctuation and symbols.
        if !ignore_punct {
            let general_punct = matches!(c as u32, 0x2010..=0x2027 | 0x2030..=0x205E | 0x3000..=0x303F | 0x00A1 | 0x00A7 | 0x00AB | 0x00B6 | 0x00B7 | 0x00BB | 0x00BF);
            let currency = matches!(c as u32, 0x00A2..=0x00A5 | 0x058F | 0x060B | 0x09F2 | 0x09F3 | 0x0E3F | 0x17DB | 0x20A0..=0x20CF | 0xFDFC | 0xFE69 | 0xFF04 | 0xFFE0 | 0xFFE1 | 0xFFE5 | 0xFFE6);
            k.primary.push(if general_punct {
                G_PUNCT + 0x100 + c as u32
            } else if currency {
                G_CURRENCY + 0x100 + c as u32
            } else {
                G_SYMBOL + 0x100 + c as u32
            });
            k.secondary.push(1);
            k.tertiary.push(0);
        }
    }
    k
}

/// Compare two strings like a root/English collator.
pub fn collate(a: &str, b: &str, flags: u32) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let ka = collation_keys(a, flags);
    let kb = collation_keys(b, flags);
    let sens = flags & SENS_MASK;
    let o = ka.primary.cmp(&kb.primary);
    if o != Ordering::Equal {
        return o;
    }
    if sens == 1 || sens == 3 {
        let o = ka.secondary.cmp(&kb.secondary);
        if o != Ordering::Equal {
            return o;
        }
    }
    if sens >= 2 {
        let (ta, tb) = if flags & UPPER_FIRST != 0 { (ka.tertiary.iter().map(|x| 1 - x).collect::<Vec<_>>(), kb.tertiary.iter().map(|x| 1 - x).collect::<Vec<_>>()) } else { (ka.tertiary, kb.tertiary) };
        let o = ta.cmp(&tb);
        if o != Ordering::Equal {
            return o;
        }
    }
    if sens == 3 && flags & IGNORE_PUNCT == 0 {
        // Identical level: code point order breaks remaining ties.
        let na = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd().normalize(a).into_owned();
        let nb = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd().normalize(b).into_owned();
        return na.cmp(&nb);
    }
    Ordering::Equal
}

/// collate(a, b, flags) -> -1, 0 or 1.
fn n_collate(a: &mut dyn HostArgs) -> HostResult {
    let x = st(a, 0);
    let y = st(a, 1);
    let n = num(a, 2);
    let flags = if n.is_finite() { n as u32 } else { 3 };
    Ok(HostValue::from(match collate(&x, &y, flags) {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    }))
}

// ----------------------------------------------------------- segmentation

/// segment(str, granularity) -> flat [utf16 start, isWordLike, ...] of each segment.
fn n_segment(a: &mut dyn HostArgs) -> HostResult {
    let s = st(a, 0);
    let g = st(a, 1);
    let starts: Vec<(usize, &str)> = match g.as_str() {
        "word" => s.split_word_bound_indices().collect(),
        "sentence" => s.split_sentence_bound_indices().collect(),
        _ => s.grapheme_indices(true).collect(),
    };
    // Byte offsets -> UTF-16 offsets.
    let mut out = Vec::with_capacity(starts.len() * 2);
    let mut u16pos = 0usize;
    let mut bytepos = 0usize;
    for (start, seg) in starts {
        u16pos += s[bytepos..start].encode_utf16().count();
        bytepos = start;
        out.push(HostValue::from(u16pos));
        out.push(HostValue::Bool(g == "word" && seg.chars().any(|c| c.is_alphanumeric())));
    }
    Ok(HostValue::Array(out))
}

pub static INTL_NATIVES: &[(&str, NativeFn, u32)] = &[("tzResolve", n_tz_resolve, 1), ("tzOffset", n_tz_offset, 2), ("tzList", n_tz_list, 0), ("collate", n_collate, 3), ("segment", n_segment, 2)];

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(v: &[&str], flags: u32) -> Vec<String> {
        let mut v: Vec<String> = v.iter().map(|s| s.to_string()).collect();
        v.sort_by(|a, b| collate(a, b, flags));
        v
    }

    #[test]
    fn root_order() {
        assert_eq!(sorted(&["b", "a", "B", "ä", "A", "10", "9"], 3), ["10", "9", "a", "A", "ä", "b", "B"]);
        assert_eq!(sorted(&["b10", "b9", "a", "B1"], 4), ["a", "B1", "b9", "b10"]);
        assert_eq!(collate("a", "Á", 0), Ordering::Equal);
        assert_eq!(collate("a", "á", 1), Ordering::Less);
        assert_eq!(collate("résumé", "resume", 0), Ordering::Equal);
        assert_eq!(collate("a b", "ab", 3), Ordering::Less);
        assert_eq!(collate("Straße", "strasse", 0), Ordering::Equal);
    }

    #[test]
    fn zones() {
        assert_eq!(zone("utc").map(|z| z.0), Some("UTC".to_string()));
        assert_eq!(zone("+0530").map(|z| z.0), Some("+05:30".to_string()));
        assert!(zone("Mars/Base").is_none());
        assert_eq!(parse_offset("-08:00"), Some(-8 * 3600));
    }
}
