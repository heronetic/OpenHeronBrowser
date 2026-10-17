// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronShell — the browser window: tab strip, toolbar, omnibox, find bar,
//! menus, scrolling and compositing of page frames.

pub mod chrome;

use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{DeviceEvent, DeviceId, ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WKey, ModifiersState, NamedKey};
use winit::window::{CursorGrabMode, CursorIcon, Window, WindowId};

use crate::css::style::Cursor;
use crate::css::values::Color;
use crate::page::{self, FrameData, Key, Mods, NavRequest, PageEvent, PageMsg};
use crate::paint::raster::Rasterizer;
use crate::paint::{ImageData, Item, Rect};
use crate::text::FontContext;
use chrome::*;

pub enum UserEvent {
    Page(u64, PageEvent),
}

struct TabState {
    id: u64,
    tx: Sender<PageMsg>,
    join: Option<std::thread::JoinHandle<()>>,
    title: String,
    url: String,
    secure: bool,
    loading: bool,
    progress: f32,
    favicon: Option<Arc<ImageData>>,
    frame: Option<Arc<FrameData>>,
    scroll: (f32, f32),
    target_y: f32,
    can_back: bool,
    can_fwd: bool,
    cursor: Cursor,
    status: String,
    crashed: Option<String>,
    find_result: (usize, usize),
    zoom: f32,
    /// The docked DevTools pane inspecting this tab (a page of its own).
    devtools: Option<Box<TabState>>,
    /// Requested height of the DevTools pane (CSS px).
    dev_h: f32,
    /// The latest performance-overlay sample of this tab.
    perf: Option<page::PerfSample>,
    /// When the tab was last in front.
    last_active: Instant,
    /// Put to sleep to save memory: its page was shut down and is reloaded
    /// from `url` when the tab is selected again.
    asleep: bool,
}

/// Height of the bar between a page and its DevTools pane.
const SPLIT_H: f32 = 5.0;

fn dev_height(want: f32, total: f32) -> f32 {
    want.clamp(110.0, (total - 80.0).max(110.0))
}

