// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! WebSocket client (RFC 6455) for script's `WebSocket`: one thread per
//! connection does the opening handshake over HTTP/1.1 (through the proxy
//! when one is configured, TLS for wss:), then reads frames (with a short
//! read timeout) and writes the frames script queues.

use std::io::{self, Read, Write};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use url::Url;

use super::{base64_encode, client, Stream};

/// What a connection reports to its page.
#[derive(Debug)]
pub enum WsEvent {
    Open { protocol: String, extensions: String },
    Text(String),
    Binary(Vec<u8>),
    /// The connection is closed (code 1006 = abnormal: no close frame).
    Closed { code: u16, reason: String, clean: bool },
    /// The connection failed (before or after opening); `Closed` follows.
    Error,
}

/// What script asks of a connection.
pub enum WsCmd {
    Text(String),
    Binary(Vec<u8>),
    Close(Option<u16>, String),
}

const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Open a WebSocket to `url` (ws: or wss:); events go to `on_event` from the
/// connection's thread. Dropping every sender of `cmds` closes it.
pub fn connect(url: Url, protocols: Vec<String>, origin: String, cmds: Receiver<WsCmd>, on_event: impl Fn(WsEvent) + Send + 'static) {
    std::thread::Builder::new()
        .name("websocket".into())
        .spawn(move || {
            let mut stream = match handshake(&url, &protocols, &origin) {
                Ok((s, protocol, extensions)) => {
                    on_event(WsEvent::Open { protocol, extensions });
                    s
                }
                Err(_) => {
                    on_event(WsEvent::Error);
                    on_event(WsEvent::Closed { code: 1006, reason: String::new(), clean: false });
                    return;
                }
            };
            run(&mut stream, cmds, &on_event);
        })
        .ok();
}

fn tcp_of(s: &Stream) -> &std::net::TcpStream {
    match s {
        Stream::Plain(t) => t,
        Stream::Tls(t) => &t.sock,
    }
}

fn random_key() -> [u8; 16] {
    use ring::rand::SecureRandom;
    let mut k = [0u8; 16];
    let _ = ring::rand::SystemRandom::new().fill(&mut k);
    k
}

fn handshake(url: &Url, protocols: &[String], origin: &str) -> Result<(Stream, String, String), String> {
    let secure = url.scheme() == "wss";
    let host = url.host_str().ok_or("no host")?.to_string();
    let port = url.port_or_known_default().unwrap_or(if secure { 443 } else { 80 });
    let conn = client().open(if secure { "https" } else { "http" }, &host, port).map_err(|e| format!("{e:?}"))?;
    let mut stream = conn.reader.into_inner();
    let key = base64_encode(&random_key());
    let mut path = url.path().to_string();
    if let Some(q) = url.query() {
        path.push('?');
        path.push_str(q);
    }
    let host_header = if url.port().is_some() { format!("{host}:{port}") } else { host.clone() };
    let mut req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nOrigin: {origin}\r\nUser-Agent: {}\r\nPragma: no-cache\r\nCache-Control: no-cache\r\n",
        client().ua
    );
    if !protocols.is_empty() {
        req.push_str(&format!("Sec-WebSocket-Protocol: {}\r\n", protocols.join(", ")));
    }
    // Cookies of the equivalent http(s) URL.
    let mut http_url = url.clone();
    let _ = http_url.set_scheme(if secure { "https" } else { "http" });
    let initiator = Url::parse(origin).ok();
    if let Some(c) = client().cookies.lock().unwrap().header_for(&http_url, initiator.as_ref(), false, true) {
        req.push_str(&format!("Cookie: {c}\r\n"));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;
    // The response head, byte by byte (frames may follow at once).
    let mut head = Vec::new();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut b).map_err(|e| e.to_string())? == 0 || head.len() > 64 * 1024 {
            return Err("closed during handshake".into());
        }
        head.push(b[0]);
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let mut lines = text.split("\r\n");
    let status = lines.next().unwrap_or("");
    if status.split_whitespace().nth(1) != Some("101") {
        return Err(format!("bad status: {status}"));
    }
    let mut accept = None;
    let mut protocol = String::new();
    let mut extensions = String::new();
    let (mut upgrade_ok, mut connection_ok) = (false, false);
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        match k.as_str() {
            "sec-websocket-accept" => accept = Some(v.to_string()),
            "sec-websocket-protocol" => protocol = v.to_string(),
            "sec-websocket-extensions" => extensions = v.to_string(),
            "upgrade" => upgrade_ok = v.eq_ignore_ascii_case("websocket"),
            "connection" => connection_ok = v.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")),
            "set-cookie" => client().cookies.lock().unwrap().set_from_header(v, &http_url, initiator.as_ref(), false),
            _ => {}
        }
    }
    let want = base64_encode(ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, format!("{key}{GUID}").as_bytes()).as_ref());
    if !upgrade_ok || !connection_ok || accept.as_deref() != Some(want.as_str()) {
        return Err("bad handshake".into());
    }
    // A subprotocol must be one we asked for; extensions were not offered.
    if (!protocol.is_empty() && !protocols.iter().any(|p| p == &protocol)) || !extensions.is_empty() {
        return Err("bad negotiation".into());
    }
    Ok((stream, protocol, extensions))
}

fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    out.push(0x80 | opcode);
    let len = payload.len();
    if len < 126 {
        out.push(0x80 | len as u8);
    } else if len < 65536 {
        out.push(0x80 | 126);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        out.push(0x80 | 127);
        out.extend_from_slice(&(len as u64).to_be_bytes());
    }
    let mask = {
        let k = random_key();
        [k[0], k[1], k[2], k[3]]
    };
    out.extend_from_slice(&mask);
    out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i & 3]));
    out
}

fn close_payload(code: Option<u16>, reason: &str) -> Vec<u8> {
    match code {
        None => Vec::new(),
        Some(c) => {
            let mut p = c.to_be_bytes().to_vec();
            p.extend_from_slice(reason.as_bytes());
            p
        }
    }
}

fn run(stream: &mut Stream, cmds: Receiver<WsCmd>, on_event: &dyn Fn(WsEvent)) {
    let _ = tcp_of(stream).set_read_timeout(Some(Duration::from_millis(15)));
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = vec![0u8; 64 * 1024];
    // A fragmented message: (opcode, data so far).
    let mut partial: Option<(u8, Vec<u8>)> = None;
    let mut close_sent = false;
    let fail = |on_event: &dyn Fn(WsEvent)| {
        on_event(WsEvent::Error);
        on_event(WsEvent::Closed { code: 1006, reason: String::new(), clean: false });
    };
    loop {
        // Script's frames first.
        loop {
            match cmds.try_recv() {
                Ok(cmd) => {
                    if close_sent {
                        continue;
                    }
                    let f = match cmd {
                        WsCmd::Text(t) => frame(0x1, t.as_bytes()),
                        WsCmd::Binary(b) => frame(0x2, &b),
                        WsCmd::Close(code, reason) => {
                            close_sent = true;
                            frame(0x8, &close_payload(code, &reason))
                        }
                    };
                    if stream.write_all(&f).and_then(|_| stream.flush()).is_err() {
                        fail(on_event);
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The page went away: close (best effort) and stop.
                    if !close_sent {
                        let _ = stream.write_all(&frame(0x8, &close_payload(Some(1001), "")));
                        let _ = stream.flush();
                    }
                    return;
                }
            }
        }
        match stream.read(&mut tmp) {
            Ok(0) => {
                fail(on_event);
                return;
            }
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {}
            Err(_) => {
                fail(on_event);
                return;
            }
        }
        // Whole frames in the buffer.
        loop {
            if buf.len() < 2 {
                break;
            }
            let (b0, b1) = (buf[0], buf[1]);
            let fin = b0 & 0x80 != 0;
            let opcode = b0 & 0x0f;
            if b0 & 0x70 != 0 || b1 & 0x80 != 0 {
                // Reserved bits (no extensions) or a masked server frame.
                fail(on_event);
                return;
            }
            let mut len = (b1 & 0x7f) as usize;
            let mut at = 2;
            if len == 126 {
                if buf.len() < 4 {
                    break;
                }
                len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
                at = 4;
            } else if len == 127 {
                if buf.len() < 10 {
                    break;
                }
                let l = u64::from_be_bytes(buf[2..10].try_into().unwrap());
                if l > (1 << 31) {
                    fail(on_event);
                    return;
                }
                len = l as usize;
                at = 10;
            }
            if buf.len() < at + len {
                break;
            }
            let payload: Vec<u8> = buf[at..at + len].to_vec();
            buf.drain(..at + len);
            match opcode {
                0x0 => {
                    let Some((op, mut data)) = partial.take() else {
                        fail(on_event);
                        return;
                    };
                    data.extend_from_slice(&payload);
                    if fin {
                        if !deliver(op, data, on_event) {
                            fail(on_event);
                            return;
                        }
                    } else {
                        partial = Some((op, data));
                    }
                }
                0x1 | 0x2 => {
                    if partial.is_some() {
                        fail(on_event);
                        return;
                    }
                    if fin {
                        if !deliver(opcode, payload, on_event) {
                            fail(on_event);
                            return;
                        }
                    } else {
                        partial = Some((opcode, payload));
                    }
                }
                0x8 => {
                    let (code, reason) = if payload.len() >= 2 {
                        (u16::from_be_bytes([payload[0], payload[1]]), String::from_utf8_lossy(&payload[2..]).to_string())
                    } else {
                        (1005, String::new())
                    };
                    if !close_sent {
                        let echo = if payload.len() >= 2 { close_payload(Some(code), "") } else { Vec::new() };
                        let _ = stream.write_all(&frame(0x8, &echo));
                        let _ = stream.flush();
                    }
                    on_event(WsEvent::Closed { code, reason, clean: true });
                    return;
                }
                0x9 => {
                    if !close_sent && stream.write_all(&frame(0xA, &payload)).and_then(|_| stream.flush()).is_err() {
                        fail(on_event);
                        return;
                    }
                }
                0xA => {}
                _ => {
                    fail(on_event);
                    return;
                }
            }
        }
    }
}

fn deliver(opcode: u8, data: Vec<u8>, on_event: &dyn Fn(WsEvent)) -> bool {
    if opcode == 0x1 {
        match String::from_utf8(data) {
            Ok(s) => on_event(WsEvent::Text(s)),
            Err(_) => return false,
        }
    } else {
        on_event(WsEvent::Binary(data));
    }
    true
}
