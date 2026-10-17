// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronNet — the OpenHeronBrowser network stack.
//!
//! A small, security-focused HTTP client built directly on top of TCP and
//! rustls. Features: HTTP/2 over TLS (ALPN; one multiplexed connection per
//! origin) with HTTP/1.1 fallback, HTTPS (TLS 1.2/1.3, Mozilla + OS roots),
//! keep-alive connection pooling, CONNECT proxies (HTTPS_PROXY), redirects, gzip/deflate/
//! brotli decoding, cookies (with third-party blocking), a bounded in-memory
//! HTTP cache, and data:/file: URLs. Requests run on a shared worker pool so
//! page threads never block on the network.

pub mod archive;
pub mod cookies;
pub mod cache;
pub mod h2;
pub mod cors;
pub mod ws;

use std::collections::{HashMap, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use url::Url;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Chrome major version OHB advertises for compatibility. Sites such as
/// google.com serve a legacy page to engines they don't recognize, so OHB
/// presents a Chrome-compatible UA with its own product token appended (as
/// Edge, Opera and Vivaldi do).
pub const CHROME_COMPAT: &str = "141.0.0.0";
const MAX_REDIRECTS: usize = 10;
const MAX_BODY: usize = 64 * 1024 * 1024; // 64 MiB hard cap per response
const MAX_HEADER_BYTES: usize = 256 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const POOL_IDLE_LIMIT: Duration = Duration::from_secs(60);
const POOL_PER_HOST: usize = 6;

/// The platform token used in the UA string and `navigator.platform`.
pub fn ua_platform() -> (&'static str, &'static str) {
    // Debugging aid: OHB_EMULATE_PLATFORM=mac|windows|linux presents as that
    // platform (sites serve per-platform variants of their pages).
    static EMU: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    match EMU.get_or_init(|| std::env::var("OHB_EMULATE_PLATFORM").ok()).as_deref() {
        Some("mac") => return ("Macintosh; Intel Mac OS X 10_15_7", "MacIntel"),
        Some("windows") => return ("Windows NT 10.0; Win64; x64", "Win32"),
        Some("linux") => return ("X11; Linux x86_64", "Linux x86_64"),
        _ => {}
    }
    if cfg!(target_os = "windows") {
        ("Windows NT 10.0; Win64; x64", "Win32")
    } else if cfg!(target_os = "macos") {
        ("Macintosh; Intel Mac OS X 10_15_7", "MacIntel")
    } else {
        ("X11; Linux x86_64", "Linux x86_64")
    }
}

pub fn user_agent() -> String {
    // Debugging aid: OHB_UA overrides the user agent string.
    if let Ok(ua) = std::env::var("OHB_UA") {
        return ua;
    }
    let (platform, _) = ua_platform();
    format!("Mozilla/5.0 ({platform}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{CHROME_COMPAT} Safari/537.36 OpenHeronBrowser/{VERSION}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Document,
    Stylesheet,
    Script,
    Image,
    Font,
    Fetch,
    Favicon,
}

#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// URL of the document that initiated the request (None = user typed it).
    pub initiator: Option<Url>,
    pub kind: RequestKind,
    /// Whether cookies may be attached / stored.
    pub credentials: bool,
    /// A document request for an iframe (rather than a top-level page).
    pub frame: bool,
    /// Follow redirects (false: a 3xx response comes back as is).
    pub follow_redirects: bool,
    /// Whether the HTTP cache may answer this request / store its response.
    pub cache_lookup: bool,
    pub cache_store: bool,
    /// Send a Referer header (from the initiator).
    pub send_referer: bool,
    /// Observes the response as it arrives (head, then decoded body chunks).
    pub sink: Option<BodySink>,
}

/// A streaming observer of an HTTP/1 response: the head as soon as it is
/// parsed, then each piece of the (content-decoded) body. Returning false
/// aborts the request (the connection is closed).
#[derive(Clone)]
pub struct BodySink(pub Arc<dyn Fn(SinkEvent) -> bool + Send + Sync>);

impl std::fmt::Debug for BodySink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BodySink")
    }
}

pub enum SinkEvent<'a> {
    Head { status: u16, status_text: &'a str, headers: &'a [(String, String)] },
    Chunk(&'a [u8]),
}

/// Content decoding for a streamed body (gzip, zlib deflate, brotli).
enum StreamDecoder {
    Identity,
    Gzip(flate2::write::MultiGzDecoder<Vec<u8>>),
    Zlib(flate2::write::ZlibDecoder<Vec<u8>>),
    Br(Box<brotli_decompressor::DecompressorWriter<Vec<u8>>>),
    /// Not streamable (or failed): the whole body arrives at the end.
    Off,
}

impl StreamDecoder {
    fn new(enc: &str) -> StreamDecoder {
        match enc.trim() {
            "" | "identity" => StreamDecoder::Identity,
            "gzip" | "x-gzip" => StreamDecoder::Gzip(flate2::write::MultiGzDecoder::new(Vec::new())),
            "deflate" => StreamDecoder::Zlib(flate2::write::ZlibDecoder::new(Vec::new())),
            "br" => StreamDecoder::Br(Box::new(brotli_decompressor::DecompressorWriter::new(Vec::new(), 8192))),
            _ => StreamDecoder::Off,
        }
    }
    fn feed(&mut self, raw: &[u8]) -> Vec<u8> {
        let r = match self {
            StreamDecoder::Identity => return raw.to_vec(),
            StreamDecoder::Off => return Vec::new(),
            StreamDecoder::Gzip(w) => w.write_all(raw).and_then(|_| w.flush()).map(|_| std::mem::take(w.get_mut())),
            StreamDecoder::Zlib(w) => w.write_all(raw).and_then(|_| w.flush()).map(|_| std::mem::take(w.get_mut())),
            StreamDecoder::Br(w) => w.write_all(raw).and_then(|_| w.flush()).map(|_| std::mem::take(w.get_mut())),
        };
        match r {
            Ok(v) => v,
            Err(_) => {
                *self = StreamDecoder::Off;
                Vec::new()
            }
        }
    }
}

impl Request {
    pub fn get(url: Url, kind: RequestKind, initiator: Option<Url>) -> Request {
        Request {
            method: "GET".into(),
            url,
            headers: Vec::new(),
            body: None,
            initiator,
            kind,
            credentials: true,
            frame: false,
            follow_redirects: true,
            cache_lookup: true,
            cache_store: true,
            send_referer: true,
            sink: None,
        }
    }
}

/// Who a request is for: the tab and its top-level page (set on the page
/// thread, carried to the network workers).
#[derive(Clone, Debug)]
pub struct ReqContext {
    pub tab: u64,
    pub top: Option<Url>,
}

thread_local! {
    static REQ_CTX: std::cell::RefCell<Option<Arc<ReqContext>>> = const { std::cell::RefCell::new(None) };
}

/// Set this thread's request context (page threads, at each navigation).
pub fn set_request_context(ctx: Option<ReqContext>) {
    REQ_CTX.with(|c| *c.borrow_mut() = ctx.map(Arc::new));
}

fn current_context() -> Option<Arc<ReqContext>> {
    REQ_CTX.with(|c| c.borrow().clone())
}

/// A request filter (extensions): `Some(Ok(resp))` answers the request
/// (a stand-in resource or a 307 redirect), `Some(Err(_))` blocks it.
pub type RequestHook = Box<dyn Fn(&Request, Option<&ReqContext>) -> Option<Result<Response, NetError>> + Send + Sync>;

static REQUEST_HOOK: OnceLock<RequestHook> = OnceLock::new();

pub fn set_request_hook(h: RequestHook) {
    let _ = REQUEST_HOOK.set(h);
}

