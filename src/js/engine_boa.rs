// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! The Boa adapter: implements [`ScriptEngine`] and [`HostArgs`] on top of
//! `boa_engine`. This is the only file (with `modules.rs`) that knows which
//! JavaScript engine runs the page; a V8 adapter would provide the same
//! types.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::builtins::promise::{OperationType, PromiseState};
use boa_engine::context::HostHooks;
use boa_engine::object::builtins::{JsArray, JsArrayBuffer, JsPromise, JsTypedArray};
use boa_engine::object::ObjectInitializer;
use boa_engine::property::{Attribute, PropertyKey};
use boa_engine::{js_string, Context, JsError, JsNativeError, JsObject, JsString, JsValue, NativeFunction, Source};

use super::host::*;
use super::modules::WebModuleLoader;

/// HostPromiseRejectionTracker: remembers promises rejected without a
/// handler so `unhandledrejection` can be fired once the job queue drains.
#[derive(Default)]
struct RejectionTracker {
    pending: RefCell<Vec<JsObject>>,
}

impl HostHooks for RejectionTracker {
    fn promise_rejection_tracker(&self, promise: &JsObject<boa_engine::builtins::promise::Promise>, op: OperationType, _ctx: &mut Context) {
        let obj = promise.clone().upcast();
        let mut p = self.pending.borrow_mut();
        match op {
            OperationType::Reject => {
                if p.len() < 1000 {
                    p.push(obj);
                }
            }
            OperationType::Handle => p.retain(|o| o != &obj),
        }
    }
    /// `Date`'s local time follows the tz database, like `Intl.DateTimeFormat`.
    fn local_timezone_offset_seconds(&self, unix_time_seconds: i64) -> i32 {
        super::intl::local_offset_seconds(unix_time_seconds)
    }
}

/// The job queue: promise reactions in FIFO order, module-loading futures run
/// to completion (the module loader answers synchronously). A run stops after
/// a time budget so a runaway microtask loop can't freeze the page forever.
#[derive(Default)]
struct PageJobs {
    promise: RefCell<std::collections::VecDeque<boa_engine::job::PromiseJob>>,
    asyncs: RefCell<std::collections::VecDeque<boa_engine::job::NativeAsyncJob>>,
    generic: RefCell<std::collections::VecDeque<boa_engine::job::GenericJob>>,
    budget_ms: std::cell::Cell<u64>,
}

impl boa_engine::job::JobExecutor for PageJobs {
    fn enqueue_job(self: Rc<Self>, job: boa_engine::job::Job, _ctx: &mut Context) {
        use boa_engine::job::Job;
        match job {
            Job::PromiseJob(p) => self.promise.borrow_mut().push_back(p),
            Job::AsyncJob(a) => self.asyncs.borrow_mut().push_back(a),
            Job::GenericJob(g) => self.generic.borrow_mut().push_back(g),
            // Timers are the page's own (prelude.js); finalization callbacks are optional.
            _ => {}
        }
    }

    fn run_jobs(self: Rc<Self>, ctx: &mut Context) -> boa_engine::JsResult<()> {
        let start = std::time::Instant::now();
        let budget = std::time::Duration::from_millis(self.budget_ms.get().max(1));
        let mut n: u64 = 0;
        loop {
            let a = self.asyncs.borrow_mut().pop_front();
            if let Some(a) = a {
                let cell = RefCell::new(&mut *ctx);
                if let Err(e) = futures_lite::future::block_on(a.call(&cell)) {
                    if std::env::var("OHB_QUIET").is_err() {
                        eprintln!("[heron-script] async job: {e}");
                    }
                }
                continue;
            }
            let p = self.promise.borrow_mut().pop_front();
            if let Some(p) = p {
                if let Err(e) = p.call(ctx) {
                    if std::env::var("OHB_QUIET").is_err() {
                        eprintln!("[heron-script] promise job: {e}");
                    }
                }
                n += 1;
                if n % 1024 == 0 && start.elapsed() > budget {
                    eprintln!("[heron-script] microtask queue still busy after {n} jobs in {:?}; yielding to the event loop", start.elapsed());
                    break;
                }
                continue;
            }
            let g = self.generic.borrow_mut().pop_front();
            if let Some(g) = g {
                let _ = g.call(ctx);
                continue;
            }
            break;
        }
        ctx.clear_kept_objects();
        Ok(())
    }
}

thread_local! {
    /// Frame realms made on this thread (kept alive until dropped).
    static FRAME_REALMS: RefCell<Vec<boa_engine::realm::Realm>> = const { RefCell::new(Vec::new()) };
}

static SAB_NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Shared buffers posted between threads, by id. (Entries stay for the
/// process's life: a message may be received more than once.)
fn sab_registry() -> &'static std::sync::Mutex<std::collections::HashMap<u64, boa_engine::builtins::array_buffer::SharedArrayBuffer>> {
    static R: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u64, boa_engine::builtins::array_buffer::SharedArrayBuffer>>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default)
}

