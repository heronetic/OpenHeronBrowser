// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Image / SVG decoding and the per-page resource store.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::dom::{Document, NodeId};
use crate::layout::ResourceView;
use crate::paint::ImageData;

const MAX_IMAGE_DIM: u32 = 8192;
const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

pub enum ImgState {
    Loading,
    Ready(Arc<ImageData>),
    Failed,
    /// Loaded, but a document rather than an image (for <object>).
    Document,
}

/// An external SVG document referenced by `<use href="sprite.svg#id">`: its
/// source, with the byte range of each element that has an id.
pub struct SvgDoc {
    pub text: String,
    pub ids: HashMap<String, (usize, usize)>,
}

impl SvgDoc {
    pub fn parse(text: String) -> Option<SvgDoc> {
        let mut ids = HashMap::new();
        {
            let opt = roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() };
            let doc = roxmltree::Document::parse_with_options(&text, opt).ok()?;
            for n in doc.descendants().filter(|n| n.is_element()) {
                if let Some(id) = n.attribute("id") {
                    ids.entry(id.to_string()).or_insert((n.range().start, n.range().end));
                }
            }
        }
        Some(SvgDoc { text, ids })
    }
    pub fn element(&self, id: &str) -> Option<&str> {
        let &(a, b) = self.ids.get(id)?;
        self.text.get(a..b)
    }
}

pub enum SvgDocState {
    Loading,
    Ready(Arc<SvgDoc>),
    Failed,
}

pub struct Resources {
    pub images: HashMap<String, ImgState>,
    pub svg_docs: HashMap<String, SvgDocState>,
    pub svg_cache: RefCell<HashMap<(NodeId, u32, u32, u64, u32), Option<Arc<ImageData>>>>,
    pub dpr: f32,
    pub doc_gen: std::cell::Cell<u64>,
    pub bytes: usize,
    /// 2D canvas backing stores by <canvas> node.
    pub canvases: HashMap<NodeId, Arc<ImageData>>,
    /// Rendered iframe documents by <iframe> node.
    pub frames: HashMap<NodeId, Arc<ImageData>>,
}

impl Resources {
    pub fn new(dpr: f32) -> Self {
        Resources { images: HashMap::new(), svg_docs: HashMap::new(), svg_cache: RefCell::new(HashMap::new()), dpr, doc_gen: std::cell::Cell::new(0), bytes: 0, canvases: HashMap::new(), frames: HashMap::new() }
    }

    /// A signature of the loaded images, canvases and frame renderings (it
    /// changes when one of them does).
    pub fn images_ready(&self, mine: impl Fn(NodeId) -> bool) -> usize {
        let mut sig = self.images.values().filter(|s| matches!(s, ImgState::Ready(_))).count();
        for (n, c) in self.canvases.iter().chain(self.frames.iter()) {
            if mine(*n) {
                sig = sig.wrapping_mul(31).wrapping_add(Arc::as_ptr(c) as usize);
            }
        }
        sig
    }

    /// Bytes held by decoded images, canvases and frame renderings.
    pub fn image_bytes(&self) -> usize {
        let px = |i: &ImageData| i.pixmap.data().len();
        self.images.values().map(|s| if let ImgState::Ready(i) = s { px(i) } else { 0 }).sum::<usize>()
            + self.svg_cache.borrow().values().flatten().map(|i| px(i)).sum::<usize>()
            + self.canvases.values().map(|i| px(i)).sum::<usize>()
            + self.frames.values().map(|i| px(i)).sum::<usize>()
    }

    pub fn get(&self, url: &str) -> Option<Arc<ImageData>> {
        match self.images.get(url) {
            Some(ImgState::Ready(i)) => Some(i.clone()),
            _ => None,
        }
    }

    pub fn memory(&self) -> usize {
        self.images
            .values()
            .map(|s| match s {
                ImgState::Ready(i) => i.bytes(),
                _ => 0,
            })
            .sum()
    }
}

thread_local! {
    /// The media environment `<source media>` is matched against (set with
    /// each style pass).
    static PICTURE_ENV: std::cell::Cell<Option<crate::css::MediaEnv>> = const { std::cell::Cell::new(None) };
}

pub fn set_picture_env(env: crate::css::MediaEnv) {
    PICTURE_ENV.with(|e| e.set(Some(env)));
}

/// The `<source>` an `<img>` in a `<picture>` uses: the first with a
/// supported type and matching media.
pub fn picture_source(doc: &Document, img: NodeId) -> Option<NodeId> {
    let p = doc.parent_element(img)?;
    if !doc.is_tag(p, "picture") {
        return None;
    }
    for s in doc.element_children(p) {
        if s == img {
            return None;
        }
        if !doc.is_tag(s, "source") || doc.attr(s, "srcset").map(|v| v.trim().is_empty()).unwrap_or(true) {
            continue;
        }
        let ty = doc.attr(s, "type").unwrap_or("").trim().to_ascii_lowercase();
        if !ty.is_empty() && !matches!(ty.as_str(), "image/webp" | "image/png" | "image/jpeg" | "image/gif" | "image/svg+xml" | "image/avif" | "image/bmp" | "image/x-icon" | "image/vnd.microsoft.icon" | "image/apng") {
            continue;
        }
        if let Some(m) = doc.attr(s, "media").map(str::trim).filter(|m| !m.is_empty()) {
            let ok = PICTURE_ENV.with(|e| e.get()).map(|env| crate::css::media_matches(&crate::css::tokenizer::parse_cvs(m), &env)).unwrap_or(false);
            if !ok {
                continue;
            }
        }
        return Some(s);
    }
    None
}

