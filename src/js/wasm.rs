// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! WebAssembly for web pages: the JS API (`WebAssembly.Module`, `Instance`,
//! `Memory`, `Table`, `Global`, `validate`/`compile`/`instantiate`…) over the
//! wasmi interpreter.
//!
//! The JS classes live in `wasm.js`, loaded the first time a script touches
//! `WebAssembly`; they call the natives on `__ohb_wasm` installed here, which
//! work with numeric handles into per-realm registries. A memory's `buffer`
//! is an `ArrayBuffer` aliasing the wasm linear memory (no copies); it is
//! detached whenever the memory moves or grows, checked each time control
//! passes between wasm and script.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use boa_engine::object::builtins::{JsArray, JsArrayBuffer};
use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsBigInt, JsError, JsNativeError, JsObject, JsResult, JsString, JsSymbol, JsValue, NativeFunction};
use wasmi::{AsContextMut, Caller, Config, Engine, Extern, ExternRef, ExternType, Func, FuncType, Global, Linker, Memory, MemoryType, Module, Mutability, Nullable, Ref, RefType, Store, StoreContextMut, Table, TableType, Val, ValType};

/// Per-store data: everything wasm-side code needs to reach script.
pub struct Host {
    /// The script context of the current call into wasm.
    ctx: *mut Context,
    /// JS values held by wasm (`externref`s), by index.
    handles: Vec<JsValue>,
    /// JS functions imported by wasm, by index.
    host_fns: Vec<JsObject>,
    /// A script exception thrown inside an import, to rethrow from the call.
    pending: Option<JsError>,
    modules: Vec<Module>,
    /// Per module: imports and exports in the order the binary lists them.
    module_info: Vec<(Vec<(String, String, &'static str)>, Vec<(String, &'static str)>, Vec<u32>)>,
    /// The wasm function index of exported functions (their JS `name`).
    func_index: HashMap<usize, u32>,
    funcs: Vec<Func>,
    func_ids: HashMap<Vec<u8>, usize>,
    memories: Vec<Memory>,
    mem_ids: HashMap<Vec<u8>, usize>,
    /// The buffer handed to script per memory, with the bytes it aliases.
    buffers: HashMap<usize, (JsObject, usize, usize)>,
    tables: Vec<Table>,
    table_ids: HashMap<Vec<u8>, usize>,
    globals: Vec<Global>,
    global_ids: HashMap<Vec<u8>, usize>,
    /// `makeFunc(id)` from wasm.js: the JS function for an exported wasm function.
    make_func: Option<JsObject>,
    /// Error constructors (CompileError, LinkError, RuntimeError).
    errors: Option<(JsObject, JsObject, JsObject)>,
    /// Key that detaches memory buffers (script can't).
    detach_key: JsValue,
    /// Marks the JS functions wrapping wasm functions (value: their id).
    func_sym: JsSymbol,
}

struct Shared {
    store: RefCell<Store<Host>>,
    engine: Engine,
}

thread_local! {
    /// Time spent running wasm on this thread (ns), for the performance overlay.
    static WASM_NS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static WASM_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Live stores on this thread (for memory accounting).
    static STORES: RefCell<Vec<std::rc::Weak<Shared>>> = const { RefCell::new(Vec::new()) };
}

/// Total time spent in wasm calls on this thread (ns).
pub fn wasm_time_ns() -> u64 {
    WASM_NS.with(|n| n.get())
}

/// Bytes of wasm linear memory held by live instances on this thread.
pub fn wasm_memory_bytes() -> usize {
    STORES.with(|s| {
        let mut s = s.borrow_mut();
        s.retain(|w| w.strong_count() > 0);
        let mut total = 0usize;
        for w in s.iter() {
            if let Some(sh) = w.upgrade() {
                if let Ok(store) = sh.store.try_borrow() {
                    for m in &store.data().memories {
                        total += m.data(&*store).len();
                    }
                }
            }
        }
        total
    })
}

thread_local! {
    /// Callers of host functions in progress: (store, caller) pairs, so code
    /// re-entering wasm from an import uses the suspended caller's context.
    static ACTIVE: RefCell<Vec<(usize, *mut (), bool)>> = const { RefCell::new(Vec::new()) };
}

/// Raw bytes of a wasmi handle (they have no Eq/Hash): identity keys.
fn hkey<T: Copy>(h: &T) -> Vec<u8> {
    // SAFETY: handles are plain integer data (store index + arena index).
    unsafe { std::slice::from_raw_parts(h as *const T as *const u8, std::mem::size_of::<T>()) }.to_vec()
}

