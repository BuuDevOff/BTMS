//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The JPEG of a tile, byte for byte the file the `image` crate's encoder writes, in a third of
//! the time.
//!
//! Encoding the 21 tiles of a block took as long as a fifth of the whole block: the `image`
//! encoder reads its pixels one by one through a generic view, rounds each of its 786 432
//! coefficients per tile with a call to the C library, and hands its output to the writer one
//! byte at a time. This module does the same arithmetic on whole 8 × 8 blocks of the tile's own
//! bytes and writes into a buffer:
//!
//! - the same colour conversion (fixed point), the same integer cosine transform, the same
//!   Huffman coding;
//! - the same quantization: the `image` encoder divides in floats and rounds half away from
//!   zero; here the same quotient is taken in integers ([`Quantizer`]), which gives the same
//!   result for every coefficient a picture can have (checked one by one in the tests);
//! - the header (quantization tables for the quality, Huffman tables, frame) is the one the
//!   `image` encoder itself writes: it is asked for once and its tables are read back from it, so
//!   that this module holds no table of its own that could drift.
//!
//! The forward cosine transform is the accurate integer one of the Independent JPEG Group's
//! libjpeg (`jfdctint.c`), which the `image` crate also uses: this software is based in part on
//! the work of the Independent JPEG Group.
//!
//! The tests compare whole files with the `image` encoder's; `BTMS_CHECK_JPEG=1` does so for
//! every tile written.

use std::sync::LazyLock;

use anyhow::{Context, Result, bail, ensure};

/// The order coefficients are written in (zigzag): position in the file → index in the block.
#[rustfmt::skip]
const UNZIGZAG: [u8; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// (code length in bits, code) of every symbol of a Huffman table; length 17: not in the table.
type Codes = [(u8, u16); 256];

/// What does not change from one picture to the next, for a size and a quality.
pub struct Encoder {
    width: usize,
    height: usize,
    /// Everything before the picture's data: what the `image` encoder writes up to the scan header.
    head: Vec<u8>,
    /// Quantization tables (luminance, chrominance), in block order.
    quantizers: [Quantizer; 2],
    /// Huffman codes: DC then AC, for luminance then chrominance.
    dc: [Codes; 2],
    ac: [Codes; 2],
}

/// The `image` crate's own encoding (4:4:4, baseline), the reference.
pub fn reference(rgb: &[u8], width: usize, height: usize, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality).encode(
        rgb,
        width as u32,
        height as u32,
        image::ExtendedColorType::Rgb8,
    )?;
    Ok(out)
}

impl Encoder {
    /// An encoder of `width` × `height` pictures (multiples of 8) at `quality`.
    pub fn new(width: usize, height: usize, quality: u8) -> Result<Encoder> {
        ensure!(
            width.is_multiple_of(8) && height.is_multiple_of(8) && width > 0 && height > 0,
            "JPEG: {width}×{height} is not made of 8 × 8 blocks"
        );
        // the header and the tables are the `image` encoder's own: read back from a file it writes
        let file = reference(&vec![0u8; width * height * 3], width, height, quality)?;
        ensure!(file.starts_with(&[0xFF, 0xD8]), "JPEG: no start of image");
        let (mut at, mut tables, mut head) = (2, [[0u8; 64]; 2], None);
        let (mut dc, mut ac) = ([[(17u8, 0u16); 256]; 2], [[(17u8, 0u16); 256]; 2]);
        while head.is_none() {
            let segment = file.get(at..at + 4).context("JPEG: truncated header")?;
            ensure!(segment[0] == 0xFF, "JPEG: marker expected");
            let len = u16::from_be_bytes([segment[2], segment[3]]) as usize;
            let body = file.get(at + 4..at + 2 + len).context("JPEG: truncated segment")?;
            match segment[1] {
                // quantization table: its number, then its 64 values in zigzag order
                0xDB => {
                    ensure!(body.len() == 65 && body[0] < 2, "JPEG: unexpected quantization table");
                    for (i, &v) in body[1..].iter().enumerate() {
                        tables[body[0] as usize][UNZIGZAG[i] as usize] = v;
                    }
                }
                // Huffman table: class (DC, AC) and number, how many codes of each length, the symbols
                0xC4 => {
                    ensure!(body.len() >= 17 && body[0] & 0xEE == 0, "JPEG: unexpected Huffman table");
                    let codes = huffman_codes(body[1..17].try_into().unwrap(), &body[17..])?;
                    let which = (body[0] & 1) as usize;
                    if body[0] >> 4 == 0 { dc[which] = codes } else { ac[which] = codes }
                }
                // the scan header: the picture's data follows
                0xDA => head = Some(file[..at + 2 + len].to_vec()),
                _ => {}
            }
            at += 2 + len;
        }
        Ok(Encoder { width, height, head: head.unwrap(), quantizers: tables.map(|t| Quantizer::new(&t)), dc, ac })
    }

