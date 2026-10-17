// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! ES modules for web pages: a module loader that resolves specifiers as URLs
//! (with import maps), fetches module scripts over the network and caches the
//! module map, so `<script type=module>`, static `import` and dynamic
//! `import()` work. It replaces the engine's default loader, which would read
//! modules from the local filesystem.
//!
//! The loader only talks to the engine through `boa_engine::module`; swapping
//! the engine means re-implementing this file's `ModuleLoader` impl only.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use boa_engine::module::{Module, ModuleLoader, ModuleRequest, Referrer};
use boa_engine::{js_string, Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, Source};
use url::Url;

use crate::net::{Request, RequestKind};

/// An import map (`<script type="importmap">`).
#[derive(Default, Clone)]
pub struct ImportMap {
    imports: Vec<(String, Url)>,
    scopes: Vec<(Url, Vec<(String, Url)>)>,
}

impl ImportMap {
    fn parse_specifier_map(v: &serde_like::Value, base: &Url) -> Vec<(String, Url)> {
        let mut out = Vec::new();
        if let serde_like::Value::Object(m) = v {
            for (k, val) in m {
                if let serde_like::Value::String(s) = val {
                    if let Ok(u) = base.join(s) {
                        let key = match parse_url_like(k, base) {
                            Some(u) => u.to_string(),
                            None => k.clone(),
                        };
                        out.push((key, u));
                    }
                }
            }
        }
        // Longest keys first so the most specific prefix wins.
        out.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        out
    }

    /// Merge an import map's JSON text (later maps don't override earlier entries).
    pub fn merge(&mut self, json: &str, base: &Url) {
        let Some(v) = serde_like::parse(json) else { return };
        let serde_like::Value::Object(top) = v else { return };
        for (k, val) in &top {
            match k.as_str() {
                "imports" => {
                    for (s, u) in ImportMap::parse_specifier_map(val, base) {
                        if !self.imports.iter().any(|(e, _)| *e == s) {
                            self.imports.push((s, u));
                        }
                    }
                    self.imports.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
                }
                "scopes" => {
                    if let serde_like::Value::Object(sc) = val {
                        for (prefix, m) in sc {
                            if let Ok(pu) = base.join(prefix) {
                                self.scopes.push((pu, ImportMap::parse_specifier_map(m, base)));
                            }
                        }
                        self.scopes.sort_by(|a, b| b.0.as_str().len().cmp(&a.0.as_str().len()));
                    }
                }
                _ => {}
            }
        }
    }

    fn lookup(map: &[(String, Url)], key: &str) -> Option<Url> {
        for (k, u) in map {
            if k == key {
                return Some(u.clone());
            }
            if k.ends_with('/') && key.starts_with(k.as_str()) {
                return u.join(&key[k.len()..]).ok();
            }
        }
        None
    }

    fn resolve(&self, key: &str, referrer: &Url) -> Option<Url> {
        for (scope, map) in &self.scopes {
            if referrer.as_str().starts_with(scope.as_str()) {
                if let Some(u) = ImportMap::lookup(map, key) {
                    return Some(u);
                }
            }
        }
        ImportMap::lookup(&self.imports, key)
    }
}

/// URL-like specifiers: absolute URLs and `/`, `./`, `../` paths.
fn parse_url_like(spec: &str, base: &Url) -> Option<Url> {
    if spec.starts_with('/') || spec.starts_with("./") || spec.starts_with("../") {
        return base.join(spec).ok();
    }
    Url::parse(spec).ok()
}

/// HTML's "resolve a module specifier".
pub fn resolve_specifier(spec: &str, referrer: &Url, map: &ImportMap) -> Result<Url, String> {
    let as_url = parse_url_like(spec, referrer);
    let key = as_url.as_ref().map(|u| u.to_string()).unwrap_or_else(|| spec.to_string());
    if let Some(u) = map.resolve(&key, referrer) {
        return Ok(u);
    }
    as_url.ok_or_else(|| format!("Failed to resolve module specifier \"{spec}\". Relative references must start with either \"/\", \"./\", or \"../\"."))
}

