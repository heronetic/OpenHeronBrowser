// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! `<iframe>` rendering: each frame's document (from `srcdoc` or `src`) is
//! parsed, styled with its own sheets, laid out at the frame's size and
//! rasterized into an image the parent paints in the frame's content box.
//!
//! Frame documents are static: scripts in them do not run, and they have no
//! scrolling or input of their own. Same-origin documents scripted through
//! `contentDocument` live in the parent's arena and are not shown here.

use super::*;
use crate::layout::ResourceView;
use crate::paint::raster::Rasterizer;

/// One frame's document, its style sheets and its last rendering.
pub struct FrameView {
    /// What the frame shows: `srcdoc:` + markup, or the resolved `src` URL.
    pub key: String,
    pub doc: Option<Document>,
    /// Linked style sheets by URL (None while loading).
    pub sheets: HashMap<String, Option<Rc<Stylesheet>>>,
    /// Bumped whenever the document or a sheet changes.
    pub version: u64,
    /// (width, height, version, images ready) of the last rendering.
    rendered: Option<(u32, u32, u64, usize)>,
    /// A live frame (its own page) instead of a static rendering.
    pub remote: Option<RemoteFrame>,
    /// A local frame: a same-origin document in this page's arena, scripted
    /// in a realm of this thread (see `local` below).
    pub local: Option<LocalFrame>,
}

/// A same-origin frame whose document lives in the parent's arena and whose
/// scripts run on the parent's thread.
#[derive(Default)]
pub struct LocalFrame {
    /// The last rendering's styles, layout and hit regions (frame viewport
    /// coordinates), for geometry queries and input.
    pub styles: Option<Styles>,
    pub layout: Option<LayoutTree>,
    pub hits: Vec<crate::paint::HitRegion>,
    /// (document, its generation, size, scroll, images ready, sheets version) of the last rendering.
    rendered: Option<(NodeId, u64, (u32, u32), (i32, i32), usize, u64)>,
    /// Parsed `<style>` elements: node -> (text, sheet).
    style_cache: HashMap<NodeId, (String, Rc<Stylesheet>)>,
    /// The frame's current document node (set by script once it exists).
    pub doc: Option<NodeId>,
    /// The document's URL.
    pub url: String,
    /// The frame's viewport size (its content box), CSS px.
    pub size: (u32, u32),
    /// The frame document's viewport scroll position.
    pub scroll: (f32, f32),
    /// The size the frame's window last saw (for its `resize` event).
    pub reported: (u32, u32),
}

impl FrameView {
    fn empty() -> FrameView {
        FrameView { key: String::new(), doc: None, sheets: HashMap::new(), version: 0, rendered: None, remote: None, local: None }
    }
}

/// Resources as a frame document sees them: images by URL, no canvases or
/// nested frames (those belong to the parent document's node ids).
struct FrameRes<'a> {
    res: &'a Resources,
}

impl ResourceView for FrameRes<'_> {
    fn image_for(&self, doc: &Document, node: NodeId) -> Option<Arc<ImageData>> {
        ResView { res: self.res }.image_for(doc, node)
    }
    fn image_url(&self, url: &str) -> Option<Arc<ImageData>> {
        self.res.get(url)
    }
    fn svg_for(&self, doc: &Document, node: NodeId, w: f32, h: f32, styles: &Styles) -> Option<Arc<ImageData>> {
        ResView { res: self.res }.svg_for(doc, node, w, h, styles)
    }
    fn image_failed(&self, doc: &Document, node: NodeId) -> bool {
        ResView { res: self.res }.image_failed(doc, node)
    }
}

impl Page {
    /// Bring every visible `<iframe>`'s rendering up to date (after layout).
    pub(crate) fn update_frames(&mut self) {
        let Some(tree) = self.layout.as_ref() else { return };
        let mut wanted: Vec<(NodeId, u32, u32)> = Vec::new();
        for b in &tree.boxes {
            if !matches!(b.kind, layout::BoxKind::Replaced(layout::ReplacedKind::Frame)) {
                continue;
            }
            let Some(n) = b.node else { continue };
            if !self.doc.is_tag(n, "iframe") {
                continue;
            }
            let (w, h) = (b.content_w().round() as u32, b.content_h().round() as u32);
            if w <= 4096 && h <= 4096 {
                wanted.push((n, w, h));
            }
        }
        // Frames load whether or not they are rendered (display:none or zero
        // size, as with challenge widgets that show themselves later): live
        // ones run at a nominal size until they get a real one.
        if self.js_enabled {
            let mut all: Vec<NodeId> = self.doc.descendants(DOCUMENT).filter(|&e| self.doc.is_tag(e, "iframe")).collect();
            let roots: Vec<NodeId> = self.doc.shadow_roots.values().copied().collect();
            for r in roots {
                if self.doc.is_connected(r) {
                    all.extend(self.doc.descendants(r).filter(|&e| self.doc.is_tag(e, "iframe")));
                }
            }
            // Frames inside local frame documents (nested to any depth).
            let mut i = 0;
            while i < all.len() {
                let f = all[i];
                i += 1;
                if let Some(d) = self.frame_views.get(&f).and_then(|v| v.local.as_ref()).and_then(|l| l.doc) {
                    if self.doc.valid(d) && self.doc.is_document(d) && all.len() < 512 {
                        let inner: Vec<NodeId> = self.doc.descendants(d).filter(|&e| self.doc.is_tag(e, "iframe")).collect();
                        for e in inner {
                            if !all.contains(&e) {
                                all.push(e);
                            }
                        }
                    }
                }
            }
            for n in all {
                if !wanted.iter().any(|w| w.0 == n) {
                    let key = self.frame_key(n);
                    if self.local_frame_ok(n, &key) || self.live_frame_ok(&key) {
                        wanted.push((n, 0, 0));
                    }
                }
            }
        }
        // Re-navigate frames whose src was set again: drop their current view.
        for n in std::mem::take(&mut self.frame_reload) {
            // (A different URL navigates the usual way.)
            let key = self.frame_attr_key(n);
            if self.frame_views.get(&n).map(|f| f.key != key).unwrap_or(true) {
                continue;
            }
            if let Some(f) = self.frame_views.remove(&n) {
                if let Some(r) = &f.remote {
                    self.frames_loading.remove(&r.id);
                }
            }
            self.res.frames.remove(&n);
            self.frame_nav.remove(&n);
            self.frame_redirects.remove(&n);
        }
        self.res.frames.retain(|n, _| wanted.iter().any(|w| w.0 == *n));
        let before = self.frames_loading.len();
        let fl = &mut self.frames_loading;
        self.frame_views.retain(|n, f| {
            let keep = wanted.iter().any(|w| w.0 == *n);
            if !keep {
                if let Some(r) = &f.remote {
                    fl.remove(&r.id);
                }
                fl.remove(&(STATIC_FRAME | *n as u64));
            }
            keep
        });
        let fvs = &self.frame_views;
        self.local_loading.retain(|n| fvs.contains_key(n));
        self.frame_nav.retain(|n, _| fvs.contains_key(n));
        self.frame_redirects.retain(|n, _| fvs.contains_key(n));
        let released = before > 0 && self.frames_loading.is_empty();
        for (n, w, h) in wanted {
            let key = self.frame_key(n);
            if self.sync_local_frame(n, &key, w, h) {
                if w > 0 && h > 0 {
                    self.render_local_frame(n, false);
                }
                continue;
            }
            let key = self.frame_redirects.get(&n).filter(|r| r.0 == key).map(|r| r.1.clone()).unwrap_or(key);
            if self.sync_live_frame(n, &key, w.max(1), h.max(1)) {
                continue;
            }
            // Unrendered frames past the live-frame limit still load (for
            // their load event and contentDocument), they just don't paint.
            if (w == 0 || h == 0) && !(key.starts_with("http://") || key.starts_with("https://")) {
                continue;
            }
            let stale = self.frame_views.get(&n).map(|f| f.key != key || f.remote.is_some()).unwrap_or(true);
            if stale {
                self.res.frames.remove(&n);
                let mut fv = FrameView { key: key.clone(), ..FrameView::empty() };
                if let Some(markup) = key.strip_prefix("srcdoc:") {
                    let mut d = crate::html::parse_document(markup, self.doc.url.clone());
                    d.base_url = self.doc.base_url.clone();
                    fv.doc = Some(d);
                } else if key == "about:blank" {
                    fv.doc = Some(crate::html::parse_document("", Url::parse("about:blank").unwrap()));
                } else if let Ok(u) = Url::parse(&key) {
                    self.fetch_frame(n, key.clone(), u, true);
                }
                self.frame_views.insert(n, fv);
            }
            if w == 0 || h == 0 {
                continue;
            }
            self.frame_subresources(n);
            self.render_frame(n, w, h);
        }
        if released && self.loading {
            self.update_progress();
        }
        // Local frames whose viewport changed size get `resize` on their window.
        let mut resized: Vec<NodeId> = Vec::new();
        for (&n, fv) in self.frame_views.iter_mut() {
            if let Some(l) = fv.local.as_mut() {
                if l.size != l.reported && l.size.0 > 0 && l.size.1 > 0 {
                    if l.reported != (0, 0) && l.doc.is_some() {
                        resized.push(n);
                    }
                    l.reported = l.size;
                }
            }
        }
        for n in resized {
            self.js_tasks.push(JsTask::CallGlobal("__ohb_localFrameResize", vec![crate::js::host::HostValue::from(n as f64)]));
        }
    }