impl TabState {
    fn send(&self, m: PageMsg) {
        let _ = self.tx.send(m);
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hot {
    None,
    Tab(usize),
    TabClose(usize),
    NewTab,
    Back,
    Forward,
    Reload,
    Home,
    Omnibox,
    Menu,
    MenuItem(usize),
    Suggestion(usize),
    FindPrev,
    FindNext,
    FindClose,
    FindField,
    Content,
    AlertOk,
    /// The permission / download prompt's buttons.
    PromptYes,
    PromptNo,
    CtxItem(usize),
    Scrollbar,
    Shield,
    /// Inside the DevTools pane.
    Dev,
    /// The bar between the page and DevTools (drag to resize).
    DevSplit,
    /// The startup prompt's checkboxes and buttons.
    GateAgree,
    GateSave,
    GateContinue,
    GateQuit,
    /// Anywhere else while the startup prompt is up.
    Gate,
}

/// A question for the user from a tab: a site permission or a download.
#[derive(Clone)]
enum PromptKind {
    Permission { id: u64, perm: String, site: String },
    Download { id: u64, name: String, size: usize, site: String },
}

#[derive(Clone)]
struct Prompt {
    tab_id: u64,
    kind: PromptKind,
}

/// Context menus and the HeronBlock popup.
const CTX_W: f32 = 270.0;
/// A non-clickable heading row in a context menu.
const CTX_INFO: u8 = 255;

struct Layout {
    tabs: Vec<(Rect, Rect)>,
    new_tab: Rect,
    back: Rect,
    forward: Rect,
    reload: Rect,
    home: Rect,
    omnibox: Rect,
    menu: Rect,
    shield: Rect,
}

const MENU_ITEMS: &[&str] = &["New tab", "Reopen closed tab", "History", "Extensions", "Developer tools", "Performance overlay", "Render resolution", "Zoom in", "Zoom out", "Actual size", "Find in page…", "Toggle JavaScript", "Clear cookies & cache", "Settings", "Site data & storage", "About OpenHeronBrowser", "Heronetic.com", "Restart browser", "Quit"];

struct App {
    proxy: EventLoopProxy<UserEvent>,
    window: Option<Rc<Window>>,
    context: Option<softbuffer::Context<Rc<Window>>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    tabs: Vec<TabState>,
    active: usize,
    next_id: u64,
    fonts: FontContext,
    family: Rc<Vec<String>>,
    raster: Rasterizer,
    dpr: f32,
    size: (u32, u32),
    mouse: (f32, f32),
    mouse_in: bool,
    mods: ModifiersState,
    omni: TextField,
    omni_focus: bool,
    omni_edited: bool,
    find: Option<TextField>,
    find_focus: bool,
    menu_open: bool,
    hot: Hot,
    suggestions: Vec<(String, String)>,
    sugg_idx: Option<usize>,
    history: Arc<Mutex<Vec<(String, String)>>>,
    closed: Vec<String>,
    alert: Option<String>,
    /// Pending permission and download questions (the active tab's first one shows).
    prompts: Vec<Prompt>,
    /// A short message in the status bubble (e.g. "Saved to …") and when it ends.
    notice: Option<(String, Instant)>,
    ctx_menu: Option<(f32, f32, Vec<(String, u8)>, Option<String>)>,
    js_enabled: bool,
    page_cache: Option<PageCache>,
    caret_on: bool,
    last_blink: Instant,
    last_click: (Instant, f32, f32),
    initial: Vec<String>,
    /// Set by "Restart browser": the new process saves the profile from now on.
    restarting: bool,
    clipboard: Option<arboard::Clipboard>,
    pressed_in_content: bool,
    /// Scrollbar drag: (mouse y at press, scroll y at press).
    sb_drag: Option<(f32, f32)>,
    frame_pm: Option<tiny_skia::Pixmap>,
    /// Keyboard goes to the DevTools pane.
    dev_focus: bool,
    pressed_in_dev: bool,
    /// Resizing DevTools: (mouse y at press, pane height at press).
    dev_drag: Option<(f32, f32)>,
    dev_cache: Option<(*const FrameData, tiny_skia::Pixmap, (f32, f32))>,
    /// Page point of the last content context menu (for "Inspect").
    ctx_point: (f32, f32),
    /// Performance overlay (top left of the page).
    hud: bool,
    hud_hover: bool,
    /// The theme the window's title bar was last given.
    title_dark: Option<bool>,
    /// The active tab holds the pointer lock (cursor hidden and captured).
    pointer_locked: bool,
    /// Page rendering resolution relative to the screen (1, ½ or ¼).
    render_scale: f32,
    /// Time spent rasterizing the page since the last overlay update.
    perf_raster: std::time::Duration,
    /// Window frames presented, for the frame rate.
    perf_frames: std::collections::VecDeque<Instant>,
    perf_mark: Instant,
    /// (fps, raster % of the UI thread, process memory) shown by the overlay.
    perf_ui: (f32, f32, Option<u64>),
    mem_check: Instant,
    /// The startup prompt ("This browser is still hatching"): shown at every
    /// start and only closed by answering it.
    gate: Option<Gate>,
    /// Where the prompt's controls were last drawn (for hit testing).
    gate_layout: Option<GateLayout>,
    /// The prompt's heron mark, rendered for the current scale.
    gate_icon: Option<(u32, Arc<ImageData>)>,
}

struct Gate {
    agree: bool,
    save: bool,
    since: Instant,
}

impl Gate {
    /// The prompt fades in once the window has had a moment to load.
    fn appear(&self) -> f32 {
        let t = (self.since.elapsed().as_secs_f32() - 0.6) / 0.28;
        let t = t.clamp(0.0, 1.0);
        1.0 - (1.0 - t).powi(3)
    }
}

#[derive(Clone, Copy)]
struct GateLayout {
    agree: Rect,
    save: Rect,
    cont: Rect,
    quit: Rect,
}

struct PageCache {
    pm: tiny_skia::Pixmap,
    frame: *const FrameData,
    /// The frame the pixels were drawn from (diffed against the next one).
    keep: Arc<FrameData>,
    scroll: (f32, f32),
    scale: f32,
    /// Rendered without the fixed layer (drawn over it each frame).
    split: bool,
}

fn cursor_icon(c: Cursor) -> CursorIcon {
    match c {
        Cursor::Default => CursorIcon::Default,
        Cursor::ContextMenu => CursorIcon::ContextMenu,
        Cursor::Help => CursorIcon::Help,
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::Progress => CursorIcon::Progress,
        Cursor::Wait => CursorIcon::Wait,
        Cursor::Cell => CursorIcon::Cell,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Text => CursorIcon::Text,
        Cursor::VerticalText => CursorIcon::VerticalText,
        Cursor::Alias => CursorIcon::Alias,
        Cursor::Copy => CursorIcon::Copy,
        Cursor::Move => CursorIcon::Move,
        Cursor::NoDrop => CursorIcon::NoDrop,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::EResize => CursorIcon::EResize,
        Cursor::NResize => CursorIcon::NResize,
        Cursor::NeResize => CursorIcon::NeResize,
        Cursor::NwResize => CursorIcon::NwResize,
        Cursor::SResize => CursorIcon::SResize,
        Cursor::SeResize => CursorIcon::SeResize,
        Cursor::SwResize => CursorIcon::SwResize,
        Cursor::WResize => CursorIcon::WResize,
        Cursor::EwResize => CursorIcon::EwResize,
        Cursor::NsResize => CursorIcon::NsResize,
        Cursor::NeswResize => CursorIcon::NeswResize,
        Cursor::NwseResize => CursorIcon::NwseResize,
        Cursor::ColResize => CursorIcon::ColResize,
        Cursor::RowResize => CursorIcon::RowResize,
        Cursor::AllScroll => CursorIcon::AllScroll,
        Cursor::ZoomIn => CursorIcon::ZoomIn,
        Cursor::ZoomOut => CursorIcon::ZoomOut,
        _ => CursorIcon::Default,
    }
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>, initial: Vec<String>, js: bool) -> App {
        App {
            proxy,
            window: None,
            context: None,
            surface: None,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            fonts: FontContext::new(),
            family: Rc::new(vec!["system-ui".into(), "sans-serif".into()]),
            raster: Rasterizer::new(),
            dpr: 1.0,
            size: (1280, 860),
            mouse: (0.0, 0.0),
            mouse_in: false,
            mods: ModifiersState::empty(),
            omni: TextField::default(),
            omni_focus: false,
            omni_edited: false,
            find: None,
            find_focus: false,
            menu_open: false,
            hot: Hot::None,
            suggestions: Vec::new(),
            sugg_idx: None,
            history: Arc::new(Mutex::new(Vec::new())),
            closed: Vec::new(),
            alert: None,
            gate: crate::persist::pending().then(|| Gate { agree: false, save: false, since: Instant::now() }),
            gate_layout: None,
            gate_icon: None,
            prompts: Vec::new(),
            notice: None,
            ctx_menu: None,
            js_enabled: js,
            page_cache: None,
            caret_on: true,
            last_blink: Instant::now(),
            last_click: (Instant::now() - Duration::from_secs(5), 0.0, 0.0),
            initial,
            restarting: false,
            clipboard: arboard::Clipboard::new().ok(),
            pressed_in_content: false,
            sb_drag: None,
            frame_pm: None,
            hud: {
                let on = crate::prefs::get("hud").is_some();
                page::HUD.store(on, std::sync::atomic::Ordering::Relaxed);
                on
            },
            hud_hover: false,
            title_dark: None,
            pointer_locked: false,
            render_scale: 1.0,
            perf_raster: std::time::Duration::ZERO,
            perf_frames: std::collections::VecDeque::new(),
            perf_mark: Instant::now(),
            perf_ui: (0.0, 0.0, None),
            mem_check: Instant::now(),
            dev_focus: false,
            pressed_in_dev: false,
            dev_drag: None,
            dev_cache: None,
            ctx_point: (0.0, 0.0),
        }
    }

    fn css_size(&self) -> (f32, f32) {
        (self.size.0 as f32 / self.dpr, self.size.1 as f32 / self.dpr)
    }

    fn content_rect(&self) -> Rect {
        self.content_rect_for(self.tab())
    }

    /// The page area of a tab (smaller while its DevTools pane is open).
    fn content_rect_for(&self, t: Option<&TabState>) -> Rect {
        let (w, h) = self.css_size();
        let total = (h - CHROME_H).max(1.0);
        match t.filter(|t| t.devtools.is_some()) {
            Some(t) => Rect::new(0.0, CHROME_H, w, (total - dev_height(t.dev_h, total) - SPLIT_H).max(1.0)),
            None => Rect::new(0.0, CHROME_H, w, total),
        }
    }

    /// The active tab's DevTools pane.
    fn dev_rect(&self) -> Option<Rect> {
        self.tab()?.devtools.as_ref()?;
        let c = self.content_rect();
        let (w, h) = self.css_size();
        Some(Rect::new(0.0, c.bottom() + SPLIT_H, w, (h - c.bottom() - SPLIT_H).max(1.0)))
    }

    fn split_rect(&self) -> Option<Rect> {
        let d = self.dev_rect()?;
        Some(Rect::new(0.0, d.y - SPLIT_H, d.w, SPLIT_H))
    }

    fn to_dev(&self, x: f32, y: f32) -> (f32, f32) {
        let d = self.dev_rect().unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
        (x - d.x, y - d.y)
    }

    fn dev_tab(&self) -> Option<&TabState> {
        self.tab()?.devtools.as_deref()
    }

    /// Open (or, with an empty `frag` when open, close) DevTools for the
    /// active tab. `frag`: "console", "pick", "at=X,Y".
    fn toggle_devtools(&mut self, frag: &str) {
        let Some(i) = (if self.tabs.is_empty() { None } else { Some(self.active) }) else { return };
        if self.tabs[i].devtools.is_some() {
            self.close_devtools(i);
            if frag.is_empty() {
                return;
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        let proxy = Mutex::new(self.proxy.clone());
        let sink: page::UiSink = Arc::new(move |tab, ev| {
            let _ = proxy.lock().unwrap().send_event(UserEvent::Page(tab, ev));
        });
        let target = self.tabs[i].id;
        let (w, h) = self.css_size();
        let total = (h - CHROME_H).max(1.0);
        if self.tabs[i].dev_h <= 0.0 {
            self.tabs[i].dev_h = (total * 0.42).round();
        }
        let dh = dev_height(self.tabs[i].dev_h, total);
        let url = if frag.is_empty() { format!("ohb://devtools?tab={target}") } else { format!("ohb://devtools?tab={target}#{frag}") };
        let handle = page::spawn_tab(id, sink, self.history.clone(), NavRequest::get(&url), (w, dh), self.dpr, true);
        let d = TabState {
            id,
            tx: handle.tx,
            join: handle.join,
            title: "DevTools".into(),
            url,
            secure: false,
            loading: true,
            progress: 0.0,
            favicon: None,
            frame: None,
            scroll: (0.0, 0.0),
            target_y: 0.0,
            can_back: false,
            can_fwd: false,
            cursor: Cursor::Default,
            status: String::new(),
            crashed: None,
            find_result: (0, 0),
            zoom: 1.0,
            devtools: None,
            dev_h: 0.0,
            perf: None,
            last_active: Instant::now(),
            asleep: false,
        };
        self.tabs[i].devtools = Some(Box::new(d));
        self.dev_focus = true;
        self.omni_focus = false;
        self.dev_cache = None;
        self.page_cache = None;
        self.resize_tabs();
        self.clamp_scroll();
        self.redraw();
    }

    fn close_devtools(&mut self, i: usize) {
        if let Some(d) = self.tabs.get_mut(i).and_then(|t| t.devtools.take()) {
            d.send(PageMsg::Shutdown);
            drop(d.join);
        }
        self.dev_focus = false;
        self.pressed_in_dev = false;
        self.dev_cache = None;
        self.page_cache = None;
        self.resize_tabs();
        self.redraw();
    }

    fn tab(&self) -> Option<&TabState> {
        self.tabs.get(self.active)
    }

    fn tab_mut(&mut self) -> Option<&mut TabState> {
        self.tabs.get_mut(self.active)
    }

    fn redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn page_viewport(&self, zoom: f32) -> (f32, f32) {
        let c = self.content_rect();
        (c.w / zoom, c.h / zoom)
    }

    fn open_tab(&mut self, url: &str, activate: bool) {
        let id = self.next_id;
        self.next_id += 1;
        let proxy = Mutex::new(self.proxy.clone());
        let sink: page::UiSink = Arc::new(move |tab, ev| {
            let _ = proxy.lock().unwrap().send_event(UserEvent::Page(tab, ev));
        });
        let vp = self.page_viewport(1.0);
        let handle = page::spawn_tab(id, sink, self.history.clone(), NavRequest::get(url), vp, self.dpr, self.js_enabled);
        let t = TabState {
            id,
            tx: handle.tx,
            join: handle.join,
            title: "New Tab".into(),
            url: url.to_string(),
            secure: url.starts_with("https://"),
            loading: true,
            progress: 0.05,
            favicon: None,
            frame: None,
            scroll: (0.0, 0.0),
            target_y: 0.0,
            can_back: false,
            can_fwd: false,
            cursor: Cursor::Default,
            status: String::new(),
            crashed: None,
            find_result: (0, 0),
            zoom: 1.0,
            devtools: None,
            dev_h: 0.0,
            perf: None,
            last_active: Instant::now(),
            asleep: false,
        };
        let insert_at = if self.tabs.is_empty() { 0 } else { self.active + 1 };
        self.tabs.insert(insert_at, t);
        if activate || self.tabs.len() == 1 {
            self.active = insert_at;
            self.page_cache = None;
            self.omni.set(if url == "ohb://newtab" { "" } else { url });
            if url == "ohb://newtab" {
                self.omni_focus = true;
                self.omni.set("");
            }
        } else if insert_at <= self.active && self.tabs.len() > 1 {
            // keep the same tab active
        }
        self.redraw();
    }

    /// Restart the engine: save the open tabs, start a fresh browser process
    /// that reopens them (on the same active tab), and exit this one. All of
    /// the old process's memory, threads and caches go with it.
    fn restart_browser(&mut self, el: &ActiveEventLoop) {
        let urls: Vec<String> = self.tabs.iter().map(|t| t.url.clone()).collect();
        // A saved session hands the tabs over in a file; an unsaved one
        // writes nothing and passes them on the command line.
        let saved = crate::persist::allowed();
        if saved && !crate::save_restore_session(&urls, self.active) {
            self.notice = Some(("Couldn't save the open tabs, so the browser wasn't restarted.".into(), Instant::now()));
            return;
        }
        crate::storage::flush();
        for t in &self.tabs {
            t.send(PageMsg::Shutdown);
        }
        let mut cmd = match std::env::current_exe() {
            Ok(exe) => std::process::Command::new(exe),
            Err(e) => {
                self.notice = Some((format!("Couldn't restart: {e}"), Instant::now()));
                return;
            }
        };
        if saved {
            cmd.arg("--restore-session");
        } else {
            cmd.arg(format!("--active-tab={}", self.active));
            cmd.args(urls.iter().filter(|u| u.starts_with("http://") || u.starts_with("https://") || u.starts_with("ohb://") || u.as_str() == "about:blank"));
        }
        match cmd.spawn() {
            Ok(_) => {
                self.restarting = true;
                el.exit();
            }
            Err(e) => {
                self.notice = Some((format!("Couldn't restart: {e}"), Instant::now()));
            }
        }
    }

    fn close_tab(&mut self, i: usize, el: &ActiveEventLoop) {
        if i >= self.tabs.len() {
            return;
        }
        let t = self.tabs.remove(i);
        // Its unanswered questions go with it (a pending download is dropped).
        for p in self.prompts.iter().filter(|p| p.tab_id == t.id) {
            if let PromptKind::Download { id, .. } = p.kind {
                page::drop_download(id);
            }
        }
        self.prompts.retain(|p| p.tab_id != t.id);
        if !t.url.starts_with("ohb://") {
            self.closed.push(t.url.clone());
        }
        if let Some(d) = &t.devtools {
            d.send(PageMsg::Shutdown);
        }
        t.send(PageMsg::Shutdown);
        drop(t.join);
        if self.tabs.is_empty() {
            el.exit();
            return;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        } else if i < self.active {
            self.active -= 1;
        }
        self.switch_to(self.active);
    }

    /// Capture (true) or release the cursor for the Pointer Lock API.
    fn grab_pointer(&mut self, on: bool) {
        self.pointer_locked = on;
        if let Some(w) = &self.window {
            if on {
                if w.set_cursor_grab(CursorGrabMode::Locked).is_err() {
                    let _ = w.set_cursor_grab(CursorGrabMode::Confined);
                }
            } else {
                let _ = w.set_cursor_grab(CursorGrabMode::None);
            }
            w.set_cursor_visible(!on);
        }
    }

    /// The browser takes the pointer lock back (Esc, focus loss, tab switch).
    fn release_pointer_lock(&mut self) {
        if self.pointer_locked {
            self.grab_pointer(false);
            if let Some(t) = self.tab() {
                t.send(PageMsg::PointerLockLost);
            }
        }
    }

    fn switch_to(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if i != self.active {
            self.release_pointer_lock();
        }
        if let Some(t) = self.tabs.get_mut(self.active) {
            t.last_active = Instant::now();
        }
        self.tabs[i].last_active = Instant::now();
        if self.tabs[i].asleep {
            self.wake_tab(i);
        }
        self.active = i;
        self.page_cache = None;
        self.dev_cache = None;
        self.dev_focus = false;
        self.omni_focus = false;
        self.menu_open = false;
        let url = self.tabs[i].url.clone();
        self.omni.set(if url.starts_with("ohb://newtab") { "" } else { &url });
        // A new-tab page types into the address bar (as in Chrome).
        if url.starts_with("ohb://newtab") || url.is_empty() {
            self.omni_focus = true;
        }
        self.find = None;
        self.update_title();
        self.resize_tabs();
        self.redraw();
    }

    /// Reload a sleeping tab's page.
    fn wake_tab(&mut self, i: usize) {
        let id = self.tabs[i].id;
        let url = self.tabs[i].url.clone();
        let proxy = Mutex::new(self.proxy.clone());
        let sink: page::UiSink = Arc::new(move |tab, ev| {
            let _ = proxy.lock().unwrap().send_event(UserEvent::Page(tab, ev));
        });
        let vp = self.page_viewport(self.tabs[i].zoom);
        let handle = page::spawn_tab(id, sink, self.history.clone(), NavRequest::get(&url), vp, self.dpr, self.js_enabled);
        let t = &mut self.tabs[i];
        t.tx = handle.tx;
        t.join = handle.join;
        t.asleep = false;
        t.loading = true;
        t.progress = 0.05;
        t.scroll = (0.0, 0.0);
        t.target_y = 0.0;
    }

    /// Memory saver: when the browser uses a lot of memory, background tabs
    /// unused the longest are put to sleep (and every tab after 30 minutes
    /// in the background).
    fn sleep_tabs(&mut self) {
        let budget: u64 = std::env::var("OHB_MEMORY_BUDGET_MB").ok().and_then(|v| v.parse().ok()).unwrap_or(4096) << 20;
        let rss = crate::sysmem::rss_bytes().unwrap_or(0);
        let active = self.active;
        let candidates = |tabs: &Vec<TabState>, min_idle: Duration| -> Option<usize> {
            tabs.iter()
                .enumerate()
                .filter(|(i, t)| *i != active && !t.asleep && t.devtools.is_none() && !t.url.starts_with("ohb://") && t.last_active.elapsed() >= min_idle)
                .min_by_key(|(_, t)| t.last_active)
                .map(|(i, _)| i)
        };
        let pick = if rss > budget { candidates(&self.tabs, Duration::from_secs(20)) } else { candidates(&self.tabs, Duration::from_secs(30 * 60)) };
        if let Some(i) = pick {
            let t = &mut self.tabs[i];
            t.send(PageMsg::Shutdown);
            t.join = None;
            t.asleep = true;
            t.frame = None;
            t.perf = None;
            t.loading = false;
        }
    }

    fn update_title(&self) {
        if let (Some(w), Some(t)) = (&self.window, self.tab()) {
            w.set_title(&format!("{} — OpenHeronBrowser", if t.title.is_empty() { "New Tab" } else { &t.title }));
        }
    }

    fn resize_tabs(&self) {
        let (ww, wh) = self.css_size();
        for t in &self.tabs {
            let c = self.content_rect_for(Some(t));
            t.send(PageMsg::Resize { w: c.w, h: c.h, dpr: self.dpr });
            if let Some(d) = &t.devtools {
                d.send(PageMsg::Resize { w: ww, h: (wh - c.bottom() - SPLIT_H).max(1.0), dpr: self.dpr });
            }
        }
    }

    fn navigate(&mut self, input: &str) {
        let url = page::normalize_input(input);
        if let Some(t) = self.tab_mut() {
            t.send(PageMsg::Navigate(NavRequest::get(&url)));
            t.url = url.clone();
            t.loading = true;
            t.progress = 0.05;
            t.crashed = None;
        }
        self.omni.set(&url);
        self.omni_focus = false;
        self.suggestions.clear();
        self.redraw();
    }

    fn layout(&mut self) -> Layout {
        let (w, _) = self.css_size();
        let n = self.tabs.len().max(1) as f32;
        // The window keeps its native title bar (traffic lights sit above the tab strip).
        let left = 8.0;
        let avail = (w - left - 48.0).max(100.0);
        let tw = (avail / n).clamp(48.0, 240.0);
        let mut tabs = Vec::new();
        for i in 0..self.tabs.len() {
            let r = Rect::new(left + i as f32 * tw, 6.0, tw, TAB_H - 6.0);
            let close = Rect::new(r.right() - 26.0, r.y + (r.h - 18.0) / 2.0, 18.0, 18.0);
            tabs.push((r, close));
        }
        let new_tab = Rect::new(left + self.tabs.len() as f32 * tw + 6.0, 9.0, 28.0, 28.0);
        let y = TAB_H + (TOOL_H - 32.0) / 2.0;
        let back = Rect::new(8.0, y, 32.0, 32.0);
        let forward = Rect::new(42.0, y, 32.0, 32.0);
        let reload = Rect::new(76.0, y, 32.0, 32.0);
        let home = Rect::new(110.0, y, 32.0, 32.0);
        let menu = Rect::new(w - 42.0, y, 32.0, 32.0);
        let shield = Rect::new(w - 78.0, y, 32.0, 32.0);
        let omnibox = Rect::new(150.0, TAB_H + (TOOL_H - 34.0) / 2.0, (w - 150.0 - 88.0).max(80.0), 34.0);
        Layout { tabs, new_tab, back, forward, reload, home, omnibox, menu, shield }
    }

    /// HeronBlock for the active tab: (active on this page, requests blocked).
    fn shield_state(&self) -> (bool, usize) {
        let Some(t) = self.tab() else { return (false, 0) };
        let on = crate::ext::is_enabled(crate::ext::HERONBLOCK) && url::Url::parse(&t.url).map(|u| matches!(u.scheme(), "http" | "https") && !crate::ext::is_paused(crate::ext::HERONBLOCK, &u)).unwrap_or(false);
        (on, crate::ext::blocked_count(t.id))
    }

    fn menu_rect(&mut self) -> Rect {
        let l = self.layout();
        Rect::new(l.menu.right() - 300.0, CHROME_H - 4.0, 300.0, MENU_ITEMS.len() as f32 * 32.0 + 12.0)
    }

    /// The active tab's pending question, if any.
    fn prompt(&self) -> Option<&Prompt> {
        let id = self.tab()?.id;
        self.prompts.iter().find(|p| p.tab_id == id)
    }

    /// The prompt bubble (below the address bar's left end) and its buttons.
    fn prompt_rects(&self) -> (Rect, Rect, Rect) {
        let r = Rect::new(150.0, CHROME_H + 6.0, 380.0, 112.0);
        let yes = Rect::new(r.right() - 96.0, r.bottom() - 44.0, 82.0, 32.0);
        let no = Rect::new(yes.x - 90.0, yes.y, 82.0, 32.0);
        (r, yes, no)
    }

    /// Answer the active tab's prompt.
    fn answer_prompt(&mut self, yes: bool, remember: bool) {
        let Some(id) = self.tab().map(|t| t.id) else { return };
        let Some(i) = self.prompts.iter().position(|p| p.tab_id == id) else { return };
        let p = self.prompts.remove(i);
        match p.kind {
            PromptKind::Permission { id: rid, perm, site } => {
                if remember {
                    crate::prefs::set_permission(&site, &perm, if yes { crate::prefs::Decision::Allow } else { crate::prefs::Decision::Block });
                }
                let data = if yes && perm == "clipboard-read" { self.paste() } else { None };
                if let Some(t) = self.tabs.iter().find(|t| t.id == p.tab_id) {
                    t.send(PageMsg::PermissionReply { id: rid, allow: yes, data });
                }
            }
            PromptKind::Download { id: did, name, .. } => {
                if yes {
                    match page::save_download(did) {
                        Some(path) => self.notice = Some((format!("Downloaded {name} to {}", path.display()), Instant::now() + std::time::Duration::from_secs(6))),
                        None => self.notice = Some((format!("Couldn't save {name}"), Instant::now() + std::time::Duration::from_secs(6))),
                    }
                } else {
                    page::drop_download(did);
                }
            }
        }
        self.redraw();
    }

    fn find_rect(&self) -> Rect {
        let (w, _) = self.css_size();
        Rect::new(w - 380.0, CHROME_H + 6.0, 368.0, 44.0)
    }

    fn hit(&mut self, x: f32, y: f32) -> Hot {
        if self.gate.is_some() {
            let Some(l) = self.gate_layout else { return Hot::Gate };
            return if l.agree.contains(x, y) {
                Hot::GateAgree
            } else if l.save.contains(x, y) {
                Hot::GateSave
            } else if l.cont.contains(x, y) {
                Hot::GateContinue
            } else if l.quit.contains(x, y) {
                Hot::GateQuit
            } else {
                Hot::Gate
            };
        }
        if self.alert.is_some() {
            let (w, h) = self.css_size();
            let ok = Rect::new(w / 2.0 + 90.0, h / 2.0 + 30.0, 80.0, 32.0);
            return if ok.contains(x, y) { Hot::AlertOk } else { Hot::None };
        }
        if self.prompt().is_some() {
            let (r, yes, no) = self.prompt_rects();
            if yes.contains(x, y) {
                return Hot::PromptYes;
            }
            if no.contains(x, y) {
                return Hot::PromptNo;
            }
            if r.contains(x, y) {
                return Hot::None;
            }
        }
        if let Some((mx, my, items, _)) = &self.ctx_menu {
            let r = Rect::new(*mx, *my, CTX_W, items.len() as f32 * 30.0 + 8.0);
            if r.contains(x, y) {
                let i = ((y - my - 4.0) / 30.0).floor().max(0.0) as usize;
                return Hot::CtxItem(i.min(items.len().saturating_sub(1)));
            }
        }
        if self.menu_open {
            let r = self.menu_rect();
            if r.contains(x, y) {
                let i = ((y - r.y - 6.0) / 32.0).floor().max(0.0) as usize;
                return Hot::MenuItem(i.min(MENU_ITEMS.len() - 1));
            }
        }
        let l = self.layout();
        if self.omni_focus && !self.suggestions.is_empty() {
            let base = Rect::new(l.omnibox.x, l.omnibox.bottom() + 2.0, l.omnibox.w, self.suggestions.len() as f32 * 34.0 + 8.0);
            if base.contains(x, y) {
                let i = ((y - base.y - 4.0) / 34.0).floor().max(0.0) as usize;
                return Hot::Suggestion(i.min(self.suggestions.len() - 1));
            }
        }
        if let Some(_) = &self.find {
            let fr = self.find_rect();
            if fr.contains(x, y) {
                let bx = fr.right() - 108.0;
                if x >= bx && x < bx + 32.0 {
                    return Hot::FindPrev;
                }
                if x >= bx + 34.0 && x < bx + 66.0 {
                    return Hot::FindNext;
                }
                if x >= bx + 70.0 {
                    return Hot::FindClose;
                }
                return Hot::FindField;
            }
        }
        if y < TAB_H {
            for (i, (r, c)) in l.tabs.iter().enumerate() {
                if c.contains(x, y) && r.w > 70.0 {
                    return Hot::TabClose(i);
                }
                if r.contains(x, y) {
                    return Hot::Tab(i);
                }
            }
            if l.new_tab.contains(x, y) {
                return Hot::NewTab;
            }
            return Hot::None;
        }
        if y < CHROME_H {
            for (r, h) in [(l.back, Hot::Back), (l.forward, Hot::Forward), (l.reload, Hot::Reload), (l.home, Hot::Home), (l.menu, Hot::Menu), (l.shield, Hot::Shield), (l.omnibox, Hot::Omnibox)] {
                if r.contains(x, y) {
                    return h;
                }
            }
            return Hot::None;
        }
        if let Some(d) = self.dev_rect() {
            if d.contains(x, y) {
                return Hot::Dev;
            }
        }
        if let Some(sp) = self.split_rect() {
            if Rect::new(sp.x, sp.y - 2.0, sp.w, sp.h + 2.0).contains(x, y) {
                return Hot::DevSplit;
            }
        }
        if let Some((track, _, _)) = self.scrollbar() {
            if track.contains(x, y) {
                return Hot::Scrollbar;
            }
        }
        Hot::Content
    }

    /// Viewport scrollbar geometry in window CSS px: (track, thumb, scroll range in doc px).
    fn scrollbar(&self) -> Option<(Rect, Rect, f32)> {
        let t = self.tab()?;
        let f = t.frame.as_ref()?;
        let (_, vh) = self.page_viewport(t.zoom);
        let range = f.doc_h - vh;
        if range <= 1.0 || !f.user_scroll {
            return None;
        }
        let c = self.content_rect();
        let track = Rect::new(c.right() - 14.0, c.y, 14.0, c.h);
        let th = (c.h * vh / f.doc_h).max(32.0).min(c.h);
        let ty = c.y + (t.scroll.1 / range).clamp(0.0, 1.0) * (c.h - th);
        Some((track, Rect::new(track.x + 3.0, ty + 2.0, 8.0, th - 4.0), range))
    }

    fn to_page(&self, x: f32, y: f32) -> (f32, f32) {
        let z = self.tab().map(|t| t.zoom).unwrap_or(1.0);
        (x / z, (y - CHROME_H) / z)
    }

    fn set_zoom(&mut self, z: f32) {
        if let Some(t) = self.tab_mut() {
            t.zoom = z.clamp(0.3, 3.0);
            let zz = t.zoom;
            t.send(PageMsg::Zoom(zz));
        }
        self.page_cache = None;
        self.redraw();
    }

    fn update_suggestions(&mut self) {
        let q = self.omni.text.to_lowercase();
        self.suggestions.clear();
        self.sugg_idx = None;
        if q.trim().is_empty() {
            return;
        }
        let h = self.history.lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        for (u, t) in h.iter().rev() {
            if (u.to_lowercase().contains(&q) || t.to_lowercase().contains(&q)) && seen.insert(u.clone()) {
                self.suggestions.push((u.clone(), t.clone()));
                if self.suggestions.len() >= 6 {
                    break;
                }
            }
        }
        // Always offer a search entry first.
        let looks_url = !q.contains(' ') && q.contains('.');
        if !looks_url {
            self.suggestions.insert(0, (format!("ohb-search:{}", self.omni.text), format!("Search DuckDuckGo for “{}”", self.omni.text)));
        }
        self.suggestions.truncate(7);
    }

    fn clamp_scroll(&mut self) {
        let (vw, vh) = {
            let z = self.tab().map(|t| t.zoom).unwrap_or(1.0);
            self.page_viewport(z)
        };
        if let Some(t) = self.tab_mut() {
            let (dw, dh) = t.frame.as_ref().map(|f| (f.doc_w, f.doc_h)).unwrap_or((vw, vh));
            let maxy = (dh - vh).max(0.0);
            let maxx = (dw - vw).max(0.0);
            t.target_y = t.target_y.clamp(0.0, maxy);
            t.scroll.1 = t.scroll.1.clamp(0.0, maxy);
            t.scroll.0 = t.scroll.0.clamp(0.0, maxx);
        }
    }

    fn scroll_by(&mut self, dx: f32, dy: f32, smooth: bool) {
        if !self.tab().and_then(|t| t.frame.as_ref().map(|f| f.user_scroll)).unwrap_or(true) {
            return; // overflow: hidden on the viewport
        }
        if let Some(t) = self.tab_mut() {
            t.target_y += dy;
            t.scroll.0 += dx;
            if !smooth {
                t.scroll.1 = t.target_y;
            }
        }
        self.clamp_scroll();
        self.notify_scroll();
        self.redraw();
    }

    fn notify_scroll(&self) {
        if let Some(t) = self.tab() {
            t.send(PageMsg::Scrolled { x: t.scroll.0, y: t.target_y });
        }
    }

    fn animate(&mut self) -> bool {
        let mut animating = false;
        if let Some(t) = self.tab_mut() {
            let d = t.target_y - t.scroll.1;
            if d.abs() > 0.5 {
                t.scroll.1 += d * 0.35;
                animating = true;
            } else if d != 0.0 {
                t.scroll.1 = t.target_y;
                animating = true;
            }
        }
        animating
    }

    // ------------------------------------------------------------ rendering

    fn render(&mut self) {
        let (w, h) = self.size;
        if w == 0 || h == 0 {
            return;
        }
        let scale = self.dpr;
        let mut pm = match self.frame_pm.take() {
            Some(p) if p.width() == w && p.height() == h => p,
            _ => match tiny_skia::Pixmap::new(w, h) {
                Some(p) => p,
                None => return,
            },
        };
        // Page content (cached; rasterized only when frame/scroll/zoom change).
        let content = self.content_rect();
        let content_dev_y = (content.y * scale).round() as u32;
        let content_dev_h = ((content.bottom() * scale).round() as u32).min(h).saturating_sub(content_dev_y);
        if let Some(t) = self.tabs.get(self.active) {
            let zoom = t.zoom;
            // Reduced-resolution mode: the page renders into a smaller
            // surface that is scaled up to the window (the browser UI stays sharp).
            let rs = self.render_scale;
            let (full_w, full_content_h) = (w, content_dev_h);
            let w = if rs < 1.0 { ((w as f32 * rs).round() as u32).max(1) } else { w };
            let content_dev_h = if rs < 1.0 { ((content_dev_h as f32 * rs).round() as u32).max(1) } else { content_dev_h };
            let page_scale = scale * zoom * rs;
            let t_raster = std::time::Instant::now();
            if let Some(f) = t.frame.clone() {
                // With a separable fixed layer, the cache holds the scrolling
                // content only and the fixed layer is drawn over it each frame.
                let split = crate::paint::raster::fixed_layer_ok(&f.list, t.scroll);
                let need = match &self.page_cache {
                    Some(c) => c.frame != Arc::as_ptr(&f) || c.scroll != t.scroll || c.scale != page_scale || c.pm.width() != w || c.pm.height() != content_dev_h || c.split != split,
                    None => true,
                };
                self.raster.fixed_mode = if split { 1 } else { 0 };
                // A new frame at the same scroll/scale: repaint only what changed.
                let mut need = need;
                if need && content_dev_h > 0 {
                    if let Some(c) = self.page_cache.as_mut() {
                        if c.frame != Arc::as_ptr(&f) && c.scroll == t.scroll && c.scale == page_scale && c.pm.width() == w && c.pm.height() == content_dev_h && c.split == split {
                            let t0 = std::time::Instant::now();
                            let dmg = crate::paint::damage::damage(&c.keep.list, &f.list, t.scroll, split);
                            let mut done = false;
                            match dmg {
                                crate::paint::damage::Damage::None => done = true,
                                crate::paint::damage::Damage::Rect(r, margin) => {
                                    done = crate::paint::damage::patch(&mut self.raster, &f.list, &mut c.pm, t.scroll, page_scale, r, margin, 0.7, split).is_some();
                                }
                                crate::paint::damage::Damage::Full => {}
                            }
                            if std::env::var_os("OHB_DAMAGE_DEBUG").is_some() {
                                eprintln!("[damage] {:?} in {:?} done={done}", dmg, t0.elapsed());
                            }
                            if done {
                                c.frame = Arc::as_ptr(&f);
                                c.keep = f.clone();
                                need = false;
                            }
                        }
                    }
                }
                if need && content_dev_h > 0 {
                    let mut reuse = None;
                    // Changes between the cached frame and this one, patched after the shift.
                    let mut also: Option<(crate::paint::Rect, f32)> = None;
                    if let Some(c) = self.page_cache.take() {
                        // Fast path: vertical scroll only, no fixed layer → shift pixels.
                        if c.scale == page_scale && c.pm.width() == w && c.pm.height() == content_dev_h && c.scroll.0 == t.scroll.0 && (f.list.fixed == 0 || split) && c.split == split {
                            if c.frame == Arc::as_ptr(&f) {
                                reuse = Some(c);
                            } else {
                                // A new frame came with the scroll (scroll listeners,
                                // sticky boxes): still shift, then repaint what changed.
                                match crate::paint::damage::damage(&c.keep.list, &f.list, c.scroll, split) {
                                    crate::paint::damage::Damage::None => reuse = Some(c),
                                    crate::paint::damage::Damage::Rect(r, margin) if r.h < 4000.0 => {
                                        also = Some((r, margin));
                                        reuse = Some(c);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    let bg = if f.list.bg.a == 0 { Color::WHITE } else { f.list.bg };
                    let mut cache = match reuse {
                        Some(mut c) => {
                            let dy_dev = ((t.scroll.1 - c.scroll.1) * page_scale).round() as i64;
                            let rows = content_dev_h as i64;
                            if dy_dev.abs() < rows && dy_dev != 0 {
                                let stride = (w * 4) as usize;
                                let data = c.pm.data_mut();
                                if dy_dev > 0 {
                                    let off = dy_dev as usize * stride;
                                    data.copy_within(off.., 0);
                                } else {
                                    let off = (-dy_dev) as usize * stride;
                                    let len = data.len() - off;
                                    data.copy_within(..len, off);
                                }
                                // Repaint the exposed strip.
                                let (y0, y1) = if dy_dev > 0 { (rows - dy_dev, rows) } else { (0, -dy_dev) };
                                let strip = tiny_skia::Rect::from_ltrb(0.0, y0 as f32, w as f32, y1 as f32).unwrap();
                                let mut p = tiny_skia::Paint::default();
                                p.set_color_rgba8(bg.r, bg.g, bg.b, 255);
                                c.pm.fill_rect(strip, &p, tiny_skia::Transform::identity(), None);
                                // Exact scroll for this frame must match the shifted pixels.
                                let ys = c.scroll.1 + dy_dev as f32 / page_scale;
                                let old_scroll = c.scroll;
                                // Render the strip through a small surface (filter
                                // layers then cover the strip, not the window).
                                let strip_doc = crate::paint::Rect::new(t.scroll.0, ys + y0 as f32 / page_scale, w as f32 / page_scale, (y1 - y0) as f32 / page_scale);
                                let margin = crate::paint::damage::filter_margin(&f.list);
                                if crate::paint::damage::patch(&mut self.raster, &f.list, &mut c.pm, (t.scroll.0, ys), page_scale, strip_doc, margin, 1.0, split).is_none() {
                                    self.raster.render(&f.list, &mut c.pm, (t.scroll.0, ys), page_scale, 0.0, (y0 as f32, y1 as f32));
                                }
                                c.scroll = (t.scroll.0, ys);
                                // Sticky boxes don't move with the shifted pixels:
                                // repaint them at their old and new positions.
                                let mut sticky_full = false;
                                match crate::paint::damage::sticky_damage(&f.list, old_scroll, c.scroll) {
                                    crate::paint::damage::StickyDamage::None => {}
                                    crate::paint::damage::StickyDamage::Rect(r) => {
                                        let margin = crate::paint::damage::filter_margin(&f.list);
                                        if crate::paint::damage::patch(&mut self.raster, &f.list, &mut c.pm, c.scroll, page_scale, r, margin, 1.0, split).is_none() {
                                            sticky_full = true;
                                        }
                                    }
                                    crate::paint::damage::StickyDamage::Full => sticky_full = true,
                                }
                                if sticky_full {
                                    c.pm.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, 255));
                                    self.raster.render(&f.list, &mut c.pm, c.scroll, page_scale, 0.0, (0.0, content_dev_h as f32));
                                }
                                if let Some((r, margin)) = also {
                                    if crate::paint::damage::patch(&mut self.raster, &f.list, &mut c.pm, c.scroll, page_scale, r, margin, 1.0, split).is_none() {
                                        c.pm.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, 255));
                                        self.raster.render(&f.list, &mut c.pm, c.scroll, page_scale, 0.0, (0.0, content_dev_h as f32));
                                    }
                                }
                                c
                            } else if dy_dev == 0 {
                                c.scroll = t.scroll;
                                if let Some((r, margin)) = also {
                                    if crate::paint::damage::patch(&mut self.raster, &f.list, &mut c.pm, c.scroll, page_scale, r, margin, 1.0, split).is_none() {
                                        c.pm.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, 255));
                                        self.raster.render(&f.list, &mut c.pm, c.scroll, page_scale, 0.0, (0.0, content_dev_h as f32));
                                    }
                                }
                                c
                            } else {
                                c.pm.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, 255));
                                self.raster.render(&f.list, &mut c.pm, t.scroll, page_scale, 0.0, (0.0, content_dev_h as f32));
                                c.scroll = t.scroll;
                                c
                            }
                        }
                        None => {
                            let mut cpm = tiny_skia::Pixmap::new(w, content_dev_h.max(1)).unwrap();
                            cpm.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, 255));
                            self.raster.render(&f.list, &mut cpm, t.scroll, page_scale, 0.0, (0.0, content_dev_h as f32));
                            PageCache { pm: cpm, frame: Arc::as_ptr(&f), keep: f.clone(), scroll: t.scroll, scale: page_scale, split }
                        }
                    };
                    cache.frame = Arc::as_ptr(&f);
                    cache.keep = f.clone();
                    self.page_cache = Some(cache);
                }
                self.raster.fixed_mode = 0;
                if let Some(c) = &self.page_cache {
                    if rs < 1.0 {
                        // Scale the low-resolution page up to the content area.
                        let k = full_w as f32 / c.pm.width() as f32;
                        let ky = full_content_h as f32 / c.pm.height() as f32;
                        let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Nearest, ..Default::default() };
                        pm.draw_pixmap(0, 0, c.pm.as_ref(), &paint, tiny_skia::Transform::from_row(k, 0.0, 0.0, ky, 0.0, content_dev_y as f32), None);
                    } else {
                        pm.draw_pixmap(0, content_dev_y as i32, c.pm.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
                    }
                    if split {
                        let sc = c.scroll;
                        self.raster.fixed_mode = 2;
                        self.raster.render(&f.list, &mut pm, sc, scale * zoom, content_dev_y as f32, (content_dev_y as f32, (content_dev_y + full_content_h) as f32));
                        self.raster.fixed_mode = 0;
                    }
                }
                self.perf_raster += t_raster.elapsed();
            } else {
                pm.fill(tiny_skia::Color::WHITE);
            }
        }
        // The DevTools pane (its own page) below the content.
        if let (Some(dr), Some(t)) = (self.dev_rect(), self.tabs.get(self.active)) {
            let dev_y = (dr.y * scale).round() as u32;
            let dev_h = h.saturating_sub(dev_y);
            if let Some(d) = t.devtools.as_ref() {
                match d.frame.clone() {
                    Some(f) if dev_h > 0 => {
                        let need = match &self.dev_cache {
                            Some((ptr, dpm, sc)) => *ptr != Arc::as_ptr(&f) || dpm.width() != w || dpm.height() != dev_h || *sc != d.scroll,
                            None => true,
                        };
                        if need {
                            if let Some(mut dpm) = tiny_skia::Pixmap::new(w, dev_h) {
                                let bg = if f.list.bg.a == 0 { Color::WHITE } else { f.list.bg };
                                dpm.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, 255));
                                self.raster.render(&f.list, &mut dpm, d.scroll, scale, 0.0, (0.0, dev_h as f32));
                                self.dev_cache = Some((Arc::as_ptr(&f), dpm, d.scroll));
                            }
                        }
                        if let Some((_, dpm, _)) = &self.dev_cache {
                            pm.draw_pixmap(0, dev_y as i32, dpm.as_ref(), &tiny_skia::PixmapPaint::default(), tiny_skia::Transform::identity(), None);
                        }
                    }
                    _ => {
                        if let Some(r) = tiny_skia::Rect::from_xywh(0.0, dev_y as f32, w as f32, dev_h as f32) {
                            let mut p = tiny_skia::Paint::default();
                            p.set_color_rgba8(255, 255, 255, 255);
                            pm.fill_rect(r, &p, tiny_skia::Transform::identity(), None);
                        }
                    }
                }
            }
        }
        // Chrome + overlays as a UI display list.
        let items = self.build_chrome();
        let list = crate::paint::DisplayList { items, fixed: 0, bg: Color::TRANSPARENT };
        self.raster.render(&list, &mut pm, (0.0, 0.0), scale, 0.0, (0.0, h as f32));
        // Present.
        if let Some(surface) = self.surface.as_mut() {
            if let (Some(nw), Some(nh)) = (NonZeroU32::new(w), NonZeroU32::new(h)) {
                let _ = surface.resize(nw, nh);
                if let Ok(mut buf) = surface.buffer_mut() {
                    let src = pm.data();
                    for (dst, px) in buf.iter_mut().zip(src.chunks_exact(4)) {
                        *dst = ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | px[2] as u32;
                    }
                    let _ = buf.present();
                }
            }
        }
        self.frame_pm = Some(pm);
        if self.hud {
            let now = Instant::now();
            self.perf_frames.push_back(now);
            while self.perf_frames.front().map(|t| now.duration_since(*t) > std::time::Duration::from_secs(1)).unwrap_or(false) {
                self.perf_frames.pop_front();
            }
            let since = self.perf_mark.elapsed();
            if since >= std::time::Duration::from_millis(500) {
                let pct = (self.perf_raster.as_secs_f32() / since.as_secs_f32() * 100.0).min(100.0);
                self.perf_ui = (self.perf_frames.len() as f32, pct, crate::sysmem::rss_bytes());
                self.perf_raster = std::time::Duration::ZERO;
                self.perf_mark = now;
            }
        }
    }

    fn build_chrome(&mut self) -> Vec<Item> {
        let dark = crate::prefs::dark_theme();
        if self.title_dark != Some(dark) {
            self.title_dark = Some(dark);
            if let Some(w) = &self.window {
                w.set_theme(Some(if dark { winit::window::Theme::Dark } else { winit::window::Theme::Light }));
            }
        }
        let l = self.layout();
        let (w, h) = self.css_size();
        let hot = self.hot;
        let active = self.active;
        let tabs_info: Vec<(String, bool, Option<Arc<ImageData>>, bool)> = self.tabs.iter().map(|t| (t.title.clone(), t.loading, t.favicon.clone(), t.crashed.is_some())).collect();
        let (can_back, can_fwd, loading, progress, secure, url, status, find_result, zoom) = match self.tab() {
            Some(t) => (t.can_back, t.can_fwd, t.loading, t.progress, t.secure, t.url.clone(), t.status.clone(), t.find_result, t.zoom),
            None => (false, false, false, 0.0, false, String::new(), String::new(), (0, 0), 1.0),
        };
        let omni = self.omni.clone();
        let placeholder = crate::prefs::placeholder();
        let omni_focus = self.omni_focus;
        let caret_on = self.caret_on;
        let suggestions = self.suggestions.clone();
        let sugg_idx = self.sugg_idx;
        let find = self.find.clone();
        let find_focus = self.find_focus;
        let menu_open = self.menu_open;
        let menu_rect = self.menu_rect();
        let find_rect = self.find_rect();
        let alert = self.alert.clone();
        let prompt = self.prompt().cloned();
        let prompt_rects = self.prompt_rects();
        let notice = self.notice.as_ref().filter(|(_, until)| Instant::now() < *until).map(|(m, _)| m.clone());
        let ctx_menu = self.ctx_menu.clone();
        let js = self.js_enabled;
        let shield = self.shield_state();
        let split = self.split_rect();
        let dev_drag = self.dev_drag.is_some();
        let sb = self.scrollbar();
        let sb_drag = self.sb_drag.is_some();
        let family = self.family.clone();
        let render_scale = self.render_scale;
        let hud = if self.hud { Some((self.tab().and_then(|t| t.perf.clone()), self.perf_ui, self.render_scale, self.hud_rect(), self.hud_hover)) } else { None };
        let gate_state = self.gate.as_ref().map(|g| (g.agree, g.save, g.appear()));
        let gate_icon = if gate_state.is_some() {
            let px = (GATE_ICON * self.dpr).round() as u32;
            if self.gate_icon.as_ref().map(|(s, _)| *s != px).unwrap_or(true) {
                let pm = crate::shell::chrome::render_warning_icon(px);
                self.gate_icon = Some((px, Arc::new(ImageData { width: px, height: px, pixmap: pm, density: self.dpr, nat: Some((GATE_ICON, GATE_ICON)), svg_par: None })));
            }
            self.gate_icon.as_ref().map(|(_, i)| i.clone())
        } else {
            None
        };
        let mut ui = Ui::new(&mut self.fonts, family);

        // Overlay scrollbar for the page viewport.
        if let Some((track, thumb, _)) = sb {
            let active_sb = hot == Hot::Scrollbar || sb_drag;
            if active_sb {
                ui.rect(track, Color::rgba(240, 242, 245, 220));
                ui.rect(Rect::new(track.x, track.y, 1.0, track.h), Color::rgba(0, 0, 0, 20));
            }
            let th = if active_sb { Rect::new(thumb.x - 1.0, thumb.y, thumb.w + 2.0, thumb.h) } else { Rect::new(thumb.x + 2.0, thumb.y, thumb.w - 3.0, thumb.h) };
            ui.rrect(th, th.w / 2.0, if sb_drag { Color::rgba(0, 0, 0, 150) } else if active_sb { Color::rgba(0, 0, 0, 115) } else { Color::rgba(0, 0, 0, 80) });
        }

        // The bar above DevTools.
        if let Some(sp) = split {
            ui.rect(sp, if hot == Hot::DevSplit || dev_drag { Color::rgb(200, 210, 225) } else { Color::rgb(232, 234, 237) });
            ui.rect(Rect::new(sp.x, sp.y, sp.w, 1.0), Color::rgb(203, 207, 214));
            ui.rect(Rect::new(sp.x, sp.bottom() - 1.0, sp.w, 1.0), Color::rgb(203, 207, 214));
        }
        // Tab strip.
        ui.rect(Rect::new(0.0, 0.0, w, TAB_H), pal().frame);
        for (i, (r, close)) in l.tabs.iter().enumerate() {
            let is_active = i == active;
            let (title, tab_loading, favicon, crashed) = &tabs_info[i];
            if is_active {
                ui.rrect4(*r, [10.0, 10.0, 0.0, 0.0], pal().tab_active);
                if pal().tab_mark.a > 0 {
                    ui.rrect(Rect::new(r.x + 12.0, r.y + 4.0, r.w - 24.0, 2.5), 1.25, pal().tab_mark);
                }
            } else if hot == Hot::Tab(i) || hot == Hot::TabClose(i) {
                ui.rrect4(Rect::new(r.x + 2.0, r.y + 3.0, r.w - 4.0, r.h - 6.0), [8.0; 4], pal().tab_hover);
            } else if i + 1 != active && i + 1 < l.tabs.len() {
                ui.rect(Rect::new(r.right() - 0.5, r.y + 9.0, 1.0, r.h - 18.0), pal().separator);
            }
            let mut tx = r.x + 12.0;
            let icon_y = r.y + r.h / 2.0;
            if *tab_loading {
                // spinner
                let t = Instant::now().elapsed().as_secs_f32();
                let _ = t;
                let phase = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0) % 1000) as f32 / 1000.0 * std::f32::consts::TAU;
                for k in 0..8 {
                    let a = phase + k as f32 * std::f32::consts::TAU / 8.0;
                    let alpha = (255.0 * (k as f32 + 1.0) / 8.0) as u8;
                    let (x0, y0) = (tx + 8.0 + a.cos() * 3.5, icon_y + a.sin() * 3.5);
                    let (x1, y1) = (tx + 8.0 + a.cos() * 7.0, icon_y + a.sin() * 7.0);
                    ui.line(x0, y0, x1, y1, 1.6, Color::rgba(pal().accent.r, pal().accent.g, pal().accent.b, alpha));
                }
            } else if *crashed {
                draw_icon(&mut ui, Icon::Warning, tx + 8.0, icon_y, Color::rgb(200, 80, 40));
            } else if let Some(f) = favicon {
                ui.image(Rect::new(tx, icon_y - 8.0, 16.0, 16.0), f.clone());
            } else {
                draw_icon(&mut ui, Icon::Globe, tx + 8.0, icon_y, pal().text_dim);
            }
            tx += 24.0;
            let close_visible = r.w > 70.0;
            let max_w = (r.right() - tx - if close_visible { 30.0 } else { 6.0 }).max(0.0);
            ui.clip(*r);
            ui.text_ellipsis(title, tx, r.y, r.h, max_w, 12.5, if is_active { 700 } else { 600 }, if is_active { pal().text } else { pal().text_dim });
            ui.unclip();
            if close_visible {
                if hot == Hot::TabClose(i) {
                    ui.rrect(*close, 9.0, pal().hover);
                }
                draw_icon(&mut ui, Icon::Close, close.x + 9.0, close.y + 9.0, pal().icon);
            }
        }
        if hot == Hot::NewTab {
            ui.rrect(l.new_tab, 14.0, pal().tab_hover);
        }
        draw_icon(&mut ui, Icon::Plus, l.new_tab.x + 14.0, l.new_tab.y + 14.0, pal().icon);

        // Toolbar.
        ui.rect(Rect::new(0.0, TAB_H, w, TOOL_H), pal().toolbar);
        ui.rect(Rect::new(0.0, CHROME_H - 1.0, w, 1.0), pal().edge);
        let btn = |ui: &mut Ui, r: Rect, icon: Icon, enabled: bool, is_hot: bool| {
            if is_hot && enabled {
                ui.rrect(r, 16.0, pal().hover);
            }
            draw_icon(ui, icon, r.x + 16.0, r.y + 16.0, if enabled { pal().icon } else { pal().icon_disabled });
        };
        btn(&mut ui, l.back, Icon::Back, can_back, hot == Hot::Back);
        btn(&mut ui, l.forward, Icon::Forward, can_fwd, hot == Hot::Forward);
        btn(&mut ui, l.reload, if loading { Icon::Stop } else { Icon::Reload }, true, hot == Hot::Reload);
        btn(&mut ui, l.home, Icon::Home, true, hot == Hot::Home);
        btn(&mut ui, l.menu, Icon::Menu, true, hot == Hot::Menu || menu_open);
        // HeronBlock: shield with this page's blocked count.
        {
            let (on, n) = shield;
            if hot == Hot::Shield {
                ui.rrect(l.shield, 16.0, pal().hover);
            }
            let col = if on { pal().accent } else { pal().icon_disabled };
            draw_icon(&mut ui, Icon::Shield, l.shield.x + 16.0, l.shield.y + 16.0, col);
            if on && n > 0 {
                let label = if n > 99 { "99+".to_string() } else { n.to_string() };
                let tw = ui.measure(&label, 9.5, 600);
                let bw = (tw + 7.0).max(14.0);
                let br = Rect::new(l.shield.right() - bw + 1.0, l.shield.y + 1.0, bw, 14.0);
                ui.rrect(br, 7.0, pal().accent);
                ui.text(&label, br.x + (bw - tw) / 2.0, br.y, br.h, 9.5, 700, pal().on_accent);
            }
        }
        // Omnibox.
        let o = l.omnibox;
        if omni_focus {
            ui.rrect(o, 17.0, pal().omni_focus);
            ui.border(o, 2.0, 17.0, pal().omni_focus_ring);
        } else {
            ui.rrect(o, 17.0, if hot == Hot::Omnibox { pal().omni_hot } else { pal().omni });
        }
        let icon_x = o.x + 18.0;
        let is_internal = url.starts_with("ohb://") || url.starts_with("about:") || url.is_empty();
        if is_internal {
            draw_icon(&mut ui, Icon::Search, icon_x, o.y + 17.0, pal().text_dim);
        } else if secure {
            draw_icon(&mut ui, Icon::Lock, icon_x, o.y + 17.0, pal().icon);
        } else {
            draw_icon(&mut ui, Icon::Info, icon_x, o.y + 17.0, pal().warn);
        }
        let field = Rect::new(o.x + 36.0, o.y, o.w - 48.0 - if zoom != 1.0 { 50.0 } else { 0.0 }, o.h);
        if omni_focus {
            draw_field(&mut ui, &omni, field, 14.0, true, caret_on, &placeholder, pal().text);
        } else if is_internal || url.starts_with("about:") {
            ui.text(&placeholder, field.x, field.y, field.h, 14.0, 400, pal().text_dim);
        } else {
            // Chrome-style emphasis: dim scheme/path, strong host.
            ui.clip(field);
            let (pre, host, rest) = split_url(&url);
            let mut x = field.x;
            if !secure && url.starts_with("http://") {
                x += ui.text("Not secure  ", x, field.y, field.h, 13.0, 500, pal().warn);
            }
            x += ui.text(&pre, x, field.y, field.h, 14.0, 400, pal().text_dim);
            x += ui.text(&host, x, field.y, field.h, 14.0, 400, pal().text);
            ui.text(&rest, x, field.y, field.h, 14.0, 400, pal().text_dim);
            ui.unclip();
        }
        if zoom != 1.0 {
            let zr = Rect::new(o.right() - 56.0, o.y + 6.0, 46.0, 22.0);
            ui.rrect(zr, 11.0, pal().chip);
            ui.text(&format!("{}%", (zoom * 100.0).round()), zr.x + 8.0, zr.y, zr.h, 11.5, 500, pal().text_dim);
        }
        // Progress bar.
        if loading {
            ui.rect(Rect::new(0.0, CHROME_H - 2.0, w * progress.clamp(0.05, 1.0), 2.0), pal().accent);
        }
        // Status bubble (a browser notice wins over the hovered link).
        let status = notice.unwrap_or(status);
        if !status.is_empty() {
            let tw = ui.measure(&status, 12.0, 400).min(w * 0.6);
            let r = Rect::new(0.0, h - 24.0, tw + 16.0, 24.0);
            ui.rrect4(r, [0.0, 6.0, 0.0, 0.0], pal().popup);
            ui.border(r, 1.0, 0.0, pal().popup_edge);
            ui.clip(r);
            ui.text(&status, r.x + 8.0, r.y, r.h, 12.0, 400, pal().text_dim);
            ui.unclip();
        }
        // Find bar.
        if let Some(f) = &find {
            let r = find_rect;
            ui.shadow(r, 10.0, 12.0, Color::rgba(0, 0, 0, 50));
            ui.rrect(r, 10.0, pal().popup);
            ui.border(r, 1.0, 10.0, pal().popup_edge);
            let fr = Rect::new(r.x + 14.0, r.y + 4.0, r.w - 200.0, r.h - 8.0);
            draw_field(&mut ui, f, fr, 13.5, find_focus, caret_on, "Find in page", pal().text);
            let label = if f.text.is_empty() { String::new() } else { format!("{}/{}", find_result.1, find_result.0) };
            ui.text(&label, r.right() - 170.0, r.y, r.h, 12.5, 400, pal().text_dim);
            let bx = r.right() - 108.0;
            for (i, ic) in [(0usize, Icon::Up), (1, Icon::Down), (2, Icon::Close)] {
                let br = Rect::new(bx + i as f32 * 34.0 + if i == 2 { 4.0 } else { 0.0 }, r.y + 6.0, 32.0, 32.0);
                let is_hot = matches!((i, hot), (0, Hot::FindPrev) | (1, Hot::FindNext) | (2, Hot::FindClose));
                if is_hot {
                    ui.rrect(br, 16.0, pal().hover);
                }
                draw_icon(&mut ui, ic, br.x + 16.0, br.y + 16.0, pal().icon);
            }
        }
        // Omnibox suggestions.
        if omni_focus && !suggestions.is_empty() {
            let r = Rect::new(o.x, o.bottom() + 2.0, o.w, suggestions.len() as f32 * 34.0 + 8.0);
            ui.shadow(r, 12.0, 16.0, Color::rgba(0, 0, 0, 60));
            ui.rrect(r, 12.0, pal().popup);
            ui.border(r, 1.0, 12.0, pal().popup_edge);
            for (i, (u, t)) in suggestions.iter().enumerate() {
                let rr = Rect::new(r.x + 4.0, r.y + 4.0 + i as f32 * 34.0, r.w - 8.0, 34.0);
                if sugg_idx == Some(i) || hot == Hot::Suggestion(i) {
                    ui.rrect(rr, 8.0, pal().popup_hover);
                }
                let is_search = u.starts_with("ohb-search:");
                draw_icon(&mut ui, if is_search { Icon::Search } else { Icon::Globe }, rr.x + 18.0, rr.y + 17.0, pal().text_dim);
                ui.clip(rr);
                if is_search {
                    ui.text(t, rr.x + 38.0, rr.y, rr.h, 13.5, 400, pal().text);
                } else {
                    let tw = ui.text_ellipsis_w(if t.is_empty() { u } else { t }, rr.x + 38.0, rr.y, rr.h, rr.w * 0.5, 13.5, 500, pal().text);
                    ui.text_ellipsis(&format!("— {u}"), rr.x + 46.0 + tw, rr.y, rr.h, rr.w - tw - 60.0, 12.5, 400, pal().link);
                }
                ui.unclip();
            }
        }
        // Performance overlay, under the menus and popups (HeronBlock's too).
        // (Clicks pass through it; it fades further while the pointer is over it.)
        if let Some((perf, (fps, raster_pct, rss), rs, r, hover)) = hud.as_ref() {
            ui.items.push(Item::PushOpacity(if *hover { 0.3 } else { 0.8 }));
            draw_hud(&mut ui, r.x, r.y, perf.as_ref(), *fps, *raster_pct, *rss, *rs);
            ui.items.push(Item::PopOpacity);
        }
        // Menu.
        if menu_open {
            let r = menu_rect;
            ui.shadow(r, 10.0, 18.0, Color::rgba(0, 0, 0, 70));
            ui.rrect(r, 10.0, pal().popup);
            ui.border(r, 1.0, 10.0, pal().popup_edge);
            for (i, label) in MENU_ITEMS.iter().enumerate() {
                let rr = Rect::new(r.x + 6.0, r.y + 6.0 + i as f32 * 32.0, r.w - 12.0, 32.0);
                if hot == Hot::MenuItem(i) {
                    ui.rrect(rr, 6.0, pal().popup_hover);
                }
                let text = match *label {
                    "Toggle JavaScript" => format!("JavaScript: {}", if js { "On" } else { "Off" }),
                    "Render resolution" => format!("Render resolution: {}", if render_scale > 0.75 { "Full" } else if render_scale > 0.375 { "½" } else { "¼" }),
                    "Performance overlay" => format!("Performance overlay: {}", if hud.is_some() { "On" } else { "Off" }),
                    _ => label.to_string(),
                };
                ui.text(&text, rr.x + 14.0, rr.y, rr.h, 13.5, 400, pal().text);
                let short = match *label {
                    "New tab" => "Ctrl+T",
                    "Reopen closed tab" => "Ctrl+Shift+T",
                    "History" => "Ctrl+H",
                    "Zoom in" => "Ctrl +",
                    "Zoom out" => "Ctrl −",
                    "Actual size" => "Ctrl 0",
                    "Find in page…" => "Ctrl+F",
                    "Developer tools" => "F12",
                    "Performance overlay" => "Ctrl+Shift+P",
                    "Render resolution" => "Ctrl+Shift+R",
                    "Quit" => "Ctrl+Q",
                    _ => "",
                };
                if !short.is_empty() {
                    let sw = ui.measure(short, 12.0, 400);
                    ui.text(short, rr.right() - sw - 12.0, rr.y, rr.h, 12.0, 400, pal().text_dim);
                }
            }
        }
        // Context menu.
        if let Some((mx, my, items, _)) = &ctx_menu {
            let r = Rect::new(*mx, *my, CTX_W, items.len() as f32 * 30.0 + 8.0);
            ui.shadow(r, 8.0, 14.0, Color::rgba(0, 0, 0, 70));
            ui.rrect(r, 8.0, pal().popup);
            ui.border(r, 1.0, 8.0, pal().popup_edge);
            for (i, (label, action)) in items.iter().enumerate() {
                let rr = Rect::new(r.x + 4.0, r.y + 4.0 + i as f32 * 30.0, r.w - 8.0, 30.0);
                let info = *action == CTX_INFO;
                if hot == Hot::CtxItem(i) && !info {
                    ui.rrect(rr, 6.0, pal().popup_hover);
                }
                ui.text_ellipsis(label, rr.x + 12.0, rr.y, rr.h, rr.w - 20.0, 13.0, if info { 600 } else { 400 }, if info { pal().text_dim } else { pal().text });
            }
        }
        // Permission / download prompt.
        if let Some(p) = &prompt {
            let (r, yes, no) = prompt_rects;
            ui.shadow(r, 12.0, 18.0, Color::rgba(0, 0, 0, 90));
            ui.rrect(r, 12.0, pal().popup);
            ui.border(r, 1.0, 12.0, pal().popup_edge);
            let (title, line, yes_l, no_l) = match &p.kind {
                PromptKind::Permission { perm, site, .. } => {
                    let host = split_url(site).1;
                    (format!("{} wants to", if host.is_empty() { site.clone() } else { host }), crate::prefs::permission_label(perm).to_string(), "Allow", "Block")
                }
                PromptKind::Download { name, size, site, .. } => {
                    let sz = if *size >= 1 << 20 { format!("{:.1} MB", *size as f64 / 1048576.0) } else { format!("{:.0} KB", (*size as f64 / 1024.0).max(1.0)) };
                    (format!("Download from {site}?"), format!("{name}  ·  {sz}"), "Save", "Cancel")
                }
            };
            ui.text_ellipsis(&title, r.x + 18.0, r.y + 12.0, 22.0, r.w - 36.0, 14.0, 700, pal().text);
            draw_icon(&mut ui, if matches!(p.kind, PromptKind::Download { .. }) { Icon::Down } else { Icon::Lock }, r.x + 26.0, r.y + 50.0, pal().text_dim);
            ui.text_ellipsis(&line, r.x + 42.0, r.y + 39.0, 22.0, r.w - 60.0, 13.0, 500, pal().text_dim);
            ui.rrect(no, 16.0, if hot == Hot::PromptNo { pal().popup_hover } else { pal().hover });
            let nw = ui.measure(no_l, 13.0, 600);
            ui.text(no_l, no.x + (no.w - nw) / 2.0, no.y, no.h, 13.0, 600, pal().text);
            ui.rrect(yes, 16.0, if hot == Hot::PromptYes { pal().text } else { pal().accent });
            let yw = ui.measure(yes_l, 13.0, 700);
            ui.text(yes_l, yes.x + (yes.w - yw) / 2.0, yes.y, yes.h, 13.0, 700, pal().on_accent);
        }
        // Alert dialog.
        if let Some(msg) = &alert {
            ui.rect(Rect::new(0.0, 0.0, w, h), Color::rgba(0, 0, 0, 80));
            let r = Rect::new(w / 2.0 - 190.0, h / 2.0 - 80.0, 380.0, 160.0);
            ui.shadow(r, 12.0, 24.0, Color::rgba(0, 0, 0, 90));
            ui.rrect(r, 12.0, pal().popup);
            ui.border(r, 1.0, 12.0, pal().popup_edge);
            let host = split_url(&url).1;
            ui.text(&format!("{host} says"), r.x + 20.0, r.y + 14.0, 24.0, 15.0, 600, pal().text);
            let mut y = r.y + 46.0;
            for line in wrap_text(&mut ui, msg, r.w - 40.0, 13.5).into_iter().take(3) {
                ui.text(&line, r.x + 20.0, y, 20.0, 13.5, 400, pal().text);
                y += 20.0;
            }
            let ok = Rect::new(w / 2.0 + 90.0, h / 2.0 + 30.0, 80.0, 32.0);
            ui.rrect(ok, 16.0, if hot == Hot::AlertOk { pal().text } else { pal().accent });
            ui.text("OK", ok.x + 29.0, ok.y, ok.h, 13.5, 700, pal().on_accent);
        }
        // The startup prompt, over everything.
        let mut gate_layout = None;
        if let Some(g) = gate_state {
            gate_layout = Some(draw_gate(&mut ui, w, h, g, hot, gate_icon));
        }
        let items = ui.items;
        self.gate_layout = gate_layout;
        items
    }
}