/// Parsed (not yet scope-analyzed) ASTs of large scripts, per context: the
/// same library loaded into several realms (a page and its same-origin
/// frames: reCAPTCHA's 830 KB script runs in three) is parsed once and the
/// AST cloned and analyzed against each realm's global scope.
#[derive(Default)]
struct AstCache {
    entries: RefCell<Vec<AstEntry>>,
}

struct AstEntry {
    hash: u64,
    len: usize,
    ast: boa_engine::ast::Script,
    source: boa_engine::ast::SourceText,
    /// Realms that already have this parse (a second load in the same realm
    /// parses again, so its template objects are distinct).
    realms: Vec<usize>,
    at: std::time::Instant,
}

/// Parse a classic script for `realm` (None: the current one), reusing a
/// cached parse of the same source made for another realm.
fn parse_script_cached(src: &str, path: &str, realm: Option<boa_engine::realm::Realm>, ctx: &mut Context) -> boa_engine::JsResult<boa_engine::Script> {
    const MIN: usize = 32 * 1024;
    let p = std::path::Path::new(path);
    static OFF: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("OHB_NO_AST_CACHE").is_some());
    if src.len() < MIN || *OFF {
        return boa_engine::Script::parse(Source::from_bytes(src).with_path(p), realm, ctx);
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    ctx.is_strict().hash(&mut h);
    let hash = h.finish();
    let target = realm.clone().unwrap_or_else(|| ctx.realm().clone());
    let rid = target.global_object().as_ref() as *const _ as *const u8 as usize;
    if !ctx.has_data::<AstCache>() {
        ctx.insert_data(AstCache::default());
    }
    let hit = ctx.get_data::<AstCache>().and_then(|c| {
        let mut e = c.entries.borrow_mut();
        let ent = e.iter_mut().find(|x| x.hash == hash && x.len == src.len())?;
        if ent.realms.contains(&rid) {
            return None;
        }
        ent.realms.push(rid);
        Some((ent.ast.clone(), ent.source.clone()))
    });
    if let Some((ast, source)) = hit {
        return boa_engine::Script::from_unanalyzed(ast, source, Some(p.to_path_buf()), Some(target), ctx);
    }
    let (ast, source) = boa_engine::Script::parse_unanalyzed(Source::from_bytes(src).with_path(p), ctx)?;
    if let Some(c) = ctx.get_data::<AstCache>() {
        let mut e = c.entries.borrow_mut();
        e.retain(|x| !(x.hash == hash && x.len == src.len()) && x.at.elapsed() < std::time::Duration::from_secs(60));
        // An AST costs many times its source in memory: keep only scripts
        // parsed for a same-origin frame's realm (a sibling frame is likely to
        // load the same library), a few megabytes of source, for a minute.
        if realm.is_some() {
            e.push(AstEntry { hash, len: src.len(), ast: ast.clone(), source: source.clone(), realms: vec![rid], at: std::time::Instant::now() });
            while e.len() > 4 || e.iter().map(|x| x.len).sum::<usize>() > 3 << 20 {
                e.remove(0);
            }
        }
    }
    boa_engine::Script::from_unanalyzed(ast, source, Some(p.to_path_buf()), Some(target), ctx)
}

fn realm_of(g: &JsObject) -> Option<boa_engine::realm::Realm> {
    FRAME_REALMS.with(|r| r.borrow().iter().find(|x| x.global_object() == g).cloned())
}

pub struct BoaEngine {
    ctx: Context,
    tracker: Rc<RejectionTracker>,
    loader: Rc<WebModuleLoader>,
    jobs: Rc<PageJobs>,
    wasm: super::wasm::WasmHandle,
}

impl Drop for BoaEngine {
    /// A document's engine is going away: let go of every JS value held from
    /// Rust (module map, pending rejections, queued jobs, same-origin frame
    /// realms). Any one of them would keep the whole old realm reachable.
    fn drop(&mut self) {
        self.loader.clear();
        self.tracker.pending.borrow_mut().clear();
        self.jobs.promise.borrow_mut().clear();
        self.jobs.asyncs.borrow_mut().clear();
        self.jobs.generic.borrow_mut().clear();
        FRAME_REALMS.with(|r| r.borrow_mut().clear());
        self.wasm.release();
    }
}

