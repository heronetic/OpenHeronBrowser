// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! The Fetch Standard's "main fetch" for script-initiated requests (fetch(),
//! XMLHttpRequest): request modes (same-origin, cors, no-cors), CORS
//! preflights and checks, credentials modes, redirect modes (follow, error,
//! manual) with the tainted-origin rules, and response tainting.

use super::{client, BodySink, NetError, Request, Response, SinkEvent};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use url::Url;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    SameOrigin,
    Cors,
    NoCors,
    /// Prelude-internal media/resource loads: no CORS check, basic response.
    Media,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Creds {
    Omit,
    SameOrigin,
    Include,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Redirect {
    Follow,
    Error,
    Manual,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cache {
    Default,
    NoStore,
    Reload,
    NoCache,
    ForceCache,
    OnlyIfCached,
}

/// What a streaming caller sees: the final response's head (after redirects
/// and CORS checks), then its body chunks.
pub enum StreamOut<'a> {
    Head { status: u16, status_text: &'a str, url: &'a Url, headers: &'a [(String, String)], rtype: &'static str, redirected: bool },
    Chunk(&'a [u8]),
}

/// A streaming caller (returns false to abort).
#[derive(Clone)]
pub struct StreamTo(pub Arc<dyn Fn(StreamOut) -> bool + Send + Sync>);

impl std::fmt::Debug for StreamTo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StreamTo")
    }
}

#[derive(Clone, Debug)]
pub struct Opts {
    /// The client's URL (its origin is the request's origin).
    pub client: Url,
    pub mode: Mode,
    pub creds: Creds,
    pub redirect: Redirect,
    pub cache: Cache,
    /// XHR's "use-CORS-preflight flag" (upload listeners).
    pub force_preflight: bool,
    /// The request's referrer: "client", "" (no referrer) or a URL.
    pub referrer: String,
    /// The referrer policy ("" = strict-origin-when-cross-origin).
    pub referrer_policy: String,
    /// Stream the final response to this caller as it arrives.
    pub stream: Option<StreamTo>,
}

impl Opts {
    pub fn parse(client: Url, mode: &str, creds: &str, redirect: &str, cache: &str) -> Opts {
        Opts {
            client,
            mode: match mode {
                "same-origin" => Mode::SameOrigin,
                "no-cors" => Mode::NoCors,
                "media" => Mode::Media,
                _ => Mode::Cors,
            },
            creds: match creds {
                "omit" => Creds::Omit,
                "include" => Creds::Include,
                _ => Creds::SameOrigin,
            },
            redirect: match redirect {
                "error" => Redirect::Error,
                "manual" => Redirect::Manual,
                _ => Redirect::Follow,
            },
            cache: match cache {
                "no-store" => Cache::NoStore,
                "reload" => Cache::Reload,
                "no-cache" => Cache::NoCache,
                "force-cache" => Cache::ForceCache,
                "only-if-cached" => Cache::OnlyIfCached,
                _ => Cache::Default,
            },
            force_preflight: false,
            referrer: "client".into(),
            referrer_policy: String::new(),
            stream: None,
        }
    }
}

const POLICIES: [&str; 8] = ["no-referrer", "no-referrer-when-downgrade", "same-origin", "origin", "strict-origin", "origin-when-cross-origin", "strict-origin-when-cross-origin", "unsafe-url"];

/// A Referrer-Policy header's policy (the last recognized token).
fn header_policy(r: &Response) -> Option<String> {
    let v = combined(r, "referrer-policy")?;
    v.split(',').map(|t| t.trim().to_ascii_lowercase()).filter(|t| POLICIES.contains(&t.as_str())).last()
}

/// "Potentially trustworthy" URLs (https, wss, file, loopback hosts).
fn trustworthy(u: &Url) -> bool {
    match u.scheme() {
        "https" | "wss" | "file" => true,
        "http" | "ws" => match u.host() {
            Some(url::Host::Domain(d)) => d == "localhost" || d.ends_with(".localhost"),
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        },
        _ => false,
    }
}