    /// What an iframe shows: its srcdoc, else its resolved src (about:blank
    /// by default); a local frame that script navigated shows that URL until
    /// the attributes change.
    fn frame_key(&self, n: NodeId) -> String {
        let key = self.frame_attr_key(n);
        if let Some((k, u)) = self.frame_nav.get(&n) {
            if *k == key {
                return u.clone();
            }
        }
        key
    }

    fn frame_attr_key(&self, n: NodeId) -> String {
        if let Some(s) = self.doc.attr(n, "srcdoc") {
            return format!("srcdoc:{s}");
        }
        let src = self.doc.attr(n, "src").map(|s| s.trim()).unwrap_or("");
        if src.is_empty() {
            return "about:blank".into();
        }
        let resolved = match self.frame_doc_url(n) {
            // In a local frame document: against that document's URL.
            Some(base) if base.scheme() != "about" => base.join(src).ok(),
            _ => self.doc.resolve_url(src),
        };
        match resolved {
            Some(u) if matches!(u.scheme(), "http" | "https" | "data") => u.to_string(),
            Some(u) if u.as_str() == "about:blank" || u.as_str().starts_with("about:blank#") || u.as_str().starts_with("about:blank?") => "about:blank".into(),
            // Local files only from local documents.
            Some(u) if u.scheme() == "file" && self.doc.url.scheme() == "file" => u.to_string(),
            _ => "about:blank".into(),
        }
    }

    fn fetch_frame(&mut self, n: NodeId, key: String, url: Url, document: bool) {
        if document && !self.load_fired {
            self.frames_loading.insert(STATIC_FRAME | n as u64);
        }
        if document && self.frame_views.get(&n).map(|f| f.local.is_some()).unwrap_or(false) {
            self.local_loading.insert(n);
        }
        let tx = self.tx.clone();
        let gen = self.nav_gen;
        let kind = if document { RequestKind::Document } else { RequestKind::Stylesheet };
        let target = url.to_string();
        // The referrer: who navigated the frame, else the document it is in.
        let referrer = match self.frame_referrer.take() {
            Some((f, r)) if f == n && document => r,
            other => {
                self.frame_referrer = other;
                self.frame_doc_url(n).filter(|u| u.scheme() != "about").unwrap_or_else(|| self.doc.url.clone())
            }
        };
        if std::env::var("OHB_FRAME_DEBUG").is_ok() { eprintln!("[frame] fetch {n} {url} referrer={referrer} doc={document} owner={} locals={:?}", self.doc.owner_doc(n), self.frame_views.values().filter_map(|f| f.local.as_ref()).map(|l| (l.doc, l.url.clone())).collect::<Vec<_>>()); }
        let mut req = Request::get(url, kind, Some(referrer));
        req.frame = document;
        if document {
            if let Some((f, body, ct)) = self.frame_post.take() {
                if f == n {
                    req.method = "POST".into();
                    req.body = Some(body);
                    if !ct.is_empty() {
                        req.headers.push(("Content-Type".into(), ct));
                    }
                } else {
                    self.frame_post = Some((f, body, ct));
                }
            }
        }
        net::fetch_async(req, move |r| {
            let msg = if document { NetMsg::Frame(n, key, r) } else { NetMsg::FrameSheet(n, key, target, r) };
            let _ = tx.send(PageMsg::Net(gen, msg));
        });
    }

    /// A frame document arrived.
    pub(crate) fn on_frame(&mut self, n: NodeId, key: String, res: Result<Response, NetError>) {
        // A local frame holds the load event until script finished loading it.
        let local = self.frame_views.get(&n).map(|f| f.local.is_some() && f.key == key).unwrap_or(false);
        let held = !local && self.frames_loading.remove(&(STATIC_FRAME | n as u64));
        self.on_frame_doc(n, key, res);
        if held && self.frames_loading.is_empty() && self.loading {
            self.update_progress();
        }
    }

    fn on_frame_doc(&mut self, n: NodeId, key: String, res: Result<Response, NetError>) {
        let Some(fv) = self.frame_views.get_mut(&n) else { return };
        if fv.key != key {
            return;
        }
        let ct = res.as_ref().map(|r| r.content_type()).unwrap_or_default();
        let xml = is_xml_type(&ct) || ct.trim().eq_ignore_ascii_case("image/svg+xml");
        let (html, charset) = match &res {
            Ok(r) if r.ok() => crate::html::encoding::decode_doc(&r.body, r.charset().as_deref(), !xml, xml),
            _ => (String::new(), "UTF-8"),
        };
        let url = res.as_ref().map(|r| r.url.clone()).ok().or_else(|| Url::parse(&key).ok()).unwrap_or_else(|| Url::parse("about:blank").unwrap());
        let same_origin = url.origin() == self.doc.url.origin() && url.origin().is_tuple();
        if fv.local.is_some() {
            if same_origin || res.is_err() {
                // The document goes into the arena, scripted on this thread.
                use crate::js::host::HostValue as H;
                let ct = if ct.is_empty() { "text/html".to_string() } else { ct.clone() };
                let args = vec![H::from(n as f64), H::from(url.as_str()), H::from(ct.as_str()), H::from(charset), H::String(html), H::from(xml)];
                self.js_tasks.push(JsTask::CallGlobal("__ohb_localFrameDoc", args));
            } else {
                // Redirected cross-origin: it loads live from where it ended up.
                self.frame_redirects.insert(n, (key.clone(), url.to_string()));
                self.frames_loading.remove(&(STATIC_FRAME | n as u64));
                self.local_loading.remove(&n);
                self.need_layout = true;
            }
            return;
        }
        if same_origin {
            use crate::js::host::HostValue as H;
            let ct = if ct.is_empty() { "text/html".to_string() } else { ct.clone() };
            let args = vec![H::from(n as f64), H::from(url.as_str()), H::from(ct.as_str()), H::from(charset), H::from(html.as_str()), H::from(xml)];
            self.js_tasks.push(JsTask::CallGlobal("__ohb_frameSnapshot", args));
        }
        fv.doc = Some(if xml { crate::html::xml::parse_document(&html, url).0 } else { crate::html::parse_document(&html, url) });
        fv.version += 1;
        self.need_paint = true;
        if std::env::var("OHB_FRAME_DEBUG").is_ok() { eprintln!("[frame] static load {n} {key}"); }
        self.js_tasks.push(JsTask::Dispatch { target: n, ty: "load".into(), init: EventInit::default(), then: DefaultAction::None });
    }

    /// A linked style sheet of a frame document arrived.
    pub(crate) fn on_frame_sheet(&mut self, n: NodeId, key: String, url: String, res: Result<Response, NetError>) {
        let Some(fv) = self.frame_views.get_mut(&n) else { return };
        if fv.key != key {
            return;
        }
        let text = match &res {
            Ok(r) if r.ok() => crate::html::encoding::decode(&r.body, r.charset().as_deref(), false),
            _ => String::new(),
        };
        let base = Url::parse(&url).unwrap_or_else(|_| Url::parse("about:blank").unwrap());
        fv.sheets.insert(url, Some(Rc::new(css::parse_stylesheet(&text, &base, css::Origin::Author))));
        fv.version += 1;
        self.need_paint = true;
    }

    /// The URL of the local frame document `n` belongs to (None in the main document).
    pub(crate) fn frame_doc_url(&self, n: NodeId) -> Option<Url> {
        let d = self.doc.owner_doc(n);
        if d == DOCUMENT {
            return None;
        }
        self.frame_views.values().filter_map(|f| f.local.as_ref()).find(|l| l.doc == Some(d)).and_then(|l| Url::parse(&l.url).ok())
    }

