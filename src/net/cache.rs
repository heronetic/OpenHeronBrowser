// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Bounded in-memory HTTP cache (LRU by bytes) following RFC 9111: explicit
//! and heuristic freshness (Cache-Control max-age, Expires minus Date,
//! Last-Modified), Age, Vary matching, validators for revalidation and
//! freshening a stored response from a 304.

use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::Response;

struct Entry {
    resp: Response,
    stored: Instant,
    /// Age when stored (the Age header).
    initial_age: Duration,
    lifetime: Duration,
    /// Cache-Control: no-cache on the response: always revalidate.
    no_cache: bool,
    /// The request's values of the headers named by Vary.
    vary: Vec<(String, Option<String>)>,
    size: usize,
    last_use: u64,
}

impl Entry {
    fn fresh(&self) -> bool {
        !self.no_cache && self.initial_age + self.stored.elapsed() < self.lifetime
    }
}

/// A stored response and whether it may be used without revalidation.
pub struct Cached {
    pub resp: Response,
    pub fresh: bool,
}

pub struct HttpCache {
    map: HashMap<String, Entry>,
    bytes: usize,
    max_bytes: usize,
    tick: u64,
}

/// Statuses that are "heuristically cacheable" (RFC 9110 §15.1).
const HEURISTIC: [u16; 11] = [200, 203, 204, 206, 300, 301, 308, 404, 405, 410, 501];

/// Seconds since the epoch of an HTTP-date (IMF-fixdate, RFC 850 or asctime).
pub fn parse_http_date(s: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let s = s.trim();
    let parts: Vec<&str> = s.split(|c: char| c == ' ' || c == ',' || c == '-').filter(|p| !p.is_empty()).collect();
    // IMF-fixdate: Sun, 06 Nov 1994 08:49:37 GMT  -> [Sun, 06, Nov, 1994, 08:49:37, GMT]
    // RFC 850:     Sunday, 06-Nov-94 08:49:37 GMT -> [Sunday, 06, Nov, 94, 08:49:37, GMT]
    // asctime:     Sun Nov  6 08:49:37 1994        -> [Sun, Nov, 6, 08:49:37, 1994]
    let month = |m: &str| MONTHS.iter().position(|x| m.len() >= 3 && x.eq_ignore_ascii_case(&m[..3])).map(|i| i as i64 + 1);
    let (day, mon, year, time) = if parts.len() >= 5 && month(parts[1]).is_some() && !parts[1].chars().next()?.is_ascii_digit() {
        (parts[2].parse::<i64>().ok()?, month(parts[1])?, parts[4].parse::<i64>().ok()?, parts[3])
    } else if parts.len() >= 5 {
        let y = parts[3].parse::<i64>().ok()?;
        (parts[1].parse::<i64>().ok()?, month(parts[2])?, if y < 100 { if y < 70 { 2000 + y } else { 1900 + y } } else { y }, parts[4])
    } else {
        return None;
    };
    let t: Vec<i64> = time.split(':').map(|x| x.parse::<i64>().ok()).collect::<Option<Vec<_>>>()?;
    if t.len() != 3 || !(1..=31).contains(&day) {
        return None;
    }
    // Days from the civil date (Howard Hinnant's algorithm).
    let (y, m) = if mon <= 2 { (year - 1, mon + 9) } else { (year, mon - 3) };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + t[0] * 3600 + t[1] * 60 + t[2])
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn directives(resp: &Response) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for (k, v) in &resp.headers {
        if !k.eq_ignore_ascii_case("cache-control") {
            continue;
        }
        for d in v.split(',') {
            let d = d.trim();
            if d.is_empty() {
                continue;
            }
            match d.split_once('=') {
                Some((n, val)) => out.push((n.trim().to_ascii_lowercase(), Some(val.trim().trim_matches('"').to_string()))),
                None => out.push((d.to_ascii_lowercase(), None)),
            }
        }
    }
    out
}

/// The response's freshness lifetime: max-age, else Expires − Date, else
/// heuristic (a tenth of the time since Last-Modified) for cacheable statuses.
fn lifetime(resp: &Response, dirs: &[(String, Option<String>)]) -> Option<Duration> {
    if let Some((_, Some(v))) = dirs.iter().find(|(n, _)| n == "max-age") {
        return Some(Duration::from_secs(v.parse::<u64>().unwrap_or(0)));
    }
    let date = resp.header("date").and_then(parse_http_date).unwrap_or_else(now_secs);
    if let Some(e) = resp.header("expires") {
        return Some(match parse_http_date(e) {
            Some(exp) if exp > date => Duration::from_secs((exp - date) as u64),
            _ => Duration::ZERO,
        });
    }
    if HEURISTIC.contains(&resp.status) {
        if let Some(lm) = resp.header("last-modified").and_then(parse_http_date) {
            if date > lm && !resp.content_type().contains("html") {
                return Some(Duration::from_secs((((date - lm) / 10) as u64).min(86400)));
            }
        }
    }
    None
}

impl HttpCache {
    pub fn new(max_bytes: usize) -> Self {
        HttpCache { map: HashMap::new(), bytes: 0, max_bytes, tick: 0 }
    }

    fn key(url: &url::Url) -> String {
        let mut u = url.clone();
        u.set_fragment(None);
        u.to_string()
    }

