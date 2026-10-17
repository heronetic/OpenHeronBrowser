// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! HeronShell chrome: geometry, drawing helpers, icons and text fields.

use std::rc::Rc;
use std::sync::Arc;

use crate::css::style::{BorderStyle, FontStyle};
use crate::css::values::Color;
use crate::paint::{GlyphRun, ImageData, Item, Radii, Rect};
use crate::text::FontContext;

pub const TAB_H: f32 = 40.0;
pub const TOOL_H: f32 = 44.0;
pub const CHROME_H: f32 = TAB_H + TOOL_H;

pub struct Palette {
    pub frame: Color,
    pub tab_active: Color,
    pub tab_hover: Color,
    pub toolbar: Color,
    pub omni: Color,
    pub omni_hot: Color,
    pub omni_focus: Color,
    pub omni_focus_ring: Color,
    pub text: Color,
    pub text_dim: Color,
    pub icon: Color,
    pub icon_disabled: Color,
    pub hover: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub separator: Color,
    pub edge: Color,
    pub popup: Color,
    pub popup_hover: Color,
    pub popup_edge: Color,
    pub link: Color,
    pub warn: Color,
    pub chip: Color,
    pub tab_mark: Color,
}

/// The light theme.
pub const LIGHT: Palette = Palette {
    frame: Color::rgb(222, 227, 234),
    tab_active: Color::rgb(255, 255, 255),
    tab_hover: Color::rgb(236, 239, 244),
    toolbar: Color::rgb(255, 255, 255),
    omni: Color::rgb(239, 242, 246),
    omni_hot: Color::rgb(232, 236, 241),
    omni_focus: Color::rgb(255, 255, 255),
    omni_focus_ring: Color::rgb(47, 111, 237),
    text: Color::rgb(32, 36, 46),
    text_dim: Color::rgb(95, 101, 114),
    icon: Color::rgb(70, 76, 89),
    icon_disabled: Color::rgb(180, 185, 194),
    hover: Color::rgb(232, 235, 240),
    accent: Color::rgb(47, 111, 237),
    on_accent: Color::rgb(255, 255, 255),
    separator: Color::rgb(200, 205, 214),
    edge: Color::rgb(218, 222, 229),
    popup: Color::rgb(255, 255, 255),
    popup_hover: Color::rgb(236, 241, 252),
    popup_edge: Color::rgb(210, 214, 222),
    link: Color::rgb(26, 79, 208),
    warn: Color::rgb(180, 90, 20),
    chip: Color::rgb(222, 228, 236),
    tab_mark: Color::TRANSPARENT,
};

/// The default theme: a black (#0B0B0C) shell with the heron-gold (#F5C842) accent.
pub const BLACK: Palette = Palette {
    frame: Color::rgb(11, 11, 12),
    tab_active: Color::rgb(11, 11, 12),
    tab_hover: Color::rgb(20, 20, 23),
    toolbar: Color::rgb(11, 11, 12),
    omni: Color::rgb(22, 22, 25),
    omni_hot: Color::rgb(30, 30, 34),
    omni_focus: Color::rgb(26, 26, 30),
    omni_focus_ring: Color::rgb(245, 200, 66),
    text: Color::rgb(244, 244, 246),
    text_dim: Color::rgb(140, 140, 150),
    icon: Color::rgb(226, 226, 232),
    icon_disabled: Color::rgb(78, 78, 86),
    hover: Color::rgb(34, 34, 38),
    accent: Color::rgb(245, 200, 66),
    on_accent: Color::rgb(11, 11, 12),
    separator: Color::rgb(44, 44, 50),
    edge: Color::rgb(28, 28, 32),
    popup: Color::rgb(18, 18, 21),
    popup_hover: Color::rgb(40, 40, 46),
    popup_edge: Color::rgb(48, 48, 54),
    link: Color::rgb(138, 180, 248),
    warn: Color::rgb(255, 140, 80),
    chip: Color::rgb(44, 44, 50),
    tab_mark: Color::rgb(245, 200, 66),
};

thread_local! {
    static THEME: std::cell::Cell<(u64, bool)> = const { std::cell::Cell::new((0, true)) };
}

/// The current theme's palette (Settings → Theme; black by default).
pub fn pal() -> &'static Palette {
    let g = crate::prefs::GENERATION.load(std::sync::atomic::Ordering::Relaxed);
    let dark = THEME.with(|t| {
        let (gen, dark) = t.get();
        if gen == g {
            dark
        } else {
            let d = crate::prefs::dark_theme();
            t.set((g, d));
            d
        }
    });
    if dark {
        &BLACK
    } else {
        &LIGHT
    }
}

