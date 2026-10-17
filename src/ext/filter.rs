// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Content filters: network rules and element hiding in the Adblock Plus /
//! uBlock Origin list syntax (the widely used subset), and
//! declarativeNetRequest-style JSON rules (WebExtensions).
//!
//! Network rules are indexed three ways so a URL only meets the few rules
//! that could match it: pure domain rules (`||ads.example^`) by host, other
//! rules by a token that must appear in the URL, and a small rest that is
//! checked for every request.

use std::collections::{HashMap, HashSet};

/// The resource type of a request, as filter options name them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResType {
    Document = 0,
    Subdocument,
    Script,
    Stylesheet,
    Image,
    Font,
    Xhr,
    Media,
    Ping,
    Other,
}

impl ResType {
    fn bit(self) -> u16 {
        1 << (self as u16)
    }
    pub fn name(self) -> &'static str {
        match self {
            ResType::Document => "document",
            ResType::Subdocument => "subdocument",
            ResType::Script => "script",
            ResType::Stylesheet => "stylesheet",
            ResType::Image => "image",
            ResType::Font => "font",
            ResType::Xhr => "xmlhttprequest",
            ResType::Media => "media",
            ResType::Ping => "ping",
            ResType::Other => "other",
        }
    }
}

const ALL_TYPES: u16 = (1 << 10) - 1;
/// Rules without type options apply to everything but top-level documents.
const DEFAULT_TYPES: u16 = ALL_TYPES & !(1 << ResType::Document as u16);

fn type_bits(name: &str) -> Option<u16> {
    Some(match name {
        "script" => ResType::Script.bit(),
        "image" | "img" => ResType::Image.bit(),
        "stylesheet" | "css" => ResType::Stylesheet.bit(),
        "font" => ResType::Font.bit(),
        "xmlhttprequest" | "xhr" | "fetch" | "websocket" => ResType::Xhr.bit(),
        "subdocument" | "frame" | "sub_frame" => ResType::Subdocument.bit(),
        "document" | "doc" | "main_frame" => ResType::Document.bit(),
        "media" => ResType::Media.bit(),
        "ping" | "beacon" => ResType::Ping.bit(),
        "other" | "object" | "object-subrequest" | "csp_report" | "webtransport" | "webbundle" => ResType::Other.bit(),
        _ => return None,
    })
}

/// One request, as the matcher sees it.
pub struct ReqInfo<'a> {
    pub url: &'a str,
    /// The URL lowercased (most rules are case-insensitive).
    pub lower: String,
    pub host: String,
    /// Byte offset of the host in `url`.
    pub host_start: usize,
    /// Host of the document making the request ("" when none).
    pub doc_host: String,
    pub third_party: bool,
    pub ty: ResType,
}

impl<'a> ReqInfo<'a> {
    pub fn new(url: &'a str, host: &str, doc_host: &str, third_party: bool, ty: ResType) -> ReqInfo<'a> {
        let lower = url.to_ascii_lowercase();
        let host = host.to_ascii_lowercase();
        let after = lower.find("://").map(|i| i + 3).unwrap_or(0);
        let host_start = lower[after..].find(host.as_str()).map(|i| i + after).unwrap_or(after);
        ReqInfo { url, lower, host, host_start, doc_host: doc_host.to_ascii_lowercase(), third_party, ty }
    }
}

/// What a request should become.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// No rule says anything.
    None,
    /// Explicitly allowed (an exception rule matched).
    Allow,
    /// Blocked; optionally answered with a built-in neutered resource
    /// (`redirect=`) or a file from the extension.
    Block { redirect: Option<String> },
    /// Redirect to another URL (declarativeNetRequest `redirect.url`).
    RedirectUrl(String),
    /// Retry over https.
    Upgrade,
}

#[derive(Clone, Debug)]
enum Part {
    Lit(Vec<u8>),
    Wild,
    Sep,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Block,
    Allow,
    Upgrade,
}