impl Ui<'_> {
    fn text_ellipsis_w(&mut self, text: &str, x: f32, y: f32, h: f32, max_w: f32, size: f32, weight: u16, c: Color) -> f32 {
        let w = self.measure(text, size, weight).min(max_w);
        self.text_ellipsis(text, x, y, h, max_w, size, weight, c);
        w
    }
}

/// Size of the heron mark in the startup prompt (CSS px).
const GATE_ICON: f32 = 136.0;

/// Greedy word wrap measured at `weight`.
fn wrap_weight(ui: &mut Ui, s: &str, max_w: f32, size: f32, weight: u16) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if !cur.is_empty() && ui.measure(&cand, size, weight) > max_w {
            lines.push(std::mem::replace(&mut cur, word.to_string()));
        } else {
            cur = cand;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// The startup prompt: a black card over a dimmed window with the warning,
/// the risk agreement (required) and the save-session choice. Returns where
/// its controls are.
fn draw_gate(ui: &mut Ui, w: f32, h: f32, (agree, save, t): (bool, bool, f32), hot: Hot, icon: Option<Arc<ImageData>>) -> GateLayout {
    const WHITE: Color = Color::rgb(244, 244, 245);
    const DIM: Color = Color::rgba(244, 244, 245, 158);
    const FAINT: Color = Color::rgba(244, 244, 245, 110);
    const GOLD: Color = Color::rgb(245, 200, 66);
    const CORAL: Color = Color::rgb(255, 122, 112);
    const BLACK: Color = Color::rgb(11, 11, 12);
    let card_w = (w - 40.0).min(720.0).max(320.0);
    let pad = if card_w < 520.0 { 24.0 } else { 36.0 };
    let inner = card_w - pad * 2.0;
    let icon_col = if card_w < 520.0 { 0.0 } else { GATE_ICON + 28.0 };
    let text_w = inner - icon_col;
    // Lay out once to measure the card, then draw it centered.
    let header = wrap_weight(ui, "This browser is still hatching.", text_w, 31.0, 800);
    let body = wrap_weight(
        ui,
        "OpenHeronBrowser is an early demo of a new web engine by Heronetic. Not every web technology works yet, and pages aren't sandboxed the way they are in mainstream browsers. Don't enter passwords, payment details or other sensitive information.",
        text_w,
        15.0,
        400,
    );
    let risk = "Browse at your own risk.";
    let risk_inline = body.last().map(|l| ui.measure(l, 15.0, 400) + ui.measure(&format!(" {risk}"), 15.0, 700) <= text_w).unwrap_or(false);
    let label_w = inner - 34.0;
    let agree_lines = wrap_weight(ui, "I understand the risks. OpenHeronBrowser is an unfinished demo that I use at my own risk, and Heronetic isn't liable for any loss or damage that results.", label_w, 14.0, 500);
    let save_sub = wrap_weight(ui, "Keep cookies, sign-ins, site data and settings for next time. Unchecked, nothing is written to disk and everything is forgotten when you quit.", label_w, 13.0, 400);
    let text_h = 22.0 + header.len() as f32 * 38.0 + 12.0 + (body.len() + if risk_inline { 0 } else { 1 }) as f32 * 23.0;
    let top_h = text_h.max(if icon_col > 0.0 { GATE_ICON } else { 0.0 });
    let agree_h = agree_lines.len() as f32 * 20.0;
    let save_h = 20.0 + save_sub.len() as f32 * 19.0;
    let card_h = pad + top_h + 26.0 + 1.0 + 24.0 + agree_h + 18.0 + save_h + 28.0 + 40.0 + pad;
    let x0 = ((w - card_w) / 2.0).round();
    let y0 = (((h - card_h) / 2.0).max(16.0) + (1.0 - t) * 14.0).round();
    let card = Rect::new(x0, y0, card_w, card_h);

    ui.items.push(Item::PushOpacity(t));
    // Dim the browser behind.
    ui.rect(Rect::new(0.0, 0.0, w, h), Color::rgba(5, 5, 6, 190));
    ui.shadow(card, 22.0, 48.0, Color::rgba(0, 0, 0, 160));
    ui.rrect(card, 22.0, BLACK);
    ui.border(card, 1.0, 22.0, Color::rgba(255, 122, 112, 110));

    let (cx, mut y) = (x0 + pad, y0 + pad);
    if let (Some(img), true) = (icon, icon_col > 0.0) {
        ui.image(Rect::new(x0 + card_w - pad - GATE_ICON, y0 + pad - 4.0, GATE_ICON, GATE_ICON), img);
    }
    ui.text("OpenHeronBrowser (DEMO)", cx, y, 18.0, 13.0, 700, CORAL);
    y += 22.0;
    for l in &header {
        ui.text(l, cx, y, 38.0, 31.0, 800, WHITE);
        y += 38.0;
    }
    y += 12.0;
    for (i, l) in body.iter().enumerate() {
        let lw = ui.text(l, cx, y, 23.0, 15.0, 400, DIM);
        if risk_inline && i + 1 == body.len() {
            ui.text(&format!(" {risk}"), cx + lw, y, 23.0, 15.0, 700, WHITE);
        }
        y += 23.0;
    }
    if !risk_inline {
        ui.text(risk, cx, y, 23.0, 15.0, 700, WHITE);
    }
    y = y0 + pad + top_h + 26.0;
    ui.rect(Rect::new(cx, y, inner, 1.0), Color::rgba(255, 255, 255, 22));
    y += 25.0;

    let checkbox = |ui: &mut Ui, x: f32, y: f32, on: bool, hover: bool| {
        let r = Rect::new(x, y, 20.0, 20.0);
        if on {
            ui.rrect(r, 6.0, GOLD);
            ui.line(x + 5.0, y + 10.5, x + 8.6, y + 14.2, 2.3, BLACK);
            ui.line(x + 8.6, y + 14.2, x + 15.2, y + 6.4, 2.3, BLACK);
        } else {
            ui.rrect(r, 6.0, Color::rgba(255, 255, 255, if hover { 26 } else { 10 }));
            ui.border(r, 1.5, 6.0, Color::rgba(255, 255, 255, if hover { 170 } else { 95 }));
        }
    };
    let agree_r = Rect::new(cx - 8.0, y - 8.0, inner + 16.0, agree_h + 16.0);
    if hot == Hot::GateAgree {
        ui.rrect(agree_r, 12.0, Color::rgba(255, 255, 255, 8));
    }
    checkbox(ui, cx, y, agree, hot == Hot::GateAgree);
    for (i, l) in agree_lines.iter().enumerate() {
        ui.text(l, cx + 34.0, y + i as f32 * 20.0, 20.0, 14.0, 500, WHITE);
    }
    y += agree_h + 18.0;
    let save_r = Rect::new(cx - 8.0, y - 8.0, inner + 16.0, save_h + 16.0);
    if hot == Hot::GateSave {
        ui.rrect(save_r, 12.0, Color::rgba(255, 255, 255, 8));
    }
    checkbox(ui, cx, y, save, hot == Hot::GateSave);
    ui.text("Save this session to disk?", cx + 34.0, y, 20.0, 14.0, 600, WHITE);
    for (i, l) in save_sub.iter().enumerate() {
        ui.text(l, cx + 34.0, y + 20.0 + i as f32 * 19.0, 19.0, 13.0, 400, FAINT);
    }
    y += save_h + 28.0;

    // Buttons, right-aligned; the note on the left.
    let cont = Rect::new(x0 + card_w - pad - 132.0, y, 132.0, 40.0);
    let quit = Rect::new(cont.x - 12.0 - 92.0, y, 92.0, 40.0);
    ui.rrect(quit, 20.0, Color::rgba(255, 255, 255, if hot == Hot::GateQuit { 30 } else { 14 }));
    let qw = ui.measure("Quit", 14.0, 600);
    ui.text("Quit", quit.x + (quit.w - qw) / 2.0, quit.y, quit.h, 14.0, 600, WHITE);
    let (cbg, cfg) = if agree {
        (if hot == Hot::GateContinue { Color::rgb(255, 216, 102) } else { GOLD }, BLACK)
    } else {
        (Color::rgba(245, 200, 66, 46), Color::rgba(11, 11, 12, 150))
    };
    ui.rrect(cont, 20.0, cbg);
    let cw = ui.measure("Next", 14.0, 700);
    ui.text("Next", cont.x + (cont.w - cw) / 2.0, cont.y, cont.h, 14.0, 700, cfg);
    if !agree && quit.x - cx > 140.0 {
        let note = "Check the first box to continue.";
        ui.text_ellipsis(note, cx, y, 40.0, quit.x - cx - 16.0, 12.5, 400, FAINT);
    }
    ui.items.push(Item::PopOpacity);
    GateLayout { agree: agree_r, save: save_r, cont, quit }
}

fn wrap_text(ui: &mut Ui, s: &str, max_w: f32, size: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for para in s.split('\n') {
        let mut cur = String::new();
        for word in para.split(' ') {
            let trial = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
            if ui.measure(&trial, size, 400) > max_w && !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur = word.to_string();
            } else {
                cur = trial;
            }
        }
        lines.push(cur);
    }
    lines
}

