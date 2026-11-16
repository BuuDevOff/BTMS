//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! Textures: a dictionary (`.ytd`, or the one a model carries) and its images, decoded to RGBA.
//! Layout of CodeWalker's `TextureDictionary` (names at 0x20, textures at 0x30) and `Texture` (PC
//! Legacy: name 0x28, width 0x50, height 0x52, row size 0x56, format 0x58, levels 0x5D, pixels
//! 0x70). The row size the file gives (of one row of pixels: for a block format, a quarter of a
//! row of 4×4 blocks) must be the one its format and width give.

use anyhow::{Context, Result, bail};

use crate::formats::resource::{Resource, joaat};

const ROOT: u64 = 0x5000_0000;

#[derive(Clone)]
pub struct Texture {
    pub name: String,
    pub width: usize,
    pub height: usize,
    pub format: u32,
    /// The first level's pixels as stored.
    pub data: Vec<u8>,
}

const DXT1: u32 = u32::from_le_bytes(*b"DXT1");
const DXT3: u32 = u32::from_le_bytes(*b"DXT3");
const DXT5: u32 = u32::from_le_bytes(*b"DXT5");
const ATI1: u32 = u32::from_le_bytes(*b"ATI1");
const ATI2: u32 = u32::from_le_bytes(*b"ATI2");
const BC7: u32 = u32::from_le_bytes(*b"BC7 ");
const A8R8G8B8: u32 = 21;
const A8B8G8R8: u32 = 32;
const L8: u32 = 50;
const A8: u32 = 28;

pub fn format_name(format: u32) -> String {
    match format {
        A8R8G8B8 => "A8R8G8B8".into(),
        A8B8G8R8 => "A8B8G8R8".into(),
        L8 => "L8".into(),
        A8 => "A8".into(),
        f if f > 0xFFFF => String::from_utf8_lossy(&f.to_le_bytes()).trim().to_string(),
        f => format!("format {f}"),
    }
}

/// (bytes per 4×4 block, or per pixel for an uncompressed format; is it in blocks).
fn layout(format: u32) -> Option<(usize, bool)> {
    Some(match format {
        DXT1 | ATI1 => (8, true),
        DXT3 | DXT5 | ATI2 | BC7 => (16, true),
        A8R8G8B8 | A8B8G8R8 => (4, false),
        L8 | A8 => (1, false),
        _ => return None,
    })
}

pub fn parse_ytd(res: &Resource) -> Result<Vec<Texture>> {
    if res.version != 13 {
        bail!("ytd of version {} (13 expected)", res.version);
    }
    parse_texture_dictionary(res, ROOT)
}

/// The names of the textures of a `.ytd` (system pages suffice).
pub fn texture_names(res: &Resource) -> Result<Vec<String>> {
    res.pointers(ROOT + 0x30)?.into_iter().map(|t| res.cstr(res.u64(t + 0x28)?)).collect()
}

pub fn parse_texture_dictionary(res: &Resource, at: u64) -> Result<Vec<Texture>> {
    let (hashes, hash_count) = res.list(at + 0x20)?;
    let textures = res.pointers(at + 0x30)?;
    if hash_count != textures.len() {
        bail!("{hash_count} names for {} textures", textures.len());
    }
    let mut out = Vec::with_capacity(textures.len());
    for (i, t) in textures.into_iter().enumerate() {
        let tex = texture(res, t).with_context(|| format!("texture {i}"))?;
        let hash = res.u32(hashes + i as u64 * 4)?;
        if hash != joaat(&tex.name) {
            bail!("texture {}: its name does not match the dictionary's hash 0x{hash:08X}", tex.name);
        }
        out.push(tex);
    }
    Ok(out)
}

fn texture(res: &Resource, at: u64) -> Result<Texture> {
    let name = res.cstr(res.u64(at + 0x28)?)?;
    let (width, height) = (res.u16(at + 0x50)? as usize, res.u16(at + 0x52)? as usize);
    let row = res.u16(at + 0x56)? as usize;
    let format = res.u32(at + 0x58)?;
    // (the count of mip levels is not used, but read all the same: a texture cut short is refused)
    res.u8(at + 0x5D)?;
    let pixels = res.u64(at + 0x70)?;
    let (unit, blocks) = layout(format).with_context(|| format!("{name}: {} not supported", format_name(format)))?;
    let (row_len, rows) =
        if blocks { (width.div_ceil(4).max(1) * unit, height.div_ceil(4).max(1)) } else { (width * unit, height) };
    // the file gives the size of one row of pixels: a quarter of a row of 4×4 blocks
    // (not checked in a file made by a modding tool: the game does not read it, and such tools
    // get it wrong, giving an ATI2 texture the row size of DXT1)
    let expected = if blocks { row_len / 4 } else { row_len };
    if row != expected && !res.modded {
        bail!("{name}: row of {row} bytes, {expected} expected for {} {width}×{height}", format_name(format));
    }
    let data = res.bytes(pixels, row_len * rows)?.to_vec();
    Ok(Texture { name, width, height, format, data })
}

impl Texture {
    /// The first level in RGBA, 8 bits per channel.
    pub fn rgba(&self) -> Result<Vec<u8>> {
        let (w, h) = (self.width, self.height);
        let mut out = vec![0u8; w * h * 4];
        let (unit, blocks) = layout(self.format).context("format")?;
        if !blocks {
            for (i, px) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let s = &self.data[i * unit..(i + 1) * unit];
                px.copy_from_slice(&match self.format {
                    A8R8G8B8 => [s[2], s[1], s[0], s[3]],
                    A8B8G8R8 => [s[0], s[1], s[2], s[3]],
                    L8 => [s[0], s[0], s[0], 255],
                    _ => [255, 255, 255, s[0]],
                });
            }
            return Ok(out);
        }
        let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
        let mut block = [0u8; 4 * 4 * 4];
        for by in 0..bh {
            for bx in 0..bw {
                let src = &self.data[(by * bw + bx) * unit..][..unit];
                match self.format {
                    DXT1 => bcdec_rs::bc1(src, &mut block, 16),
                    DXT3 => bcdec_rs::bc2(src, &mut block, 16),
                    DXT5 => bcdec_rs::bc3(src, &mut block, 16),
                    BC7 => bcdec_rs::bc7(src, &mut block, 16),
                    ATI1 => {
                        let mut r = [0u8; 16];
                        bcdec_rs::bc4(src, &mut r, 4, false);
                        for (i, v) in r.iter().enumerate() {
                            block[i * 4..i * 4 + 4].copy_from_slice(&[*v, *v, *v, 255]);
                        }
                    }
                    _ => {
                        let mut rg = [0u8; 32];
                        bcdec_rs::bc5(src, &mut rg, 8, false);
                        for i in 0..16 {
                            block[i * 4..i * 4 + 4].copy_from_slice(&[rg[i * 2], rg[i * 2 + 1], 0, 255]);
                        }
                    }
                }
                for y in 0..4 {
                    for x in 0..4 {
                        let (px, py) = (bx * 4 + x, by * 4 + y);
                        if px < w && py < h {
                            out[(py * w + px) * 4..][..4].copy_from_slice(&block[(y * 4 + x) * 4..][..4]);
                        }
                    }
                }
            }
        }
        Ok(out)
    }
}
