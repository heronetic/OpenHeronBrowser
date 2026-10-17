// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronPage — one browsing context (a tab). Owns the document, styles,
//! layout, resources and history, and runs on its own thread.

pub mod avif;
pub mod canvas;
pub mod devtools;
pub mod frames;
pub mod internal;
pub mod pdf;
pub mod resources;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use url::Url;

use crate::css::{self, style::Cursor, Declaration, RuleIndex, Stylesheet, Styles};
use crate::dom::{Document, NodeData, NodeId, DOCUMENT};
use crate::layout::{self, LayoutTree};
use crate::net::{self, NetError, Request, RequestKind, Response};
use crate::paint::{builder, DisplayList, HitRegion, ImageData};
use crate::text::FontContext;
use resources::{ImgState, ResView, Resources};

// ------------------------------------------------------------------ messages

#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Enter,
    Tab,
    Escape,
    PageUp,
    PageDown,
    Space,
    Char(String),
    Other(String),
}

/// Fetch's "should response to request be blocked due to its MIME type?" for
/// classic scripts (audio, image, video and CSV never run), plus nosniff.
fn script_mime_ok(r: &Response) -> bool {
    let ess = r.content_type();
    if ess.starts_with("audio/") || ess.starts_with("image/") || ess.starts_with("video/") || ess == "text/csv" {
        return false;
    }
    let nosniff = r.header("x-content-type-options").map(|v| v.split(',').next().unwrap_or("").trim().eq_ignore_ascii_case("nosniff")).unwrap_or(false);
    if nosniff {
        const JS: [&str; 16] = [
            "application/ecmascript", "application/javascript", "application/x-ecmascript", "application/x-javascript", "text/ecmascript", "text/javascript", "text/javascript1.0", "text/javascript1.1",
            "text/javascript1.2", "text/javascript1.3", "text/javascript1.4", "text/javascript1.5", "text/jscript", "text/livescript", "text/x-ecmascript", "text/x-javascript",
        ];
        return JS.contains(&ess.as_str());
    }
    true
}

/// The JavaScript MIME type essences (HTML "JavaScript MIME type essence match").
pub const JS_MIME_ESSENCES: [&str; 16] = [
    "application/ecmascript", "application/javascript", "application/x-ecmascript", "application/x-javascript", "text/ecmascript", "text/javascript", "text/javascript1.0", "text/javascript1.1",
    "text/javascript1.2", "text/javascript1.3", "text/javascript1.4", "text/javascript1.5", "text/jscript", "text/livescript", "text/x-ecmascript", "text/x-javascript",
];

/// A <script>'s type per "prepare the script element": "classic", "module",
/// "importmap", or None (a data block).
pub fn script_type_kind(type_attr: Option<&str>, language: Option<&str>) -> Option<&'static str> {
    let ws = |c: char| matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' ');
    let ty = match (type_attr, language) {
        (Some(""), _) | (None, None) | (None, Some("")) => return Some("classic"),
        (None, Some(l)) => format!("text/{l}"),
        (Some(t), _) => t.trim_matches(ws).to_string(),
    };
    let l = ty.to_ascii_lowercase();
    if JS_MIME_ESSENCES.contains(&l.as_str()) {
        Some("classic")
    } else if l == "module" {
        Some("module")
    } else if l == "importmap" {
        Some("importmap")
    } else {
        None
    }
}

/// The head of a streamed script fetch response.
#[derive(Debug)]
pub struct FetchHead {
    pub status: u16,
    pub status_text: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub rtype: &'static str,
    pub redirected: bool,
}

/// A script fetch in flight: body bytes received so far (for a streamed read),
/// whether script waits for more, and the abort switch.
#[derive(Default)]
pub struct FetchStream {
    pub buf: Vec<u8>,
    pub want: bool,
    pub streamed: bool,
    pub done: Option<bool>,
    pub discard: bool,
    pub cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// A form's entry list as script built it (HTML "constructing the entry list"),
/// with a multipart/form-data body when the form asks for one.
pub struct FormEntries {
    pub pairs: Vec<(String, String)>,
    pub multipart: Option<(Vec<u8>, String)>,
    /// The action URL as script parsed it (base and encoding of the form's document).
    pub action: Option<Url>,
}

#[derive(Clone, Debug)]
pub struct NavRequest {
    pub url: String,
    pub method: String,
    pub body: Option<Vec<u8>>,
    pub content_type: Option<String>,
    pub referrer: Option<Url>,
    /// History handling: true = push a new entry.
    pub push: bool,
    pub restore_scroll: Option<(f32, f32)>,
}

impl NavRequest {
    pub fn get(url: &str) -> NavRequest {
        NavRequest { url: url.to_string(), method: "GET".into(), body: None, content_type: None, referrer: None, push: true, restore_scroll: None }
    }
}

pub enum NetMsg {
    Document(NavRequest, Result<Response, NetError>),
    Stylesheet(usize, Url, Result<Response, NetError>),
    Script(usize, Result<Response, NetError>),
    Image(String, Option<Arc<ImageData>>),
    /// A resource requested as an image loaded fine but is a document (HTML/text/XML):
    /// `<object>` shows it as a nested browsing context instead of fallback content.
    ImageDoc(String),
    /// An external SVG document for `<use>` references.
    SvgDoc(String, Option<Arc<resources::SvgDoc>>),
    /// A `<link rel=preload>` finished (node, success).
    Preload(NodeId, bool),
    Font(String, u16, crate::css::style::FontStyle, Option<Vec<u8>>),
    Fetch(u64, Result<crate::net::cors::Outcome, NetError>),
    /// A script fetch's response head (before its body), and body chunks.
    FetchHead(u64, Box<FetchHead>),
    FetchChunk(u64, Vec<u8>),
    Favicon(Option<Arc<ImageData>>),
    Worker(u32, crate::js::worker::WorkerEvent),
    /// An iframe's document (frame node, frame key).
    Frame(NodeId, String, Result<Response, NetError>),
    /// A style sheet linked from an iframe's document (frame node, frame key, URL).
    FrameSheet(NodeId, String, String, Result<Response, NetError>),
}

pub enum PageMsg {
    Navigate(NavRequest),
    Reload,
    Stop,
    Back,
    Forward,
    Resize { w: f32, h: f32, dpr: f32 },
    Zoom(f32),
    MouseMove { x: f32, y: f32 },
    /// A wheel the window already scrolled for: passive listeners see it.
    WheelNotify { x: f32, y: f32, dx: f32, dy: f32 },
    /// Raw pointer motion while the pointer is locked (Pointer Lock API).
    MouseDelta { dx: f32, dy: f32 },
    /// The browser released the pointer lock (Esc, focus or tab change).
    PointerLockLost,
    MouseDown { x: f32, y: f32, button: u8, mods: Mods },
    MouseUp { x: f32, y: f32, button: u8, mods: Mods },
    MouseLeave,
    Wheel { x: f32, y: f32, dx: f32, dy: f32 },
    Scrolled { x: f32, y: f32 },
    Key { key: Key, mods: Mods },
    /// A key pressed in the UI (its release follows as KeyUp).
    KeyDown { key: Key, mods: Mods },
    KeyUp { key: Key, mods: Mods },
    /// The window lost focus: held keys are released (their keyup).
    ReleaseKeys,
    Text(String),
    Find(String, bool),
    FindClose,
    SelectAllText,
    CopySelection,
    /// Evaluate script in the page (headless `--eval`); answers with `EvalResult`.
    Eval(String),
    /// DevTools traffic (requests to an inspected tab, answers and events
    /// to a DevTools page).
    Devtools(devtools::DevMsg),
    /// From one of this page's live frames.
    Child(u64, frames::ChildMsg),
    /// `postMessage` from the page embedding this (frame) page.
    /// `source`: the sending sibling frame's name (None: the parent).
    PostMessage { data: crate::js::host::HostValue, origin: String, target_origin: String, ports: Vec<u64>, source: Option<String> },
    /// A message on one of this page's MessagePorts.
    PortMessage { pid: u64, data: crate::js::host::HostValue, ports: Vec<u64> },
    /// A WebSocket connection's event (script's id).
    Ws(u64, crate::net::ws::WsEvent),
    Net(u64, NetMsg),
    /// The user's answer to a `PageEvent::Permission` (with the clipboard
    /// text for `clipboard-read`).
    PermissionReply { id: u64, allow: bool, data: Option<String> },
    /// Paste (Cmd/Ctrl+V) into the focused editable: a `paste` event, then
    /// the text is inserted unless script cancels it.
    Paste(String),
    /// Stop holding the load event for frames that are still loading
    /// (sent a while after this navigation's own resources finished).
    FramesTimeout(u64),
    Shutdown,
}

pub struct FrameData {
    pub list: DisplayList,
    pub doc_w: f32,
    pub doc_h: f32,
    pub focus_editable: bool,
    pub has_scrollers: bool,
    /// Script listens for wheel events (non-passively): the UI routes the
    /// wheel through the page.
    pub wheel_listeners: bool,
    /// Only passive wheel listeners: the UI scrolls and tells the page.
    pub wheel_passive: bool,
    /// Inner scroll containers' visible rects (document coordinates, or
    /// viewport ones when fixed) and frames: wheels over them go to the page.
    pub scroll_regions: Vec<(crate::paint::Rect, bool)>,
    pub has_frames: bool,
    /// The viewport can be scrolled by the user (root `overflow` isn't hidden/clip).
    pub user_scroll: bool,
    /// Where text input goes (the focused editable's box, document
    /// coordinates): the window places IME candidate popups there.
    pub ime_rect: Option<[f32; 4]>,
}

pub enum PageEvent {
    Frame(Arc<FrameData>),
    Title(String),
    Url(String, bool),
    Loading(bool, f32),
    Cursor(Cursor),
    Status(String),
    OpenTab(String),
    HistoryState(bool, bool),
    ScrollTo(f32, f32),
    Favicon(Option<Arc<ImageData>>),
    Alert(String),
    FindResult(usize, usize),
    Crashed(String),
    Copy(String),
    EvalResult(String),
    /// Script work is queued (or not): the page is still settling.
    Busy(bool),
    /// A DevTools page asked to be closed.
    DevtoolsClose,
    /// Where the tab's time and memory went (the performance overlay).
    Perf(PerfSample),
    /// Script locked (true) or unlocked the pointer.
    PointerLock(bool),
    /// A site asks for a permission (`clipboard-read`); the answer comes
    /// back as `PageMsg::PermissionReply` with the same id.
    Permission { id: u64, perm: String, site: String },
    /// A response is a file to download: the browser asks before saving it
    /// (the body waits in `PENDING_DOWNLOADS` under `id`).
    Download { id: u64, name: String, size: usize, site: String },
}

/// Downloads waiting for the user's answer: id -> (file name, body).
pub static PENDING_DOWNLOADS: std::sync::LazyLock<std::sync::Mutex<HashMap<u64, (String, Arc<Vec<u8>>)>>> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
static DOWNLOAD_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Save a pending download to the Downloads folder (the user said yes).
/// Returns the saved path.
pub fn save_download(id: u64) -> Option<std::path::PathBuf> {
    let (name, body) = PENDING_DOWNLOADS.lock().unwrap().remove(&id)?;
    let dir = downloads_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut path = dir.join(&name);
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("({n}) {name}"));
        n += 1;
    }
    std::fs::write(&path, &*body).ok()?;
    Some(path)
}

pub fn drop_download(id: u64) {
    PENDING_DOWNLOADS.lock().unwrap().remove(&id);
}