/// Run `f` with the store's context: the innermost suspended caller of this
/// store when wasm is on the stack, else the store itself.
fn with_store<R>(sh: &Rc<Shared>, f: impl FnOnce(&mut StoreContextMut<'_, Host>) -> R) -> R {
    let id = Rc::as_ptr(sh) as usize;
    let active = ACTIVE.with(|a| a.borrow().iter().rev().find(|(s, _, _)| *s == id).map(|(_, c, k)| (*c, *k)));
    match active {
        // SAFETY: the pointer is a live `Caller` on a frame below us (popped
        // before that frame returns); the store isn't otherwise borrowed.
        Some((c, true)) => {
            let caller = unsafe { &mut *(c as *mut Caller<'static, Host>) };
            let mut sc = caller.as_context_mut();
            return f(&mut sc);
        }
        // SAFETY: a store context borrowed by an outer `with_store` whose
        // closure is suspended in a script call (e.g. `makeFunc`, `valueOf`)
        // that re-entered the wasm natives; it outlives this call.
        Some((c, false)) => {
            let sc = unsafe { &mut *(c as *mut StoreContextMut<'static, Host>) };
            return f(sc);
        }
        None => {}
    }
    let Ok(mut store) = sh.store.try_borrow_mut() else {
        panic!("wasm store re-entered without an active context");
    };
    let mut sc = store.as_context_mut();
    let p = &mut sc as *mut StoreContextMut<'_, Host> as *mut ();
    ACTIVE.with(|a| a.borrow_mut().push((id, p, false)));
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) {
            ACTIVE.with(|a| {
                a.borrow_mut().pop();
            });
        }
    }
    let _pop = Pop;
    f(&mut sc)
}

fn type_err(m: impl Into<String>) -> JsError {
    JsNativeError::typ().with_message(m.into()).into()
}

fn range_err(m: impl Into<String>) -> JsError {
    JsNativeError::range().with_message(m.into()).into()
}

/// A `WebAssembly.{Compile,Link,Runtime}Error`.
fn wasm_err(sc: &StoreContextMut<'_, Host>, kind: usize, msg: &str, ctx: &mut Context) -> JsError {
    if let Some((c, l, r)) = &sc.data().errors {
        let ctor = [c, l, r][kind.min(2)].clone();
        if let Ok(o) = ctor.construct(&[JsValue::from(JsString::from(msg))], None, ctx) {
            return JsError::from_opaque(o.into());
        }
    }
    type_err(msg)
}

fn val_type_name(t: ValType) -> &'static str {
    match t {
        ValType::I32 => "i32",
        ValType::I64 => "i64",
        ValType::F32 => "f32",
        ValType::F64 => "f64",
        ValType::V128 => "v128",
        ValType::FuncRef => "anyfunc",
        ValType::ExternRef => "externref",
    }
}

fn parse_val_type(s: &str) -> Option<ValType> {
    Some(match s {
        "i32" => ValType::I32,
        "i64" => ValType::I64,
        "f32" => ValType::F32,
        "f64" => ValType::F64,
        "v128" => ValType::V128,
        "anyfunc" | "funcref" => ValType::FuncRef,
        "externref" => ValType::ExternRef,
        _ => return None,
    })
}

fn register_func(sc: &mut StoreContextMut<'_, Host>, f: Func) -> usize {
    let k = hkey(&f);
    let h = sc.data_mut();
    if let Some(&i) = h.func_ids.get(&k) {
        return i;
    }
    h.funcs.push(f);
    let i = h.funcs.len() - 1;
    h.func_ids.insert(k, i);
    i
}

fn register_memory(sc: &mut StoreContextMut<'_, Host>, m: Memory) -> usize {
    let k = hkey(&m);
    let h = sc.data_mut();
    if let Some(&i) = h.mem_ids.get(&k) {
        return i;
    }
    h.memories.push(m);
    let i = h.memories.len() - 1;
    h.mem_ids.insert(k, i);
    i
}

fn register_table(sc: &mut StoreContextMut<'_, Host>, t: Table) -> usize {
    let k = hkey(&t);
    let h = sc.data_mut();
    if let Some(&i) = h.table_ids.get(&k) {
        return i;
    }
    h.tables.push(t);
    let i = h.tables.len() - 1;
    h.table_ids.insert(k, i);
    i
}

fn register_global(sc: &mut StoreContextMut<'_, Host>, g: Global) -> usize {
    let k = hkey(&g);
    let h = sc.data_mut();
    if let Some(&i) = h.global_ids.get(&k) {
        return i;
    }
    h.globals.push(g);
    let i = h.globals.len() - 1;
    h.global_ids.insert(k, i);
    i
}

/// Detach memory buffers whose bytes moved or changed size (memory.grow).
fn refresh_buffers(sc: &mut StoreContextMut<'_, Host>, ctx: &mut Context) {
    if sc.data().buffers.is_empty() {
        return;
    }
    let ids: Vec<usize> = sc.data().buffers.keys().copied().collect();
    for id in ids {
        let m = sc.data().memories[id];
        let (ptr, len) = (m.data_ptr(&*sc) as usize, m.data_size(&*sc));
        let stale = sc.data().buffers.get(&id).map(|b| b.1 != ptr || b.2 != len).unwrap_or(false);
        if stale {
            if let Some((o, _, _)) = sc.data_mut().buffers.remove(&id) {
                let key = sc.data().detach_key.clone();
                if let Ok(ab) = JsArrayBuffer::from_object(o) {
                    let _ = ab.detach(&key);
                }
            }
        }
    }
    let _ = ctx;
}

/// ToJSValue.
fn to_js(sc: &mut StoreContextMut<'_, Host>, v: &Val, ctx: &mut Context) -> JsResult<JsValue> {
    Ok(match v {
        Val::I32(x) => JsValue::from(*x),
        Val::I64(x) => JsValue::from(JsBigInt::new(*x)),
        Val::F32(x) => JsValue::from(f32::from(*x) as f64),
        Val::F64(x) => JsValue::from(f64::from(*x)),
        Val::V128(_) => return Err(type_err("type incompatibility when transforming from/to JS")),
        Val::FuncRef(Nullable::Null) | Val::ExternRef(Nullable::Null) => JsValue::null(),
        Val::FuncRef(Nullable::Val(f)) => {
            let id = register_func(sc, *f);
            let mk = sc.data().make_func.clone().ok_or_else(|| type_err("wasm not initialized"))?;
            mk.call(&JsValue::undefined(), &[JsValue::from(id as u32)], ctx)?
        }
        Val::ExternRef(Nullable::Val(e)) => {
            let idx = e.data(&*sc).downcast_ref::<usize>().copied();
            match idx {
                Some(i) => sc.data().handles.get(i).cloned().unwrap_or_default(),
                None => JsValue::undefined(),
            }
        }
    })
}

/// ToWebAssemblyValue.
fn to_wasm(sc: &mut StoreContextMut<'_, Host>, v: &JsValue, t: ValType, ctx: &mut Context) -> JsResult<Val> {
    Ok(match t {
        ValType::I32 => Val::I32(v.to_i32(ctx)?),
        ValType::I64 => Val::I64(v.to_big_int64(ctx)?),
        ValType::F32 => Val::F32((v.to_number(ctx)? as f32).into()),
        ValType::F64 => Val::F64(v.to_number(ctx)?.into()),
        ValType::V128 => return Err(type_err("type incompatibility when transforming from/to JS")),
        ValType::FuncRef => {
            if v.is_null() {
                Val::FuncRef(Nullable::Null)
            } else {
                let sym = sc.data().func_sym.clone();
                let id = v.as_object().map(|o| o.get(sym, ctx)).transpose()?.and_then(|x| x.as_number());
                match id {
                    Some(i) if (i as usize) < sc.data().funcs.len() => Val::FuncRef(Nullable::Val(sc.data().funcs[i as usize])),
                    _ => return Err(type_err("type incompatibility when transforming from/to JS")),
                }
            }
        }
        ValType::ExternRef => {
            if v.is_null() {
                Val::ExternRef(Nullable::Null)
            } else {
                let h = sc.data_mut();
                h.handles.push(v.clone());
                let i = h.handles.len() - 1;
                Val::ExternRef(Nullable::Val(ExternRef::new(&mut *sc, i)))
            }
        }
    })
}

fn val_ref(v: Val) -> Ref {
    match v {
        Val::FuncRef(f) => Ref::Func(f),
        Val::ExternRef(e) => Ref::Extern(e),
        _ => Ref::Func(Nullable::Null),
    }
}

fn ref_val(r: Ref) -> Val {
    match r {
        Ref::Func(f) => Val::FuncRef(f),
        Ref::Extern(e) => Val::ExternRef(e),
    }
}

fn zero(t: ValType) -> Val {
    Val::default_for_ty(t)
}

/// Bytes of a BufferSource (wasm.js passes an ArrayBuffer).
fn bytes_of(v: &JsValue) -> JsResult<Vec<u8>> {
    let o = v.as_object().ok_or_else(|| type_err("WebAssembly: argument must be a BufferSource"))?;
    let ab = JsArrayBuffer::from_object(o.clone()).map_err(|_| type_err("WebAssembly: argument must be a BufferSource"))?;
    Ok(ab.data().map(|d| d.to_vec()).unwrap_or_default())
}

fn arg(args: &[JsValue], i: usize) -> JsValue {
    args.get(i).cloned().unwrap_or_default()
}

fn idx(args: &[JsValue], i: usize) -> usize {
    args.get(i).map(as_index).unwrap_or(usize::MAX)
}

fn as_index(v: &JsValue) -> usize {
    match v.as_number() {
        Some(n) if n >= 0.0 && n.is_finite() => n as usize,
        _ => usize::MAX,
    }
}

fn js_str(s: &str) -> JsValue {
    JsValue::from(JsString::from(s))
}

fn kind_name(t: &ExternType) -> &'static str {
    match t {
        ExternType::Func(_) => "function",
        ExternType::Table(_) => "table",
        ExternType::Memory(_) => "memory",
        ExternType::Global(_) => "global",
    }
}

/// Imports and exports in binary order (wasmi groups them by kind).
fn parse_order(b: &[u8]) -> Option<(Vec<(String, String, &'static str)>, Vec<(String, &'static str)>, Vec<u32>)> {
    struct R<'a> {
        b: &'a [u8],
        i: usize,
    }
    impl R<'_> {
        fn byte(&mut self) -> Option<u8> {
            let v = *self.b.get(self.i)?;
            self.i += 1;
            Some(v)
        }
        fn leb(&mut self) -> Option<u64> {
            let mut r = 0u64;
            let mut shift = 0;
            loop {
                let x = self.byte()?;
                if shift < 64 {
                    r |= u64::from(x & 0x7f) << shift;
                }
                if x & 0x80 == 0 {
                    return Some(r);
                }
                shift += 7;
                if shift > 70 {
                    return None;
                }
            }
        }
        fn name(&mut self) -> Option<String> {
            let n = self.leb()? as usize;
            let s = self.b.get(self.i..self.i.checked_add(n)?)?;
            self.i += n;
            Some(String::from_utf8_lossy(s).into_owned())
        }
        fn limits(&mut self) -> Option<()> {
            let f = self.byte()?;
            self.leb()?;
            if f & 1 != 0 {
                self.leb()?;
            }
            if f & 8 != 0 {
                self.leb()?;
            }
            Some(())
        }
        fn reftype(&mut self) -> Option<()> {
            match self.byte()? {
                0x63 | 0x64 => {
                    self.leb()?; // heap type (s33)
                    Some(())
                }
                _ => Some(()),
            }
        }
    }
    let kind = |k: u8| -> Option<&'static str> {
        Some(match k {
            0 => "function",
            1 => "table",
            2 => "memory",
            3 => "global",
            4 => "tag",
            _ => return None,
        })
    };
    if b.len() < 8 || &b[0..4] != b"\0asm" {
        return None;
    }
    let mut r = R { b, i: 8 };
    let mut imports = Vec::new();
    let mut exports = Vec::new();
    let mut indices = Vec::new();
    while r.i < b.len() {
        let id = r.byte()?;
        let size = r.leb()? as usize;
        let end = r.i.checked_add(size)?;
        if end > b.len() {
            return None;
        }
        if id == 2 {
            let n = r.leb()?;
            for _ in 0..n {
                let m = r.name()?;
                let nm = r.name()?;
                let k = r.byte()?;
                match k {
                    0 => {
                        r.leb()?;
                    }
                    1 => {
                        r.reftype()?;
                        r.limits()?;
                    }
                    2 => r.limits()?,
                    3 => {
                        r.reftype()?;
                        r.byte()?;
                    }
                    4 => {
                        r.byte()?;
                        r.leb()?;
                    }
                    _ => return None,
                }
                imports.push((m, nm, kind(k)?));
            }
        } else if id == 7 {
            let n = r.leb()?;
            for _ in 0..n {
                let nm = r.name()?;
                let k = r.byte()?;
                indices.push(r.leb()? as u32);
                exports.push((nm, kind(k)?));
            }
        }
        r.i = end;
    }
    Some((imports, exports, indices))
}

