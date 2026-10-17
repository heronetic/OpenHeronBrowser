// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: MIT

//! Stateless Web Crypto (SubtleCrypto) primitives for OpenHeronBrowser.
//!
//! The script side (`webcrypto.js`) owns CryptoKey objects, algorithm
//! normalization and usage checks; this crate does the cryptography. Every
//! operation is a pure function of its arguments: key material crosses the
//! boundary as bytes (symmetric keys raw, private keys PKCS#8 DER, public keys
//! SPKI DER), so nothing is kept between calls.
//!
//! `call(op, args)` takes a plain object (see `call` for the shapes) and
//! returns plain data or `(DOMException name, message)`.

use base64ct::{Base64UrlUnpadded, Encoding};
use rand_core::{OsRng, RngCore};
use sha2::Digest;

/// A plain value crossing the boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum V {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Bytes(Vec<u8>),
    Arr(Vec<V>),
    Obj(Vec<(String, V)>),
}

/// (DOMException name, message). "TypeError" names a TypeError, not a DOMException.
pub type Error = (String, String);
type R<T> = Result<T, Error>;

fn err(name: &str, msg: impl Into<String>) -> Error {
    (name.to_string(), msg.into())
}
fn data_err(msg: impl Into<String>) -> Error {
    err("DataError", msg)
}
fn op_err(msg: impl Into<String>) -> Error {
    err("OperationError", msg)
}
fn ns_err(msg: impl Into<String>) -> Error {
    err("NotSupportedError", msg)
}
fn ia_err(msg: impl Into<String>) -> Error {
    err("InvalidAccessError", msg)
}
fn type_err(msg: impl Into<String>) -> Error {
    err("TypeError", msg)
}

impl V {
    /// Member of an object (None when missing or not an object).
    pub fn get(&self, k: &str) -> Option<&V> {
        match self {
            V::Obj(p) => p.iter().find(|(n, _)| n == k).map(|(_, v)| v),
            _ => None,
        }
    }
    /// Member that is present and not null/undefined.
    fn opt(&self, k: &str) -> Option<&V> {
        self.get(k).filter(|v| !matches!(v, V::Undefined | V::Null))
    }
    fn os(&self, k: &str) -> Option<&str> {
        match self.opt(k) {
            Some(V::Str(s)) => Some(s),
            _ => None,
        }
    }
    fn s(&self, k: &str) -> R<&str> {
        self.os(k).ok_or_else(|| type_err(format!("Member '{k}' is required (string).")))
    }
    fn ob(&self, k: &str) -> Option<&[u8]> {
        match self.opt(k) {
            Some(V::Bytes(b)) => Some(b),
            _ => None,
        }
    }
    fn b(&self, k: &str) -> R<&[u8]> {
        self.ob(k).ok_or_else(|| type_err(format!("Member '{k}' is required (BufferSource).")))
    }
    fn n(&self, k: &str) -> Option<f64> {
        match self.opt(k) {
            Some(V::Num(n)) => Some(*n),
            _ => None,
        }
    }
}

fn obj(pairs: Vec<(&str, V)>) -> V {
    V::Obj(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}
fn b64e(b: &[u8]) -> V {
    V::Str(Base64UrlUnpadded::encode_string(b))
}
fn b64d(s: &str) -> R<Vec<u8>> {
    Base64UrlUnpadded::decode_vec(s.trim_end_matches('=')).map_err(|_| data_err("Invalid base64url data in JWK."))
}
fn jwk_bytes(jwk: &V, k: &str) -> R<Vec<u8>> {
    match jwk.os(k) {
        Some(s) => b64d(s),
        None => Err(data_err(format!("The JWK member \"{k}\" is missing."))),
    }
}
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
fn random(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    OsRng.fill_bytes(&mut v);
    v
}

// ------------------------------------------------------------------ hashes

#[derive(Clone, Copy, Debug, PartialEq)]
enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

macro_rules! with_hash {
    ($h:expr, $H:ident => $body:expr) => {
        match $h {
            Hash::Sha1 => {
                type $H = sha1::Sha1;
                $body
            }
            Hash::Sha256 => {
                type $H = sha2::Sha256;
                $body
            }
            Hash::Sha384 => {
                type $H = sha2::Sha384;
                $body
            }
            Hash::Sha512 => {
                type $H = sha2::Sha512;
                $body
            }
        }
    };
}

impl Hash {
    fn parse(s: &str) -> R<Hash> {
        match s.to_ascii_uppercase().as_str() {
            "SHA-1" => Ok(Hash::Sha1),
            "SHA-256" => Ok(Hash::Sha256),
            "SHA-384" => Ok(Hash::Sha384),
            "SHA-512" => Ok(Hash::Sha512),
            _ => Err(ns_err(format!("Unrecognized hash algorithm: {s}"))),
        }
    }
    /// From a member that is a name or `{name}`.
    fn of(alg: &V, member: &str) -> R<Hash> {
        match alg.opt(member) {
            Some(V::Str(s)) => Hash::parse(s),
            Some(o @ V::Obj(_)) => Hash::parse(o.s("name")?),
            _ => Err(type_err(format!("Member '{member}' is required (hash algorithm)."))),
        }
    }
    fn suffix(self) -> &'static str {
        match self {
            Hash::Sha1 => "1",
            Hash::Sha256 => "256",
            Hash::Sha384 => "384",
            Hash::Sha512 => "512",
        }
    }
    fn block_bits(self) -> u32 {
        match self {
            Hash::Sha1 | Hash::Sha256 => 512,
            Hash::Sha384 | Hash::Sha512 => 1024,
        }
    }
    fn digest(self, data: &[u8]) -> Vec<u8> {
        with_hash!(self, H => H::digest(data).to_vec())
    }
}

fn hmac(h: Hash, key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    with_hash!(h, H => {
        let mut m = <Hmac<H> as Mac>::new_from_slice(key).expect("HMAC accepts any key length");
        m.update(data);
        m.finalize().into_bytes().to_vec()
    })
}

// ------------------------------------------------------------------ algorithm names

const ALGS: &[&str] = &[
    "RSASSA-PKCS1-v1_5", "RSA-PSS", "RSA-OAEP", "ECDSA", "ECDH", "AES-CTR", "AES-CBC", "AES-GCM", "AES-KW", "HMAC",
    "SHA-1", "SHA-256", "SHA-384", "SHA-512", "HKDF", "PBKDF2",
];

