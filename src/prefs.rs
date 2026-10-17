// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Browser preferences (ohb://settings): search engine, address-bar
//! placeholder, theme, and per-site permissions.
//!
//! Saved as `settings.txt` (one `key=value` per line) in the profile folder.
//! Headless runs (`--screenshot`, tests) use the defaults and never write.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

pub const DEFAULT_PLACEHOLDER: &str = "Search with OpenHeronBrowser Demo";

struct Prefs {
    map: Mutex<BTreeMap<String, String>>,
}

static PERSIST: AtomicBool = AtomicBool::new(false);
/// Bumped on every change (the shell re-reads cached values when it moves).
pub static GENERATION: AtomicU64 = AtomicU64::new(1);

fn prefs() -> &'static Prefs {
    static P: OnceLock<Prefs> = OnceLock::new();
    P.get_or_init(|| Prefs { map: Mutex::new(BTreeMap::new()) })
}

fn path() -> std::path::PathBuf {
    crate::ext::profile_dir().join("settings.txt")
}

/// Load the saved settings and save later changes (the windowed browser).
pub fn enable_persistence() {
    PERSIST.store(true, Ordering::SeqCst);
    if let Ok(s) = std::fs::read_to_string(path()) {
        let mut m = prefs().map.lock().unwrap();
        for line in s.lines() {
            if let Some((k, v)) = line.split_once('=') {
                m.insert(k.trim().to_string(), v.to_string());
            }
        }
    }
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// A settings change made while disk writes were held (see `persist`).
static HELD: AtomicBool = AtomicBool::new(false);

/// Write a settings change held until the session was allowed to save.
pub fn flush_held() {
    if HELD.swap(false, Ordering::SeqCst) {
        let m = prefs().map.lock().unwrap().clone();
        save(&m);
    }
}

fn save(m: &BTreeMap<String, String>) {
    if !PERSIST.load(Ordering::SeqCst) {
        return;
    }
    if !crate::persist::allowed() {
        if crate::persist::pending() {
            HELD.store(true, Ordering::SeqCst);
        }
        return;
    }
    let mut out = String::new();
    for (k, v) in m {
        out.push_str(k);
        out.push('=');
        out.push_str(&v.replace(['\n', '\r'], " "));
        out.push('\n');
    }
    let p = path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = p.with_extension("tmp");
    if std::fs::write(&tmp, out).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

pub fn get(k: &str) -> Option<String> {
    prefs().map.lock().unwrap().get(k).cloned()
}

pub fn set(k: &str, v: Option<&str>) {
    let mut m = prefs().map.lock().unwrap();
    match v {
        Some(v) => {
            m.insert(k.to_string(), v.to_string());
        }
        None => {
            m.remove(k);
        }
    }
    save(&m);
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

// ------------------------------------------------------------ search engine

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Engine {
    Google,
    DuckDuckGo,
    Bing,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Engine::Google => "Google",
            Engine::DuckDuckGo => "DuckDuckGo",
            Engine::Bing => "Bing",
        }
    }
    pub fn id(self) -> &'static str {
        match self {
            Engine::Google => "google",
            Engine::DuckDuckGo => "duckduckgo",
            Engine::Bing => "bing",
        }
    }
    /// The search form's action (the query goes in `q`).
    pub fn action(self) -> &'static str {
        match self {
            Engine::Google => "https://www.google.com/search",
            Engine::DuckDuckGo => "https://html.duckduckgo.com/html/",
            Engine::Bing => "https://www.bing.com/search",
        }
    }
}

pub fn engine() -> Engine {
    match get("search").as_deref() {
        Some("google") => Engine::Google,
        Some("bing") => Engine::Bing,
        _ => Engine::DuckDuckGo,
    }
}

/// The results URL for a search typed in the address bar.
pub fn search_url(q: &str) -> String {
    let enc: String = url::form_urlencoded::byte_serialize(q.as_bytes()).collect();
    format!("{}?q={enc}", engine().action())
}

/// The address bar's placeholder text.
pub fn placeholder() -> String {
    match get("placeholder") {
        Some(p) if !p.trim().is_empty() => p,
        Some(_) => String::new(),
        None => DEFAULT_PLACEHOLDER.to_string(),
    }
}

/// The black (default) or light browser theme.
pub fn dark_theme() -> bool {
    get("theme").as_deref() != Some("light")
}

// ------------------------------------------------------------ permissions

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    Allow,
    Block,
    Ask,
}

/// Permissions a site can be asked about.
pub const PERMISSIONS: &[(&str, &str)] = &[("clipboard-read", "Read your clipboard"), ("downloads", "Download files"), ("notifications", "Show notifications"), ("geolocation", "Know your location")];

pub fn permission_label(p: &str) -> &'static str {
    PERMISSIONS.iter().find(|(k, _)| *k == p).map(|(_, l)| *l).unwrap_or("Use a browser feature")
}