/// A UI drawing list in CSS px (scaled at raster time).
pub struct Ui<'f> {
    pub items: Vec<Item>,
    pub fonts: &'f mut FontContext,
    pub family: Rc<Vec<String>>,
}

impl<'f> Ui<'f> {
    pub fn new(fonts: &'f mut FontContext, family: Rc<Vec<String>>) -> Self {
        Ui { items: Vec::with_capacity(256), fonts, family }
    }

    pub fn rect(&mut self, r: Rect, c: Color) {
        self.items.push(Item::Rect { r, color: c });
    }

    pub fn rrect(&mut self, r: Rect, rad: f32, c: Color) {
        self.items.push(Item::RRect { r, radii: Radii::all(rad), color: c });
    }

    pub fn rrect4(&mut self, r: Rect, radii: [f32; 4], c: Color) {
        self.items.push(Item::RRect { r, radii: Radii { x: radii, y: radii }, color: c });
    }

    pub fn border(&mut self, r: Rect, w: f32, rad: f32, c: Color) {
        self.items.push(Item::Border { r, widths: [w; 4], colors: [c; 4], styles: [BorderStyle::Solid; 4], radii: Radii::all(rad) });
    }

    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, w: f32, c: Color) {
        self.items.push(Item::Line { x0, y0, x1, y1, width: w, color: c, style: BorderStyle::Solid });
    }

    pub fn shadow(&mut self, r: Rect, rad: f32, blur: f32, c: Color) {
        self.items.push(Item::Shadow { r, radii: Radii::all(rad), blur, spread: 0.0, color: c, inset: false, hole: None });
    }

    pub fn clip(&mut self, r: Rect) {
        self.items.push(Item::PushClip { r, radii: None });
    }

    pub fn unclip(&mut self) {
        self.items.push(Item::PopClip);
    }

    pub fn image(&mut self, r: Rect, img: Arc<ImageData>) {
        self.items.push(Item::Image { r, img, radii: None, pixelated: false });
    }

    pub fn measure(&mut self, text: &str, size: f32, weight: u16) -> f32 {
        self.fonts.shape(text, &self.family, weight, FontStyle::Normal, size, 0.0).width
    }

    /// Draw text with its left edge at x and vertically centered in [y, y+h).
    pub fn text(&mut self, text: &str, x: f32, y: f32, h: f32, size: f32, weight: u16, c: Color) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let sh = self.fonts.shape(text, &self.family, weight, FontStyle::Normal, size, 0.0);
        let font = self.fonts.primary(&self.family, weight, FontStyle::Normal);
        let sc = font.scale(size);
        let (a, d) = (font.ascent * sc, font.descent * sc);
        let baseline = (y + (h - (a + d)) / 2.0 + a).round();
        let mut px = 0.0;
        for r in &sh.runs {
            let glyphs: Vec<(u16, f32, f32)> = r.glyphs.iter().map(|g| (g.id, px + g.x, g.y)).collect();
            px += r.width;
            self.items.push(Item::Text { x, y: baseline, run: Arc::new(GlyphRun { font: r.font.clone(), size: r.size, glyphs }), color: c });
        }
        sh.width
    }

    /// Text clipped to max width with an ellipsis.
    pub fn text_ellipsis(&mut self, text: &str, x: f32, y: f32, h: f32, max_w: f32, size: f32, weight: u16, c: Color) {
        let w = self.measure(text, size, weight);
        if w <= max_w {
            self.text(text, x, y, h, size, weight, c);
            return;
        }
        let ell = "…";
        let ew = self.measure(ell, size, weight);
        let mut s = String::new();
        for ch in text.chars() {
            let mut t = s.clone();
            t.push(ch);
            if self.measure(&t, size, weight) + ew > max_w {
                break;
            }
            s = t;
        }
        s.push_str(ell);
        self.text(&s, x, y, h, size, weight, c);
    }
}

// ------------------------------------------------------------------ icons

pub enum Icon {
    Back,
    Forward,
    Reload,
    Stop,
    Home,
    Menu,
    Close,
    Plus,
    Lock,
    Info,
    Warning,
    Search,
    Up,
    Down,
    Globe,
    Shield,
}

