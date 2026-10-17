// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronExtensions — a small WebExtensions-style extension system.
//!
//! An extension is a folder with a `manifest.json` (the WebExtensions
//! format, a subset of it):
//!
//! ```json
//! { "name": "My extension", "version": "1.0", "description": "…",
//!   "content_scripts": [{ "matches": ["*://*.example.com/*"], "exclude_matches": [],
//!                          "js": ["content.js"], "css": ["style.css"],
//!                          "run_at": "document_idle" }],
//!   "declarative_net_request": { "rule_resources": [{ "id": "r", "enabled": true, "path": "rules.json" }] },
//!   "filter_lists": ["filters.txt"] }
//! ```
//!
//! * Content scripts run in the page (main world) at `document_start`,
//!   `document_end` or `document_idle`, with a `chrome` / `browser` object
//!   (`runtime.id`, `runtime.getManifest()`, `runtime.getURL()`,
//!   `storage.local`).
//! * Content CSS is a user style sheet (its `!important` beats the page's).
//! * Network rules: declarativeNetRequest JSON (block, allow, redirect,
//!   upgradeScheme) and, as an OHB extension, filter lists in Adblock Plus /
//!   uBlock Origin syntax (network filters and `##` element hiding).
//!
//! Extensions load from the profile's `extensions` folder and from
//! `--load-extension DIR`. HeronBlock, the built-in content blocker, is an
//! extension too (its filters are compiled in).

pub mod filter;
pub mod resources;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use url::Url;

use crate::net::{self, NetError, Request, RequestKind, Response};
use filter::{FilterSet, ReqInfo, ResType, Verdict};

pub const HERONBLOCK: &str = "heronblock";
const HERONBLOCK_LIST: &str = include_str!("heronblock.txt");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RunAt {
    DocumentStart,
    DocumentEnd,
    DocumentIdle,
}

struct ContentScript {
    matches: Vec<MatchPattern>,
    exclude: Vec<MatchPattern>,
    js: Vec<(String, Arc<str>)>,
    css: Vec<Arc<str>>,
    run_at: RunAt,
}

pub struct Extension {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub builtin: bool,
    pub dir: Option<PathBuf>,
    manifest_json: String,
    scripts: Vec<ContentScript>,
    filters: Option<FilterSet>,
    web_accessible: Vec<String>,
    pub errors: Vec<String>,
}

impl Extension {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(f) = &self.filters {
            if f.stats.network > 0 {
                parts.push(format!("{} network rules", f.stats.network));
            }
            if f.stats.cosmetic > 0 {
                parts.push(format!("{} element-hiding rules", f.stats.cosmetic));
            }
        }
        let n = self.scripts.len();
        if n > 0 {
            parts.push(format!("{n} content script{}", if n == 1 { "" } else { "s" }));
        }
        parts.join(" · ")
    }
}

/// A WebExtensions match pattern (`<all_urls>`, `*://*.host/path*`).
#[derive(Clone, Debug)]
struct MatchPattern {
    all: bool,
    schemes: Vec<String>,
    host: String,
    sub: bool,
    path: String,
}