impl BoaEngine {
    pub fn new(base: Option<url::Url>) -> Option<BoaEngine> {
        let tracker = Rc::new(RejectionTracker::default());
        let loader = Rc::new(WebModuleLoader::new());
        *loader.base.borrow_mut() = base;
        let jobs = Rc::new(PageJobs::default());
        jobs.budget_ms.set(std::env::var("OHB_MICROTASK_BUDGET_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(5000));
        let mut ctx = Context::builder().host_hooks(tracker.clone()).module_loader(loader.clone()).job_executor(jobs.clone()).build().ok()?;
        {
            let limits = ctx.runtime_limits_mut();
            // OHB_LOOP_LIMIT=<n> helps find runaway loops (they throw with a stack).
            let lim = std::env::var("OHB_LOOP_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(200_000_000);
            limits.set_loop_iteration_limit(lim);
            limits.set_recursion_limit(10_000);
            limits.set_stack_size_limit(256 * 1024);
        }
        let wasm = super::wasm::install(&mut ctx);
        Some(BoaEngine { ctx, tracker, loader, jobs, wasm })
    }

    /// Workers run on their own threads: no loop limit.
    pub fn unlimited_loops(&mut self) {
        self.ctx.runtime_limits_mut().set_loop_iteration_limit(u64::MAX);
    }

    fn global_fn(&mut self, name: &str) -> Option<JsObject> {
        let g = self.ctx.global_object();
        let f = g.get(JsString::from(name), &mut self.ctx).ok()?;
        f.as_callable()
    }
}

// ------------------------------------------------------------ conversions

/// Engine value -> plain data (bounded depth; functions become undefined).
fn to_host(v: &JsValue, ctx: &mut Context, depth: u32) -> Result<HostValue, JsError> {
    if v.is_undefined() {
        return Ok(HostValue::Undefined);
    }
    if v.is_null() {
        return Ok(HostValue::Null);
    }
    if let Some(b) = v.as_boolean() {
        return Ok(HostValue::Bool(b));
    }
    if let Some(n) = v.as_number() {
        return Ok(HostValue::Number(n));
    }
    if let Some(s) = v.as_string() {
        return Ok(HostValue::String(s.to_std_string_lossy()));
    }
    let Some(o) = v.as_object() else {
        // BigInt, Symbol
        return Ok(HostValue::String(v.to_string(ctx)?.to_std_string_lossy()));
    };
    if o.is_callable() || depth > 8 {
        return Ok(HostValue::Undefined);
    }
    if let Ok(buf) = JsArrayBuffer::from_object(o.clone()) {
        return Ok(HostValue::Bytes(buf.to_vec().unwrap_or_default()));
    }
    if let Ok(ta) = JsTypedArray::from_object(o.clone()) {
        use boa_engine::builtins::typed_array::TypedArrayKind as K;
        let kind = ta.kind();
        if matches!(kind, Some(K::Uint8 | K::Uint8Clamped | K::Int8)) {
            let off = ta.byte_offset(ctx)?;
            let len = ta.byte_length(ctx)?;
            let buf = ta.buffer(ctx)?;
            if let Some(b) = buf.as_object().and_then(|b| JsArrayBuffer::from_object(b.clone()).ok()) {
                let data = b.data();
                let bytes = data.as_deref().map(|d| d.get(off..off + len).unwrap_or(&[]).to_vec()).unwrap_or_default();
                return Ok(HostValue::Bytes(bytes));
            }
        }
        let len = ta.length(ctx)?.min(1 << 24);
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            out.push(HostValue::Number(ta.at(i as i64, ctx)?.to_number(ctx)?));
        }
        return Ok(HostValue::Array(out));
    }
    if o.is_array() {
        let arr = JsArray::from_object(o.clone())?;
        let len = (arr.length(ctx)? as usize).min(1 << 24);
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            out.push(to_host(&arr.get(i as u64, ctx)?, ctx, depth + 1)?);
        }
        return Ok(HostValue::Array(out));
    }
    // Plain object: own enumerable string keys (Object.keys).
    let mut props = Vec::new();
    let object_ctor = ctx.intrinsics().constructors().object().constructor();
    let keys_fn = object_ctor.get(js_string!("keys"), ctx)?;
    let keys = match keys_fn.as_callable() {
        Some(f) => f.call(&JsValue::undefined(), &[v.clone()], ctx)?,
        None => return Ok(HostValue::Object(props)),
    };
    if let Some(ko) = keys.as_object() {
        let arr = JsArray::from_object(ko.clone())?;
        let len = (arr.length(ctx)? as usize).min(10_000);
        for i in 0..len {
            let k = arr.get(i as u64, ctx)?.to_string(ctx)?;
            let val = o.get(PropertyKey::from(k.clone()), ctx)?;
            props.push((k.to_std_string_lossy(), to_host(&val, ctx, depth + 1)?));
        }
    }
    Ok(HostValue::Object(props))
}

/// Plain data -> engine value.
fn from_host(v: &HostValue, ctx: &mut Context) -> JsValue {
    match v {
        HostValue::Undefined => JsValue::undefined(),
        HostValue::Null => JsValue::null(),
        HostValue::Bool(b) => JsValue::from(*b),
        HostValue::Number(n) => JsValue::from(*n),
        HostValue::String(s) => JsValue::from(JsString::from(s.as_str())),
        HostValue::Array(items) => {
            let vals: Vec<JsValue> = items.iter().map(|x| from_host(x, ctx)).collect();
            JsArray::from_iter(vals, ctx).into()
        }
        HostValue::Object(props) => {
            let vals: Vec<(String, JsValue)> = props.iter().map(|(k, x)| (k.clone(), from_host(x, ctx))).collect();
            let mut init = ObjectInitializer::new(ctx);
            for (k, x) in vals {
                init.property(JsString::from(k.as_str()), x, Attribute::all());
            }
            init.build().into()
        }
        HostValue::Bytes(b) => {
            let block = boa_engine::object::builtins::AlignedVec::from_iter(0, b.iter().copied());
            match JsArrayBuffer::from_byte_block(block, ctx) {
                Ok(buf) => buf.into(),
                Err(_) => JsValue::null(),
            }
        }
    }
}

fn host_error(e: HostError) -> JsError {
    let n = match e.kind {
        HostErrorKind::TypeError => JsNativeError::typ(),
        HostErrorKind::RangeError => JsNativeError::range(),
        HostErrorKind::SyntaxError => JsNativeError::syntax(),
        HostErrorKind::Error => JsNativeError::error(),
    };
    n.with_message(e.message).into()
}

struct BoaArgs<'a> {
    args: &'a [JsValue],
    ctx: &'a mut Context,
}

