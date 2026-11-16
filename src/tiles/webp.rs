//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The WebP of a tile, with libwebp itself (`libwebp-sys`: its C sources built and linked
//! statically).
//!
//! - **Lossy** ([`Quality::Lossy`]): VP8, the colour at half resolution as in every lossy WebP
//!   (4:2:0). A partly empty tile keeps its transparency exactly: the alpha plane is compressed
//!   without loss (`alpha_quality` 100); only the colour under fully transparent pixels may
//!   change, which nobody sees.
//! - **Lossless** ([`Quality::Lossless`]): VP8L, every pixel decoded bit for bit as it was given,
//!   the colour under transparent pixels included (`exact`).
//!
//! How hard libwebp works for that quality is the [`Effort`]: [`Effort::Fast`] for a render (the
//! tiles must keep up with the drawing), [`Effort::Careful`] for `convert` (a publication,
//! nothing is waiting): lossy method 4 and the "sharp" RGB → YUV conversion (iterative: thin
//! coloured lines such as road markings keep their colour).
//!
//! An opaque tile is given without its alpha (`RGBX`): the file has no alpha plane at all.
//! The encoder runs on the calling thread (the tiles' writing threads are many already).

use std::ffi::{c_int, c_void};
use std::mem::MaybeUninit;
use std::sync::Once;

use anyhow::{Result, bail, ensure};
use libwebp_sys as sys;

/// What the encoder keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    /// Every pixel as given (VP8L).
    Lossless,
    /// With loss, at this quality (0 to 100, libwebp's scale: VP8).
    Lossy(u8),
}

/// How hard the encoder works for a quality: the files differ a little in size and fidelity, a
/// lot in time (measured on one thread).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Effort {
    /// `fast`, the default of `tiles`: lossy method 0 without the "sharp" conversion (9 ms a
    /// full tile instead of 36, 2.6 % smaller, 0.14 dB of PSNR less), lossless level 0.
    #[default]
    Fast,
    /// `careful`, the default of `convert`: lossy method 4 with the "sharp" conversion,
    /// lossless level 6.
    Careful,
}

impl Effort {
    /// The word of `--webp-effort` and of the manifest's `effort`.
    pub fn word(self) -> &'static str {
        match self {
            Effort::Fast => "fast",
            Effort::Careful => "careful",
        }
    }

    /// `--webp-effort fast|careful`.
    pub fn parse(text: &str) -> Result<Effort> {
        match text.trim() {
            "fast" => Ok(Effort::Fast),
            "careful" => Ok(Effort::Careful),
            _ => bail!(
                "--webp-effort {text}: expected fast (the default of a render: method 0, without sharp YUV) or careful (the default of convert: method 4, sharp YUV conversion)"
            ),
        }
    }
}

/// How the encoder works. [`Settings::tiles`] is what the tiles are written with; the other
/// settings are there to be measured against it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub quality: Quality,
    /// Lossy: libwebp's `method`, 0 (fastest) to 6 (slowest, smallest). Lossless: the level of
    /// `WebPConfigLosslessPreset`, 0 to 9 (9: method 6 at quality 100, the smallest).
    pub effort: u8,
    /// Lossless only: libwebp's near-lossless preprocessing, 100 (off) down to 0 (the most
    /// change): the colours of busy areas are rounded before the lossless coding.
    pub near_lossless: u8,
    /// Lossy only: the "sharp" RGB → YUV conversion (slower; colour edges kept).
    pub sharp_yuv: bool,
}