fn canon(name: &str) -> R<&'static str> {
    ALGS.iter()
        .find(|a| a.eq_ignore_ascii_case(name))
        .copied()
        .ok_or_else(|| ns_err(format!("Unrecognized algorithm name: {name}")))
}

fn is_rsa(n: &str) -> bool {
    matches!(n, "RSASSA-PKCS1-v1_5" | "RSA-PSS" | "RSA-OAEP")
}
fn is_ec(n: &str) -> bool {
    matches!(n, "ECDSA" | "ECDH")
}
fn is_aes(n: &str) -> bool {
    matches!(n, "AES-CTR" | "AES-CBC" | "AES-GCM" | "AES-KW")
}

/// Run one SubtleCrypto operation.
///
/// Common members: `alg` is the normalized algorithm (or params) object with
/// a canonical-or-any-case `name`; hashes may be a name or `{name}`. For key
/// operations `key` is the key material bytes, `keyType` is "secret" /
/// "public" / "private", and `keyAlg` is the key's own algorithm (name, hash,
/// namedCurve, length) when it differs from `alg` (sign/encrypt params).
///
/// - `digest {alg, data}` -> Bytes
/// - `generateKey {alg}` -> `{type:"secret", key, length}` or
///   `{publicKey, privateKey, modulusLength?, publicExponent?, namedCurve?}`
/// - `importKey {alg, format, keyData}` (keyData Bytes, or a JWK Obj) ->
///   `{type, key, length?, modulusLength?, publicExponent?, namedCurve?}`
/// - `exportKey {alg, format, keyType, key}` -> Bytes, or a JWK Obj (without
///   `key_ops`/`ext`, which the caller adds)
/// - `encrypt` / `decrypt {alg, keyAlg?, key, data}` -> Bytes (AES-KW too)
/// - `sign {alg, keyAlg?, key, data}` -> Bytes
/// - `verify {alg, keyAlg?, key, data, signature}` -> Bool
/// - `deriveBits {alg, keyAlg?, key, length}` (ECDH: `alg.public` is the peer's
///   SPKI bytes; `length` Num or Null) -> Bytes
pub fn call(op: &str, args: &V) -> Result<V, Error> {
    let alg = args.opt("alg").ok_or_else(|| type_err("An algorithm is required."))?;
    let name = canon(alg.s("name")?)?;
    let key_alg = args.opt("keyAlg").unwrap_or(alg);
    match op {
        "digest" => Ok(V::Bytes(Hash::parse(name)?.digest(args.b("data")?))),
        "generateKey" => generate_key(name, alg),
        "importKey" => {
            let format = args.s("format")?;
            let data = args.opt("keyData").ok_or_else(|| type_err("Key data is required."))?;
            import_key(name, alg, format, data)
        }
        "exportKey" => {
            let ty = args.os("keyType").or_else(|| args.os("type")).unwrap_or("secret");
            export_key(name, alg, args.s("format")?, ty, args.b("key")?)
        }
        "encrypt" | "decrypt" => crypt(op == "encrypt", name, alg, key_alg, args.b("key")?, args.b("data")?).map(V::Bytes),
        "wrapKeyAesKw" => aes_kw_wrap(args.b("key")?, args.b("data")?).map(V::Bytes),
        "unwrapKeyAesKw" => aes_kw_unwrap(args.b("key")?, args.b("data")?).map(V::Bytes),
        "sign" => sign(name, alg, key_alg, args.b("key")?, args.b("data")?).map(V::Bytes),
        "verify" => verify(name, alg, key_alg, args.b("key")?, args.b("data")?, args.b("signature")?).map(V::Bool),
        "deriveBits" => {
            let length = match args.opt("length") {
                Some(V::Num(n)) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as usize),
                Some(V::Num(_)) => return Err(op_err("Invalid length.")),
                _ => None,
            };
            derive_bits(name, alg, key_alg, args.b("key")?, length).map(V::Bytes)
        }
        _ => Err(ns_err(format!("Unsupported operation: {op}"))),
    }
}

// ------------------------------------------------------------------ AES

enum Aes {
    A128(aes::Aes128),
    A192(aes::Aes192),
    A256(aes::Aes256),
}

impl Aes {
    fn new(key: &[u8]) -> R<Aes> {
        use aes::cipher::KeyInit;
        Ok(match key.len() {
            16 => Aes::A128(aes::Aes128::new_from_slice(key).unwrap()),
            24 => Aes::A192(aes::Aes192::new_from_slice(key).unwrap()),
            32 => Aes::A256(aes::Aes256::new_from_slice(key).unwrap()),
            _ => return Err(data_err("AES key data must be 128, 192 or 256 bits.")),
        })
    }
    fn enc(&self, b: &mut [u8; 16]) {
        use aes::cipher::BlockEncrypt;
        let blk = aes::Block::from_mut_slice(b);
        match self {
            Aes::A128(c) => c.encrypt_block(blk),
            Aes::A192(c) => c.encrypt_block(blk),
            Aes::A256(c) => c.encrypt_block(blk),
        }
    }
    fn dec(&self, b: &mut [u8; 16]) {
        use aes::cipher::BlockDecrypt;
        let blk = aes::Block::from_mut_slice(b);
        match self {
            Aes::A128(c) => c.decrypt_block(blk),
            Aes::A192(c) => c.decrypt_block(blk),
            Aes::A256(c) => c.decrypt_block(blk),
        }
    }
}

fn aes_cbc(encrypt: bool, key: &[u8], iv: &[u8], data: &[u8]) -> R<Vec<u8>> {
    if iv.len() != 16 {
        return Err(op_err("AES-CBC iv must be 16 bytes."));
    }
    let c = Aes::new(key)?;
    let mut prev = [0u8; 16];
    prev.copy_from_slice(iv);
    if encrypt {
        let pad = 16 - data.len() % 16;
        let mut buf = data.to_vec();
        buf.extend(std::iter::repeat(pad as u8).take(pad));
        for chunk in buf.chunks_mut(16) {
            let mut b = [0u8; 16];
            for i in 0..16 {
                b[i] = chunk[i] ^ prev[i];
            }
            c.enc(&mut b);
            chunk.copy_from_slice(&b);
            prev = b;
        }
        Ok(buf)
    } else {
        if data.is_empty() || data.len() % 16 != 0 {
            return Err(op_err("The provided data is not a multiple of the AES block size."));
        }
        let mut out = Vec::with_capacity(data.len());
        for chunk in data.chunks(16) {
            let mut b = [0u8; 16];
            b.copy_from_slice(chunk);
            c.dec(&mut b);
            for i in 0..16 {
                b[i] ^= prev[i];
            }
            out.extend_from_slice(&b);
            prev.copy_from_slice(chunk);
        }
        let pad = *out.last().unwrap() as usize;
        if pad == 0 || pad > 16 || !out[out.len() - pad..].iter().all(|&x| x as usize == pad) {
            return Err(op_err("Decryption failed (bad padding)."));
        }
        out.truncate(out.len() - pad);
        Ok(out)
    }
}

