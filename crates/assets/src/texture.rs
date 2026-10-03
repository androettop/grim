use grim_package::{Error, Reader, Result};

use crate::palette::Palette;

use crate::lazy::read_lazy_bytes;
use crate::{check, Ctx};

/// UE1 `ETextureFormat` (`Format` and `CompFormat` properties).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureFormat {
    P8,
    Rgba7,
    Rgb16,
    Dxt1,
    Rgb8,
    Rgba8,
}

impl TextureFormat {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Self::P8,
            1 => Self::Rgba7,
            2 => Self::Rgb16,
            3 => Self::Dxt1,
            4 => Self::Rgb8,
            5 => Self::Rgba8,
            _ => return None,
        })
    }

    pub fn mip_bytes(self, u: usize, v: usize) -> usize {
        match self {
            Self::P8 => u * v,
            Self::Rgba7 | Self::Rgba8 => u * v * 4,
            Self::Rgb16 => u * v * 2,
            Self::Rgb8 => u * v * 3,
            Self::Dxt1 => u.max(4) * v.max(4) / 2,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Mip {
    pub u_size: i32,
    pub v_size: i32,
    pub u_bits: u8,
    pub v_bits: u8,
    pub data: Vec<u8>,
}

/// Mips in `format` plus, when `bHasComp`, a second chain in `comp_format`.
#[derive(Debug, Clone)]
pub struct Texture {
    pub format: TextureFormat,
    pub mips: Vec<Mip>,
    pub comp_format: Option<TextureFormat>,
    pub comp_mips: Vec<Mip>,
}

/// Empty mip data is only allowed in procedural textures.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MipData {
    Required,
    MayBeEmpty,
}

fn read_mips(r: &mut Reader, version: u16, format: TextureFormat, policy: MipData) -> Result<Vec<Mip>> {
    // Minimum on-disk mip: [i32 lazy] + compact + 2×i32 + 2×u8.
    let min = if version < crate::lazy::LAZY_SEEK_VERSION { 11 } else { 15 };
    let n = r.count(min)?;
    (0..n)
        .map(|_| {
            let data_at = r.offset();
            let (_, data) = read_lazy_bytes(r, version, 1)?;
            let data = data.to_vec();
            let at = r.offset();
            let m = Mip { u_size: r.i32()?, v_size: r.i32()?, u_bits: r.u8()?, v_bits: r.u8()?, data };
            check(at, "USize vs UBits", m.u_size, 1 << m.u_bits)?;
            check(at, "VSize vs VBits", m.v_size, 1 << m.v_bits)?;
            let want = format.mip_bytes(m.u_size as usize, m.v_size as usize);
            if !(policy == MipData::MayBeEmpty && m.data.is_empty()) {
                check(data_at, &format!("{}x{} {format:?} mip bytes", m.u_size, m.v_size), m.data.len(), want)?;
            }
            Ok(m)
        })
        .collect()
}

pub(crate) fn texture_format(cx: &Ctx, prop: &str, at: usize) -> Result<Option<TextureFormat>> {
    match cx.base.byte_property(cx.pkg, prop) {
        None => Ok(None),
        Some(v) => TextureFormat::from_u8(v)
            .map(Some)
            .ok_or_else(|| Error::invalid(at, format!("unknown {prop}: {v}"))),
    }
}

impl Texture {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        Self::parse_with(cx, r, MipData::Required)
    }

    pub(crate) fn parse_with(cx: &Ctx, r: &mut Reader, policy: MipData) -> Result<Self> {
        let at = r.offset();
        // No `Format` property: class default, P8.
        let format = texture_format(cx, "Format", at)?.unwrap_or(TextureFormat::P8);
        let mips = read_mips(r, cx.version(), format, policy)?;
        let has_comp = cx.base.bool_property(cx.pkg, "bHasComp").unwrap_or(false);
        let (comp_format, comp_mips) = if has_comp {
            let f = texture_format(cx, "CompFormat", at)?
                .ok_or_else(|| Error::invalid(at, "bHasComp without CompFormat"))?;
            (Some(f), read_mips(r, cx.version(), f, policy)?)
        } else {
            (None, Vec::new())
        };
        Ok(Self { format, mips, comp_format, comp_mips })
    }
}


