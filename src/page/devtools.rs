// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! DevTools plumbing.
//!
//! A DevTools pane is an ordinary page (`ohb://devtools?tab=N`) running in
//! its own thread. Its script talks to the inspected tab through
//! `ohbDevtools.send(method, params)`: the request travels as a
//! `PageMsg::Devtools` to the target tab's thread, which answers from its
//! DOM, styles and layout (here) or from its script engine (`__ohb_devtools`
//! in the prelude), and pushes events (console messages, picked elements)
//! back while attached.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};

use super::*;

pub enum DevMsg {
    /// To the inspected tab.
    Request { from: u64, id: u64, method: String, params: String },
    /// To the DevTools page: the answer to request `id`.
    Response { id: u64, result: String },
    /// To the DevTools page: something happened in the inspected tab.
    Event(String),
}

fn routes() -> &'static Mutex<HashMap<u64, Sender<PageMsg>>> {
    static R: OnceLock<Mutex<HashMap<u64, Sender<PageMsg>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Make a tab reachable for DevTools messages.
pub fn register(tab: u64, tx: Sender<PageMsg>) {
    routes().lock().unwrap().insert(tab, tx);
}

pub fn unregister(tab: u64) {
    routes().lock().unwrap().remove(&tab);
}

/// Send any message to a tab or frame page by id.
pub fn route(tab: u64, m: PageMsg) -> bool {
    let tx = routes().lock().unwrap().get(&tab).cloned();
    match tx {
        Some(tx) => tx.send(m).is_ok(),
        None => false,
    }
}

pub fn send(tab: u64, m: DevMsg) -> bool {
    let tx = routes().lock().unwrap().get(&tab).cloned();
    match tx {
        Some(tx) => tx.send(PageMsg::Devtools(m)).is_ok(),
        None => false,
    }
}

pub struct ConsoleMsg {
    pub level: String,
    pub text: String,
    /// Structured argument previews (JSON) when DevTools was attached.
    pub args: String,
    pub time: f64,
}

#[derive(Default)]
pub struct DevState {
    pub console: VecDeque<ConsoleMsg>,
    /// The DevTools page attached to this tab.
    pub client: Option<u64>,
    pub highlight: Option<NodeId>,
    pub picking: bool,
    /// Scripts and style sheets this document ran / loaded: (name, text).
    pub scripts: Vec<(String, Arc<str>)>,
    pub styles: Vec<(String, Arc<str>)>,
    /// For a DevTools page: the tab it inspects.
    pub target: Option<u64>,
    pub next_req: u64,
    /// The mouseup that ends a pick isn't the page's.
    pub swallow_up: bool,
}

fn now_ms() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

impl DevState {
    /// A new document: its own console, scripts and highlight.
    pub fn reset_document(&mut self) {
        self.console.clear();
        self.scripts.clear();
        self.styles.clear();
        self.highlight = None;
        self.picking = false;
    }
}

impl Page {
    /// A console message from the page (or the engine).
    pub fn console_add(&mut self, level: &str, text: String, args: String) {
        let time = now_ms();
        if self.dt.console.len() >= 2000 {
            self.dt.console.pop_front();
        }
        if let Some(c) = self.dt.client {
            let ev = json!({ "type": "console", "msg": { "level": level, "text": text, "args": args, "time": time } });
            send(c, DevMsg::Event(ev.to_string()));
        }
        self.dt.console.push_back(ConsoleMsg { level: level.to_string(), text, args, time });
    }

    pub fn note_script_source(&mut self, name: &str, text: &str) {
        if self.dt.scripts.len() >= 500 || text.is_empty() {
            return;
        }
        if self.dt.scripts.iter().any(|(n, t)| n == name && t.len() == text.len()) {
            return;
        }
        self.dt.scripts.push((name.to_string(), Arc::from(text)));
    }