/// Whether the performance overlay is showing (pages then report samples).
pub static HUD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The top-level tab a page thread belongs to (its own id for a tab; frame
/// threads inherit their parent's).
thread_local!(pub(crate) static ROOT_TAB: std::cell::Cell<u64> = const { std::cell::Cell::new(0) });

/// Live heap bytes per page thread (tab or frame): id -> (top tab id, bytes).
static THREAD_MEM: std::sync::LazyLock<Mutex<HashMap<u64, (u64, usize)>>> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Removes a page thread's entry when it exits.
struct ThreadMemGuard(u64);
impl Drop for ThreadMemGuard {
    fn drop(&mut self) {
        if let Ok(mut m) = THREAD_MEM.lock() {
            m.remove(&self.0);
        }
        // The page thread is gone: so are its document's per-tab records.
        net::clear_net_log(self.0);
        net::blob_revoke_owner(self.0);
        crate::ext::reset_tab(self.0);
    }
}

/// Records the calling page thread's live heap.
fn report_thread_memory(id: u64) {
    let root = ROOT_TAB.with(|r| r.get());
    // Images are decoded on network threads, so their pixels aren't in this
    // thread's heap: count them separately.
    let bytes = crate::heap_in_use() + with_page(|p| p.res.image_bytes()).unwrap_or(0);
    if let Ok(mut m) = THREAD_MEM.lock() {
        m.insert(id, (if root == 0 { id } else { root }, bytes));
    }
}

/// All page threads' reported memory.
pub fn all_tabs_memory() -> usize {
    THREAD_MEM.lock().map(|m| m.values().map(|(_, b)| *b).sum()).unwrap_or(0)
}

/// A tab's memory: the live heaps of its page thread and its frames' threads.
pub fn tab_memory(root: u64) -> usize {
    THREAD_MEM.lock().map(|m| m.values().filter(|(r, _)| *r == root).map(|(_, b)| *b).sum()).unwrap_or(0)
}

/// One performance-overlay sample of a tab's thread.
#[derive(Clone, Debug, Default)]
pub struct PerfSample {
    /// Wall time covered (ms) and the share of it in each phase (ms).
    pub wall: f64,
    pub js: f64,
    pub style: f64,
    pub layout: f64,
    pub paint: f64,
    pub gc: f64,
    /// Frames the page produced.
    pub frames: u32,
    pub js_heap: usize,
    /// Time running WebAssembly (ms, part of `js`) and its linear memory.
    pub wasm: f64,
    pub wasm_mem: usize,
    pub images: usize,
    pub dom_nodes: usize,
    pub boxes: usize,
    /// The tab's memory (its page thread and frames; see `tab_memory`).
    pub tab_mem: usize,
}

thread_local!(static PHASE_NS: std::cell::Cell<[u64; 2]> = const { std::cell::Cell::new([0, 0]) });

fn phase_add(i: usize, d: Duration) {
    PHASE_NS.with(|c| {
        let mut v = c.get();
        v[i] += d.as_nanos() as u64;
        c.set(v);
    });
}

/// Style and layout time (ns) spent on this thread so far.
fn phase_ns() -> [u64; 2] {
    PHASE_NS.with(|c| c.get())
}

pub type UiSink = Arc<dyn Fn(u64, PageEvent) + Send + Sync>;

// ------------------------------------------------------------------ JS hooks

pub enum JsTask {
    RunScripts,
    Dispatch { target: NodeId, ty: String, init: EventInit, then: DefaultAction },
    WindowEvent(String),
    /// Animation and transition events: (type, animationName/propertyName, elapsedTime, pseudoElement).
    AnimEvent { target: NodeId, ty: String, name: String, elapsed: f64, pseudo: String },
    Resize,
    Eval(String),
    /// A DevTools request for the script engine (`__ohb_devtools`).
    Devtools { from: u64, id: u64, method: String, params: String },
    /// A DevTools answer/event for this (DevTools) page's script.
    DevtoolsDeliver(String),
    /// `postMessage` from a live frame (`frame`: its element).
    FrameMessage { frame: NodeId, data: crate::js::host::HostValue, origin: String, target_origin: String, ports: Vec<u64> },
    /// `postMessage` from the parent page.
    ParentMessage { data: crate::js::host::HostValue, origin: String, target_origin: String, ports: Vec<u64>, source: Option<String> },
    PortMessage { pid: u64, data: crate::js::host::HostValue, ports: Vec<u64> },
    /// Call a global function (test automation callbacks).
    CallGlobal(&'static str, Vec<crate::js::host::HostValue>),
    /// A form's submit or reset was requested (button activation, implicit
    /// submission): script runs validation and the events; without script
    /// the form is submitted / reset directly.
    FormAction { form: NodeId, submitter: Option<NodeId>, reset: bool },
    /// A form submission's navigation, run as its own task.
    PlannedNav(Box<FormNav>),
    /// Keyboard input that arrived while a mouse press's focus change was
    /// still queued: handled in order, once focus has moved.
    Replay(PageMsg),
    /// The pointer moved: pointermove and mousemove at the latest position
    /// (moves coalesce while script is busy).
    MouseMove,
}

/// Scopes of registered "coi-serviceworker" scripts (the widely used shim
/// whose only job is adding COOP/COEP headers to same-origin responses,
/// making the page cross-origin isolated). Service workers aren't run, so
/// that header injection is emulated: documents in these scopes are isolated.
static COI_SCOPES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn coi_register(scope: &str) {
    let mut v = COI_SCOPES.lock().unwrap();
    if !v.iter().any(|s| s == scope) {
        v.push(scope.to_string());
    }
}

pub fn coi_scope_of(url: &str) -> Option<String> {
    COI_SCOPES.lock().unwrap().iter().find(|s| url.starts_with(s.as_str())).cloned()
}

fn thrash_debug() -> bool {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("OHB_THRASH").is_some());
    *ON
}

/// `KeyboardEvent.code` for a `KeyboardEvent.key` value (US layout).
fn key_code(key: &str) -> String {
    let mut ch = key.chars();
    match (ch.next(), ch.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => format!("Key{}", c.to_ascii_uppercase()),
        (Some(c), None) if c.is_ascii_digit() => format!("Digit{c}"),
        (Some(' '), None) => "Space".into(),
        (Some(c), None) => match c {
            '-' | '_' => "Minus", '=' | '+' => "Equal", '[' | '{' => "BracketLeft", ']' | '}' => "BracketRight", '\\' | '|' => "Backslash",
            ';' | ':' => "Semicolon", '\'' | '"' => "Quote", ',' | '<' => "Comma", '.' | '>' => "Period", '/' | '?' => "Slash", '`' | '~' => "Backquote",
            '!' => "Digit1", '@' => "Digit2", '#' => "Digit3", '$' => "Digit4", '%' => "Digit5", '^' => "Digit6", '&' => "Digit7", '*' => "Digit8", '(' => "Digit9", ')' => "Digit0",
            _ => "",
        }
        .into(),
        _ => key.to_string(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct EventInit {
    pub x: f32,
    pub y: f32,
    pub button: u8,
    pub mods: Mods,
    pub key: String,
    pub code: String,
    pub bubbles: bool,
    pub cancelable: bool,
    /// Wheel deltas (px).
    pub dx: f32,
    pub dy: f32,
    /// A key held down (auto-repeat).
    pub repeat: bool,
    /// UIEvent.detail: the click count for mouse button events.
    pub detail: i32,
    /// MouseEvent.buttons at the time of the input (None: when dispatched).
    pub buttons: Option<u16>,
    /// movementX / movementY.
    pub mx: f32,
    pub my: f32,
}

#[derive(Clone, Debug)]
/// A planned form navigation (see `Page::submit_form`).
pub struct FormNav {
    pub form: NodeId,
    req: NavRequest,
    target_frame: Option<Option<NodeId>>,
    target_blank: bool,
    target_lc: String,
    method: String,
    home: Option<NodeId>,
}

pub enum DefaultAction {
    None,
    /// A keyboard activation (Space released on a button): its click event.
    ClickEvent { target: NodeId, mods: Mods },
    Click { target: NodeId, mods: Mods, button: u8 },
    /// Checkbox/radio click: the state was toggled before dispatch (pre-activation);
    /// `prev` restores it if the click is canceled.
    Activate { target: NodeId, prev: Vec<(NodeId, bool)> },
    Submit { form: NodeId, submitter: Option<NodeId> },
    Key { key: Key, mods: Mods },
    Text(String),
    /// A wheel event nobody canceled: scroll what's under the pointer.
    Wheel { x: f32, y: f32, dx: f32, dy: f32 },
    /// mousedown's default: focus the clicked focusable area (or nothing).
    Focus { node: Option<NodeId>, x: f32 },
}

pub struct ScriptSlot {
    pub node: NodeId,
    pub text: Option<String>,
    pub url: Option<Url>,
    pub module: bool,
    /// `<script type="importmap">`
    pub importmap: bool,
    pub asynchronous: bool,
    pub deferred: bool,
    /// A script-inserted external script whose `async` was set to false: it
    /// runs as soon as possible but in insertion order with its kind (HTML
    /// "list of scripts that will execute in order as soon as possible").
    pub in_order: bool,
    pub done: bool,
    pub failed: bool,
}

impl SheetSlot {
    /// Loaded imports in cascade order (an import's own imports first).
    fn imports_in_order(&self) -> Vec<Rc<Stylesheet>> {
        let lead = self.lead.clone();
        let mut v: Vec<&(f64, u32, Option<Rc<Stylesheet>>, Option<String>)> = self.imports.iter().filter(|i| i.2.is_some()).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        lead.into_iter().chain(v.into_iter().filter_map(|i| i.2.clone())).collect()
    }
}

struct SheetSlot {
    sheet: Option<Rc<Stylesheet>>,
    /// `@import`ed sheets (nested ones too): (cascade order key, nesting
    /// depth, sheet once loaded).
    imports: Vec<(f64, u32, Option<Rc<Stylesheet>>, Option<String>)>,
    /// Imports still loading, whether one (or the sheet itself) failed, and
    /// whether the owner's `load`/`error` event was fired.
    imp_pending: u32,
    failed: bool,
    load_fired: bool,
    /// Layers the sheet declares ahead of its imports (see leading_layers).
    lead: Option<Rc<Stylesheet>>,
    pending: bool,
    /// The owning `<link>`/`<style>`; its `media`, `rel`, `disabled` and
    /// connectedness are checked on every cascade.
    node: Option<NodeId>,
    /// Render-blocking (its media matched when it was inserted).
    blocking: bool,
    /// A `<link>` sheet's source text (CSSOM reads its rules from it).
    text: Option<Rc<str>>,
    /// Its URL (CSSOM: the base for relative URLs in rules it inserts).
    url: Option<Url>,
}

#[derive(Clone)]
struct HistEntry {
    url: String,
    title: String,
    scroll: (f32, f32),
}

pub struct Page {
    pub tab_id: u64,
    pub ui: UiSink,
    pub tx: Sender<PageMsg>,
    pub doc: Document,
    pub nav_gen: u64,
    pub loading: bool,
    pending: usize,
    total_requests: usize,
    sheets: Vec<SheetSlot>,
    /// Loaded sheet texts by URL (CSSImportRule.styleSheet).
    pub import_texts: HashMap<String, Rc<str>>,
    /// The URL each `<link>` slot was fetched from.
    sheet_urls: HashMap<usize, Url>,
    /// `<style>` sheets whose rules CSSOM changed: the text the engine reads.
    pub style_override: HashMap<NodeId, Rc<str>>,
    /// `<style>` elements whose CSSOM text changed but isn't parsed yet
    /// (parsed once, at the next style pass, however many rules changed).
    pub pending_sheet_text: Vec<NodeId>,
    pub(crate) index: Option<Rc<RuleIndex>>,
    sheets_dirty: bool,
    /// Which sheet owner nodes were in the document at the last style pass.
    sheets_sig: u64,
    /// Rule indexes of the connected shadow trees (rebuilt each style pass).
    shadow_indexes: HashMap<NodeId, Rc<RuleIndex>>,
    /// Shadow-tree rule indexes by their sheets (shared between instances of a component).
    shadow_index_cache: HashMap<Vec<usize>, Rc<RuleIndex>>,
    /// Parsed `<style>` / constructed sheets of shadow trees, by text.
    shadow_sheet_cache: HashMap<Rc<str>, Rc<Stylesheet>>,
    /// Extensions' user style sheet for this page (element hiding, content CSS).
    ext_sheet: Option<Rc<Stylesheet>>,
    /// DevTools state (console, sources, highlight).
    pub dt: devtools::DevState,
    /// Set when this page is a live `<iframe>` inside another page.
    pub frame_parent: Option<frames::FrameParent>,
    /// Live frames: under the mouse, holding the mouse, and taking keys.
    hover_frame: Option<NodeId>,
    frame_capture: Option<NodeId>,
    /// The local frame holding the mouse (pressed in it).
    local_capture: Option<NodeId>,
    frame_focus: Option<NodeId>,
    /// The document's HTML as received (DevTools sources).
    pub doc_source: String,
    pub styles: Option<Styles>,
    /// CSS animations, transitions and Web Animations.
    pub anim: css::anim::AnimEngine,
    /// Timeline time (ms) of the next animation frame, if one is needed.
    pub anim_due: Option<f64>,
    pub layout: Option<LayoutTree>,
    pub hits: Vec<HitRegion>,
    pub fonts: FontContext,
    pub res: Resources,
    declared_fonts: Vec<css::FontFace>,
    /// Families of FontFaces added from script (lowercase), and the ones the
    /// page's styles were found to use (reported to script once each).
    pub js_font_families: HashSet<String>,
    js_fonts_reported: HashSet<String>,
    pub fonts_rescan: bool,
    requested_fonts: HashSet<String>,
    /// CSS Font Loading: per declared face key, 1 loading, 2 loaded, 3 error.
    pub font_status: HashMap<String, u8>,
    /// The document being installed is XML (set from its content type).
    install_xml: bool,
    /// Lazy images being fetched (they don't hold the load event), and the
    /// lazy image URLs of the last style pass.
    lazy_imgs: HashSet<String>,
    lazy_wanted: HashSet<String>,
    /// The open PDF when this is the PDF viewer.
    pdf: Option<pdf::PdfDoc>,
    /// The current document's MIME type (`document.contentType`).
    pub doc_content_type: String,
    /// The document's encoding (document.characterSet).
    pub doc_charset: &'static str,
    /// Encoding of the document being installed (set while loading it).
    pending_charset: Option<&'static str>,
    /// Live frames started before the load event that haven't finished
    /// loading: they hold the load event (frame ids).
    pub(crate) frames_loading: std::collections::HashSet<u64>,
    frames_timer: bool,
    pub win: (f32, f32),
    pub dpr: f32,
    pub zoom: f32,
    pub scroll: (f32, f32),
    pub elem_scroll: HashMap<NodeId, (f32, f32)>,
    pub need_style: bool,
    /// OHB_THRASH debugging: what changed before the pending layout.
    thrash_note: String,
    /// The next restyle must recompute every element (something besides
    /// recorded DOM mutations changed); otherwise only dirty subtrees are.
    pub full_style: bool,
    /// What the last style pass depended on besides the DOM, and the DOM
    /// generation / accounted bumps at that point.
    style_snap: Option<StyleSnap>,
    snap_gen: u64,
    snap_bumps: u64,
    /// `:has()` results of the style passes since the last full one.
    has_log: HashMap<(NodeId, usize), (bool, u32)>,
    /// Elements mutated since the last restyle (for resource discovery).
    style_sites: Vec<NodeId>,
    /// Nodes whose child lists selector matching depended on (`STRUCT_*`).
    struct_log: HashMap<NodeId, u16>,
    /// Query-container content sizes from the last layout (for @container).
    pub container_sizes: HashMap<NodeId, (f32, f32)>,
    pub need_layout: bool,
    /// A document request is in flight: build its script runtime meanwhile.
    pub prewarm_js: bool,
    pub need_paint: bool,
    last_gen: u64,
    pub hover_chain: Vec<NodeId>,
    pub active_chain: Vec<NodeId>,
    history: Vec<HistEntry>,
    hist_idx: usize,
    inline_cache: HashMap<String, Rc<Vec<Declaration>>>,
    /// Styles for a sub-document (iframe contentDocument), keyed by (root, generation).
    subdoc_styles: Option<(NodeId, u64, Rc<css::Styles>)>,
    /// Rendered iframe documents by frame element.
    frame_views: HashMap<NodeId, frames::FrameView>,
    /// Frames navigated by script: (the src key then, the URL now shown).
    frame_nav: HashMap<NodeId, (String, String)>,
    /// Iframes whose src was set (even to the same URL): navigate them again.
    pub(crate) frame_reload: HashSet<NodeId>,
    /// Same-origin frame URLs that redirected cross-origin: (key, final URL).
    frame_redirects: HashMap<NodeId, (String, String)>,
    /// The referrer of the next navigation of a frame (set by script).
    pub frame_referrer: Option<(NodeId, Url)>,
    /// A form POST going to a local frame: (frame, body, content type).
    pub frame_post: Option<(NodeId, Vec<u8>, String)>,
    /// Local frames whose document is loading (until script reports its load).
    local_loading: HashSet<NodeId>,
    pub caret: usize,
    /// The other end of the selection in the focused text field (the caret is one end).
    pub field_anchor: Option<usize>,
    /// An open `<select>` dropdown (its options and where it is drawn).
    pub select_popup: Option<crate::paint::builder::SelectPopupPaint>,
    /// The option node of each row of the open dropdown (None: group heading).
    select_popup_opts: Vec<Option<NodeId>>,
    /// The press that closed a dropdown by clicking its select: its click doesn't reopen it.
    select_popup_suppress: bool,
    /// The control the caret position belongs to (kept across blur so a
    /// control focused again keeps its selection).
    pub caret_for: Option<NodeId>,
    pub caret_on: bool,
    find_matches: Vec<(NodeId, usize, usize)>,
    find_idx: usize,
    pub scripts: Vec<ScriptSlot>,
    /// Script-created `<script>` elements whose "force async" flag is still set.
    pub script_force_async: HashSet<NodeId>,
    pub pending_nav: Option<NavRequest>,
    pub js_tasks: Vec<JsTask>,
    first_paint_deadline: Option<Instant>,
    /// `<img>` nodes waiting on each image URL, and the URL each node last
    /// fired load/error for.
    img_nodes: HashMap<String, Vec<NodeId>>,
    img_fired: HashMap<NodeId, String>,
    pub dcl_fired: bool,
    pub load_fired: bool,
    pub global_history: Arc<Mutex<Vec<(String, String)>>>,
    mouse_down_target: Option<NodeId>,
    /// Text selection (anchor, focus) and whether a drag-selection is in progress.
    pub sel: Option<(crate::layout::selection::SelPoint, crate::layout::selection::SelPoint)>,
    /// The DOM selection as script last reported it: (anchor node, offset, focus node, offset);
    /// `sel_gen` is the DOM generation it was synced at.
    pub dom_sel: Option<(NodeId, usize, NodeId, usize)>,
    pub sel_gen: u64,
    /// document.designMode is "on".
    pub design_mode: bool,
    selecting: bool,
    last_down: (std::time::Instant, f32, f32, u8),
    /// Last button press (time, x, y, button, click count) for UIEvent.detail.
    press_count: (std::time::Instant, f32, f32, u8, u32),
    pub last_frame: Option<Arc<FrameData>>,
    pub js_enabled: bool,
    /// COOP same-origin + COEP require-corp/credentialless on the document.
    pub cross_origin_isolated: bool,
    last_cursor: Cursor,
    last_status: String,
    pub fetch_results: Vec<(u64, Result<crate::net::cors::Outcome, NetError>)>,
    /// Raw bodies of completed fetches, until script reads them as bytes.
    pub fetch_bodies: crate::js::host::BodyStore,
    pub fetch_streams: HashMap<u64, FetchStream>,
    /// Open WebSocket connections by script id (dropping one closes it).
    pub ws_conns: HashMap<u64, std::sync::mpsc::Sender<crate::net::ws::WsCmd>>,
    pub worker_events: Vec<(u32, crate::js::worker::WorkerEvent)>,
    pub restore_scroll: Option<(f32, f32)>,
    pub target_node: Option<NodeId>,
    pub user_gesture: bool,
    /// Keys physically held (pressed in the UI, not yet released): their
    /// keyup comes from the release instead of right after the keydown.
    pub held_keys: HashSet<String>,
    key_repeat: bool,
    /// The button Space went down on (it activates on keyup).
    pub space_target: Option<NodeId>,
    /// The element the current keydown was dispatched to.
    keydown_target: Option<NodeId>,
    /// Mouse buttons held (MouseEvent.buttons bits).
    pub buttons_down: u8,
    /// The latest pointer position not yet sent to script.
    pub pending_move: Option<(f32, f32, Option<NodeId>)>,
    /// Pointer motion since the last mousemove sent to script, the last
    /// position, and whether script holds the pointer lock.
    pub move_delta: (f32, f32),
    pub last_move: Option<(f32, f32)>,
    pub pl_active: bool,
    /// Script added a (non-passive) wheel/mousewheel listener (the UI then
    /// sends wheels here), or only passive ones.
    pub wheel_listeners: bool,
    pub wheel_passive: bool,
    /// The last title sent to the window.
    pub last_title: std::cell::RefCell<Option<String>>,
    /// Wheel event transaction: the latched target and when it last got a wheel.
    pub wheel_tx: Option<(NodeId, std::time::Instant, (f32, f32))>,
    /// Scroll containers (DOCUMENT: the viewport) whose position changed since the
    /// last frame: they get `scroll` at the next frame, then `scrollend` one later.
    pub pending_scroll: Vec<NodeId>,
    pub pending_scrollend: Vec<NodeId>,
    pub local_storage: HashMap<String, String>,
    pub session_storage: HashMap<String, String>,
    pub history_state_json: String,
    load_started: Instant,
}

thread_local! {
    static PAGE: RefCell<Option<Page>> = const { RefCell::new(None) };
}

/// Run `f` with the current tab's page (None if not on a tab thread or re-entrant).
pub fn with_page<R>(f: impl FnOnce(&mut Page) -> R) -> Option<R> {
    PAGE.with(|p| match p.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => None,
    })
}

/// XML media types parsed as XML (SVG images keep the image viewer).
fn is_xml_type(ct: &str) -> bool {
    let ct = ct.trim().to_ascii_lowercase();
    ct == "application/xhtml+xml" || ct == "application/xml" || ct == "text/xml" || (ct.ends_with("+xml") && ct != "image/svg+xml")
}

fn is_html_type(ct: &str) -> bool {
    ct.is_empty() || ct.contains("html") || ct.contains("xml")
}

/// Addresses typed without a scheme get https:// first. If that fails to
/// connect (a plain-HTTP site, a local dev server), the load is retried
/// once over http:// before an error page is shown.
fn https_guesses() -> &'static std::sync::Mutex<Vec<String>> {
    static G: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> = std::sync::OnceLock::new();
    G.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

fn note_https_guess(url: &str) {
    if let Ok(mut g) = https_guesses().lock() {
        if g.len() >= 64 {
            g.remove(0);
        }
        g.push(url.to_string());
    }
}

/// The http:// address to try when `url` was an https:// guess that failed
/// with `err` (connection, TLS, timeout or protocol errors; an unknown host
/// would fail over http too).
fn http_fallback_for(url: &str, err: &NetError) -> Option<String> {
    if !matches!(err, NetError::Connect(_) | NetError::Tls(_) | NetError::Timeout | NetError::Protocol(_) | NetError::Io(_)) {
        return None;
    }
    let mut g = https_guesses().lock().ok()?;
    let norm = |s: &str| s.trim_end_matches('/').to_string();
    let i = g.iter().position(|u| norm(u) == norm(url))?;
    g.remove(i);
    url.strip_prefix("https://").map(|rest| format!("http://{rest}"))
}

pub fn normalize_input(input: &str) -> String {
    let t = input.trim();
    if t.is_empty() {
        return "ohb://newtab".into();
    }
    let lower = t.to_ascii_lowercase();
    for p in ["http://", "https://", "file://", "ohb://", "about:", "data:", "view-source:"] {
        if lower.starts_with(p) {
            return t.to_string();
        }
    }
    if t.starts_with('/') || t.starts_with("~/") || (t.len() > 2 && t.as_bytes()[1] == b':' && (t.as_bytes()[2] == b'\\' || t.as_bytes()[2] == b'/')) {
        let path = if let Some(rest) = t.strip_prefix("~/") {
            std::env::var("HOME").map(|h| format!("{h}/{rest}")).unwrap_or(t.to_string())
        } else {
            t.to_string()
        };
        if let Ok(u) = Url::from_file_path(&path) {
            return u.to_string();
        }
    }
    let looks_like_host = !t.contains(' ') && (t.contains('.') || t.starts_with("localhost")) && !t.ends_with('.');
    if looks_like_host {
        let u = format!("https://{t}");
        note_https_guess(&u);
        return u;
    }
    crate::prefs::search_url(t)
}

/// Scroll anchor selection (CSS Scroll Anchoring, simplified): the first
/// in-flow element fully inside the viewport, looking into partially visible
/// ones; returns it with its current document y.
fn scroll_anchor(tree: &layout::LayoutTree, doc: &Document, sy: f32, vh: f32) -> Option<(NodeId, f32)> {
    let anchor_none = |st: &css::style::ComputedStyle| match css::style::gen_val(st, "overflow-anchor") {
        css::style::GenVal::Decl(v, ..) => v.iter().any(|c| c.ident().map(|i| i.eq_ignore_ascii_case("none")).unwrap_or(false)),
        _ => false,
    };
    let html = *tree.boxes[tree.root].children.first()?;
    if anchor_none(&tree.boxes[html].style) {
        return None;
    }
    let (top, bottom) = (sy, sy + vh);
    let mut fallback = None;
    let mut stack: Vec<std::vec::IntoIter<usize>> = vec![tree.boxes[html].children.clone().into_iter()];
    let mut steps = 0;
    while let Some(it) = stack.last_mut() {
        let Some(b) = it.next() else {
            stack.pop();
            continue;
        };
        steps += 1;
        if steps > 20000 {
            break;
        }
        let bx = &tree.boxes[b];
        let st = &bx.style;
        if bx.out_of_flow || bx.fixed || bx.inline_level || st.position == css::style::Position::Sticky || anchor_none(st) || bx.h <= 0.0 {
            continue;
        }
        let (y0, y1) = (bx.abs_y, bx.abs_y + bx.h);
        if y1 <= top || y0 >= bottom {
            continue;
        }
        let Some(n) = bx.node.filter(|&n| doc.element(n).is_some()) else {
            // Anonymous boxes: look inside.
            stack.push(bx.children.clone().into_iter());
            continue;
        };
        if y0 >= top && y1 <= bottom {
            return Some((n, y0));
        }
        if fallback.is_none() {
            fallback = Some((n, y0));
        }
        if bx.ifc.is_none() && !bx.children.is_empty() {
            fallback = Some((n, y0));
            stack.push(bx.children.clone().into_iter());
        }
    }
    fallback
}

impl Page {
    pub fn new(tab_id: u64, ui: UiSink, tx: Sender<PageMsg>, history: Arc<Mutex<Vec<(String, String)>>>, dpr: f32) -> Page {
        Page {
            tab_id,
            ui,
            tx,
            doc: Document::new(Url::parse("about:blank").unwrap()),
            nav_gen: 0,
            loading: false,
            pending: 0,
            total_requests: 0,
            sheets: Vec::new(),
            import_texts: HashMap::new(),
            sheet_urls: HashMap::new(),
            style_override: HashMap::new(),
            pending_sheet_text: Vec::new(),
            index: None,
            shadow_indexes: HashMap::new(),
            shadow_index_cache: HashMap::new(),
            shadow_sheet_cache: HashMap::new(),
            ext_sheet: None,
            dt: devtools::DevState::default(),
            frame_parent: None,
            hover_frame: None,
            frame_capture: None,
            local_capture: None,
            frame_focus: None,
            doc_source: String::new(),
            sheets_dirty: true,
            sheets_sig: 0,
            styles: None,
            anim: Default::default(),
            anim_due: None,
            layout: None,
            hits: Vec::new(),
            fonts: FontContext::new(),
            res: Resources::new(dpr),
            declared_fonts: Vec::new(),
            js_font_families: HashSet::new(),
            js_fonts_reported: HashSet::new(),
            fonts_rescan: false,
            requested_fonts: HashSet::new(),
            font_status: HashMap::new(),
            install_xml: false,
            lazy_imgs: HashSet::new(),
            lazy_wanted: HashSet::new(),
            pdf: None,
            doc_content_type: String::from("text/html"),
            doc_charset: "UTF-8",
            pending_charset: None,
            frames_loading: Default::default(),
            frames_timer: false,
            win: (1024.0, 768.0),
            dpr,
            zoom: 1.0,
            scroll: (0.0, 0.0),
            elem_scroll: HashMap::new(),
            need_style: true,
            thrash_note: String::new(),
            full_style: true,
            style_snap: None,
            snap_gen: 0,
            snap_bumps: 0,
            has_log: HashMap::new(),
            style_sites: Vec::new(),
            struct_log: HashMap::new(),
            container_sizes: HashMap::new(),
            need_layout: true,
            prewarm_js: false,
            need_paint: true,
            last_gen: 0,
            hover_chain: Vec::new(),
            active_chain: Vec::new(),
            history: Vec::new(),
            hist_idx: 0,
            inline_cache: HashMap::new(),
            subdoc_styles: None,
            frame_views: HashMap::new(),
            frame_nav: HashMap::new(),
            frame_reload: HashSet::new(),
            frame_redirects: HashMap::new(),
            frame_referrer: None,
            frame_post: None,
            local_loading: HashSet::new(),
            caret: 0,
            field_anchor: None,
            select_popup: None,
            select_popup_opts: Vec::new(),
            select_popup_suppress: false,
            caret_for: None,
            caret_on: true,
            dom_sel: None,
            sel_gen: 0,
            design_mode: false,
            find_matches: Vec::new(),
            find_idx: 0,
            scripts: Vec::new(),
            script_force_async: HashSet::new(),
            pending_nav: None,
            js_tasks: Vec::new(),
            first_paint_deadline: None,
            img_nodes: HashMap::new(),
            img_fired: HashMap::new(),
            dcl_fired: false,
            load_fired: false,
            global_history: history,
            mouse_down_target: None,
            sel: None,
            selecting: false,
            last_down: (std::time::Instant::now() - std::time::Duration::from_secs(5), 0.0, 0.0, 0),
            press_count: (std::time::Instant::now() - std::time::Duration::from_secs(5), 0.0, 0.0, 0, 0),
            last_frame: None,
            js_enabled: true,
            last_cursor: Cursor::Default,
            last_status: String::new(),
            fetch_results: Vec::new(),
            fetch_bodies: Default::default(),
            fetch_streams: HashMap::new(),
            ws_conns: HashMap::new(),
            worker_events: Vec::new(),
            restore_scroll: None,
            target_node: None,
            user_gesture: false,
            cross_origin_isolated: false,
            held_keys: HashSet::new(),
            key_repeat: false,
            space_target: None,
            keydown_target: None,
            buttons_down: 0,
            pending_move: None,
            move_delta: (0.0, 0.0),
            last_move: None,
            pl_active: false,
            wheel_listeners: false,
            wheel_passive: false,
            last_title: std::cell::RefCell::new(None),
            wheel_tx: None,
            pending_scroll: Vec::new(),
            pending_scrollend: Vec::new(),
            local_storage: HashMap::new(),
            session_storage: HashMap::new(),
            history_state_json: "null".into(),
            load_started: Instant::now(),
        }
    }

    pub fn emit(&self, ev: PageEvent) {
        // The title is re-sent with every frame: only changes go out.
        if let PageEvent::Title(t) = &ev {
            if self.last_title.borrow().as_deref() == Some(t.as_str()) {
                return;
            }
            *self.last_title.borrow_mut() = Some(t.clone());
        }
        if let PageEvent::Url(..) = &ev {
            *self.last_title.borrow_mut() = None;
        }
        if let Some(fp) = &self.frame_parent {
            self.frame_emit(fp, ev);
            return;
        }
        (self.ui)(self.tab_id, ev);
    }

    pub fn viewport(&self) -> (f32, f32) {
        (self.win.0 / self.zoom, self.win.1 / self.zoom)
    }

    // -------------------------------------------------------------- navigation

    pub fn navigate(&mut self, mut req: NavRequest) {
        let mut target = normalize_input(&req.url);
        // Web pages can't open browser pages (settings and their actions).
        if (target.starts_with("ohb://") || target.starts_with("about:extensions")) && !target.starts_with("ohb://newtab") {
            if let Some(r) = &req.referrer {
                if r.scheme() != "ohb" {
                    eprintln!("[nav] blocked {} from {}: not allowed to open browser pages", target, r);
                    return;
                }
            }
        }
        // Settings actions run, then show the settings page in place.
        if crate::ext::perform_action(&target) {
            target = "ohb://extensions".into();
            req.url = target.clone();
            req.push = false;
        }
        if crate::storage::perform_action(&target) {
            target = "ohb://storage".into();
            req.url = target.clone();
            req.push = false;
        }
        if crate::prefs::perform_action(&target) {
            target = "ohb://settings".into();
            req.url = target.clone();
            req.push = false;
        }
        // Same-document fragment navigation.
        if let (Ok(new), true) = (Url::parse(&target), req.method == "GET") {
            let mut a = new.clone();
            a.set_fragment(None);
            let mut b = self.doc.url.clone();
            b.set_fragment(None);
            // (Also while the page is still loading: scripts set location.hash
            // early, e.g. Wikipedia's media viewer; that never reloads the page.)
            if a == b && new.fragment().is_some() && (self.layout.is_some() || self.dcl_fired) {
                self.fragment_navigate(new, !req.push);
                return;
            }
        }
        // Save scroll position of the current entry.
        if let Some(e) = self.history.get_mut(self.hist_idx) {
            e.scroll = self.scroll;
        }
        self.nav_gen += 1;
        // The document goes away: so do its WebSockets.
        self.ws_conns.clear();
        crate::ext::reset_tab(self.tab_id);
        net::clear_net_log(self.tab_id);
        net::set_request_context(Some(net::ReqContext { tab: self.tab_id, top: Url::parse(&target).ok() }));
        if std::env::var("OHB_DEBUG_SCRIPTS").is_ok() {
            eprintln!("[nav] navigate to {target}");
        }
        let gen = self.nav_gen;
        self.loading = true;
        self.load_started = Instant::now();
        self.pending = 0;
        self.total_requests = 1;
        self.restore_scroll = req.restore_scroll;
        self.emit(PageEvent::Loading(true, 0.05));
        self.emit(PageEvent::Url(target.clone(), target.starts_with("https://")));
        // An iframe's srcdoc document (its markup came with the request).
        if target == "about:srcdoc" && self.frame_parent.is_some() {
            if let Some(body) = req.body.take() {
                let url = Url::parse("about:srcdoc").unwrap();
                let resp = Response { url, status: 200, status_text: "OK".into(), headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())], body: Arc::new(body), redirected: false, from_cache: false };
                self.on_document(req, Ok(resp));
                return;
            }
        }
        // Internal pages.
        if let Some(html) = self.internal_page(&target) {
            let url = Url::parse(&target).unwrap_or_else(|_| Url::parse("ohb://newtab").unwrap());
            let resp = Response { url, status: 200, status_text: "OK".into(), headers: vec![("Content-Type".into(), "text/html".into())], body: Arc::new(html.into_bytes()), redirected: false, from_cache: false };
            self.on_document(req, Ok(resp));
            return;
        }
        if let Some(src) = target.strip_prefix("view-source:") {
            let url = match Url::parse(src) {
                Ok(u) => u,
                Err(e) => {
                    self.show_error(&target, &format!("ERR_INVALID_URL: {e}"));
                    return;
                }
            };
            let tx = self.tx.clone();
            let r = Request::get(url, RequestKind::Document, None);
            let mut req2 = req.clone();
            req2.url = target.clone();
            net::fetch_async(r, move |res| {
                let res = res.map(|mut r| {
                    let text = String::from_utf8_lossy(&r.body).into_owned();
                    let html = format!("<!doctype html><title>view-source:{}</title><pre style=\"white-space:pre-wrap;font-size:13px\">{}</pre>", net::html_escape(r.url.as_str()), net::html_escape(&text));
                    r.headers = vec![("Content-Type".into(), "text/html".into())];
                    r.body = Arc::new(html.into_bytes());
                    r.url = Url::parse(&format!("view-source:{}", r.url)).unwrap_or(r.url.clone());
                    r
                });
                let _ = tx.send(PageMsg::Net(gen, NetMsg::Document(req2, res)));
            });
            return;
        }
        let url = match Url::parse(&target) {
            Ok(u) => u,
            Err(e) => {
                self.show_error(&target, &format!("ERR_INVALID_URL: {e}"));
                return;
            }
        };
        let mut r = Request::get(url, RequestKind::Document, req.referrer.clone());
        r.method = req.method.clone();
        r.body = req.body.clone();
        if let Some(ct) = &req.content_type {
            r.headers.push(("Content-Type".into(), ct.clone()));
        }
        let tx = self.tx.clone();
        let req2 = req.clone();
        let mut req3 = req2.clone();
        req3.url = target;
        net::fetch_async(r, move |res| {
            let _ = tx.send(PageMsg::Net(gen, NetMsg::Document(req3, res)));
        });
        // The tab loop prepares the next script runtime while this loads.
        self.prewarm_js = self.js_enabled;
    }

    fn internal_page(&self, target: &str) -> Option<String> {
        let t = target.trim_end_matches('/');
        match t {
            "ohb://newtab" | "about:newtab" | "about:home" | "ohb://home" => Some(internal::newtab()),
            "ohb://history" | "about:history" => {
                let h = self.global_history.lock().unwrap().clone();
                Some(internal::history(&h))
            }
            "ohb://about" | "about:ohb" | "about:version" => {
                let jar = net::client().cookies.lock().unwrap().len();
                let cache = net::client().cache.lock().unwrap().bytes();
                let stats = format!(
                    "<tr><td>Cookies</td><td>{jar}</td></tr><tr><td>HTTP cache</td><td>{} KB</td></tr><tr><td>Images in this tab</td><td>{} KB</td></tr><tr><td>Web fonts in this tab</td><td>{}</td></tr><tr><td>User agent</td><td>{}</td></tr>",
                    cache / 1024,
                    self.res.memory() / 1024,
                    self.fonts.web_font_count(),
                    net::html_escape(&net::user_agent())
                );
                Some(internal::about(&stats))
            }
            "about:blank" => Some(String::new()),
            "ohb://extensions" | "about:extensions" => Some(crate::ext::page()),
            "ohb://storage" | "about:storage" => Some(internal::storage_page()),
            "ohb://settings" | "about:settings" => Some(internal::settings_page()),
            _ if t.starts_with("ohb://devtools") => Some(include_str!("devtools.html").to_string()),
            "ohb://crash" => panic!("crash requested via ohb://crash"),
            _ if t.starts_with("ohb://crashed") => {
                let reason = t.split_once("?reason=").map(|(_, r)| String::from_utf8_lossy(&net::percent_decode(r.as_bytes())).into_owned()).unwrap_or_default();
                Some(internal::crashed(&reason))
            }
            _ => None,
        }
    }

    /// Same-document navigation to a fragment: the URL changes now, the
    /// target scrolls into view and `hashchange` follows as a task.
    pub(crate) fn fragment_navigate(&mut self, new: Url, replace: bool) {
        let old = self.doc.url.to_string();
        self.doc.url = new.clone();
        if replace {
            if let Some(e) = self.history.get_mut(self.hist_idx) {
                e.url = new.to_string();
            }
        } else {
            self.push_history(new.as_str());
        }
        if self.layout.is_some() {
            self.scroll_to_fragment();
        }
        self.emit(PageEvent::Url(self.doc.url.to_string(), self.doc.url.scheme() == "https"));
        self.emit_history_state();
        if old != new.as_str() {
            use crate::js::host::HostValue as H;
            self.js_tasks.push(JsTask::CallGlobal("__ohb_hashchange", vec![H::from(old.as_str()), H::from(new.as_str())]));
        }
    }

    fn show_error(&mut self, url: &str, err: &str) {
        let html = internal::error(url, err);
        let u = Url::parse(url).unwrap_or_else(|_| Url::parse("about:blank").unwrap());
        self.install_document(&html, u, true);
        self.finish_loading();
    }

    fn push_history(&mut self, url: &str) {
        if self.history.get(self.hist_idx).map(|e| e.url == url).unwrap_or(false) {
            return;
        }
        if !self.history.is_empty() {
            self.history.truncate(self.hist_idx + 1);
        }
        self.history.push(HistEntry { url: url.to_string(), title: String::new(), scroll: (0.0, 0.0) });
        self.hist_idx = self.history.len() - 1;
        if self.history.len() > 200 {
            self.history.remove(0);
            self.hist_idx -= 1;
        }
    }

    pub fn push_state(&mut self, url: Option<Url>, replace: bool) {
        if let Some(u) = url {
            if u.origin() == self.doc.url.origin() {
                self.doc.url = u;
            }
        }
        let s = self.doc.url.to_string();
        if replace {
            if let Some(e) = self.history.get_mut(self.hist_idx) {
                e.url = s.clone();
            }
        } else {
            self.push_history(&s);
        }
        self.emit(PageEvent::Url(s, self.doc.url.scheme() == "https"));
        self.emit_history_state();
    }

    fn emit_history_state(&self) {
        let back = self.hist_idx > 0;
        let fwd = self.hist_idx + 1 < self.history.len();
        self.emit(PageEvent::HistoryState(back, fwd));
    }

    pub fn go(&mut self, delta: i32) {
        let idx = self.hist_idx as i32 + delta;
        if idx < 0 || idx as usize >= self.history.len() {
            return;
        }
        if let Some(e) = self.history.get_mut(self.hist_idx) {
            e.scroll = self.scroll;
        }
        self.hist_idx = idx as usize;
        let e = self.history[self.hist_idx].clone();
        let mut req = NavRequest::get(&e.url);
        req.push = false;
        req.restore_scroll = Some(e.scroll);
        self.navigate(req);
    }

    pub fn reload(&mut self) {
        if let Some(e) = self.history.get(self.hist_idx).cloned() {
            let mut req = NavRequest::get(&e.url);
            req.push = false;
            req.restore_scroll = Some(self.scroll);
            // Bypass the HTTP cache for the document.
            net::client().cache.lock().unwrap().clear();
            self.navigate(req);
        }
    }

    pub fn stop(&mut self) {
        self.nav_gen += 1;
        self.finish_loading();
    }

    fn on_document(&mut self, req: NavRequest, res: Result<Response, NetError>) {
        let resp = match res {
            Ok(r) => r,
            Err(e) => {
                if req.method == "GET" {
                    if let Some(http) = http_fallback_for(&req.url, &e) {
                        eprintln!("[nav] {} failed ({e}); trying {http}", req.url);
                        let mut r = req;
                        r.url = http;
                        self.navigate(r);
                        return;
                    }
                }
                self.show_error(&req.url, &e.to_string());
                return;
            }
        };
        let ct = resp.content_type();
        // SVG documents in frames are XML documents (top-level ones get the image viewer).
        let svg_doc = ct.trim().eq_ignore_ascii_case("image/svg+xml");
        self.doc_content_type = if is_xml_type(&ct) || svg_doc { ct.clone() } else if ct.is_empty() || !(is_html_type(&ct) || ct.starts_with("text/")) { "text/html".into() } else if ct.starts_with("text/") && !is_html_type(&ct) { "text/html".into() } else { ct.clone() };
        let disposition = resp.header("content-disposition").unwrap_or("").to_ascii_lowercase();
        {
            let coop = resp.header("cross-origin-opener-policy").unwrap_or("").trim().to_ascii_lowercase();
            let coep = resp.header("cross-origin-embedder-policy").unwrap_or("").trim().to_ascii_lowercase();
            self.cross_origin_isolated = coop.starts_with("same-origin") && !coop.contains("allow-popups") && (coep.starts_with("require-corp") || coep.starts_with("credentialless"));
            // A coi-serviceworker registered for this scope adds these headers.
            if coi_scope_of(resp.url.as_str()).is_some() {
                self.cross_origin_isolated = true;
            }
        }
        let final_url = resp.url.clone();
        let is_pdf = ct.trim().to_ascii_lowercase().starts_with("application/pdf") || (ct.contains("octet-stream") && final_url.path().to_ascii_lowercase().ends_with(".pdf") && resp.body.starts_with(b"%PDF"));
        let is_download = disposition.starts_with("attachment") || !(is_pdf || is_html_type(&ct) || ct.starts_with("text/") || ct.starts_with("image/") || ct.contains("json") || ct.contains("javascript"));
        if is_download && final_url.scheme() != "ohb" {
            // A file to save: the page stays as it is, and the browser asks
            // first (nothing is written without the user's yes).
            let name = disposition
                .split("filename=")
                .nth(1)
                .map(|s| s.split(';').next().unwrap_or("").trim().trim_matches(|c| c == '"' || c == '\'').to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| final_url.path_segments().and_then(|s| s.last().map(|x| String::from_utf8_lossy(&crate::net::percent_decode(x.as_bytes())).into_owned())).filter(|s| !s.is_empty()))
                .unwrap_or_else(|| "download".into());
            let mut name: String = name.chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '\0') && !c.is_control()).collect();
            if name.starts_with('.') {
                name.insert(0, '_');
            }
            let size = resp.body.len();
            let id = DOWNLOAD_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            PENDING_DOWNLOADS.lock().unwrap().insert(id, (name.clone(), resp.body.clone()));
            let site = final_url.host_str().unwrap_or("").to_string();
            if self.frame_parent.is_none() {
                self.emit(PageEvent::Download { id, name, size, site });
            } else {
                drop_download(id);
            }
            if self.history.is_empty() || self.doc.url.as_str() == "about:blank" {
                // A new tab opened just for the download.
                if self.history.is_empty() {
                    self.push_history("about:blank");
                }
            }
            self.emit(PageEvent::Url(self.doc.url.to_string(), self.doc.url.scheme() == "https"));
            self.finish_loading();
            return;
        }
        if req.push {
            self.push_history(final_url.as_str());
        } else if let Some(e) = self.history.get_mut(self.hist_idx) {
            e.url = final_url.to_string();
        }
        if self.history.is_empty() {
            self.push_history(final_url.as_str());
        }
        let html = if is_pdf {
            pdf::viewer_html(final_url.as_str(), &resp.body)
        } else if ct.starts_with("image/") && !svg_doc {
            format!(
                "<!doctype html><html><head><title>{0}</title><style>body{{margin:0;background:#1e1e1e;display:flex;align-items:center;justify-content:center;min-height:100vh}}img{{max-width:100%;max-height:100vh}}</style></head><body><img src=\"{0}\"></body></html>",
                net::html_escape(final_url.as_str())
            )
        } else if (is_xml_type(&ct) || svg_doc) && final_url.scheme() != "ohb" {
            // XHTML and other XML documents go through the XML parser.
            let (text, cs) = crate::html::encoding::decode_doc(&resp.body, resp.charset().as_deref(), false, true);
            self.install_xml = true;
            self.pending_charset = Some(cs);
            text
        } else if is_html_type(&ct) || final_url.scheme() == "ohb" {
            let (text, cs) = crate::html::encoding::decode_doc(&resp.body, resp.charset().as_deref(), true, false);
            self.pending_charset = Some(cs);
            text
        } else {
            let (text, cs) = crate::html::encoding::decode_doc(&resp.body, resp.charset().as_deref(), false, false);
            self.pending_charset = Some(cs);
            format!("<!doctype html><html><head><title>{}</title></head><body><pre style=\"word-wrap:break-word;white-space:pre-wrap\">{}</pre></body></html>", net::html_escape(final_url.as_str()), net::html_escape(&text))
        };
        self.install_document(&html, final_url, false);
    }

    /// Replace the current document with freshly parsed HTML.
    fn install_document(&mut self, html: &str, url: Url, is_error: bool) {
        // Requests from here on are this page's (extensions filter by it).
        net::set_request_context(Some(net::ReqContext { tab: self.tab_id, top: Some(url.clone()) }));
        let ext_css = if is_error { String::new() } else { crate::ext::page_css(&url) };
        self.ext_sheet = if ext_css.is_empty() { None } else { Some(Rc::new(css::parse_stylesheet(&ext_css, &url, css::Origin::User))) };
        let doc = if std::mem::take(&mut self.install_xml) {
            let (mut d, _err) = crate::html::xml::parse_document(html, url.clone());
            d.is_xml = true;
            d
        } else {
            crate::html::parse_document(html, url.clone())
        };
        self.doc = doc;
        self.doc_charset = self.pending_charset.take().unwrap_or("UTF-8");
        self.doc.charset = self.doc_charset;
        // A srcdoc document resolves URLs against its parent's.
        if self.doc.url.as_str() == "about:srcdoc" {
            if let Some(fp) = &self.frame_parent {
                if self.doc.base_url.as_str() == "about:srcdoc" {
                    self.doc.base_url = fp.parent_url.clone();
                }
                self.doc.fallback_base = Some(fp.parent_url.clone());
            }
        }
        self.dt.reset_document();
        self.doc_source = html.to_string();
        if let Some(c) = self.dt.client {
            devtools::send(c, devtools::DevMsg::Event(serde_json::json!({ "type": "navigated", "url": url.as_str() }).to_string()));
        }
        self.scripts.clear();
        self.sheets.clear();
        self.import_texts.clear();
        self.sheet_urls.clear();
        self.img_nodes.clear();
        self.img_fired.clear();
        self.index = None;
        { self.sheets_dirty = true; crate::page::full_why(file!(), line!()); }
        self.styles = None;
        self.anim.clear();
        self.anim_due = None;
        self.layout = None;
        self.sel = None;
        self.selecting = false;
        self.hits.clear();
        self.fonts = FontContext::new();
        self.res = Resources::new(self.dpr);
        self.lazy_imgs.clear();
        self.lazy_wanted.clear();
        // The PDF viewer: its pages are drawn on demand (pdf_tick); until
        // then they hold a place without a network request.
        self.pdf = pdf::PENDING.with(|p| p.borrow_mut().take());
        if let Some(d) = &self.pdf {
            for i in 0..d.sizes.len() {
                self.res.images.insert(pdf::page_url(d.tag, i), ImgState::Loading);
            }
        }
        self.frame_views.clear();
        self.frame_nav.clear();
        self.frame_redirects.clear();
        self.local_loading.clear();
        self.declared_fonts.clear();
        self.requested_fonts.clear();
        self.font_status.clear();
        self.elem_scroll.clear();
        self.hover_chain.clear();
        self.active_chain.clear();
        self.find_matches.clear();
        self.inline_cache.clear();
        self.dcl_fired = false;
        self.load_fired = false;
        self.frames_loading.clear();
        self.frames_timer = false;
        self.scroll = self.restore_scroll.unwrap_or((0.0, 0.0));
        self.need_style = true;
        { self.full_style = true; crate::page::full_why(file!(), line!()); }
        self.need_layout = true;
        self.need_paint = true;
        self.last_gen = self.doc.generation;
        self.target_node = None;
        self.history_state_json = "null".into();
        self.emit(PageEvent::ScrollTo(self.scroll.0, self.scroll.1));
        self.emit(PageEvent::Url(url.to_string(), url.scheme() == "https"));
        self.emit_history_state();
        let title = self.display_title();
        self.emit(PageEvent::Title(title.clone()));
        if let Some(e) = self.history.get_mut(self.hist_idx) {
            e.title = title.clone();
        }
        if !is_error && matches!(url.scheme(), "http" | "https" | "file") {
            let mut h = self.global_history.lock().unwrap();
            h.push((url.to_string(), title));
            if h.len() > 5000 {
                h.remove(0);
            }
        }
        self.collect_head_resources();
        self.first_paint_deadline = Some(Instant::now() + Duration::from_millis(1500));
        self.load_favicon();
        self.js_tasks.push(JsTask::RunScripts);
        self.update_progress();
    }

    pub fn display_title(&self) -> String {
        let t = self.doc.title();
        if !t.is_empty() {
            return t;
        }
        let u = &self.doc.url;
        u.host_str().map(|h| h.to_string()).unwrap_or_else(|| u.to_string())
    }

    pub fn update_progress(&mut self) {
        if !self.loading {
            return;
        }
        let done = self.total_requests.saturating_sub(self.pending) as f32;
        let p = (0.1 + 0.9 * done / self.total_requests.max(1) as f32).min(0.95);
        self.emit(PageEvent::Loading(true, p));
        if self.pending == 0 && self.scripts.iter().all(|s| s.done) {
            // Images delay the load event: they are requested by the style
            // pass, so run it for a document changed since the last one.
            if !self.load_fired && self.dcl_fired && (self.styles.is_none() || self.need_style) && !self.css_blocking() {
                self.ensure_layout();
                if self.pending > 0 {
                    return;
                }
            }
            // Frames delay the load event: start the ones not yet running
            // (they normally start at the first render) and wait for them.
            if !self.load_fired && self.dcl_fired && self.js_enabled && self.frames_loading.is_empty() && self.doc.descendants(DOCUMENT).any(|e| self.doc.is_tag(e, "iframe")) {
                if self.layout.is_none() || self.need_layout || self.need_style {
                    self.ensure_layout();
                }
                self.update_frames();
            }
            if !self.frames_loading.is_empty() {
                if !self.frames_timer {
                    self.frames_timer = true;
                    let tx = self.tx.clone();
                    let gen = self.nav_gen;
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(10));
                        let _ = tx.send(PageMsg::FramesTimeout(gen));
                    });
                }
                return;
            }
            self.finish_loading();
        }
    }

    fn finish_loading(&mut self) {
        if self.loading {
            self.loading = false;
            self.emit(PageEvent::Loading(false, 1.0));
        }
        if !self.load_fired && self.dcl_fired {
            self.load_fired = true;
            self.js_tasks.push(JsTask::WindowEvent("load".into()));
        }
    }

    fn load_favicon(&mut self) {
        let doc = &self.doc;
        if doc.url.scheme() == "ohb" {
            // Browser pages wear the browser's own mark.
            let px = (16.0 * self.dpr.max(1.0)).round().max(16.0) as u32;
            let pm = crate::shell::chrome::render_icon(px);
            let img = crate::paint::ImageData { width: px, height: px, pixmap: pm, density: px as f32 / 16.0, nat: Some((16.0, 16.0)), svg_par: None };
            self.emit(PageEvent::Favicon(Some(Arc::new(img))));
            return;
        }
        if !matches!(doc.url.scheme(), "http" | "https") {
            self.emit(PageEvent::Favicon(None));
            return;
        }
        let mut href = None;
        if let Some(head) = doc.head() {
            for c in doc.element_children(head) {
                if doc.is_tag(c, "link") {
                    let rel = doc.attr(c, "rel").unwrap_or("").to_ascii_lowercase();
                    if rel.split_whitespace().any(|r| r == "icon") || rel == "shortcut icon" {
                        href = doc.attr(c, "href").and_then(|h| doc.resolve_url(h));
                        if href.is_some() && !doc.attr(c, "href").unwrap_or("").ends_with(".svg") {
                            break;
                        }
                    }
                }
            }
        }
        let url = href.or_else(|| doc.url.join("/favicon.ico").ok());
        if let Some(u) = url {
            let tx = self.tx.clone();
            let gen = self.nav_gen;
            net::fetch_async(Request::get(u.clone(), RequestKind::Favicon, Some(doc.url.clone())), move |r| {
                let img = r.ok().filter(|r| r.ok()).and_then(|r| resources::decode_image(&r.body, &r.content_type(), r.url.as_str(), 1.0)).map(Arc::new);
                let _ = tx.send(PageMsg::Net(gen, NetMsg::Favicon(img)));
            });
        }
    }

    // ----------------------------------------------------------- subresources

    fn collect_head_resources(&mut self) {
        let doc = &self.doc;
        let mut sheet_nodes: Vec<(NodeId, bool)> = Vec::new(); // (node, is_link)
        let mut script_nodes: Vec<NodeId> = Vec::new();
        let mut preload_nodes: Vec<NodeId> = Vec::new();
        for n in doc.descendants(DOCUMENT) {
            let el = match doc.element(n) {
                Some(e) => e,
                None => continue,
            };
            if el.ns != crate::dom::Namespace::Html && !(el.ns == crate::dom::Namespace::Svg && el.name == "style") {
                continue;
            }
            match el.name.as_str() {
                "link" => {
                    let rel = el.attr("rel").unwrap_or("").to_ascii_lowercase();
                    // Like Chrome, fetch every stylesheet; `media` decides at
                    // cascade time whether it applies (and whether it blocks).
                    if rel.split_whitespace().any(|r| r == "stylesheet") && !rel.contains("alternate") && el.attr("disabled").is_none() {
                        sheet_nodes.push((n, true));
                    }
                    if rel.split_whitespace().any(|r| r == "modulepreload") {
                        if let Some(u) = el.attr("href").and_then(|h| doc.resolve_url(h.trim())) {
                            crate::js::modules::prefetch_module(u, Some(doc.url.clone()), 1);
                        }
                    }
                    if rel.split_whitespace().any(|r| r == "preload") {
                        preload_nodes.push(n);
                    }
                }
                "style" => sheet_nodes.push((n, false)),
                "script" => script_nodes.push(n),
                _ => {}
            }
        }
        for (n, is_link) in sheet_nodes {
            self.add_sheet_node(n, is_link);
        }
        for n in script_nodes {
            self.add_script_node(n);
        }
        for n in preload_nodes {
            self.preload_link(n);
        }
    }

    /// A `<style>` element's sheet text (CSSOM's edited rules, if any).
    pub fn style_text(&self, n: NodeId) -> String {
        match self.style_override.get(&n) {
            Some(t) => t.to_string(),
            None => self.doc.child_text_content(n),
        }
    }

    /// Parse `<style>` sheets whose CSSOM text changed since the last pass.
    pub fn flush_pending_sheets(&mut self) {
        if self.pending_sheet_text.is_empty() {
            return;
        }
        for n in std::mem::take(&mut self.pending_sheet_text) {
            if self.doc.valid(n) && self.doc.is_connected(n) {
                self.add_sheet_node(n, false);
            }
        }
    }

    pub fn add_sheet_node(&mut self, n: NodeId, is_link: bool) {
        let idx = self.sheets.len();
        // A <style> in a shadow tree is read when its tree's rules are indexed.
        // (Its tree's index is rebuilt each pass; a changed index restyles
        // just that host — see `incremental_roots`.)
        if !is_link && !self.doc.in_document_tree(n) {
            self.need_style = true;
            self.need_layout = true;
            return;
        }
        if is_link && self.sheets.iter().any(|s| s.node == Some(n)) && !self.doc.in_document_tree(n) {
            return;
        }
        // A node keeps one slot: re-adding it (new text, new href) replaces
        // the sheet in place, so its rules keep their place in the cascade.
        let existing = self.sheets.iter().position(|s| s.node == Some(n));
        if is_link {
            let href = match self.doc.attr(n, "href").and_then(|h| self.doc.resolve_url(h)) {
                Some(u) => u,
                None => return,
            };
            if let Some(i) = existing {
                if self.sheet_urls.get(&i) == Some(&href) {
                    { self.sheets_dirty = true; crate::page::full_why(file!(), line!()); }
                    self.need_style = true;
                    { self.full_style = true; crate::page::full_why(file!(), line!()); }
                    return;
                }
            }
            // Sheets from the parser (not ones script adds later) block scripts and first paint.
            let blocking = self.node_media_matches(n) && !self.dcl_fired;
            let slot = SheetSlot { sheet: None, imports: Vec::new(), imp_pending: 0, failed: false, load_fired: false, lead: None, pending: true, node: Some(n), blocking, text: None, url: None };
            let i = match existing {
                Some(i) => {
                    self.sheets[i] = slot;
                    i
                }
                None => {
                    self.sheets.push(slot);
                    idx
                }
            };
            self.sheet_urls.insert(i, href.clone());
            self.fetch_sheet(i, href);
        } else {
            let text = self.style_text(n);
            let base = self.doc.base_url.clone();
            let sheet = css::parse_stylesheet(&text, &base, css::Origin::Author);
            let imports = sheet.imports.clone();
            let lead = sheet.leading_layers().map(Rc::new);
            let slot = SheetSlot { sheet: Some(Rc::new(sheet)), imports: Vec::new(), imp_pending: 0, failed: false, load_fired: false, lead, pending: false, node: Some(n), blocking: false, text: None, url: None };
            let i = match existing {
                Some(i) => {
                    self.sheets[i] = slot;
                    i
                }
                None => {
                    self.sheets.push(slot);
                    idx
                }
            };
            self.start_imports(i, &imports, &base, None);
            self.maybe_fire_sheet_load(i);
            { self.sheets_dirty = true; crate::page::full_why(file!(), line!()); }
            self.need_style = true;
            { self.full_style = true; crate::page::full_why(file!(), line!()); }
        }
    }

    /// Load a sheet's `@import`s; `within` is the import (slot index) whose
    /// own imports these are.
    fn start_imports(&mut self, idx: usize, imports: &[css::Import], base: &Url, within: Option<usize>) {
        if imports.is_empty() {
            return;
        }
        let env = self.media_env();
        let (pkey, pdepth) = match within.and_then(|w| self.sheets.get(idx).and_then(|s| s.imports.get(w))) {
            Some(&(k, d, _, _)) => (k, d),
            None => (0.0, 0),
        };
        if pdepth >= 4 {
            return;
        }
        for (j, imp) in imports.iter().take(16).enumerate() {
            if !imp.supports_ok {
                continue;
            }
            if let Some(m) = &imp.media {
                if !css::media_matches(m, &env) {
                    continue;
                }
            }
            if let Ok(u) = base.join(&imp.url) {
                if self.sheets[idx].imports.len() >= 100 {
                    break;
                }
                let key = if within.is_none() {
                    j as f64
                } else {
                    let span = 0.5f64.powi(pdepth as i32 + 1);
                    pkey - span + (j as f64 + 1.0) * span / 32.0
                };
                let slot = self.sheets[idx].imports.len();
                self.sheets[idx].imports.push((key, pdepth + 1, None, imp.layer.clone()));
                self.sheets[idx].imp_pending += 1;
                // Encode (parent, import) in the slot id.
                self.fetch_sheet(1_000_000 + idx * 100 + slot, u);
            }
        }
    }

    /// Fire `load` (or `error`) on a sheet's `<link>`/`<style>` once the sheet
    /// and all its imports have loaded.
    fn maybe_fire_sheet_load(&mut self, i: usize) {
        let Some(s) = self.sheets.get_mut(i) else { return };
        if s.pending || s.imp_pending > 0 || s.load_fired {
            return;
        }
        s.load_fired = true;
        let failed = s.failed;
        if let Some(n) = s.node {
            self.js_tasks.push(JsTask::Dispatch { target: n, ty: if failed { "error" } else { "load" }.into(), init: EventInit::default(), then: DefaultAction::None });
        }
    }

    fn fetch_sheet(&mut self, slot: usize, url: Url) {
        self.pending += 1;
        self.total_requests += 1;
        let tx = self.tx.clone();
        let gen = self.nav_gen;
        let u2 = url.clone();
        net::fetch_async(Request::get(url, RequestKind::Stylesheet, Some(self.doc.url.clone())), move |r| {
            let _ = tx.send(PageMsg::Net(gen, NetMsg::Stylesheet(slot, u2, r)));
        });
    }

    /// A cheap identity for a `<link>` sheet's current text (-1: none yet).
    pub fn link_sheet_gen(&self, n: NodeId) -> f64 {
        match self.sheets.iter().rev().find(|s| s.node == Some(n)).and_then(|s| s.text.as_ref()) {
            Some(t) => (Rc::as_ptr(t) as *const u8 as usize as f64) + t.len() as f64 * 1e-6,
            None => -1.0,
        }
    }

    /// A loaded `<link>` sheet's source text and URL (None while loading or failed).
    pub fn link_sheet_text(&self, n: NodeId) -> Option<(Rc<str>, Url)> {
        let s = self.sheets.iter().rev().find(|s| s.node == Some(n))?;
        Some((s.text.clone()?, s.url.clone()?))
    }

    /// CSSOM changed a `<link>` sheet's rules: re-parse them in its place.
    pub fn set_link_sheet_text(&mut self, n: NodeId, text: &str) {
        let Some(i) = self.sheets.iter().rposition(|s| s.node == Some(n)) else { return };
        let Some(url) = self.sheets[i].url.clone() else { return };
        let sheet = css::parse_stylesheet(text, &url, css::Origin::Author);
        let s = &mut self.sheets[i];
        s.sheet = Some(Rc::new(sheet));
        s.text = Some(Rc::from(text));
        { self.sheets_dirty = true; crate::page::full_why(file!(), line!()); }
        self.need_style = true;
        { self.full_style = true; crate::page::full_why(file!(), line!()); }
    }

    fn on_stylesheet(&mut self, slot: usize, url: Url, res: Result<Response, NetError>) {
        self.pending = self.pending.saturating_sub(1);
        // HTML §4.2.4: outside quirks mode an HTTP response whose Content-Type
        // isn't text/css is not a stylesheet (Acid3: the text/html "empty.css").
        let wrong_type = matches!(&res, Ok(r) if r.ok()
            && matches!(url.scheme(), "http" | "https")
            && (!self.doc.quirks || slot >= 1_000_000)
            && { let ct = r.content_type(); !ct.is_empty() && ct != "text/css" });
        let ok = matches!(&res, Ok(r) if r.ok()) && !wrong_type;
        let text = match res {
            Ok(r) if r.ok() && !wrong_type => crate::html::encoding::decode(&r.body, r.charset().as_deref(), false),
            _ => String::new(),
        };
        let base = url.clone();
        self.note_style_source(url.as_str(), &text);
        let mut sheet = css::parse_stylesheet(&text, &base, css::Origin::Author);
        let imports = sheet.imports.clone();
        // `@import … layer(name)`: the imported rules belong to that layer.
        if slot >= 1_000_000 {
            let (parent, child) = ((slot - 1_000_000) / 100, (slot - 1_000_000) % 100);
            if let Some(l) = self.sheets.get(parent).and_then(|s| s.imports.get(child)).and_then(|i| i.3.clone()) {
                let wrapped = if l.is_empty() { format!("@layer {{\n{text}\n}}") } else { format!("@layer {l} {{\n{text}\n}}") };
                sheet = css::parse_stylesheet(&wrapped, &base, css::Origin::Author);
            }
        }
        if ok && self.import_texts.len() < 300 {
            self.import_texts.insert(base.to_string(), Rc::from(text.as_str()));
        }
        if slot >= 1_000_000 {
            let parent = (slot - 1_000_000) / 100;
            let child = (slot - 1_000_000) % 100;
            let mut found = false;
            if let Some(s) = self.sheets.get_mut(parent) {
                if let Some(c) = s.imports.get_mut(child) {
                    c.2 = Some(Rc::new(sheet));
                    found = true;
                }
            }
            if found {
                self.start_imports(parent, &imports, &base, Some(child));
                if let Some(s) = self.sheets.get_mut(parent) {
                    s.imp_pending = s.imp_pending.saturating_sub(1);
                    s.failed |= !ok;
                }
                self.maybe_fire_sheet_load(parent);
            }
        } else if let Some(s) = self.sheets.get_mut(slot) {
            s.lead = sheet.leading_layers().map(Rc::new);
            s.sheet = Some(Rc::new(sheet));
            s.pending = false;
            s.text = if ok { Some(Rc::from(text.as_str())) } else { None };
            s.url = Some(base.clone());
            s.failed |= !ok;
            self.start_imports(slot, &imports, &base, None);
            // `load`/`error` on the <link> once it and its imports loaded (the
            // async-CSS `onload="this.media='all'"` idiom).
            self.maybe_fire_sheet_load(slot);
        }
        { self.sheets_dirty = true; crate::page::full_why(file!(), line!()); }
        self.need_style = true;
        { self.full_style = true; crate::page::full_why(file!(), line!()); }
        self.update_progress();
    }

    pub fn add_script_node(&mut self, n: NodeId) {
        let el = match self.doc.element(n) {
            Some(e) => e,
            None => return,
        };
        let kind = script_type_kind(el.attr("type"), el.attr("language"));
        let module = kind == Some("module");
        if kind == Some("importmap") {
            // Inline only; applies to modules resolved after it.
            if el.attr("src").is_none() {
                let text = Some(self.doc.child_text_content(n));
                self.scripts.push(ScriptSlot { node: n, text, url: None, module: false, importmap: true, asynchronous: false, deferred: false, in_order: false, done: false, failed: false });
            }
            return;
        }
        if kind.is_none() {
            return; // data blocks (application/json, text/template, ...)
        }
        if el.attr("nomodule").is_some() {
            return;
        }
        let src = el.attr("src").map(|s| s.to_string());
        let asynchronous = el.attr("async").is_some();
        let deferred = el.attr("defer").is_some() || module;
        let idx = self.scripts.len();
        let url = src.as_ref().and_then(|s| self.doc.resolve_url(s));
        let text = if src.is_none() { Some(self.doc.child_text_content(n)) } else { None };
        let failed = src.is_some() && url.is_none();
        if module && self.js_enabled {
            if let Some(t) = &text {
                crate::js::modules::prefetch_imports(t, &self.doc.base_url.clone(), Some(self.doc.url.clone()));
            }
        }
        self.scripts.push(ScriptSlot { node: n, text, url: url.clone(), module, importmap: false, asynchronous, deferred, in_order: false, done: false, failed });
        if let Some(u) = url {
            if !self.js_enabled {
                self.scripts[idx].done = true;
                return;
            }
            // Block insecure scripts on secure pages.
            let mut u = u;
            if self.doc.url.scheme() == "https" && u.scheme() == "http" {
                let _ = u.set_scheme("https");
            }
            self.pending += 1;
            self.total_requests += 1;
            let tx = self.tx.clone();
            let gen = self.nav_gen;
            net::fetch_async(Request::get(u, RequestKind::Script, Some(self.doc.url.clone())), move |r| {
                let _ = tx.send(PageMsg::Net(gen, NetMsg::Script(idx, r)));
            });
        }
    }

    fn on_script(&mut self, idx: usize, res: Result<Response, NetError>) {
        if std::env::var("OHB_DEBUG_SCRIPTS").is_ok() {
            eprintln!("[scripts] on_script {} slot={:?} res={:?}", idx, self.scripts.get(idx).and_then(|s| s.url.as_ref().map(|u| u.to_string())), res.as_ref().map(|r| (r.status, r.url.to_string())).map_err(|e| e.to_string()));
        }
        self.pending = self.pending.saturating_sub(1);
        if let Some(s) = self.scripts.get_mut(idx) {
            match res {
                Ok(r) if r.ok() && script_mime_ok(&r) => {
                    let text = crate::html::encoding::decode(&r.body, r.charset().as_deref(), false);
                    // Start loading a module's graph now rather than when it
                    // gets its turn to run (module scripts run in order).
                    if s.module {
                        crate::js::modules::prefetch_imports(&text, &r.url, Some(self.doc.url.clone()));
                    }
                    s.text = Some(text);
                }
                _ => s.failed = true,
            }
        }
        self.js_tasks.push(JsTask::RunScripts);
        self.update_progress();
    }

    pub fn media_env(&self) -> css::MediaEnv {
        let (w, h) = self.viewport();
        css::MediaEnv { width: w, height: h, dpr: self.dpr * self.zoom, dark: false, dev_w: w, dev_h: h }
    }

    fn ordered_sheets(&self) -> Vec<Rc<Stylesheet>> {
        let mut v = vec![css::ua_sheet()];
        if self.doc.quirks {
            v.push(css::quirks_sheet());
        }
        if let Some(u) = &self.ext_sheet {
            v.push(u.clone());
        }
        // Document order of the owner nodes decides precedence.
        let mut order: Vec<usize> = (0..self.sheets.len()).collect();
        if self.sheets.len() > 1 {
            let mut pos: HashMap<NodeId, usize> = HashMap::new();
            let owners: HashSet<NodeId> = self.sheets.iter().filter_map(|s| s.node).collect();
            for (i, n) in self.doc.descendants(DOCUMENT).enumerate() {
                if owners.contains(&n) {
                    pos.insert(n, i);
                }
            }
            order.sort_by_key(|&i| self.sheets[i].node.map(|n| pos.get(&n).copied().unwrap_or(usize::MAX)).unwrap_or(0));
        }
        if std::env::var_os("OHB_SHEET_DEBUG").is_some() {
            for &i in &order {
                let sl = &self.sheets[i];
                eprintln!("[sheet] slot={} node={:?} {} rules={} pending={} active={}", i, sl.node, sl.node.map(|n| self.doc.tag(n).to_string()).unwrap_or_default(), sl.sheet.as_ref().map(|s| s.rules.len()).unwrap_or(0), sl.pending, sl.node.map(|n| self.sheet_node_active(n) && self.doc.in_document_tree(n)).unwrap_or(true));
            }
            eprintln!("[sheet] ----");
        }
        for s in order.iter().map(|&i| &self.sheets[i]) {
            if let Some(n) = s.node {
                // Sheets inside shadow trees apply only there.
                if !self.sheet_node_active(n) || !self.doc.in_document_tree(n) {
                    continue;
                }
            }
            v.extend(s.imports_in_order());
            if let Some(sh) = &s.sheet {
                v.push(sh.clone());
            }
        }
        // document.adoptedStyleSheets come after the document's own sheets.
        if let Some(list) = self.doc.adopted.get(&DOCUMENT) {
            for t in list.clone() {
                v.push(self.text_sheet(&t));
            }
        }
        v
    }

    /// A parsed author sheet for CSS text (`<style>` in a shadow tree, constructed sheets), cached by text.
    pub(crate) fn text_sheet(&self, text: &Rc<str>) -> Rc<Stylesheet> {
        thread_local! {
            static CACHE: std::cell::RefCell<HashMap<String, HashMap<Rc<str>, Rc<Stylesheet>>>> = std::cell::RefCell::new(HashMap::new());
        }
        if let Some(s) = self.shadow_sheet_cache.get(text) {
            return s.clone();
        }
        // A constructed sheet's own base URL (see CSSStyleSheet._cssText).
        let own_base = text.strip_prefix("/*ohb-base:").and_then(|r| r.find("*/").map(|e| &r[..e])).and_then(|u| Url::parse(u).ok());
        let base = own_base.unwrap_or_else(|| self.doc.base_url.clone());
        let bkey = base.as_str().to_string();
        CACHE.with(|c| {
            let mut c = c.borrow_mut();
            if let Some(s) = c.get(&bkey).and_then(|m| m.get(text)) {
                return s.clone();
            }
            if c.values().map(|m| m.len()).sum::<usize>() > 4000 {
                c.clear();
            }
            let sh = Rc::new(css::parse_stylesheet(text, &base, css::Origin::Author));
            c.entry(bkey).or_default().insert(text.clone(), sh.clone());
            sh
        })
    }

    /// Rule indexes for every connected shadow tree: the UA sheet, then the
    /// tree's `<style>` and `<link rel=stylesheet>` elements in tree order,
    /// then its adopted sheets. Identical sheet lists share one index.
    fn build_shadow_indexes(&mut self) {
        self.shadow_indexes.clear();
        if self.doc.shadow_info.is_empty() {
            return;
        }
        let mut roots: Vec<NodeId> = self.doc.shadow_info.keys().copied().filter(|&r| self.doc.is_connected(r)).collect();
        roots.sort_unstable();
        let mut link_slots: HashMap<NodeId, usize> = HashMap::new();
        for (i, s) in self.sheets.iter().enumerate() {
            if let Some(n) = s.node {
                link_slots.insert(n, i);
            }
        }
        for r in roots {
            let mut sheets: Vec<Rc<Stylesheet>> = vec![css::ua_sheet()];
            if self.doc.quirks {
                sheets.push(css::quirks_sheet());
            }
            if let Some(u) = &self.ext_sheet {
                sheets.push(u.clone());
            }
            let owners: Vec<NodeId> = self.doc.descendants(r).filter(|&d| self.doc.is_tag(d, "style") || self.doc.is_tag(d, "link")).collect();
            for n in owners {
                if !self.node_media_matches(n) {
                    continue;
                }
                if self.doc.is_tag(n, "style") {
                    let text: Rc<str> = Rc::from(self.style_text(n));
                    sheets.push(self.text_sheet(&text));
                    continue;
                }
                let rel = self.doc.attr(n, "rel").unwrap_or("").to_ascii_lowercase();
                if !rel.split_whitespace().any(|x| x == "stylesheet") || self.doc.has_attr(n, "disabled") {
                    continue;
                }
                match link_slots.get(&n) {
                    Some(&i) => {
                        sheets.extend(self.sheets[i].imports_in_order());
                        if let Some(sh) = &self.sheets[i].sheet {
                            sheets.push(sh.clone());
                        }
                    }
                    None => {
                        // Inserted while disconnected: start loading it now.
                        let before = self.sheets.len();
                        self.add_sheet_node(n, true);
                        if self.sheets.len() > before {
                            link_slots.insert(n, before);
                        }
                    }
                }
            }
            if let Some(list) = self.doc.adopted.get(&r) {
                for t in list.clone() {
                    sheets.push(self.text_sheet(&t));
                }
            }
            let key: Vec<usize> = sheets.iter().map(|s| Rc::as_ptr(s) as usize).collect();
            let idx = match self.shadow_index_cache.get(&key) {
                Some(i) => i.clone(),
                None => {
                    if self.shadow_index_cache.len() > 2000 {
                        self.shadow_index_cache.clear();
                    }
                    let i = Rc::new(RuleIndex::new(sheets));
                    self.shadow_index_cache.insert(key, i.clone());
                    i
                }
            };
            self.shadow_indexes.insert(r, idx);
        }
    }

    /// Does any style rule (of the document or a shadow tree) use :hover/:active?
    fn any_hover_rules(&self) -> bool {
        self.index.as_ref().map(|i| i.has_hover_rules).unwrap_or(false) || self.shadow_indexes.values().any(|i| i.has_hover_rules)
    }

    /// Whether any stylesheet (or @import) is still loading.
    pub fn sheets_loading(&self) -> bool {
        self.sheets.iter().any(|s| s.pending || s.imp_pending > 0)
    }

    pub fn css_blocking(&self) -> bool {
        self.sheets.iter().any(|s| (s.pending && s.blocking) || s.imp_pending > 0) && self.first_paint_deadline.map(|d| Instant::now() < d).unwrap_or(false)
    }

    /// A sheet owner's media/disabled/rel changed: rebuild the rule index.
    pub fn restyle_sheets(&mut self) {
        { self.sheets_dirty = true; crate::page::full_why(file!(), line!()); }
        self.need_style = true;
        { self.full_style = true; crate::page::full_why(file!(), line!()); }
    }

    /// Does the `media` attribute of a `<link>`/`<style>` match right now?
    fn node_media_matches(&self, n: NodeId) -> bool {
        match self.doc.attr(n, "media").map(str::trim).filter(|m| !m.is_empty()) {
            Some(m) => css::media_matches(&css::tokenizer::parse_cvs(m), &self.media_env()),
            None => true,
        }
    }

    /// Whether a style sheet's owner node currently contributes rules: still in
    /// the document, not disabled, still `rel=stylesheet`, and media matching.
    fn sheet_node_active(&self, n: NodeId) -> bool {
        if !self.doc.is_connected(n) {
            return false;
        }
        if self.doc.is_tag(n, "link") {
            let rel = self.doc.attr(n, "rel").unwrap_or("").to_ascii_lowercase();
            if !rel.split_whitespace().any(|r| r == "stylesheet") || self.doc.attr(n, "disabled").is_some() {
                return false;
            }
        }
        self.node_media_matches(n)
    }

    /// An `<img>`'s source may have changed (or it was created by script):
    /// start loading it and arrange its load/error event.
    pub fn img_src_changed(&mut self, n: NodeId) {
        // Images in documents that aren't active (DOMParser, createHTMLDocument,
        // template contents) don't load.
        if self.doc.owner_doc(n) != DOCUMENT {
            return;
        }
        let root = self.doc.root_of(n);
        if root != DOCUMENT && self.doc.template_contents.values().any(|&f| f == root) {
            return;
        }
        // Setting src/srcset (even to the same value) runs "update the image
        // data" again: a new load/error event fires.
        self.img_fired.remove(&n);
        if let Some((u, d)) = self.node_img_url(n) {
            self.watch_img(u.clone(), n);
            self.request_image(u, d);
        }
    }

    /// Attribute changes that are "relevant mutations" for an img beyond
    /// src/srcset (handled by the caller): a CORS or referrer policy state
    /// change, or a change to a `<source>` before it in its `<picture>`.
    pub fn img_attr_mutation(&mut self, id: NodeId, name: &str, old: Option<&str>, new: Option<&str>) {
        fn cors(v: Option<&str>) -> u8 {
            match v {
                None => 0,
                Some(v) if v.eq_ignore_ascii_case("use-credentials") => 2,
                Some(_) => 1,
            }
        }
        fn refpol(v: Option<&str>) -> String {
            const K: [&str; 8] = ["no-referrer", "no-referrer-when-downgrade", "same-origin", "origin", "strict-origin", "origin-when-cross-origin", "strict-origin-when-cross-origin", "unsafe-url"];
            v.map(|v| v.to_ascii_lowercase()).filter(|v| K.contains(&v.as_str())).unwrap_or_default()
        }
        if self.doc.is_tag(id, "img") {
            let changed = match name {
                "crossorigin" => cors(old) != cors(new),
                "referrerpolicy" => refpol(old) != refpol(new),
                "sizes" => self.doc.attr(id, "srcset").is_some(),
                _ => false,
            };
            if changed && (self.doc.attr(id, "src").is_some() || self.doc.attr(id, "srcset").is_some()) {
                self.img_src_changed(id);
            }
        } else if self.doc.is_tag(id, "source") && matches!(name, "srcset" | "sizes" | "media" | "type") {
            if let Some(par) = self.doc.parent(id).filter(|&p| self.doc.is_tag(p, "picture")) {
                let kids: Vec<NodeId> = self.doc.children(par).to_vec();
                if let Some(pos) = kids.iter().position(|&k| k == id) {
                    if let Some(&img) = kids[pos + 1..].iter().find(|&&k| self.doc.is_tag(k, "img")) {
                        self.img_src_changed(img);
                    }
                }
            }
        }
    }

    fn watch_img(&mut self, url: String, n: NodeId) {
        let v = self.img_nodes.entry(url.clone()).or_default();
        if !v.contains(&n) {
            v.push(n);
        }
        // Already settled (cached): the event still fires, asynchronously.
        if matches!(self.res.images.get(&url), Some(ImgState::Ready(_)) | Some(ImgState::Failed)) {
            self.fire_img_events(&url);
        }
    }

    fn fire_img_events(&mut self, url: &str) {
        let ok = matches!(self.res.images.get(url), Some(ImgState::Ready(_)));
        let failed = matches!(self.res.images.get(url), Some(ImgState::Failed));
        if !ok && !failed {
            return;
        }
        let nodes = self.img_nodes.get(url).cloned().unwrap_or_default();
        for n in nodes {
            if self.img_fired.get(&n).map(|u| u == url).unwrap_or(false) {
                continue;
            }
            // Only if this is still the node's current source.
            if self.node_img_url(n).map(|(u, _)| u == url).unwrap_or(false) {
                self.img_fired.insert(n, url.to_string());
                self.js_tasks.push(JsTask::Dispatch { target: n, ty: if ok { "load" } else { "error" }.into(), init: EventInit::default(), then: DefaultAction::None });
            }
        }
    }

    pub(crate) fn request_image(&mut self, url: String, density: f32) {
        if self.res.images.contains_key(&url) {
            return;
        }
        let parsed = match Url::parse(&url) {
            Ok(u) => u,
            Err(_) => return,
        };
        if self.res.images.len() > 3000 {
            return;
        }
        self.res.images.insert(url.clone(), ImgState::Loading);
        // `loading=lazy` images don't delay the load event.
        if self.lazy_wanted.contains(&url) {
            self.lazy_imgs.insert(url.clone());
        } else {
            self.pending += 1;
        }
        self.total_requests += 1;
        let tx = self.tx.clone();
        let gen = self.nav_gen;
        net::fetch_async(Request::get(parsed, RequestKind::Image, Some(self.doc.url.clone())), move |r| {
            let r = r.ok().filter(|r| r.ok() || r.url.scheme() == "data");
            let img = r.as_ref().and_then(|r| resources::decode_image(&r.body, &r.content_type(), r.url.as_str(), density)).map(Arc::new);
            let is_doc = img.is_none() && r.as_ref().map(|r| {
                let ct = r.content_type().to_ascii_lowercase();
                ct.starts_with("text/html") || ct.starts_with("text/plain") || ct.contains("xml")
            }).unwrap_or(false);
            if is_doc {
                let _ = tx.send(PageMsg::Net(gen, NetMsg::ImageDoc(url)));
            } else {
                let _ = tx.send(PageMsg::Net(gen, NetMsg::Image(url, img)));
            }
        });
    }

    /// `<link rel=preload href as=…>`: fetch it (warming the cache) and fire
    /// `load`/`error` on the element, which pages use to swap in stylesheets.
    pub fn preload_link(&mut self, n: NodeId) {
        let Some(href) = self.doc.attr(n, "href").map(|h| h.trim().to_string()).filter(|h| !h.is_empty()) else { return };
        let Some(url) = self.doc.resolve_url(&href) else { return };
        if !matches!(url.scheme(), "http" | "https" | "data") {
            return;
        }
        let kind = match self.doc.attr(n, "as").unwrap_or("").to_ascii_lowercase().as_str() {
            "style" => RequestKind::Stylesheet,
            "script" => RequestKind::Script,
            "font" => RequestKind::Font,
            "image" => RequestKind::Image,
            "fetch" => RequestKind::Fetch,
            "" => return,
            _ => RequestKind::Fetch,
        };
        self.pending += 1;
        let tx = self.tx.clone();
        let gen = self.nav_gen;
        net::fetch_async(Request::get(url, kind, Some(self.doc.url.clone())), move |r| {
            let ok = r.map(|r| r.ok() || r.url.scheme() == "data").unwrap_or(false);
            let _ = tx.send(PageMsg::Net(gen, NetMsg::Preload(n, ok)));
        });
    }

    /// Fetch an external SVG document for `<use href="file.svg#id">`.
    fn request_svg_doc(&mut self, url: String) {
        if self.res.svg_docs.contains_key(&url) || self.res.svg_docs.len() > 200 {
            return;
        }
        let Ok(parsed) = Url::parse(&url) else { return };
        self.res.svg_docs.insert(url.clone(), resources::SvgDocState::Loading);
        self.pending += 1;
        self.total_requests += 1;
        let tx = self.tx.clone();
        let gen = self.nav_gen;
        net::fetch_async(Request::get(parsed, RequestKind::Image, Some(self.doc.url.clone())), move |r| {
            let d = r.ok().filter(|r| r.ok() || r.url.scheme() == "data").and_then(|r| resources::SvgDoc::parse(String::from_utf8_lossy(&r.body).into_owned())).map(Arc::new);
            let _ = tx.send(PageMsg::Net(gen, NetMsg::SvgDoc(url, d)));
        });
    }

    fn on_image(&mut self, url: String, img: Option<Arc<ImageData>>) {
        if !self.lazy_imgs.remove(&url) {
            self.pending = self.pending.saturating_sub(1);
        }
        match img {
            Some(i) => {
                self.res.bytes += i.bytes();
                self.res.images.insert(url.clone(), ImgState::Ready(i));
            }
            None => {
                self.res.images.insert(url.clone(), ImgState::Failed);
            }
        }
        self.fire_img_events(&url);
        self.need_layout = true;
        self.update_progress();
    }

    /// After styling: discover images (img, CSS backgrounds) and web fonts to load.
    /// Request the images and web fonts the styled document uses (after an
    /// incremental restyle, only those of the restyled subtrees).
    fn discover_resources(&mut self, dirty: Option<&HashMap<NodeId, u8>>) {
        let (wanted, used, img_pairs, all, svg_wanted, lazy) = {
            let styles = match &self.styles {
                Some(s) => s,
                None => return,
            };
            let mut wanted: Vec<(String, f32)> = Vec::new();
            let mut lazy: Vec<String> = Vec::new();
            let mut svg_wanted: Vec<String> = Vec::new();
            let mut img_pairs: Vec<(String, NodeId)> = Vec::new();
            let all: Vec<NodeId> = match dirty {
                Some(roots) => {
                    let mut v: Vec<NodeId> = Vec::new();
                    let mut seen: HashSet<NodeId> = HashSet::new();
                    for (&r, &fl) in roots {
                        let mut tops = vec![r];
                        if fl & css::DIRTY_SIBLINGS != 0 {
                            let mut c = self.doc.next_sibling(r);
                            while let Some(x) = c {
                                tops.push(x);
                                c = self.doc.next_sibling(x);
                            }
                        }
                        for t in tops {
                            if seen.insert(t) {
                                v.push(t);
                                v.extend(self.doc.shadow_including_descendants(t));
                            }
                        }
                    }
                    // Mutated elements whose style didn't need redoing (an
                    // image's new src…).
                    for &n in &self.style_sites {
                        if seen.insert(n) {
                            v.push(n);
                        }
                    }
                    v
                }
                None if self.doc.shadow_roots.is_empty() => self.doc.descendants(DOCUMENT).collect(),
                None => self.doc.shadow_including_descendants(DOCUMENT),
            };
            for &n in &all {
                let st = match styles.get(n) {
                    Some(s) => s,
                    None => continue,
                };
                if st.display == css::style::Display::None {
                    continue;
                }
                if let Some(el) = self.doc.element(n) {
                    if el.name == "object" || el.name == "embed" {
                        // Only image resources are supported; others fall back.
                        let ty = el.attr("type").unwrap_or("").to_ascii_lowercase();
                        if ty.is_empty() || ty.starts_with("image/") || ty == "text/html" {
                            if let Some((u, d)) = resources::img_url(&self.doc, n, self.dpr) {
                                let data_mime_ok = !u.starts_with("data:") || u[5..].to_ascii_lowercase().starts_with("image/");
                                if data_mime_ok {
                                    wanted.push((u, d));
                                }
                            }
                        }
                    } else if el.name == "img" || (el.name == "input" && el.attr("type").map(|t| t.eq_ignore_ascii_case("image")).unwrap_or(false)) {
                        if let Some((u, d)) = resources::img_url(&self.doc, n, self.dpr) {
                            if el.name == "img" {
                                img_pairs.push((u.clone(), n));
                                if el.attr("loading").is_some_and(|l| l.trim().eq_ignore_ascii_case("lazy")) {
                                    lazy.push(u.clone());
                                }
                            }
                            wanted.push((u, d));
                        }
                    } else if el.name == "video" {
                        if let Some(p) = el.attr("poster").and_then(|p| self.doc.resolve_url(p)) {
                            wanted.push((p.to_string(), 1.0));
                        }
                    } else if el.name == "use" {
                        if let Some((base, _)) = el.attr("href").or(el.attr("xlink:href")).and_then(|h| h.trim().split_once('#')) {
                            if !base.is_empty() {
                                if let Some(u) = self.doc.resolve_url(base) {
                                    svg_wanted.push(u.to_string());
                                }
                            }
                        }
                    }
                }
                if let Some(layers) = &st.background {
                    for l in layers.iter() {
                        if let css::style::BgImage::Url(u) = &l.image {
                            if let Some(abs) = self.doc.resolve_url(u) {
                                wanted.push((abs.to_string(), 1.0));
                            }
                        }
                    }
                }
                if let Some(bi) = &st.border_image {
                    if let css::style::BgImage::Url(u) = &bi.source {
                        if let Some(abs) = self.doc.resolve_url(u) {
                            wanted.push((abs.to_string(), 1.0));
                        }
                    }
                }
                if let Some(u) = &st.list_style_image {
                    if let Some(abs) = self.doc.resolve_url(u) {
                        wanted.push((abs.to_string(), 1.0));
                    }
                }
                if let Some(m) = &st.mask {
                    if let css::style::BgImage::Url(u) = &m.image {
                        if let Some(abs) = self.doc.resolve_url(u) {
                            wanted.push((abs.to_string(), 1.0));
                        }
                    }
                }
            }
            for (_, ps) in styles.before.iter().chain(styles.after.iter()).chain(styles.marker.iter()) {
                if let Some(c) = &ps.content {
                    for it in c.iter() {
                        if let css::style::ContentItem::Url(u) = it {
                            if let Some(abs) = self.doc.resolve_url(u) {
                                wanted.push((abs.to_string(), 1.0));
                            }
                        }
                    }
                }
                if let Some(layers) = &ps.background {
                    for l in layers.iter() {
                        if let css::style::BgImage::Url(u) = &l.image {
                            if let Some(abs) = self.doc.resolve_url(u) {
                                wanted.push((abs.to_string(), 1.0));
                            }
                        }
                    }
                }
                if let Some(m) = &ps.mask {
                    if let css::style::BgImage::Url(u) = &m.image {
                        if let Some(abs) = self.doc.resolve_url(u) {
                            wanted.push((abs.to_string(), 1.0));
                        }
                    }
                }
            }
            let mut used: HashSet<(String, u16, bool)> = HashSet::new();
            if !self.declared_fonts.is_empty() || !self.js_font_families.is_empty() {
                // Distinct styles only (text nodes and siblings share them).
                let mut seen: HashSet<usize> = HashSet::new();
                let scan: Box<dyn Iterator<Item = &Rc<css::style::ComputedStyle>>> = match dirty {
                    Some(_) => Box::new(all.iter().filter_map(|&n| styles.get(n))),
                    None => Box::new(styles.map.iter().flatten()),
                };
                for s in scan {
                    if !seen.insert(Rc::as_ptr(s) as usize) {
                        continue;
                    }
                    let italic = s.font_style != css::style::FontStyle::Normal;
                    for f in s.font_family.iter().take(3) {
                        used.insert((f.to_ascii_lowercase(), s.font_weight, italic));
                    }
                }
                for s in styles.before.values().chain(styles.after.values()) {
                    let italic = s.font_style != css::style::FontStyle::Normal;
                    for f in s.font_family.iter().take(3) {
                        used.insert((f.to_ascii_lowercase(), s.font_weight, italic));
                    }
                }
            }
            (wanted, used, img_pairs, all, svg_wanted, lazy)
        };
        // Once lazy, an image never delays the load event (even if made eager later).
        self.lazy_wanted.extend(lazy);
        for (u, d) in wanted {
            self.request_image(u, d);
        }
        for u in svg_wanted {
            self.request_svg_doc(u);
        }
        for (u, n) in img_pairs {
            if !self.img_fired.get(&n).map(|f| *f == u).unwrap_or(false) {
                self.watch_img(u, n);
            }
        }
        // Web fonts: load faces for families actually used.
        let mut to_load: Vec<css::FontFace> = Vec::new();
        // Families with a declared face (case-insensitive), so the page's
        // characters are only gathered when some face could be loaded.
        static DEBUG_FONTS: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("OHB_DEBUG_FONTS").is_some());
        if *DEBUG_FONTS {
            eprintln!("[fonts] scan dirty={} declared={} used={:?}", dirty.is_some(), self.declared_fonts.len(), used.iter().take(12).collect::<Vec<_>>());
        }
        // Script-added FontFaces load when their family is used.
        if !self.js_font_families.is_empty() {
            let fams: Vec<String> = used.iter().map(|u| u.0.clone()).filter(|f| self.js_font_families.contains(f)).collect();
            let new: Vec<crate::js::host::HostValue> = fams.into_iter().filter(|f| self.js_fonts_reported.insert(f.clone())).map(|f| crate::js::host::HostValue::from(f.as_str())).collect();
            if !new.is_empty() {
                self.js_tasks.push(JsTask::CallGlobal("__ohb_fontsUsed", vec![crate::js::host::HostValue::Array(new)]));
            }
        }
        let used: Vec<(String, u16, bool)> = used.into_iter().filter(|(fam, _, _)| self.declared_fonts.iter().any(|f| f.family.eq_ignore_ascii_case(fam))).collect();
        let page_chars: HashSet<u32> = if used.is_empty() {
            HashSet::new()
        } else {
            let mut set = HashSet::new();
            let texts: Box<dyn Iterator<Item = NodeId>> = match dirty {
                Some(_) => Box::new(all.iter().copied()),
                None => Box::new(self.doc.descendants(DOCUMENT)),
            };
            for n in texts {
                if let NodeData::Text(t) = &self.doc.node(n).data {
                    if let Some(p) = self.doc.parent(n) {
                        if matches!(self.doc.tag(p), "script" | "style" | "template" | "noscript") {
                            continue;
                        }
                    }
                    for ch in t.chars() {
                        if set.len() > 4000 {
                            break;
                        }
                        set.insert(ch as u32);
                    }
                }
            }
            set
        };
        for (fam, w, italic) in used {
            let candidates: Vec<&css::FontFace> = self.declared_fonts.iter().filter(|f| f.family.to_ascii_lowercase() == fam).collect();
            if candidates.is_empty() {
                continue;
            }
            let best = best_font_face(candidates.iter().copied(), w, italic).cloned();
            if let Some(best) = best {
                // All unicode-range subsets of that weight/style intersecting the page's text.
                let group: Vec<css::FontFace> = self
                    .declared_fonts
                    .iter()
                    .filter(|f| f.family.to_ascii_lowercase() == fam && f.weight == best.weight && f.style == best.style && f.variant == best.variant)
                    .filter(|f| f.ranges.is_empty() || f.ranges.iter().any(|(a, b)| page_chars.iter().any(|c| c >= a && c <= b)) || f.ranges.iter().any(|(a, b)| *a <= 0x61 && *b >= 0x61))
                    .cloned()
                    .collect();
                // A face redeclared with the same unicode-range (a stylesheet
                // included twice with different file hashes): only the last
                // one is used, so don't load the others.
                let group: Vec<css::FontFace> = group.iter().enumerate().filter(|(i, f)| !group[i + 1..].iter().any(|g| g.ranges == f.ranges)).map(|(_, f)| f.clone()).collect();
                for ff in group {
                    let key = font_face_key(&ff);
                    if self.requested_fonts.insert(key) {
                        to_load.push(ff);
                    }
                }
            }
        }
        for ff in to_load {
            self.load_font(ff);
        }
    }

    /// CSS Font Loading `FontFaceSet.load()`: start loading the declared faces of
    /// `family` matching weight/style (and covering `text`'s characters).
    /// Returns their keys.
    pub fn load_font_family(&mut self, family: &str, weight: u16, italic: bool, text: &str) -> Vec<String> {
        let cands: Vec<css::FontFace> = self.declared_fonts.iter().filter(|f| f.family.eq_ignore_ascii_case(family)).cloned().collect();
        let Some(best) = best_font_face(cands.iter(), weight, italic).cloned() else {
            return Vec::new();
        };
        let chars: Vec<u32> = if text.is_empty() { vec![0x20] } else { text.chars().map(|c| c as u32).collect() };
        let mut keys = Vec::new();
        for ff in cands.into_iter().filter(|f| f.weight == best.weight && f.style == best.style) {
            if !(ff.ranges.is_empty() || ff.ranges.iter().any(|(a, b)| chars.iter().any(|c| c >= a && c <= b))) {
                continue;
            }
            let key = font_face_key(&ff);
            if self.requested_fonts.insert(key.clone()) {
                self.load_font(ff);
            }
            keys.push(key);
        }
        keys
    }

    /// Declared @font-face rules for `document.fonts`: (key, family, weight
    /// range, italic, unicode-range text, status).
    pub fn declared_font_list(&self) -> Vec<(String, String, (u16, u16), bool, String, u8)> {
        self.declared_fonts
            .iter()
            .map(|f| {
                let key = font_face_key(f);
                let st = self.font_status.get(&key).copied().unwrap_or(0);
                let ur = f.ranges.iter().map(|(a, b)| if a == b { format!("U+{a:X}") } else { format!("U+{a:X}-{b:X}") }).collect::<Vec<_>>().join(", ");
                (key, f.family.clone(), f.weight, f.style != css::style::FontStyle::Normal, ur, st)
            })
            .collect()
    }

    pub(crate) fn load_font(&mut self, ff: css::FontFace) {
        // Prefer woff2 > woff > ttf/otf.
        let rank = |fmt: &str, url: &str| {
            let u = url.to_ascii_lowercase();
            if fmt.contains("woff2") || u.contains(".woff2") {
                0
            } else if fmt.contains("woff") || u.contains(".woff") {
                1
            } else if fmt.contains("truetype") || fmt.contains("opentype") || u.contains(".ttf") || u.contains(".otf") {
                2
            } else if fmt.contains("svg") || fmt.contains("embedded-opentype") || u.contains(".eot") || u.contains(".svg") {
                9
            } else {
                3
            }
        };
        let mut srcs = ff.sources.clone();
        srcs.sort_by_key(|(u, f)| rank(f, u));
        let (src, fmt) = match srcs.first() {
            Some(s) => s.clone(),
            None => return,
        };
        if rank(&fmt, &src) >= 9 {
            return;
        }
        let url = match ff.base.join(&src) {
            Ok(u) => u,
            Err(_) => return,
        };
        let face_key = font_face_key(&ff);
        self.font_status.insert(face_key.clone(), 1);
        self.pending += 1;
        self.total_requests += 1;
        let tx = self.tx.clone();
        let gen = self.nav_gen;
        let fam = ff.family.clone();
        let weight = (ff.weight.0 + ff.weight.1) / 2;
        let wr = ff.weight;
        let ranges_s: String = ff.ranges.iter().map(|(a, b)| format!("{a:x}-{b:x}")).collect::<Vec<_>>().join(",");
        let style = ff.style;
        net::fetch_async(Request::get(url, RequestKind::Font, Some(self.doc.url.clone())), move |r| {
            let bytes = r.ok().filter(|r| r.ok()).map(|r| (*r.body).clone());
            let _ = weight;
            let _ = tx.send(PageMsg::Net(gen, NetMsg::Font(format!("{fam}\u{0}{}\u{0}{}\u{0}{ranges_s}\u{0}{face_key}", wr.0, wr.1), weight, style, bytes)));
        });
    }

    fn on_font(&mut self, key: String, style: css::style::FontStyle, bytes: Option<Vec<u8>>) {
        self.pending = self.pending.saturating_sub(1);
        let mut parts = key.split('\u{0}');
        let fam = parts.next().unwrap_or("").to_string();
        let w0: u16 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(400);
        let w1: u16 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(400);
        let ranges = css::parse_unicode_range(parts.next().unwrap_or(""));
        let face_key = parts.next().unwrap_or("").to_string();
        let mut ok = false;
        if let Some(b) = bytes {
            if self.fonts.add_web_font(&fam, (w0, w1), style, &b, ranges) {
                self.need_layout = true;
                ok = true;
                // Same-origin frames may be using it too.
                for fv in self.frame_views.values_mut() {
                    if fv.local.is_some() {
                        fv.version += 1;
                    }
                }
            }
        }
        if !face_key.is_empty() {
            self.font_status.insert(face_key.clone(), if ok { 2 } else { 3 });
            self.js_tasks.push(JsTask::CallGlobal("__ohb_fontLoaded", vec![crate::js::host::HostValue::from(face_key.as_str()), crate::js::host::HostValue::from(ok)]));
        }
        self.update_progress();
    }

    // ------------------------------------------------------------- rendering

    pub fn mark_dom_dirty(&mut self) {
        if self.doc.generation != self.last_gen {
            self.last_gen = self.doc.generation;
            self.need_style = true;
        }
    }

    /// Queue animation events for script and schedule the next frame.
    fn after_animation_pass(&mut self, now: f64, cost_ms: f64) {
        for ev in std::mem::take(&mut self.anim.events) {
            // (Silent transitions: a native shorthand the longhands report.)
            if ev.name.is_empty() && ev.kind.starts_with("transition") {
                continue;
            }
            let (node, pseudo) = if ev.node & css::ANIM_BEFORE != 0 {
                (ev.node & !css::ANIM_BEFORE, "::before")
            } else if ev.node & css::ANIM_AFTER != 0 {
                (ev.node & !css::ANIM_AFTER, "::after")
            } else {
                (ev.node, "")
            };
            if self.doc.valid(node) {
                self.js_tasks.push(JsTask::AnimEvent { target: node, ty: ev.kind.to_string(), name: ev.name, elapsed: ev.elapsed, pseudo: pseudo.to_string() });
            }
        }
        // Frames while something moves (slower when styling is expensive), else
        // a wake-up when a delayed effect begins.
        self.anim_due = if self.anim.running { Some(now + (cost_ms * 3.0).clamp(16.0, 100.0)) } else { self.anim.next_wake };
    }

    /// Restyle when an animation frame is due. Returns the time until the next one.
    pub fn anim_tick(&mut self) -> Option<Duration> {
        let due = self.anim_due?;
        let now = crate::js::host::page_now_ms();
        if now >= due {
            self.anim_due = None;
            self.mark_dom_dirty();
            if !self.need_style && self.anim.paint_only && self.styles.is_some() && self.layout.is_some() && !self.need_layout {
                self.anim_paint_frame(now);
            } else {
                self.need_style = true;
                { self.full_style = true; crate::page::full_why(file!(), line!()); }
            }
            return None;
        }
        Some(Duration::from_secs_f64((due - now) / 1000.0))
    }

    /// An animation frame where only paint properties move: re-animate the
    /// animated elements from their remembered base styles and patch the
    /// styles of their boxes; no restyle or relayout.
    fn anim_paint_frame(&mut self, now: f64) {
        let t0 = Instant::now();
        let Some(index) = self.index.clone() else {
            self.need_style = true;
            { self.full_style = true; crate::page::full_why(file!(), line!()); }
            return;
        };
        let env = self.media_env();
        self.anim.begin_frame(now);
        let keys: Vec<NodeId> = self.anim.bases.keys().copied().collect();
        let mut updates: Vec<(NodeId, Rc<css::style::ComputedStyle>)> = Vec::new();
        {
            let styles = self.styles.as_ref().unwrap();
            for key in keys {
                let b = self.anim.bases[&key].clone();
                let node = key & !(css::ANIM_BEFORE | css::ANIM_AFTER);
                let prev = if key & css::ANIM_BEFORE != 0 { styles.before.get(&node) } else if key & css::ANIM_AFTER != 0 { styles.after.get(&node) } else { styles.get(node) };
                let prev = prev.cloned();
                let kf = |name: &str| index.keyframes_for(name, &env);
                let k = css::anim::KfCtx { parent: &b.parent, cx: &b.cx, keyframes: &kf };
                let st = match self.anim.apply(key, &b.base, prev.as_deref(), &k) {
                    Some(s) => Rc::new(s),
                    None => b.base.clone(),
                };
                updates.push((key, st));
            }
        }
        if !self.anim.paint_only {
            // Something now affects layout: do it properly.
            self.need_style = true;
            { self.full_style = true; crate::page::full_why(file!(), line!()); }
            return;
        }
        let styles = self.styles.as_mut().unwrap();
        let tree = self.layout.as_mut().unwrap();
        let mut by_key: HashMap<NodeId, Rc<css::style::ComputedStyle>> = HashMap::new();
        for (key, st) in updates {
            let node = key & !(css::ANIM_BEFORE | css::ANIM_AFTER);
            if key & css::ANIM_BEFORE != 0 {
                styles.before.insert(node, st.clone());
            } else if key & css::ANIM_AFTER != 0 {
                styles.after.insert(node, st.clone());
            } else if let Some(slot) = styles.map.get_mut(node) {
                *slot = Some(st.clone());
            }
            by_key.insert(key, st);
        }
        for b in tree.boxes.iter_mut() {
            let Some(n) = b.node else { continue };
            let key = match b.pseudo {
                1 => n | css::ANIM_BEFORE,
                2 => n | css::ANIM_AFTER,
                0 => n,
                _ => continue,
            };
            if let Some(st) = by_key.get(&key) {
                b.style = st.clone();
            }
        }
        self.after_animation_pass(now, t0.elapsed().as_secs_f64() * 1000.0);
        self.need_paint = true;
    }

    /// The subtrees an incremental restyle must redo, or `None` when the
    /// next pass has to be a full one.
    fn incremental_roots(&mut self, index: &RuleIndex, snap: &StyleSnap, target: Option<NodeId>) -> Option<HashMap<NodeId, u8>> {
        use crate::css::{DIRTY_SELF, DIRTY_SIBLINGS, DIRTY_SUBTREE, INV_DESC, INV_SELF, INV_SIB};
        use crate::dom::StyleRecord;
        // Shadow trees whose rule index changed (adopted sheets, a new shadow
        // root, its <style>s): restyle just their hosts' subtrees.
        let mut shadow_hosts: Vec<NodeId> = Vec::new();
        let snap_ok = match self.style_snap.as_ref() {
            Some(o) if o == snap => true,
            Some(o) if o.eq_except_shadow(snap) => {
                let old: HashMap<NodeId, usize> = o.shadow.iter().copied().collect();
                let new: HashMap<NodeId, usize> = snap.shadow.iter().copied().collect();
                for (r, ptr) in old.iter().chain(new.iter()) {
                    let changed = old.get(r) != new.get(r) || old.get(r) != Some(ptr);
                    if changed {
                        if let Some(h) = self.doc.host_of(*r) {
                            if !shadow_hosts.contains(&h) {
                                shadow_hosts.push(h);
                            }
                        }
                    }
                }
                shadow_hosts.len() <= 64
            }
            _ => false,
        };
        let ok = !self.full_style
            && self.styles.is_some()
            && snap_ok
            && self.doc.generation.wrapping_sub(self.snap_gen) == self.doc.style_bumps.wrapping_sub(self.snap_bumps)
            && !self.has_log.contains_key(&(usize::MAX, 0))
            && !no_incremental_style();
        if !ok {
            if std::env::var("OHB_STYLE_STATS").as_deref() == Ok("5") {
                let sn = self.style_snap.as_ref();
                eprintln!(
                    "[style-full] haslog={} shadowhas={} full={} styles={} snap_eq={} (index {} shadow {} env {} hover {} active {} focus {} target {} containers {} quirks {}) gen={}",
                    self.has_log.contains_key(&(usize::MAX, 0)),
                    self.shadow_indexes.values().any(|i| i.has_has),
                    self.full_style,
                    self.styles.is_some(),
                    sn == Some(snap),
                    sn.map(|o| o.index == snap.index).unwrap_or(false),
                    sn.map(|o| o.shadow == snap.shadow).unwrap_or(false),
                    sn.map(|o| o.env == snap.env).unwrap_or(false),
                    sn.map(|o| o.hover == snap.hover).unwrap_or(false),
                    sn.map(|o| o.active == snap.active).unwrap_or(false),
                    sn.map(|o| o.focus == snap.focus).unwrap_or(false),
                    sn.map(|o| o.target == snap.target).unwrap_or(false),
                    sn.map(|o| o.containers == snap.containers).unwrap_or(false),
                    sn.map(|o| o.quirks == snap.quirks).unwrap_or(false),
                    self.doc.generation.wrapping_sub(self.snap_gen) == self.doc.style_bumps.wrapping_sub(self.snap_bumps)
                );
            }
            return None;
        }
        // Every rule index that can style the document (shadow trees' too).
        let mut indexes: Vec<&RuleIndex> = vec![index];
        let mut seen_idx: HashSet<usize> = HashSet::new();
        for i in self.shadow_indexes.values() {
            if seen_idx.insert(Rc::as_ptr(i) as usize) {
                indexes.push(i);
            }
        }
        let quirks = self.doc.quirks;
        let inv = |map: fn(&RuleIndex) -> &HashMap<String, u8>, key: &str| -> u8 { indexes.iter().map(|i| map(i).get(key).copied().unwrap_or(0)).fold(0, |a, b| a | b) };
        // INV_* relations -> DIRTY_* flags for the element carrying the feature.
        let to_flags = |k: u8| -> u8 {
            let mut fl = 0;
            if k & INV_SELF != 0 {
                fl |= DIRTY_SELF;
            }
            if k & INV_DESC != 0 {
                fl |= DIRTY_SUBTREE;
            }
            if k & INV_SIB != 0 {
                fl |= DIRTY_SIBLINGS;
            }
            fl
        };
        let dbg = std::env::var("OHB_STYLE_STATS").as_deref() == Ok("3");
        let mut roots: HashMap<NodeId, u8> = HashMap::new();
        let mut sites: Vec<NodeId> = Vec::new();
        let mut marks: Vec<(NodeId, u8)> = Vec::new();
        for h in shadow_hosts {
            if self.doc.valid(h) {
                self.style_sites.push(h);
                *roots.entry(h).or_insert(0) |= DIRTY_SELF | DIRTY_SUBTREE;
                sites.push(h);
            }
        }
        for rec in &self.doc.style_dirty {
            if dbg {
                let n = match rec {
                    StyleRecord::Children(n) | StyleRecord::Inserted(n, _) | StyleRecord::Removed(n, _) | StyleRecord::Attr(n, ..) | StyleRecord::State(n) | StyleRecord::Text(n) => *n,
                };
                eprintln!("[style-rec] {:?} <{} id={:?} class={:?}>", rec, self.doc.tag(n), self.doc.attr(n, "id"), self.doc.attr(n, "class").map(|c| c.chars().take(60).collect::<String>()));
            }
            marks.clear();
            match rec {
                StyleRecord::Children(p) | StyleRecord::Inserted(p, _) | StyleRecord::Removed(p, _) => {
                    let p = *p;
                    if !self.doc.valid(p) {
                        continue;
                    }
                    let inserted = match rec {
                        StyleRecord::Inserted(_, x) => Some(*x),
                        _ => None,
                    };
                    // Slot assignment (shadow hosts and trees) is not tracked
                    // per node: restyle the whole host.
                    let host = if self.doc.shadow_roots.contains_key(&p) {
                        Some(p)
                    } else {
                        let r = self.doc.root_of(p);
                        if self.doc.is_shadow_root(r) && (inserted.is_none() || inserted.is_some_and(|x| self.doc.is_tag(x, "slot") || self.doc.descendants(x).any(|d| self.doc.is_tag(d, "slot")))) {
                            self.doc.host_of(r)
                        } else {
                            None
                        }
                    };
                    if let Some(h) = host {
                        marks.push((h, DIRTY_SUBTREE));
                        sites.push(h);
                    }
                    // What matching of the parent and its children depended on.
                    use css::selector::{STRUCT_BWD, STRUCT_EMPTY, STRUCT_FWD, STRUCT_SIB};
                    let pl = self.struct_log.get(&p).copied().unwrap_or(0);
                    let k_if = |cat: u32| ((pl >> cat) & 7) as u8;
                    if dbg {
                        eprintln!("[style-struct] {} fwd={} bwd={} sib={} empty={}", p, k_if(STRUCT_FWD), k_if(STRUCT_BWD), k_if(STRUCT_SIB), k_if(STRUCT_EMPTY));
                    }
                    // The parent's own `:empty`; inserted (or moved) nodes.
                    marks.push((p, to_flags(k_if(STRUCT_EMPTY))));
                    sites.push(p);
                    let kids: Vec<NodeId> = self.doc.element_children(p).collect();
                    // Where a removed child was: before the next element still in place.
                    let removed_at = match rec {
                        StyleRecord::Removed(_, next) => match next {
                            None => Some(kids.len()),
                            Some(nx) if self.doc.parent(*nx) == Some(p) => {
                                let mut c = Some(*nx);
                                while let Some(x) = c.filter(|&x| !self.doc.is_element(x)) {
                                    c = self.doc.next_sibling(x);
                                }
                                match c {
                                    Some(e) => kids.iter().position(|&k| k == e),
                                    None => Some(kids.len()),
                                }
                            }
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some(i) = removed_at {
                        let before = to_flags(k_if(STRUCT_BWD));
                        let after = to_flags(k_if(STRUCT_FWD) | k_if(STRUCT_SIB));
                        for (j, &k) in kids.iter().enumerate() {
                            let f = if j < i { before } else { after };
                            if f != 0 {
                                marks.push((k, f));
                            }
                        }
                        sites.extend(kids.get(i).copied());
                        sites.extend(i.checked_sub(1).and_then(|j| kids.get(j)).copied());
                    } else {
                    match inserted.and_then(|x| kids.iter().position(|&k| k == x).map(|i| (x, i))) {
                        Some((x, i)) => {
                            marks.push((x, DIRTY_SUBTREE));
                            sites.push(x);
                            // Siblings whose position-dependent matching changed.
                            let before = to_flags(k_if(STRUCT_BWD));
                            let after = to_flags(k_if(STRUCT_FWD) | k_if(STRUCT_SIB));
                            for (j, &k) in kids.iter().enumerate() {
                                let f = if j < i { before } else if j > i { after } else { 0 };
                                if f != 0 {
                                    marks.push((k, f));
                                }
                            }
                        }
                        None => {
                            if let Some(x) = inserted {
                                // A text node: only `:empty` (done above).
                                marks.push((x, 0));
                            } else {
                                let f = to_flags(k_if(STRUCT_FWD) | k_if(STRUCT_BWD) | k_if(STRUCT_SIB));
                                for &k in &kids {
                                    if f != 0 {
                                        marks.push((k, f));
                                    }
                                    sites.push(k);
                                }
                            }
                        }
                    }
                    }
                }
                StyleRecord::State(n) => {
                    marks.push((*n, DIRTY_SUBTREE | DIRTY_SIBLINGS));
                    sites.push(*n);
                }
                StyleRecord::Text(_) => {}
                StyleRecord::Attr(n, name, old, new) => {
                    let all = INV_SELF | INV_DESC | INV_SIB;
                    let kinds = match name.as_str() {
                        "class" if !quirks => {
                            let o: HashSet<&str> = old.as_deref().unwrap_or("").split_ascii_whitespace().collect();
                            let nw: HashSet<&str> = new.as_deref().unwrap_or("").split_ascii_whitespace().collect();
                            o.symmetric_difference(&nw).map(|c| inv(|i| &i.inv.class, c)).fold(0, |a, b| a | b) | inv(|i| &i.inv.attr, "class")
                        }
                        "id" if !quirks => old.iter().chain(new.iter()).map(|v| inv(|i| &i.inv.id, v)).fold(0, |a, b| a | b) | inv(|i| &i.inv.attr, "id"),
                        "style" => INV_SELF | inv(|i| &i.inv.attr, "style"),
                        other => {
                            let lc = other.to_ascii_lowercase();
                            inv(|i| &i.inv.attr, &lc) | if style_inert_attr(&lc) { 0 } else { all }
                        }
                    };
                    marks.push((*n, to_flags(kinds)));
                    sites.push(*n);
                }
            }
            for &(n, mut fl) in &marks {
                if !self.doc.valid(n) {
                    continue;
                }
                self.style_sites.push(n);
                if fl == 0 {
                    continue;
                }
                let mut root = n;
                // A slotted node's siblings may render in other slots: restyle from the host.
                if fl & DIRTY_SIBLINGS != 0 {
                    if let Some(p) = self.doc.parent(n).filter(|p| self.doc.shadow_roots.contains_key(p)) {
                        root = p;
                        fl = DIRTY_SUBTREE;
                    }
                }
                *roots.entry(root).or_insert(0) |= fl;
            }
        }
        if dbg {
            for (n, f) in &roots {
                eprintln!("[style-root] {} <{} class={:?}> flags={}", n, self.doc.tag(*n), self.doc.attr(*n, "class").map(|c| c.chars().take(40).collect::<String>()), f);
            }
        }
        let any_has = indexes.iter().any(|i| i.has_has);
        if any_has && !sites.is_empty() {
            // Shadow hosts whose trees use `:has()`: restyle when a mutation
            // happens anywhere under them.
            let marked: HashSet<NodeId> = self.has_log.keys().filter(|k| k.1 == css::selector::HAS_HOST_MARK).map(|k| k.0).collect();
            if !marked.is_empty() {
                // (:host-context() and the like see the host's ancestors too.)
                let site_set: HashSet<NodeId> = sites.iter().copied().collect();
                for &h in &marked {
                    let mut cur = Some(h);
                    let mut k = 0;
                    while let Some(c) = cur {
                        k += 1;
                        if site_set.contains(&c) && c != h && self.doc.valid(h) {
                            *roots.entry(h).or_insert(0) |= DIRTY_SUBTREE;
                            break;
                        }
                        if k > 4096 {
                            break;
                        }
                        cur = match self.doc.parent(c) {
                            Some(p) if self.doc.is_shadow_root(p) => self.doc.host_of(p),
                            other => other,
                        };
                    }
                }
                let mut seen: HashSet<NodeId> = HashSet::new();
                for &s in &sites {
                    let mut cur = Some(s);
                    while let Some(c) = cur {
                        if !seen.insert(c) {
                            break;
                        }
                        if marked.contains(&c) && self.doc.valid(c) {
                            *roots.entry(c).or_insert(0) |= DIRTY_SUBTREE;
                        }
                        cur = match self.doc.parent(c) {
                            Some(p) if self.doc.is_shadow_root(p) => self.doc.host_of(p),
                            other => other,
                        };
                    }
                }
            }
        }
        if any_has && !self.has_log.is_empty() && !sites.is_empty() {
            // `:has()` anchors that can see a mutation: its ancestors and the
            // elements before them (and itself) among their siblings.
            let mut cand: HashSet<NodeId> = HashSet::new();
            for &s in &sites {
                let mut cur = Some(s);
                while let Some(c) = cur {
                    if !cand.insert(c) {
                        break;
                    }
                    let mut ps = self.doc.prev_element_sibling(c);
                    while let Some(x) = ps {
                        if !cand.insert(x) {
                            break;
                        }
                        ps = self.doc.prev_element_sibling(x);
                    }
                    cur = self.doc.parent(c);
                }
            }
            let st = css::selector::MatchState { hover_chain: &self.hover_chain, focus: self.doc.focused, active_chain: &self.active_chain, target, scope: None, host: None, shadow_root: None };
            let complex = indexes.iter().any(|i| i.has_complex);
            let mut changed: Vec<((NodeId, usize), (bool, u32))> = Vec::new();
            for (&(a, ptr), &(old, len)) in self.has_log.iter() {
                if ptr == css::selector::HAS_HOST_MARK || !(complex || cand.contains(&a)) || !self.doc.valid(a) {
                    continue;
                }
                // SAFETY: the log was recorded against `index` (unchanged, as
                // the snapshot compares its address) whose selectors are alive.
                let now = unsafe { css::selector::replay_has(ptr, len, &self.doc, a, &st) };
                if now != old {
                    changed.push(((a, ptr), (now, len)));
                }
            }
            for (k, v) in changed {
                self.has_log.insert(k, v);
                let a = k.0;
                match self.doc.parent(a).filter(|p| self.doc.shadow_roots.contains_key(p)) {
                    Some(host) => *roots.entry(host).or_insert(0) |= DIRTY_SUBTREE,
                    None => *roots.entry(a).or_insert(0) |= DIRTY_SUBTREE | DIRTY_SIBLINGS,
                }
            }
        }
        Some(roots)
    }

    /// Debugging (`OHB_VERIFY_STYLE=1`): compare an incremental pass with a full one.
    fn verify_styles(&mut self, styles: &css::Styles, index: &RuleIndex, env: css::MediaEnv, target: Option<NodeId>) {
        let mut cache = HashMap::new();
        let mut eng = css::StyleEngine {
            doc: &self.doc,
            index,
            env,
            hover_chain: self.hover_chain.clone(),
            active_chain: self.active_chain.clone(),
            target,
            inline_cache: &mut cache,
            root: DOCUMENT,
            descend_hidden: false,
            seed: None,
            container_sizes: Some(&self.container_sizes),
            anim: None,
            prev: None,
            shadow_indexes: Some(&self.shadow_indexes),
            dirty: None,
        };
        let full = eng.compute();
        let mut bad = 0;
        let describe = |n: NodeId| format!("{} <{} id={:?} class={:?}>", n, self.doc.tag(n), self.doc.attr(n, "id"), self.doc.attr(n, "class").map(|c| c.chars().take(50).collect::<String>()));
        let diff = |a: &css::style::ComputedStyle, b: &css::style::ComputedStyle| -> String {
            let (da, db) = (format!("{a:#?}"), format!("{b:#?}"));
            da.lines().zip(db.lines()).find(|(x, y)| x != y).map(|(x, y)| format!("{} vs {}", x.trim(), y.trim())).unwrap_or_default()
        };
        for n in 0..full.map.len().max(styles.map.len()) {
            // Animated elements (and what inherits from them) differ from a static pass.
            let mut animated = false;
            let mut cur = Some(n);
            while let Some(c) = cur {
                if self.anim.tracks(c) || self.anim.tracks(c | css::ANIM_BEFORE) || self.anim.tracks(c | css::ANIM_AFTER) {
                    animated = true;
                    break;
                }
                cur = self.doc.parent(c);
            }
            if animated {
                continue;
            }
            let a = styles.map.get(n).and_then(|s| s.as_ref());
            let b = full.map.get(n).and_then(|s| s.as_ref());
            let msg = match (a, b) {
                (Some(x), Some(y)) if **x != **y => Some(diff(x, y)),
                (Some(_), None) => Some("styled only incrementally".into()),
                (None, Some(_)) => Some("styled only in full".into()),
                _ => None,
            };
            if let Some(m) = msg {
                bad += 1;
                if bad <= 10 {
                    eprintln!("[style-verify] {}: {}", describe(n), m);
                }
            }
            for (pa, pb, what) in [(&styles.before, &full.before, "::before"), (&styles.after, &full.after, "::after")] {
                match (pa.get(&n), pb.get(&n)) {
                    (Some(x), Some(y)) if **x != **y => {
                        bad += 1;
                        if bad <= 10 {
                            eprintln!("[style-verify] {}{}: {}", describe(n), what, diff(x, y));
                        }
                    }
                    (Some(_), None) | (None, Some(_)) => {
                        bad += 1;
                        if bad <= 10 {
                            eprintln!("[style-verify] {}{}: present in only one", describe(n), what);
                        }
                    }
                    _ => {}
                }
            }
        }
        eprintln!("[style-verify] {} mismatches ({} nodes)", bad, full.map.len());
    }

    pub fn ensure_style(&mut self) {
        if self.in_frame_ctx() {
            return; // a local frame's context: its styles are current
        }
        let t0 = Instant::now();
        self.ensure_style_inner();
        phase_add(0, t0.elapsed());
    }

    /// Rebuilds the rule index if sheets changed (without restyling).
    /// Returns true if it was rebuilt.
    pub fn refresh_rule_index(&mut self) -> bool {
        // A sheet's owner node removed or re-inserted (by any DOM path)
        // changes which rules apply.
        {
            let mut sig: u64 = self.sheets.len() as u64;
            for (i, sl) in self.sheets.iter().enumerate() {
                if let Some(n) = sl.node {
                    if self.doc.valid(n) && self.doc.is_connected(n) && self.doc.in_document_tree(n) {
                        sig = sig.wrapping_mul(1_000_003).wrapping_add(i as u64 + 1);
                    }
                }
            }
            if sig != self.sheets_sig {
                self.sheets_sig = sig;
                self.sheets_dirty = true;
            }
        }
        if self.sheets_dirty || self.index.is_none() {
            let sheets = self.ordered_sheets();
            let idx = RuleIndex::new(sheets);
            self.declared_fonts = idx.font_faces();
            self.index = Some(Rc::new(idx));
            self.sheets_dirty = false;
            self.need_style = true;
            return true;
        }
        false
    }

    fn ensure_style_inner(&mut self) {
        self.mark_dom_dirty();
        self.flush_pending_sheets();
        if !self.need_style && self.styles.is_some() {
            return;
        }
        if thrash_debug() {
            use std::fmt::Write;
            let mut kinds: HashMap<String, usize> = HashMap::new();
            for r in &self.doc.style_dirty {
                let k = match r {
                    crate::dom::StyleRecord::Attr(n, name, ..) => format!("attr:{}:{}", name, self.doc.tag(*n)),
                    crate::dom::StyleRecord::Children(_) => "children".into(),
                    crate::dom::StyleRecord::Inserted(..) => "inserted".into(),
                    crate::dom::StyleRecord::Removed(..) => "removed".into(),
                    crate::dom::StyleRecord::State(_) => "state".into(),
                    crate::dom::StyleRecord::Text(_) => "text".into(),
                };
                *kinds.entry(k).or_default() += 1;
            }
            let mut v: Vec<_> = kinds.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            let mut note = String::new();
            for (k, c) in v.iter().take(4) {
                let _ = write!(note, "{k}x{c} ");
            }
            self.thrash_note.push_str(&note);
        }
        self.refresh_rule_index();
        let index = self.index.clone().unwrap();
        self.build_shadow_indexes();
        let target = self.doc.url.fragment().and_then(|f| self.doc.get_element_by_id(f));
        let env = self.media_env();
        resources::set_picture_env(env);
        let mut containers: Vec<(NodeId, (f32, f32))> = self.container_sizes.iter().map(|(&k, &v)| (k, v)).collect();
        containers.sort_unstable_by_key(|c| c.0);
        let mut shadow: Vec<(NodeId, usize)> = self.shadow_indexes.iter().map(|(&k, v)| (k, Rc::as_ptr(v) as usize)).collect();
        shadow.sort_unstable();
        let snap = StyleSnap {
            index: Rc::as_ptr(&index) as usize,
            shadow,
            env: (env.width, env.height, env.dpr, env.dark),
            hover: self.hover_chain.clone(),
            active: self.active_chain.clone(),
            focus: self.doc.focused,
            target,
            containers,
            quirks: self.doc.quirks,
        };
        let tr = Instant::now();
        let dirty = self.incremental_roots(&index, &snap, target);
        let roots_ms = tr.elapsed().as_secs_f64() * 1000.0;
        let incremental = dirty.is_some();
        let prev = self.styles.take();
        let now = crate::js::host::timeline_ms();
        self.anim.begin(now);
        let env = self.media_env();
        let mut eng = css::StyleEngine {
            doc: &self.doc,
            index: &index,
            env,
            hover_chain: self.hover_chain.clone(),
            active_chain: self.active_chain.clone(),
            target,
            inline_cache: &mut self.inline_cache,
            root: DOCUMENT,
            descend_hidden: false,
            seed: None,
            container_sizes: Some(&self.container_sizes),
            anim: Some(&mut self.anim),
            prev: prev.as_ref(),
            shadow_indexes: Some(&self.shadow_indexes),
            dirty: dirty.as_ref(),
        };
        let t0 = Instant::now();
        // Record :has() results for the next incremental pass (carried over
        // for the elements this pass doesn't restyle).
        let any_has = index.has_has || self.shadow_indexes.values().any(|i| i.has_has);
        if any_has {
            let log = if incremental { std::mem::take(&mut self.has_log) } else { HashMap::new() };
            css::selector::HAS_LOG.with(|l| *l.borrow_mut() = Some(log));
        }
        let slog = if incremental { std::mem::take(&mut self.struct_log) } else { HashMap::new() };
        css::selector::STRUCT_LOG.with(|l| *l.borrow_mut() = Some(slog));
        let styles = eng.compute();
        // Only attribute changes that render through styles, and no style
        // actually changed: the layout (and painting) stay valid.
        let layout_kept = incremental
            && std::env::var_os("OHB_NO_LAYOUT_KEEP").is_none()
            && !css::styles_changed()
            && self.doc.style_dirty.iter().all(|r| match r {
                crate::dom::StyleRecord::Attr(_, name, ..) => layout_inert_attr(name),
                _ => false,
            });
        self.struct_log = css::selector::STRUCT_LOG.with(|l| l.borrow_mut().take()).unwrap_or_default();
        if incremental && verify_incremental_style() {
            self.verify_styles(&styles, &index, env, target);
        }
        if any_has {
            self.has_log = css::selector::HAS_LOG.with(|l| l.borrow_mut().take()).unwrap_or_default();
        } else {
            self.has_log.clear();
        }
        self.full_style = self.has_log.len() > 200_000;
        self.style_snap = Some(snap);
        self.snap_gen = self.doc.generation;
        self.snap_bumps = self.doc.style_bumps;
        self.doc.style_dirty.clear();
        crate::js::profile_note("style", t0);
        if std::env::var_os("OHB_STYLE_STATS").is_some() {
            let st = css::STYLE_STATS.with(|s| std::mem::take(&mut *s.borrow_mut()));
            eprintln!("[style{}] roots={} ({:.1}ms) {:.1}ms elements={} cand id={} class={} tag={} attr={} universal={} evaluated={} matched={} | rules: universal={} pseudo={} nodes={}", if incremental { "+" } else { "" }, dirty.as_ref().map(|d| d.len()).unwrap_or(0), roots_ms, t0.elapsed().as_secs_f64() * 1000.0, st[0], st[1], st[2], st[3], st[7], st[4], st[6], st[5], index.universal_len(), index.pseudo_len(), self.doc.nodes.len());
        }
        self.anim.finish();
        self.after_animation_pass(now, t0.elapsed().as_secs_f64() * 1000.0);
        self.styles = Some(styles);
        self.need_style = false;
        if !layout_kept {
            self.need_layout = true;
        }
        let full = std::mem::take(&mut self.fonts_rescan) || std::env::var_os("OHB_FULL_DISCOVER").is_some();
        self.discover_resources(if full { None } else { dirty.as_ref() });
        self.style_sites.clear();
    }

    /// Computed styles for a sub-document living in this arena (only its own
    /// `<style>` elements apply), cached until the DOM changes.
    pub fn subdoc_styles(&mut self, root: NodeId) -> Rc<css::Styles> {
        // The frame's content box is the sub-document's viewport (for media queries).
        let mut env = self.media_env();
        if let Some(&frame) = self.doc.frame_of.get(&root) {
            self.ensure_layout();
            let size = self.layout.as_ref().and_then(|t| t.node_box.get(&frame).map(|&b| (t.boxes[b].content_w(), t.boxes[b].content_h())));
            let (w, h) = size.unwrap_or((0.0, 0.0));
            env.width = w;
            env.height = h;
        }
        if let Some((r, g, st)) = &self.subdoc_styles {
            if *r == root && *g == self.doc.generation {
                return st.clone();
            }
        }
        let mut sheets = vec![css::ua_sheet()];
        if self.doc.sub_quirks.get(&root).copied().unwrap_or(false) {
            sheets.push(css::quirks_sheet());
        }
        // The frame's local document: its base URL and loaded <link> sheets.
        let frame = self.doc.frame_of.get(&root).copied();
        let local = frame.and_then(|f| self.frame_views.get(&f)).and_then(|fv| fv.local.as_ref().map(|l| (l.url.clone(), fv.sheets.clone())));
        let base = local.as_ref().map(|(u, _)| self.local_base_of(root, u)).unwrap_or_else(|| self.doc.base_url.clone());
        let owners: Vec<NodeId> = self.doc.descendants(root).filter(|&n| self.doc.is_tag(n, "style") || self.doc.is_tag(n, "link")).collect();
        for n in owners {
            if let Some(m) = self.doc.attr(n, "media").map(str::trim).filter(|m| !m.is_empty()) {
                if !css::media_matches(&css::tokenizer::parse_cvs(m), &env) {
                    continue;
                }
            }
            if self.doc.is_tag(n, "style") {
                let text = self.style_text(n);
                sheets.push(Rc::new(css::parse_stylesheet(&text, &base, css::Origin::Author)));
                continue;
            }
            let rel = self.doc.attr(n, "rel").unwrap_or("").to_ascii_lowercase();
            if !rel.split_ascii_whitespace().any(|r| r == "stylesheet") {
                continue;
            }
            if let (Some((_, ls)), Some(u)) = (&local, self.doc.attr(n, "href").and_then(|h| base.join(h.trim()).ok())) {
                if let Some(Some(sh)) = ls.get(u.as_str()) {
                    sheets.push(sh.clone());
                }
            }
        }
        let idx = RuleIndex::new(sheets);
        let mut eng = css::StyleEngine {
            doc: &self.doc,
            index: &idx,
            env,
            hover_chain: Vec::new(),
            active_chain: Vec::new(),
            target: None,
            inline_cache: &mut self.inline_cache,
            root,
            descend_hidden: true,
            seed: None,
            container_sizes: Some(&self.container_sizes),
            anim: None,
            prev: None,
            shadow_indexes: None,
            dirty: None,
        };
        let st = Rc::new(eng.compute());
        self.subdoc_styles = Some((root, self.doc.generation, st.clone()));
        st
    }

    /// Computed style of an element inside a `display: none` subtree of the main
    /// document (not computed by the normal style pass).
    pub fn hidden_style(&mut self, id: NodeId) -> Option<(Rc<css::style::ComputedStyle>, Option<Rc<css::style::ComputedStyle>>, Option<Rc<css::style::ComputedStyle>>)> {
        self.ensure_style();
        let styles = self.styles.as_ref()?;
        // The nearest styled ancestor in the flat tree (none: the element
        // is outside the flat tree, e.g. an unslotted child of a host).
        let mut anc = self.doc.flat_parent_element(id);
        while let Some(a) = anc {
            if styles.get(a).is_some() {
                break;
            }
            anc = self.doc.flat_parent_element(a);
        }
        let anc = anc?;
        let ps = styles.get(anc)?.clone();
        let styles_anc_ps = ps.clone();
        let rf = self.doc.document_element().and_then(|r| styles.get(r)).map(|s| s.font_size).unwrap_or(16.0);
        let index = self.index.clone()?;
        let mut eng = css::StyleEngine {
            doc: &self.doc,
            index: &index,
            env: self.media_env(),
            hover_chain: Vec::new(),
            active_chain: Vec::new(),
            target: None,
            inline_cache: &mut self.inline_cache,
            root: self.doc.render_root.get(),
            descend_hidden: true,
            seed: Some((anc, ps, rf)),
            container_sizes: Some(&self.container_sizes),
            anim: None,
            prev: None,
            shadow_indexes: Some(&self.shadow_indexes),
            dirty: None,
        };
        let t0 = Instant::now();
        // Only the element's ancestors (tree and flat tree) up to `anc`.
        let mut path: HashSet<NodeId> = HashSet::new();
        path.insert(id);
        let mut cur = self.doc.flat_parent_element(id);
        let mut guard = 0;
        while let Some(c) = cur.filter(|&c| c != anc) {
            guard += 1;
            if guard > 4096 {
                break;
            }
            path.insert(c);
            cur = self.doc.flat_parent_element(c);
        }
        css::STYLE_PATH.with(|p| *p.borrow_mut() = Some(path));
        let st = eng.compute();
        css::STYLE_PATH.with(|p| *p.borrow_mut() = None);
        let found = st.get(id).is_some();
        let st = match found {
            true => st,
            false => {
                // Not reached along that path (slots, nested shadow trees): style the whole subtree.
                let mut eng = css::StyleEngine {
                    doc: &self.doc,
                    index: &index,
                    env: self.media_env(),
                    hover_chain: Vec::new(),
                    active_chain: Vec::new(),
                    target: None,
                    inline_cache: &mut self.inline_cache,
                    root: self.doc.render_root.get(),
                    descend_hidden: true,
                    seed: Some((anc, styles_anc_ps.clone(), rf)),
                    container_sizes: Some(&self.container_sizes),
                    anim: None,
                    prev: None,
                    shadow_indexes: Some(&self.shadow_indexes),
                    dirty: None,
                };
                eng.compute()
            }
        };
        crate::js::profile_note("hidden_style", t0);
        Some((st.get(id)?.clone(), st.before.get(&id).cloned(), st.after.get(&id).cloned()))
    }

    pub fn ensure_layout(&mut self) {
        if self.in_frame_ctx() {
            return;
        }
        self.ensure_style();
        let t0 = Instant::now();
        self.ensure_layout_inner();
        phase_add(1, t0.elapsed());
    }

    fn ensure_layout_inner(&mut self) {
        if !self.need_layout && self.layout.is_some() {
            self.update_sticky();
            return;
        }
        let (vw, vh) = self.viewport();
        // Scroll anchoring: remember where a visible element was so content
        // inserted or resized above it doesn't move what the user is reading.
        let anchor = if self.scroll.1 > 0.0 { self.layout.as_ref().and_then(|t| scroll_anchor(t, &self.doc, self.scroll.1, vh)) } else { None };
        self.res.doc_gen.set(self.doc.generation);
        let styles = self.styles.as_ref().unwrap();
        let view = ResView { res: &self.res };
        let t0 = Instant::now();
        let mut tree = layout::layout_document(&self.doc, styles, &mut self.fonts, &view, vw, vh);
        // Container queries depend on container sizes from layout: restyle and
        // lay out again when they changed (once; containment keeps them stable).
        if self.index.as_ref().map(|i| i.has_container_rules).unwrap_or(false) {
            for _ in 0..2 {
                let sizes: HashMap<NodeId, (f32, f32)> = tree
                    .boxes
                    .iter()
                    .filter(|b| b.style.container_type != 0)
                    .filter_map(|b| b.node.map(|n| (n, (b.content_w(), b.content_h()))))
                    .collect();
                if sizes == self.container_sizes {
                    break;
                }
                self.container_sizes = sizes;
                self.need_style = true;
                { self.full_style = true; crate::page::full_why(file!(), line!()); }
                self.ensure_style();
                let styles = self.styles.as_ref().unwrap();
                let view = ResView { res: &self.res };
                tree = layout::layout_document(&self.doc, styles, &mut self.fonts, &view, vw, vh);
            }
        }
        crate::js::profile_note("layout", t0);
        if thrash_debug() {
            eprintln!("[thrash] layout {:.1}ms by {} after: {}", t0.elapsed().as_secs_f64() * 1e3, crate::js::CURRENT_NATIVE.with(|c| c.get()), std::mem::take(&mut self.thrash_note));
        }
        if std::env::var("OHB_DUMP_LAYOUT").is_ok() {
            let _ = std::fs::write(std::env::var("OHB_DUMP_LAYOUT").unwrap(), layout::dump(&tree, &self.doc));
        }
        if let Some((node, old_y)) = anchor {
            if let Some(&b) = tree.node_box.get(&node) {
                let d = tree.boxes[b].abs_y - old_y;
                if std::env::var("OHB_DEBUG_ANCHOR").is_ok() {
                    eprintln!("[anchor] node {node} {old_y} -> {} scroll {:?} dh {}", tree.boxes[b].abs_y, self.scroll, tree.doc_height);
                }
                if d.abs() >= 0.5 && tree.boxes[b].h > 0.0 {
                    let max = (tree.doc_height - vh).max(0.0);
                    let ny = (self.scroll.1 + d).clamp(0.0, max);
                    if ny != self.scroll.1 {
                        self.scroll.1 = ny;
                        self.emit(PageEvent::ScrollTo(self.scroll.0, ny));
                        self.note_scrolled(DOCUMENT);
                    }
                }
            }
        }
        self.layout = Some(tree);
        self.need_layout = false;
        self.need_paint = true;
        self.clamp_elem_scroll();
        self.update_sticky();
    }

    /// Position sticky boxes for the current scroll offsets.
    fn update_sticky(&mut self) {
        let Some(tree) = self.layout.as_mut() else { return };
        if tree.sticky.is_empty() {
            return;
        }
        if tree.apply_sticky(&self.doc, self.scroll, &self.elem_scroll) {
            self.need_paint = true;
        }
    }

    /// The largest scroll offsets of an element's scroll container (None: no box).
    pub fn elem_scroll_max(&self, n: NodeId) -> Option<(f32, f32)> {
        let tree = self.layout.as_ref()?;
        let &b = tree.node_box.get(&n)?;
        let bx = &tree.boxes[b];
        let client_h = bx.h - bx.border[0] - bx.border[2];
        let client_w = bx.w - bx.border[1] - bx.border[3];
        Some(((bx.scroll_w - client_w).max(0.0), (bx.scroll_h - client_h).max(0.0)))
    }

    /// Keep element scroll offsets within range after layout changes.
    fn clamp_elem_scroll(&mut self) {
        if self.elem_scroll.is_empty() {
            return;
        }
        let keys: Vec<NodeId> = self.elem_scroll.keys().copied().collect();
        for n in keys {
            let cur = self.elem_scroll[&n];
            let max = self.elem_scroll_max(n).unwrap_or((0.0, 0.0));
            let c = (cur.0.min(max.0).max(0.0), cur.1.min(max.1).max(0.0));
            if c != cur {
                self.elem_scroll.insert(n, c);
                self.note_scrolled(n);
            }
        }
    }

    /// `Element.scrollIntoView`: scroll each scroll container around `n`, inner
    /// to outer and then the viewport, to bring it into view (CSSOM View
    /// "scroll a target into view").
    pub fn scroll_into_view(&mut self, n: NodeId, block: &str, inline: &str) {
        self.ensure_layout();
        fn axis(e0: f32, e1: f32, p0: f32, p1: f32, mode: &str) -> f32 {
            match mode {
                "start" => e0 - p0,
                "end" => e1 - p1,
                "center" => (e0 + e1) / 2.0 - (p0 + p1) / 2.0,
                _ => {
                    // nearest
                    if e0 >= p0 && e1 <= p1 {
                        0.0
                    } else if (e0 < p0 && e1 - e0 <= p1 - p0) || (e1 > p1 && e1 - e0 > p1 - p0) {
                        e0 - p0
                    } else {
                        e1 - p1
                    }
                }
            }
        }
        let root = self.doc.document_element();
        let body = self.doc.body();
        let mut containers = Vec::new();
        {
            let Some(tree) = self.layout.as_ref() else { return };
            let Some(&b) = tree.node_box.get(&n).or_else(|| self.doc.descendants(n).find_map(|k| tree.node_box.get(&k))) else { return };
            let mut cur = tree.boxes[b].parent;
            while let Some(p) = cur {
                let pb = &tree.boxes[p];
                if let Some(pn) = pb.node {
                    if Some(pn) != root && Some(pn) != body && pn != n && pb.style.is_scroll_container() {
                        if !containers.contains(&pn) {
                            containers.push(pn);
                        }
                    }
                }
                cur = pb.parent;
            }
        }
        // Sticky boxes scroll into view by their unshifted position.
        let unshifted = |p: &mut Self| -> Option<crate::paint::Rect> {
            let mut r = p.node_rect(n)?;
            if let Some(tree) = p.layout.as_ref() {
                if let Some(&b) = tree.node_box.get(&n) {
                    let (dx, dy) = tree.sticky_shift(b);
                    r.x -= dx;
                    r.y -= dy;
                }
            }
            Some(r)
        };
        for c in containers {
            let (Some(er), Some(cr)) = (unshifted(self), self.node_rect(c)) else { continue };
            let Some(tree) = self.layout.as_ref() else { return };
            let Some(&cb) = tree.node_box.get(&c) else { continue };
            let bx = &tree.boxes[cb];
            let (px, py) = (cr.x + bx.border[3], cr.y + bx.border[0]);
            let (pw, ph) = (bx.w - bx.border[1] - bx.border[3], bx.h - bx.border[0] - bx.border[2]);
            let max = self.elem_scroll_max(c).unwrap_or((0.0, 0.0));
            let cur = self.elem_scroll.get(&c).copied().unwrap_or((0.0, 0.0));
            let dy = axis(er.y, er.bottom(), py, py + ph, block);
            let dx = axis(er.x, er.right(), px, px + pw, inline);
            let ns = ((cur.0 + dx).clamp(0.0, max.0), (cur.1 + dy).clamp(0.0, max.1));
            if ns != cur {
                self.elem_scroll.insert(c, ns);
                self.note_scrolled(c);
            }
        }
        if let Some(er) = unshifted(self) {
            let (vw, vh) = self.viewport();
            let (dw, dh) = self.layout.as_ref().map(|l| (l.doc_width, l.doc_height)).unwrap_or((vw, vh));
            let dy = axis(er.y, er.bottom(), self.scroll.1, self.scroll.1 + vh, block);
            let dx = axis(er.x, er.right(), self.scroll.0, self.scroll.0 + vw, inline);
            let ns = ((self.scroll.0 + dx).clamp(0.0, (dw - vw).max(0.0)), (self.scroll.1 + dy).clamp(0.0, (dh - vh).max(0.0)));
            if ns != self.scroll {
                self.scroll = ns;
                self.emit(PageEvent::ScrollTo(ns.0, ns.1));
                self.note_scrolled(DOCUMENT);
            }
        }
        self.need_paint = true;
    }

    /// A scroll position changed (DOCUMENT for the viewport).
    pub fn note_scrolled(&mut self, n: NodeId) {
        if !self.pending_scroll.contains(&n) {
            self.pending_scroll.push(n);
        }
        self.need_paint = true;
    }

    /// Frame step ("run the scroll steps"): the containers that scrolled since
    /// the last frame (they get `scroll` now), and last frame's that have
    /// stopped (they get `scrollend`).
    pub fn take_scroll_events(&mut self) -> (Vec<NodeId>, Vec<NodeId>) {
        let ends: Vec<NodeId> = std::mem::take(&mut self.pending_scrollend).into_iter().filter(|n| !self.pending_scroll.contains(n)).collect();
        let now: Vec<NodeId> = std::mem::take(&mut self.pending_scroll);
        self.pending_scrollend = now.clone();
        (now, ends)
    }

    pub fn scroll_events_pending(&self) -> bool {
        !self.pending_scroll.is_empty() || !self.pending_scrollend.is_empty()
    }

    /// Produce a new frame if anything changed. Returns true if a frame was sent.
    pub fn render(&mut self) -> bool {
        if self.in_frame_ctx() {
            return false;
        }
        self.mark_dom_dirty();
        if !(self.need_style || self.need_layout || self.need_paint) {
            return false;
        }
        if self.css_blocking() && self.layout.is_none() {
            return false;
        }
        let title = self.display_title();
        self.ensure_layout();
        if self.pdf_tick() {
            // Pages that gained or lost their pixels.
            self.need_layout = true;
            self.ensure_layout();
        }
        self.update_frames();
        let ime_rect = match self.doc.focused {
            Some(f) if self.is_editable(f) || self.is_edit_host(f) => self.node_rect(f).map(|r| [r.x, r.y, r.w, r.h]),
            _ => None,
        };
        let styles = self.styles.as_ref().unwrap();
        let tree = self.layout.as_ref().unwrap();
        let view = ResView { res: &self.res };
        let focus = self.doc.focused;
        // The highlight follows the DOM selection; an editing host with focus shows its caret.
        let mut edit_caret = None;
        if let Some((an, ao, fnode, fo)) = self.dom_sel {
            if self.doc.valid(an) && self.doc.valid(fnode) {
                let a = tree.dom_to_point(&self.doc, an, ao);
                let f = tree.dom_to_point(&self.doc, fnode, fo);
                if let (Some(a), Some(f)) = (a, f) {
                    self.sel = Some((a, f));
                }
                let collapsed = an == fnode && ao == fo;
                if collapsed {
                    self.sel = None;
                    if let Some(host) = focus.filter(|&h| self.is_edit_host(h) && (h == fnode || self.doc.contains(h, fnode))) {
                        edit_caret = Some(match f {
                            Some(p) => crate::paint::builder::EditCaret::At(p),
                            None => crate::paint::builder::EditCaret::Empty(host),
                        });
                    }
                }
            }
        }
        let span = self.sel.and_then(|(a, b)| tree.span(a, b));
        let mut inp = builder::PaintInput {
            tree,
            doc: &self.doc,
            styles,
            res: &view,
            scroll: &self.elem_scroll,
            focus,
            caret: Some(self.caret),
            caret_visible: self.caret_on,
            fonts: &mut self.fonts,
            find: &self.find_matches,
            selection: span.as_ref(),
            edit_caret,
            field_sel: self.field_anchor.map(|a| (a, self.caret)),
            select_popup: self.select_popup.clone(),
        };
        let (list, hits) = builder::build_display_list(&mut inp);
        let focus_editable = focus.map(|f| self.is_editable(f) || self.is_edit_host(f)).unwrap_or(false);
        if let Ok(path) = std::env::var("OHB_DUMP_DL") {
            if !self.loading {
                let mut out = String::new();
                for it in list.items.iter() {
                    out.push_str(&format!("{it:?}\n"));
                }
                let _ = std::fs::write(path, out);
            }
        }
        let has_scrollers = hits.iter().any(|h| h.scroller.is_some());
        // Viewport overflow comes from the root element, or from <body> when the root's is visible.
        let user_scroll = {
            let root = self.doc.document_element().and_then(|r| styles.get(r)).map(|s| s.overflow_y);
            let body = self.doc.body().and_then(|b| styles.get(b)).map(|s| s.overflow_y);
            let ov = match root {
                Some(o) if o != css::style::Overflow::Visible => Some(o),
                _ => body,
            };
            ov.map(|o| !o.clips() || o.scrollable()).unwrap_or(true)
        };
        let (doc_w, doc_h) = (tree.doc_width, tree.doc_height);
        let mut list = list;
        if self.dt.highlight.is_some() {
            let ov = self.dt_overlay();
            list.items.extend(ov);
        }
        let wheel_listeners = self.wheel_listeners;
        let wheel_passive = self.wheel_passive;
        let mut scroll_regions: Vec<(crate::paint::Rect, bool)> = Vec::new();
        for h in hits.iter().filter(|h| h.scroller.is_some()) {
            let r = h.clip.unwrap_or(h.rect);
            if !scroll_regions.iter().any(|(q, f)| *f == h.fixed && q.x <= r.x && q.y <= r.y && q.x + q.w >= r.x + r.w && q.y + q.h >= r.y + r.h) {
                scroll_regions.push((r, h.fixed));
            }
            if scroll_regions.len() > 256 {
                break;
            }
        }
        let has_frames = !self.frame_views.is_empty();
        let frame = Arc::new(FrameData { list, doc_w, doc_h, focus_editable, has_scrollers, wheel_listeners, wheel_passive, scroll_regions, has_frames, user_scroll, ime_rect });
        self.hits = hits;
        self.need_paint = false;
        self.last_frame = Some(frame.clone());
        self.emit(PageEvent::Frame(frame));
        self.emit(PageEvent::Title(title.clone()));
        if let Some(e) = self.history.get_mut(self.hist_idx) {
            e.title = title;
        }
        if let Some(s) = self.restore_scroll.take() {
            self.scroll = s;
            self.emit(PageEvent::ScrollTo(s.0, s.1));
        } else if self.doc.url.fragment().is_some() && !self.loading && self.target_node.is_none() {
            self.scroll_to_fragment();
        }
        true
    }

    fn scroll_to_fragment(&mut self) {
        let frag = match self.doc.url.fragment() {
            Some(f) => crate::net::percent_decode(f.as_bytes()),
            None => return,
        };
        let frag = String::from_utf8_lossy(&frag).into_owned();
        let node = self.doc.get_element_by_id(&frag).or_else(|| {
            self.doc.descendants(DOCUMENT).find(|&n| self.doc.is_tag(n, "a") && self.doc.attr(n, "name") == Some(frag.as_str()))
        });
        if let Some(n) = node {
            self.target_node = Some(n);
            if let Some(r) = self.node_rect(n) {
                self.scroll = (0.0, r.y.max(0.0));
                self.emit(PageEvent::ScrollTo(0.0, r.y.max(0.0)));
            }
        } else if frag.is_empty() || frag == "top" {
            self.scroll = (0.0, 0.0);
            self.emit(PageEvent::ScrollTo(0.0, 0.0));
        }
    }

    /// Border box of a node's first layout box in document coordinates.
    /// PDF viewer: draw the pages near the viewport, drop the far ones.
    fn pdf_tick(&mut self) -> bool {
        let Some(mut d) = self.pdf.take() else { return false };
        if d.nodes.len() != d.sizes.len() {
            let prefix = format!("ohb-pdf:{}/", d.tag);
            d.nodes = self.doc.descendants(DOCUMENT).filter(|&n| self.doc.is_tag(n, "img") && self.doc.attr(n, "src").is_some_and(|s| s.starts_with(&prefix))).collect();
        }
        let (_, vh) = self.viewport();
        let top = self.scroll.1;
        let (near0, near1) = (top - vh, top + 2.0 * vh);
        let (keep0, keep1) = (top - 4.0 * vh, top + 5.0 * vh);
        let mut changed = false;
        let nodes = d.nodes.clone();
        for (i, n) in nodes.into_iter().enumerate() {
            let Some(r) = self.node_rect(n) else { continue };
            let url = pdf::page_url(d.tag, i);
            let (y0, y1) = (r.y, r.y + r.h);
            if y1 >= near0 && y0 <= near1 {
                if !d.rendered.contains(&i) {
                    d.rendered.insert(i);
                    match d.render(i) {
                        Some(img) => {
                            self.res.images.insert(url, ImgState::Ready(img));
                        }
                        None => {
                            self.res.images.insert(url, ImgState::Failed);
                        }
                    }
                    changed = true;
                }
            } else if (y1 < keep0 || y0 > keep1) && d.rendered.remove(&i) {
                self.res.images.insert(url, ImgState::Loading);
                changed = true;
            }
        }
        if changed {
            self.need_paint = true;
            self.res.doc_gen.set(self.res.doc_gen.get() + 1);
        }
        self.pdf = Some(d);
        changed
    }

    pub fn node_rect(&mut self, n: NodeId) -> Option<crate::paint::Rect> {
        self.node_rect_ex(n, true)
    }

    /// Whether a node's box is inside a fixed-position box (laid out in
    /// viewport coordinates, so page scrolling does not move it).
    pub fn node_in_fixed(&mut self, n: NodeId) -> bool {
        self.ensure_layout();
        let Some(tree) = self.layout.as_ref() else { return false };
        let mut cur = tree.node_box.get(&n).copied();
        while let Some(b) = cur {
            if tree.boxes[b].fixed {
                return true;
            }
            cur = tree.boxes[b].parent;
        }
        false
    }

    /// CSSOM View getClientRects(): one border box per line for inline boxes,
    /// the (transformed) border box otherwise, none without a box.
    pub fn node_client_rects(&mut self, n: NodeId) -> Vec<crate::paint::Rect> {
        self.ensure_layout();
        let Some(tree) = self.layout.as_ref() else { return Vec::new() };
        let Some(&b) = tree.node_box.get(&n) else { return Vec::new() };
        let bx = &tree.boxes[b];
        if !matches!(bx.kind, layout::BoxKind::InlineBox | layout::BoxKind::Text(_)) {
            return self.node_rect(n).into_iter().collect();
        }
        let mut out: Vec<crate::paint::Rect> = Vec::new();
        let mut cur = bx.parent;
        while let Some(p) = cur {
            let pb = &tree.boxes[p];
            if let Some(ifc) = &pb.ifc {
                let (ox, oy) = (pb.abs_x + pb.content_x(), pb.abs_y + pb.content_y());
                for d in &ifc.decos {
                    if d.box_id == b {
                        out.push(crate::paint::Rect::new(ox + d.x0, oy + d.y, d.x1 - d.x0, d.h));
                    }
                }
                if out.is_empty() {
                    for l in &ifc.lines {
                        let mut r: Option<crate::paint::Rect> = None;
                        for f in &l.frags {
                            if f.box_id == b {
                                let rr = crate::paint::Rect::new(ox + f.x, oy + f.y, f.w, f.h);
                                r = Some(match r {
                                    Some(x) => {
                                        let (x0, y0) = (x.x.min(rr.x), x.y.min(rr.y));
                                        crate::paint::Rect::new(x0, y0, x.right().max(rr.right()) - x0, x.bottom().max(rr.bottom()) - y0)
                                    }
                                    None => rr,
                                });
                            }
                        }
                        out.extend(r);
                    }
                }
                break;
            }
            cur = pb.parent;
        }
        if out.is_empty() {
            return self.node_rect(n).into_iter().collect();
        }
        // Element scroll containers above it.
        let mut cur = bx.parent;
        let (mut dx, mut dy) = (0.0, 0.0);
        while let Some(p) = cur {
            if let Some(pn) = tree.boxes[p].node {
                if let Some(&(sx, sy)) = self.elem_scroll.get(&pn) {
                    dx += sx;
                    dy += sy;
                }
            }
            cur = tree.boxes[p].parent;
        }
        for r in out.iter_mut() {
            r.x -= dx;
            r.y -= dy;
        }
        out
    }

    /// The border box as laid out (document coordinates): no transforms, no
    /// scrolling (CSSOM View offset* geometry); and whether it is fixed.
    pub fn layout_rect(&mut self, n: NodeId) -> Option<(crate::paint::Rect, bool)> {
        let r = self.node_rect_ex(n, false)?;
        let tree = self.layout.as_ref()?;
        let fixed = tree.node_box.get(&n).map(|&b| tree.boxes[b].fixed).unwrap_or(false);
        Some((r, fixed))
    }

    fn node_rect_ex(&mut self, n: NodeId, transformed: bool) -> Option<crate::paint::Rect> {
        self.ensure_layout();
        let tree = self.layout.as_ref()?;
        if let Some(&b) = tree.node_box.get(&n) {
            let bx = &tree.boxes[b];
            // Inline boxes have no own geometry: use the union of their fragments' decos.
            if matches!(bx.kind, layout::BoxKind::InlineBox | layout::BoxKind::Text(_)) {
                let mut cur = bx.parent;
                while let Some(p) = cur {
                    let pb = &tree.boxes[p];
                    if let Some(ifc) = &pb.ifc {
                        let ox = pb.abs_x + pb.content_x();
                        let oy = pb.abs_y + pb.content_y();
                        let mut r: Option<crate::paint::Rect> = None;
                        for d in &ifc.decos {
                            if d.box_id == b {
                                let rr = crate::paint::Rect::new(ox + d.x0, oy + d.y, d.x1 - d.x0, d.h);
                                r = Some(match r {
                                    Some(x) => {
                                        let x0 = x.x.min(rr.x);
                                        let y0 = x.y.min(rr.y);
                                        crate::paint::Rect::new(x0, y0, x.right().max(rr.right()) - x0, x.bottom().max(rr.bottom()) - y0)
                                    }
                                    None => rr,
                                });
                            }
                        }
                        for l in &ifc.lines {
                            for f in &l.frags {
                                if f.box_id == b {
                                    let rr = crate::paint::Rect::new(ox + f.x, oy + f.y, f.w, f.h);
                                    r = Some(match r {
                                        Some(x) => {
                                            let x0 = x.x.min(rr.x);
                                            let y0 = x.y.min(rr.y);
                                            crate::paint::Rect::new(x0, y0, x.right().max(rr.right()) - x0, x.bottom().max(rr.bottom()) - y0)
                                        }
                                        None => rr,
                                    });
                                }
                            }
                        }
                        if r.is_some() {
                            return r;
                        }
                        return Some(crate::paint::Rect::new(pb.abs_x, pb.abs_y, 0.0, 0.0));
                    }
                    cur = pb.parent;
                }
            }
            let mut r = crate::paint::Rect::new(bx.abs_x, bx.abs_y, bx.w, bx.h);
            if !transformed {
                return Some(r);
            }
            // Apply the transforms of the box and its ancestors (innermost first):
            // the result is the bounding box of the transformed border box.
            let mut cur = Some(b);
            let mut pts = [(r.x as f64, r.y as f64), (r.right() as f64, r.y as f64), (r.x as f64, r.bottom() as f64), (r.right() as f64, r.bottom() as f64)];
            let mut moved = false;
            while let Some(p) = cur {
                // Content of a vertical writing-mode root is laid out in a rotated frame.
                if p != b {
                    let pb = &tree.boxes[p];
                    if let Some(m) = layout::wm_matrix(pb, pb.abs_x, pb.abs_y) {
                        for pt in pts.iter_mut() {
                            let (x, y) = (pt.0 as f32, pt.1 as f32);
                            *pt = ((m[0] * x + m[2] * y + m[4]) as f64, (m[1] * x + m[3] * y + m[5]) as f64);
                        }
                        moved = true;
                    }
                }
                if let Some(m) = layout::box_matrix(tree, p) {
                    for pt in pts.iter_mut() {
                        if let Some(q) = m.project(pt.0, pt.1) {
                            *pt = q;
                        }
                    }
                    moved = true;
                }
                cur = tree.boxes[p].parent;
            }
            if moved {
                let (x0, x1) = pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, p| (a.0.min(p.0), a.1.max(p.0)));
                let (y0, y1) = pts.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, p| (a.0.min(p.1), a.1.max(p.1)));
                r = crate::paint::Rect::new(x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32);
            }
            // Account for element scroll containers above it.
            let mut cur = bx.parent;
            while let Some(p) = cur {
                if let Some(pn) = tree.boxes[p].node {
                    if let Some(&(sx, sy)) = self.elem_scroll.get(&pn) {
                        r.x -= sx;
                        r.y -= sy;
                    }
                }
                cur = tree.boxes[p].parent;
            }
            return Some(r);
        }
        // Not rendered (display:none or contents): use first descendant with a box.
        let kids: Vec<NodeId> = self.doc.descendants(n).collect();
        for k in kids {
            if let Some(&b) = tree.node_box.get(&k) {
                let bx = &tree.boxes[b];
                return Some(crate::paint::Rect::new(bx.abs_x, bx.abs_y, bx.w, bx.h));
            }
        }
        None
    }

    /// The quad (top-left, top-right, bottom-right, bottom-left, in client
    /// coordinates) of a box ("margin", "border", "padding" or "content") of
    /// an element with its own box, through the transforms above it; with the
    /// box's untransformed width and height.
    pub fn node_box_quad(&mut self, n: NodeId, which: &str) -> Option<([(f64, f64); 4], f32, f32)> {
        self.ensure_layout();
        let tree = self.layout.as_ref()?;
        let &b = tree.node_box.get(&n)?;
        let bx = &tree.boxes[b];
        if matches!(bx.kind, layout::BoxKind::InlineBox | layout::BoxKind::Text(_)) {
            return None;
        }
        let (m, bo, pa) = (bx.margin, bx.border, bx.padding);
        let (dt, dr, db, dl) = match which {
            "margin" => (-m[0], -m[1], -m[2], -m[3]),
            "padding" => (bo[0], bo[1], bo[2], bo[3]),
            "content" => (bo[0] + pa[0], bo[1] + pa[1], bo[2] + pa[2], bo[3] + pa[3]),
            _ => (0.0, 0.0, 0.0, 0.0),
        };
        let (x0, y0) = (bx.abs_x + dl, bx.abs_y + dt);
        let (w, h) = ((bx.w - dl - dr).max(0.0), (bx.h - dt - db).max(0.0));
        let mut pts = [(x0 as f64, y0 as f64), ((x0 + w) as f64, y0 as f64), ((x0 + w) as f64, (y0 + h) as f64), (x0 as f64, (y0 + h) as f64)];
        let mut cur = Some(b);
        while let Some(p) = cur {
            if p != b {
                let pb = &tree.boxes[p];
                if let Some(m) = layout::wm_matrix(pb, pb.abs_x, pb.abs_y) {
                    for pt in pts.iter_mut() {
                        let (x, y) = (pt.0 as f32, pt.1 as f32);
                        *pt = ((m[0] * x + m[2] * y + m[4]) as f64, (m[1] * x + m[3] * y + m[5]) as f64);
                    }
                }
            }
            if let Some(m) = layout::box_matrix(tree, p) {
                for pt in pts.iter_mut() {
                    if let Some(q) = m.project(pt.0, pt.1) {
                        *pt = q;
                    }
                }
            }
            cur = tree.boxes[p].parent;
        }
        let (mut sx, mut sy) = (self.scroll.0 as f64, self.scroll.1 as f64);
        let mut cur = bx.parent;
        while let Some(p) = cur {
            if let Some(pn) = tree.boxes[p].node {
                if let Some(&(ex, ey)) = self.elem_scroll.get(&pn) {
                    sx += ex as f64;
                    sy += ey as f64;
                }
            }
            cur = tree.boxes[p].parent;
        }
        for pt in pts.iter_mut() {
            pt.0 -= sx;
            pt.1 -= sy;
        }
        Some((pts, w, h))
    }

    // ------------------------------------------------------------ hit testing

    pub fn hit_test(&self, x: f32, y: f32) -> Option<(NodeId, Cursor, Option<NodeId>)> {
        let dx = x + self.scroll.0;
        let dy = y + self.scroll.1;
        for h in self.hits.iter().rev() {
            if !h.pointer_events {
                continue;
            }
            let (px, py) = if h.fixed { (x, y) } else { (dx, dy) };
            if !h.rect.contains(px, py) {
                continue;
            }
            if let Some(c) = &h.clip {
                if !c.contains(px, py) {
                    continue;
                }
            }
            return Some((h.node, h.cursor, h.scroller));
        }
        None
    }

    /// Elements under a viewport point, topmost first (for elementsFromPoint).
    pub fn elements_at(&mut self, x: f32, y: f32) -> Vec<NodeId> {
        if !self.in_frame_ctx() {
            self.render();
        }
        let dx = x + self.scroll.0;
        let dy = y + self.scroll.1;
        let mut out: Vec<NodeId> = Vec::new();
        for h in self.hits.iter().rev() {
            if !h.pointer_events {
                continue;
            }
            let (px, py) = if h.fixed { (x, y) } else { (dx, dy) };
            if !h.rect.contains(px, py) || h.clip.map(|c| !c.contains(px, py)).unwrap_or(false) {
                continue;
            }
            let e = self.element_of(h.node);
            if self.doc.is_element(e) && !out.contains(&e) {
                out.push(e);
            }
        }
        out
    }

    fn element_of(&self, n: NodeId) -> NodeId {
        if self.doc.is_element(n) {
            n
        } else {
            self.doc.flat_parent_element(n).unwrap_or(n)
        }
    }

    fn ancestors_inclusive(&self, n: NodeId) -> Vec<NodeId> {
        let mut v = Vec::new();
        let mut cur = Some(self.element_of(n));
        while let Some(c) = cur {
            v.push(c);
            // Through slots and shadow hosts (hover/active apply along the flat tree).
            cur = self.doc.flat_parent_element(c);
        }
        v
    }

    pub fn link_at(&self, n: NodeId) -> Option<(NodeId, String)> {
        for a in self.ancestors_inclusive(n) {
            if (self.doc.is_tag(a, "a") || self.doc.is_tag(a, "area")) && self.doc.attr(a, "href").is_some() {
                let href = self.doc.attr(a, "href").unwrap();
                if let Some(u) = self.doc.resolve_url(href) {
                    return Some((a, u.to_string()));
                }
            }
        }
        None
    }

    /// An element whose content the user edits (contenteditable, or the body in design mode).
    pub fn is_edit_host(&self, n: NodeId) -> bool {
        let mut cur = Some(n);
        while let Some(c) = cur {
            if let Some(e) = self.doc.element(c) {
                if let Some(v) = e.attr("contenteditable") {
                    let v = v.to_ascii_lowercase();
                    return v.is_empty() || v == "true" || v == "plaintext-only";
                }
            }
            if self.design_mode && self.doc.body() == Some(c) {
                return true;
            }
            cur = self.doc.parent(c).filter(|&p| self.doc.element(p).is_some());
        }
        false
    }

    pub fn is_editable(&self, n: NodeId) -> bool {
        match self.doc.element(n) {
            Some(e) => match e.name.as_str() {
                "textarea" => !e.has_attr("readonly") && !e.has_attr("disabled"),
                "input" => {
                    let t = e.attr("type").unwrap_or("text").to_ascii_lowercase();
                    matches!(t.as_str(), "text" | "search" | "email" | "url" | "password" | "tel" | "number" | "" | "date" | "time" | "datetime-local" | "month" | "week")
                        && !e.has_attr("readonly")
                        && !e.has_attr("disabled")
                }
                _ => false,
            },
            None => false,
        }
    }

    pub fn control_value(&self, n: NodeId) -> String {
        match self.doc.element(n) {
            Some(e) if e.name == "textarea" => e.value.clone().unwrap_or_else(|| self.doc.text_content(n)),
            Some(e) => e.value.clone().or_else(|| e.attr("value").map(|s| s.to_string())).unwrap_or_default(),
            None => String::new(),
        }
    }

    pub fn set_control_value(&mut self, n: NodeId, v: &str) {
        if let Some(e) = self.doc.element_mut(n) {
            e.value = Some(v.to_string());
        }
        self.doc.control_changed(n);
        self.need_paint = true;
    }

    /// A WebSocket event: script's WebSocket object hears of it.
    fn on_ws(&mut self, id: u64, ev: crate::net::ws::WsEvent) {
        use crate::js::host::HostValue as H;
        use crate::net::ws::WsEvent;
        if !self.ws_conns.contains_key(&id) {
            return;
        }
        let i = H::from(id as f64);
        let args = match ev {
            WsEvent::Open { protocol, extensions } => vec![i, H::from("open"), H::from(protocol.as_str()), H::from(extensions.as_str())],
            WsEvent::Text(t) => vec![i, H::from("text"), H::String(t)],
            WsEvent::Binary(b) => vec![i, H::from("binary"), H::Bytes(b)],
            WsEvent::Error => vec![i, H::from("error")],
            WsEvent::Closed { code, reason, clean } => {
                self.ws_conns.remove(&id);
                vec![i, H::from("close"), H::from(code as f64), H::String(reason), H::Bool(clean)]
            }
        };
        self.js_tasks.push(JsTask::CallGlobal("__ohb_wsEvent", args));
    }

    /// A mouse press's focus change (or input replayed after one) is queued.
    fn focus_pending(&self) -> bool {
        self.js_tasks.iter().any(|t| matches!(t, JsTask::Dispatch { then: DefaultAction::Focus { .. }, .. } | JsTask::Replay(_)))
    }

    /// Focus moved by the user (click, Tab, label): blur/focus events follow.
    pub fn focus_by_user(&mut self, n: Option<NodeId>) {
        let old = self.doc.focused;
        if old == n {
            return;
        }
        self.focus(n);
        let id = |x: Option<NodeId>| crate::js::host::HostValue::from(x.map(|v| v as f64).unwrap_or(-1.0));
        self.js_tasks.push(JsTask::CallGlobal("__ohb_focusEvents", vec![id(old), id(n)]));
    }

    pub fn focus(&mut self, n: Option<NodeId>) {
        if self.doc.focused == n {
            return;
        }
        self.doc.focused = n;
        if !self.doc.sub_gens.is_empty() {
            // :focus may change in a local frame's document.
            self.doc.bump_all();
        }
        if let Some(f) = n {
            if self.caret_for != Some(f) {
                self.caret = self.control_value(f).len();
                self.caret_for = Some(f);
                self.field_anchor = None;
            }
        }
        self.caret_on = true;
        self.need_style = true;
        { self.full_style = true; crate::page::full_why(file!(), line!()); }
    }

    // ----------------------------------------------------------- user input

    pub fn on_mouse_move(&mut self, x: f32, y: f32) {
        if self.dt_pick_event(x, y, false) {
            return;
        }
        if self.selecting {
            let (dx, dy) = (x + self.scroll.0, y + self.scroll.1);
            if let Some(p) = self.layout.as_ref().and_then(|t| t.text_hit(dx, dy, false)) {
                if let Some((a, f)) = self.sel {
                    if f != p {
                        self.sel = Some((a, p));
                        self.need_paint = true;
                        self.push_selection(None);
                    }
                }
            }
        }
        // movementX / movementY: the distance since the last move (raw
        // motion arrives as MouseDelta while the pointer is locked).
        if self.pl_active {
            return;
        }
        if let Some((lx, ly)) = self.last_move.replace((x, y)) {
            self.move_delta.0 += x - lx;
            self.move_delta.1 += y - ly;
        }
        // pointermove / mousemove (coalesced: one queued task, latest position).
        let frame_doc = if self.in_frame_ctx() { Some(self.doc.render_root.get()) } else { None };
        if self.pending_move.replace((x, y, frame_doc)).is_none() {
            self.js_tasks.push(JsTask::MouseMove);
        }
        let hit = self.hit_test(x, y);
        let (node, cursor) = match hit {
            Some((n, c, _)) => (Some(n), c),
            None => (None, Cursor::Default),
        };
        let mut cursor = cursor;
        let link = node.and_then(|n| self.link_at(n));
        if link.is_some() && cursor == Cursor::Text {
            cursor = Cursor::Pointer;
        }
        if cursor != self.last_cursor {
            self.last_cursor = cursor;
            self.emit(PageEvent::Cursor(cursor));
        }
        let status = link.map(|(_, u)| u).unwrap_or_default();
        if status != self.last_status {
            self.last_status = status.clone();
            self.emit(PageEvent::Status(status));
        }
        // :hover restyle only when the stylesheet uses :hover.
        let chain = node.map(|n| self.ancestors_inclusive(n)).unwrap_or_default();
        if chain != self.hover_chain {
            let old = std::mem::replace(&mut self.hover_chain, chain);
            if self.any_hover_rules() {
                self.need_style = true;
                { self.full_style = true; crate::page::full_why(file!(), line!()); }
            }
            let new_target = self.hover_chain.first().copied();
            let old_target = old.first().copied();
            self.doc.hovered = new_target;
            if new_target != old_target {
                if let Some(o) = old_target {
                    self.js_tasks.push(JsTask::Dispatch { target: o, ty: "mouseout".into(), init: EventInit { x, y, bubbles: true, ..Default::default() }, then: DefaultAction::None });
                    self.js_tasks.push(JsTask::Dispatch { target: o, ty: "mouseleave".into(), init: EventInit { x, y, ..Default::default() }, then: DefaultAction::None });
                }
                if let Some(t) = new_target {
                    self.js_tasks.push(JsTask::Dispatch { target: t, ty: "mouseover".into(), init: EventInit { x, y, bubbles: true, ..Default::default() }, then: DefaultAction::None });
                    self.js_tasks.push(JsTask::Dispatch { target: t, ty: "mouseenter".into(), init: EventInit { x, y, ..Default::default() }, then: DefaultAction::None });
                }
            }
        }
    }

    /// Dispatch the pending pointer move (see JsTask::MouseMove): to the
    /// element under the pointer, or while a button is held, to the element
    /// the press started on when the pointer left it (drags keep working).
    pub fn take_mouse_move(&mut self) -> Option<(NodeId, f32, f32)> {
        let (x, y, frame_doc) = self.pending_move.take()?;
        if let Some(d) = frame_doc {
            // A move inside a same-origin frame: hit-tested in its document.
            if !self.doc.valid(d) {
                return None;
            }
            return self.frame_ctx(d, |p| p.mouse_move_target(x, y)).map(|t| (t, x, y));
        }
        self.mouse_move_target(x, y).map(|t| (t, x, y))
    }

    fn mouse_move_target(&mut self, x: f32, y: f32) -> Option<NodeId> {
        self.ensure_layout();
        let under = self.hit_test(x, y).map(|(n, _, _)| self.element_of(n));
        let target = match (self.buttons_down != 0, self.mouse_down_target) {
            (true, Some(d)) if self.doc.valid(d) && self.doc.is_connected(d) && under.is_none() => d,
            _ => under.or(self.doc.body()).or(self.doc.document_element()).unwrap_or(DOCUMENT),
        };
        Some(target)
    }

    pub fn on_mouse_down(&mut self, x: f32, y: f32, button: u8, mods: Mods) {
        self.buttons_down |= button_bit(button);
        // Click count (UIEvent.detail): presses of the same button close in
        // time and place count up.
        let now = std::time::Instant::now();
        let (t0, lx, ly, lb, n) = self.press_count;
        let count = if lb == button && now.duration_since(t0) < std::time::Duration::from_millis(500) && (x - lx).abs() < 5.0 && (y - ly).abs() < 5.0 { n + 1 } else { 1 };
        self.press_count = (now, x, y, button, count);
        let bd = Some(self.buttons_down as u16);
        if self.dt_pick_event(x, y, true) {
            return;
        }
        // (Empty canvas outside every box: the root element is the target.)
        let target = self.hit_test(x, y).map(|(n, _, _)| self.element_of(n)).or_else(|| self.doc.document_element());
        self.mouse_down_target = target;
        if button == 0 && !self.in_frame_ctx() {
            self.begin_selection(x, y, target, mods);
            self.push_selection(target);
        }
        if let Some(t) = target {
            self.active_chain = self.ancestors_inclusive(t);
            if self.any_hover_rules() {
                self.need_style = true;
                { self.full_style = true; crate::page::full_why(file!(), line!()); }
            }
            // Focus moves as mousedown's default action (a canceled
            // mousedown keeps focus where it is).
            let focusable = self.ancestors_inclusive(t).into_iter().find(|&a| self.is_focusable(a));
            let then = if self.in_frame_ctx() {
                // A press inside a frame focuses the frame: with nothing
                // focusable under it, its body (the parent's activeElement
                // becomes the <iframe>).
                let focusable = focusable.or_else(|| self.doc.body());
                self.mouse_focus(focusable, x);
                DefaultAction::None
            } else {
                DefaultAction::Focus { node: focusable, x }
            };
            self.js_tasks.push(JsTask::Dispatch { target: t, ty: "pointerdown".into(), init: EventInit { x, y, button, mods, bubbles: true, cancelable: true, buttons: bd, ..Default::default() }, then: DefaultAction::None });
            self.js_tasks.push(JsTask::Dispatch { target: t, ty: "mousedown".into(), init: EventInit { x, y, button, mods, bubbles: true, cancelable: true, buttons: bd, detail: count as i32, ..Default::default() }, then });
        } else {
            self.focus_by_user(None);
        }
    }

    /// Focus from a mouse press at `x`: the focusable ancestor (or nothing),
    /// with the caret placed in an editable control.
    pub fn mouse_focus(&mut self, focusable: Option<NodeId>, x: f32) {
        self.focus_by_user(focusable);
        if let Some(f) = focusable {
            if self.is_editable(f) {
                self.place_caret(f, x);
                self.caret_for = Some(f);
            }
        }
    }

    /// Mouse-down text selection: click places, drag extends, double-click selects a
    /// word, triple-click a line; shift-click extends an existing selection.
    fn begin_selection(&mut self, x: f32, y: f32, target: Option<NodeId>, mods: Mods) {
        let now = std::time::Instant::now();
        let (t0, lx, ly, n) = self.last_down;
        let clicks = if now.duration_since(t0) < std::time::Duration::from_millis(450) && (x - lx).abs() < 5.0 && (y - ly).abs() < 5.0 { (n % 3) + 1 } else { 1 };
        self.last_down = (now, x, y, clicks);
        let on_control = target.map(|t| self.doc.is_tag(t, "input") || self.doc.is_tag(t, "textarea") || self.doc.is_tag(t, "select") || self.doc.is_tag(t, "button")).unwrap_or(false);
        let on_link = target.and_then(|t| self.link_at(t)).is_some();
        let had = self.sel.is_some();
        if on_control || (on_link && clicks == 1) {
            if had {
                self.sel = None;
                self.need_paint = true;
            }
            return;
        }
        let (dx, dy) = (x + self.scroll.0, y + self.scroll.1);
        let hit = self.layout.as_ref().and_then(|t| t.text_hit(dx, dy, true));
        match hit {
            Some(p) => {
                if mods.shift {
                    if let Some((a, _)) = self.sel {
                        self.sel = Some((a, p));
                        self.selecting = true;
                        self.need_paint = true;
                        return;
                    }
                }
                let tree = self.layout.as_ref().unwrap();
                self.sel = Some(match clicks {
                    2 => tree.word_at(p),
                    3 => {
                        let len = tree.boxes[p.ifc].ifc.as_ref().map(|i| i.lines[p.line].frags.len()).unwrap_or(1);
                        let last = len.saturating_sub(1);
                        (crate::layout::selection::SelPoint { frag: 0, byte: 0, ..p }, crate::layout::selection::SelPoint { frag: last, byte: usize::MAX / 2, ..p })
                    }
                    _ => (p, p),
                });
                self.selecting = clicks == 1;
                self.need_paint = true;
            }
            None => {
                if had {
                    self.sel = None;
                    self.need_paint = true;
                }
            }
        }
    }

    /// Keys in a focused editing host: editing actions and caret movement.
    fn edit_host_key(&mut self, _host: NodeId, key: &Key, mods: Mods) -> bool {
        use crate::js::host::HostValue as H;
        let cmd = mods.ctrl || mods.meta;
        let word = mods.ctrl || mods.alt;
        let action: Option<&str> = match key {
            Key::Backspace => Some(if word { "deleteWordBackward" } else { "deleteContentBackward" }),
            Key::Delete => Some(if word { "deleteWordForward" } else { "deleteContentForward" }),
            Key::Enter => Some(if mods.shift { "insertLineBreak" } else { "insertParagraph" }),
            Key::Char(c) if cmd && c.eq_ignore_ascii_case("b") => Some("formatBold"),
            Key::Char(c) if cmd && c.eq_ignore_ascii_case("i") => Some("formatItalic"),
            Key::Char(c) if cmd && c.eq_ignore_ascii_case("u") => Some("formatUnderline"),
            Key::Char(c) if cmd && c.eq_ignore_ascii_case("x") => {
                let t = self.selection_text();
                if !t.is_empty() {
                    self.emit(PageEvent::Copy(t));
                }
                Some("deleteByCut")
            }
            _ => None,
        };
        self.caret_on = true;
        if let Some(a) = action {
            self.js_tasks.push(JsTask::CallGlobal("__ohb_editAction", vec![H::from(a), H::Null]));
            return true;
        }
        match key {
            Key::Char(c) if cmd && c.eq_ignore_ascii_case("a") => {
                self.js_tasks.push(JsTask::CallGlobal("__ohb_editSelectAll", vec![]));
                true
            }
            Key::Char(c) if cmd && c.eq_ignore_ascii_case("c") => {
                let t = self.selection_text();
                if !t.is_empty() {
                    self.emit(PageEvent::Copy(t));
                }
                true
            }
            Key::Left | Key::Right => {
                let fwd = matches!(key, Key::Right);
                self.js_tasks.push(JsTask::CallGlobal("__ohb_editMove", vec![H::from(fwd), H::from(mods.shift), H::from(if word { "word" } else { "character" })]));
                true
            }
            Key::Up | Key::Down | Key::Home | Key::End => {
                self.edit_move_line(key, mods.shift);
                true
            }
            Key::Space | Key::Tab => true,
            _ => false,
        }
    }

    /// Up/Down/Home/End in an editing host, through the layout.
    fn edit_move_line(&mut self, key: &Key, extend: bool) {
        use crate::js::host::HostValue as H;
        let Some((an, ao, fnode, fo)) = self.dom_sel else { return };
        let Some(tree) = self.layout.as_ref() else { return };
        let Some(p) = tree.dom_to_point(&self.doc, fnode, fo) else { return };
        let target = match key {
            Key::Home | Key::End => {
                let b = &tree.boxes[p.ifc];
                let n = b.ifc.as_ref().map(|i| i.lines[p.line].frags.len()).unwrap_or(0);
                let (fi, byte) = if matches!(key, Key::Home) { (0, 0) } else { (n.saturating_sub(1), usize::MAX / 2) };
                // The first/last text fragment of the line.
                let frags = &b.ifc.as_ref().unwrap().lines[p.line].frags;
                let pick = if matches!(key, Key::Home) {
                    frags.iter().position(|f| matches!(f.kind, crate::layout::inline::FragKind::Text { .. })).unwrap_or(fi)
                } else {
                    frags.iter().rposition(|f| matches!(f.kind, crate::layout::inline::FragKind::Text { .. })).unwrap_or(fi)
                };
                let len = match &frags.get(pick).map(|f| &f.kind) {
                    Some(crate::layout::inline::FragKind::Text { text, .. }) => text.len(),
                    _ => 0,
                };
                Some(crate::layout::selection::SelPoint { frag: pick, byte: if byte == 0 { 0 } else { len }, ..p })
            }
            _ => {
                let (x, y, h) = match tree.caret_rect(p) {
                    Some(r) => r,
                    None => return,
                };
                let ty = if matches!(key, Key::Up) { y - 2.0 } else { y + h + 2.0 };
                tree.text_hit(x, ty, false).filter(|q| q.ifc != p.ifc || q.line != p.line)
            }
        };
        let Some(t) = target else { return };
        let Some((tn, to)) = tree.point_to_dom(&self.doc, t) else { return };
        let (bn, bo) = if extend { (an, ao) } else { (tn, to) };
        self.js_tasks.push(JsTask::CallGlobal("__ohb_setSelection", vec![H::from(bn as f64), H::from(bo as f64), H::from(tn as f64), H::from(to as f64)]));
    }

    /// Hand the native (mouse) selection to script as the DOM selection; a
    /// click into an editing host without text puts the caret at its start.
    fn push_selection(&mut self, target: Option<NodeId>) {
        use crate::js::host::HostValue as H;
        let pts = match (self.sel, self.layout.as_ref()) {
            (Some((a, f)), Some(t)) => t.point_to_dom(&self.doc, a).zip(t.point_to_dom(&self.doc, f)),
            _ => None,
        };
        let args = match pts {
            Some(((an, ao), (fnode, fo))) => vec![H::from(an as f64), H::from(ao as f64), H::from(fnode as f64), H::from(fo as f64)],
            None => {
                let host = target.and_then(|t| self.ancestors_inclusive(t).into_iter().find(|&a| self.is_edit_host(a)));
                match host {
                    Some(h) => vec![H::from(h as f64), H::from(0), H::from(h as f64), H::from(0)],
                    None => vec![H::Null],
                }
            }
        };
        self.js_tasks.push(JsTask::CallGlobal("__ohb_setSelection", args));
    }

    /// The currently selected page text (empty when nothing is selected).
    pub fn selection_text(&self) -> String {
        match (self.sel, self.layout.as_ref()) {
            (Some((a, b)), Some(tree)) => tree.span(a, b).map(|sp| tree.selected_text(&sp)).unwrap_or_default(),
            _ => String::new(),
        }
    }

    pub fn select_all_text(&mut self) {
        if let Some(t) = self.layout.as_ref() {
            self.sel = t.text_extent();
            self.need_paint = true;
        }
    }

    fn place_caret(&mut self, f: NodeId, x: f32) {
        let r = match self.node_rect(f) {
            Some(r) => r,
            None => return,
        };
        let value = self.control_value(f);
        let style = self.styles.as_ref().and_then(|s| s.get(f)).cloned();
        if let Some(s) = style {
            let pad = s.padding[3].resolve(0.0) + s.bw(3);
            let rel = x + self.scroll.0 - r.x - pad;
            let is_pw = self.doc.attr(f, "type").map(|t| t.eq_ignore_ascii_case("password")).unwrap_or(false);
            let shown = if is_pw { "•".repeat(value.chars().count()) } else { value.clone() };
            let sh = self.fonts.shape(&shown, &s.font_family, s.font_weight, s.font_style, s.font_size, s.letter_spacing);
            let b = sh.byte_at_x(rel, &shown);
            self.field_anchor = None;
            self.caret = if is_pw { value.char_indices().nth(shown[..b].chars().count()).map(|(i, _)| i).unwrap_or(value.len()) } else { b.min(value.len()) };
        }
    }

    fn is_focusable(&self, n: NodeId) -> bool {
        match self.doc.element(n) {
            Some(e) => {
                if e.has_attr("disabled") {
                    return false;
                }
                (matches!(e.name.as_str(), "textarea" | "select" | "button") || e.name == "input" && !e.attr("type").map(|t| t.eq_ignore_ascii_case("hidden")).unwrap_or(false)) || (e.name == "a" && e.has_attr("href")) || e.attr("tabindex").map(|t| t.trim().parse::<i64>().is_ok()).unwrap_or(false) || e.has_attr("contenteditable")
            }
            None => false,
        }
    }

    pub fn on_mouse_up(&mut self, x: f32, y: f32, button: u8, mods: Mods) {
        self.buttons_down &= !button_bit(button);
        let count = if self.press_count.3 == button { self.press_count.4 as i32 } else { 1 };
        let bd = Some(self.buttons_down as u16);
        if std::mem::take(&mut self.dt.swallow_up) {
            return;
        }
        if self.selecting {
            self.selecting = false;
            if let Some((a, f)) = self.sel {
                if a == f {
                    self.sel = None;
                    self.need_paint = true;
                }
            }
        }
        let target = self.hit_test(x, y).map(|(n, _, _)| self.element_of(n)).or_else(|| self.doc.document_element());
        if !self.active_chain.is_empty() {
            self.active_chain.clear();
            if self.any_hover_rules() {
                self.need_style = true;
                { self.full_style = true; crate::page::full_why(file!(), line!()); }
            }
        }
        if let Some(t) = target {
            self.js_tasks.push(JsTask::Dispatch { target: t, ty: "pointerup".into(), init: EventInit { x, y, button, mods, bubbles: true, cancelable: true, buttons: bd, ..Default::default() }, then: DefaultAction::None });
            self.js_tasks.push(JsTask::Dispatch { target: t, ty: "mouseup".into(), init: EventInit { x, y, button, mods, bubbles: true, cancelable: true, buttons: bd, detail: count, ..Default::default() }, then: DefaultAction::None });
            let same = self.mouse_down_target.map(|d| self.doc.contains(d, t) || self.doc.contains(t, d)).unwrap_or(false);
            // Disabled form controls (and their contents) don't get clicks from real input.
            let disabled = self.ancestors_inclusive(t).into_iter().any(|a| {
                matches!(self.doc.tag(a), "button" | "input" | "select" | "textarea") && crate::css::selector::actually_disabled(&self.doc, a)
            });
            if same && !disabled {
                let ty = if button == 1 { "auxclick" } else if button == 2 { "contextmenu" } else { "click" };
                self.user_gesture = true;
                let then = match if button == 0 { self.pre_activate(t) } else { None } {
                    Some(prev) => DefaultAction::Activate { target: t, prev },
                    None => DefaultAction::Click { target: t, mods, button },
                };
                self.js_tasks.push(JsTask::Dispatch { target: t, ty: ty.into(), init: EventInit { x, y, button, mods, bubbles: true, cancelable: true, buttons: bd, detail: count, ..Default::default() }, then });
                if button == 0 && count == 2 {
                    self.js_tasks.push(JsTask::Dispatch { target: t, ty: "dblclick".into(), init: EventInit { x, y, button, mods, bubbles: true, cancelable: true, buttons: bd, detail: 2, ..Default::default() }, then: DefaultAction::None });
                }
            }
        }
        self.mouse_down_target = None;
    }

    /// A click from the keyboard (or implicit submission): the click event,
    /// with the element's activation as its default action.
    pub fn synthetic_click(&mut self, t: NodeId, mods: Mods) {
        self.user_gesture = true;
        let then = match self.pre_activate(t) {
            Some(prev) => DefaultAction::Activate { target: t, prev },
            None => DefaultAction::Click { target: t, mods, button: 0 },
        };
        self.js_tasks.push(JsTask::Dispatch { target: t, ty: "click".into(), init: EventInit { button: 0, mods, bubbles: true, cancelable: true, ..Default::default() }, then });
    }

    /// Default action for a click that wasn't cancelled by script.
    pub fn default_click(&mut self, target: NodeId, mods: Mods, button: u8) {
        if button == 2 {
            return;
        }
        let chain = self.ancestors_inclusive(target);
        for &n in &chain {
            let el = match self.doc.element(n) {
                Some(e) => e.clone(),
                None => continue,
            };
            match el.name.as_str() {
                "a" | "area" if el.has_attr("href") || (el.ns == crate::dom::Namespace::Svg && el.has_attr("xlink:href")) => {
                    let href = el.attr("href").or_else(|| el.attr("xlink:href")).unwrap();
                    if href.trim_start().to_ascii_lowercase().starts_with("javascript:") {
                        let code = href.trim_start()[11..].to_string();
                        let code = String::from_utf8_lossy(&crate::net::percent_decode(code.as_bytes())).into_owned();
                        self.js_tasks.push(JsTask::Dispatch { target: n, ty: "__js_url".into(), init: EventInit { key: code, ..Default::default() }, then: DefaultAction::None });
                        return;
                    }
                    // A link in a local frame's document: resolved against it, and
                    // (unless it targets elsewhere) navigating that frame.
                    let in_frame = self.frame_of_node(n);
                    let url = match in_frame {
                        Some(fr) => match self.frame_views.get(&fr).and_then(|v| v.local.as_ref()).and_then(|l| l.doc.map(|d| (d, l.url.clone()))) {
                            Some((d, u)) => match self.local_base_of(d, &u).join(href.trim()) {
                                Ok(u) => u,
                                Err(_) => return,
                            },
                            None => return,
                        },
                        None => match self.doc.resolve_url(href) {
                            Some(u) => u,
                            None => return,
                        },
                    };
                    if let Some(fr) = in_frame {
                        let t = el.attr("target").map(|t| t.trim().to_ascii_lowercase()).unwrap_or_default();
                        let new_tab = button == 1 || mods.ctrl || mods.meta || t == "_blank";
                        if !new_tab && (t.is_empty() || t == "_self") {
                            let referrer = self.frame_doc_url(n);
                            self.local_frame_navigate(fr, url.as_str(), referrer);
                            return;
                        }
                        if new_tab {
                            self.emit(PageEvent::OpenTab(url.to_string()));
                            return;
                        }
                    }
                    if el.has_attr("download") && matches!(url.scheme(), "http" | "https") {
                        // Let the navigation handle it as a download.
                    }
                    let new_tab = button == 1 || mods.ctrl || mods.meta || el.attr("target").map(|t| t == "_blank").unwrap_or(false);
                    // `target` naming an iframe loads the link there.
                    let tname = el.attr("target").map(|t| t.trim().to_string()).filter(|t| !t.is_empty() && !t.starts_with('_'));
                    let frame = tname.as_deref().and_then(|t| self.doc.descendants(DOCUMENT).find(|&n| (self.doc.is_tag(n, "iframe") || self.doc.is_tag(n, "frame")) && self.doc.attr(n, "name") == Some(t)));
                    if let (false, Some(f)) = (new_tab, frame) {
                        self.doc.set_attr(f, "src", url.as_str());
                        self.need_layout = true;
                        return;
                    }
                    let to_top = matches!(el.attr("target").map(|t| t.trim().to_ascii_lowercase()).as_deref(), Some("_top" | "_parent"));
                    if new_tab && crate::ext::blocks_popup(&url, &self.doc.url, Some(self.tab_id)) {
                        // HeronBlock: an ad link opening a new tab.
                    } else if new_tab {
                        self.emit(PageEvent::OpenTab(url.to_string()));
                    } else if to_top && self.frame_parent.is_some() {
                        if let Some(fp) = &self.frame_parent {
                            let _ = fp.tx.send(PageMsg::Child(fp.frame_id, frames::ChildMsg::NavigateTop(url.to_string())));
                        }
                    } else {
                        let mut req = NavRequest::get(url.as_str());
                        req.referrer = Some(self.doc.url.clone());
                        self.pending_nav = Some(req);
                    }
                    return;
                }
                "input" => {
                    let t = el.attr("type").unwrap_or("text").to_ascii_lowercase();
                    match t.as_str() {
                        "checkbox" => {
                            let cur = el.checked.unwrap_or_else(|| el.has_attr("checked"));
                            self.set_checked(n, !cur);
                            self.fire_change(n);
                        }
                        "radio" => {
                            let name = el.attr("name").unwrap_or("").to_string();
                            let form = self.form_of(n);
                            let others: Vec<NodeId> = self
                                .doc
                                .descendants(DOCUMENT)
                                .filter(|&o| self.doc.is_tag(o, "input") && self.doc.attr(o, "type").map(|t| t.eq_ignore_ascii_case("radio")).unwrap_or(false) && self.doc.attr(o, "name").unwrap_or("") == name && self.form_of(o) == form)
                                .collect();
                            for o in others {
                                if let Some(e) = self.doc.element_mut(o) {
                                    e.checked = Some(o == n);
                                }
                            }
                            self.doc.bump_all();
                            self.fire_change(n);
                        }
                        "submit" | "image" => {
                            if let Some(f) = self.form_of(n) {
                                self.request_submit(f, Some(n));
                            }
                        }
                        "reset" => {
                            if let Some(f) = self.form_of(n) {
                                self.js_tasks.push(JsTask::FormAction { form: f, submitter: None, reset: true });
                            }
                        }
                        _ => {}
                    }
                    return;
                }
                "button" => {
                    let t = el.attr("type").unwrap_or("submit").to_ascii_lowercase();
                    if t == "submit" {
                        if let Some(f) = self.form_of(n) {
                            self.request_submit(f, Some(n));
                        }
                    } else if t == "reset" {
                        if let Some(f) = self.form_of(n) {
                            self.js_tasks.push(JsTask::FormAction { form: f, submitter: None, reset: true });
                        }
                    }
                    return;
                }
                "select" => {
                    let listbox = el.attr("multiple").is_some() || el.attr("size").and_then(|v| v.trim().parse::<i32>().ok()).map(|n| n > 1).unwrap_or(false);
                    if listbox {
                        self.cycle_select(n, 1);
                    } else if !std::mem::take(&mut self.select_popup_suppress) {
                        self.open_select_popup(n);
                    }
                    return;
                }
                "summary" => {
                    if let Some(d) = self.doc.parent_element(n).filter(|&p| self.doc.is_tag(p, "details")) {
                        if self.doc.attr(d, "open").is_some() {
                            self.doc.remove_attr(d, "open");
                        } else {
                            self.doc.set_attr(d, "open", "");
                        }
                    }
                    return;
                }
                "label" => {
                    let ctl = el
                        .attr("for")
                        .and_then(|id| self.doc.get_element_by_id(id))
                        .or_else(|| self.doc.descendants(n).find(|&d| matches!(self.doc.tag(d), "input" | "select" | "textarea" | "button")));
                    if let Some(c) = ctl {
                        // Clicking the label activates its control (unless the
                        // click already landed on the control itself).
                        if c != target {
                            if self.is_editable(c) {
                                self.focus_by_user(Some(c));
                            } else {
                                self.default_click(c, mods, button);
                            }
                        }
                    }
                    return;
                }
                _ => {}
            }
        }
    }

    /// Set an input's checkedness; checking a radio unchecks the rest of its group.
    pub fn set_checked(&mut self, n: NodeId, v: bool) {
        // Unchanged (frameworks re-set `checked` on every render): only the
        // dirty-checkedness flag moves, nothing needs restyling.
        let cur = self.doc.element(n).map(|e| e.checked.unwrap_or_else(|| e.has_attr("checked")));
        if cur == Some(v) {
            if let Some(e) = self.doc.element_mut(n) {
                e.checked = Some(v);
            }
            return;
        }
        let is_radio = self.doc.is_tag(n, "input") && self.doc.attr(n, "type").map(|t| t.eq_ignore_ascii_case("radio")).unwrap_or(false);
        if v && is_radio {
            for o in self.radio_group(n) {
                if let Some(e) = self.doc.element_mut(o) {
                    e.checked = Some(o == n);
                }
            }
        } else if let Some(e) = self.doc.element_mut(n) {
            e.checked = Some(v);
        }
        self.doc.bump_all();
        self.need_style = true;
        { self.full_style = true; crate::page::full_why(file!(), line!()); }
    }

    fn radio_group(&self, n: NodeId) -> Vec<NodeId> {
        let name = self.doc.attr(n, "name").unwrap_or("").to_string();
        if name.is_empty() {
            return vec![n];
        }
        let form = self.form_of(n);
        // Same tree (main document, a sub-document or a detached subtree), form owner and name.
        let root = self.doc.root_of(n);
        let mut v: Vec<NodeId> = self
            .doc
            .descendants(root)
            .filter(|&o| self.doc.is_tag(o, "input") && self.doc.attr(o, "type").map(|t| t.eq_ignore_ascii_case("radio")).unwrap_or(false) && self.doc.attr(o, "name").unwrap_or("") == name && self.form_of(o) == form)
            .collect();
        if !v.contains(&n) {
            v.push(n);
        }
        v
    }

    fn is_checked(&self, n: NodeId) -> bool {
        self.doc.element(n).map(|e| e.checked.unwrap_or_else(|| e.has_attr("checked"))).unwrap_or(false)
    }

    /// HTML "legacy-pre-activation behavior": toggle a checkbox / check a radio before
    /// the click event is dispatched. Returns the previous states for cancellation.
    pub fn pre_activate(&mut self, t: NodeId) -> Option<Vec<(NodeId, bool)>> {
        // (Legacy pre-activation ignores `disabled`; real clicks never reach disabled controls.)
        if !self.doc.is_tag(t, "input") {
            return None;
        }
        let ty = self.doc.attr(t, "type").unwrap_or("").to_ascii_lowercase();
        match ty.as_str() {
            "checkbox" => {
                let cur = self.is_checked(t);
                self.set_checked(t, !cur);
                Some(vec![(t, cur)])
            }
            "radio" => {
                let prev: Vec<(NodeId, bool)> = self.radio_group(t).into_iter().map(|o| (o, self.is_checked(o))).collect();
                self.set_checked(t, true);
                Some(prev)
            }
            _ => None,
        }
    }

    pub fn finish_activation(&mut self, target: NodeId, prev: Vec<(NodeId, bool)>, canceled: bool) {
        self.finish_activation_ex(target, prev, canceled, true);
    }

    /// Undo a canceled checkbox/radio activation, or report (and, with
    /// `fire`, queue input/change for) a completed one. Returns whether the
    /// checkedness changed.
    pub fn finish_activation_ex(&mut self, target: NodeId, prev: Vec<(NodeId, bool)>, canceled: bool, fire: bool) -> bool {
        if canceled {
            for (n, c) in prev {
                if let Some(e) = self.doc.element_mut(n) {
                    e.checked = Some(c);
                }
            }
            self.doc.bump_all();
            self.need_style = true;
            { self.full_style = true; crate::page::full_why(file!(), line!()); }
            false
        } else {
            let changed = prev.iter().any(|(n, c)| self.is_checked(*n) != *c);
            if changed {
                if fire {
                    self.fire_change(target);
                } else {
                    self.need_style = true;
                    { self.full_style = true; crate::page::full_why(file!(), line!()); }
                }
            }
            changed
        }
    }

    fn fire_change(&mut self, n: NodeId) {
        self.js_tasks.push(JsTask::Dispatch { target: n, ty: "input".into(), init: EventInit { bubbles: true, ..Default::default() }, then: DefaultAction::None });
        self.js_tasks.push(JsTask::Dispatch { target: n, ty: "change".into(), init: EventInit { bubbles: true, ..Default::default() }, then: DefaultAction::None });
        self.need_style = true;
        { self.full_style = true; crate::page::full_why(file!(), line!()); }
    }

    fn cycle_select(&mut self, n: NodeId, dir: i32) {
        let opts: Vec<NodeId> = self.doc.descendants(n).filter(|&o| self.doc.is_tag(o, "option") && self.doc.attr(o, "disabled").is_none()).collect();
        if opts.is_empty() {
            return;
        }
        let cur = opts.iter().position(|&o| self.doc.element(o).map(|e| e.selected.unwrap_or_else(|| e.has_attr("selected"))).unwrap_or(false)).unwrap_or(0);
        let next = ((cur as i32 + dir).rem_euclid(opts.len() as i32)) as usize;
        for (i, &o) in opts.iter().enumerate() {
            if let Some(e) = self.doc.element_mut(o) {
                e.selected = Some(i == next);
            }
        }
        self.doc.bump_all();
        self.fire_change(n);
    }

    /// Open the dropdown list of a `<select>` below it (or above, near the bottom).
    fn open_select_popup(&mut self, n: NodeId) {
        let Some(r) = self.node_rect(n) else { return };
        let mut items = Vec::new();
        let mut opts = Vec::new();
        let mut selected_row = None;
        for o in self.doc.descendants(n).collect::<Vec<_>>() {
            let tag = self.doc.tag(o).to_string();
            if tag == "optgroup" {
                let label = self.doc.attr(o, "label").unwrap_or("").to_string();
                items.push((label, true, true, false));
                opts.push(None);
            } else if tag == "option" {
                let label = self.doc.attr(o, "label").map(|s| s.to_string()).unwrap_or_else(|| self.doc.text_content(o).split_whitespace().collect::<Vec<_>>().join(" "));
                let disabled = self.doc.attr(o, "disabled").is_some() || self.doc.parent_element(o).map(|p| self.doc.is_tag(p, "optgroup") && self.doc.attr(p, "disabled").is_some()).unwrap_or(false);
                let sel = self.doc.element(o).map(|e| e.selected.unwrap_or_else(|| e.has_attr("selected"))).unwrap_or(false);
                if sel && selected_row.is_none() {
                    selected_row = Some(items.len());
                }
                items.push((label, disabled, false, sel));
                opts.push(Some(o));
            }
        }
        if items.is_empty() {
            return;
        }
        let fs = self.styles.as_ref().and_then(|s| s.get(n)).map(|s| s.font_size).unwrap_or(13.333).clamp(11.0, 24.0);
        let item_h = (fs * 1.45).round().max(20.0);
        let rows = items.len().min(18);
        let h = rows as f32 * item_h + 2.0;
        // Width: the select's, or wider for long labels.
        let w = {
            let s = self.styles.as_ref().and_then(|s| s.get(n)).cloned();
            let longest = items.iter().map(|it| it.0.as_str()).max_by_key(|l| l.len()).unwrap_or("");
            let lw = s.map(|s| self.fonts.shape(longest, &s.font_family, 700, s.font_style, fs, 0.0).width + 30.0).unwrap_or(0.0);
            r.w.max(lw).min(self.viewport().0 - 8.0).max(60.0)
        };
        let (_, vh) = self.viewport();
        let below = r.bottom() + h <= self.scroll.1 + vh || r.y - self.scroll.1 < h;
        let y = if below { r.bottom() } else { r.y - h };
        let first = selected_row.map(|s| s.saturating_sub(rows / 2)).unwrap_or(0).min(items.len() - rows);
        self.select_popup = Some(crate::paint::builder::SelectPopupPaint { node: n, rect: crate::paint::Rect::new(r.x, y, w, h), item_h, items, hover: selected_row, first });
        self.select_popup_opts = opts;
        self.focus_by_user(Some(n));
        self.need_paint = true;
    }

    fn close_select_popup(&mut self) {
        if self.select_popup.take().is_some() {
            self.select_popup_opts.clear();
            self.need_paint = true;
        }
    }

    /// Pick row `row` of the open dropdown.
    fn choose_select_row(&mut self, row: usize) {
        let Some(p) = self.select_popup.clone() else { return };
        if let (Some(Some(o)), Some(it)) = (self.select_popup_opts.get(row).copied(), p.items.get(row)) {
            if !it.1 {
                let opts: Vec<NodeId> = self.doc.descendants(p.node).filter(|&x| self.doc.is_tag(x, "option")).collect();
                let was = self.doc.element(o).map(|e| e.selected.unwrap_or_else(|| e.has_attr("selected"))).unwrap_or(false);
                for x in opts {
                    if let Some(e) = self.doc.element_mut(x) {
                        e.selected = Some(x == o);
                    }
                }
                self.doc.bump_all();
                if !was {
                    self.js_tasks.push(JsTask::Dispatch { target: p.node, ty: "input".into(), init: EventInit { bubbles: true, ..Default::default() }, then: DefaultAction::None });
                    self.fire_change(p.node);
                }
            }
        }
        self.close_select_popup();
    }

    /// Mouse input while a dropdown is open; returns true if it was consumed.
    fn select_popup_mouse(&mut self, kind: u8, x: f32, y: f32) -> bool {
        let Some(p) = self.select_popup.as_ref() else { return false };
        let (dx, dy) = (x + self.scroll.0, y + self.scroll.1);
        let r = p.rect;
        let inside = dx >= r.x && dx <= r.right() && dy >= r.y && dy <= r.bottom();
        let row = if inside { Some(p.first + ((dy - r.y - 1.0) / p.item_h).floor().max(0.0) as usize).filter(|&k| k < p.items.len()) } else { None };
        match kind {
            0 => {
                if let Some(p) = self.select_popup.as_mut() {
                    if row.is_some() && p.hover != row {
                        p.hover = row;
                        self.need_paint = true;
                    }
                }
                inside
            }
            1 => {
                match row {
                    Some(k) => self.choose_select_row(k),
                    None => {
                        // A press on the select itself closes it (the click must not reopen it).
                        let node = p.node;
                        let on_select = self.hit_test(x, y).map(|(n, _, _)| self.element_of(n)) == Some(node);
                        self.close_select_popup();
                        if on_select {
                            self.select_popup_suppress = true;
                        }
                    }
                }
                true
            }
            _ => inside,
        }
    }

    fn select_popup_key(&mut self, key: &Key) -> bool {
        let Some(p) = self.select_popup.as_mut() else { return false };
        let n = p.items.len();
        let rows = ((p.rect.h - 2.0) / p.item_h).floor().max(1.0) as usize;
        let step = |p: &mut crate::paint::builder::SelectPopupPaint, dir: i32| {
            let mut k = p.hover.map(|h| h as i32).unwrap_or(-1);
            for _ in 0..n {
                k = (k + dir).clamp(0, n as i32 - 1);
                if !p.items[k as usize].1 && !p.items[k as usize].2 {
                    break;
                }
            }
            p.hover = Some(k as usize);
            if (k as usize) < p.first {
                p.first = k as usize;
            } else if k as usize >= p.first + rows {
                p.first = k as usize + 1 - rows;
            }
        };
        match key {
            Key::Down => step(p, 1),
            Key::Up => step(p, -1),
            Key::Enter | Key::Space | Key::Tab => {
                if let Some(h) = p.hover {
                    self.choose_select_row(h);
                } else {
                    self.close_select_popup();
                }
                return true;
            }
            Key::Escape => {
                self.close_select_popup();
                return true;
            }
            _ => return true,
        }
        self.need_paint = true;
        true
    }

    pub fn form_of(&self, n: NodeId) -> Option<NodeId> {
        if let Some(fid) = self.doc.attr(n, "form") {
            return self.doc.get_element_by_id(fid);
        }
        let mut cur = self.doc.parent_element(n);
        while let Some(c) = cur {
            if self.doc.is_tag(c, "form") {
                return Some(c);
            }
            cur = self.doc.parent_element(c);
        }
        None
    }

    pub fn reset_form(&mut self, f: NodeId) {
        let ctrls: Vec<NodeId> = self.doc.descendants(f).filter(|&c| matches!(self.doc.tag(c), "input" | "textarea" | "select" | "option")).collect();
        for c in ctrls {
            if let Some(e) = self.doc.element_mut(c) {
                e.value = None;
                e.checked = None;
                e.selected = None;
            }
        }
        self.doc.bump_all();
    }

    pub fn request_submit(&mut self, form: NodeId, submitter: Option<NodeId>) {
        self.js_tasks.push(JsTask::FormAction { form, submitter, reset: false });
    }

    /// Build the form data set (unless script already did) and navigate.
    pub fn submit_form(&mut self, form: NodeId, submitter: Option<NodeId>, given: Option<FormEntries>) {
        // A form in a local frame's document submits in that frame, with its URLs.
        let home = self.frame_of_node(form);
        let home_url = home.and_then(|_| self.frame_doc_url(form));
        let home_base = home_url.as_ref().map(|u| self.local_base_of(self.doc.tree_document(form), u.as_str()));
        let doc = &self.doc;
        let doc_url = home_url.clone().unwrap_or_else(|| doc.url.clone());
        let mut multipart = None;
        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut given_action = None;
        let given_pairs = match given {
            Some(g) => {
                multipart = g.multipart;
                given_action = g.action;
                Some(g.pairs)
            }
            None => None,
        };
        let in_form = |c: NodeId| -> bool {
            match doc.attr(c, "form") {
                Some(id) => doc.get_element_by_id(id) == Some(form),
                None => doc.contains(form, c),
            }
        };
        let ctrls: Vec<NodeId> = if given_pairs.is_some() {
            Vec::new()
        } else {
            doc.descendants(DOCUMENT).filter(|&c| matches!(doc.tag(c), "input" | "textarea" | "select" | "button") && in_form(c)).collect()
        };
        if let Some(g) = given_pairs {
            pairs = g;
        }
        for c in ctrls {
            let el = doc.element(c).unwrap();
            if el.has_attr("disabled") {
                continue;
            }
            let name = match el.attr("name") {
                Some(n) if !n.is_empty() => n.to_string(),
                _ => continue,
            };
            match el.name.as_str() {
                "input" => {
                    let t = el.attr("type").unwrap_or("text").to_ascii_lowercase();
                    match t.as_str() {
                        "checkbox" | "radio" => {
                            if el.checked.unwrap_or_else(|| el.has_attr("checked")) {
                                pairs.push((name, el.attr("value").unwrap_or("on").to_string()));
                            }
                        }
                        "submit" | "image" | "button" | "reset" => {
                            if Some(c) == submitter && t != "reset" && t != "button" {
                                pairs.push((name, el.attr("value").unwrap_or("").to_string()));
                            }
                        }
                        "file" => pairs.push((name, String::new())),
                        _ => pairs.push((name, el.value.clone().or_else(|| el.attr("value").map(|s| s.to_string())).unwrap_or_default())),
                    }
                }
                "textarea" => pairs.push((name, el.value.clone().unwrap_or_else(|| doc.text_content(c)))),
                "select" => {
                    for o in doc.descendants(c) {
                        if doc.is_tag(o, "option") {
                            let oe = doc.element(o).unwrap();
                            if oe.selected.unwrap_or_else(|| oe.has_attr("selected")) {
                                pairs.push((name.clone(), oe.attr("value").map(|s| s.to_string()).unwrap_or_else(|| crate::dom::collapse_ws(&doc.text_content(o)))));
                            }
                        }
                    }
                    // Default: first option when none selected.
                    if !pairs.iter().any(|(n, _)| *n == name) {
                        if let Some(o) = doc.descendants(c).find(|&o| doc.is_tag(o, "option")) {
                            let oe = doc.element(o).unwrap();
                            pairs.push((name, oe.attr("value").map(|s| s.to_string()).unwrap_or_else(|| crate::dom::collapse_ws(&doc.text_content(o)))));
                        }
                    }
                }
                "button" => {
                    if Some(c) == submitter {
                        pairs.push((name, el.attr("value").unwrap_or("").to_string()));
                    }
                }
                _ => {}
            }
        }
        let action_attr = submitter.and_then(|s| doc.attr(s, "formaction")).or_else(|| doc.attr(form, "action")).unwrap_or("");
        let action = if let Some(a) = given_action {
            a
        } else if action_attr.trim().is_empty() {
            doc_url.clone()
        } else {
            match &home_base {
                Some(b) => b.join(action_attr.trim()).unwrap_or(doc_url.clone()),
                None => doc.resolve_url(action_attr).unwrap_or(doc.url.clone()),
            }
        };
        let method = submitter.and_then(|s| doc.attr(s, "formmethod")).or_else(|| doc.attr(form, "method")).unwrap_or("get").to_ascii_lowercase();
        // The form's encoding: its accept-charset (first known label), else the document's.
        let charset: &'static str = doc
            .attr(form, "accept-charset")
            .and_then(|a| a.split_ascii_whitespace().find_map(crate::html::encoding::canonical_name))
            .unwrap_or(doc.charset);
        let encoded: String = if charset == "UTF-8" {
            url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs.iter()).finish()
        } else {
            let enc = crate::html::encoding::override_fn(move |s: &str| {
                std::borrow::Cow::Owned(crate::html::encoding::encode_with(charset, s, &|c, out: &mut Vec<u8>| out.extend_from_slice(format!("&#{};", c as u32).as_bytes())))
            });
            url::form_urlencoded::Serializer::new(String::new()).encoding_override(Some(&enc)).extend_pairs(pairs.iter()).finish()
        };
        // The browsing context the result goes to: this one, a new tab, or a
        // named iframe (hidden "transport" iframes are a common pattern for
        // posting data without leaving the page).
        let target = submitter.and_then(|s| doc.attr(s, "formtarget")).or_else(|| doc.attr(form, "target")).map(|t| t.trim().to_string()).unwrap_or_default();
        let target_lc = target.to_ascii_lowercase();
        let target_blank = target_lc == "_blank";
        let target_frame = if target.is_empty() || matches!(target_lc.as_str(), "_self" | "_top" | "_parent" | "_blank") {
            None
        } else {
            Some(doc.descendants(DOCUMENT).find(|&n| (doc.is_tag(n, "iframe") || doc.is_tag(n, "frame")) && doc.attr(n, "name") == Some(target.as_str())))
        };
        let enctype = submitter.and_then(|s| doc.attr(s, "formenctype")).or_else(|| doc.attr(form, "enctype")).unwrap_or("").trim().to_ascii_lowercase();
        let (post_body, post_type) = match multipart {
            Some((b, t)) => (b, t),
            None if enctype == "text/plain" => {
                // text/plain: "name=value" CRLF per entry, in the form's encoding.
                let mut plain = String::new();
                for (k, v) in &pairs {
                    plain.push_str(k);
                    plain.push('=');
                    plain.push_str(v);
                    plain.push_str("\r\n");
                }
                let bytes = if charset == "UTF-8" {
                    plain.into_bytes()
                } else {
                    crate::html::encoding::encode_with(charset, &plain, &|c, out: &mut Vec<u8>| out.extend_from_slice(format!("&#{};", c as u32).as_bytes()))
                };
                (bytes, "text/plain".to_string())
            }
            None => (encoded.clone().into_bytes(), "application/x-www-form-urlencoded".to_string()),
        };
        let req = if method == "post" {
            NavRequest {
                url: action.to_string(),
                method: "POST".into(),
                body: Some(post_body),
                content_type: Some(post_type),
                referrer: Some(doc_url.clone()),
                push: true,
                restore_scroll: None,
            }
        } else {
            let mut u = action.clone();
            u.set_query(Some(&encoded));
            u.set_fragment(None);
            let mut r = NavRequest::get(u.as_str());
            r.referrer = Some(doc_url.clone());
            r
        };
        // The navigation is planned (a task); a later submission of the same
        // form replaces a plan that has not run yet.
        self.js_tasks.retain(|t| !matches!(t, JsTask::PlannedNav(n) if n.form == form));
        self.js_tasks.push(JsTask::PlannedNav(Box::new(FormNav { form, req, target_frame, target_blank, target_lc, method, home })));
    }

    /// Run a form submission's planned navigation.
    pub fn run_form_nav(&mut self, nav: FormNav) {
        let FormNav { req, target_frame, target_blank, target_lc, method, home, .. } = nav;
        match target_frame {
            // A named iframe: GET loads it there; a POST is sent without
            // leaving the page (the frame's response isn't shown).
            Some(Some(frame)) => {
                let local = self.frame_views.get(&frame).map(|f| f.local.is_some()).unwrap_or(false) || self.local_frame_ok(frame, &req.url);
                if method == "post" && local {
                    // A local frame shows the response.
                    self.frame_post = Some((frame, req.body.clone().unwrap_or_default(), req.content_type.clone().unwrap_or_default()));
                    let referrer = req.referrer.clone();
                    if !self.local_frame_navigate(frame, &req.url, referrer) {
                        self.frame_post = None;
                        self.background_post(&req);
                    }
                } else if method == "post" {
                    self.background_post(&req);
                } else {
                    self.doc.set_attr(frame, "src", &req.url);
                    self.need_layout = true;
                }
            }
            // A name no frame has opens a new browsing context.
            Some(None) if method != "post" => self.emit(PageEvent::OpenTab(req.url.clone())),
            Some(None) => self.background_post(&req),
            None if target_blank && method != "post" => self.emit(PageEvent::OpenTab(req.url.clone())),
            None if target_blank => self.background_post(&req),
            None => match home {
                Some(frame) if !matches!(target_lc.as_str(), "_top" | "_parent") => {
                    let referrer = req.referrer.clone();
                    if method == "post" {
                        self.frame_post = Some((frame, req.body.clone().unwrap_or_default(), req.content_type.clone().unwrap_or_default()));
                        if !self.local_frame_navigate(frame, &req.url, referrer) {
                            self.frame_post = None;
                            self.background_post(&req);
                        }
                    } else if !self.local_frame_navigate(frame, &req.url, referrer) {
                        self.doc.set_attr(frame, "src", &req.url);
                        self.need_layout = true;
                    }
                }
                _ => self.pending_nav = Some(req),
            },
        }
    }

    /// Send a form POST whose response goes to another browsing context
    /// (a named iframe or a new window) without navigating this page.
    fn background_post(&mut self, req: &NavRequest) {
        let Ok(url) = Url::parse(&req.url) else { return };
        let mut r = Request::get(url, RequestKind::Document, Some(self.doc.url.clone()));
        r.method = "POST".into();
        r.body = req.body.clone();
        if let Some(ct) = &req.content_type {
            r.headers.push(("Content-Type".into(), ct.clone()));
        }
        net::fetch_async(r, |_| {});
    }

    /// A held key was released: its keyup.
    pub fn on_key_up(&mut self, key: Key, mods: Mods) {
        let name = key_name_of(&key);
        if !self.held_keys.remove(&held_key_id(&name)) {
            return;
        }
        let target = self.doc.focused.or(self.doc.body()).unwrap_or(DOCUMENT);
        let then = self.keyup_default(&name, mods);
        self.js_tasks.push(JsTask::Dispatch {
            target,
            ty: "keyup".into(),
            init: EventInit { key: name.clone(), code: key_code(&name), mods, bubbles: true, cancelable: true, ..Default::default() },
            then,
        });
    }

    /// A keyup's default action: Space released on the button it went down on clicks it.
    pub fn keyup_default(&mut self, name: &str, mods: Mods) -> DefaultAction {
        if name == " " {
            if let Some(t) = self.space_target.take() {
                if self.doc.focused == Some(t) && self.doc.valid(t) {
                    return DefaultAction::ClickEvent { target: t, mods };
                }
            }
        }
        DefaultAction::None
    }

    pub fn on_key(&mut self, key: Key, mods: Mods) {
        let target = self.doc.focused.or(self.doc.body()).unwrap_or(DOCUMENT);
        self.keydown_target = Some(target);
        let key_name = key_name_of(&key);
        self.js_tasks.push(JsTask::Dispatch {
            target,
            ty: "keydown".into(),
            init: EventInit { key: key_name.clone(), code: key_code(&key_name), mods, bubbles: true, cancelable: true, repeat: self.key_repeat, ..Default::default() },
            then: DefaultAction::Key { key, mods },
        });
        // (keypress and keyup follow the keydown's default action; see
        // the Key default action in js::pump.)
    }

    pub fn on_text(&mut self, text: String) {
        if let Some(f) = self.doc.focused {
            if !self.is_editable(f) && self.is_edit_host(f) {
                // Editing hosts: script runs beforeinput, the editing command and input.
                use crate::js::host::HostValue as H;
                self.caret_on = true;
                self.js_tasks.push(JsTask::CallGlobal("__ohb_editAction", vec![H::from("insertText"), H::from(text.as_str())]));
                return;
            }
            if self.is_editable(f) {
                self.js_tasks.push(JsTask::Dispatch {
                    target: f,
                    ty: "beforeinput".into(),
                    init: EventInit { key: text.clone(), bubbles: true, cancelable: true, ..Default::default() },
                    then: DefaultAction::Text(text),
                });
            }
        }
    }

    pub fn default_text(&mut self, text: &str) {
        let f = match self.doc.focused {
            Some(f) if self.is_editable(f) => f,
            _ => return,
        };
        let is_textarea = self.doc.is_tag(f, "textarea");
        let text: String = if is_textarea { text.to_string() } else { text.replace(['\n', '\r'], "") };
        let mut v = self.control_value(f);
        let max = self.doc.attr(f, "maxlength").and_then(|m| m.trim().parse::<usize>().ok());
        // Typing replaces the selection.
        if let Some(a) = self.field_anchor.take() {
            let (s, e) = (a.min(self.caret).min(v.len()), a.max(self.caret).min(v.len()));
            if s < e && v.is_char_boundary(s) && v.is_char_boundary(e) {
                v.replace_range(s..e, "");
                self.caret = s;
                self.set_control_value(f, &v);
            }
        }
        let c = self.caret.min(v.len());
        // maxlength (UTF-16 code units): the text is cut at a code point boundary.
        let text = match max {
            Some(m) => {
                let room = m.saturating_sub(v.encode_utf16().count());
                let mut used = 0;
                let cut: String = text.chars().take_while(|ch| { used += ch.len_utf16(); used <= room }).collect();
                if cut.is_empty() {
                    return;
                }
                cut
            }
            None => text,
        };
        v.insert_str(c, &text);
        self.caret = c + text.len();
        self.set_control_value(f, &v);
        self.caret_on = true;
        self.js_tasks.push(JsTask::Dispatch { target: f, ty: "input".into(), init: EventInit { key: text, bubbles: true, ..Default::default() }, then: DefaultAction::None });
    }

    /// Default key handling after keydown wasn't cancelled. Returns true if consumed.
    pub fn default_key(&mut self, key: Key, mods: Mods) -> bool {
        let f = self.doc.focused;
        if let Some(h) = f.filter(|&f| !self.is_editable(f) && self.is_edit_host(f)) {
            if self.edit_host_key(h, &key, mods) {
                return true;
            }
        }
        if (mods.ctrl || mods.meta) && !f.map(|f| self.is_editable(f)).unwrap_or(false) {
            if let Key::Char(ref s) = key {
                if s.eq_ignore_ascii_case("c") {
                    let t = self.selection_text();
                    if !t.is_empty() {
                        self.emit(PageEvent::Copy(t));
                    }
                    return true;
                }
                if s.eq_ignore_ascii_case("a") {
                    self.select_all_text();
                    return true;
                }
            }
        }
        if let Some(f) = f {
            if self.is_editable(f) {
                let mut v = self.control_value(f);
                let mut c = self.caret.min(v.len());
                // (A script may have changed the value under the caret.)
                while c > 0 && !v.is_char_boundary(c) {
                    c -= 1;
                }
                let prev = |s: &str, i: usize| s[..i].char_indices().last().map(|(p, _)| p).unwrap_or(0);
                let next = |s: &str, i: usize| s[i..].chars().next().map(|ch| i + ch.len_utf8()).unwrap_or(i);
                let word_prev = |s: &str, i: usize| {
                    let t = s[..i].trim_end();
                    t.rfind(|ch: char| ch.is_whitespace()).map(|p| p + 1).unwrap_or(0)
                };
                // The selection (anchor..caret), if any.
                let sel = self.field_anchor.map(|a| (a.min(c).min(v.len()), a.max(c).min(v.len()))).filter(|(s, e)| s < e && v.is_char_boundary(*s) && v.is_char_boundary(*e));
                let extend = mods.shift && matches!(key, Key::Left | Key::Right | Key::Home | Key::End | Key::Up | Key::Down);
                if extend {
                    if self.field_anchor.is_none() {
                        self.field_anchor = Some(c);
                    }
                } else if matches!(key, Key::Left | Key::Right | Key::Home | Key::End | Key::Up | Key::Down) {
                    if let Some((s, e)) = sel {
                        self.field_anchor = None;
                        if matches!(key, Key::Left | Key::Right) {
                            self.caret = if key == Key::Left { s } else { e };
                            self.caret_on = true;
                            self.need_paint = true;
                            return true;
                        }
                    }
                    self.field_anchor = None;
                }
                let cmd = mods.ctrl || mods.meta;
                if let Some((s, e)) = sel {
                    let cut = matches!(key, Key::Backspace | Key::Delete) || (cmd && matches!(&key, Key::Char(k) if k.eq_ignore_ascii_case("x")));
                    if cmd && matches!(&key, Key::Char(k) if k.eq_ignore_ascii_case("c") || k.eq_ignore_ascii_case("x")) {
                        if self.doc.attr(f, "type").map(|t| !t.eq_ignore_ascii_case("password")).unwrap_or(true) {
                            self.emit(PageEvent::Copy(v[s..e].to_string()));
                        }
                    }
                    if cut {
                        v.replace_range(s..e, "");
                        self.caret = s;
                        self.field_anchor = None;
                        self.set_control_value(f, &v);
                        self.fire_input(f);
                        self.caret_on = true;
                        self.need_paint = true;
                        return true;
                    }
                    if cmd && matches!(&key, Key::Char(k) if k.eq_ignore_ascii_case("c")) {
                        return true;
                    }
                }
                match key {
                    Key::Backspace => {
                        if c > 0 {
                            let p = if mods.ctrl || mods.alt { word_prev(&v, c) } else { prev(&v, c) };
                            v.replace_range(p..c, "");
                            self.caret = p;
                            self.set_control_value(f, &v);
                            self.fire_input(f);
                        }
                    }
                    Key::Delete => {
                        if c < v.len() {
                            let n = next(&v, c);
                            v.replace_range(c..n, "");
                            self.set_control_value(f, &v);
                            self.fire_input(f);
                        }
                    }
                    Key::Left => self.caret = if mods.ctrl || mods.alt { word_prev(&v, c) } else { prev(&v, c) },
                    Key::Right => self.caret = next(&v, c),
                    Key::Home | Key::Up if !self.doc.is_tag(f, "textarea") || key == Key::Home => self.caret = 0,
                    Key::End | Key::Down if !self.doc.is_tag(f, "textarea") || key == Key::End => self.caret = v.len(),
                    Key::Enter => {
                        if self.doc.is_tag(f, "textarea") {
                            self.default_text("\n");
                        } else if let Some(form) = self.form_of(f) {
                            let submitter = self.doc.descendants(form).find(|&b| {
                                (self.doc.is_tag(b, "button") && self.doc.attr(b, "type").map(|t| t.eq_ignore_ascii_case("submit")).unwrap_or(true))
                                    || (self.doc.is_tag(b, "input") && self.doc.attr(b, "type").map(|t| t.eq_ignore_ascii_case("submit") || t.eq_ignore_ascii_case("image")).unwrap_or(false))
                            });
                            // Implicit submission clicks the default button (script
                            // can cancel that click); without one the form submits.
                            match submitter {
                                Some(b) if crate::css::selector::actually_disabled(&self.doc, b) => {}
                                Some(b) => self.synthetic_click(b, mods),
                                None => self.request_submit(form, None),
                            }
                        }
                    }
                    Key::Tab => {
                        self.focus_next(!mods.shift);
                    }
                    Key::Escape => {
                        // A close request: popovers and dialogs' close watchers (script).
                        self.js_tasks.push(JsTask::Eval("__ohb_escape()".into()));
                    }
                    Key::Char(ref s) if (mods.ctrl || mods.meta) && s.eq_ignore_ascii_case("a") => {
                        // Select all of the field's text.
                        self.field_anchor = if v.is_empty() { None } else { Some(0) };
                        self.caret = v.len();
                    }
                    Key::Char(ref s) if (mods.ctrl || mods.meta) && s.eq_ignore_ascii_case("c") => {}
                    // Typing (the text arrives separately): never scrolls the page.
                    Key::Space => return true,
                    Key::Char(_) if !mods.ctrl && !mods.meta => return true,
                    _ => return false,
                }
                self.caret_on = true;
                self.need_paint = true;
                return true;
            }
            if self.doc.is_tag(f, "select") {
                match key {
                    Key::Up => {
                        self.cycle_select(f, -1);
                        return true;
                    }
                    Key::Down => {
                        self.cycle_select(f, 1);
                        return true;
                    }
                    _ => {}
                }
            }
            if key == Key::Enter && (self.doc.is_tag(f, "a") || self.doc.is_tag(f, "button") || self.doc.is_tag(f, "input")) {
                // A click event (pages handle activation in click listeners).
                self.synthetic_click(f, mods);
                return true;
            }
            if key == Key::Space && (self.doc.is_tag(f, "button") || self.doc.is_tag(f, "input")) {
                // Space activates on keyup, if focus is still there (and was
                // there for the keydown).
                if self.keydown_target == Some(f) {
                    self.space_target = Some(f);
                }
                return true;
            }
        }
        if key == Key::Tab {
            self.focus_next(!mods.shift);
            return true;
        }
        if key == Key::Escape {
            // A close request for popovers' and dialogs' close watchers.
            self.js_tasks.push(JsTask::Eval("__ohb_escape()".into()));
            return true;
        }
        false
    }

    fn fire_input(&mut self, f: NodeId) {
        self.js_tasks.push(JsTask::Dispatch { target: f, ty: "input".into(), init: EventInit { bubbles: true, ..Default::default() }, then: DefaultAction::None });
    }

    fn focus_next(&mut self, forward: bool) {
        let all: Vec<NodeId> = self
            .doc
            .descendants(DOCUMENT)
            .filter(|&n| self.is_focusable(n) && !self.doc.attr(n, "tabindex").and_then(|t| t.trim().parse::<i64>().ok()).map(|t| t < 0).unwrap_or(false) && self.node_box_visible(n))
            .collect();
        if all.is_empty() {
            return;
        }
        let cur = self.doc.focused.and_then(|f| all.iter().position(|&n| n == f));
        let next = match (cur, forward) {
            (Some(i), true) => (i + 1) % all.len(),
            (Some(i), false) => (i + all.len() - 1) % all.len(),
            (None, true) => 0,
            (None, false) => all.len() - 1,
        };
        let n = all[next];
        self.focus_by_user(Some(n));
        if let Some(r) = self.node_rect(n) {
            let (_, vh) = self.viewport();
            if r.y < self.scroll.1 || r.bottom() > self.scroll.1 + vh {
                let y = (r.y - vh / 3.0).max(0.0);
                self.scroll.1 = y;
                self.emit(PageEvent::ScrollTo(self.scroll.0, y));
            }
        }
    }

    fn node_box_visible(&self, n: NodeId) -> bool {
        self.layout.as_ref().map(|t| t.node_box.contains_key(&n)).unwrap_or(false)
    }

    /// Wheel over a scrollable element: scroll it. Returns false if no element scrolled.
    /// Scroll for a wheel: the scroller under the pointer, else the viewport
    /// (the UI's, or the page's own when automated).
    pub fn default_wheel(&mut self, x: f32, y: f32, dx: f32, dy: f32) {
        if self.on_wheel(x, y, dx, dy) {
            return;
        }
        if self.in_frame_ctx() {
            // A local frame's viewport scrolls itself.
            let (vw, vh) = self.viewport();
            let (dw, dh) = self.layout.as_ref().map(|l| (l.doc_width, l.doc_height)).unwrap_or((vw, vh));
            let ns = ((self.scroll.0 + dx).clamp(0.0, (dw - vw).max(0.0)), (self.scroll.1 + dy).clamp(0.0, (dh - vh).max(0.0)));
            if ns != self.scroll {
                self.scroll = ns;
                let d = self.doc.render_root.get();
                self.note_scrolled(d);
            }
            return;
        }
        if crate::js::wpt_enabled() {
            self.ensure_layout();
            let (vw, vh) = self.viewport();
            let (dw, dh) = self.layout.as_ref().map(|l| (l.doc_width, l.doc_height)).unwrap_or((vw, vh));
            let ns = ((self.scroll.0 + dx).clamp(0.0, (dw - vw).max(0.0)), (self.scroll.1 + dy).clamp(0.0, (dh - vh).max(0.0)));
            if ns != self.scroll {
                self.scroll = ns;
                self.emit(PageEvent::ScrollTo(ns.0, ns.1));
                self.note_scrolled(DOCUMENT);
            }
            return;
        }
        // Nothing scrolled inside: let the UI scroll the viewport.
        self.emit(PageEvent::ScrollTo(f32::NAN, dy));
    }

    pub fn on_wheel(&mut self, x: f32, y: f32, dx: f32, dy: f32) -> bool {
        let hit = self.hit_test(x, y);
        let mut scroller = hit.and_then(|(_, _, s)| s);
        let tree = match &self.layout {
            Some(t) => t,
            None => return false,
        };
        while let Some(n) = scroller {
            if let Some(&b) = tree.node_box.get(&n) {
                let bx = &tree.boxes[b];
                let client_h = bx.h - bx.border[0] - bx.border[2];
                let client_w = bx.w - bx.border[1] - bx.border[3];
                let maxy = (bx.scroll_h - client_h).max(0.0);
                let maxx = (bx.scroll_w - client_w).max(0.0);
                let cur = self.elem_scroll.get(&n).copied().unwrap_or((0.0, 0.0));
                let ny = (cur.1 + dy).clamp(0.0, maxy);
                let nx = (cur.0 + dx).clamp(0.0, maxx);
                if (ny - cur.1).abs() > 0.1 || (nx - cur.0).abs() > 0.1 {
                    self.elem_scroll.insert(n, (nx, ny));
                    self.need_paint = true;
                    self.note_scrolled(n);
                    return true;
                }
                // Find an outer scroller.
                let mut p = bx.parent;
                scroller = None;
                while let Some(pp) = p {
                    let pb = &tree.boxes[pp];
                    if (pb.style.overflow_y.scrollable() || pb.style.overflow_x.scrollable()) && pb.node.is_some() {
                        scroller = pb.node;
                        break;
                    }
                    p = pb.parent;
                }
            } else {
                break;
            }
        }
        false
    }

    // ------------------------------------------------------------------ find

    pub fn find(&mut self, q: &str, forward: bool) {
        let ql = q.to_lowercase();
        if ql.is_empty() {
            self.find_matches.clear();
            self.need_paint = true;
            self.emit(PageEvent::FindResult(0, 0));
            return;
        }
        let mut matches = Vec::new();
        for n in self.doc.descendants(DOCUMENT) {
            if let NodeData::Text(t) = &self.doc.node(n).data {
                let tl = t.to_lowercase();
                let mut from = 0;
                while let Some(i) = tl[from..].find(&ql) {
                    matches.push((n, from + i, from + i + ql.len()));
                    from += i + ql.len().max(1);
                    if matches.len() > 5000 {
                        break;
                    }
                }
            }
        }
        // Only rendered text.
        if let Some(t) = &self.layout {
            matches.retain(|(n, _, _)| t.node_box.contains_key(n));
        }
        let same = matches.len() == self.find_matches.len() && matches.iter().zip(&self.find_matches).all(|(a, b)| a == b);
        if same && !matches.is_empty() {
            let n = matches.len();
            self.find_idx = if forward { (self.find_idx + 1) % n } else { (self.find_idx + n - 1) % n };
        } else {
            self.find_idx = 0;
        }
        self.find_matches = matches;
        self.need_paint = true;
        if let Some(&(n, _, _)) = self.find_matches.get(self.find_idx) {
            if let Some(r) = self.node_rect(n) {
                let (_, vh) = self.viewport();
                let y = (r.y - vh / 3.0).max(0.0);
                self.scroll.1 = y;
                self.emit(PageEvent::ScrollTo(self.scroll.0, y));
            }
        }
        self.emit(PageEvent::FindResult(self.find_matches.len(), if self.find_matches.is_empty() { 0 } else { self.find_idx + 1 }));
    }

    pub fn find_close(&mut self) {
        self.find_matches.clear();
        self.need_paint = true;
    }

    // --------------------------------------------------------------- dispatch

    pub fn handle(&mut self, msg: PageMsg) -> bool {
        match msg {
            PageMsg::Navigate(req) => self.navigate(req),
            PageMsg::Reload => self.reload(),
            PageMsg::Stop => self.stop(),
            PageMsg::Back => self.go(-1),
            PageMsg::Forward => self.go(1),
            PageMsg::Resize { w, h, dpr } => {
                if (w, h) != self.win || dpr != self.dpr {
                    let width_changed = w != self.win.0;
                    self.win = (w, h);
                    // <link media> / <style media> are evaluated when the index is built.
                    if self.sheets.iter().any(|s| s.node.map(|n| self.doc.attr(n, "media").is_some()).unwrap_or(false)) {
                        { self.sheets_dirty = true; crate::page::full_why(file!(), line!()); }
                    }
                    if dpr != self.dpr {
                        self.dpr = dpr;
                        self.res.dpr = dpr;
                    }
                    self.need_style = true;
                    { self.full_style = true; crate::page::full_why(file!(), line!()); }
                    if width_changed || self.layout.is_none() {
                        self.need_layout = true;
                    } else {
                        self.need_layout = true;
                    }
                    self.js_tasks.push(JsTask::Resize);
                }
            }
            PageMsg::Zoom(z) => {
                self.zoom = z.clamp(0.25, 5.0);
                self.need_style = true;
                { self.full_style = true; crate::page::full_why(file!(), line!()); }
                self.need_layout = true;
            }
            PageMsg::MouseMove { x, y } if self.select_popup.is_some() && self.select_popup_mouse(0, x, y) => {}
            PageMsg::MouseDown { x, y, .. } if self.select_popup.is_some() && self.select_popup_mouse(1, x, y) => {}
            PageMsg::MouseUp { x, y, .. } if self.select_popup.is_some() && self.select_popup_mouse(2, x, y) => {}
            PageMsg::Key { key, .. } if self.select_popup.is_some() && self.select_popup_key(&key) => {}
            PageMsg::Wheel { x, y, dy, .. } if self.select_popup.as_ref().map(|p| { let (dx, dy2) = (x + self.scroll.0, y + self.scroll.1); dx >= p.rect.x && dx <= p.rect.right() && dy2 >= p.rect.y && dy2 <= p.rect.bottom() }).unwrap_or(false) => {
                if let Some(p) = self.select_popup.as_mut() {
                    let rows = ((p.rect.h - 2.0) / p.item_h).floor().max(1.0) as usize;
                    let max_first = p.items.len().saturating_sub(rows);
                    let d = (dy / p.item_h).round() as i32;
                    let d = if d == 0 { dy.signum() as i32 } else { d };
                    p.first = (p.first as i32 + d).clamp(0, max_first as i32) as usize;
                    self.need_paint = true;
                }
            }
            PageMsg::MouseMove { x, y } => {
                if !self.frame_mouse(0, x, y, 0, Mods::default()) {
                    self.on_mouse_move(x, y)
                }
            }
            PageMsg::MouseDelta { dx, dy } => {
                if self.pl_active {
                    self.move_delta.0 += dx;
                    self.move_delta.1 += dy;
                    let (x, y) = self.last_move.unwrap_or((0.0, 0.0));
                    if self.pending_move.replace((x, y, None)).is_none() {
                        self.js_tasks.push(JsTask::MouseMove);
                    }
                }
            }
            PageMsg::PointerLockLost => {
                if self.pl_active {
                    self.pl_active = false;
                    self.js_tasks.push(JsTask::Eval("__ohb_pointerLockLost && __ohb_pointerLockLost()".into()));
                }
            }
            PageMsg::MouseDown { x, y, button, mods } => {
                if !self.frame_mouse(1, x, y, button, mods) {
                    self.on_mouse_down(x, y, button, mods)
                }
            }
            PageMsg::MouseUp { x, y, button, mods } => {
                if !self.frame_mouse(2, x, y, button, mods) {
                    self.on_mouse_up(x, y, button, mods)
                }
            }
            PageMsg::MouseLeave => {
                if let Some(f) = self.hover_frame.take() {
                    self.send_to_frame(f, PageMsg::MouseLeave);
                }
                if !self.hover_chain.is_empty() {
                    self.hover_chain.clear();
                    self.need_style = true;
                    { self.full_style = true; crate::page::full_why(file!(), line!()); }
                }
                if !self.last_status.is_empty() {
                    self.last_status.clear();
                    self.emit(PageEvent::Status(String::new()));
                }
            }
            PageMsg::WheelNotify { x, y, dx, dy } => {
                if let Some(target) = self.hit_test(x, y).map(|(n, _, _)| self.element_of(n)).or(self.doc.body()) {
                    self.js_tasks.push(JsTask::Dispatch {
                        target,
                        ty: "wheel".into(),
                        init: EventInit { x, y, dx, dy, bubbles: true, cancelable: false, ..Default::default() },
                        then: DefaultAction::None,
                    });
                }
            }
            PageMsg::Wheel { x, y, dx, dy } => {
                if self.frame_wheel(x, y, dx, dy) {
                } else if self.wheel_listeners {
                    // Script may cancel it: dispatch first, scroll as the default action.
                    // Wheel events in one transaction (no pause over 500ms) keep their first target.
                    // (A target that lost its box or moved ends the transaction.)
                    self.ensure_layout();
                    let latched = match self.wheel_tx {
                        Some((n, t, pos)) if t.elapsed() < std::time::Duration::from_millis(500) && self.doc.valid(n) && self.doc.is_connected(n) => {
                            self.node_rect(n).filter(|r| (r.x - pos.0).abs() < 0.5 && (r.y - pos.1).abs() < 0.5 && (r.w > 0.0 || r.h > 0.0)).map(|_| n)
                        }
                        _ => None,
                    };
                    let target = latched.or_else(|| self.hit_test(x, y).map(|(n, _, _)| self.element_of(n))).or(self.doc.body()).or(self.doc.document_element()).unwrap_or(DOCUMENT);
                    let pos = self.node_rect(target).map(|r| (r.x, r.y)).unwrap_or((0.0, 0.0));
                    self.wheel_tx = Some((target, std::time::Instant::now(), pos));
                    self.js_tasks.push(JsTask::Dispatch {
                        target,
                        ty: "wheel".into(),
                        init: EventInit { x, y, dx, dy, bubbles: true, cancelable: true, ..Default::default() },
                        then: DefaultAction::Wheel { x, y, dx, dy },
                    });
                } else {
                    self.default_wheel(x, y, dx, dy);
                }
            }
            PageMsg::Scrolled { x, y } => {
                if (x, y) != self.scroll {
                    self.close_select_popup();
                    self.scroll = (x, y);
                    self.note_scrolled(DOCUMENT);
                    if self.pdf.is_some() {
                        self.need_paint = true;
                    }
                }
            }
            PageMsg::Key { key, mods } if self.focus_pending() => self.js_tasks.push(JsTask::Replay(PageMsg::Key { key, mods })),
            PageMsg::Text(t) if self.focus_pending() => self.js_tasks.push(JsTask::Replay(PageMsg::Text(t))),
            PageMsg::Paste(t) if self.focus_pending() => self.js_tasks.push(JsTask::Replay(PageMsg::Paste(t))),
            PageMsg::Key { key, mods } => {
                if let Err(PageMsg::Key { key, mods }) = self.frame_input(PageMsg::Key { key, mods }) {
                    self.on_key(key, mods)
                }
            }
            PageMsg::KeyDown { key, mods } if self.focus_pending() => self.js_tasks.push(JsTask::Replay(PageMsg::Key { key, mods })),
            PageMsg::KeyDown { key, mods } => {
                if let Err(PageMsg::Key { key, mods }) = self.frame_input(PageMsg::Key { key, mods }) {
                    self.key_repeat = !self.held_keys.insert(held_key_id(&key_name_of(&key)));
                    self.on_key(key, mods);
                    self.key_repeat = false;
                }
            }
            PageMsg::KeyUp { key, mods } => self.on_key_up(key, mods),
            PageMsg::ReleaseKeys => {
                let held: Vec<String> = self.held_keys.iter().cloned().collect();
                for k in held {
                    self.on_key_up(if k.chars().count() == 1 { Key::Char(k) } else { key_from_name(&k) }, Mods::default());
                }
            }
            PageMsg::Paste(t) if t.is_empty() => {}
            PageMsg::Paste(t) => {
                if let Err(PageMsg::Text(t)) = self.frame_input(PageMsg::Text(t)) {
                    if self.js_enabled {
                        self.js_tasks.push(JsTask::CallGlobal("__ohb_paste", vec![crate::js::host::HostValue::from(t.as_str())]));
                    } else {
                        self.on_text(t);
                    }
                }
            }
            PageMsg::PermissionReply { id, allow, data } => {
                use crate::js::host::HostValue as H;
                self.js_tasks.push(JsTask::CallGlobal("__ohb_permReply", vec![H::Number(id as f64), H::Bool(allow), data.map(H::String).unwrap_or(H::Null)]));
            }
            PageMsg::Text(t) if t.is_empty() => {}
            PageMsg::Text(t) => {
                if let Err(PageMsg::Text(t)) = self.frame_input(PageMsg::Text(t)) {
                    self.on_text(t)
                }
            }
            PageMsg::Find(q, fwd) => self.find(&q, fwd),
            PageMsg::FindClose => self.find_close(),
            PageMsg::SelectAllText => self.select_all_text(),
            PageMsg::CopySelection => {
                let t = self.selection_text();
                if !t.is_empty() {
                    self.emit(PageEvent::Copy(t));
                }
            }
            PageMsg::Eval(code) => self.js_tasks.push(JsTask::Eval(code)),
            PageMsg::Devtools(m) => self.devtools_msg(m),
            PageMsg::Child(id, m) => self.on_child(id, m),
            PageMsg::FramesTimeout(gen) => {
                if gen == self.nav_gen && !self.frames_loading.is_empty() {
                    self.frames_loading.clear();
                    self.update_progress();
                }
            }
            PageMsg::PostMessage { data, origin, target_origin, ports, source } => self.js_tasks.push(JsTask::ParentMessage { data, origin, target_origin, ports, source }),
            PageMsg::PortMessage { pid, data, ports } => self.js_tasks.push(JsTask::PortMessage { pid, data, ports }),
            PageMsg::Ws(id, ev) => self.on_ws(id, ev),
            PageMsg::Net(gen, m) => {
                if gen != self.nav_gen {
                    if let NetMsg::Fetch(..) = m {
                    } else {
                        return true;
                    }
                }
                match m {
                    NetMsg::Document(req, r) => self.on_document(req, r),
                    NetMsg::Stylesheet(slot, url, r) => self.on_stylesheet(slot, url, r),
                    NetMsg::Script(i, r) => self.on_script(i, r),
                    NetMsg::Image(u, img) => self.on_image(u, img),
                    NetMsg::ImageDoc(u) => {
                        self.on_image(u.clone(), None);
                        self.res.images.insert(u, ImgState::Document);
                    }
                    NetMsg::Preload(n, ok) => {
                        self.pending = self.pending.saturating_sub(1);
                        if self.doc.valid(n) {
                            self.js_tasks.push(JsTask::Dispatch { target: n, ty: if ok { "load" } else { "error" }.into(), init: EventInit::default(), then: DefaultAction::None });
                        }
                        self.update_progress();
                    }
                    NetMsg::SvgDoc(u, d) => {
                        self.pending = self.pending.saturating_sub(1);
                        self.res.svg_docs.insert(u, match d { Some(d) => resources::SvgDocState::Ready(d), None => resources::SvgDocState::Failed });
                        self.need_layout = true;
                        self.update_progress();
                    }
                    NetMsg::Font(k, _w, st, b) => self.on_font(k, st, b),
                    NetMsg::Fetch(id, r) => {
                        if let Some(fs) = self.fetch_streams.get_mut(&id) {
                            fs.done = Some(r.is_ok());
                        }
                        self.fetch_results.push((id, r))
                    }
                    NetMsg::FetchHead(id, h) => {
                        use crate::js::host::HostValue as H;
                        let flat: Vec<H> = h.headers.iter().flat_map(|(k, v)| [H::from(k.to_ascii_lowercase().as_str()), H::from(v.as_str())]).collect();
                        self.js_tasks.push(JsTask::CallGlobal(
                            "__ohb_fetchHead",
                            vec![H::from(id as f64), H::from(h.status as i32), H::from(h.status_text.as_str()), H::from(h.url.as_str()), H::Array(flat), H::from(h.rtype), H::from(h.redirected)],
                        ));
                    }
                    NetMsg::FetchChunk(id, b) => {
                        if let Some(fs) = self.fetch_streams.get_mut(&id).filter(|fs| !fs.discard) {
                            fs.buf.extend_from_slice(&b);
                            if fs.want {
                                fs.want = false;
                                self.js_tasks.push(JsTask::CallGlobal("__ohb_fetchChunk", vec![crate::js::host::HostValue::from(id as f64)]));
                            }
                        }
                    }
                    NetMsg::Favicon(img) => self.emit(PageEvent::Favicon(img)),
                    NetMsg::Worker(id, ev) => self.worker_events.push((id, ev)),
                    NetMsg::Frame(n, k, r) => self.on_frame(n, k, r),
                    NetMsg::FrameSheet(n, k, u, r) => self.on_frame_sheet(n, k, u, r),
                }
            }
            PageMsg::Shutdown => return false,
        }
        true
    }

    /// Parser-run scripts wait for pending style sheets (and their imports)
    /// before them; rendering only waits a short while (css_blocking), script
    /// up to a much longer cap so a stalled sheet can't hang the page.
    pub fn scripts_ready_to_run(&self) -> bool {
        let pending = self.sheets.iter().any(|s| (s.pending && s.blocking) || s.imp_pending > 0);
        !pending || self.first_paint_deadline.map(|d| Instant::now() >= d + Duration::from_secs(10)).unwrap_or(true)
    }
}