fn split_url(url: &str) -> (String, String, String) {
    if let Some(i) = url.find("://") {
        let after = &url[i + 3..];
        let end = after.find('/').unwrap_or(after.len());
        let host = &after[..end];
        let rest = &after[end..];
        let pre = &url[..i + 3];
        let pre = if pre == "https://" { "" } else { pre };
        let rest = if rest == "/" { "" } else { rest };
        return (pre.to_string(), host.to_string(), rest.to_string());
    }
    (String::new(), url.to_string(), String::new())
}

fn map_key(k: &WKey) -> Option<Key> {
    Some(match k {
        WKey::Named(n) => match n {
            NamedKey::Backspace => Key::Backspace,
            NamedKey::Delete => Key::Delete,
            NamedKey::ArrowLeft => Key::Left,
            NamedKey::ArrowRight => Key::Right,
            NamedKey::ArrowUp => Key::Up,
            NamedKey::ArrowDown => Key::Down,
            NamedKey::Home => Key::Home,
            NamedKey::End => Key::End,
            NamedKey::Enter => Key::Enter,
            NamedKey::Tab => Key::Tab,
            NamedKey::Escape => Key::Escape,
            NamedKey::PageUp => Key::PageUp,
            NamedKey::PageDown => Key::PageDown,
            NamedKey::Space => Key::Space,
            other => Key::Other(format!("{other:?}")),
        },
        WKey::Character(c) => Key::Char(c.to_string()),
        _ => return None,
    })
}