/// Decoded image, RGBA8.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Copies the colour of opaque neighbours into fully transparent texels, so filtering does not
/// blend the palette's transparent colour (often magenta) into masked edges.
fn bleed_transparent(px: &mut [u8], w: usize, h: usize) {
    for _ in 0..2 {
        let source = px.to_vec();
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                if source[i + 3] != 0 {
                    continue;
                }
                let (mut sum, mut n) = ([0u32; 3], 0u32);
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                            continue;
                        }
                        let j = (ny as usize * w + nx as usize) * 4;
                        if source[j + 3] == 0 {
                            continue;
                        }
                        for c in 0..3 {
                            sum[c] += source[j + c] as u32;
                        }
                        n += 1;
                    }
                }
                if n > 0 {
                    for c in 0..3 {
                        px[i + c] = (sum[c] / n) as u8;
                    }
                }
            }
        }
    }
}

pub fn decode_mip(mip: &Mip, format: TextureFormat, palette: Option<&Palette>, masked: bool) -> std::result::Result<Rgba, String> {
    let (w, h) = (mip.u_size as usize, mip.v_size as usize);
    let mut px = vec![0u8; w * h * 4];
    match format {
        TextureFormat::P8 => {
            let pal = palette.ok_or("P8 texture without palette")?;
            for (i, &idx) in mip.data.iter().enumerate() {
                let c = pal.colors.get(idx as usize).ok_or("palette index out of range")?;
                let a = if masked && idx == 0 { 0 } else { 255 };
                px[i * 4..i * 4 + 4].copy_from_slice(&[c.r, c.g, c.b, a]);
            }
        }
        TextureFormat::Dxt1 => decode_dxt1(&mip.data, w, h, &mut px),
        TextureFormat::Rgba8 => {
            // Hypothesis: BGRA byte order. To be checked visually.
            for (i, c) in mip.data.chunks_exact(4).enumerate() {
                px[i * 4..i * 4 + 4].copy_from_slice(&[c[2], c[1], c[0], c[3]]);
            }
        }
        other => return Err(format!("format {other:?} not decoded by the viewer yet")),
    }
    if masked {
        bleed_transparent(&mut px, w, h);
    }
    Ok(Rgba { width: w as u32, height: h as u32, pixels: px })
}

fn rgb565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 31) as u32;
    let g = ((c >> 5) & 63) as u32;
    let b = (c & 31) as u32;
    [(r * 255 / 31) as u8, (g * 255 / 63) as u8, (b * 255 / 31) as u8]
}

/// Standard DXT1 / BC1.
fn decode_dxt1(data: &[u8], w: usize, h: usize, px: &mut [u8]) {
    let bw = w.max(4) / 4;
    for (bi, block) in data.chunks_exact(8).enumerate() {
        let (bx, by) = (bi % bw, bi / bw);
        let c0 = u16::from_le_bytes([block[0], block[1]]);
        let c1 = u16::from_le_bytes([block[2], block[3]]);
        let (a, b) = (rgb565(c0), rgb565(c1));
        let mix = |x: u8, y: u8, wx: u32, wy: u32| ((x as u32 * wx + y as u32 * wy) / (wx + wy)) as u8;
        let palette: [[u8; 4]; 4] = if c0 > c1 {
            [
                [a[0], a[1], a[2], 255],
                [b[0], b[1], b[2], 255],
                [mix(a[0], b[0], 2, 1), mix(a[1], b[1], 2, 1), mix(a[2], b[2], 2, 1), 255],
                [mix(a[0], b[0], 1, 2), mix(a[1], b[1], 1, 2), mix(a[2], b[2], 1, 2), 255],
            ]
        } else {
            [
                [a[0], a[1], a[2], 255],
                [b[0], b[1], b[2], 255],
                [mix(a[0], b[0], 1, 1), mix(a[1], b[1], 1, 1), mix(a[2], b[2], 1, 1), 255],
                [0, 0, 0, 0],
            ]
        };
        let bits = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
        for j in 0..16 {
            let (x, y) = (bx * 4 + j % 4, by * 4 + j / 4);
            if x < w && y < h {
                let c = palette[((bits >> (2 * j)) & 3) as usize];
                px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&c);
            }
        }
    }
}

