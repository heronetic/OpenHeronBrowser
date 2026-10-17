// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Cookie jar with RFC 6265 matching and Secure/HttpOnly/SameSite handling.
//! Third-party (cross-site) requests only see and set `SameSite=None; Secure`
//! cookies, like Chrome; that is what embedded sign-in and challenge widgets
//! rely on. Persistent cookies are saved with the profile (see
//! `crate::storage`); session cookies end with the browser.

use std::time::{Duration, SystemTime};
use url::Url;

#[derive(Clone, Debug)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub host_only: bool,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
    pub expires: Option<SystemTime>,
    created: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

#[derive(Default)]
pub struct CookieJar {
    cookies: Vec<Cookie>,
    counter: u64,
    /// Bumped on every change (for the profile saver).
    changes: u64,
}

const MAX_COOKIES: usize = 3000;
const MAX_COOKIE_SIZE: usize = 4096;

fn default_path(url: &Url) -> String {
    let p = url.path();
    match p.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(i) => p[..i].to_string(),
    }
}

fn domain_match(host: &str, domain: &str) -> bool {
    host == domain || (host.ends_with(domain) && host.as_bytes()[host.len() - domain.len() - 1] == b'.')
}

fn path_match(req: &str, cookie: &str) -> bool {
    if req == cookie {
        return true;
    }
    if req.starts_with(cookie) {
        return cookie.ends_with('/') || req.as_bytes().get(cookie.len()) == Some(&b'/');
    }
    false
}

fn parse_http_date(s: &str) -> Option<SystemTime> {
    // Accepts e.g. "Wed, 21 Oct 2015 07:28:00 GMT" and "Wed, 21-Oct-2015 07:28:00 GMT".
    let s = s.replace('-', " ");
    let parts: Vec<&str> = s.split_whitespace().collect();
    let mut day = None;
    let mut month = None;
    let mut year = None;
    let mut time = None;
    for p in parts {
        let p = p.trim_end_matches(',');
        if p.contains(':') {
            let t: Vec<u64> = p.split(':').filter_map(|x| x.parse().ok()).collect();
            if t.len() == 3 {
                time = Some((t[0], t[1], t[2]));
            }
        } else if let Ok(n) = p.parse::<i64>() {
            if day.is_none() && n <= 31 && p.len() <= 2 {
                day = Some(n);
            } else {
                year = Some(if n < 70 { n + 2000 } else if n < 100 { n + 1900 } else { n });
            }
        } else {
            let m = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]
                .iter()
                .position(|m| p.to_ascii_lowercase().starts_with(m));
            if m.is_some() {
                month = m.map(|x| x as i64 + 1);
            }
        }
    }
    let (d, m, y, (hh, mm, ss)) = (day?, month?, year?, time?);
    // Days from civil (Howard Hinnant's algorithm).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + (hh * 3600 + mm * 60 + ss) as i64;
    if secs < 0 {
        return Some(SystemTime::UNIX_EPOCH);
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs as u64))
}

impl CookieJar {
    fn is_third_party(url: &Url, initiator: Option<&Url>, top_level: bool) -> bool {
        if top_level {
            return false;
        }
        match initiator {
            Some(i) if matches!(i.scheme(), "http" | "https") => super::site_of(i) != super::site_of(url),
            _ => false,
        }
    }

    /// Store a cookie from a Set-Cookie header (or document.cookie when `from_script`).
    pub fn set_from_header(&mut self, header: &str, url: &Url, initiator: Option<&Url>, top_level: bool) {
        self.set_cookie(header, url, initiator, top_level, false);
    }

    pub fn set_from_script(&mut self, s: &str, url: &Url) {
        self.set_cookie(s, url, None, true, true);
    }

