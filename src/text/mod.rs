// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronText — fonts (system, embedded, and @font-face web fonts), font
//! matching with per-character fallback, and text shaping via rustybuzz.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::css::style::FontStyle;

pub type FontBytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// A loaded font face. Parsed tables borrow from `data`, which is kept alive
/// (and never moved, being behind an Arc) for the lifetime of the Font.
pub struct Font {
    pub uid: u32,
    pub family: String,
    pub upem: f32,
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
    pub x_height: f32,
    pub underline_pos: f32,
    pub underline_thickness: f32,
    pub synth_bold: bool,
    pub synth_italic: bool,
    pub has_color: bool,
    pub weight: u16,
    pub italic: bool,
    pub index: u32,
    /// Variation axes (fvar): (tag, min, default, max).
    pub axes: Vec<(ttf_parser::Tag, f32, f32, f32)>,
    /// The variation coordinates applied to this instance.
    pub coords: Vec<(ttf_parser::Tag, f32)>,
    /// The instance this one was derived from (by variation or synthesis).
    pub base_uid: u32,
    hb: rustybuzz::Face<'static>,
    face: ttf_parser::Face<'static>,
    _data: FontBytes,
}

// SAFETY: the parsed faces only hold shared references into `_data`, which is
// immutable and thread-safe (Arc<dyn AsRef<[u8]> + Send + Sync).
unsafe impl Send for Font {}
unsafe impl Sync for Font {}

static NEXT_UID: AtomicU32 = AtomicU32::new(1);

impl Font {
    pub fn from_bytes(data: FontBytes, index: u32, family: &str) -> Option<Font> {
        let slice: &[u8] = (*data).as_ref();
        // SAFETY: `data` is owned by the returned Font and outlives the faces.
        let slice: &'static [u8] = unsafe { std::slice::from_raw_parts(slice.as_ptr(), slice.len()) };
        let face = ttf_parser::Face::parse(slice, index).ok()?;
        let hb = rustybuzz::Face::from_face(face.clone());
        let upem = face.units_per_em() as f32;
        let (ascent, descent, line_gap) = {
            let a = face.ascender() as f32;
            let d = -(face.descender() as f32);
            let g = face.line_gap() as f32;
            if a + d <= 0.0 {
                (upem * 0.8, upem * 0.2, 0.0)
            } else {
                (a, d, g.max(0.0))
            }
        };
        let x_height = face.x_height().map(|x| x as f32).unwrap_or(upem * 0.5);
        let (up, ut) = match face.underline_metrics() {
            Some(m) => (-(m.position as f32), (m.thickness as f32).max(upem / 20.0)),
            None => (upem * 0.1, upem / 14.0),
        };
        let has_color = face.tables().colr.is_some() || face.tables().sbix.is_some() || face.tables().cbdt.is_some();
        let weight = face.weight().to_number();
        let italic = face.is_italic() || face.is_oblique();
        let axes: Vec<(ttf_parser::Tag, f32, f32, f32)> = face.variation_axes().into_iter().map(|a| (a.tag, a.min_value, a.def_value, a.max_value)).collect();
        let uid = NEXT_UID.fetch_add(1, Ordering::Relaxed);
        Some(Font {
            uid,
            axes,
            coords: Vec::new(),
            base_uid: uid,
            family: family.to_string(),
            upem,
            ascent,
            descent,
            line_gap,
            x_height,
            underline_pos: up,
            underline_thickness: ut,
            synth_bold: false,
            synth_italic: false,
            has_color,
            weight,
            italic,
            index,
            hb,
            face,
            _data: data,
        })
    }

