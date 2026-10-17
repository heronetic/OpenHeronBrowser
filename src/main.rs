// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! OpenHeronBrowser (OHB) — a from-scratch web browser built on the
//! HeronBrowser architecture.

#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod css;
mod dom;
mod ext;

// The script engine and DOM allocate many small objects; mimalloc handles
// that much better than the system allocator.
#[cfg(not(ohb_sysalloc))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
mod forms;
mod sysmem;

/// Allocator settings: tabs and frames each run on their own thread, so
/// memory a finished thread leaves behind must go back to the OS: purge its
/// free pages when it exits, and let other threads reclaim its segments
/// when they free the blocks still in them.
fn tune_allocator() {
    #[cfg(not(ohb_sysalloc))]
    {
        extern "C" {
            fn mi_option_set(option: i32, value: std::ffi::c_long);
        }
        // Option numbers of mimalloc v2 (include/mimalloc.h).
        const ABANDONED_PAGE_PURGE: i32 = 12;
        const ABANDONED_RECLAIM_ON_FREE: i32 = 26;
        // SAFETY: plain option setters, called once at startup.
        unsafe {
            mi_option_set(ABANDONED_PAGE_PURGE, 1);
            mi_option_set(ABANDONED_RECLAIM_ON_FREE, 1);
            // OHB_MEM_DEBUG reports memory left in exited threads' segments.
            if std::env::var_os("OHB_MEM_DEBUG").is_some() {
                const VISIT_ABANDONED: i32 = 29;
                mi_option_set(VISIT_ABANDONED, 1);
            }
        }
    }
}

/// Return this thread's freed memory to the OS (after a garbage collection).
pub fn release_free_memory() {
    #[cfg(not(ohb_sysalloc))]
    {
        extern "C" {
            fn mi_collect(force: bool);
        }
        // SAFETY: mi_collect only frees the calling thread's unused pages.
        unsafe { mi_collect(true) };
    }
    #[cfg(all(ohb_sysalloc, target_os = "linux"))]
    {
        extern "C" {
            fn malloc_trim(pad: usize) -> i32;
        }
        // SAFETY: returns free heap memory to the OS.
        unsafe { malloc_trim(0) };
    }
}

/// Bytes in use by the system allocator (debug builds with `ohb_sysalloc`).
/// OHB_MEM_DEBUG: live heap of service threads (net workers, h2 readers),
/// updated when they collect.
pub static SERVICE_HEAP: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<std::thread::ThreadId, (&'static str, usize)>>> = std::sync::LazyLock::new(Default::default);

/// Collect this thread's freed memory; under OHB_MEM_DEBUG also record its heap.
pub fn collect_service_thread(kind: &'static str) {
    release_free_memory();
    static DBG: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| std::env::var_os("OHB_MEM_DEBUG").is_some());
    if *DBG {
        let b = heap_in_use();
        if let Ok(mut m) = SERVICE_HEAP.lock() {
            m.insert(std::thread::current().id(), (kind, b));
        }
    }
}

/// OHB_MEM_DEBUG: allocator-wide numbers (window thread).
pub fn mem_debug_report() {
    #[cfg(not(ohb_sysalloc))]
    {
        extern "C" {
            fn mi_process_info(elapsed: *mut usize, user: *mut usize, sys: *mut usize, rss: *mut usize, peak_rss: *mut usize, commit: *mut usize, peak_commit: *mut usize, faults: *mut usize);
        }
        let mut v = [0usize; 8];
        // SAFETY: plain out-parameters.
        unsafe { mi_process_info(&mut v[0], &mut v[1], &mut v[2], &mut v[3], &mut v[4], &mut v[5], &mut v[6], &mut v[7]) };
        let (mut net, mut h2, mut n) = (0usize, 0usize, 0usize);
        if let Ok(m) = SERVICE_HEAP.lock() {
            for (k, b) in m.values() {
                n += 1;
                if *k == "h2" { h2 += b } else { net += b }
            }
        }
        let _ = v;
        // Exited threads' segments: live bytes and committed bytes.
        #[repr(C)]
        struct Area { blocks: *mut u8, reserved: usize, committed: usize, used: usize, block_size: usize, full_block_size: usize }
        extern "C" {
            fn mi_subproc_main() -> *mut u8;
            fn mi_abandoned_visit_blocks(sub: *mut u8, tag: i32, visit_blocks: bool, f: extern "C" fn(*const u8, *const Area, *mut u8, usize, *mut u8) -> bool, arg: *mut u8) -> bool;
        }
        extern "C" fn visit(_h: *const u8, area: *const Area, _b: *mut u8, _sz: usize, arg: *mut u8) -> bool {
            // SAFETY: mimalloc passes a valid area; arg is our [usize; 3].
            unsafe {
                if !area.is_null() {
                    let t = &mut *(arg as *mut [usize; 3]);
                    t[0] += (*area).used * (*area).full_block_size;
                    t[1] += (*area).committed;
                    t[2] += 1;
                }
            }
            true
        }
        let mut ab = [0usize; 3];
        // SAFETY: visits abandoned segments (enabled by option 29 at startup); the callback only reads.
        unsafe { mi_abandoned_visit_blocks(mi_subproc_main(), -1, false, visit, ab.as_mut_ptr() as *mut u8) };
        let fp = crate::sysmem::footprint().map(|(f, i, c, r)| format!(" footprint={}MB internal={}MB compressed={}MB reusable={}MB", f >> 20, i >> 20, c >> 20, r >> 20)).unwrap_or_default();
        let tags = crate::sysmem::vm_tags();
        if !tags.is_empty() {
            let t: Vec<String> = tags.iter().take(8).map(|(t, r, c, d, u)| format!("tag{t}:res{}/cmp{}/dirty{}/reus{}", r >> 20, c >> 20, d >> 20, u >> 20)).collect();
            eprintln!("[memdbg] vm {}", t.join(" "));
        }
        eprintln!("[memdbg]{fp} rss={}MB window_heap={}MB tabs_heap={}MB net_heap={}MB h2_heap={}MB ({n} threads) abandoned live={}MB committed={}MB pages={}", crate::sysmem::rss_bytes().unwrap_or(0) >> 20, heap_in_use() >> 20, page::all_tabs_memory() >> 20, net >> 20, h2 >> 20, ab[0] >> 20, ab[1] >> 20, ab[2]);
    }
}