/// Lossy method of the tiles of a render ([`Effort::Fast`]). Measured on one thread on the 147
/// full tiles of 7 blocks drawn in memory, at quality 90: 9.3 ms a tile without the "sharp"
/// conversion, the only setting under 10 ms (method 1: 11.6, method 2: 12.1, method 4: 23.2;
/// "sharp" adds 13 ms to any of them); 90.9 KB and 38.42 dB against 93.3 KB and 38.56 dB for
/// method 4 with "sharp". On a partly empty tile its alpha plane, coded with the same effort, is
/// larger (21.9 KB instead of 14.4): such tiles are rare (the edges of the map).
pub const FAST_LOSSY_METHOD: u8 = 0;
/// Lossless level of a render (`--quality lossless`): 6.9 ms a tile, 9 % larger than level 6
/// (92 ms).
pub const FAST_LOSSLESS_LEVEL: u8 = 0;
/// Lossy method of [`Effort::Careful`] (with the "sharp" conversion): 36.4 ms a tile on one
/// thread; method 6 makes files 0.6 % smaller for twice the time, and 19 times the time on a
/// partly empty tile (its alpha plane: 452 ms instead of 24); method 3 is method 4.
pub const LOSSY_METHOD: u8 = 4;
/// Lossless level of [`Effort::Careful`]: level 9 is 3 % smaller for 30 times the time.
pub const LOSSLESS_LEVEL: u8 = 6;

impl Settings {
    /// What the tiles are written with, at `quality`, with `effort`.
    pub fn tiles(quality: Quality, effort: Effort) -> Settings {
        match (quality, effort) {
            (Quality::Lossless, Effort::Fast) => {
                Settings { quality, effort: FAST_LOSSLESS_LEVEL, near_lossless: 100, sharp_yuv: false }
            }
            (Quality::Lossless, Effort::Careful) => {
                Settings { quality, effort: LOSSLESS_LEVEL, near_lossless: 100, sharp_yuv: false }
            }
            (Quality::Lossy(_), Effort::Fast) => {
                Settings { quality, effort: FAST_LOSSY_METHOD, near_lossless: 100, sharp_yuv: false }
            }
            (Quality::Lossy(_), Effort::Careful) => {
                Settings { quality, effort: LOSSY_METHOD, near_lossless: 100, sharp_yuv: true }
            }
        }
    }
}

/// The version of libwebp built in, as `1.6.0`.
pub fn version() -> String {
    // SAFETY: a function without argument that returns a number.
    let v = unsafe { sys::WebPGetEncoderVersion() };
    format!("{}.{}.{}", (v >> 16) & 0xff, (v >> 8) & 0xff, v & 0xff)
}

/// libwebp fills its tables of functions (by processor) the first time it encodes or decodes; on
/// Windows it does so without a lock. Done once here, on one thread, before any other use (with
/// the `_now` functions only: calling [`encode`] or [`decode`] in here would wait for itself).
fn warm() {
    static WARM: Once = Once::new();
    WARM.call_once(|| {
        let mut rgba = vec![0u8; 16 * 16 * 4];
        for (i, p) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            p.copy_from_slice(&[(i * 7) as u8, (i * 13) as u8, (i * 29) as u8, if i % 5 == 0 { 0 } else { 255 }]);
        }
        let careful = |q| Settings::tiles(q, Effort::Careful);
        for settings in [
            careful(Quality::Lossy(80)),
            Settings::tiles(Quality::Lossy(80), Effort::Fast),
            careful(Quality::Lossless),
            Settings { near_lossless: 60, ..careful(Quality::Lossless) },
        ] {
            for opaque in [true, false] {
                if let Ok(bytes) = encode_now(&rgba, 16, 16, opaque, &settings) {
                    let _ = decode_now(&bytes);
                }
            }
        }
    });
}

/// Appends what libwebp writes to the `Vec<u8>` the picture's `custom_ptr` points to.
unsafe extern "C" fn write_to_vec(data: *const u8, size: usize, picture: *const sys::WebPPicture) -> c_int {
    // SAFETY: `custom_ptr` is the `Vec<u8>` `encode_now` set, alive and not otherwise borrowed while
    // `WebPEncode` runs; `data` holds `size` bytes (libwebp's contract for its writer).
    unsafe {
        let out = &mut *((*picture).custom_ptr as *mut Vec<u8>);
        if size > 0 {
            out.extend_from_slice(std::slice::from_raw_parts(data, size));
        }
    }
    1
}