#[derive(Debug)]
struct NetRule {
    parts: Vec<Part>,
    host_anchor: bool,
    start_anchor: bool,
    end_anchor: bool,
    regex: Option<regress::Regex>,
    kind: Kind,
    important: bool,
    /// 0 any, 1 third-party only, 2 first-party only.
    party: u8,
    types: u16,
    /// Document domains: included (`domain=a.com`) and excluded (`~b.com`).
    doc_inc: Vec<String>,
    doc_exc: Vec<String>,
    req_inc: Vec<String>,
    req_exc: Vec<String>,
    redirect: Option<String>,
    redirect_url: Option<String>,
    match_case: bool,
    priority: i64,
    text: String,
}

/// Is `host` equal to `domain` or one of its subdomains?
fn host_in(host: &str, domain: &str) -> bool {
    host == domain || host.len() > domain.len() && host.ends_with(domain) && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
}

fn is_sep(c: u8) -> bool {
    !(c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b'%'))
}

fn match_parts(parts: &[Part], url: &[u8], pos: usize, end_anchor: bool, budget: &mut u32) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    match parts.first() {
        None => !end_anchor || pos == url.len(),
        Some(Part::Lit(l)) => url.len() >= pos + l.len() && &url[pos..pos + l.len()] == l.as_slice() && match_parts(&parts[1..], url, pos + l.len(), end_anchor, budget),
        Some(Part::Sep) => {
            if pos == url.len() {
                return match_parts(&parts[1..], url, pos, end_anchor, budget);
            }
            is_sep(url[pos]) && match_parts(&parts[1..], url, pos + 1, end_anchor, budget)
        }
        Some(Part::Wild) => {
            let rest = &parts[1..];
            if rest.is_empty() {
                return true;
            }
            // Jump straight to occurrences of the next literal.
            if let Some(Part::Lit(l)) = rest.first() {
                let mut p = pos;
                while p + l.len() <= url.len() {
                    match find(&url[p..], l) {
                        Some(i) => {
                            if match_parts(rest, url, p + i, end_anchor, budget) {
                                return true;
                            }
                            p += i + 1;
                        }
                        None => return false,
                    }
                }
                return false;
            }
            (pos..=url.len()).any(|p| match_parts(rest, url, p, end_anchor, budget))
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let first = needle[0];
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        match hay[i..].iter().position(|&c| c == first) {
            Some(j) => {
                i += j;
                if i + needle.len() <= hay.len() && &hay[i..i + needle.len()] == needle {
                    return Some(i);
                }
                i += 1;
            }
            None => return None,
        }
    }
    None
}

impl NetRule {
    fn pattern_matches(&self, r: &ReqInfo) -> bool {
        let url = if self.match_case { r.url.as_bytes() } else { r.lower.as_bytes() };
        if let Some(re) = &self.regex {
            let s = if self.match_case { r.url } else { r.lower.as_str() };
            return re.find(s).is_some();
        }
        let mut budget = 20_000;
        if self.host_anchor {
            // At the start of the host or of any of its labels.
            let hs = r.host_start;
            let he = (hs + r.host.len()).min(url.len());
            let mut starts = vec![hs];
            for i in hs..he {
                if url[i] == b'.' {
                    starts.push(i + 1);
                }
            }
            return starts.into_iter().any(|p| match_parts(&self.parts, url, p, self.end_anchor, &mut budget));
        }
        if self.start_anchor {
            return match_parts(&self.parts, url, 0, self.end_anchor, &mut budget);
        }
        match self.parts.first() {
            Some(Part::Lit(l)) => {
                let mut p = 0;
                while p + l.len() <= url.len() {
                    match find(&url[p..], l) {
                        Some(i) => {
                            if match_parts(&self.parts, url, p + i, self.end_anchor, &mut budget) {
                                return true;
                            }
                            p += i + 1;
                        }
                        None => return false,
                    }
                }
                false
            }
            _ => (0..=url.len()).any(|p| match_parts(&self.parts, url, p, self.end_anchor, &mut budget)),
        }
    }

    fn applies(&self, r: &ReqInfo) -> bool {
        if self.types & r.ty.bit() == 0 {
            return false;
        }
        match self.party {
            1 if !r.third_party => return false,
            2 if r.third_party => return false,
            _ => {}
        }
        if !self.doc_inc.is_empty() && !self.doc_inc.iter().any(|d| host_in(&r.doc_host, d)) {
            return false;
        }
        if self.doc_exc.iter().any(|d| host_in(&r.doc_host, d)) {
            return false;
        }
        if !self.req_inc.is_empty() && !self.req_inc.iter().any(|d| host_in(&r.host, d)) {
            return false;
        }
        if self.req_exc.iter().any(|d| host_in(&r.host, d)) {
            return false;
        }
        self.pattern_matches(r)
    }
}