impl App {
    fn page_mods(&self) -> Mods {
        Mods { ctrl: self.mods.control_key(), shift: self.mods.shift_key(), alt: self.mods.alt_key(), meta: self.mods.super_key() }
    }

    fn cmd(&self) -> bool {
        if cfg!(target_os = "macos") {
            self.mods.super_key()
        } else {
            self.mods.control_key()
        }
    }

    fn copy(&mut self, s: &str) {
        if let Some(cb) = self.clipboard.as_mut() {
            let _ = cb.set_text(s.to_string());
        }
    }

    fn paste(&mut self) -> Option<String> {
        self.clipboard.as_mut().and_then(|cb| cb.get_text().ok())
    }

    fn activate_menu(&mut self, i: usize, el: &ActiveEventLoop) {
        self.menu_open = false;
        match MENU_ITEMS.get(i).copied().unwrap_or("") {
            "New tab" => self.open_tab("ohb://newtab", true),
            "Reopen closed tab" => {
                if let Some(u) = self.closed.pop() {
                    self.open_tab(&u, true);
                }
            }
            "History" => self.open_tab("ohb://history", true),
            "Extensions" => self.open_tab("ohb://extensions", true),
            "Developer tools" => self.toggle_devtools(""),
            "Performance overlay" => self.toggle_hud(),
            "Render resolution" => {
                // Full → ½ → ¼ → full: the page renders smaller and is scaled up.
                self.render_scale = if self.render_scale > 0.75 { 0.5 } else if self.render_scale > 0.375 { 0.25 } else { 1.0 };
                self.page_cache = None;
            }
            "Zoom in" => {
                let z = self.tab().map(|t| t.zoom).unwrap_or(1.0);
                self.set_zoom(z + 0.1);
            }
            "Zoom out" => {
                let z = self.tab().map(|t| t.zoom).unwrap_or(1.0);
                self.set_zoom(z - 0.1);
            }
            "Actual size" => self.set_zoom(1.0),
            "Find in page…" => self.open_find(),
            "Toggle JavaScript" => {
                self.js_enabled = !self.js_enabled;
                // Applies to newly opened tabs; reload current tab in a fresh tab context.
                if let Some(u) = self.tab().map(|t| t.url.clone()) {
                    let i = self.active;
                    self.open_tab(&u, true);
                    self.close_tab(i, el);
                }
            }
            "Clear cookies & cache" => {
                crate::storage::clear_all();
                self.history.lock().unwrap().clear();
            }
            "Settings" => self.open_tab("ohb://settings", true),
            "Site data & storage" => self.open_tab("ohb://storage", true),
            "About OpenHeronBrowser" => self.open_tab("ohb://about", true),
            "Heronetic.com" => self.open_tab("https://heronetic.com/", true),
            "Restart browser" => self.restart_browser(el),
            "Quit" => el.exit(),
            _ => {}
        }
        self.redraw();
    }

    /// Where the performance overlay sits: the content area's top right.
    fn hud_rect(&self) -> Rect {
        let c = self.content_rect();
        Rect::new(c.right() - HUD_W - 18.0, c.y + 10.0, HUD_W, HUD_H)
    }

    fn toggle_hud(&mut self) {
        self.hud = !self.hud;
        self.hud_hover = false;
        page::HUD.store(self.hud, std::sync::atomic::Ordering::Relaxed);
        // Remembered: the overlay stays on across tabs and restarts.
        crate::prefs::set("hud", if self.hud { Some("1") } else { None });
        self.perf_frames.clear();
        self.perf_raster = std::time::Duration::ZERO;
        self.perf_mark = Instant::now();
        for t in self.tabs.iter_mut() {
            t.perf = None;
        }
        self.redraw();
    }