pub fn downloads_dir() -> std::path::PathBuf {
    let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into());
    std::path::Path::new(&home).join("Downloads")
}

// ----------------------------------------------------------------- tab thread

pub struct TabHandle {
    pub tx: Sender<PageMsg>,
    pub join: Option<std::thread::JoinHandle<()>>,
}

/// Spawn a tab thread.
pub fn spawn_tab(tab_id: u64, ui: UiSink, history: Arc<Mutex<Vec<(String, String)>>>, initial: NavRequest, win: (f32, f32), dpr: f32, js_enabled: bool) -> TabHandle {
    let (tx, rx) = std::sync::mpsc::channel::<PageMsg>();
    let tx2 = tx.clone();
    devtools::register(tab_id, tx.clone());
    let join = std::thread::Builder::new()
        .name(format!("heron-tab-{tab_id}"))
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let ui2 = ui.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tab_main_with(tab_id, ui.clone(), tx2.clone(), rx, history, initial, win, dpr, js_enabled, None);
            }));
            if let Err(e) = result {
                let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "unknown error".into());
                ui2(tab_id, PageEvent::Crashed(msg));
            }
            PAGE.with(|p| {
                if let Ok(mut g) = p.try_borrow_mut() {
                    *g = None;
                }
            });
            devtools::unregister(tab_id);
        })
        .expect("spawn tab thread");
    TabHandle { tx, join: Some(join) }
}