/// One image candidate of a parsed `srcset`.
#[derive(Clone, Debug)]
pub struct SrcCand {
    pub url: String,
    pub w: Option<u32>,
    pub x: Option<f32>,
}

fn is_valid_float(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && b[i] == b'-' {
        i += 1;
    }
    let int_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let has_int = i > int_start;
    let mut has_frac = false;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let fs = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == fs {
            return false;
        }
        has_frac = true;
    }
    if !has_int && !has_frac {
        return false;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        i += 1;
        if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
            i += 1;
        }
        let es = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == es {
            return false;
        }
    }
    i == b.len()
}

/// HTML "parse a srcset attribute" (candidates with invalid descriptors are dropped).
pub fn parse_srcset_spec(input: &str) -> Vec<SrcCand> {
    let c: Vec<char> = input.chars().collect();
    let ws = |ch: char| matches!(ch, ' ' | '\t' | '\n' | '\x0c' | '\r');
    let mut pos = 0;
    let mut out = Vec::new();
    loop {
        while pos < c.len() && (ws(c[pos]) || c[pos] == ',') {
            pos += 1;
        }
        if pos >= c.len() {
            return out;
        }
        let start = pos;
        while pos < c.len() && !ws(c[pos]) {
            pos += 1;
        }
        let mut url: String = c[start..pos].iter().collect();
        let mut descriptors: Vec<String> = Vec::new();
        if url.ends_with(',') {
            while url.ends_with(',') {
                url.pop();
            }
        } else {
            while pos < c.len() && ws(c[pos]) {
                pos += 1;
            }
            let mut cur = String::new();
            // 0 = in descriptor, 1 = in parens, 2 = after descriptor
            let mut state = 0;
            loop {
                let ch = c.get(pos).copied();
                match state {
                    0 => match ch {
                        Some(x) if ws(x) => {
                            if !cur.is_empty() {
                                descriptors.push(std::mem::take(&mut cur));
                                state = 2;
                            }
                        }
                        Some(',') => {
                            pos += 1;
                            if !cur.is_empty() {
                                descriptors.push(std::mem::take(&mut cur));
                            }
                            break;
                        }
                        Some('(') => {
                            cur.push('(');
                            state = 1;
                        }
                        None => {
                            if !cur.is_empty() {
                                descriptors.push(std::mem::take(&mut cur));
                            }
                            break;
                        }
                        Some(x) => cur.push(x),
                    },
                    1 => match ch {
                        Some(')') => {
                            cur.push(')');
                            state = 0;
                        }
                        None => {
                            descriptors.push(std::mem::take(&mut cur));
                            break;
                        }
                        Some(x) => cur.push(x),
                    },
                    _ => match ch {
                        Some(x) if ws(x) => {}
                        None => break,
                        Some(_) => {
                            state = 0;
                            continue;
                        }
                    },
                }
                pos += 1;
            }
        }
        // The descriptor parser.
        let (mut error, mut w, mut x, mut h) = (false, None::<u32>, None::<f32>, None::<u32>);
        for d in &descriptors {
            let (num, last) = d.split_at(d.len() - d.chars().last().map(|l| l.len_utf8()).unwrap_or(0));
            let digits = !num.is_empty() && num.bytes().all(|b| b.is_ascii_digit());
            match last {
                "w" if digits => {
                    let v: u64 = num.parse().unwrap_or(u64::MAX);
                    if w.is_some() || x.is_some() || v == 0 {
                        error = true;
                    } else {
                        w = Some(v.min(u32::MAX as u64) as u32);
                    }
                }
                "x" if is_valid_float(num) => {
                    let v: f64 = num.parse().unwrap_or(f64::NAN);
                    if w.is_some() || x.is_some() || h.is_some() || !(v >= 0.0) {
                        error = true;
                    } else {
                        x = Some(v as f32);
                    }
                }
                "h" if digits => {
                    let v: u64 = num.parse().unwrap_or(u64::MAX);
                    if h.is_some() || x.is_some() || v == 0 {
                        error = true;
                    } else {
                        h = Some(v.min(u32::MAX as u64) as u32);
                    }
                }
                _ => error = true,
            }
        }
        if h.is_some() && w.is_none() {
            error = true;
        }
        if !error && !url.is_empty() {
            out.push(SrcCand { url, w, x });
        }
    }
}

/// Whether an image type in `<source type>` is supported.
fn image_type_supported(ty: &str) -> bool {
    let t = ty.trim().to_ascii_lowercase();
    let t = t.split(';').next().unwrap_or("").trim();
    t.is_empty() || matches!(t, "image/webp" | "image/png" | "image/jpeg" | "image/jpg" | "image/pjpeg" | "image/gif" | "image/svg+xml" | "image/avif" | "image/bmp" | "image/x-icon" | "image/vnd.microsoft.icon" | "image/apng" | "image/x-png")
}