#[derive(Clone, Debug)]
pub struct Response {
    pub url: Url,
    pub status: u16,
    pub status_text: String,
    pub headers: Vec<(String, String)>,
    pub body: Arc<Vec<u8>>,
    pub redirected: bool,
    pub from_cache: bool,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        header_value(&self.headers, name)
    }
    pub fn content_type(&self) -> String {
        self.header("content-type")
            .map(|s| s.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
            .unwrap_or_default()
    }
    pub fn charset(&self) -> Option<String> {
        let ct = self.header("content-type")?;
        for part in ct.split(';').skip(1) {
            let mut kv = part.splitn(2, '=');
            if kv.next()?.trim().eq_ignore_ascii_case("charset") {
                return kv.next().map(|v| v.trim().trim_matches('"').to_ascii_lowercase());
            }
        }
        None
    }
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[derive(Clone, Debug)]
pub enum NetError {
    InvalidUrl(String),
    Unsupported(String),
    Dns(String),
    Connect(String),
    Tls(String),
    Timeout,
    TooLarge,
    Protocol(String),
    Blocked(String),
    Io(String),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetError::InvalidUrl(s) => write!(f, "ERR_INVALID_URL: {s}"),
            NetError::Unsupported(s) => write!(f, "ERR_UNKNOWN_URL_SCHEME: {s}"),
            NetError::Dns(s) => write!(f, "ERR_NAME_NOT_RESOLVED: {s}"),
            NetError::Connect(s) => write!(f, "ERR_CONNECTION_FAILED: {s}"),
            NetError::Tls(s) => write!(f, "ERR_CERT_INVALID / TLS error: {s}"),
            NetError::Timeout => write!(f, "ERR_TIMED_OUT"),
            NetError::TooLarge => write!(f, "ERR_RESPONSE_TOO_LARGE"),
            NetError::Protocol(s) => write!(f, "ERR_INVALID_RESPONSE: {s}"),
            NetError::Blocked(s) => write!(f, "ERR_BLOCKED_BY_CLIENT: {s}"),
            NetError::Io(s) => write!(f, "ERR_CONNECTION_RESET: {s}"),
        }
    }
}

impl From<io::Error> for NetError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => NetError::Timeout,
            _ => {
                // rustls surfaces certificate failures as InvalidData io errors.
                let msg = e.to_string();
                if msg.contains("certificate") || msg.contains("Certificate") || msg.contains("tls") {
                    NetError::Tls(msg)
                } else {
                    NetError::Io(msg)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TLS configuration
// ---------------------------------------------------------------------------

/// TLS client configuration; `h2` offers HTTP/2 through ALPN.
fn tls_config_alpn(h2: bool) -> Arc<rustls::ClientConfig> {
    static CFG2: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    if !h2 {
        return tls_config();
    }
    CFG2.get_or_init(|| {
        let mut cfg = (*tls_config()).clone();
        cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Arc::new(cfg)
    })
    .clone()
}

/// Is HTTP/2 enabled (OHB_NO_H2 turns it off)?
pub fn h2_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("OHB_NO_H2").is_none())
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        // Mozilla's curated root program, compiled in so the browser works on
        // any machine...
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        // ...plus the operating system's trust store (corporate CAs, proxies).
        let native = rustls_native_certs::load_native_certs();
        for cert in native.certs {
            let _ = roots.add(cert);
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let inner = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().expect("tls verifier");
        let mut cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("tls versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AiaVerifier { inner }))
            .with_no_client_auth();
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Arc::new(cfg)
    })
    .clone()
}

/// Standard certificate verification, plus "AIA fetching" as browsers do:
/// when a server leaves out an intermediate certificate, it is downloaded
/// from the address the server's certificate names (Authority Information
/// Access, CA Issuers) and the chain is verified again. Trust still comes
/// only from the root store.
#[derive(Debug)]
struct AiaVerifier {
    inner: Arc<rustls::client::WebPkiServerVerifier>,
}

/// The CA Issuers URL in a DER certificate's Authority Information Access.
fn aia_issuer_url(der: &[u8]) -> Option<String> {
    // id-ad-caIssuers (1.3.6.1.5.5.7.48.2) followed by a [6] URI.
    const OID: &[u8] = &[0x06, 0x08, 0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x02];
    let i = der.windows(OID.len()).position(|w| w == OID)? + OID.len();
    if der.get(i) != Some(&0x86) {
        return None;
    }
    let len = *der.get(i + 1)? as usize;
    if len >= 0x80 {
        return None;
    }
    let url = std::str::from_utf8(der.get(i + 2..i + 2 + len)?).ok()?;
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_string())
}

/// Download an issuer certificate (DER or PEM), cached per URL.
fn fetch_issuer(url: &str) -> Option<Vec<u8>> {
    static CACHE: std::sync::LazyLock<Mutex<HashMap<String, Option<Vec<u8>>>>> = std::sync::LazyLock::new(Default::default);
    if let Some(v) = CACHE.lock().unwrap().get(url) {
        return v.clone();
    }
    let got = (|| {
        let u = Url::parse(url).ok()?;
        let resp = client().fetch(Request::get(u, RequestKind::Fetch, None)).ok()?;
        if !resp.ok() || resp.body.len() > 64 * 1024 {
            return None;
        }
        let body = &resp.body[..];
        if body.starts_with(b"-----BEGIN") {
            let text = String::from_utf8_lossy(body);
            let b64: String = text.lines().filter(|l| !l.starts_with("-----")).collect();
            return base64_decode(b64.as_bytes());
        }
        (body.first() == Some(&0x30)).then(|| body.to_vec())
    })();
    CACHE.lock().unwrap().insert(url.to_string(), got.clone());
    got
}

impl rustls::client::danger::ServerCertVerifier for AiaVerifier {
    fn verify_server_cert(&self, end_entity: &rustls::pki_types::CertificateDer<'_>, intermediates: &[rustls::pki_types::CertificateDer<'_>], server_name: &rustls::pki_types::ServerName<'_>, ocsp: &[u8], now: rustls::pki_types::UnixTime) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let first = self.inner.verify_server_cert(end_entity, intermediates, server_name, ocsp, now);
        if !matches!(first, Err(rustls::Error::InvalidCertificate(rustls::CertificateError::UnknownIssuer))) {
            return first;
        }
        let mut chain: Vec<rustls::pki_types::CertificateDer<'static>> = intermediates.iter().map(|c| c.clone().into_owned()).collect();
        let mut cur: Vec<u8> = chain.last().map(|c| c.to_vec()).unwrap_or_else(|| end_entity.to_vec());
        for _ in 0..3 {
            let Some(url) = aia_issuer_url(&cur) else { break };
            let Some(der) = fetch_issuer(&url) else { break };
            chain.push(rustls::pki_types::CertificateDer::from(der.clone()));
            if let Ok(v) = self.inner.verify_server_cert(end_entity, &chain, server_name, ocsp, now) {
                return Ok(v);
            }
            cur = der;
        }
        first
    }
    fn verify_tls12_signature(&self, message: &[u8], cert: &rustls::pki_types::CertificateDer<'_>, dss: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }
    fn verify_tls13_signature(&self, message: &[u8], cert: &rustls::pki_types::CertificateDer<'_>, dss: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

// ---------------------------------------------------------------------------
// Connections
// ---------------------------------------------------------------------------

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => match s.read(buf) {
                // Servers commonly close without close_notify; treat as EOF.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(0),
                r => r,
            },
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

struct Conn {
    reader: BufReader<Stream>,
    last_used: Instant,
}

impl Conn {
    fn stream(&mut self) -> &mut Stream {
        self.reader.get_mut()
    }
}

type PoolKey = (String, String, u16);

/// At most this many HTTP/1.1 requests in flight per origin (like Chromium's
/// six connections per host); HTTP/2 origins multiplex without a limit.
const H1_MAX_PER_HOST: usize = 6;

struct H1Slots {
    m: Mutex<HashMap<PoolKey, usize>>,
    cv: Condvar,
}

fn h1_slots() -> &'static H1Slots {
    static S: OnceLock<H1Slots> = OnceLock::new();
    S.get_or_init(|| H1Slots { m: Mutex::new(HashMap::new()), cv: Condvar::new() })
}

struct H1Slot(PoolKey);

impl H1Slot {
    fn acquire(key: &PoolKey) -> H1Slot {
        let s = h1_slots();
        let mut m = s.m.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        while m.get(key).copied().unwrap_or(0) >= H1_MAX_PER_HOST && Instant::now() < deadline {
            m = s.cv.wait_timeout(m, Duration::from_millis(250)).unwrap().0;
        }
        *m.entry(key.clone()).or_insert(0) += 1;
        H1Slot(key.clone())
    }
}

impl Drop for H1Slot {
    fn drop(&mut self) {
        let s = h1_slots();
        let mut m = s.m.lock().unwrap();
        if let Some(n) = m.get_mut(&self.0) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                m.remove(&self.0);
            }
        }
        s.cv.notify_all();
    }
}

#[derive(Clone, Debug)]
struct ProxyConfig {
    host: String,
    port: u16,
    auth: Option<String>,
    no_proxy: Vec<String>,
}