#[allow(clippy::too_many_arguments)]
fn tab_main_with(tab_id: u64, ui: UiSink, tx: Sender<PageMsg>, rx: Receiver<PageMsg>, history: Arc<Mutex<Vec<(String, String)>>>, initial: NavRequest, win: (f32, f32), dpr: f32, js_enabled: bool, frame_parent: Option<frames::FrameParent>) {
    // A tab's memory is its own thread's heap plus its frames' (see tab_memory).
    if frame_parent.is_none() {
        ROOT_TAB.with(|r| r.set(tab_id));
    }
    let _mem_guard = ThreadMemGuard(tab_id);
    let mut page = Page::new(tab_id, ui, tx, history, dpr);
    page.win = win;
    page.js_enabled = js_enabled;
    page.frame_parent = frame_parent;
    frames::set_thread_owner(tab_id);
    PAGE.with(|p| *p.borrow_mut() = Some(page));
    let mut js: Option<crate::js::JsRuntime> = None;
    with_page(|p| p.navigate(initial));
    let mut seen_gen = with_page(|p| p.nav_gen).unwrap_or(0);
    let mut last_blink = Instant::now();
    // Input held back while the page has queued script work (see below).
    let mut held_input: std::collections::VecDeque<PageMsg> = std::collections::VecDeque::new();
    let mut held_since = Instant::now();
    // Last user input, for collecting garbage while the user pauses.
    let mut last_input_at = Instant::now();
    let mut last_idle_gc = Instant::now();
    let mut last_gc_run = Instant::now();
    let mut last_debug = Instant::now();
    let mut last_render = Instant::now();
    let mut render_gap = Duration::from_millis(250);
    let mut perf_acc = PerfSample::default();
    let mut perf_since = Instant::now();
    let mut perf_ph = phase_ns();
    let mut perf_gc = boa_engine::gc::gc_stats().1;
    let mut perf_wasm = crate::js::wasm::wasm_time_ns();
    let mut was_busy = false;
    if with_page(|p| std::mem::take(&mut p.prewarm_js)).unwrap_or(false) {
        crate::js::JsRuntime::prewarm();
    }
    loop {
        // Run JS tasks, timers and render.
        let anim_wait = with_page(|p| p.anim_tick()).flatten();
        let scroll_pending = with_page(|p| p.scroll_events_pending()).unwrap_or(false);
        let anim_wait = if scroll_pending { Some(anim_wait.unwrap_or(Duration::from_millis(16)).min(Duration::from_millis(16))) } else { anim_wait };
        static LOOP_TRACE: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("OHB_LOOP_TRACE").is_some());
        let t_pump = Instant::now();
        let ph0 = phase_ns();
        let busy = crate::js::pump(&mut js);
        let d_pump = t_pump.elapsed();
        let ph1 = phase_ns();
        perf_acc.js += d_pump.as_secs_f64() * 1e3 - ((ph1[0] - ph0[0]) + (ph1[1] - ph0[1])) as f64 / 1e6;
        if busy != was_busy {
            was_busy = busy;
            with_page(|p| p.emit(PageEvent::Busy(busy)));
        }
        // While script work is queued, render a few times a second (it
        // shows progress without starving the script of time): at most
        // every 250ms, and so that rendering takes under a fifth of the time.
        let t_render = Instant::now();
        if !busy || last_render.elapsed() > render_gap {
            let t = Instant::now();
            let ph0 = phase_ns();
            if with_page(|p| p.render()).unwrap_or(false) {
                last_render = Instant::now();
                render_gap = (t.elapsed() * 4).max(Duration::from_millis(250));
                perf_acc.frames += 1;
            }
            let ph1 = phase_ns();
            perf_acc.paint += t.elapsed().as_secs_f64() * 1e3 - ((ph1[0] - ph0[0]) + (ph1[1] - ph0[1])) as f64 / 1e6;
        }
        // Performance overlay: a sample every half second.
        if perf_since.elapsed() >= Duration::from_millis(500) {
            let ph = phase_ns();
            let (_, gc_ns, gc_bytes, _) = boa_engine::gc::gc_stats();
            static MEM_TRACE: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("OHB_MEM_TRACE").is_some());
            static MEM_DEBUG: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("OHB_MEM_DEBUG").is_some());
            if *MEM_DEBUG && !*MEM_TRACE {
                report_thread_memory(tab_id);
            }
            if *MEM_TRACE {
                let (imgs, nodes, frames) = with_page(|p| (p.res.image_bytes(), p.doc.nodes.len(), p.frame_views.len())).unwrap_or((0, 0, 0));
                report_thread_memory(tab_id);
                eprintln!("[mem] tab#{tab_id} total={}MB", tab_memory(tab_id) >> 20);
                eprintln!("[mem] @{:.0} heap={}MB rss={}MB js={}MB wasm={}MB img={}MB nodes={} frames={}", crate::js::host::page_now_ms(), crate::heap_in_use() >> 20, crate::sysmem::rss_bytes().unwrap_or(0) >> 20, gc_bytes >> 20, crate::js::wasm::wasm_memory_bytes() >> 20, imgs >> 20, nodes, frames);
            }
            if HUD.load(std::sync::atomic::Ordering::Relaxed) {
                report_thread_memory(tab_id);
                let mut s = std::mem::take(&mut perf_acc);
                s.tab_mem = tab_memory(tab_id);
                s.wall = perf_since.elapsed().as_secs_f64() * 1e3;
                s.style = (ph[0] - perf_ph[0]) as f64 / 1e6;
                s.layout = (ph[1] - perf_ph[1]) as f64 / 1e6;
                s.gc = (gc_ns - perf_gc) as f64 / 1e6;
                s.js = (s.js - s.gc).max(0.0);
                s.js_heap = gc_bytes;
                let wn = crate::js::wasm::wasm_time_ns();
                s.wasm = (wn.saturating_sub(perf_wasm) as f64 / 1e6).min(s.js);
                perf_wasm = wn;
                s.wasm_mem = crate::js::wasm::wasm_memory_bytes();
                with_page(|p| {
                    s.images = p.res.image_bytes();
                    s.dom_nodes = p.doc.nodes.len();
                    s.boxes = p.layout.as_ref().map(|l| l.boxes.len()).unwrap_or(0);
                    p.emit(PageEvent::Perf(s));
                });
            } else {
                perf_acc = PerfSample::default();
            }
            perf_since = Instant::now();
            perf_ph = ph;
            perf_gc = gc_ns;
            perf_wasm = crate::js::wasm::wasm_time_ns();
        }
        if *LOOP_TRACE && (d_pump > Duration::from_millis(2) || t_render.elapsed() > Duration::from_millis(2)) {
            eprintln!("[loop] @{:.0} pump {:.1}ms render {:.1}ms busy={busy}", crate::js::host::page_now_ms(), d_pump.as_secs_f64() * 1e3, t_render.elapsed().as_secs_f64() * 1e3);
            if boa_engine::gc::alloc_stats_on() && d_pump > Duration::from_millis(50) {
                let v = boa_engine::gc::alloc_counts_take();
                let total: u64 = v.iter().map(|e| e.1).sum();
                let top: Vec<String> = v.iter().take(12).map(|(n, c)| format!("{c} {}", n.replace("boa_engine::", "").replace("object::jsobject::VTableObject", "Obj").replace("builtins::", ""))).collect();
                eprintln!("[alloc-pump] total {total}: {}", top.join(" | "));
                eprintln!("[alloc-sites] {}", boa_engine::vm::prof_report());
            }
        }
        // Wait for the next message or deadline.
        let next_timer = js.as_ref().and_then(|j| j.next_deadline());
        let mut timeout = Duration::from_millis(500);
        if let Some(d) = next_timer {
            timeout = timeout.min(d);
        }
        if busy {
            timeout = Duration::ZERO;
        }
        if let Some(d) = anim_wait.or_else(|| with_page(|p| p.anim_due.map(|_| Duration::from_millis(16))).flatten()) {
            timeout = timeout.min(d);
        }
        let (loading, editing, blocking) = with_page(|p| (p.loading, p.doc.focused.map(|f| p.is_editable(f)).unwrap_or(false), p.css_blocking())).unwrap_or((false, false, false));
        if blocking {
            timeout = timeout.min(Duration::from_millis(50));
        }
        if loading {
            timeout = timeout.min(Duration::from_millis(100));
        }
        if editing {
            let since = last_blink.elapsed();
            let blink = Duration::from_millis(530);
            if since >= blink {
                last_blink = Instant::now();
                with_page(|p| {
                    p.caret_on = !p.caret_on;
                    p.need_paint = true;
                });
                timeout = timeout.min(blink);
            } else {
                timeout = timeout.min(blink - since);
            }
        }
        if std::env::var_os("OHB_LOAD_DEBUG").is_some() && last_debug.elapsed() > Duration::from_secs(3) {
            last_debug = Instant::now();
            with_page(|p| {
                let undone: Vec<String> = p.scripts.iter().filter(|s| !s.done).map(|s| format!("{}{}", if s.text.is_some() { "ready:" } else { "" }, s.url.as_ref().map(|u| u.as_str().chars().take(70).collect::<String>()).unwrap_or_else(|| "(inline)".into()))).collect();
                eprintln!("[load] t={:.1}s loading={} pending={} total={} tasks={} scripts undone={} {:?}", crate::js::host::page_now_ms() / 1000.0, p.loading, p.pending, p.total_requests, p.js_tasks.len(), undone.len(), &undone[..undone.len().min(4)]);
            });
        }
        // User input waits while the page has script work queued (a render
        // React scheduled after the last keystroke, a zero-delay timer): a
        // fast browser finishes that work before the next key arrives, and
        // pages that sync editor contents from their own state depend on it.
        // One held input is let through each time the queue is idle.
        let page_idle = !busy && with_page(|p| p.js_tasks.is_empty()).unwrap_or(true) && !js.as_ref().and_then(|j| j.next_deadline()).map(|d| d.is_zero()).unwrap_or(false);
        // Collect garbage in a pause in the input (and with no script work
        // queued) rather than when the heap fills up mid-keystroke.
        // A tab left alone for a while gives memory back more eagerly.
        if page_idle && held_input.is_empty() && last_input_at.elapsed() > Duration::from_millis(350) && last_idle_gc.elapsed() > Duration::from_secs(2) {
            // (A quiet tab: no input for 15s and no collection for 15s;
            // `last_idle_gc` is only the last check.)
            let quiet = last_input_at.elapsed() > Duration::from_secs(15) && last_gc_run.elapsed() > Duration::from_secs(15);
            last_idle_gc = Instant::now();
            let t = Instant::now();
            if boa_engine::gc::collect_if_over(if quiet { 20 } else { 55 }) {
                last_gc_run = Instant::now();
                crate::release_free_memory();
                if *LOOP_TRACE {
                    eprintln!("[loop] idle gc {:.1}ms", t.elapsed().as_secs_f64() * 1e3);
                }
            }
        }
        let mut alive = true;
        // A page that is never idle (games: a timer or animation frame is
        // always due) still gets its input promptly: held input is let
        // through after a short wait, all of it at once.
        if !held_input.is_empty() && !page_idle && held_since.elapsed() > Duration::from_millis(40) {
            while let Some(m) = held_input.pop_front() {
                alive = with_page(|p| p.handle(m)).unwrap_or(false);
                if !alive {
                    break;
                }
            }
            held_since = Instant::now();
            timeout = Duration::ZERO;
        } else if !held_input.is_empty() && page_idle {
            if let Some(m) = held_input.pop_front() {
                alive = with_page(|p| p.handle(m)).unwrap_or(false);
                held_since = Instant::now();
            }
            // More held input (or this one's work) is waiting: don't block.
            timeout = Duration::ZERO;
        } else if !held_input.is_empty() {
            timeout = Duration::ZERO;
        }
        let msg = if alive {
            match rx.recv_timeout(timeout) {
                Ok(m) => Some(m),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => None,
                Err(_) => break,
            }
        } else {
            None
        };
        let is_input = |m: &PageMsg| matches!(m, PageMsg::MouseMove { .. } | PageMsg::MouseDown { .. } | PageMsg::MouseUp { .. } | PageMsg::Wheel { .. } | PageMsg::Key { .. } | PageMsg::KeyDown { .. } | PageMsg::KeyUp { .. } | PageMsg::Text(_) | PageMsg::Paste(_));
        if let Some(m) = msg {
            if is_input(&m) && !matches!(m, PageMsg::MouseMove { .. }) {
                last_input_at = Instant::now();
            }
            let mut take = |m: PageMsg, held_input: &mut std::collections::VecDeque<PageMsg>| -> bool {
                // Hold input behind queued script work (and behind held input).
                // Only while a text field or editor has focus: elsewhere (games,
                // canvases) input goes straight in, without the wait.
                if is_input(&m) && (!held_input.is_empty() || (editing && with_page(|p| !p.js_tasks.is_empty()).unwrap_or(false))) {
                    if held_input.is_empty() {
                        held_since = Instant::now();
                    }
                    // Consecutive moves: only the latest position matters.
                    if matches!(m, PageMsg::MouseMove { .. }) && matches!(held_input.back(), Some(PageMsg::MouseMove { .. })) {
                        held_input.pop_back();
                    }
                    held_input.push_back(m);
                    return true;
                }
                with_page(|p| p.handle(m)).unwrap_or(false)
            };
            alive = take(m, &mut held_input);
            // Coalesce bursts (mouse moves, network completions).
            let mut n = 0;
            while alive && n < 256 {
                match rx.try_recv() {
                    Ok(m) => alive = take(m, &mut held_input),
                    Err(_) => break,
                }
                n += 1;
            }
        }
        if !alive {
            break;
        }
        with_page(|p| p.frame_apply_scroll());
        // Navigation requested by the page or script.
        let nav = with_page(|p| p.pending_nav.take()).flatten();
        if let Some(n) = nav {
            // A new document gets a fresh JS realm; a same-document
            // (fragment) navigation keeps the running one.
            let before = with_page(|p| p.nav_gen).unwrap_or(0);
            with_page(|p| p.navigate(n));
            if with_page(|p| p.nav_gen).unwrap_or(0) != before && js.is_some() {
                // The old document's whole JS heap is garbage now: collect it
                // and give the memory back instead of waiting for the next
                // page to fill the heap.
                js = None;
                let b0 = boa_engine::gc::gc_stats().2;
                boa_engine::gc::force_collect();
                crate::release_free_memory();
                if std::env::var_os("OHB_MEM_TRACE").is_some() {
                    eprintln!("[mem] navigation: js heap {}MB -> {}MB, rss {}MB", b0 >> 20, boa_engine::gc::gc_stats().2 >> 20, crate::sysmem::rss_bytes().unwrap_or(0) >> 20);
                }
            }
        }
        // Any navigation to a new document (address bar, history, reload,
        // script): the old document's JS runtime and heap must go now. A
        // page without scripts never replaces the runtime, so the previous
        // site's whole heap stayed alive; and a replaced runtime's objects
        // stayed until the next collection, which an idle page never runs.
        let gen_now = with_page(|p| p.nav_gen).unwrap_or(0);
        if gen_now != seen_gen {
            seen_gen = gen_now;
            let stale = js.as_ref().map(|j| j.doc_gen() != gen_now).unwrap_or(false);
            if stale {
                js = None;
            }
            let b0 = boa_engine::gc::gc_stats().2;
            boa_engine::gc::force_collect();
            crate::release_free_memory();
            if std::env::var_os("OHB_MEM_TRACE").is_some() {
                eprintln!("[mem] document change (dropped runtime: {stale}): js heap {}MB -> {}MB, rss {}MB", b0 >> 20, boa_engine::gc::gc_stats().2 >> 20, crate::sysmem::rss_bytes().unwrap_or(0) >> 20);
                let roots = boa_engine::gc::rooted_report();
                let top: Vec<String> = roots.iter().take(15).map(|(n, c)| format!("{c}x{}", n.replace("boa_engine::", ""))).collect();
                eprintln!("[mem] GC roots: {}", top.join(" | "));
                for f in boa_engine::debug_rooted_functions().iter().take(10) {
                    eprintln!("[mem]   rooted function {f}");
                }
            }
        }
        // Set up the next document's script runtime while its request is in flight.
        if with_page(|p| std::mem::take(&mut p.prewarm_js)).unwrap_or(false) {
            crate::js::JsRuntime::prewarm();
        }
        // New document installed → reset JS runtime.
        let fresh = with_page(|p| p.scripts.iter().all(|s| !s.done) && p.js_tasks.iter().any(|t| matches!(t, JsTask::RunScripts)) && !p.dcl_fired).unwrap_or(false);
        if fresh {
            if let Some(j) = &js {
                if j.doc_gen() != with_page(|p| p.nav_gen).unwrap_or(0) {
                    js = None;
                    let b0 = boa_engine::gc::gc_stats().2;
                    boa_engine::gc::force_collect();
                    crate::release_free_memory();
                    if std::env::var_os("OHB_MEM_TRACE").is_some() {
                        eprintln!("[mem] new document: js heap {}MB -> {}MB, rss {}MB", b0 >> 20, boa_engine::gc::gc_stats().2 >> 20, crate::sysmem::rss_bytes().unwrap_or(0) >> 20);
                    }
                }
            }
        }
    }
}

