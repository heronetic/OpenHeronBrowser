// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Character encodings (the WHATWG Encoding Standard, through encoding_rs):
//! label lookup, BOM sniffing, the HTML `<meta>` prescan, the XML declaration,
//! and a UTF-8/windows-1252 guess for undeclared resources.

use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8, WINDOWS_1252};

/// Decode a resource: BOM, then the declared (transport) charset, then for
/// HTML the `<meta>` prescan, else a UTF-8 / windows-1252 guess.
pub fn decode(bytes: &[u8], declared: Option<&str>, sniff_html: bool) -> String {
    decode_doc(bytes, declared, sniff_html, false).0
}

/// The encoding a label names (`None` for unknown labels and "replacement").
pub fn for_label(label: &str) -> Option<&'static Encoding> {
    Encoding::for_label_no_replacement(label.as_bytes())
}

/// The encoding a document's label names (the replacement encoding included).
fn for_doc_label(label: &str) -> Option<&'static Encoding> {
    Encoding::for_label(label.as_bytes())
}

/// Encode `s` in the encoding named `name` (its output encoding: UTF-16 and
/// replacement encode as UTF-8); an unmappable character is handed to `err`,
/// with the encoder back in its ASCII state (ISO-2022-JP).
pub fn encode_with(name: &str, s: &str, err: &dyn Fn(char, &mut Vec<u8>)) -> Vec<u8> {
    let enc = Encoding::for_label(name.as_bytes()).unwrap_or(UTF_8).output_encoding();
    if enc == UTF_8 {
        return s.as_bytes().to_vec();
    }
    let mut encoder = enc.new_encoder();
    let mut out = Vec::with_capacity(s.len() * 2 + 8);
    let mut src = s;
    let mut buf = vec![0u8; 64];
    loop {
        let (r, read, written) = encoder.encode_from_utf8_without_replacement(src, &mut buf, true);
        out.extend_from_slice(&buf[..written]);
        src = &src[read..];
        match r {
            encoding_rs::EncoderResult::InputEmpty => break,
            encoding_rs::EncoderResult::OutputFull => {}
            encoding_rs::EncoderResult::Unmappable(c) => {
                // Back to ASCII (an ASCII character switches stateful encoders), then the error.
                let (_, _, w) = encoder.encode_from_utf8_without_replacement("&", &mut buf, false);
                out.extend_from_slice(&buf[..w.saturating_sub(1)]);
                err(c, &mut out);
            }
        }
    }
    out
}

/// Pins a closure to the higher-ranked signature URL encoding overrides take.
pub fn override_fn<F: Fn(&str) -> std::borrow::Cow<'_, [u8]>>(f: F) -> F {
    f
}

/// The canonical name of the encoding for `label` (e.g. "ISO-8859-2").
pub fn canonical_name(label: &str) -> Option<&'static str> {
    for_label(label).map(|e| e.name())
}

/// Decode a document, also returning the encoding's name (`document.characterSet`).
/// `xml` reads the encoding from an XML declaration instead of `<meta>`.
pub fn decode_doc(bytes: &[u8], declared: Option<&str>, sniff_html: bool, xml: bool) -> (String, &'static str) {
    if let Some((enc, bom)) = Encoding::for_bom(bytes) {
        let (s, _) = enc.decode_without_bom_handling(&bytes[bom..]);
        return (s.into_owned(), enc.name());
    }
    let mut enc = declared.and_then(for_doc_label);
    if enc.is_none() && sniff_html {
        enc = prescan_meta(&bytes[..bytes.len().min(1024)]);
        if enc.is_none() && bytes.len() > 1024 {
            enc = prescan_meta(&bytes[..bytes.len().min(8192)]);
        }
    }
    if enc.is_none() && xml {
        enc = xml_declaration(bytes);
    }
    match enc {
        Some(e) if xml && e == UTF_8 => (xml_utf8(bytes), UTF_8.name()),
        Some(e) => {
            let (s, _) = e.decode_without_bom_handling(bytes);
            (s.into_owned(), e.name())
        }
        None if xml => (xml_utf8(bytes), UTF_8.name()),
        None => guess(bytes),
    }
}

/// UTF-8 for an XML document: an invalid byte sequence is a fatal error
/// (XML 1.0 §4.3.3), so the text ends there and the parser reports the
/// document as not well-formed, keeping what came before (as browsers do).
fn xml_utf8(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(e) => String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned(),
    }
}

