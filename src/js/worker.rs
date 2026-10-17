// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Dedicated workers: each `new Worker(url)` gets its own thread with its
//! own script engine, the shared prelude (the parts that make sense without a
//! document) and `worker.js`. Messages cross threads as structured-cloned
//! plain data (`HostValue`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::time::Duration;

use url::Url;

use super::host::*;
use crate::page::{NetMsg, PageMsg};

const WORKER_SHIM: &str = include_str!("worker.js");

pub enum ToWorker {
    Message(HostValue, Vec<u64>),
    /// A message on one of the worker's MessagePorts.
    Port(u64, HostValue, Vec<u64>),
    Terminate,
}

#[derive(Debug)]
pub enum WorkerEvent {
    /// A message with transferred MessagePorts.
    Message(HostValue, Vec<u64>),
    Error(String),
}

/// Workers as owners of transferable MessagePorts: owner id -> channel.
fn worker_routes() -> &'static std::sync::Mutex<HashMap<u64, Sender<ToWorker>>> {
    static R: std::sync::OnceLock<std::sync::Mutex<HashMap<u64, Sender<ToWorker>>>> = std::sync::OnceLock::new();
    R.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Deliver a port message if `owner` is a worker.
pub fn route_port(owner: u64, pid: u64, data: HostValue, ports: Vec<u64>) -> bool {
    let tx = worker_routes().lock().unwrap().get(&owner).cloned();
    match tx {
        Some(tx) => tx.send(ToWorker::Port(pid, data, ports)).is_ok(),
        None => false,
    }
}

fn next_owner_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 52);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

thread_local! {
    /// Page side: live workers by id.
    static WORKERS: RefCell<HashMap<u32, (Sender<ToWorker>, u64)>> = RefCell::new(HashMap::new());
    static NEXT_ID: Cell<u32> = const { Cell::new(1) };
    /// Worker side: this thread's identity and its way back to the page.
    static SELF: RefCell<Option<WorkerSelf>> = const { RefCell::new(None) };
}

struct WorkerSelf {
    id: u32,
    gen: u64,
    url: Url,
    page: Sender<PageMsg>,
    /// The page's owner id (for transferred ports).
    page_owner: u64,
    closing: bool,
}