/// What a style pass depended on besides the DOM: an incremental restyle is
/// only valid while all of it is unchanged.
#[derive(PartialEq)]
struct StyleSnap {
    index: usize,
    shadow: Vec<(NodeId, usize)>,
    env: (f32, f32, f32, bool),
    hover: Vec<NodeId>,
    active: Vec<NodeId>,
    focus: Option<NodeId>,
    target: Option<NodeId>,
    containers: Vec<(NodeId, (f32, f32))>,
    quirks: bool,
}

impl StyleSnap {
    fn eq_except_shadow(&self, o: &StyleSnap) -> bool {
        self.index == o.index && self.env == o.env && self.hover == o.hover && self.active == o.active && self.focus == o.focus && self.target == o.target && self.containers == o.containers && self.quirks == o.quirks
    }
}


/// Attributes that render only through computed styles (and are not read
/// by layout or painting directly).
fn layout_inert_attr(name: &str) -> bool {
    matches!(name, "class" | "id" | "style" | "role" | "title" | "tabindex" | "nonce" | "itemprop" | "itemscope" | "itemtype" | "itemid" | "jsaction" | "jscontroller" | "jsname" | "jsmodel" | "jslog" | "spellcheck" | "translate")
        || name.starts_with("data-")
        || name.starts_with("aria-")
}