/// The WebP of a `width` × `height` RGBA picture. `opaque`: its alpha is not looked at, and the
/// file has none (a tile whose pixels are all opaque).
pub fn encode(rgba: &[u8], width: usize, height: usize, opaque: bool, settings: &Settings) -> Result<Vec<u8>> {
    warm();
    encode_now(rgba, width, height, opaque, settings)
}

fn encode_now(rgba: &[u8], width: usize, height: usize, opaque: bool, settings: &Settings) -> Result<Vec<u8>> {
    let max = sys::WEBP_MAX_DIMENSION as usize;
    ensure!(width > 0 && height > 0 && width <= max && height <= max, "WebP: {width} × {height} px cannot be encoded");
    ensure!(rgba.len() == width * height * 4, "WebP: {} bytes for a picture of {width} × {height} px", rgba.len());
    // SAFETY: `config` and `picture` are initialised by libwebp before any field is read or set
    // (`WebPConfigInitInternal`, `WebPPictureInitInternal`, with the ABI version the bindings are
    // made for). The import reads `height` rows of `width × 4` bytes from `rgba`, checked above,
    // and copies them into the picture's own buffer, which `WebPPictureFree` gives back on every
    // path. The writer only appends to `out`, which outlives `WebPEncode`.
    unsafe {
        let mut config = MaybeUninit::<sys::WebPConfig>::uninit();
        let quality = match settings.quality {
            Quality::Lossless => 75.0,
            Quality::Lossy(q) => q.min(100) as f32,
        };
        if sys::WebPConfigInitInternal(
            config.as_mut_ptr(),
            sys::WebPPreset::WEBP_PRESET_DEFAULT,
            quality,
            sys::WEBP_ENCODER_ABI_VERSION as c_int,
        ) == 0
        {
            bail!("WebP: unexpected libwebp version");
        }
        let mut config = config.assume_init();
        match settings.quality {
            Quality::Lossless => {
                if sys::WebPConfigLosslessPreset(&mut config, settings.effort.min(9) as c_int) == 0 {
                    bail!("WebP: lossless level {} refused", settings.effort);
                }
                config.lossless = 1;
                config.exact = 1;
                config.near_lossless = settings.near_lossless.min(100) as c_int;
            }
            Quality::Lossy(_) => {
                config.method = settings.effort.min(6) as c_int;
                config.use_sharp_yuv = settings.sharp_yuv as c_int;
                // the transparency kept exactly: the alpha plane without loss
                config.alpha_compression = 1;
                config.alpha_quality = 100;
                config.alpha_filtering = 1;
            }
        }
        if sys::WebPValidateConfig(&config) == 0 {
            bail!("WebP: settings refused by libwebp ({settings:?})");
        }
        let mut picture = MaybeUninit::<sys::WebPPicture>::uninit();
        if sys::WebPPictureInitInternal(picture.as_mut_ptr(), sys::WEBP_ENCODER_ABI_VERSION as c_int) == 0 {
            bail!("WebP: unexpected libwebp version");
        }
        let mut picture = picture.assume_init();
        // ARGB kept as given: the lossless coder needs it, and the lossy one then makes its YUV
        // itself, the sharp way when asked
        picture.use_argb = 1;
        picture.width = width as c_int;
        picture.height = height as c_int;
        let stride = (width * 4) as c_int;
        let imported = if opaque {
            sys::WebPPictureImportRGBX(&mut picture, rgba.as_ptr(), stride)
        } else {
            sys::WebPPictureImportRGBA(&mut picture, rgba.as_ptr(), stride)
        };
        if imported == 0 {
            sys::WebPPictureFree(&mut picture);
            bail!("WebP: not enough memory for the picture ({width} × {height} px)");
        }
        let mut out: Vec<u8> = Vec::with_capacity(width * height / 4);
        picture.writer = Some(write_to_vec);
        picture.custom_ptr = &mut out as *mut Vec<u8> as *mut c_void;
        let done = sys::WebPEncode(&config, &mut picture);
        let error = picture.error_code;
        sys::WebPPictureFree(&mut picture);
        ensure!(done != 0, "WebP encoding: {error:?}");
        Ok(out)
    }
}