fn aes_ctr(key: &[u8], counter: &[u8], length: f64, data: &[u8]) -> R<Vec<u8>> {
    if counter.len() != 16 {
        return Err(op_err("AES-CTR counter must be 16 bytes."));
    }
    if !(length >= 1.0 && length <= 128.0) || length.fract() != 0.0 {
        return Err(op_err("AES-CTR length must be between 1 and 128."));
    }
    let bits = length as u32;
    let c = Aes::new(key)?;
    let blocks = (data.len() + 15) / 16;
    if bits < 128 && (blocks as u128) > (1u128 << bits) {
        return Err(op_err("The AES-CTR counter would wrap around."));
    }
    let ctr = u128::from_be_bytes(counter.try_into().unwrap());
    let mask = if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
    let mut out = data.to_vec();
    for (i, chunk) in out.chunks_mut(16).enumerate() {
        let v = (ctr & !mask) | (ctr.wrapping_add(i as u128) & mask);
        let mut b = v.to_be_bytes();
        c.enc(&mut b);
        for (x, k) in chunk.iter_mut().zip(b.iter()) {
            *x ^= k;
        }
    }
    Ok(out)
}

fn ghash(h: &[u8; 16], parts: &[&[u8]]) -> [u8; 16] {
    use ghash::universal_hash::{KeyInit, UniversalHash};
    let mut g = ghash::GHash::new(h.into());
    let mut lens = Vec::new();
    for p in parts {
        g.update_padded(p);
        lens.push((p.len() as u64) * 8);
    }
    let mut lb = [0u8; 16];
    if lens.len() == 2 {
        lb[..8].copy_from_slice(&lens[0].to_be_bytes());
        lb[8..].copy_from_slice(&lens[1].to_be_bytes());
    } else {
        lb[8..].copy_from_slice(&lens[0].to_be_bytes());
    }
    g.update(&[lb.into()]);
    g.finalize().into()
}

fn inc32(b: &mut [u8; 16]) {
    let n = u32::from_be_bytes(b[12..].try_into().unwrap()).wrapping_add(1);
    b[12..].copy_from_slice(&n.to_be_bytes());
}

/// AES-GCM (NIST SP 800-38D) with any IV length and tag lengths 32..128.
fn aes_gcm(encrypt: bool, key: &[u8], iv: &[u8], aad: &[u8], tag_bits: f64, data: &[u8]) -> R<Vec<u8>> {
    if ![32.0, 64.0, 96.0, 104.0, 112.0, 120.0, 128.0].contains(&tag_bits) {
        return Err(op_err(format!("{tag_bits} is not a valid AES-GCM tag length.")));
    }
    if iv.is_empty() {
        return Err(op_err("The AES-GCM iv must not be empty."));
    }
    let tlen = tag_bits as usize / 8;
    let c = Aes::new(key)?;
    let mut h = [0u8; 16];
    c.enc(&mut h);
    let j0: [u8; 16] = if iv.len() == 12 {
        let mut j = [0u8; 16];
        j[..12].copy_from_slice(iv);
        j[15] = 1;
        j
    } else {
        ghash(&h, &[iv])
    };
    let (input, tag_in) = if encrypt {
        (data, &[][..])
    } else {
        if data.len() < tlen {
            return Err(op_err("The AES-GCM ciphertext is shorter than the tag."));
        }
        data.split_at(data.len() - tlen)
    };
    let gctr = |src: &[u8]| -> Vec<u8> {
        let mut cb = j0;
        let mut out = src.to_vec();
        for chunk in out.chunks_mut(16) {
            inc32(&mut cb);
            let mut ks = cb;
            c.enc(&mut ks);
            for (x, k) in chunk.iter_mut().zip(ks.iter()) {
                *x ^= k;
            }
        }
        out
    };
    let tag_for = |ct: &[u8]| -> Vec<u8> {
        let s = ghash(&h, &[aad, ct]);
        let mut ek = j0;
        c.enc(&mut ek);
        (0..tlen).map(|i| ek[i] ^ s[i]).collect()
    };
    if encrypt {
        let mut ct = gctr(input);
        let tag = tag_for(&ct);
        ct.extend_from_slice(&tag);
        Ok(ct)
    } else {
        let tag = tag_for(input);
        if !ct_eq(&tag, tag_in) {
            return Err(op_err("The operation failed for an operation-specific reason"));
        }
        Ok(gctr(input))
    }
}

const KW_IV: [u8; 8] = [0xA6; 8];

/// AES Key Wrap (RFC 3394).
fn aes_kw_wrap(key: &[u8], data: &[u8]) -> R<Vec<u8>> {
    if data.len() < 16 || data.len() % 8 != 0 {
        return Err(op_err("AES-KW input must be a multiple of 8 bytes and at least 16 bytes."));
    }
    let c = Aes::new(key)?;
    let n = data.len() / 8;
    let mut a = KW_IV;
    let mut r: Vec<[u8; 8]> = data.chunks(8).map(|x| x.try_into().unwrap()).collect();
    for j in 0..6u64 {
        for i in 0..n {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            b[8..].copy_from_slice(&r[i]);
            c.enc(&mut b);
            let t = (n as u64) * j + (i as u64) + 1;
            a.copy_from_slice(&b[..8]);
            for (x, y) in a.iter_mut().zip(t.to_be_bytes()) {
                *x ^= y;
            }
            r[i].copy_from_slice(&b[8..]);
        }
    }
    let mut out = a.to_vec();
    for x in r {
        out.extend_from_slice(&x);
    }
    Ok(out)
}

