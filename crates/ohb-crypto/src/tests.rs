// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: MIT

use super::*;

fn hx(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}
fn o(pairs: Vec<(&str, V)>) -> V {
    obj(pairs)
}
fn s(x: &str) -> V {
    V::Str(x.into())
}
fn b(x: &[u8]) -> V {
    V::Bytes(x.to_vec())
}
fn bytes(v: V) -> Vec<u8> {
    match v {
        V::Bytes(b) => b,
        other => panic!("expected bytes, got {other:?}"),
    }
}
fn member(v: &V, k: &str) -> V {
    v.get(k).cloned().unwrap_or(V::Undefined)
}

fn enc(alg: V, key: &[u8], data: &[u8]) -> Result<Vec<u8>, Error> {
    call("encrypt", &o(vec![("alg", alg), ("key", b(key)), ("data", b(data))])).map(bytes)
}
fn dec(alg: V, key: &[u8], data: &[u8]) -> Result<Vec<u8>, Error> {
    call("decrypt", &o(vec![("alg", alg), ("key", b(key)), ("data", b(data))])).map(bytes)
}

#[test]
fn aes_gcm_nist() {
    // Test case 2.
    let key = vec![0u8; 16];
    let alg = o(vec![("name", s("AES-GCM")), ("iv", b(&[0u8; 12]))]);
    let ct = enc(alg.clone(), &key, &[0u8; 16]).unwrap();
    assert_eq!(ct, hx("0388dace60b6a392f328c2b971b2fe78 ab6e47d42cec13bdf53a67b21257bddf"));
    assert_eq!(dec(alg, &key, &ct).unwrap(), vec![0u8; 16]);
    // Test cases 4 and 6 (AAD, 60-byte IV).
    let key = hx("feffe9928665731c6d6a8f9467308308");
    let pt = hx("d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39");
    let aad = hx("feedfacedeadbeeffeedfacedeadbeefabaddad2");
    let alg = o(vec![("name", s("aes-gcm")), ("iv", b(&hx("cafebabefacedbaddecaf888"))), ("additionalData", b(&aad))]);
    let ct = enc(alg.clone(), &key, &pt).unwrap();
    assert_eq!(
        ct,
        hx("42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091 5bc94fbc3221a5db94fae95ae7121a47")
    );
    assert_eq!(dec(alg.clone(), &key, &ct).unwrap(), pt);
    let mut bad = ct.clone();
    bad[3] ^= 1;
    assert_eq!(dec(alg, &key, &bad).unwrap_err().0, "OperationError");
    let iv6 = hx("9313225df88406e555909c5aff5269aa6a7a9538534f7da1e4c303d2a318a728c3c0c95156809539fcf0e2429a6b525416aedbf5a0de6a57a637b39b");
    let alg = o(vec![("name", s("AES-GCM")), ("iv", b(&iv6)), ("additionalData", b(&aad)), ("tagLength", V::Num(96.0))]);
    let ct = enc(alg.clone(), &key, &pt).unwrap();
    assert_eq!(
        ct,
        hx("8ce24998625615b603a033aca13fb894be9112a5c3a211a8ba262a3cca7e2ca701e4a9a4fba43c90ccdcb281d48c7c6fd62875d2aca417034c34aee5 619cc5aefffe0bfa462af43c")
    );
    assert_eq!(dec(alg, &key, &ct).unwrap(), pt);
}