    /// Does this frame's document run locally (same origin, on this thread)?
    pub(crate) fn local_frame_ok(&self, n: NodeId, key: &str) -> bool {
        if !self.js_enabled || std::env::var("OHB_LOCAL_FRAMES").map(|v| v == "0").unwrap_or(false) {
            return false;
        }
        // A sandbox without allow-same-origin gives the frame an opaque origin.
        if let Some(sb) = self.doc.attr(n, "sandbox") {
            if !sb.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("allow-same-origin")) {
                return false;
            }
        }
        if self.frame_redirects.get(&n).map(|r| r.0 == key).unwrap_or(false) {
            return false;
        }
        self.local_url_ok(key)
    }

    /// Same origin as this page (or a document that inherits its origin).
    pub(crate) fn local_url_ok(&self, key: &str) -> bool {
        if key == "about:blank" || key.starts_with("srcdoc:") {
            return true;
        }
        match Url::parse(key) {
            Ok(u) => matches!(u.scheme(), "http" | "https") && u.origin().is_tuple() && u.origin() == self.doc.url.origin(),
            Err(_) => false,
        }
    }

    /// Keep a local frame in step with its element: navigate it when what it
    /// shows changes. Returns false when the frame is not local.
    fn sync_local_frame(&mut self, n: NodeId, key: &str, w: u32, h: u32) -> bool {
        if !self.local_frame_ok(n, key) {
            if let Some(fv) = self.frame_views.get_mut(&n) {
                if fv.local.is_some() {
                    // No longer local (sandboxed, or now cross-origin): script lets go of it.
                    fv.local = None;
                    fv.key = String::new();
                    self.js_tasks.push(JsTask::CallGlobal("__ohb_localFrameGone", vec![crate::js::host::HostValue::from(n as f64)]));
                }
            }
            return false;
        }
        let fv = self.frame_views.entry(n).or_insert_with(FrameView::empty);
        if fv.local.is_none() {
            fv.remote = None;
            fv.doc = None;
            fv.rendered = None;
            fv.key = String::new();
            fv.local = Some(LocalFrame::default());
            self.res.frames.remove(&n);
        }
        if w > 0 && h > 0 {
            if let Some(l) = fv.local.as_mut() {
                l.size = (w, h);
            }
        }
        if fv.key == key {
            return true;
        }
        let first = fv.key.is_empty();
        fv.key = key.to_string();
        self.start_local_nav(n, key, first);
        true
    }

    fn start_local_nav(&mut self, n: NodeId, key: &str, first: bool) {
        use crate::js::host::HostValue as H;
        let doc_args = |url: &str, markup: &str| vec![H::from(n as f64), H::from(url), H::from("text/html"), H::from("UTF-8"), H::from(markup), H::from(false)];
        if key == "about:blank" {
            // The initial about:blank document is made by script when the
            // frame is inserted; a later navigation to it gets a new one.
            if !first {
                self.hold_frame_load(n);
                self.js_tasks.push(JsTask::CallGlobal("__ohb_localFrameDoc", doc_args("about:blank", "")));
            } else {
                self.js_tasks.push(JsTask::CallGlobal("__ohb_localFrameInitial", vec![H::from(n as f64)]));
            }
        } else if let Some(markup) = key.strip_prefix("srcdoc:") {
            self.hold_frame_load(n);
            self.js_tasks.push(JsTask::CallGlobal("__ohb_localFrameDoc", doc_args("about:srcdoc", markup)));
        } else if let Ok(u) = Url::parse(key) {
            self.fetch_frame(n, key.to_string(), u, true);
        }
    }

    /// Are frames in document `d` (a local frame's) still loading? (Their
    /// documents' load events wait for them.) Starts frames not yet started.
    pub fn frames_pending_in(&mut self, d: NodeId) -> bool {
        if !self.doc.valid(d) {
            return false;
        }
        let inner: Vec<NodeId> = self.doc.descendants(d).filter(|&e| self.doc.is_tag(e, "iframe")).collect();
        if inner.is_empty() {
            return false;
        }
        if inner.iter().any(|f| !self.frame_views.contains_key(f)) {
            self.ensure_layout();
            self.update_frames();
        }
        inner.iter().any(|f| match self.frame_views.get(f) {
            Some(v) => match (&v.local, &v.remote) {
                (Some(_), _) => self.local_loading.contains(f),
                (None, Some(r)) => !r.loaded,
                _ => false,
            },
            None => false,
        })
    }

    /// The page's load event waits for this frame's.
    fn hold_frame_load(&mut self, n: NodeId) {
        self.local_loading.insert(n);
        if !self.load_fired {
            self.frames_loading.insert(STATIC_FRAME | n as u64);
        }
    }

    /// Script finished loading a local frame's document (its load event ran).
    pub fn local_frame_loaded(&mut self, n: NodeId) {
        self.local_loading.remove(&n);
        let held = self.frames_loading.remove(&(STATIC_FRAME | n as u64));
        if held && self.frames_loading.is_empty() && self.loading {
            self.update_progress();
        }
    }

    /// Script gave a local frame its document (and its URL).
    pub fn local_frame_doc(&mut self, n: NodeId, d: NodeId, url: String) {
        let g = self.doc.generation;
        if let Some(fv) = self.frame_views.get_mut(&n) {
            if let Some(l) = fv.local.as_mut() {
                if let Some(old) = l.doc.filter(|&o| o != d) {
                    self.doc.sub_gens.remove(&old);
                    l.scroll = (0.0, 0.0);
                    l.layout = None;
                    l.styles = None;
                    l.hits.clear();
                    l.style_cache.clear();
                    fv.sheets.clear();
                }
                l.doc = Some(d);
                l.url = url;
                l.rendered = None;
                self.doc.sub_gens.insert(d, g);
            }
        }
        self.need_paint = true;
    }

    /// Run `f` with the layout, styles, hit regions, scroll position and
    /// viewport of the local frame whose document `n` is in standing in for
    /// the page's (geometry queries from the frame's scripts); else as is.
    pub fn frame_ctx<R>(&mut self, n: NodeId, f: impl FnOnce(&mut Page) -> R) -> R {
        let Some(fr) = self.frame_of_node(n) else { return f(self) };
        if self.doc.render_root.get() != DOCUMENT {
            return f(self); // already in a frame's context
        }
        self.ensure_layout();
        // The frame's size follows its element's box now (not at the next frame).
        let (vw, vh, _, _) = self.frame_viewport(fr);
        // (A zero-size frame has a zero-size viewport for media queries.)
        if vw >= 0.0 && vh >= 0.0 {
            if let Some(l) = self.local_frame_mut(fr) {
                l.size = (vw.round() as u32, vh.round() as u32);
            }
        }
        self.render_local_frame(fr, false);
        let Some(mut lf) = self.frame_views.get_mut(&fr).and_then(|v| v.local.take()) else { return f(self) };
        let (Some(d), true) = (lf.doc, lf.layout.is_some()) else {
            if let Some(v) = self.frame_views.get_mut(&fr) {
                v.local = Some(lf);
            }
            return f(self);
        };
        let base = self.local_base(d, &lf.url);
        std::mem::swap(&mut self.layout, &mut lf.layout);
        std::mem::swap(&mut self.styles, &mut lf.styles);
        std::mem::swap(&mut self.hits, &mut lf.hits);
        std::mem::swap(&mut self.scroll, &mut lf.scroll);
        let saved_win = self.win;
        self.win = (lf.size.0 as f32 * self.zoom, lf.size.1 as f32 * self.zoom);
        let (ns, nl) = (self.need_style, self.need_layout);
        self.need_style = false;
        self.need_layout = false;
        let prev_root = self.doc.render_root.replace(d);
        *self.doc.render_base.borrow_mut() = Some(base);
        let pq = self.doc.quirks;
        self.doc.quirks = self.doc.sub_quirks.get(&d).copied().unwrap_or(false);
        let r = f(self);
        self.doc.quirks = pq;
        self.doc.render_root.set(prev_root);
        *self.doc.render_base.borrow_mut() = None;
        self.need_style |= ns;
        self.need_layout |= nl;
        self.win = saved_win;
        std::mem::swap(&mut self.layout, &mut lf.layout);
        std::mem::swap(&mut self.styles, &mut lf.styles);
        std::mem::swap(&mut self.hits, &mut lf.hits);
        std::mem::swap(&mut self.scroll, &mut lf.scroll);
        if let Some(v) = self.frame_views.get_mut(&fr) {
            v.local = Some(lf);
        }
        r
    }

    /// Is a local frame's context in effect (inside `frame_ctx`)?
    pub fn in_frame_ctx(&self) -> bool {
        self.doc.render_root.get() != DOCUMENT
    }

    /// The local frame whose document `n` is in (None in the main document).
    pub fn frame_of_node(&self, n: NodeId) -> Option<NodeId> {
        if self.doc.sub_gens.is_empty() || !self.doc.valid(n) {
            return None;
        }
        let d = self.doc.tree_document(n);
        if d == DOCUMENT || !self.doc.sub_gens.contains_key(&d) {
            return None;
        }
        self.local_frame_of_doc(d)
    }

    pub(crate) fn local_base_of(&self, d: NodeId, url: &str) -> Url {
        self.local_base(d, url)
    }

    /// The base URL of a local frame's document: its <base>, else its URL
    /// (about:blank and srcdoc documents use their parent's).
    fn local_base(&self, d: NodeId, url: &str) -> Url {
        let fallback = Url::parse(url).ok().filter(|u| u.scheme() != "about").unwrap_or_else(|| self.doc.current_base());
        let head = self.doc.element_children(d).next().and_then(|h| self.doc.find_child_tag(h, "head"));
        if let Some(head) = head {
            for e in self.doc.descendants(head) {
                if self.doc.is_tag(e, "base") {
                    if let Some(h) = self.doc.attr(e, "href") {
                        if let Ok(u) = fallback.join(h.trim()) {
                            return u;
                        }
                    }
                }
            }
        }
        fallback
    }

    /// Style, lay out and paint a local frame's document at `w`×`h` (when it
    /// changed since the last time), into the image the parent paints.
    pub(crate) fn render_local_frame(&mut self, n: NodeId, force: bool) {
        let Some(fv) = self.frame_views.get(&n) else { return };
        let Some(l) = fv.local.as_ref() else { return };
        let Some(d) = l.doc else { return };
        if !self.doc.valid(d) || !self.doc.is_document(d) {
            return;
        }
        let (w, h) = (l.size.0.max(1), l.size.1.max(1));
        let real_size = l.size;
        let gen = self.doc.sub_gens.get(&d).copied().unwrap_or(0);
        let base = self.local_base(d, &l.url);
        // Images and linked sheets of the document.
        let mut sheet_urls: Vec<(NodeId, Url)> = Vec::new();
        let mut images: Vec<(String, f32)> = Vec::new();
        let prev_root = self.doc.render_root.replace(d);
        *self.doc.render_base.borrow_mut() = Some(base.clone());
        for e in self.doc.descendants(d) {
            if self.doc.is_tag(e, "link") {
                let rel = self.doc.attr(e, "rel").unwrap_or("").to_ascii_lowercase();
                if rel.split_ascii_whitespace().any(|r| r == "stylesheet") {
                    if let Some(u) = self.doc.attr(e, "href").and_then(|h| base.join(h.trim()).ok()) {
                        sheet_urls.push((e, u));
                    }
                }
            } else if self.doc.is_tag(e, "img") || self.doc.is_tag(e, "input") {
                if let Some(p) = resources::img_url(&self.doc, e, self.dpr) {
                    images.push(p);
                }
            }
        }
        self.doc.render_root.set(prev_root);
        *self.doc.render_base.borrow_mut() = None;
        let key = fv.key.clone();
        let mut new_sheets = Vec::new();
        for (_, u) in &sheet_urls {
            if !fv.sheets.contains_key(u.as_str()) {
                new_sheets.push(u.clone());
            }
        }
        for u in new_sheets {
            if let Some(fv) = self.frame_views.get_mut(&n) {
                fv.sheets.insert(u.to_string(), None);
            }
            self.fetch_frame(n, key.clone(), u, false);
        }
        for (u, dd) in images {
            self.request_image(u, dd);
        }
        let Some(fv) = self.frame_views.get(&n) else { return };
        let Some(l) = fv.local.as_ref() else { return };
        let imgs = { let doc = &self.doc; self.res.images_ready(|x| x != n && doc.valid(x) && doc.tree_document(x) == d) };
        let state = (d, gen, (w, h), (l.scroll.0 as i32, l.scroll.1 as i32), imgs, fv.version);
        if !force && l.rendered == Some(state) {
            return;
        }
        let Some(mut fv) = self.frame_views.remove(&n) else { return };
        let mut lf = fv.local.take().unwrap_or_default();
        let prev_root = self.doc.render_root.replace(d);
        *self.doc.render_base.borrow_mut() = Some(base.clone());
        let prev_quirks = self.doc.quirks;
        self.doc.quirks = self.doc.sub_quirks.get(&d).copied().unwrap_or(false);
        // Sheets in document order: <style> text and loaded <link>s.
        let mut sheets = vec![css::ua_sheet()];
        if self.doc.quirks {
            sheets.push(css::quirks_sheet());
        }
        let mut seen_styles = HashSet::new();
        // (Media queries against the frame's viewport.)
        let mut menv = self.media_env();
        menv.width = real_size.0 as f32;
        menv.height = real_size.1 as f32;
        let media_ok = |doc: &crate::dom::Document, e: NodeId| match doc.attr(e, "media").map(str::trim).filter(|m| !m.is_empty()) {
            Some(m) => css::media_matches(&css::tokenizer::parse_cvs(m), &menv),
            None => true,
        };
        for e in self.doc.descendants(d) {
            if (self.doc.is_tag(e, "style") || self.doc.is_tag(e, "link")) && !media_ok(&self.doc, e) {
                continue;
            }
            if self.doc.is_tag(e, "style") {
                let text = self.style_text(e);
                seen_styles.insert(e);
                let sh = match lf.style_cache.get(&e) {
                    Some((t, sh)) if *t == text => sh.clone(),
                    _ => {
                        let sh = Rc::new(css::parse_stylesheet(&text, &base, css::Origin::Author));
                        lf.style_cache.insert(e, (text, sh.clone()));
                        sh
                    }
                };
                sheets.push(sh);
            } else if self.doc.is_tag(e, "link") {
                if let Some((_, u)) = sheet_urls.iter().find(|(x, _)| *x == e) {
                    if let Some(Some(sh)) = fv.sheets.get(u.as_str()) {
                        sheets.push(sh.clone());
                    }
                }
            }
        }
        lf.style_cache.retain(|k, _| seen_styles.contains(k));
        if let Some(list) = self.doc.adopted.get(&d).cloned() {
            for t in list {
                sheets.push(self.text_sheet(&t));
            }
        }
        let idx = RuleIndex::new(sheets);
        let mut env = self.media_env();
        env.width = real_size.0 as f32;
        env.height = real_size.1 as f32;
        let hover: Vec<NodeId> = self.hover_chain.iter().copied().filter(|&x| self.doc.tree_document(x) == d).collect();
        let active: Vec<NodeId> = self.active_chain.iter().copied().filter(|&x| self.doc.tree_document(x) == d).collect();
        let styles = css::StyleEngine {
            doc: &self.doc,
            index: &idx,
            env,
            hover_chain: hover,
            active_chain: active,
            target: None,
            inline_cache: &mut self.inline_cache,
            root: d,
            descend_hidden: false,
            seed: None,
            container_sizes: None,
            anim: None,
            prev: None,
            shadow_indexes: None,
            dirty: None,
        }
        .compute();
        // Web fonts the frame's document declares and uses.
        let faces = idx.font_faces();
        if !faces.is_empty() {
            let mut used: std::collections::HashSet<(String, u16, bool)> = std::collections::HashSet::new();
            for e in self.doc.descendants(d) {
                let Some(st) = styles.get(e) else { continue };
                let italic = st.font_style != css::style::FontStyle::Normal;
                for f in st.font_family.iter().take(3) {
                    used.insert((f.to_ascii_lowercase(), st.font_weight, italic));
                }
            }
            let mut to_load = Vec::new();
            for (fam, w, italic) in used {
                let cands: Vec<&css::FontFace> = faces.iter().filter(|f| f.family.eq_ignore_ascii_case(&fam)).collect();
                let Some(best) = cands.iter().min_by_key(|f| super::font_face_rank(f, w, italic)) else { continue };
                for ff in cands.iter().filter(|f| f.weight == best.weight && f.style == best.style) {
                    let key = super::font_face_key(ff);
                    if self.requested_fonts.insert(key) {
                        to_load.push((*ff).clone());
                    }
                }
            }
            for ff in to_load {
                self.load_font(ff);
            }
        }
        // CSS images (backgrounds, border images, masks, list markers) the
        // frame's document uses; requested below and re-rendered on arrival.
        let mut css_images: Vec<String> = Vec::new();
        {
            let mut push = |u: &str| {
                if let Ok(abs) = base.join(u.trim()) {
                    css_images.push(abs.to_string());
                }
            };
            for e in self.doc.descendants(d) {
                let Some(st) = styles.get(e) else { continue };
                if let Some(layers) = &st.background {
                    for l in layers.iter() {
                        if let css::style::BgImage::Url(u) = &l.image {
                            push(u);
                        }
                    }
                }
                if let Some(bi) = &st.border_image {
                    if let css::style::BgImage::Url(u) = &bi.source {
                        push(u);
                    }
                }
                if let Some(m) = &st.mask {
                    if let css::style::BgImage::Url(u) = &m.image {
                        push(u);
                    }
                }
                if let Some(u) = &st.list_style_image {
                    push(u);
                }
            }
            for (_, ps) in styles.before.iter().chain(styles.after.iter()) {
                if let Some(layers) = &ps.background {
                    for l in layers.iter() {
                        if let css::style::BgImage::Url(u) = &l.image {
                            push(u);
                        }
                    }
                }
            }
        }
        let view = ResView { res: &self.res };
        let tree = layout::layout_document(&self.doc, &styles, &mut self.fonts, &view, w as f32, h as f32);
        // Scroll position stays within the document.
        let (mx, my) = ((tree.doc_width - w as f32).max(0.0), (tree.doc_height - h as f32).max(0.0));
        lf.scroll = (lf.scroll.0.clamp(0.0, mx), lf.scroll.1.clamp(0.0, my));
        let focus = self.doc.focused.filter(|&f| self.doc.tree_document(f) == d);
        let mut inp = builder::PaintInput {
            tree: &tree,
            doc: &self.doc,
            styles: &styles,
            res: &view,
            scroll: &self.elem_scroll,
            focus,
            caret: None,
            caret_visible: false,
            fonts: &mut self.fonts,
            find: &[],
            selection: None,
            edit_caret: None,
            field_sel: None,
            select_popup: None,
        };
        let (list, hits) = builder::build_display_list(&mut inp);
        self.doc.quirks = prev_quirks;
        self.doc.render_root.set(prev_root);
        *self.doc.render_base.borrow_mut() = None;
        let scale = self.dpr.max(1.0);
        let (pw, ph) = (((w as f32) * scale).round() as u32, ((h as f32) * scale).round() as u32);
        if let Some(mut pm) = tiny_skia::Pixmap::new(pw.max(1), ph.max(1)) {
            let bg = list.bg;
            if bg.a > 0 {
                pm.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, bg.a));
            }
            Rasterizer::new().render(&list, &mut pm, lf.scroll, scale, 0.0, (0.0, ph as f32));
            let img = ImageData { pixmap: pm, width: pw, height: ph, density: scale, nat: Some((w as f32, h as f32)), svg_par: None };
            self.res.frames.insert(n, Arc::new(img));
        }
        lf.rendered = Some((d, gen, (w, h), (lf.scroll.0 as i32, lf.scroll.1 as i32), imgs, fv.version));
        css_images.sort();
        css_images.dedup();
        for u in css_images {
            if self.res.images.get(&u).is_none() {
                self.request_image(u, 1.0);
            }
        }
        lf.styles = Some(styles);
        lf.layout = Some(tree);
        lf.hits = hits;
        fv.local = Some(lf);
        self.frame_views.insert(n, fv);
        self.need_paint = true;
    }

    /// Script navigates a local frame (location.assign and friends).
    pub fn local_frame_navigate(&mut self, n: NodeId, url: &str, referrer: Option<Url>) -> bool {
        let attr = self.frame_attr_key(n);
        if self.frame_views.get(&n).and_then(|f| f.local.as_ref()).is_none() {
            // Not set up yet (no frame update since it was inserted): it is now.
            if !self.doc.is_tag(n, "iframe") || !self.local_frame_ok(n, &attr) {
                return false;
            }
            let fv = self.frame_views.entry(n).or_insert_with(FrameView::empty);
            fv.remote = None;
            fv.local = Some(LocalFrame::default());
            fv.key = attr.clone();
        }
        self.frame_referrer = referrer.map(|r| (n, r));
        self.frame_nav.insert(n, (attr, url.to_string()));
        self.frame_redirects.remove(&n);
        if !self.local_url_ok(url) {
            // Cross-origin: the frame goes live there (next frame update).
            self.need_layout = true;
            return true;
        }
        if let Some(fv) = self.frame_views.get_mut(&n) {
            fv.key = url.to_string();
        }
        self.start_local_nav(n, url, false);
        true
    }

    /// A frame's viewport: (width, height, scrollX, scrollY) in CSS px — its
    /// content box (from layout).
    pub fn frame_viewport(&mut self, n: NodeId) -> (f32, f32, f32, f32) {
        let (sx, sy) = self.local_frame(n).map(|l| l.scroll).unwrap_or((0.0, 0.0));
        if self.doc.owner_doc(n) == DOCUMENT && self.doc.is_connected(n) {
            self.ensure_layout();
            if let Some(tree) = self.layout.as_ref() {
                if let Some(&b) = tree.node_box.get(&n) {
                    let bx = &tree.boxes[b];
                    return (bx.content_w(), bx.content_h(), sx, sy);
                }
            }
            return (0.0, 0.0, sx, sy);
        }
        let (w, h) = self.local_frame(n).map(|l| (l.size.0 as f32, l.size.1 as f32)).unwrap_or((0.0, 0.0));
        (w, h, sx, sy)
    }

    /// The local frame showing document `d`.
    /// An image element's source URL, resolved against its own document's
    /// base (a same-origin frame's, for `new Image()` made by frame script).
    pub(crate) fn node_img_url(&self, n: NodeId) -> Option<(String, f32)> {
        if self.doc.render_root.get() == DOCUMENT && !self.doc.sub_gens.is_empty() && self.doc.valid(n) {
            let d = self.doc.owner_doc(n);
            if d != DOCUMENT {
                if let Some(lf) = self.local_frame_of_doc(d).and_then(|fr| self.frame_views.get(&fr)).and_then(|v| v.local.as_ref()) {
                    let base = self.local_base(d, &lf.url);
                    let old_root = self.doc.render_root.replace(d);
                    let old_base = self.doc.render_base.replace(Some(base));
                    let r = resources::img_url(&self.doc, n, self.dpr);
                    self.doc.render_root.set(old_root);
                    *self.doc.render_base.borrow_mut() = old_base;
                    return r;
                }
            }
        }
        resources::img_url(&self.doc, n, self.dpr)
    }

    pub fn local_frame_of_doc(&self, d: NodeId) -> Option<NodeId> {
        self.frame_views.iter().find(|(_, f)| f.local.as_ref().map(|l| l.doc == Some(d)).unwrap_or(false)).map(|(&n, _)| n)
    }

    pub fn local_frame(&self, n: NodeId) -> Option<&LocalFrame> {
        self.frame_views.get(&n).and_then(|f| f.local.as_ref())
    }

    pub fn local_frame_mut(&mut self, n: NodeId) -> Option<&mut LocalFrame> {
        self.frame_views.get_mut(&n).and_then(|f| f.local.as_mut())
    }

    /// Request a frame document's linked sheets and images.
    fn frame_subresources(&mut self, n: NodeId) {
        let mut sheets: Vec<Url> = Vec::new();
        let mut images: Vec<(String, f32)> = Vec::new();
        {
            let Some(fv) = self.frame_views.get(&n) else { return };
            let Some(d) = fv.doc.as_ref() else { return };
            for e in d.descendants(DOCUMENT) {
                if d.is_tag(e, "link") {
                    let rel = d.attr(e, "rel").unwrap_or("").to_ascii_lowercase();
                    if rel.split_ascii_whitespace().any(|r| r == "stylesheet") {
                        if let Some(u) = d.attr(e, "href").and_then(|h| d.resolve_url(h)) {
                            if !fv.sheets.contains_key(u.as_str()) {
                                sheets.push(u);
                            }
                        }
                    }
                } else if d.is_tag(e, "img") {
                    if let Some(p) = resources::img_url(d, e, self.dpr) {
                        images.push(p);
                    }
                }
            }
        }
        let key = self.frame_views.get(&n).map(|f| f.key.clone()).unwrap_or_default();
        for u in sheets {
            if let Some(fv) = self.frame_views.get_mut(&n) {
                fv.sheets.insert(u.to_string(), None);
            }
            self.fetch_frame(n, key.clone(), u, false);
        }
        for (u, d) in images {
            self.request_image(u, d);
        }
    }

    /// Style, lay out and rasterize a frame document at `w`×`h` CSS px.
    fn render_frame(&mut self, n: NodeId, w: u32, h: u32) {
        let Some(mut fv) = self.frame_views.remove(&n) else { return };
        let ready = |res: &Resources, fv: &FrameView| -> usize {
            let Some(d) = fv.doc.as_ref() else { return 0 };
            d.descendants(DOCUMENT).filter(|&e| d.is_tag(e, "img")).filter(|&e| ResView { res }.image_for(d, e).is_some()).count()
        };
        let imgs = ready(&self.res, &fv);
        let state = (w, h, fv.version, imgs);
        if fv.doc.is_none() || fv.rendered == Some(state) {
            self.frame_views.insert(n, fv);
            return;
        }
        let doc = fv.doc.as_ref().unwrap();
        // Sheets in document order: <style> text and loaded <link>s.
        let mut sheets = vec![css::ua_sheet()];
        let mut menv = self.media_env();
        menv.width = w as f32;
        menv.height = h as f32;
        for e in doc.descendants(DOCUMENT) {
            if let Some(m) = doc.attr(e, "media").map(str::trim).filter(|m| !m.is_empty()) {
                if (doc.is_tag(e, "style") || doc.is_tag(e, "link")) && !css::media_matches(&css::tokenizer::parse_cvs(m), &menv) {
                    continue;
                }
            }
            if doc.is_tag(e, "style") {
                sheets.push(Rc::new(css::parse_stylesheet(&doc.text_content(e), &doc.base_url, css::Origin::Author)));
            } else if doc.is_tag(e, "link") {
                if let Some(Some(s)) = doc.attr(e, "href").and_then(|h| doc.resolve_url(h)).and_then(|u| fv.sheets.get(u.as_str()).cloned()) {
                    sheets.push(s);
                }
            }
        }
        let idx = RuleIndex::new(sheets);
        let mut env = self.media_env();
        env.width = w as f32;
        env.height = h as f32;
        let mut cache = HashMap::new();
        let styles = css::StyleEngine {
            doc,
            index: &idx,
            env,
            hover_chain: Vec::new(),
            active_chain: Vec::new(),
            target: None,
            inline_cache: &mut cache,
            root: DOCUMENT,
            descend_hidden: false,
            seed: None,
            container_sizes: None,
            anim: None,
            prev: None,
            shadow_indexes: None,
            dirty: None,
        }
        .compute();
        // Frame node ids overlap the parent's: keep their SVG renderings apart.
        let saved_gen = self.res.doc_gen.get();
        self.res.doc_gen.set(u64::MAX - n as u64);
        let view = FrameRes { res: &self.res };
        let tree = layout::layout_document(doc, &styles, &mut self.fonts, &view, w as f32, h as f32);
        let scroll = HashMap::new();
        let mut inp = builder::PaintInput {
            tree: &tree,
            doc,
            styles: &styles,
            res: &view,
            scroll: &scroll,
            focus: None,
            caret: None,
            caret_visible: false,
            fonts: &mut self.fonts,
            find: &[],
            selection: None,
            edit_caret: None,
            field_sel: None,
            select_popup: None,
        };
        let (list, _) = builder::build_display_list(&mut inp);
        self.res.doc_gen.set(saved_gen);
        let scale = self.dpr.max(1.0);
        let (pw, ph) = (((w as f32) * scale).round() as u32, ((h as f32) * scale).round() as u32);
        if let Some(mut pm) = tiny_skia::Pixmap::new(pw.max(1), ph.max(1)) {
            let bg = list.bg;
            if bg.a > 0 {
                pm.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, bg.a));
            }
            Rasterizer::new().render(&list, &mut pm, (0.0, 0.0), scale, 0.0, (0.0, ph as f32));
            let img = ImageData { pixmap: pm, width: pw, height: ph, density: scale, nat: Some((w as f32, h as f32)), svg_par: None };
            self.res.frames.insert(n, Arc::new(img));
        }
        fv.rendered = Some(state);
        self.frame_views.insert(n, fv);
    }
}

