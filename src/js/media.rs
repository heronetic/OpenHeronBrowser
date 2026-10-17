// Copyright 2026 Marissa Tanner, Heronetic
// SPDX-License-Identifier: Apache-2.0

//! Media services for scripts: audio decoding (`decodeAudioData`) and media
//! probing (duration, tracks, video size) for `<audio>`/`<video>`. Decoding
//! is pure Rust (Symphonia: MP3, AAC/MP4, Vorbis/Ogg, FLAC, WAV/PCM/ADPCM,
//! ALAC, Matroska audio).

use std::io::Cursor;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::host::*;
use crate::page::with_page;

/// Decoded PCM, one Vec per channel.
pub struct Pcm {
    pub rate: u32,
    pub channels: Vec<Vec<f32>>,
}

fn open(bytes: Vec<u8>) -> Option<Box<dyn FormatReader>> {
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let fmt = FormatOptions { enable_gapless: true, ..Default::default() };
    symphonia::default::get_probe().format(&Hint::new(), mss, &fmt, &MetadataOptions::default()).ok().map(|p| p.format)
}

/// Decode the first audio track completely.
pub fn decode(bytes: Vec<u8>) -> Option<Pcm> {
    let mut format = open(bytes)?;
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL)?.clone();
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).ok()?;
    let mut rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut errors = 0;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(_) => break,
        };
        if packet.track_id() != track.id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = *buf.spec();
                let n = spec.channels.count();
                if n == 0 || buf.frames() == 0 {
                    continue;
                }
                rate = spec.rate;
                let mut sb = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
                sb.copy_planar_ref(buf);
                let samples = sb.samples();
                let frames = samples.len() / n;
                if channels.len() < n {
                    let len = channels.first().map(|c| c.len()).unwrap_or(0);
                    channels.resize(n, vec![0.0; len]);
                }
                for (c, ch) in channels.iter_mut().enumerate().take(n) {
                    ch.extend_from_slice(&samples[c * frames..(c + 1) * frames]);
                }
            }
            Err(Error::DecodeError(_)) => {
                errors += 1;
                if errors > 64 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if channels.is_empty() || rate == 0 {
        return None;
    }
    Some(Pcm { rate, channels })
}

/// Linear-interpolation resampling to `to` Hz.
pub fn resample(pcm: Pcm, to: u32) -> Pcm {
    if to == 0 || to == pcm.rate {
        return pcm;
    }
    let ratio = pcm.rate as f64 / to as f64;
    let channels = pcm
        .channels
        .into_iter()
        .map(|src| {
            if src.is_empty() {
                return src;
            }
            let n = ((src.len() as f64) / ratio).round().max(1.0) as usize;
            (0..n)
                .map(|i| {
                    let x = i as f64 * ratio;
                    let i0 = (x.floor() as usize).min(src.len() - 1);
                    let i1 = (i0 + 1).min(src.len() - 1);
                    let f = (x - i0 as f64) as f32;
                    src[i0] + (src[i1] - src[i0]) * f
                })
                .collect()
        })
        .collect();
    Pcm { rate: to, channels }
}

/// audioDecode(bytes, sampleRate) -> [rate, channels, frames, planar f32 bytes] or null.
fn n_audio_decode(a: &mut dyn HostArgs) -> HostResult {
    let bytes = match a.value(0)? {
        HostValue::Bytes(b) => b,
        _ => return Ok(HostValue::Null),
    };
    let to = num(a, 1);
    let Some(pcm) = decode(bytes) else { return Ok(HostValue::Null) };
    let pcm = if to.is_finite() && to >= 3000.0 { resample(pcm, to as u32) } else { pcm };
    let frames = pcm.channels.first().map(|c| c.len()).unwrap_or(0);
    let mut out = Vec::with_capacity(frames * pcm.channels.len() * 4);
    for ch in &pcm.channels {
        for s in ch {
            out.extend_from_slice(&s.to_le_bytes());
        }
    }
    Ok(HostValue::Array(vec![HostValue::from(pcm.rate), HostValue::from(pcm.channels.len()), HostValue::from(frames), HostValue::Bytes(out)]))
}

// ------------------------------------------------------------------ probing

#[derive(Default, Debug, PartialEq)]
pub struct MediaInfo {
    pub duration: Option<f64>,
    pub audio: bool,
    pub video: bool,
    pub width: u32,
    pub height: u32,
}

fn be32(b: &[u8], i: usize) -> Option<u32> {
    b.get(i..i + 4).map(|x| u32::from_be_bytes([x[0], x[1], x[2], x[3]]))
}
fn be64(b: &[u8], i: usize) -> Option<u64> {
    b.get(i..i + 8).map(|x| u64::from_be_bytes([x[0], x[1], x[2], x[3], x[4], x[5], x[6], x[7]]))
}