/// Parse a URL pattern (`||host^path*`, `|https://`, `/banner/`).
fn parse_pattern(p: &str, match_case: bool) -> Option<(Vec<Part>, bool, bool, bool, Option<regress::Regex>)> {
    if p.len() > 2 && p.starts_with('/') && p.ends_with('/') {
        let flags = if match_case { "" } else { "i" };
        let re = regress::Regex::with_flags(&p[1..p.len() - 1], flags).ok()?;
        return Some((Vec::new(), false, false, false, Some(re)));
    }
    let mut s = p;
    let mut host_anchor = false;
    let mut start_anchor = false;
    let mut end_anchor = false;
    if let Some(r) = s.strip_prefix("||") {
        host_anchor = true;
        s = r;
    } else if let Some(r) = s.strip_prefix('|') {
        start_anchor = true;
        s = r;
    }
    if let Some(r) = s.strip_suffix('|') {
        end_anchor = true;
        s = r;
    }
    let mut parts: Vec<Part> = Vec::new();
    let mut lit: Vec<u8> = Vec::new();
    for c in s.bytes() {
        match c {
            b'*' => {
                if !lit.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut lit)));
                }
                if !matches!(parts.last(), Some(Part::Wild)) {
                    parts.push(Part::Wild);
                }
            }
            b'^' => {
                if !lit.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut lit)));
                }
                parts.push(Part::Sep);
            }
            c => lit.push(if match_case { c } else { c.to_ascii_lowercase() }),
        }
    }
    if !lit.is_empty() {
        parts.push(Part::Lit(lit));
    }
    // Leading/trailing wildcards add nothing.
    while matches!(parts.first(), Some(Part::Wild)) && !host_anchor && !start_anchor {
        parts.remove(0);
    }
    if matches!(parts.last(), Some(Part::Wild)) {
        parts.pop();
        end_anchor = false;
    }
    Some((parts, host_anchor, start_anchor, end_anchor, None))
}

/// The pure host of a `||host^…` rule, when the pattern starts with one.
fn rule_host(rule: &NetRule) -> Option<String> {
    if !rule.host_anchor || rule.regex.is_some() {
        return None;
    }
    let Some(Part::Lit(l)) = rule.parts.first() else { return None };
    let end = l.iter().position(|&c| matches!(c, b'/' | b':' | b'?')).unwrap_or(l.len());
    let host = &l[..end];
    if host.is_empty() || host.contains(&b'*') || !host.iter().all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_')) {
        return None;
    }
    // The host must end there (a separator, path, or the pattern's end).
    let bounded = end < l.len() || matches!(rule.parts.get(1), Some(Part::Sep)) || (rule.parts.len() == 1 && rule.end_anchor);
    if !bounded || host.ends_with(b".") {
        return None;
    }
    Some(String::from_utf8_lossy(host).to_ascii_lowercase())
}

/// A token every matching URL must contain as a whole alphanumeric run.
fn rule_token(rule: &NetRule) -> Option<String> {
    if rule.regex.is_some() {
        return None;
    }
    let mut best: Option<Vec<u8>> = None;
    let n = rule.parts.len();
    for (pi, part) in rule.parts.iter().enumerate() {
        let Part::Lit(l) = part else { continue };
        let left_ok_at_start = if pi == 0 { rule.host_anchor || rule.start_anchor } else { matches!(rule.parts[pi - 1], Part::Sep) };
        let right_ok_at_end = if pi + 1 == n { rule.end_anchor } else { matches!(rule.parts[pi + 1], Part::Sep) };
        let mut i = 0;
        while i < l.len() {
            if !l[i].is_ascii_alphanumeric() {
                i += 1;
                continue;
            }
            let s = i;
            while i < l.len() && l[i].is_ascii_alphanumeric() {
                i += 1;
            }
            let left = if s == 0 { left_ok_at_start } else { true };
            let right = if i == l.len() { right_ok_at_end } else { true };
            let tok = &l[s..i];
            if left && right && tok.len() >= 2 && !is_common_token(tok) && best.as_ref().map(|b| tok.len() > b.len()).unwrap_or(true) {
                best = Some(tok.to_vec());
            }
        }
    }
    best.map(|b| String::from_utf8_lossy(&b).to_ascii_lowercase())
}