pub fn heap_in_use() -> usize {
    #[cfg(all(ohb_sysalloc, target_os = "linux"))]
    {
        #[repr(C)]
        struct MallInfo2 { arena: usize, ordblks: usize, smblks: usize, hblks: usize, hblkhd: usize, usmblks: usize, fsmblks: usize, uordblks: usize, fordblks: usize, keepcost: usize }
        extern "C" {
            fn mallinfo2() -> MallInfo2;
        }
        // SAFETY: plain query of allocator statistics.
        let m = unsafe { mallinfo2() };
        return m.uordblks + m.hblkhd;
    }
    // mimalloc: live bytes in the calling thread's heap.
    #[cfg(not(ohb_sysalloc))]
    {
        #[repr(C)]
        struct Area { blocks: *mut u8, reserved: usize, committed: usize, used: usize, block_size: usize, full_block_size: usize }
        extern "C" {
            fn mi_heap_get_default() -> *mut u8;
            fn mi_heap_visit_blocks(heap: *const u8, visit_all: bool, f: extern "C" fn(*const u8, *const Area, *mut u8, usize, *mut u8) -> bool, arg: *mut u8) -> bool;
        }
        extern "C" fn visit(_h: *const u8, area: *const Area, _b: *mut u8, _sz: usize, arg: *mut u8) -> bool {
            // SAFETY: mimalloc passes a valid area; arg is our counter.
            unsafe {
                if !area.is_null() {
                    *(arg as *mut usize) += (*area).used * (*area).full_block_size;
                }
            }
            true
        }
        let mut total: usize = 0;
        // SAFETY: visits this thread's own heap; the callback only reads areas.
        unsafe { mi_heap_visit_blocks(mi_heap_get_default(), false, visit, &mut total as *mut usize as *mut u8) };
        return total;
    }
    #[allow(unreachable_code)]
    0
}
mod html;
mod js;
mod layout;
mod net;
mod page;
mod prefs;
mod paint;
mod shell;
mod storage;
mod persist;
mod text;

use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

fn usage() {
    println!(
        "OpenHeronBrowser {} — HeronBrowser architecture

Usage:
  ohb [URL...]                         Open the browser (optionally with URLs in tabs)
  ohb --screenshot OUT.png URL         Render a page headlessly to PNG
      [--width 1280] [--height 800]    Viewport size in CSS pixels
      [--full]                         Capture the full page height
      [--wait MS]                      Extra settle time after load (default 1500)
      [--scale 1.0]                    Device pixel ratio
      [--no-js]                        Disable JavaScript
      [--eval JS]                      Evaluate JS after load and print the result
      [--input ACTIONS]                Scripted input after load: click:X,Y;type:TEXT;key:Enter;wait:MS
      [--dump-dom]                     Print document.documentElement.outerHTML
      [--wait-for JS]                  Finish as soon as JS is truthy (after --wait)
      [--timeout MS]                   Give up waiting after MS (default 45000)
  --load-extension DIR                 Load an unpacked extension (repeatable)
  --no-extensions                      Run without extensions (incl. HeronBlock)
  ohb --version",
        env!("CARGO_PKG_VERSION")
    );
}

/// When the browser runs without a terminal (opened from Finder or the
/// Dock), keep its error output in the profile (`ohb.log`, the previous
/// run's in `ohb.prev.log`), and note panics with where they happened.
/// A crash that ends the process (a stack overflow, say) can then be read
/// afterwards. Until the session is allowed to write to disk (`persist`),
/// the output is held in memory; it's written out if the user saves the
/// session and dropped if not.
enum LogSink {
    Held(Vec<u8>),
    File(std::fs::File),
    Off,
}

static LOG_SINK: std::sync::Mutex<Option<LogSink>> = std::sync::Mutex::new(None);

fn log_dir() -> std::path::PathBuf {
    std::env::var_os("OHB_LOG_DIR").map(std::path::PathBuf::from).unwrap_or_else(ext::profile_dir)
}

/// The session's disk decision reached the log: write it out or drop it.
pub fn log_decided(save: bool) {
    let mut g = LOG_SINK.lock().unwrap();
    let Some(LogSink::Held(buf)) = g.take() else { return };
    if !save {
        *g = Some(LogSink::Off);
        return;
    }
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let log = dir.join("ohb.log");
    let _ = std::fs::rename(&log, dir.join("ohb.prev.log"));
    *g = match std::fs::File::create(&log) {
        Ok(mut f) => {
            use std::io::Write;
            let _ = f.write_all(&buf);
            Some(LogSink::File(f))
        }
        Err(_) => Some(LogSink::Off),
    };
}