    /// A fresh stored response for a plain GET (no Vary-dependent entries).
    pub fn lookup(&mut self, url: &url::Url) -> Option<Response> {
        let k = Self::key(url);
        self.tick += 1;
        let tick = self.tick;
        let e = self.map.get_mut(&k)?;
        if !e.fresh() || !e.vary.is_empty() {
            return None;
        }
        e.last_use = tick;
        let mut r = e.resp.clone();
        r.from_cache = true;
        Some(r)
    }

    /// The stored response for a request (its headers must match the entry's
    /// Vary), fresh or not.
    pub fn lookup_for(&mut self, url: &url::Url, req_headers: &[(String, String)]) -> Option<Cached> {
        let k = Self::key(url);
        self.tick += 1;
        let tick = self.tick;
        let e = self.map.get_mut(&k)?;
        for (name, val) in &e.vary {
            let cur = super::header_value(req_headers, name).map(|s| s.to_string());
            if &cur != val {
                return None;
            }
        }
        e.last_use = tick;
        let mut r = e.resp.clone();
        r.from_cache = true;
        Some(Cached { resp: r, fresh: e.fresh() })
    }

    pub fn store(&mut self, resp: &Response) {
        self.store_for(resp, &[], false);
    }

    /// Store a response to a GET request. `validators_ok`: keep it even with
    /// no freshness lifetime, for revalidation (ETag / Last-Modified).
    pub fn store_for(&mut self, resp: &Response, req_headers: &[(String, String)], validators_ok: bool) {
        if resp.url.scheme() == "data" || resp.url.scheme() == "file" || resp.url.scheme() == "blob" {
            return;
        }
        let dirs = directives(resp);
        let has = |n: &str| dirs.iter().any(|(d, _)| d == n);
        if has("no-store") || (has("private") && resp.header("set-cookie").is_some()) {
            return;
        }
        let vary_hdr = resp.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("vary")).map(|(_, v)| v.clone()).collect::<Vec<_>>().join(",");
        if vary_hdr.split(',').any(|v| v.trim() == "*") {
            return;
        }
        let explicit = lifetime(resp, &dirs);
        let validators = resp.header("etag").is_some() || resp.header("last-modified").is_some();
        let life = match explicit {
            Some(t) => t,
            None if validators_ok && validators && HEURISTIC.contains(&resp.status) => Duration::ZERO,
            None => return,
        };
        // Only the script path keeps responses that need revalidation, and only 200s otherwise.
        if !validators_ok && (resp.status != 200 || life.is_zero() || has("no-cache")) {
            return;
        }
        if life.is_zero() && !(validators_ok && validators) {
            return;
        }
        let size = resp.body.len() + 512;
        if size > self.max_bytes / 8 {
            return;
        }
        let vary = vary_hdr
            .split(',')
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty())
            .map(|n| {
                let val = super::header_value(req_headers, &n).map(|s| s.to_string());
                (n, val)
            })
            .collect();
        let initial_age = Duration::from_secs(resp.header("age").and_then(|a| a.trim().parse::<u64>().ok()).unwrap_or(0));
        self.tick += 1;
        let k = Self::key(&resp.url);
        if let Some(old) = self.map.remove(&k) {
            self.bytes -= old.size;
        }
        self.bytes += size;
        let mut stored = resp.clone();
        stored.redirected = false;
        self.map.insert(k, Entry { resp: stored, stored: Instant::now(), initial_age, lifetime: life, no_cache: has("no-cache"), vary, size, last_use: self.tick });
        while self.bytes > self.max_bytes {
            let victim = self.map.iter().min_by_key(|(_, e)| e.last_use).map(|(k, _)| k.clone());
            match victim {
                Some(v) => {
                    if let Some(e) = self.map.remove(&v) {
                        self.bytes -= e.size;
                    }
                }
                None => break,
            }
        }
    }

    /// Freshen the stored response for `url` with a 304's headers; returns it.
    pub fn freshen(&mut self, url: &url::Url, not_modified: &Response) -> Option<Response> {
        let k = Self::key(url);
        let e = self.map.get_mut(&k)?;
        for (name, v) in &not_modified.headers {
            let n = name.to_ascii_lowercase();
            if matches!(n.as_str(), "content-length" | "content-encoding" | "transfer-encoding" | "connection" | "keep-alive") {
                continue;
            }
            e.resp.headers.retain(|(k2, _)| !k2.eq_ignore_ascii_case(&n));
            e.resp.headers.push((name.clone(), v.clone()));
        }
        let dirs = directives(&e.resp);
        e.no_cache = dirs.iter().any(|(d, _)| d == "no-cache");
        e.lifetime = lifetime(&e.resp, &dirs).unwrap_or(Duration::ZERO);
        e.stored = Instant::now();
        e.initial_age = Duration::ZERO;
        let mut r = e.resp.clone();
        r.from_cache = true;
        Some(r)
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.bytes = 0;
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::parse_http_date;

    #[test]
    fn http_dates() {
        assert_eq!(parse_http_date("Sun, 06 Nov 1994 08:49:37 GMT"), Some(784111777));
        assert_eq!(parse_http_date("Sunday, 06-Nov-94 08:49:37 GMT"), Some(784111777));
        assert_eq!(parse_http_date("Sun Nov  6 08:49:37 1994"), Some(784111777));
        assert_eq!(parse_http_date("garbage"), None);
    }
}