// ------------------------------------------------------------- live frames
//
// An `<iframe>` with an http(s) document runs as a page of its own on its
// own thread (scripts, input, scrolling, nested frames). It rasterizes its
// viewport and sends the pixels here; the parent paints them in the
// frame's content box, forwards input that lands on the frame, and relays
// `postMessage` both ways.

/// Messages from a frame's page to the page embedding it.
pub enum ChildMsg {
    Pixels(Arc<ImageData>),
    Cursor(crate::css::style::Cursor),
    Status(String),
    OpenTab(String),
    /// A link or script asked to navigate the top-level page.
    NavigateTop(String),
    /// The frame finished loading; same-origin frames include a copy of
    /// their document for the parent's scriptable stand-in.
    Loaded(Option<Box<FrameSnapshot>>),
    Alert(String),
    Copy(String),
    /// `parent.postMessage` / `top.postMessage` / `parent.frames[x].postMessage`
    /// (with transferred ports).
    Message { data: crate::js::host::HostValue, origin: String, target_origin: String, to: MsgTarget, ports: Vec<u64> },
}

/// A same-origin frame's document as markup (HTML serialization, or the XML
/// source), for `iframe.contentDocument` in the parent.
pub struct FrameSnapshot {
    pub url: String,
    pub content_type: String,
    pub charset: &'static str,
    pub markup: String,
    pub xml: bool,
}

