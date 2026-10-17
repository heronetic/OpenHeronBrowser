// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronScript — JavaScript integration.
//!
//! The DOM is exposed through a small set of native functions on a hidden
//! `__n` object that operate on numeric node ids (`natives.rs`). A JavaScript
//! prelude (prelude.js) builds the familiar DOM/BOM API surface (Node,
//! Element, document, window, events, timers, fetch, ...) on top of it.
//!
//! The engine sits behind `host::ScriptEngine`; `engine_boa.rs` adapts Boa.
//! Everything else here (natives, prelude, the page runtime below) is
//! engine-neutral and talks in `host::HostValue`s.

use std::time::{Duration, Instant};

use crate::dom::{NodeId, DOCUMENT};
use crate::page::{with_page, DefaultAction, EventInit, JsTask};

pub mod engine_boa;
pub mod host;
pub mod modules;

thread_local! {
    /// The native most recently called from script (debugging: who forced a layout).
    pub static CURRENT_NATIVE: std::cell::Cell<&'static str> = const { std::cell::Cell::new("") };
    /// Debugging: print every native call (OHB_TRACE_INIT traces runtime setup).
    pub static TRACE_NATIVES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// OHB_NATIVE_PROF: time spent per native (inclusive), drained by `native_prof_report`.
    static NATIVE_PROF: std::cell::RefCell<std::collections::HashMap<&'static str, (u32, Duration)>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

pub fn native_prof_on() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("OHB_NATIVE_PROF").is_some());
    *ON
}

pub fn native_prof_add(name: &'static str, d: Duration) {
    NATIVE_PROF.with(|m| {
        let mut m = m.borrow_mut();
        let e = m.entry(name).or_insert((0, Duration::ZERO));
        e.0 += 1;
        e.1 += d;
    });
}

/// Print (and reset) the slowest natives since the last report.
pub fn native_prof_report(what: &str) {
    static GC_STATS: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("OHB_GC_STATS").is_some());
    if *GC_STATS {
        thread_local!(static LAST: std::cell::Cell<(usize, u64)> = const { std::cell::Cell::new((0, 0)) });
        let (c, ns, bytes, threshold) = boa_engine::gc::gc_stats();
        let (c0, ns0) = LAST.with(|l| l.replace((c, ns)));
        if c > c0 {
            eprintln!("[gc] {what}: {} collections, {:.1}ms (total {c}, {:.1}ms; heap {:.1}MB, next at {:.1}MB)", c - c0, (ns - ns0) as f64 / 1e6, ns as f64 / 1e6, bytes as f64 / 1048576.0, threshold as f64 / 1048576.0);
        }
    }
    if !native_prof_on() {
        return;
    }
    let mut v: Vec<(&'static str, (u32, Duration))> = NATIVE_PROF.with(|m| m.borrow_mut().drain().collect());
    v.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    let top: Vec<String> = v.iter().take(8).filter(|e| e.1 .1 > Duration::from_millis(2)).map(|(n, (c, d))| format!("{n}x{c}={:.1}ms", d.as_secs_f64() * 1e3)).collect();
    if !top.is_empty() {
        eprintln!("[native-prof] {what}: {}", top.join(" "));
    }
}
mod intl;
mod media;
mod natives;
mod zipstream;
pub use natives::wpt_enabled;
pub mod worker;
pub mod wasm;

use host::{jstr, HostValue, ScriptEngine};

const PRELUDE: &str = include_str!("prelude.js");
/// ECMA-402 for engines without their own Intl; evaluated before the prelude
/// (it needs the natives the prelude hides).
pub(crate) const INTL: &str = include_str!("intl.js");
/// In-memory IndexedDB, evaluated after the prelude.
const IDB: &str = include_str!("idb.js");
/// Web Audio, evaluated after the prelude.
const WEBAUDIO: &str = include_str!("webaudio.js");
/// XPath 1.0 (document.evaluate), evaluated after the prelude.
const XPATH: &str = include_str!("xpath.js");
pub(crate) const WEBCRYPTO: &str = include_str!("webcrypto.js");
/// DOMParser / XMLSerializer / XML innerHTML, evaluated after the prelude.
const DOMPARSING: &str = include_str!("domparsing.js");
const DEVTOOLS_BRIDGE: &str = "(function(){const N=globalThis.__n;Object.defineProperty(globalThis,'ohbDevtools',{value:Object.freeze({send:(m,p)=>N.dtSend(String(m),JSON.stringify(p===undefined?{}:p)),close:()=>N.dtClose(),target:N.dtTarget()}),enumerable:false});})();";
pub(crate) const HIDE_NATIVES: &str = "delete globalThis.__n; delete globalThis.__ohb_microtasks; delete globalThis.__ohb_builtin; delete globalThis.__ohb_newRealm; delete globalThis.__ohb_runScriptIn; delete globalThis.__ohb_runModuleIn; delete globalThis.__ohb_sabShare; delete globalThis.__ohb_sabGet; delete globalThis.__ohb_dropRealm; delete globalThis.__ohb_makeDDA; if (globalThis.__ohb_shapeWindow) { try { globalThis.__ohb_shapeWindow(); } catch (e) {} delete globalThis.__ohb_shapeWindow; }";

/// `OHB_PROFILE=1` prints per-script parse and run times.
pub fn profiling() -> bool {
    static P: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *P.get_or_init(|| std::env::var_os("OHB_PROFILE").is_some())
}

/// Under OHB_PROFILE, accumulate time per engine phase and print a summary
/// line whenever a phase has used another 250 ms.
pub fn profile_note(phase: &'static str, start: Instant) {
    if !profiling() {
        return;
    }
    thread_local! {
        static ACC: std::cell::RefCell<std::collections::HashMap<&'static str, (u32, Duration, Duration)>> = Default::default();
    }
    let d = start.elapsed();
    ACC.with(|a| {
        let mut a = a.borrow_mut();
        let e = a.entry(phase).or_insert((0, Duration::ZERO, Duration::ZERO));
        e.0 += 1;
        e.1 += d;
        if e.1 - e.2 >= Duration::from_millis(250) {
            e.2 = e.1;
            eprintln!("[profile] {phase}: {} passes, {:?} total (last {:?})", e.0, e.1, d);
        }
    });
}