impl MatchPattern {
    fn parse(p: &str) -> Option<MatchPattern> {
        if p == "<all_urls>" {
            return Some(MatchPattern { all: true, schemes: Vec::new(), host: String::new(), sub: false, path: String::new() });
        }
        let (scheme, rest) = p.split_once("://")?;
        let schemes = match scheme {
            "*" => vec!["http".to_string(), "https".to_string()],
            s => vec![s.to_ascii_lowercase()],
        };
        let (host, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/*"),
        };
        let (host, sub) = if host == "*" {
            (String::new(), true)
        } else if let Some(h) = host.strip_prefix("*.") {
            (h.to_ascii_lowercase(), true)
        } else {
            (host.to_ascii_lowercase(), false)
        };
        Some(MatchPattern { all: false, schemes, host, sub, path: path.to_string() })
    }

    fn matches(&self, u: &Url) -> bool {
        if self.all {
            return matches!(u.scheme(), "http" | "https" | "file");
        }
        if !self.schemes.iter().any(|s| s == u.scheme()) {
            return false;
        }
        let h = u.host_str().unwrap_or("").to_ascii_lowercase();
        let host_ok = if self.host.is_empty() { self.sub } else if self.sub { h == self.host || h.ends_with(&format!(".{}", self.host)) } else { h == self.host };
        if !host_ok {
            return false;
        }
        let mut path = u.path().to_string();
        if let Some(q) = u.query() {
            path.push('?');
            path.push_str(q);
        }
        glob(&self.path, &path)
    }
}

/// `*` wildcard matching.
fn glob(pat: &str, s: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut rest = s;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(p) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else {
            match rest.find(p) {
                Some(j) => rest = &rest[j + p.len()..],
                None => return false,
            }
        }
    }
    true
}

#[derive(Default)]
struct Settings {
    disabled: HashSet<String>,
    /// Sites (registrable domains) where an extension is paused.
    paused: HashMap<String, HashSet<String>>,
}

struct Registry {
    exts: Vec<Arc<Extension>>,
    settings: Settings,
    extra_dirs: Vec<PathBuf>,
}

fn registry() -> &'static RwLock<Registry> {
    static R: OnceLock<RwLock<Registry>> = OnceLock::new();
    R.get_or_init(|| RwLock::new(Registry { exts: Vec::new(), settings: Settings::default(), extra_dirs: Vec::new() }))
}

/// Extensions are off entirely (`OHB_NO_EXT=1` / `--no-extensions`).
fn globally_off() -> bool {
    static OFF: OnceLock<bool> = OnceLock::new();
    *OFF.get_or_init(|| std::env::var_os("OHB_NO_EXT").is_some())
}

/// The profile folder for settings and extensions.
pub fn profile_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("OHB_PROFILE_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "windows") {
        if let Some(a) = std::env::var_os("APPDATA") {
            return PathBuf::from(a).join("OpenHeronBrowser");
        }
        return home.join("AppData").join("Roaming").join("OpenHeronBrowser");
    }
    if cfg!(target_os = "macos") {
        return home.join("Library").join("Application Support").join("OpenHeronBrowser");
    }
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(x) if !x.is_empty() => PathBuf::from(x).join("openheronbrowser"),
        _ => home.join(".config").join("openheronbrowser"),
    }
}

pub fn extensions_dir() -> PathBuf {
    profile_dir().join("extensions")
}

fn settings_path() -> PathBuf {
    profile_dir().join("extensions.json")
}

fn load_settings() -> Settings {
    let mut s = Settings::default();
    let Ok(text) = std::fs::read_to_string(settings_path()) else { return s };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return s };
    if let Some(a) = v["disabled"].as_array() {
        s.disabled = a.iter().filter_map(|x| x.as_str().map(String::from)).collect();
    }
    if let Some(o) = v["paused"].as_object() {
        for (k, list) in o {
            let set: HashSet<String> = list.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
            s.paused.insert(k.clone(), set);
        }
    }
    s
}

static SETTINGS_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Write extension settings changed while disk writes were held.
pub fn flush_held() {
    if SETTINGS_HELD.swap(false, std::sync::atomic::Ordering::SeqCst) {
        let r = registry().read().unwrap();
        save_settings(&r.settings);
    }
}

fn save_settings(s: &Settings) {
    if std::env::var_os("OHB_PROFILE_READONLY").is_some() {
        return;
    }
    if !crate::persist::allowed() {
        if crate::persist::pending() {
            SETTINGS_HELD.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        return;
    }
    let mut disabled: Vec<&String> = s.disabled.iter().collect();
    disabled.sort();
    let mut paused = serde_json::Map::new();
    for (k, v) in &s.paused {
        let mut l: Vec<&String> = v.iter().collect();
        l.sort();
        paused.insert(k.clone(), serde_json::json!(l));
    }
    let v = serde_json::json!({ "disabled": disabled, "paused": paused });
    let _ = std::fs::create_dir_all(profile_dir());
    let _ = std::fs::write(settings_path(), serde_json::to_string_pretty(&v).unwrap_or_default());
}

/// Load extensions (built-in, the profile's folder, and `extra` folders)
/// and hook network filtering. Call once at startup.
pub fn init(extra: Vec<PathBuf>) {
    {
        let mut r = registry().write().unwrap();
        r.extra_dirs = extra;
        r.settings = load_settings();
    }
    reload();
    net::set_request_hook(Box::new(hook));
}

/// Rescan the extension folders.
pub fn reload() {
    let mut exts = vec![Arc::new(heronblock())];
    let extra = registry().read().unwrap().extra_dirs.clone();
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(extensions_dir()) {
        let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path()).filter(|p| p.join("manifest.json").is_file()).collect();
        v.sort();
        dirs.extend(v);
    }
    dirs.extend(extra);
    let mut seen: HashSet<String> = exts.iter().map(|e| e.id.clone()).collect();
    for d in dirs {
        match load_dir(&d) {
            Ok(mut e) => {
                while seen.contains(&e.id) {
                    e.id.push('_');
                }
                seen.insert(e.id.clone());
                exts.push(Arc::new(e));
            }
            Err(err) => eprintln!("[extensions] {}: {err}", d.display()),
        }
    }
    registry().write().unwrap().exts = exts;
}

fn heronblock() -> Extension {
    let mut f = FilterSet::new();
    f.add_list(HERONBLOCK_LIST);
    let manifest = serde_json::json!({
        "name": "HeronBlock", "version": crate::net::VERSION, "manifest_version": 3,
        "description": "Blocks ads, trackers and fingerprinting scripts, which makes pages lighter and faster."
    });
    Extension {
        id: HERONBLOCK.into(),
        name: "HeronBlock".into(),
        version: crate::net::VERSION.into(),
        description: "Blocks ads, trackers and fingerprinting scripts, which makes pages lighter and faster. Scripts that pages call into are swapped for harmless stand-ins so sites keep working.".into(),
        builtin: true,
        dir: None,
        manifest_json: manifest.to_string(),
        scripts: Vec::new(),
        filters: Some(f),
        web_accessible: Vec::new(),
        errors: Vec::new(),
    }
}

/// Read an extension folder.
fn load_dir(dir: &Path) -> Result<Extension, String> {
    let text = std::fs::read_to_string(dir.join("manifest.json")).map_err(|e| format!("manifest.json: {e}"))?;
    // Manifests may carry a BOM.
    let m: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("manifest.json: {e}"))?;
    let name = m["name"].as_str().ok_or("manifest.json has no name")?.to_string();
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_else(|| name.to_ascii_lowercase())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>();
    let mut errors = Vec::new();
    // Files must stay inside the extension folder.
    let read = |rel: &str, errors: &mut Vec<String>| -> Option<String> {
        let rel = rel.trim_start_matches('/');
        if rel.split(['/', '\\']).any(|c| c == "..") {
            errors.push(format!("{rel}: path leaves the extension folder"));
            return None;
        }
        match std::fs::read(dir.join(rel)) {
            Ok(b) => Some(String::from_utf8_lossy(&b).trim_start_matches('\u{feff}').to_string()),
            Err(e) => {
                errors.push(format!("{rel}: {e}"));
                None
            }
        }
    };
    let strs = |v: &serde_json::Value| -> Vec<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
    let mut scripts = Vec::new();
    for cs in m["content_scripts"].as_array().cloned().unwrap_or_default() {
        let matches: Vec<MatchPattern> = strs(&cs["matches"]).iter().filter_map(|p| MatchPattern::parse(p)).collect();
        let exclude: Vec<MatchPattern> = strs(&cs["exclude_matches"]).iter().filter_map(|p| MatchPattern::parse(p)).collect();
        let js: Vec<(String, Arc<str>)> = strs(&cs["js"]).iter().filter_map(|f| read(f, &mut errors).map(|t| (f.clone(), Arc::from(t)))).collect();
        let css: Vec<Arc<str>> = strs(&cs["css"]).iter().filter_map(|f| read(f, &mut errors).map(Arc::from)).collect();
        let run_at = match cs["run_at"].as_str() {
            Some("document_start") => RunAt::DocumentStart,
            Some("document_end") => RunAt::DocumentEnd,
            _ => RunAt::DocumentIdle,
        };
        if !matches.is_empty() {
            scripts.push(ContentScript { matches, exclude, js, css, run_at });
        }
    }
    let mut filters = FilterSet::new();
    for res in m["declarative_net_request"]["rule_resources"].as_array().cloned().unwrap_or_default() {
        if res["enabled"].as_bool() == Some(false) {
            continue;
        }
        if let Some(t) = res["path"].as_str().and_then(|p| read(p, &mut errors)) {
            match serde_json::from_str::<serde_json::Value>(&t) {
                Ok(v) => filters.add_dnr(&v),
                Err(e) => errors.push(format!("{}: {e}", res["path"].as_str().unwrap_or(""))),
            }
        }
    }
    for p in strs(&m["filter_lists"]) {
        if let Some(t) = read(&p, &mut errors) {
            filters.add_list(&t);
        }
    }
    let mut web_accessible = Vec::new();
    for w in m["web_accessible_resources"].as_array().cloned().unwrap_or_default() {
        match w.as_str() {
            Some(s) => web_accessible.push(s.to_string()),
            None => web_accessible.extend(strs(&w["resources"])),
        }
    }
    Ok(Extension {
        id,
        name,
        version: m["version"].as_str().unwrap_or("").to_string(),
        description: m["description"].as_str().unwrap_or("").to_string(),
        builtin: false,
        dir: Some(dir.to_path_buf()),
        manifest_json: m.to_string(),
        scripts,
        filters: if filters.is_empty() { None } else { Some(filters) },
        web_accessible,
        errors,
    })
}

// ------------------------------------------------------------------ state

pub struct ExtState {
    pub ext: Arc<Extension>,
    pub enabled: bool,
    pub paused_sites: Vec<String>,
}

pub fn list() -> Vec<ExtState> {
    let r = registry().read().unwrap();
    r.exts
        .iter()
        .map(|e| {
            let mut paused: Vec<String> = r.settings.paused.get(&e.id).map(|s| s.iter().cloned().collect()).unwrap_or_default();
            paused.sort();
            ExtState { ext: e.clone(), enabled: !r.settings.disabled.contains(&e.id), paused_sites: paused }
        })
        .collect()
}

pub fn is_enabled(id: &str) -> bool {
    !globally_off() && !registry().read().unwrap().settings.disabled.contains(id)
}

pub fn set_enabled(id: &str, on: bool) {
    let mut r = registry().write().unwrap();
    if on {
        r.settings.disabled.remove(id);
    } else {
        r.settings.disabled.insert(id.to_string());
    }
    save_settings(&r.settings);
}

/// Is `id` paused on the site of `url`?
pub fn is_paused(id: &str, url: &Url) -> bool {
    let site = net::site_of(url);
    registry().read().unwrap().settings.paused.get(id).map(|s| s.contains(&site)).unwrap_or(false)
}

pub fn set_paused(id: &str, site: &str, paused: bool) {
    let mut r = registry().write().unwrap();
    let set = r.settings.paused.entry(id.to_string()).or_default();
    if paused {
        set.insert(site.to_string());
    } else {
        set.remove(site);
    }
    save_settings(&r.settings);
}

/// Extensions active for a page at `url`.
fn active_for(url: Option<&Url>) -> Vec<Arc<Extension>> {
    if globally_off() {
        return Vec::new();
    }
    let r = registry().read().unwrap();
    let site = url.map(net::site_of);
    r.exts
        .iter()
        .filter(|e| !r.settings.disabled.contains(&e.id))
        .filter(|e| match &site {
            Some(s) => !r.settings.paused.get(&e.id).map(|p| p.contains(s)).unwrap_or(false),
            None => true,
        })
        .cloned()
        .collect()
}

// ------------------------------------------------------------- statistics

#[derive(Default, Clone)]
pub struct TabStats {
    pub blocked: usize,
    /// Recently blocked: (url, resource type, rule).
    pub log: Vec<(String, &'static str, String)>,
}

fn stats() -> &'static Mutex<HashMap<u64, TabStats>> {
    static S: OnceLock<Mutex<HashMap<u64, TabStats>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

static TOTAL_BLOCKED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn tab_stats(tab: u64) -> TabStats {
    stats().lock().unwrap().get(&tab).cloned().unwrap_or_default()
}

pub fn blocked_count(tab: u64) -> usize {
    stats().lock().unwrap().get(&tab).map(|s| s.blocked).unwrap_or(0)
}

pub fn total_blocked() -> usize {
    TOTAL_BLOCKED.load(std::sync::atomic::Ordering::Relaxed)
}

/// A tab started a new page.
pub fn reset_tab(tab: u64) {
    stats().lock().unwrap().remove(&tab);
}

fn note_blocked(tab: Option<u64>, url: &str, ty: ResType, rule: &str) {
    TOTAL_BLOCKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if std::env::var_os("OHB_DEBUG_NET").is_some() {
        eprintln!("[heronblock] blocked {} {url} ({rule})", ty.name());
    }
    let Some(tab) = tab else { return };
    let mut s = stats().lock().unwrap();
    let e = s.entry(tab).or_default();
    e.blocked += 1;
    if e.log.len() < 300 {
        e.log.push((url.to_string(), ty.name(), rule.to_string()));
    }
}

// ------------------------------------------------------------ net filtering

fn res_type(req: &Request) -> ResType {
    match req.kind {
        RequestKind::Document if req.frame => ResType::Subdocument,
        RequestKind::Document => ResType::Document,
        RequestKind::Stylesheet => ResType::Stylesheet,
        RequestKind::Script => ResType::Script,
        RequestKind::Image | RequestKind::Favicon => ResType::Image,
        RequestKind::Font => ResType::Font,
        RequestKind::Fetch => ResType::Xhr,
    }
}

/// The network hook: decide each http(s) request.
fn hook(req: &Request, ctx: Option<&net::ReqContext>) -> Option<Result<Response, NetError>> {
    if globally_off() || !matches!(req.url.scheme(), "http" | "https") {
        return None;
    }
    // Top-level pages are the user's choice.
    if req.kind == RequestKind::Document && !req.frame && req.initiator.is_none() {
        return None;
    }
    let top = ctx.and_then(|c| c.top.as_ref()).or(req.initiator.as_ref());
    let exts = active_for(top);
    if exts.iter().all(|e| e.filters.is_none()) {
        return None;
    }
    let host = req.url.host_str().unwrap_or("");
    let doc = req.initiator.as_ref().or(top);
    let doc_host = doc.and_then(|u| u.host_str()).unwrap_or("");
    let third = doc.map(|d| net::site_of(d) != net::site_of(&req.url)).unwrap_or(false);
    let ty = res_type(req);
    let info = ReqInfo::new(req.url.as_str(), host, doc_host, third, ty);
    for e in &exts {
        let Some(f) = &e.filters else { continue };
        let (v, rule) = f.check(&info);
        match v {
            Verdict::None | Verdict::Allow => continue,
            Verdict::Block { redirect } => {
                note_blocked(ctx.map(|c| c.tab), req.url.as_str(), ty, rule.unwrap_or(""));
                if let Some(name) = redirect {
                    if let Some(resp) = redirect_resource(e, &name, req) {
                        return Some(Ok(resp));
                    }
                }
                return Some(Err(NetError::Blocked(format!("{} ({})", e.name, rule.unwrap_or("")))));
            }
            Verdict::RedirectUrl(u) => {
                return Some(Ok(redirect_to(req, &u)));
            }
            Verdict::Upgrade => {
                let mut u = req.url.clone();
                let _ = u.set_scheme("https");
                return Some(Ok(redirect_to(req, u.as_str())));
            }
        }
    }
    None
}

/// Popup blocking: whether a new tab a page opens (window.open, a
/// target=_blank link) goes to a blocked ad or tracking host.
pub fn blocks_popup(url: &Url, opener: &Url, tab: Option<u64>) -> bool {
    if globally_off() || !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let exts = active_for(Some(opener));
    let host = url.host_str().unwrap_or("");
    let third = net::site_of(opener) != net::site_of(url);
    if !third {
        return false;
    }
    let info = ReqInfo::new(url.as_str(), host, opener.host_str().unwrap_or(""), third, ResType::Subdocument);
    for e in &exts {
        let Some(f) = &e.filters else { continue };
        if let (Verdict::Block { .. }, rule) = f.check(&info) {
            note_blocked(tab, url.as_str(), ResType::Subdocument, rule.unwrap_or("popup"));
            return true;
        }
    }
    false
}

/// Whether a subframe at `url` in a document at `doc` would be blocked, so
/// the page can skip starting a frame thread for an ad or tracking iframe.
pub fn blocks_subframe(url: &Url, doc: &Url) -> bool {
    if globally_off() || !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let exts = active_for(Some(doc));
    let host = url.host_str().unwrap_or("");
    let third = net::site_of(doc) != net::site_of(url);
    let info = ReqInfo::new(url.as_str(), host, doc.host_str().unwrap_or(""), third, ResType::Subdocument);
    for e in &exts {
        let Some(f) = &e.filters else { continue };
        match f.check(&info) {
            // Counted when the static frame's fetch is blocked.
            (Verdict::Block { redirect: None }, _) => return true,
            (Verdict::None | Verdict::Allow, _) => continue,
            _ => return false,
        }
    }
    false
}

fn redirect_to(req: &Request, to: &str) -> Response {
    Response {
        url: req.url.clone(),
        status: 307,
        status_text: "Internal Redirect".into(),
        headers: vec![("Location".into(), to.to_string()), ("Non-Authoritative-Reason".into(), "Extension".into())],
        body: Arc::new(Vec::new()),
        redirected: false,
        from_cache: false,
    }
}

fn redirect_resource(e: &Extension, name: &str, req: &Request) -> Option<Response> {
    let (ct, body) = if let Some(path) = name.strip_prefix("ext:") {
        let dir = e.dir.as_ref()?;
        let rel = path.trim_start_matches('/');
        if rel.split(['/', '\\']).any(|c| c == "..") {
            return None;
        }
        let body = std::fs::read(dir.join(rel)).ok()?;
        (mime_for(rel), body)
    } else {
        resources::get(name)?
    };
    Some(Response {
        url: req.url.clone(),
        status: 200,
        status_text: "OK".into(),
        headers: vec![("Content-Type".into(), ct.to_string()), ("Access-Control-Allow-Origin".into(), "*".into())],
        body: Arc::new(body),
        redirected: false,
        from_cache: false,
    })
}

fn mime_for(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "js" | "mjs" => "application/javascript",
        "css" => "text/css",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "png" => "image/png",
        "gif" => "image/gif",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        _ => "text/plain",
    }
}

/// `ohb-extension://ID/path`: a file an extension lists in
/// `web_accessible_resources`.
pub fn extension_resource(u: &Url) -> Option<Response> {
    let id = u.host_str()?;
    let r = registry().read().unwrap();
    let e = r.exts.iter().find(|e| e.id == id)?;
    let rel = u.path().trim_start_matches('/');
    if !e.web_accessible.iter().any(|p| glob(p.trim_start_matches('/'), rel)) {
        return None;
    }
    let mut resp = redirect_resource(e, &format!("ext:{rel}"), &Request::get(u.clone(), RequestKind::Fetch, None))?;
    resp.url = u.clone();
    Some(resp)
}

// --------------------------------------------------------- page injection

/// The user style sheet for a page: element hiding plus content-script CSS.
pub fn page_css(url: &Url) -> String {
    let mut css = String::new();
    if !matches!(url.scheme(), "http" | "https" | "file") {
        return css;
    }
    let host = url.host_str().unwrap_or("");
    for e in active_for(Some(url)) {
        if let Some(f) = &e.filters {
            let sels = f.hidden_selectors(host);
            // One rule per selector: an invalid one only drops itself.
            for s in sels {
                css.push_str(s);
                css.push_str("{display:none!important}\n");
            }
        }
        for cs in &e.scripts {
            if cs.matches.iter().any(|m| m.matches(url)) && !cs.exclude.iter().any(|m| m.matches(url)) {
                for c in &cs.css {
                    css.push_str(c);
                    css.push('\n');
                }
            }
        }
    }
    css
}

/// Content scripts to run on `url` at `when`: (script name, wrapped code).
pub fn content_scripts(url: &Url, when: RunAt) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if !matches!(url.scheme(), "http" | "https" | "file") {
        return out;
    }
    for e in active_for(Some(url)) {
        for cs in &e.scripts {
            if cs.run_at != when || !cs.matches.iter().any(|m| m.matches(url)) || cs.exclude.iter().any(|m| m.matches(url)) {
                continue;
            }
            for (name, code) in &cs.js {
                out.push((format!("ohb-extension://{}/{}", e.id, name.trim_start_matches('/')), wrap(&e, code)));
            }
        }
    }
    out
}

/// A content script runs in its own function scope with the extension API.
fn wrap(e: &Extension, code: &str) -> String {
    let id = serde_json::to_string(&e.id).unwrap_or_default();
    let name = serde_json::to_string(&e.name).unwrap_or_default();
    format!(
        r#"(function(){{
const __id={id}, __manifest={m};
const __key=Symbol.for('ohb.extension.storage.'+__id);
const __store=globalThis[__key]||(Object.defineProperty(globalThis,__key,{{value:new Map(),enumerable:false}}),globalThis[__key]);
const __cb=(p,cb)=>{{if(typeof cb==='function'){{p.then(v=>cb(v),()=>cb());return undefined;}}return p;}};
const __pick=(keys)=>{{const o={{}};if(keys==null){{for(const [k,v] of __store)o[k]=structuredClone(v);}}else if(typeof keys==='string'){{if(__store.has(keys))o[keys]=structuredClone(__store.get(keys));}}else if(Array.isArray(keys)){{for(const k of keys)if(__store.has(k))o[k]=structuredClone(__store.get(k));}}else{{for(const k of Object.keys(keys))o[k]=__store.has(k)?structuredClone(__store.get(k)):keys[k];}}return o;}};
const __area={{get:(k,cb)=>__cb(Promise.resolve(__pick(k)),cb),set:(items,cb)=>{{for(const k of Object.keys(items||{{}}))__store.set(k,structuredClone(items[k]));return __cb(Promise.resolve(),cb);}},remove:(k,cb)=>{{for(const x of [].concat(k))__store.delete(x);return __cb(Promise.resolve(),cb);}},clear:(cb)=>{{__store.clear();return __cb(Promise.resolve(),cb);}},getBytesInUse:(k,cb)=>__cb(Promise.resolve(0),cb)}};
const __ev=()=>{{const l=new Set();return {{addListener:f=>l.add(f),removeListener:f=>l.delete(f),hasListener:f=>l.has(f),_fire:(...a)=>{{for(const f of l)try{{f(...a);}}catch(e){{console.error(e);}}}}}};}};
const chrome={{runtime:{{id:__id,getManifest:()=>JSON.parse(JSON.stringify(__manifest)),getURL:(p)=>'ohb-extension://'+__id+'/'+String(p).replace(/^\//,''),sendMessage:(...a)=>{{const cb=a.find(x=>typeof x==='function');if(cb)setTimeout(()=>cb(undefined),0);return Promise.resolve();}},onMessage:__ev(),onConnect:__ev(),lastError:undefined}},storage:{{local:__area,sync:__area,session:__area,onChanged:__ev()}},i18n:{{getMessage:(k)=>'',getUILanguage:()=>navigator.language}}}};
const browser=chrome;
try {{
{code}
}} catch (e) {{ console.error('[' + {name} + '] content script error:', e); }}
}})();"#,
        m = e.manifest_json,
    )
}

// --------------------------------------------------------------- UI page

/// Run an `ohb://extensions/…` action (`toggle?id=`, `resume?id=&site=`,
/// `reload`). Returns false when `target` isn't one.
pub fn perform_action(target: &str) -> bool {
    if !is_action(target) {
        return false;
    }
    let Ok(u) = Url::parse(target) else { return false };
    let q: HashMap<String, String> = u.query_pairs().into_owned().collect();
    let id = q.get("id").cloned().unwrap_or_default();
    match u.path().trim_matches('/') {
        "toggle" => set_enabled(&id, !is_enabled(&id)),
        "resume" => {
            if let Some(site) = q.get("site") {
                set_paused(&id, site, false);
            }
        }
        "reload" => reload(),
        _ => {}
    }
    true
}

/// The `ohb://extensions` page.
pub fn page() -> String {
    let esc = net::html_escape;
    let mut cards = String::new();
    for s in list() {
        let e = &s.ext;
        let on = s.enabled && !globally_off();
        let mut extra = String::new();
        if e.id == HERONBLOCK {
            extra.push_str(&format!("<div class=muted>Blocked this session: <b>{}</b> requests</div>", total_blocked()));
        }
        if !s.paused_sites.is_empty() {
            extra.push_str("<div class=muted>Turned off on: ");
            for site in &s.paused_sites {
                extra.push_str(&format!("<span class=chip>{} <a href=\"ohb://extensions/resume?id={}&amp;site={}\">turn on</a></span> ", esc(site), esc(&e.id), esc(site)));
            }
            extra.push_str("</div>");
        }
        for err in &e.errors {
            extra.push_str(&format!("<div class=errline>⚠ {}</div>", esc(err)));
        }
        let path = e.dir.as_ref().map(|d| format!("<div class=muted><code>{}</code></div>", esc(&d.display().to_string()))).unwrap_or_else(|| "<div class=muted>Built in</div>".into());
        cards.push_str(&format!(
            "<div class=\"card ext\"><div class=row><div class=grow><b>{}</b> <span class=muted>{}</span><div>{}</div><div class=muted>{}</div>{}{}</div><a class=\"switch {}\" href=\"ohb://extensions/toggle?id={}\" title=\"{}\"><span></span></a></div></div>",
            esc(&e.name),
            esc(&e.version),
            esc(&e.description),
            esc(&e.summary()),
            path,
            extra,
            if on { "on" } else { "off" },
            esc(&e.id),
            if on { "Turn off" } else { "Turn on" }
        ));
    }
    let off_note = if globally_off() { "<div class=card><b>Extensions are turned off</b> for this run (<code>OHB_NO_EXT</code> / <code>--no-extensions</code>).</div>" } else { "" };
    format!(
        r#"<!doctype html><html><head><meta charset=utf-8><title>Extensions</title><style>
:root {{ color-scheme: light; }}
body {{ margin: 0; font-family: system-ui, sans-serif; background: #f6f8fb; color: #1f2430; }}
.wrap {{ max-width: 760px; margin: 0 auto; padding: 48px 24px; }}
h1 {{ font-size: 28px; font-weight: 600; margin: 0 0 6px 0; }}
.sub {{ color: #5d6575; margin-bottom: 22px; }}
.card {{ background: white; border: 1px solid #e3e8f0; border-radius: 12px; padding: 16px 20px; margin: 12px 0; }}
.row {{ display: flex; align-items: flex-start; gap: 16px; }}
.grow {{ flex: 1; min-width: 0; }}
.grow > div {{ margin-top: 4px; }}
.muted {{ color: #6b7385; font-size: 13px; }}
.errline {{ color: #b3261e; font-size: 13px; }}
.chip {{ display: inline-block; background: #eef1f6; border-radius: 10px; padding: 1px 8px; margin: 2px 2px 0 0; }}
code {{ background: #eef1f6; padding: 1px 5px; border-radius: 4px; font-size: 12px; }}
a {{ color: #1a4fd0; }}
.switch {{ display: block; flex: none; width: 40px; height: 22px; border-radius: 11px; background: #c4cad6; position: relative; margin-top: 2px; }}
.switch span {{ position: absolute; top: 3px; left: 3px; width: 16px; height: 16px; border-radius: 8px; background: white; }}
.switch.on {{ background: #2f6fed; }}
.switch.on span {{ left: 21px; }}
.btn {{ display: inline-block; padding: 7px 14px; border-radius: 16px; background: #2f6fed; color: white; text-decoration: none; font-size: 13px; font-weight: 600; }}
</style></head><body><div class=wrap>
<h1>Extensions</h1><div class=sub>Add-ons that change how pages load and look.</div>
{off_note}{cards}
<div class=card><b>Install an extension</b><div class=muted style="margin-top:6px">Put an unpacked extension (a folder with a <code>manifest.json</code>) in <code>{dir}</code>, then <a href="ohb://extensions/reload">reload extensions</a>. You can also start OpenHeronBrowser with <code>--load-extension FOLDER</code>. Supported: content scripts and CSS, declarativeNetRequest rules, and filter lists in Adblock Plus / uBlock Origin syntax (<code>"filter_lists": ["list.txt"]</code>).</div></div>
</div></body></html>"#,
        dir = esc(&extensions_dir().display().to_string()),
    )
}

/// Is `target` an `ohb://extensions` action that changes settings?
pub fn is_action(target: &str) -> bool {
    let t = target.trim_end_matches('/');
    t.starts_with("ohb://extensions/") && t.len() > "ohb://extensions/".len()
}