    fn open_find(&mut self) {
        let mut f = TextField::default();
        if let Some(old) = &self.find {
            f = old.clone();
            f.select_all();
        }
        self.find = Some(f);
        self.find_focus = true;
        self.omni_focus = false;
        self.redraw();
    }

    /// Answer the startup prompt (or toggle its checkboxes).
    fn gate_action(&mut self, hot: Hot, el: &ActiveEventLoop) {
        let Some(g) = self.gate.as_mut() else { return };
        if g.appear() < 1.0 {
            return;
        }
        match hot {
            Hot::GateAgree => g.agree = !g.agree,
            Hot::GateSave => g.save = !g.save,
            Hot::GateContinue if g.agree => {
                let save = g.save;
                self.gate = None;
                self.gate_layout = None;
                crate::persist::decide(save);
                if let Some(w) = &self.window {
                    w.set_cursor(CursorIcon::Default);
                }
            }
            Hot::GateQuit => el.exit(),
            _ => {}
        }
        self.redraw();
    }

    fn click(&mut self, hot: Hot, x: f32, y: f32, button: MouseButton, el: &ActiveEventLoop) {
        if self.gate.is_some() {
            if button == MouseButton::Left {
                self.gate_action(hot, el);
            }
            return;
        }
        if self.alert.is_some() {
            if hot == Hot::AlertOk {
                self.alert = None;
            }
            self.redraw();
            return;
        }
        if hot == Hot::PromptYes || hot == Hot::PromptNo {
            self.answer_prompt(hot == Hot::PromptYes, true);
            return;
        }
        if let Some((_, _, items, link)) = self.ctx_menu.take() {
            if let Hot::CtxItem(i) = hot {
                if let Some((_, action)) = items.get(i) {
                    match action {
                        0 => self.tab().into_iter().for_each(|t| t.send(PageMsg::Back)),
                        1 => self.tab().into_iter().for_each(|t| t.send(PageMsg::Forward)),
                        2 => self.tab().into_iter().for_each(|t| t.send(PageMsg::Reload)),
                        3 => {
                            if let Some(l) = &link {
                                self.open_tab(l, false);
                            }
                        }
                        4 => {
                            if let Some(l) = &link {
                                self.copy(l);
                            }
                        }
                        5 => {
                            if let Some(u) = self.tab().map(|t| t.url.clone()) {
                                self.open_tab(&format!("view-source:{u}"), true);
                            }
                        }
                        6 => self.tab().into_iter().for_each(|t| t.send(PageMsg::CopySelection)),
                        7 => self.tab().into_iter().for_each(|t| t.send(PageMsg::SelectAllText)),
                        21 => {
                            // Pause / resume HeronBlock on this site, then reload.
                            if let Some(u) = self.tab().and_then(|t| url::Url::parse(&t.url).ok()) {
                                let site = crate::net::site_of(&u);
                                if !crate::ext::is_enabled(crate::ext::HERONBLOCK) {
                                    crate::ext::set_enabled(crate::ext::HERONBLOCK, true);
                                    crate::ext::set_paused(crate::ext::HERONBLOCK, &site, false);
                                } else {
                                    let paused = crate::ext::is_paused(crate::ext::HERONBLOCK, &u);
                                    crate::ext::set_paused(crate::ext::HERONBLOCK, &site, !paused);
                                }
                                self.tab().into_iter().for_each(|t| t.send(PageMsg::Reload));
                            }
                        }
                        22 => self.open_tab("ohb://extensions", true),
                        8 => {
                            let (px, py) = self.ctx_point;
                            self.toggle_devtools(&format!("at={},{}", px.round(), py.round()));
                        }
                        _ => {}
                    }
                }
                self.redraw();
                return;
            }
            // The shield again: just close its popup (a toggle).
            if hot == Hot::Shield && items.iter().any(|(_, a)| *a == 22) {
                self.redraw();
                return;
            }
            self.redraw();
        }
        if self.menu_open {
            if let Hot::MenuItem(i) = hot {
                self.activate_menu(i, el);
                return;
            }
            self.menu_open = false;
            self.redraw();
            if hot != Hot::Menu {
                // fall through to normal handling
            } else {
                return;
            }
        }
        if !matches!(hot, Hot::Omnibox | Hot::Suggestion(_)) && self.omni_focus {
            self.omni_focus = false;
            self.suggestions.clear();
            if let Some(u) = self.tab().map(|t| t.url.clone()) {
                self.omni.set(if u.starts_with("ohb://newtab") { "" } else { &u });
            }
        }
        if !matches!(hot, Hot::FindField | Hot::FindNext | Hot::FindPrev) {
            self.find_focus = false;
        }
        match hot {
            Hot::Tab(i) => {
                if button == MouseButton::Middle {
                    self.close_tab(i, el);
                } else {
                    self.switch_to(i);
                }
            }
            Hot::TabClose(i) => self.close_tab(i, el),
            Hot::NewTab => self.open_tab("ohb://newtab", true),
            Hot::Back => self.tab().into_iter().for_each(|t| t.send(PageMsg::Back)),
            Hot::Forward => self.tab().into_iter().for_each(|t| t.send(PageMsg::Forward)),
            Hot::Reload => {
                if let Some(t) = self.tab() {
                    t.send(if t.loading { PageMsg::Stop } else { PageMsg::Reload });
                }
            }
            Hot::Home => self.navigate("ohb://newtab"),
            Hot::Menu => {
                self.menu_open = !self.menu_open;
            }
            Hot::Shield => {
                let l = self.layout();
                let (on, n) = self.shield_state();
                let url = self.tab().map(|t| t.url.clone()).unwrap_or_default();
                let parsed = url::Url::parse(&url).ok().filter(|u| matches!(u.scheme(), "http" | "https"));
                let mut items = vec![(if on { format!("HeronBlock · {n} blocked on this page") } else { "HeronBlock is off here".to_string() }, CTX_INFO)];
                if let Some(u) = &parsed {
                    let site = crate::net::site_of(u);
                    items.push((if on { format!("Turn off on {site}") } else { format!("Turn on for {site}") }, 21));
                }
                items.push(("Manage extensions…".to_string(), 22));
                let (w, _) = self.css_size();
                self.ctx_menu = Some(((l.shield.right() - CTX_W).max(4.0).min(w - CTX_W - 4.0), CHROME_H - 4.0, items, None));
            }
            Hot::Omnibox => {
                let now = Instant::now();
                let dbl = now.duration_since(self.last_click.0) < Duration::from_millis(400) && (x - self.last_click.1).abs() < 4.0;
                if !self.omni_focus {
                    self.omni_focus = true;
                    self.omni.select_all();
                    self.omni_edited = false;
                } else if dbl {
                    self.omni.select_all();
                } else {
                    let l = self.layout();
                    let rel = x - (l.omnibox.x + 36.0);
                    let omni = self.omni.clone();
                    let family = self.family.clone();
                    let mut ui = Ui::new(&mut self.fonts, family);
                    let c = caret_from_x(&mut ui, &omni, 14.0, rel);
                    self.omni.caret = c;
                    if !self.mods.shift_key() {
                        self.omni.anchor = c;
                    }
                }
                self.find_focus = false;
                self.last_click = (now, x, y);
            }
            Hot::Suggestion(i) => {
                if let Some((u, _)) = self.suggestions.get(i).cloned() {
                    let target = u.strip_prefix("ohb-search:").map(crate::prefs::search_url);
                    self.navigate(&target.unwrap_or(u));
                }
            }
            Hot::FindField => self.find_focus = true,
            Hot::FindNext | Hot::FindPrev => {
                if let (Some(f), Some(t)) = (&self.find, self.tab()) {
                    t.send(PageMsg::Find(f.text.clone(), hot == Hot::FindNext));
                }
            }
            Hot::FindClose => {
                self.find = None;
                self.tab().into_iter().for_each(|t| t.send(PageMsg::FindClose));
            }
            _ => {}
        }
        self.redraw();
    }

    fn key(&mut self, ev: KeyEvent, el: &ActiveEventLoop) {
        // Esc always takes the pointer lock back (and isn't sent to the page).
        if self.pointer_locked && matches!(ev.logical_key, WKey::Named(NamedKey::Escape)) {
            if ev.state == ElementState::Pressed {
                self.release_pointer_lock();
            }
            return;
        }
        if ev.state != ElementState::Pressed {
            // A release: the page's keyup for a key it saw go down (it
            // ignores keys it isn't holding).
            if let (Some(k), Some(t)) = (map_key(&ev.logical_key), self.tab()) {
                t.send(PageMsg::KeyUp { key: k, mods: self.page_mods() });
            }
            return;
        }
        let cmd = self.cmd();
        let shift = self.mods.shift_key();
        let alt = self.mods.alt_key();
        let word_mod = if cfg!(target_os = "macos") { alt } else { self.mods.control_key() };
        if self.gate.is_some() {
            // Only answering the prompt (Enter once agreed) or quitting.
            match &ev.logical_key {
                WKey::Named(NamedKey::Enter) => self.gate_action(Hot::GateContinue, el),
                WKey::Character(c) if cmd && c.eq_ignore_ascii_case("q") => el.exit(),
                _ => {}
            }
            return;
        }
        if self.alert.is_some() {
            if matches!(ev.logical_key, WKey::Named(NamedKey::Enter) | WKey::Named(NamedKey::Escape) | WKey::Named(NamedKey::Space)) {
                self.alert = None;
                self.redraw();
            }
            return;
        }
        if self.prompt().is_some() && matches!(ev.logical_key, WKey::Named(NamedKey::Escape)) {
            // Dismissed: a "no" this time, not remembered.
            self.answer_prompt(false, false);
            return;
        }
        // Global shortcuts.
        if let WKey::Character(c) = &ev.logical_key {
            let c = c.to_lowercase();
            // ⌘⇧P: performance overlay; ⌘⇧R: render resolution (full / ½ / ¼).
            if cmd && shift && c == "p" {
                self.toggle_hud();
                return;
            }
            if cmd && shift && c == "r" {
                self.render_scale = if self.render_scale > 0.75 { 0.5 } else if self.render_scale > 0.375 { 0.25 } else { 1.0 };
                self.page_cache = None;
                self.redraw();
                return;
            }
            if cmd && shift && matches!(c.as_str(), "i" | "j" | "c") {
                match c.as_str() {
                    "i" => self.toggle_devtools(""),
                    "j" => self.toggle_devtools("console"),
                    _ => self.toggle_devtools("pick"),
                }
                return;
            }
            if cmd {
                match c.as_str() {
                    "t" if shift => {
                        if let Some(u) = self.closed.pop() {
                            self.open_tab(&u, true);
                        }
                        return;
                    }
                    "t" => {
                        self.open_tab("ohb://newtab", true);
                        return;
                    }
                    "w" => {
                        let i = self.active;
                        self.close_tab(i, el);
                        return;
                    }
                    "q" => {
                        el.exit();
                        return;
                    }
                    "l" => {
                        self.omni_focus = true;
                        self.omni.select_all();
                        self.redraw();
                        return;
                    }
                    "r" => {
                        self.tab().into_iter().for_each(|t| t.send(PageMsg::Reload));
                        return;
                    }
                    "f" => {
                        self.open_find();
                        return;
                    }
                    "h" => {
                        self.open_tab("ohb://history", true);
                        return;
                    }
                    "=" | "+" => {
                        let z = self.tab().map(|t| t.zoom).unwrap_or(1.0);
                        self.set_zoom(z + 0.1);
                        return;
                    }
                    "-" => {
                        let z = self.tab().map(|t| t.zoom).unwrap_or(1.0);
                        self.set_zoom(z - 0.1);
                        return;
                    }
                    "0" => {
                        self.set_zoom(1.0);
                        return;
                    }
                    d if d.len() == 1 && d.chars().next().unwrap().is_ascii_digit() => {
                        let n = d.parse::<usize>().unwrap_or(1);
                        let i = if n == 9 { self.tabs.len().saturating_sub(1) } else { (n - 1).min(self.tabs.len().saturating_sub(1)) };
                        self.switch_to(i);
                        return;
                    }
                    _ => {}
                }
            }
        }
        match &ev.logical_key {
            WKey::Named(NamedKey::Tab) if self.mods.control_key() => {
                let n = self.tabs.len();
                if n > 0 {
                    let i = if shift { (self.active + n - 1) % n } else { (self.active + 1) % n };
                    self.switch_to(i);
                }
                return;
            }
            WKey::Named(NamedKey::F12) => {
                self.toggle_devtools("");
                return;
            }
            WKey::Named(NamedKey::F5) => {
                self.tab().into_iter().for_each(|t| t.send(PageMsg::Reload));
                return;
            }
            WKey::Named(NamedKey::F6) => {
                self.omni_focus = true;
                self.omni.select_all();
                self.redraw();
                return;
            }
            WKey::Named(NamedKey::ArrowLeft) if alt && !self.omni_focus && !self.find_focus => {
                self.tab().into_iter().for_each(|t| t.send(PageMsg::Back));
                return;
            }
            WKey::Named(NamedKey::ArrowRight) if alt && !self.omni_focus && !self.find_focus => {
                self.tab().into_iter().for_each(|t| t.send(PageMsg::Forward));
                return;
            }
            WKey::Named(NamedKey::BrowserBack) => {
                self.tab().into_iter().for_each(|t| t.send(PageMsg::Back));
                return;
            }
            WKey::Named(NamedKey::BrowserForward) => {
                self.tab().into_iter().for_each(|t| t.send(PageMsg::Forward));
                return;
            }
            _ => {}
        }
        // Omnibox / find field editing.
        if self.omni_focus || (self.find_focus && self.find.is_some()) {
            let is_omni = self.omni_focus;
            let mut submit = false;
            let mut escape = false;
            let mut changed = false;
            let mut sugg_move: i32 = 0;
            let mut clip_copy: Option<String> = None;
            let mut paste = false;
            {
                let field = if is_omni { &mut self.omni } else { self.find.as_mut().unwrap() };
                match &ev.logical_key {
                    WKey::Named(NamedKey::Enter) => submit = true,
                    WKey::Named(NamedKey::Escape) => escape = true,
                    WKey::Named(NamedKey::Backspace) => {
                        field.backspace(word_mod);
                        changed = true;
                    }
                    WKey::Named(NamedKey::Delete) => {
                        field.delete();
                        changed = true;
                    }
                    WKey::Named(NamedKey::ArrowLeft) => field.left(shift, word_mod),
                    WKey::Named(NamedKey::ArrowRight) => field.right(shift, word_mod),
                    WKey::Named(NamedKey::Home) => field.home(shift),
                    WKey::Named(NamedKey::End) => field.end(shift),
                    WKey::Named(NamedKey::ArrowDown) => sugg_move = 1,
                    WKey::Named(NamedKey::ArrowUp) => sugg_move = -1,
                    WKey::Character(c) if cmd => match c.to_lowercase().as_str() {
                        "a" => field.select_all(),
                        "c" => clip_copy = Some(field.selected_text()),
                        "x" => {
                            clip_copy = Some(field.selected_text());
                            field.backspace(false);
                            changed = true;
                        }
                        "v" => paste = true,
                        _ => {}
                    },
                    _ => {
                        if let Some(t) = &ev.text {
                            if !cmd && !t.chars().any(|c| c.is_control()) {
                                field.insert(t);
                                changed = true;
                            }
                        }
                    }
                }
            }
            if let Some(s) = clip_copy {
                self.copy(&s);
            }
            if paste {
                if let Some(s) = self.paste() {
                    let s = s.replace(['\n', '\r'], " ");
                    if is_omni {
                        self.omni.insert(&s);
                    } else if let Some(f) = self.find.as_mut() {
                        f.insert(&s);
                    }
                    changed = true;
                }
            }
            self.caret_on = true;
            self.last_blink = Instant::now();
            if is_omni {
                if changed {
                    self.omni_edited = true;
                    self.update_suggestions();
                }
                if sugg_move != 0 && !self.suggestions.is_empty() {
                    let n = self.suggestions.len() as i32;
                    let cur = self.sugg_idx.map(|x| x as i32).unwrap_or(-1);
                    let nx = (cur + sugg_move).rem_euclid(n);
                    self.sugg_idx = Some(nx as usize);
                }
                if submit {
                    if let Some(i) = self.sugg_idx {
                        self.click(Hot::Suggestion(i), 0.0, 0.0, MouseButton::Left, el);
                    } else {
                        let text = self.omni.text.clone();
                        if self.mods.alt_key() {
                            let u = page::normalize_input(&text);
                            self.open_tab(&u, true);
                        } else {
                            self.navigate(&text);
                        }
                    }
                }
                if escape {
                    self.omni_focus = false;
                    self.suggestions.clear();
                    if let Some(u) = self.tab().map(|t| t.url.clone()) {
                        self.omni.set(if u.starts_with("ohb://newtab") { "" } else { &u });
                    }
                }
            } else {
                let q = self.find.as_ref().map(|f| f.text.clone()).unwrap_or_default();
                if changed || submit {
                    self.tab().into_iter().for_each(|t| t.send(PageMsg::Find(q.clone(), !shift)));
                }
                if escape {
                    self.find = None;
                    self.find_focus = false;
                    self.tab().into_iter().for_each(|t| t.send(PageMsg::FindClose));
                }
            }
            self.redraw();
            return;
        }
        if let WKey::Named(NamedKey::Escape) = ev.logical_key {
            if self.find.is_some() {
                self.find = None;
                self.tab().into_iter().for_each(|t| t.send(PageMsg::FindClose));
                self.redraw();
            }
            if let Some(t) = self.tab() {
                if t.loading {
                    t.send(PageMsg::Stop);
                }
            }
        }
        // DevTools pane keyboard.
        if self.dev_focus && self.dev_tab().is_some() {
            let editable = self.dev_tab().and_then(|t| t.frame.as_ref().map(|f| f.focus_editable)).unwrap_or(false);
            if cmd {
                if let WKey::Character(c) = &ev.logical_key {
                    match c.to_lowercase().as_str() {
                        "v" if editable => {
                            if let Some(s) = self.paste() {
                                self.dev_tab().into_iter().for_each(|t| t.send(PageMsg::Text(s.clone())));
                            }
                            return;
                        }
                        "c" if !editable => {
                            self.dev_tab().into_iter().for_each(|t| t.send(PageMsg::CopySelection));
                            return;
                        }
                        _ => {}
                    }
                }
            }
            if let Some(k) = map_key(&ev.logical_key) {
                let mods = self.page_mods();
                if let Some(t) = self.dev_tab() {
                    t.send(PageMsg::Key { key: k, mods });
                    if !cmd {
                        if let Some(txt) = &ev.text {
                            if !txt.chars().any(|c| c.is_control()) {
                                t.send(PageMsg::Text(txt.to_string()));
                            }
                        }
                    }
                }
            }
            return;
        }
        // Page keyboard handling.
        let editable = self.tab().and_then(|t| t.frame.as_ref().map(|f| f.focus_editable)).unwrap_or(false);
        let vh = self.content_rect().h;
        if !editable && !cmd {
            let dy = match &ev.logical_key {
                WKey::Named(NamedKey::ArrowDown) => Some(48.0),
                WKey::Named(NamedKey::ArrowUp) => Some(-48.0),
                WKey::Named(NamedKey::PageDown) => Some(vh * 0.85),
                WKey::Named(NamedKey::PageUp) => Some(-vh * 0.85),
                WKey::Named(NamedKey::Space) => Some(if shift { -vh * 0.85 } else { vh * 0.85 }),
                WKey::Named(NamedKey::Home) => Some(-1e9),
                WKey::Named(NamedKey::End) => Some(1e9),
                _ => None,
            };
            if let Some(dy) = dy {
                // The page sees the key first (games and custom handlers use
                // arrows and space); it scrolls only if the keydown isn't
                // cancelled.
                match (map_key(&ev.logical_key), self.tab()) {
                    (Some(k), Some(t)) => {
                        t.send(PageMsg::KeyDown { key: k, mods: self.page_mods() });
                        // The focus may have just moved to a text field (the
                        // window's copy of the page state lags): the page
                        // inserts the space if so, and scrolls otherwise.
                        if matches!(ev.logical_key, WKey::Named(NamedKey::Space)) {
                            t.send(PageMsg::Text(" ".into()));
                        }
                    }
                    _ => self.scroll_by(0.0, dy, true),
                }
                return;
            }
        }
        if cmd && !editable {
            if let WKey::Character(c) = &ev.logical_key {
                if c.eq_ignore_ascii_case("v") {
                    return;
                }
            }
        }
        if cmd && editable {
            if let WKey::Character(c) = &ev.logical_key {
                if c.eq_ignore_ascii_case("v") {
                    if let Some(s) = self.paste() {
                        self.tab().into_iter().for_each(|t| t.send(PageMsg::Text(s.clone())));
                    }
                    return;
                }
            }
        }
        if let Some(k) = map_key(&ev.logical_key) {
            let mods = self.page_mods();
            if let Some(t) = self.tab() {
                t.send(PageMsg::KeyDown { key: k, mods });
                if !cmd {
                    if let Some(txt) = &ev.text {
                        // (Modifier keys can carry an empty text: nothing to insert.)
                        if !txt.is_empty() && !txt.chars().any(|c| c.is_control()) {
                            t.send(PageMsg::Text(txt.to_string()));
                        }
                    }
                }
            }
        }
    }