pub fn draw_icon(ui: &mut Ui, icon: Icon, cx: f32, cy: f32, c: Color) {
    let w = 1.8;
    match icon {
        Icon::Back | Icon::Forward => {
            let s = if matches!(icon, Icon::Back) { -1.0 } else { 1.0 };
            ui.line(cx - 7.0 * s, cy, cx + 7.0 * s, cy, w, c);
            ui.line(cx + 7.0 * s, cy, cx + 1.5 * s, cy - 5.5, w, c);
            ui.line(cx + 7.0 * s, cy, cx + 1.5 * s, cy + 5.5, w, c);
        }
        Icon::Up | Icon::Down => {
            let s = if matches!(icon, Icon::Up) { -1.0 } else { 1.0 };
            ui.line(cx - 5.0, cy - 2.5 * s, cx, cy + 2.5 * s, w, c);
            ui.line(cx, cy + 2.5 * s, cx + 5.0, cy - 2.5 * s, w, c);
        }
        Icon::Reload => {
            // A circular arrow: a 300° arc open at the top right, with a
            // solid arrowhead pointing clockwise.
            let r = 6.0;
            let gap_mid = -std::f32::consts::FRAC_PI_4 - 0.25;
            let a0 = gap_mid + 0.5;
            let a1 = gap_mid - 0.62 + std::f32::consts::TAU;
            let n = 24;
            let mut prev = (cx + r * a0.cos(), cy + r * a0.sin());
            for i in 1..=n {
                let a = a0 + (a1 - a0) * i as f32 / n as f32;
                let p = (cx + r * a.cos(), cy + r * a.sin());
                ui.line(prev.0, prev.1, p.0, p.1, 2.0, c);
                prev = p;
            }
            // Arrowhead at the end of the arc, along the clockwise tangent.
            let (ex, ey) = (cx + r * a1.cos(), cy + r * a1.sin());
            let (tx, ty) = (-a1.sin(), a1.cos());
            let (rx, ry) = (a1.cos(), a1.sin());
            let tip = (ex + tx * 4.2, ey + ty * 4.2);
            let b1 = (ex + rx * 3.6 - tx * 0.6, ey + ry * 3.6 - ty * 0.6);
            let b2 = (ex - rx * 3.6 - tx * 0.6, ey - ry * 3.6 - ty * 0.6);
            let mut pb = tiny_skia::PathBuilder::new();
            pb.move_to(tip.0, tip.1);
            pb.line_to(b1.0, b1.1);
            pb.line_to(b2.0, b2.1);
            pb.close();
            if let Some(path) = pb.finish() {
                ui.items.push(Item::PushClipPath(Arc::new(path)));
                ui.rect(Rect::new(cx - 12.0, cy - 12.0, 24.0, 24.0), c);
                ui.items.push(Item::PopClip);
            }
        }
        Icon::Stop | Icon::Close => {
            let k = if matches!(icon, Icon::Close) { 4.0 } else { 6.0 };
            ui.line(cx - k, cy - k, cx + k, cy + k, w, c);
            ui.line(cx - k, cy + k, cx + k, cy - k, w, c);
        }
        Icon::Plus => {
            ui.line(cx - 6.0, cy, cx + 6.0, cy, w, c);
            ui.line(cx, cy - 6.0, cx, cy + 6.0, w, c);
        }
        Icon::Home => {
            ui.line(cx - 7.5, cy - 0.5, cx, cy - 7.0, w, c);
            ui.line(cx, cy - 7.0, cx + 7.5, cy - 0.5, w, c);
            ui.line(cx - 5.5, cy - 2.0, cx - 5.5, cy + 6.0, w, c);
            ui.line(cx + 5.5, cy - 2.0, cx + 5.5, cy + 6.0, w, c);
            ui.line(cx - 5.5, cy + 6.0, cx + 5.5, cy + 6.0, w, c);
        }
        Icon::Menu => {
            for dy in [-5.0, 0.0, 5.0] {
                ui.rrect(Rect::new(cx - 1.6, cy + dy - 1.6, 3.2, 3.2), 1.6, c);
            }
        }
        Icon::Lock => {
            ui.border(Rect::new(cx - 3.5, cy - 7.0, 7.0, 8.0), 1.5, 3.5, c);
            ui.rrect(Rect::new(cx - 5.5, cy - 2.0, 11.0, 8.5), 1.5, c);
        }
        Icon::Info => {
            ui.border(Rect::new(cx - 7.0, cy - 7.0, 14.0, 14.0), 1.5, 7.0, c);
            ui.rect(Rect::new(cx - 0.75, cy - 1.5, 1.5, 5.0), c);
            ui.rect(Rect::new(cx - 0.75, cy - 4.5, 1.5, 1.5), c);
        }
        Icon::Warning => {
            ui.line(cx, cy - 7.0, cx - 7.5, cy + 6.0, w, c);
            ui.line(cx - 7.5, cy + 6.0, cx + 7.5, cy + 6.0, w, c);
            ui.line(cx + 7.5, cy + 6.0, cx, cy - 7.0, w, c);
            ui.rect(Rect::new(cx - 0.75, cy - 3.0, 1.5, 5.0), c);
            ui.rect(Rect::new(cx - 0.75, cy + 3.0, 1.5, 1.5), c);
        }
        Icon::Search => {
            ui.border(Rect::new(cx - 6.0, cy - 6.0, 10.0, 10.0), 1.6, 5.0, c);
            ui.line(cx + 2.5, cy + 2.5, cx + 6.5, cy + 6.5, 2.0, c);
        }
        Icon::Shield => {
            let pts = [(cx, cy - 7.5), (cx + 6.5, cy - 5.0), (cx + 6.0, cy + 1.0), (cx + 3.5, cy + 4.8), (cx, cy + 7.5), (cx - 3.5, cy + 4.8), (cx - 6.0, cy + 1.0), (cx - 6.5, cy - 5.0), (cx, cy - 7.5)];
            for k in 1..pts.len() {
                ui.line(pts[k - 1].0, pts[k - 1].1, pts[k].0, pts[k].1, 1.6, c);
            }
        }
        Icon::Globe => {
            ui.border(Rect::new(cx - 7.0, cy - 7.0, 14.0, 14.0), 1.4, 7.0, c);
            ui.border(Rect::new(cx - 3.0, cy - 7.0, 6.0, 14.0), 1.2, 3.0, c);
            ui.line(cx - 7.0, cy, cx + 7.0, cy, 1.2, c);
        }
    }
}