/// Call a wasm function from script: convert arguments, run it, convert results.
fn call_func(sh: &Rc<Shared>, id: usize, args: &JsValue, ctx: &mut Context) -> JsResult<JsValue> {
    let argv: Vec<JsValue> = match args.as_object() {
        Some(o) => {
            let a = JsArray::from_object(o.clone())?;
            let n = a.length(ctx)?;
            (0..n).map(|i| a.get(i, ctx)).collect::<JsResult<_>>()?
        }
        None => Vec::new(),
    };
    let tys: Vec<ValType> = with_store(sh, |sc| sc.data().funcs.get(id).map(|f| f.ty(&*sc).params().to_vec())).ok_or_else(|| type_err("not a wasm function"))?;
    // Numbers first, outside the store (valueOf() may call back into wasm).
    let mut pre: Vec<Option<Val>> = Vec::with_capacity(tys.len());
    for (i, t) in tys.iter().enumerate() {
        let v = argv.get(i).cloned().unwrap_or_default();
        pre.push(match t {
            ValType::I32 => Some(Val::I32(v.to_i32(ctx)?)),
            ValType::I64 => Some(Val::I64(v.to_big_int64(ctx)?)),
            ValType::F32 => Some(Val::F32((v.to_number(ctx)? as f32).into())),
            ValType::F64 => Some(Val::F64(v.to_number(ctx)?.into())),
            _ => None,
        });
    }
    let ctx_ptr: *mut Context = ctx;
    with_store(sh, |sc| {
        let f = *sc.data().funcs.get(id).ok_or_else(|| type_err("not a wasm function"))?;
        let ty = f.ty(&*sc);
        let mut params = Vec::with_capacity(ty.params().len());
        for (i, t) in ty.params().iter().enumerate() {
            match pre[i].take() {
                Some(v) => params.push(v),
                None => {
                    let v = argv.get(i).cloned().unwrap_or_default();
                    // SAFETY: `ctx` is the live context of this native call.
                    params.push(to_wasm(sc, &v, *t, unsafe { &mut *ctx_ptr })?);
                }
            }
        }
        let mut results: Vec<Val> = ty.results().iter().map(|t| zero(*t)).collect();
        let prev = std::mem::replace(&mut sc.data_mut().ctx, ctx_ptr);
        let depth = WASM_DEPTH.with(|d| {
            let v = d.get();
            d.set(v + 1);
            v
        });
        let t0 = std::time::Instant::now();
        let r = f.call(&mut *sc, &params, &mut results);
        WASM_DEPTH.with(|d| d.set(depth));
        if depth == 0 {
            WASM_NS.with(|n| n.set(n.get() + t0.elapsed().as_nanos() as u64));
        }
        sc.data_mut().ctx = prev;
        let ctx = unsafe { &mut *ctx_ptr };
        refresh_buffers(sc, ctx);
        if let Err(e) = r {
            if let Some(js) = sc.data_mut().pending.take() {
                return Err(js);
            }
            return Err(trap_error(sc, &e, ctx));
        }
        match results.len() {
            0 => Ok(JsValue::undefined()),
            1 => to_js(sc, &results[0], ctx),
            _ => {
                let mut out = Vec::new();
                for r in &results {
                    out.push(to_js(sc, r, ctx)?);
                }
                Ok(JsArray::from_iter(out, ctx).into())
            }
        }
    })
}