    fn on_devtools_event(&mut self, i: usize, ev: PageEvent) {
        let is_active = i == self.active;
        let Some(d) = self.tabs[i].devtools.as_mut() else { return };
        let mut redraw = is_active;
        match ev {
            PageEvent::Frame(f) => d.frame = Some(f),
            PageEvent::Cursor(c) => {
                d.cursor = c;
                if is_active && self.hot == Hot::Dev {
                    if let Some(w) = &self.window {
                        w.set_cursor(cursor_icon(c));
                    }
                }
                redraw = false;
            }
            PageEvent::ScrollTo(x, y) => {
                if !x.is_nan() {
                    d.scroll = (x, y);
                    d.target_y = y;
                }
            }
            PageEvent::Loading(l, p) => {
                d.loading = l;
                d.progress = p;
                redraw = false;
            }
            PageEvent::OpenTab(u) => {
                self.open_tab(&u, true);
                return;
            }
            PageEvent::Copy(s) => {
                self.copy(&s);
                redraw = false;
            }
            PageEvent::DevtoolsClose | PageEvent::Crashed(_) => {
                self.close_devtools(i);
                return;
            }
            _ => redraw = false,
        }
        if redraw {
            self.redraw();
        }
    }

    fn on_page_event(&mut self, tab_id: u64, ev: PageEvent) {
        if let Some(i) = self.tabs.iter().position(|t| t.devtools.as_ref().map(|d| d.id == tab_id).unwrap_or(false)) {
            self.on_devtools_event(i, ev);
            return;
        }
        let idx = match self.tabs.iter().position(|t| t.id == tab_id) {
            Some(i) => i,
            None => return,
        };
        let is_active = idx == self.active;
        let hud_on = self.hud;
        let zoom = self.tabs[idx].zoom;
        let (vw, vh) = self.page_viewport(zoom);
        let t = &mut self.tabs[idx];
        let mut redraw = is_active;
        let mut grab: Option<bool> = None;
        match ev {
            PageEvent::Frame(f) => {
                // IME popups (accents, CJK candidates) appear at the caret.
                if is_active {
                    if let (Some(w), Some(r)) = (&self.window, f.ime_rect) {
                        let (sx, sy) = t.scroll;
                        let x = (r[0] - sx) * zoom;
                        let y = (r[1] - sy) * zoom + CHROME_H;
                        w.set_ime_cursor_area(winit::dpi::LogicalPosition::new(x as f64, y as f64), winit::dpi::LogicalSize::new((r[2] * zoom).max(1.0) as f64, (r[3] * zoom).max(1.0) as f64));
                    }
                }
                t.frame = Some(f);
                let (dw, dh) = t.frame.as_ref().map(|f| (f.doc_w, f.doc_h)).unwrap();
                let maxy = (dh - vh).max(0.0);
                if t.target_y > maxy {
                    t.target_y = maxy;
                    t.scroll.1 = t.scroll.1.min(maxy);
                }
                t.scroll.0 = t.scroll.0.min((dw - vw).max(0.0));
            }
            // (Sent with every frame: only a change updates the window.)
            PageEvent::Title(s) if s == t.title => {}
            PageEvent::Title(s) => {
                t.title = s;
                if is_active {
                    self.update_title();
                }
            }
            PageEvent::Url(u, _) if u.starts_with("ohb://crashed") => {}
            PageEvent::Url(u, secure) => {
                t.url = u.clone();
                t.secure = secure;
                if is_active && !self.omni_focus {
                    self.omni.set(if u.starts_with("ohb://newtab") { "" } else { &u });
                }
            }
            PageEvent::Loading(l, p) => {
                t.loading = l;
                t.progress = p;
            }
            PageEvent::Cursor(c) => {
                t.cursor = c;
                if is_active && self.hot == Hot::Content {
                    if let Some(w) = &self.window {
                        w.set_cursor(cursor_icon(c));
                    }
                }
                redraw = false;
            }
            PageEvent::Status(s) => t.status = s,
            PageEvent::OpenTab(u) => {
                self.open_tab(&u, false);
                return;
            }
            PageEvent::HistoryState(b, f) => {
                t.can_back = b;
                t.can_fwd = f;
            }
            PageEvent::ScrollTo(x, y) => {
                if x.is_nan() {
                    // Scroll-by request (unconsumed wheel/keys).
                    t.target_y += y;
                } else {
                    t.scroll = (x, y);
                    t.target_y = y;
                }
                if is_active {
                    self.clamp_scroll();
                    if x.is_nan() {
                        self.notify_scroll();
                    }
                }
            }
            PageEvent::Favicon(f) => t.favicon = f,
            PageEvent::Alert(m) => {
                if is_active {
                    self.alert = Some(m);
                }
            }
            PageEvent::Permission { id, perm, site } => match crate::prefs::permission(&site, &perm) {
                crate::prefs::Decision::Allow => {
                    let data = if perm == "clipboard-read" { self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) } else { None };
                    self.tabs[idx].send(PageMsg::PermissionReply { id, allow: true, data });
                }
                crate::prefs::Decision::Block => self.tabs[idx].send(PageMsg::PermissionReply { id, allow: false, data: None }),
                crate::prefs::Decision::Ask => {
                    let tab_id = self.tabs[idx].id;
                    // One question per site and permission at a time.
                    if self.prompts.iter().any(|p| p.tab_id == tab_id && matches!(&p.kind, PromptKind::Permission { perm: q, site: s, .. } if *q == perm && *s == site)) {
                        self.tabs[idx].send(PageMsg::PermissionReply { id, allow: false, data: None });
                    } else {
                        self.prompts.push(Prompt { tab_id, kind: PromptKind::Permission { id, perm, site } });
                    }
                }
            },
            PageEvent::Download { id, name, size, site } => {
                let tab_id = self.tabs[idx].id;
                self.prompts.push(Prompt { tab_id, kind: PromptKind::Download { id, name, size, site } });
            }
            PageEvent::FindResult(n, i) => t.find_result = (n, i),
            PageEvent::Crashed(e) => {
                t.crashed = Some(e.clone());
                t.loading = false;
                t.frame = None;
                // The tab thread is gone; start a fresh one for this tab that shows the crash page.
                let enc: String = e
                    .bytes()
                    .take(400)
                    .map(|b| if b.is_ascii_alphanumeric() || b == b' ' || b == b'.' { (b as char).to_string() } else { format!("%{b:02X}") })
                    .collect::<String>()
                    .replace(' ', "%20");
                let id = t.id;
                let proxy = Mutex::new(self.proxy.clone());
                let sink: page::UiSink = Arc::new(move |tab, ev| {
                    let _ = proxy.lock().unwrap().send_event(UserEvent::Page(tab, ev));
                });
                let handle = page::spawn_tab(id, sink, self.history.clone(), NavRequest::get(&format!("ohb://crashed?reason={enc}")), (vw, vh), self.dpr, self.js_enabled);
                let t = &mut self.tabs[idx];
                t.tx = handle.tx;
                t.join = handle.join;
                if is_active {
                    self.page_cache = None;
                }
            }
            PageEvent::Copy(s) => {
                self.copy(&s);
                redraw = false;
            }
            PageEvent::Perf(s) => {
                t.perf = Some(s);
                redraw = hud_on && is_active;
            }
            PageEvent::PointerLock(on) => {
                redraw = false;
                if is_active {
                    grab = Some(on);
                } else if on {
                    // Only the visible tab may lock the pointer.
                    t.send(PageMsg::PointerLockLost);
                }
            }
            PageEvent::EvalResult(_) | PageEvent::Busy(_) | PageEvent::DevtoolsClose => redraw = false,
        }
        if let Some(on) = grab {
            self.grab_pointer(on);
        }
        if redraw {
            self.redraw();
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn exiting(&mut self, _el: &ActiveEventLoop) {
        // Save cookies and site data before the process ends (a restart
        // saved them already; the new process may be writing by now).
        if !self.restarting {
            crate::storage::flush();
        }
    }

    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let icon_pm = render_icon(64);
        let mut rgba = icon_pm.data().to_vec();
        for px in rgba.chunks_exact_mut(4) {
            let a = px[3] as u32;
            if a > 0 && a < 255 {
                for c in px.iter_mut().take(3) {
                    *c = ((*c as u32 * 255) / a).min(255) as u8;
                }
            }
        }
        let attrs = Window::default_attributes()
            .with_title("OpenHeronBrowser")
            .with_inner_size(LogicalSize::new(1280.0, 860.0))
            .with_min_inner_size(LogicalSize::new(420.0, 300.0))
            .with_window_icon(winit::window::Icon::from_rgba(rgba, 64, 64).ok())
            // The title bar follows the shell's theme.
            .with_theme(Some(if crate::prefs::dark_theme() { winit::window::Theme::Dark } else { winit::window::Theme::Light }));
        #[cfg(all(unix, not(target_os = "macos")))]
        let attrs = {
            use winit::platform::wayland::WindowAttributesExtWayland;
            use winit::platform::x11::WindowAttributesExtX11;
            let a = WindowAttributesExtX11::with_name(attrs, "ohb", "OpenHeronBrowser");
            WindowAttributesExtWayland::with_name(a, "openheronbrowser", "ohb")
        };
        let window = match el.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                eprintln!("OpenHeronBrowser: could not create a window: {e}");
                el.exit();
                return;
            }
        };
        window.set_ime_allowed(true);
        self.dpr = window.scale_factor() as f32;
        let sz = window.inner_size();
        self.size = (sz.width.max(1), sz.height.max(1));
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.window = Some(window);
        self.context = Some(context);
        self.surface = Some(surface);
        let initial = std::mem::take(&mut self.initial);
        if initial.is_empty() {
            self.open_tab("ohb://newtab", true);
        } else {
            for (i, u) in initial.iter().enumerate() {
                // Each one goes after the last (open_tab inserts after the active tab).
                if !self.tabs.is_empty() {
                    self.active = self.tabs.len() - 1;
                }
                self.open_tab(&page::normalize_input(u), i == 0);
            }
            // A restart reopens on the tab that was active.
            let a = crate::RESTORE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed).min(self.tabs.len().saturating_sub(1));
            self.active = a;
            self.switch_to(a);
        }
        self.redraw();
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, ev: DeviceEvent) {
        // Raw motion while the pointer is locked: movementX / movementY.
        if self.pointer_locked {
            if let DeviceEvent::MouseMotion { delta } = ev {
                if let Some(t) = self.tab() {
                    t.send(PageMsg::MouseDelta { dx: delta.0 as f32, dy: delta.1 as f32 });
                }
            }
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Page(id, pe) => self.on_page_event(id, pe),
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                for t in &self.tabs {
                    if let Some(d) = &t.devtools {
                        d.send(PageMsg::Shutdown);
                    }
                    t.send(PageMsg::Shutdown);
                }
                el.exit();
            }
            WindowEvent::Resized(sz) => {
                self.size = (sz.width.max(1), sz.height.max(1));
                self.page_cache = None;
                self.dev_cache = None;
                self.resize_tabs();
                self.clamp_scroll();
                self.redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.dpr = scale_factor as f32;
                self.page_cache = None;
                self.resize_tabs();
                self.redraw();
            }
            WindowEvent::RedrawRequested => {
                let animating = self.animate();
                if animating {
                    self.notify_scroll();
                }
                self.render();
                if animating {
                    self.redraw();
                }
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                let (x, y) = (position.x as f32 / self.dpr, position.y as f32 / self.dpr);
                self.mouse = (x, y);
                self.mouse_in = true;
                if self.hud {
                    let over = self.hud_rect().contains(x, y);
                    if over != self.hud_hover {
                        self.hud_hover = over;
                        self.redraw();
                    }
                }
                if let Some((y0, s0)) = self.sb_drag {
                    if let Some((track, thumb, range)) = self.scrollbar() {
                        let travel = (track.h - thumb.h - 4.0).max(1.0);
                        let ny = s0 + (y - y0) * range / travel;
                        if let Some(t) = self.tab_mut() {
                            t.target_y = ny;
                            t.scroll.1 = ny;
                        }
                        self.clamp_scroll();
                        self.notify_scroll();
                        self.redraw();
                    }
                    return;
                }
                if let Some((y0, h0)) = self.dev_drag {
                    if let Some(t) = self.tab_mut() {
                        t.dev_h = (h0 - (y - y0)).max(60.0);
                    }
                    self.page_cache = None;
                    self.dev_cache = None;
                    self.resize_tabs();
                    self.clamp_scroll();
                    self.redraw();
                    return;
                }
                if self.pressed_in_dev || self.hit(x, y) == Hot::Dev {
                    let (dx, dy) = self.to_dev(x, y);
                    self.dev_tab().into_iter().for_each(|t| t.send(PageMsg::MouseMove { x: dx, y: dy }));
                }
                if self.pressed_in_content && self.hit(x, y) != Hot::Content {
                    // Keep feeding the page during drag-selection outside the content area.
                    let (px, py) = self.to_page(x, y);
                    self.tab().into_iter().for_each(|t| t.send(PageMsg::MouseMove { x: px, y: py }));
                }
                let hot = self.hit(x, y);
                if hot != self.hot {
                    let was_content = self.hot == Hot::Content;
                    self.hot = hot;
                    if let Some(w) = &self.window {
                        let c = match hot {
                            Hot::Content => self.tab().map(|t| cursor_icon(t.cursor)).unwrap_or(CursorIcon::Default),
                            Hot::Dev => self.dev_tab().map(|t| cursor_icon(t.cursor)).unwrap_or(CursorIcon::Default),
                            Hot::DevSplit => CursorIcon::RowResize,
                            Hot::Omnibox | Hot::FindField => CursorIcon::Text,
                            Hot::GateAgree | Hot::GateSave | Hot::GateQuit => CursorIcon::Pointer,
                            Hot::GateContinue if self.gate.as_ref().map(|g| g.agree).unwrap_or(false) => CursorIcon::Pointer,
                            _ => CursorIcon::Default,
                        };
                        w.set_cursor(c);
                    }
                    if was_content && hot != Hot::Content {
                        self.tab().into_iter().for_each(|t| t.send(PageMsg::MouseLeave));
                    }
                    if self.hot != Hot::Dev && !self.pressed_in_dev {
                        self.dev_tab().into_iter().for_each(|t| t.send(PageMsg::MouseLeave));
                    }
                    self.redraw();
                }
                if hot == Hot::Content && self.ctx_menu.is_none() && !self.menu_open {
                    let (px, py) = self.to_page(x, y);
                    self.tab().into_iter().for_each(|t| t.send(PageMsg::MouseMove { x: px, y: py }));
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.mouse_in = false;
                self.hot = Hot::None;
                self.tab().into_iter().for_each(|t| t.send(PageMsg::MouseLeave));
                self.redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let (x, y) = self.mouse;
                let hot = self.hit(x, y);
                let b = match button {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    MouseButton::Back => {
                        if state == ElementState::Pressed {
                            self.tab().into_iter().for_each(|t| t.send(PageMsg::Back));
                        }
                        return;
                    }
                    MouseButton::Forward => {
                        if state == ElementState::Pressed {
                            self.tab().into_iter().for_each(|t| t.send(PageMsg::Forward));
                        }
                        return;
                    }
                    _ => return,
                };
                let overlay = self.menu_open || self.ctx_menu.is_some() || self.alert.is_some() || self.gate.is_some();
                if state == ElementState::Released && self.sb_drag.is_some() {
                    self.sb_drag = None;
                    self.redraw();
                    return;
                }
                if state == ElementState::Released && self.dev_drag.is_some() {
                    self.dev_drag = None;
                    self.redraw();
                    return;
                }
                if hot == Hot::DevSplit && !overlay && button == MouseButton::Left && state == ElementState::Pressed {
                    let h0 = self.dev_rect().map(|d| d.h).unwrap_or(300.0);
                    self.dev_drag = Some((y, h0));
                    return;
                }
                if (hot == Hot::Dev && !overlay) || (state == ElementState::Released && self.pressed_in_dev) {
                    let (dx, dy) = self.to_dev(x, y);
                    let mods = self.page_mods();
                    match state {
                        ElementState::Pressed => {
                            self.pressed_in_dev = true;
                            self.dev_focus = true;
                            if self.omni_focus || self.find_focus {
                                self.omni_focus = false;
                                self.find_focus = false;
                                self.suggestions.clear();
                                if let Some(u) = self.tab().map(|t| t.url.clone()) {
                                    self.omni.set(if u.starts_with("ohb://newtab") { "" } else { &u });
                                }
                            }
                            self.dev_tab().into_iter().for_each(|t| t.send(PageMsg::MouseDown { x: dx, y: dy, button: b, mods }));
                        }
                        ElementState::Released => {
                            self.pressed_in_dev = false;
                            self.dev_tab().into_iter().for_each(|t| t.send(PageMsg::MouseUp { x: dx, y: dy, button: b, mods }));
                        }
                    }
                    self.redraw();
                    return;
                }
                if hot == Hot::Scrollbar && !overlay && button == MouseButton::Left && state == ElementState::Pressed {
                    if let Some((_, thumb, _)) = self.scrollbar() {
                        let vh = self.page_viewport(self.tab().map(|t| t.zoom).unwrap_or(1.0)).1;
                        if y < thumb.y {
                            self.scroll_by(0.0, -vh * 0.9, true);
                        } else if y > thumb.bottom() {
                            self.scroll_by(0.0, vh * 0.9, true);
                        } else {
                            let s0 = self.tab().map(|t| t.scroll.1).unwrap_or(0.0);
                            if let Some(t) = self.tab_mut() {
                                t.target_y = s0;
                            }
                            self.sb_drag = Some((y, s0));
                        }
                    }
                    self.redraw();
                    return;
                }
                if hot == Hot::Content && !overlay {
                    let (px, py) = self.to_page(x, y);
                    let mods = self.page_mods();
                    match state {
                        ElementState::Pressed => {
                            self.pressed_in_content = true;
                            self.dev_focus = false;
                            if self.omni_focus || self.find_focus {
                                self.omni_focus = false;
                                self.find_focus = false;
                                self.suggestions.clear();
                                if let Some(u) = self.tab().map(|t| t.url.clone()) {
                                    self.omni.set(if u.starts_with("ohb://newtab") { "" } else { &u });
                                }
                                self.redraw();
                            }
                            if button == MouseButton::Right {
                                let link = self.tab().map(|t| t.status.clone()).filter(|s| !s.is_empty());
                                let mut items = vec![("Back".to_string(), 0u8), ("Forward".to_string(), 1), ("Reload".to_string(), 2)];
                                if link.is_some() {
                                    items.insert(0, ("Copy link address".to_string(), 4));
                                    items.insert(0, ("Open link in new tab".to_string(), 3));
                                }
                                items.push(("Copy".to_string(), 6));
                                items.push(("Select all".to_string(), 7));
                                items.push(("View page source".to_string(), 5));
                                items.push(("Inspect".to_string(), 8));
                                self.ctx_point = (px, py);
                                let (w, h) = self.css_size();
                                let mx = x.min(w - CTX_W - 4.0);
                                let my = y.min(h - (items.len() as f32 * 30.0 + 12.0));
                                self.ctx_menu = Some((mx, my, items, link));
                                self.redraw();
                            }
                            self.tab().into_iter().for_each(|t| t.send(PageMsg::MouseDown { x: px, y: py, button: b, mods }));
                        }
                        ElementState::Released => {
                            self.pressed_in_content = false;
                            self.tab().into_iter().for_each(|t| t.send(PageMsg::MouseUp { x: px, y: py, button: b, mods }));
                        }
                    }
                    return;
                }
                if state == ElementState::Pressed {
                    self.click(hot, x, y, button, el);
                } else if self.pressed_in_content {
                    self.pressed_in_content = false;
                    let (px, py) = self.to_page(x, y);
                    let mods = self.page_mods();
                    self.tab().into_iter().for_each(|t| t.send(PageMsg::MouseUp { x: px, y: py, button: b, mods }));
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (-x * 48.0, -y * 48.0),
                    MouseScrollDelta::PixelDelta(PhysicalPosition { x, y }) => (-x as f32 / self.dpr, -y as f32 / self.dpr),
                };
                if self.ctx_menu.is_some() || self.menu_open {
                    self.ctx_menu = None;
                    self.menu_open = false;
                    self.redraw();
                }
                if self.hot == Hot::Dev {
                    let (px, py) = self.to_dev(self.mouse.0, self.mouse.1);
                    self.dev_tab().into_iter().for_each(|t| t.send(PageMsg::Wheel { x: px, y: py, dx, dy }));
                } else if self.hot == Hot::Scrollbar {
                    let smooth = matches!(delta, MouseScrollDelta::LineDelta(..));
                    self.scroll_by(dx, dy, smooth);
                } else if self.hot == Hot::Content {
                    let (px, py) = self.to_page(self.mouse.0, self.mouse.1);
                    // The page handles the wheel when script may cancel it, or
                    // when it lands on an inner scroller or a frame; otherwise
                    // the window scrolls right away (passive listeners are
                    // told afterwards), so busy pages still scroll smoothly.
                    let (route, notify) = self
                        .tab()
                        .and_then(|t| {
                            t.frame.as_ref().map(|f| {
                                let over_scroller = f.scroll_regions.iter().any(|(r, fixed)| {
                                    let (qx, qy) = if *fixed { (px, py) } else { (px + t.scroll.0, py + t.scroll.1) };
                                    r.contains(qx, qy)
                                });
                                (f.wheel_listeners || f.has_frames || over_scroller, f.wheel_passive)
                            })
                        })
                        .unwrap_or((false, false));
                    if route {
                        self.tab().into_iter().for_each(|t| t.send(PageMsg::Wheel { x: px, y: py, dx, dy }));
                    } else {
                        let smooth = matches!(delta, MouseScrollDelta::LineDelta(..));
                        self.scroll_by(dx, dy, smooth);
                        if notify {
                            self.tab().into_iter().for_each(|t| t.send(PageMsg::WheelNotify { x: px, y: py, dx, dy }));
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.key(event, el),
            WindowEvent::Ime(Ime::Commit(text)) => {
                if self.gate.is_some() {
                    return;
                }
                if self.omni_focus {
                    self.omni.insert(&text);
                    self.update_suggestions();
                } else if let (true, Some(f)) = (self.find_focus, self.find.as_mut()) {
                    f.insert(&text);
                } else if self.dev_focus && self.dev_tab().is_some() {
                    self.dev_tab().into_iter().for_each(|t| t.send(PageMsg::Text(text.clone())));
                } else {
                    self.tab().into_iter().for_each(|t| t.send(PageMsg::Text(text.clone())));
                }
                self.redraw();
            }
            WindowEvent::Focused(true) => self.redraw(),
            WindowEvent::Focused(false) => {
                self.release_pointer_lock();
                if let Some(t) = self.tab() {
                    t.send(PageMsg::ReleaseKeys);
                }
            }
            _ => {}
        }
    }

    fn new_events(&mut self, _el: &ActiveEventLoop, cause: winit::event::StartCause) {
        // Timer wake-ups drive the loading spinner (~20 fps) without a busy redraw loop.
        if let winit::event::StartCause::ResumeTimeReached { .. } = cause {
            if self.tabs.iter().any(|t| t.loading) {
                self.redraw();
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let mut deadline: Option<Instant> = None;
        // Caret blink for chrome text fields.
        if self.omni_focus || self.find_focus {
            if self.last_blink.elapsed() >= Duration::from_millis(530) {
                self.caret_on = !self.caret_on;
                self.last_blink = Instant::now();
                self.redraw();
            }
            deadline = Some(self.last_blink + Duration::from_millis(530));
        }
        if self.gate.as_ref().map(|g| g.appear() < 1.0).unwrap_or(false) {
            self.redraw();
            let d = Instant::now() + Duration::from_millis(16);
            deadline = Some(deadline.map_or(d, |x| x.min(d)));
        }
        if self.tabs.iter().any(|t| t.loading) {
            let d = Instant::now() + Duration::from_millis(50);
            deadline = Some(deadline.map_or(d, |x| x.min(d)));
        }
        // Memory saver check every 10 s.
        if self.mem_check.elapsed() >= Duration::from_secs(10) {
            self.mem_check = Instant::now();
            // Tabs free memory this thread allocated (frames, events): it
            // returns to the OS only when this thread collects.
            crate::release_free_memory();
            if std::env::var_os("OHB_MEM_DEBUG").is_some() {
                crate::mem_debug_report();
            }
            if self.tabs.len() > 1 {
                self.sleep_tabs();
            }
        }
        let d = self.mem_check + Duration::from_secs(10);
        deadline = Some(deadline.map_or(d, |x| x.min(d)));
        el.set_control_flow(match deadline {
            Some(d) => ControlFlow::WaitUntil(d),
            None => ControlFlow::Wait,
        });
    }
}

/// Run the browser UI.
pub fn run(urls: Vec<String>, js: bool) {
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("OpenHeronBrowser: cannot start the windowing system: {e}");
            eprintln!("Tip: use `ohb --screenshot out.png URL` for headless rendering.");
            std::process::exit(1);
        }
    };
    let proxy = event_loop.create_proxy();
    let mut app = App::new(proxy, urls, js);
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("OpenHeronBrowser: event loop error: {e}");
    }
}

/// The performance overlay: frame rate, where the tab's thread spends its
/// time, the window's rasterization share and memory.
#[allow(clippy::too_many_arguments)]
fn draw_hud(ui: &mut Ui, x: f32, y: f32, perf: Option<&page::PerfSample>, fps: f32, raster_pct: f32, _rss: Option<u64>, rs: f32) {
    let w = HUD_W;
    let h = HUD_H;
    let panel = Rect::new(x, y, w, h);
    ui.shadow(panel, 12.0, 16.0, Color::rgba(0, 0, 0, 60));
    ui.rrect(panel, 12.0, Color::rgba(8, 8, 10, 236));
    ui.border(panel, 1.0, 12.0, Color::rgba(245, 200, 66, 90));
    let dim = Color::rgba(200, 204, 214, 255);
    let faint = Color::rgba(140, 144, 156, 255);
    let bright = Color::rgb(248, 248, 250);
    ui.text("OPENHERON · PERFORMANCE", x + 14.0, y + 10.0, 16.0, 10.0, 700, faint);
    let fps_s = format!("{:.0}", fps);
    let fw = ui.measure(&fps_s, 22.0, 700);
    ui.text(&fps_s, x + w - 46.0 - fw, y + 6.0, 26.0, 22.0, 700, bright);
    ui.text("fps", x + w - 40.0, y + 13.0, 16.0, 11.0, 600, dim);
    // Page thread phases (JavaScript includes the WebAssembly it calls).
    let phases: [(&str, Color); 6] = [
        ("Script", Color::rgb(245, 183, 72)),
        ("Style", Color::rgb(167, 139, 250)),
        ("Layout", Color::rgb(79, 179, 246)),
        ("Paint", Color::rgb(45, 212, 191)),
        ("GC", Color::rgb(244, 114, 182)),
        ("Idle", Color::rgb(71, 85, 105)),
    ];
    let mut wasm_pct = 0.0;
    let vals: [f64; 6] = match perf {
        Some(p) if p.wall > 0.0 => {
            let busy = [p.js, p.style, p.layout, p.paint, p.gc];
            let sum: f64 = busy.iter().map(|v| v.max(0.0)).sum();
            let scale = if sum > p.wall { p.wall / sum } else { 1.0 };
            let b: Vec<f64> = busy.iter().map(|v| v.max(0.0) * scale / p.wall * 100.0).collect();
            let idle = (100.0 - b.iter().sum::<f64>()).max(0.0);
            wasm_pct = (p.wasm.max(0.0) * scale / p.wall * 100.0).min(b[0]);
            [b[0], b[1], b[2], b[3], b[4], idle]
        }
        _ => [0.0, 0.0, 0.0, 0.0, 0.0, 100.0],
    };
    ui.text("Tab thread", x + 14.0, y + 34.0, 16.0, 11.0, 600, dim);
    if wasm_pct >= 0.5 {
        let ws = format!("Wasm {:.0}%", wasm_pct);
        let ww = ui.measure(&ws, 10.5, 600);
        ui.text(&ws, x + w - 14.0 - ww, y + 34.0, 16.0, 10.5, 600, Color::rgb(245, 183, 72));
    }
    let bar = Rect::new(x + 14.0, y + 54.0, w - 28.0, 10.0);
    ui.rrect(bar, 5.0, Color::rgba(255, 255, 255, 18));
    let mut bx = bar.x;
    for (i, (_, c)) in phases.iter().enumerate() {
        let bw = (vals[i] / 100.0) as f32 * bar.w;
        if bw >= 0.5 {
            ui.rect(Rect::new(bx, bar.y, bw, bar.h), *c);
            bx += bw;
        }
    }
    for (i, (name, c)) in phases.iter().enumerate() {
        let col = (i % 3) as f32;
        let row = (i / 3) as f32;
        let lx = x + 14.0 + col * 96.0;
        let ly = y + 72.0 + row * 20.0;
        ui.rrect(Rect::new(lx, ly + 5.0, 8.0, 8.0), 4.0, *c);
        ui.text(name, lx + 13.0, ly, 18.0, 11.0, 500, dim);
        let v = format!("{:.0}%", vals[i]);
        let vw = ui.measure(&v, 11.0, 700);
        ui.text(&v, lx + 86.0 - vw, ly, 18.0, 11.0, 700, bright);
    }
    // Window thread.
    let res = if rs > 0.75 { "full" } else if rs > 0.375 { "½" } else { "¼" };
    ui.text(&format!("Window raster {:.0}%   ·   render {res} resolution", raster_pct), x + 14.0, y + 118.0, 16.0, 11.0, 500, dim);
    // Memory: this tab's (its page thread's heap, its frames' and its
    // decoded images), not the whole browser's.
    let mb = |b: f64| if b >= 1024.0 * 1024.0 * 1024.0 { format!("{:.2} GB", b / 1073741824.0) } else { format!("{:.0} MB", b / 1048576.0) };
    ui.text("Memory · this tab", x + 14.0, y + 142.0, 18.0, 11.0, 600, dim);
    let total = perf.filter(|p| p.tab_mem > 0).map(|p| mb(p.tab_mem as f64)).unwrap_or_else(|| "…".into());
    let tw = ui.measure(&total, 15.0, 700);
    ui.text(&total, x + w - 14.0 - tw, y + 140.0, 20.0, 15.0, 700, bright);
    if let Some(p) = perf {
        let line = format!("JS {} · Wasm {} · img {} · {} nodes", mb(p.js_heap as f64), mb(p.wasm_mem as f64), mb(p.images as f64), p.dom_nodes);
        ui.text_ellipsis(&line, x + 14.0, y + 164.0, 16.0, w - 28.0, 10.5, 500, faint);
        let ft = format!("{} boxes · {} page frames · {:.0} ms sampled", p.boxes, p.frames, p.wall);
        ui.text(&ft, x + 14.0, y + 180.0, 14.0, 10.0, 500, faint);
    } else {
        ui.text("This tab's details appear once it reports.", x + 14.0, y + 164.0, 16.0, 10.5, 500, faint);
    }
}

const HUD_W: f32 = 306.0;
const HUD_H: f32 = 200.0;