fn proxy_from_env() -> Option<ProxyConfig> {
    let var = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))?;
    let with_scheme = if var.contains("://") { var.clone() } else { format!("http://{var}") };
    let u = Url::parse(&with_scheme).ok()?;
    let host = u.host_str()?.to_string();
    let port = u.port_or_known_default().unwrap_or(8080);
    let auth = if !u.username().is_empty() {
        let raw = format!("{}:{}", u.username(), u.password().unwrap_or(""));
        Some(base64_encode(raw.as_bytes()))
    } else {
        None
    };
    let no_proxy = std::env::var("NO_PROXY")
        .or_else(|_| std::env::var("no_proxy"))
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    Some(ProxyConfig { host, port, auth, no_proxy })
}

impl ProxyConfig {
    fn bypass(&self, host: &str) -> bool {
        let h = host.to_ascii_lowercase();
        self.no_proxy.iter().any(|n| {
            if n == "*" {
                return true;
            }
            // Strip CIDR / port decorations we don't evaluate.
            let n = n.split('/').next().unwrap_or(n);
            h == n || h.ends_with(&format!(".{n}"))
        })
    }
}

/// A freshly opened connection: HTTP/1.1, or HTTP/2 when ALPN chose it.
enum Opened {
    H1(Conn),
    H2(Arc<h2::H2Conn>),
}

pub struct NetClient {
    pool: Mutex<HashMap<PoolKey, Vec<Conn>>>,
    /// HTTP/2 connections by origin (one each, shared by all requests).
    h2: Mutex<HashMap<PoolKey, Arc<h2::H2Conn>>>,
    /// Whether an origin speaks HTTP/2 (unknown until its first connection).
    h2_known: Mutex<HashMap<PoolKey, bool>>,
    /// Serializes the first connection to an origin so concurrent requests
    /// share it when it turns out to be HTTP/2.
    connecting: Mutex<HashMap<PoolKey, Arc<Mutex<()>>>>,
    pub cookies: Mutex<cookies::CookieJar>,
    pub cache: Mutex<cache::HttpCache>,
    proxy: Option<ProxyConfig>,
    ua: String,
}

pub fn client() -> &'static NetClient {
    static CLIENT: OnceLock<NetClient> = OnceLock::new();
    CLIENT.get_or_init(|| NetClient {
        pool: Mutex::new(HashMap::new()),
        h2: Mutex::new(HashMap::new()),
        h2_known: Mutex::new(HashMap::new()),
        connecting: Mutex::new(HashMap::new()),
        cookies: Mutex::new(cookies::CookieJar::default()),
        cache: Mutex::new(cache::HttpCache::new(48 * 1024 * 1024)),
        proxy: proxy_from_env(),
        ua: user_agent(),
    })
}

fn resolve_and_connect(host: &str, port: u16) -> Result<TcpStream, NetError> {
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| NetError::Dns(format!("{host}: {e}")))?
        .collect();
    if addrs.is_empty() {
        return Err(NetError::Dns(host.to_string()));
    }
    let mut last = NetError::Connect(host.to_string());
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(s) => {
                let _ = s.set_nodelay(true);
                let _ = s.set_read_timeout(Some(IO_TIMEOUT));
                let _ = s.set_write_timeout(Some(IO_TIMEOUT));
                return Ok(s);
            }
            Err(e) => {
                last = if e.kind() == io::ErrorKind::TimedOut {
                    NetError::Timeout
                } else {
                    NetError::Connect(format!("{addr}: {e}"))
                }
            }
        }
    }
    Err(last)
}

impl NetClient {
    fn open(&self, scheme: &str, host: &str, port: u16) -> Result<Conn, NetError> {
        match self.open_any(scheme, host, port, false)? {
            Opened::H1(c) => Ok(c),
            Opened::H2(_) => Err(NetError::Protocol("unexpected h2".into())),
        }
    }

