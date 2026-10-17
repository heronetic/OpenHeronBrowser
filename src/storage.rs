// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Profile storage: localStorage and IndexedDB shared by every tab, and the
//! cookie jar, saved to the profile folder so sign-ins and site data survive
//! a restart.
//!
//! Persistence is on for the windowed browser (`enable_persistence`), off for
//! headless runs (`--screenshot`, tests) unless `OHB_PERSIST=1`, so automated
//! runs never read or change a user's data. `OHB_EPHEMERAL=1` keeps a windowed
//! session in memory only. Writes are batched: a saver thread writes the
//! changed files (atomically, through a temporary file) every couple of
//! seconds, and `flush` writes them at exit.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// origin -> key -> value
type Local = HashMap<String, BTreeMap<String, String>>;
/// origin -> database name -> serialized database (idb.js snapshot)
type Idb = HashMap<String, BTreeMap<String, String>>;

pub struct Profile {
    persistent: AtomicBool,
    local: Mutex<Local>,
    idb: Mutex<Idb>,
    local_dirty: AtomicBool,
    idb_dirty: AtomicBool,
    cookies_saved: AtomicU64,
}

pub fn profile() -> &'static Profile {
    static P: OnceLock<Profile> = OnceLock::new();
    P.get_or_init(|| Profile {
        persistent: AtomicBool::new(false),
        local: Mutex::new(HashMap::new()),
        idb: Mutex::new(HashMap::new()),
        local_dirty: AtomicBool::new(false),
        idb_dirty: AtomicBool::new(false),
        cookies_saved: AtomicU64::new(u64::MAX),
    })
}

fn dir() -> PathBuf {
    crate::ext::profile_dir().join("storage")
}

/// Per-origin quota for localStorage (like Chrome's 5 MB, counted in chars).
const LOCAL_QUOTA: usize = 5 * 1024 * 1024;

fn read_json(name: &str) -> Option<serde_json::Value> {
    let bytes = std::fs::read(dir().join(name)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_atomic(name: &str, data: &[u8]) {
    let d = dir();
    if std::fs::create_dir_all(&d).is_err() {
        return;
    }
    let tmp = d.join(format!("{name}.tmp"));
    if std::fs::write(&tmp, data).is_ok() {
        let _ = std::fs::rename(&tmp, d.join(name));
    }
}

/// The "keep site data between sessions" setting (on unless turned off).
pub fn keep_data_setting() -> bool {
    !dir().join("ephemeral").exists()
}

/// Turn keeping site data between sessions on or off. Off deletes the saved
/// files now (the current session keeps its data in memory).
pub fn set_keep_data(on: bool) {
    if !crate::persist::allowed() {
        // This session isn't saved: the setting changes nothing on disk.
        profile().persistent.store(on && std::env::var_os("OHB_EPHEMERAL").is_none(), Ordering::SeqCst);
        return;
    }
    let d = dir();
    if on {
        let _ = std::fs::remove_file(d.join("ephemeral"));
        let p = profile();
        if !p.persistent.load(Ordering::SeqCst) && std::env::var_os("OHB_EPHEMERAL").is_none() {
            p.persistent.store(true, Ordering::SeqCst);
            p.cookies_saved.store(u64::MAX, Ordering::SeqCst);
            p.local_dirty.store(true, Ordering::SeqCst);
            p.idb_dirty.store(true, Ordering::SeqCst);
            start_saver();
            flush();
        }
    } else {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("ephemeral"), b"");
        profile().persistent.store(false, Ordering::SeqCst);
        for f in ["cookies.json", "local_storage.json", "indexeddb.json"] {
            let _ = std::fs::remove_file(d.join(f));
        }
    }
}

fn start_saver() {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::Builder::new()
        .name("heron-profile".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_secs(2));
            flush();
        })
        .ok();
}

/// Load the saved profile and start saving changes (the windowed browser).
pub fn enable_persistence() {
    if std::env::var_os("OHB_EPHEMERAL").is_some() || !keep_data_setting() {
        return;
    }
    let p = profile();
    if p.persistent.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(v) = read_json("cookies.json") {
        let mut jar = crate::net::client().cookies.lock().unwrap();
        jar.load_json(&v);
        p.cookies_saved.store(jar.changes(), Ordering::SeqCst);
    }
    if let Some(serde_json::Value::Object(m)) = read_json("local_storage.json") {
        let mut local = p.local.lock().unwrap();
        for (origin, kv) in m {
            if let serde_json::Value::Object(kv) = kv {
                let e = local.entry(origin).or_default();
                for (k, v) in kv {
                    if let serde_json::Value::String(s) = v {
                        e.insert(k, s);
                    }
                }
            }
        }
    }
    if let Some(serde_json::Value::Object(m)) = read_json("indexeddb.json") {
        let mut idb = p.idb.lock().unwrap();
        for (origin, dbs) in m {
            if let serde_json::Value::Object(dbs) = dbs {
                let e = idb.entry(origin).or_default();
                for (name, v) in dbs {
                    if let serde_json::Value::String(s) = v {
                        e.insert(name, s);
                    }
                }
            }
        }
    }
    start_saver();
}

