// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! AVIF still images: the HEIF container (`avif-parse`) holds AV1 intra
//! frames for the color planes and an optional alpha plane, decoded with
//! rav1d (a Rust port of dav1d) and converted from YUV to RGBA.

use rav1d::include::dav1d::data::Dav1dData;
use rav1d::include::dav1d::dav1d::{Dav1dContext, Dav1dSettings};
use rav1d::include::dav1d::headers::{DAV1D_PIXEL_LAYOUT_I400, DAV1D_PIXEL_LAYOUT_I420, DAV1D_PIXEL_LAYOUT_I422};
use rav1d::include::dav1d::picture::Dav1dPicture;
use rav1d::src::lib::{dav1d_close, dav1d_data_create, dav1d_data_unref, dav1d_default_settings, dav1d_get_picture, dav1d_open, dav1d_picture_unref, dav1d_send_data};
use std::mem::MaybeUninit;
use std::ptr::NonNull;

/// Whether the bytes start like an AVIF (HEIF) file.
pub fn is_avif(bytes: &[u8]) -> bool {
    bytes.len() > 12 && &bytes[4..8] == b"ftyp" && matches!(&bytes[8..12], b"avif" | b"avis" | b"mif1" | b"miaf")
}

/// A decoded plane set: size, layout, bit depth, color info and samples.
struct Planes {
    w: usize,
    h: usize,
    layout: u32,
    bpc: u32,
    mtrx: u32,
    full_range: bool,
    /// Y, U, V samples widened to u16 (U/V empty for monochrome).
    y: Vec<u16>,
    u: Vec<u16>,
    v: Vec<u16>,
    cw: usize,
}

fn decode_av1(obus: &[u8]) -> Option<Planes> {
    // SAFETY: the dav1d API is used as documented: settings are initialized by
    // dav1d_default_settings, the context, data and picture are created and
    // released by the library's own functions, and the picture planes are read
    // within the stride/size the decoder reports.
    unsafe {
        let mut s = MaybeUninit::<Dav1dSettings>::uninit();
        dav1d_default_settings(NonNull::new(s.as_mut_ptr())?);
        let mut s = s.assume_init();
        s.n_threads = 1;
        s.max_frame_delay = 1;
        let mut ctx: Option<Dav1dContext> = None;
        if dav1d_open(NonNull::new(&mut ctx), NonNull::new(&mut s)).0 != 0 {
            return None;
        }
        let mut data = Dav1dData::default();
        let p = dav1d_data_create(NonNull::new(&mut data), obus.len());
        if p.is_null() {
            dav1d_close(NonNull::new(&mut ctx));
            return None;
        }
        std::ptr::copy_nonoverlapping(obus.as_ptr(), p, obus.len());
        let mut pic = Dav1dPicture::default();
        let mut got = false;
        for _ in 0..64 {
            if data.sz > 0 {
                let r = dav1d_send_data(ctx.clone(), NonNull::new(&mut data)).0;
                if r != 0 && r != -(libc_eagain()) {
                    break;
                }
            }
            let r = dav1d_get_picture(ctx.clone(), NonNull::new(&mut pic)).0;
            if r == 0 {
                got = true;
                break;
            }
            if r != -(libc_eagain()) {
                break;
            }
        }
        if data.sz > 0 {
            dav1d_data_unref(NonNull::new(&mut data));
        }
        let out = if got { read_planes(&pic) } else { None };
        if got {
            dav1d_picture_unref(NonNull::new(&mut pic));
        }
        dav1d_close(NonNull::new(&mut ctx));
        out
    }
}

fn libc_eagain() -> i32 {
    // EAGAIN is 11 on Linux and 35 on macOS / BSDs.
    if cfg!(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd")) {
        35
    } else {
        11
    }
}