thread_local! {
    /// Which realm's module map imports go to (0: the page's own realm).
    static REALM_TAG: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Set the realm tag for module loading; returns the previous one.
pub fn set_realm_tag(tag: u64) -> u64 {
    REALM_TAG.with(|t| t.replace(tag))
}

pub struct WebModuleLoader {
    /// The module map: URL -> module.
    map: RefCell<HashMap<String, Module>>,
    /// Document base URL (for classic scripts and inline modules).
    pub base: RefCell<Option<Url>>,
    pub import_map: RefCell<ImportMap>,
}

impl WebModuleLoader {
    pub fn new() -> WebModuleLoader {
        WebModuleLoader { map: RefCell::new(HashMap::new()), base: RefCell::new(None), import_map: RefCell::new(ImportMap::default()) }
    }

    /// Forget every module (the document is going away).
    pub fn clear(&self) {
        self.map.borrow_mut().clear();
    }

    fn referrer_url(&self, referrer: &Referrer) -> Option<Url> {
        referrer.path().and_then(|p| p.to_str()).and_then(|s| Url::parse(s).ok()).or_else(|| self.base.borrow().clone())
    }

    fn fetch_module(&self, url: &Url, json: bool, context: &mut Context) -> JsResult<Module> {
        let local_page = self.base.borrow().as_ref().map(|b| b.scheme() == "file").unwrap_or(false);
        if !(matches!(url.scheme(), "http" | "https" | "data" | "blob") || url.scheme() == "file" && local_page) {
            return Err(JsNativeError::typ().with_message(format!("Failed to fetch dynamically imported module: {url}")).into());
        }
        let t0 = std::time::Instant::now();
        let fetched = fetch_text_typed(url, self.base.borrow().clone());
        if crate::js::profiling() {
            eprintln!("[profile] module fetch {url} {:?}", t0.elapsed());
        }
        let (text, essence) = fetched.map_err(|e| JsError::from(JsNativeError::typ().with_message(format!("Failed to fetch dynamically imported module: {url} ({e})"))))?;
        // Module scripts need a JavaScript MIME type; JSON modules a JSON MIME type.
        let ok = match &essence {
            None => false,
            Some(e) if json => e == "application/json" || e == "text/json" || e.ends_with("+json"),
            Some(e) => crate::page::JS_MIME_ESSENCES.contains(&e.as_str()),
        };
        if !ok && url.scheme() != "file" {
            let what = if json { "a JSON module" } else { "a JavaScript-or-Wasm module script" };
            return Err(JsNativeError::typ().with_message(format!("Failed to load module script: Expected {what} but the server responded with a MIME type of \"{}\".", essence.unwrap_or_default())).into());
        }
        if json {
            return Module::parse_json(JsString::from(text.as_str()), context);
        }
        let key = url.to_string();
        if crate::js::too_deeply_nested(&text) {
            return Err(boa_engine::JsNativeError::syntax().with_message("module nested too deeply").into());
        }
        prefetch_imports(&text, url, self.base.borrow().clone());
        let t1 = std::time::Instant::now();
        let m = Module::parse(Source::from_bytes(text.trim_start_matches('\u{feff}')).with_path(Path::new(&key)), None, context);
        if crate::js::profiling() {
            eprintln!("[profile] module parse {url} ({} KB) {:?}", text.len() / 1024, t1.elapsed());
        }
        m
    }
}

/// A Content-Type's MIME type essence (lowercase) when it is a valid MIME type.
fn mime_essence(ct: &str) -> Option<String> {
    let first = ct.split(';').next().unwrap_or("").trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r'));
    let (t, st) = first.split_once('/')?;
    let token = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b));
    if !token(t) || !token(st) {
        return None;
    }
    Some(first.to_ascii_lowercase())
}

type Fetched = Result<(String, Option<String>), String>;

/// Module scripts fetched ahead of the loader asking for them: a module
/// graph's static imports are discovered from each module's source as it
/// arrives and fetched in parallel (the engine's loader asks one at a time).
struct Prefetch {
    map: std::sync::Mutex<HashMap<String, (std::time::Instant, Option<Fetched>, bool)>>,
    cv: std::sync::Condvar,
}

fn prefetcher() -> &'static Prefetch {
    static P: std::sync::OnceLock<Prefetch> = std::sync::OnceLock::new();
    P.get_or_init(|| Prefetch { map: std::sync::Mutex::new(HashMap::new()), cv: std::sync::Condvar::new() })
}