impl HostArgs for BoaArgs<'_> {
    fn len(&self) -> usize {
        self.args.len()
    }
    fn number(&mut self, i: usize) -> f64 {
        match self.args.get(i) {
            Some(v) if !v.is_undefined() && !v.is_null() => v.to_number(self.ctx).unwrap_or(f64::NAN),
            _ => f64::NAN,
        }
    }
    fn string(&mut self, i: usize) -> Option<String> {
        match self.args.get(i) {
            Some(v) if !v.is_undefined() && !v.is_null() => Some(v.to_string(self.ctx).map(|s| s.to_std_string_lossy()).unwrap_or_default()),
            _ => None,
        }
    }
    fn boolean_or(&self, i: usize, default: bool) -> bool {
        self.args.get(i).map(|v| v.to_boolean()).unwrap_or(default)
    }
    fn is_object(&self, i: usize) -> bool {
        self.args.get(i).map(|v| v.is_object()).unwrap_or(false)
    }
    fn value(&mut self, i: usize) -> Result<HostValue, HostError> {
        match self.args.get(i) {
            Some(v) => to_host(v, self.ctx, 0).map_err(|e| HostError::type_error(e.to_string())),
            None => Ok(HostValue::Undefined),
        }
    }
}

fn error_string(e: &JsError, ctx: &mut Context) -> String {
    // A thrown object: report its name, message and stack.
    if let Some(v) = e.as_opaque() {
        if let Some(o) = v.as_object() {
            let mut get = |k: &str| o.get(JsString::from(k), ctx).ok().filter(|v| !v.is_undefined()).and_then(|v| v.to_string(ctx).ok()).map(|s| s.to_std_string_lossy());
            let name = get("name");
            let msg = get("message");
            let stack = get("stack");
            if name.is_some() || msg.is_some() {
                let mut out = format!("{}: {}", name.unwrap_or_else(|| "Error".into()), msg.unwrap_or_default());
                if let Some(st) = stack {
                    out.push_str(" | ");
                    out.push_str(&st.chars().take(400).collect::<String>());
                }
                return out;
            }
        }
    }
    e.to_string()
}