    pub fn face(&self) -> &ttf_parser::Face<'static> {
        &self.face
    }

    fn axis(&self, tag: &[u8; 4]) -> Option<(f32, f32, f32)> {
        let t = ttf_parser::Tag::from_bytes(tag);
        self.axes.iter().find(|a| a.0 == t).map(|a| (a.1, a.2, a.3))
    }

    /// A variable font with a weight axis: weights come from the axis, not synthesis.
    pub fn has_wght(&self) -> bool {
        self.axis(b"wght").is_some()
    }

    pub fn has_opsz(&self) -> bool {
        self.axis(b"opsz").is_some()
    }

    /// Re-parse the font data with this instance's variation coordinates.
    fn reparse(&self) -> Font {
        let mut f = Font::from_bytes(self._data.clone(), self.index, &self.family).expect("reparse");
        f.apply_coords(&self.coords);
        f.base_uid = self.base_uid;
        f.weight = self.weight;
        f
    }

    fn apply_coords(&mut self, coords: &[(ttf_parser::Tag, f32)]) {
        let mut vars = Vec::new();
        for &(t, v) in coords {
            if self.face.set_variation(t, v).is_some() {
                vars.push(rustybuzz::Variation { tag: t, value: v });
            }
        }
        self.hb.set_variations(&vars);
        self.coords = coords.to_vec();
        // Metrics that depend on the instance.
        if let Some(x) = self.face.x_height() {
            self.x_height = x as f32;
        }
    }

    /// The instance of a variable font at `weight` (wght axis) and, when
    /// `opsz` is given, that optical size (opsz axis: font-optical-sizing:auto
    /// uses the font size in px). Fonts without those axes are returned as is.
    pub fn varied(self: &Arc<Font>, weight: Option<u16>, opsz: Option<f32>) -> Arc<Font> {
        self.varied_with(weight, opsz, &[])
    }

    /// `varied`, then explicit axis values (font-variation-settings) on top.
    pub fn varied_with(self: &Arc<Font>, weight: Option<u16>, opsz: Option<f32>, extra: &[(u32, f32)]) -> Arc<Font> {
        if self.axes.is_empty() {
            return self.clone();
        }
        let mut coords = self.coords.clone();
        let mut set = |tag: &[u8; 4], v: f32, coords: &mut Vec<(ttf_parser::Tag, f32)>| {
            if let Some((min, _def, max)) = self.axis(tag) {
                let t = ttf_parser::Tag::from_bytes(tag);
                let v = v.clamp(min, max);
                match coords.iter_mut().find(|c| c.0 == t) {
                    Some(c) => c.1 = v,
                    None => coords.push((t, v)),
                }
            }
        };
        if let Some(w) = weight {
            set(b"wght", w as f32, &mut coords);
        }
        if let Some(o) = opsz {
            set(b"opsz", (o * 4.0).round() / 4.0, &mut coords);
        }
        for &(t, v) in extra {
            set(&t.to_be_bytes(), v, &mut coords);
        }
        if coords == self.coords {
            return self.clone();
        }
        let key: Vec<(u32, u32)> = coords.iter().map(|(t, v)| (t.0, v.to_bits())).collect();
        VARIED.with(|m| {
            let k = (self.base_uid, self.synth_bold, self.synth_italic, key);
            if let Some(f) = m.borrow().get(&k) {
                return f.clone();
            }
            let mut f = Font::from_bytes(self._data.clone(), self.index, &self.family).expect("reparse");
            f.apply_coords(&coords);
            f.base_uid = self.base_uid;
            f.synth_bold = self.synth_bold;
            f.synth_italic = self.synth_italic;
            if let Some(w) = weight {
                f.weight = w;
            }
            // An instance that comes out blank: the default instance instead.
            if !f.renders() {
                return self.clone();
            }
            let f = Arc::new(f);
            let mut mm = m.borrow_mut();
            if mm.len() > 512 {
                mm.clear();
            }
            mm.insert(k, f.clone());
            f
        })
    }

    /// Does Latin text in this font shape to real glyphs that draw? (Some
    /// system fonts parse but come out blank here: they are skipped.)
    pub fn renders(&self) -> bool {
        let Some(g) = self.face.glyph_index('H').filter(|g| g.0 != 0) else { return true };
        if self.face.glyph_hor_advance(g).unwrap_or(0) == 0 {
            return false;
        }
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str("Hag");
        let out = rustybuzz::shape(&self.hb, &[], buf);
        if out.glyph_infos().iter().any(|i| i.glyph_id == 0) || out.glyph_positions().iter().map(|p| p.x_advance).sum::<i32>() <= 0 {
            return false;
        }
        crate::paint::raster::glyph_has_ink(self, g.0)
    }

    pub fn has_glyph(&self, c: char) -> bool {
        self.face.glyph_index(c).map(|g| g.0 != 0).unwrap_or(false)
    }

    pub fn scale(&self, size: f32) -> f32 {
        size / self.upem
    }

    /// (average character width, maximum character width) in px at `size`, the
    /// metrics browsers use to size `<input size>` / `<textarea cols>`.
    pub fn char_widths(&self, size: f32) -> (f32, f32) {
        let sc = self.scale(size);
        let os2_avg = self
            .face
            .raw_face()
            .table(ttf_parser::Tag::from_bytes(b"OS/2"))
            .filter(|t| t.len() >= 4)
            .map(|t| i16::from_be_bytes([t[2], t[3]]) as f32)
            .filter(|v| *v > 0.0);
        // Blink (GetAvgCharWidth / HasValidAvgCharWidth): the OS/2 average,
        // rounded up when its fraction is ≥ .5, unless it is far wider than
        // "0" (CJK fonts); otherwise the width of "0" and no max-char term.
        let zero = self.face.glyph_index('0').filter(|g| g.0 != 0).and_then(|g| self.face.glyph_hor_advance(g)).map(|a| a as f32 * sc);
        let valid = os2_avg.map(|v| zero.map(|z| v * sc <= z * 1.7).unwrap_or(true)).unwrap_or(false);
        if !valid {
            let z = zero.unwrap_or(size * 0.5);
            return (z, z);
        }
        let raw = os2_avg.unwrap_or(0.0) * sc;
        let avg = raw.max(raw.round());
        let bb = self.face.global_bounding_box();
        let max = ((bb.x_max as f32 - bb.x_min as f32) * sc).round();
        (avg, max)
    }

    /// Per-em ratios for the CSS font-relative units: (ex, ch, cap, ic) — the
    /// x-height, the advance of "0", the cap height and the advance of "水".
    pub fn unit_ratios(&self) -> (f32, f32, f32, f32) {
        let adv = |c: char| self.face.glyph_index(c).filter(|g| g.0 != 0).and_then(|g| self.face.glyph_hor_advance(g)).map(|a| a as f32 / self.upem);
        let ex = self.x_height / self.upem;
        let cap = self.face.capital_height().map(|c| c as f32 / self.upem).filter(|v| *v > 0.0).unwrap_or(0.7);
        (ex, adv('0').unwrap_or(0.5), cap, adv('水').unwrap_or(1.0))
    }

    /// Ascent and descent in px at `size` (the text's content box), rounded
    /// like the layout engine.
    pub fn ascent_descent(&self, size: f32) -> (f32, f32) {
        let sc = self.scale(size);
        ((self.ascent * sc).round(), (self.descent * sc).round())
    }

    /// `line-height: normal` in px at `size`, rounded like the layout engine.
    pub fn normal_line_height(&self, size: f32) -> f32 {
        let sc = self.scale(size);
        (self.ascent * sc).round() + (self.descent * sc).round() + (self.line_gap * sc).round()
    }

    /// Clone with synthetic emboldening / slanting flags (cached per thread).
    fn synthesized(self: &Arc<Font>, bold: bool, italic: bool) -> Arc<Font> {
        if !bold && !italic {
            return self.clone();
        }
        SYNTH.with(|m| {
            let key = (self.uid, bold, italic);
            if let Some(f) = m.borrow().get(&key) {
                return f.clone();
            }
            let mut f = self.reparse();
            f.synth_bold = bold;
            f.synth_italic = italic;
            let f = Arc::new(f);
            m.borrow_mut().insert(key, f.clone());
            f
        })
    }
}

thread_local! {
    static SYNTH: std::cell::RefCell<HashMap<(u32, bool, bool), Arc<Font>>> = std::cell::RefCell::new(HashMap::new());
    #[allow(clippy::type_complexity)]
    static VARIED: std::cell::RefCell<HashMap<(u32, bool, bool, Vec<(u32, u32)>), Arc<Font>>> = std::cell::RefCell::new(HashMap::new());
}

// --------------------------------------------------------------------------
// System font database (shared by all tabs)
// --------------------------------------------------------------------------

static EMBEDDED_SANS: &[u8] = include_bytes!("../../assets/DejaVuSans.ttf");

pub struct SystemFonts {
    pub db: fontdb::Database,
    families: HashMap<String, Vec<fontdb::ID>>,
    loaded: Mutex<HashMap<fontdb::ID, Option<Arc<Font>>>>,
    embedded: Arc<Font>,
}