/// Attributes that only affect styles through attribute selectors (no
/// pseudo-class, presentational hint or UA behavior depends on them).
fn style_inert_attr(name: &str) -> bool {
    name.starts_with("data-")
        || name.starts_with("aria-")
        || matches!(
            name,
            "role" | "title" | "alt" | "tabindex" | "itemprop" | "itemscope" | "itemtype" | "itemid" | "target" | "download" | "src" | "srcset" | "sizes" | "loading" | "decoding" | "fetchpriority" | "crossorigin" | "referrerpolicy" | "integrity" | "nonce" | "async" | "defer" | "datetime" | "for" | "autocomplete" | "inputmode" | "enterkeyhint" | "spellcheck" | "translate" | "jsaction" | "jscontroller" | "jsname" | "jsmodel" | "jslog" | "ping" | "rel"
        )
}

fn verify_incremental_style() -> bool {
    thread_local! {
        static ON: bool = std::env::var_os("OHB_VERIFY_STYLE").is_some();
    }
    ON.with(|v| *v)
}

/// `OHB_NO_INCREMENTAL_STYLE=1` makes every restyle a full one (debugging).
fn no_incremental_style() -> bool {
    thread_local! {
        static OFF: bool = std::env::var_os("OHB_NO_INCREMENTAL_STYLE").is_some();
    }
    OFF.with(|v| *v)
}