/// Start a worker running the script at `url`. Returns its id.
pub fn start(url: Url, module: bool, page: Sender<PageMsg>, gen: u64, doc_url: Url, name: String, shared: bool) -> u32 {
    let id = NEXT_ID.with(|n| {
        let v = n.get();
        n.set(v + 1);
        v
    });
    let (tx, rx) = channel::<ToWorker>();
    let owner = next_owner_id();
    let page_owner = crate::page::frames::thread_owner();
    worker_routes().lock().unwrap().insert(owner, tx.clone());
    // A blob: script URL is read now (pages often revoke it right after).
    let blob_src = if url.scheme() == "blob" { crate::net::blob_snapshot(url.as_str()).map(|(b, _)| String::from_utf8_lossy(&b).into_owned()) } else { None };
    let spawned = std::thread::Builder::new().name(format!("ohb-worker-{id}")).stack_size(256 << 20).spawn(move || {
        crate::page::frames::set_thread_owner(owner);
        SELF.with(|s| *s.borrow_mut() = Some(WorkerSelf { id, gen, url: url.clone(), page: page.clone(), page_owner, closing: false }));
        let send = |ev: WorkerEvent| {
            if std::env::var_os("OHB_DEBUG_WORKERS").is_some() {
                eprintln!("[worker {id}] -> {}", format!("{ev:?}").chars().take(300).collect::<String>());
            }
            let _ = page.send(PageMsg::Net(gen, NetMsg::Worker(id, ev)));
        };
        let Some(mut engine) = super::engine_boa::BoaEngine::new(Some(url.clone())) else { return };
        engine.unlimited_loops();
        let mut natives: Vec<(&'static str, NativeFn, u32)> = super::natives::all();
        natives.extend_from_slice(WORKER_NATIVES);
        engine.install_natives(&natives);
        let boot = format!(
            "globalThis.__ohb_workerURL = {}; globalThis.__ohb_workerName = {}; globalThis.__ohb_sharedWorker = {}; globalThis.__n_worker = globalThis.__n;",
            json_string(url.as_str()),
            json_string(&name),
            shared
        );
        let _ = engine.run_classic(&boot, "ohb://worker-boot.js");
        let _ = engine.run_classic(super::INTL, "ohb://intl.js");
        if let Err(e) = engine.run_classic(super::prelude_source(), "ohb://prelude.js") {
            send(WorkerEvent::Error(format!("worker prelude failed: {e}")));
            return;
        }
        if let Err(e) = engine.run_classic(WORKER_SHIM, "ohb://worker.js") {
            send(WorkerEvent::Error(format!("worker shim failed: {e}")));
            return;
        }
        let _ = engine.run_classic(super::WEBCRYPTO, "ohb://webcrypto.js");
        let _ = engine.run_classic(super::HIDE_NATIVES, "ohb://init.js");
        let fetched = match blob_src {
            Some(s) => Ok(s),
            None => super::modules::fetch_text(&url, Some(doc_url)),
        };
        let src = match fetched {
            Ok(s) => s,
            Err(e) => {
                send(WorkerEvent::Error(format!("Failed to load worker script {url}: {e}")));
                return;
            }
        };
        let r = if module { engine.run_module(&src, url.as_str()) } else { engine.run_classic(&src, url.as_str()).map(|_| ()) };
        if let Err(e) = r {
            send(WorkerEvent::Error(e));
        }
        engine.report_unhandled_rejections("__ohb_unhandledRejection");
        loop {
            // Timers, then wait for the next message or timer.
            let mut next = -1.0;
            for _ in 0..1000 {
                next = engine.call_global("__ohb_runTimers", &[HostValue::from(page_now_ms())]).map(|v| v.number()).unwrap_or(-1.0);
                engine.report_unhandled_rejections("__ohb_unhandledRejection");
                if next != -2.0 {
                    break;
                }
            }
            if SELF.with(|s| s.borrow().as_ref().map(|w| w.closing).unwrap_or(true)) {
                break;
            }
            let wait = if next >= 0.0 { Duration::from_secs_f64(((next - page_now_ms()).max(0.0)) / 1000.0) } else { Duration::from_secs(3600) };
            match rx.recv_timeout(wait) {
                Ok(ToWorker::Message(v, ports)) => {
                    let ids = HostValue::Array(ports.iter().map(|&x| HostValue::Number(x as f64)).collect());
                    if let Err(e) = engine.call_global("__ohb_workerOnMessage", &[v, ids]) {
                        send(WorkerEvent::Error(e));
                    }
                    engine.report_unhandled_rejections("__ohb_unhandledRejection");
                }
                Ok(ToWorker::Port(pid, v, ports)) => {
                    let ids = HostValue::Array(ports.iter().map(|&x| HostValue::Number(x as f64)).collect());
                    let _ = engine.call_global("__ohb_portMessage", &[HostValue::Number(pid as f64), v, ids]);
                    engine.report_unhandled_rejections("__ohb_unhandledRejection");
                }
                Ok(ToWorker::Terminate) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    });
    if spawned.is_err() {
        worker_routes().lock().unwrap().remove(&owner);
        return 0;
    }
    WORKERS.with(|w| w.borrow_mut().insert(id, (tx, owner)));
    id
}

pub fn post(id: u32, v: HostValue, ports: Vec<u64>) {
    if std::env::var_os("OHB_DEBUG_WORKERS").is_some() {
        eprintln!("[worker {id}] <- {}", format!("{v:?}").chars().take(300).collect::<String>());
    }
    WORKERS.with(|w| {
        if let Some((tx, owner)) = w.borrow().get(&id) {
            crate::page::frames::port_transfer(&ports, *owner);
            let _ = tx.send(ToWorker::Message(v, ports));
        }
    });
}

pub fn terminate(id: u32) {
    WORKERS.with(|w| {
        if let Some((tx, owner)) = w.borrow_mut().remove(&id) {
            worker_routes().lock().unwrap().remove(&owner);
            let _ = tx.send(ToWorker::Terminate);
        }
    });
}

/// Stop every worker of the page (navigation).
pub fn terminate_all() {
    WORKERS.with(|w| {
        for (_, (tx, owner)) in w.borrow_mut().drain() {
            worker_routes().lock().unwrap().remove(&owner);
            let _ = tx.send(ToWorker::Terminate);
        }
    });
}

fn json_string(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ------------------------------------------------------------ worker natives

fn w_post(a: &mut dyn HostArgs) -> HostResult {
    let v = a.value(0)?;
    let ports: Vec<u64> = match a.value(1) {
        Ok(HostValue::Array(l)) => l.into_iter().filter_map(|x| if let HostValue::Number(n) = x { Some(n as u64) } else { None }).collect(),
        _ => Vec::new(),
    };
    SELF.with(|s| {
        if let Some(w) = s.borrow().as_ref() {
            crate::page::frames::port_transfer(&ports, w.page_owner);
            if std::env::var_os("OHB_DEBUG_WORKERS").is_some() {
                eprintln!("[worker {}] -> {}", w.id, format!("{v:?}").chars().take(300).collect::<String>());
            }
            let _ = w.page.send(PageMsg::Net(w.gen, NetMsg::Worker(w.id, WorkerEvent::Message(v, ports))));
        }
    });
    Ok(HostValue::Undefined)
}

fn w_error(a: &mut dyn HostArgs) -> HostResult {
    let m = st(a, 0);
    SELF.with(|s| {
        if let Some(w) = s.borrow().as_ref() {
            let _ = w.page.send(PageMsg::Net(w.gen, NetMsg::Worker(w.id, WorkerEvent::Error(m))));
        }
    });
    Ok(HostValue::Undefined)
}

fn w_close(_a: &mut dyn HostArgs) -> HostResult {
    SELF.with(|s| {
        if let Some(w) = s.borrow_mut().as_mut() {
            w.closing = true;
        }
    });
    Ok(HostValue::Undefined)
}

/// Blocking fetch for worker scripts (`importScripts`, `fetch` and XHR in workers):
/// (url, method, headers[flat], body, credentials, mode, redirect, cache, forcePreflight)
///   -> [status, statusText, url, headers[flat], body bytes, type, redirected].
fn w_fetch_sync(a: &mut dyn HostArgs) -> HostResult {
    let u = st(a, 0);
    let method = st(a, 1).to_ascii_uppercase();
    let headers = a.value(2)?;
    let body = match a.value(3)? {
        HostValue::Bytes(b) => Some(b),
        v if v.is_nullish() => None,
        v => Some(v.string().into_bytes()),
    };
    let opt = |a: &mut dyn HostArgs, i: usize| a.value(i).ok().filter(|v| !v.is_nullish()).map(|v| v.string()).unwrap_or_default();
    let cred = opt(a, 4);
    let mode = opt(a, 5);
    let redirect = opt(a, 6);
    let cache = opt(a, 7);
    let force_preflight = a.value(8).map(|v| v.truthy()).unwrap_or(false);
    let referrer = a.value(9).ok().filter(|v| !v.is_nullish()).map(|v| v.string()).unwrap_or_else(|| "client".into());
    let referrer_policy = opt(a, 10);
    let base = SELF.with(|s| s.borrow().as_ref().map(|w| w.url.clone()));
    let url = match base.as_ref().and_then(|b| b.join(&u).ok()).or_else(|| Url::parse(&u).ok()) {
        Some(u) => u,
        None => return Err(HostError::type_error(format!("Failed to fetch: invalid URL {u}"))),
    };
    if url.scheme() == "file" && base.as_ref().map(|b| b.scheme() != "file").unwrap_or(true) {
        return Err(HostError::type_error("Failed to fetch: file access is not allowed"));
    }
    let client = base.clone().unwrap_or_else(|| url.clone());
    let mut req = crate::net::Request::get(url, crate::net::RequestKind::Fetch, base);
    if !method.is_empty() {
        req.method = method;
    }
    if let Some(h) = headers.as_array() {
        for kv in h.chunks(2) {
            if kv.len() == 2 {
                req.headers.push((kv[0].string(), kv[1].string()));
            }
        }
    }
    req.body = body;
    // importScripts (no mode given) loads like a classic script: no CORS.
    let mode = if mode.is_empty() { "media".to_string() } else { mode };
    let mut opts = crate::net::cors::Opts::parse(client, &mode, &cred, &redirect, &cache);
    opts.force_preflight = force_preflight;
    opts.referrer = referrer;
    opts.referrer_policy = referrer_policy;
    match crate::net::cors::fetch(req, &opts) {
        Ok(o) => {
            let r = o.resp;
            let mut flat = Vec::new();
            for (k, v) in &r.headers {
                flat.push(HostValue::String(k.to_ascii_lowercase()));
                flat.push(HostValue::String(v.clone()));
            }
            Ok(HostValue::Array(vec![
                HostValue::from(r.status as f64),
                HostValue::String(r.status_text.clone()),
                HostValue::String(r.url.to_string()),
                HostValue::Array(flat),
                HostValue::Bytes(r.body.as_ref().clone()),
                HostValue::String(o.rtype.to_string()),
                HostValue::from(o.redirected),
            ]))
        }
        Err(e) => Err(HostError::type_error(format!("Failed to fetch: {e}"))),
    }
}

static WORKER_NATIVES: &[(&str, NativeFn, u32)] = &[("workerPost", w_post, 2), ("workerError", w_error, 1), ("workerClose", w_close, 0), ("fetchSync", w_fetch_sync, 11)];