fn aes_kw_unwrap(key: &[u8], data: &[u8]) -> R<Vec<u8>> {
    if data.len() < 24 || data.len() % 8 != 0 {
        return Err(op_err("AES-KW input must be a multiple of 8 bytes and at least 24 bytes."));
    }
    let c = Aes::new(key)?;
    let n = data.len() / 8 - 1;
    let mut a: [u8; 8] = data[..8].try_into().unwrap();
    let mut r: Vec<[u8; 8]> = data[8..].chunks(8).map(|x| x.try_into().unwrap()).collect();
    for j in (0..6u64).rev() {
        for i in (0..n).rev() {
            let t = (n as u64) * j + (i as u64) + 1;
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&a);
            for (x, y) in b[..8].iter_mut().zip(t.to_be_bytes()) {
                *x ^= y;
            }
            b[8..].copy_from_slice(&r[i]);
            c.dec(&mut b);
            a.copy_from_slice(&b[..8]);
            r[i].copy_from_slice(&b[8..]);
        }
    }
    if !ct_eq(&a, &KW_IV) {
        return Err(op_err("AES-KW integrity check failed."));
    }
    Ok(r.concat())
}

// ------------------------------------------------------------------ RSA

use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey};
use rsa::traits::{PrivateKeyParts, PublicKeyParts};
use rsa::{BigUint, RsaPrivateKey, RsaPublicKey};

fn rsa_priv(der: &[u8]) -> R<RsaPrivateKey> {
    let k = RsaPrivateKey::from_pkcs8_der(der).map_err(|_| data_err("Invalid RSA PKCS#8 key data."))?;
    k.validate().map_err(|_| data_err("Invalid RSA private key."))?;
    Ok(k)
}
fn rsa_pub(der: &[u8]) -> R<RsaPublicKey> {
    RsaPublicKey::from_public_key_der(der).map_err(|_| data_err("Invalid RSA SPKI key data."))
}
fn rsa_priv_der(k: &RsaPrivateKey) -> R<Vec<u8>> {
    Ok(k.to_pkcs8_der().map_err(|e| op_err(e.to_string()))?.as_bytes().to_vec())
}
fn rsa_pub_der(k: &RsaPublicKey) -> R<Vec<u8>> {
    Ok(k.to_public_key_der().map_err(|e| op_err(e.to_string()))?.as_bytes().to_vec())
}
fn rsa_info(k: &RsaPublicKey) -> Vec<(&'static str, V)> {
    vec![("modulusLength", V::Num(k.n().bits() as f64)), ("publicExponent", V::Bytes(k.e().to_bytes_be()))]
}
fn rsa_jwk_alg(name: &str, h: Hash) -> String {
    match name {
        "RSASSA-PKCS1-v1_5" => format!("RS{}", h.suffix()),
        "RSA-PSS" => format!("PS{}", h.suffix()),
        _ => match h {
            Hash::Sha1 => "RSA-OAEP".into(),
            _ => format!("RSA-OAEP-{}", h.suffix()),
        },
    }
}

fn rsa_sign(name: &str, alg: &V, h: Hash, key: &RsaPrivateKey, data: &[u8]) -> R<Vec<u8>> {
    use rsa::{Pkcs1v15Sign, Pss};
    let hashed = h.digest(data);
    let r = if name == "RSA-PSS" {
        let salt = salt_len(alg)?;
        with_hash!(h, H => key.sign_with_rng(&mut OsRng, Pss::new_with_salt::<H>(salt), &hashed))
    } else {
        with_hash!(h, H => key.sign(Pkcs1v15Sign::new::<H>(), &hashed))
    };
    r.map_err(|e| op_err(e.to_string()))
}

fn rsa_verify(name: &str, alg: &V, h: Hash, key: &RsaPublicKey, data: &[u8], sig: &[u8]) -> R<bool> {
    use rsa::{Pkcs1v15Sign, Pss};
    let hashed = h.digest(data);
    Ok(if name == "RSA-PSS" {
        let salt = salt_len(alg)?;
        with_hash!(h, H => key.verify(Pss::new_with_salt::<H>(salt), &hashed, sig)).is_ok()
    } else {
        with_hash!(h, H => key.verify(Pkcs1v15Sign::new::<H>(), &hashed, sig)).is_ok()
    })
}

fn salt_len(alg: &V) -> R<usize> {
    match alg.n("saltLength") {
        Some(n) if n >= 0.0 && n.fract() == 0.0 => Ok(n as usize),
        Some(_) => Err(type_err("Invalid saltLength.")),
        None => Err(type_err("Member 'saltLength' is required.")),
    }
}

fn oaep_label(alg: &V) -> R<Option<String>> {
    match alg.ob("label") {
        None => Ok(None),
        Some(b) if b.is_empty() => Ok(None),
        Some(b) => String::from_utf8(b.to_vec()).map(Some).map_err(|_| ns_err("RSA-OAEP labels must be valid UTF-8 here.")),
    }
}

fn rsa_oaep(encrypt: bool, alg: &V, h: Hash, key: &[u8], data: &[u8]) -> R<Vec<u8>> {
    use rsa::Oaep;
    let label = oaep_label(alg)?;
    let pad = || -> Oaep {
        with_hash!(h, H => match &label { Some(l) => Oaep::new_with_label::<H, _>(l), None => Oaep::new::<H>() })
    };
    if encrypt {
        rsa_pub(key)?.encrypt(&mut OsRng, pad(), data).map_err(|e| op_err(e.to_string()))
    } else {
        rsa_priv(key)?.decrypt(pad(), data).map_err(|_| op_err("The operation failed for an operation-specific reason"))
    }
}

fn rsa_generate(alg: &V) -> R<V> {
    let bits = alg.n("modulusLength").ok_or_else(|| type_err("Member 'modulusLength' is required."))?;
    let exp = alg.b("publicExponent")?;
    if !(256.0..=16384.0).contains(&bits) || bits % 8.0 != 0.0 {
        return Err(op_err("The modulus length must be a multiple of 8 between 256 and 16384."));
    }
    let e = BigUint::from_bytes_be(exp);
    if e != BigUint::from(3u32) && e != BigUint::from(65537u32) {
        return Err(op_err("The public exponent must be 3 or 65537."));
    }
    let k = RsaPrivateKey::new_with_exp(&mut OsRng, bits as usize, &e).map_err(|e| op_err(e.to_string()))?;
    let public = k.to_public_key();
    let mut out = vec![("privateKey", V::Bytes(rsa_priv_der(&k)?)), ("publicKey", V::Bytes(rsa_pub_der(&public)?))];
    out.extend(rsa_info(&public));
    Ok(obj(out))
}

