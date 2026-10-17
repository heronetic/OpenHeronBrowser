// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Whether this session may write to disk.
//!
//! The windowed browser asks at every start (the "still hatching" prompt)
//! whether to save the session. Until the answer, nothing is written: the
//! profile (cookies, site data), settings, extension settings and the log
//! stay in memory. "Save" writes everything held so far and keeps saving;
//! "don't save" keeps the whole session in memory and forgets it on quit.
//! Previously saved data is still read at startup either way. Downloads
//! the user explicitly saves are written regardless.
//!
//! Headless runs (`--screenshot`) start decided ("save"), as before.

use std::sync::atomic::{AtomicU8, Ordering};

const SAVE: u8 = 0;
const PENDING: u8 = 1;
const DISCARD: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(SAVE);

/// Hold every disk write until `decide` (the windowed browser, at start).
pub fn hold() {
    STATE.store(PENDING, Ordering::SeqCst);
}

/// May this session write to disk now?
pub fn allowed() -> bool {
    STATE.load(Ordering::SeqCst) == SAVE
}

pub fn pending() -> bool {
    STATE.load(Ordering::SeqCst) == PENDING
}

/// The user's answer: write what was held and keep saving, or keep the
/// whole session in memory.
pub fn decide(save: bool) {
    STATE.store(if save { SAVE } else { DISCARD }, Ordering::SeqCst);
    if save {
        if !crate::storage::keep_data_setting() {
            crate::storage::set_keep_data(true);
        }
        crate::prefs::flush_held();
        crate::ext::flush_held();
        crate::storage::flush();
    }
    crate::log_decided(save);
}