/// Notes a phase's time under OHB_PROFILE when dropped.
pub struct ProfGuard(pub &'static str, pub Instant);
impl Drop for ProfGuard {
    fn drop(&mut self) {
        profile_note(self.0, self.1);
    }
}

/// The prelude with build-time identity (UA, platform, versions) filled in,
/// so `navigator` always agrees with the HTTP `User-Agent` header.
pub(crate) fn prelude_source() -> &'static str {
    static SRC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SRC.get_or_init(|| {
        let (_, platform) = crate::net::ua_platform();
        let uad_platform = if platform.starts_with("Win") { "Windows" } else if platform.starts_with("Mac") { "macOS" } else { "Linux" };
        let chrome_major = crate::net::CHROME_COMPAT.split('.').next().unwrap_or("0");
        let ohb_major = crate::net::VERSION.split('.').take(2).collect::<Vec<_>>().join(".");
        PRELUDE
            .replace("__OHB_UA__", &crate::net::user_agent())
            .replace("__OHB_PLATFORM__", platform)
            .replace("__OHB_UADATA_PLATFORM__", uad_platform)
            .replace("architecture: 'x86'", if cfg!(target_arch = "aarch64") { "architecture: 'arm'" } else { "architecture: 'x86'" })
            .replace("__OHB_CHROME_FULL__", crate::net::CHROME_COMPAT)
            .replace("__OHB_CHROME__", chrome_major)
            .replace("__OHB_MAJOR__", &ohb_major)
    })
}

fn pid_list(p: &[u64]) -> HostValue {
    HostValue::Array(p.iter().map(|&x| HostValue::Number(x as f64)).collect())
}

/// Remember a script's text for DevTools' Sources panel.
fn note_source(name: &str, node: Option<NodeId>, src: &str) {
    with_page(|p| {
        let label = if url::Url::parse(name).is_ok() && name != p.doc.url.as_str() {
            name.to_string()
        } else {
            match node {
                Some(n) => format!("{} (inline script #{n})", p.doc.url),
                None => format!("{} (script)", p.doc.url),
            }
        };
        p.note_script_source(&label, src);
    });
}

pub struct JsRuntime {
    engine: Box<dyn ScriptEngine>,
    nav_gen: u64,
    next_deadline_ms: Option<f64>,
    raf_pending: bool,
    last_raf: Instant,
}

// ----------------------------------------------------------------- runtime

/// A runtime with the built-in scripts loaded, not yet tied to a document.
pub struct Prepared {
    engine: Box<dyn ScriptEngine>,
}

thread_local! {
    static SPARE: std::cell::RefCell<Option<Prepared>> = const { std::cell::RefCell::new(None) };
}

impl JsRuntime {
    pub fn new(nav_gen: u64) -> Option<JsRuntime> {
        let prepared = SPARE.with(|s| s.borrow_mut().take()).or_else(JsRuntime::prepare)?;
        JsRuntime::attach(prepared, nav_gen)
    }

    /// Build a runtime ahead of need (while a document is still loading):
    /// the engine, natives and built-in scripts, which don't depend on the
    /// document. `attach` finishes it for the document.
    pub fn prewarm() {
        if SPARE.with(|s| s.borrow().is_some()) {
            return;
        }
        if let Some(p) = JsRuntime::prepare() {
            SPARE.with(|s| *s.borrow_mut() = Some(p));
        }
    }

    fn prepare() -> Option<Prepared> {
        let t_init = Instant::now();
        let mut engine: Box<dyn ScriptEngine> = Box::new(engine_boa::BoaEngine::new(None)?);
        engine.install_natives(&natives::all());
        let trace = std::env::var_os("OHB_TRACE_INIT").is_some();
        TRACE_NATIVES.with(|t| t.set(trace));
        if let Err(e) = engine.run_classic(INTL, "ohb://intl.js") {
            eprintln!("[heron-script] Intl init failed: {e}");
        }
        if let Err(e) = engine.run_classic(prelude_source(), "ohb://prelude.js") {
            eprintln!("[heron-script] prelude failed: {e}");
            return None;
        }
        TRACE_NATIVES.with(|t| t.set(false));
        if let Err(e) = engine.run_classic(IDB, "ohb://idb.js") {
            eprintln!("[heron-script] indexedDB init failed: {e}");
        }
        if let Err(e) = engine.run_classic(WEBAUDIO, "ohb://webaudio.js") {
            eprintln!("[heron-script] Web Audio init failed: {e}");
        }
        if let Err(e) = engine.run_classic(WEBCRYPTO, "ohb://webcrypto.js") {
            eprintln!("[heron-script] Web Crypto init failed: {e}");
        }
        if let Err(e) = engine.run_classic(XPATH, "ohb://xpath.js") {
            eprintln!("[heron-script] XPath init failed: {e}");
        }
        if let Err(e) = engine.run_classic(DOMPARSING, "ohb://domparsing.js") {
            eprintln!("[heron-script] DOM Parsing init failed: {e}");
        }
        if profiling() {
            eprintln!("[profile] runtime prepare: {:?}", t_init.elapsed());
        }
        Some(Prepared { engine })
    }