/// A WebP file decoded: (width, height, RGBA). Without alpha in the file, every pixel is opaque.
pub fn decode(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>)> {
    warm();
    decode_now(bytes)
}

fn decode_now(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>)> {
    let (mut w, mut h): (c_int, c_int) = (0, 0);
    // SAFETY: libwebp reads `bytes` within its length; the output buffer holds `h` rows of `w × 4`
    // bytes, which is what the stride and the size given to `WebPDecodeRGBAInto` say; the size of
    // the picture is checked against WebP's own bound before the buffer is made.
    unsafe {
        if sys::WebPGetInfo(bytes.as_ptr(), bytes.len(), &mut w, &mut h) == 0 {
            bail!("unreadable WebP");
        }
        let max = sys::WEBP_MAX_DIMENSION as c_int;
        ensure!(w > 0 && h > 0 && w <= max && h <= max, "WebP: size {w} × {h} refused");
        let (w, h) = (w as usize, h as usize);
        let mut rgba = vec![0u8; w * h * 4];
        if sys::WebPDecodeRGBAInto(bytes.as_ptr(), bytes.len(), rgba.as_mut_ptr(), rgba.len(), (w * 4) as c_int)
            .is_null()
        {
            bail!("unreadable WebP (data)");
        }
        Ok((w, h, rgba))
    }
}

/// Whether a WebP file has an alpha plane (None: not a WebP), read from its header only.
pub fn has_alpha(bytes: &[u8]) -> Option<bool> {
    let mut features = MaybeUninit::<sys::WebPBitstreamFeatures>::zeroed();
    // SAFETY: libwebp reads the header of `bytes` within its length and fills `features`, which
    // is read only when it says it did.
    unsafe {
        if sys::WebPGetFeatures(bytes.as_ptr(), bytes.len(), features.as_mut_ptr()) != sys::VP8StatusCode::VP8_STATUS_OK
        {
            return None;
        }
        Some(features.assume_init().has_alpha != 0)
    }
}