fn log_to_profile() {
    #[cfg(unix)]
    {
        extern "C" {
            fn isatty(fd: i32) -> i32;
            fn dup2(src: i32, dst: i32) -> i32;
        }
        use std::os::unix::io::AsRawFd;
        if unsafe { isatty(2) } == 1 {
            return;
        }
        if let Ok((mut reader, writer)) = std::io::pipe() {
            *LOG_SINK.lock().unwrap() = Some(LogSink::Held(Vec::new()));
            unsafe {
                dup2(writer.as_raw_fd(), 2);
            }
            std::mem::forget(writer);
            let _ = std::thread::Builder::new().name("heron-log".into()).spawn(move || {
                use std::io::{Read, Write};
                let mut chunk = [0u8; 8192];
                while let Ok(n) = reader.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    match LOG_SINK.lock().unwrap().as_mut() {
                        Some(LogSink::Held(buf)) => {
                            buf.extend_from_slice(&chunk[..n]);
                            // Keep the most recent 4 MB.
                            if buf.len() > 4 << 20 {
                                let cut = buf.len() - (3 << 20);
                                buf.drain(..cut);
                            }
                        }
                        Some(LogSink::File(f)) => {
                            let _ = f.write_all(&chunk[..n]);
                        }
                        _ => {}
                    }
                }
            });
            if !persist::pending() {
                log_decided(persist::allowed());
            }
        }
    }
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let t = std::thread::current();
        let when = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        eprintln!("[panic] t={when} thread '{}': {info}", t.name().unwrap_or("?"));
        default(info);
    }));
}

/// The tab to show after "Restart browser" reopens the saved tabs.
pub static RESTORE_ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn restore_path() -> std::path::PathBuf {
    ext::profile_dir().join("restart-session.txt")
}

/// Save the open tabs for the process that "Restart browser" starts:
/// the active index on the first line, then one URL per line.
pub fn save_restore_session(urls: &[String], active: usize) -> bool {
    let mut out = format!("{active}\n");
    for u in urls {
        out.push_str(&u.replace(['\n', '\r'], ""));
        out.push('\n');
    }
    let p = restore_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(p, out).is_ok()
}

/// Read (and remove) the saved tabs. Only http(s), file-less browser pages
/// and about:blank are reopened.
fn take_restore_session() -> Option<(Vec<String>, usize)> {
    let p = restore_path();
    let s = std::fs::read_to_string(&p).ok()?;
    let _ = std::fs::remove_file(&p);
    let mut lines = s.lines();
    let active: usize = lines.next()?.trim().parse().ok()?;
    let tabs: Vec<String> = lines
        .map(|l| l.trim().to_string())
        // Browser pages reopen without their action query (ohb://settings?…).
        .map(|l| if l.starts_with("ohb://") { l.split('?').next().unwrap_or("").to_string() } else { l })
        .filter(|l| l.starts_with("https://") || l.starts_with("http://") || l.starts_with("ohb://") || l == "about:blank")
        .collect();
    if tabs.is_empty() {
        return None;
    }
    Some((tabs, active))
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

/// Release builds on Windows use the GUI subsystem; reattach to the parent
/// console so command-line modes (--help, --screenshot, …) can print.
#[cfg(windows)]
fn attach_console() {
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(pid: u32) -> i32;
    }
    unsafe {
        AttachConsole(u32::MAX);
    }
}