/// Where a frame's message goes.
#[derive(Clone, Debug)]
pub enum MsgTarget {
    Parent,
    Top,
    /// A sibling frame, by name or index in the parent.
    Sibling(String),
}

// ------------------------------------------------------------ message ports
//
// MessagePorts are entangled pairs that may be transferred to other pages
// (frames run on their own threads): a port's messages go to whichever page
// owns its peer now.

struct PortEntry {
    peer: u64,
    owner: u64,
}

fn ports() -> &'static Mutex<HashMap<u64, PortEntry>> {
    static P: std::sync::OnceLock<Mutex<HashMap<u64, PortEntry>>> = std::sync::OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

thread_local! {
    /// The page or worker this thread runs (owner of ports made here).
    static OWNER: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

pub fn set_thread_owner(id: u64) {
    OWNER.with(|o| o.set(id));
}

pub fn thread_owner() -> u64 {
    OWNER.with(|o| o.get())
}

/// A new entangled pair owned by page `owner`.
pub fn port_pair(owner: u64) -> (u64, u64) {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let a = N.fetch_add(2, std::sync::atomic::Ordering::Relaxed);
    let b = a + 1;
    let mut m = ports().lock().unwrap();
    m.insert(a, PortEntry { peer: b, owner });
    m.insert(b, PortEntry { peer: a, owner });
    (a, b)
}

/// The ports now belong to page `owner`.
pub fn port_transfer(pids: &[u64], owner: u64) {
    let mut m = ports().lock().unwrap();
    for p in pids {
        if let Some(e) = m.get_mut(p) {
            e.owner = owner;
        }
    }
}

/// Post on port `pid`: the message goes to its peer's owner.
pub fn port_post(pid: u64, data: crate::js::host::HostValue, transfer: Vec<u64>) {
    let target = {
        let m = ports().lock().unwrap();
        m.get(&pid).and_then(|e| m.get(&e.peer).map(|p| (e.peer, p.owner)))
    };
    if std::env::var_os("OHB_DEBUG_FRAMES").is_some() {
        let d = format!("{data:?}");
        let tail: String = d.chars().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect();
        eprintln!("[ports] post on {pid} -> {:?}: len={} ...{}", target, d.len(), tail);
    }
    if let Some((peer, owner)) = target {
        port_transfer(&transfer, owner);
        // A port of this same page (a MessageChannel used as a task
        // scheduler, as React's is): queue the message behind the tasks
        // already queued here, not behind input waiting in the channel, so
        // work a page scheduled runs before the next keystroke.
        if owner == thread_owner() {
            let queued = crate::page::with_page(|p| {
                p.js_tasks.push(crate::page::JsTask::PortMessage { pid: peer, data: data.clone(), ports: transfer.clone() });
            });
            if queued.is_some() {
                return;
            }
        }
        if !crate::js::worker::route_port(owner, peer, data.clone(), transfer.clone()) {
            devtools::route(owner, PageMsg::PortMessage { pid: peer, data, ports: transfer });
        }
    }
}

pub fn port_close(pid: u64) {
    let mut m = ports().lock().unwrap();
    if let Some(e) = m.remove(&pid) {
        m.remove(&e.peer);
    }
}

/// A frame page's link to its parent.
pub struct FrameParent {
    pub tx: Sender<PageMsg>,
    pub frame_id: u64,
    /// The parent page's id (for routing transferred message ports).
    pub parent_id: u64,
    /// The frame element's name (window.name).
    pub name: String,
    pub parent_url: Url,
    pub depth: u32,
    raster: RefCell<Rasterizer>,
    last_frame: RefCell<Option<Arc<FrameData>>>,
    scroll_req: std::cell::Cell<Option<(f32, f32)>>,
}

/// The parent's handle on a live frame.
pub struct RemoteFrame {
    pub id: u64,
    pub tx: Sender<PageMsg>,
    pub url: String,
    pub size: (u32, u32),
    /// Its document finished loading.
    pub loaded: bool,
}

impl Drop for RemoteFrame {
    fn drop(&mut self) {
        let _ = self.tx.send(PageMsg::Shutdown);
    }
}

fn next_frame_id() -> u64 {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1 << 40);
    N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Frame documents that run live: http(s), data: and srcdoc documents.
pub fn live_scheme(key: &str) -> bool {
    key.starts_with("https://") || key.starts_with("http://") || key.starts_with("srcdoc:") || key.starts_with("data:")
}

/// `frames_loading` key bit for a frame loading statically (by node id).
const STATIC_FRAME: u64 = 1 << 62;

/// Live frames per page (the rest render statically).
const MAX_LIVE_FRAMES: usize = 16;
/// Nesting depth for live frames.
const MAX_FRAME_DEPTH: u32 = 3;

impl FrameParent {
    fn new(tx: Sender<PageMsg>, frame_id: u64, parent_id: u64, parent_url: Url, depth: u32) -> FrameParent {
        FrameParent { tx, frame_id, parent_id, name: String::new(), parent_url, depth, raster: RefCell::new(Rasterizer::new()), last_frame: RefCell::new(None), scroll_req: std::cell::Cell::new(None) }
    }
}

/// Start a frame's page.
fn spawn_frame(id: u64, parent: (Sender<PageMsg>, u64, Url, u32), name: String, req: NavRequest, win: (f32, f32), dpr: f32) -> Option<Sender<PageMsg>> {
    let (tx, rx) = std::sync::mpsc::channel::<PageMsg>();
    let tx2 = tx.clone();
    devtools::register(id, tx.clone());
    let ui: UiSink = Arc::new(|_, _| {});
    let dbg = std::env::var_os("OHB_DEBUG_FRAMES").is_some();
    if dbg {
        eprintln!("[frames] spawn {id} {}", req.url.as_str().chars().take(120).collect::<String>());
    }
    let root = super::ROOT_TAB.with(|r| r.get());
    std::thread::Builder::new()
        .name(format!("heron-frame-{id}"))
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            super::ROOT_TAB.with(|r| r.set(root));
            let ptx = parent.0.clone();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let history = Arc::new(Mutex::new(Vec::new()));
                let mut fp = FrameParent::new(parent.0, id, parent.1, parent.2, parent.3);
                fp.name = name;
                tab_main_with(id, ui, tx2, rx, history, req, win, dpr, true, Some(fp));
            }));
            if r.is_err() {
                // Don't hold the parent's load event for a crashed frame.
                let _ = ptx.send(PageMsg::Child(id, ChildMsg::Loaded(None)));
            }
            if dbg {
                eprintln!("[frames] exit {id}");
            }
            PAGE.with(|p| {
                if let Ok(mut g) = p.try_borrow_mut() {
                    *g = None;
                }
            });
            devtools::unregister(id);
        })
        .ok()?;
    Some(tx)
}

