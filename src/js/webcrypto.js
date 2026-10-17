// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

// HeronScript Web Crypto: SubtleCrypto and CryptoKey (Web Cryptography API
// Level 2) over the native `N.webcrypto(op, args)` (crates/ohb-crypto).
// Evaluated after the prelude, which defines `crypto` with getRandomValues,
// randomUUID and a digest-only `subtle`; this replaces `crypto.subtle`.
//
// Script owns the CryptoKey objects: each keeps its type, extractability,
// algorithm, usages and key material (raw bytes for secret keys, PKCS#8 for
// private keys, SPKI for public keys) in a hidden slot. The native side is
// stateless and does the cryptography; algorithm normalization, usage checks
// and wrapKey/unwrapKey/deriveKey composition live here.
(function (global) {
  'use strict';
  const N = global.__n_worker || global.__n;
  const cryptoObj = global.crypto;
  if (!N || typeof N.webcrypto !== 'function' || !cryptoObj) return;
  const B = global.__ohb_builtin || {};
  const DOMEx = global.DOMException;
  const NativePromise = Promise;
  const Enc = global.TextEncoder, Dec = global.TextDecoder;
  const hidden = (o, k, v) => Object.defineProperty(o, k, { value: v, writable: true, configurable: true });
  const domErr = (msg, name) => new DOMEx(msg, name);
  const toStr = Object.prototype.toString;
  const abSlice = ArrayBuffer.prototype.slice;

  // ------------------------------------------------------------ bytes
  // A copy of a BufferSource as an ArrayBuffer (null when it is not one).
  function copyBytes(x) {
    if (x === null || typeof x !== 'object') return null;
    if (ArrayBuffer.isView(x)) {
      const off = x.byteOffset, len = x.byteLength;
      return abSlice.call(x.buffer, off, off + len);
    }
    const t = toStr.call(x);
    if (t === '[object ArrayBuffer]' || t === '[object SharedArrayBuffer]') {
      if (t === '[object SharedArrayBuffer]') { const u = new Uint8Array(x); return u.slice().buffer; }
      return abSlice.call(x, 0);
    }
    return null;
  }
  function bufferSource(x, method, what) {
    const b = copyBytes(x);
    if (!b) throw new TypeError(`Failed to execute '${method}' on 'SubtleCrypto': ${what || 'The provided value'} is not of type '(ArrayBuffer or ArrayBufferView)'.`);
    return b;
  }

  // Run a native operation, turning "Name: message" errors into DOMExceptions.
  function native(op, args) {
    try {
      return N.webcrypto(op, args);
    } catch (e) {
      const text = e && typeof e.message === 'string' ? e.message : String(e);
      const m = /^([A-Za-z]+): ([\s\S]*)$/.exec(text);
      if (m && m[1] === 'TypeError') throw new TypeError(m[2]);
      if (m) throw domErr(m[2], m[1]);
      throw domErr(text, 'OperationError');
    }
  }

  // ------------------------------------------------------------ algorithms
  const NAMES = ['RSASSA-PKCS1-v1_5', 'RSA-PSS', 'RSA-OAEP', 'ECDSA', 'ECDH', 'AES-CTR', 'AES-CBC', 'AES-GCM', 'AES-KW', 'HMAC',
    'SHA-1', 'SHA-256', 'SHA-384', 'SHA-512', 'HKDF', 'PBKDF2'];
  const CANON = new Map(NAMES.map((n) => [n.toUpperCase(), n]));
  // Dictionary members per operation and algorithm: [type, required].
  const RSA_GEN = { modulusLength: ['ulong', 1], publicExponent: ['bigint', 1], hash: ['hash', 1] };
  const RSA_IMP = { hash: ['hash', 1] };
  const EC = { namedCurve: ['str', 1] };
  const AES_GEN = { length: ['ushort', 1] };
  const HMAC_KEY = { hash: ['hash', 1], length: ['ulong', 0] };
  const SHAS = { 'SHA-1': {}, 'SHA-256': {}, 'SHA-384': {}, 'SHA-512': {} };
  const CIPHERS = {
    'RSA-OAEP': { label: ['buf', 0] },
    'AES-CTR': { counter: ['buf', 1], length: ['octet', 1] },
    'AES-CBC': { iv: ['buf', 1] },
    'AES-GCM': { iv: ['buf', 1], additionalData: ['buf', 0], tagLength: ['octet', 0] },
  };
  const SIGNERS = { 'RSASSA-PKCS1-v1_5': {}, 'RSA-PSS': { saltLength: ['ulong', 1] }, ECDSA: { hash: ['hash', 1] }, HMAC: {} };
  const OPS = {
    encrypt: CIPHERS, decrypt: CIPHERS, sign: SIGNERS, verify: SIGNERS, digest: SHAS,
    generateKey: {
      'RSASSA-PKCS1-v1_5': RSA_GEN, 'RSA-PSS': RSA_GEN, 'RSA-OAEP': RSA_GEN, ECDSA: EC, ECDH: EC,
      'AES-CTR': AES_GEN, 'AES-CBC': AES_GEN, 'AES-GCM': AES_GEN, 'AES-KW': AES_GEN, HMAC: HMAC_KEY,
    },
    importKey: {
      'RSASSA-PKCS1-v1_5': RSA_IMP, 'RSA-PSS': RSA_IMP, 'RSA-OAEP': RSA_IMP, ECDSA: EC, ECDH: EC,
      'AES-CTR': {}, 'AES-CBC': {}, 'AES-GCM': {}, 'AES-KW': {}, HMAC: HMAC_KEY, HKDF: {}, PBKDF2: {},
    },
    deriveBits: {
      ECDH: { public: ['key', 1] },
      HKDF: { hash: ['hash', 1], salt: ['buf', 1], info: ['buf', 1] },
      PBKDF2: { salt: ['buf', 1], iterations: ['ulong', 1], hash: ['hash', 1] },
    },
    wrapKey: { 'AES-KW': {} },
    unwrapKey: { 'AES-KW': {} },
    getKeyLength: { 'AES-CTR': AES_GEN, 'AES-CBC': AES_GEN, 'AES-GCM': AES_GEN, 'AES-KW': AES_GEN, HMAC: HMAC_KEY, HKDF: {}, PBKDF2: {} },
  };
  function enforceRange(v, max, member) {
    const n = Number(v);
    if (!Number.isFinite(n)) throw new TypeError(`Failed to read the '${member}' property: Value is not a finite number.`);
    const t = Math.trunc(n);
    if (t < 0 || t > max) throw new TypeError(`Failed to read the '${member}' property: Value is outside the accepted range.`);
    return t;
  }
  function convertMember(type, v, member, method) {
    switch (type) {
      case 'buf': return bufferSource(v, method, `Failed to read the '${member}' property: The provided value`);
      case 'hash': return normalize('digest', v, method);
      case 'octet': return enforceRange(v, 255, member);
      case 'ushort': return enforceRange(v, 65535, member);
      case 'ulong': return enforceRange(v, 4294967295, member);
      case 'str': return String(v);
      case 'bigint': {
        if (!(ArrayBuffer.isView(v) && toStr.call(v) === '[object Uint8Array]')) throw new TypeError(`Failed to read the '${member}' property: The provided value is not of type 'Uint8Array'.`);
        return new Uint8Array(copyBytes(v));
      }
      case 'key':
        if (!SLOT.has(v)) throw new TypeError(`Failed to read the '${member}' property: The provided value is not of type 'CryptoKey'.`);
        return v;
    }
    return v;
  }
  // WebCrypto "normalize an algorithm" for an operation.
  function normalize(op, alg, method) {
    if (typeof alg === 'string') return normalize(op, { name: alg }, method);
    if (alg === null || typeof alg !== 'object') throw new TypeError(`Failed to execute '${method}' on 'SubtleCrypto': Algorithm: Not an object`);
    if (alg.name === undefined) throw new TypeError(`Failed to execute '${method}' on 'SubtleCrypto': Algorithm: name: Missing or not a string`);
    const canon = CANON.get(String(alg.name).toUpperCase());
    const table = OPS[op];
    if (!canon || !table[canon]) throw domErr('Unrecognized algorithm name', 'NotSupportedError');
    const out = { name: canon };
    const members = table[canon];
    for (const k of Object.keys(members).sort()) {
      const v = alg[k];
      if (v === undefined) {
        if (members[k][1]) throw new TypeError(`Failed to execute '${method}' on 'SubtleCrypto': ${canon}: ${k}: Missing required property`);
        continue;
      }
      out[k] = convertMember(members[k][0], v, k, method);
    }
    return out;
  }
  // A normalized algorithm (or a key's internal algorithm) as plain data for
  // the native side: hashes as names, Uint8Arrays as ArrayBuffers.
  function forNative(p) {
    const o = {};
    for (const k of Object.keys(p)) {
      let v = p[k];
      if (v && typeof v === 'object') {
        if (k === 'hash') v = String(v.name);
        else if (SLOT.has(v)) v = SLOT.get(v).material;
        else if (ArrayBuffer.isView(v)) v = copyBytes(v);
      }
      o[k] = v;
    }
    return o;
  }

  // ------------------------------------------------------------ usages
  const USAGE_ORDER = ['encrypt', 'decrypt', 'sign', 'verify', 'deriveKey', 'deriveBits', 'wrapKey', 'unwrapKey'];
  const SIGN_U = { public: ['verify'], private: ['sign'] };
  const AES_U = { secret: ['encrypt', 'decrypt', 'wrapKey', 'unwrapKey'] };
  const KDF_U = { secret: ['deriveKey', 'deriveBits'] };
  const USAGES = {
    'RSASSA-PKCS1-v1_5': SIGN_U, 'RSA-PSS': SIGN_U, ECDSA: SIGN_U,
    'RSA-OAEP': { public: ['encrypt', 'wrapKey'], private: ['decrypt', 'unwrapKey'] },
    ECDH: { public: [], private: ['deriveKey', 'deriveBits'] },
    'AES-CTR': AES_U, 'AES-CBC': AES_U, 'AES-GCM': AES_U, 'AES-KW': { secret: ['wrapKey', 'unwrapKey'] },
    HMAC: { secret: ['sign', 'verify'] }, HKDF: KDF_U, PBKDF2: KDF_U,
  };
  const JWK_USE = { 'RSASSA-PKCS1-v1_5': 'sig', 'RSA-PSS': 'sig', ECDSA: 'sig', HMAC: 'sig', 'RSA-OAEP': 'enc', ECDH: 'enc', 'AES-CTR': 'enc', 'AES-CBC': 'enc', 'AES-GCM': 'enc', 'AES-KW': 'enc' };
  // sequence<KeyUsage>: deduplicated, in a canonical order.
  function readUsages(u, method) {
    if (u === null || typeof u !== 'object' || typeof u[Symbol.iterator] !== 'function') throw new TypeError(`Failed to execute '${method}' on 'SubtleCrypto': The provided value cannot be converted to a sequence.`);
    const set = new Set();
    for (const x of u) {
      const s = String(x);
      if (!USAGE_ORDER.includes(s)) throw new TypeError(`Failed to execute '${method}' on 'SubtleCrypto': The provided value '${s}' is not a valid enum value of type KeyUsage.`);
      set.add(s);
    }
    return USAGE_ORDER.filter((x) => set.has(x));
  }
  function checkUsages(usages, allowed) {
    for (const u of usages) if (!allowed.includes(u)) throw domErr(`Cannot create a key using the specified key usages.`, 'SyntaxError');
  }

  // ------------------------------------------------------------ CryptoKey
  const SLOT = new WeakMap();
  function slot(k) {
    const s = SLOT.get(k);
    if (!s) throw new TypeError('Illegal invocation');
    return s;
  }
  class CryptoKey {
    constructor() { throw new TypeError('Illegal constructor'); }
    get type() { return slot(this).type; }
    get extractable() { return slot(this).extractable; }
    get algorithm() {
      const s = slot(this);
      if (!s.algObj) s.algObj = publicAlgorithm(s.alg);
      return s.algObj;
    }
    get usages() {
      const s = slot(this);
      if (!s.usagesArr) s.usagesArr = s.usages.slice();
      return s.usagesArr;
    }
  }
  Object.defineProperty(CryptoKey.prototype, Symbol.toStringTag, { value: 'CryptoKey', configurable: true });
  for (const k of ['type', 'extractable', 'algorithm', 'usages']) {
    const d = Object.getOwnPropertyDescriptor(CryptoKey.prototype, k);
    d.enumerable = true;
    Object.defineProperty(CryptoKey.prototype, k, d);
  }
  // `alg` is the internal algorithm: {name, length?, hash? (name),
  // namedCurve?, modulusLength?, publicExponent? (Uint8Array)}.
  function makeKey(type, extractable, alg, usages, material) {
    const k = Object.create(CryptoKey.prototype);
    SLOT.set(k, { type, extractable: !!extractable, alg, usages: usages.slice(), material, algObj: null, usagesArr: null });
    return k;
  }
  // The KeyAlgorithm dictionary a key exposes.
  function publicAlgorithm(a) {
    const o = { name: a.name };
    if (a.modulusLength !== undefined) { o.modulusLength = a.modulusLength; o.publicExponent = new Uint8Array(a.publicExponent); }
    if (a.namedCurve !== undefined) o.namedCurve = a.namedCurve;
    if (a.hash !== undefined) o.hash = { name: a.hash };
    if (a.length !== undefined) o.length = a.length;
    return o;
  }
  function keyOf(k, method, what) {
    if (!SLOT.has(k)) throw new TypeError(`Failed to execute '${method}' on 'SubtleCrypto': ${what} is not of type 'CryptoKey'.`);
    return SLOT.get(k);
  }
  function needUsage(s, alg, usage) {
    if (alg && alg.name !== s.alg.name) throw domErr(`The requested operation is not valid for the provided key.`, 'InvalidAccessError');
    if (!s.usages.includes(usage)) throw domErr(`The key does not support the '${usage}' operation.`, 'InvalidAccessError');
  }
  function keyArgs(s, params, extra) {
    const o = { alg: forNative(params), keyAlg: forNative(s.alg), keyType: s.type, key: s.material };
    if (extra) Object.assign(o, extra);
    return o;
  }
  // Structured cloning (IndexedDB, postMessage), when the prelude offers a hook.
  if (typeof B.scRegister === 'function') {
    B.scRegister(CryptoKey, 'ckey', (v) => {
      const s = slot(v);
      const alg = Object.assign({}, s.alg);
      if (alg.publicExponent) alg.publicExponent = copyBytes(alg.publicExponent);
      return { t: 'ckey', ty: s.type, ex: s.extractable, alg, us: s.usages.slice(), m: abSlice.call(s.material, 0) };
    }, (n) => {
      const alg = Object.assign({}, n.alg);
      if (alg.publicExponent) alg.publicExponent = new Uint8Array(alg.publicExponent);
      return makeKey(n.ty, n.ex, alg, n.us, n.m);
    });
  }

  // ------------------------------------------------------------ operations (synchronous cores)
  const FORMATS = ['raw', 'spki', 'pkcs8', 'jwk'];
  function readFormat(f, method) {
    const s = String(f);
    if (!FORMATS.includes(s)) throw new TypeError(`Failed to execute '${method}' on 'SubtleCrypto': The provided value '${s}' is not a valid enum value of type KeyFormat.`);
    return s;
  }
  const JWK_MEMBERS = ['kty', 'use', 'alg', 'crv', 'x', 'y', 'd', 'n', 'e', 'p', 'q', 'dp', 'dq', 'qi', 'k'];
  function readJwk(j, method) {
    if (j === null || typeof j !== 'object' || copyBytes(j)) throw new TypeError(`Failed to execute '${method}' on 'SubtleCrypto': Key data must be an object for JWK import`);
    const o = {};
    for (const k of JWK_MEMBERS) if (j[k] !== undefined) o[k] = String(j[k]);
    if (j.key_ops !== undefined) {
      if (j.key_ops === null || typeof j.key_ops !== 'object') throw new TypeError(`Failed to execute '${method}' on 'SubtleCrypto': key_ops: Not a sequence`);
      o.key_ops = Array.from(j.key_ops, String);
    }
    if (j.ext !== undefined) o.ext = !!j.ext;
    if (j.oth !== undefined) throw domErr('Multi-prime RSA keys are not supported.', 'NotSupportedError');
    return o;
  }

  function importKeySync(format, keyData, alg, extractable, usages, method) {
    const name = alg.name;
    const allowed = USAGES[name];
    let type;
    if (allowed.secret) type = 'secret';
    else if (format === 'pkcs8') type = 'private';
    else if (format === 'jwk') type = keyData.d !== undefined ? 'private' : 'public';
    else type = 'public';
    checkUsages(usages, allowed[type] || []);
    if ((name === 'HKDF' || name === 'PBKDF2') && extractable) throw domErr(`${name} keys cannot be extractable.`, 'SyntaxError');
    if (format === 'jwk') {
      if (usages.length && keyData.use !== undefined && keyData.use !== JWK_USE[name]) throw domErr(`The JWK "use" member was not "${JWK_USE[name]}".`, 'DataError');
      if (keyData.key_ops !== undefined) {
        const ops = keyData.key_ops;
        if (new Set(ops).size !== ops.length) throw domErr('The JWK "key_ops" member contains duplicates.', 'DataError');
        for (const u of usages) if (!ops.includes(u)) throw domErr(`The JWK "key_ops" member does not include "${u}".`, 'DataError');
      }
      if (keyData.ext === false && extractable) throw domErr('The JWK "ext" member was false but the key is extractable.', 'DataError');
    }
    let data = keyData;
    if (format === 'jwk') {
      data = {};
      for (const k of JWK_MEMBERS) if (keyData[k] !== undefined) data[k] = keyData[k];
    }
    const r = native('importKey', { alg: forNative(alg), format, keyData: data });
    const internal = { name };
    if (r.length !== undefined) internal.length = r.length;
    if (alg.hash) internal.hash = alg.hash.name;
    if (r.namedCurve !== undefined) internal.namedCurve = r.namedCurve;
    if (r.modulusLength !== undefined) { internal.modulusLength = r.modulusLength; internal.publicExponent = new Uint8Array(r.publicExponent); }
    if ((r.type === 'secret' || r.type === 'private') && usages.length === 0) throw domErr('Usages cannot be empty when creating a key.', 'SyntaxError');
    return makeKey(r.type, extractable, internal, usages, r.key);
  }

  function exportKeySync(format, s) {
    if (!s.extractable) throw domErr('The key is not extractable.', 'InvalidAccessError');
    const r = native('exportKey', { alg: forNative(s.alg), format, keyType: s.type, key: s.material });
    if (format !== 'jwk') return r;
    const jwk = {};
    for (const k of Object.keys(r)) jwk[k] = r[k];
    jwk.key_ops = s.usages.slice();
    jwk.ext = s.extractable;
    return jwk;
  }

  function generateKeySync(alg, extractable, usages) {
    const name = alg.name;
    const allowed = USAGES[name];
    if (allowed.secret) {
      checkUsages(usages, allowed.secret);
      const r = native('generateKey', { alg: forNative(alg) });
      const internal = { name, length: r.length };
      if (alg.hash) internal.hash = alg.hash.name;
      if (usages.length === 0) throw domErr('Usages cannot be empty when creating a key.', 'SyntaxError');
      return makeKey('secret', extractable, internal, usages, r.key);
    }
    checkUsages(usages, allowed.public.concat(allowed.private));
    const pubUsages = usages.filter((u) => allowed.public.includes(u));
    const privUsages = usages.filter((u) => allowed.private.includes(u));
    if (privUsages.length === 0) throw domErr('Usages cannot be empty when creating a key.', 'SyntaxError');
    const r = native('generateKey', { alg: forNative(alg) });
    const internal = { name };
    if (r.namedCurve !== undefined) internal.namedCurve = r.namedCurve;
    if (r.modulusLength !== undefined) { internal.modulusLength = r.modulusLength; internal.publicExponent = new Uint8Array(r.publicExponent); }
    if (alg.hash) internal.hash = alg.hash.name;
    const pubAlg = Object.assign({}, internal);
    if (pubAlg.publicExponent) pubAlg.publicExponent = new Uint8Array(pubAlg.publicExponent);
    return {
      publicKey: makeKey('public', true, pubAlg, pubUsages, r.publicKey),
      privateKey: makeKey('private', extractable, internal, privUsages, r.privateKey),
    };
  }

  function deriveBitsSync(alg, s, length) {
    if (alg.name === 'ECDH') {
      const p = SLOT.get(alg.public);
      if (p.type !== 'public') throw domErr('The public key must be a public key.', 'InvalidAccessError');
      if (p.alg.name !== s.alg.name || p.alg.namedCurve !== s.alg.namedCurve) throw domErr('The public key is not compatible with the private key.', 'InvalidAccessError');
    }
    return native('deriveBits', keyArgs(s, alg, { length }));
  }

  const HASH_BLOCK = { 'SHA-1': 512, 'SHA-256': 512, 'SHA-384': 1024, 'SHA-512': 1024 };
  function keyLength(alg) {
    if (alg.name === 'HKDF' || alg.name === 'PBKDF2') return null;
    if (alg.name === 'HMAC') {
      if (alg.length === undefined) return HASH_BLOCK[alg.hash.name];
      if (alg.length === 0) throw new TypeError('HMAC key length must not be 0.');
      return alg.length;
    }
    if (![128, 192, 256].includes(alg.length)) throw domErr('AES key length must be 128, 192 or 256 bits.', 'OperationError');
    return alg.length;
  }

  function utf8(s) { return new Enc().encode(s).buffer; }
  function normalizeWrap(op, fallback, alg, method) {
    try { return [op, normalize(op, alg, method)]; } catch (e) {
      if (!(e instanceof DOMEx) || e.name !== 'NotSupportedError') throw e;
      return [fallback, normalize(fallback, alg, method)];
    }
  }

  // ------------------------------------------------------------ SubtleCrypto
  // Every method returns a promise and never throws synchronously.
  function run(self, method, argc, need, fn) {
    return new NativePromise((resolve) => {
      if (!(self instanceof SubtleCrypto)) throw new TypeError('Illegal invocation');
      if (argc < need) throw new TypeError(`Failed to execute '${method}' on 'SubtleCrypto': ${need} arguments required, but only ${argc} present.`);
      resolve(fn());
    });
  }
  class SubtleCrypto {
    constructor() { throw new TypeError('Illegal constructor'); }
    encrypt(algorithm, key, data) {
      return run(this, 'encrypt', arguments.length, 3, () => {
        const alg = normalize('encrypt', algorithm, 'encrypt');
        const s = keyOf(key, 'encrypt', "parameter 2");
        const bytes = bufferSource(data, 'encrypt', 'parameter 3');
        needUsage(s, alg, 'encrypt');
        return native('encrypt', keyArgs(s, alg, { data: bytes }));
      });
    }
    decrypt(algorithm, key, data) {
      return run(this, 'decrypt', arguments.length, 3, () => {
        const alg = normalize('decrypt', algorithm, 'decrypt');
        const s = keyOf(key, 'decrypt', "parameter 2");
        const bytes = bufferSource(data, 'decrypt', 'parameter 3');
        needUsage(s, alg, 'decrypt');
        return native('decrypt', keyArgs(s, alg, { data: bytes }));
      });
    }
    sign(algorithm, key, data) {
      return run(this, 'sign', arguments.length, 3, () => {
        const alg = normalize('sign', algorithm, 'sign');
        const s = keyOf(key, 'sign', "parameter 2");
        const bytes = bufferSource(data, 'sign', 'parameter 3');
        needUsage(s, alg, 'sign');
        return native('sign', keyArgs(s, alg, { data: bytes }));
      });
    }
    verify(algorithm, key, signature, data) {
      return run(this, 'verify', arguments.length, 4, () => {
        const alg = normalize('verify', algorithm, 'verify');
        const s = keyOf(key, 'verify', "parameter 2");
        const sig = bufferSource(signature, 'verify', 'parameter 3');
        const bytes = bufferSource(data, 'verify', 'parameter 4');
        needUsage(s, alg, 'verify');
        return !!native('verify', keyArgs(s, alg, { data: bytes, signature: sig }));
      });
    }
    digest(algorithm, data) {
      return run(this, 'digest', arguments.length, 2, () => {
        const alg = normalize('digest', algorithm, 'digest');
        const bytes = bufferSource(data, 'digest', 'parameter 2');
        if (typeof N.digest === 'function') return N.digest(alg.name, new Uint8Array(bytes));
        return native('digest', { alg: { name: alg.name }, data: bytes });
      });
    }
    generateKey(algorithm, extractable, keyUsages) {
      return run(this, 'generateKey', arguments.length, 3, () => {
        const alg = normalize('generateKey', algorithm, 'generateKey');
        const usages = readUsages(keyUsages, 'generateKey');
        return generateKeySync(alg, !!extractable, usages);
      });
    }
    deriveKey(algorithm, baseKey, derivedKeyType, extractable, keyUsages) {
      return run(this, 'deriveKey', arguments.length, 5, () => {
        const alg = normalize('deriveBits', algorithm, 'deriveKey');
        const importAlg = normalize('importKey', derivedKeyType, 'deriveKey');
        const lenAlg = normalize('getKeyLength', derivedKeyType, 'deriveKey');
        const s = keyOf(baseKey, 'deriveKey', 'parameter 2');
        const usages = readUsages(keyUsages, 'deriveKey');
        needUsage(s, alg, 'deriveKey');
        const bits = deriveBitsSync(alg, s, keyLength(lenAlg));
        return importKeySync('raw', bits, importAlg, !!extractable, usages, 'deriveKey');
      });
    }
    deriveBits(algorithm, baseKey, length) {
      return run(this, 'deriveBits', arguments.length, 2, () => {
        const alg = normalize('deriveBits', algorithm, 'deriveBits');
        const s = keyOf(baseKey, 'deriveBits', 'parameter 2');
        const len = length === undefined || length === null ? null : Number(length) >>> 0;
        needUsage(s, alg, 'deriveBits');
        return deriveBitsSync(alg, s, len);
      });
    }
    importKey(format, keyData, algorithm, extractable, keyUsages) {
      return run(this, 'importKey', arguments.length, 5, () => {
        const fmt = readFormat(format, 'importKey');
        const data = fmt === 'jwk' ? readJwk(keyData, 'importKey') : bufferSource(keyData, 'importKey', 'parameter 2');
        const alg = normalize('importKey', algorithm, 'importKey');
        const usages = readUsages(keyUsages, 'importKey');
        return importKeySync(fmt, data, alg, !!extractable, usages, 'importKey');
      });
    }
    exportKey(format, key) {
      return run(this, 'exportKey', arguments.length, 2, () => {
        const fmt = readFormat(format, 'exportKey');
        const s = keyOf(key, 'exportKey', 'parameter 2');
        return exportKeySync(fmt, s);
      });
    }
    wrapKey(format, key, wrappingKey, wrapAlgorithm) {
      return run(this, 'wrapKey', arguments.length, 4, () => {
        const fmt = readFormat(format, 'wrapKey');
        const s = keyOf(key, 'wrapKey', 'parameter 2');
        const w = keyOf(wrappingKey, 'wrapKey', 'parameter 3');
        const [, alg] = normalizeWrap('wrapKey', 'encrypt', wrapAlgorithm, 'wrapKey');
        needUsage(w, alg, 'wrapKey');
        let bytes = exportKeySync(fmt, s);
        if (fmt === 'jwk') bytes = utf8(JSON.stringify(bytes));
        return native('encrypt', keyArgs(w, alg, { data: bytes }));
      });
    }
    unwrapKey(format, wrappedKey, unwrappingKey, unwrapAlgorithm, unwrappedKeyAlgorithm, extractable, keyUsages) {
      return run(this, 'unwrapKey', arguments.length, 7, () => {
        const fmt = readFormat(format, 'unwrapKey');
        const wrapped = bufferSource(wrappedKey, 'unwrapKey', 'parameter 2');
        const w = keyOf(unwrappingKey, 'unwrapKey', 'parameter 3');
        const [, alg] = normalizeWrap('unwrapKey', 'decrypt', unwrapAlgorithm, 'unwrapKey');
        const importAlg = normalize('importKey', unwrappedKeyAlgorithm, 'unwrapKey');
        const usages = readUsages(keyUsages, 'unwrapKey');
        needUsage(w, alg, 'unwrapKey');
        const bytes = native('decrypt', keyArgs(w, alg, { data: wrapped }));
        let data = bytes;
        if (fmt === 'jwk') {
          let parsed;
          try { parsed = JSON.parse(new Dec('utf-8', { fatal: true }).decode(new Uint8Array(bytes))); } catch (_) {
            throw domErr('The unwrapped key is not valid JWK JSON.', 'DataError');
          }
          data = readJwk(parsed, 'unwrapKey');
        }
        return importKeySync(fmt, data, importAlg, !!extractable, usages, 'unwrapKey');
      });
    }
  }
  Object.defineProperty(SubtleCrypto.prototype, Symbol.toStringTag, { value: 'SubtleCrypto', configurable: true });
  for (const k of Object.getOwnPropertyNames(SubtleCrypto.prototype)) {
    if (k === 'constructor') continue;
    const d = Object.getOwnPropertyDescriptor(SubtleCrypto.prototype, k);
    d.enumerable = true;
    Object.defineProperty(SubtleCrypto.prototype, k, d);
  }

  // ------------------------------------------------------------ install
  const subtle = Object.create(SubtleCrypto.prototype);
  const proto = Object.getPrototypeOf(cryptoObj);
  const home = proto && proto !== Object.prototype ? proto : cryptoObj;
  try { delete cryptoObj.subtle; } catch (_) {}
  const getter = { get subtle() { return subtle; } };
  Object.defineProperty(home, 'subtle', { get: Object.getOwnPropertyDescriptor(getter, 'subtle').get, enumerable: true, configurable: true });
  if (home !== cryptoObj && Object.prototype.hasOwnProperty.call(cryptoObj, 'subtle')) {
    Object.defineProperty(cryptoObj, 'subtle', { get: Object.getOwnPropertyDescriptor(getter, 'subtle').get, enumerable: true, configurable: true });
  }
  hidden(global, 'SubtleCrypto', SubtleCrypto);
  hidden(global, 'CryptoKey', CryptoKey);
})(this);