// ------------------------------------------------------------- text field

#[derive(Default, Clone)]
pub struct TextField {
    pub text: String,
    pub caret: usize,
    pub anchor: usize,
}

impl TextField {
    pub fn set(&mut self, s: &str) {
        self.text = s.to_string();
        self.caret = self.text.len();
        self.anchor = 0;
    }
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.text.len();
    }
    pub fn selection(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }
    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }
    pub fn selected_text(&self) -> String {
        let (a, b) = self.selection();
        self.text[a..b].to_string()
    }
    fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        let (a, b) = self.selection();
        self.text.replace_range(a..b, "");
        self.caret = a;
        self.anchor = a;
        true
    }
    pub fn insert(&mut self, s: &str) {
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        self.delete_selection();
        self.text.insert_str(self.caret, &s);
        self.caret += s.len();
        self.anchor = self.caret;
    }
    fn prev(&self, i: usize) -> usize {
        self.text[..i].char_indices().last().map(|(p, _)| p).unwrap_or(0)
    }
    fn next(&self, i: usize) -> usize {
        self.text[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i)
    }
    fn word_prev(&self, i: usize) -> usize {
        let t = self.text[..i].trim_end_matches(|c: char| !c.is_alphanumeric());
        t.rfind(|c: char| !c.is_alphanumeric()).map(|p| p + 1).unwrap_or(0)
    }
    fn word_next(&self, i: usize) -> usize {
        let rest = &self.text[i..];
        let skip = rest.find(|c: char| c.is_alphanumeric()).unwrap_or(rest.len());
        let rest2 = &rest[skip..];
        let w = rest2.find(|c: char| !c.is_alphanumeric()).unwrap_or(rest2.len());
        i + skip + w
    }
    pub fn backspace(&mut self, word: bool) {
        if self.delete_selection() {
            return;
        }
        if self.caret > 0 {
            let p = if word { self.word_prev(self.caret) } else { self.prev(self.caret) };
            self.text.replace_range(p..self.caret, "");
            self.caret = p;
            self.anchor = p;
        }
    }
    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.caret < self.text.len() {
            let n = self.next(self.caret);
            self.text.replace_range(self.caret..n, "");
        }
    }
    pub fn left(&mut self, shift: bool, word: bool) {
        if !shift && self.has_selection() {
            let (a, _) = self.selection();
            self.caret = a;
        } else {
            self.caret = if word { self.word_prev(self.caret) } else { self.prev(self.caret) };
        }
        if !shift {
            self.anchor = self.caret;
        }
    }
    pub fn right(&mut self, shift: bool, word: bool) {
        if !shift && self.has_selection() {
            let (_, b) = self.selection();
            self.caret = b;
        } else {
            self.caret = if word { self.word_next(self.caret) } else { self.next(self.caret) };
        }
        if !shift {
            self.anchor = self.caret;
        }
    }
    pub fn home(&mut self, shift: bool) {
        self.caret = 0;
        if !shift {
            self.anchor = 0;
        }
    }
    pub fn end(&mut self, shift: bool) {
        self.caret = self.text.len();
        if !shift {
            self.anchor = self.caret;
        }
    }
}