fn main() {
    tune_allocator();
    // Debugging aid: OHB_PARSE_BENCH=FILE times parsing FILE as a module.
    if let Some(f) = std::env::var_os("OHB_PARSE_BENCH") {
        let src = std::fs::read_to_string(&f).unwrap_or_default();
        let n: usize = std::env::var("OHB_PARSE_N").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
        let mut ctx = boa_engine::Context::default();
        for _ in 0..n {
            let t = Instant::now();
            let r = boa_engine::Module::parse(boa_engine::Source::from_bytes(src.as_bytes()), None, &mut ctx);
            eprintln!("parse {} KB: {:?} ok={}", src.len() / 1024, t.elapsed(), r.is_ok());
        }
        return;
    }
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // Extensions: `--load-extension DIR` (repeatable), `--no-extensions`.
    let mut ext_dirs = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if let Some(d) = args[i].strip_prefix("--load-extension=") {
            ext_dirs.push(std::path::PathBuf::from(d));
            args.remove(i);
        } else if args[i] == "--load-extension" && i + 1 < args.len() {
            ext_dirs.push(std::path::PathBuf::from(args.remove(i + 1)));
            args.remove(i);
        } else {
            i += 1;
        }
    }
    if args.iter().any(|a| a == "--no-extensions") {
        std::env::set_var("OHB_NO_EXT", "1");
    }
    #[cfg(windows)]
    if args.iter().any(|a| a.starts_with("--")) {
        attach_console();
    }
    // Test hook (OHB_WPT only): feed random markup and CSS to the parsers.
    if std::env::var_os("OHB_WPT").is_some() {
        if let Some(n) = arg_value(&args, "--fuzz-parsers") {
            fuzz_parsers(n.parse().unwrap_or(1000));
            return;
        }
    }
    if let Some(out) = arg_value(&args, "--write-icon") {
        let size: u32 = args.iter().skip_while(|a| *a != "--write-icon").nth(2).and_then(|v| v.parse().ok()).unwrap_or(512);
        let pm = shell::chrome::render_icon(size);
        if let Err(e) = pm.save_png(&out) {
            eprintln!("failed to write icon: {e}");
            std::process::exit(1);
        }
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        usage();
        return;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("OpenHeronBrowser {} demo (HeronBrowser architecture) by Heronetic", env!("CARGO_PKG_VERSION"));
        return;
    }
    let no_js = args.iter().any(|a| a == "--no-js");
    ext::init(ext_dirs);
    if let Some(out) = arg_value(&args, "--screenshot") {
        let flags_with_values = ["--screenshot", "--width", "--height", "--wait", "--scale", "--eval", "--input", "--wait-for", "--timeout"];
        let mut url = None;
        let mut i = 0;
        while i < args.len() {
            if flags_with_values.contains(&args[i].as_str()) {
                i += 2;
                continue;
            }
            if !args[i].starts_with("--") {
                url = Some(args[i].clone());
            }
            i += 1;
        }
        let url = url.unwrap_or_else(|| "ohb://newtab".into());
        let w: f32 = arg_value(&args, "--width").and_then(|v| v.parse().ok()).unwrap_or(1280.0);
        let h: f32 = arg_value(&args, "--height").and_then(|v| v.parse().ok()).unwrap_or(800.0);
        let wait: u64 = arg_value(&args, "--wait").and_then(|v| v.parse().ok()).unwrap_or(1500);
        let scale: f32 = arg_value(&args, "--scale").and_then(|v| v.parse().ok()).unwrap_or(1.0);
        let full = args.iter().any(|a| a == "--full");
        let mut evals: Vec<String> = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if a == "--eval" {
                if let Some(v) = args.get(i + 1) {
                    evals.push(v.clone());
                }
            }
        }
        if args.iter().any(|a| a == "--dump-dom") {
            evals.push("document.documentElement ? document.documentElement.outerHTML : ''".into());
        }
        let input = arg_value(&args, "--input").unwrap_or_default();
        let wait_for = arg_value(&args, "--wait-for");
        let timeout: u64 = arg_value(&args, "--timeout").and_then(|v| v.parse().ok()).unwrap_or(45_000);
        storage::maybe_enable_for_headless();
        let code = headless(&url, &out, w, h, scale, wait, full, !no_js, &evals, &input, wait_for.as_deref(), timeout);
        storage::flush();
        std::process::exit(code);
    }
    let restore = args.iter().any(|a| a == "--restore-session");
    if let Some(n) = args.iter().find_map(|a| a.strip_prefix("--active-tab=")).and_then(|v| v.parse::<usize>().ok()) {
        RESTORE_ACTIVE.store(n, std::sync::atomic::Ordering::Relaxed);
    }
    let mut urls: Vec<String> = args.into_iter().filter(|a| !a.starts_with("--")).collect();
    if restore {
        if let Some((tabs, active)) = take_restore_session() {
            urls = tabs;
            RESTORE_ACTIVE.store(active, std::sync::atomic::Ordering::Relaxed);
        }
    }
    // Nothing is written to disk until the user answers the startup prompt
    // (OHB_ACCEPT_WARNING=save|nosave answers it for automated runs).
    match std::env::var("OHB_ACCEPT_WARNING").as_deref() {
        Ok("save") => {}
        Ok(_) => persist::decide(false),
        Err(_) => persist::hold(),
    }
    log_to_profile();
    // Cookies and site data are kept in the profile between sessions.
    storage::enable_persistence();
    prefs::enable_persistence();
    shell::run(urls, !no_js);
    storage::flush();
}