    /// The JPEG file of a picture given as RGBA bytes (alpha is not looked at).
    pub fn encode_rgba(&self, rgba: &[u8]) -> Vec<u8> {
        assert_eq!(rgba.len(), self.width * self.height * 4, "JPEG: picture of the wrong size");
        let mut bits = Bits { out: Vec::with_capacity(self.head.len() + rgba.len() / 8), pending: 0, count: 0 };
        bits.out.extend_from_slice(&self.head);
        let mut previous = [0i32; 3];
        let mut samples = [[0u8; 64]; 3];
        let mut block = [0i32; 64];
        for by in (0..self.height).step_by(8) {
            for bx in (0..self.width).step_by(8) {
                // RGB to YCbCr (BT.601 full range, in 16.16 fixed point)
                for y in 0..8 {
                    let row = &rgba[((by + y) * self.width + bx) * 4..][..32];
                    for (x, p) in row.as_chunks::<4>().0.iter().enumerate() {
                        let (r, g, b) = (p[0] as usize, p[1] as usize, p[2] as usize);
                        for c in 0..3 {
                            samples[c][y * 8 + x] = ((YCBCR[c][0][r] + YCBCR[c][1][g] + YCBCR[c][2][b]) >> 16) as u8;
                        }
                    }
                }
                for c in 0..3 {
                    let table = (c > 0) as usize;
                    cosine_transform(&samples[c], &mut block);
                    self.quantizers[table].apply(&mut block);
                    previous[c] = bits.block(&block, previous[c], &self.dc[table], &self.ac[table]);
                }
            }
        }
        bits.finish()
    }
}

/// The codes of a Huffman table from how many codes each length has and its symbols in order
/// (JPEG annex C): codes of a length count up, and gain a zero bit each time the length grows.
fn huffman_codes(counts: &[u8; 16], symbols: &[u8]) -> Result<Codes> {
    ensure!(counts.iter().map(|&n| n as usize).sum::<usize>() == symbols.len(), "JPEG: inconsistent Huffman table");
    let mut codes = [(17u8, 0u16); 256];
    let (mut code, mut next) = (0u32, 0);
    for (length, &n) in counts.iter().enumerate() {
        for _ in 0..n {
            if code >= 1 << (length + 1) {
                bail!("JPEG: Huffman table too full");
            }
            codes[symbols[next] as usize] = (length as u8 + 1, code as u16);
            code += 1;
            next += 1;
        }
        code <<= 1;
    }
    Ok(codes)
}