/// The Referrer Policy spec's "determine request's referrer" for `current`.
fn referer_for(source: Option<&Url>, policy: &str, current: &Url) -> Option<String> {
    let source = source?.clone();
    if matches!(source.scheme(), "about" | "blob" | "data" | "javascript") {
        return None;
    }
    let mut full = source.clone();
    let _ = full.set_username("");
    let _ = full.set_password(None);
    full.set_fragment(None);
    let origin_only = format!("{}/", source.origin().ascii_serialization());
    let mut url_s = full.to_string();
    if url_s.len() > 4096 {
        url_s = origin_only.clone();
    }
    let same = source.origin() == current.origin();
    let downgrade = trustworthy(&source) && !trustworthy(current);
    match policy {
        "no-referrer" => None,
        "origin" => Some(origin_only),
        "unsafe-url" => Some(url_s),
        "strict-origin" => (!downgrade).then_some(origin_only),
        "same-origin" => same.then_some(url_s),
        "origin-when-cross-origin" => Some(if same { url_s } else { origin_only }),
        "no-referrer-when-downgrade" => (!downgrade).then_some(url_s),
        _ => {
            if same {
                Some(url_s)
            } else if downgrade {
                None
            } else {
                Some(origin_only)
            }
        }
    }
}

/// A fetched response and how script may see it.
#[derive(Debug)]
pub struct Outcome {
    pub resp: Response,
    /// "basic", "cors", "opaque" or "opaqueredirect".
    pub rtype: &'static str,
    /// More than one URL in the URL list.
    pub redirected: bool,
}

fn blocked(msg: impl Into<String>) -> NetError {
    NetError::Blocked(msg.into())
}

fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

const SAFE_METHODS: [&str; 3] = ["GET", "HEAD", "POST"];

fn no_cors_unsafe_byte(b: u8) -> bool {
    (b < 0x20 && b != 0x09) || matches!(b, b'"' | b'(' | b')' | b':' | b'<' | b'>' | b'?' | b'@' | b'[' | b'\\' | b']' | b'{' | b'}' | 0x7f)
}

/// CORS-safelisted request-header (name, value).
fn safelisted(name: &str, value: &str) -> bool {
    if value.len() > 128 {
        return false;
    }
    let v = value.as_bytes();
    match name {
        "accept" => !v.iter().any(|&b| no_cors_unsafe_byte(b)),
        "accept-language" | "content-language" => v.iter().all(|&b| b.is_ascii_alphanumeric() || matches!(b, b' ' | b'*' | b',' | b'-' | b'.' | b';' | b'=')),
        "content-type" => {
            if v.iter().any(|&b| no_cors_unsafe_byte(b)) {
                return false;
            }
            let ess = value.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            matches!(ess.as_str(), "application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain")
        }
        "range" => {
            let Some(r) = value.strip_prefix("bytes=") else { return false };
            let Some((a, b)) = r.split_once('-') else { return false };
            !a.is_empty() && a.bytes().all(|c| c.is_ascii_digit()) && b.bytes().all(|c| c.is_ascii_digit()) && (b.is_empty() || a.parse::<u64>().ok() <= b.parse::<u64>().ok())
        }
        _ => false,
    }
}