fn is_common_token(t: &[u8]) -> bool {
    matches!(t, b"http" | b"https" | b"com" | b"www" | b"js" | b"net" | b"org" | b"html" | b"php")
}

fn url_tokens(lower: &str) -> HashSet<&str> {
    lower.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| t.len() >= 2).collect()
}

/// Element hiding.
#[derive(Default, Debug)]
struct Cosmetic {
    generic: Vec<String>,
    /// (included domains, excluded domains, selector)
    specific: Vec<(Vec<String>, Vec<String>, String)>,
    /// `#@#` exceptions: (domains, selector); no domains = everywhere.
    exceptions: Vec<(Vec<String>, String)>,
}

/// A compiled filter list.
#[derive(Default, Debug)]
pub struct FilterSet {
    rules: Vec<NetRule>,
    by_host: HashMap<String, Vec<u32>>,
    by_token: HashMap<String, Vec<u32>>,
    rest: Vec<u32>,
    cosmetic: Cosmetic,
    /// Sites with `$document` exceptions (filtering fully off) and
    /// `$generichide` / `$elemhide` exceptions.
    doc_allow: Vec<String>,
    generichide: Vec<String>,
    elemhide: Vec<String>,
    pub stats: ListStats,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct ListStats {
    pub network: usize,
    pub cosmetic: usize,
    pub skipped: usize,
}

impl FilterSet {
    pub fn new() -> FilterSet {
        FilterSet::default()
    }

    /// Add rules from a list in ABP / uBO syntax.
    pub fn add_list(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('!') || line.starts_with('[') || line.starts_with("# ") || line == "#" {
                continue;
            }
            if !self.add_filter(line) {
                self.stats.skipped += 1;
            }
        }
        self.reindex();
    }

    fn add_filter(&mut self, line: &str) -> bool {
        // Element hiding: `domains##selector`, `domains#@#selector`.
        if let Some(i) = line.find("#@#") {
            let (doms, sel) = (&line[..i], &line[i + 3..]);
            if !cosmetic_ok(sel) {
                return false;
            }
            let (inc, _) = split_domains(doms, ',');
            self.cosmetic.exceptions.push((inc, sel.trim().to_string()));
            self.stats.cosmetic += 1;
            return true;
        }
        if let Some(i) = line.find("##") {
            let (doms, sel) = (&line[..i], &line[i + 2..]);
            if !cosmetic_ok(sel) {
                return false;
            }
            let sel = sel.trim().to_string();
            if doms.is_empty() {
                self.cosmetic.generic.push(sel);
            } else {
                let (inc, exc) = split_domains(doms, ',');
                if inc.is_empty() {
                    // `~a.com##x`: everywhere except there.
                    self.cosmetic.specific.push((Vec::new(), exc, sel));
                } else {
                    self.cosmetic.specific.push((inc, exc, sel));
                }
            }
            self.stats.cosmetic += 1;
            return true;
        }
        // Other cosmetic flavors (`#?#`, `#$#`, `#%#`, `$$`) aren't supported.
        if line.contains("#?#") || line.contains("#$#") || line.contains("#%#") || line.contains("$$") || line.contains("##+js") {
            return false;
        }
        self.add_network(line)
    }