/// # Safety
/// `pic` must be a picture returned by `dav1d_get_picture` and not yet unref'd.
unsafe fn read_planes(pic: &Dav1dPicture) -> Option<Planes> {
    let (w, h) = (pic.p.w.max(0) as usize, pic.p.h.max(0) as usize);
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return None;
    }
    let bpc = pic.p.bpc as u32;
    let layout = pic.p.layout;
    let seq = pic.seq_hdr?;
    // SAFETY: the sequence header lives as long as the picture.
    let seq = unsafe { seq.as_ref() };
    let (cw, ch) = match layout {
        DAV1D_PIXEL_LAYOUT_I420 => ((w + 1) / 2, (h + 1) / 2),
        DAV1D_PIXEL_LAYOUT_I422 => ((w + 1) / 2, h),
        DAV1D_PIXEL_LAYOUT_I400 => (0, 0),
        _ => (w, h),
    };
    let read = |plane: usize, pw: usize, ph: usize, stride: isize| -> Option<Vec<u16>> {
        let base = pic.data[plane]?.as_ptr() as *const u8;
        let mut out = Vec::with_capacity(pw * ph);
        for y in 0..ph {
            // SAFETY: rows lie within the plane at the reported stride.
            let row = unsafe { base.offset(y as isize * stride) };
            for x in 0..pw {
                let v = if bpc > 8 {
                    // SAFETY: high bit depth samples are 16-bit, row-aligned.
                    unsafe { (row as *const u16).add(x).read_unaligned() }
                } else {
                    // SAFETY: 8-bit samples within the row.
                    unsafe { *row.add(x) as u16 }
                };
                out.push(v);
            }
        }
        Some(out)
    };
    let y = read(0, w, h, pic.stride[0] as isize)?;
    let (u, v) = if cw > 0 { (read(1, cw, ch, pic.stride[1] as isize)?, read(2, cw, ch, pic.stride[1] as isize)?) } else { (Vec::new(), Vec::new()) };
    Some(Planes { w, h, layout, bpc, mtrx: seq.mtrx, full_range: seq.color_range != 0, y, u, v, cw })
}

/// Decode an AVIF file to straight-alpha RGBA8.
pub fn decode(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let avif = avif_parse::read_avif(&mut std::io::Cursor::new(bytes)).ok()?;
    let c = decode_av1(&avif.primary_item)?;
    let alpha = avif.alpha_item.as_ref().and_then(|a| decode_av1(a));
    let max = ((1u32 << c.bpc) - 1) as f32;
    let s = (1u32 << c.bpc.saturating_sub(8)) as f32;
    // Luma/chroma weights for the matrix (unspecified: BT.601, as libavif does).
    let (kr, kb) = match c.mtrx {
        1 => (0.2126, 0.0722),
        9 | 10 => (0.2627, 0.0593),
        7 => (0.212, 0.087),
        4 => (0.30, 0.11),
        _ => (0.299, 0.114),
    };
    let identity = c.mtrx == 0;
    let mut out = vec![0u8; c.w * c.h * 4];
    let chroma = |x: usize, y: usize| -> (f32, f32) {
        if c.cw == 0 {
            return (0.0, 0.0);
        }
        let (cx, cy) = match c.layout {
            DAV1D_PIXEL_LAYOUT_I420 => (x / 2, y / 2),
            DAV1D_PIXEL_LAYOUT_I422 => (x / 2, y),
            _ => (x, y),
        };
        let i = cy * c.cw + cx.min(c.cw - 1);
        let (u, v) = (*c.u.get(i).unwrap_or(&0) as f32, *c.v.get(i).unwrap_or(&0) as f32);
        if c.full_range {
            ((u - (max + 1.0) / 2.0) / max, (v - (max + 1.0) / 2.0) / max)
        } else {
            ((u - 128.0 * s) / (224.0 * s), (v - 128.0 * s) / (224.0 * s))
        }
    };
    let to8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    for yy in 0..c.h {
        for xx in 0..c.w {
            let yv = c.y[yy * c.w + xx] as f32;
            let (r, g, b) = if identity {
                let i = yy * c.cw.max(1) + xx;
                let n = |v: u16| v as f32 / max;
                (n(*c.v.get(i).unwrap_or(&0)), yv / max, n(*c.u.get(i).unwrap_or(&0)))
            } else {
                let yn = if c.full_range { yv / max } else { (yv - 16.0 * s) / (219.0 * s) };
                let (cb, cr) = chroma(xx, yy);
                let r = yn + 2.0 * (1.0 - kr) * cr;
                let b = yn + 2.0 * (1.0 - kb) * cb;
                let g = (yn - kr * r - kb * b) / (1.0 - kr - kb);
                (r, g, b)
            };
            let o = (yy * c.w + xx) * 4;
            out[o] = to8(r);
            out[o + 1] = to8(g);
            out[o + 2] = to8(b);
            out[o + 3] = 255;
        }
    }
    if let Some(a) = alpha.filter(|a| a.w == c.w && a.h == c.h) {
        let amax = ((1u32 << a.bpc) - 1) as f32;
        let asc = (1u32 << a.bpc.saturating_sub(8)) as f32;
        for i in 0..c.w * c.h {
            let v = a.y[i] as f32;
            let an = if a.full_range { v / amax } else { (v - 16.0 * asc) / (219.0 * asc) };
            let an = an.clamp(0.0, 1.0);
            out[i * 4 + 3] = to8(an);
            if avif.premultiplied_alpha && an > 0.0 {
                for k in 0..3 {
                    out[i * 4 + k] = ((out[i * 4 + k] as f32 / an).round().min(255.0)) as u8;
                }
            }
        }
    }
    Some((c.w as u32, c.h as u32, out))
}