/// The CORS-unsafe request-header names of a header list (sorted, lowercase).
fn unsafe_header_names(headers: &[(String, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut safe_len = 0usize;
    let mut safe_names: Vec<String> = Vec::new();
    for (k, v) in headers {
        let n = k.to_ascii_lowercase();
        if n == "origin" || n == "referer" {
            continue;
        }
        if safelisted(&n, v) {
            safe_len += v.len();
            safe_names.push(n);
        } else if !out.contains(&n) {
            out.push(n);
        }
    }
    if safe_len > 1024 {
        for n in safe_names {
            if !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out.sort();
    out
}

fn header<'a>(r: &'a Response, name: &str) -> Option<&'a str> {
    r.header(name)
}

/// All values of a header combined (", ").
fn combined(r: &Response, name: &str) -> Option<String> {
    combined_h(&r.headers, name)
}

fn combined_h(headers: &[(String, String)], name: &str) -> Option<String> {
    let v: Vec<&str> = headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect();
    if v.is_empty() {
        None
    } else {
        Some(v.join(", "))
    }
}

/// CORS check: Access-Control-Allow-Origin (and -Credentials).
fn cors_check(r: &Response, origin: &str, creds: Creds) -> bool {
    cors_check_h(&r.headers, origin, creds)
}

fn cors_check_h(headers: &[(String, String)], origin: &str, creds: Creds) -> bool {
    let r = headers;
    let Some(acao) = combined_h(r, "access-control-allow-origin") else { return false };
    let acao = acao.trim();
    if acao == "*" && creds != Creds::Include {
        return true;
    }
    if acao != origin {
        return false;
    }
    if creds != Creds::Include {
        return true;
    }
    combined_h(r, "access-control-allow-credentials").map(|v| v.trim() == "true").unwrap_or(false)
}

/// A comma-separated list header's tokens (None if one is not a valid token).
fn token_list(r: &Response, name: &str) -> Option<Vec<String>> {
    let Some(v) = combined(r, name) else { return Some(Vec::new()) };
    let mut out = Vec::new();
    for t in v.split(',') {
        let t = t.trim_matches(|c| c == ' ' || c == '\t');
        if t.is_empty() {
            continue;
        }
        if !t.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)) {
            return None;
        }
        out.push(t.to_string());
    }
    Some(out)
}

fn origin_string(client: &Url, tainted: bool) -> String {
    if tainted {
        return "null".into();
    }
    let o = client.origin().ascii_serialization();
    if o == "null" {
        "null".into()
    } else {
        o
    }
}

/// CORS preflight for `req` (OPTIONS with the Access-Control-Request-* headers).
fn preflight(req: &Request, o: &Opts, origin: &str, unsafe_names: &[String]) -> Result<(), NetError> {
    let mut pf = Request::get(req.url.clone(), req.kind, req.initiator.clone());
    pf.method = "OPTIONS".into();
    pf.credentials = false;
    pf.follow_redirects = false;
    pf.cache_lookup = false;
    pf.cache_store = false;
    pf.send_referer = req.send_referer;
    pf.headers.push(("Accept".into(), "*/*".into()));
    pf.headers.push(("Access-Control-Request-Method".into(), req.method.clone()));
    if !unsafe_names.is_empty() {
        pf.headers.push(("Access-Control-Request-Headers".into(), unsafe_names.join(",")));
    }
    pf.headers.push(("Origin".into(), origin.to_string()));
    for (k, v) in &req.headers {
        if k.eq_ignore_ascii_case("referer") {
            pf.headers.push((k.clone(), v.clone()));
        }
    }
    // Only the head matters: the body is not read (it may never end).
    let head: Arc<std::sync::Mutex<Option<(u16, String, Vec<(String, String)>)>>> = Arc::new(std::sync::Mutex::new(None));
    let h2 = head.clone();
    pf.sink = Some(BodySink(Arc::new(move |ev| {
        if let SinkEvent::Head { status, status_text, headers } = ev {
            *h2.lock().unwrap() = Some((status, status_text.to_string(), headers.to_vec()));
        }
        false
    })));
    let url = pf.url.clone();
    let r = match client().fetch(pf) {
        Ok(r) => r,
        Err(e) => match head.lock().unwrap().take() {
            Some((status, status_text, headers)) => Response { url, status, status_text, headers, body: Arc::new(Vec::new()), redirected: false, from_cache: false },
            None => return Err(e),
        },
    };
    if !cors_check(&r, origin, o.creds) {
        return Err(blocked(format!("CORS preflight for {} did not allow {origin}", req.url)));
    }
    if !(200..300).contains(&r.status) {
        return Err(blocked(format!("CORS preflight for {} failed with status {}", req.url, r.status)));
    }
    let Some(mut methods) = token_list(&r, "access-control-allow-methods") else { return Err(blocked("CORS preflight: bad Access-Control-Allow-Methods")) };
    let Some(names) = token_list(&r, "access-control-allow-headers") else { return Err(blocked("CORS preflight: bad Access-Control-Allow-Headers")) };
    if methods.is_empty() && o.force_preflight {
        methods.push(req.method.clone());
    }
    let wildcard = o.creds != Creds::Include;
    if !methods.iter().any(|m| m == &req.method) && !SAFE_METHODS.contains(&req.method.as_str()) && !(wildcard && methods.iter().any(|m| m == "*")) {
        return Err(blocked(format!("CORS preflight: method {} is not allowed", req.method)));
    }
    let names: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    for n in unsafe_names {
        let listed = names.iter().any(|x| x == n);
        if listed {
            continue;
        }
        if n == "authorization" || !(wildcard && names.iter().any(|x| x == "*")) {
            return Err(blocked(format!("CORS preflight: header {n} is not allowed")));
        }
    }
    Ok(())
}