fn rsa_import(name: &str, alg: &V, format: &str, data: &V) -> R<V> {
    let h = Hash::of(alg, "hash")?;
    let (ty, der, public) = match (format, data) {
        ("spki", V::Bytes(b)) => {
            let k = rsa_pub(b)?;
            ("public", rsa_pub_der(&k)?, k)
        }
        ("pkcs8", V::Bytes(b)) => {
            let k = rsa_priv(b)?;
            ("private", rsa_priv_der(&k)?, k.to_public_key())
        }
        ("jwk", jwk @ V::Obj(_)) => {
            if jwk.os("kty") != Some("RSA") {
                return Err(data_err("The JWK \"kty\" member was not \"RSA\"."));
            }
            if let Some(a) = jwk.os("alg") {
                if a != rsa_jwk_alg(name, h) {
                    return Err(data_err(format!("The JWK \"alg\" member was not \"{}\".", rsa_jwk_alg(name, h))));
                }
            }
            let n = BigUint::from_bytes_be(&jwk_bytes(jwk, "n")?);
            let e = BigUint::from_bytes_be(&jwk_bytes(jwk, "e")?);
            if jwk.opt("d").is_some() {
                let d = BigUint::from_bytes_be(&jwk_bytes(jwk, "d")?);
                let p = BigUint::from_bytes_be(&jwk_bytes(jwk, "p")?);
                let q = BigUint::from_bytes_be(&jwk_bytes(jwk, "q")?);
                let k = RsaPrivateKey::from_components(n, e, d, vec![p, q]).map_err(|_| data_err("Invalid RSA private JWK."))?;
                k.validate().map_err(|_| data_err("Invalid RSA private JWK."))?;
                ("private", rsa_priv_der(&k)?, k.to_public_key())
            } else {
                let k = RsaPublicKey::new(n, e).map_err(|_| data_err("Invalid RSA public JWK."))?;
                ("public", rsa_pub_der(&k)?, k)
            }
        }
        ("raw", _) => return Err(ns_err(format!("The 'raw' format is not supported for {name}."))),
        ("spki" | "pkcs8", _) => return Err(type_err("Key data must be a BufferSource.")),
        ("jwk", _) => return Err(type_err("Key data must be a JsonWebKey object.")),
        _ => return Err(ns_err(format!("Unsupported key format: {format}"))),
    };
    let mut out = vec![("type", V::Str(ty.into())), ("key", V::Bytes(der))];
    out.extend(rsa_info(&public));
    Ok(obj(out))
}

fn rsa_export(name: &str, alg: &V, format: &str, ty: &str, key: &[u8]) -> R<V> {
    match (format, ty) {
        ("spki", "public") => Ok(V::Bytes(rsa_pub_der(&rsa_pub(key)?)?)),
        ("pkcs8", "private") => Ok(V::Bytes(rsa_priv_der(&rsa_priv(key)?)?)),
        ("jwk", _) => {
            let h = Hash::of(alg, "hash")?;
            let mut out = vec![("kty", V::Str("RSA".into())), ("alg", V::Str(rsa_jwk_alg(name, h)))];
            if ty == "private" {
                let mut k = rsa_priv(key)?;
                k.precompute().map_err(|e| op_err(e.to_string()))?;
                let pr = k.primes();
                if pr.len() != 2 {
                    return Err(ns_err("Multi-prime RSA keys are not supported."));
                }
                let (dp, dq, qi) = match (k.dp(), k.dq(), k.crt_coefficient()) {
                    (Some(dp), Some(dq), Some(qi)) => (dp.clone(), dq.clone(), qi),
                    _ => return Err(op_err("RSA CRT values unavailable.")),
                };
                out.push(("n", b64e(&k.n().to_bytes_be())));
                out.push(("e", b64e(&k.e().to_bytes_be())));
                out.push(("d", b64e(&k.d().to_bytes_be())));
                out.push(("p", b64e(&pr[0].to_bytes_be())));
                out.push(("q", b64e(&pr[1].to_bytes_be())));
                out.push(("dp", b64e(&dp.to_bytes_be())));
                out.push(("dq", b64e(&dq.to_bytes_be())));
                out.push(("qi", b64e(&qi.to_bytes_be())));
            } else {
                let k = rsa_pub(key)?;
                out.push(("n", b64e(&k.n().to_bytes_be())));
                out.push(("e", b64e(&k.e().to_bytes_be())));
            }
            Ok(obj(out))
        }
        ("spki" | "pkcs8", _) => Err(ia_err("The key type does not match the export format.")),
        _ => Err(ns_err(format!("The '{format}' format is not supported for {name}."))),
    }
}

// ------------------------------------------------------------------ EC

#[derive(Clone, Copy, PartialEq, Debug)]
enum Curve {
    P256,
    P384,
    P521,
}

impl Curve {
    fn parse(s: &str) -> R<Curve> {
        match s {
            "P-256" => Ok(Curve::P256),
            "P-384" => Ok(Curve::P384),
            "P-521" => Ok(Curve::P521),
            _ => Err(ns_err(format!("Unsupported named curve: {s}"))),
        }
    }
    fn of(alg: &V) -> R<Curve> {
        Curve::parse(alg.s("namedCurve")?)
    }
    fn name(self) -> &'static str {
        match self {
            Curve::P256 => "P-256",
            Curve::P384 => "P-384",
            Curve::P521 => "P-521",
        }
    }
    fn es_alg(self) -> &'static str {
        match self {
            Curve::P256 => "ES256",
            Curve::P384 => "ES384",
            Curve::P521 => "ES512",
        }
    }
}