/// Like `enable_persistence` for headless runs that ask for it (`OHB_PERSIST=1`).
pub fn maybe_enable_for_headless() {
    if std::env::var_os("OHB_PERSIST").is_some() {
        enable_persistence();
    }
}

pub fn is_persistent() -> bool {
    profile().persistent.load(Ordering::SeqCst)
}

/// Write whatever changed since the last save.
pub fn flush() {
    let p = profile();
    if !p.persistent.load(Ordering::SeqCst) || !crate::persist::allowed() {
        return;
    }
    let (changes, json) = {
        let jar = crate::net::client().cookies.lock().unwrap();
        (jar.changes(), if jar.changes() != p.cookies_saved.load(Ordering::SeqCst) { Some(jar.to_json()) } else { None })
    };
    if let Some(j) = json {
        write_atomic("cookies.json", j.to_string().as_bytes());
        p.cookies_saved.store(changes, Ordering::SeqCst);
    }
    if p.local_dirty.swap(false, Ordering::SeqCst) {
        let data = {
            let local = p.local.lock().unwrap();
            serde_json::to_vec(&*local).unwrap_or_default()
        };
        write_atomic("local_storage.json", &data);
    }
    if p.idb_dirty.swap(false, Ordering::SeqCst) {
        let data = {
            let idb = p.idb.lock().unwrap();
            serde_json::to_vec(&*idb).unwrap_or_default()
        };
        write_atomic("indexeddb.json", &data);
    }
}

// ------------------------------------------------------------ localStorage

pub fn local_get(origin: &str, key: &str) -> Option<String> {
    profile().local.lock().unwrap().get(origin).and_then(|m| m.get(key).cloned())
}

/// Returns false when the origin's quota would be exceeded.
pub fn local_set(origin: &str, key: &str, value: &str) -> bool {
    let p = profile();
    let mut local = p.local.lock().unwrap();
    let m = local.entry(origin.to_string()).or_default();
    let used: usize = m.iter().filter(|(k, _)| k.as_str() != key).map(|(k, v)| k.len() + v.len()).sum();
    if used + key.len() + value.len() > LOCAL_QUOTA {
        return false;
    }
    if m.get(key).map(|v| v == value).unwrap_or(false) {
        return true;
    }
    m.insert(key.to_string(), value.to_string());
    p.local_dirty.store(true, Ordering::SeqCst);
    true
}

pub fn local_remove(origin: &str, key: &str) {
    let p = profile();
    if let Some(m) = p.local.lock().unwrap().get_mut(origin) {
        if m.remove(key).is_some() {
            p.local_dirty.store(true, Ordering::SeqCst);
        }
    }
}

pub fn local_clear(origin: &str) {
    let p = profile();
    if p.local.lock().unwrap().remove(origin).is_some() {
        p.local_dirty.store(true, Ordering::SeqCst);
    }
}

pub fn local_keys(origin: &str) -> Vec<String> {
    profile().local.lock().unwrap().get(origin).map(|m| m.keys().cloned().collect()).unwrap_or_default()
}

// ------------------------------------------------------------ IndexedDB

pub fn idb_get(origin: &str, name: &str) -> Option<String> {
    profile().idb.lock().unwrap().get(origin).and_then(|m| m.get(name).cloned())
}

pub fn idb_put(origin: &str, name: &str, data: &str) {
    let p = profile();
    p.idb.lock().unwrap().entry(origin.to_string()).or_default().insert(name.to_string(), data.to_string());
    p.idb_dirty.store(true, Ordering::SeqCst);
}

pub fn idb_delete(origin: &str, name: &str) {
    let p = profile();
    if let Some(m) = p.idb.lock().unwrap().get_mut(origin) {
        if m.remove(name).is_some() {
            p.idb_dirty.store(true, Ordering::SeqCst);
        }
    }
}

pub fn idb_names(origin: &str) -> Vec<String> {
    profile().idb.lock().unwrap().get(origin).map(|m| m.keys().cloned().collect()).unwrap_or_default()
}

// ------------------------------------------------------------ management

/// Site (registrable domain) of an origin string like "https://www.example.com".
fn site_of_origin(origin: &str) -> String {
    url::Url::parse(origin).map(|u| crate::net::site_of(&u)).unwrap_or_else(|_| origin.to_string())
}