pub fn system() -> &'static SystemFonts {
    static SYS: OnceLock<SystemFonts> = OnceLock::new();
    SYS.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        let mut families: HashMap<String, Vec<fontdb::ID>> = HashMap::new();
        for f in db.faces() {
            for (name, _) in &f.families {
                families.entry(name.to_ascii_lowercase()).or_default().push(f.id);
            }
        }
        let data: FontBytes = Arc::new(EMBEDDED_SANS);
        let embedded = Arc::new(Font::from_bytes(data, 0, "DejaVu Sans").expect("embedded font"));
        SystemFonts { db, families, loaded: Mutex::new(HashMap::new()), embedded }
    })
}

struct MmapBytes(memmap2::Mmap);
impl AsRef<[u8]> for MmapBytes {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl SystemFonts {
    pub fn load(&self, id: fontdb::ID) -> Option<Arc<Font>> {
        if let Some(f) = self.loaded.lock().unwrap().get(&id) {
            return f.clone();
        }
        let info = self.db.face(id)?;
        let family = info.families.first().map(|(n, _)| n.clone()).unwrap_or_default();
        let index = info.index;
        let data: Option<FontBytes> = match &info.source {
            fontdb::Source::Binary(b) => Some(b.clone()),
            fontdb::Source::File(path) => std::fs::File::open(path)
                .ok()
                // SAFETY: font files are treated as read-only; a concurrently
                // truncated file could fault, as in every mmap-based font stack.
                .and_then(|f| unsafe { memmap2::Mmap::map(&f) }.ok())
                .map(|m| Arc::new(MmapBytes(m)) as FontBytes),
            fontdb::Source::SharedFile(_, d) => Some(d.clone()),
        };
        let font = data.and_then(|d| Font::from_bytes(d, index, &family)).filter(|f| {
            let ok = f.renders();
            if !ok && std::env::var_os("OHB_DEBUG_FONTS").is_some() {
                eprintln!("[fonts] skipping {family:?}: its glyphs don't render");
            }
            ok
        }).map(Arc::new);
        self.loaded.lock().unwrap().insert(id, font.clone());
        font
    }

    pub fn family_faces(&self, name: &str) -> Option<&Vec<fontdb::ID>> {
        self.families.get(&name.to_ascii_lowercase())
    }

    pub fn embedded(&self) -> Arc<Font> {
        self.embedded.clone()
    }
}

/// The system font database prepared for resvg (SVG `<text>`): OHB's own
/// faces plus the embedded fallback, with generic families mapped the same
/// way as HTML text.
pub fn svg_fontdb() -> std::sync::Arc<fontdb::Database> {
    static DB: OnceLock<std::sync::Arc<fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let sys = system();
        let mut db = sys.db.clone();
        db.load_font_data(EMBEDDED_SANS.to_vec());
        let pick = |generic: &str| -> String {
            generic_candidates(generic)
                .iter()
                .find(|c| sys.families.contains_key(&c.to_ascii_lowercase()))
                .map(|s| s.to_string())
                .unwrap_or_else(|| "DejaVu Sans".into())
        };
        db.set_serif_family(pick("serif"));
        db.set_sans_serif_family(pick("sans-serif"));
        db.set_monospace_family(pick("monospace"));
        db.set_cursive_family(pick("cursive"));
        db.set_fantasy_family(pick("fantasy"));
        std::sync::Arc::new(db)
    })
    .clone()
}

pub fn generic_candidates(name: &str) -> &'static [&'static str] {
    // Named families only map here where Chromium substitutes them on every
    // platform (the metric-compatible core fonts); any other missing family
    // is skipped so the next one in the list applies, as in Chromium.
    #[cfg(target_os = "macos")]
    let name = match name {
        "-apple-system" | "blinkmacsystemfont" => "system-ui",
        n => n,
    };
    #[cfg(target_os = "windows")]
    let name = match name {
        "segoe ui" => "system-ui",
        n => n,
    };
    match name {
        "serif" | "ui-serif" | "times" | "times new roman" => &[
            "Times New Roman", "Tinos", "Liberation Serif", "DejaVu Serif", "Noto Serif", "Georgia", "Times", "FreeSerif", "Cambria",
        ],
        "sans-serif" | "helvetica" | "arial" => &[
            "Arial", "Helvetica", "Arimo", "Liberation Sans", "Helvetica Neue", "Segoe UI", "Noto Sans", "DejaVu Sans", "Roboto", "FreeSans", "Verdana",
        ],
        "monospace" | "ui-monospace" | "courier" | "courier new" => &[
            "Consolas", "Menlo", "SF Mono", "DejaVu Sans Mono", "Cousine", "Liberation Mono", "Noto Sans Mono", "Courier New", "Monaco", "FreeMono", "Courier",
        ],
        "system-ui" | "ui-sans-serif" | "ui-rounded" | "-webkit-system-font" => &[
            "Segoe UI", ".AppleSystemUIFont", "SF Pro Text", "SF Pro", "Helvetica Neue", "Ubuntu", "Cantarell", "Noto Sans", "Roboto", "Liberation Sans", "DejaVu Sans", "Arial",
        ],
        "cursive" => &["Comic Sans MS", "Apple Chancery", "URW Chancery L", "Comic Neue", "DejaVu Sans"],
        "fantasy" => &["Impact", "Papyrus", "DejaVu Sans"],
        "emoji" | "apple color emoji" | "segoe ui emoji" | "noto color emoji" => &["Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji", "Twemoji", "EmojiOne", "Symbola"],
        "math" => &["Latin Modern Math", "STIX Two Math", "Cambria Math", "DejaVu Math TeX Gyre", "STIX Math", "DejaVu Sans"],
        _ => &[],
    }
}

/// Family substitutions from the system's fontconfig configuration
/// (`<alias>` rules), so names like "Helvetica Neue" or "system-ui" resolve
/// the way the desktop (and Chromium on Linux) resolves them.
#[derive(Default, Debug)]
struct FcAlias {
    prefer: Vec<String>,
    accept: Vec<String>,
    default: Vec<String>,
}