/// Identity of a declared @font-face (family, weights, style, first range, first source).
/// CSS Fonts §5.2 weight matching as a rank (lower is better) of a face's
/// weight range for a desired weight: within range first; for 400–500 the
/// weights up to 500, then lighter, then heavier; below 400 lighter first;
/// above 500 heavier first. Style mismatches rank after any weight.
/// The face matching (weight, italic) best. Faces the rank can't tell apart
/// keep declaration order, except that a face redeclared with identical
/// descriptors replaces the earlier one (CSS Fonts §4.5).
pub fn best_font_face<'a>(faces: impl Iterator<Item = &'a css::FontFace> + Clone, w: u16, italic: bool) -> Option<&'a css::FontFace> {
    let first = faces.clone().min_by_key(|f| font_face_rank(f, w, italic))?;
    faces
        .filter(|f| f.weight == first.weight && f.style == first.style && f.variant == first.variant && f.ranges == first.ranges)
        .last()
        .or(Some(first))
}

pub fn font_face_rank(f: &css::FontFace, w: u16, italic: bool) -> i32 {
    let (lo, hi) = (f.weight.0 as i32, f.weight.1 as i32);
    let w = w as i32;
    let wr = if w >= lo && w <= hi {
        0
    } else if (400..=500).contains(&w) {
        if lo > w && lo <= 500 { lo - w } else if hi < w { 1000 + (w - hi) } else { 2000 + (lo - w) }
    } else if w < 400 {
        if hi < w { w - hi } else { 1000 + (lo - w) }
    } else if lo > w {
        lo - w
    } else {
        1000 + (w - hi)
    };
    wr + if (f.style != css::style::FontStyle::Normal) == italic { 0 } else { 10000 }
}

