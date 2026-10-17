// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! The built-in PDF viewer: pages are rasterized with hayro (a pure-Rust PDF
//! renderer) and shown as images on a plain page. Pages render when they
//! come near the viewport and are dropped again far from it, so long files
//! open quickly and stay within a small memory budget.

use std::collections::HashSet;
use std::sync::Arc;

use crate::dom::NodeId;
use crate::paint::ImageData;

/// Pages listed in the viewer (a sanity cap for huge files).
const MAX_PAGES: usize = 2000;
/// Rendering scale: 2 device px per CSS px (PDF points are 4/3 CSS px).
const SCALE: f32 = 2.0 * 4.0 / 3.0;
/// Pages above this many device pixels render at a lower scale.
const MAX_PIXELS: f32 = 2400.0 * 3200.0;

/// An open PDF: the parsed file and which pages have pixels.
pub struct PdfDoc {
    pdf: hayro::hayro_syntax::Pdf,
    pub tag: u64,
    /// Page sizes in CSS px.
    pub sizes: Vec<(f32, f32)>,
    pub rendered: HashSet<usize>,
    /// The page <img> elements, in order (found after the document is parsed).
    pub nodes: Vec<NodeId>,
}

thread_local! {
    /// The PDF being opened on this thread, picked up by the page when its
    /// document is installed.
    pub static PENDING: std::cell::RefCell<Option<PdfDoc>> = const { std::cell::RefCell::new(None) };
}

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

pub fn page_url(tag: u64, i: usize) -> String {
    format!("ohb-pdf:{tag}/{i}")
}

impl PdfDoc {
    /// Rasterize page `i` (None if it can't be drawn).
    pub fn render(&self, i: usize) -> Option<Arc<ImageData>> {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            use hayro::hayro_interpret::InterpreterSettings;
            let pages = self.pdf.pages();
            let page = pages.get(i)?;
            let (w, h) = page.render_dimensions();
            if !(w > 0.0 && h > 0.0) {
                return None;
            }
            let mut scale = SCALE;
            if w * h * scale * scale > MAX_PIXELS {
                scale = (MAX_PIXELS / (w * h)).sqrt();
            }
            let cache = hayro::RenderCache::new();
            let pm = hayro::render(page, &cache, &InterpreterSettings::default(), &hayro::RenderSettings::default(), &hayro::PixmapSettings { x_scale: scale, y_scale: scale, bg_color: hayro::vello_cpu::color::palette::css::WHITE });
            let (pw, ph) = (pm.width() as u32, pm.height() as u32);
            let mut tpm = tiny_skia::Pixmap::new(pw.max(1), ph.max(1))?;
            // Both are premultiplied RGBA8.
            let src = pm.data_as_u8_slice();
            if src.len() != tpm.data().len() {
                return None;
            }
            tpm.data_mut().copy_from_slice(src);
            let css = (w * 4.0 / 3.0, h * 4.0 / 3.0);
            Some(Arc::new(ImageData { pixmap: tpm, width: pw, height: ph, density: pw as f32 / css.0, nat: Some(css), svg_par: None }))
        }));
        r.ok().flatten()
    }
}

/// The viewer document for a PDF response (the file itself waits in `PENDING`).
pub fn viewer_html(url: &str, bytes: &[u8]) -> String {
    use crate::net::html_escape as esc;
    PENDING.with(|p| *p.borrow_mut() = None);
    let tag = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = url::Url::parse(url).ok().and_then(|u| u.path_segments().and_then(|s| s.last().map(|x| String::from_utf8_lossy(&crate::net::percent_decode(x.as_bytes())).into_owned()))).filter(|s| !s.is_empty()).unwrap_or_else(|| "document.pdf".into());
    // A malformed file must not take the tab down.
    let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<PdfDoc, String> {
        let pdf = hayro::hayro_syntax::Pdf::new(bytes.to_vec()).map_err(|e| format!("{e:?}"))?;
        let sizes: Vec<(f32, f32)> = pdf.pages().iter().take(MAX_PAGES).map(|p| {
            let (w, h) = p.render_dimensions();
            (w * 4.0 / 3.0, h * 4.0 / 3.0)
        }).collect();
        Ok(PdfDoc { pdf, tag, sizes, rendered: HashSet::new(), nodes: Vec::new() })
    }))
    .unwrap_or_else(|_| Err("the file couldn't be read".into()));
    let mut body = String::new();
    let note;
    match opened {
        Ok(doc) => {
            for (i, (w, h)) in doc.sizes.iter().enumerate() {
                body.push_str(&format!("<img class=pg src=\"{}\" width=\"{:.0}\" height=\"{:.0}\" alt=\"Page {}\">", page_url(tag, i), w, h, i + 1));
            }
            let total = doc.sizes.len();
            note = format!("{} page{}", total, if total == 1 { "" } else { "s" });
            if total == 0 {
                body.push_str("<p class=err>This PDF has no pages that can be shown.</p>");
            }
            PENDING.with(|p| *p.borrow_mut() = Some(doc));
        }
        Err(e) => {
            note = String::new();
            body.push_str(&format!("<p class=err>This PDF couldn't be opened ({}).</p>", esc(&e)));
        }
    }
    format!(
        r#"<!doctype html><html><head><meta charset=utf-8><title>{name}</title><style>
html {{ background: #2a2a2e; }}
body {{ margin: 0; font: 13px system-ui, sans-serif; color: #e8e8ec; }}
.bar {{ position: sticky; top: 0; z-index: 1; display: flex; gap: 16px; align-items: center; padding: 10px 18px; background: #111114; border-bottom: 1px solid #2f2f35; }}
.bar b {{ font-weight: 600; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }}
.bar span {{ color: #9a9aa3; }}
.pages {{ display: flex; flex-direction: column; align-items: center; gap: 14px; padding: 18px 12px 40px; }}
img.pg {{ display: block; max-width: 100%; height: auto; background: white; box-shadow: 0 2px 10px rgba(0,0,0,.5); }}
.err {{ color: #ffb3a0; }}
</style></head><body><div class=bar><b>{name}</b><span>{note}</span></div><div class=pages>{body}</div></body></html>"#,
        name = esc(&name),
        note = esc(&note),
    )
}