fn fc_aliases() -> &'static HashMap<String, FcAlias> {
    static ALIASES: OnceLock<HashMap<String, FcAlias>> = OnceLock::new();
    ALIASES.get_or_init(|| {
        let mut map: HashMap<String, FcAlias> = HashMap::new();
        if std::env::var_os("OHB_NO_FONTCONFIG").is_some() {
            return map;
        }
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        let mut dir_files = |d: std::path::PathBuf| {
            if let Ok(rd) = std::fs::read_dir(&d) {
                let mut v: Vec<_> = rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().map(|x| x == "conf").unwrap_or(false)).collect();
                v.sort();
                files.extend(v);
            }
        };
        dir_files("/etc/fonts/conf.d".into());
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".config")));
        if let Some(x) = &xdg {
            dir_files(x.join("fontconfig/conf.d"));
        }
        files.push("/etc/fonts/local.conf".into());
        if let Some(x) = &xdg {
            files.push(x.join("fontconfig/fonts.conf"));
        }
        for f in files {
            if let Ok(text) = std::fs::read_to_string(&f) {
                parse_fc_aliases(&text, &mut map);
            }
        }
        map
    })
}

/// Collect `<alias>` rules from one fontconfig file. Families in `<prefer>`
/// go before the aliased name (earlier files first), `<accept>` right after
/// it, `<default>` at the end.
fn parse_fc_aliases(text: &str, map: &mut HashMap<String, FcAlias>) {
    fn families(block: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = block;
        while let Some(i) = rest.find("<family>") {
            let after = &rest[i + 8..];
            let Some(j) = after.find("</family>") else { break };
            let name = after[..j].trim().replace("&amp;", "&");
            if !name.is_empty() {
                out.push(name);
            }
            rest = &after[j + 9..];
        }
        out
    }
    fn section<'a>(block: &'a str, tag: &str) -> Option<&'a str> {
        let open = format!("<{tag}");
        let close = format!("</{tag}>");
        let i = block.find(&open)?;
        let body = &block[i..];
        let start = body.find('>')? + 1;
        let end = body.find(&close)?;
        (start <= end).then(|| &body[start..end])
    }
    // Drop comments first.
    let mut clean = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("<!--") {
        clean.push_str(&rest[..i]);
        match rest[i..].find("-->") {
            Some(j) => rest = &rest[i + j + 3..],
            None => {
                rest = "";
                break;
            }
        }
    }
    clean.push_str(rest);
    let mut rest = clean.as_str();
    while let Some(i) = rest.find("<alias") {
        let body = &rest[i..];
        let Some(end) = body.find("</alias>") else { break };
        let block = &body[..end];
        rest = &body[end + 8..];
        // The aliased names are the <family> elements before any prefer/accept/default.
        let head_end = ["<prefer", "<accept", "<default"].iter().filter_map(|t| block.find(t)).min().unwrap_or(block.len());
        let targets = families(&block[..head_end]);
        let prefer = section(block, "prefer").map(families).unwrap_or_default();
        let accept = section(block, "accept").map(families).unwrap_or_default();
        let default = section(block, "default").map(families).unwrap_or_default();
        for t in targets {
            let e = map.entry(t.to_ascii_lowercase()).or_default();
            e.prefer.extend(prefer.iter().cloned());
            e.accept.extend(accept.iter().cloned());
            e.default.extend(default.iter().cloned());
        }
    }
}

const FALLBACK_CHAIN: &[&str] = &[
    "Segoe UI", "Arial", "Helvetica", "Liberation Sans", "DejaVu Sans", "Noto Sans", "Segoe UI Symbol", "Apple Symbols",
    "Noto Sans Symbols", "Noto Sans Symbols 2", "Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji", "Noto Sans CJK SC",
    "Noto Sans CJK JP", "Microsoft YaHei", "PingFang SC", "Hiragino Sans", "Yu Gothic", "MS Gothic", "Malgun Gothic",
    "Apple SD Gothic Neo", "WenQuanYi Zen Hei", "WenQuanYi Micro Hei", "Noto Sans Arabic", "Noto Sans Hebrew",
    "Noto Sans Devanagari", "Noto Sans Thai", "Nirmala UI", "Leelawadee UI", "Arial Unicode MS", "FreeSans", "DejaVu Sans Mono",
];

/// The primary system font for a family list (web fonts are not consulted):
/// what the cascade uses to resolve `ex`, `ch`, `cap`, `ic` and `lh` units.
pub fn system_primary(families: &[String], weight: u16, style: FontStyle) -> Arc<Font> {
    thread_local! {
        static CACHE: std::cell::RefCell<HashMap<(String, u16, bool), Arc<Font>>> = std::cell::RefCell::new(HashMap::new());
    }
    let key = (families.join(","), weight, style != FontStyle::Normal);
    if let Some(f) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return f;
    }
    let ctx = FontContext::new();
    let f = families
        .iter()
        .find_map(|fam| ctx.lookup_family(fam, weight, style))
        .or_else(|| ctx.lookup_family("serif", weight, style))
        .map(|(f, _, _)| f)
        .unwrap_or_else(|| system().embedded());
    CACHE.with(|c| c.borrow_mut().insert(key, f.clone()));
    f
}

/// Pick the face from `ids` that best matches weight/style (CSS Fonts §5.2, simplified).
fn best_face(sys: &SystemFonts, ids: &[fontdb::ID], weight: u16, style: FontStyle) -> Option<fontdb::ID> {
    let want_italic = style != FontStyle::Normal;
    let mut best: Option<(i32, fontdb::ID)> = None;
    for &id in ids {
        let info = match sys.db.face(id) {
            Some(i) => i,
            None => continue,
        };
        let italic = info.style != fontdb::Style::Normal;
        let w = info.weight.0 as i32;
        let mut score = if italic == want_italic { 0 } else { 10_000 };
        let d = (w - weight as i32).abs();
        score += d * 2 + if (weight >= 400 && w < weight as i32) || (weight < 400 && w > weight as i32) { 1 } else { 0 };
        if info.stretch != fontdb::Stretch::Normal {
            score += 500;
        }
        if best.map(|(s, _)| score < s).unwrap_or(true) {
            best = Some((score, id));
        }
    }
    best.map(|(_, id)| id)
}

struct WebFace {
    family: String,
    weight: (u16, u16),
    style: FontStyle,
    font: Arc<Font>,
    ranges: Vec<(u32, u32)>,
}

impl WebFace {
    fn covers(&self, c: char) -> bool {
        let u = c as u32;
        (self.ranges.is_empty() || self.ranges.iter().any(|(a, b)| u >= *a && u <= *b)) && self.font.has_glyph(c)
    }
}