    fn set_cookie(&mut self, header: &str, url: &Url, initiator: Option<&Url>, top_level: bool, from_script: bool) {
        // The name and value together are limited (attributes come on top).
        if header.split(';').next().map(|nv| nv.len()).unwrap_or(0) > MAX_COOKIE_SIZE || header.len() > 4 * MAX_COOKIE_SIZE {
            return;
        }
        let third_party = Self::is_third_party(url, initiator, top_level);
        let host = match url.host_str() {
            Some(h) => h.to_ascii_lowercase(),
            None => return,
        };
        let mut parts = header.split(';');
        let nv = parts.next().unwrap_or("");
        let (name, value) = match nv.split_once('=') {
            Some((n, v)) => (n.trim().to_string(), v.trim().trim_matches('"').to_string()),
            None => (String::new(), nv.trim().to_string()),
        };
        if name.is_empty() && value.is_empty() {
            return;
        }
        let mut c = Cookie {
            name,
            value,
            domain: host.clone(),
            host_only: true,
            path: default_path(url),
            secure: false,
            http_only: false,
            same_site: SameSite::Lax,
            expires: None,
            created: 0,
        };
        let mut max_age: Option<i64> = None;
        for attr in parts {
            let (k, v) = match attr.split_once('=') {
                Some((k, v)) => (k.trim().to_ascii_lowercase(), v.trim().to_string()),
                None => (attr.trim().to_ascii_lowercase(), String::new()),
            };
            match k.as_str() {
                "domain" => {
                    let d = v.trim_start_matches('.').to_ascii_lowercase();
                    if d.is_empty() {
                        continue;
                    }
                    // Must domain-match and must not be a bare public suffix.
                    if !domain_match(&host, &d) || !d.contains('.') {
                        return;
                    }
                    c.domain = d;
                    c.host_only = false;
                }
                "path" if v.starts_with('/') => c.path = v,
                "secure" => c.secure = true,
                "httponly" => c.http_only = true,
                "samesite" => {
                    c.same_site = match v.to_ascii_lowercase().as_str() {
                        "strict" => SameSite::Strict,
                        "none" => SameSite::None,
                        _ => SameSite::Lax,
                    }
                }
                "max-age" => max_age = v.parse().ok(),
                "expires" => {
                    if max_age.is_none() {
                        c.expires = parse_http_date(&v);
                    }
                }
                _ => {}
            }
        }
        if let Some(ma) = max_age {
            c.expires = Some(if ma <= 0 {
                SystemTime::UNIX_EPOCH
            } else {
                SystemTime::now() + Duration::from_secs(ma as u64)
            });
        }
        // Secure cookies need a secure origin (localhost counts as one).
        let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]") || host.ends_with(".localhost");
        if c.secure && url.scheme() != "https" && !local {
            return;
        }
        // Cross-site contexts may only set SameSite=None; Secure cookies.
        if third_party && !(c.same_site == SameSite::None && c.secure) {
            return;
        }
        // __Secure- / __Host- prefixes.
        if c.name.starts_with("__Secure-") && !c.secure {
            return;
        }
        if c.name.starts_with("__Host-") && (!c.secure || !c.host_only || c.path != "/") {
            return;
        }
        if from_script && c.http_only {
            return;
        }
        // Replace existing cookie with same (name, domain, path). As in
        // Chromium, a host-only cookie and a Domain= cookie for the same host
        // are different cookies (deleting one leaves the other).
        if let Some(pos) = self
            .cookies
            .iter()
            .position(|o| o.name == c.name && o.domain == c.domain && o.path == c.path && o.host_only == c.host_only)
        {
            if from_script && self.cookies[pos].http_only {
                return;
            }
            self.cookies.remove(pos);
            self.changes += 1;
        }
        let expired = c.expires.map(|e| e <= SystemTime::now()).unwrap_or(false);
        if !expired {
            self.counter += 1;
            c.created = self.counter;
            self.changes += 1;
            self.cookies.push(c);
            if self.cookies.len() > MAX_COOKIES {
                // Expired cookies go first, then the oldest one.
                let now = SystemTime::now();
                self.cookies.retain(|c| c.expires.map(|e| e > now).unwrap_or(true));
                if self.cookies.len() > MAX_COOKIES {
                    self.cookies.remove(0);
                }
            }
        }
    }

    /// Cookie header value for a request.
    pub fn header_for(&self, url: &Url, initiator: Option<&Url>, top_level: bool, http: bool) -> Option<String> {
        let third_party = Self::is_third_party(url, initiator, top_level);
        let host = url.host_str()?.to_ascii_lowercase();
        let path = url.path();
        let secure = url.scheme() == "https" || matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]") || host.ends_with(".localhost");
        let now = SystemTime::now();
        let cross_site_nav = match initiator {
            Some(i) => super::site_of(i) != super::site_of(url),
            None => false,
        };
        let mut matched: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| {
                (if c.host_only { host == c.domain } else { domain_match(&host, &c.domain) })
                    && path_match(path, &c.path)
                    && (!c.secure || secure)
                    && (http || !c.http_only)
                    && c.expires.map(|e| e > now).unwrap_or(true)
                    && !(cross_site_nav && c.same_site == SameSite::Strict)
                    && !(third_party && c.same_site != SameSite::None)
            })
            .collect();
        if matched.is_empty() {
            return None;
        }
        matched.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then(a.created.cmp(&b.created)));
        Some(
            matched
                .iter()
                .map(|c| if c.name.is_empty() { c.value.clone() } else { format!("{}={}", c.name, c.value) })
                .collect::<Vec<_>>()
                .join("; "),
        )
    }

    /// document.cookie getter.
    pub fn script_cookies(&self, url: &Url) -> String {
        self.header_for(url, None, true, false).unwrap_or_default()
    }

    pub fn clear(&mut self) {
        self.cookies.clear();
        self.changes += 1;
    }

    /// A counter that changes whenever the jar does.
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// Remove the cookies of `site` (a registrable domain) and its subdomains.
    pub fn clear_site(&mut self, site: &str) {
        let site = site.trim_start_matches('.').to_ascii_lowercase();
        let before = self.cookies.len();
        self.cookies.retain(|c| !domain_match(&c.domain, &site));
        if self.cookies.len() != before {
            self.changes += 1;
        }
    }

    /// Cookie counts per domain (for the storage settings page).
    pub fn domains(&self) -> Vec<(String, usize)> {
        let mut m: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        let now = SystemTime::now();
        for c in self.cookies.iter().filter(|c| c.expires.map(|e| e > now).unwrap_or(true)) {
            *m.entry(c.domain.clone()).or_default() += 1;
        }
        m.into_iter().collect()
    }

    /// The persistent (unexpired, with an expiry) cookies as JSON.
    pub fn to_json(&self) -> serde_json::Value {
        let now = SystemTime::now();
        let list: Vec<serde_json::Value> = self
            .cookies
            .iter()
            .filter_map(|c| {
                let e = c.expires?;
                if e <= now {
                    return None;
                }
                let secs = e.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs();
                Some(serde_json::json!({
                    "n": c.name, "v": c.value, "d": c.domain, "h": c.host_only, "p": c.path,
                    "s": c.secure, "o": c.http_only,
                    "ss": match c.same_site { SameSite::Strict => "strict", SameSite::Lax => "lax", SameSite::None => "none" },
                    "e": secs,
                }))
            })
            .collect();
        serde_json::Value::Array(list)
    }

    /// Restore cookies saved by `to_json` (expired ones are dropped).
    pub fn load_json(&mut self, v: &serde_json::Value) {
        let Some(list) = v.as_array() else { return };
        let now = SystemTime::now();
        for x in list {
            let s = |k: &str| x.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let b = |k: &str| x.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
            let Some(secs) = x.get("e").and_then(|v| v.as_u64()) else { continue };
            let expires = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
            if expires <= now {
                continue;
            }
            let name = s("n");
            let domain = s("d");
            let path = s("p");
            if domain.is_empty() || self.cookies.iter().any(|o| o.name == name && o.domain == domain && o.path == path) {
                continue;
            }
            self.counter += 1;
            self.cookies.push(Cookie {
                name,
                value: s("v"),
                domain,
                host_only: b("h"),
                path: if path.is_empty() { "/".into() } else { path },
                secure: b("s"),
                http_only: b("o"),
                same_site: match s("ss").as_str() {
                    "strict" => SameSite::Strict,
                    "none" => SameSite::None,
                    _ => SameSite::Lax,
                },
                expires: Some(expires),
                created: self.counter,
            });
        }
        self.cookies.truncate(MAX_COOKIES);
    }

    pub fn len(&self) -> usize {
        self.cookies.len()
    }
}
