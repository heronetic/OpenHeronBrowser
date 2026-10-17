// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Streaming codecs for CompressionStream / DecompressionStream (gzip,
//! deflate, deflate-raw and brotli), one context per stream and thread.
//!
//! Decompression follows the Compression Streams Standard: input after the
//! end of the compressed data is an error (reported after the output that
//! precedes it), and so is input that ends before the compressed data does.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Write;

use brotli_decompressor::{BrotliDecompressStream, BrotliResult, BrotliState, StandardAlloc};
use flate2::{Compression, Crc, Decompress, FlushDecompress, Status};

enum Codec {
    Gzip(flate2::write::GzEncoder<Vec<u8>>),
    Zlib(flate2::write::ZlibEncoder<Vec<u8>>),
    Raw(flate2::write::DeflateEncoder<Vec<u8>>),
    Brotli(Box<brotli::CompressorWriter<Vec<u8>>>),
    Inflate(Inflater),
    Gunzip(Gunzip),
    Unbrotli(Box<Unbrotli>),
}

/// zlib or raw deflate.
struct Inflater {
    d: Decompress,
    done: bool,
}

#[derive(PartialEq)]
enum GzPhase {
    Header,
    Body,
    Trailer,
    Done,
}

struct Gunzip {
    phase: GzPhase,
    pending: Vec<u8>,
    d: Decompress,
    crc: Crc,
}

struct Unbrotli {
    st: BrotliState<StandardAlloc, StandardAlloc, StandardAlloc>,
    done: bool,
}

thread_local! {
    static CODECS: RefCell<HashMap<u64, Codec>> = RefCell::new(HashMap::new());
    static NEXT: RefCell<u64> = const { RefCell::new(1) };
}

/// A new context for `format`, or None for an unknown format.
pub fn open(format: &str, compress: bool) -> Option<u64> {
    let lvl = Compression::default();
    let c = match (format, compress) {
        ("gzip", true) => Codec::Gzip(flate2::write::GzEncoder::new(Vec::new(), lvl)),
        ("deflate", true) => Codec::Zlib(flate2::write::ZlibEncoder::new(Vec::new(), lvl)),
        ("deflate-raw", true) => Codec::Raw(flate2::write::DeflateEncoder::new(Vec::new(), lvl)),
        ("brotli", true) => Codec::Brotli(Box::new(brotli::CompressorWriter::new(Vec::new(), 4096, 5, 22))),
        ("gzip", false) => Codec::Gunzip(Gunzip { phase: GzPhase::Header, pending: Vec::new(), d: Decompress::new(false), crc: Crc::new() }),
        ("deflate", false) => Codec::Inflate(Inflater { d: Decompress::new(true), done: false }),
        ("deflate-raw", false) => Codec::Inflate(Inflater { d: Decompress::new(false), done: false }),
        ("brotli", false) => Codec::Unbrotli(Box::new(Unbrotli { st: BrotliState::new(StandardAlloc::default(), StandardAlloc::default(), StandardAlloc::default()), done: false })),
        _ => return None,
    };
    let id = NEXT.with(|n| {
        let mut n = n.borrow_mut();
        let id = *n;
        *n += 1;
        id
    });
    CODECS.with(|m| m.borrow_mut().insert(id, c));
    Some(id)
}

pub fn close(id: u64) {
    CODECS.with(|m| m.borrow_mut().remove(&id));
}

/// The outcome of feeding a chunk: the output so far, and whether the input
/// was bad (corrupt, or data after the end of the compressed stream).
pub struct Fed {
    pub out: Vec<u8>,
    pub error: bool,
}

/// Feed `input`. An erroring context is dropped.
pub fn push(id: u64, input: &[u8]) -> Fed {
    let r = CODECS.with(|m| {
        let mut m = m.borrow_mut();
        let Some(c) = m.get_mut(&id) else { return Fed { out: Vec::new(), error: true } };
        match c {
            Codec::Gzip(e) => enc(e.write_all(input), e.get_mut()),
            Codec::Zlib(e) => enc(e.write_all(input), e.get_mut()),
            Codec::Raw(e) => enc(e.write_all(input), e.get_mut()),
            Codec::Brotli(e) => {
                let r = e.write_all(input);
                enc(r, e.get_mut())
            }
            Codec::Inflate(i) => inflate(i, input),
            Codec::Gunzip(g) => gunzip(g, input),
            Codec::Unbrotli(b) => unbrotli(b, input),
        }
    });
    if r.error {
        close(id);
    }
    r
}