macro_rules! ec_curve {
    ($m:ident, $c:ident, $flen:expr) => {
        mod $m {
            use super::{data_err, op_err, R};
            use $c::ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
            use $c::ecdsa::{Signature, SigningKey, VerifyingKey};
            use $c::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
            use $c::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey, EncodePublicKey};
            use $c::{EncodedPoint, FieldBytes, PublicKey, SecretKey};
            use rand_core::OsRng;

            pub const FLEN: usize = $flen;

            fn sk(der: &[u8]) -> R<SecretKey> {
                SecretKey::from_pkcs8_der(der).map_err(|_| data_err("Invalid EC PKCS#8 key data (or wrong curve)."))
            }
            fn pk(der: &[u8]) -> R<PublicKey> {
                PublicKey::from_public_key_der(der).map_err(|_| data_err("Invalid EC SPKI key data (or wrong curve)."))
            }
            fn sk_der(k: &SecretKey) -> R<Vec<u8>> {
                Ok(k.to_pkcs8_der().map_err(|e| op_err(e.to_string()))?.as_bytes().to_vec())
            }
            fn pk_der(k: &PublicKey) -> R<Vec<u8>> {
                Ok(k.to_public_key_der().map_err(|e| op_err(e.to_string()))?.as_bytes().to_vec())
            }
            pub fn generate() -> R<(Vec<u8>, Vec<u8>)> {
                let k = SecretKey::random(&mut OsRng);
                Ok((sk_der(&k)?, pk_der(&k.public_key())?))
            }
            pub fn canon_pkcs8(der: &[u8]) -> R<Vec<u8>> {
                sk_der(&sk(der)?)
            }
            pub fn canon_spki(der: &[u8]) -> R<Vec<u8>> {
                pk_der(&pk(der)?)
            }
            pub fn raw_to_spki(raw: &[u8]) -> R<Vec<u8>> {
                pk_der(&PublicKey::from_sec1_bytes(raw).map_err(|_| data_err("Invalid EC point."))?)
            }
            pub fn spki_to_raw(der: &[u8]) -> R<Vec<u8>> {
                Ok(pk(der)?.to_encoded_point(false).as_bytes().to_vec())
            }
            fn point(x: &[u8], y: &[u8]) -> R<PublicKey> {
                if x.len() != FLEN || y.len() != FLEN {
                    return Err(data_err("JWK coordinates have the wrong length."));
                }
                let ep = EncodedPoint::from_affine_coordinates(FieldBytes::from_slice(x), FieldBytes::from_slice(y), false);
                Option::from(PublicKey::from_encoded_point(&ep)).ok_or_else(|| data_err("The JWK point is not on the curve."))
            }
            pub fn jwk_public(x: &[u8], y: &[u8]) -> R<Vec<u8>> {
                pk_der(&point(x, y)?)
            }
            pub fn jwk_private(x: &[u8], y: &[u8], d: &[u8]) -> R<Vec<u8>> {
                let p = point(x, y)?;
                if d.len() != FLEN {
                    return Err(data_err("The JWK \"d\" member has the wrong length."));
                }
                let k = SecretKey::from_bytes(FieldBytes::from_slice(d)).map_err(|_| data_err("Invalid EC private key."))?;
                if k.public_key() != p {
                    return Err(data_err("The JWK public and private parts do not match."));
                }
                sk_der(&k)
            }
            /// (x, y) of a public key.
            pub fn xy(spki: &[u8]) -> R<(Vec<u8>, Vec<u8>)> {
                let ep = pk(spki)?.to_encoded_point(false);
                Ok((ep.x().unwrap().to_vec(), ep.y().unwrap().to_vec()))
            }
            /// (x, y, d) of a private key.
            pub fn xyd(pkcs8: &[u8]) -> R<(Vec<u8>, Vec<u8>, Vec<u8>)> {
                let k = sk(pkcs8)?;
                let ep = k.public_key().to_encoded_point(false);
                Ok((ep.x().unwrap().to_vec(), ep.y().unwrap().to_vec(), k.to_bytes().to_vec()))
            }
            /// The digest as a field-size value (left-padded when shorter).
            fn prehash(digest: &[u8]) -> Vec<u8> {
                if digest.len() >= FLEN {
                    digest.to_vec()
                } else {
                    let mut v = vec![0u8; FLEN - digest.len()];
                    v.extend_from_slice(digest);
                    v
                }
            }
            pub fn sign(pkcs8: &[u8], digest: &[u8]) -> R<Vec<u8>> {
                let s = SigningKey::from_bytes(&sk(pkcs8)?.to_bytes()).map_err(|e| op_err(e.to_string()))?;
                let sig: Signature = s.sign_prehash(&prehash(digest)).map_err(|e| op_err(e.to_string()))?;
                Ok(sig.to_bytes().to_vec())
            }
            pub fn verify(spki: &[u8], digest: &[u8], sig: &[u8]) -> R<bool> {
                let v = VerifyingKey::from_affine(*pk(spki)?.as_affine()).map_err(|_| data_err("Invalid EC public key."))?;
                let Ok(sig) = Signature::from_slice(sig) else { return Ok(false) };
                Ok(v.verify_prehash(&prehash(digest), &sig).is_ok())
            }
            pub fn ecdh(pkcs8: &[u8], spki: &[u8]) -> R<Vec<u8>> {
                let s = sk(pkcs8)?;
                let p = pk(spki).map_err(|_| super::ia_err("The public key is not on the key's curve."))?;
                Ok($c::ecdh::diffie_hellman(s.to_nonzero_scalar(), p.as_affine()).raw_secret_bytes().to_vec())
            }
        }
    };
}

ec_curve!(ec256, p256, 32);
ec_curve!(ec384, p384, 48);
ec_curve!(ec521, p521, 66);

macro_rules! on_curve {
    ($curve:expr, $f:ident ( $($a:expr),* )) => {
        match $curve {
            Curve::P256 => ec256::$f($($a),*),
            Curve::P384 => ec384::$f($($a),*),
            Curve::P521 => ec521::$f($($a),*),
        }
    };
}

fn ec_import(name: &str, alg: &V, format: &str, data: &V) -> R<V> {
    let curve = Curve::of(alg)?;
    let (ty, der) = match (format, data) {
        ("raw", V::Bytes(b)) => ("public", on_curve!(curve, raw_to_spki(b))?),
        ("spki", V::Bytes(b)) => ("public", on_curve!(curve, canon_spki(b))?),
        ("pkcs8", V::Bytes(b)) => ("private", on_curve!(curve, canon_pkcs8(b))?),
        ("jwk", jwk @ V::Obj(_)) => {
            if jwk.os("kty") != Some("EC") {
                return Err(data_err("The JWK \"kty\" member was not \"EC\"."));
            }
            if jwk.os("crv") != Some(curve.name()) {
                return Err(data_err("The JWK \"crv\" member did not match the namedCurve."));
            }
            if name == "ECDSA" {
                if let Some(a) = jwk.os("alg") {
                    if a != curve.es_alg() {
                        return Err(data_err(format!("The JWK \"alg\" member was not \"{}\".", curve.es_alg())));
                    }
                }
            }
            let x = jwk_bytes(jwk, "x")?;
            let y = jwk_bytes(jwk, "y")?;
            if jwk.opt("d").is_some() {
                ("private", on_curve!(curve, jwk_private(&x, &y, &jwk_bytes(jwk, "d")?))?)
            } else {
                ("public", on_curve!(curve, jwk_public(&x, &y))?)
            }
        }
        ("raw" | "spki" | "pkcs8", _) => return Err(type_err("Key data must be a BufferSource.")),
        ("jwk", _) => return Err(type_err("Key data must be a JsonWebKey object.")),
        _ => return Err(ns_err(format!("Unsupported key format: {format}"))),
    };
    Ok(obj(vec![("type", V::Str(ty.into())), ("key", V::Bytes(der)), ("namedCurve", V::Str(curve.name().into()))]))
}

