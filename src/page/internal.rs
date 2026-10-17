// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Built-in pages (ohb://newtab, ohb://history, ohb://about) and error pages.

use crate::net::html_escape;
use std::sync::OnceLock;

/// The heron app icon at `px` pixels as a PNG data: URL (rendered once per size).
fn icon_png_url(px: u32) -> String {
    static ICONS: OnceLock<std::sync::Mutex<std::collections::HashMap<u32, String>>> = OnceLock::new();
    let m = ICONS.get_or_init(Default::default);
    if let Some(u) = m.lock().unwrap().get(&px) {
        return u.clone();
    }
    let pm = crate::shell::chrome::render_icon(px);
    let u = match pm.encode_png() {
        Ok(png) => format!("data:image/png;base64,{}", crate::net::base64_encode(&png)),
        Err(_) => String::new(),
    };
    m.lock().unwrap().insert(px, u.clone());
    u
}

fn icon_url() -> String {
    icon_png_url(112)
}

const STYLE: &str = r#"
:root { color-scheme: light; }
body { margin: 0; font-family: system-ui, sans-serif; background: #f6f8fb; color: #1f2430; }
.wrap { max-width: 760px; margin: 0 auto; padding: 44px 24px; }
h1 { font-size: 30px; font-weight: 600; margin: 0 0 8px 0; letter-spacing: -0.5px; }
.sub { color: #5d6575; margin-bottom: 28px; }
.logo { display: flex; align-items: center; gap: 14px; margin-bottom: 24px; }
.mark { width: 56px; height: 56px; display: block; }
form.search { display: flex; gap: 8px; margin: 18px 0 30px; }
form.search input { flex: 1; height: 46px; font-size: 16px; padding: 0 18px; border-radius: 23px; border: 1px solid #cfd6e4; background: white; }
form.search button { height: 46px; padding: 0 22px; border-radius: 23px; border: none; background: #2f6fed; color: white; font-size: 15px; font-weight: 600; }
.tiles { display: grid; grid-template-columns: repeat(auto-fill, minmax(200px, 1fr)); gap: 12px; }
.tile { display: block; background: white; border: 1px solid #e3e8f0; border-radius: 12px; padding: 14px 16px; color: #1f2430; text-decoration: none; }
.tile b { display: block; font-size: 14px; margin-bottom: 4px; }
.tile span { color: #6b7385; font-size: 12px; }
.card { background: white; border: 1px solid #e3e8f0; border-radius: 12px; padding: 18px 22px; margin: 14px 0; }
.err { font-size: 64px; margin-bottom: 8px; }
code { background: #eef1f6; padding: 2px 6px; border-radius: 4px; }
.badge { display: inline-block; vertical-align: 5px; margin-left: 10px; padding: 3px 9px; border-radius: 999px; font-size: 11px; font-weight: 700; letter-spacing: 0.08em; text-transform: uppercase; background: #F5C842; color: #0B0B0C; }
.notice { display: flex; gap: 14px; align-items: flex-start; margin: 0 0 2px; padding: 16px 18px; border: 1.5px solid #ef6f66; border-radius: 14px; background: rgba(239, 111, 102, 0.07); }
.notice .ic { flex: none; width: 26px; height: 26px; border-radius: 50%; background: #ef6f66; color: #fff; font-weight: 800; font-size: 15px; line-height: 26px; text-align: center; }
.notice b { display: block; font-size: 14.5px; margin-bottom: 4px; color: #d4554c; }
.notice p { margin: 0; font-size: 13.5px; line-height: 1.5; color: #4a4f5c; }
.foot { margin-top: 30px; font-size: 12px; color: #7a8293; }
.hero { text-align: center; margin: 0 0 22px; }
.hero .mark-lg { display: block; width: 112px; height: 112px; margin: 0 auto 18px; border-radius: 25px; box-shadow: 0 0 0 1px rgba(15, 18, 25, 0.14), 0 10px 30px rgba(15, 18, 25, 0.10); }
.hero h1 { font-size: 34px; margin: 0 0 6px; }
.hero .sub { margin: 0; }
.notice b.risk { display: inline; font-size: inherit; margin: 0; color: inherit; }
ul.hist { list-style: none; padding: 0; }
ul.hist li { padding: 8px 0; border-bottom: 1px solid #edf0f5; }
ul.hist a { color: #1a4fd0; text-decoration: none; }
.muted { color: #7a8293; font-size: 13px; }
table.kv td, table.kv th { padding: 4px 16px 4px 0; vertical-align: top; }
a.btn { display: inline-block; padding: 7px 14px; border-radius: 8px; background: #2f6fed; color: white; text-decoration: none; font-size: 13px; margin: 4px 0; }
a.btn.on { background: #1f2430; }
form.row { display: flex; gap: 8px; margin: 12px 0 4px; }
form.row input { flex: 1; height: 34px; font-size: 14px; padding: 0 12px; border-radius: 8px; border: 1px solid #cfd6e4; }
form.row button { height: 36px; padding: 0 16px; border-radius: 8px; border: none; background: #2f6fed; color: white; font-weight: 600; }
"#;

/// The black theme for browser pages.
const DARK: &str = r#"
:root { color-scheme: dark; }
body { background: #0B0B0C; color: #ececef; }
.sub, .muted, .tile span { color: #9a9aa3; }
.tile, .card { background: #111114; border-color: #26262b; color: #ececef; }
form.search input, form.row input { background: #111114; border-color: #33333a; color: #ececef; }
form.search button, form.row button, a.btn { background: #F5C842; color: #0B0B0C; }
a.btn.on { background: #ececef; color: #0B0B0C; }
.notice { border-color: rgba(255, 122, 112, 0.6); background: rgba(255, 122, 112, 0.07); }
.notice .ic { background: #ff7a70; color: #0B0B0C; }
.notice b { color: #ff8f86; }
.notice p { color: #c4c4cc; }
.foot { color: #6e6e78; }
.hero .mark-lg { box-shadow: 0 0 0 1px rgba(255, 255, 255, 0.13), 0 10px 34px rgba(245, 200, 66, 0.06); }
.notice b.risk { color: #f4f4f5; }
code { background: #1c1c20; }
ul.hist li { border-color: #1c1c20; }
ul.hist a, a { color: #8ab4f8; }
"#;

fn page(title: &str, body: &str) -> String {
    let dark = if crate::prefs::dark_theme() { DARK } else { "" };
    format!("<!doctype html><html><head><meta charset=utf-8><title>{}</title><link rel=icon href=\"{}\"><style>{}{}</style></head><body><div class=wrap>{}</div></body></html>", html_escape(title), icon_png_url(32), STYLE, dark, body)
}

/// The demo warning shown on the default page and in About.
const DEMO_NOTICE: &str = r#"<div class=notice role=note><div class=ic>!</div><div><b>This browser is still hatching.</b><p>OpenHeronBrowser is an early demo of a new web engine. Some sites won't work, and pages aren't sandboxed, so don't enter passwords or other sensitive information. <b class=risk>Browse at your own risk.</b></p></div></div>"#;

pub fn newtab() -> String {
    let tiles = [
        ("Heronetic", "heronetic.com", "https://heronetic.com/"),
        ("Google", "google.com", "https://www.google.com/"),
        ("Wikipedia", "wikipedia.org", "https://en.wikipedia.org/"),
        ("Hacker News", "news.ycombinator.com", "https://news.ycombinator.com/"),
        ("Weather", "weather.com", "https://weather.com/"),
        ("BBC", "bbc.com", "https://www.bbc.com/"),
        ("Rust", "rust-lang.org", "https://www.rust-lang.org/"),
        ("History", "ohb://history", "ohb://history"),
        ("About OHB", "ohb://about", "ohb://about"),
    ];
    let mut t = String::new();
    for (name, sub, url) in tiles {
        t.push_str(&format!("<a class=tile href=\"{url}\"><b>{name}</b><span>{sub}</span></a>"));
    }
    let icon = icon_png_url(224);
    page(
        "New Tab",
        &format!(
            r#"<div class=hero><img class=mark-lg src="{icon}" alt=""><h1>OpenHeronBrowser<span class=badge>Demo</span></h1><div class=sub>A from-scratch web engine by Heronetic</div></div>
{DEMO_NOTICE}
<form class=search action="{action}" method=get><input name=q placeholder="{ph}"><button type=submit>Search</button></form>
<div class=tiles>{t}</div>"#,
            action = crate::prefs::engine().action(),
            ph = html_escape(&crate::prefs::placeholder()),
        ),
    )
}

pub fn about(stats: &str) -> String {
    let icon = icon_url();
    let ver = crate::net::VERSION;
    page(
        "About OpenHeronBrowser",
        &format!(
            r#"<div class=logo><img class=mark src="{icon}" alt=""><div><h1>OpenHeronBrowser {ver}<span class=badge>Demo</span></h1><div class=sub>A demonstration of the HeronBrowser engine by Heronetic</div></div></div>
{DEMO_NOTICE}
<div class=card><b>Engine components</b>
<ul>
<li><b>HeronNet</b> — HTTP/1.1 client over rustls (TLS 1.2/1.3), connection pooling, gzip/deflate/brotli, cookies with third‑party blocking, in‑memory cache</li>
<li><b>HeronParse</b> — HTML5‑style tokenizer and tree builder</li>
<li><b>HeronStyle</b> — CSS tokenizer, selectors level 4, media queries, custom properties, calc(), cascade with rule hashing and style sharing</li>
<li><b>HeronLayout</b> — block, inline, float, flexbox, grid, table and positioned layout</li>
<li><b>HeronText</b> — system + web fonts (TTF/OTF/WOFF/WOFF2), fallback, shaping</li>
<li><b>HeronPaint</b> — display lists with culling, software rasterizer, glyph cache, color emoji</li>
<li><b>HeronScript</b> — JavaScript via the Boa engine with a DOM bridge</li>
<li><b>HeronShell</b> — tabs, history, omnibox, find‑in‑page, zoom</li>
</ul></div>
<div class=card><b>Security</b><ul>
<li>Memory-safe Rust engine; every tab runs on its own thread and a crashed tab can't take down the browser</li>
<li>Not sandboxed: tabs share the browser process, unlike the per-site process sandbox of mainstream browsers</li>
<li>Certificate validation with Mozilla and OS roots; mixed content is auto-upgraded</li>
<li>Third‑party cookies blocked, HttpOnly/Secure/SameSite honored, cookies kept in memory only</li>
<li>Script runtime limits (recursion / loop budgets), no local file access from web pages</li>
</ul></div>
<div class=card><b>Session</b><table class=kv>{stats}</table></div>
<p class=muted><a href="https://heronetic.com/">heronetic.com</a></p>
<p class=muted>Keyboard: Ctrl/⌘+T new tab · Ctrl/⌘+W close · Ctrl/⌘+L address bar · Ctrl/⌘+F find · Ctrl/⌘+R reload · Alt+←/→ history · Ctrl/⌘ +/−/0 zoom</p>"#
        ),
    )
}

pub fn history(entries: &[(String, String)]) -> String {
    let mut list = String::new();
    for (url, title) in entries.iter().rev().take(500) {
        let t = if title.is_empty() { url.as_str() } else { title.as_str() };
        list.push_str(&format!("<li><a href=\"{}\">{}</a><div class=muted>{}</div></li>", html_escape(url), html_escape(t), html_escape(url)));
    }
    if list.is_empty() {
        list = "<li class=muted>No history yet.</li>".into();
    }
    page("History", &format!("<h1>History</h1><div class=sub>This session's browsing history</div><ul class=hist>{list}</ul>"))
}

pub fn error(url: &str, err: &str) -> String {
    let (icon, title, hint) = if err.contains("TLS") || err.contains("CERT") || err.contains("certificate") {
        ("🔒", "Your connection is not private", "The site's certificate could not be verified, so OpenHeronBrowser stopped loading it to protect your data.")
    } else if err.contains("NAME_NOT_RESOLVED") {
        ("🌐", "This site can't be reached", "The server's DNS address could not be found. Check the address for typos.")
    } else if err.contains("BLOCKED") {
        ("⛔", "This page was blocked", "The request was refused.")
    } else if err.contains("TIMED_OUT") {
        ("⏳", "This site took too long to respond", "The connection timed out.")
    } else {
        ("⚠️", "This page isn't working", "Something went wrong while loading the page.")
    };
    page(
        title,
        &format!(
            "<div class=err>{icon}</div><h1>{}</h1><div class=sub>{}</div><div class=card><b>{}</b><br><code>{}</code></div><p><a href=\"{}\">Try again</a></p>",
            html_escape(title),
            html_escape(hint),
            html_escape(url),
            html_escape(err),
            html_escape(url)
        ),
    )
}

pub fn crashed(reason: &str) -> String {
    page("Aw, Snap!", &format!("<div class=err>🪶</div><h1>Aw, Snap!</h1><div class=sub>Something went wrong while displaying this page. Other tabs are unaffected.</div><div class=card><code>{}</code></div>", html_escape(reason)))
}

pub fn download(url: &str, path: &str, size: usize) -> String {
    page(
        "Download complete",
        &format!(
            "<div class=err>⬇️</div><h1>Download complete</h1><div class=card><table class=kv><tr><td>From</td><td>{}</td></tr><tr><td>Saved to</td><td><code>{}</code></td></tr><tr><td>Size</td><td>{} KB</td></tr></table></div>",
            html_escape(url),
            html_escape(path),
            size / 1024
        ),
    )
}

/// ohb://storage: cookies and site data per site, with clear actions.
pub fn settings_page() -> String {
    page("Settings", &crate::prefs::page_html())
}

pub fn storage_page() -> String {
    page("Site data & storage", &crate::storage::page_html())
}
