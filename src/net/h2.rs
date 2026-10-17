// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HTTP/2 (RFC 9113) for HeronNet: one TLS connection per origin carries
//! many concurrent requests as streams.
//!
//! The connection is negotiated with ALPN ("h2"). A reader thread owns the
//! socket's read side: it feeds TLS records into the shared rustls session,
//! parses frames and completes streams. Requests run on the network worker
//! threads: each encodes its headers (HPACK, literal fields), sends them and
//! waits for its stream to finish. Flow control: large receive windows
//! (16 MiB) replenished as data is consumed; request bodies respect the
//! peer's windows. Header compression on the receive side uses loona-hpack.

use super::{NetError, Request, Response};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
/// Receive windows as Chromium sets them (stream 6 MiB, connection 15 MiB).
const RECV_WINDOW: u32 = 6 * 1024 * 1024;
const CONN_WINDOW: u32 = 15 * 1024 * 1024;
const MAX_HEADER_LIST: usize = 256 * 1024;

const DATA: u8 = 0x0;
const HEADERS: u8 = 0x1;
const PRIORITY: u8 = 0x2;
const RST_STREAM: u8 = 0x3;
const SETTINGS: u8 = 0x4;
const PUSH_PROMISE: u8 = 0x5;
const PING: u8 = 0x6;
const GOAWAY: u8 = 0x7;
const WINDOW_UPDATE: u8 = 0x8;
const CONTINUATION: u8 = 0x9;

const END_STREAM: u8 = 0x1;
const ACK: u8 = 0x1;
const END_HEADERS: u8 = 0x4;
const PADDED: u8 = 0x8;
const PRIORITY_FLAG: u8 = 0x20;

/// Error codes we send.
const NO_ERROR: u32 = 0x0;
const PROTOCOL_ERROR: u32 = 0x1;
const REFUSED_STREAM: u32 = 0x7;
const CANCEL: u32 = 0x8;

#[derive(Default)]
struct StreamState {
    status: u16,
    headers: Vec<(String, String)>,
    got_headers: bool,
    body: Vec<u8>,
    done: bool,
    error: Option<(NetError, bool)>, // (error, retryable)
    send_window: i64,
    unacked: u32,
}

struct State {
    streams: HashMap<u32, StreamState>,
    next_id: u32,
    /// Peer settings.
    max_frame: usize,
    max_concurrent: usize,
    initial_send_window: i64,
    conn_send_window: i64,
    conn_unacked: u32,
    /// GOAWAY received (last stream id) or I/O failure: no new streams.
    goaway: Option<u32>,
    dead: bool,
    last_used: Instant,
    /// HEADERS/CONTINUATION reassembly: (stream, flags of HEADERS, block).
    pending_headers: Option<(u32, u8, Vec<u8>)>,
    decoder: loona_hpack::Decoder<'static>,
}

pub struct H2Conn {
    tls: Mutex<rustls::ClientConnection>,
    sock: Mutex<TcpStream>,
    /// Held from choosing a stream id until its HEADERS are written: new
    /// streams must open in increasing id order (RFC 9113 §5.1.1).
    open_lock: Mutex<()>,
    state: Mutex<State>,
    cv: Condvar,
    pub authority: String,
}

impl H2Conn {
    /// Start HTTP/2 on a TLS session whose handshake chose "h2".
    pub fn start(mut tls: rustls::ClientConnection, sock: TcpStream, authority: String) -> Result<Arc<H2Conn>, NetError> {
        // We drain TLS output right after every write, so never let rustls
        // refuse plaintext (a partial write would corrupt the frame stream).
        tls.set_buffer_limit(None);
        let reader = sock.try_clone()?;
        let _ = reader.set_read_timeout(Some(Duration::from_secs(5)));
        let conn = Arc::new(H2Conn {
            tls: Mutex::new(tls),
            sock: Mutex::new(sock),
            open_lock: Mutex::new(()),
            state: Mutex::new(State {
                streams: HashMap::new(),
                next_id: 1,
                max_frame: 16384,
                max_concurrent: 100,
                initial_send_window: 65535,
                conn_send_window: 65535,
                conn_unacked: 0,
                goaway: None,
                dead: false,
                last_used: Instant::now(),
                pending_headers: None,
                decoder: loona_hpack::Decoder::new(),
            }),
            cv: Condvar::new(),
            authority,
        });
        // Preface, our settings (no push, big windows), connection window.
        let mut out = PREFACE.to_vec();
        let mut settings = Vec::new();
        // Chromium's SETTINGS (header table, no push, window, header list).
        for (id, v) in [(0x1u16, 65536u32), (0x2, 0), (0x4, RECV_WINDOW), (0x6, MAX_HEADER_LIST as u32)] {
            settings.extend_from_slice(&id.to_be_bytes());
            settings.extend_from_slice(&v.to_be_bytes());
        }
        frame(&mut out, SETTINGS, 0, 0, &settings);
        frame(&mut out, WINDOW_UPDATE, 0, 0, &(CONN_WINDOW - 65535).to_be_bytes());
        conn.send(&out)?;
        if std::env::var_os("OHB_DEBUG_NET").is_some() {
            eprintln!("[net] h2 connection to {}", conn.authority);
        }
        let c2 = conn.clone();
        std::thread::Builder::new()
            .name("heron-h2".into())
            .stack_size(512 * 1024)
            .spawn(move || c2.read_loop(reader))
            .map_err(|e| NetError::Io(e.to_string()))?;
        Ok(conn)
    }