    fn open_any(&self, scheme: &str, host: &str, port: u16, allow_h2: bool) -> Result<Opened, NetError> {
        let tcp = match &self.proxy {
            Some(p) if !p.bypass(host) => {
                let mut s = resolve_and_connect(&p.host, p.port)?;
                let mut req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
                if let Some(a) = &p.auth {
                    req.push_str(&format!("Proxy-Authorization: Basic {a}\r\n"));
                }
                req.push_str("\r\n");
                s.write_all(req.as_bytes())?;
                // Read the proxy's reply byte-by-byte so we never consume
                // bytes that belong to the tunnelled TLS stream.
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if s.read(&mut b)? == 0 || head.len() > 16 * 1024 {
                        return Err(NetError::Connect("proxy closed connection".into()));
                    }
                    head.push(b[0]);
                }
                let line = String::from_utf8_lossy(&head);
                let code = line.split_whitespace().nth(1).unwrap_or("");
                if code != "200" {
                    return Err(NetError::Blocked(format!(
                        "proxy refused tunnel to {host}: {}",
                        line.lines().next().unwrap_or("")
                    )));
                }
                s
            }
            _ => resolve_and_connect(host, port)?,
        };
        let stream = if scheme == "https" {
            let name = rustls::pki_types::ServerName::try_from(host.to_string())
                .map_err(|e| NetError::InvalidUrl(e.to_string()))?;
            let conn = rustls::ClientConnection::new(tls_config_alpn(allow_h2), name)
                .map_err(|e| NetError::Tls(e.to_string()))?;
            let mut tls = rustls::StreamOwned::new(conn, tcp);
            // Drive the handshake now so certificate errors are reported as such.
            while tls.conn.is_handshaking() {
                match tls.conn.complete_io(&mut tls.sock) {
                    Ok(_) => {}
                    Err(e) => return Err(NetError::Tls(e.to_string())),
                }
            }
            if allow_h2 && tls.conn.alpn_protocol() == Some(b"h2") {
                let rustls::StreamOwned { conn, sock } = tls;
                let authority = if Some(port) == default_port(scheme) { host.to_string() } else { format!("{host}:{port}") };
                return Ok(Opened::H2(h2::H2Conn::start(conn, sock, authority)?));
            }
            Stream::Tls(Box::new(tls))
        } else {
            Stream::Plain(tcp)
        };
        Ok(Opened::H1(Conn { reader: BufReader::with_capacity(32 * 1024, stream), last_used: Instant::now() }))
    }

    /// The origin's live HTTP/2 connection, if any.
    fn h2_get(&self, key: &PoolKey) -> Option<Arc<h2::H2Conn>> {
        let mut m = self.h2.lock().unwrap();
        match m.get(key) {
            Some(c) if c.usable() => Some(c.clone()),
            Some(_) => {
                m.remove(key);
                None
            }
            None => None,
        }
    }

    fn h2_drop(&self, key: &PoolKey, c: &Arc<h2::H2Conn>) {
        let mut m = self.h2.lock().unwrap();
        if m.get(key).map(|x| Arc::ptr_eq(x, c)).unwrap_or(false) {
            m.remove(key);
        }
    }

    /// An HTTP/2 connection for the origin, or an HTTP/1.1 one. The first
    /// connection to an origin is made alone, so requests that arrive
    /// meanwhile wait and then share it if it speaks HTTP/2.
    fn connection(&self, key: &PoolKey) -> Result<(Opened, bool), NetError> {
        let (scheme, host, port) = (&key.0, &key.1, key.2);
        let h2_ok = scheme == "https" && h2_enabled();
        if h2_ok {
            if let Some(c) = self.h2_get(key) {
                return Ok((Opened::H2(c), true));
            }
        }
        let known = self.h2_known.lock().unwrap().get(key).copied();
        if !h2_ok || known == Some(false) {
            return Ok(match self.checkout(key) {
                Some(c) => (Opened::H1(c), true),
                None => (self.open_any(scheme, host, port, h2_ok)?, false),
            });
        }
        let gate = self.connecting.lock().unwrap().entry(key.clone()).or_default().clone();
        let _g = gate.lock().unwrap();
        if let Some(c) = self.h2_get(key) {
            return Ok((Opened::H2(c), true));
        }
        if self.h2_known.lock().unwrap().get(key) == Some(&false) {
            if let Some(c) = self.checkout(key) {
                return Ok((Opened::H1(c), true));
            }
        }
        let opened = self.open_any(scheme, host, port, true)?;
        match &opened {
            Opened::H2(c) => {
                self.h2.lock().unwrap().insert(key.clone(), c.clone());
                self.h2_known.lock().unwrap().insert(key.clone(), true);
            }
            Opened::H1(_) => {
                self.h2_known.lock().unwrap().insert(key.clone(), false);
            }
        }
        Ok((opened, false))
    }

    fn checkout(&self, key: &PoolKey) -> Option<Conn> {
        let mut pool = self.pool.lock().unwrap();
        let list = pool.get_mut(key)?;
        while let Some(c) = list.pop() {
            if c.last_used.elapsed() < POOL_IDLE_LIMIT {
                return Some(c);
            }
        }
        None
    }

    fn checkin(&self, key: PoolKey, mut conn: Conn) {
        conn.last_used = Instant::now();
        let mut pool = self.pool.lock().unwrap();
        let list = pool.entry(key).or_default();
        if list.len() < POOL_PER_HOST {
            list.push(conn);
        }
    }

    /// Perform a request, following redirects. Blocking — call from a worker.
    pub fn fetch(&self, mut req: Request) -> Result<Response, NetError> {
        let mut redirected = false;
        for _ in 0..=MAX_REDIRECTS {
            match req.url.scheme() {
                "data" => return data_url(&req.url),
                "blob" => return blob_url(&req),
                "file" => return file_url(&req),
                "ohb-extension" | "chrome-extension" => {
                    return crate::ext::extension_resource(&req.url).ok_or_else(|| NetError::Blocked(format!("{} is not web-accessible", req.url)));
                }
                "http" | "https" => {}
                "about" if req.url.as_str() == "about:blank" => {
                    return Ok(synthetic(req.url.clone(), 200, "text/html", Vec::new()));
                }
                s => return Err(NetError::Unsupported(s.to_string())),
            }
            // Mixed content: never load active/passive http subresources into
            // an https page; upgrade them instead (like modern Chrome).
            if req.url.scheme() == "http" && req.kind != RequestKind::Document {
                if let Some(init) = &req.initiator {
                    if init.scheme() == "https" {
                        let _ = req.url.set_scheme("https");
                    }
                }
            }
            let hooked = match REQUEST_HOOK.get() {
                Some(h) => {
                    let ctx = current_context();
                    h(&req, ctx.as_deref())
                }
                None => None,
            };
            let from_hook = hooked.is_some();
            let resp = match hooked {
                Some(r) => r?,
                None => {
                    if req.method == "GET" && req.cache_lookup {
                        if let Some(resp) = self.cache.lock().unwrap().lookup(&req.url) {
                            let mut r = resp;
                            r.redirected = redirected;
                            return Ok(r);
                        }
                    }
                    self.fetch_once(&req)?
                }
            };
            if matches!(resp.status, 301 | 302 | 303 | 307 | 308) && req.follow_redirects {
                if let Some(loc) = resp.header("location") {
                    let next = req
                        .url
                        .join(loc)
                        .map_err(|e| NetError::Protocol(format!("bad redirect: {e}")))?;
                    if resp.status == 303 || ((resp.status == 301 || resp.status == 302) && req.method == "POST") {
                        req.method = "GET".into();
                        req.body = None;
                        req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
                    }
                    // Never forward credentials-bearing headers cross-origin.
                    if next.origin() != req.url.origin() {
                        req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
                    }
                    req.url = next;
                    redirected = true;
                    continue;
                }
            }
            let mut resp = resp;
            resp.redirected = redirected;
            if req.method == "GET" && !from_hook && req.cache_store {
                self.cache.lock().unwrap().store(&resp);
            }
            return Ok(resp);
        }
        Err(NetError::Protocol("too many redirects".into()))
    }

    fn fetch_once(&self, req: &Request) -> Result<Response, NetError> {
        let Some(ar) = archive::archive().filter(|_| req.method == "GET") else {
            return self.fetch_live(req);
        };
        if let Some(resp) = ar.lookup(&req.url) {
            self.store_cookies(req, &resp);
            return Ok(resp);
        }
        if ar.offline {
            if std::env::var_os("OHB_QUIET").is_none() {
                eprintln!("[archive] not recorded: {}", req.url);
            }
            return Ok(synthetic(req.url.clone(), 404, "text/plain", b"not in archive".to_vec()));
        }
        // Retry transient failures so one flaky fetch doesn't poison a test run.
        let mut last = None;
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(400 * attempt));
            }
            match self.fetch_live(req) {
                Ok(resp) if matches!(resp.status, 502..=504) => last = Some(Ok(resp)),
                Ok(resp) => {
                    ar.store(&resp);
                    return Ok(resp);
                }
                Err(e) => last = Some(Err(e)),
            }
        }
        last.unwrap_or(Err(NetError::Timeout))
    }

    fn store_cookies(&self, req: &Request, resp: &Response) {
        if !req.credentials {
            return;
        }
        let mut jar = self.cookies.lock().unwrap();
        for (k, v) in &resp.headers {
            if k.eq_ignore_ascii_case("set-cookie") {
                if std::env::var_os("OHB_DEBUG_COOKIES").is_some() {
                    // Name and attributes only (never the value).
                    let (name, rest) = v.split_once('=').unwrap_or((v, ""));
                    let attrs = rest.split_once(';').map(|x| x.1).unwrap_or("");
                    eprintln!("[cookie] {} {} <- {}={{..}};{} (status {})", req.method, resp.url, name, attrs, resp.status);
                }
                jar.set_from_header(v, &resp.url, req.initiator.as_ref(), req.kind == RequestKind::Document);
            }
        }
    }

    fn fetch_live(&self, req: &Request) -> Result<Response, NetError> {
        let url = &req.url;
        let host = url.host_str().ok_or_else(|| NetError::InvalidUrl(url.to_string()))?.to_string();
        let port = url.port_or_known_default().unwrap_or(80);
        let scheme = url.scheme().to_string();
        let key: PoolKey = (scheme.clone(), host.clone(), port);

        let headers = self.request_headers(req, &host, port);
        let head = self.build_request_head(req, &host, port, &headers);
        let h1_known = !(scheme == "https" && h2_enabled()) || self.h2_known.lock().unwrap().get(&key) == Some(&false);
        let _slot = if h1_known { Some(H1Slot::acquire(&key)) } else { None };
        // Try a pooled connection first; if it turns out to be dead, retry
        // once on a fresh connection (only for idempotent requests).
        let mut attempt = 0;
        loop {
            let (mut conn, reused) = match self.connection(&key)? {
                (Opened::H1(c), reused) => (c, reused),
                (Opened::H2(c), reused) => match c.request(req, &headers) {
                    Ok(resp) => {
                        self.store_cookies(req, &resp);
                        return Ok(resp);
                    }
                    Err((e, retry)) => {
                        self.h2_drop(&key, &c);
                        attempt += 1;
                        let idempotent = matches!(req.method.as_str(), "GET" | "HEAD" | "OPTIONS");
                        // Stream resets and dropped connections are retried
                        // for idempotent requests (not stalls: that would
                        // double the wait).
                        let transient = !matches!(e, NetError::Timeout);
                        if ((retry && attempt < 5) || (idempotent && (reused || transient) && attempt < 3)) && attempt < 5 {
                            continue;
                        }
                        return Err(e);
                    }
                },
            };
            let head_seen = std::cell::Cell::new(false);
            let result = self.exchange(&mut conn, req, &head, &head_seen);
            match result {
                Ok((resp, keep_alive)) => {
                    if keep_alive {
                        self.checkin(key, conn);
                    }
                    self.store_cookies(req, &resp);
                    return Ok(resp);
                }
                Err(e) => {
                    attempt += 1;
                    let idempotent = matches!(req.method.as_str(), "GET" | "HEAD" | "OPTIONS");
                    // Not after a streaming observer saw the response: it would see it twice.
                    if reused && attempt < 2 && idempotent && !head_seen.get() && !matches!(&e, NetError::Blocked(m) if m == "aborted") {
                        continue;
                    }
                    return Err(e);
                }
            }
        }
    }

    /// The request's header fields (without Host and connection headers),
    /// in Chromium's order with its client hints and fetch metadata.
    fn request_headers(&self, req: &Request, _host: &str, _port: u16) -> Vec<(String, String)> {
        let url = &req.url;
        let mut h: Vec<(String, String)> = Vec::new();
        let has = |name: &str| req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
        let secure = url.scheme() == "https";
        let doc = req.kind == RequestKind::Document;
        if secure {
            let (_, platform) = ua_platform();
            let plat = if platform.starts_with("Mac") { "macOS" } else if platform.starts_with("Win") { "Windows" } else { "Linux" };
            let chrome = CHROME_COMPAT.split('.').next().unwrap_or("141");
            let major = VERSION.split('.').take(2).collect::<Vec<_>>().join(".");
            h.push(("sec-ch-ua".into(), format!("\"OpenHeronBrowser\";v=\"{major}\", \"Chromium\";v=\"{chrome}\", \"Not=A?Brand\";v=\"24\"")));
            h.push(("sec-ch-ua-mobile".into(), "?0".into()));
            h.push(("sec-ch-ua-platform".into(), format!("\"{plat}\"")));
        }
        if doc {
            h.push(("Upgrade-Insecure-Requests".into(), "1".into()));
        }
        h.push(("User-Agent".into(), self.ua.clone()));
        if !has("accept") {
            let accept = match req.kind {
                RequestKind::Document => "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8",
                RequestKind::Stylesheet => "text/css,*/*;q=0.1",
                RequestKind::Image | RequestKind::Favicon => "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
                RequestKind::Font => "*/*",
                _ => "*/*",
            };
            h.push(("Accept".into(), accept.into()));
        }
        if secure && !has("sec-fetch-mode") {
            let site = match &req.initiator {
                None => "none",
                Some(i) if i.origin() == url.origin() => "same-origin",
                Some(i) if site_of(i) == site_of(url) => "same-site",
                Some(_) => "cross-site",
            };
            let (mode, dest) = match req.kind {
                RequestKind::Document if req.frame => ("navigate", "iframe"),
                RequestKind::Document => ("navigate", "document"),
                RequestKind::Stylesheet => ("no-cors", "style"),
                RequestKind::Script => ("no-cors", "script"),
                RequestKind::Image | RequestKind::Favicon => ("no-cors", "image"),
                RequestKind::Font => ("cors", "font"),
                RequestKind::Fetch => ("cors", "empty"),
            };
            h.push(("Sec-Fetch-Site".into(), site.into()));
            h.push(("Sec-Fetch-Mode".into(), mode.into()));
            if doc && req.initiator.is_none() {
                h.push(("Sec-Fetch-User".into(), "?1".into()));
            }
            h.push(("Sec-Fetch-Dest".into(), dest.into()));
        }
        h.push(("Accept-Encoding".into(), "gzip, deflate, br".into()));
        if !has("accept-language") {
            h.push(("Accept-Language".into(), "en-US,en;q=0.9".into()));
        }
        // Privacy: Global Privacy Control signal.
        h.push(("Sec-GPC".into(), "1".into()));
        // Referrer policy: strict-origin-when-cross-origin.
        if let Some(init) = req.initiator.as_ref().filter(|_| req.send_referer && !has("referer")) {
            if matches!(init.scheme(), "http" | "https") && !(init.scheme() == "https" && url.scheme() == "http") {
                let referer = if init.origin() == url.origin() {
                    let mut r = init.clone();
                    r.set_fragment(None);
                    r.to_string()
                } else {
                    format!("{}/", init.origin().ascii_serialization())
                };
                h.push(("Referer".into(), referer));
            }
        }
        if req.credentials {
            let jar = self.cookies.lock().unwrap();
            if let Some(c) = jar.header_for(url, req.initiator.as_ref(), req.kind == RequestKind::Document, true) {
                h.push(("Cookie".into(), c));
            }
        }
        for (k, v) in &req.headers {
            // Disallow header injection.
            if k.contains(['\r', '\n', ':']) || v.contains(['\r', '\n']) {
                continue;
            }
            let lk = k.to_ascii_lowercase();
            if matches!(lk.as_str(), "host" | "connection" | "content-length" | "cookie" | "user-agent" | "accept-encoding" | "transfer-encoding") {
                continue;
            }
            h.push((k.clone(), v.clone()));
        }
        if let Some(b) = &req.body {
            h.push(("Content-Length".into(), b.len().to_string()));
        } else if req.method == "POST" || req.method == "PUT" {
            h.push(("Content-Length".into(), "0".into()));
        }
        if doc && secure {
            h.push(("priority".into(), "u=0, i".into()));
        }
        h
    }

    fn build_request_head(&self, req: &Request, host: &str, port: u16, headers: &[(String, String)]) -> String {
        let url = &req.url;
        let mut target = url.path().to_string();
        if let Some(q) = url.query() {
            target.push('?');
            target.push_str(q);
        }
        let default_port = url.port_or_known_default() == Some(port) && url.port().is_none();
        let host_header = if default_port { host.to_string() } else { format!("{host}:{port}") };
        let mut h = format!("{} {} HTTP/1.1\r\nHost: {}\r\n", req.method, target, host_header);
        for (k, v) in headers {
            h.push_str(k);
            h.push_str(": ");
            h.push_str(v);
            h.push_str("\r\n");
            if k == "Accept-Language" {
                h.push_str("Connection: keep-alive\r\n");
            }
        }
        h.push_str("\r\n");
        h
    }

    fn exchange(&self, conn: &mut Conn, req: &Request, head: &str, head_seen: &std::cell::Cell<bool>) -> Result<(Response, bool), NetError> {
        {
            let s = conn.stream();
            s.write_all(head.as_bytes())?;
            if let Some(b) = &req.body {
                s.write_all(b)?;
            }
            s.flush()?;
        }
        let r = &mut conn.reader;
        // Status line (skip any 1xx informational responses).
        let (status, status_text, http10, headers) = loop {
            let line = read_line(r)?;
            if line.is_empty() {
                return Err(NetError::Io("connection closed".into()));
            }
            let mut parts = line.splitn(3, ' ');
            let version = parts.next().unwrap_or("");
            if !version.starts_with("HTTP/") {
                return Err(NetError::Protocol(format!("bad status line: {}", truncate(&line, 80))));
            }
            let status: u16 = parts
                .next()
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| NetError::Protocol("bad status code".into()))?;
            let text = parts.next().unwrap_or("").to_string();
            let mut headers = Vec::new();
            let mut total = 0usize;
            loop {
                let l = read_line(r)?;
                if l.is_empty() {
                    break;
                }
                total += l.len();
                if total > MAX_HEADER_BYTES {
                    return Err(NetError::TooLarge);
                }
                if let Some((k, v)) = l.split_once(':') {
                    // HTTP whitespace only (a value keeps its \x0b and \x0c).
                    headers.push((k.trim().to_string(), v.trim_matches([' ', '\t']).to_string()));
                }
            }
            if (100..200).contains(&status) && status != 101 {
                continue;
            }
            break (status, text, version == "HTTP/1.0", headers);
        };

        let conn_hdr = header_value(&headers, "connection").unwrap_or("").to_ascii_lowercase();
        let mut keep_alive = if http10 { conn_hdr.contains("keep-alive") } else { !conn_hdr.contains("close") };
        let no_body = req.method == "HEAD" || status == 204 || status == 205 || status == 304;
        // A no-content response that announces content anyway (some servers send it):
        // the connection can't be trusted for the next request.
        if (status == 204 || status == 205 || status == 304)
            && (header_value(&headers, "content-length").map(|v| v.trim() != "0").unwrap_or(false) || header_value(&headers, "transfer-encoding").is_some())
        {
            keep_alive = false;
        }
        let te = header_value(&headers, "transfer-encoding").unwrap_or("").to_ascii_lowercase();
        let enc = header_value(&headers, "content-encoding").unwrap_or("").to_ascii_lowercase();
        // A streaming observer sees the head now and the body as it arrives.
        let sink = req.sink.clone();
        let aborted = || NetError::Blocked("aborted".into());
        if let Some(s) = &sink {
            head_seen.set(true);
            if !(s.0)(SinkEvent::Head { status, status_text: &status_text, headers: &headers }) {
                return Err(aborted());
            }
        }
        let mut dec = StreamDecoder::new(&enc);
        let mut emit = |raw: &[u8]| -> bool {
            match &sink {
                Some(s) if !raw.is_empty() => {
                    let d = dec.feed(raw);
                    d.is_empty() || (s.0)(SinkEvent::Chunk(&d))
                }
                _ => true,
            }
        };
        let mut body = Vec::new();
        if !no_body {
            if te.contains("chunked") {
                read_chunked(r, &mut body, &mut emit)?;
            } else if let Some(len) = header_value(&headers, "content-length").and_then(|v| v.parse::<usize>().ok()) {
                if len > MAX_BODY {
                    return Err(NetError::TooLarge);
                }
                if sink.is_none() {
                    body.resize(len, 0);
                    r.read_exact(&mut body)?;
                } else {
                    body.reserve(len);
                    let mut buf = [0u8; 16384];
                    while body.len() < len {
                        let want = (len - body.len()).min(buf.len());
                        let n = r.read(&mut buf[..want])?;
                        if n == 0 {
                            return Err(NetError::Io("connection closed mid-body".into()));
                        }
                        body.extend_from_slice(&buf[..n]);
                        if !emit(&buf[..n]) {
                            return Err(aborted());
                        }
                    }
                }
            } else {
                keep_alive = false;
                let mut buf = [0u8; 16384];
                loop {
                    let n = r.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    body.extend_from_slice(&buf[..n]);
                    if !emit(&buf[..n]) {
                        return Err(aborted());
                    }
                    if body.len() > MAX_BODY {
                        return Err(NetError::TooLarge);
                    }
                }
            }
        }
        let body = decode_body(&enc, body)?;
        Ok((
            Response {
                url: req.url.clone(),
                status,
                status_text,
                headers,
                body: Arc::new(body),
                redirected: false,
                from_cache: false,
            },
            keep_alive,
        ))
    }
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "https" => Some(443),
        "http" => Some(80),
        _ => None,
    }
}