#[allow(clippy::too_many_arguments)]
fn headless(url: &str, out: &str, w: f32, h: f32, scale: f32, wait_ms: u64, full: bool, js: bool, evals: &[String], input: &str, wait_for: Option<&str>, timeout_ms: u64) -> i32 {
    let (tx, rx) = mpsc::channel::<page::PageEvent>();
    let tx = Mutex::new(tx);
    let sink: page::UiSink = Arc::new(move |_id, ev| {
        let _ = tx.lock().unwrap().send(ev);
    });
    let history = Arc::new(Mutex::new(Vec::new()));
    let started = Instant::now();
    let handle = page::spawn_tab(1, sink, history, page::NavRequest::get(url), (w, h), scale, js);
    let mut frame: Option<Arc<page::FrameData>> = None;
    let mut loaded_at: Option<Instant> = None;
    let mut busy = false;
    let mut idle_since = Instant::now();
    let mut scroll = (0.0f32, 0.0f32);
    let deadline = Duration::from_millis(timeout_ms);
    // `--wait-for EXPR`: once loaded (and `--wait` has passed), poll EXPR and
    // stop as soon as it's truthy, busy page or not.
    let mut wf_pending = false;
    let mut wf_last = Instant::now();
    let mut wf_done = false;
    // --wait-for evals sent and not yet answered (their late answers must not
    // be taken for the --eval results).
    let mut wf_outstanding = 0usize;
    // Scripted input (`--input "click:X,Y;type:TEXT;key:Enter;wait:MS"`) runs
    // once the page has settled, then the page settles again.
    let mut actions: Vec<(String, String)> = input.split(';').filter(|a| !a.trim().is_empty()).map(|a| {
        let (k, v) = a.split_once(':').unwrap_or((a, ""));
        (k.trim().to_string(), v.to_string())
    }).collect();
    actions.reverse();
    let mut phase_start = started;
    // OHB_DAMAGE_VERIFY: check the window's partial repaint against full renders.
    let dverify = std::env::var_os("OHB_DAMAGE_VERIFY").is_some();
    let mut dstate: Option<(Arc<page::FrameData>, tiny_skia::Pixmap, (f32, f32))> = None;
    loop {
        if phase_start.elapsed() > deadline {
            eprintln!("timeout waiting for page; capturing what we have");
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(ev) => match ev {
                page::PageEvent::Frame(f) => {
                    if dverify {
                        damage_verify(&mut dstate, &f, scroll, (w, h), scale);
                    }
                    frame = Some(f)
                }
                page::PageEvent::Loading(false, _) => {
                    if loaded_at.is_none() {
                        loaded_at = Some(Instant::now());
                    }
                }
                page::PageEvent::Loading(true, _) => loaded_at = None,
                page::PageEvent::Busy(b) => {
                    busy = b;
                    if !b {
                        idle_since = Instant::now();
                    }
                }
                page::PageEvent::Title(t) => eprintln!("title: {t}"),
                page::PageEvent::Url(u, _) => {
                    eprintln!("url: {u}");
                    // A document committed while loading: frames before it
                    // were the previous one's (about:blank).
                    if loaded_at.is_none() {
                        frame = None;
                    }
                }
                page::PageEvent::Crashed(e) => {
                    eprintln!("tab crashed: {e}");
                    return 2;
                }
                page::PageEvent::ScrollTo(x, y) => {
                    if !x.is_nan() {
                        scroll = (x, y);
                        if dverify {
                            if let Some(f) = frame.clone() {
                                damage_verify(&mut dstate, &f, scroll, (w, h), scale);
                            }
                        }
                    }
                }
                page::PageEvent::EvalResult(r) if wf_outstanding > 0 => {
                    wf_outstanding -= 1;
                    wf_pending = false;
                    let r = r.trim();
                    if !(r.is_empty() || r == "false" || r == "undefined" || r == "null" || r == "0" || r.starts_with("ERROR")) {
                        wf_done = true;
                    }
                }
                _ => {}
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
        if let Some(expr) = wait_for {
            if wf_done && actions.is_empty() {
                break;
            }
            if let Some(t) = loaded_at {
                if !wf_pending && t.elapsed() > Duration::from_millis(wait_ms) && wf_last.elapsed() > Duration::from_millis(150) {
                    wf_pending = true;
                    wf_outstanding += 1;
                    wf_last = Instant::now();
                    let _ = handle.tx.send(page::PageMsg::Eval(expr.to_string()));
                }
            }
            if wf_pending && wf_last.elapsed() > Duration::from_secs(5) {
                wf_pending = false;
            }
            continue;
        }
        // Loaded, waited, and script work has settled (a frame after it).
        if let Some(t) = loaded_at {
            if t.elapsed() > Duration::from_millis(wait_ms) && frame.is_some() && !busy && idle_since.elapsed() > Duration::from_millis(300) {
                if actions.is_empty() {
                    break;
                }
                // Run the input script, then wait for the page to settle again.
                while let Some((k, v)) = actions.pop() {
                    let mods = page::Mods::default();
                    match k.as_str() {
                        "click" => {
                            let mut it = v.split(',').map(|n| n.trim().parse::<f32>().unwrap_or(0.0));
                            let (x, y) = (it.next().unwrap_or(0.0), it.next().unwrap_or(0.0));
                            let _ = handle.tx.send(page::PageMsg::MouseMove { x, y });
                            let _ = handle.tx.send(page::PageMsg::MouseDown { x, y, button: 0, mods });
                            let _ = handle.tx.send(page::PageMsg::MouseUp { x, y, button: 0, mods });
                        }
                        "move" | "delta" => {
                            // "move:x,y" (pointer position) / "delta:dx,dy" (raw locked motion)
                            let mut it = v.split(',').map(|n| n.trim().parse::<f32>().unwrap_or(0.0));
                            let (x, y) = (it.next().unwrap_or(0.0), it.next().unwrap_or(0.0));
                            let _ = handle.tx.send(if k == "move" { page::PageMsg::MouseMove { x, y } } else { page::PageMsg::MouseDelta { dx: x, dy: y } });
                        }
                        "drag" => {
                            // "drag:x1,y1,x2,y2": press, move in steps, release
                            let p: Vec<f32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                            if let [x1, y1, x2, y2, ..] = p.as_slice() {
                                let _ = handle.tx.send(page::PageMsg::MouseMove { x: *x1, y: *y1 });
                                let _ = handle.tx.send(page::PageMsg::MouseDown { x: *x1, y: *y1, button: 0, mods });
                                for i in 1..=10 {
                                    let t = i as f32 / 10.0;
                                    std::thread::sleep(Duration::from_millis(60));
                                    let _ = handle.tx.send(page::PageMsg::MouseMove { x: x1 + (x2 - x1) * t, y: y1 + (y2 - y1) * t });
                                }
                                std::thread::sleep(Duration::from_millis(60));
                                let _ = handle.tx.send(page::PageMsg::MouseUp { x: *x2, y: *y2, button: 0, mods });
                            }
                        }
                        "scroll" => {
                            // "scroll:dy" or "scroll:x,y,dy" (a wheel event, CSS px)
                            let p: Vec<f32> = v.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                            let (x, y, dy) = match p.as_slice() {
                                [dy] => (w / 2.0, h / 2.0, *dy),
                                [x, y, dy, ..] => (*x, *y, *dy),
                                _ => (w / 2.0, h / 2.0, 100.0),
                            };
                            let _ = handle.tx.send(page::PageMsg::Wheel { x, y, dx: 0.0, dy });
                        }
                        "type" => {
                            for c in v.chars() {
                                let _ = handle.tx.send(page::PageMsg::Key { key: page::Key::Char(c.to_string()), mods });
                                let _ = handle.tx.send(page::PageMsg::Text(c.to_string()));
                                std::thread::sleep(Duration::from_millis(20));
                            }
                        }
                        "key" | "down" | "up" => {
                            // "Shift+Left", "Ctrl+b", "Enter", ... ("down"/"up": hold and release)
                            let mut mods = mods;
                            let mut name = v.as_str();
                            while let Some((m, rest)) = name.split_once('+').filter(|(m, r)| !r.is_empty() && matches!(*m, "Shift" | "Ctrl" | "Alt" | "Meta")) {
                                match m {
                                    "Shift" => mods.shift = true,
                                    "Ctrl" => mods.ctrl = true,
                                    "Alt" => mods.alt = true,
                                    _ => mods.meta = true,
                                }
                                name = rest;
                            }
                            let key = match name {
                                "Enter" => page::Key::Enter,
                                "Tab" => page::Key::Tab,
                                "Escape" => page::Key::Escape,
                                "Backspace" => page::Key::Backspace,
                                "Delete" => page::Key::Delete,
                                "Down" => page::Key::Down,
                                "Up" => page::Key::Up,
                                "Left" => page::Key::Left,
                                "Right" => page::Key::Right,
                                "Home" => page::Key::Home,
                                "End" => page::Key::End,
                                c if c.chars().count() == 1 => page::Key::Char(c.to_string()),
                                other => page::Key::Other(other.to_string()),
                            };
                            let _ = handle.tx.send(match k.as_str() {
                                "down" => page::PageMsg::KeyDown { key, mods },
                                "up" => page::PageMsg::KeyUp { key, mods },
                                _ => page::PageMsg::Key { key, mods },
                            });
                        }
                        "wait" => {
                            // Keep taking the page's frames meanwhile (queued
                            // frames would hold their images alive).
                            let until = Instant::now() + Duration::from_millis(v.trim().parse().unwrap_or(500));
                            while let Some(left) = until.checked_duration_since(Instant::now()) {
                                match rx.recv_timeout(left.min(Duration::from_millis(100))) {
                                    Ok(page::PageEvent::Frame(f)) => frame = Some(f),
                                    Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                                    Err(_) => break,
                                }
                            }
                            break;
                        }
                        _ => eprintln!("unknown input action {k}"),
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                loaded_at = Some(Instant::now());
                idle_since = Instant::now();
                phase_start = Instant::now();
            }
        }
    }
    for code in evals {
        let _ = handle.tx.send(page::PageMsg::Eval(code.clone()));
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(page::PageEvent::EvalResult(_)) if wf_outstanding > 0 => wf_outstanding -= 1,
                Ok(page::PageEvent::EvalResult(r)) => {
                    println!("{r}");
                    break;
                }
                Ok(page::PageEvent::Frame(f)) => frame = Some(f),
                Ok(_) => {}
                Err(_) => {
                    eprintln!("eval timed out");
                    break;
                }
            }
        }
    }
    // OHB_EVAL_FRAME: let the page draw what the evals changed.
    if !evals.is_empty() && std::env::var_os("OHB_EVAL_FRAME").is_some() {
        let until = Instant::now() + Duration::from_millis(400);
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if let Ok(page::PageEvent::Frame(f)) = rx.recv_timeout(left) {
                frame = Some(f);
            }
        }
    }
    let _ = handle.tx.send(page::PageMsg::Shutdown);
    let frame = match frame {
        Some(f) => f,
        None => {
            eprintln!("no frame was produced");
            return 1;
        }
    };
    let css_h = if full { frame.doc_h.min(16000.0).max(h) } else { h };
    let pw = (w * scale).round() as u32;
    let ph = (css_h * scale).round() as u32;
    let mut pm = tiny_skia::Pixmap::new(pw.max(1), ph.max(1)).unwrap();
    let bg = frame.list.bg;
    let bgc = if bg.a == 0 { css::values::Color::WHITE } else { bg };
    pm.fill(tiny_skia::Color::from_rgba8(bgc.r, bgc.g, bgc.b, 255));
    let mut r = paint::raster::Rasterizer::new();
    let t = Instant::now();
    let sc = if full { (0.0, 0.0) } else { scroll };
    // Debugging aid: OHB_SPLIT_RENDER draws the page the way the window
    // does when the fixed layer is separate (content, then fixed overlay).
    if std::env::var_os("OHB_SPLIT_RENDER").is_some() && paint::raster::fixed_layer_ok(&frame.list, sc) {
        eprintln!("(split fixed layer)");
        r.fixed_mode = 1;
        r.render(&frame.list, &mut pm, sc, scale, 0.0, (0.0, ph as f32));
        r.fixed_mode = 2;
        r.render(&frame.list, &mut pm, sc, scale, 0.0, (0.0, ph as f32));
        r.fixed_mode = 0;
    } else {
        r.render(&frame.list, &mut pm, sc, scale, 0.0, (0.0, ph as f32));
    }
    // Debugging aid: the display list's large rects and stacking operations.
    if std::env::var_os("OHB_DUMP_DL").is_some() {
        for (i, it) in frame.list.items.iter().enumerate() {
            let line = match it {
                paint::Item::Rect { r, color } | paint::Item::RRect { r, color, .. } if r.w * r.h > 200000.0 => format!("rect {:?} {:?}", r, color),
                paint::Item::Gradient { r, .. } if r.w * r.h > 200000.0 => format!("gradient {:?}", r),
                paint::Item::Image { r, .. } if r.w * r.h > 200000.0 => format!("image {:?}", r),
                paint::Item::PushOpacity(o) => format!("push-opacity {o}"),
                paint::Item::PushGroup { opacity, .. } => format!("push-group {opacity}"),
                paint::Item::PushFilter(_) => "push-filter".into(),
                paint::Item::PushMask(_) => "push-mask".into(),
                paint::Item::BeginFixed => "begin-fixed".into(),
                paint::Item::EndFixed => "end-fixed".into(),
                paint::Item::Backdrop { r, .. } => format!("backdrop {:?}", r),
                _ => continue,
            };
            eprintln!("[dl] {i} {line}");
        }
    }
    eprintln!(
        "rendered {} items ({} fixed) in {:?}; document {}x{}; total {:?}",
        frame.list.items.len(),
        frame.list.fixed,
        t.elapsed(),
        frame.doc_w,
        frame.doc_h,
        started.elapsed()
    );
    match pm.save_png(out) {
        Ok(_) => {
            eprintln!("saved {out}");
            0
        }
        Err(e) => {
            eprintln!("failed to save: {e}");
            1
        }
    }
}

/// Debugging aid (OHB_DAMAGE_VERIFY): update a cached rendering of the
/// previous frame through the damage path, as the window does, and compare
/// it with a full rendering of the new frame.
fn damage_verify(state: &mut Option<(Arc<page::FrameData>, tiny_skia::Pixmap, (f32, f32))>, f: &Arc<page::FrameData>, scroll: (f32, f32), size: (f32, f32), scale: f32) {
    let (pw, ph) = ((size.0 * scale).round() as u32, (size.1 * scale).round() as u32);
    let full = |r: &mut paint::raster::Rasterizer| {
        let mut pm = tiny_skia::Pixmap::new(pw, ph).unwrap();
        let bg = if f.list.bg.a == 0 { css::values::Color::WHITE } else { f.list.bg };
        pm.fill(tiny_skia::Color::from_rgba8(bg.r, bg.g, bg.b, 255));
        let t = Instant::now();
        r.render(&f.list, &mut pm, scroll, scale, 0.0, (0.0, ph as f32));
        (pm, t.elapsed())
    };
    let mut r = paint::raster::Rasterizer::new();
    let (want, t_full) = full(&mut r);
    if let Some((old, mut pm, sc)) = state.take() {
        // A vertical scroll: shift the pixels, as the window does, then repaint
        // the exposed strip, what changed between frames and sticky content.
        let dy_dev = ((scroll.1 - sc.1) * scale).round() as i64;
        if sc.0 == scroll.0 && sc != scroll && !f.list.items.iter().any(|it| matches!(it, paint::Item::BeginFixed)) && dy_dev.abs() < ph as i64 && ((scroll.1 - sc.1) * scale - dy_dev as f32).abs() < 0.01 {
            let t = Instant::now();
            let stride = (pw * 4) as usize;
            {
                let data = pm.data_mut();
                if dy_dev > 0 {
                    data.copy_within(dy_dev as usize * stride.., 0);
                } else {
                    let off = (-dy_dev) as usize * stride;
                    let len = data.len() - off;
                    data.copy_within(..len, off);
                }
            }
            let rows = ph as i64;
            let (y0, y1) = if dy_dev > 0 { (rows - dy_dev, rows) } else { (0, -dy_dev) };
            let margin = paint::damage::filter_margin(&f.list);
            let strip = paint::Rect::new(scroll.0, scroll.1 + y0 as f32 / scale, pw as f32 / scale, (y1 - y0) as f32 / scale);
            let mut ok = paint::damage::patch(&mut r, &f.list, &mut pm, scroll, scale, strip, margin, 1.0, false).is_some();
            if ok && !Arc::ptr_eq(&old, f) {
                match paint::damage::damage(&old.list, &f.list, sc, false) {
                    paint::damage::Damage::None => {}
                    paint::damage::Damage::Rect(rc, m) => ok = paint::damage::patch(&mut r, &f.list, &mut pm, scroll, scale, rc, m, 1.0, false).is_some(),
                    paint::damage::Damage::Full => ok = false,
                }
            }
            if ok {
                match paint::damage::sticky_damage(&f.list, sc, scroll) {
                    paint::damage::StickyDamage::None => {}
                    paint::damage::StickyDamage::Rect(rc) => ok = paint::damage::patch(&mut r, &f.list, &mut pm, scroll, scale, rc, margin, 1.0, false).is_some(),
                    paint::damage::StickyDamage::Full => ok = false,
                }
            }
            if !ok {
                pm = want.clone();
            }
            let t_patch = t.elapsed();
            let (a, b) = (pm.data(), want.data());
            let bad = (0..a.len()).step_by(4).filter(|&i| (0..3).map(|k| (a[i + k] as i32 - b[i + k] as i32).abs()).max().unwrap_or(0) > 8).count();
            eprintln!("[damage-verify] scroll {:.0}->{:.0} ok={ok}: shift {t_patch:?} vs full {t_full:?}; mismatched px {bad}", sc.1, scroll.1);
            if bad > 0 && std::env::var_os("OHB_DAMAGE_DUMP").is_some() {
                let _ = pm.save_png("/tmp/dmg_scroll_patch.png");
                let _ = want.save_png("/tmp/dmg_scroll_full.png");
            }
            *state = Some((f.clone(), if bad > 0 { want } else { pm }, scroll));
            return;
        }
        if sc == scroll {
            let t = Instant::now();
            let d = paint::damage::damage(&old.list, &f.list, scroll, false);
            let mut area = 0.0;
            match &d {
                paint::damage::Damage::Full => pm = want.clone(),
                paint::damage::Damage::None => {}
                paint::damage::Damage::Rect(rc, margin) => {
                    match paint::damage::patch(&mut r, &f.list, &mut pm, scroll, scale, *rc, *margin, 1.0, false) {
                        Some(a) => area = a,
                        None => pm = want.clone(),
                    }
                }
            }
            let t_patch = t.elapsed();
            let (a, b) = (pm.data(), want.data());
            let mut bad = 0usize;
            let mut first = None;
            for i in (0..a.len()).step_by(4) {
                let diff = (0..3).map(|k| (a[i + k] as i32 - b[i + k] as i32).abs()).max().unwrap_or(0);
                if diff > 8 {
                    bad += 1;
                    if first.is_none() {
                        let px = (i / 4) as u32;
                        first = Some((px % pw, px / pw));
                    }
                }
            }
            let kind = match &d {
                paint::damage::Damage::Full => "full".to_string(),
                paint::damage::Damage::None => "none".to_string(),
                paint::damage::Damage::Rect(_, m) => format!("rect {:.0}% m{m}", area * 100.0),
            };
            eprintln!("[damage-verify] {kind}: patch {t_patch:?} vs full {t_full:?}; mismatched px {bad}{}", first.map(|p| format!(" first at {:?}", p)).unwrap_or_default());
            if bad > 0 && std::env::var_os("OHB_DAMAGE_DUMP").is_some() && !std::path::Path::new("/tmp/dmg_patch.png").exists() {
                let _ = pm.save_png("/tmp/dmg_patch.png");
                let _ = want.save_png("/tmp/dmg_full.png");
            }
            if bad > 0 && std::env::var_os("OHB_DAMAGE_DUMP").is_some() {
                if let Some((fx, fy)) = first {
                    let (dx, dy) = (fx as f32 / scale + scroll.0, fy as f32 / scale + scroll.1);
                    for (name, l) in [("old", &old.list), ("new", &f.list)] {
                        for (i, it) in l.items.iter().enumerate() {
                            if it.bounds().map(|b| b.inflate(3.0).contains(dx, dy)).unwrap_or(false) || !it.draws() {
                                if it.draws() || i < 400 {
                                    eprintln!("  {name}[{i}] {:?}", it);
                                }
                            }
                        }
                    }
                }
            }
            if bad > 0 {
                // Resynchronise so one discrepancy is reported once.
                *state = Some((f.clone(), want, scroll));
                return;
            }
            *state = Some((f.clone(), pm, scroll));
            return;
        }
    }
    *state = Some((f.clone(), want, scroll));
}

/// Random inputs for the HTML, CSS and selector parsers; reports panics.
fn fuzz_parsers(n: usize) {
    const PIECES: &[&str] = &[
        "<", ">", "</", "/", "script", "style", "textarea", "title", "!--", "-->", "!DOCTYPE", "doctype", "html", "&amp;", "&#", "&#x", "&nbsp", ";",
        "\u{a0}", "\u{e9}", "\u{1f600}", "\u{4e2d}", "=", "\"", "'", " ", "\n", "a", "b", "div", "p", "table", "tr", "td", "svg", "math", "![CDATA[", "]]>",
        "{", "}", ":", "@media", "@import", "@supports", "@font-face", "url(", ")", "(", "cq", "cqw", "1", "0.5", "px", "em", "%", "!important", "\\", "/*", "*/",
        "#", ".", "*", "[", "]", "~", "+", "|", "^=", "$=", ":not(", ":has(", ":nth-child(", "2n+1", "::before", "var(--x", "calc(", "--x", ",", "?", "@", "\0",
    ];
    let mut seed: u64 = 0x9E3779B97F4A7C15;
    let mut rnd = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let pieces: Vec<String> = PIECES.iter().map(|p| p.to_string()).collect();
    std::panic::set_hook(Box::new(|_| {}));
    let mut panics = 0;
    for i in 0..n {
        let len = (rnd() % 60) as usize;
        let mut s = String::new();
        for _ in 0..len {
            s.push_str(&pieces[(rnd() % pieces.len() as u64) as usize]);
        }
        let url = url::Url::parse("https://example.com/").unwrap();
        for (what, r) in [
            ("html", std::panic::catch_unwind(|| { let _ = html::parse_document(&s, url.clone()); })),
            ("css", std::panic::catch_unwind(|| { let _ = css::parse_stylesheet(&s, &url, css::Origin::Author); })),
            ("decl", std::panic::catch_unwind(|| { let _ = css::parse_declarations(&s); })),
            ("sel", std::panic::catch_unwind(|| { let _ = css::selector::parse_selector_list_str(&s); })),
        ] {
            if r.is_err() {
                panics += 1;
                if panics <= 20 {
                    eprintln!("[fuzz] {what} panicked on case {i}: {:?}", s);
                }
            }
        }
    }
    eprintln!("[fuzz] {n} cases, {panics} panics");
}
