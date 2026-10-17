// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

// HeronScript IndexedDB: an in-memory implementation of the IndexedDB API.
//
// Databases live for the lifetime of the tab's page, like OHB's other storage
// (cookies, localStorage), and are not written to disk. The object model
// follows the IndexedDB 3.0 spec: factories, versioned databases with
// upgradeneeded transactions, object stores with key paths and key
// generators, unique and multiEntry indexes, key ranges, cursors in all four
// directions, and transactions that commit when they run out of requests or
// roll back on abort.
//
// Scheduling differs from browsers in one deliberate way. Boa runs promise
// jobs only after the outermost script returns, so a transaction stays open
// until a full timer turn passes with no new requests. That keeps promise
// wrappers (idb, Dexie, localForage) working, at the cost of not raising
// TransactionInactiveError for requests made from a later task.
(function (global) {
  'use strict';
  if (global.indexedDB) return;
  const DOMEx = global.DOMException;
  const clone = global.structuredClone;
  const later = (fn) => global.setTimeout(fn, 0);
  const err = (name, msg) => new DOMEx(msg || name, name);

  // ---------------------------------------------------------------- keys
  // Key types in spec order: number < date < string < binary < array.
  function keyType(k) {
    if (typeof k === 'number') return Number.isNaN(k) ? 0 : 1;
    if (k instanceof Date) return Number.isNaN(k.getTime()) ? 0 : 2;
    if (typeof k === 'string') return 3;
    if (k instanceof ArrayBuffer || ArrayBuffer.isView(k)) return 4;
    if (Array.isArray(k)) return 5;
    return 0;
  }
  function bytesOf(k) {
    return k instanceof ArrayBuffer ? new Uint8Array(k) : new Uint8Array(k.buffer, k.byteOffset, k.byteLength);
  }
  // Convert a JS value to a key, or return undefined if it is not a valid key.
  function toKey(v, seen) {
    switch (keyType(v)) {
      case 1: case 3: return v;
      case 2: return new Date(v.getTime());
      case 4: return bytesOf(v).slice().buffer;
      case 5: {
        seen = seen || new Set();
        if (seen.has(v)) return undefined;
        seen.add(v);
        const out = [];
        for (let i = 0; i < v.length; i++) {
          if (!(i in v)) return undefined;
          const k = toKey(v[i], seen);
          if (k === undefined) return undefined;
          out.push(k);
        }
        seen.delete(v);
        return out;
      }
      default: return undefined;
    }
  }
  function mustKey(v) {
    const k = toKey(v);
    if (k === undefined) throw err('DataError', 'The parameter is not a valid key.');
    return k;
  }
  function cmp(a, b) {
    const ta = keyType(a), tb = keyType(b);
    if (ta !== tb) return ta < tb ? -1 : 1;
    switch (ta) {
      case 1: case 3: return a < b ? -1 : a > b ? 1 : 0;
      case 2: { const x = a.getTime(), y = b.getTime(); return x < y ? -1 : x > y ? 1 : 0; }
      case 4: {
        const x = bytesOf(a), y = bytesOf(b), n = Math.min(x.length, y.length);
        for (let i = 0; i < n; i++) if (x[i] !== y[i]) return x[i] < y[i] ? -1 : 1;
        return x.length === y.length ? 0 : x.length < y.length ? -1 : 1;
      }
      case 5: {
        const n = Math.min(a.length, b.length);
        for (let i = 0; i < n; i++) { const c = cmp(a[i], b[i]); if (c) return c; }
        return a.length === b.length ? 0 : a.length < b.length ? -1 : 1;
      }
    }
    return 0;
  }

  // ---------------------------------------------------------------- key paths
  function validKeyPath(kp) {
    if (typeof kp === 'string') return kp === '' || kp.split('.').every((p) => /^[$A-Z_a-z][$\w]*$/.test(p));
    return Array.isArray(kp) && kp.length > 0 && kp.every((p) => typeof p === 'string' && validKeyPath(p));
  }
  function evalPath(value, kp) {
    if (Array.isArray(kp)) {
      const out = [];
      for (const p of kp) { const k = evalPath(value, p); if (k === undefined) return undefined; out.push(k); }
      return out;
    }
    if (kp === '') return toKey(value);
    let v = value;
    for (const part of kp.split('.')) {
      if (v === null || v === undefined) return undefined;
      if (typeof v === 'string' && part === 'length') { v = v.length; continue; }
      if (typeof v !== 'object' || !(part in Object(v))) return undefined;
      v = v[part];
    }
    return toKey(v);
  }
  function injectKey(value, kp, key) {
    const parts = kp.split('.');
    let v = value;
    for (let i = 0; i < parts.length - 1; i++) {
      if (v[parts[i]] === undefined) v[parts[i]] = {};
      v = v[parts[i]];
      if (typeof v !== 'object' || v === null) throw err('DataError', 'Cannot inject key into value.');
    }
    v[parts[parts.length - 1]] = key;
  }
  const copyPath = (kp) => (Array.isArray(kp) ? kp.slice() : kp);

  // ---------------------------------------------------------------- key ranges
  class IDBKeyRange {
    constructor(lower, upper, lowerOpen, upperOpen) {
      this._l = lower; this._u = upper; this._lo = !!lowerOpen; this._uo = !!upperOpen;
    }
    get lower() { return this._l; }
    get upper() { return this._u; }
    get lowerOpen() { return this._lo; }
    get upperOpen() { return this._uo; }
    includes(key) {
      key = mustKey(key);
      if (this._l !== undefined) { const c = cmp(key, this._l); if (c < 0 || (c === 0 && this._lo)) return false; }
      if (this._u !== undefined) { const c = cmp(key, this._u); if (c > 0 || (c === 0 && this._uo)) return false; }
      return true;
    }
    static only(v) { v = mustKey(v); return new IDBKeyRange(v, v, false, false); }
    static lowerBound(v, open) { return new IDBKeyRange(mustKey(v), undefined, open, true); }
    static upperBound(v, open) { return new IDBKeyRange(undefined, mustKey(v), true, open); }
    static bound(l, u, lo, uo) {
      l = mustKey(l); u = mustKey(u);
      const c = cmp(l, u);
      if (c > 0 || (c === 0 && (lo || uo))) throw err('DataError', 'The lower key is greater than the upper key.');
      return new IDBKeyRange(l, u, lo, uo);
    }
  }
  // A query argument: undefined/null (everything), a key, or a range.
  function toRange(q, nullOk = true) {
    if (q instanceof IDBKeyRange) return q;
    if (q === undefined || (q === null && nullOk)) return null;
    return IDBKeyRange.only(q);
  }
  const inRange = (r, k) => !r || r.includes(k);

  // ---------------------------------------------------------------- sorted record lists
  // records: [{ key, value }] sorted by key (stores) or by (key, primaryKey) (indexes).
  function lowerBoundIdx(arr, key, pk) {
    let lo = 0, hi = arr.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      let c = cmp(arr[mid].key, key);
      if (c === 0 && pk !== undefined) c = cmp(arr[mid].pk, pk);
      if (c < 0) lo = mid + 1; else hi = mid;
    }
    return lo;
  }
  function findIdx(arr, key) {
    const i = lowerBoundIdx(arr, key);
    return i < arr.length && cmp(arr[i].key, key) === 0 ? i : -1;
  }

  // ---------------------------------------------------------------- DOMStringList
  function stringList(names) {
    const list = names.slice().sort();
    Object.defineProperty(list, 'contains', { value: (s) => list.includes(String(s)) });
    Object.defineProperty(list, 'item', { value: (i) => (i >= 0 && i < list.length ? list[i] : null) });
    return list;
  }

  // ---------------------------------------------------------------- events
  class IDBVersionChangeEvent extends Event {
    constructor(type, init) { super(type, init); init = init || {}; this.oldVersion = init.oldVersion || 0; this.newVersion = init.newVersion === undefined ? null : init.newVersion; }
  }
  function fire(target, type, opts) {
    const ev = opts && opts.versions
      ? new IDBVersionChangeEvent(type, Object.assign({ bubbles: false, cancelable: false }, opts.versions))
      : new Event(type, { bubbles: !!(opts && opts.bubbles), cancelable: !!(opts && opts.cancelable) });
    target.dispatchEvent(ev);
    return ev;
  }
  function handlers(proto, names) {
    for (const n of names) {
      Object.defineProperty(proto, 'on' + n, {
        get() { return this['_on' + n] || null; },
        set(f) { this['_on' + n] = typeof f === 'function' ? f : null; },
        configurable: true,
      });
    }
  }

  // ---------------------------------------------------------------- storage model
  const databases = new Map(); // name -> { name, version, stores: Map, connections: Set }

  function newStoreData(name, keyPath, autoIncrement) {
    return { name, keyPath, autoIncrement, gen: 1, records: [], indexes: new Map() };
  }
  function copyStoreData(s) {
    const c = Object.assign({}, s, { records: s.records.slice(), indexes: new Map() });
    for (const [n, ix] of s.indexes) c.indexes.set(n, Object.assign({}, ix, { records: ix.records.slice() }));
    return c;
  }
  function indexKeys(ix, value) {
    const k = evalPath(value, ix.keyPath);
    if (k !== undefined) return ix.multiEntry && Array.isArray(k) ? dedupe(k) : [k];
    if (ix.multiEntry && !Array.isArray(ix.keyPath)) {
      // multiEntry: invalid array members are skipped, not fatal.
      let v = value;
      for (const part of ix.keyPath.split('.')) { if (v == null || typeof v !== 'object') return []; v = v[part]; }
      if (Array.isArray(v)) return dedupe(v.map((x) => toKey(x)).filter((x) => x !== undefined));
    }
    return [];
  }
  function dedupe(keys) {
    const out = [];
    for (const k of keys) if (!out.some((x) => cmp(x, k) === 0)) out.push(k);
    return out;
  }
  function removeFromIndexes(store, pk) {
    for (const ix of store.indexes.values()) ix.records = ix.records.filter((r) => cmp(r.pk, pk) !== 0);
  }
  function addToIndex(ix, value, pk) {
    for (const k of indexKeys(ix, value)) ix.records.splice(lowerBoundIdx(ix.records, k, pk), 0, { key: k, pk });
  }

  // ---------------------------------------------------------------- requests
  class IDBRequest extends EventTarget {
    constructor(source, tx) {
      super();
      this._source = source; this._tx = tx || null; this._ready = 'pending'; this._result = undefined; this._error = null;
    }
    get result() { if (this._ready !== 'done') throw err('InvalidStateError', 'The request has not finished.'); return this._result; }
    get error() { if (this._ready !== 'done') throw err('InvalidStateError', 'The request has not finished.'); return this._error; }
    get source() { return this._source; }
    get transaction() { return this._tx; }
    get readyState() { return this._ready; }
    get _eventParent() { return this._tx; }
  }
  handlers(IDBRequest.prototype, ['success', 'error']);
  class IDBOpenDBRequest extends IDBRequest {
    get _eventParent() { return null; }
  }
  handlers(IDBOpenDBRequest.prototype, ['blocked', 'upgradeneeded']);

  // Complete a request: set result/error and fire success/error.
  // Returns false if an unhandled error should abort the transaction.
  function settle(req, fn) {
    let result, error = null;
    try { result = fn(); } catch (e) { error = e instanceof DOMEx ? e : err('UnknownError', String(e && e.message || e)); }
    req._ready = 'done';
    if (error) {
      req._result = undefined; req._error = error;
      const ev = fire(req, 'error', { bubbles: true, cancelable: true });
      return ev.defaultPrevented;
    }
    req._result = result; req._error = null;
    fire(req, 'success');
    return true;
  }

  // ---------------------------------------------------------------- transactions
  class IDBTransaction extends EventTarget {
    constructor(db, names, mode, upgrade) {
      super();
      this._db = db; this._names = names; this._mode = mode; this._upgrade = !!upgrade;
      this._queue = []; this._state = 'active'; this._error = null; this._stores = new Map();
      this._snap = new Map(); // store name -> copy, taken before the first write
      this._idle = 0;
      db._txs.add(this);
      later(() => this._pump());
    }
    get db() { return this._db; }
    get mode() { return this._mode; }
    get error() { return this._error; }
    get durability() { return 'default'; }
    get objectStoreNames() { return stringList(this._upgrade ? [...this._db._data.stores.keys()] : this._names); }
    get _eventParent() { return this._db; }
    objectStore(name) {
      if (this._state === 'finished') throw err('InvalidStateError', 'The transaction has finished.');
      name = String(name);
      if (!(this._upgrade ? this._db._data.stores.has(name) : this._names.includes(name))) throw err('NotFoundError', `No objectStore named ${name} in this transaction.`);
      let s = this._stores.get(name);
      if (!s) { s = new IDBObjectStore(this, name); this._stores.set(name, s); }
      return s;
    }
    abort() {
      if (this._state === 'finished' || this._state === 'committing') throw err('InvalidStateError', 'The transaction has finished.');
      this._abort(null);
    }
    commit() {
      if (this._state !== 'active') throw err('InvalidStateError', 'The transaction is not active.');
      this._state = 'committing';
    }
    _check(write) {
      if (this._state !== 'active') throw err('TransactionInactiveError', 'The transaction is not active.');
      if (write && this._mode === 'readonly') throw err('ReadOnlyError', 'The transaction is read-only.');
    }
    _data(name) {
      const d = this._db._data.stores.get(name);
      if (!d) throw err('InvalidStateError', 'The object store has been deleted.');
      return d;
    }
    _writable(name) {
      if (!this._snap.has(name) && !this._upgrade) this._snap.set(name, copyStoreData(this._data(name)));
      return this._data(name);
    }
    _request(source, fn, req) {
      req = req || new IDBRequest(source, this);
      this._queue.push({ req, fn });
      return req;
    }
    _pump() {
      if (this._state === 'finished') return;
      if (this._queue.length) {
        this._idle = 0;
        while (this._queue.length && this._state !== 'finished') {
          const { req, fn } = this._queue.shift();
          if (!settle(req, fn)) { this._abort(req._error); return; }
        }
        later(() => this._pump());
        return;
      }
      // Give promise callbacks (which run after this task) one more turn to
      // queue follow-up requests before committing.
      if (this._state === 'active' && this._idle++ < 1) { later(() => this._pump()); return; }
      this._finish();
    }
    _finish() {
      this._state = 'finished';
      this._db._txs.delete(this);
      fire(this, 'complete');
      if (this._onDone) this._onDone(true);
    }
    _abort(error) {
      if (this._state === 'finished') return;
      for (const [name, copy] of this._snap) this._db._data.stores.set(name, copy);
      if (this._upgrade && this._schema) {
        this._db._data.stores = this._schema.stores;
        this._db._data.version = this._schema.version;
      }
      this._state = 'finished';
      this._error = error || null;
      for (const { req } of this._queue) {
        req._ready = 'done'; req._result = undefined; req._error = err('AbortError', 'The transaction was aborted.');
        fire(req, 'error', { bubbles: true, cancelable: true });
      }
      this._queue = [];
      this._db._txs.delete(this);
      later(() => {
        fire(this, 'abort', { bubbles: true });
        if (this._onDone) this._onDone(false);
      });
    }
  }
  handlers(IDBTransaction.prototype, ['complete', 'error', 'abort']);

  // ---------------------------------------------------------------- database connections
  class IDBDatabase extends EventTarget {
    constructor(data) { super(); this._data = data; this._closed = false; this._txs = new Set(); this._version = data.version; data.connections.add(this); }
    get name() { return this._data.name; }
    get version() { return this._version; }
    get objectStoreNames() { return stringList([...this._data.stores.keys()]); }
    _upgradeTx() {
      for (const t of this._txs) if (t._upgrade && t._state !== 'finished') return t;
      return null;
    }
    createObjectStore(name, options) {
      const tx = this._upgradeTx();
      if (!tx) throw err('InvalidStateError', 'Not in a versionchange transaction.');
      name = String(name);
      options = options || {};
      const keyPath = options.keyPath === undefined || options.keyPath === null ? null : copyPath(options.keyPath);
      if (keyPath !== null && !validKeyPath(keyPath)) throw err('SyntaxError', 'The keyPath is not valid.');
      if (this._data.stores.has(name)) throw err('ConstraintError', `An object store named ${name} already exists.`);
      if (options.autoIncrement && (keyPath === '' || Array.isArray(keyPath))) throw err('InvalidAccessError', 'autoIncrement needs a non-empty string keyPath.');
      this._data.stores.set(name, newStoreData(name, keyPath, !!options.autoIncrement));
      return tx.objectStore(name);
    }
    deleteObjectStore(name) {
      const tx = this._upgradeTx();
      if (!tx) throw err('InvalidStateError', 'Not in a versionchange transaction.');
      if (!this._data.stores.delete(String(name))) throw err('NotFoundError', `No objectStore named ${name}.`);
      tx._stores.delete(String(name));
    }
    transaction(names, mode, options) {
      if (this._closed) throw err('InvalidStateError', 'The database connection is closing.');
      if (this._upgradeTx()) throw err('InvalidStateError', 'A version change transaction is running.');
      names = typeof names === 'string' ? [names] : Array.from(names || [], String);
      names = [...new Set(names)];
      if (!names.length) throw err('InvalidAccessError', 'The storeNames parameter was empty.');
      for (const n of names) if (!this._data.stores.has(n)) throw err('NotFoundError', `No objectStore named ${n}.`);
      mode = mode === undefined ? 'readonly' : String(mode);
      if (mode !== 'readonly' && mode !== 'readwrite') throw new TypeError(`Invalid transaction mode: ${mode}`);
      return new IDBTransaction(this, names, mode, false);
    }
    close() { this._closed = true; this._data.connections.delete(this); }
  }
  handlers(IDBDatabase.prototype, ['abort', 'close', 'error', 'versionchange']);

  // ---------------------------------------------------------------- object stores and indexes
  function getAllFrom(records, range, count, pick) {
    const out = [];
    const start = range && range.lower !== undefined ? lowerBoundIdx(records, range.lower) : 0;
    for (let i = start; i < records.length; i++) {
      const r = records[i];
      if (range && range.upper !== undefined) { const c = cmp(r.key, range.upper); if (c > 0 || (c === 0 && range.upperOpen)) break; }
      if (!inRange(range, r.key)) continue;
      out.push(pick(r));
      if (count && out.length >= count) break;
    }
    return out;
  }
  function countArg(c) {
    if (c === undefined || c === 0) return 0;
    c = Number(c);
    if (!(c >= 0) || c > 4294967295) throw new TypeError('Invalid count.');
    return Math.floor(c);
  }
  // getAll/getAllKeys accept (query, count) or an options object (IDB 3.1).
  function getAllArgs(q, count) {
    if (q && typeof q === 'object' && !(q instanceof IDBKeyRange) && !Array.isArray(q) && keyType(q) === 0) {
      return [toRange(q.query), countArg(q.count), q.direction || 'next'];
    }
    return [toRange(q), countArg(count), 'next'];
  }

  class IDBObjectStore {
    constructor(tx, name) { this._tx = tx; this._name = name; this._ix = new Map(); }
    get _d() { return this._tx._data(this._name); }
    get name() { return this._name; }
    set name(v) {
      if (!this._tx._upgrade) throw err('InvalidStateError', 'Not in a versionchange transaction.');
      v = String(v);
      if (v === this._name) return;
      const stores = this._tx._db._data.stores;
      if (stores.has(v)) throw err('ConstraintError', `An object store named ${v} already exists.`);
      const d = stores.get(this._name); stores.delete(this._name); d.name = v; stores.set(v, d);
      this._tx._stores.delete(this._name); this._name = v; this._tx._stores.set(v, this);
    }
    get keyPath() { return copyPath(this._d.keyPath); }
    get autoIncrement() { return this._d.autoIncrement; }
    get indexNames() { return stringList([...this._d.indexes.keys()]); }
    get transaction() { return this._tx; }

    _store(value, key, noOverwrite) {
      this._tx._check(true);
      const d = this._d;
      if (d.keyPath !== null && key !== undefined) throw err('DataError', 'The object store uses in-line keys and the key parameter was provided.');
      if (d.keyPath === null && !d.autoIncrement && key === undefined) throw err('DataError', 'The object store uses out-of-line keys and has no key generator and the key parameter was not provided.');
      if (key !== undefined) key = mustKey(key);
      const v = clone(value);
      if (d.keyPath !== null) {
        key = evalPath(v, d.keyPath);
        if (key === undefined) {
          if (!d.autoIncrement) throw err('DataError', 'Evaluating the object store\'s key path did not yield a value.');
          if (v === null || typeof v !== 'object') throw err('DataError', 'Cannot inject a generated key into a primitive value.');
        }
      }
      const name = this._name, tx = this._tx;
      return tx._request(this, () => {
        const s = tx._writable(name);
        let k = key;
        if (k === undefined) {
          if (s.gen > 9007199254740992) throw err('ConstraintError', 'The key generator has reached its maximum value.');
          k = s.gen++;
          if (s.keyPath !== null) injectKey(v, s.keyPath, k);
        } else if (s.autoIncrement && typeof k === 'number' && k >= s.gen) {
          s.gen = Math.floor(k) + 1;
        }
        const at = findIdx(s.records, k);
        if (at >= 0 && noOverwrite) throw err('ConstraintError', 'Key already exists in the object store.');
        for (const ix of s.indexes.values()) {
          if (!ix.unique) continue;
          for (const ik of indexKeys(ix, v)) {
            const j = findIdx(ix.records, ik);
            if (j >= 0 && cmp(ix.records[j].pk, k) !== 0) throw err('ConstraintError', `Unable to add key to index '${ix.name}': at least one key does not satisfy the uniqueness requirements.`);
          }
        }
        if (at >= 0) { s.records[at] = { key: k, value: v }; removeFromIndexes(s, k); }
        else s.records.splice(lowerBoundIdx(s.records, k), 0, { key: k, value: v });
        for (const ix of s.indexes.values()) addToIndex(ix, v, k);
        return k;
      });
    }
    put(value, key) { return this._store(value, key, false); }
    add(value, key) { return this._store(value, key, true); }
    delete(query) {
      this._tx._check(true);
      const range = toRange(query, false);
      const name = this._name, tx = this._tx;
      return tx._request(this, () => {
        const s = tx._writable(name);
        const keep = [];
        for (const r of s.records) { if (inRange(range, r.key)) removeFromIndexes(s, r.key); else keep.push(r); }
        s.records = keep;
        return undefined;
      });
    }
    clear() {
      this._tx._check(true);
      const name = this._name, tx = this._tx;
      return tx._request(this, () => {
        const s = tx._writable(name);
        s.records = [];
        for (const ix of s.indexes.values()) ix.records = [];
        return undefined;
      });
    }
    get(query) {
      this._tx._check(false);
      const range = toRange(query, false);
      return this._tx._request(this, () => { const r = getAllFrom(this._d.records, range, 1, (x) => x)[0]; return r ? clone(r.value) : undefined; });
    }
    getKey(query) {
      this._tx._check(false);
      const range = toRange(query, false);
      return this._tx._request(this, () => { const r = getAllFrom(this._d.records, range, 1, (x) => x)[0]; return r ? r.key : undefined; });
    }
    getAll(query, count) {
      this._tx._check(false);
      const [range, n, dir] = getAllArgs(query, count);
      return this._tx._request(this, () => {
        let recs = this._d.records; if (dir.startsWith('prev')) recs = recs.slice().reverse();
        return getAllFrom(recs, dir.startsWith('prev') ? null : range, dir.startsWith('prev') ? 0 : n, (r) => r).filter((r) => inRange(range, r.key)).slice(0, n || undefined).map((r) => clone(r.value));
      });
    }
    getAllKeys(query, count) {
      this._tx._check(false);
      const [range, n] = getAllArgs(query, count);
      return this._tx._request(this, () => getAllFrom(this._d.records, range, n, (r) => r.key));
    }
    getAllRecords(options) {
      this._tx._check(false);
      const [range, n] = getAllArgs(options || {}, undefined);
      return this._tx._request(this, () => getAllFrom(this._d.records, range, n, (r) => ({ key: r.key, primaryKey: r.key, value: clone(r.value) })));
    }
    count(query) {
      this._tx._check(false);
      const range = toRange(query);
      return this._tx._request(this, () => this._d.records.reduce((n, r) => n + (inRange(range, r.key) ? 1 : 0), 0));
    }
    openCursor(query, direction) { return openCursor(this, query, direction, true); }
    openKeyCursor(query, direction) { return openCursor(this, query, direction, false); }
    createIndex(name, keyPath, options) {
      if (!this._tx._upgrade) throw err('InvalidStateError', 'Not in a versionchange transaction.');
      name = String(name); options = options || {};
      const d = this._d;
      if (d.indexes.has(name)) throw err('ConstraintError', `An index named ${name} already exists.`);
      if (!validKeyPath(keyPath)) throw err('SyntaxError', 'The keyPath is not valid.');
      if (Array.isArray(keyPath) && options.multiEntry) throw err('InvalidAccessError', 'multiEntry indexes cannot have array key paths.');
      const ix = { name, keyPath: copyPath(keyPath), unique: !!options.unique, multiEntry: !!options.multiEntry, records: [] };
      for (const r of d.records) {
        if (ix.unique) for (const k of indexKeys(ix, r.value)) if (findIdx(ix.records, k) >= 0) { this._tx._abort(err('ConstraintError', 'Index uniqueness violated.')); break; }
        addToIndex(ix, r.value, r.key);
      }
      d.indexes.set(name, ix);
      return this.index(name);
    }
    deleteIndex(name) {
      if (!this._tx._upgrade) throw err('InvalidStateError', 'Not in a versionchange transaction.');
      if (!this._d.indexes.delete(String(name))) throw err('NotFoundError', `No index named ${name}.`);
      this._ix.delete(String(name));
    }
    index(name) {
      if (this._tx._state === 'finished') throw err('InvalidStateError', 'The transaction has finished.');
      name = String(name);
      if (!this._d.indexes.has(name)) throw err('NotFoundError', `No index named ${name}.`);
      let ix = this._ix.get(name);
      if (!ix) { ix = new IDBIndex(this, name); this._ix.set(name, ix); }
      return ix;
    }
  }

  class IDBIndex {
    constructor(store, name) { this._store = store; this._name = name; }
    get _d() { return this._store._d.indexes.get(this._name); }
    get name() { return this._name; }
    get objectStore() { return this._store; }
    get keyPath() { return copyPath(this._d.keyPath); }
    get unique() { return this._d.unique; }
    get multiEntry() { return this._d.multiEntry; }
    _value(pk) { const s = this._store._d; const i = findIdx(s.records, pk); return i >= 0 ? clone(s.records[i].value) : undefined; }
    get(query) {
      this._store._tx._check(false);
      const range = toRange(query, false);
      return this._store._tx._request(this, () => { const r = getAllFrom(this._d.records, range, 1, (x) => x)[0]; return r ? this._value(r.pk) : undefined; });
    }
    getKey(query) {
      this._store._tx._check(false);
      const range = toRange(query, false);
      return this._store._tx._request(this, () => { const r = getAllFrom(this._d.records, range, 1, (x) => x)[0]; return r ? r.pk : undefined; });
    }
    getAll(query, count) {
      this._store._tx._check(false);
      const [range, n] = getAllArgs(query, count);
      return this._store._tx._request(this, () => getAllFrom(this._d.records, range, n, (r) => this._value(r.pk)));
    }
    getAllKeys(query, count) {
      this._store._tx._check(false);
      const [range, n] = getAllArgs(query, count);
      return this._store._tx._request(this, () => getAllFrom(this._d.records, range, n, (r) => r.pk));
    }
    count(query) {
      this._store._tx._check(false);
      const range = toRange(query);
      return this._store._tx._request(this, () => this._d.records.reduce((n, r) => n + (inRange(range, r.key) ? 1 : 0), 0));
    }
    openCursor(query, direction) { return openCursor(this, query, direction, true); }
    openKeyCursor(query, direction) { return openCursor(this, query, direction, false); }
  }

  // ---------------------------------------------------------------- cursors
  const DIRS = ['next', 'nextunique', 'prev', 'prevunique'];
  function openCursor(source, query, direction, withValue) {
    const tx = source instanceof IDBIndex ? source._store._tx : source._tx;
    tx._check(false);
    direction = direction === undefined ? 'next' : String(direction);
    if (!DIRS.includes(direction)) throw new TypeError(`Invalid cursor direction: ${direction}`);
    const range = toRange(query);
    const cursor = new (withValue ? IDBCursorWithValue : IDBCursor)(source, direction, range, tx);
    const req = tx._request(source, () => cursor._step(undefined, undefined, 1));
    cursor._req = req;
    return req;
  }
  class IDBCursor {
    constructor(source, direction, range, tx) {
      this._source = source; this._dir = direction; this._range = range; this._tx = tx;
      this._key = undefined; this._pk = undefined; this._value = undefined; this._got = false; this._req = null;
    }
    get source() { return this._source; }
    get direction() { return this._dir; }
    get key() { return this._key; }
    get primaryKey() { return this._source instanceof IDBIndex ? this._pk : this._key; }
    get request() { return this._req; }
    _records() { return this._source instanceof IDBIndex ? this._source._d.records : this._source._d.records.map((r) => ({ key: r.key, pk: r.key, value: r.value })); }
    // Move to the next matching record after the current position, optionally
    // at or past `target` (key) / `targetPk`; `skip` counts records to pass.
    _step(target, targetPk, skip) {
      const recs = this._records();
      const fwd = this._dir.startsWith('next'), unique = this._dir.endsWith('unique');
      const isIndex = this._source instanceof IDBIndex;
      let found = null;
      const order = fwd ? recs : recs.slice().reverse();
      for (const r of order) {
        if (!inRange(this._range, r.key)) continue;
        if (this._key !== undefined) {
          const c = cmp(r.key, this._key) * (fwd ? 1 : -1);
          if (c < 0) continue;
          if (c === 0) {
            if (unique || !isIndex) continue;
            const pc = cmp(r.pk, this._pk) * (fwd ? 1 : -1);
            if (pc <= 0) continue;
          }
        }
        if (target !== undefined) {
          const c = cmp(r.key, target) * (fwd ? 1 : -1);
          if (c < 0) continue;
          if (c === 0 && targetPk !== undefined && cmp(r.pk, targetPk) * (fwd ? 1 : -1) < 0) continue;
        }
        if (unique && !fwd && isIndex) {
          // prevunique yields the first (lowest primary key) record of each key.
          const first = recs[lowerBoundIdx(recs, r.key)];
          if (first !== r) continue;
        }
        if (--skip > 0) { this._key = r.key; this._pk = r.pk; continue; }
        found = r; break;
      }
      if (!found) { this._key = undefined; this._pk = undefined; this._value = undefined; return null; }
      this._key = found.key; this._pk = found.pk;
      if (this instanceof IDBCursorWithValue) {
        this._value = isIndex ? this._source._value(found.pk) : clone(found.value);
      }
      this._got = false;
      return this;
    }
    _again(fn) {
      this._tx._check(false);
      if (this._got) throw err('InvalidStateError', 'The cursor is being iterated or has iterated past its end.');
      this._got = true;
      this._req._ready = 'pending';
      this._tx._request(this._source, fn, this._req);
    }
    advance(count) {
      count = Number(count);
      if (!(count >= 1)) throw new TypeError('Failed to execute \'advance\' on \'IDBCursor\': A count argument with value 0 (zero) was supplied, must be greater than 0.');
      this._again(() => this._step(undefined, undefined, Math.floor(count)));
    }
    continue(key) {
      let k;
      if (key !== undefined) {
        k = mustKey(key);
        const c = cmp(k, this._key);
        if (this._dir.startsWith('next') ? c <= 0 : c >= 0) throw err('DataError', 'The parameter is less than or equal to this cursor\'s position.');
      }
      this._again(() => this._step(k, undefined, 1));
    }
    continuePrimaryKey(key, pk) {
      if (!(this._source instanceof IDBIndex)) throw err('InvalidAccessError', 'continuePrimaryKey requires an index cursor.');
      const k = mustKey(key), p = mustKey(pk);
      this._again(() => this._step(k, p, 1));
    }
    update(value) {
      this._tx._check(true);
      if (this._got || this._key === undefined) throw err('InvalidStateError', 'The cursor is not positioned on a record.');
      const store = this._source instanceof IDBIndex ? this._source._store : this._source;
      if (store._d.keyPath !== null) {
        const k = evalPath(clone(value), store._d.keyPath);
        if (k === undefined || cmp(k, this.primaryKey) !== 0) throw err('DataError', 'The effective key does not match the cursor\'s primary key.');
        return store.put(value);
      }
      return store.put(value, this.primaryKey);
    }
    delete() {
      this._tx._check(true);
      if (this._got || this._key === undefined) throw err('InvalidStateError', 'The cursor is not positioned on a record.');
      const store = this._source instanceof IDBIndex ? this._source._store : this._source;
      return store.delete(this.primaryKey);
    }
  }
  class IDBCursorWithValue extends IDBCursor {
    get value() { return this._value; }
  }

  // ---------------------------------------------------------------- factory
  class IDBFactory {
    open(name, version) {
      if (name === undefined) throw new TypeError('Failed to execute \'open\' on \'IDBFactory\': 1 argument required, but only 0 present.');
      name = String(name);
      if (version !== undefined) {
        version = Number(version);
        if (!(version >= 1) || version > 9007199254740991) throw new TypeError('The version provided must be a positive integer.');
        version = Math.floor(version);
      }
      const req = new IDBOpenDBRequest(null, null);
      later(() => {
        let data = databases.get(name);
        if (!data) { data = { name, version: 0, stores: new Map(), connections: new Set() }; databases.set(name, data); }
        const want = version === undefined ? Math.max(data.version, 1) : version;
        if (want < data.version) {
          req._ready = 'done'; req._error = err('VersionError', `The requested version (${want}) is less than the existing version (${data.version}).`);
          fire(req, 'error', { bubbles: true, cancelable: true });
          return;
        }
        if (want === data.version) {
          req._ready = 'done'; req._result = new IDBDatabase(data);
          fire(req, 'success');
          return;
        }
        // Version change: ask other connections to close, then upgrade.
        const old = data.version;
        for (const c of [...data.connections]) fire(c, 'versionchange', { versions: { oldVersion: old, newVersion: want } });
        const open = [...data.connections].filter((c) => !c._closed);
        if (open.length) fire(req, 'blocked', { versions: { oldVersion: old, newVersion: want } });
        const schema = { version: old, stores: new Map([...data.stores].map(([k, v]) => [k, copyStoreData(v)])) };
        data.version = want;
        const db = new IDBDatabase(data);
        const tx = new IDBTransaction(db, [], 'versionchange', true);
        tx._schema = schema;
        req._result = db; req._tx = tx; req._ready = 'done';
        tx._onDone = (ok) => {
          req._tx = null;
          if (ok) { fire(req, 'success'); return; }
          db.close();
          db._version = old;
          if (old === 0) databases.delete(name);
          req._result = undefined; req._error = err('AbortError', 'Version change transaction was aborted in upgradeneeded event handler.');
          fire(req, 'error', { bubbles: true, cancelable: true });
        };
        db._version = want;
        fire(req, 'upgradeneeded', { versions: { oldVersion: old, newVersion: want } });
      });
      return req;
    }
    deleteDatabase(name) {
      name = String(name);
      const req = new IDBOpenDBRequest(null, null);
      later(() => {
        const data = databases.get(name);
        const old = data ? data.version : 0;
        if (data) {
          for (const c of [...data.connections]) fire(c, 'versionchange', { versions: { oldVersion: old, newVersion: null } });
          databases.delete(name);
        }
        req._ready = 'done'; req._result = undefined;
        fire(req, 'success', { versions: { oldVersion: old, newVersion: null } });
      });
      return req;
    }
    databases() {
      return Promise.resolve([...databases.values()].filter((d) => d.version > 0).map((d) => ({ name: d.name, version: d.version })));
    }
    cmp(a, b) { return cmp(mustKey(a), mustKey(b)); }
  }

  const indexedDB = new IDBFactory();
  const api = {
    indexedDB, IDBFactory, IDBDatabase, IDBTransaction, IDBObjectStore, IDBIndex, IDBCursor, IDBCursorWithValue,
    IDBKeyRange, IDBRequest, IDBOpenDBRequest, IDBVersionChangeEvent,
  };
  for (const k in api) Object.defineProperty(global, k, { value: api[k], writable: true, configurable: true, enumerable: false });
})(this);