/// Relative/absolute URL specifiers of a module's static imports and re-exports.
pub fn static_import_specifiers(src: &str) -> Vec<String> {
    let b = src.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut find = |kw: &[u8]| {
        let mut i = 0;
        while i + kw.len() < b.len() {
            let Some(p) = src[i..].find(std::str::from_utf8(kw).unwrap()) else { break };
            let at = i + p;
            i = at + kw.len();
            // Keyword boundaries.
            if at > 0 && (b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_' || b[at - 1] == b'$' || b[at - 1] == b'.') {
                continue;
            }
            let mut j = i;
            while j < b.len() && (b[j] == b' ' || b[j] == b'\n' || b[j] == b'\t' || b[j] == b'\r') {
                j += 1;
            }
            if j >= b.len() || !(b[j] == b'"' || b[j] == b'\'') {
                continue;
            }
            let q = b[j];
            let st = j + 1;
            let Some(e) = b[st..].iter().take(400).position(|&c| c == q || c == b'\n') else { continue };
            if b[st + e] != q {
                continue;
            }
            let spec = &src[st..st + e];
            if (spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') || spec.starts_with("http://") || spec.starts_with("https://")) && !out.iter().any(|o| o == spec) {
                out.push(spec.to_string());
            }
        }
    };
    find(b"from");
    find(b"import");
    out
}

/// Start fetching module `url` (and, once it arrives, its static imports).
pub fn prefetch_module(url: Url, initiator: Option<Url>, depth: u32) {
    if depth > 12 || !matches!(url.scheme(), "http" | "https") {
        return;
    }
    let key = url.to_string();
    {
        let mut m = prefetcher().map.lock().unwrap();
        if m.contains_key(&key) {
            return;
        }
        // Big apps (x.com: ~700 modules) need a generous cap; consumed
        // entries keep only a tombstone, so memory is bounded by the
        // responses not yet taken.
        if m.len() > 4000 {
            m.retain(|_, (t, _, _)| t.elapsed() < std::time::Duration::from_secs(120));
            if m.len() > 4000 {
                return;
            }
        }
        m.insert(key.clone(), (std::time::Instant::now(), None, false));
    }
    let base = url.clone();
    crate::net::fetch_async(Request::get(url, RequestKind::Script, initiator.clone()), move |r| {
        let res: Fetched = match r {
            Ok(r) if r.ok() => {
                let essence = r.header("content-type").and_then(mime_essence);
                Ok((crate::html::encoding::decode(&r.body, r.charset().as_deref(), false), essence))
            }
            Ok(r) => Err(format!("HTTP {}", r.status)),
            Err(e) => Err(e.to_string()),
        };
        let deps: Vec<Url> = match &res {
            Ok((text, Some(e))) if crate::page::JS_MIME_ESSENCES.contains(&e.as_str()) => static_import_specifiers(text).iter().filter_map(|s| base.join(s).ok()).collect(),
            _ => Vec::new(),
        };
        {
            let p = prefetcher();
            let mut m = p.map.lock().unwrap();
            if let Some(slot) = m.get_mut(&key) {
                if !slot.2 {
                    slot.1 = Some(res);
                }
            }
            p.cv.notify_all();
        }
        for d in deps {
            prefetch_module(d, initiator.clone(), depth + 1);
        }
    });
}

/// A prefetched (or in-flight) module response, waiting for it if needed.
fn take_prefetched(url: &Url) -> Option<Fetched> {
    let key = url.to_string();
    let p = prefetcher();
    let mut m = p.map.lock().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        match m.get_mut(&key) {
            None | Some((_, _, true)) => return None,
            Some(slot) if slot.1.is_some() => {
                // Leave a tombstone so the graph isn't prefetched again; a
                // failed prefetch is retried by the loader's own fetch.
                slot.2 = true;
                return slot.1.take().filter(|r| r.is_ok());
            }
            Some(slot) => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    slot.2 = true;
                    return None;
                }
                m = p.cv.wait_timeout(m, deadline - now).unwrap().0;
            }
        }
    }
}

/// Prefetch the static imports of a module script about to run.
pub fn prefetch_imports(src: &str, base: &Url, initiator: Option<Url>) {
    for s in static_import_specifiers(src) {
        if let Ok(u) = base.join(&s) {
            prefetch_module(u, initiator.clone(), 1);
        }
    }
}