/// Finish the stream: the remaining output, or None when the compressed input
/// ended early. The context is dropped either way.
pub fn finish(id: u64) -> Option<Vec<u8>> {
    let c = CODECS.with(|m| m.borrow_mut().remove(&id))?;
    match c {
        Codec::Gzip(e) => e.finish().ok(),
        Codec::Zlib(e) => e.finish().ok(),
        Codec::Raw(e) => e.finish().ok(),
        Codec::Brotli(e) => Some(e.into_inner()),
        Codec::Inflate(mut i) => {
            let f = inflate(&mut i, &[]);
            (!f.error && i.done).then_some(f.out)
        }
        Codec::Gunzip(g) => (g.phase == GzPhase::Done).then(Vec::new),
        Codec::Unbrotli(mut b) => {
            let f = unbrotli(&mut b, &[]);
            (!f.error && b.done).then_some(f.out)
        }
    }
}

fn enc(r: std::io::Result<()>, out: &mut Vec<u8>) -> Fed {
    Fed { out: std::mem::take(out), error: r.is_err() }
}

/// Run `d` over `input`; Ok((output, consumed, stream end)).
fn run_inflate(d: &mut Decompress, input: &[u8]) -> Result<(Vec<u8>, usize, bool), ()> {
    let mut out: Vec<u8> = Vec::with_capacity(input.len().saturating_mul(3).clamp(4096, 1 << 20));
    let start_in = d.total_in();
    loop {
        let used = (d.total_in() - start_in) as usize;
        if out.capacity() - out.len() < 4096 {
            out.reserve(out.capacity().max(4096));
        }
        let (before_in, before_out) = (d.total_in(), d.total_out());
        let st = d.decompress_vec(&input[used..], &mut out, FlushDecompress::None).map_err(|_| ())?;
        let used_now = (d.total_in() - start_in) as usize;
        if st == Status::StreamEnd {
            return Ok((out, used_now, true));
        }
        let progressed = d.total_in() > before_in || d.total_out() > before_out;
        // Done with this input once it is all taken and the output had room
        // (nothing left buffered inside the inflater).
        if !progressed || (used_now >= input.len() && out.len() < out.capacity()) {
            return Ok((out, used_now, false));
        }
    }
}

fn inflate(i: &mut Inflater, input: &[u8]) -> Fed {
    if i.done {
        return Fed { out: Vec::new(), error: !input.is_empty() };
    }
    match run_inflate(&mut i.d, input) {
        Ok((out, used, end)) => {
            if end {
                i.done = true;
            }
            Fed { error: end && used < input.len(), out }
        }
        Err(()) => Fed { out: Vec::new(), error: true },
    }
}

/// Length of a complete gzip header at the start of `b`: Ok(Some(n)), Ok(None)
/// when more bytes are needed, Err for a malformed header.
fn gzip_header_len(b: &[u8]) -> Result<Option<usize>, ()> {
    if !b.is_empty() && b[0] != 0x1f {
        return Err(());
    }
    if b.len() > 1 && b[1] != 0x8b {
        return Err(());
    }
    if b.len() > 2 && b[2] != 8 {
        return Err(());
    }
    if b.len() < 10 {
        return Ok(None);
    }
    let flg = b[3];
    if flg & 0xe0 != 0 {
        return Err(());
    }
    let mut p = 10;
    if flg & 0x04 != 0 {
        if b.len() < p + 2 {
            return Ok(None);
        }
        let xlen = u16::from_le_bytes([b[p], b[p + 1]]) as usize;
        p += 2 + xlen;
        if b.len() < p {
            return Ok(None);
        }
    }
    for bit in [0x08u8, 0x10] {
        if flg & bit != 0 {
            match b[p.min(b.len())..].iter().position(|&c| c == 0) {
                Some(z) => p += z + 1,
                None => return Ok(None),
            }
        }
    }
    if flg & 0x02 != 0 {
        if b.len() < p + 2 {
            return Ok(None);
        }
        let want = (Crc::new_crc16(&b[..p])) as u16;
        if u16::from_le_bytes([b[p], b[p + 1]]) != want {
            return Err(());
        }
        p += 2;
    }
    Ok(Some(p))
}

trait Crc16 {
    fn new_crc16(b: &[u8]) -> u32;
}
impl Crc16 for Crc {
    /// The header CRC: the low 16 bits of the CRC-32 of the header bytes.
    fn new_crc16(b: &[u8]) -> u32 {
        let mut c = Crc::new();
        c.update(b);
        c.sum() & 0xffff
    }
}