fn trap_error(sc: &StoreContextMut<'_, Host>, e: &wasmi::Error, ctx: &mut Context) -> JsError {
    match e.as_trap_code() {
        Some(wasmi::TrapCode::StackOverflow) => range_err("Maximum call stack size exceeded"),
        Some(code) => wasm_err(sc, 2, code.trap_message(), ctx),
        None => wasm_err(sc, 2, &e.to_string(), ctx),
    }
}

/// A host function for an imported JS callable.
fn host_func(sc: &mut StoreContextMut<'_, Host>, store_id: usize, ty: FuncType, f: JsObject) -> Func {
    sc.data_mut().host_fns.push(f);
    let fi = sc.data().host_fns.len() - 1;
    let rtys: Vec<ValType> = ty.results().to_vec();
    Func::new(&mut *sc, ty, move |mut caller: Caller<'_, Host>, params: &[Val], results: &mut [Val]| -> Result<(), wasmi::Error> {
        let ctx_ptr = caller.data().ctx;
        if ctx_ptr.is_null() {
            return Err(wasmi::Error::new("wasm: no script context"));
        }
        // SAFETY: set by the call into wasm that is suspended under us.
        let ctx = unsafe { &mut *ctx_ptr };
        let f = caller.data().host_fns[fi].clone();
        let cptr = &mut caller as *mut Caller<'_, Host> as *mut ();
        ACTIVE.with(|a| a.borrow_mut().push((store_id, cptr, true)));
        let r = (|| -> JsResult<()> {
            let mut sc = caller.as_context_mut();
            refresh_buffers(&mut sc, ctx);
            let mut args = Vec::with_capacity(params.len());
            for p in params {
                args.push(to_js(&mut sc, p, ctx)?);
            }
            drop(sc);
            let ret = f.call(&JsValue::undefined(), &args, ctx)?;
            let mut sc = caller.as_context_mut();
            refresh_buffers(&mut sc, ctx);
            match rtys.len() {
                0 => {}
                1 => results[0] = to_wasm(&mut sc, &ret, rtys[0], ctx)?,
                n => {
                    // Multi-value: an iterable of n values.
                    let it = ret.as_object().ok_or_else(|| type_err("wasm: expected an iterable of results"))?;
                    let arr = JsArray::from_object(it.clone()).or_else(|_| {
                        let from = ctx.intrinsics().constructors().array().constructor();
                        let a = from.get(js_string!("from"), ctx)?.as_callable().ok_or_else(|| type_err("Array.from"))?.call(&from.clone().into(), &[ret.clone()], ctx)?;
                        JsArray::from_object(a.as_object().ok_or_else(|| type_err("iterable"))?.clone())
                    })?;
                    if arr.length(ctx)? as usize != n {
                        return Err(type_err("wasm: wrong number of results"));
                    }
                    for (i, t) in rtys.iter().enumerate() {
                        let v = arr.get(i as u64, ctx)?;
                        results[i] = to_wasm(&mut sc, &v, *t, ctx)?;
                    }
                }
            }
            Ok(())
        })();
        ACTIVE.with(|a| a.borrow_mut().pop());
        match r {
            Ok(()) => Ok(()),
            Err(e) => {
                caller.data_mut().pending = Some(e);
                Err(wasmi::Error::new("JavaScript exception"))
            }
        }
    })
}

/// A memory buffer for script (cached until the memory moves or grows).
fn mem_buffer(sc: &mut StoreContextMut<'_, Host>, id: usize, ctx: &mut Context) -> JsResult<JsValue> {
    refresh_buffers(sc, ctx);
    if let Some((o, _, _)) = sc.data().buffers.get(&id) {
        return Ok(o.clone().into());
    }
    let m = *sc.data().memories.get(id).ok_or_else(|| type_err("not a memory"))?;
    let ptr = m.data_ptr(&*sc);
    let len = m.data_size(&*sc);
    let key = sc.data().detach_key.clone();
    let keep: Rc<dyn std::any::Any> = Rc::new(());
    // SAFETY: the bytes stay put until the memory grows, and every grow is
    // followed by `refresh_buffers` before script runs again.
    let ab = unsafe { JsArrayBuffer::from_external(ptr, len, keep, key, ctx)? };
    let o: JsObject = ab.into();
    sc.data_mut().buffers.insert(id, (o.clone(), ptr as usize, len));
    Ok(o.into())
}

type NFn = fn(&Rc<Shared>, &[JsValue], &mut Context) -> JsResult<JsValue>;