/// Main fetch with the request's mode, credentials and redirect mode.
pub fn fetch(mut req: Request, o: &Opts) -> Result<Outcome, NetError> {
    let client_origin = o.client.origin();
    // Response tainting: "basic" until a cross-origin hop makes it cors/opaque.
    let mut tainting: &'static str = "basic";
    let mut tainted_origin = false;
    let mut hops = 0usize;
    let had_origin_header = req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("origin"));
    let mut policy = if o.referrer_policy.is_empty() { "strict-origin-when-cross-origin".to_string() } else { o.referrer_policy.clone() };
    // The request's referrer: computed at each hop, and that result is the next hop's source.
    let mut referrer: Option<Url> = match o.referrer.as_str() {
        "" => None,
        "client" => Some(o.client.clone()),
        u => Url::parse(u).ok(),
    };
    // The HTTP cache is consulted here (with the request's cache mode), not by the client.
    req.cache_lookup = false;
    req.cache_store = false;
    loop {
        let url = req.url.clone();
        let scheme = url.scheme().to_string();
        let same = url.origin() == client_origin && tainting == "basic";
        let local = matches!(scheme.as_str(), "data" | "about" | "blob");
        if o.mode == Mode::Media || same || scheme == "data" {
            // basic
        } else if o.mode == Mode::SameOrigin {
            return Err(blocked(format!("{url} is not same-origin (mode \"same-origin\")")));
        } else if o.mode == Mode::NoCors {
            if o.redirect != Redirect::Follow {
                return Err(blocked("no-cors requests must follow redirects"));
            }
            tainting = "opaque";
        } else {
            if !matches!(scheme.as_str(), "http" | "https") {
                return Err(blocked(format!("CORS: unsupported scheme {scheme}")));
            }
            tainting = "cors";
        }
        let origin = origin_string(&o.client, tainted_origin);
        // The Referer header for this hop (the policy may change on redirects).
        req.send_referer = false;
        req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("referer"));
        let computed = referer_for(referrer.as_ref(), &policy, &url);
        referrer = computed.as_deref().and_then(|r| Url::parse(r).ok());
        if let Some(r) = computed {
            req.headers.push(("Referer".into(), r));
        }
        // Credentials: cookies only with "include", or "same-origin" on a basic hop.
        req.credentials = match o.creds {
            Creds::Include => true,
            Creds::SameOrigin => tainting == "basic",
            Creds::Omit => false,
        };
        // The Origin header: CORS requests, and non-GET/HEAD requests.
        req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("origin") || had_origin_header);
        if !had_origin_header && !local && (tainting == "cors" || (req.method != "GET" && req.method != "HEAD")) {
            req.headers.push(("Origin".into(), origin.clone()));
        }
        if tainting == "cors" {
            let unsafe_names = unsafe_header_names(&req.headers.iter().filter(|(k, _)| !k.eq_ignore_ascii_case("origin")).cloned().collect::<Vec<_>>());
            if o.force_preflight || !SAFE_METHODS.contains(&req.method.as_str()) || !unsafe_names.is_empty() {
                preflight(&req, o, &origin, &unsafe_names)?;
            }
        }
        req.follow_redirects = false;
        // Streaming: only the hop whose response script will see (not a redirect
        // being followed, an authentication retry, or a failed CORS check).
        req.sink = None;
        if let Some(user) = &o.stream {
            let user = user.clone();
            let exposed = Arc::new(AtomicBool::new(false));
            let ex = exposed.clone();
            let follow = o.redirect == Redirect::Follow;
            let check = tainting == "cors";
            let (origin_c, creds, hop_url, redirected) = (origin.clone(), o.creds, url.clone(), hops > 0);
            let may_auth = tainting != "cors" && req.credentials && (!url.username().is_empty() || url.password().is_some()) && !req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization"));
            let rtype = tainting;
            req.sink = Some(BodySink(Arc::new(move |ev| match ev {
                SinkEvent::Head { status, status_text, headers } => {
                    let has = |n: &str| headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
                    if is_redirect(status) && (has("location") || !follow) {
                        return true;
                    }
                    if (status == 401 && may_auth && has("www-authenticate")) || (check && !cors_check_h(headers, &origin_c, creds)) || headers.iter().any(|(_, v)| v.contains('\0')) {
                        return true;
                    }
                    ex.store(true, Ordering::SeqCst);
                    (user.0)(StreamOut::Head { status, status_text, url: &hop_url, headers, rtype, redirected })
                }
                // Opaque bodies are never read: only the abort switch is consulted.
                SinkEvent::Chunk(b) => !ex.load(Ordering::SeqCst) || (user.0)(StreamOut::Chunk(if rtype == "opaque" { &[] } else { b })),
            })));
        }
        // HTTP-network-or-cache fetch: the cache mode decides between the
        // stored response, a revalidation, and the network.
        let has_h = |h: &[(String, String)], n: &str| h.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
        let mut mode = o.cache;
        if mode == Cache::Default && ["if-modified-since", "if-none-match", "if-unmodified-since", "if-match", "if-range"].iter().any(|n| has_h(&req.headers, n)) {
            mode = Cache::NoStore;
        }
        let mut net_req = req.clone();
        if mode == Cache::NoCache && !has_h(&net_req.headers, "cache-control") {
            net_req.headers.push(("Cache-Control".into(), "max-age=0".into()));
        }
        if matches!(mode, Cache::NoStore | Cache::Reload) {
            if !has_h(&net_req.headers, "pragma") {
                net_req.headers.push(("Pragma".into(), "no-cache".into()));
            }
            if !has_h(&net_req.headers, "cache-control") {
                net_req.headers.push(("Cache-Control".into(), "no-cache".into()));
            }
        }
        let cacheable_method = req.method == "GET";
        let mut stored = None;
        let mut revalidating = false;
        if cacheable_method && !matches!(mode, Cache::NoStore | Cache::Reload) {
            let hit = client().cache.lock().unwrap().lookup_for(&url, &req.headers);
            match hit {
                Some(c) if matches!(mode, Cache::ForceCache | Cache::OnlyIfCached) || (mode == Cache::Default && c.fresh) => stored = Some(c.resp),
                Some(c) => {
                    if let Some(et) = c.resp.header("etag").filter(|_| !has_h(&net_req.headers, "if-none-match")) {
                        net_req.headers.push(("If-None-Match".into(), et.to_string()));
                    }
                    if let Some(lm) = c.resp.header("last-modified").filter(|_| !has_h(&net_req.headers, "if-modified-since")) {
                        net_req.headers.push(("If-Modified-Since".into(), lm.to_string()));
                    }
                    revalidating = true;
                    // A 304 is not what script sees: no streaming for a revalidation.
                    net_req.sink = None;
                }
                None if mode == Cache::OnlyIfCached => return Err(blocked(format!("{url} is not cached (cache mode \"only-if-cached\")"))),
                None => {}
            }
        }
        let from_cache = stored.is_some();
        let mut resp = match stored {
            Some(r) => r,
            None => client().fetch(net_req.clone())?,
        };
        if revalidating && resp.status == 304 {
            if let Some(r) = client().cache.lock().unwrap().freshen(&url, &resp) {
                resp = r;
            }
        }
        if cacheable_method && !from_cache && mode != Cache::NoStore && resp.status != 304 && !resp.from_cache {
            client().cache.lock().unwrap().store_for(&resp, &req.headers, true);
        }
        // HTTP authentication with the URL's credentials (XHR's open() user/password).
        if resp.status == 401
            && tainting != "cors"
            && req.credentials
            && (!url.username().is_empty() || url.password().is_some())
            && header(&resp, "www-authenticate").is_some()
            && !req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        {
            let user = String::from_utf8_lossy(&super::percent_decode(url.username().as_bytes())).into_owned();
            let pass = String::from_utf8_lossy(&super::percent_decode(url.password().unwrap_or("").as_bytes())).into_owned();
            let mut again = net_req.clone();
            again.headers.push(("Authorization".into(), format!("Basic {}", super::base64_encode(format!("{user}:{pass}").as_bytes()))));
            resp = client().fetch(again)?;
        }
        if resp.headers.iter().any(|(_, v)| v.contains('\0')) {
            return Err(NetError::Protocol("NUL in a response header value".into()));
        }
        if tainting == "cors" && !cors_check(&resp, &origin, o.creds) {
            return Err(blocked(format!("CORS: {} did not allow {origin}", resp.url)));
        }
        if is_redirect(resp.status) {
            // The redirect mode applies to any redirect status (with or without a Location).
            match o.redirect {
                Redirect::Error if o.mode != Mode::Media => return Err(blocked(format!("redirect from {url} (redirect mode \"error\")"))),
                Redirect::Manual if o.mode != Mode::Media => return Ok(Outcome { resp, rtype: "opaqueredirect", redirected: hops > 0 }),
                _ => {}
            }
            if let Some(loc) = header(&resp, "location").map(|s| s.to_string()) {
                if let Some(p) = header_policy(&resp) {
                    policy = p;
                }
                let next = url.join(&loc).map_err(|e| NetError::Protocol(format!("bad redirect location: {e}")))?;
                if !matches!(next.scheme(), "http" | "https") {
                    return Err(blocked(format!("redirect to {next}")));
                }
                hops += 1;
                if hops > 20 {
                    return Err(NetError::Protocol("too many redirects".into()));
                }
                let has_creds = !next.username().is_empty() || next.password().is_some();
                if has_creds && (tainting == "cors" || (o.mode == Mode::Cors && next.origin() != client_origin)) {
                    return Err(blocked("redirect to a URL with credentials"));
                }
                if (resp.status == 303 && req.method != "GET" && req.method != "HEAD") || ((resp.status == 301 || resp.status == 302) && req.method == "POST") {
                    req.method = "GET".into();
                    req.body = None;
                    req.headers.retain(|(k, _)| {
                        let k = k.to_ascii_lowercase();
                        !matches!(k.as_str(), "content-type" | "content-encoding" | "content-language" | "content-location")
                    });
                }
                if next.origin() != url.origin() {
                    req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
                    if url.origin() != client_origin {
                        tainted_origin = true;
                    }
                }
                req.url = next;
                continue;
            }
        }
        return Ok(Outcome { resp, rtype: if o.mode == Mode::Media { "basic" } else { tainting }, redirected: hops > 0 });
    }
}