/// Per-page font context: web fonts + matching/fallback caches + shaping cache.
pub struct FontContext {
    web: Vec<WebFace>,
    pub pending_families: Vec<String>,
    primary_cache: HashMap<(usize, u16, FontStyle), Arc<Font>>,
    fallback_cache: HashMap<(char, usize, u16, bool), Option<Arc<Font>>>,
    shape_cache: HashMap<(u32, u32, u32, u32, usize, String), Rc<ShapedText>>,
    space_cache: HashMap<(u32, u32), f32>,
    fallback_fonts: Option<Vec<Arc<Font>>>,
    /// Keeps family-list Rcs alive so cache keys based on their address stay unique.
    pinned: Vec<Rc<Vec<String>>>,
    /// Addresses of `pinned` (a set: the list can be long).
    pinned_set: std::collections::HashSet<usize>,
}

impl Default for FontContext {
    fn default() -> Self {
        Self::new()
    }
}

impl FontContext {
    pub fn new() -> FontContext {
        FontContext {
            web: Vec::new(),
            pending_families: Vec::new(),
            primary_cache: HashMap::new(),
            fallback_cache: HashMap::new(),
            shape_cache: HashMap::new(),
            space_cache: HashMap::new(),
            fallback_fonts: None,
            pinned: Vec::new(),
            pinned_set: std::collections::HashSet::new(),
        }
    }

    fn pin(&mut self, f: &Rc<Vec<String>>) {
        let addr = Rc::as_ptr(f) as usize;
        if !self.pinned_set.contains(&addr) {
            if self.pinned.len() > 4_000 {
                self.pinned.clear();
                self.pinned_set.clear();
                self.primary_cache.clear();
                self.fallback_cache.clear();
                self.shape_cache.clear();
            }
            self.pinned_set.insert(addr);
            self.pinned.push(f.clone());
        }
    }

    /// Register a downloaded web font. Accepts TTF/OTF/WOFF/WOFF2.
    pub fn add_web_font(&mut self, family: &str, weight: (u16, u16), style: FontStyle, bytes: &[u8], ranges: Vec<(u32, u32)>) -> bool {
        let data = match decode_font_container(bytes) {
            Some(d) => d,
            None => return false,
        };
        let arc: FontBytes = Arc::new(data);
        let font = match Font::from_bytes(arc, 0, family) {
            Some(f) => Arc::new(f),
            None => return false,
        };
        self.web.push(WebFace { family: family.to_ascii_lowercase(), weight, style, font, ranges });
        self.pending_families.retain(|f| !f.eq_ignore_ascii_case(family));
        self.primary_cache.clear();
        self.fallback_cache.clear();
        self.shape_cache.clear();
        self.space_cache.clear();
        true
    }

    pub fn web_font_count(&self) -> usize {
        self.web.len()
    }

    fn web_match(&self, family_lc: &str, weight: u16, style: FontStyle) -> Option<(Arc<Font>, bool, bool)> {
        let mut best: Option<(i32, &WebFace)> = None;
        for f in self.web.iter().filter(|f| f.family == family_lc) {
            let in_range = weight >= f.weight.0 && weight <= f.weight.1;
            let dist = if in_range { 0 } else { (weight as i32 - f.weight.0 as i32).abs().min((weight as i32 - f.weight.1 as i32).abs()) };
            let style_pen = if (f.style == FontStyle::Normal) == (style == FontStyle::Normal) { 0 } else { 5000 };
            let latin_pen = if f.covers('a') { 0 } else { 2 };
            let score = dist * 4 + style_pen + latin_pen;
            if best.map(|(s, _)| score < s).unwrap_or(true) {
                best = Some((score, f));
            }
        }
        best.map(|(_, f)| {
            let var = f.font.has_wght();
            let bold = weight >= 600 && f.weight.1 < 600 && !var;
            let italic = style != FontStyle::Normal && f.style == FontStyle::Normal;
            (if var { f.font.varied(Some(weight), None) } else { f.font.clone() }, bold, italic)
        })
    }

    fn system_match(family_lc: &str, weight: u16, style: FontStyle) -> Option<(Arc<Font>, bool, bool)> {
        let sys = system();
        let ids = sys.family_faces(family_lc)?;
        let id = best_face(sys, ids, weight, style)?;
        let font = sys.load(id)?;
        let var = font.has_wght();
        let bold = weight >= 600 && font.weight < 550 && !var;
        let italic = style != FontStyle::Normal && !font.italic;
        Some((if var { font.varied(Some(weight), None) } else { font }, bold, italic))
    }

    fn lookup_family(&self, fam: &str, weight: u16, style: FontStyle) -> Option<(Arc<Font>, bool, bool)> {
        let lc = fam.to_ascii_lowercase();
        if let Some(m) = self.web_match(&lc, weight, style) {
            return Some(m);
        }
        // fontconfig aliases apply to named families, system-ui and monospace
        // (Chromium's default fixed font is "Monospace", resolved by fontconfig);
        // serif and sans-serif keep the browser defaults (Times New Roman and
        // Arial, or their metric-compatible substitutes).
        // The core families resolve the way Blink does: the family itself,
        // then its alternate name (Times <-> Times New Roman, Helvetica <->
        // Arial, Courier <-> Courier New), then only metric-compatible
        // substitutes (Liberation, Tinos/Arimo/Cousine) — not fontconfig's
        // looser aliases (TeX Gyre, Nimbus, URW), whose vertical metrics differ.
        if matches!(lc.as_str(), "times" | "times new roman" | "helvetica" | "arial" | "courier" | "courier new") {
            if let Some(m) = Self::system_match(&lc, weight, style) {
                return Some(m);
            }
            let alt = match lc.as_str() {
                "times" => "times new roman",
                "times new roman" => "times",
                "helvetica" => "arial",
                "arial" => "helvetica",
                "courier" => "courier new",
                _ => "courier",
            };
            if let Some(m) = Self::system_match(alt, weight, style) {
                return Some(m);
            }
            let compat: &[&str] = match lc.as_str() {
                "times" | "times new roman" => &["tinos", "liberation serif"],
                "helvetica" | "arial" => &["arimo", "liberation sans"],
                _ => &["cousine", "liberation mono"],
            };
            return compat.iter().find_map(|c| Self::system_match(c, weight, style));
        }
        let fc = if matches!(lc.as_str(), "serif" | "sans-serif" | "cursive" | "fantasy") { None } else { fc_aliases().get(&lc) };
        let try_all = |list: &[String]| list.iter().find_map(|c| Self::system_match(&c.to_ascii_lowercase(), weight, style));
        if let Some(m) = fc.and_then(|a| try_all(&a.prefer)) {
            return Some(m);
        }
        if let Some(m) = Self::system_match(&lc, weight, style) {
            return Some(m);
        }
        if let Some(m) = fc.and_then(|a| try_all(&a.accept)) {
            return Some(m);
        }
        for cand in generic_candidates(&lc) {
            if let Some(m) = Self::system_match(&cand.to_ascii_lowercase(), weight, style) {
                return Some(m);
            }
        }
        fc.and_then(|a| try_all(&a.default))
    }