    pub fn note_style_source(&mut self, name: &str, text: &str) {
        if self.dt.styles.len() >= 300 || text.is_empty() {
            return;
        }
        self.dt.styles.push((name.to_string(), Arc::from(text)));
    }

    fn dt_event(&self, ev: Value) {
        if let Some(c) = self.dt.client {
            send(c, DevMsg::Event(ev.to_string()));
        }
    }

    pub(crate) fn devtools_msg(&mut self, m: DevMsg) {
        match m {
            DevMsg::Request { from, id, method, params } => self.dt_request(from, id, &method, &params),
            DevMsg::Response { id, result } => {
                let msg = json!({ "kind": "response", "id": id, "result": serde_json::from_str::<Value>(&result).unwrap_or(Value::Null) });
                self.js_tasks.push(JsTask::DevtoolsDeliver(msg.to_string()));
            }
            DevMsg::Event(ev) => {
                let msg = json!({ "kind": "event", "event": serde_json::from_str::<Value>(&ev).unwrap_or(Value::Null) });
                self.js_tasks.push(JsTask::DevtoolsDeliver(msg.to_string()));
            }
        }
    }

    fn dt_request(&mut self, from: u64, id: u64, method: &str, params: &str) {
        let p: Value = serde_json::from_str(params).unwrap_or(Value::Null);
        let reply = |v: Value| {
            send(from, DevMsg::Response { id, result: v.to_string() });
        };
        let node_param = |p: &Value| p["node"].as_u64().map(|n| n as NodeId);
        match method {
            "attach" => {
                self.dt.client = Some(from);
                let console: Vec<Value> = self.dt.console.iter().map(|m| json!({ "level": m.level, "text": m.text, "args": m.args, "time": m.time })).collect();
                reply(json!({ "url": self.doc.url.as_str(), "title": self.display_title(), "console": console, "js": self.js_enabled }));
            }
            "detach" => {
                if self.dt.client == Some(from) {
                    self.dt.client = None;
                }
                self.dt.highlight = None;
                self.dt.picking = false;
                self.need_paint = true;
                reply(Value::Null);
            }
            "highlight" => {
                let n = node_param(&p);
                if self.dt.highlight != n {
                    self.dt.highlight = n;
                    self.need_paint = true;
                }
                reply(Value::Null);
            }
            "pick" => {
                self.dt.picking = p["on"].as_bool().unwrap_or(false);
                if !self.dt.picking {
                    self.dt.highlight = None;
                    self.need_paint = true;
                }
                reply(Value::Null);
            }
            "scrollIntoView" => {
                if let Some(n) = node_param(&p) {
                    if let Some(r) = self.node_rect(n) {
                        let (_, vh) = self.viewport();
                        if r.y < self.scroll.1 || r.bottom() > self.scroll.1 + vh {
                            let y = (r.y - vh / 3.0).max(0.0);
                            self.scroll = (self.scroll.0, y);
                            self.emit(PageEvent::ScrollTo(self.scroll.0, y));
                        }
                    }
                }
                reply(Value::Null);
            }
            "inspectAt" => {
                let x = p["x"].as_f64().unwrap_or(0.0) as f32;
                let y = p["y"].as_f64().unwrap_or(0.0) as f32;
                let n = self.hit_test(x, y).map(|(n, _, _)| self.element_of(n));
                reply(json!({ "node": n }));
            }
            "matchedRules" => {
                let n = node_param(&p).unwrap_or(0);
                reply(self.dt_matched_rules(n));
            }
            "sources" => {
                let mut list: Vec<Value> = self.dt.scripts.iter().enumerate().map(|(i, (n, t))| json!({ "id": format!("s{i}"), "name": n, "size": t.len(), "kind": "script" })).collect();
                list.extend(self.dt.styles.iter().enumerate().map(|(i, (n, t))| json!({ "id": format!("c{i}"), "name": n, "size": t.len(), "kind": "style" })));
                reply(json!({ "url": self.doc.url.as_str(), "sources": list }));
            }
            "source" => {
                let sid = p["id"].as_str().unwrap_or("");
                let text = if let Some(i) = sid.strip_prefix('s').and_then(|x| x.parse::<usize>().ok()) {
                    self.dt.scripts.get(i).map(|x| x.1.to_string())
                } else if let Some(i) = sid.strip_prefix('c').and_then(|x| x.parse::<usize>().ok()) {
                    self.dt.styles.get(i).map(|x| x.1.to_string())
                } else if sid == "document" {
                    Some(self.doc_source.clone())
                } else {
                    None
                };
                reply(json!({ "text": text }));
            }
            "network" => {
                let list: Vec<Value> = net::net_log(self.tab_id)
                    .into_iter()
                    .map(|e| json!({ "url": e.url, "method": e.method, "kind": e.kind, "status": e.status, "size": e.size, "mime": e.mime, "start": e.start, "ms": e.ms, "error": e.error, "cached": e.cached }))
                    .collect();
                reply(json!({ "requests": list, "blocked": crate::ext::blocked_count(self.tab_id) }));
            }
            _ => {
                if self.js_enabled {
                    self.js_tasks.push(JsTask::Devtools { from, id, method: method.to_string(), params: params.to_string() });
                } else {
                    reply(json!({ "error": "JavaScript is turned off in this tab" }));
                }
            }
        }
    }