#[test]
fn aes_cbc_ctr_kw_vectors() {
    let key = hx("2b7e151628aed2a6abf7158809cf4f3c");
    let pt = hx("6bc1bee22e409f96e93d7e117393172a");
    let alg = o(vec![("name", s("AES-CBC")), ("iv", b(&hx("000102030405060708090a0b0c0d0e0f")))]);
    let ct = enc(alg.clone(), &key, &pt).unwrap();
    assert_eq!(ct.len(), 32);
    assert_eq!(&ct[..16], &hx("7649abac8119b246cee98e9b12e9197d")[..]);
    assert_eq!(dec(alg.clone(), &key, &ct).unwrap(), pt);
    assert_eq!(dec(alg, &key, &ct[..16]).unwrap_err().0, "OperationError");

    let alg = o(vec![("name", s("AES-CTR")), ("counter", b(&hx("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"))), ("length", V::Num(64.0))]);
    let ct = enc(alg.clone(), &key, &pt).unwrap();
    assert_eq!(ct, hx("874d6191b620e3261bef6864990db6ce"));
    assert_eq!(dec(alg, &key, &ct).unwrap(), pt);
    // Counter wrap within the low bits: 2 blocks with a 1-bit counter is fine, 3 is not.
    let alg = o(vec![("name", s("AES-CTR")), ("counter", b(&[0u8; 16])), ("length", V::Num(1.0))]);
    assert!(enc(alg.clone(), &key, &[0u8; 32]).is_ok());
    assert_eq!(enc(alg, &key, &[0u8; 33]).unwrap_err().0, "OperationError");

    // RFC 3394 4.1
    let kek = hx("000102030405060708090A0B0C0D0E0F");
    let kd = hx("00112233445566778899AABBCCDDEEFF");
    let alg = o(vec![("name", s("AES-KW"))]);
    let w = enc(alg.clone(), &kek, &kd).unwrap();
    assert_eq!(w, hx("1FA68B0A8112B447AEF34BD8FB5A7B829D3E862371D2CFE5"));
    assert_eq!(dec(alg.clone(), &kek, &w).unwrap(), kd);
    let mut bad = w.clone();
    bad[0] ^= 1;
    assert_eq!(dec(alg, &kek, &bad).unwrap_err().0, "OperationError");
    // RFC 3394 4.6 (256-bit KEK, 256-bit data)
    let kek = hx("000102030405060708090A0B0C0D0E0F101112131415161718191A1B1C1D1E1F");
    let kd = hx("00112233445566778899AABBCCDDEEFF000102030405060708090A0B0C0D0E0F");
    let w = call("wrapKeyAesKw", &o(vec![("alg", o(vec![("name", s("AES-KW"))])), ("key", b(&kek)), ("data", b(&kd))])).map(bytes).unwrap();
    assert_eq!(w, hx("28C9F404C4B810F4CBCCB35CFB87F8263F5786E2D80ED326CBC7F0E71A99F43BFB988B9B7A02DD21"));
}

fn hmac_sign(hash: &str, key: &[u8], data: &[u8]) -> Vec<u8> {
    let ka = o(vec![("name", s("HMAC")), ("hash", o(vec![("name", s(hash))]))]);
    bytes(call("sign", &o(vec![("alg", o(vec![("name", s("HMAC"))])), ("keyAlg", ka), ("key", b(key)), ("data", b(data))])).unwrap())
}

#[test]
fn hmac_rfc4231() {
    let key = [0x0bu8; 20];
    assert_eq!(hmac_sign("SHA-256", &key, b"Hi There"), hx("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"));
    assert_eq!(
        hmac_sign("SHA-512", &key, b"Hi There"),
        hx("87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cdedaa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854")
    );
    let sig = hmac_sign("SHA-256", b"Jefe", b"what do ya want for nothing?");
    assert_eq!(sig, hx("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"));
    let ka = o(vec![("name", s("HMAC")), ("hash", s("SHA-256"))]);
    let args = |sig: &[u8]| {
        o(vec![
            ("alg", o(vec![("name", s("HMAC"))])),
            ("keyAlg", ka.clone()),
            ("key", b(b"Jefe")),
            ("data", b(b"what do ya want for nothing?")),
            ("signature", b(sig)),
        ])
    };
    assert_eq!(call("verify", &args(&sig)).unwrap(), V::Bool(true));
    assert_eq!(call("verify", &args(&sig[..31])).unwrap(), V::Bool(false));
}