pub fn font_face_key(ff: &css::FontFace) -> String {
    format!("{}|{}-{}|{:?}|{:?}|{}", ff.family, ff.weight.0, ff.weight.1, ff.style, ff.ranges.first(), ff.sources.first().map(|s| s.0.as_str()).unwrap_or(""))
}

/// Debugging (OHB_STYLE_STATS=5): who asked for a full restyle.
pub fn full_why(file: &str, line: u32) {
    static ON: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var("OHB_STYLE_STATS").as_deref() == Ok("5"));
    if *ON {
        eprintln!("[full-why] {file}:{line} native={}", crate::js::CURRENT_NATIVE.with(|c| c.get()));
    }
}


/// A key's `key` value.
pub fn key_name_of(key: &Key) -> String {
    match &key {
        Key::Char(c) => c.clone(),
        Key::Backspace => "Backspace".into(),
        Key::Delete => "Delete".into(),
        Key::Left => "ArrowLeft".into(),
        Key::Right => "ArrowRight".into(),
        Key::Up => "ArrowUp".into(),
        Key::Down => "ArrowDown".into(),
        Key::Home => "Home".into(),
        Key::End => "End".into(),
        Key::Enter => "Enter".into(),
        Key::Tab => "Tab".into(),
        Key::Escape => "Escape".into(),
        Key::PageUp => "PageUp".into(),
        Key::PageDown => "PageDown".into(),
        Key::Space => " ".into(),
        Key::Other(s) => s.clone(),
    }
}

/// How a held key is remembered (a letter's case can change while held).
pub fn held_key_id(name: &str) -> String {
    if name.chars().count() == 1 { name.to_lowercase() } else { name.to_string() }
}


/// MouseEvent.buttons bit for a `button` number.
pub fn button_bit(button: u8) -> u8 {
    match button {
        0 => 1,
        1 => 4,
        2 => 2,
        b if b < 8 => 1 << b,
        _ => 0,
    }
}

/// The Key for a `key` value (the inverse of key_name_of).
pub fn key_from_name(name: &str) -> Key {
    match name {
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "ArrowLeft" => Key::Left,
        "ArrowRight" => Key::Right,
        "ArrowUp" => Key::Up,
        "ArrowDown" => Key::Down,
        "Home" => Key::Home,
        "End" => Key::End,
        "Enter" => Key::Enter,
        "Tab" => Key::Tab,
        "Escape" => Key::Escape,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        " " => Key::Space,
        other => Key::Other(other.to_string()),
    }
}