/// The saved decision for `perm` on `site` (scheme://host[:port]).
pub fn permission(site: &str, perm: &str) -> Decision {
    match get(&format!("perm.{perm}.{site}")).as_deref() {
        Some("allow") => Decision::Allow,
        Some("block") => Decision::Block,
        _ => Decision::Ask,
    }
}

pub fn set_permission(site: &str, perm: &str, d: Decision) {
    let k = format!("perm.{perm}.{site}");
    match d {
        Decision::Allow => set(&k, Some("allow")),
        Decision::Block => set(&k, Some("block")),
        Decision::Ask => set(&k, None),
    }
}

/// Every saved site permission: (permission, site, decision).
pub fn saved_permissions() -> Vec<(String, String, Decision)> {
    let m = prefs().map.lock().unwrap();
    let mut out = Vec::new();
    for (k, v) in m.iter() {
        if let Some(rest) = k.strip_prefix("perm.") {
            if let Some((p, site)) = rest.split_once('.') {
                let d = if v == "allow" { Decision::Allow } else { Decision::Block };
                out.push((p.to_string(), site.to_string(), d));
            }
        }
    }
    out
}

// ------------------------------------------------------------ settings page

/// Run a settings action from an `ohb://settings?…` URL (browser pages only;
/// `Page::navigate` refuses web pages). Returns true if one ran.
pub fn perform_action(target: &str) -> bool {
    let Some(q) = target.strip_prefix("ohb://settings?") else { return false };
    let mut did = false;
    for pair in q.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = v.replace('+', " ");
        let v = String::from_utf8_lossy(&crate::net::percent_decode(v.as_bytes())).into_owned();
        match k {
            "search" if ["google", "duckduckgo", "bing"].contains(&v.as_str()) => {
                set("search", Some(&v));
                did = true;
            }
            "placeholder" => {
                let v: String = v.chars().filter(|c| !c.is_control()).take(80).collect();
                set("placeholder", Some(&v));
                did = true;
            }
            "placeholder_reset" => {
                set("placeholder", None);
                did = true;
            }
            "theme" if v == "light" || v == "black" => {
                set("theme", Some(&v));
                did = true;
            }
            "forget" if !v.is_empty() => {
                // "perm|site"
                if let Some((p, site)) = v.split_once('|') {
                    set_permission(site, p, Decision::Ask);
                    did = true;
                }
            }
            _ => {}
        }
    }
    did
}

/// The ohb://settings page body.
pub fn page_html() -> String {
    use crate::net::html_escape as esc;
    let eng = engine();
    let mut engines = String::new();
    for e in [Engine::Google, Engine::DuckDuckGo, Engine::Bing] {
        let cls = if e == eng { "btn on" } else { "btn" };
        engines.push_str(&format!("<a class=\"{cls}\" href=\"ohb://settings?search={}\">{}</a> ", e.id(), e.name()));
    }
    let dark = dark_theme();
    let theme = format!(
        "<a class=\"{}\" href=\"ohb://settings?theme=black\">Black</a> <a class=\"{}\" href=\"ohb://settings?theme=light\">Light</a>",
        if dark { "btn on" } else { "btn" },
        if dark { "btn" } else { "btn on" }
    );
    let mut perms = String::new();
    for (p, site, d) in saved_permissions() {
        let enc: String = url::form_urlencoded::byte_serialize(format!("{p}|{site}").as_bytes()).collect();
        perms.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td><a href=\"ohb://settings?forget={enc}\">Reset</a></td></tr>",
            esc(&site),
            esc(permission_label(&p)),
            if d == Decision::Allow { "Allowed" } else { "Blocked" }
        ));
    }
    if perms.is_empty() {
        perms.push_str("<tr><td colspan=4 class=muted>No site has asked for a permission yet.</td></tr>");
    }
    format!(
        r#"<h1>Settings</h1><div class=sub>Changes apply right away.</div>
<div class=card><b>Search engine</b><div class=muted>Used for searches typed in the address bar and on the new tab page.</div><p>{engines}</p></div>
<div class=card><b>Theme</b><div class=muted>The colour of the tabs and toolbar.</div><p>{theme}</p></div>
<div class=card><b>Site permissions</b><div class=muted>Sites ask before reading your clipboard or downloading files.</div><table class=kv><tr><th align=left>Site</th><th align=left>Permission</th><th align=left>Decision</th><th></th></tr>{perms}</table></div>
<div class=card><b>Address bar text</b><div class=muted>Shown in the empty address bar.</div>
<form action="ohb://settings" method=get class=row><input name=placeholder value="{}" maxlength=80><button type=submit>Save</button></form>
<p><a href="ohb://settings?placeholder_reset=1">Reset to “{}”</a></p></div>
<div class=card><b>Site data</b><p><a class=btn href="ohb://storage">Cookies &amp; site data…</a> <a class=btn href="ohb://extensions">Extensions…</a></p></div>"#,
        esc(&placeholder()),
        esc(DEFAULT_PLACEHOLDER)
    )
}