fn ec_export(name: &str, alg: &V, format: &str, ty: &str, key: &[u8]) -> R<V> {
    let curve = Curve::of(alg)?;
    match (format, ty) {
        ("raw", "public") => Ok(V::Bytes(on_curve!(curve, spki_to_raw(key))?)),
        ("spki", "public") => Ok(V::Bytes(on_curve!(curve, canon_spki(key))?)),
        ("pkcs8", "private") => Ok(V::Bytes(on_curve!(curve, canon_pkcs8(key))?)),
        ("jwk", _) => {
            let mut out = vec![("kty", V::Str("EC".into())), ("crv", V::Str(curve.name().into()))];
            if ty == "private" {
                let (x, y, d) = on_curve!(curve, xyd(key))?;
                out.push(("x", b64e(&x)));
                out.push(("y", b64e(&y)));
                out.push(("d", b64e(&d)));
            } else {
                let (x, y) = on_curve!(curve, xy(key))?;
                out.push(("x", b64e(&x)));
                out.push(("y", b64e(&y)));
            }
            let _ = name;
            Ok(obj(out))
        }
        ("raw" | "spki" | "pkcs8", _) => Err(ia_err("The key type does not match the export format.")),
        _ => Err(ns_err(format!("Unsupported key format: {format}"))),
    }
}

// ------------------------------------------------------------------ symmetric import/export

fn aes_jwk_alg(name: &str, bits: usize) -> String {
    format!("A{bits}{}", &name[4..])
}

fn secret_import(name: &str, alg: &V, format: &str, data: &V) -> R<V> {
    let raw: Vec<u8> = match (format, data) {
        ("raw", V::Bytes(b)) => b.clone(),
        ("jwk", jwk @ V::Obj(_)) if is_aes(name) || name == "HMAC" => {
            if jwk.os("kty") != Some("oct") {
                return Err(data_err("The JWK \"kty\" member was not \"oct\"."));
            }
            let k = jwk_bytes(jwk, "k")?;
            if let Some(a) = jwk.os("alg") {
                let want = if name == "HMAC" { format!("HS{}", Hash::of(alg, "hash")?.suffix()) } else { aes_jwk_alg(name, k.len() * 8) };
                if a != want {
                    return Err(data_err(format!("The JWK \"alg\" member was not \"{want}\".")));
                }
            }
            k
        }
        ("raw", _) => return Err(type_err("Key data must be a BufferSource.")),
        ("jwk", V::Obj(_)) => return Err(ns_err(format!("The 'jwk' format is not supported for {name}."))),
        ("jwk", _) => return Err(type_err("Key data must be a JsonWebKey object.")),
        ("spki" | "pkcs8", _) => return Err(ns_err(format!("The '{format}' format is not supported for {name}."))),
        _ => return Err(ns_err(format!("Unsupported key format: {format}"))),
    };
    let mut out = vec![("type", V::Str("secret".into()))];
    if is_aes(name) {
        if ![16, 24, 32].contains(&raw.len()) {
            return Err(data_err("AES key data must be 128, 192 or 256 bits."));
        }
        out.push(("length", V::Num((raw.len() * 8) as f64)));
    } else if name == "HMAC" {
        Hash::of(alg, "hash")?;
        if raw.is_empty() {
            return Err(data_err("HMAC key data must not be empty."));
        }
        let bits = raw.len() * 8;
        let len = match alg.n("length") {
            Some(l) => {
                if l <= 0.0 || l > bits as f64 || l <= (bits - 8) as f64 || l.fract() != 0.0 {
                    return Err(data_err("The HMAC length does not match the key data."));
                }
                l
            }
            None => bits as f64,
        };
        out.push(("length", V::Num(len)));
    }
    out.push(("key", V::Bytes(raw)));
    Ok(obj(out))
}

fn secret_export(name: &str, alg: &V, format: &str, key: &[u8]) -> R<V> {
    match format {
        "raw" if is_aes(name) || name == "HMAC" => Ok(V::Bytes(key.to_vec())),
        "jwk" if is_aes(name) || name == "HMAC" => {
            let a = if name == "HMAC" { format!("HS{}", Hash::of(alg, "hash")?.suffix()) } else { aes_jwk_alg(name, key.len() * 8) };
            Ok(obj(vec![("kty", V::Str("oct".into())), ("k", b64e(key)), ("alg", V::Str(a))]))
        }
        _ => Err(ns_err(format!("The '{format}' format is not supported for {name}."))),
    }
}

// ------------------------------------------------------------------ operations

fn generate_key(name: &str, alg: &V) -> R<V> {
    if is_aes(name) {
        let len = alg.n("length").ok_or_else(|| type_err("Member 'length' is required."))?;
        if ![128.0, 192.0, 256.0].contains(&len) {
            return Err(op_err("AES key length must be 128, 192 or 256 bits."));
        }
        return Ok(obj(vec![("type", V::Str("secret".into())), ("key", V::Bytes(random(len as usize / 8))), ("length", V::Num(len))]));
    }
    if name == "HMAC" {
        let h = Hash::of(alg, "hash")?;
        let len = match alg.n("length") {
            Some(l) if l > 0.0 && l.fract() == 0.0 => l as usize,
            Some(_) => return Err(op_err("Invalid HMAC key length.")),
            None => h.block_bits() as usize,
        };
        let mut key = random((len + 7) / 8);
        if len % 8 != 0 {
            *key.last_mut().unwrap() &= 0xffu8 << (8 - len % 8);
        }
        return Ok(obj(vec![("type", V::Str("secret".into())), ("key", V::Bytes(key)), ("length", V::Num(len as f64))]));
    }
    if is_rsa(name) {
        Hash::of(alg, "hash")?;
        return rsa_generate(alg);
    }
    if is_ec(name) {
        let curve = Curve::of(alg)?;
        let (private, public) = on_curve!(curve, generate())?;
        return Ok(obj(vec![
            ("privateKey", V::Bytes(private)),
            ("publicKey", V::Bytes(public)),
            ("namedCurve", V::Str(curve.name().into())),
        ]));
    }
    Err(ns_err(format!("generateKey is not supported for {name}.")))
}