fn truncate(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn read_line<R: BufRead>(r: &mut R) -> Result<String, NetError> {
    let mut buf = Vec::new();
    let n = r.take(64 * 1024).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(String::new());
    }
    while buf.last() == Some(&b'\n') || buf.last() == Some(&b'\r') {
        buf.pop();
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn read_chunked<R: BufRead>(r: &mut R, out: &mut Vec<u8>, emit: &mut dyn FnMut(&[u8]) -> bool) -> Result<(), NetError> {
    loop {
        let before = out.len();
        if read_one_chunk(r, out)? {
            return Ok(());
        }
        if out.len() == before {
            break;
        }
        if !emit(&out[before..]) {
            return Err(NetError::Blocked("aborted".into()));
        }
    }
    // Trailers until blank line.
    loop {
        let l = read_line(r)?;
        if l.is_empty() {
            break;
        }
    }
    Ok(())
}

/// One chunk of a chunked body appended to `out` (nothing appended: the
/// last chunk). Returns true when the body ended without trailers to read.
fn read_one_chunk<R: BufRead>(r: &mut R, out: &mut Vec<u8>) -> Result<bool, NetError> {
    let mut line = read_line(r)?;
    if line.trim().is_empty() {
        // Tolerate a stray blank line between chunks.
        line = read_line(r)?;
        if line.trim().is_empty() {
            return Ok(true);
        }
    }
    let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| NetError::Protocol("bad chunk size".into()))?;
    if size == 0 {
        return Ok(false);
    }
    read_chunk_body(r, size, out)?;
    Ok(false)
}

fn read_chunk_body<R: BufRead>(r: &mut R, size: usize, out: &mut Vec<u8>) -> Result<(), NetError> {
    if out.len() + size > MAX_BODY {
        return Err(NetError::TooLarge);
    }
    let start = out.len();
    out.resize(start + size, 0);
    r.read_exact(&mut out[start..])?;
    let mut crlf = [0u8; 2];
    r.read_exact(&mut crlf)?;
    Ok(())
}

pub(crate) fn decode_body(enc: &str, body: Vec<u8>) -> Result<Vec<u8>, NetError> {
    let enc = enc.trim();
    if enc.is_empty() || enc == "identity" || body.is_empty() {
        return Ok(body);
    }
    let mut out = Vec::with_capacity(body.len() * 3);
    let limit = MAX_BODY as u64 + 1;
    let res = match enc {
        "gzip" | "x-gzip" => flate2::read::MultiGzDecoder::new(&body[..]).take(limit).read_to_end(&mut out),
        "deflate" => {
            // "deflate" is supposed to be zlib-wrapped but many servers send raw deflate.
            let r = flate2::read::ZlibDecoder::new(&body[..]).take(limit).read_to_end(&mut out);
            if r.is_err() {
                out.clear();
                flate2::read::DeflateDecoder::new(&body[..]).take(limit).read_to_end(&mut out)
            } else {
                r
            }
        }
        "br" => brotli_decompressor::Decompressor::new(&body[..], 8192).take(limit).read_to_end(&mut out),
        "zstd" => match ruzstd::decoding::StreamingDecoder::new(&body[..]) {
            Ok(d) => d.take(limit).read_to_end(&mut out),
            Err(e) => Err(std::io::Error::other(e.to_string())),
        },
        _ => return Ok(body),
    };
    match res {
        Ok(_) if out.len() > MAX_BODY => Err(NetError::TooLarge),
        Ok(_) => Ok(out),
        Err(e) => Err(NetError::Protocol(format!("content decoding failed ({enc}): {e}"))),
    }
}

fn synthetic(url: Url, status: u16, ctype: &str, body: Vec<u8>) -> Response {
    Response {
        url,
        status,
        status_text: "OK".into(),
        headers: vec![("Content-Type".into(), ctype.into())],
        body: Arc::new(body),
        redirected: false,
        from_cache: false,
    }
}

/// `blob:` URLs (`URL.createObjectURL`): bytes and type, shared by the page
/// and its workers.
fn blob_store() -> &'static Mutex<HashMap<String, (Arc<Vec<u8>>, String)>> {
    static STORE: OnceLock<Mutex<HashMap<String, (Arc<Vec<u8>>, String)>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Register bytes under a fresh `blob:<origin>/<uuid>` URL.
pub fn blob_register(origin: &str, bytes: Vec<u8>, ctype: &str) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0) as u64;
    let a = t ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let uuid = format!("{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}", (a >> 32) as u32, (a >> 16) as u16, (a & 0xfff) as u16, 0x8000 | ((n & 0x3fff) as u16), (a.rotate_left(13) ^ n) & 0xffff_ffff_ffff);
    let url = format!("blob:{}/{uuid}", if origin.is_empty() || origin == "null" { "null" } else { origin });
    if let Ok(mut m) = blob_store().lock() {
        m.insert(url.clone(), (Arc::new(bytes), ctype.to_string()));
    }
    url
}