    fn add_network(&mut self, line: &str) -> bool {
        let (mut body, exception) = match line.strip_prefix("@@") {
            Some(r) => (r, true),
            None => (line, false),
        };
        // Options after the last `$` that isn't inside a regex.
        let mut opts = "";
        if !(body.starts_with('/') && body.ends_with('/')) {
            if let Some(i) = body.rfind('$') {
                // `$` inside a path is rare; options are word-like.
                let o = &body[i + 1..];
                if o.chars().all(|c| c.is_ascii_alphanumeric() || ",~=|-_.*:/".contains(c)) {
                    opts = o;
                    body = &body[..i];
                }
            }
        }
        let mut rule = NetRule {
            parts: Vec::new(),
            host_anchor: false,
            start_anchor: false,
            end_anchor: false,
            regex: None,
            kind: if exception { Kind::Allow } else { Kind::Block },
            important: false,
            party: 0,
            types: 0,
            doc_inc: Vec::new(),
            doc_exc: Vec::new(),
            req_inc: Vec::new(),
            req_exc: Vec::new(),
            redirect: None,
            redirect_url: None,
            match_case: false,
            priority: 0,
            text: line.to_string(),
        };
        let mut not_types = 0u16;
        for o in opts.split(',').filter(|o| !o.is_empty()) {
            let (neg, o) = match o.strip_prefix('~') {
                Some(r) => (true, r),
                None => (false, o),
            };
            let (k, v) = match o.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (o, None),
            };
            match (k, v) {
                ("third-party" | "3p", None) => rule.party = if neg { 2 } else { 1 },
                ("first-party" | "1p", None) => rule.party = if neg { 1 } else { 2 },
                ("important", None) => rule.important = true,
                ("match-case", None) => rule.match_case = true,
                ("domain" | "from", Some(v)) => {
                    let (inc, exc) = split_domains(v, '|');
                    rule.doc_inc = inc;
                    rule.doc_exc = exc;
                }
                ("to", Some(v)) => {
                    let (inc, exc) = split_domains(v, '|');
                    rule.req_inc = inc;
                    rule.req_exc = exc;
                }
                ("redirect" | "redirect-rule", Some(v)) => {
                    // `name:priority`
                    rule.redirect = Some(v.split(':').next().unwrap_or(v).to_string());
                }
                ("document" | "doc", None) if exception => {
                    self.doc_allow_from(body);
                    return true;
                }
                ("generichide" | "ghide", None) if exception => {
                    if let Some(h) = simple_host(body) {
                        self.generichide.push(h);
                    }
                    return true;
                }
                ("elemhide" | "ehide", None) if exception => {
                    if let Some(h) = simple_host(body) {
                        self.elemhide.push(h);
                    }
                    return true;
                }
                ("all", None) => rule.types = ALL_TYPES,
                ("popup" | "popunder", None) => return false,
                (t, None) => match type_bits(t) {
                    Some(b) => {
                        if neg {
                            not_types |= b;
                        } else {
                            rule.types |= b;
                        }
                    }
                    // Unknown options (csp=, removeparam=, …): skip the rule
                    // rather than apply it too broadly.
                    None => return false,
                },
                _ => return false,
            }
        }
        if rule.types == 0 {
            rule.types = DEFAULT_TYPES;
        }
        rule.types &= !not_types;
        if rule.types == 0 {
            return false;
        }
        if body.is_empty() || body == "*" {
            if rule.doc_inc.is_empty() && rule.req_inc.is_empty() {
                return false;
            }
            body = "*";
        }
        let Some((parts, ha, sa, ea, re)) = parse_pattern(body, rule.match_case) else { return false };
        rule.parts = parts;
        rule.host_anchor = ha;
        rule.start_anchor = sa;
        rule.end_anchor = ea;
        rule.regex = re;
        if rule.important {
            rule.priority += 1 << 40;
        }
        self.rules.push(rule);
        self.stats.network += 1;
        true
    }

    fn doc_allow_from(&mut self, body: &str) {
        if let Some(h) = simple_host(body) {
            self.doc_allow.push(h);
        }
    }

    /// Add declarativeNetRequest rules (a JSON array).
    pub fn add_dnr(&mut self, json: &serde_json::Value) {
        let Some(list) = json.as_array() else { return };
        for r in list {
            if !self.add_dnr_rule(r) {
                self.stats.skipped += 1;
            }
        }
        self.reindex();
    }

    fn add_dnr_rule(&mut self, r: &serde_json::Value) -> bool {
        let action = &r["action"];
        let cond = &r["condition"];
        let kind = match action["type"].as_str().unwrap_or("") {
            "block" => Kind::Block,
            "allow" | "allowAllRequests" => Kind::Allow,
            "upgradeScheme" => Kind::Upgrade,
            "redirect" => Kind::Block,
            _ => return false,
        };
        let strs = |v: &serde_json::Value| -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str()).map(|s| s.to_ascii_lowercase()).collect()).unwrap_or_default() };
        let match_case = cond["isUrlFilterCaseSensitive"].as_bool().unwrap_or(false);
        let (parts, ha, sa, ea, re) = if let Some(re) = cond["regexFilter"].as_str() {
            match regress::Regex::with_flags(re, if match_case { "" } else { "i" }) {
                Ok(re) => (Vec::new(), false, false, false, Some(re)),
                Err(_) => return false,
            }
        } else {
            match parse_pattern(cond["urlFilter"].as_str().unwrap_or("*"), match_case) {
                Some(p) => p,
                None => return false,
            }
        };
        let mut types = 0u16;
        for t in strs(&cond["resourceTypes"]) {
            types |= type_bits(&t).unwrap_or(0);
        }
        if types == 0 {
            types = DEFAULT_TYPES;
            if action["type"].as_str() == Some("allowAllRequests") {
                types = ALL_TYPES;
            }
        }
        for t in strs(&cond["excludedResourceTypes"]) {
            types &= !type_bits(&t).unwrap_or(0);
        }
        let party = match cond["domainType"].as_str() {
            Some("thirdParty") => 1,
            Some("firstParty") => 2,
            _ => 0,
        };
        let mut doc_inc = strs(&cond["initiatorDomains"]);
        doc_inc.extend(strs(&cond["domains"]));
        let mut doc_exc = strs(&cond["excludedInitiatorDomains"]);
        doc_exc.extend(strs(&cond["excludedDomains"]));
        let (mut redirect, mut redirect_url) = (None, None);
        if action["type"].as_str() == Some("redirect") {
            let rd = &action["redirect"];
            if let Some(p) = rd["extensionPath"].as_str() {
                redirect = Some(format!("ext:{p}"));
            } else if let Some(u) = rd["url"].as_str() {
                redirect_url = Some(u.to_string());
            } else {
                return false;
            }
        }
        self.rules.push(NetRule {
            parts,
            host_anchor: ha,
            start_anchor: sa,
            end_anchor: ea,
            regex: re,
            kind,
            important: false,
            party,
            types,
            doc_inc,
            doc_exc,
            req_inc: strs(&cond["requestDomains"]),
            req_exc: strs(&cond["excludedRequestDomains"]),
            redirect,
            redirect_url,
            match_case,
            priority: r["priority"].as_i64().unwrap_or(1),
            text: format!("rule {}", r["id"]),
        });
        self.stats.network += 1;
        true
    }

    fn reindex(&mut self) {
        self.by_host.clear();
        self.by_token.clear();
        self.rest.clear();
        for (i, r) in self.rules.iter().enumerate() {
            let i = i as u32;
            if let Some(h) = rule_host(r) {
                self.by_host.entry(h).or_default().push(i);
            } else if let Some(t) = rule_token(r) {
                self.by_token.entry(t).or_default().push(i);
            } else if r.req_inc.len() == 1 {
                self.by_host.entry(r.req_inc[0].clone()).or_default().push(i);
            } else {
                self.rest.push(i);
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty() && self.cosmetic.generic.is_empty() && self.cosmetic.specific.is_empty()
    }

    /// Is filtering turned off for documents on `host` (`@@||host^$document`)?
    pub fn doc_allowed(&self, host: &str) -> bool {
        self.doc_allow.iter().any(|d| host_in(host, d))
    }

    /// Decide a request. Returns the verdict and the deciding rule's text.
    pub fn check(&self, r: &ReqInfo) -> (Verdict, Option<&str>) {
        if !r.doc_host.is_empty() && self.doc_allowed(&r.doc_host) {
            return (Verdict::Allow, None);
        }
        let mut cands: Vec<u32> = Vec::new();
        let h = r.host.as_str();
        let mut s = h;
        loop {
            if let Some(v) = self.by_host.get(s) {
                cands.extend_from_slice(v);
            }
            match s.find('.') {
                Some(i) => s = &s[i + 1..],
                None => break,
            }
        }
        for t in url_tokens(&r.lower) {
            if let Some(v) = self.by_token.get(t) {
                cands.extend_from_slice(v);
            }
        }
        cands.extend_from_slice(&self.rest);
        cands.sort_unstable();
        cands.dedup();
        // Highest priority wins; at equal priority allow beats block beats
        // upgrade (as in declarativeNetRequest; ABP exceptions beat blocks
        // unless the block is `$important`).
        let rank = |k: Kind| match k {
            Kind::Allow => 3,
            Kind::Block => 2,
            Kind::Upgrade => 1,
        };
        let mut best: Option<&NetRule> = None;
        for &i in &cands {
            let rule = &self.rules[i as usize];
            if let Some(b) = best {
                if (rule.priority, rank(rule.kind)) <= (b.priority, rank(b.kind)) {
                    continue;
                }
            }
            if rule.applies(r) {
                best = Some(rule);
            }
        }
        let Some(b) = best else { return (Verdict::None, None) };
        let v = match b.kind {
            Kind::Allow => Verdict::Allow,
            Kind::Upgrade => {
                if r.url.starts_with("http:") {
                    Verdict::Upgrade
                } else {
                    Verdict::None
                }
            }
            Kind::Block => match &b.redirect_url {
                Some(u) => Verdict::RedirectUrl(u.clone()),
                None => Verdict::Block { redirect: b.redirect.clone() },
            },
        };
        (v, Some(b.text.as_str()))
    }

    /// Element-hiding selectors for pages on `host`.
    pub fn hidden_selectors(&self, host: &str) -> Vec<&str> {
        let host = host.to_ascii_lowercase();
        if self.doc_allowed(&host) || self.elemhide.iter().any(|d| host_in(&host, d)) {
            return Vec::new();
        }
        let excepted: HashSet<&str> = self.cosmetic.exceptions.iter().filter(|(d, _)| d.is_empty() || d.iter().any(|d| host_in(&host, d))).map(|(_, s)| s.as_str()).collect();
        let mut out = Vec::new();
        if !self.generichide.iter().any(|d| host_in(&host, d)) {
            for s in &self.cosmetic.generic {
                if !excepted.contains(s.as_str()) {
                    out.push(s.as_str());
                }
            }
        }
        for (inc, exc, s) in &self.cosmetic.specific {
            if (inc.is_empty() || inc.iter().any(|d| host_in(&host, d))) && !exc.iter().any(|d| host_in(&host, d)) && !excepted.contains(s.as_str()) {
                out.push(s.as_str());
            }
        }
        out
    }
}

/// `a.com,~b.com` → (included, excluded).
fn split_domains(s: &str, sep: char) -> (Vec<String>, Vec<String>) {
    let mut inc = Vec::new();
    let mut exc = Vec::new();
    for d in s.split(sep).map(|d| d.trim()).filter(|d| !d.is_empty()) {
        match d.strip_prefix('~') {
            Some(x) => exc.push(x.to_ascii_lowercase()),
            None => inc.push(d.to_ascii_lowercase()),
        }
    }
    (inc, exc)
}

/// `||example.com^` → `example.com`.
fn simple_host(body: &str) -> Option<String> {
    let b = body.trim_start_matches("||").trim_start_matches('|');
    let b = b.trim_start_matches("https://").trim_start_matches("http://");
    let end = b.find(|c: char| matches!(c, '^' | '/' | '*' | '|' | ':')).unwrap_or(b.len());
    let h = &b[..end];
    if h.is_empty() {
        None
    } else {
        Some(h.to_ascii_lowercase())
    }
}

/// Selectors we can apply as plain CSS (no procedural extensions).
fn cosmetic_ok(sel: &str) -> bool {
    let s = sel.trim();
    !s.is_empty()
        && !s.starts_with('+')
        && !["-abp-", ":has-text(", ":contains(", ":xpath(", ":upward(", ":remove(", ":style(", ":matches-css", ":min-text-length(", ":watch-attr(", ":others(", ":matches-path(", ":matches-attr(", ":if(", ":if-not(", ":nth-ancestor(", ":remove-attr(", ":remove-class("]
            .iter()
            .any(|x| s.contains(x))
        && !s.contains('{')
        && !s.contains('}')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req<'a>(url: &'a str, doc: &str, ty: ResType) -> ReqInfo<'a> {
        let u = url::Url::parse(url).unwrap();
        let host = u.host_str().unwrap_or("").to_string();
        let tp = !doc.is_empty() && !host_in(&host, doc) && !host_in(doc, &host);
        ReqInfo::new(url, &host, doc, tp, ty)
    }

    #[test]
    fn network_rules() {
        let mut f = FilterSet::new();
        f.add_list("||doubleclick.net^\n||example.com/ads/*$script\n/banner/*/img^\n@@||doubleclick.net/allowed^\n||tracker.io^$third-party\n||ga.example^$script,redirect=noop.js\n|https://exact.test/x|\n");
        let b = |u: &str, d: &str, t| f.check(&req(u, d, t)).0;
        assert_eq!(b("https://ad.doubleclick.net/x", "news.com", ResType::Script), Verdict::Block { redirect: None });
        assert_eq!(b("https://doubleclick.net/", "news.com", ResType::Image), Verdict::Block { redirect: None });
        assert_eq!(b("https://notdoubleclick.net/", "news.com", ResType::Image), Verdict::None);
        assert_eq!(b("https://doubleclick.net/allowed/1", "news.com", ResType::Image), Verdict::Allow);
        assert_eq!(b("https://example.com/ads/a.js", "x.com", ResType::Script), Verdict::Block { redirect: None });
        assert_eq!(b("https://example.com/ads/a.png", "x.com", ResType::Image), Verdict::None);
        assert_eq!(b("https://cdn.x/banner/12/img?x", "x.com", ResType::Image), Verdict::Block { redirect: None });
        assert_eq!(b("https://cdn.x/banner/12/imgx", "x.com", ResType::Image), Verdict::None);
        assert_eq!(b("https://tracker.io/p", "a.com", ResType::Image), Verdict::Block { redirect: None });
        assert_eq!(b("https://tracker.io/p", "tracker.io", ResType::Image), Verdict::None);
        assert_eq!(b("https://ga.example/a.js", "a.com", ResType::Script), Verdict::Block { redirect: Some("noop.js".into()) });
        assert_eq!(b("https://exact.test/x", "a.com", ResType::Image), Verdict::Block { redirect: None });
        assert_eq!(b("https://exact.test/xy", "a.com", ResType::Image), Verdict::None);
        // Top-level documents are never blocked by default-typed rules.
        assert_eq!(b("https://doubleclick.net/", "", ResType::Document), Verdict::None);
    }

    #[test]
    fn cosmetic_rules() {
        let mut f = FilterSet::new();
        f.add_list("##.ad-banner\nnews.com##.promo\n~shop.news.com,news.com##.x\nnews.com#@#.ad-banner\nexample.org##div:has-text(Ad)\n");
        assert_eq!(f.hidden_selectors("other.com"), vec![".ad-banner"]);
        assert_eq!(f.hidden_selectors("news.com"), vec![".promo", ".x"]);
        assert_eq!(f.hidden_selectors("shop.news.com"), vec![".promo"]);
        assert_eq!(f.stats.skipped, 1);
    }

    #[test]
    fn dnr_rules() {
        let mut f = FilterSet::new();
        let j: serde_json::Value = serde_json::from_str(r#"[{"id":1,"priority":1,"action":{"type":"block"},"condition":{"urlFilter":"||ads.test^","resourceTypes":["script"]}},
            {"id":2,"priority":2,"action":{"type":"allow"},"condition":{"urlFilter":"||ads.test/ok","resourceTypes":["script"]}},
            {"id":3,"priority":1,"action":{"type":"upgradeScheme"},"condition":{"requestDomains":["plain.test"]}}]"#).unwrap();
        f.add_dnr(&j);
        assert_eq!(f.check(&req("https://ads.test/a.js", "a.com", ResType::Script)).0, Verdict::Block { redirect: None });
        assert_eq!(f.check(&req("https://ads.test/ok.js", "a.com", ResType::Script)).0, Verdict::Allow);
        assert_eq!(f.check(&req("http://plain.test/x", "a.com", ResType::Image)).0, Verdict::Upgrade);
    }
}