/// HTML "update the source set" + "select an image source" for an `<img>`
/// (picture `<source>`s with media/type/sizes, srcset densities and widths,
/// `src` as the 1x fallback). Returns the chosen URL (unresolved) and density.
/// `auto_w` is the image's rendered width, for `sizes=auto` on lazy images.
pub fn select_image_source(doc: &Document, img: NodeId, env: &crate::css::MediaEnv, auto_w: Option<f32>) -> Option<(String, f32)> {
    let el = doc.element(img)?;
    let lazy = el.attr("loading").is_some_and(|l| l.trim().eq_ignore_ascii_case("lazy"));
    // "img allows auto-sizes": lazy, with a sizes attribute starting with auto.
    let allows_auto = lazy
        && el.attr("sizes").is_some_and(|s| {
            let t = s.trim_start_matches(|c: char| c.is_ascii_whitespace()).to_ascii_lowercase();
            t.strip_prefix("auto").is_some_and(|r| r.trim_start().is_empty() || r.trim_start().starts_with(','))
        });
    let mut chosen: Option<(Vec<SrcCand>, Option<String>)> = None;
    if let Some(p) = doc.parent_element(img).filter(|&p| doc.is_tag(p, "picture")) {
        for s in doc.element_children(p) {
            if s == img {
                break;
            }
            if !doc.is_tag(s, "source") {
                continue;
            }
            let Some(ss) = doc.attr(s, "srcset") else { continue };
            let set = parse_srcset_spec(ss);
            if set.is_empty() {
                continue;
            }
            if let Some(m) = doc.attr(s, "media") {
                if !crate::css::media_matches(&crate::css::tokenizer::parse_cvs(m), env) {
                    continue;
                }
            }
            if !image_type_supported(doc.attr(s, "type").unwrap_or("")) {
                continue;
            }
            // A source without sizes uses the image's auto-sizes.
            let sizes = doc.attr(s, "sizes").map(str::to_string).or_else(|| if allows_auto { el.attr("sizes").map(str::to_string) } else { None });
            chosen = Some((set, sizes));
            break;
        }
    }
    let (mut set, sizes) = match chosen {
        Some(c) => c,
        None => {
            let mut set = el.attr("srcset").map(parse_srcset_spec).unwrap_or_default();
            if let Some(src) = el.attr("src").filter(|s| !s.is_empty()) {
                if !set.iter().any(|c| c.w.is_some()) && !set.iter().any(|c| c.x == Some(1.0) || (c.w.is_none() && c.x.is_none())) {
                    set.push(SrcCand { url: src.to_string(), w: None, x: Some(1.0) });
                }
            }
            (set, el.attr("sizes").map(str::to_string))
        }
    };
    if set.is_empty() {
        return None;
    }
    let auto = if allows_auto { auto_w } else { None };
    let source_size = || crate::css::sizes_px(sizes.as_deref().unwrap_or(""), env, auto);
    let has_w = set.iter().any(|c| c.w.is_some());
    let size = if has_w { source_size() } else { 0.0 };
    let mut dens: Vec<(usize, f32)> = set
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let d = match (c.w, c.x) {
                (Some(w), _) => {
                    if size > 0.0 {
                        w as f32 / size
                    } else {
                        f32::INFINITY
                    }
                }
                (None, Some(x)) => x,
                _ => 1.0,
            };
            (i, d)
        })
        .collect();
    // Chrome: sort by density, drop duplicates, take the first at least the device ratio.
    dens.sort_by(|a, b| a.1.total_cmp(&b.1));
    dens.dedup_by(|b, a| a.1 == b.1);
    let dpr = env.dpr.max(0.01);
    let pick = dens.iter().find(|d| d.1 >= dpr).or(dens.last()).copied()?;
    let c = set.swap_remove(pick.0);
    Some((c.url, if pick.1.is_finite() && pick.1 > 0.0 { pick.1 } else { 1.0 }))
}

/// Choose the image URL for an <img> (srcset-aware) relative to the document.
pub fn img_url(doc: &Document, node: NodeId, dpr: f32) -> Option<(String, f32)> {
    let el = doc.element(node)?;
    match el.name.as_str() {
        "object" => return el.attr("data").filter(|d| !d.trim().is_empty()).and_then(|d| doc.resolve_url(d)).map(|u| (u.to_string(), 1.0)),
        "embed" => return el.attr("src").filter(|d| !d.trim().is_empty()).and_then(|d| doc.resolve_url(d)).map(|u| (u.to_string(), 1.0)),
        _ => {}
    }
    // <picture> sources and srcset: the HTML source selection (sizes, media, densities).
    if el.attr("srcset").is_some() || picture_source(doc, node).is_some() {
        let mut env = PICTURE_ENV.with(|e| e.get()).unwrap_or(crate::css::MediaEnv { width: 1280.0, height: 800.0, dpr, dark: false, dev_w: 1280.0, dev_h: 800.0 });
        env.dpr = dpr;
        if let Some((u, d)) = select_image_source(doc, node, &env, None) {
            if let Some(x) = doc.resolve_url(&u) {
                return Some((x.to_string(), d));
            }
        }
    }
    if let Some(ss) = el.attr("data-srcset").filter(|_| el.attr("srcset").is_none()) {
        let slot = el.attr("width").and_then(|w| w.trim().trim_end_matches("px").parse::<f32>().ok());
        if let Some((u, d)) = pick_srcset_with(ss, dpr, el.attr("src"), slot) {
            if let Some(x) = doc.resolve_url(&u) {
                return Some((x.to_string(), d));
            }
        }
    }
    let src = el
        .attr("src")
        .filter(|s| !s.trim().is_empty() && !s.starts_with("data:image/gif;base64,R0lGOD") && !s.starts_with("data:image/svg+xml,%3Csvg%20xmlns"))
        .or_else(|| el.attr("data-src"))
        .or_else(|| el.attr("data-lazy-src"))
        .or_else(|| el.attr("src"))?;
    doc.resolve_url(src).map(|u| (u.to_string(), 1.0))
}

/// Parse a srcset and choose the best candidate for the device pixel ratio.
pub fn pick_srcset(ss: &str, dpr: f32) -> Option<(String, f32)> {
    pick_srcset_with(ss, dpr, None, None)
}