/// Fetch a module script with its MIME type essence (None: missing or invalid).
pub fn fetch_text_typed(url: &Url, initiator: Option<Url>) -> Result<(String, Option<String>), String> {
    if let Some(r) = take_prefetched(url) {
        return r;
    }
    let r = crate::net::client().fetch(Request::get(url.clone(), RequestKind::Script, initiator)).map_err(|e| e.to_string())?;
    if !r.ok() {
        return Err(format!("HTTP {}", r.status));
    }
    let essence = r.header("content-type").and_then(mime_essence);
    Ok((crate::html::encoding::decode(&r.body, r.charset().as_deref(), false), essence))
}

/// Fetch a module script (blocking: module graphs load on the script thread).
pub fn fetch_text(url: &Url, initiator: Option<Url>) -> Result<String, String> {
    let r = crate::net::client().fetch(Request::get(url.clone(), RequestKind::Script, initiator)).map_err(|e| e.to_string())?;
    if !r.ok() {
        return Err(format!("HTTP {}", r.status));
    }
    Ok(crate::html::encoding::decode(&r.body, r.charset().as_deref(), false))
}

impl ModuleLoader for WebModuleLoader {
    fn load_imported_module(self: Rc<Self>, referrer: Referrer, request: ModuleRequest, context: &RefCell<&mut Context>) -> impl std::future::Future<Output = JsResult<Module>> {
        let result = (|| {
            let spec = request.specifier().to_std_string_escaped();
            let base = self.referrer_url(&referrer).ok_or_else(|| JsError::from(JsNativeError::typ().with_message("no base URL for module resolution")))?;
            let url = resolve_specifier(&spec, &base, &self.import_map.borrow()).map_err(|m| JsError::from(JsNativeError::typ().with_message(m)))?;
            let json = request.get_attribute("type").map(|t| t.to_std_string_escaped() == "json").unwrap_or(false);
            let key = if json { format!("json:{url}") } else { url.to_string() };
            let key = match REALM_TAG.with(|t| t.get()) {
                0 => key,
                tag => format!("{tag:x}|{key}"),
            };
            if let Some(m) = self.map.borrow().get(&key) {
                return Ok(m.clone());
            }
            let m = self.fetch_module(&url, json, &mut context.borrow_mut())?;
            self.map.borrow_mut().insert(key, m.clone());
            Ok(m)
        })();
        async { result }
    }

    fn init_import_meta(self: Rc<Self>, import_meta: &JsObject, module: &Module, context: &mut Context) {
        let url = module.path().and_then(|p| p.to_str()).map(|s| s.to_string()).or_else(|| self.base.borrow().as_ref().map(|u| u.to_string())).unwrap_or_default();
        let _ = import_meta.set(js_string!("url"), JsString::from(url.as_str()), false, context);
        let loader = self.clone();
        let resolve = unsafe {
            NativeFunction::from_closure(move |_this, args, ctx| {
                let spec = args.first().cloned().unwrap_or_default().to_string(ctx)?.to_std_string_escaped();
                let base = Url::parse(&url).ok().or_else(|| loader.base.borrow().clone()).ok_or_else(|| JsError::from(JsNativeError::typ().with_message("no base URL")))?;
                let u = resolve_specifier(&spec, &base, &loader.import_map.borrow()).map_err(|m| JsError::from(JsNativeError::typ().with_message(m)))?;
                Ok(JsValue::from(JsString::from(u.as_str())))
            })
        };
        let f = boa_engine::object::FunctionObjectBuilder::new(context.realm(), resolve).name(js_string!("resolve")).length(1).build();
        let _ = import_meta.set(js_string!("resolve"), f, false, context);
    }
}

/// A tiny JSON reader for import maps (no dependency on serde).
mod serde_like {
    #[derive(Debug)]
    pub enum Value {
        Null,
        Bool(bool),
        Number(f64),
        String(String),
        Array(Vec<Value>),
        Object(Vec<(String, Value)>),
    }

    pub fn parse(s: &str) -> Option<Value> {
        let mut p = P { s: s.as_bytes(), i: 0 };
        let v = p.value()?;
        p.ws();
        if p.i == p.s.len() {
            Some(v)
        } else {
            None
        }
    }