impl ScriptEngine for BoaEngine {
    fn install_natives(&mut self, natives: &[(&'static str, NativeFn, u32)]) {
        let mut o = ObjectInitializer::new(&mut self.ctx);
        for &(name, f, len) in natives {
            let fun = NativeFunction::from_copy_closure(move |_this, args, ctx| {
                crate::js::CURRENT_NATIVE.with(|c| c.set(name));
                if crate::js::TRACE_NATIVES.with(|t| t.get()) {
                    eprintln!("[native] {name}");
                }
                let mut a = BoaArgs { args, ctx };
                let t0 = crate::js::native_prof_on().then(std::time::Instant::now);
                let r = match f(&mut a) {
                    Ok(v) => Ok(from_host(&v, a.ctx)),
                    Err(e) => Err(host_error(e)),
                };
                if let Some(t0) = t0 {
                    crate::js::native_prof_add(name, t0.elapsed());
                }
                r
            });
            o.function(fun, JsString::from(name), len as usize);
        }
        let obj = o.build();
        let _ = self.ctx.register_global_property(js_string!("__n"), obj, Attribute::all());
        // Run a classic script now, from inside script (a dynamically inserted
        // inline <script> executes during the insertion that connects it).
        let run = NativeFunction::from_copy_closure(|_this, args, ctx| {
            let src = args.first().map(|v| v.to_string(ctx).map(|s| s.to_std_string_lossy())).transpose()?.unwrap_or_default();
            let path = args.get(1).map(|v| v.to_string(ctx).map(|s| s.to_std_string_lossy())).transpose()?.unwrap_or_else(|| "inline".into());
            let src = src.trim_start_matches('\u{feff}');
            if crate::js::too_deeply_nested(src) {
                return Err(JsNativeError::syntax().with_message("script nested too deeply").into());
            }
            let script = parse_script_cached(src, &path, None, ctx)?;
            script.evaluate(ctx)
        });
        let f = boa_engine::object::FunctionObjectBuilder::new(self.ctx.realm(), run).name(js_string!("__ohb_runScript")).length(2).build();
        let _ = self.ctx.register_global_property(js_string!("__ohb_runScript"), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        // A microtask checkpoint (HTML "clean up after running a callback"):
        // run between the listeners of an event the browser dispatches, so
        // promise reactions and mutation observers see `window.event` as in
        // other browsers. Not re-entrant (a checkpoint inside one is a no-op).
        let microtasks = NativeFunction::from_copy_closure(|_this, _args, ctx| {
            thread_local!(static IN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
            if !IN.with(|i| i.replace(true)) {
                let _ = ctx.run_jobs();
                IN.with(|i| i.set(false));
            }
            Ok(JsValue::undefined())
        });
        let f = boa_engine::object::FunctionObjectBuilder::new(self.ctx.realm(), microtasks).name(js_string!("__ohb_microtasks")).length(0).build();
        let _ = self.ctx.register_global_property(js_string!("__ohb_microtasks"), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        // document.all: an object with [[IsHTMLDDA]] (typeof "undefined",
        // falsy, == null) whose prototype answers property lookups and whose
        // calls go to the given function.
        let make_dda = NativeFunction::from_copy_closure(|_this, args, ctx| {
            let proto = args.first().and_then(|v| v.as_object());
            let call = args.get(1).and_then(|v| v.as_object()).filter(|f| f.is_callable());
            let data = boa_engine::builtins::is_html_dda::IsHTMLDDA { delegate: call };
            let o = boa_engine::JsObject::from_proto_and_data(proto, data);
            let _ = ctx;
            Ok(o.into())
        });
        let f = boa_engine::object::FunctionObjectBuilder::new(self.ctx.realm(), make_dda).name(js_string!("__ohb_makeDDA")).length(2).build();
        let _ = self.ctx.register_global_property(js_string!("__ohb_makeDDA"), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        // Is a value an ordinary object (no internal slots: not a Date, Map,
        // RegExp, boxed primitive, buffer, error, proxy, …)? Structured clone
        // uses it to skip brand checks that throw (and so allocate) for
        // every plain object it copies.
        let is_ordinary = NativeFunction::from_copy_closure(|_this, args, _ctx| {
            let ord = args.first().and_then(|v| v.as_object()).is_some_and(|o| o.is::<boa_engine::builtins::object::OrdinaryObject>());
            Ok(JsValue::from(ord))
        });
        let f = boa_engine::object::FunctionObjectBuilder::new(self.ctx.realm(), is_ordinary).name(js_string!("__ohb_isOrdinary")).length(1).build();
        let _ = self.ctx.register_global_property(js_string!("__ohb_isOrdinary"), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        // Structured clone of plain data (objects with Object.prototype or a
        // null prototype, plain arrays, primitives; cycles and shared
        // references kept) in one native pass. Returns undefined when it
        // meets anything else (the JS serializer then does the full job).
        let fast_clone = NativeFunction::from_copy_closure(|_this, args, ctx| {
            let v = args.first().cloned().unwrap_or_default();
            let mut memo: std::collections::HashMap<usize, JsObject> = std::collections::HashMap::new();
            Ok(plain_clone(&v, ctx, &mut memo, 0)?.unwrap_or_default())
        });
        let f = boa_engine::object::FunctionObjectBuilder::new(self.ctx.realm(), fast_clone).name(js_string!("__ohb_plainClone")).length(1).build();
        let _ = self.ctx.register_global_property(js_string!("__ohb_plainClone"), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        // SharedArrayBuffers across threads (workers of a cross-origin
        // isolated page): share one gives an id; get makes an object over the
        // same memory in this context.
        let sab_share = NativeFunction::from_copy_closure(|_this, args, _ctx| {
            let o = args.first().and_then(|v| v.as_object()).ok_or_else(|| JsNativeError::typ().with_message("not a SharedArrayBuffer"))?;
            let sab = boa_engine::object::builtins::JsSharedArrayBuffer::from_object(o)?;
            let inner = sab.inner();
            let id = SAB_NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            sab_registry().lock().unwrap().insert(id, inner);
            Ok(JsValue::from(id as f64))
        });
        let f = boa_engine::object::FunctionObjectBuilder::new(self.ctx.realm(), sab_share).name(js_string!("__ohb_sabShare")).length(1).build();
        let _ = self.ctx.register_global_property(js_string!("__ohb_sabShare"), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        let sab_get = NativeFunction::from_copy_closure(|_this, args, ctx| {
            let id = args.first().map(|v| v.to_number(ctx)).transpose()?.unwrap_or(-1.0) as u64;
            let inner = sab_registry().lock().unwrap().get(&id).cloned().ok_or_else(|| JsNativeError::typ().with_message("SharedArrayBuffer is gone"))?;
            Ok(boa_engine::object::builtins::JsSharedArrayBuffer::from_buffer(inner, ctx).into())
        });
        let f = boa_engine::object::FunctionObjectBuilder::new(self.ctx.realm(), sab_get).name(js_string!("__ohb_sabGet")).length(1).build();
        let _ = self.ctx.register_global_property(js_string!("__ohb_sabGet"), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        // Realms for same-origin frames on this thread: a new realm's global
        // object, scripts run in a realm (by its global), and dropping one.
        let new_realm = NativeFunction::from_copy_closure(|_this, _args, ctx| {
            let t0 = std::time::Instant::now();
            let realm = ctx.create_realm()?;
            if crate::js::profiling() {
                eprintln!("[profile] frame realm created in {:?}", t0.elapsed());
            }
            let g = realm.global_object().clone();
            FRAME_REALMS.with(|r| r.borrow_mut().push(realm));
            Ok(g.into())
        });
        let f = boa_engine::object::FunctionObjectBuilder::new(self.ctx.realm(), new_realm).name(js_string!("__ohb_newRealm")).length(0).build();
        let _ = self.ctx.register_global_property(js_string!("__ohb_newRealm"), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        let run_in = NativeFunction::from_copy_closure(|_this, args, ctx| {
            let g = args.first().and_then(|v| v.as_object()).ok_or_else(|| JsNativeError::typ().with_message("not a realm"))?;
            let realm = realm_of(&g).ok_or_else(|| JsNativeError::typ().with_message("not a realm"))?;
            let src = args.get(1).map(|v| v.to_string(ctx).map(|s| s.to_std_string_lossy())).transpose()?.unwrap_or_default();
            let path = args.get(2).map(|v| v.to_string(ctx).map(|s| s.to_std_string_lossy())).transpose()?.unwrap_or_else(|| "inline".into());
            let src = src.trim_start_matches('\u{feff}');
            if crate::js::too_deeply_nested(src) {
                return Err(JsNativeError::syntax().with_message("script nested too deeply").into());
            }
            let old = ctx.enter_realm(realm.clone());
            let t0 = std::time::Instant::now();
            let parsed = parse_script_cached(src, &path, Some(realm), ctx);
            let t1 = std::time::Instant::now();
            let r = parsed.and_then(|s| s.evaluate(ctx));
            if crate::js::profiling() {
                eprintln!("[profile] frame script {} ({} KB): parse {:?} run {:?}", path.chars().take(100).collect::<String>(), src.len() / 1024, t1 - t0, t1.elapsed());
            }
            ctx.enter_realm(old);
            r
        });
        let f = boa_engine::object::FunctionObjectBuilder::new(self.ctx.realm(), run_in).name(js_string!("__ohb_runScriptIn")).length(3).build();
        let _ = self.ctx.register_global_property(js_string!("__ohb_runScriptIn"), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        // A module script in a frame's realm: its graph loads, links and
        // evaluates there (the module map is kept per realm).
        let run_module_in = NativeFunction::from_copy_closure(|_this, args, ctx| {
            let g = args.first().and_then(|v| v.as_object()).ok_or_else(|| JsNativeError::typ().with_message("not a realm"))?;
            let realm = realm_of(&g).ok_or_else(|| JsNativeError::typ().with_message("not a realm"))?;
            let src = args.get(1).map(|v| v.to_string(ctx).map(|s| s.to_std_string_lossy())).transpose()?.unwrap_or_default();
            let key = args.get(2).map(|v| v.to_string(ctx).map(|s| s.to_std_string_lossy())).transpose()?.unwrap_or_else(|| "inline".into());
            let src = src.trim_start_matches('\u{feff}');
            if crate::js::too_deeply_nested(src) {
                return Err(JsNativeError::syntax().with_message("module nested too deeply").into());
            }
            let tag = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                g.hash(&mut h);
                h.finish() | 1
            };
            if let Ok(base) = url::Url::parse(&key) {
                super::modules::prefetch_imports(src, &base, Some(base.clone()));
            }
            let old_tag = super::modules::set_realm_tag(tag);
            let old = ctx.enter_realm(realm.clone());
            let r = boa_engine::module::Module::parse(Source::from_bytes(src).with_path(std::path::Path::new(&key)), Some(realm), ctx).map(|m| {
                let p = m.load_link_evaluate(ctx);
                let _ = ctx.run_jobs();
                p
            });
            ctx.enter_realm(old);
            super::modules::set_realm_tag(old_tag);
            match r {
                Ok(p) => match p.state() {
                    PromiseState::Rejected(e) => Err(JsError::from_opaque(e)),
                    _ => Ok(JsValue::undefined()),
                },
                Err(e) => Err(e),
            }
        });
        let f = boa_engine::object::FunctionObjectBuilder::new(self.ctx.realm(), run_module_in).name(js_string!("__ohb_runModuleIn")).length(3).build();
        let _ = self.ctx.register_global_property(js_string!("__ohb_runModuleIn"), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        let drop_realm = NativeFunction::from_copy_closure(|_this, args, _ctx| {
            if let Some(g) = args.first().and_then(|v| v.as_object()) {
                FRAME_REALMS.with(|r| r.borrow_mut().retain(|x| x.global_object() != &g));
            }
            Ok(JsValue::undefined())
        });
        let f = boa_engine::object::FunctionObjectBuilder::new(self.ctx.realm(), drop_realm).name(js_string!("__ohb_dropRealm")).length(1).build();
        let _ = self.ctx.register_global_property(js_string!("__ohb_dropRealm"), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        // Static NodeList indices: define o[i] = arr[i] (enumerable,
        // configurable, read-only) for a fresh ordinary object in one call.
        let def_indexed = NativeFunction::from_copy_closure(|_this, args, ctx| {
            let o = args.first().and_then(|v| v.as_object()).ok_or_else(|| JsNativeError::typ().with_message("not an object"))?;
            let arr = args.get(1).and_then(|v| v.as_object()).ok_or_else(|| JsNativeError::typ().with_message("not an array"))?;
            let len = arr.get(js_string!("length"), ctx)?.to_length(ctx)?;
            for i in 0..len {
                let v = arr.get(i, ctx)?;
                o.insert_property(
                    i,
                    boa_engine::property::PropertyDescriptor::builder().value(v).enumerable(true).configurable(true).writable(false),
                );
            }
            Ok(JsValue::undefined())
        });
        let f = boa_engine::object::FunctionObjectBuilder::new(self.ctx.realm(), def_indexed).name(js_string!("__ohb_defineIndexed")).length(2).build();
        let _ = self.ctx.register_global_property(js_string!("__ohb_defineIndexed"), f, Attribute::WRITABLE | Attribute::CONFIGURABLE);
    }

    fn run_classic(&mut self, src: &str, path: &str) -> Result<HostValue, String> {
        let src = src.trim_start_matches('\u{feff}');
        if crate::js::too_deeply_nested(src) {
            return Err(format!("SyntaxError: {path}: script nested too deeply"));
        }
        let t0 = std::time::Instant::now();
        let builtin = path.starts_with("ohb://") && path != "ohb://inject.js";
        if builtin {
            boa_engine::set_builtin_source(true);
        }
        let parsed = parse_script_cached(src, path, None, &mut self.ctx);
        let t_parse = t0.elapsed();
        let r = parsed.and_then(|script| script.evaluate(&mut self.ctx));
        if builtin {
            boa_engine::set_builtin_source(false);
        }
        // Parsing a big script makes a large, short-lived AST: give the
        // memory back rather than keeping it in the allocator's free lists.
        if src.len() > 256 * 1024 {
            crate::release_free_memory();
        }
        if path.starts_with("ohb://") && std::env::var_os("OHB_PROFILE_INIT").is_some() {
            crate::release_free_memory();
            eprintln!("[profile] {path}: parse {:?} run {:?} rss {}MB heap {}MB gc {}MB", t_parse, t0.elapsed() - t_parse, crate::sysmem::rss_bytes().unwrap_or(0) >> 20, crate::heap_in_use() >> 20, boa_engine::gc::gc_stats().2 >> 20);
            eprintln!("[profile]   code blocks [n, bytecode, consts, ic, bindings, srcmap, handlers] = {:?}", boa_engine::code_block_stats());
        }
        match r {
            // A script's completion value is only ever used when primitive;
            // converting an object would walk (and run getters across) its
            // whole graph.
            Ok(v) if v.is_object() => Ok(HostValue::Undefined),
            Ok(v) => Ok(to_host(&v, &mut self.ctx, 0).unwrap_or_default()),
            Err(e) => Err(error_string(&e, &mut self.ctx)),
        }
    }

    fn run_module(&mut self, src: &str, key: &str) -> Result<(), String> {
        let src = src.trim_start_matches('\u{feff}');
        if crate::js::too_deeply_nested(src) {
            return Err(format!("SyntaxError: {key}: module nested too deeply"));
        }
        let parsed = boa_engine::module::Module::parse(Source::from_bytes(src).with_path(std::path::Path::new(key)), None, &mut self.ctx);
        match parsed {
            Ok(m) => {
                let p = m.load_link_evaluate(&mut self.ctx);
                let _ = self.ctx.run_jobs();
                match p.state() {
                    PromiseState::Rejected(e) => {
                        let err = JsError::from_opaque(e);
                        Err(error_string(&err, &mut self.ctx))
                    }
                    _ => Ok(()),
                }
            }
            Err(e) => Err(error_string(&e, &mut self.ctx)),
        }
    }

    fn call_global(&mut self, name: &str, args: &[HostValue]) -> Result<HostValue, String> {
        let f = self.global_fn(name).ok_or_else(|| format!("{name} is not a function"))?;
        let jargs: Vec<JsValue> = args.iter().map(|a| from_host(a, &mut self.ctx)).collect();
        match f.call(&JsValue::undefined(), &jargs, &mut self.ctx) {
            Ok(v) => Ok(to_host(&v, &mut self.ctx, 0).unwrap_or_default()),
            Err(e) => Err(error_string(&e, &mut self.ctx)),
        }
    }

    fn eval_to_string(&mut self, code: &str) -> String {
        match self.ctx.eval(Source::from_bytes(code)) {
            Ok(v) => {
                if let Some(s) = v.as_string() {
                    return s.to_std_string_escaped();
                }
                let g = self.ctx.global_object();
                let json = g.get(js_string!("JSON"), &mut self.ctx).ok().and_then(|j| j.as_object());
                let stringify = json.as_ref().and_then(|j| j.get(js_string!("stringify"), &mut self.ctx).ok());
                match stringify.as_ref().and_then(|f| f.as_callable()) {
                    Some(f) => match f.call(&JsValue::undefined(), &[v.clone(), JsValue::null(), JsValue::from(1)], &mut self.ctx) {
                        Ok(s) if !s.is_undefined() => s.to_string(&mut self.ctx).map(|s| s.to_std_string_escaped()).unwrap_or_default(),
                        _ => v.display().to_string(),
                    },
                    None => v.display().to_string(),
                }
            }
            Err(e) => format!("ERROR: {e}"),
        }
    }

    fn run_jobs(&mut self) {
        let _ = self.ctx.run_jobs();
    }

    fn report_unhandled_rejections(&mut self, handler: &str) {
        for _ in 0..4 {
            let _ = self.ctx.run_jobs();
            let pending = std::mem::take(&mut *self.tracker.pending.borrow_mut());
            if pending.is_empty() {
                return;
            }
            let Some(f) = self.global_fn(handler) else { return };
            for p in pending {
                let reason = match JsPromise::from_object(p.clone()).map(|jp| jp.state()) {
                    Ok(PromiseState::Rejected(v)) => v,
                    _ => continue,
                };
                if let Err(e) = f.call(&JsValue::undefined(), &[p.into(), reason], &mut self.ctx) {
                    if std::env::var("OHB_QUIET").is_err() {
                        eprintln!("[heron-script] {handler}: {e}");
                    }
                }
            }
        }
    }

    fn set_base_url(&mut self, url: Option<url::Url>) {
        if url.is_some() {
            *self.loader.base.borrow_mut() = url;
        }
    }

    fn add_import_map(&mut self, json: &str) {
        let base = self.loader.base.borrow().clone();
        if let Some(b) = base {
            self.loader.import_map.borrow_mut().merge(json, &b);
        }
    }
}


/// See `__ohb_plainClone`: Ok(None) means "not plain data, use the full
/// structured clone".
fn plain_clone(v: &JsValue, ctx: &mut Context, memo: &mut std::collections::HashMap<usize, JsObject>, depth: u32) -> boa_engine::JsResult<Option<JsValue>> {
    let Some(o) = v.as_object() else {
        // Symbols can't be cloned (the full path throws the right error).
        return Ok(if v.is_symbol() { None } else { Some(v.clone()) });
    };
    if depth > 1000 {
        return Ok(None);
    }
    let addr = o.as_ref() as *const _ as *const u8 as usize;
    if let Some(c) = memo.get(&addr) {
        return Ok(Some(c.clone().into()));
    }
    let obj_proto = ctx.intrinsics().constructors().object().prototype();
    let arr_proto = ctx.intrinsics().constructors().array().prototype();
    let proto = o.prototype();
    let is_arr = o.is_array();
    if is_arr {
        if !proto.as_ref().is_some_and(|p| JsObject::equals(p, &arr_proto)) {
            return Ok(None);
        }
    } else if !o.is::<boa_engine::builtins::object::OrdinaryObject>() || !proto.as_ref().is_none_or(|p| JsObject::equals(p, &obj_proto)) {
        return Ok(None);
    }
    let keys = o.own_property_keys(ctx)?;
    let out: JsObject = if is_arr { JsArray::new(ctx)?.into() } else { JsObject::with_object_proto(ctx.intrinsics()) };
    memo.insert(addr, out.clone());
    let mut len: Option<JsValue> = None;
    for k in keys {
        if matches!(k, PropertyKey::Symbol(_)) {
            continue;
        }
        let desc = o.borrow().properties().get(&k);
        let Some(desc) = desc else { continue };
        if desc.is_accessor_descriptor() {
            return Ok(None);
        }
        if is_arr && matches!(&k, PropertyKey::String(s) if s == &js_string!("length")) {
            len = desc.value().cloned();
            continue;
        }
        if !desc.enumerable().unwrap_or(false) {
            continue;
        }
        let val = desc.value().cloned().unwrap_or_default();
        let Some(c) = plain_clone(&val, ctx, memo, depth + 1)? else { return Ok(None) };
        out.create_data_property_or_throw(k, c, ctx)?;
    }
    if let Some(l) = len {
        out.set(js_string!("length"), l, true, ctx)?;
    }
    Ok(Some(out.into()))
}