impl Page {
    /// Should this frame run live? (http(s) documents with scripting on.)
    fn live_frame_ok(&self, key: &str) -> bool {
        self.js_enabled
            && live_scheme(key)
            && self.frame_parent.as_ref().map(|f| f.depth < MAX_FRAME_DEPTH).unwrap_or(true)
            && std::env::var_os("OHB_STATIC_FRAMES").is_none()
    }

    /// Keep a live frame in step with its element: start it, follow `src`
    /// changes and resize it. Returns false when it should render statically.
    pub(crate) fn sync_live_frame(&mut self, n: NodeId, key: &str, w: u32, h: u32) -> bool {
        if !self.live_frame_ok(key) {
            return false;
        }
        let live = self.frame_views.values().filter(|f| f.remote.is_some()).count();
        // A frame that started loading statically (past the live-frame limit) stays static.
        if self.frame_views.get(&n).map(|f| f.remote.is_none() && f.key == key).unwrap_or(false) {
            return false;
        }
        let fv = self.frame_views.entry(n).or_insert_with(FrameView::empty);
        if fv.local.is_some() {
            // A local frame that went cross-origin: it runs live now.
            fv.local = None;
            fv.key = String::new();
            self.js_tasks.push(JsTask::CallGlobal("__ohb_localFrameGone", vec![crate::js::host::HostValue::from(n as f64)]));
        }
        if let Some(r) = fv.remote.as_mut() {
            if r.url == key {
                if r.size != (w, h) {
                    r.size = (w, h);
                    let _ = r.tx.send(PageMsg::Resize { w: w as f32, h: h as f32, dpr: self.dpr });
                }
                return true;
            }
        } else if live >= MAX_LIVE_FRAMES {
            return false;
        }
        // An ad or tracking frame HeronBlock would block: no thread for it.
        if !key.starts_with("srcdoc:") {
            if let Ok(u) = Url::parse(key) {
                if crate::ext::blocks_subframe(&u, &self.doc.url) {
                    return false;
                }
            }
        }
        // New frame or a new src: (re)start it.
        if let Some(old) = fv.remote.take() {
            self.frames_loading.remove(&old.id);
        }
        fv.key = key.to_string();
        fv.doc = None;
        fv.rendered = None;
        self.res.frames.remove(&n);
        let id = next_frame_id();
        let fp = (self.tx.clone(), self.tab_id, self.doc.url.clone(), self.frame_parent.as_ref().map(|f| f.depth).unwrap_or(0) + 1);
        let mut req = match key.strip_prefix("srcdoc:") {
            // An iframe srcdoc document: about:srcdoc with the markup as its body.
            Some(markup) => {
                let mut r = NavRequest::get("about:srcdoc");
                r.body = Some(markup.as_bytes().to_vec());
                r
            }
            None => NavRequest::get(key),
        };
        req.referrer = Some(self.doc.url.clone());
        let name = self.doc.attr(n, "name").unwrap_or("").to_string();
        if let Some(tx) = spawn_frame(id, fp, name, req, (w as f32, h as f32), self.dpr) {
            if let Some(fv) = self.frame_views.get_mut(&n) {
                fv.remote = Some(RemoteFrame { id, tx, url: key.to_string(), size: (w, h), loaded: false });
            }
            if !self.load_fired {
                self.frames_loading.insert(id);
            }
        }
        true
    }