fn import_key(name: &str, alg: &V, format: &str, data: &V) -> R<V> {
    if is_rsa(name) {
        return rsa_import(name, alg, format, data);
    }
    if is_ec(name) {
        return ec_import(name, alg, format, data);
    }
    if is_aes(name) || name == "HMAC" {
        return secret_import(name, alg, format, data);
    }
    if name == "PBKDF2" || name == "HKDF" {
        return match (format, data) {
            ("raw", V::Bytes(b)) => Ok(obj(vec![("type", V::Str("secret".into())), ("key", V::Bytes(b.clone()))])),
            ("raw", _) => Err(type_err("Key data must be a BufferSource.")),
            _ => Err(ns_err(format!("The '{format}' format is not supported for {name}."))),
        };
    }
    Err(ns_err(format!("importKey is not supported for {name}.")))
}

fn export_key(name: &str, alg: &V, format: &str, ty: &str, key: &[u8]) -> R<V> {
    if is_rsa(name) {
        return rsa_export(name, alg, format, ty, key);
    }
    if is_ec(name) {
        return ec_export(name, alg, format, ty, key);
    }
    secret_export(name, alg, format, key)
}

fn crypt(encrypt: bool, name: &str, alg: &V, key_alg: &V, key: &[u8], data: &[u8]) -> R<Vec<u8>> {
    match name {
        "AES-GCM" => {
            let iv = alg.b("iv")?;
            let aad = alg.ob("additionalData").unwrap_or(&[]);
            let tag = alg.n("tagLength").unwrap_or(128.0);
            aes_gcm(encrypt, key, iv, aad, tag, data)
        }
        "AES-CBC" => aes_cbc(encrypt, key, alg.b("iv")?, data),
        "AES-CTR" => {
            let length = alg.n("length").ok_or_else(|| type_err("Member 'length' is required."))?;
            aes_ctr(key, alg.b("counter")?, length, data)
        }
        "AES-KW" => {
            if encrypt {
                aes_kw_wrap(key, data)
            } else {
                aes_kw_unwrap(key, data)
            }
        }
        "RSA-OAEP" => rsa_oaep(encrypt, alg, Hash::of(key_alg, "hash")?, key, data),
        _ => Err(ns_err(format!("{} is not supported for {name}.", if encrypt { "encrypt" } else { "decrypt" }))),
    }
}

fn sign(name: &str, alg: &V, key_alg: &V, key: &[u8], data: &[u8]) -> R<Vec<u8>> {
    match name {
        "HMAC" => Ok(hmac(Hash::of(key_alg, "hash")?, key, data)),
        "RSASSA-PKCS1-v1_5" | "RSA-PSS" => rsa_sign(name, alg, Hash::of(key_alg, "hash")?, &rsa_priv(key)?, data),
        "ECDSA" => {
            let digest = Hash::of(alg, "hash")?.digest(data);
            on_curve!(Curve::of(key_alg)?, sign(key, &digest))
        }
        _ => Err(ns_err(format!("sign is not supported for {name}."))),
    }
}

fn verify(name: &str, alg: &V, key_alg: &V, key: &[u8], data: &[u8], sig: &[u8]) -> R<bool> {
    match name {
        "HMAC" => Ok(ct_eq(&hmac(Hash::of(key_alg, "hash")?, key, data), sig)),
        "RSASSA-PKCS1-v1_5" | "RSA-PSS" => rsa_verify(name, alg, Hash::of(key_alg, "hash")?, &rsa_pub(key)?, data, sig),
        "ECDSA" => {
            let digest = Hash::of(alg, "hash")?.digest(data);
            on_curve!(Curve::of(key_alg)?, verify(key, &digest, sig))
        }
        _ => Err(ns_err(format!("verify is not supported for {name}."))),
    }
}

fn derive_bits(name: &str, alg: &V, key_alg: &V, key: &[u8], length: Option<usize>) -> R<Vec<u8>> {
    let need_len = || -> R<usize> {
        let l = length.ok_or_else(|| op_err("A length is required."))?;
        if l % 8 != 0 {
            return Err(op_err("The length must be a multiple of 8."));
        }
        Ok(l / 8)
    };
    match name {
        "PBKDF2" => {
            let h = Hash::of(alg, "hash")?;
            let salt = alg.b("salt")?;
            let iters = alg.n("iterations").ok_or_else(|| type_err("Member 'iterations' is required."))?;
            if iters < 1.0 || iters > u32::MAX as f64 {
                return Err(op_err("PBKDF2 iterations must be at least 1."));
            }
            let mut out = vec![0u8; need_len()?];
            with_hash!(h, H => pbkdf2::pbkdf2_hmac::<H>(key, salt, iters as u32, &mut out));
            Ok(out)
        }
        "HKDF" => {
            let h = Hash::of(alg, "hash")?;
            let salt = alg.b("salt")?;
            let info = alg.b("info")?;
            let mut out = vec![0u8; need_len()?];
            with_hash!(h, H => hkdf::Hkdf::<H>::new(Some(salt), key).expand(info, &mut out))
                .map_err(|_| op_err("The HKDF output length is too large."))?;
            Ok(out)
        }
        "ECDH" => {
            let peer = alg.b("public")?;
            let curve = Curve::of(key_alg)?;
            let mut secret = on_curve!(curve, ecdh(key, peer))?;
            if let Some(l) = length {
                if l > secret.len() * 8 {
                    return Err(op_err("The length is too large for this curve."));
                }
                secret.truncate((l + 7) / 8);
                if l % 8 != 0 {
                    *secret.last_mut().unwrap() &= 0xffu8 << (8 - l % 8);
                }
            }
            Ok(secret)
        }
        _ => Err(ns_err(format!("deriveBits is not supported for {name}."))),
    }
}

#[cfg(test)]
mod tests;