/// The forward cosine transform of an 8 × 8 block of samples, results scaled by 8: the accurate
/// integer transform of libjpeg (`jfdctint.c`, Loeffler, Ligtenberg and Moschytz), one pass on the
/// rows then one on the columns, with 13 bits of constants and 2 bits kept between the passes.
fn cosine_transform(samples: &[u8; 64], out: &mut [i32; 64]) {
    const BITS: u32 = 13;
    const PASS1: u32 = 2;
    // the constants of the transform times 2^13
    const F0_298: i32 = 2446;
    const F0_390: i32 = 3196;
    const F0_541: i32 = 4433;
    const F0_765: i32 = 6270;
    const F0_899: i32 = 7373;
    const F1_175: i32 = 9633;
    const F1_501: i32 = 12_299;
    const F1_847: i32 = 15_137;
    const F1_961: i32 = 16_069;
    const F2_053: i32 = 16_819;
    const F2_562: i32 = 20_995;
    const F3_072: i32 = 25_172;

    /// The odd half, shared by both passes: from the four differences, the four odd outputs
    /// before their final shift (the rounding of that shift already added).
    #[inline(always)]
    fn odd(t0: i32, t1: i32, t2: i32, t3: i32) -> [i32; 4] {
        let (a, b) = (t0 + t2, t1 + t3);
        let z = (a + b) * F1_175 + (1 << (BITS - PASS1 - 1));
        let (a, b) = (a * -F0_390 + z, b * -F1_961 + z);
        let z03 = (t0 + t3) * -F0_899;
        let z12 = (t1 + t2) * -F2_562;
        [t0 * F1_501 + z03 + a, t1 * F3_072 + z12 + b, t2 * F2_053 + z12 + a, t3 * F0_298 + z03 + b]
    }

    // rows: results scaled by 2^PASS1 (and by sqrt(8), as the columns will be)
    for row in 0..8 {
        let s = |k: usize| samples[row * 8 + k] as i32;
        let o = &mut out[row * 8..row * 8 + 8];
        let (t0, t1, t2, t3) = (s(0) + s(7), s(1) + s(6), s(2) + s(5), s(3) + s(4));
        let (t10, t12, t11, t13) = (t0 + t3, t0 - t3, t1 + t2, t1 - t2);
        // the samples go from 0..255 to -128..127 here
        o[0] = (t10 + t11 - 8 * 128) << PASS1;
        o[4] = (t10 - t11) << PASS1;
        let z = (t12 + t13) * F0_541 + (1 << (BITS - PASS1 - 1));
        o[2] = (z + t12 * F0_765) >> (BITS - PASS1);
        o[6] = (z - t13 * F1_847) >> (BITS - PASS1);
        let [o1, o3, o5, o7] = odd(s(0) - s(7), s(1) - s(6), s(2) - s(5), s(3) - s(4));
        (o[1], o[3], o[5], o[7]) =
            (o1 >> (BITS - PASS1), o3 >> (BITS - PASS1), o5 >> (BITS - PASS1), o7 >> (BITS - PASS1));
    }
    // columns: the scaling by 2^PASS1 goes, the results stay scaled by 8
    for col in 0..8 {
        let v = |k: usize| out[k * 8 + col];
        let (t0, t1, t2, t3) = (v(0) + v(7), v(1) + v(6), v(2) + v(5), v(3) + v(4));
        let (t10, t12, t11, t13) = (t0 + t3 + (1 << (PASS1 - 1)), t0 - t3, t1 + t2, t1 - t2);
        let [o1, o3, o5, o7] = odd(v(0) - v(7), v(1) - v(6), v(2) - v(5), v(3) - v(4));
        out[col] = (t10 + t11) >> PASS1;
        out[4 * 8 + col] = (t10 - t11) >> PASS1;
        let z = (t12 + t13) * F0_541 + (1 << (BITS + PASS1 - 1));
        out[2 * 8 + col] = (z + t12 * F0_765) >> (BITS + PASS1);
        out[6 * 8 + col] = (z - t13 * F1_847) >> (BITS + PASS1);
        out[8 + col] = o1 >> (BITS + PASS1);
        out[3 * 8 + col] = o3 >> (BITS + PASS1);
        out[5 * 8 + col] = o5 >> (BITS + PASS1);
        out[7 * 8 + col] = o7 >> (BITS + PASS1);
    }
}