#[test]
fn pbkdf2_hkdf_vectors() {
    let pb = |iters: f64| {
        let alg = o(vec![("name", s("PBKDF2")), ("hash", s("SHA-1")), ("salt", b(b"salt")), ("iterations", V::Num(iters))]);
        bytes(call("deriveBits", &o(vec![("alg", alg), ("key", b(b"password")), ("length", V::Num(160.0))])).unwrap())
    };
    assert_eq!(pb(1.0), hx("0c60c80f961f0e71f3a9b524af6012062fe037a6"));
    assert_eq!(pb(2.0), hx("ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957"));
    assert_eq!(pb(4096.0), hx("4b007901b765489abead49d926f721d065a429c1"));

    let alg = o(vec![
        ("name", s("HKDF")),
        ("hash", s("SHA-256")),
        ("salt", b(&hx("000102030405060708090a0b0c"))),
        ("info", b(&hx("f0f1f2f3f4f5f6f7f8f9"))),
    ]);
    let okm = bytes(call("deriveBits", &o(vec![("alg", alg.clone()), ("key", b(&[0x0bu8; 22])), ("length", V::Num(336.0))])).unwrap());
    assert_eq!(okm, hx("3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"));
    assert_eq!(call("deriveBits", &o(vec![("alg", alg.clone()), ("key", b(&[1])), ("length", V::Null)])).unwrap_err().0, "OperationError");
    assert_eq!(call("deriveBits", &o(vec![("alg", alg), ("key", b(&[1])), ("length", V::Num(12.0))])).unwrap_err().0, "OperationError");
}