/// Split a srcset per the HTML spec: URLs may contain commas; candidates are
/// separated by commas that follow a URL's whitespace/descriptors.
fn parse_srcset(ss: &str) -> Vec<(String, String)> {
    let b = ss.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b',') {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let start = i;
        while i < b.len() && !b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut url = &ss[start..i];
        let mut desc = String::new();
        if url.ends_with(',') {
            url = url.trim_end_matches(',');
        } else {
            // Descriptors until the next comma (outside parentheses).
            let mut depth = 0;
            let ds = i;
            while i < b.len() {
                match b[i] {
                    b'(' => depth += 1,
                    b')' => depth = (depth - 1).max(0),
                    b',' if depth == 0 => break,
                    _ => {}
                }
                i += 1;
            }
            desc = ss[ds..i].trim().to_string();
            i += 1;
        }
        if !url.is_empty() {
            out.push((url.to_string(), desc));
        }
    }
    out
}

/// Choose the best srcset candidate; `src` is an implicit 1x candidate and
/// `slot_w` the expected display width (CSS px) for `w` descriptors.
pub fn pick_srcset_with(ss: &str, dpr: f32, src: Option<&str>, slot_w: Option<f32>) -> Option<(String, f32)> {
    let mut cands: Vec<(String, f32, bool)> = Vec::new(); // (url, value, is_width)
    for (url, desc) in parse_srcset(ss) {
        let d = desc.split_whitespace().next().unwrap_or("1x");
        if let Some(x) = d.strip_suffix('x') {
            cands.push((url, x.parse().unwrap_or(1.0), false));
        } else if let Some(w) = d.strip_suffix('w') {
            cands.push((url, w.parse().unwrap_or(0.0), true));
        } else if d.is_empty() {
            cands.push((url, 1.0, false));
        }
    }
    if let Some(src) = src.filter(|s| !s.trim().is_empty()) {
        if !cands.iter().any(|c| c.2) && !cands.iter().any(|c| (c.1 - 1.0).abs() < 0.01) {
            cands.push((src.to_string(), 1.0, false));
        }
    }
    if cands.is_empty() {
        return None;
    }
    if cands.iter().any(|c| c.2) {
        let want = slot_w.unwrap_or(800.0) * dpr;
        let mut best = cands[0].clone();
        for c in cands.iter().filter(|c| c.2) {
            if (c.1 >= want && (best.1 < want || c.1 < best.1)) || (best.1 < want && c.1 > best.1) || !best.2 {
                best = c.clone();
            }
        }
        // Density from the chosen width relative to the slot.
        let dens = match slot_w {
            Some(sw) if sw > 0.0 && best.1 > 0.0 => (best.1 / sw).max(0.1),
            _ => 1.0,
        };
        return Some((best.0, if slot_w.is_some() { dens } else { 1.0 }));
    }
    let mut best = cands[0].clone();
    for c in &cands {
        if (c.1 >= dpr && (best.1 < dpr || c.1 < best.1)) || (best.1 < dpr && c.1 > best.1) {
            best = c.clone();
        }
    }
    Some((best.0, best.1.max(0.1)))
}