/// The products of the colour conversion, by component (Y, Cb, Cr), by channel (R, G, B), by
/// value: Y = (19595 R + 38469 G + 7471 B + 32767) >> 16 and so on; the rounding and the +128 of
/// the chrominances are in the blue tables.
static YCBCR: [[[i32; 256]; 3]; 3] = {
    const FACTORS: [[i32; 3]; 3] = [[19595, 38469, 7471], [-11059, -21709, 32768], [32768, -27439, -5329]];
    const ROUNDING: [i32; 3] = [32767, 128 * 65536 + 32767, 128 * 65536 + 32767];
    let mut tables = [[[0i32; 256]; 3]; 3];
    let mut c = 0;
    while c < 3 {
        let mut k = 0;
        while k < 3 {
            let mut v = 0;
            while v < 256 {
                tables[c][k][v] = FACTORS[c][k] * v as i32 + if k == 2 { ROUNDING[c] } else { 0 };
                v += 1;
            }
            k += 1;
        }
        c += 1;
    }
    tables
};

/// A quantization table: each coefficient (scaled by 8) becomes
/// `((c / 8) as f32 / divisor).round()`, as the `image` encoder computes it, here in integers.
///
/// With n = c / 8 (towards zero) and q the divisor, that is n / q rounded half away from zero:
/// sign(n) · floor((2|n| + q) / 2q). The float quotient cannot round to the other side of a half:
/// n / q is at least 1 / 2q ≥ 1 / 510 away from one (unless it is one, and then exact), and its
/// float is within 2^-24 of it in proportion, less than 0.002 as long as |n / q| < 32768
/// (a coefficient of 8-bit samples is below 2048). The floor is taken by a multiplication:
/// floor(a / d) = (a · (floor(2^32 / d) + 1)) >> 32 for a < 2^32 / d.
struct Quantizer {
    divisors: [u32; 64],
    /// floor(2^32 / 2q) + 1.
    inverses: [u32; 64],
}

impl Quantizer {
    fn new(table: &[u8; 64]) -> Quantizer {
        Quantizer {
            divisors: table.map(u32::from),
            inverses: table.map(|q| ((1u64 << 32) / (2 * q as u64) + 1) as u32),
        }
    }

    #[inline(always)]
    fn one(c: i32, divisor: u32, inverse: u32) -> i32 {
        let n = (c + ((c >> 31) & 7)) >> 3;
        let sign = n >> 31;
        let size = ((n ^ sign) - sign) as u32;
        let quotient = (((2 * size + divisor) as u64 * inverse as u64) >> 32) as i32;
        (quotient ^ sign) - sign
    }

    fn apply(&self, block: &mut [i32; 64]) {
        for (i, b) in block.iter_mut().enumerate() {
            *b = Self::one(*b, self.divisors[i], self.inverses[i]);
        }
    }
}

/// The picture's data: Huffman codes and coefficient bits, the first bit written in the high
/// end of a byte, a zero byte after each 0xFF.
struct Bits {
    out: Vec<u8>,
    /// The `count` bits not yet written, in the low end.
    pending: u64,
    count: u32,
}