#[test]
fn digest_and_errors() {
    let d = bytes(call("digest", &o(vec![("alg", o(vec![("name", s("sha-256"))])), ("data", b(b"abc"))])).unwrap());
    assert_eq!(d, hx("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
    assert_eq!(call("digest", &o(vec![("alg", o(vec![("name", s("MD5"))])), ("data", b(b""))])).unwrap_err().0, "NotSupportedError");
    let e = call("importKey", &o(vec![("alg", o(vec![("name", s("AES-GCM"))])), ("format", s("raw")), ("keyData", b(&[0u8; 7]))])).unwrap_err();
    assert_eq!(e.0, "DataError");
    let e = call("importKey", &o(vec![("alg", o(vec![("name", s("AES-GCM"))])), ("format", s("spki")), ("keyData", b(&[0u8; 16]))])).unwrap_err();
    assert_eq!(e.0, "NotSupportedError");
}

#[test]
fn secret_jwk_round_trip() {
    let g = call("generateKey", &o(vec![("alg", o(vec![("name", s("AES-GCM")), ("length", V::Num(256.0))]))])).unwrap();
    let key = bytes(member(&g, "key"));
    assert_eq!(key.len(), 32);
    let alg = o(vec![("name", s("AES-GCM")), ("length", V::Num(256.0))]);
    let jwk = call("exportKey", &o(vec![("alg", alg.clone()), ("format", s("jwk")), ("keyType", s("secret")), ("key", b(&key))])).unwrap();
    assert_eq!(member(&jwk, "alg"), s("A256GCM"));
    assert_eq!(member(&jwk, "kty"), s("oct"));
    let imp = call("importKey", &o(vec![("alg", alg), ("format", s("jwk")), ("keyData", jwk)])).unwrap();
    assert_eq!(bytes(member(&imp, "key")), key);
    assert_eq!(member(&imp, "length"), V::Num(256.0));

    let g = call("generateKey", &o(vec![("alg", o(vec![("name", s("HMAC")), ("hash", s("SHA-512"))]))])).unwrap();
    assert_eq!(member(&g, "length"), V::Num(1024.0));
    let alg = o(vec![("name", s("HMAC")), ("hash", o(vec![("name", s("SHA-512"))]))]);
    let jwk = call("exportKey", &o(vec![("alg", alg.clone()), ("format", s("jwk")), ("keyType", s("secret")), ("key", member(&g, "key"))])).unwrap();
    assert_eq!(member(&jwk, "alg"), s("HS512"));
    let bad = o(vec![("name", s("HMAC")), ("hash", s("SHA-256"))]);
    assert_eq!(call("importKey", &o(vec![("alg", bad), ("format", s("jwk")), ("keyData", jwk)])).unwrap_err().0, "DataError");
}

fn pair(alg: &V) -> (Vec<u8>, Vec<u8>, V) {
    let g = call("generateKey", &o(vec![("alg", alg.clone())])).unwrap();
    (bytes(member(&g, "privateKey")), bytes(member(&g, "publicKey")), g)
}

#[test]
fn rsa_round_trips() {
    let t = std::time::Instant::now();
    let ka = o(vec![
        ("name", s("RSASSA-PKCS1-v1_5")),
        ("modulusLength", V::Num(2048.0)),
        ("publicExponent", b(&[1, 0, 1])),
        ("hash", s("SHA-256")),
    ]);
    let (private, public, g) = pair(&ka);
    eprintln!("RSA-2048 keygen: {:?}", t.elapsed());
    assert_eq!(member(&g, "modulusLength"), V::Num(2048.0));
    assert_eq!(member(&g, "publicExponent"), b(&[1, 0, 1]));
    let sig = bytes(call("sign", &o(vec![("alg", o(vec![("name", s("RSASSA-PKCS1-v1_5"))])), ("keyAlg", ka.clone()), ("key", b(&private)), ("data", b(b"hello"))])).unwrap());
    assert_eq!(sig.len(), 256);
    let vargs = |data: &[u8], key: &[u8]| {
        o(vec![
            ("alg", o(vec![("name", s("RSASSA-PKCS1-v1_5"))])),
            ("keyAlg", ka.clone()),
            ("key", b(key)),
            ("data", b(data)),
            ("signature", b(&sig)),
        ])
    };
    assert_eq!(call("verify", &vargs(b"hello", &public)).unwrap(), V::Bool(true));
    assert_eq!(call("verify", &vargs(b"hellO", &public)).unwrap(), V::Bool(false));

    // JWK export/import of both halves.
    let jwk = call("exportKey", &o(vec![("alg", ka.clone()), ("format", s("jwk")), ("keyType", s("private")), ("key", b(&private))])).unwrap();
    assert_eq!(member(&jwk, "alg"), s("RS256"));
    assert_eq!(member(&jwk, "e"), s("AQAB"));
    for k in ["n", "d", "p", "q", "dp", "dq", "qi"] {
        assert!(matches!(member(&jwk, k), V::Str(_)), "{k}");
    }
    let imp = call("importKey", &o(vec![("alg", ka.clone()), ("format", s("jwk")), ("keyData", jwk)])).unwrap();
    assert_eq!(member(&imp, "type"), s("private"));
    assert_eq!(bytes(member(&imp, "key")), private);
    let pjwk = call("exportKey", &o(vec![("alg", ka.clone()), ("format", s("jwk")), ("keyType", s("public")), ("key", b(&public))])).unwrap();
    let imp = call("importKey", &o(vec![("alg", ka.clone()), ("format", s("jwk")), ("keyData", pjwk)])).unwrap();
    assert_eq!(bytes(member(&imp, "key")), public);
    assert_eq!(call("verify", &vargs(b"hello", &bytes(member(&imp, "key")))).unwrap(), V::Bool(true));
    let spki = call("exportKey", &o(vec![("alg", ka.clone()), ("format", s("spki")), ("keyType", s("public")), ("key", b(&public))])).unwrap();
    assert_eq!(bytes(spki), public);
    assert_eq!(
        call("exportKey", &o(vec![("alg", ka.clone()), ("format", s("spki")), ("keyType", s("private")), ("key", b(&private))])).unwrap_err().0,
        "InvalidAccessError"
    );

    // PSS with the same key material.
    let pss_alg = o(vec![("name", s("RSA-PSS")), ("saltLength", V::Num(32.0))]);
    let pka = o(vec![("name", s("RSA-PSS")), ("hash", s("SHA-256"))]);
    let sig = bytes(call("sign", &o(vec![("alg", pss_alg.clone()), ("keyAlg", pka.clone()), ("key", b(&private)), ("data", b(b"x"))])).unwrap());
    let ok = call("verify", &o(vec![("alg", pss_alg), ("keyAlg", pka), ("key", b(&public)), ("data", b(b"x")), ("signature", b(&sig))])).unwrap();
    assert_eq!(ok, V::Bool(true));

    // OAEP with and without a label.
    let oka = o(vec![("name", s("RSA-OAEP")), ("hash", s("SHA-1"))]);
    for label in [None, Some(&b"lbl"[..])] {
        let mut a = vec![("name", s("RSA-OAEP"))];
        if let Some(l) = label {
            a.push(("label", b(l)));
        }
        let a = o(a);
        let ct = bytes(call("encrypt", &o(vec![("alg", a.clone()), ("keyAlg", oka.clone()), ("key", b(&public)), ("data", b(b"secret"))])).unwrap());
        let pt = bytes(call("decrypt", &o(vec![("alg", a.clone()), ("keyAlg", oka.clone()), ("key", b(&private)), ("data", b(&ct))])).unwrap());
        assert_eq!(pt, b"secret");
        let mut bad = ct.clone();
        bad[10] ^= 1;
        assert_eq!(call("decrypt", &o(vec![("alg", a), ("keyAlg", oka.clone()), ("key", b(&private)), ("data", b(&bad))])).unwrap_err().0, "OperationError");
    }
    let jwk = call("exportKey", &o(vec![("alg", oka.clone()), ("format", s("jwk")), ("keyType", s("public")), ("key", b(&public))])).unwrap();
    assert_eq!(member(&jwk, "alg"), s("RSA-OAEP"));
    // PKCS#8 round trip.
    let p8 = bytes(call("exportKey", &o(vec![("alg", ka.clone()), ("format", s("pkcs8")), ("keyType", s("private")), ("key", b(&private))])).unwrap());
    let imp = call("importKey", &o(vec![("alg", ka), ("format", s("pkcs8")), ("keyData", b(&p8))])).unwrap();
    assert_eq!(member(&imp, "modulusLength"), V::Num(2048.0));
}

#[test]
fn ec_round_trips() {
    for (curve, flen) in [("P-256", 32usize), ("P-384", 48), ("P-521", 66)] {
        let ka = o(vec![("name", s("ECDSA")), ("namedCurve", s(curve))]);
        let (private, public, _) = pair(&ka);
        for hash in ["SHA-1", "SHA-256", "SHA-384", "SHA-512"] {
            let sa = o(vec![("name", s("ECDSA")), ("hash", o(vec![("name", s(hash))]))]);
            let sig = bytes(call("sign", &o(vec![("alg", sa.clone()), ("keyAlg", ka.clone()), ("key", b(&private)), ("data", b(b"msg"))])).unwrap());
            assert_eq!(sig.len(), 2 * flen, "{curve} {hash}");
            let v = |data: &[u8]| {
                call("verify", &o(vec![("alg", sa.clone()), ("keyAlg", ka.clone()), ("key", b(&public)), ("data", b(data)), ("signature", b(&sig))])).unwrap()
            };
            assert_eq!(v(b"msg"), V::Bool(true), "{curve} {hash}");
            assert_eq!(v(b"msh"), V::Bool(false));
        }
        // raw / jwk export and import.
        let raw = bytes(call("exportKey", &o(vec![("alg", ka.clone()), ("format", s("raw")), ("keyType", s("public")), ("key", b(&public))])).unwrap());
        assert_eq!(raw.len(), 1 + 2 * flen);
        assert_eq!(raw[0], 4);
        let imp = call("importKey", &o(vec![("alg", ka.clone()), ("format", s("raw")), ("keyData", b(&raw))])).unwrap();
        assert_eq!(bytes(member(&imp, "key")), public);
        let jwk = call("exportKey", &o(vec![("alg", ka.clone()), ("format", s("jwk")), ("keyType", s("private")), ("key", b(&private))])).unwrap();
        assert_eq!(member(&jwk, "crv"), s(curve));
        let imp = call("importKey", &o(vec![("alg", ka.clone()), ("format", s("jwk")), ("keyData", jwk)])).unwrap();
        assert_eq!(member(&imp, "type"), s("private"));
        assert_eq!(bytes(member(&imp, "key")), private);
        // Wrong curve.
        let other = o(vec![("name", s("ECDSA")), ("namedCurve", s(if curve == "P-256" { "P-384" } else { "P-256" }))]);
        assert_eq!(call("importKey", &o(vec![("alg", other), ("format", s("spki")), ("keyData", b(&public))])).unwrap_err().0, "DataError");

        // ECDH agreement.
        let da = o(vec![("name", s("ECDH")), ("namedCurve", s(curve))]);
        let (a_priv, a_pub, _) = pair(&da);
        let (b_priv, b_pub, _) = pair(&da);
        let derive = |k: &[u8], peer: &[u8], len: V| {
            call("deriveBits", &o(vec![("alg", o(vec![("name", s("ECDH")), ("public", b(peer))])), ("keyAlg", da.clone()), ("key", b(k)), ("length", len)]))
        };
        let s1 = bytes(derive(&a_priv, &b_pub, V::Null).unwrap());
        let s2 = bytes(derive(&b_priv, &a_pub, V::Null).unwrap());
        assert_eq!(s1, s2);
        assert_eq!(s1.len(), flen);
        let short = bytes(derive(&a_priv, &b_pub, V::Num(12.0)).unwrap());
        assert_eq!(short, vec![s1[0], s1[1] & 0xf0]);
        assert_eq!(derive(&a_priv, &b_pub, V::Num((flen * 8 + 8) as f64)).unwrap_err().0, "OperationError");
    }
}

#[test]
fn ecdsa_known_signature() {
    // RFC 6979 A.2.5 P-256 / SHA-256 / "sample" (deterministic signature).
    let d = hx("C9AFA9D845BA75166B5C215767B1D6934E50C3DB36E89B127B8A622B120F6721");
    let x = hx("60FED4BA255A9D31C961EB74C6356D68C049B8923B61FA6CE669622E60F29FB6");
    let y = hx("7903FE1008B8BC99A41AE9E95628BC64F2F1B20C2D7E9F5177A3C294D4462299");
    let ka = o(vec![("name", s("ECDSA")), ("namedCurve", s("P-256"))]);
    let jwk = o(vec![("kty", s("EC")), ("crv", s("P-256")), ("x", b64e(&x)), ("y", b64e(&y)), ("d", b64e(&d))]);
    let imp = call("importKey", &o(vec![("alg", ka.clone()), ("format", s("jwk")), ("keyData", jwk)])).unwrap();
    let sa = o(vec![("name", s("ECDSA")), ("hash", s("SHA-256"))]);
    let sig = bytes(call("sign", &o(vec![("alg", sa), ("keyAlg", ka), ("key", member(&imp, "key")), ("data", b(b"sample"))])).unwrap());
    assert_eq!(
        sig,
        hx("EFD48B2AACB6A8FD1140DD9CD45E81D69D2C877B56AAF991C34D0EA84EAF3716 F7CB1C942D657C41D436C7A1B6E29F65F3E900DBB9AFF4064DC4AB2F843ACDA8")
    );
}