/// The bytes and type behind a `blob:` URL now (the URL's blob entry is
/// captured when it is parsed, so a later revocation doesn't affect a load
/// that started with it).
pub fn blob_snapshot(url: &str) -> Option<(Arc<Vec<u8>>, String)> {
    let key = url.split('#').next().unwrap_or("");
    blob_store().lock().ok().and_then(|m| m.get(key).cloned())
}

/// blob: URLs created by each page thread (tab or frame), revoked when the tab
/// or frame is closed.
fn blob_owners() -> &'static Mutex<HashMap<u64, Vec<String>>> {
    static O: OnceLock<Mutex<HashMap<u64, Vec<String>>>> = OnceLock::new();
    O.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn blob_set_owner(url: &str, owner: u64) {
    if let Ok(mut m) = blob_owners().lock() {
        m.entry(owner).or_default().push(url.to_string());
    }
}

/// Revoke every blob: URL a document created.
pub fn blob_revoke_owner(owner: u64) {
    let urls = blob_owners().lock().ok().and_then(|mut m| m.remove(&owner)).unwrap_or_default();
    if urls.is_empty() {
        return;
    }
    if let Ok(mut m) = blob_store().lock() {
        for u in urls {
            m.remove(&u);
        }
    }
}

pub fn blob_revoke(url: &str) {
    if let Ok(mut m) = blob_store().lock() {
        m.remove(url);
    }
}

fn blob_url(req: &Request) -> Result<Response, NetError> {
    let url = &req.url;
    if req.method != "GET" {
        return Err(NetError::Blocked(format!("{} {url}", req.method)));
    }
    let key = url.as_str().split('#').next().unwrap_or("");
    let hit = blob_store().lock().ok().and_then(|m| m.get(key).cloned());
    let Some((body, ctype)) = hit else { return Err(NetError::InvalidUrl(format!("{url} (revoked or unknown blob)"))) };
    // A type that isn't a valid MIME type is served as no type.
    let ctype = if ctype.is_empty() || parse_mime(&ctype).is_some() { ctype } else { String::new() };
    let range = req.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("range")).map(|(_, v)| v.clone());
    let Some(range) = range else {
        return Ok(Response {
            url: url.clone(),
            status: 200,
            status_text: "OK".into(),
            headers: vec![("Content-Type".into(), ctype), ("Content-Length".into(), body.len().to_string())],
            body,
            redirected: false,
            from_cache: false,
        });
    };
    // A single byte range (whitespace allowed), as the Fetch Standard parses it.
    let bad = || NetError::Blocked(format!("bad range {range:?}"));
    let v = range.trim_matches(|c| c == ' ' || c == '\t');
    let v = v.strip_prefix("bytes").ok_or_else(bad)?.trim_start_matches([' ', '\t']);
    let v = v.strip_prefix('=').ok_or_else(bad)?.trim_matches([' ', '\t']);
    let (a, b) = v.split_once('-').ok_or_else(bad)?;
    let (a, b) = (a.trim_matches([' ', '\t']), b.trim_matches([' ', '\t']));
    let num = |s: &str| -> Result<Option<u64>, NetError> {
        if s.is_empty() {
            Ok(None)
        } else if s.bytes().all(|c| c.is_ascii_digit()) {
            s.parse::<u64>().map(Some).map_err(|_| bad())
        } else {
            Err(bad())
        }
    };
    let (start, end) = (num(a)?, num(b)?);
    let full = body.len() as u64;
    let (start, end) = match (start, end) {
        (None, None) => return Err(bad()),
        (Some(s), Some(e)) if s > e => return Err(bad()),
        (None, Some(e)) => {
            let s = full.saturating_sub(e);
            (s, if e == 0 { return Err(bad()) } else { full.saturating_sub(1).max(s) })
        }
        (Some(s), e) => {
            if s >= full {
                return Err(bad());
            }
            (s, e.map(|e| e.min(full - 1)).unwrap_or(full - 1))
        }
    };
    let slice = body[start as usize..=(end as usize).min(body.len().saturating_sub(1))].to_vec();
    Ok(Response {
        url: url.clone(),
        status: 206,
        status_text: "Partial Content".into(),
        headers: vec![
            ("Content-Type".into(), ctype),
            ("Content-Length".into(), slice.len().to_string()),
            ("Content-Range".into(), format!("bytes {start}-{end}/{full}")),
        ],
        body: Arc::new(slice),
        redirected: false,
        from_cache: false,
    })
}