    /// Finish a prepared runtime for the current document.
    fn attach(prepared: Prepared, nav_gen: u64) -> Option<JsRuntime> {
        worker::terminate_all();
        let t_init = Instant::now();
        let mut engine = prepared.engine;
        engine.set_base_url(with_page(|p| p.doc.base_url.clone()));
        // A DevTools page gets a bridge to the tab it inspects.
        if let Some(url) = with_page(|p| p.doc.url.clone()) {
            if url.scheme() == "ohb" && url.host_str() == Some("devtools") {
                let target = url.query_pairs().find(|(k, _)| k == "tab").and_then(|(_, v)| v.parse::<u64>().ok());
                with_page(|p| p.dt.target = target);
                if let Err(e) = engine.run_classic(DEVTOOLS_BRIDGE, "ohb://devtools-bridge.js") {
                    eprintln!("[heron-script] DevTools bridge failed: {e}");
                }
            }
        }
        // The natives stay private to the built-in scripts.
        let _ = engine.run_classic(HIDE_NATIVES, "ohb://init.js");
        // Debugging aid: OHB_INJECT=<file.js> runs before any page script.
        if let Some(code) = std::env::var_os("OHB_INJECT").and_then(|f| std::fs::read_to_string(f).ok()) {
            if let Err(e) = engine.run_classic(&code, "ohb://inject.js") {
                eprintln!("[heron-script] OHB_INJECT failed: {e}");
            }
        }
        // Extensions' document_start content scripts.
        if let Some(url) = with_page(|p| p.doc.url.clone()) {
            for (name, code) in crate::ext::content_scripts(&url, crate::ext::RunAt::DocumentStart) {
                if let Err(e) = engine.run_classic(&code, &name) {
                    eprintln!("[extensions] {name}: {e}");
                }
            }
        }
        engine.run_jobs();
        if profiling() {
            eprintln!("[profile] runtime attach: {:?}", t_init.elapsed());
        }
        Some(JsRuntime { engine, nav_gen, next_deadline_ms: None, raf_pending: false, last_raf: Instant::now() })
    }

    /// Drain the microtask queue, then report promises that are still
    /// rejected without a handler (`unhandledrejection`).
    fn flush_jobs(&mut self) {
        self.engine.report_unhandled_rejections("__ohb_unhandledRejection");
    }

    pub fn doc_gen(&self) -> u64 {
        self.nav_gen
    }