/// Stored data per site: (site, cookies, localStorage bytes, IndexedDB bytes).
pub fn usage() -> Vec<(String, usize, usize, usize)> {
    let mut m: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    for (domain, n) in crate::net::client().cookies.lock().unwrap().domains() {
        let site = url::Url::parse(&format!("https://{domain}/")).map(|u| crate::net::site_of(&u)).unwrap_or(domain);
        m.entry(site).or_default().0 += n;
    }
    let p = profile();
    for (origin, kv) in p.local.lock().unwrap().iter() {
        let bytes: usize = kv.iter().map(|(k, v)| k.len() + v.len()).sum();
        if bytes > 0 {
            m.entry(site_of_origin(origin)).or_default().1 += bytes;
        }
    }
    for (origin, dbs) in p.idb.lock().unwrap().iter() {
        let bytes: usize = dbs.values().map(|v| v.len()).sum();
        if bytes > 0 {
            m.entry(site_of_origin(origin)).or_default().2 += bytes;
        }
    }
    m.into_iter().map(|(s, (c, l, i))| (s, c, l, i)).collect()
}

/// Delete cookies, localStorage and IndexedDB of one site.
pub fn clear_site(site: &str) {
    crate::net::client().cookies.lock().unwrap().clear_site(site);
    let p = profile();
    p.local.lock().unwrap().retain(|o, _| site_of_origin(o) != site);
    p.idb.lock().unwrap().retain(|o, _| site_of_origin(o) != site);
    p.local_dirty.store(true, Ordering::SeqCst);
    p.idb_dirty.store(true, Ordering::SeqCst);
    flush();
}

/// Delete all cookies and site data (and the HTTP cache).
pub fn clear_all() {
    crate::net::client().cookies.lock().unwrap().clear();
    crate::net::client().cache.lock().unwrap().clear();
    let p = profile();
    p.local.lock().unwrap().clear();
    p.idb.lock().unwrap().clear();
    p.local_dirty.store(true, Ordering::SeqCst);
    p.idb_dirty.store(true, Ordering::SeqCst);
    flush();
}

/// Run a storage-settings action from an `ohb://storage?…` URL (browser pages
/// only; `Page::navigate` refuses web pages). Returns true if one ran.
pub fn perform_action(target: &str) -> bool {
    let Some(q) = target.strip_prefix("ohb://storage?") else { return false };
    let mut did = false;
    for pair in q.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = String::from_utf8_lossy(&crate::net::percent_decode(v.as_bytes())).into_owned();
        match k {
            "clear" if v == "all" => {
                clear_all();
                did = true;
            }
            "clear_site" if !v.is_empty() => {
                clear_site(&v);
                did = true;
            }
            "keep" => {
                set_keep_data(v == "1");
                did = true;
            }
            _ => {}
        }
    }
    did
}

/// The ohb://storage page body.
pub fn page_html() -> String {
    use crate::net::html_escape as esc;
    let rows = usage();
    let kb = |b: usize| if b == 0 { "—".to_string() } else if b < 1024 { format!("{b} B") } else { format!("{:.1} KB", b as f64 / 1024.0) };
    let mut t = String::new();
    for (site, c, l, i) in &rows {
        let enc: String = site.bytes().map(|b| if b.is_ascii_alphanumeric() || b == b'.' || b == b'-' { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
        t.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><a href=\"ohb://storage?clear_site={}\">Clear</a></td></tr>",
            esc(site),
            c,
            kb(*l),
            kb(*i),
            enc
        ));
    }
    if rows.is_empty() {
        t.push_str("<tr><td colspan=5 class=muted>No cookies or site data are stored.</td></tr>");
    }
    let keep = keep_data_setting();
    let status = if is_persistent() {
        "Cookies, localStorage and IndexedDB are saved in your profile and kept when you quit."
    } else if keep {
        "This session keeps site data in memory only."
    } else {
        "Site data is deleted when you quit."
    };
    let toggle = if keep {
        "<a class=btn href=\"ohb://storage?keep=0\">Don't keep site data after quitting</a>"
    } else {
        "<a class=btn href=\"ohb://storage?keep=1\">Keep site data after quitting</a>"
    };
    format!(
        r#"<h1>Site data &amp; storage</h1><div class=sub>{status}</div>
<div class=card><b>Profile folder</b><div class=muted>{}</div><p>{toggle} &nbsp; <a class=btn href="ohb://storage?clear=all">Clear all cookies &amp; site data</a></p></div>
<div class=card><table class=kv><tr><th align=left>Site</th><th align=left>Cookies</th><th align=left>Local storage</th><th align=left>IndexedDB</th><th></th></tr>{t}</table></div>"#,
        esc(&crate::ext::profile_dir().display().to_string())
    )
}
