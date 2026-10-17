// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Web archive mode for reproducible testing.
//!
//! With `OHB_ARCHIVE=<dir>` every GET response is looked up in `<dir>` before
//! touching the network. Misses are fetched live (retrying transient
//! failures) and written to the archive, so the next run sees identical
//! bytes. With `OHB_ARCHIVE_ONLY=1` misses become 404s instead of live
//! fetches.
//!
//! On-disk format (shared with the Playwright reference harness):
//! - `<dir>/<key>.meta`: line 1 is the status code, line 2 the URL, and each
//!   further line is one `Name: value` response header.
//! - `<dir>/<key>.body`: the decoded response body.
//!
//! `<key>` is the 16-hex-digit FNV-1a 64 hash of the URL without its fragment.
//! Each hop of a redirect chain is stored separately, so redirects replay
//! through the normal redirect loop.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use url::Url;

use super::Response;

pub struct Archive {
    dir: PathBuf,
    pub offline: bool,
}

/// The archive configured through `OHB_ARCHIVE`, if any.
pub fn archive() -> Option<&'static Archive> {
    static A: OnceLock<Option<Archive>> = OnceLock::new();
    A.get_or_init(|| {
        let dir = std::env::var_os("OHB_ARCHIVE")?;
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).ok()?;
        Some(Archive { dir, offline: std::env::var("OHB_ARCHIVE_ONLY").map(|v| v == "1").unwrap_or(false) })
    })
    .as_ref()
}

pub fn key(url: &Url) -> String {
    let mut u = url.clone();
    u.set_fragment(None);
    let mut h: u64 = 0xcbf29ce484222325;
    for b in u.as_str().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

impl Archive {
    pub fn lookup(&self, url: &Url) -> Option<Response> {
        let k = key(url);
        let meta = std::fs::read_to_string(self.dir.join(format!("{k}.meta"))).ok()?;
        let body = std::fs::read(self.dir.join(format!("{k}.body"))).unwrap_or_default();
        let mut lines = meta.lines();
        let status: u16 = lines.next()?.trim().parse().ok()?;
        let _stored_url = lines.next()?;
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .collect();
        Some(Response {
            url: url.clone(),
            status,
            status_text: String::new(),
            headers,
            body: Arc::new(body),
            redirected: false,
            from_cache: false,
        })
    }

    pub fn store(&self, resp: &Response) {
        // Transient proxy/server failures are not worth replaying.
        if matches!(resp.status, 502..=504) {
            return;
        }
        let k = key(&resp.url);
        let mut meta = format!("{}\n{}\n", resp.status, resp.url);
        for (n, v) in &resp.headers {
            // The stored body is already decoded and unframed.
            if n.eq_ignore_ascii_case("content-encoding")
                || n.eq_ignore_ascii_case("content-length")
                || n.eq_ignore_ascii_case("transfer-encoding")
                || n.eq_ignore_ascii_case("connection")
                || n.eq_ignore_ascii_case("keep-alive")
            {
                continue;
            }
            if v.contains('\n') {
                continue;
            }
            meta.push_str(&format!("{n}: {v}\n"));
        }
        // Body first, then meta, so a reader never sees meta without its body.
        let tmp = self.dir.join(format!("{k}.body.tmp{}", std::process::id()));
        if std::fs::write(&tmp, resp.body.as_slice()).is_ok() {
            let _ = std::fs::rename(&tmp, self.dir.join(format!("{k}.body")));
            let _ = std::fs::write(self.dir.join(format!("{k}.meta")), meta);
        }
    }
}