impl Bits {
    #[inline(always)]
    fn put(&mut self, bits: u32, size: u32) {
        self.pending = (self.pending << size) | bits as u64;
        self.count += size;
        while self.count >= 8 {
            let byte = (self.pending >> (self.count - 8)) as u8;
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0);
            }
            self.count -= 8;
        }
    }

    /// The size in bits of a coefficient and those bits (a negative one: its value minus one).
    #[inline(always)]
    fn coefficient(c: i32) -> (u32, u32) {
        let size = 16 - (c.unsigned_abs() as u16).leading_zeros();
        let mask = (1u32 << size) - 1;
        (size, (if c < 0 { c - 1 } else { c }) as u16 as u32 & mask)
    }

    #[inline(always)]
    fn symbol(&mut self, symbol: u8, codes: &Codes, extra: u32, extra_size: u32) {
        let (size, code) = codes[symbol as usize];
        assert!(size <= 16, "JPEG: symbol not in the Huffman table");
        self.put((code as u32) << extra_size | extra, size as u32 + extra_size);
    }

    /// Writes a quantized block: its first coefficient as the difference with the previous
    /// block's, the others in zigzag order as (run of zeros, size) symbols. Returns that first
    /// coefficient.
    fn block(&mut self, block: &[i32; 64], previous: i32, dc: &Codes, ac: &Codes) -> i32 {
        let (size, bits) = Self::coefficient(block[0] - previous);
        self.symbol(size as u8, dc, bits, size);
        let mut zeros = 0u32;
        for &k in &UNZIGZAG[1..] {
            let c = block[k as usize];
            if c == 0 {
                zeros += 1;
                continue;
            }
            while zeros > 15 {
                self.symbol(0xF0, ac, 0, 0);
                zeros -= 16;
            }
            let (size, bits) = Self::coefficient(c);
            self.symbol((zeros << 4 | size) as u8, ac, bits, size);
            zeros = 0;
        }
        if block[63] == 0 {
            self.symbol(0x00, ac, 0, 0);
        }
        block[0]
    }

    /// Fills the last byte with ones and ends the picture.
    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            let fill = 8 - self.count;
            self.put((1 << fill) - 1, fill);
        }
        self.out.extend_from_slice(&[0xFF, 0xD9]);
        self.out
    }
}

/// `BTMS_CHECK_JPEG=1`: every tile is also encoded by the `image` crate, and both compared.
static CHECK: LazyLock<bool> = LazyLock::new(|| std::env::var("BTMS_CHECK_JPEG").is_ok_and(|v| v != "0"));

