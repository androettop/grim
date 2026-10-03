use grim_package::property::read_name;
use grim_package::{Error, NameIndex, Reader, Result, Unknown};

use crate::lazy::read_lazy_bytes;
use crate::Ctx;

/// Set exactly when [`Sound::trailer`] is present, in all 8195 Sounds (0x104/0x184 in XA,
/// 0x0/0x80 in wav).
pub const SOUND_HAS_TRAILER: u32 = 0x80;

/// `Engine.Sound`, KnowWonder variant.
///
/// ```text
/// FName  format            "XA" (ADPCM) or "wav" (full RIFF)
/// u32    flags             SOUND_HAS_TRAILER; other bits unknown
/// f32    duration          seconds; XA: == sample_count / sample_rate (verified)
/// u32    sample_count      0 in wav
/// u32    bits_per_sample   0 in wav
/// u32    channels          0 in wav
/// u32    sample_rate       0 in wav
/// lazy<u8> data
/// [lazy<u8> trailer]       only if flags & SOUND_HAS_TRAILER
/// ```
#[derive(Debug, Clone)]
pub struct Sound {
    pub format: NameIndex,
    pub flags: u32,
    pub duration: f32,
    pub sample_count: u32,
    pub bits_per_sample: u32,
    pub channels: u32,
    pub sample_rate: u32,
    pub data: Vec<u8>,
    /// Present in dialog and some effects; content unknown (lip-sync data?).
    pub trailer: Option<Unknown>,
}

impl Sound {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let format = read_name(cx.pkg, r)?;
        let flags = r.u32()?;
        let duration = r.f32()?;
        let sample_count = r.u32()?;
        let bits_per_sample = r.u32()?;
        let channels = r.u32()?;
        let sample_rate = r.u32()?;
        let (_, data) = read_lazy_bytes(r, cx.version(), 1)?;
        let data = data.to_vec();
        let trailer = if flags & SOUND_HAS_TRAILER != 0 {
            let (_, t) = read_lazy_bytes(r, cx.version(), 1)?;
            Some(Unknown::new(r.offset() - t.len(), t))
        } else {
            None
        };
        if !r.is_at_end() {
            return Err(Error::invalid(r.offset(), format!("Sound with flags {flags:#x}: {} bytes left over", r.remaining())));
        }
        Ok(Self { format, flags, duration, sample_count, bits_per_sample, channels, sample_rate, data, trailer })
    }
}

/// XA filter coefficients (/64), indexed by the high nibble of each block header.
const XA_FILTERS: [(i32, i32); 5] = [(0, 0), (60, 0), (115, -52), (98, -55), (122, -60)];

/// Decodes an `XA` sound to 16-bit PCM.
///
/// PlayStation-style ADPCM without the flags byte: 15-byte blocks, a header (high nibble:
/// filter, low nibble: shift) and 28 signed 4-bit residuals, **high nibble first**. Sizes match
/// every XA sound (`len == ceil(samples / 28) * 15`); the nibble order was confirmed by ear.
pub fn decode_xa(s: &Sound) -> Result<Vec<i16>> {
    let mut out = Vec::with_capacity(s.sample_count as usize);
    let (mut s1, mut s2) = (0i32, 0i32);
    for (k, block) in s.data.chunks(15).enumerate() {
        if block.len() != 15 {
            return Err(Error::invalid(k * 15, "truncated XA block (offset within data)"));
        }
        let shift = (block[0] & 0x0f) as i32;
        let filter = (block[0] >> 4) as usize;
        let (k0, k1) = *XA_FILTERS
            .get(filter)
            .ok_or_else(|| Error::invalid(k * 15, format!("XA filter {filter} out of range")))?;
        for &b in &block[1..] {
            for nib in [b >> 4, b & 0x0f] {
                let v = ((nib << 4) as i8 >> 4) as i32;
                let smp = (((v << 12) >> shift) + ((s1 * k0 + s2 * k1 + 32) >> 6)).clamp(-32768, 32767);
                s2 = s1;
                s1 = smp;
                out.push(smp as i16);
            }
        }
    }
    out.truncate(s.sample_count as usize);
    Ok(out)
}

/// A sound ready to play: 16-bit PCM with the rate and channel count it was recorded at.
#[derive(Debug, Clone)]
pub struct Pcm {
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

/// Decodes a sound of either format the game uses. `XA` carries its own rate and channels;
/// `wav` carries a whole RIFF file, whose header says them.
pub fn decode(pkg: &grim_package::Package, s: &Sound) -> Result<Pcm> {
    match pkg.name(s.format).to_ascii_lowercase().as_str() {
        "xa" => Ok(Pcm { rate: s.sample_rate, channels: s.channels.max(1) as u16, samples: decode_xa(s)? }),
        "wav" => decode_wav(&s.data),
        f => Err(Error::invalid(0, format!("unknown sound format {f}"))),
    }
}

/// Reads a RIFF/WAVE file: the `fmt ` chunk for the rate and channels, the `data` chunk for the
/// samples. Only PCM is accepted, which is what the game ships.
pub fn decode_wav(bytes: &[u8]) -> Result<Pcm> {
    let at = |i: usize, n: usize| bytes.get(i..i + n);
    let u16_at = |i: usize| at(i, 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |i: usize| at(i, 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if at(0, 4) != Some(b"RIFF") || at(8, 4) != Some(b"WAVE") {
        return Err(Error::invalid(0, "not a RIFF/WAVE sound"));
    }
    let (mut rate, mut channels, mut bits) = (0u32, 1u16, 16u16);
    let mut samples = Vec::new();
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = at(i, 4).ok_or_else(|| Error::invalid(i, "truncated RIFF chunk"))?;
        let size = u32_at(i + 4).ok_or_else(|| Error::invalid(i + 4, "truncated RIFF chunk size"))? as usize;
        let body = i + 8;
        match id {
            b"fmt " => {
                let format = u16_at(body).ok_or_else(|| Error::invalid(body, "truncated fmt chunk"))?;
                if format != 1 {
                    return Err(Error::invalid(body, format!("WAVE format {format} is not PCM")));
                }
                channels = u16_at(body + 2).unwrap_or(1).max(1);
                rate = u32_at(body + 4).unwrap_or(22050);
                bits = u16_at(body + 14).unwrap_or(16);
            }
            b"data" => {
                let end = (body + size).min(bytes.len());
                let data = &bytes[body..end];
                samples = match bits {
                    8 => data.iter().map(|&b| (b as i16 - 128) << 8).collect(),
                    16 => data.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect(),
                    b => return Err(Error::invalid(body, format!("{b}-bit WAVE samples"))),
                };
            }
            _ => {}
        }
        // Chunks are padded to an even length.
        i = body + size + (size & 1);
    }
    if rate == 0 {
        return Err(Error::invalid(0, "WAVE without a fmt chunk"));
    }
    Ok(Pcm { rate, channels, samples })
}