/// The Fetch Standard's data: URL processor.
fn data_url(url: &Url) -> Result<Response, NetError> {
    let bad = || NetError::InvalidUrl("data url".into());
    let full = url.as_str();
    let input = match full.find('#') {
        Some(i) => &full[..i],
        None => full,
    };
    let rest = input.get(5..).ok_or_else(bad)?;
    let (meta, data) = rest.split_once(',').ok_or_else(bad)?;
    let mut mime = meta.trim_matches(|c: char| c.is_ascii_whitespace()).to_string();
    let mut body = percent_decode(data.as_bytes());
    // ";" + spaces + "base64" at the end (ASCII case-insensitive).
    let lower = mime.to_ascii_lowercase();
    if lower.ends_with("base64") {
        let head = mime[..mime.len() - 6].trim_end_matches(' ');
        if let Some(h) = head.strip_suffix(';') {
            mime = h.to_string();
            body = forgiving_base64(&body).ok_or_else(bad)?;
        }
    }
    if mime.starts_with(';') {
        mime = format!("text/plain{mime}");
    }
    let ctype = parse_mime(&mime).map(|m| serialize_mime(&m)).unwrap_or_else(|| "text/plain;charset=US-ASCII".into());
    Ok(synthetic(url.clone(), 200, &ctype, body))
}

/// Infra's forgiving-base64 decode.
pub fn forgiving_base64(data: &[u8]) -> Option<Vec<u8>> {
    let mut d: Vec<u8> = data.iter().copied().filter(|c| !matches!(c, b' ' | b'\t' | b'\n' | b'\x0c' | b'\r')).collect();
    if d.len() % 4 == 0 {
        if d.ends_with(b"==") {
            d.truncate(d.len() - 2);
        } else if d.ends_with(b"=") {
            d.truncate(d.len() - 1);
        }
    }
    if d.len() % 4 == 1 || d.iter().any(|&c| !(c.is_ascii_alphanumeric() || c == b'+' || c == b'/')) {
        return None;
    }
    let mut out = Vec::with_capacity(d.len() * 3 / 4);
    let (mut buf, mut bits) = (0u32, 0);
    for &c in &d {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            _ => 63,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// A parsed MIME type: (type, subtype, parameters), lowercase names (MIME Sniffing).
pub struct Mime {
    pub ty: String,
    pub sub: String,
    pub params: Vec<(String, String)>,
}

fn http_token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// MIME Sniffing's "parse a MIME type".
pub fn parse_mime(input: &str) -> Option<Mime> {
    let ws = |c: char| matches!(c, '\t' | '\n' | '\r' | ' ');
    let s: Vec<char> = input.trim_matches(ws).chars().collect();
    let mut pos = 0;
    while pos < s.len() && s[pos] != '/' {
        pos += 1;
    }
    let ty: String = s[..pos].iter().collect();
    if !http_token(&ty) || pos >= s.len() {
        return None;
    }
    pos += 1;
    let st = pos;
    while pos < s.len() && s[pos] != ';' {
        pos += 1;
    }
    let sub: String = s[st..pos].iter().collect::<String>().trim_end_matches(ws).to_string();
    if !http_token(&sub) {
        return None;
    }
    let mut m = Mime { ty: ty.to_ascii_lowercase(), sub: sub.to_ascii_lowercase(), params: Vec::new() };
    while pos < s.len() {
        pos += 1;
        while pos < s.len() && ws(s[pos]) {
            pos += 1;
        }
        let ns = pos;
        while pos < s.len() && s[pos] != ';' && s[pos] != '=' {
            pos += 1;
        }
        let name: String = s[ns..pos].iter().collect::<String>().to_ascii_lowercase();
        if pos < s.len() {
            if s[pos] == ';' {
                continue;
            }
            pos += 1;
        }
        if pos >= s.len() {
            break;
        }
        let value: String;
        if s[pos] == '"' {
            let mut v = String::new();
            pos += 1;
            loop {
                while pos < s.len() && s[pos] != '"' && s[pos] != '\\' {
                    v.push(s[pos]);
                    pos += 1;
                }
                if pos >= s.len() {
                    break;
                }
                let c = s[pos];
                pos += 1;
                if c == '\\' {
                    if pos >= s.len() {
                        v.push('\\');
                        break;
                    }
                    v.push(s[pos]);
                    pos += 1;
                } else {
                    break;
                }
            }
            while pos < s.len() && s[pos] != ';' {
                pos += 1;
            }
            value = v;
        } else {
            let vs = pos;
            while pos < s.len() && s[pos] != ';' {
                pos += 1;
            }
            value = s[vs..pos].iter().collect::<String>().trim_end_matches(ws).to_string();
            if value.is_empty() {
                continue;
            }
        }
        let qs_ok = value.chars().all(|c| c == '\t' || (' '..='~').contains(&c) || ('\u{80}'..='\u{ff}').contains(&c));
        if http_token(&name) && qs_ok && !m.params.iter().any(|(k, _)| *k == name) {
            m.params.push((name, value));
        }
    }
    Some(m)
}

pub fn serialize_mime(m: &Mime) -> String {
    let mut out = format!("{}/{}", m.ty, m.sub);
    for (k, v) in &m.params {
        out.push(';');
        out.push_str(k);
        out.push('=');
        if http_token(v) {
            out.push_str(v);
        } else {
            out.push('"');
            for c in v.chars() {
                if c == '"' || c == '\\' {
                    out.push('\\');
                }
                out.push(c);
            }
            out.push('"');
        }
    }
    out
}

fn file_url(req: &Request) -> Result<Response, NetError> {
    // Security: web pages may never read local files. Only the user (no
    // initiator) or other file: pages may load file: URLs.
    if let Some(init) = &req.initiator {
        if init.scheme() != "file" {
            return Err(NetError::Blocked("web page attempted to access a local file".into()));
        }
    }
    let path = req.url.to_file_path().map_err(|_| NetError::InvalidUrl(req.url.to_string()))?;
    if path.is_dir() {
        let mut html = format!(
            "<!doctype html><title>Index of {0}</title><h1>Index of {0}</h1><ul>",
            html_escape(&path.display().to_string())
        );
        if let Ok(rd) = std::fs::read_dir(&path) {
            let mut names: Vec<(String, bool)> = rd
                .flatten()
                .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path().is_dir()))
                .collect();
            names.sort();
            html.push_str("<li><a href=\"../\">../</a></li>");
            for (n, d) in names {
                let slash = if d { "/" } else { "" };
                html.push_str(&format!(
                    "<li><a href=\"{0}{1}\">{0}{1}</a></li>",
                    html_escape(&n),
                    slash
                ));
            }
        }
        html.push_str("</ul>");
        return Ok(synthetic(req.url.clone(), 200, "text/html", html.into_bytes()));
    }
    let meta = std::fs::metadata(&path).map_err(|e| NetError::Io(format!("{}: {e}", path.display())))?;
    if meta.len() as usize > MAX_BODY {
        return Err(NetError::TooLarge);
    }
    let body = std::fs::read(&path).map_err(|e| NetError::Io(format!("{}: {e}", path.display())))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let ctype = mime_from_ext(&ext);
    Ok(synthetic(req.url.clone(), 200, ctype, body))
}