/// Decode raster or SVG image bytes (runs on a worker thread).
pub fn decode_image(bytes: &[u8], ctype: &str, url: &str, density: f32) -> Option<ImageData> {
    let looks_svg = ctype.contains("svg") || url.split('?').next().unwrap_or("").ends_with(".svg") || {
        let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
        head.contains("<svg")
    };
    if looks_svg {
        return render_svg(bytes, None, 2.0);
    }
    if super::avif::is_avif(bytes) {
        let (w, h, rgba) = super::avif::decode(bytes)?;
        let mut d = ImageData::from_rgba(w, h, rgba)?;
        d.density = density;
        return Some(d);
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    limits.max_alloc = Some(MAX_IMAGE_BYTES);
    reader.limits(limits);
    let img = reader.decode().ok()?;
    let (w, h) = (img.width(), img.height());
    let img = if w > MAX_IMAGE_DIM || h > MAX_IMAGE_DIM {
        // Memory: cap decoded size.
        let k = MAX_IMAGE_DIM as f32 / w.max(h) as f32;
        img.resize((w as f32 * k) as u32, (h as f32 * k) as u32, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w2, h2) = (rgba.width(), rgba.height());
    let mut d = ImageData::from_rgba(w2, h2, rgba.into_raw())?;
    // Keep layout size equal to the original intrinsic size.
    d.density = density * (w2 as f32 / w.max(1) as f32);
    Some(d)
}

/// Pictures an SVG may embed by name (`<image href="ohbfo:…">`): rendered
/// foreignObject HTML. Pages' own hrefs never resolve to files.
static SVG_EMBEDS: std::sync::LazyLock<std::sync::Mutex<HashMap<String, Arc<Vec<u8>>>>> = std::sync::LazyLock::new(Default::default);

/// Register a rendered picture under `key` (PNG-encoded once).
pub fn set_svg_embed(key: &str, img: &tiny_skia::Pixmap) {
    let mut m = SVG_EMBEDS.lock().unwrap();
    if m.contains_key(key) {
        return;
    }
    if m.len() > 1024 {
        m.clear();
    }
    if let Ok(png) = img.encode_png() {
        m.insert(key.to_string(), Arc::new(png));
    }
}

pub fn has_svg_embed(key: &str) -> bool {
    SVG_EMBEDS.lock().unwrap().contains_key(key)
}

thread_local! {
    /// foreignObject element → its rendered picture's key, for the SVG
    /// being serialized right now (set by the paint builder).
    pub static FO_KEYS: RefCell<HashMap<NodeId, (String, f32, f32)>> = RefCell::new(HashMap::new());
}

fn svg_options() -> resvg::usvg::Options<'static> {
    use resvg::usvg::{FontFamily, FontResolver, FontStretch, FontStyle};
    let mut opt = resvg::usvg::Options::default();
    // data: URLs and our own embeds only: never local files.
    opt.image_href_resolver = resvg::usvg::ImageHrefResolver {
        resolve_data: resvg::usvg::ImageHrefResolver::default_data_resolver(),
        resolve_string: Box::new(|href: &str, _o: &resvg::usvg::Options| {
            if href.starts_with("ohbfo:") {
                SVG_EMBEDS.lock().unwrap().get(href).map(|b| resvg::usvg::ImageKind::PNG(b.clone()))
            } else {
                None
            }
        }),
    };
    // SVG text uses the same fonts as HTML text (the default is an empty db).
    opt.fontdb = crate::text::svg_fontdb();
    opt.font_family = "Times New Roman".into();
    // Named families fall back like HTML text does (Arial → Liberation Sans …)
    // before the generic serif default.
    opt.font_resolver = FontResolver {
        select_font: Box::new(|font, db| {
            let mut names: Vec<String> = Vec::new();
            let mut generics: Vec<fontdb::Family> = Vec::new();
            for f in font.families() {
                match f {
                    FontFamily::Named(n) => {
                        names.push(n.clone());
                        names.extend(crate::text::generic_candidates(&n.to_ascii_lowercase()).iter().map(|c| c.to_string()));
                    }
                    FontFamily::Serif => generics.push(fontdb::Family::Serif),
                    FontFamily::SansSerif => generics.push(fontdb::Family::SansSerif),
                    FontFamily::Monospace => generics.push(fontdb::Family::Monospace),
                    FontFamily::Cursive => generics.push(fontdb::Family::Cursive),
                    FontFamily::Fantasy => generics.push(fontdb::Family::Fantasy),
                }
            }
            let mut list: Vec<fontdb::Family> = names.iter().map(|n| fontdb::Family::Name(n.as_str())).collect();
            list.extend(generics);
            list.push(fontdb::Family::Serif);
            let stretch = match font.stretch() {
                FontStretch::UltraCondensed => fontdb::Stretch::UltraCondensed,
                FontStretch::ExtraCondensed => fontdb::Stretch::ExtraCondensed,
                FontStretch::Condensed => fontdb::Stretch::Condensed,
                FontStretch::SemiCondensed => fontdb::Stretch::SemiCondensed,
                FontStretch::Normal => fontdb::Stretch::Normal,
                FontStretch::SemiExpanded => fontdb::Stretch::SemiExpanded,
                FontStretch::Expanded => fontdb::Stretch::Expanded,
                FontStretch::ExtraExpanded => fontdb::Stretch::ExtraExpanded,
                FontStretch::UltraExpanded => fontdb::Stretch::UltraExpanded,
            };
            let style = match font.style() {
                FontStyle::Normal => fontdb::Style::Normal,
                FontStyle::Italic => fontdb::Style::Italic,
                FontStyle::Oblique => fontdb::Style::Oblique,
            };
            db.query(&fontdb::Query { families: &list, weight: fontdb::Weight(font.weight()), stretch, style })
        }),
        select_fallback: FontResolver::default_fallback_selector(),
    };
    opt
}

fn memchr_filter(bytes: &[u8]) -> bool {
    bytes.windows(7).any(|w| w == b"<filter")
}

/// Render SVG markup at its intrinsic size × oversample (or a given size).
pub fn render_svg(bytes: &[u8], size: Option<(f32, f32)>, oversample: f32) -> Option<ImageData> {
    let opt = svg_options();
    let tree = match resvg::usvg::Tree::from_data(bytes, &opt) {
        Ok(t) => t,
        Err(e) => {
            if std::env::var_os("OHB_DEBUG_SVG").is_some() {
                eprintln!("[svg] parse error: {e}");
            }
            return None;
        }
    };
    let sz = tree.size();
    let (tw, th) = size.unwrap_or((sz.width(), sz.height()));
    // Large artwork with filters (blurred backgrounds): filter cost grows
    // with the pixel count and the blur hides the lower resolution anyway.
    let oversample = if oversample > 1.0 && tw * th * oversample * oversample > 2.0e6 && memchr_filter(bytes) { 1.0 } else { oversample };
    let (pw, ph) = ((tw * oversample).ceil().max(1.0) as u32, (th * oversample).ceil().max(1.0) as u32);
    if pw > 8192 || ph > 8192 {
        return None;
    }
    let mut pm = tiny_skia::Pixmap::new(pw, ph)?;
    let t = tiny_skia::Transform::from_scale(pw as f32 / sz.width().max(0.001), ph as f32 / sz.height().max(0.001));
    let t0 = std::time::Instant::now();
    resvg::render(&tree, t, &mut pm.as_mut());
    if std::env::var_os("OHB_DEBUG_SVG").is_some() {
        eprintln!("[svg] rendered {pw}x{ph} (size {:?}, oversample {oversample}) in {:?}", size, t0.elapsed());
    }
    Some(ImageData { width: pw, height: ph, pixmap: pm, density: pw as f32 / tw.max(0.001), nat: Some((tw, th)), svg_par: svg_root_par(bytes) })
}

/// The root element's preserveAspectRatio, when it has a viewBox (else the
/// image simply stretches like a bitmap).
fn svg_root_par(bytes: &[u8]) -> Option<(f32, f32, bool)> {
    let text = std::str::from_utf8(&bytes[..bytes.len().min(16384)]).ok()?;
    let start = text.find("<svg")?;
    let end = start + text[start..].find('>')?;
    let tag = &text[start..end];
    let attr = |name: &str| -> Option<String> {
        let mut from = 0;
        while let Some(i) = tag[from..].find(name) {
            let at = from + i;
            from = at + name.len();
            let before = tag[..at].chars().last();
            if !before.is_some_and(|c| c.is_whitespace()) {
                continue;
            }
            let rest = tag[from..].trim_start();
            let rest = rest.strip_prefix('=')?.trim_start();
            let q = rest.chars().next()?;
            if q != '"' && q != '\'' {
                return None;
            }
            let body = &rest[1..];
            return Some(body[..body.find(q)?].to_string());
        }
        None
    };
    attr("viewBox")?;
    let par = attr("preserveAspectRatio").unwrap_or_default();
    let mut words = par.split_whitespace();
    let align = words.next().unwrap_or("xMidYMid");
    if align == "none" {
        return None;
    }
    let slice = words.next() == Some("slice");
    let f = |s: &str| if s.contains("Min") { 0.0 } else if s.contains("Max") { 1.0 } else { 0.5 };
    let (x, y) = if align.len() == 8 { (f(&align[..4]), f(&align[4..])) } else { (0.5, 0.5) };
    Some((x, y, slice))
}

/// The SVG renderer can't draw a foreignObject's HTML: its text (the usual
/// content: diagram labels from Mermaid and the like) becomes SVG text in the
/// computed font, laid out by lines and aligned like the HTML would be.
fn foreign_object_text(doc: &Document, fo: NodeId, styles: &crate::css::Styles) -> String {
    use crate::css::style::{Display, LineHeight, TextAlign};
    let num = |a: &str| doc.attr(fo, a).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).unwrap_or(0.0);
    let (x, y, w) = (num("x"), num("y"), num("width"));
    if styles.get(fo).is_some_and(|s| s.display == Display::None) {
        return String::new();
    }
    let mut lines: Vec<String> = vec![String::new()];
    let mut first: Option<NodeId> = None;
    fn walk(doc: &Document, n: NodeId, styles: &crate::css::Styles, lines: &mut Vec<String>, first: &mut Option<NodeId>) {
        for &c in doc.children(n) {
            if let crate::dom::NodeData::Text(t) = &doc.node(c).data {
                let t = crate::dom::collapse_ws(t);
                if !t.is_empty() {
                    let last = lines.last_mut().unwrap();
                    if !last.is_empty() && !last.ends_with(' ') {
                        last.push(' ');
                    }
                    last.push_str(&t);
                    first.get_or_insert(n);
                }
            } else if let Some(e) = doc.element(c) {
                if styles.get(c).is_some_and(|s| s.display == Display::None) {
                    continue;
                }
                let block = matches!(e.name.as_str(), "p" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6");
                if e.name == "br" || (block && !lines.last().unwrap().is_empty()) {
                    lines.push(String::new());
                }
                walk(doc, c, styles, lines, first);
                if block && !lines.last().unwrap().is_empty() {
                    lines.push(String::new());
                }
            }
        }
    }
    walk(doc, fo, styles, &mut lines, &mut first);
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let Some(st) = first.and_then(|f| styles.get(f)) else { return String::new() };
    if lines.is_empty() {
        return String::new();
    }
    let fs = st.font_size.max(1.0);
    let lh = match st.line_height {
        LineHeight::Number(k) => k * fs,
        LineHeight::Px(p) => p,
        LineHeight::Normal => fs * 1.2,
    };
    let (ax, anchor) = match st.text_align {
        TextAlign::Center | TextAlign::WebkitCenter => (x + w / 2.0, "middle"),
        TextAlign::Right | TextAlign::End => (x + w, "end"),
        _ => (x, "start"),
    };
    // A label background (Mermaid's edge labels) behind the text.
    let mut bg = String::new();
    let mut at = first;
    while let Some(n) = at {
        if n == fo {
            break;
        }
        if let Some(ns) = styles.get(n) {
            let b = ns.background_color.resolve(ns.color);
            if b.a > 0 {
                let h = lines.len() as f32 * lh;
                bg = format!("<rect x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\" fill=\"#{:02x}{:02x}{:02x}\" fill-opacity=\"{}\" stroke=\"none\"/>", b.r, b.g, b.b, b.a as f32 / 255.0);
                break;
            }
        }
        at = doc.parent(n);
    }
    let fam: Vec<String> = st.font_family.iter().map(|f| if f.contains(' ') { format!("'{f}'") } else { f.clone() }).collect();
    let c = st.color;
    let mut out = bg;
    out += &format!(
        "<text font-size=\"{fs}\" font-family=\"{}\" font-weight=\"{}\" fill=\"#{:02x}{:02x}{:02x}\" fill-opacity=\"{}\" text-anchor=\"{anchor}\" stroke=\"none\">",
        fam.join(",").replace('"', ""),
        st.font_weight,
        c.r,
        c.g,
        c.b,
        c.a as f32 / 255.0
    );
    for (i, l) in lines.iter().enumerate() {
        let by = y + i as f32 * lh + lh / 2.0 + fs * 0.35;
        out.push_str(&format!("<tspan x=\"{ax}\" y=\"{by}\">"));
        for ch in l.chars() {
            match ch {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                _ => out.push(ch),
            }
        }
        out.push_str("</tspan>");
    }
    out.push_str("</text>");
    out
}

pub struct ResView<'a> {
    pub res: &'a Resources,
}