/// ISO BMFF boxes in `b[start..end]`: (type, body start, body end).
fn boxes(b: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut i = start;
    while i + 8 <= end {
        let Some(size32) = be32(b, i) else { break };
        let kind = [b[i + 4], b[i + 5], b[i + 6], b[i + 7]];
        let (hdr, size) = match size32 {
            1 => match be64(b, i + 8) {
                Some(s) => (16usize, s as usize),
                None => break,
            },
            0 => (8, end - i),
            s => (8, s as usize),
        };
        if size < hdr || i + size > end {
            out.push((kind, i + hdr, end));
            break;
        }
        out.push((kind, i + hdr, i + size));
        i += size;
    }
    out
}

/// Duration and tracks of an MP4/MOV file from its `moov` box.
fn probe_mp4(b: &[u8]) -> Option<MediaInfo> {
    let top = boxes(b, 0, b.len());
    if !top.iter().any(|(k, _, _)| k == b"ftyp" || k == b"moov") {
        return None;
    }
    let &(_, ms, me) = top.iter().find(|(k, _, _)| k == b"moov")?;
    let mut info = MediaInfo::default();
    for (k, s, e) in boxes(b, ms, me) {
        match &k {
            b"mvhd" => {
                let v = *b.get(s)?;
                let (scale, dur) = if v == 1 { (be32(b, s + 20)?, be64(b, s + 24)?) } else { (be32(b, s + 12)?, be32(b, s + 16)? as u64) };
                if scale > 0 && dur != u64::MAX && dur != u32::MAX as u64 {
                    info.duration = Some(dur as f64 / scale as f64);
                }
            }
            b"trak" => {
                let kids = boxes(b, s, e);
                let mut wh = (0, 0);
                if let Some(&(_, ts, _)) = kids.iter().find(|(k, _, _)| k == b"tkhd") {
                    let v = *b.get(ts)?;
                    let off = if v == 1 { ts + 88 } else { ts + 76 };
                    wh = (be32(b, off).unwrap_or(0) >> 16, be32(b, off + 4).unwrap_or(0) >> 16);
                }
                let handler = kids.iter().find(|(k, _, _)| k == b"mdia").and_then(|&(_, s, e)| boxes(b, s, e).into_iter().find(|(k, _, _)| k == b"hdlr")).and_then(|(_, hs, _)| b.get(hs + 8..hs + 12).map(|x| [x[0], x[1], x[2], x[3]]));
                match handler.as_ref().map(|h| &h[..]) {
                    Some(b"vide") => {
                        info.video = true;
                        if info.width == 0 {
                            info.width = wh.0;
                            info.height = wh.1;
                        }
                    }
                    Some(b"soun") => info.audio = true,
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Some(info)
}

/// An EBML variable-length integer at `i`: (value, length). IDs keep their marker bits.
fn vint(b: &[u8], i: usize, keep_marker: bool) -> Option<(u64, usize)> {
    let first = *b.get(i)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return None;
    }
    let mut v = if keep_marker { first as u64 } else { (first as u64) & ((1u64 << (8 - len)) - 1) };
    for k in 1..len {
        v = (v << 8) | *b.get(i + k)? as u64;
    }
    Some((v, len))
}

/// Elements in `b[start..end]`: (id, data start, data end). Unknown sizes run to `end`.
fn ebml_children(b: &[u8], start: usize, end: usize) -> Vec<(u64, usize, usize)> {
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        let Some((id, il)) = vint(b, i, true) else { break };
        let Some((size, sl)) = vint(b, i + il, false) else { break };
        let ds = i + il + sl;
        let unknown = size == (1u64 << (7 * sl)) - 1;
        let de = if unknown { end } else { (ds as u64 + size).min(end as u64) as usize };
        out.push((id, ds, de));
        if de <= i {
            break;
        }
        i = de;
    }
    out
}

fn ebml_uint(b: &[u8], s: usize, e: usize) -> u64 {
    b[s..e.min(b.len())].iter().fold(0u64, |v, x| (v << 8) | *x as u64)
}

/// Duration, tracks and video size of a WebM/Matroska file.
fn probe_mkv(b: &[u8]) -> Option<MediaInfo> {
    if !b.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return None;
    }
    let top = ebml_children(b, 0, b.len());
    let &(_, ss, se) = top.iter().find(|(id, _, _)| *id == 0x1853_8067)?;
    let mut info = MediaInfo::default();
    let mut scale = 1_000_000u64;
    let mut dur: Option<f64> = None;
    for (id, s, e) in ebml_children(b, ss, se) {
        match id {
            0x1549_A966 => {
                for (cid, cs, ce) in ebml_children(b, s, e) {
                    match cid {
                        0x2A_D7B1 => scale = ebml_uint(b, cs, ce).max(1),
                        0x4489 if ce - cs == 8 => dur = Some(f64::from_be_bytes(b.get(cs..cs + 8)?.try_into().ok()?)),
                        0x4489 if ce - cs == 4 => dur = Some(f32::from_be_bytes(b.get(cs..cs + 4)?.try_into().ok()?) as f64),
                        _ => {}
                    }
                }
            }
            0x1654_AE6B => {
                for (tid, ts, te) in ebml_children(b, s, e) {
                    if tid != 0xAE {
                        continue;
                    }
                    for (fid, fs, fe) in ebml_children(b, ts, te) {
                        match fid {
                            0x83 => match ebml_uint(b, fs, fe) {
                                1 => info.video = true,
                                2 => info.audio = true,
                                _ => {}
                            },
                            0xE0 => {
                                info.video = true;
                                for (vid, vs, ve) in ebml_children(b, fs, fe) {
                                    match vid {
                                        0xB0 if info.width == 0 => info.width = ebml_uint(b, vs, ve) as u32,
                                        0xBA if info.height == 0 => info.height = ebml_uint(b, vs, ve) as u32,
                                        _ => {}
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            // Clusters follow; everything needed comes before them.
            0x1F43_B675 => break,
            _ => {}
        }
    }
    info.duration = dur.map(|d| d * scale as f64 / 1e9);
    Some(info)
}

/// Probe a media resource: container, tracks, duration, video size.
pub fn probe(bytes: Vec<u8>) -> Option<MediaInfo> {
    if let Some(i) = probe_mp4(&bytes) {
        return Some(i);
    }
    if let Some(i) = probe_mkv(&bytes) {
        if i.video || i.duration.is_some() {
            return Some(i);
        }
    }
    let mut format = open(bytes)?;
    let mut info = MediaInfo::default();
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).cloned();
    let Some(track) = track else { return Some(info) };
    info.audio = true;
    let p = &track.codec_params;
    let tb = p.time_base.or_else(|| p.sample_rate.map(|r| symphonia::core::units::TimeBase::new(1, r)));
    if let (Some(tb), Some(n)) = (tb, p.n_frames) {
        let t = tb.calc_time(n);
        info.duration = Some(t.seconds as f64 + t.frac);
    } else if let Some(tb) = tb {
        // No frame count in the headers (e.g. MP3 without a Xing frame): sum packet durations.
        let mut total: u64 = 0;
        let mut seen = false;
        while let Ok(pk) = format.next_packet() {
            if pk.track_id() == track.id {
                total += pk.dur;
                seen = true;
            }
        }
        if seen {
            let t = tb.calc_time(total);
            info.duration = Some(t.seconds as f64 + t.frac);
        }
    }
    Some(info)
}

/// mediaProbe(fetch body id | bytes) -> {duration, audio, video, width, height} or null.
fn n_media_probe(a: &mut dyn HostArgs) -> HostResult {
    let bytes = match a.value(0)? {
        HostValue::Bytes(b) => b,
        HostValue::Number(id) => match with_page(|p| p.fetch_bodies.take(id as u64)).flatten() {
            Some(b) => b.as_ref().clone(),
            None => return Ok(HostValue::Null),
        },
        _ => return Ok(HostValue::Null),
    };
    Ok(match probe(bytes) {
        Some(i) => HostValue::object(vec![
            ("duration", i.duration.map(HostValue::from).unwrap_or(HostValue::Number(f64::NAN))),
            ("audio", HostValue::Bool(i.audio)),
            ("video", HostValue::Bool(i.video)),
            ("width", HostValue::from(i.width)),
            ("height", HostValue::from(i.height)),
        ]),
        None => HostValue::Null,
    })
}

pub static MEDIA_NATIVES: &[(&str, NativeFn, u32)] = &[("audioDecode", n_audio_decode, 2), ("mediaProbe", n_media_probe, 1)];

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, samples: &[i16]) -> Vec<u8> {
        let mut v = Vec::new();
        let data = samples.len() as u32 * 2;
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data).to_le_bytes());
        v.extend_from_slice(b"WAVEfmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * 2).to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data.to_le_bytes());
        for s in samples {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    #[test]
    fn decodes_and_probes_wav() {
        let samples: Vec<i16> = (0..8000).map(|i| if i % 2 == 0 { 16384 } else { -16384 }).collect();
        let pcm = decode(wav(8000, &samples)).expect("decode");
        assert_eq!(pcm.rate, 8000);
        assert_eq!(pcm.channels.len(), 1);
        assert_eq!(pcm.channels[0].len(), 8000);
        assert!((pcm.channels[0][0] - 0.5).abs() < 1e-3);
        let up = resample(pcm, 16000);
        assert_eq!(up.channels[0].len(), 16000);
        let info = probe(wav(8000, &samples)).expect("probe");
        assert!(info.audio && !info.video);
        assert!((info.duration.unwrap() - 1.0).abs() < 1e-6);
        assert!(probe(b"not media at all".to_vec()).is_none());
    }
}