fn gunzip(g: &mut Gunzip, input: &[u8]) -> Fed {
    let mut out = Vec::new();
    let mut rest: &[u8] = input;
    loop {
        match g.phase {
            GzPhase::Header => {
                let have = g.pending.len();
                g.pending.extend_from_slice(rest);
                match gzip_header_len(&g.pending) {
                    Err(()) => return Fed { out, error: true },
                    Ok(None) => return Fed { out, error: false },
                    Ok(Some(n)) => {
                        // Bytes of `rest` that belong to the header.
                        let from_rest = n - have;
                        rest = &rest[from_rest..];
                        g.pending.clear();
                        g.phase = GzPhase::Body;
                    }
                }
            }
            GzPhase::Body => match run_inflate(&mut g.d, rest) {
                Err(()) => return Fed { out, error: true },
                Ok((o, used, end)) => {
                    g.crc.update(&o);
                    out.extend_from_slice(&o);
                    rest = &rest[used..];
                    if !end {
                        return Fed { out, error: false };
                    }
                    g.phase = GzPhase::Trailer;
                }
            },
            GzPhase::Trailer => {
                let need = 8 - g.pending.len();
                let take = need.min(rest.len());
                g.pending.extend_from_slice(&rest[..take]);
                rest = &rest[take..];
                if g.pending.len() < 8 {
                    return Fed { out, error: false };
                }
                let crc = u32::from_le_bytes([g.pending[0], g.pending[1], g.pending[2], g.pending[3]]);
                let isize = u32::from_le_bytes([g.pending[4], g.pending[5], g.pending[6], g.pending[7]]);
                if crc != g.crc.sum() || isize != g.crc.amount() {
                    return Fed { out, error: true };
                }
                g.phase = GzPhase::Done;
            }
            GzPhase::Done => return Fed { out, error: !rest.is_empty() },
        }
    }
}

fn unbrotli(b: &mut Unbrotli, input: &[u8]) -> Fed {
    if b.done {
        return Fed { out: Vec::new(), error: !input.is_empty() };
    }
    let mut out = Vec::new();
    let mut buf = vec![0u8; 65536];
    let mut avail_in = input.len();
    let mut in_off = 0usize;
    let mut total_out = 0usize;
    loop {
        let mut avail_out = buf.len();
        let mut out_off = 0usize;
        let r = BrotliDecompressStream(&mut avail_in, &mut in_off, input, &mut avail_out, &mut out_off, &mut buf, &mut total_out, &mut b.st);
        out.extend_from_slice(&buf[..out_off]);
        match r {
            BrotliResult::NeedsMoreOutput => continue,
            BrotliResult::NeedsMoreInput => return Fed { out, error: false },
            BrotliResult::ResultSuccess => {
                b.done = true;
                return Fed { error: in_off < input.len(), out };
            }
            BrotliResult::ResultFailure => return Fed { out, error: true },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(format: &str, data: &[u8], split: usize) -> Vec<u8> {
        let c = open(format, true).unwrap();
        let mut z = Vec::new();
        for ch in data.chunks(split.max(1)) {
            let f = push(c, ch);
            assert!(!f.error);
            z.extend(f.out);
        }
        z.extend(finish(c).unwrap());
        let d = open(format, false).unwrap();
        let mut o = Vec::new();
        for ch in z.chunks(split.max(1)) {
            let f = push(d, ch);
            assert!(!f.error, "{format}");
            o.extend(f.out);
        }
        o.extend(finish(d).unwrap());
        o
    }

    #[test]
    fn roundtrips() {
        let data: Vec<u8> = (0..20000u32).map(|i| (i * 7 % 251) as u8).collect();
        for f in ["gzip", "deflate", "deflate-raw", "brotli"] {
            for split in [1, 7, 1000, 100000] {
                assert_eq!(roundtrip(f, &data, split), data, "{f} {split}");
            }
            assert_eq!(roundtrip(f, &[], 1), Vec::<u8>::new());
        }
    }

    #[test]
    fn trailing_and_truncated() {
        for f in ["gzip", "deflate", "deflate-raw", "brotli"] {
            let c = open(f, true).unwrap();
            let mut z = push(c, b"hello hello hello").out;
            z.extend(finish(c).unwrap());
            // Trailing junk: the output, then an error.
            let d = open(f, false).unwrap();
            let mut zz = z.clone();
            zz.push(0);
            let fed = push(d, &zz);
            assert!(fed.error, "{f} trailing");
            assert_eq!(fed.out, b"hello hello hello");
            // Truncated: an error at the end.
            let d = open(f, false).unwrap();
            let fed = push(d, &z[..z.len() - 1]);
            assert!(fed.error || finish(d).is_none(), "{f} truncated");
        }
    }
}