impl ResourceView for ResView<'_> {
    fn canvas_for(&self, node: NodeId) -> Option<Arc<ImageData>> {
        self.res.canvases.get(&node).cloned()
    }
    fn frame_image(&self, node: NodeId) -> Option<Arc<ImageData>> {
        self.res.frames.get(&node).cloned()
    }
    fn image_for(&self, doc: &Document, node: NodeId) -> Option<Arc<ImageData>> {
        let (url, _) = img_url(doc, node, self.res.dpr)?;
        self.res.get(&url)
    }
    fn image_url(&self, url: &str) -> Option<Arc<ImageData>> {
        self.res.get(url)
    }
    fn object_is_document(&self, doc: &Document, node: NodeId) -> bool {
        match img_url(doc, node, self.res.dpr) {
            Some((url, _)) => matches!(self.res.images.get(&url), Some(ImgState::Document)),
            None => false,
        }
    }
    fn image_failed(&self, doc: &Document, node: NodeId) -> bool {
        match img_url(doc, node, self.res.dpr) {
            Some((url, _)) => matches!(self.res.images.get(&url), Some(ImgState::Failed)),
            None => true,
        }
    }
    fn svg_for(&self, doc: &Document, node: NodeId, w: f32, h: f32, styles: &crate::css::Styles) -> Option<Arc<ImageData>> {
        if w < 0.5 || h < 0.5 {
            return None;
        }
        let color = styles.get(node).map(|s| s.color).unwrap_or(crate::css::values::Color::BLACK);
        // CSS fill/stroke become attributes: on the root whenever CSS supplies one
        // (inherited from the page) unless the element sets its own, and on any
        // element where a CSS rule changed the value (CSS beats presentation attributes).
        // With a <style> sheet inside, the renderer would apply its rules over
        // presentation attributes: the cascade's fill and stroke go in the
        // style attribute on every element instead.
        let sheet_inside = doc.descendants(node).any(|d| doc.element(d).is_some_and(|e| e.name == "style"));
        let over = |n: NodeId| -> Vec<(&'static str, String)> {
            let mut v = Vec::new();
            // The CSS box is the SVG viewport: the viewBox maps into it with
            // preserveAspectRatio (and without one, content stays unscaled).
            if n == node {
                v.push(("width", format!("{w}")));
                v.push(("height", format!("{h}")));
            }
            let (Some(st), Some(el)) = (styles.get(n), doc.element(n)) else { return v };
            if el.name.eq_ignore_ascii_case("foreignObject") {
                // Rendered HTML content (by the paint builder), else its text.
                if let Some((key, fw, fh)) = FO_KEYS.with(|k| k.borrow().get(&n).cloned()) {
                    let num = |a: &str| doc.attr(n, a).and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok()).unwrap_or(0.0);
                    let mut img = format!("<image x=\"{}\" y=\"{}\" width=\"{fw}\" height=\"{fh}\" preserveAspectRatio=\"none\" href=\"{key}\"", num("x"), num("y"));
                    for a in ["mask", "clip-path", "transform", "opacity", "filter"] {
                        if let Some(val) = doc.attr(n, a) {
                            let mut esc = String::new();
                            for ch in val.chars() {
                                match ch {
                                    '"' => esc.push_str("&quot;"),
                                    '&' => esc.push_str("&amp;"),
                                    '<' => esc.push_str("&lt;"),
                                    _ => esc.push(ch),
                                }
                            }
                            img.push_str(&format!(" {a}=\"{esc}\""));
                        }
                    }
                    img.push_str("/>");
                    v.push(("__replace", img));
                } else {
                    v.push(("__replace", foreign_object_text(doc, n, styles)));
                }
                return v;
            }
            let parent = doc.parent(n).and_then(|p| styles.get(p));
            let mut pre = String::new();
            for (attr, val, pval) in [("fill", &st.fill, parent.map(|p| &p.fill)), ("stroke", &st.stroke, parent.map(|p| &p.stroke))] {
                let Some(paint) = val else { continue };
                let set_here = pval != Some(val);
                if sheet_inside && el.name != "style" {
                    pre.push_str(&format!("{attr}:{};", paint.to_attr(st.color)));
                } else if set_here || (n == node && !el.has_attr(attr)) {
                    v.push((attr, paint.to_attr(st.color)));
                }
            }
            // Other presentation properties from CSS go in the style attribute (which
            // beats presentation attributes), ahead of the element's own inline style.
            let mut css = pre;
            if let Some(extra) = &st.svg_extra {
                for (i, val) in extra.iter() {
                    let (name, inherited) = crate::css::style::SVG_PROPS[*i as usize];
                    let from_parent = inherited && n != node && parent.and_then(|p| p.svg_value(*i)) == Some(val);
                    if from_parent || val.contains("var(") {
                        continue;
                    }
                    css.push_str(name);
                    css.push(':');
                    css.push_str(val);
                    css.push(';');
                }
            }
            if n != node {
                if st.display == crate::css::style::Display::None
                    && matches!(el.name.as_str(), "g" | "path" | "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" | "text" | "tspan" | "textPath" | "textpath" | "use" | "image" | "svg" | "a" | "foreignObject" | "foreignobject" | "switch")
                {
                    css.push_str("display:none;");
                }
                let pvis = parent.map(|p| p.visibility);
                if st.visibility != crate::css::style::Visibility::Visible && pvis != Some(st.visibility) {
                    css.push_str("visibility:hidden;");
                } else if st.visibility == crate::css::style::Visibility::Visible && pvis.is_some_and(|p| p != st.visibility) {
                    css.push_str("visibility:visible;");
                }
                if st.opacity < 1.0 {
                    css.push_str(&format!("opacity:{};", st.opacity));
                }
            }
            // The element's own style: declarations using var() can't be
            // resolved by the SVG renderer (their computed values are given
            // as attributes / above instead).
            let own = el.attr("style").map(|o| {
                if o.contains("var(") {
                    o.split(';').filter(|d| !d.contains("var(") && !d.trim().is_empty()).map(|d| format!("{};", d.trim())).collect::<String>()
                } else {
                    o.to_string()
                }
            });
            let own_changed = el.attr("style").is_some_and(|o| o.contains("var("));
            if !css.is_empty() || own_changed {
                if let Some(own) = own {
                    css.push_str(&own);
                }
                v.push(("style", css));
            }
            v
        };
        // <use> references outside this <svg>: same-document targets (a hidden
        // sprite elsewhere in the page) and external sprite files are copied in.
        let mut defs = String::new();
        let mut rewrites: Vec<(NodeId, &'static str, String)> = Vec::new();
        {
            let mut seen: Vec<String> = Vec::new();
            let scope = doc.root_of(node);
            for u in doc.descendants(node) {
                let Some(el) = doc.element(u) else { continue };
                if el.name != "use" {
                    continue;
                }
                let (aname, href) = match (el.attr("href"), el.attr("xlink:href")) {
                    (Some(h), _) => ("href", h),
                    (None, Some(h)) => ("xlink:href", h),
                    _ => continue,
                };
                let href = href.trim();
                if let Some(frag) = href.strip_prefix('#') {
                    if frag.is_empty() || seen.iter().any(|s| s == frag) {
                        continue;
                    }
                    let target = if scope == crate::dom::DOCUMENT { doc.get_element_by_id(frag) } else { doc.get_element_by_id_in(scope, frag) };
                    let Some(t) = target else { continue };
                    if doc.contains(node, t) || doc.contains(t, node) {
                        continue;
                    }
                    seen.push(frag.to_string());
                    defs.push_str(&doc.svg_fragment_with(t, &over));
                } else if let Some((base, frag)) = href.split_once('#') {
                    let Some(abs) = doc.resolve_url(base) else { continue };
                    let Some(SvgDocState::Ready(sd)) = self.res.svg_docs.get(abs.as_str()) else { continue };
                    let Some(src) = sd.element(frag) else { continue };
                    rewrites.push((u, aname, format!("#{frag}")));
                    if !seen.iter().any(|s| s == frag) {
                        seen.push(frag.to_string());
                        defs.push_str(src);
                    }
                }
            }
        }
        // url(#id) references (mask, clip-path, fill, filter…) to elements
        // outside this <svg> (a shared hidden <svg> of masks, as on Discord):
        // the targets are copied in.
        {
            let scope = doc.root_of(node);
            let mut seen: Vec<String> = Vec::new();
            for u in doc.descendants(node) {
                let Some(el) = doc.element(u) else { continue };
                for a in ["mask", "clip-path", "fill", "stroke", "filter", "marker-start", "marker-mid", "marker-end"] {
                    let Some(val) = el.attr(a) else { continue };
                    let Some(rest) = val.trim().strip_prefix("url(") else { continue };
                    let id = rest.trim_end_matches(')').trim().trim_matches(|c| c == '"' || c == '\'');
                    let Some(frag) = id.strip_prefix('#') else { continue };
                    if frag.is_empty() || seen.iter().any(|s| s == frag) || seen.len() > 64 {
                        continue;
                    }
                    let target = if scope == crate::dom::DOCUMENT { doc.get_element_by_id(frag) } else { doc.get_element_by_id_in(scope, frag) };
                    let Some(t) = target else { continue };
                    if doc.contains(node, t) {
                        continue;
                    }
                    seen.push(frag.to_string());
                    defs.push_str(&doc.svg_fragment_with(t, &over));
                }
            }
        }
        let mut markup = if rewrites.is_empty() {
            doc.svg_markup_with(node, &over)
        } else {
            let over2 = |n: NodeId| {
                let mut v = over(n);
                for (u, a, h) in &rewrites {
                    if *u == n {
                        v.push((*a, h.clone()));
                    }
                }
                v
            };
            doc.svg_markup_with(node, &over2)
        };
        if !defs.is_empty() {
            if let Some(i) = markup.rfind("</") {
                markup.insert_str(i, &format!("<defs>{defs}</defs>"));
            }
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&markup, &mut hasher);
        let mhash = std::hash::Hasher::finish(&hasher);
        let ckey = (color.r as u32) << 24 | (color.g as u32) << 16 | (color.b as u32) << 8 | color.a as u32;
        let key = (node, (w * 4.0) as u32, (h * 4.0) as u32, mhash, ckey);
        if let Some(v) = self.res.svg_cache.borrow().get(&key) {
            return v.clone();
        }
        // currentColor support: resolve to the inherited text color.
        if markup.contains("currentColor") || markup.contains("currentcolor") {
            let hex = format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b);
            markup = markup.replace("currentColor", &hex).replace("currentcolor", &hex);
        }
        if std::env::var_os("OHB_DEBUG_SVG").is_some() {
            if std::env::var_os("OHB_DEBUG_SVG").is_some_and(|v| v == "full") {
                eprintln!("[svg] {w}x{h} {markup}");
            } else {
                eprintln!("[svg] {w}x{h} {}", markup.chars().take(600).collect::<String>());
            }
        }
        let img = render_svg(markup.as_bytes(), Some((w, h)), self.res.dpr.max(1.0) * 1.5).map(Arc::new);
        let mut c = self.res.svg_cache.borrow_mut();
        // Bounded by count and by pixels (animated SVGs render anew each frame).
        if c.len() > 2000 || (c.len() > 16 && c.values().flatten().map(|i| i.pixmap.data().len()).sum::<usize>() > 192 << 20) {
            c.clear();
        }
        c.insert(key, img.clone());
        img
    }
}