    /// This page's frames in document order (window.frames).
    fn frame_list(&self) -> Vec<NodeId> {
        self.doc.descendants(DOCUMENT).filter(|&e| self.doc.is_tag(e, "iframe") || self.doc.is_tag(e, "frame")).collect()
    }

    fn frame_index(&self, n: NodeId) -> Option<usize> {
        self.frame_list().iter().position(|&x| x == n)
    }

    /// `frames[name]` / `frames[i]`.
    pub fn frame_by_name(&self, name: &str) -> Option<NodeId> {
        let list = self.frame_list();
        if let Ok(i) = name.parse::<usize>() {
            return list.get(i).copied();
        }
        list.into_iter().find(|&e| self.doc.attr(e, "name") == Some(name))
    }

    fn live_frame_node(&self, id: u64) -> Option<NodeId> {
        self.frame_views.iter().find(|(_, f)| f.remote.as_ref().map(|r| r.id == id).unwrap_or(false)).map(|(&n, _)| n)
    }

    pub fn live_frame_id(&self, n: NodeId) -> Option<u64> {
        self.frame_views.get(&n).and_then(|f| f.remote.as_ref()).map(|r| r.id)
    }

    pub fn send_to_frame(&self, n: NodeId, m: PageMsg) -> bool {
        match self.frame_views.get(&n).and_then(|f| f.remote.as_ref()) {
            Some(r) => r.tx.send(m).is_ok(),
            None => false,
        }
    }

    /// A message from one of this page's live frames.
    pub(crate) fn on_child(&mut self, id: u64, m: ChildMsg) {
        let Some(n) = self.live_frame_node(id) else { return };
        match m {
            ChildMsg::Pixels(img) => {
                if std::env::var_os("OHB_FRAME_DEBUG").is_some() {
                    eprintln!("[frame] pixels {n} id={id} {}x{} @{:.3}", img.width, img.height, crate::net::wall_secs());
                }
                self.res.frames.insert(n, img);
                self.need_paint = true;
            }
            ChildMsg::Cursor(c) => {
                if self.hover_frame == Some(n) {
                    self.emit(PageEvent::Cursor(c));
                }
            }
            ChildMsg::Status(s) => self.emit(PageEvent::Status(s)),
            ChildMsg::OpenTab(u) => self.emit(PageEvent::OpenTab(u)),
            ChildMsg::NavigateTop(u) => {
                if let Some(fp) = &self.frame_parent {
                    let _ = fp.tx.send(PageMsg::Child(fp.frame_id, ChildMsg::NavigateTop(u)));
                } else {
                    let mut r = NavRequest::get(&u);
                    r.referrer = Some(self.doc.url.clone());
                    self.pending_nav = Some(r);
                }
            }
            ChildMsg::Loaded(snap) => {
                if let Some(r) = self.frame_views.get_mut(&n).and_then(|f| f.remote.as_mut()) {
                    r.loaded = true;
                }
                if std::env::var("OHB_FRAME_DEBUG").is_ok() { eprintln!("[frame] live load {n} id={id} snap={}", snap.as_ref().map(|s| s.url.to_string()).unwrap_or_default()); }
                let held = self.frames_loading.remove(&id);
                if let Some(sn) = snap {
                    use crate::js::host::HostValue as H;
                    let args = vec![H::from(n as f64), H::from(sn.url.as_str()), H::from(sn.content_type.as_str()), H::from(sn.charset), H::String(sn.markup), H::from(sn.xml)];
                    self.js_tasks.push(JsTask::CallGlobal("__ohb_frameSnapshot", args));
                }
                self.js_tasks.push(JsTask::Dispatch { target: n, ty: "load".into(), init: EventInit::default(), then: DefaultAction::None });
                if held && self.frames_loading.is_empty() && self.loading {
                    self.update_progress();
                }
            }
            ChildMsg::Alert(m) => self.emit(PageEvent::Alert(m)),
            ChildMsg::Copy(s) => self.emit(PageEvent::Copy(s)),
            ChildMsg::Message { data, origin, target_origin, to, ports } => {
                if std::env::var_os("OHB_DEBUG_FRAMES").is_some() {
                    eprintln!("[frames] message from frame {id} ({origin}) to {to:?} target={target_origin} ports={ports:?}: {}", format!("{data:?}").chars().take(300).collect::<String>());
                }
                match to {
                    MsgTarget::Top if self.frame_parent.is_some() => {
                        if let Some(fp) = &self.frame_parent {
                            port_transfer(&ports, fp.parent_id);
                            let _ = fp.tx.send(PageMsg::Child(fp.frame_id, ChildMsg::Message { data, origin, target_origin, to: MsgTarget::Top, ports }));
                        }
                    }
                    MsgTarget::Sibling(name) => {
                        // Deliver to the sibling frame, as coming from this one.
                        let sender_name = self.doc.attr(n, "name").unwrap_or("").to_string();
                        let target = self.frame_by_name(&name);
                        if let Some(t) = target {
                            if let Some(fid) = self.live_frame_id(t) {
                                port_transfer(&ports, fid);
                            }
                            let src = if sender_name.is_empty() { self.frame_index(n).map(|i| i.to_string()).unwrap_or_default() } else { sender_name };
                            self.send_to_frame(t, PageMsg::PostMessage { data, origin, target_origin, ports, source: Some(src) });
                        }
                    }
                    _ => self.js_tasks.push(JsTask::FrameMessage { frame: n, data, origin, target_origin, ports }),
                }
            }
        }
    }

    // ---- frame side

    /// Events of a frame page go to its parent instead of the UI.
    pub(crate) fn frame_emit(&self, fp: &FrameParent, ev: PageEvent) {
        let send = |m: ChildMsg| {
            let _ = fp.tx.send(PageMsg::Child(fp.frame_id, m));
        };
        match ev {
            PageEvent::Frame(f) => {
                *fp.last_frame.borrow_mut() = Some(f.clone());
                self.frame_raster(fp, &f);
            }
            PageEvent::ScrollTo(x, y) => {
                // Combine with a pending request: scroll-bys add up.
                let prev = fp.scroll_req.get();
                let next = match (prev, x.is_nan()) {
                    (Some((px, py)), true) if px.is_nan() => (f32::NAN, py + y),
                    (Some((px, py)), true) => (px, py + y),
                    _ => (x, y),
                };
                fp.scroll_req.set(Some(next));
            }
            PageEvent::Cursor(c) => send(ChildMsg::Cursor(c)),
            PageEvent::Status(s) => send(ChildMsg::Status(s)),
            PageEvent::OpenTab(u) => send(ChildMsg::OpenTab(u)),
            PageEvent::Alert(m) => send(ChildMsg::Alert(m)),
            PageEvent::Copy(s) => send(ChildMsg::Copy(s)),
            PageEvent::Loading(false, _) => send(ChildMsg::Loaded(self.frame_snapshot(fp))),
            _ => {}
        }
    }