    /// Can this connection take another request?
    pub fn usable(&self) -> bool {
        let st = self.state.lock().unwrap();
        !st.dead && st.goaway.is_none() && st.next_id < 0x7fff_0000 && st.last_used.elapsed() < Duration::from_secs(240)
    }

    fn send(&self, bytes: &[u8]) -> Result<(), NetError> {
        let r = (|| -> Result<(), NetError> {
            let mut tls = self.tls.lock().unwrap();
            tls.writer().write_all(bytes)?;
            let mut sock = self.sock.lock().unwrap();
            while tls.wants_write() {
                tls.write_tls(&mut *sock)?;
            }
            Ok(())
        })();
        if let Err(e) = &r {
            // A failed write leaves the frame stream unusable.
            self.fail_all(e.clone(), true);
        }
        r
    }

    fn fail_all(&self, err: NetError, retryable: bool) {
        let mut st = self.state.lock().unwrap();
        st.dead = true;
        for s in st.streams.values_mut() {
            if !s.done && s.error.is_none() {
                s.error = Some((err.clone(), retryable && !s.got_headers));
            }
        }
        self.cv.notify_all();
    }

    fn read_loop(self: Arc<Self>, mut sock: TcpStream) {
        let mut buf = vec![0u8; 64 * 1024];
        let mut plain: Vec<u8> = Vec::new();
        loop {
            if self.state.lock().unwrap().dead {
                return;
            }
            let n = match sock.read(&mut buf) {
                Ok(0) => {
                    self.fail_all(NetError::Io("connection closed".into()), true);
                    return;
                }
                Ok(n) => n,
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                    // Idle: close connections nobody has used for a while.
                    let idle = {
                        let st = self.state.lock().unwrap();
                        st.streams.is_empty() && st.last_used.elapsed() > Duration::from_secs(240)
                    };
                    if idle {
                        let mut out = Vec::new();
                        let mut p = 0u32.to_be_bytes().to_vec();
                        p.extend_from_slice(&NO_ERROR.to_be_bytes());
                        frame(&mut out, GOAWAY, 0, 0, &p);
                        let _ = self.send(&out);
                        self.fail_all(NetError::Io("idle".into()), true);
                        return;
                    }
                    // Response bodies are allocated on this thread and freed
                    // on the page's: the allocator only gets those pages back
                    // when the owning thread collects them.
                    crate::collect_service_thread("h2");
                    continue;
                }
                Err(e) => {
                    self.fail_all(NetError::Io(e.to_string()), true);
                    return;
                }
            };
            {
                let mut tls = self.tls.lock().unwrap();
                let mut rd = &buf[..n];
                let mut tmp = [0u8; 16384];
                // Feed records and drain plaintext in turn (rustls bounds its
                // plaintext buffer).
                while !rd.is_empty() {
                    if let Err(e) = tls.read_tls(&mut rd) {
                        drop(tls);
                        self.fail_all(NetError::Tls(e.to_string()), false);
                        return;
                    }
                    if let Err(e) = tls.process_new_packets() {
                        drop(tls);
                        self.fail_all(NetError::Tls(e.to_string()), false);
                        return;
                    }
                    loop {
                        match tls.reader().read(&mut tmp) {
                            Ok(0) => break,
                            Ok(k) => plain.extend_from_slice(&tmp[..k]),
                            Err(_) => break,
                        }
                    }
                }
                if tls.wants_write() {
                    let mut s = self.sock.lock().unwrap();
                    while tls.wants_write() {
                        if tls.write_tls(&mut *s).is_err() {
                            break;
                        }
                    }
                }
            }
            // Parse complete frames.
            let mut off = 0;
            while plain.len() - off >= 9 {
                let len = ((plain[off] as usize) << 16) | ((plain[off + 1] as usize) << 8) | plain[off + 2] as usize;
                if plain.len() - off < 9 + len {
                    break;
                }
                let ty = plain[off + 3];
                let flags = plain[off + 4];
                let sid = u32::from_be_bytes([plain[off + 5], plain[off + 6], plain[off + 7], plain[off + 8]]) & 0x7fff_ffff;
                let payload = plain[off + 9..off + 9 + len].to_vec();
                off += 9 + len;
                if let Err(e) = self.on_frame(ty, flags, sid, payload) {
                    let mut out = Vec::new();
                    let mut p = 0u32.to_be_bytes().to_vec();
                    p.extend_from_slice(&PROTOCOL_ERROR.to_be_bytes());
                    frame(&mut out, GOAWAY, 0, 0, &p);
                    let _ = self.send(&out);
                    self.fail_all(e, false);
                    return;
                }
            }
            plain.drain(..off);
        }
    }

    fn on_frame(&self, ty: u8, flags: u8, sid: u32, mut p: Vec<u8>) -> Result<(), NetError> {
        let proto = |m: &str| NetError::Protocol(format!("h2: {m}"));
        // A header block must be followed by its CONTINUATIONs only.
        {
            let st = self.state.lock().unwrap();
            if st.pending_headers.is_some() && ty != CONTINUATION {
                return Err(proto("expected CONTINUATION"));
            }
        }
        match ty {
            DATA => {
                let data = strip_padding(&mut p, flags)?;
                let n = (p.len() as u32).max(data.len() as u32);
                let mut out = Vec::new();
                {
                    let mut st = self.state.lock().unwrap();
                    st.conn_unacked += n;
                    if st.conn_unacked >= CONN_WINDOW / 2 {
                        frame(&mut out, WINDOW_UPDATE, 0, 0, &st.conn_unacked.to_be_bytes());
                        st.conn_unacked = 0;
                    }
                    if let Some(s) = st.streams.get_mut(&sid) {
                        if s.body.len() + data.len() > 64 * 1024 * 1024 {
                            s.error = Some((NetError::TooLarge, false));
                        } else {
                            s.body.extend_from_slice(&data);
                        }
                        s.unacked += n;
                        if flags & END_STREAM != 0 {
                            s.done = true;
                        } else if s.unacked >= RECV_WINDOW / 2 {
                            frame(&mut out, WINDOW_UPDATE, 0, sid, &s.unacked.to_be_bytes());
                            s.unacked = 0;
                        }
                    }
                    self.cv.notify_all();
                }
                if !out.is_empty() {
                    self.send(&out)?;
                }
            }
            HEADERS => {
                let mut block = strip_padding(&mut p, flags)?;
                if flags & PRIORITY_FLAG != 0 {
                    if block.len() < 5 {
                        return Err(proto("short HEADERS"));
                    }
                    block.drain(..5);
                }
                if flags & END_HEADERS != 0 {
                    self.on_header_block(sid, flags, &block)?;
                } else {
                    self.state.lock().unwrap().pending_headers = Some((sid, flags, block));
                }
            }
            CONTINUATION => {
                let done = {
                    let mut st = self.state.lock().unwrap();
                    let Some((psid, _, ref mut block)) = st.pending_headers else { return Err(proto("unexpected CONTINUATION")) };
                    if psid != sid {
                        return Err(proto("CONTINUATION on another stream"));
                    }
                    block.extend_from_slice(&p);
                    if block.len() > MAX_HEADER_LIST {
                        return Err(proto("header block too large"));
                    }
                    if flags & END_HEADERS != 0 { st.pending_headers.take() } else { None }
                };
                if let Some((s, hflags, block)) = done {
                    self.on_header_block(s, hflags, &block)?;
                }
            }
            PRIORITY => {}
            RST_STREAM => {
                let code = if p.len() >= 4 { u32::from_be_bytes([p[0], p[1], p[2], p[3]]) } else { 0 };
                let mut st = self.state.lock().unwrap();
                if let Some(s) = st.streams.get_mut(&sid) {
                    if !s.done {
                        let retry = code == REFUSED_STREAM && !s.got_headers;
                        s.error = Some((NetError::Protocol(format!("h2 stream reset ({code})")), retry));
                    }
                }
                self.cv.notify_all();
            }
            SETTINGS => {
                if flags & ACK != 0 {
                    return Ok(());
                }
                {
                    let mut st = self.state.lock().unwrap();
                    for c in p.chunks_exact(6) {
                        let id = u16::from_be_bytes([c[0], c[1]]);
                        let v = u32::from_be_bytes([c[2], c[3], c[4], c[5]]);
                        match id {
                            0x3 => st.max_concurrent = (v as usize).clamp(1, 1000),
                            0x4 => {
                                let delta = v as i64 - st.initial_send_window;
                                st.initial_send_window = v as i64;
                                for s in st.streams.values_mut() {
                                    s.send_window += delta;
                                }
                            }
                            0x5 => st.max_frame = (v as usize).clamp(16384, 1 << 24),
                            _ => {}
                        }
                    }
                    self.cv.notify_all();
                }
                let mut out = Vec::new();
                frame(&mut out, SETTINGS, ACK, 0, &[]);
                self.send(&out)?;
            }
            PUSH_PROMISE => {
                // Push is disabled in our SETTINGS; refuse anything promised.
                if p.len() >= 4 {
                    let promised = u32::from_be_bytes([p[0], p[1], p[2], p[3]]) & 0x7fff_ffff;
                    let mut out = Vec::new();
                    frame(&mut out, RST_STREAM, 0, promised, &CANCEL.to_be_bytes());
                    self.send(&out)?;
                }
            }
            PING => {
                if flags & ACK == 0 {
                    let mut out = Vec::new();
                    frame(&mut out, PING, ACK, 0, &p);
                    self.send(&out)?;
                }
            }
            GOAWAY => {
                let last = if p.len() >= 4 { u32::from_be_bytes([p[0], p[1], p[2], p[3]]) & 0x7fff_ffff } else { 0 };
                if std::env::var_os("OHB_DEBUG_NET").is_some() {
                    let code = if p.len() >= 8 { u32::from_be_bytes([p[4], p[5], p[6], p[7]]) } else { 0 };
                    let open = self.state.lock().unwrap().streams.len();
                    eprintln!("[net] h2 GOAWAY from {} last={last} code={code} open={open} debug={:?}", self.authority, String::from_utf8_lossy(p.get(8..).unwrap_or(&[])));
                }
                let mut st = self.state.lock().unwrap();
                st.goaway = Some(last);
                for (&id, s) in st.streams.iter_mut() {
                    if id > last && !s.done {
                        s.error = Some((NetError::Io("h2 GOAWAY".into()), true));
                    }
                }
                self.cv.notify_all();
            }
            WINDOW_UPDATE => {
                if p.len() >= 4 {
                    let inc = (u32::from_be_bytes([p[0], p[1], p[2], p[3]]) & 0x7fff_ffff) as i64;
                    let mut st = self.state.lock().unwrap();
                    if sid == 0 {
                        st.conn_send_window += inc;
                    } else if let Some(s) = st.streams.get_mut(&sid) {
                        s.send_window += inc;
                    }
                    self.cv.notify_all();
                }
            }
            _ => {} // unknown frame types are ignored
        }
        Ok(())
    }

    fn on_header_block(&self, sid: u32, flags: u8, block: &[u8]) -> Result<(), NetError> {
        let mut st = self.state.lock().unwrap();
        // The decoder's dynamic table must see every block, even for streams
        // we've forgotten.
        let list = st.decoder.decode(block).map_err(|e| NetError::Protocol(format!("h2 HPACK: {e:?}")))?;
        if let Some(s) = st.streams.get_mut(&sid) {
            let mut status = 0u16;
            let mut hs = Vec::new();
            for (k, v) in list {
                let k = String::from_utf8_lossy(&k).into_owned();
                let v = String::from_utf8_lossy(&v).into_owned();
                if k == ":status" {
                    status = v.parse().unwrap_or(0);
                } else if !k.starts_with(':') {
                    hs.push((k, v));
                }
            }
            if !s.got_headers {
                // Informational (1xx) responses come before the final one.
                if (100..200).contains(&status) {
                    return Ok(());
                }
                s.status = status;
                s.headers = hs;
                s.got_headers = true;
            }
            // (A later block is trailers: ignored.)
            if flags & END_STREAM != 0 {
                s.done = true;
            }
            self.cv.notify_all();
        }
        Ok(())
    }

    /// Run one request on this connection. `Err((e, true))` means it never
    /// reached the server and may be retried elsewhere.
    pub fn request(&self, req: &Request, headers: &[(String, String)]) -> Result<Response, (NetError, bool)> {
        let url = &req.url;
        let mut path = url.path().to_string();
        if let Some(q) = url.query() {
            path.push('?');
            path.push_str(q);
        }
        let body = req.body.as_ref().map(|b| b.as_slice()).unwrap_or(&[]);
        // Header block: pseudo-headers first, names lowercase.
        let mut block = Vec::new();
        // Pseudo-headers in Chromium's order (method, authority, scheme, path).
        hpack_literal(&mut block, b":method", req.method.as_bytes());
        hpack_literal(&mut block, b":authority", self.authority.as_bytes());
        hpack_literal(&mut block, b":scheme", url.scheme().as_bytes());
        hpack_literal(&mut block, b":path", path.as_bytes());
        for (k, v) in headers {
            let lk = k.to_ascii_lowercase();
            if matches!(lk.as_str(), "host" | "connection" | "keep-alive" | "proxy-connection" | "transfer-encoding" | "upgrade" | "te") {
                continue;
            }
            if std::env::var_os("OHB_DEBUG_H2").is_some() {
                eprintln!("[h2] {} {}: {}", path, lk, v);
            }
            hpack_literal(&mut block, lk.as_bytes(), v.as_bytes());
        }
        // Open the stream.
        let open_guard = self.open_lock.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let (sid, max_frame) = {
            let mut st = self.state.lock().unwrap();
            loop {
                if st.dead || st.goaway.is_some() {
                    return Err((NetError::Io("h2 connection closed".into()), true));
                }
                let open = st.streams.values().filter(|s| !s.done && s.error.is_none()).count();
                if open < st.max_concurrent {
                    break;
                }
                let (g, t) = self.cv.wait_timeout(st, Duration::from_millis(500)).unwrap();
                st = g;
                if t.timed_out() && Instant::now() > deadline {
                    return Err((NetError::Timeout, true));
                }
            }
            let sid = st.next_id;
            st.next_id += 2;
            st.last_used = Instant::now();
            let send_window = st.initial_send_window;
            st.streams.insert(sid, StreamState { send_window, ..Default::default() });
            (sid, st.max_frame)
        };
        let end_stream = body.is_empty();
        let mut out = Vec::new();
        // Stream priority in the HEADERS frame, as Chromium sends it:
        // exclusive on the root, weight by resource type.
        let weight: u8 = match req.kind {
            super::RequestKind::Document | super::RequestKind::Stylesheet => 255,
            super::RequestKind::Script | super::RequestKind::Font | super::RequestKind::Fetch => 219,
            super::RequestKind::Image => 182,
            super::RequestKind::Favicon => 146,
        };
        let mut first_payload = vec![0x80, 0, 0, 0, weight];
        let room = max_frame - 5;
        let (head, tail) = block.split_at(block.len().min(room));
        first_payload.extend_from_slice(head);
        let rest: Vec<&[u8]> = tail.chunks(max_frame).collect();
        let hflags = if end_stream { END_STREAM } else { 0 } | if rest.is_empty() { END_HEADERS } else { 0 } | PRIORITY_FLAG;
        frame(&mut out, HEADERS, hflags, sid, &first_payload);
        for (i, c) in rest.iter().enumerate() {
            frame(&mut out, CONTINUATION, if i + 1 == rest.len() { END_HEADERS } else { 0 }, sid, c);
        }
        let sent_headers = self.send(&out);
        drop(open_guard);
        if let Err(e) = sent_headers {
            self.forget(sid);
            return Err((e, true));
        }
        // Request body under flow control.
        let mut sent = 0;
        while sent < body.len() {
            let n = {
                let mut st = self.state.lock().unwrap();
                loop {
                    if st.dead {
                        drop(st);
                        self.forget(sid);
                        return Err((NetError::Io("h2 connection closed".into()), true));
                    }
                    let sw = st.streams.get(&sid).map(|s| s.send_window).unwrap_or(0);
                    let avail = sw.min(st.conn_send_window).min(max_frame as i64);
                    if avail > 0 {
                        let n = (avail as usize).min(body.len() - sent);
                        st.conn_send_window -= n as i64;
                        if let Some(s) = st.streams.get_mut(&sid) {
                            s.send_window -= n as i64;
                        }
                        break n;
                    }
                    let (g, t) = self.cv.wait_timeout(st, Duration::from_millis(500)).unwrap();
                    st = g;
                    if t.timed_out() && Instant::now() > deadline {
                        drop(st);
                        self.reset(sid);
                        return Err((NetError::Timeout, false));
                    }
                }
            };
            let mut out = Vec::new();
            let last = sent + n == body.len();
            frame(&mut out, DATA, if last { END_STREAM } else { 0 }, sid, &body[sent..sent + n]);
            if let Err(e) = self.send(&out) {
                // The request never completed (no END_STREAM), so the server
                // can't have acted on it: safe to resend elsewhere.
                self.forget(sid);
                return Err((e, true));
            }
            sent += n;
        }
        // Wait for the response.
        let mut st = self.state.lock().unwrap();
        let idle_limit = Duration::from_secs(30);
        let mut last_progress = (Instant::now(), 0usize);
        loop {
            let s = st.streams.get(&sid).unwrap();
            if let Some((e, retry)) = s.error.clone() {
                st.streams.remove(&sid);
                return Err((e, retry));
            }
            if s.done {
                let s = st.streams.remove(&sid).unwrap();
                st.last_used = Instant::now();
                drop(st);
                let enc = super::header_value(&s.headers, "content-encoding").unwrap_or("").to_ascii_lowercase();
                let body = super::decode_body(&enc, s.body).map_err(|e| (e, false))?;
                return Ok(Response { url: req.url.clone(), status: s.status, status_text: String::new(), headers: s.headers, body: Arc::new(body), redirected: false, from_cache: false });
            }
            let got = s.body.len() + s.got_headers as usize;
            if got != last_progress.1 {
                last_progress = (Instant::now(), got);
            } else if last_progress.0.elapsed() > idle_limit {
                drop(st);
                self.reset(sid);
                return Err((NetError::Timeout, false));
            }
            st = self.cv.wait_timeout(st, Duration::from_millis(500)).unwrap().0;
        }
    }

    fn forget(&self, sid: u32) {
        self.state.lock().unwrap().streams.remove(&sid);
        self.cv.notify_all();
    }

    fn reset(&self, sid: u32) {
        self.forget(sid);
        let mut out = Vec::new();
        frame(&mut out, RST_STREAM, 0, sid, &CANCEL.to_be_bytes());
        let _ = self.send(&out);
    }
}