    fn call_global(&mut self, name: &str, args: &[HostValue]) -> Option<HostValue> {
        let t0 = Instant::now();
        let r = self.engine.call_global(name, args);
        if t0.elapsed() > Duration::from_millis(500) && std::env::var_os("OHB_SLOW_LISTENERS").is_some() {
            eprintln!("[slow call] {name} took {:?}", t0.elapsed());
        }
        if t0.elapsed() > Duration::from_millis(if native_prof_on() { 40 } else { 200 }) {
            native_prof_report(name);
        }
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                if std::env::var("OHB_QUIET").is_err() {
                    eprintln!("[heron-script] {name}: {e}");
                }
                None
            }
        }
    }

    /// The URL scripts and modules resolve relative imports against: the
    /// script's own URL, or the document base for inline code.
    fn script_path(&mut self, name: &str) -> String {
        let base = with_page(|p| p.doc.base_url.clone());
        self.engine.set_base_url(base.clone());
        if url::Url::parse(name).is_ok() {
            name.to_string()
        } else {
            base.map(|u| u.to_string()).unwrap_or_default()
        }
    }

    /// Extensions' content scripts for this document at `when`.
    pub fn run_content_scripts(&mut self, when: crate::ext::RunAt) {
        let Some(url) = with_page(|p| p.doc.url.clone()) else { return };
        for (name, code) in crate::ext::content_scripts(&url, when) {
            if let Err(e) = self.engine.run_classic(&code, &name) {
                eprintln!("[extensions] {name}: {e}");
            }
        }
        self.flush_jobs();
    }

    /// `<script type="importmap">`.
    pub fn add_import_map(&mut self, json: &str) {
        let base = with_page(|p| p.doc.base_url.clone());
        self.engine.set_base_url(base);
        self.engine.add_import_map(json);
    }

    fn report_error(&mut self, what: &str, e: String) {
        if std::env::var("OHB_QUIET").is_err() {
            eprintln!("[heron-script] error in {what}: {}", e.chars().take(300).collect::<String>());
        }
        self.call_global("__ohb_reportError", &[jstr(&e)]);
    }

    /// Run a module script (`<script type="module">`): load its dependency
    /// graph, link and evaluate it.
    pub fn run_module(&mut self, src: &str, node: Option<NodeId>, name: &str) {
        let path = self.script_path(name);
        note_source(name, node, src);
        // Inline modules get a unique key so they never collide in the module map.
        let key = if url::Url::parse(name).is_ok() { path.clone() } else { format!("{path}#inline-module-{}", node.unwrap_or(0)) };
        let t0 = Instant::now();
        if let Ok(base) = url::Url::parse(&key) {
            modules::prefetch_imports(src, &base, Some(base.clone()));
        }
        let r = self.engine.run_module(src, &key);
        if profiling() {
            eprintln!("[profile] @{:.3} module {} ({} KB): {:?}", crate::net::wall_secs(), name.chars().take(100).collect::<String>(), src.len() / 1024, t0.elapsed());
        }
        native_prof_report(name);
        if let Err(e) = r {
            self.report_error(&format!("module {name}"), e);
        }
        self.flush_jobs();
    }

    pub fn run_script(&mut self, src: &str, node: Option<NodeId>, name: &str) {
        self.call_global("__ohb_setCurrentScript", &[HostValue::from(node.map(|n| n as i32).unwrap_or(-1))]);
        let path = self.script_path(name);
        note_source(name, node, src);
        let t0 = Instant::now();
        let r = self.engine.run_classic(src, &path);
        if profiling() {
            eprintln!("[profile] script {} ({} KB): {:?}", name.chars().take(100).collect::<String>(), src.len() / 1024, t0.elapsed());
        }
        native_prof_report(name);
        if let Err(e) = r {
            self.report_error(name, e);
        }
        // The microtask checkpoint is part of running the script, so jobs it
        // queued still see it as document.currentScript (Next.js/Turbopack
        // read it from a promise callback).
        self.flush_jobs();
        self.call_global("__ohb_setCurrentScript", &[HostValue::from(-1)]);
    }

    pub fn dispatch(&mut self, target: NodeId, ty: &str, init: &EventInit) -> bool {
        let o = HostValue::object(vec![
            ("clientX", HostValue::from(init.x as f64)),
            ("clientY", HostValue::from(init.y as f64)),
            ("button", HostValue::from(init.button as i32)),
            ("key", jstr(&init.key)),
            ("code", jstr(&init.code)),
            ("ctrlKey", HostValue::from(init.mods.ctrl)),
            ("shiftKey", HostValue::from(init.mods.shift)),
            ("altKey", HostValue::from(init.mods.alt)),
            ("metaKey", HostValue::from(init.mods.meta)),
            ("bubbles", HostValue::from(init.bubbles)),
            ("cancelable", HostValue::from(init.cancelable)),
            ("deltaX", HostValue::from(init.dx as f64)),
            ("deltaY", HostValue::from(init.dy as f64)),
            ("repeat", HostValue::from(init.repeat)),
            ("buttons", HostValue::from(if ty.starts_with("mouse") || ty.starts_with("pointer") || ty == "click" || ty == "dblclick" || ty == "auxclick" || ty == "contextmenu" || ty == "wheel" { init.buttons.map(|b| b as i32).unwrap_or_else(|| with_page(|p| p.buttons_down).unwrap_or(0) as i32) } else { 0 })),
            ("detail", HostValue::from(init.detail)),
            ("movementX", HostValue::from(init.mx as f64)),
            ("movementY", HostValue::from(init.my as f64)),
        ]);
        let t0 = Instant::now();
        let r = self.call_global("__ohb_dispatch", &[HostValue::from(target as i32), jstr(ty), o]);
        let t1 = t0.elapsed();
        self.flush_jobs();
        if t0.elapsed() > Duration::from_millis(300) && std::env::var_os("OHB_SLOW_LISTENERS").is_some() {
            let desc = with_page(|p| format!("<{} id={:?} src={:?}>", p.doc.tag(target), p.doc.attr(target, "id"), p.doc.attr(target, "src").map(|s| s.chars().take(80).collect::<String>()))).unwrap_or_default();
            eprintln!("[slow dispatch] {} on {}: listeners {:?}, microtasks {:?}", ty, desc, t1, t0.elapsed() - t1);
        }
        r.map(|v| v.truthy()).unwrap_or(false)
    }

    pub fn window_event(&mut self, ty: &str) {
        self.call_global("__ohb_windowEvent", &[jstr(ty)]);
        self.flush_jobs();
    }

    /// Evaluate `code` and stringify the result (strings as-is, other values
    /// through JSON.stringify). Used by headless `--eval`.
    pub fn eval_to_string(&mut self, code: &str) -> String {
        let r = self.engine.eval_to_string(code);
        self.flush_jobs();
        r
    }

    pub fn eval_url(&mut self, code: &str) {
        let path = self.script_path("javascript:");
        if let Err(e) = self.engine.run_classic(code, &path) {
            eprintln!("[heron-script] javascript: URL error: {e}");
        }
        self.flush_jobs();
    }

    pub fn fetch_done(&mut self, id: u64, r: Result<crate::net::cors::Outcome, crate::net::NetError>) {
        let args: Vec<HostValue> = match r {
            Ok(o) => {
                let resp = o.resp;
                let mut flat = Vec::new();
                for (k, v) in &resp.headers {
                    flat.push(k.to_ascii_lowercase());
                    flat.push(v.clone());
                }
                // The body stays native until script reads it (natives::n_fetch_body).
                let url = resp.url.to_string();
                let status = resp.status;
                let status_text = resp.status_text.clone();
                with_page(|p| p.fetch_bodies.insert(id, resp.body));
                vec![
                    HostValue::from(id as f64),
                    HostValue::from(status as i32),
                    jstr(&status_text),
                    jstr(&url),
                    host::jstrs(flat),
                    jstr(o.rtype),
                    HostValue::from(o.redirected),
                    HostValue::Null,
                ]
            }
            Err(e) => vec![HostValue::from(id as f64), HostValue::from(0), jstr(""), jstr(""), HostValue::Null, jstr(""), HostValue::from(false), jstr(&e.to_string())],
        };
        self.call_global("__ohb_fetchDone", &args);
        self.flush_jobs();
        // Unread streams go: the whole body waits in fetch_bodies.
        with_page(|p| {
            if p.fetch_streams.get(&id).map(|fs| !fs.streamed).unwrap_or(false) {
                p.fetch_streams.remove(&id);
            }
        });
    }

    /// A message or error from a worker thread.
    pub fn worker_event(&mut self, id: u32, ev: worker::WorkerEvent) {
        let (kind, data, ports) = match ev {
            worker::WorkerEvent::Message(v, ports) => ("message", v, ports),
            worker::WorkerEvent::Error(e) => ("error", HostValue::String(e), Vec::new()),
        };
        self.call_global("__ohb_workerEvent", &[HostValue::from(id), jstr(kind), data, pid_list(&ports)]);
        self.flush_jobs();
    }

    /// Run due timers (until `deadline`, leaving the rest due).
    pub fn run_timers(&mut self, deadline: Instant) {
        let now = host::page_now_ms();
        let mut next = None;
        for _ in 0..500 {
            host::set_timeline_frozen(Some(host::page_now_ms()));
            next = self.call_global("__ohb_runTimers", &[HostValue::from(now)]);
            self.flush_jobs();
            host::set_timeline_frozen(None);
            if next.as_ref().map(|v| v.number()) != Some(-2.0) {
                break;
            }
            if Instant::now() > deadline {
                next = Some(HostValue::from(now)); // more are due: come back right away
                break;
            }
        }
        self.next_deadline_ms = next.map(|v| v.number()).filter(|n| *n >= 0.0);
        // Animation frames (~60 Hz while requested).
        let has_raf = self.call_global("__ohb_hasRaf", &[]).map(|v| v.truthy()).unwrap_or(false);
        let scrolls = with_page(|p| p.scroll_events_pending()).unwrap_or(false);
        self.raf_pending = has_raf || scrolls;
        if (has_raf || scrolls) && self.last_raf.elapsed() >= Duration::from_millis(16) {
            self.last_raf = Instant::now();
            // Update the rendering: scroll steps, then animation frame callbacks.
            if scrolls {
                let (now_s, ends) = with_page(|p| p.take_scroll_events()).unwrap_or_default();
                for (list, ty) in [(now_s, "scroll"), (ends, "scrollend")] {
                    for n in list {
                        if n == crate::dom::DOCUMENT {
                            self.window_event(ty);
                        } else if with_page(|p| p.doc.valid(n)).unwrap_or(false) {
                            self.dispatch(n, ty, &crate::page::EventInit::default());
                        }
                    }
                }
            }
            // Animation frame callbacks, then resize/intersection observations.
            host::set_timeline_frozen(Some(now));
            self.call_global("__ohb_runRaf", &[HostValue::from(now)]);
            self.flush_jobs();
            host::set_timeline_frozen(None);
        }
    }

    pub fn next_deadline(&self) -> Option<Duration> {
        let now = host::page_now_ms();
        let mut d: Option<Duration> = self.next_deadline_ms.map(|t| Duration::from_secs_f64(((t - now).max(0.0)) / 1000.0));
        if self.raf_pending {
            let r = Duration::from_millis(16).saturating_sub(self.last_raf.elapsed());
            d = Some(d.map(|x| x.min(r)).unwrap_or(r));
        }
        d
    }
}