pub fn mime_from_ext(ext: &str) -> &'static str {
    match ext {
        "html" | "htm" | "xhtml" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" | "md" | "rs" | "toml" | "py" | "c" | "h" | "sh" | "log" => "text/plain",
        "xml" => "text/xml",
        _ => "application/octet-stream",
    }
}

pub fn html_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '&' => o.push_str("&amp;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

pub fn percent_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' && i + 2 < s.len() {
            let h = |c: u8| (c as char).to_digit(16);
            if let (Some(a), Some(b)) = (h(s[i + 1]), h(s[i + 2])) {
                out.push((a * 16 + b) as u8);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn base64_decode(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for &c in data {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' | b'\x0c' => continue,
            _ => return None,
        } as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Worker pool
// ---------------------------------------------------------------------------

type Job = Box<dyn FnOnce() + Send + 'static>;

struct Queue {
    high: VecDeque<Job>,
    low: VecDeque<Job>,
}

struct Pool {
    q: Mutex<Queue>,
    cv: Condvar,
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<&'static Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let p: &'static Pool = Box::leak(Box::new(Pool {
            q: Mutex::new(Queue { high: VecDeque::new(), low: VecDeque::new() }),
            cv: Condvar::new(),
        }));
        let workers = 20;
        for i in 0..workers {
            std::thread::Builder::new()
                .name(format!("heron-net-{i}"))
                .stack_size(2 * 1024 * 1024)
                .spawn(move || {
                    // Bodies and decoded images are allocated here and freed
                    // later on other threads (a closed tab's): the allocator
                    // returns those pages only when this thread collects, so an
                    // idle worker does that every few seconds (cheap when there's nothing).
                    loop {
                        let job = {
                            let mut q = p.q.lock().unwrap();
                            loop {
                                if let Some(j) = q.high.pop_front() {
                                    break Some(j);
                                }
                                if let Some(j) = q.low.pop_front() {
                                    break Some(j);
                                }
                                let (g, t) = p.cv.wait_timeout(q, std::time::Duration::from_secs(6)).unwrap();
                                q = g;
                                if t.timed_out() {
                                    break None;
                                }
                            }
                        };
                        match job {
                            Some(job) => {
                                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                            }
                            None => crate::collect_service_thread("net"),
                        }
                    }
                })
                .expect("spawn net worker");
        }
        p
    })
}

/// Run a closure on the shared network/decoding worker pool.
pub fn spawn_job(high_priority: bool, job: impl FnOnce() + Send + 'static) {
    let p = pool();
    {
        let mut q = p.q.lock().unwrap();
        if high_priority {
            q.high.push_back(Box::new(job));
        } else {
            q.low.push_back(Box::new(job));
        }
    }
    p.cv.notify_one();
}

/// One finished request, for DevTools' Network panel.
#[derive(Clone, Debug)]
pub struct NetLogEntry {
    pub url: String,
    pub method: String,
    pub kind: &'static str,
    pub status: u16,
    pub size: usize,
    pub mime: String,
    /// Start (ms since the Unix epoch) and duration.
    pub start: f64,
    pub ms: f64,
    pub error: Option<String>,
    pub cached: bool,
}

fn net_log_map() -> &'static Mutex<HashMap<u64, Vec<NetLogEntry>>> {
    static L: OnceLock<Mutex<HashMap<u64, Vec<NetLogEntry>>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The requests a tab made since its page started.
pub fn net_log(tab: u64) -> Vec<NetLogEntry> {
    net_log_map().lock().unwrap().get(&tab).cloned().unwrap_or_default()
}

pub fn clear_net_log(tab: u64) {
    net_log_map().lock().unwrap().remove(&tab);
}

fn kind_name(req: &Request) -> &'static str {
    match req.kind {
        RequestKind::Document if req.frame => "iframe",
        RequestKind::Document => "document",
        RequestKind::Stylesheet => "stylesheet",
        RequestKind::Script => "script",
        RequestKind::Image => "image",
        RequestKind::Font => "font",
        RequestKind::Fetch => "fetch",
        RequestKind::Favicon => "favicon",
    }
}

fn log_request(tab: u64, req: &Request, r: &Result<Response, NetError>, start: f64, ms: f64) {
    let (status, size, mime, error, cached) = match r {
        Ok(resp) => (resp.status, resp.body.len(), resp.content_type(), None, resp.from_cache),
        Err(e) => (0, 0, String::new(), Some(e.to_string()), false),
    };
    let mut m = net_log_map().lock().unwrap();
    let v = m.entry(tab).or_default();
    if v.len() < 5000 {
        v.push(NetLogEntry { url: req.url.to_string(), method: req.method.clone(), kind: kind_name(req), status, size, mime, start, ms, error, cached });
    }
}

/// Fetch asynchronously; `done` runs on a worker thread.
/// Wall-clock seconds (mod 1000), for debug timelines.
pub fn wall_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() % 1000.0).unwrap_or(0.0)
}

pub fn fetch_async(req: Request, done: impl FnOnce(Result<Response, NetError>) + Send + 'static) {
    let high = matches!(req.kind, RequestKind::Document | RequestKind::Stylesheet | RequestKind::Script);
    let ctx = current_context();
    spawn_job(high, move || {
        let tab = ctx.as_ref().map(|c| c.tab);
        let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0);
        let logged = tab.map(|_| req.clone());
        REQ_CTX.with(|c| *c.borrow_mut() = ctx);
        static DBG: OnceLock<bool> = OnceLock::new();
        let dbg = *DBG.get_or_init(|| std::env::var_os("OHB_DEBUG_NET").is_some());
        let t = std::time::Instant::now();
        let u = if dbg { req.url.to_string() } else { String::new() };
        if let (Ok(pat), Some(b)) = (std::env::var("OHB_DEBUG_BODY"), req.body.as_ref()) {
            if req.url.as_str().contains(&pat) {
                eprintln!("[net] body of {} {}: {}", req.method, req.url, String::from_utf8_lossy(&b[..b.len().min(4000)]));
            }
        }
        let r = client().fetch(req);
        if let (Some(tab), Some(lr)) = (tab, logged.as_ref()) {
            log_request(tab, lr, &r, wall, t.elapsed().as_secs_f64() * 1000.0);
        }
        if dbg {
            match &r {
                Ok(resp) => eprintln!("[net] @{:.3} {} {} {}B {:?}", wall_secs(), resp.status, u, resp.body.len(), t.elapsed()),
                Err(e) => eprintln!("[net] ERR {u}: {e} {:?}", t.elapsed()),
            }
        }
        // Never let a panicking consumer (e.g. a decoder) leave the page waiting.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || done(r)));
    });
}

/// Script fetch (fetch(), XMLHttpRequest) through the Fetch Standard's modes
/// and CORS rules, asynchronously; `done` runs on a worker thread.
pub fn fetch_cors_async(req: Request, opts: cors::Opts, done: impl FnOnce(Result<cors::Outcome, NetError>) + Send + 'static) {
    let ctx = current_context();
    spawn_job(false, move || {
        let tab = ctx.as_ref().map(|c| c.tab);
        let wall = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0);
        let logged = tab.map(|_| req.clone());
        REQ_CTX.with(|c| *c.borrow_mut() = ctx);
        let t = std::time::Instant::now();
        let r = cors::fetch(req, &opts);
        if let (Some(tab), Some(lr)) = (tab, logged.as_ref()) {
            let lres = match &r {
                Ok(o) => Ok(o.resp.clone()),
                Err(e) => Err(NetError::Blocked(e.to_string())),
            };
            log_request(tab, lr, &lres, wall, t.elapsed().as_secs_f64() * 1000.0);
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || done(r)));
    });
}

/// Same-origin check helper.
pub fn same_origin(a: &Url, b: &Url) -> bool {
    a.origin() == b.origin()
}

/// Approximate "site" (registrable domain) used for third-party decisions.
pub fn site_of(url: &Url) -> String {
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
        return host;
    }
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() <= 2 {
        return host;
    }
    let n = labels.len();
    // Heuristic for two-level public suffixes like co.uk, com.au, github.io.
    let sl = labels[n - 2];
    let two_level = matches!(sl, "co" | "com" | "org" | "net" | "gov" | "ac" | "edu" | "ne" | "or" | "go")
        && labels[n - 1].len() == 2
        || matches!(
            format!("{}.{}", sl, labels[n - 1]).as_str(),
            "github.io" | "gitlab.io" | "netlify.app" | "vercel.app" | "pages.dev" | "herokuapp.com" | "blogspot.com" | "appspot.com"
        );
    if two_level && n >= 3 {
        labels[n - 3..].join(".")
    } else {
        labels[n - 2..].join(".")
    }
}