fn frame(out: &mut Vec<u8>, ty: u8, flags: u8, sid: u32, payload: &[u8]) {
    let len = payload.len() as u32;
    out.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8, ty, flags]);
    out.extend_from_slice(&(sid & 0x7fff_ffff).to_be_bytes());
    out.extend_from_slice(payload);
}

/// Remove DATA/HEADERS padding; returns the content.
fn strip_padding(p: &mut Vec<u8>, flags: u8) -> Result<Vec<u8>, NetError> {
    if flags & PADDED == 0 {
        return Ok(std::mem::take(p));
    }
    let pad = *p.first().ok_or_else(|| NetError::Protocol("h2: bad padding".into()))? as usize;
    if pad + 1 > p.len() {
        return Err(NetError::Protocol("h2: bad padding".into()));
    }
    let n = p.len();
    Ok(p[1..n - pad].to_vec())
}

/// HPACK integer with an N-bit prefix (RFC 7541 §5.1).
fn hpack_int(out: &mut Vec<u8>, first: u8, prefix: u8, mut v: usize) {
    let max = (1usize << prefix) - 1;
    if v < max {
        out.push(first | v as u8);
        return;
    }
    out.push(first | max as u8);
    v -= max;
    while v >= 128 {
        out.push((v % 128) as u8 | 0x80);
        v /= 128;
    }
    out.push(v as u8);
}

/// "Literal Header Field without Indexing — New Name" (no Huffman): keeps
/// no encoder state, so streams can be encoded in any order.
fn hpack_literal(out: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    out.push(0x00);
    hpack_int(out, 0, 7, name.len());
    out.extend_from_slice(name);
    hpack_int(out, 0, 7, value.len());
    out.extend_from_slice(value);
}