    fn dt_matched_rules(&mut self, n: NodeId) -> Value {
        if !self.doc.is_element(n) {
            return json!({ "rules": [] });
        }
        self.ensure_style();
        let sheets = self.ordered_sheets();
        let env = self.media_env();
        let rules = css::matched_rules_debug(&sheets, &self.doc, n, &env, &self.hover_chain, self.doc.focused);
        let list: Vec<Value> = rules
            .into_iter()
            .rev()
            .map(|r| {
                let origin = match r.origin {
                    css::Origin::UserAgent => "user-agent",
                    css::Origin::User => "user",
                    css::Origin::Author => "author",
                };
                json!({ "selector": r.selector, "origin": origin, "source": r.source, "media": r.media, "decls": r.decls.iter().map(|(k, v, i)| json!([k, v, i])).collect::<Vec<_>>() })
            })
            .collect();
        let inline = self.doc.attr(n, "style").unwrap_or("").to_string();
        json!({ "rules": list, "inline": inline })
    }

    /// Hovered-element highlight (box model) for the display list.
    pub(crate) fn dt_overlay(&mut self) -> Vec<crate::paint::Item> {
        use crate::css::values::Color;
        use crate::paint::{Item, Rect};
        let Some(n) = self.dt.highlight else { return Vec::new() };
        if !self.doc.valid(n) || !self.doc.is_connected(n) {
            return Vec::new();
        }
        let Some(r) = self.node_rect(n) else { return Vec::new() };
        let (mut m, mut b, mut pd) = ([0.0f32; 4], [0.0f32; 4], [0.0f32; 4]);
        let mut fixed = false;
        if let Some(tree) = self.layout.as_ref() {
            if let Some(&bi) = tree.node_box.get(&n) {
                let bx = &tree.boxes[bi];
                if !matches!(bx.kind, layout::BoxKind::InlineBox | layout::BoxKind::Text(_)) {
                    m = bx.margin;
                    b = bx.border;
                    pd = bx.padding;
                }
                fixed = bx.fixed;
            }
        }
        let mut r = r;
        if fixed {
            r.x += self.scroll.0;
            r.y += self.scroll.1;
        }
        // [top, right, bottom, left]
        let outer = Rect::new(r.x - m[3], r.y - m[0], r.w + m[1] + m[3], r.h + m[0] + m[2]);
        let pad = Rect::new(r.x + b[3], r.y + b[0], (r.w - b[1] - b[3]).max(0.0), (r.h - b[0] - b[2]).max(0.0));
        let content = Rect::new(pad.x + pd[3], pad.y + pd[0], (pad.w - pd[1] - pd[3]).max(0.0), (pad.h - pd[0] - pd[2]).max(0.0));
        let mut items = Vec::new();
        // Each layer is drawn as a frame around the next one.
        let ring = |items: &mut Vec<Item>, o: Rect, i: Rect, c: Color| {
            items.push(Item::Rect { r: Rect::new(o.x, o.y, o.w, (i.y - o.y).max(0.0)), color: c });
            items.push(Item::Rect { r: Rect::new(o.x, i.bottom(), o.w, (o.bottom() - i.bottom()).max(0.0)), color: c });
            items.push(Item::Rect { r: Rect::new(o.x, i.y, (i.x - o.x).max(0.0), i.h), color: c });
            items.push(Item::Rect { r: Rect::new(i.right(), i.y, (o.right() - i.right()).max(0.0), i.h), color: c });
        };
        ring(&mut items, outer, r, Color::rgba(246, 178, 107, 102));
        ring(&mut items, r, pad, Color::rgba(255, 229, 153, 140));
        ring(&mut items, pad, content, Color::rgba(147, 196, 125, 140));
        items.push(Item::Rect { r: content, color: Color::rgba(111, 168, 220, 120) });
        // Label: tag#id.class  w × h
        let label = {
            let mut s = self.doc.tag(n).to_string();
            if let Some(id) = self.doc.attr(n, "id").filter(|x| !x.is_empty()) {
                s.push('#');
                s.push_str(id);
            }
            if let Some(cls) = self.doc.attr(n, "class") {
                for c in cls.split_whitespace().take(3) {
                    s.push('.');
                    s.push_str(c);
                }
            }
            s
        };
        let dims = format!("{} × {}", fmt_px(r.w), fmt_px(r.h));
        let family = std::rc::Rc::new(vec!["system-ui".to_string(), "sans-serif".to_string()]);
        let (_, vh) = self.viewport();
        let scroll = self.scroll;
        let mut ui = crate::shell::chrome::Ui::new(&mut self.fonts, family);
        let lw = ui.measure(&label, 12.0, 600) + ui.measure(&dims, 12.0, 400) + 24.0;
        let below = outer.bottom() + 6.0;
        let ly = if outer.y - 30.0 >= scroll.1 { outer.y - 30.0 } else if below + 24.0 <= scroll.1 + vh { below } else { outer.y + 4.0 };
        let lr = Rect::new(outer.x.max(scroll.0 + 2.0), ly, lw, 24.0);
        ui.shadow(lr, 4.0, 6.0, Color::rgba(0, 0, 0, 60));
        ui.rrect(lr, 4.0, Color::WHITE);
        let x = ui.text(&label, lr.x + 8.0, lr.y, lr.h, 12.0, 600, Color::rgb(136, 18, 128));
        ui.text(&dims, lr.x + 16.0 + x, lr.y, lr.h, 12.0, 400, Color::rgb(80, 80, 80));
        items.extend(ui.items);
        items
    }

    /// Element picking: hovering highlights, a click selects. Returns true
    /// when the event was consumed.
    pub(crate) fn dt_pick_event(&mut self, x: f32, y: f32, click: bool) -> bool {
        if !self.dt.picking {
            return false;
        }
        let node = self.hit_test(x, y).map(|(n, _, _)| self.element_of(n));
        if let Some(n) = node {
            if self.dt.highlight != Some(n) {
                self.dt.highlight = Some(n);
                self.need_paint = true;
            }
            if click {
                self.dt.picking = false;
                self.dt.swallow_up = true;
                self.dt_event(json!({ "type": "picked", "node": n }));
            } else {
                self.dt_event(json!({ "type": "hovered", "node": n }));
            }
        }
        true
    }
}

fn fmt_px(v: f32) -> String {
    let r = (v * 100.0).round() / 100.0;
    if r.fract() == 0.0 {
        format!("{}", r as i64)
    } else {
        format!("{r}")
    }
}