/// How long one `pump` may run script before the page gets to render and
/// handle network results (the rest waits for the next turn).
static PUMP_BUDGET: std::sync::LazyLock<Duration> =
    std::sync::LazyLock::new(|| Duration::from_millis(std::env::var("OHB_PUMP_BUDGET_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(60)));

/// Bracket nesting beyond which a script is refused (the recursive-descent
/// parser and compiler would overflow the thread's stack and abort the process).
const MAX_JS_NESTING: usize = 3000;

/// Does `src` nest brackets deeper than `MAX_JS_NESTING`? Strings, template
/// literals, comments and regular expression literals are skipped (regular
/// expressions recognized by the usual previous-token heuristic).
pub fn too_deeply_nested(src: &str) -> bool {
    let b = src.as_bytes();
    if b.len() < MAX_JS_NESTING {
        return false;
    }
    let n = b.len();
    let mut i = 0;
    let mut depth = 0usize;
    // Depths at which template-literal substitutions (`${`) opened.
    let mut tmpl: Vec<usize> = Vec::new();
    // The last significant byte (b'a' for identifiers/numbers/strings) and
    // whether it ended a keyword after which `/` starts a regex.
    let mut prev = b'(';
    let mut prev_kw = false;
    let mut in_template = false;
    while i < n {
        if in_template {
            match b[i] {
                b'\\' => i += 2,
                b'`' => {
                    in_template = false;
                    prev = b'a';
                    prev_kw = false;
                    i += 1;
                }
                b'$' if i + 1 < n && b[i + 1] == b'{' => {
                    in_template = false;
                    tmpl.push(depth);
                    depth += 1;
                    prev = b'(';
                    prev_kw = false;
                    i += 2;
                }
                _ => i += 1,
            }
            continue;
        }
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'\'' | b'"' => {
                i += 1;
                while i < n && b[i] != c && b[i] != b'\n' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                prev = b'a';
                prev_kw = false;
            }
            b'`' => {
                in_template = true;
                i += 1;
            }
            b'/' if i + 1 < n && b[i + 1] == b'/' => {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < n && b[i + 1] == b'*' => {
                i += 2;
                while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b'/' => {
                let regex = prev_kw || !matches!(prev, b'a' | b')' | b']');
                i += 1;
                if regex {
                    let mut class = false;
                    while i < n && b[i] != b'\n' {
                        match b[i] {
                            b'\\' => i += 1,
                            b'[' => class = true,
                            b']' => class = false,
                            b'/' if !class => break,
                            _ => {}
                        }
                        i += 1;
                    }
                    i += 1;
                    prev = b'a';
                } else {
                    prev = b'/';
                }
                prev_kw = false;
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                if depth > MAX_JS_NESTING {
                    return true;
                }
                prev = c;
                prev_kw = false;
                i += 1;
            }
            b')' | b']' => {
                depth = depth.saturating_sub(1);
                prev = c;
                prev_kw = false;
                i += 1;
            }
            b'}' => {
                if tmpl.last() == Some(&depth.saturating_sub(1)) && !tmpl.is_empty() {
                    tmpl.pop();
                    depth = depth.saturating_sub(1);
                    in_template = true;
                } else {
                    depth = depth.saturating_sub(1);
                    prev = c;
                    prev_kw = false;
                }
                i += 1;
            }
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80 => {
                let start = i;
                while i < n && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' || b[i] >= 0x80) {
                    i += 1;
                }
                let w = &b[start..i];
                prev_kw = matches!(w, b"return" | b"typeof" | b"instanceof" | b"in" | b"of" | b"new" | b"delete" | b"void" | b"throw" | b"case" | b"do" | b"else" | b"yield" | b"await");
                prev = b'a';
            }
            _ => {
                prev = c;
                prev_kw = false;
                i += 1;
            }
        }
    }
    false
}

/// Process queued JS work: scripts, events (with default actions), fetches,
/// timers — for about `PUMP_BUDGET`; returns whether work is left over.
pub fn pump(js: &mut Option<crate::js::JsRuntime>) -> bool {
    let start = Instant::now();
    for _round in 0..32 {
        let mut tasks = with_page(|p| std::mem::take(&mut p.js_tasks)).unwrap_or_default();
        // Until DOMContentLoaded, the parser's scripts go before events (as
        // when a parser runs them as it reaches them).
        if !with_page(|p| p.dcl_fired).unwrap_or(true) && tasks.iter().any(|t| matches!(t, JsTask::RunScripts)) {
            let (mut first, rest): (Vec<JsTask>, Vec<JsTask>) = tasks.into_iter().partition(|t| matches!(t, JsTask::RunScripts));
            first.truncate(1);
            first.extend(rest);
            tasks = first;
        }
        let fetches = with_page(|p| std::mem::take(&mut p.fetch_results)).unwrap_or_default();
        let wevents = with_page(|p| std::mem::take(&mut p.worker_events)).unwrap_or_default();
        if tasks.is_empty() && fetches.is_empty() && wevents.is_empty() {
            break;
        }
        if let Some(j) = js.as_mut() {
            for (id, ev) in wevents {
                j.worker_event(id, ev);
            }
        }
        let mut tasks = tasks.into_iter();
        while let Some(t) = tasks.next() {
            host::set_timeline_frozen(Some(host::page_now_ms()));
            if start.elapsed() > *PUMP_BUDGET {
                host::set_timeline_frozen(None);
                // Out of time: put the rest back in front of anything queued meanwhile.
                let mut rest: Vec<JsTask> = std::iter::once(t).chain(tasks).collect();
                with_page(|p| {
                    // A script runner that yielded stays in front.
                    let mut queued = std::mem::take(&mut p.js_tasks);
                    if !p.dcl_fired && queued.iter().any(|q| matches!(q, JsTask::RunScripts)) {
                        queued.retain(|q| !matches!(q, JsTask::RunScripts));
                        rest.retain(|q| !matches!(q, JsTask::RunScripts));
                        rest.insert(0, JsTask::RunScripts);
                    }
                    rest.append(&mut queued);
                    p.js_tasks = rest;
                    let mut f = fetches;
                    f.append(&mut p.fetch_results);
                    p.fetch_results = f;
                });
                return true;
            }
            match t {
                JsTask::RunScripts => run_scripts(js),
                JsTask::Dispatch { target, ty, init, then } => {
                    if ty == "__js_url" {
                        if let Some(j) = js.as_mut() {
                            j.eval_url(&init.key);
                        }
                        continue;
                    }
                    let td = Instant::now();
                    let _tp = ProfGuard("js dispatch", td);
                    let prevented = match js.as_mut() {
                        Some(j) if with_page(|p| p.doc.valid(target)).unwrap_or(false) => j.dispatch(target, &ty, &init),
                        _ => false,
                    };
                    if td.elapsed() > Duration::from_millis(300) && std::env::var_os("OHB_SLOW_LISTENERS").is_some() {
                        let desc = with_page(|p| format!("<{} id={:?} src={:?}>", p.doc.tag(target), p.doc.attr(target, "id"), p.doc.attr(target, "src").map(|s| s.chars().take(80).collect::<String>()))).unwrap_or_default();
                        eprintln!("[slow dispatch] {} on {} took {:?}", ty, desc, td.elapsed());
                    }
                    // A key press: keypress (for keys that produce a character, when
                    // keydown wasn't canceled) before the default action, and keyup
                    // after whatever the default action queued (beforeinput, input).
                    let key_info = if let DefaultAction::Key { key, mods } = &then { Some((key.clone(), *mods)) } else { None };
                    let mut prevented = prevented;
                    if let (Some((key, mods)), false) = (&key_info, prevented) {
                        let printable = matches!(key, crate::page::Key::Enter | crate::page::Key::Space) || matches!(key, crate::page::Key::Char(c) if c.chars().count() == 1);
                        if printable && !mods.ctrl && !mods.meta && !mods.alt {
                            if let Some(j) = js.as_mut() {
                                if with_page(|p| p.doc.valid(target)).unwrap_or(false) {
                                    let mut ki = init.clone();
                                    ki.code = init.code.clone();
                                    prevented = j.dispatch(target, "keypress", &ki);
                                }
                            }
                        }
                    }
                    let keyup = key_info.as_ref().map(|(_, mods)| EventInit { key: init.key.clone(), code: init.code.clone(), mods: *mods, bubbles: true, ..Default::default() });
                    if let DefaultAction::Activate { target, prev } = then {
                        with_page(|p| p.finish_activation(target, prev, prevented));
                        with_page(|p| p.user_gesture = false);
                        continue;
                    }
                    if !prevented {
                        match then {
                            DefaultAction::None | DefaultAction::Activate { .. } => {}
                            DefaultAction::Click { target, mods, button } => {
                                with_page(|p| p.default_click(target, mods, button));
                            }
                            DefaultAction::Submit { form, submitter } => {
                                // Script builds the entry list (formdata event), then submits natively.
                                match js.as_mut() {
                                    Some(j) => {
                                        let sub = submitter.map(|s| HostValue::from(s as f64)).unwrap_or(HostValue::Null);
                                        j.call_global("__ohb_submitForm", &[HostValue::from(form as f64), sub]);
                                        j.flush_jobs();
                                    }
                                    None => {
                                        with_page(|p| p.submit_form(form, submitter, None));
                                    }
                                }
                            }
                            DefaultAction::Key { key, mods } => {
                                let consumed = with_page(|p| p.default_key(key.clone(), mods)).unwrap_or(false);
                                if !consumed {
                                    // Not handled by the page: let the UI scroll.
                                    with_page(|p| {
                                        let (_, vh) = p.viewport();
                                        let dy = match key {
                                            crate::page::Key::Down => 40.0,
                                            crate::page::Key::Up => -40.0,
                                            crate::page::Key::PageDown | crate::page::Key::Space => vh * 0.85 * if mods.shift { -1.0 } else { 1.0 },
                                            crate::page::Key::PageUp => -vh * 0.85,
                                            crate::page::Key::Home => -1e9,
                                            crate::page::Key::End => 1e9,
                                            _ => 0.0,
                                        };
                                        if dy != 0.0 {
                                            p.emit(crate::page::PageEvent::ScrollTo(f32::NAN, dy));
                                        }
                                    });
                                }
                            }
                            DefaultAction::Text(t) => {
                                with_page(|p| p.default_text(&t));
                            }
                            DefaultAction::Wheel { x, y, dx, dy } => {
                                with_page(|p| p.default_wheel(x, y, dx, dy));
                            }
                            DefaultAction::ClickEvent { target, mods } => {
                                with_page(|p| p.synthetic_click(target, mods));
                            }
                            DefaultAction::Focus { node, x } => {
                                with_page(|p| p.mouse_focus(node, x));
                            }
                        }
                    }
                    if let Some(up) = keyup {
                        // (A key still held in the UI gets its keyup when released.)
                        with_page(|p| {
                            if !p.held_keys.contains(&crate::page::held_key_id(&up.key)) {
                                let then = p.keyup_default(&up.key, up.mods);
                                p.js_tasks.push(JsTask::Dispatch { target, ty: "keyup".into(), init: up, then });
                            }
                        });
                    }
                    with_page(|p| p.user_gesture = false);
                }
                JsTask::WindowEvent(name) => {
                    if let Some(j) = js.as_mut() {
                        j.window_event(&name);
                    }
                }
                JsTask::AnimEvent { target, ty, name, elapsed, pseudo } => {
                    if let Some(j) = js.as_mut() {
                        if with_page(|p| p.doc.valid(target)).unwrap_or(false) {
                            j.call_global("__ohb_animEvent", &[HostValue::from(target as i32), jstr(&ty), jstr(&name), HostValue::from(elapsed), jstr(&pseudo)]);
                            j.flush_jobs();
                        }
                    }
                }
                JsTask::Resize => {
                    if let Some(j) = js.as_mut() {
                        j.window_event("resize");
                    }
                }
                JsTask::Eval(code) => {
                    let r = match js.as_mut() {
                        Some(j) => j.eval_to_string(&code),
                        None => "ERROR: JavaScript is not running".into(),
                    };
                    with_page(|p| p.emit(crate::page::PageEvent::EvalResult(r)));
                }
                JsTask::Devtools { from, id, method, params } => {
                    let r = match js.as_mut() {
                        Some(j) => match j.call_global("__ohb_devtools", &[jstr(&method), jstr(&params)]) {
                            Some(HostValue::String(s)) => s.to_string(),
                            _ => "{\"error\":\"evaluation failed\"}".into(),
                        },
                        None => "{\"error\":\"JavaScript is not running\"}".into(),
                    };
                    if let Some(j) = js.as_mut() {
                        j.flush_jobs();
                    }
                    crate::page::devtools::send(from, crate::page::devtools::DevMsg::Response { id, result: r });
                }
                JsTask::FrameMessage { frame, data, origin, target_origin, ports } => {
                    if let Some(j) = js.as_mut() {
                        j.call_global("__ohb_frameMessage", &[HostValue::from(frame as i32), data, jstr(&origin), jstr(&target_origin), pid_list(&ports)]);
                        j.flush_jobs();
                    }
                }
                JsTask::ParentMessage { data, origin, target_origin, ports, source } => {
                    if let Some(j) = js.as_mut() {
                        let src = source.map(|s| jstr(&s)).unwrap_or(HostValue::Null);
                        j.call_global("__ohb_parentMessage", &[data, jstr(&origin), jstr(&target_origin), pid_list(&ports), src]);
                        j.flush_jobs();
                    }
                }
                JsTask::PortMessage { pid, data, ports } => {
                    if let Some(j) = js.as_mut() {
                        j.call_global("__ohb_portMessage", &[HostValue::Number(pid as f64), data, pid_list(&ports)]);
                        j.flush_jobs();
                    }
                }
                JsTask::DevtoolsDeliver(msg) => {
                    if let Some(j) = js.as_mut() {
                        j.call_global("__ohb_devtools_client", &[jstr(&msg)]);
                        j.flush_jobs();
                    }
                }
                JsTask::CallGlobal(name, args) => {
                    if let Some(j) = js.as_mut() {
                        j.call_global(name, &args);
                        j.flush_jobs();
                    }
                }
                JsTask::PlannedNav(nav) => {
                    with_page(|p| p.run_form_nav(*nav));
                }
                JsTask::Replay(msg) => {
                    with_page(|p| p.handle(msg));
                }
                JsTask::MouseMove => {
                    let _tm = ProfGuard("js mousemove", Instant::now());
                    if let (Some(Some((target, x, y))), Some(j)) = (with_page(|p| p.take_mouse_move()), js.as_mut()) {
                        let (mx, my) = with_page(|p| std::mem::take(&mut p.move_delta)).unwrap_or_default();
                        let init = crate::page::EventInit { x, y, mx, my, bubbles: true, cancelable: true, ..Default::default() };
                        j.dispatch(target, "pointermove", &init);
                        if with_page(|p| p.doc.valid(target)).unwrap_or(false) {
                            j.dispatch(target, "mousemove", &init);
                        }
                    }
                }
                JsTask::FormAction { form, submitter, reset } => match js.as_mut() {
                    Some(j) => {
                        let f = HostValue::from(form as f64);
                        if reset {
                            j.call_global("__ohb_formReset", &[f]);
                        } else {
                            let sub = HostValue::from(submitter.map(|s| s as f64).unwrap_or(-1.0));
                            j.call_global("__ohb_requestSubmit", &[f, sub]);
                        }
                        j.flush_jobs();
                    }
                    None => {
                        with_page(|p| if reset { p.reset_form(form) } else { p.submit_form(form, submitter, None) });
                    }
                },
            }
        }
        if let Some(j) = js.as_mut() {
            for (id, r) in fetches {
                host::set_timeline_frozen(Some(host::page_now_ms()));
                let _tf = ProfGuard("js fetch/ws", Instant::now());
                j.fetch_done(id, r);
            }
        }
        host::set_timeline_frozen(None);
        if start.elapsed() > *PUMP_BUDGET {
            return true;
        }
    }
    host::set_timeline_frozen(None);
    if let Some(j) = js.as_mut() {
        let _tt = ProfGuard("js timers+raf", Instant::now());
        j.run_timers(start + *PUMP_BUDGET);
        // The selection's live range moves with DOM changes: refresh the page's copy.
        if with_page(|p| p.dom_sel.is_some() && p.sel_gen != p.doc.generation).unwrap_or(false) {
            j.call_global("__ohb_syncSelection", &[]);
        }
    }
    with_page(|p| !p.js_tasks.is_empty() || !p.fetch_results.is_empty()).unwrap_or(false)
}

fn run_scripts(js: &mut Option<JsRuntime>) {
    let started = Instant::now();
    let (enabled, ready, gen) = with_page(|p| (p.js_enabled, p.scripts_ready_to_run(), p.nav_gen)).unwrap_or((false, false, 0));
    if !ready {
        // Re-queue until render-blocking CSS is in.
        with_page(|p| {
            if !p.js_tasks.iter().any(|t| matches!(t, JsTask::RunScripts)) {
                p.js_tasks.push(JsTask::RunScripts);
            }
        });
        return;
    }
    if enabled && (js.is_none() || js.as_ref().map(|j| j.doc_gen() != gen).unwrap_or(false)) {
        // Drop the old document's runtime first and collect its heap, so
        // the two never coexist.
        let had = js.take().is_some();
        if had {
            boa_engine::gc::force_collect();
        }
        *js = JsRuntime::new(gen);
    }
    loop {
        // Pick the next runnable script: classic scripts in order, async when ready,
        // deferred/module scripts after all classic ones.
        let next = with_page(|p| {
            let classic_blocked = p.scripts.iter().any(|s| !s.done && !s.asynchronous && !s.deferred && s.text.is_none() && !s.failed);
            let mut pick = None;
            let first_in_order = p.scripts.iter().position(|s| !s.done && s.in_order);
            for (i, s) in p.scripts.iter().enumerate() {
                if s.done {
                    continue;
                }
                if s.in_order && Some(i) != first_in_order {
                    continue;
                }
                if s.failed {
                    pick = Some(i);
                    break;
                }
                if s.asynchronous && s.text.is_some() {
                    pick = Some(i);
                    break;
                }
                if !s.asynchronous && !s.deferred {
                    if s.text.is_some() {
                        pick = Some(i);
                    }
                    break; // must preserve order
                }
            }
            if pick.is_none() && !classic_blocked {
                let all_classic_done = p.scripts.iter().all(|s| s.done || s.asynchronous || s.deferred);
                if all_classic_done {
                    for (i, s) in p.scripts.iter().enumerate() {
                        if s.done || !s.deferred || s.asynchronous {
                            continue;
                        }
                        if s.text.is_some() || s.failed {
                            pick = Some(i);
                        }
                        break;
                    }
                }
            }
            pick.map(|i| {
                let s = &mut p.scripts[i];
                s.done = true;
                let inert = p.doc.attr(s.node, "data-ohb-inert").is_some();
                let text = if s.failed || inert { None } else { s.text.take() };
                (i, s.node, text, s.url.as_ref().map(|u| u.to_string()).unwrap_or_else(|| "inline".into()), s.module, s.failed, s.importmap)
            })
        })
        .flatten();
        let (_, node, text, name, module, failed, importmap) = match next {
            Some(x) => x,
            None => {
                if std::env::var("OHB_DEBUG_SCRIPTS").is_ok() {
                    with_page(|p| {
                        for s in p.scripts.iter().filter(|s| !s.done) {
                            eprintln!("[scripts] pending {:?} async={} defer={} text={} failed={} module={}", s.url.as_ref().map(|u| u.as_str()), s.asynchronous, s.deferred, s.text.is_some(), s.failed, s.module);
                        }
                    });
                }
                break;
            }
        };
        if let (Some(j), Some(src)) = (js.as_mut(), text) {
            if importmap {
                j.add_import_map(&src);
                continue;
            }
            if module {
                j.run_module(&src, Some(node), &name);
            } else {
                j.run_script(&src, Some(node), &name);
            }
            j.dispatch(node, "load", &EventInit::default());
        } else if failed {
            if let Some(j) = js.as_mut() {
                j.dispatch(node, "error", &EventInit::default());
            }
        }
        // Let the page render and take in network results between scripts.
        if started.elapsed() > *PUMP_BUDGET {
            with_page(|p| p.js_tasks.insert(0, JsTask::RunScripts));
            return;
        }
    }
    with_page(|p| {
        if p.loading && p.dcl_fired && p.scripts.iter().all(|s| s.done) {
            p.update_progress();
        }
    });
    // DOMContentLoaded once all parser-inserted scripts are done.
    let fire_dcl = with_page(|p| {
        let done = p.scripts.iter().all(|s| s.done || s.asynchronous);
        if done && !p.dcl_fired {
            p.dcl_fired = true;
            true
        } else {
            false
        }
    })
    .unwrap_or(false);
    if fire_dcl {
        if let Some(j) = js.as_mut() {
            j.run_content_scripts(crate::ext::RunAt::DocumentEnd);
            j.call_global("__ohb_domContentLoaded", &[]);
            j.flush_jobs();
            j.run_content_scripts(crate::ext::RunAt::DocumentIdle);
        }
        // Autofocus: the first focusable element with `autofocus`, unless
        // something already took focus.
        let af = with_page(|p| {
            if p.doc.focused.is_some() {
                return None;
            }
            let n = p.doc.descendants(DOCUMENT).find(|&n| {
                p.doc.element(n).map(|e| {
                    e.has_attr("autofocus")
                        && !e.has_attr("disabled")
                        && (matches!(e.name.as_str(), "textarea" | "select" | "button") || e.name == "input" && e.attr("type") != Some("hidden") || e.has_attr("tabindex") || e.has_attr("contenteditable"))
                })
                .unwrap_or(false)
            })?;
            p.focus(Some(n));
            Some(n)
        })
        .flatten();
        if let (Some(n), Some(j)) = (af, js.as_mut()) {
            j.dispatch(n, "focus", &EventInit::default());
            j.flush_jobs();
        }
        with_page(|p| {
            let all_done = p.scripts.iter().all(|s| s.done);
            if !p.loading && !p.load_fired && all_done {
                p.load_fired = true;
                p.js_tasks.push(JsTask::WindowEvent("load".into()));
            }
            // Progress may now complete.
            if p.loading && all_done {
                p.update_progress();
            }
        });
    }
}