/// A document fetched by script (XMLHttpRequest's document response):
/// BOM, then the given charset, then `<meta>` (HTML) or the XML declaration,
/// else UTF-8 — never a guess.
pub fn decode_fetched_doc(bytes: &[u8], declared: Option<&str>, html: bool) -> (String, &'static str) {
    if let Some((enc, bom)) = Encoding::for_bom(bytes) {
        let (s, _) = enc.decode_without_bom_handling(&bytes[bom..]);
        return (s.into_owned(), enc.name());
    }
    let mut enc = declared.and_then(for_label);
    if enc.is_none() {
        enc = if html { prescan_meta(&bytes[..bytes.len().min(1024)]) } else { xml_declaration(bytes) };
    }
    let e = enc.unwrap_or(UTF_8);
    let (s, _) = e.decode_without_bom_handling(bytes);
    (s.into_owned(), e.name())
}

/// No declaration: ASCII reads as windows-1252 (Chromium's default for
/// undeclared documents), valid UTF-8 as UTF-8, mostly-invalid UTF-8 as
/// windows-1252.
fn guess(bytes: &[u8]) -> (String, &'static str) {
    if bytes.is_ascii() {
        // SAFETY-free: ASCII is valid UTF-8.
        return (String::from_utf8_lossy(bytes).into_owned(), WINDOWS_1252.name());
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), UTF_8.name()),
        Err(_) => {
            let lossy = String::from_utf8_lossy(bytes);
            let bad = lossy.matches('\u{FFFD}').count();
            if bad * 50 > bytes.len().max(1) {
                let (s, _) = WINDOWS_1252.decode_without_bom_handling(bytes);
                (s.into_owned(), WINDOWS_1252.name())
            } else {
                (lossy.into_owned(), UTF_8.name())
            }
        }
    }
}

/// Encoding named by a `<meta>` charset: UTF-16 means UTF-8 there, and
/// x-user-defined means windows-1252.
fn meta_encoding(label: &str) -> Option<&'static Encoding> {
    let e = for_doc_label(label)?;
    Some(if e == UTF_16LE || e == UTF_16BE {
        UTF_8
    } else if e.name() == "x-user-defined" {
        WINDOWS_1252
    } else {
        e
    })
}

/// The charset in a `content` attribute ("text/html; charset=...").
fn charset_from_content(v: &str) -> Option<String> {
    let lower = v.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find("charset") {
        let mut k = from + i + 7;
        let b = lower.as_bytes();
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        if k >= b.len() || b[k] != b'=' {
            from = from + i + 7;
            continue;
        }
        k += 1;
        while k < b.len() && b[k].is_ascii_whitespace() {
            k += 1;
        }
        if k >= b.len() {
            return None;
        }
        let q = b[k];
        if q == b'"' || q == b'\'' {
            let rest = &v[k + 1..];
            return rest.find(q as char).map(|e| rest[..e].to_string());
        }
        let rest = &v[k..];
        let end = rest.find(|c: char| c.is_ascii_whitespace() || c == ';').unwrap_or(rest.len());
        return Some(rest[..end].to_string());
    }
    None
}