/// Whether the bytes are a WebP file: `RIFF....WEBP`.
pub fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP"
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn random(seed: &mut u64) -> u64 {
        *seed ^= *seed >> 12;
        *seed ^= *seed << 25;
        *seed ^= *seed >> 27;
        seed.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A picture like drawn ground: soft gradients, some noise, a few sharp lines (roads), and
    /// with `holes` a third of it empty (the sea beyond the map), soft edges between. Its noise
    /// (white, up to 22 a channel) is harder on a lossy coder than the game's textures.
    pub fn ground(w: usize, h: usize, holes: bool, seed: &mut u64) -> Vec<u8> {
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let r = random(seed);
                let road = (x + 2 * y) % 97 < 6 || (y % 131) < 4;
                let base = [90 + (x * 60 / w) as u32, 110 + (y * 50 / h) as u32, 70 + ((x + y) * 30 / (w + h)) as u32];
                let p = &mut rgba[(y * w + x) * 4..][..4];
                for k in 0..3 {
                    p[k] =
                        if road { [200, 200, 190][k] } else { (base[k] + (r >> (8 * k)) as u32 % 23).min(255) as u8 };
                }
                p[3] = if !holes || x > w / 3 {
                    255
                } else if x + 4 > w / 3 {
                    (60 * (x + 4 - w / 3)) as u8
                } else {
                    0
                };
                if p[3] == 0 {
                    p[..3].fill(0);
                }
            }
        }
        rgba
    }

    /// The peak signal to noise ratio of `b` against `a` (RGB, opaque pixels of `a`), in dB.
    pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
        let (mut sum, mut n) = (0f64, 0f64);
        for (p, q) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
            if p[3] == 255 {
                for k in 0..3 {
                    let d = p[k] as f64 - q[k] as f64;
                    sum += d * d;
                }
                n += 3.0;
            }
        }
        if sum == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / (sum / n)).log10() }
    }

    #[test]
    fn lossless_gives_back_every_bit() {
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        for (w, h, holes) in [(512, 512, false), (512, 512, true), (37, 19, true), (1, 1, false)] {
            let rgba = ground(w, h, holes, &mut seed);
            let opaque = rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
            for level in [FAST_LOSSLESS_LEVEL, 1, LOSSLESS_LEVEL] {
                let bytes = encode(
                    &rgba,
                    w,
                    h,
                    opaque,
                    &Settings { effort: level, ..Settings::tiles(Quality::Lossless, Effort::Careful) },
                )
                .unwrap();
                assert!(is_webp(&bytes));
                assert_eq!(has_alpha(&bytes), Some(!opaque), "{w}×{h}");
                let (dw, dh, back) = decode(&bytes).unwrap();
                assert_eq!((dw, dh), (w, h));
                assert!(back == rgba, "{w}×{h} level {level}: the decoded picture differs");
            }
        }
        // random bytes, the colour under transparent pixels included: still every bit
        let noise: Vec<u8> = (0..64 * 64 * 4).map(|_| random(&mut seed) as u8).collect();
        for effort in [Effort::Fast, Effort::Careful] {
            let bytes = encode(&noise, 64, 64, false, &Settings::tiles(Quality::Lossless, effort)).unwrap();
            assert!(decode(&bytes).unwrap().2 == noise, "{effort:?}");
        }
    }

    #[test]
    fn lossy_keeps_the_transparency_exactly() {
        let mut seed = 99u64;
        let rgba = ground(512, 512, true, &mut seed);
        // (the alpha plane is coded with the lossy method's effort: method 0 as well)
        for (q, effort) in [(50, Effort::Careful), (90, Effort::Careful), (50, Effort::Fast), (90, Effort::Fast)] {
            let bytes = encode(&rgba, 512, 512, false, &Settings::tiles(Quality::Lossy(q), effort)).unwrap();
            assert_eq!(has_alpha(&bytes), Some(true));
            let (_, _, back) = decode(&bytes).unwrap();
            let alpha = |v: &[u8]| v.as_chunks::<4>().0.iter().map(|p| p[3]).collect::<Vec<u8>>();
            assert!(alpha(&back) == alpha(&rgba), "quality {q} {effort:?}: the alpha plane changed");
        }
        // an opaque tile has no alpha in its file, and decodes opaque whatever its alpha said
        let mut opaque = ground(512, 512, false, &mut seed);
        opaque[3] = 7;
        for effort in [Effort::Fast, Effort::Careful] {
            let bytes = encode(&opaque, 512, 512, true, &Settings::tiles(Quality::Lossy(90), effort)).unwrap();
            assert_eq!(has_alpha(&bytes), Some(false));
            assert!(decode(&bytes).unwrap().2.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        }
    }

    #[test]
    fn lossy_stays_within_the_measured_bounds() {
        // on the tiles of blocks drawn in memory the measures give 38.5 dB on average at quality
        // 90, 29.5 dB on the worst tile (a Cayo Perico jungle), against 35.1 and 29.0 for the older
        // JPEG at 82. On this synthetic picture, white noise that no lossy coder keeps, the bounds
        // are those measured on it: at least 29 dB from quality 60, and a higher quality must cost
        // more bytes for fewer errors
        let mut seed = 7u64;
        let rgba = ground(512, 512, false, &mut seed);
        for effort in [Effort::Fast, Effort::Careful] {
            let mut last: Option<(usize, f64)> = None;
            for q in [60, 75, 90, 98] {
                let bytes = encode(&rgba, 512, 512, true, &Settings::tiles(Quality::Lossy(q), effort)).unwrap();
                let p = psnr(&rgba, &decode(&bytes).unwrap().2);
                assert!(p > 29.0, "quality {q} {effort:?}: {p:.1} dB");
                if let Some((size, before)) = last {
                    assert!(
                        bytes.len() > size && p > before,
                        "quality {q} {effort:?}: {} bytes, {p:.2} dB after {size} bytes, {before:.2} dB",
                        bytes.len()
                    );
                }
                last = Some((bytes.len(), p));
            }
        }
        // near-lossless: close, never further than its rounding allows
        let bytes = encode(
            &rgba,
            512,
            512,
            true,
            &Settings { near_lossless: 60, ..Settings::tiles(Quality::Lossless, Effort::Careful) },
        )
        .unwrap();
        let back = decode(&bytes).unwrap().2;
        let worst = rgba.iter().zip(&back).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(worst <= 8, "near-lossless 60: an error of {worst}");
    }

    #[test]
    fn the_efforts_are_those_measured() {
        // a render: method 0 without "sharp", lossless level 0; convert: method 4 with it, level 6
        let fast = Settings::tiles(Quality::Lossy(90), Effort::Fast);
        let careful = Settings::tiles(Quality::Lossy(90), Effort::Careful);
        assert_eq!(
            (fast.effort, fast.sharp_yuv, careful.effort, careful.sharp_yuv),
            (FAST_LOSSY_METHOD, false, LOSSY_METHOD, true)
        );
        assert_eq!(
            (
                Settings::tiles(Quality::Lossless, Effort::Fast).effort,
                Settings::tiles(Quality::Lossless, Effort::Careful).effort
            ),
            (FAST_LOSSLESS_LEVEL, LOSSLESS_LEVEL)
        );
        assert_eq!(Effort::default(), Effort::Fast);
        assert_eq!(
            (Effort::parse("fast").unwrap(), Effort::parse(" careful").unwrap()),
            (Effort::Fast, Effort::Careful)
        );
        for e in [Effort::Fast, Effort::Careful] {
            assert_eq!(Effort::parse(e.word()).unwrap(), e);
        }
        assert!(format!("{:#}", Effort::parse("turbo").unwrap_err()).starts_with("--webp-effort turbo: expected fast"));
        // the same picture: the fast file is a valid one, as faithful within 1 dB (on the drawn
        // tiles: 0.14 dB less, 2.6 % smaller; on this white noise, which "sharp" spends bytes on,
        // 17 % smaller)
        let mut seed = 3u64;
        let rgba = ground(512, 512, false, &mut seed);
        let (a, b) = (encode(&rgba, 512, 512, true, &fast).unwrap(), encode(&rgba, 512, 512, true, &careful).unwrap());
        let (pa, pb) = (psnr(&rgba, &decode(&a).unwrap().2), psnr(&rgba, &decode(&b).unwrap().2));
        assert!((pa - pb).abs() < 1.0, "{pa:.2} dB against {pb:.2}");
        assert!((a.len() as f64 / b.len() as f64 - 1.0).abs() < 0.3, "{} bytes against {}", a.len(), b.len());
    }

    #[test]
    fn what_is_not_a_picture_is_refused() {
        assert!(decode(b"RIFF\0\0\0\0WEBPVP8 nothing").is_err());
        assert!(decode(&[]).is_err());
        assert_eq!(has_alpha(b"\x89PNG\r\n\x1a\n"), None);
        assert!(encode(&[0; 12], 2, 2, true, &Settings::tiles(Quality::Lossy(80), Effort::Fast)).is_err());
        assert!(encode(&[], 0, 0, true, &Settings::tiles(Quality::Lossy(80), Effort::Careful)).is_err());
        assert_eq!(version().split('.').count(), 3);
    }
}