    /// Resolve the primary font for a family list.
    pub fn primary(&mut self, families: &Rc<Vec<String>>, weight: u16, style: FontStyle) -> Arc<Font> {
        let key = (Rc::as_ptr(families) as usize, weight, style);
        if let Some(f) = self.primary_cache.get(&key) {
            return f.clone();
        }
        self.pin(families);
        let mut found = None;
        for fam in families.iter() {
            if let Some(m) = self.lookup_family(fam, weight, style) {
                found = Some(m);
                break;
            }
        }
        // No family in the list exists: the UA default font, which is serif in
        // Chromium and Firefox.
        let (font, b, i) = found
            .or_else(|| self.lookup_family("serif", weight, style))
            .or_else(|| self.lookup_family("sans-serif", weight, style))
            .unwrap_or_else(|| (system().embedded(), weight >= 600, style != FontStyle::Normal));
        let font = font.synthesized(b, i);
        if self.primary_cache.len() > 4096 {
            self.primary_cache.clear();
        }
        self.primary_cache.insert(key, font.clone());
        font
    }

    fn fallback_list(&mut self) -> &Vec<Arc<Font>> {
        if self.fallback_fonts.is_none() {
            let mut v = Vec::new();
            for name in FALLBACK_CHAIN {
                if let Some((f, _, _)) = Self::system_match(&name.to_ascii_lowercase(), 400, FontStyle::Normal) {
                    if !v.iter().any(|x: &Arc<Font>| x.uid == f.uid) {
                        v.push(f);
                    }
                }
            }
            v.push(system().embedded());
            self.fallback_fonts = Some(v);
        }
        self.fallback_fonts.as_ref().unwrap()
    }

    /// Find a font that can render `c`, preferring the family list.
    pub fn font_for_char(&mut self, primary: &Arc<Font>, families: &Rc<Vec<String>>, weight: u16, style: FontStyle, c: char) -> Arc<Font> {
        if primary.has_glyph(c) || c.is_control() || c == '\u{200b}' || c == '\u{ad}' {
            return primary.clone();
        }
        self.pin(families);
        let key = (c, Rc::as_ptr(families) as usize, weight, style != FontStyle::Normal);
        if let Some(f) = self.fallback_cache.get(&key) {
            return f.clone().unwrap_or_else(|| primary.clone());
        }
        let mut found: Option<Arc<Font>> = None;
        // Other unicode-range subsets of the same web font families.
        for fam in families.iter() {
            let lc = fam.to_ascii_lowercase();
            let mut cands: Vec<&WebFace> = self.web.iter().filter(|f| f.family == lc && f.covers(c)).collect();
            cands.sort_by_key(|f| {
                let in_range = weight >= f.weight.0 && weight <= f.weight.1;
                let d = if in_range { 0 } else { (weight as i32 - f.weight.0 as i32).abs().min((weight as i32 - f.weight.1 as i32).abs()) };
                d + if (f.style == FontStyle::Normal) == (style == FontStyle::Normal) { 0 } else { 5000 }
            });
            if let Some(f) = cands.first() {
                let var = f.font.has_wght();
                let bold = weight >= 600 && f.weight.1 < 600 && !var;
                let italic = style != FontStyle::Normal && f.style == FontStyle::Normal;
                let base = if var { f.font.varied(Some(weight), None) } else { f.font.clone() };
                found = Some(base.synthesized(bold, italic));
                break;
            }
        }
        if found.is_none() {
            for fam in families.iter().skip(1) {
                if let Some((f, b, i)) = self.lookup_family(fam, weight, style) {
                    if f.has_glyph(c) {
                        found = Some(f.synthesized(b, i));
                        break;
                    }
                }
            }
        }
        if found.is_none() {
            let is_emoji = is_emoji_char(c);
            let list = self.fallback_list().clone();
            let ordered: Vec<&Arc<Font>> = if is_emoji {
                list.iter().filter(|f| f.has_color).chain(list.iter().filter(|f| !f.has_color)).collect()
            } else {
                list.iter().collect()
            };
            for f in ordered {
                if f.has_glyph(c) {
                    let bold = weight >= 600 && f.weight < 550 && !f.has_color;
                    found = Some(f.synthesized(bold, false));
                    break;
                }
            }
        }
        if found.is_none() {
            // Last resort: scan every system font (once per character).
            let sys = system();
            for info in sys.db.faces() {
                if info.style != fontdb::Style::Normal {
                    continue;
                }
                if let Some(f) = sys.load(info.id) {
                    if f.has_glyph(c) {
                        found = Some(f);
                        break;
                    }
                }
            }
        }
        if self.fallback_cache.len() > 16384 {
            self.fallback_cache.clear();
        }
        self.fallback_cache.insert(key, found.clone());
        found.unwrap_or_else(|| primary.clone())
    }

    pub fn space_width(&mut self, font: &Arc<Font>, size: f32) -> f32 {
        let key = (font.uid, size.to_bits());
        if let Some(w) = self.space_cache.get(&key) {
            return *w;
        }
        let w = font
            .face
            .glyph_index(' ')
            .and_then(|g| font.face.glyph_hor_advance(g))
            .map(|a| a as f32 * font.scale(size))
            .unwrap_or(size * 0.25);
        self.space_cache.insert(key, w);
        w
    }

    /// Shape a piece of text (no line breaking) using font fallback.
    pub fn shape(&mut self, text: &str, families: &Rc<Vec<String>>, weight: u16, style: FontStyle, size: f32, letter_spacing: f32) -> Rc<ShapedText> {
        self.shape_v(text, families, weight, style, size, letter_spacing, 0)
    }