    /// This frame's document for a same-origin parent.
    fn frame_snapshot(&self, fp: &FrameParent) -> Option<Box<FrameSnapshot>> {
        let srcdoc = self.doc.url.as_str() == "about:srcdoc";
        if !srcdoc && (fp.parent_url.origin() != self.doc.url.origin() || !fp.parent_url.origin().is_tuple()) {
            return None;
        }
        let markup = if self.doc.is_xml { self.doc_source.clone() } else { self.doc.inner_html(DOCUMENT) };
        Some(Box::new(FrameSnapshot { url: self.doc.url.to_string(), content_type: self.doc_content_type.clone(), charset: self.doc_charset, markup, xml: self.doc.is_xml }))
    }

    fn frame_raster(&self, fp: &FrameParent, f: &FrameData) {
        let (w, h) = self.viewport();
        let scale = self.dpr.max(1.0);
        let (pw, ph) = ((w * scale).round().max(1.0) as u32, (h * scale).round().max(1.0) as u32);
        if pw > 8192 || ph > 8192 {
            return;
        }
        let Some(mut pm) = tiny_skia::Pixmap::new(pw, ph) else { return };
        let bg = f.list.bg;
        if bg.a > 0 {
            pm.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, bg.a));
        }
        fp.raster.borrow_mut().render(&f.list, &mut pm, self.scroll, scale, 0.0, (0.0, ph as f32));
        let img = ImageData { pixmap: pm, width: pw, height: ph, density: scale, nat: Some((w, h)), svg_par: None };
        let _ = fp.tx.send(PageMsg::Child(fp.frame_id, ChildMsg::Pixels(Arc::new(img))));
    }

    /// Apply viewport scrolls a frame page asked for (it scrolls itself).
    pub(crate) fn frame_apply_scroll(&mut self) {
        let Some(fp) = self.frame_parent.as_ref() else { return };
        let Some((x, y)) = fp.scroll_req.take() else { return };
        let (vw, vh) = self.viewport();
        let last = fp.last_frame.borrow().clone();
        let (dw, dh) = last.as_ref().map(|f| (f.doc_w, f.doc_h)).unwrap_or((vw, vh));
        let (nx, ny) = if x.is_nan() { (self.scroll.0, self.scroll.1 + y) } else { (x, y) };
        let ns = (nx.clamp(0.0, (dw - vw).max(0.0)), ny.clamp(0.0, (dh - vh).max(0.0)));
        if ns != self.scroll {
            self.scroll = ns;
            self.js_tasks.push(JsTask::WindowEvent("scroll".into()));
            if let (Some(f), Some(fp)) = (last, self.frame_parent.as_ref()) {
                self.frame_raster(fp, &f);
            }
        }
    }

    /// Where a live frame's content box is, in viewport coordinates.
    fn live_frame_rect(&mut self, n: NodeId) -> Option<crate::paint::Rect> {
        let r = self.node_rect(n)?;
        let tree = self.layout.as_ref()?;
        let b = &tree.boxes[*tree.node_box.get(&n)?];
        let (sx, sy) = if b.fixed { (0.0, 0.0) } else { self.scroll };
        Some(crate::paint::Rect::new(r.x + b.border[3] + b.padding[3] - sx, r.y + b.border[0] + b.padding[0] - sy, b.content_w(), b.content_h()))
    }

    /// The local frame under a viewport point: (iframe, its document, the
    /// point in the frame's viewport).
    fn local_frame_at(&mut self, x: f32, y: f32) -> Option<(NodeId, NodeId, f32, f32)> {
        if !self.frame_views.values().any(|f| f.local.as_ref().map(|l| l.doc.is_some()).unwrap_or(false)) {
            return None;
        }
        let n = self.hit_test(x, y).map(|(n, _, _)| n)?;
        let d = self.frame_views.get(&n)?.local.as_ref()?.doc?;
        let r = self.live_frame_rect(n)?;
        if !r.contains(x, y) {
            return None;
        }
        Some((n, d, x - r.x, y - r.y))
    }

    /// Mouse input over a local frame (or while one holds the mouse) goes to
    /// its document. `kind`: 0 move, 1 down, 2 up.
    fn local_mouse(&mut self, kind: u8, x: f32, y: f32, button: u8, mods: Mods) -> bool {
        if self.in_frame_ctx() {
            return false;
        }
        let target = match self.local_capture {
            Some(n) => {
                let d = self.frame_views.get(&n).and_then(|f| f.local.as_ref()).and_then(|l| l.doc);
                match (d, self.live_frame_rect(n)) {
                    (Some(d), Some(r)) => Some((n, d, x - r.x, y - r.y)),
                    _ => None,
                }
            }
            None => self.local_frame_at(x, y),
        };
        let Some((n, d, lx, ly)) = target else {
            if kind == 2 {
                self.local_capture = None;
            }
            return false;
        };
        match kind {
            0 => self.frame_ctx(d, |p| p.on_mouse_move(lx, ly)),
            1 => {
                self.local_capture = Some(n);
                self.frame_ctx(d, |p| p.on_mouse_down(lx, ly, button, mods));
            }
            _ => {
                self.local_capture = None;
                self.frame_ctx(d, |p| p.on_mouse_up(lx, ly, button, mods));
            }
        }
        true
    }

    /// The live frame under a viewport point, with the point in its coordinates.
    fn live_frame_at(&mut self, x: f32, y: f32) -> Option<(NodeId, f32, f32)> {
        if !self.frame_views.values().any(|f| f.remote.is_some()) {
            return None;
        }
        let n = self.hit_test(x, y).map(|(n, _, _)| n)?;
        self.frame_views.get(&n)?.remote.as_ref()?;
        let r = self.live_frame_rect(n)?;
        if !r.contains(x, y) {
            return None;
        }
        Some((n, x - r.x, y - r.y))
    }

    /// Route a mouse event to a live frame when it lands on one (or one
    /// holds the mouse). `kind`: 0 move, 1 down, 2 up.
    pub(crate) fn frame_mouse(&mut self, kind: u8, x: f32, y: f32, button: u8, mods: Mods) -> bool {
        if self.local_mouse(kind, x, y, button, mods) {
            return true;
        }
        if let Some(n) = self.frame_capture {
            let r = self.live_frame_rect(n);
            if let Some(r) = r {
                let (lx, ly) = (x - r.x, y - r.y);
                let m = match kind {
                    0 => PageMsg::MouseMove { x: lx, y: ly },
                    1 => PageMsg::MouseDown { x: lx, y: ly, button, mods },
                    _ => PageMsg::MouseUp { x: lx, y: ly, button, mods },
                };
                self.send_to_frame(n, m);
            }
            if kind == 2 {
                self.frame_capture = None;
            }
            return true;
        }
        match self.live_frame_at(x, y) {
            Some((n, lx, ly)) => {
                if self.hover_frame != Some(n) {
                    if let Some(old) = self.hover_frame {
                        self.send_to_frame(old, PageMsg::MouseLeave);
                    }
                    self.hover_frame = Some(n);
                }
                let m = match kind {
                    0 => PageMsg::MouseMove { x: lx, y: ly },
                    1 => {
                        self.frame_capture = Some(n);
                        self.frame_focus = Some(n);
                        PageMsg::MouseDown { x: lx, y: ly, button, mods }
                    }
                    _ => PageMsg::MouseUp { x: lx, y: ly, button, mods },
                };
                self.send_to_frame(n, m);
                true
            }
            None => {
                if let Some(old) = self.hover_frame.take() {
                    self.send_to_frame(old, PageMsg::MouseLeave);
                }
                if kind == 1 {
                    self.frame_focus = None;
                }
                false
            }
        }
    }

    pub(crate) fn frame_wheel(&mut self, x: f32, y: f32, dx: f32, dy: f32) -> bool {
        if let Some((n, d, lx, ly)) = self.local_frame_at(x, y) {
            let _ = n;
            self.frame_ctx(d, |p| p.default_wheel(lx, ly, dx, dy));
            return true;
        }
        match self.live_frame_at(x, y) {
            Some((n, lx, ly)) => self.send_to_frame(n, PageMsg::Wheel { x: lx, y: ly, dx, dy }),
            None => false,
        }
    }

    /// Keyboard input goes to the frame that was clicked last.
    pub(crate) fn frame_input(&mut self, m: PageMsg) -> Result<(), PageMsg> {
        match self.frame_focus {
            Some(n) if self.frame_views.get(&n).and_then(|f| f.remote.as_ref()).is_some() => {
                self.send_to_frame(n, m);
                Ok(())
            }
            _ => Err(m),
        }
    }
}

impl Page {
    /// Whether the iframe already has a frame view (it loaded something).
    pub(crate) fn frame_views_has(&self, n: NodeId) -> bool {
        self.frame_views.contains_key(&n)
    }
}