/// The HTML encoding-sniffing prescan (meta charset / http-equiv content-type).
fn prescan_meta(head: &[u8]) -> Option<&'static Encoding> {
    let s = String::from_utf8_lossy(head);
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i..].starts_with(b"<!--") {
            match find(&b[i + 4..], b"-->") {
                Some(e) => i += 4 + e + 3,
                None => return None,
            }
            continue;
        }
        if b[i] == b'<' && i + 5 <= b.len() && b[i + 1..i + 5].eq_ignore_ascii_case(b"meta") && b.get(i + 5).map(|c| c.is_ascii_whitespace() || *c == b'/').unwrap_or(false) {
            i += 5;
            let mut got_pragma = false;
            let mut need_pragma: Option<bool> = None;
            let mut charset: Option<&'static Encoding> = None;
            let mut seen = Vec::<String>::new();
            loop {
                let Some((name, value, next)) = attr(&s, i) else { break };
                i = next;
                if seen.contains(&name) {
                    continue;
                }
                seen.push(name.clone());
                match name.as_str() {
                    "http-equiv" => {
                        if value.eq_ignore_ascii_case("content-type") {
                            got_pragma = true;
                        }
                    }
                    "content" => {
                        if charset.is_none() {
                            if let Some(c) = charset_from_content(&value) {
                                charset = meta_encoding(&c);
                                need_pragma = Some(true);
                            }
                        }
                    }
                    "charset" => {
                        charset = meta_encoding(value.trim());
                        need_pragma = Some(false);
                    }
                    _ => {}
                }
            }
            match need_pragma {
                Some(true) if !got_pragma => {}
                Some(_) if charset.is_some() => return charset,
                _ => {}
            }
            continue;
        }
        if b[i] == b'<' && (b.get(i + 1).map(|c| c.is_ascii_alphabetic()).unwrap_or(false) || (b.get(i + 1) == Some(&b'/') && b.get(i + 2).map(|c| c.is_ascii_alphabetic()).unwrap_or(false))) {
            // Skip a tag and its attributes.
            i += 1;
            while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                i += 1;
            }
            while let Some((_, _, next)) = attr(&s, i) {
                i = next;
            }
            continue;
        }
        if b[i..].starts_with(b"<!") || b[i..].starts_with(b"</") || b[i..].starts_with(b"<?") {
            match find(&b[i..], b">") {
                Some(e) => i += e + 1,
                None => return None,
            }
            continue;
        }
        i += 1;
    }
    None
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// "Get an attribute" from the prescan: (lowercased name, value, next index),
/// or None at the end of the tag.
fn attr(s: &str, mut i: usize) -> Option<(String, String, usize)> {
    let b = s.as_bytes();
    while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
        i += 1;
    }
    if i >= b.len() || b[i] == b'>' {
        return None;
    }
    let mut name = String::new();
    while i < b.len() {
        let c = b[i];
        if c == b'=' && !name.is_empty() {
            break;
        }
        if c.is_ascii_whitespace() {
            break;
        }
        if c == b'/' || c == b'>' {
            return Some((name, String::new(), i));
        }
        name.push((c as char).to_ascii_lowercase());
        i += 1;
    }
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= b.len() || b[i] != b'=' {
        return Some((name, String::new(), i));
    }
    i += 1;
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= b.len() {
        return Some((name, String::new(), i));
    }
    let q = b[i];
    if q == b'"' || q == b'\'' {
        let rest = &s[i + 1..];
        return match rest.find(q as char) {
            Some(e) => Some((name, rest[..e].to_ascii_lowercase(), i + 1 + e + 1)),
            None => Some((name, rest.to_ascii_lowercase(), s.len())),
        };
    }
    let st = i;
    while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
        i += 1;
    }
    Some((name, s[st..i].to_ascii_lowercase(), i))
}

/// The `encoding` of an XML declaration.
fn xml_declaration(bytes: &[u8]) -> Option<&'static Encoding> {
    if !bytes.starts_with(b"<?xml") {
        return None;
    }
    let end = bytes.iter().take(512).position(|&c| c == b'>')?;
    let decl = String::from_utf8_lossy(&bytes[..end]);
    let i = decl.find("encoding")?;
    let rest = decl[i + 8..].trim_start().strip_prefix('=')?.trim_start();
    let q = rest.chars().next()?;
    if q != '"' && q != '\'' {
        return None;
    }
    let v = &rest[1..rest[1..].find(q)? + 1];
    let e = for_label(v)?;
    // A document that decoded as ASCII-compatible text can't really be UTF-16.
    Some(if e == UTF_16LE || e == UTF_16BE { UTF_8 } else { e })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_prescan() {
        let (_, n) = decode_doc(b"<!doctype html><meta charset=\"latin2\"><p>x", None, true, false);
        assert_eq!(n, "ISO-8859-2");
        let (_, n) = decode_doc(b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=shift_jis\">", None, true, false);
        assert_eq!(n, "Shift_JIS");
        let (_, n) = decode_doc(b"<meta content=\"text/html; charset=shift_jis\">", None, true, false);
        assert_eq!(n, "windows-1252");
        let (_, n) = decode_doc(b"<meta charset=utf-16>", None, true, false);
        assert_eq!(n, "UTF-8");
        let (_, n) = decode_doc(b"<!-- <meta charset=koi8-r> --><meta charset=gbk>", None, true, false);
        assert_eq!(n, "GBK");
        let (s, n) = decode_doc(&[0x82, 0xa0], Some("shift_jis"), true, false);
        assert_eq!((s.as_str(), n), ("\u{3042}", "Shift_JIS"));
    }

    #[test]
    fn xml_decl() {
        let (_, n) = decode_doc(b"<?xml version='1.0' encoding='iso-8859-15'?><a/>", None, false, true);
        assert_eq!(n, "ISO-8859-15");
        let (_, n) = decode_doc(b"<a/>", None, false, true);
        assert_eq!(n, "UTF-8");
    }
}
