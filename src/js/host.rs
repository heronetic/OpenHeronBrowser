// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! The engine-neutral host boundary.
//!
//! Everything the page exposes to scripts goes through plain data
//! ([`HostValue`]) and argument readers ([`HostArgs`]). Native functions in
//! `natives.rs` and the DOM/BOM API in `prelude.js` know nothing about the
//! JavaScript engine; only the adapter (`engine_boa.rs` today, a V8 adapter
//! later) converts between these types and engine values.

use crate::dom::NodeId;
use crate::page::with_page;

/// A value crossing the native boundary.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum HostValue {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<HostValue>),
    /// A plain object (own enumerable properties, in order).
    Object(Vec<(String, HostValue)>),
    /// Binary data; becomes an `ArrayBuffer` in script.
    Bytes(Vec<u8>),
}

impl HostValue {
    pub fn is_nullish(&self) -> bool {
        matches!(self, HostValue::Undefined | HostValue::Null)
    }
    /// ECMAScript ToBoolean.
    pub fn truthy(&self) -> bool {
        match self {
            HostValue::Undefined | HostValue::Null => false,
            HostValue::Bool(b) => *b,
            HostValue::Number(n) => *n != 0.0 && !n.is_nan(),
            HostValue::String(s) => !s.is_empty(),
            _ => true,
        }
    }
    /// ECMAScript ToNumber for primitives (NaN otherwise).
    pub fn number(&self) -> f64 {
        match self {
            HostValue::Number(n) => *n,
            HostValue::Bool(b) => *b as u8 as f64,
            HostValue::Null => 0.0,
            HostValue::String(s) => {
                let t = s.trim();
                if t.is_empty() {
                    0.0
                } else {
                    t.parse().unwrap_or(f64::NAN)
                }
            }
            _ => f64::NAN,
        }
    }
    /// ECMAScript ToString for primitives.
    pub fn string(&self) -> String {
        match self {
            HostValue::Undefined => "undefined".into(),
            HostValue::Null => "null".into(),
            HostValue::Bool(b) => b.to_string(),
            HostValue::Number(n) => number_to_string(*n),
            HostValue::String(s) => s.clone(),
            HostValue::Array(v) => v.iter().map(|x| if x.is_nullish() { String::new() } else { x.string() }).collect::<Vec<_>>().join(","),
            HostValue::Object(_) => "[object Object]".into(),
            HostValue::Bytes(_) => "[object ArrayBuffer]".into(),
        }
    }
    pub fn as_array(&self) -> Option<&[HostValue]> {
        match self {
            HostValue::Array(v) => Some(v),
            _ => None,
        }
    }
    /// Property of a plain object.
    pub fn get(&self, key: &str) -> Option<&HostValue> {
        match self {
            HostValue::Object(v) => v.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn object(props: Vec<(&str, HostValue)>) -> HostValue {
        HostValue::Object(props.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
    }
}

/// ECMAScript Number::toString for the common cases.
pub fn number_to_string(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.is_infinite() {
        if n > 0.0 { "Infinity".into() } else { "-Infinity".into() }
    } else if n == n.trunc() && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

macro_rules! from_num {
    ($($t:ty),*) => {$(
        impl From<$t> for HostValue {
            fn from(v: $t) -> HostValue {
                HostValue::Number(v as f64)
            }
        }
    )*};
}
from_num!(i32, u32, i64, u64, usize, isize, f32, f64, u8, u16, i16);

impl From<bool> for HostValue {
    fn from(v: bool) -> HostValue {
        HostValue::Bool(v)
    }
}
impl From<String> for HostValue {
    fn from(v: String) -> HostValue {
        HostValue::String(v)
    }
}
impl From<&str> for HostValue {
    fn from(v: &str) -> HostValue {
        HostValue::String(v.to_string())
    }
}
impl<T: Into<HostValue>> From<Option<T>> for HostValue {
    fn from(v: Option<T>) -> HostValue {
        match v {
            Some(v) => v.into(),
            None => HostValue::Null,
        }
    }
}
impl<T: Into<HostValue>> From<Vec<T>> for HostValue {
    fn from(v: Vec<T>) -> HostValue {
        HostValue::Array(v.into_iter().map(Into::into).collect())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HostErrorKind {
    Error,
    TypeError,
    RangeError,
    SyntaxError,
}

/// An exception thrown by a native function.
#[derive(Clone, Debug)]
pub struct HostError {
    pub kind: HostErrorKind,
    pub message: String,
}

impl HostError {
    pub fn type_error(m: impl Into<String>) -> HostError {
        HostError { kind: HostErrorKind::TypeError, message: m.into() }
    }
    pub fn range_error(m: impl Into<String>) -> HostError {
        HostError { kind: HostErrorKind::RangeError, message: m.into() }
    }
    pub fn error(m: impl Into<String>) -> HostError {
        HostError { kind: HostErrorKind::Error, message: m.into() }
    }
}

pub type HostResult = Result<HostValue, HostError>;

/// The arguments of a native call, read with JavaScript's conversions.
pub trait HostArgs {
    fn len(&self) -> usize;
    /// ToNumber; NaN when missing, `undefined` or `null`.
    fn number(&mut self, i: usize) -> f64;
    /// ToString; `None` when missing, `undefined` or `null`.
    fn string(&mut self, i: usize) -> Option<String>;
    /// ToBoolean; `default` when missing.
    fn boolean_or(&self, i: usize, default: bool) -> bool;
    fn is_object(&self, i: usize) -> bool;
    /// Deep conversion to plain data: arrays and array-likes become `Array`,
    /// typed arrays and buffers `Bytes`, other objects `Object`.
    fn value(&mut self, i: usize) -> Result<HostValue, HostError>;
}

/// A native function: plain data in, plain data out.
pub type NativeFn = fn(&mut dyn HostArgs) -> HostResult;

// ------------------------------------------------------------ argument helpers

pub fn num(a: &mut dyn HostArgs, i: usize) -> f64 {
    a.number(i)
}

pub fn st(a: &mut dyn HostArgs, i: usize) -> String {
    a.string(i).unwrap_or_default()
}

pub fn opt_st(a: &mut dyn HostArgs, i: usize) -> Option<String> {
    a.string(i)
}

pub fn boolean(a: &mut dyn HostArgs, i: usize, default: bool) -> bool {
    a.boolean_or(i, default)
}

/// A node id argument that names a live node of the page's document.
pub fn nid(a: &mut dyn HostArgs, i: usize) -> Option<NodeId> {
    let n = a.number(i);
    if n.is_finite() && n >= 0.0 {
        let id = n as usize;
        if with_page(|p| p.doc.valid(id)).unwrap_or(false) {
            return Some(id);
        }
    }
    None
}

/// Numbers of an array argument (empty when it isn't one).
pub fn f32s(a: &mut dyn HostArgs, i: usize) -> Result<Vec<f32>, HostError> {
    Ok(match a.value(i)? {
        HostValue::Array(v) => v.iter().map(|x| x.number() as f32).collect(),
        HostValue::Bytes(b) => b.iter().map(|&x| x as f32).collect(),
        _ => Vec::new(),
    })
}

pub fn jstr(s: &str) -> HostValue {
    HostValue::String(s.to_string())
}

pub fn jopt(s: Option<String>) -> HostValue {
    match s {
        Some(s) => HostValue::String(s),
        None => HostValue::Null,
    }
}

pub fn jid(n: Option<NodeId>) -> HostValue {
    HostValue::Number(n.map(|x| x as f64).unwrap_or(-1.0))
}

pub fn jids(v: Vec<NodeId>) -> HostValue {
    HostValue::Array(v.into_iter().map(|i| HostValue::Number(i as f64)).collect())
}

pub fn jnums(v: &[f64]) -> HostValue {
    HostValue::Array(v.iter().map(|&x| HostValue::Number(x)).collect())
}

pub fn jstrs(v: Vec<String>) -> HostValue {
    HostValue::Array(v.into_iter().map(HostValue::String).collect())
}

/// Response bodies kept for `arrayBuffer()`/`blob()`, oldest dropped first.
#[derive(Default)]
pub struct BodyStore {
    order: std::collections::VecDeque<u64>,
    map: std::collections::HashMap<u64, std::sync::Arc<Vec<u8>>>,
    bytes: usize,
}

impl BodyStore {
    const MAX_BYTES: usize = 256 << 20;
    pub fn insert(&mut self, id: u64, body: std::sync::Arc<Vec<u8>>) {
        self.bytes += body.len();
        self.map.insert(id, body);
        self.order.push_back(id);
        while (self.bytes > Self::MAX_BYTES || self.order.len() > 4096) && self.order.len() > 1 {
            if let Some(old) = self.order.pop_front() {
                if let Some(b) = self.map.remove(&old) {
                    self.bytes -= b.len();
                }
            }
        }
    }
    pub fn take(&mut self, id: u64) -> Option<std::sync::Arc<Vec<u8>>> {
        let b = self.map.remove(&id)?;
        self.bytes -= b.len();
        self.order.retain(|&x| x != id);
        Some(b)
    }
}

thread_local! {
    static PAGE_START: std::time::Instant = std::time::Instant::now();
}

/// Milliseconds since the script thread started (`performance.now()`'s clock).
pub fn page_now_ms() -> f64 {
    PAGE_START.with(|s| s.elapsed().as_secs_f64() * 1000.0)
}

thread_local! {
    static TIMELINE_FROZEN: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

/// The document timeline's time: constant while a task runs (animations and
/// transitions started and sampled in one task see the same time), live otherwise.
pub fn timeline_ms() -> f64 {
    TIMELINE_FROZEN.with(|c| c.get()).unwrap_or_else(page_now_ms)
}

/// A task begins (`Some(t)`: at that timeline time) or ends (`None`).
pub fn set_timeline_frozen(t: Option<f64>) {
    TIMELINE_FROZEN.with(|c| c.set(t));
}

/// The engine side of the boundary: what the page runtime needs from a
/// JavaScript engine. Implemented by `engine_boa::BoaEngine`.
pub trait ScriptEngine {
    /// Register the native functions (as the hidden `__n` object).
    fn install_natives(&mut self, natives: &[(&'static str, NativeFn, u32)]);
    /// Evaluate a classic script; `path` is its URL (for stack traces and
    /// relative `import()`). Errors are returned as display strings.
    fn run_classic(&mut self, src: &str, path: &str) -> Result<HostValue, String>;
    /// Load, link and evaluate a module script with the given module-map key.
    fn run_module(&mut self, src: &str, key: &str) -> Result<(), String>;
    /// Call a global function by name.
    fn call_global(&mut self, name: &str, args: &[HostValue]) -> Result<HostValue, String>;
    /// Evaluate code and render the result like DevTools' console (JSON for objects).
    fn eval_to_string(&mut self, code: &str) -> String;
    /// Run pending promise jobs (microtasks) and module loading.
    fn run_jobs(&mut self);
    /// Promises rejected without a handler since the last call, with their reasons
    /// (as engine-held handles passed back to `__ohb_unhandledRejection`).
    fn report_unhandled_rejections(&mut self, handler: &str);
    /// Base URL and import map for module resolution.
    fn set_base_url(&mut self, url: Option<url::Url>);
    fn add_import_map(&mut self, json: &str);
}
