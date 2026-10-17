// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

// The WebAssembly JS API over the natives in wasm.rs (handles into the
// realm's wasm store). Evaluated on first access to `WebAssembly`.
(function (W) {
  'use strict';
  const ID = Symbol('wasm id');
  const hidden = (o, k, v) => Object.defineProperty(o, k, { value: v, writable: false, enumerable: false, configurable: false });
  const method = (o, k, v) => Object.defineProperty(o, k, { value: v, writable: true, enumerable: true, configurable: true });

  function errorClass(name) {
    const C = class extends Error {
      constructor(message, options) { super(message, options); }
    };
    Object.defineProperty(C, 'name', { value: name });
    Object.defineProperty(C.prototype, 'name', { value: name, writable: true, enumerable: false, configurable: true });
    return C;
  }
  const CompileError = errorClass('CompileError');
  const LinkError = errorClass('LinkError');
  const RuntimeError = errorClass('RuntimeError');

  // Exported wasm functions: one JS function per wasm function.
  const funcs = new Map();
  let FUNC = null;
  function makeFunc(id) {
    let f = funcs.get(id);
    if (f) return f;
    const [n, index] = W.funcInfo(id);
    const name = String(index);
    f = { [name]: (...args) => W.call(id, args) }[name];
    Object.defineProperty(f, 'length', { value: n });
    hidden(f, FUNC, id);
    funcs.set(id, f);
    return f;
  }
  FUNC = W.init(makeFunc, CompileError, LinkError, RuntimeError);

  function bufferBytes(b, what) {
    if (b instanceof ArrayBuffer) return b.slice(0);
    if (ArrayBuffer.isView(b) && b.buffer instanceof ArrayBuffer) return b.buffer.slice(b.byteOffset, b.byteOffset + b.byteLength);
    if (typeof SharedArrayBuffer === 'function' && (b instanceof SharedArrayBuffer || (ArrayBuffer.isView(b) && b.buffer instanceof SharedArrayBuffer))) {
      const v = ArrayBuffer.isView(b) ? new Uint8Array(b.buffer, b.byteOffset, b.byteLength) : new Uint8Array(b);
      return new Uint8Array(v).buffer;
    }
    throw new TypeError(what + ': Argument 0 must be a buffer source');
  }
  // [EnforceRange] unsigned long
  function toU32(v, what, name) {
    const n = Number(v);
    if (!Number.isFinite(n)) throw new TypeError(what + ": Property '" + name + "' must be convertible to a valid number");
    const t = Math.trunc(n);
    if (t < 0 || t > 0xffffffff) throw new TypeError(what + ": Property '" + name + "' must be in the unsigned long range");
    return t;
  }
  const requireNew = (nt, what) => { if (nt === undefined) throw new TypeError(what + ' must be invoked with \'new\''); };
  const brand = (o, C, what) => { if (!o || typeof o !== 'object' || o[ID] === undefined || !(o instanceof C)) throw new TypeError(what + ': Receiver is not a ' + C.name); return o[ID]; };

  class Module {
    constructor(bytes) {
      requireNew(new.target, 'WebAssembly.Module');
      hidden(this, ID, W.compile(bufferBytes(bytes, 'WebAssembly.Module()')));
    }
    static imports(m) {
      if (!(m instanceof Module)) throw new TypeError('WebAssembly.Module.imports(): Argument 0 must be a WebAssembly.Module');
      return W.imports(m[ID]).map(([module, name, kind]) => ({ module, name, kind }));
    }
    static exports(m) {
      if (!(m instanceof Module)) throw new TypeError('WebAssembly.Module.exports(): Argument 0 must be a WebAssembly.Module');
      return W.exports(m[ID]).map(([name, kind]) => ({ name, kind }));
    }
    static customSections(m, name) {
      if (!(m instanceof Module)) throw new TypeError('WebAssembly.Module.customSections(): Argument 0 must be a WebAssembly.Module');
      if (name === undefined) throw new TypeError('WebAssembly.Module.customSections(): Argument 1 is required');
      return W.customSections(m[ID], String(name));
    }
  }
  const moduleOf = (id) => { const m = Object.create(Module.prototype); hidden(m, ID, id); return m; };

  const memories = new Map(), tables = new Map(), globals = new Map();
  const wrap = (map, C, id) => { let o = map.get(id); if (!o) { o = Object.create(C.prototype); hidden(o, ID, id); map.set(id, o); } return o; };

  function instantiateExports(module, importObject) {
    const imps = W.imports(module[ID]);
    if (imps.length && (importObject === undefined || importObject === null)) throw new TypeError('WebAssembly.Instance(): Imports argument must be present and must be an object');
    if (importObject !== undefined && (typeof importObject !== 'object' && typeof importObject !== 'function' || importObject === null)) throw new TypeError('WebAssembly.Instance(): Argument 1 must be an object');
    const vals = imps.map(([mod, name, kind], i) => {
      const o = importObject[mod];
      if (o === null || (typeof o !== 'object' && typeof o !== 'function')) throw new TypeError('WebAssembly.Instance(): Import #' + i + ' "' + mod + '": module is not an object or function');
      const v = o[name];
      const link = (msg) => new LinkError('WebAssembly.Instance(): Import #' + i + ' "' + mod + '" "' + name + '": ' + msg);
      switch (kind) {
        case 'function':
          if (typeof v !== 'function') throw link('function import requires a callable');
          return v[FUNC] !== undefined ? ['f', v[FUNC]] : ['h', v];
        case 'memory':
          if (!(v instanceof Memory) || v[ID] === undefined) throw link('memory import must be a WebAssembly.Memory object');
          return ['m', v[ID]];
        case 'table':
          if (!(v instanceof Table) || v[ID] === undefined) throw link('table import requires a WebAssembly.Table');
          return ['t', v[ID]];
        case 'global':
          if (v instanceof Global && v[ID] !== undefined) return ['g', v[ID]];
          if (typeof v === 'number' || typeof v === 'bigint') return ['v', v];
          throw link('global import must be a number, valid Wasm reference, or WebAssembly.Global object');
        default:
          throw link('unsupported import kind ' + kind);
      }
    });
    const exps = W.instantiate(module[ID], vals);
    const ex = Object.create(null);
    for (const [name, kind, id] of exps) {
      let v;
      if (kind === 'function') v = makeFunc(id);
      else if (kind === 'memory') v = wrap(memories, Memory, id);
      else if (kind === 'table') v = wrap(tables, Table, id);
      else v = wrap(globals, Global, id);
      Object.defineProperty(ex, name, { value: v, writable: false, enumerable: true, configurable: false });
    }
    return Object.freeze(ex);
  }

  const EXPORTS = new WeakMap();
  class Instance {
    constructor(module, importObject = undefined) {
      requireNew(new.target, 'WebAssembly.Instance');
      if (!(module instanceof Module)) throw new TypeError('WebAssembly.Instance(): Argument 0 must be a WebAssembly.Module');
      EXPORTS.set(this, instantiateExports(module, importObject));
    }
    get exports() {
      if (!EXPORTS.has(this)) throw new TypeError('Receiver is not a WebAssembly.Instance');
      return EXPORTS.get(this);
    }
  }
  Object.defineProperty(Instance.prototype, 'exports', { enumerable: true });

  class Memory {
    constructor(desc) {
      requireNew(new.target, 'WebAssembly.Memory');
      if (desc === null || typeof desc !== 'object') throw new TypeError('WebAssembly.Memory(): Argument 0 must be a memory descriptor');
      const what = 'WebAssembly.Memory()';
      // Dictionary members in order, each converted as it is read.
      let initial = desc.initial;
      if (initial !== undefined) initial = toU32(initial, what, 'initial');
      const max = desc.maximum;
      let maximum = max === undefined ? -1 : toU32(max, what, 'maximum');
      let minimum = desc.minimum;
      if (minimum !== undefined) minimum = toU32(minimum, what, 'minimum');
      const shared = !!desc.shared;
      if (initial !== undefined && minimum !== undefined) throw new TypeError(what + ": The properties 'initial' and 'minimum' are not allowed at the same time");
      if (initial === undefined) initial = minimum;
      if (initial === undefined) throw new TypeError(what + ": Property 'initial' is required");
      if (initial > 65536) throw new RangeError(what + ": Property 'initial': value " + initial + ' is above the upper bound 65536');
      if (maximum > 65536) throw new RangeError(what + ": Property 'maximum': value " + maximum + ' is above the upper bound 65536');
      if (maximum >= 0 && maximum < initial) throw new RangeError(what + ": Property 'maximum': value " + maximum + ' is below the lower bound ' + initial);
      if (shared && maximum < 0) throw new TypeError(what + ": If shared is true, maximum property should be defined.");
      const id = W.memNew(initial, maximum, shared);
      hidden(this, ID, id);
      memories.set(id, this);
    }
    get buffer() { return W.memBuffer(brand(this, Memory, 'WebAssembly.Memory.buffer')); }
    grow(delta) {
      const id = brand(this, Memory, 'WebAssembly.Memory.grow()');
      if (delta === undefined) throw new TypeError('WebAssembly.Memory.grow(): Argument 0 must be convertible to a valid number');
      return W.memGrow(id, toU32(delta, 'WebAssembly.Memory.grow()', 'delta'));
    }
    type() {
      const [min, max] = W.memType(brand(this, Memory, 'WebAssembly.Memory.type()'));
      const t = { minimum: min, shared: false };
      if (max !== undefined) t.maximum = max;
      return t;
    }
  }

  class Table {
    constructor(desc, value = undefined) {
      requireNew(new.target, 'WebAssembly.Table');
      const what = 'WebAssembly.Table()';
      if (desc === null || typeof desc !== 'object') throw new TypeError(what + ': Argument 0 must be a table descriptor');
      const el = String(desc.element);
      if (el !== 'anyfunc' && el !== 'funcref' && el !== 'externref') throw new TypeError(what + ": Descriptor property 'element' must be a WebAssembly reference type");
      let initial = desc.initial;
      if (initial !== undefined) initial = toU32(initial, what, 'initial');
      const max = desc.maximum;
      const maximum = max === undefined ? -1 : toU32(max, what, 'maximum');
      let minimum = desc.minimum;
      if (minimum !== undefined) minimum = toU32(minimum, what, 'minimum');
      if (initial !== undefined && minimum !== undefined) throw new TypeError(what + ": The properties 'initial' and 'minimum' are not allowed at the same time");
      if (initial === undefined) initial = minimum;
      if (initial === undefined) throw new TypeError(what + ": Property 'initial' is required");
      if (initial > 10000000) throw new RangeError(what + ": Property 'initial': value " + initial + ' is above the upper bound 10000000');
      if (maximum >= 0 && maximum < initial) throw new RangeError(what + ": Property 'maximum': value " + maximum + ' is below the lower bound ' + initial);
      const id = W.tableNew(el === 'externref' ? 'externref' : 'anyfunc', initial, maximum, ...(arguments.length < 2 ? [] : [value]));
      hidden(this, ID, id);
      tables.set(id, this);
    }
    get length() { return W.tableLength(brand(this, Table, 'WebAssembly.Table.length')); }
    get(i) {
      const id = brand(this, Table, 'WebAssembly.Table.get()');
      return W.tableGet(id, toU32(i, 'WebAssembly.Table.get()', 'index'));
    }
    set(i, v) {
      const id = brand(this, Table, 'WebAssembly.Table.set()');
      if (arguments.length < 1) throw new TypeError('WebAssembly.Table.set(): Argument 0 is required');
      W.tableSet(id, toU32(i, 'WebAssembly.Table.set()', 'index'), ...(arguments.length < 2 ? [] : [v]));
    }
    grow(delta, v) {
      const id = brand(this, Table, 'WebAssembly.Table.grow()');
      return W.tableGrow(id, toU32(delta, 'WebAssembly.Table.grow()', 'delta'), ...(arguments.length < 2 ? [] : [v]));
    }
    type() {
      const [element, min, max] = W.tableType(brand(this, Table, 'WebAssembly.Table.type()'));
      const t = { element, minimum: min };
      if (max !== undefined) t.maximum = max;
      return t;
    }
  }

  class Global {
    constructor(desc, v = undefined) {
      requireNew(new.target, 'WebAssembly.Global');
      if (desc === null || typeof desc !== 'object') throw new TypeError('WebAssembly.Global(): Argument 0 must be a global descriptor');
      const mutable = !!desc.mutable;
      const type = String(desc.value);
      if (!['i32', 'i64', 'f32', 'f64', 'anyfunc', 'funcref', 'externref'].includes(type)) throw new TypeError("WebAssembly.Global(): Descriptor property 'value' must be a WebAssembly type");
      const id = W.globalNew(type, mutable, ...(arguments.length < 2 ? [] : [v]));
      hidden(this, ID, id);
      globals.set(id, this);
    }
    get value() { return W.globalGet(brand(this, Global, 'get WebAssembly.Global.value')); }
    set value(v) {
      W.globalSet(brand(this, Global, 'set WebAssembly.Global.value'), v);
    }
    valueOf() { return W.globalGet(brand(this, Global, 'WebAssembly.Global.valueOf()')); }
    type() {
      const [value, mutable] = W.globalType(brand(this, Global, 'WebAssembly.Global.type()'));
      return { value, mutable };
    }
  }

  const P_RESOLVE = Promise.resolve.bind(Promise), P_REJECT = Promise.reject.bind(Promise);
  // Compile/instantiate off the current task, as browsers do.
  const later = (fn) => new Promise((res, rej) => { setTimeout(() => { try { res(fn()); } catch (e) { rej(e); } }, 0); });

  function validate(bytes) { return W.validate(bufferBytes(bytes, 'WebAssembly.validate()')); }
  function compile(bytes) {
    let b;
    try { b = bufferBytes(bytes, 'WebAssembly.compile()'); } catch (e) { return P_REJECT(e); }
    return later(() => moduleOf(W.compile(b)));
  }
  function instantiate(src, importObject = undefined) {
    if (src instanceof Module) {
      if (importObject !== undefined && (importObject === null || (typeof importObject !== 'object' && typeof importObject !== 'function'))) return P_REJECT(new TypeError('WebAssembly.instantiate(): Argument 1 must be an object'));
      return later(() => { const i = Object.create(Instance.prototype); EXPORTS.set(i, instantiateExports(src, importObject)); return i; });
    }
    let b;
    try { b = bufferBytes(src, 'WebAssembly.instantiate()'); } catch (e) { return P_REJECT(e); }
    if (importObject !== undefined && (importObject === null || (typeof importObject !== 'object' && typeof importObject !== 'function'))) return P_REJECT(new TypeError('WebAssembly.instantiate(): Argument 1 must be an object'));
    return later(() => {
      const module = moduleOf(W.compile(b));
      const instance = Object.create(Instance.prototype);
      EXPORTS.set(instance, instantiateExports(module, importObject));
      return { module, instance };
    });
  }
  async function responseBytes(source, what) {
    const resp = await source;
    if (typeof Response !== 'function' || !(resp instanceof Response)) throw new TypeError(what + ': Argument 0 must be provided and must be a Response');
    const ct = (resp.headers.get('content-type') || '').split(';')[0].trim().toLowerCase();
    if (ct !== 'application/wasm') throw new TypeError(what + ": Incorrect response MIME type. Expected 'application/wasm'.");
    if (!resp.ok) throw new TypeError(what + ': HTTP status code is not ok');
    if (resp.bodyUsed) throw new TypeError(what + ': Body has already been used');
    return resp.arrayBuffer();
  }
  function compileStreaming(source) { return responseBytes(source, 'WebAssembly.compileStreaming()').then((b) => moduleOf(W.compile(b))); }
  function instantiateStreaming(source, importObject = undefined) {
    return responseBytes(source, 'WebAssembly.instantiateStreaming()').then((b) => instantiate(b, importObject));
  }

  const WA = {};
  for (const [k, v] of Object.entries({ Module, Instance, Memory, Table, Global, CompileError, LinkError, RuntimeError })) Object.defineProperty(WA, k, { value: v, writable: true, enumerable: false, configurable: true });
  for (const [k, v] of Object.entries({ validate, compile, instantiate, compileStreaming, instantiateStreaming })) {
    Object.defineProperty(v, 'name', { value: k });
    method(WA, k, v);
  }
  for (const k of ['imports', 'exports', 'customSections']) {
    const d = Object.getOwnPropertyDescriptor(Module, k);
    d.enumerable = true;
    Object.defineProperty(Module, k, d);
  }
  for (const C of [Module, Instance, Memory, Table, Global]) {
    Object.defineProperty(C.prototype, Symbol.toStringTag, { value: 'WebAssembly.' + C.name, configurable: true });
    for (const k of Object.getOwnPropertyNames(C.prototype)) {
      if (k === 'constructor') continue;
      const d = Object.getOwnPropertyDescriptor(C.prototype, k);
      d.enumerable = true;
      Object.defineProperty(C.prototype, k, d);
    }
  }
  Object.defineProperty(WA, Symbol.toStringTag, { value: 'WebAssembly', configurable: true });
  return WA;
})