/// The JPEG file of an RGBA picture of `encoder`'s size.
pub fn encode(encoder: &Encoder, rgba: &[u8], quality: u8) -> Vec<u8> {
    let file = encoder.encode_rgba(rgba);
    if *CHECK {
        let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
        let expected = reference(&rgb, encoder.width, encoder.height, quality).expect("reference encoder");
        assert!(file == expected, "BTMS_CHECK_JPEG: tile encoded differently from the reference encoder");
    }
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random(seed: &mut u64) -> u64 {
        *seed ^= *seed >> 12;
        *seed ^= *seed << 25;
        *seed ^= *seed >> 27;
        seed.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Pictures of every kind: noise, flat, gradients, hard edges, smooth noise, extremes.
    fn picture(kind: usize, w: usize, h: usize, seed: &mut u64) -> Vec<u8> {
        let mut rgba = vec![0u8; w * h * 4];
        let base = [random(seed) as u8, random(seed) as u8, random(seed) as u8];
        for y in 0..h {
            for x in 0..w {
                let p = &mut rgba[(y * w + x) * 4..][..4];
                let noise = random(seed);
                let rgb: [u8; 3] = match kind % 7 {
                    0 => [noise as u8, (noise >> 8) as u8, (noise >> 16) as u8],
                    1 => base,
                    2 => [(x * 255 / w) as u8, (y * 255 / h) as u8, ((x + y) * 255 / (w + h)) as u8],
                    3 => {
                        if (x / 5 + y / 3) % 2 == 0 {
                            [255, 255, 255]
                        } else {
                            [0, 0, 0]
                        }
                    }
                    4 => [0, 1, 2].map(|k| (base[k] as i32 + (noise >> (8 * k)) as i32 % 9 - 4).clamp(0, 255) as u8),
                    5 => {
                        if noise.is_multiple_of(3) {
                            [255, 0, 255]
                        } else {
                            [0, 255, 0]
                        }
                    }
                    _ => [0, 1, 2].map(|k| {
                        ((((x * (k + 1)) as f32 * 0.07).sin() * ((y * (3 - k)) as f32 * 0.05).cos() * 120.0)
                            + 128.0
                            + (noise % 5) as f32) as u8
                    }),
                };
                p[..3].copy_from_slice(&rgb);
                p[3] = noise as u8;
            }
        }
        rgba
    }

    #[test]
    fn same_file_as_the_image_crate() {
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let mut files = 0;
        for (w, h, quality) in [
            (512, 512, 82),
            (64, 40, 82),
            (8, 8, 82),
            (128, 128, 75),
            (96, 64, 100),
            (64, 64, 1),
            (72, 48, 30),
            (48, 80, 95),
        ] {
            let encoder = Encoder::new(w, h, quality).unwrap();
            for kind in 0..(if w == 512 { 7 } else { 21 }) {
                let rgba = picture(kind, w, h, &mut seed);
                let rgb: Vec<u8> = rgba.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
                let (ours, theirs) = (encoder.encode_rgba(&rgba), reference(&rgb, w, h, quality).unwrap());
                assert!(
                    ours == theirs,
                    "{w}×{h} quality {quality} kind {kind}: {} bytes against {}",
                    ours.len(),
                    theirs.len()
                );
                files += 1;
            }
        }
        assert!(files > 100);
    }

    #[test]
    fn quantizer_is_the_float_division_rounded() {
        // every divisor a table can hold, every coefficient 16 bits can hold (8-bit samples give
        // coefficients below 16384 in size)
        for q in 1..=255u8 {
            let quantizer = Quantizer::new(&[q; 64]);
            for c in -32768..=32767i32 {
                let expected = ((c / 8) as f32 / f32::from(q)).round() as i32;
                assert_eq!(Quantizer::one(c, quantizer.divisors[0], quantizer.inverses[0]), expected, "{c} / {q}");
            }
        }
    }

    #[test]
    fn colour_tables_are_the_products() {
        for v in 0..256usize {
            let x = v as i32;
            assert_eq!(YCBCR[0][0][v] + YCBCR[0][1][v] + YCBCR[0][2][v], 19595 * x + 38469 * x + 7471 * x + 32767);
            assert_eq!(
                YCBCR[1][0][v] + YCBCR[1][1][0] + YCBCR[1][2][255 - v],
                -11059 * x + 32768 * (255 - x) + 8_421_375
            );
            assert_eq!(
                YCBCR[2][0][255 - v] + YCBCR[2][1][v] + YCBCR[2][2][v],
                32768 * (255 - x) - 27439 * x - 5329 * x + 8_421_375
            );
        }
    }

    #[test]
    fn sizes_that_are_not_blocks_are_refused() {
        assert!(Encoder::new(510, 512, 82).is_err());
    }

    /// Not a check: the time of both encoders on a tile of smooth noise.
    /// `cargo test --release -- --ignored --nocapture jpeg_speed`
    #[test]
    #[ignore]
    fn jpeg_speed() {
        let mut seed = 77u64;
        let encoder = Encoder::new(512, 512, 82).unwrap();
        let tiles: Vec<Vec<u8>> = (0..7).map(|k| picture(k, 512, 512, &mut seed)).collect();
        let rgbs: Vec<Vec<u8>> =
            tiles.iter().map(|t| t.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect()).collect();
        let best = |f: &dyn Fn() -> usize| {
            (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    std::hint::black_box(f());
                    t.elapsed().as_secs_f64()
                })
                .fold(f64::MAX, f64::min)
        };
        for (k, (tile, rgb)) in tiles.iter().zip(&rgbs).enumerate() {
            let ours = best(&|| encoder.encode_rgba(tile).len());
            let theirs = best(&|| reference(rgb, 512, 512, 82).unwrap().len());
            println!(
                "picture {k} ({} KB): image crate {:.2} ms, this module {:.2} ms ({:.1} times faster)",
                encoder.encode_rgba(tile).len() / 1024,
                theirs * 1e3,
                ours * 1e3,
                theirs / ours
            );
        }
    }
}