    /// Shape text in a style's font, honoring vertical text orientation and
    /// OpenType features (font-variant-*, font-feature-settings).
    pub fn shape_s(&mut self, text: &str, s: &crate::css::style::ComputedStyle) -> Rc<ShapedText> {
        let feats = s.font_features();
        if feats.is_empty() && !s.font_optical_sizing_none && s.font_variation_settings.is_none() {
            return self.shape_v(text, &s.font_family, s.font_weight, s.font_style, s.font_size, s.letter_spacing, vertical_mode(s));
        }
        let opts = ShapeOpts { feats, opsz: !s.font_optical_sizing_none, vars: s.font_variation_settings.as_ref().map(|v| (**v).clone()).unwrap_or_default() };
        self.shape_vf(text, &s.font_family, s.font_weight, s.font_style, s.font_size, s.letter_spacing, vertical_mode(s), &opts)
    }

    /// `vmode`: 0 horizontal; 1 vertical upright (every glyph advances by
    /// its vertical extent); 2 text-combine-upright (fitted into 1em);
    /// 3 vertical mixed (vertical alternate glyphs).
    #[allow(clippy::too_many_arguments)]
    pub fn shape_v(&mut self, text: &str, families: &Rc<Vec<String>>, weight: u16, style: FontStyle, size: f32, letter_spacing: f32, vmode: u8) -> Rc<ShapedText> {
        self.shape_vf(text, families, weight, style, size, letter_spacing, vmode, &ShapeOpts::default())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn shape_vf(&mut self, text: &str, families: &Rc<Vec<String>>, weight: u16, style: FontStyle, size: f32, letter_spacing: f32, vmode: u8, opts: &ShapeOpts) -> Rc<ShapedText> {
        let feats = &opts.feats[..];
        if vmode == 2 {
            let key_size = size.to_bits() ^ 0x5a5a_0000;
            let primary = self.primary(families, weight, style);
            let key = (primary.uid, key_size, letter_spacing.to_bits(), weight as u32 | ((style != FontStyle::Normal) as u32) << 16 | 2 << 20, Rc::as_ptr(families) as usize, text.to_string());
            if let Some(s) = self.shape_cache.get(&key) {
                return s.clone();
            }
            // Fit the text into one em: shrink it if needed, then center it.
            let mut sh = (*self.shape_v(text.trim(), families, weight, style, size, letter_spacing, 0)).clone();
            if sh.width > size * 1.05 && sh.width > 0.0 {
                sh = (*self.shape_v(text.trim(), families, weight, style, size * size / sh.width, letter_spacing, 0)).clone();
            }
            let pad = ((size - sh.width) / 2.0).max(0.0);
            let n = sh.runs.len();
            for (i, r) in sh.runs.iter_mut().enumerate() {
                if i == 0 {
                    for g in r.glyphs.iter_mut() {
                        g.x += pad;
                    }
                    r.width += pad;
                }
                if i + 1 == n {
                    r.width += pad;
                }
            }
            sh.width += 2.0 * pad;
            let rc = Rc::new(sh);
            self.shape_cache.insert(key, rc.clone());
            return rc;
        }
        let primary = self.primary(families, weight, style);
        let fhash = feats.iter().fold(0u32, |h, (t, v)| h.wrapping_mul(31).wrapping_add(t ^ v.wrapping_mul(0x9e37)))
            ^ opts.vars.iter().fold(0u32, |h, (t, v)| h.wrapping_mul(37).wrapping_add(t ^ v.to_bits()))
            ^ if opts.opsz { 0 } else { 0x5bd1_e995 };
        let key = (primary.uid ^ fhash.rotate_left(7), size.to_bits(), letter_spacing.to_bits(), weight as u32 | ((style != FontStyle::Normal) as u32) << 16 | (vmode as u32) << 20 | ((!feats.is_empty()) as u32) << 28, Rc::as_ptr(families) as usize, text.to_string());
        if let Some(s) = self.shape_cache.get(&key) {
            return s.clone();
        }
        // Split into runs of the same font.
        let mut runs: Vec<(Arc<Font>, usize, usize)> = Vec::new();
        for (i, c) in text.char_indices() {
            let f = if c.is_whitespace() || is_joiner(c) {
                runs.last().map(|r| r.0.clone()).unwrap_or_else(|| primary.clone())
            } else {
                self.font_for_char(&primary, families, weight, style, c)
            };
            match runs.last_mut() {
                Some(r) if r.0.uid == f.uid => r.2 = i + c.len_utf8(),
                _ => runs.push((f, i, i + c.len_utf8())),
            }
        }
        let mut out = ShapedText { runs: Vec::with_capacity(runs.len()), width: 0.0 };
        let mut x = 0.0;
        for (font, s, e) in runs {
            // font-optical-sizing: auto — the opsz axis follows the font size.
            let font = if (font.has_opsz() && opts.opsz) || !opts.vars.is_empty() { font.varied_with(None, if opts.opsz { Some(size) } else { None }, &opts.vars) } else { font };
            let mut run = shape_run(&font, &text[s..e], size, letter_spacing, s as u32, vmode != 0, feats);
            if vmode == 1 {
                // Upright: each glyph takes its vertical extent along the line.
                let sc = font.scale(size);
                let adv_v = (font.ascent + font.descent) * sc;
                let mut pen = 0.0;
                for g in run.glyphs.iter_mut() {
                    let space = text[s..].get(g.cluster as usize - s..).and_then(|t| t.chars().next()).map(|c| c.is_whitespace()).unwrap_or(false);
                    g.x = pen;
                    if !space {
                        g.advance = adv_v + letter_spacing;
                    }
                    pen += g.advance;
                }
                run.width = pen;
            }
            x += run.width;
            out.runs.push(run);
        }
        out.width = x;
        let rc = Rc::new(out);
        if self.shape_cache.len() > 60_000 {
            self.shape_cache.clear();
        }
        self.shape_cache.insert(key, rc.clone());
        rc
    }
}

/// Per-style shaping options.
pub struct ShapeOpts {
    /// OpenType features (tag, value).
    pub feats: Vec<(u32, u32)>,
    /// font-optical-sizing: auto.
    pub opsz: bool,
    /// font-variation-settings.
    pub vars: Vec<(u32, f32)>,
}

impl Default for ShapeOpts {
    fn default() -> Self {
        ShapeOpts { feats: Vec::new(), opsz: true, vars: Vec::new() }
    }
}

fn is_joiner(c: char) -> bool {
    matches!(c, '\u{200d}' | '\u{fe0f}' | '\u{fe0e}' | '\u{20e3}') || ('\u{1f3fb}'..='\u{1f3ff}').contains(&c)
}

pub fn is_emoji_char(c: char) -> bool {
    let u = c as u32;
    (0x1F300..=0x1FAFF).contains(&u) || (0x2600..=0x27BF).contains(&u) || (0x1F000..=0x1F2FF).contains(&u) || (0x2B00..=0x2BFF).contains(&u)
}

#[derive(Clone, Debug)]
pub struct ShapedGlyph {
    pub id: u16,
    pub x: f32,
    pub y: f32,
    pub advance: f32,
    pub cluster: u32,
}

#[derive(Clone)]
pub struct ShapedRun {
    pub font: Arc<Font>,
    pub size: f32,
    pub glyphs: Vec<ShapedGlyph>,
    pub width: f32,
}

#[derive(Clone)]
pub struct ShapedText {
    pub runs: Vec<ShapedRun>,
    pub width: f32,
}

impl ShapedText {
    /// x offset (from start) of the byte index `idx` (for carets / hit tests).
    pub fn x_of_byte(&self, idx: usize) -> f32 {
        let mut x = 0.0;
        for r in &self.runs {
            for g in &r.glyphs {
                if g.cluster as usize >= idx {
                    return x;
                }
                x += g.advance;
            }
        }
        x
    }
    /// Byte index nearest to x.
    pub fn byte_at_x(&self, target: f32, text: &str) -> usize {
        let mut x = 0.0;
        for r in &self.runs {
            for g in &r.glyphs {
                if target < x + g.advance / 2.0 {
                    return g.cluster as usize;
                }
                x += g.advance;
            }
        }
        text.len()
    }
}

/// The shaping mode for text in style `s` (see `FontContext::shape_v`).
pub fn vertical_mode(s: &crate::css::style::ComputedStyle) -> u8 {
    if !s.is_vertical() {
        0
    } else if s.text_combine {
        2
    } else if matches!(s.writing_mode, 3 | 4) || s.text_orientation == 2 {
        0
    } else if s.text_orientation == 1 {
        1
    } else {
        3
    }
}

fn shape_run(font: &Arc<Font>, text: &str, size: f32, letter_spacing: f32, base_cluster: u32, vertical: bool, feats: &[(u32, u32)]) -> ShapedRun {
    let mut buf = rustybuzz::UnicodeBuffer::new();
    buf.push_str(text);
    buf.guess_segment_properties();
    let mut features: Vec<rustybuzz::Feature> = feats.iter().map(|&(t, v)| rustybuzz::Feature::new(rustybuzz::ttf_parser::Tag(t), v, ..)).collect();
    if vertical {
        features.push(rustybuzz::Feature::new(rustybuzz::ttf_parser::Tag::from_bytes(b"vert"), 1, ..));
    }
    // Letter-spacing disables optional ligatures (CSS Text 3 §8.2).
    if letter_spacing != 0.0 {
        for tag in [b"liga", b"clig", b"dlig", b"hlig", b"calt"] {
            features.push(rustybuzz::Feature::new(rustybuzz::ttf_parser::Tag::from_bytes(tag), 0, ..));
        }
    }
    // Shape plans are slow to build (feature lookups in the font's layout
    // tables): reuse one per font, direction, script, language and features.
    type PlanKey = (usize, rustybuzz::Direction, rustybuzz::Script, Option<String>, Vec<(u32, u32)>, bool, bool);
    thread_local! {
        static PLANS: std::cell::RefCell<std::collections::HashMap<PlanKey, (Arc<Font>, Arc<rustybuzz::ShapePlan>)>> = Default::default();
    }
    let lang = buf.language().map(|l| l.as_str().to_string());
    let key: PlanKey = (Arc::as_ptr(font) as usize, buf.direction(), buf.script(), lang, feats.to_vec(), vertical, letter_spacing != 0.0);
    let plan = PLANS.with(|p| p.borrow().get(&key).map(|(_, pl)| pl.clone()));
    let plan = match plan {
        Some(pl) => pl,
        None => {
            let pl = Arc::new(rustybuzz::ShapePlan::new(&font.hb, buf.direction(), Some(buf.script()), buf.language().as_ref(), &features));
            PLANS.with(|p| {
                let mut p = p.borrow_mut();
                if p.len() > 1024 {
                    p.clear();
                }
                p.insert(key, (font.clone(), pl.clone()));
            });
            pl
        }
    };
    let out = rustybuzz::shape_with_plan(&font.hb, &plan, buf);
    let scale = font.scale(size);
    let mut glyphs = Vec::with_capacity(out.len());
    let mut x = 0.0;
    let infos = out.glyph_infos();
    let positions = out.glyph_positions();
    for (info, pos) in infos.iter().zip(positions) {
        let adv = pos.x_advance as f32 * scale + letter_spacing;
        glyphs.push(ShapedGlyph {
            id: info.glyph_id as u16,
            x: x + pos.x_offset as f32 * scale,
            y: -(pos.y_offset as f32) * scale,
            advance: adv,
            cluster: info.cluster + base_cluster,
        });
        x += adv;
    }
    ShapedRun { font: font.clone(), size, glyphs, width: x }
}

/// Unwrap WOFF/WOFF2 containers into raw sfnt data.
pub fn decode_font_container(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() < 4 {
        return None;
    }
    match &bytes[0..4] {
        b"wOFF" => wuff::decompress_woff1(bytes).ok(),
        b"wOF2" => wuff::decompress_woff2(bytes).ok(),
        _ => Some(bytes.to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fontconfig_alias_order() {
        let mut m = HashMap::new();
        parse_fc_aliases(
            r#"<fontconfig><!-- <alias><family>Ignored</family></alias> -->
            <alias><family>Helvetica Neue</family><prefer><family>Inter</family></prefer></alias>
            <alias binding="same"><family>Arial</family><accept><family>Liberation Sans</family></accept></alias></fontconfig>"#,
            &mut m,
        );
        parse_fc_aliases("<alias><family>Helvetica Neue</family><prefer><family>Other</family></prefer><default><family>sans-serif</family></default></alias>", &mut m);
        assert_eq!(m["helvetica neue"].prefer, vec!["Inter", "Other"]);
        assert_eq!(m["helvetica neue"].default, vec!["sans-serif"]);
        assert_eq!(m["arial"].accept, vec!["Liberation Sans"]);
        assert!(!m.contains_key("ignored"));
    }
}