    struct P<'a> {
        s: &'a [u8],
        i: usize,
    }

    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
                self.i += 1;
            }
        }
        fn value(&mut self) -> Option<Value> {
            self.ws();
            match *self.s.get(self.i)? {
                b'{' => {
                    self.i += 1;
                    let mut out = Vec::new();
                    self.ws();
                    if self.s.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        return Some(Value::Object(out));
                    }
                    loop {
                        self.ws();
                        let Value::String(k) = self.value()? else { return None };
                        self.ws();
                        if self.s.get(self.i) != Some(&b':') {
                            return None;
                        }
                        self.i += 1;
                        let v = self.value()?;
                        out.push((k, v));
                        self.ws();
                        match self.s.get(self.i)? {
                            b',' => self.i += 1,
                            b'}' => {
                                self.i += 1;
                                return Some(Value::Object(out));
                            }
                            _ => return None,
                        }
                    }
                }
                b'[' => {
                    self.i += 1;
                    let mut out = Vec::new();
                    self.ws();
                    if self.s.get(self.i) == Some(&b']') {
                        self.i += 1;
                        return Some(Value::Array(out));
                    }
                    loop {
                        out.push(self.value()?);
                        self.ws();
                        match self.s.get(self.i)? {
                            b',' => self.i += 1,
                            b']' => {
                                self.i += 1;
                                return Some(Value::Array(out));
                            }
                            _ => return None,
                        }
                    }
                }
                b'"' => {
                    self.i += 1;
                    let mut out = String::new();
                    loop {
                        let c = *self.s.get(self.i)?;
                        self.i += 1;
                        match c {
                            b'"' => return Some(Value::String(out)),
                            b'\\' => {
                                let e = *self.s.get(self.i)?;
                                self.i += 1;
                                match e {
                                    b'n' => out.push('\n'),
                                    b't' => out.push('\t'),
                                    b'r' => out.push('\r'),
                                    b'b' => out.push('\u{8}'),
                                    b'f' => out.push('\u{c}'),
                                    b'u' => {
                                        let h = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
                                        self.i += 4;
                                        out.push(char::from_u32(u32::from_str_radix(h, 16).ok()?).unwrap_or('\u{fffd}'));
                                    }
                                    other => out.push(other as char),
                                }
                            }
                            _ => {
                                // Re-decode UTF-8 sequences.
                                let start = self.i - 1;
                                let len = match c {
                                    0..=0x7f => 1,
                                    0xc0..=0xdf => 2,
                                    0xe0..=0xef => 3,
                                    _ => 4,
                                };
                                let chunk = std::str::from_utf8(self.s.get(start..start + len)?).ok()?;
                                out.push_str(chunk);
                                self.i = start + len;
                            }
                        }
                    }
                }
                b't' if self.s[self.i..].starts_with(b"true") => {
                    self.i += 4;
                    Some(Value::Bool(true))
                }
                b'f' if self.s[self.i..].starts_with(b"false") => {
                    self.i += 5;
                    Some(Value::Bool(false))
                }
                b'n' if self.s[self.i..].starts_with(b"null") => {
                    self.i += 4;
                    Some(Value::Null)
                }
                _ => {
                    let start = self.i;
                    while self.i < self.s.len() && matches!(self.s[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                        self.i += 1;
                    }
                    std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok().map(Value::Number)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specifiers() {
        let base = Url::parse("https://example.com/app/index.html").unwrap();
        let map = ImportMap::default();
        assert_eq!(resolve_specifier("./a.js", &base, &map).unwrap().as_str(), "https://example.com/app/a.js");
        assert_eq!(resolve_specifier("/x/b.js", &base, &map).unwrap().as_str(), "https://example.com/x/b.js");
        assert_eq!(resolve_specifier("https://cdn.test/c.js", &base, &map).unwrap().as_str(), "https://cdn.test/c.js");
        assert!(resolve_specifier("react", &base, &map).is_err());
        let mut m = ImportMap::default();
        m.merge(r#"{"imports":{"react":"/vendor/react.js","lib/":"https://cdn.test/lib/"}}"#, &base);
        assert_eq!(resolve_specifier("react", &base, &m).unwrap().as_str(), "https://example.com/vendor/react.js");
        assert_eq!(resolve_specifier("lib/x.js", &base, &m).unwrap().as_str(), "https://cdn.test/lib/x.js");
    }
}