/// Draw an editable text field's content (selection + text + caret).
#[allow(clippy::too_many_arguments)]
pub fn draw_field(ui: &mut Ui, f: &TextField, r: Rect, size: f32, focused: bool, caret_on: bool, placeholder: &str, color: Color) {
    ui.clip(r);
    let full_w = ui.measure(&f.text, size, 400);
    let caret_x = ui.measure(&f.text[..f.caret.min(f.text.len())], size, 400);
    let scroll = if focused && caret_x > r.w - 4.0 { caret_x - r.w + 4.0 } else { 0.0 };
    let x0 = r.x - scroll;
    if focused && f.has_selection() {
        let (a, b) = f.selection();
        let xa = ui.measure(&f.text[..a], size, 400);
        let xb = ui.measure(&f.text[..b], size, 400);
        ui.rect(Rect::new(x0 + xa, r.y + 4.0, xb - xa, r.h - 8.0), Color::rgba(47, 111, 237, 70));
    }
    if f.text.is_empty() && !placeholder.is_empty() {
        ui.text(placeholder, r.x, r.y, r.h, size, 400, pal().text_dim);
    } else {
        ui.text(&f.text, x0, r.y, r.h, size, 400, color);
    }
    if focused && caret_on && !f.has_selection() {
        ui.rect(Rect::new((x0 + caret_x).round(), r.y + 6.0, 1.5, r.h - 12.0), pal().text);
    }
    let _ = full_w;
    ui.unclip();
}

pub fn caret_from_x(ui: &mut Ui, f: &TextField, size: f32, rel_x: f32) -> usize {
    let mut best = 0;
    let mut best_d = f32::INFINITY;
    let mut idxs: Vec<usize> = f.text.char_indices().map(|(i, _)| i).collect();
    idxs.push(f.text.len());
    for i in idxs {
        let x = ui.measure(&f.text[..i], size, 400);
        let d = (x - rel_x).abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

// ------------------------------------------------------------------ app icon

/// The OpenHeronBrowser icon at `size` px: a heron hatching from a
/// wireframe-globe egg, gold on black. Each size range has its own art
/// (fewer wires below 64 px, a simpler mark at favicon sizes).
pub fn render_icon(size: u32) -> tiny_skia::Pixmap {
    const FULL: &str = include_str!("../../assets/icon.svg");
    const SMALL: &str = include_str!("../../assets/icon-small.svg");
    const FAVICON: &str = include_str!("../../assets/icon-favicon.svg");
    let svg = if size <= 24 { FAVICON } else if size < 64 { SMALL } else { FULL };
    let mut pm = tiny_skia::Pixmap::new(size.max(1), size.max(1)).unwrap();
    let opt = resvg::usvg::Options::default();
    if let Ok(tree) = resvg::usvg::Tree::from_str(svg, &opt) {
        let k = size as f32 / tree.size().width().max(1.0);
        resvg::render(&tree, tiny_skia::Transform::from_scale(k, k), &mut pm.as_mut());
    }
    pm
}
/// The demo-warning mark (a distressed heron bursting from its egg) at
/// `size` px, for the startup prompt.
pub fn render_warning_icon(size: u32) -> tiny_skia::Pixmap {
    const SVG: &str = include_str!("../../assets/warning.svg");
    let mut pm = tiny_skia::Pixmap::new(size.max(1), size.max(1)).unwrap();
    if let Ok(tree) = resvg::usvg::Tree::from_str(SVG, &resvg::usvg::Options::default()) {
        let k = size as f32 / tree.size().width().max(1.0);
        resvg::render(&tree, tiny_skia::Transform::from_scale(k, k), &mut pm.as_mut());
    }
    pm
}