fn natives() -> Vec<(&'static str, NFn)> {
    vec![
        ("init", |sh, a, _ctx| {
            // init(makeFunc, CompileError, LinkError, RuntimeError)
            let o = |i: usize| a.get(i).and_then(|v| v.as_object()).map(|o| o.clone());
            let mut st = sh.store.borrow_mut();
            let h = st.data_mut();
            h.make_func = o(0);
            if let (Some(c), Some(l), Some(r)) = (o(1), o(2), o(3)) {
                h.errors = Some((c, l, r));
            }
            Ok(h.func_sym.clone().into())
        }),
        ("validate", |sh, a, _ctx| {
            let b = bytes_of(&arg(a, 0))?;
            Ok(JsValue::from(Module::validate(&sh.engine, &b).is_ok()))
        }),
        ("compile", |sh, a, ctx| {
            let b = bytes_of(&arg(a, 0))?;
            match Module::new(&sh.engine, &b) {
                Ok(m) => with_store(sh, |sc| {
                    let info = parse_order(&b).unwrap_or_else(|| {
                        (m.imports().map(|i| (i.module().to_string(), i.name().to_string(), kind_name(i.ty()))).collect(), m.exports().map(|e| (e.name().to_string(), kind_name(e.ty()))).collect(), Vec::new())
                    });
                    sc.data_mut().modules.push(m);
                    sc.data_mut().module_info.push(info);
                    Ok(JsValue::from((sc.data().modules.len() - 1) as u32))
                }),
                Err(e) => with_store(sh, |sc| Err(wasm_err(sc, 0, &format!("WebAssembly.Module(): {e}"), ctx))),
            }
        }),
        ("imports", |sh, a, ctx| {
            let id = idx(a, 0);
            let list: Vec<(String, String, &'static str)> = with_store(sh, |sc| sc.data().module_info.get(id).map(|i| i.0.clone()).unwrap_or_default());
            let items: Vec<JsValue> = list.iter().map(|(m, n, k)| JsArray::from_iter([js_str(m), js_str(n), js_str(k)], ctx).into()).collect();
            Ok(JsArray::from_iter(items, ctx).into())
        }),
        ("exports", |sh, a, ctx| {
            let id = idx(a, 0);
            let list: Vec<(String, &'static str)> = with_store(sh, |sc| sc.data().module_info.get(id).map(|i| i.1.clone()).unwrap_or_default());
            let items: Vec<JsValue> = list.iter().map(|(n, k)| JsArray::from_iter([js_str(n), js_str(k)], ctx).into()).collect();
            Ok(JsArray::from_iter(items, ctx).into())
        }),
        ("customSections", |sh, a, ctx| {
            let id = idx(a, 0);
            let name = arg(a, 1).to_string(ctx)?.to_std_string_escaped();
            let datas: Vec<Vec<u8>> = with_store(sh, |sc| {
                sc.data().modules.get(id).map(|m| m.custom_sections().filter(|c| c.name() == name).map(|c| c.data().to_vec()).collect()).unwrap_or_default()
            });
            let mut items = Vec::new();
            for d in datas {
                let block = boa_engine::object::builtins::AlignedVec::from_iter(0, d);
                items.push(JsArrayBuffer::from_byte_block(block, ctx)?.into());
            }
            Ok(JsArray::from_iter(items, ctx).into())
        }),
        ("instantiate", |sh, a, ctx| {
            // instantiate(moduleId, [[tag, value], ...]) -> [[name, kind, id], ...]
            let id = idx(a, 0);
            let imports: Vec<(String, JsValue)> = match a.get(1).and_then(|v| v.as_object()) {
                Some(o) => {
                    let arr = JsArray::from_object(o.clone())?;
                    let n = arr.length(ctx)?;
                    let mut v = Vec::new();
                    for i in 0..n {
                        let pair = arr.get(i, ctx)?;
                        let po = pair.as_object().ok_or_else(|| type_err("import"))?;
                        let tag = po.get(0, ctx)?.to_string(ctx)?.to_std_string_escaped();
                        v.push((tag, po.get(1, ctx)?));
                    }
                    v
                }
                None => Vec::new(),
            };
            let store_id = Rc::as_ptr(sh) as usize;
            let ctx_ptr: *mut Context = ctx;
            let exports: Vec<(String, &'static str, usize)> = with_store(sh, |sc| -> JsResult<_> {
                let ctx = unsafe { &mut *ctx_ptr };
                let module = sc.data().modules.get(id).cloned().ok_or_else(|| type_err("not a module"))?;
                let mut linker: Linker<Host> = Linker::new(&sh.engine);
                linker.allow_shadowing(true);
                let imps: Vec<(String, String, ExternType)> = module.imports().map(|i| (i.module().to_string(), i.name().to_string(), i.ty().clone())).collect();
                if imps.len() != imports.len() {
                    return Err(type_err("WebAssembly.Instance(): imports mismatch"));
                }
                // The values come in binary order: match each import to its position.
                let order = sc.data().module_info.get(id).map(|i| i.0.clone()).unwrap_or_default();
                let mut slots: HashMap<(String, String, &'static str), std::collections::VecDeque<usize>> = HashMap::new();
                for (pos, (m, n, k)) in order.iter().enumerate() {
                    slots.entry((m.clone(), n.clone(), *k)).or_default().push_back(pos);
                }
                let mut paired = Vec::with_capacity(imps.len());
                for (i, (m, n, ty)) in imps.iter().enumerate() {
                    let pos = slots.get_mut(&(m.clone(), n.clone(), kind_name(ty))).and_then(|q| q.pop_front()).unwrap_or(i);
                    paired.push(((m, n, ty), imports.get(pos).ok_or_else(|| type_err("WebAssembly.Instance(): imports mismatch"))?));
                }
                for ((m, n, ty), (tag, v)) in paired {
                    let link = |sc: &StoreContextMut<'_, Host>, ctx: &mut Context, what: &str| wasm_err(sc, 1, &format!("WebAssembly.Instance(): Import #0 \"{m}\" \"{n}\": {what}"), ctx);
                    let ext: Extern = match (ty, tag.as_str()) {
                        (ExternType::Func(fty), "f") => {
                            let f = *sc.data().funcs.get(as_index(v)).ok_or_else(|| type_err("bad function"))?;
                            if f.ty(&*sc) != *fty {
                                return Err(link(sc, ctx, "imported function does not match the expected type"));
                            }
                            Extern::Func(f)
                        }
                        (ExternType::Func(fty), "h") => {
                            let o = v.as_object().filter(|o| o.is_callable()).ok_or_else(|| link(sc, ctx, "function import requires a callable"))?.clone();
                            Extern::Func(host_func(sc, store_id, fty.clone(), o))
                        }
                        (ExternType::Memory(mty), "m") => {
                            let mem = *sc.data().memories.get(as_index(v)).ok_or_else(|| type_err("bad memory"))?;
                            let have = mem.ty(&*sc);
                            let cur = mem.size(&*sc);
                            if cur < mty.minimum() || (mty.maximum().is_some() && (have.maximum().is_none() || have.maximum() > mty.maximum())) || have.is_64() != mty.is_64() {
                                return Err(link(sc, ctx, "memory import has a mismatched size or type"));
                            }
                            Extern::Memory(mem)
                        }
                        (ExternType::Table(tty), "t") => {
                            let t = *sc.data().tables.get(as_index(v)).ok_or_else(|| type_err("bad table"))?;
                            let have = t.ty(&*sc);
                            if t.size(&*sc) < tty.minimum() || have.element() != tty.element() || (tty.maximum().is_some() && (have.maximum().is_none() || have.maximum() > tty.maximum())) {
                                return Err(link(sc, ctx, "table import has a mismatched size or type"));
                            }
                            Extern::Table(t)
                        }
                        (ExternType::Global(gty), "g") => {
                            let g = *sc.data().globals.get(as_index(v)).ok_or_else(|| type_err("bad global"))?;
                            let have = g.ty(&*sc);
                            if have.content() != gty.content() || have.mutability() != gty.mutability() {
                                return Err(link(sc, ctx, "imported global does not match the expected type"));
                            }
                            Extern::Global(g)
                        }
                        (ExternType::Global(gty), "v") => {
                            if gty.mutability() == Mutability::Var {
                                return Err(link(sc, ctx, "imported mutable global must be a WebAssembly.Global object"));
                            }
                            let t = gty.content();
                            if t == ValType::I64 && !v.is_bigint() {
                                return Err(link(sc, ctx, "global import must be a BigInt"));
                            }
                            if matches!(t, ValType::I32 | ValType::F32 | ValType::F64) && !v.is_number() {
                                return Err(link(sc, ctx, "global import must be a number"));
                            }
                            let val = to_wasm(sc, v, t, ctx)?;
                            Extern::Global(Global::new(&mut *sc, val, Mutability::Const))
                        }
                        _ => return Err(link(sc, ctx, "import kind mismatch")),
                    };
                    linker.define(m, n, ext).map_err(|e| wasm_err(sc, 1, &e.to_string(), ctx))?;
                }
                let prev = std::mem::replace(&mut sc.data_mut().ctx, ctx_ptr);
                let r = linker.instantiate_and_start(&mut *sc, &module);
                sc.data_mut().ctx = prev;
                refresh_buffers(sc, ctx);
                let inst = match r {
                    Ok(i) => i,
                    Err(e) => {
                        if let Some(js) = sc.data_mut().pending.take() {
                            return Err(js);
                        }
                        if e.as_trap_code().is_some() {
                            return Err(trap_error(sc, &e, ctx));
                        }
                        return Err(wasm_err(sc, 1, &format!("WebAssembly.Instance(): {e}"), ctx));
                    }
                };
                let names: Vec<String> = sc.data().module_info.get(id).map(|i| i.1.iter().map(|e| e.0.clone()).collect()).unwrap_or_default();
                let mut exps: Vec<(String, Extern)> = names.iter().filter_map(|n| inst.get_export(&*sc, n).map(|e| (n.clone(), e))).collect();
                if exps.is_empty() {
                    exps = inst.exports(&*sc).map(|e| (e.name().to_string(), e.into_extern())).collect();
                }
                let index_of: HashMap<String, u32> = sc.data().module_info.get(id).map(|i| i.1.iter().zip(i.2.iter()).filter(|(e, _)| e.1 == "function").map(|(e, ix)| (e.0.clone(), *ix)).collect()).unwrap_or_default();
                let mut out = Vec::new();
                for (name, e) in exps {
                    let (k, i) = match e {
                        Extern::Func(f) => {
                            let fid = register_func(sc, f);
                            if let Some(ix) = index_of.get(&name) {
                                sc.data_mut().func_index.entry(fid).or_insert(*ix);
                            }
                            ("function", fid)
                        }
                        Extern::Memory(m) => ("memory", register_memory(sc, m)),
                        Extern::Table(t) => ("table", register_table(sc, t)),
                        Extern::Global(g) => ("global", register_global(sc, g)),
                    };
                    out.push((name, k, i));
                }
                Ok(out)
            })?;
            let items: Vec<JsValue> = exports.iter().map(|(n, k, i)| JsArray::from_iter([js_str(n), js_str(k), JsValue::from(*i as u32)], ctx).into()).collect();
            Ok(JsArray::from_iter(items, ctx).into())
        }),
        ("call", |sh, a, ctx| call_func(sh, idx(a, 0), &arg(a, 1), ctx)),
        ("funcInfo", |sh, a, ctx| {
            // [arity, wasm function index (the JS name)]
            let id = idx(a, 0);
            let (n, ix) = with_store(sh, |sc| (sc.data().funcs.get(id).map(|f| f.ty(&*sc).params().len() as u32).unwrap_or(0), sc.data().func_index.get(&id).copied().unwrap_or(id as u32)));
            Ok(JsArray::from_iter([JsValue::from(n), JsValue::from(ix)], ctx).into())
        }),
        ("funcType", |sh, a, ctx| {
            let id = idx(a, 0);
            let (p, r): (Vec<&'static str>, Vec<&'static str>) = with_store(sh, |sc| {
                sc.data().funcs.get(id).map(|f| {
                    let t = f.ty(&*sc);
                    (t.params().iter().map(|x| val_type_name(*x)).collect(), t.results().iter().map(|x| val_type_name(*x)).collect())
                })
            })
            .unwrap_or_default();
            let pa: JsValue = JsArray::from_iter(p.into_iter().map(js_str), ctx).into();
            let ra: JsValue = JsArray::from_iter(r.into_iter().map(js_str), ctx).into();
            Ok(JsArray::from_iter([pa, ra], ctx).into())
        }),
        ("memNew", |sh, a, _ctx| {
            let initial = arg(a, 0).as_number().unwrap_or(0.0) as u32;
            let max = arg(a, 1).as_number().filter(|m| *m >= 0.0).map(|m| m as u32);
            with_store(sh, |sc| {
                let m = Memory::new(&mut *sc, MemoryType::new(initial, max)).map_err(|e| range_err(format!("WebAssembly.Memory(): could not allocate memory ({e})")))?;
                Ok(JsValue::from(register_memory(sc, m) as u32))
            })
        }),
        ("memBuffer", |sh, a, ctx| {
            let id = idx(a, 0);
            let ctx_ptr: *mut Context = ctx;
            with_store(sh, |sc| mem_buffer(sc, id, unsafe { &mut *ctx_ptr }))
        }),
        ("memGrow", |sh, a, ctx| {
            let id = idx(a, 0);
            let delta = arg(a, 1).as_number().unwrap_or(0.0) as u64;
            let ctx_ptr: *mut Context = ctx;
            with_store(sh, |sc| {
                let m = *sc.data().memories.get(id).ok_or_else(|| type_err("not a memory"))?;
                let old = m.grow(&mut *sc, delta).map_err(|_| range_err("WebAssembly.Memory.grow(): Maximum memory size exceeded"))?;
                // The old buffer is detached even when the size didn't change.
                if let Some((o, _, _)) = sc.data_mut().buffers.remove(&id) {
                    let key = sc.data().detach_key.clone();
                    if let Ok(ab) = JsArrayBuffer::from_object(o) {
                        let _ = ab.detach(&key);
                    }
                }
                refresh_buffers(sc, unsafe { &mut *ctx_ptr });
                Ok(JsValue::from(old as f64))
            })
        }),
        ("memType", |sh, a, ctx| {
            let id = idx(a, 0);
            let (min, max) = with_store(sh, |sc| sc.data().memories.get(id).map(|m| (m.size(&*sc), m.ty(&*sc).maximum()))).unwrap_or((0, None));
            Ok(JsArray::from_iter([JsValue::from(min as f64), max.map(|m| JsValue::from(m as f64)).unwrap_or_default()], ctx).into())
        }),
        ("tableNew", |sh, a, ctx| {
            // tableNew(element, initial, maximum|-1, init)
            let el = arg(a, 0).to_string(ctx)?.to_std_string_escaped();
            let rt = if el == "externref" { RefType::Extern } else { RefType::Func };
            let initial = arg(a, 1).as_number().unwrap_or(0.0) as u32;
            let max = arg(a, 2).as_number().filter(|m| *m >= 0.0).map(|m| m as u32);
            let init = arg(a, 3);
            let absent = a.len() < 4;
            let ctx_ptr: *mut Context = ctx;
            with_store(sh, |sc| {
                let ctx = unsafe { &mut *ctx_ptr };
                let vt = if rt == RefType::Extern { ValType::ExternRef } else { ValType::FuncRef };
                let r = if absent && rt == RefType::Func { Ref::default_for_ty(rt) } else { val_ref(to_wasm(sc, &init, vt, ctx)?) };
                let t = Table::new(&mut *sc, TableType::new(rt, initial, max), r).map_err(|e| range_err(format!("WebAssembly.Table(): {e}")))?;
                Ok(JsValue::from(register_table(sc, t) as u32))
            })
        }),
        ("tableLength", |sh, a, _ctx| {
            let id = idx(a, 0);
            Ok(JsValue::from(with_store(sh, |sc| sc.data().tables.get(id).map(|t| t.size(&*sc)).unwrap_or(0)) as f64))
        }),
        ("tableGet", |sh, a, ctx| {
            let id = idx(a, 0);
            let i = arg(a, 1).as_number().unwrap_or(-1.0);
            let ctx_ptr: *mut Context = ctx;
            with_store(sh, |sc| {
                let t = *sc.data().tables.get(id).ok_or_else(|| type_err("not a table"))?;
                let r = (i >= 0.0).then(|| t.get(&*sc, i as u64)).flatten().ok_or_else(|| range_err("WebAssembly.Table.get(): invalid address"))?;
                to_js(sc, &ref_val(r), unsafe { &mut *ctx_ptr })
            })
        }),
        ("tableSet", |sh, a, ctx| {
            let id = idx(a, 0);
            let i = arg(a, 1).as_number().unwrap_or(-1.0);
            let v = arg(a, 2);
            let absent = a.len() < 3;
            let ctx_ptr: *mut Context = ctx;
            with_store(sh, |sc| {
                let ctx = unsafe { &mut *ctx_ptr };
                let t = *sc.data().tables.get(id).ok_or_else(|| type_err("not a table"))?;
                let rt = t.ty(&*sc).element();
                let vt = if rt == RefType::Extern { ValType::ExternRef } else { ValType::FuncRef };
                let r = if absent && rt == RefType::Func { Ref::default_for_ty(rt) } else { val_ref(to_wasm(sc, &v, vt, ctx)?) };
                if i < 0.0 || i as u64 >= t.size(&*sc) {
                    return Err(range_err("WebAssembly.Table.set(): invalid address"));
                }
                t.set(&mut *sc, i as u64, r).map_err(|e| range_err(format!("WebAssembly.Table.set(): {e}")))?;
                Ok(JsValue::undefined())
            })
        }),
        ("tableGrow", |sh, a, ctx| {
            let id = idx(a, 0);
            let delta = arg(a, 1).as_number().unwrap_or(0.0) as u64;
            let v = arg(a, 2);
            let absent = a.len() < 3;
            let ctx_ptr: *mut Context = ctx;
            with_store(sh, |sc| {
                let ctx = unsafe { &mut *ctx_ptr };
                let t = *sc.data().tables.get(id).ok_or_else(|| type_err("not a table"))?;
                let rt = t.ty(&*sc).element();
                let vt = if rt == RefType::Extern { ValType::ExternRef } else { ValType::FuncRef };
                let r = if absent && rt == RefType::Func { Ref::default_for_ty(rt) } else { val_ref(to_wasm(sc, &v, vt, ctx)?) };
                let old = t.grow(&mut *sc, delta, r).map_err(|_| range_err("WebAssembly.Table.grow(): failed to grow table"))?;
                Ok(JsValue::from(old as f64))
            })
        }),
        ("tableType", |sh, a, ctx| {
            let id = idx(a, 0);
            let (el, min, max) = with_store(sh, |sc| sc.data().tables.get(id).map(|t| (t.ty(&*sc).element(), t.size(&*sc), t.ty(&*sc).maximum()))).unwrap_or((RefType::Func, 0, None));
            Ok(JsArray::from_iter([js_str(if el == RefType::Extern { "externref" } else { "anyfunc" }), JsValue::from(min as f64), max.map(|m| JsValue::from(m as f64)).unwrap_or_default()], ctx).into())
        }),
        ("globalNew", |sh, a, ctx| {
            // globalNew(type, mutable, value|undefined)
            let ty = arg(a, 0).to_string(ctx)?.to_std_string_escaped();
            let t = parse_val_type(&ty).filter(|t| *t != ValType::V128).ok_or_else(|| type_err("WebAssembly.Global(): Descriptor property 'value' must be a WebAssembly type"))?;
            let mutable = arg(a, 1).to_boolean();
            let v = arg(a, 2);
            let absent = a.len() < 3;
            let ctx_ptr: *mut Context = ctx;
            with_store(sh, |sc| {
                let ctx = unsafe { &mut *ctx_ptr };
                let val = if absent || (v.is_undefined() && t != ValType::ExternRef) {
                    if t == ValType::ExternRef { to_wasm(sc, &JsValue::undefined(), t, ctx)? } else { zero(t) }
                } else {
                    to_wasm(sc, &v, t, ctx)?
                };
                let g = Global::new(&mut *sc, val, if mutable { Mutability::Var } else { Mutability::Const });
                Ok(JsValue::from(register_global(sc, g) as u32))
            })
        }),
        ("globalGet", |sh, a, ctx| {
            let id = idx(a, 0);
            let ctx_ptr: *mut Context = ctx;
            with_store(sh, |sc| {
                let g = *sc.data().globals.get(id).ok_or_else(|| type_err("not a global"))?;
                let v = g.get(&*sc);
                to_js(sc, &v, unsafe { &mut *ctx_ptr })
            })
        }),
        ("globalSet", |sh, a, ctx| {
            let id = idx(a, 0);
            let v = arg(a, 1);
            let ctx_ptr: *mut Context = ctx;
            with_store(sh, |sc| {
                let ctx = unsafe { &mut *ctx_ptr };
                let g = *sc.data().globals.get(id).ok_or_else(|| type_err("not a global"))?;
                let ty = g.ty(&*sc);
                if ty.mutability() != Mutability::Var {
                    return Err(type_err("WebAssembly.Global.value: Can't set the value of an immutable global."));
                }
                let val = to_wasm(sc, &v, ty.content(), ctx)?;
                g.set(&mut *sc, val).map_err(|e| type_err(e.to_string()))?;
                Ok(JsValue::undefined())
            })
        }),
        ("globalType", |sh, a, ctx| {
            let id = idx(a, 0);
            let (t, m) = with_store(sh, |sc| sc.data().globals.get(id).map(|g| (g.ty(&*sc).content(), g.ty(&*sc).mutability() == Mutability::Var))).unwrap_or((ValType::I32, false));
            Ok(JsArray::from_iter([js_str(val_type_name(t)), JsValue::from(m)], ctx).into())
        }),
    ]
}

/// The JS side (classes and promise APIs), run on first use of `WebAssembly`.
const WASM_JS: &str = include_str!("wasm.js");

/// Install the lazy `WebAssembly` global.
/// An engine's wasm store, so the engine can release it when it goes away.
pub struct WasmHandle(std::rc::Weak<Shared>);

impl WasmHandle {
    /// Drop every JS value the store holds. The store's natives (GC objects)
    /// own the store through an `Rc`, and the store's JS values are roots: the
    /// cycle kept a document's entire JS heap alive after the document went.
    pub fn release(&self) {
        if let Some(sh) = self.0.upgrade() {
            if let Ok(mut st) = sh.store.try_borrow_mut() {
                let h = st.data_mut();
                h.handles.clear();
                h.host_fns.clear();
                h.pending = None;
                h.buffers.clear();
                h.make_func = None;
                h.errors = None;
            }
        }
    }
}

pub fn install(ctx: &mut Context) -> WasmHandle {
    let mut config = Config::default();
    config.compilation_mode(wasmi::CompilationMode::Lazy);
    config.wasm_simd(true);
    let engine = Engine::new(&config);
    let host = Host {
        ctx: std::ptr::null_mut(),
        handles: Vec::new(),
        host_fns: Vec::new(),
        pending: None,
        modules: Vec::new(),
        module_info: Vec::new(),
        func_index: HashMap::new(),
        funcs: Vec::new(),
        func_ids: HashMap::new(),
        memories: Vec::new(),
        mem_ids: HashMap::new(),
        buffers: HashMap::new(),
        tables: Vec::new(),
        table_ids: HashMap::new(),
        globals: Vec::new(),
        global_ids: HashMap::new(),
        make_func: None,
        errors: None,
        detach_key: JsSymbol::new(Some(js_string!("wasm memory"))).map(JsValue::from).unwrap_or_default(),
        func_sym: JsSymbol::new(Some(js_string!("wasm function"))).expect("symbol"),
    };
    let shared = Rc::new(Shared { store: RefCell::new(Store::new(&engine, host)), engine });
    STORES.with(|s| s.borrow_mut().push(Rc::downgrade(&shared)));
    let handle = WasmHandle(Rc::downgrade(&shared));
    let mut o = ObjectInitializer::new(ctx);
    for (name, f) in natives() {
        let sh = shared.clone();
        // SAFETY: the closure captures no GC-managed values directly (the
        // store's JS values are roots held outside the GC graph).
        let fun = unsafe { NativeFunction::from_closure(move |_this, args, ctx| f(&sh, args, ctx)) };
        o.function(fun, JsString::from(name), 0);
    }
    let natives_obj = o.build();
    // `WebAssembly` (wasm.js builds the namespace over the natives).
    let ns = (|| -> JsResult<JsValue> {
        let f = ctx.eval(boa_engine::Source::from_bytes(WASM_JS))?;
        let f = f.as_callable().ok_or_else(|| type_err("wasm.js"))?;
        f.call(&JsValue::undefined(), &[natives_obj.clone().into()], ctx)
    })();
    match ns {
        Ok(ns) => {
            let _ = ctx.register_global_property(js_string!("WebAssembly"), ns, Attribute::WRITABLE | Attribute::CONFIGURABLE);
        }
        Err(e) => eprintln!("[wasm] setup failed: {e}"),
    }
    handle
}
