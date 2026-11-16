//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The CPU half of the drawing: a [`Batch`] of the list ([`crate::render::scene`]) to pixels.
//!
//! [`Raster::draw`] gives, bit for bit, the picture that `Raster::triangle` and
//! `Raster::triangle_layers` give when called for every triangle of the batch in order (that is
//! [`Raster::draw_reference`], and what `render::assets::paint` used to do). It only spends less
//! time on it:
//!
//! - a vertex is projected once, not once per triangle that uses it;
//! - a triangle wholly outside the picture is dropped before anything else is looked at;
//! - pixels are tested four at a time, and those outside the triangle are told so without a
//!   division; the divisions of those inside are done four pixels at once;
//! - a texture sample costs no call to the C library (`floor`), no integer division (the wrap)
//!   and no float division (a byte to 0..1 is read in a table of the same 256 quotients); its four
//!   channels are computed together.
//!
//! "Four at once" is [`F4`]: four `f32` on which each operation is the plain IEEE one on every
//! lane, so that nothing is rounded differently. Every value that reaches the picture is computed
//! by the same operations in the same order as in `raster.rs`: change one side, change the other.
//! The tests at the end of this file compare both on random triangles and fail on the first
//! differing bit. `BTMS_REFERENCE_DRAWING=1` makes `draw` use the reference path, to compare whole
//! renders.
//!
//! The pass of the raster (`Raster::set_pass`, the margin of the shading: a filter of triangles,
//! heights only) is applied by `triangle_layers` before anything else; `draw` does not go through
//! it, so it applies the same two rules itself: a triangle the filter refuses is not drawn, and
//! with heights only a blended batch is not drawn and an opaque one is drawn without its textures.

use std::cell::RefCell;
use std::sync::LazyLock;

use crate::render::raster::{Image, Mode, Projection, Raster, Vertex};
use crate::render::scene::{Batch, Material};
use crate::util::profile::{self, Count};

use lanes::F4;

/// Pixels kept around the picture by [`Raster::clip`].
const CLIP_MARGIN: f32 = 8.0;

/// `byte as f32 / 255.0` for every byte: the very quotients `Image::sample` computes.
const UNIT: [f32; 256] = {
    let mut table = [0.0f32; 256];
    let mut i = 0;
    while i < 256 {
        table[i] = i as f32 / 255.0;
        i += 1;
    }
    table
};

/// Below this, a float goes to an integer and back exactly, and half-pixel coordinates are exact.
const EXACT: f32 = 4_194_304.0;

/// `BTMS_REFERENCE_DRAWING=1`: [`Raster::draw`] goes through `triangle` / `triangle_layers`.
static REFERENCE: LazyLock<bool> = LazyLock::new(|| std::env::var("BTMS_REFERENCE_DRAWING").is_ok_and(|v| v != "0"));

thread_local! {
    /// The projected vertices of the batch being drawn (kept from one batch to the next).
    static PROJECTED: RefCell<Vec<[f32; 3]>> = const { RefCell::new(Vec::new()) };
}

/// Four `f32` at once. Every operation is the IEEE operation on each lane, exactly as on single
/// `f32`s: SSE2 on x86-64 (part of the architecture: nothing to detect), plain arrays elsewhere.
#[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
mod lanes {
    use std::arch::x86_64::*;

    #[derive(Clone, Copy)]
    pub struct F4(__m128);

    // SAFETY, for every `unsafe` block of this module: the intrinsics called only require SSE2,
    // and this module is only compiled when the target has it (`cfg(target_feature = "sse2")`
    // above: every x86-64 processor does). None of them reads or writes memory through a pointer.
    impl F4 {
        #[inline(always)]
        pub fn new(a: f32, b: f32, c: f32, d: f32) -> F4 {
            F4(unsafe { _mm_setr_ps(a, b, c, d) })
        }

        #[inline(always)]
        pub fn load(v: [f32; 4]) -> F4 {
            F4(unsafe { _mm_setr_ps(v[0], v[1], v[2], v[3]) })
        }

        #[inline(always)]
        pub fn splat(x: f32) -> F4 {
            F4(unsafe { _mm_set1_ps(x) })
        }

        #[inline(always)]
        pub fn add(self, o: F4) -> F4 {
            F4(unsafe { _mm_add_ps(self.0, o.0) })
        }

        #[inline(always)]
        pub fn sub(self, o: F4) -> F4 {
            F4(unsafe { _mm_sub_ps(self.0, o.0) })
        }

        #[inline(always)]
        pub fn mul(self, o: F4) -> F4 {
            F4(unsafe { _mm_mul_ps(self.0, o.0) })
        }

        #[inline(always)]
        pub fn div(self, o: F4) -> F4 {
            F4(unsafe { _mm_div_ps(self.0, o.0) })
        }

        /// Flips the sign of the lanes where `sign` is -0.0 (and leaves those where it is 0.0).
        #[inline(always)]
        pub fn flip(self, sign: F4) -> F4 {
            F4(unsafe { _mm_xor_ps(self.0, sign.0) })
        }

        /// Bit `k` set where lane `k` of `self` is less than that of `o` (never for a NaN).
        #[inline(always)]
        pub fn lt(self, o: F4) -> u32 {
            unsafe { _mm_movemask_ps(_mm_cmplt_ps(self.0, o.0)) as u32 }
        }

        /// Bit `k` set where lane `k` of `self` is greater than that of `o` (never for a NaN).
        #[inline(always)]
        pub fn gt(self, o: F4) -> u32 {
            unsafe { _mm_movemask_ps(_mm_cmpgt_ps(self.0, o.0)) as u32 }
        }

        /// Each lane rounded down, for lanes below 2^31 in size; a zero result is +0.0.
        #[inline(always)]
        pub fn floor_small(self) -> F4 {
            unsafe {
                let t = _mm_cvtepi32_ps(_mm_cvttps_epi32(self.0));
                F4(_mm_sub_ps(t, _mm_and_ps(_mm_cmpgt_ps(t, self.0), _mm_set1_ps(1.0))))
            }
        }

        /// Lanes 0 and 1 as integers (they hold integers below 2^31 in size).
        #[inline(always)]
        pub fn ints(self) -> (i32, i32) {
            unsafe {
                let i = _mm_cvttps_epi32(self.0);
                (_mm_cvtsi128_si32(i), _mm_cvtsi128_si32(_mm_shuffle_epi32::<0b01_01_01_01>(i)))
            }
        }

        /// Lanes 2 and 3 moved to lanes 0 and 1.
        #[inline(always)]
        pub fn high(self) -> F4 {
            F4(unsafe { _mm_movehl_ps(self.0, self.0) })
        }

        /// Lane `K` (0 or 1) on all lanes.
        #[inline(always)]
        pub fn spread<const K: usize>(self) -> F4 {
            F4(unsafe {
                if K == 0 {
                    _mm_shuffle_ps::<0b00_00_00_00>(self.0, self.0)
                } else {
                    _mm_shuffle_ps::<0b01_01_01_01>(self.0, self.0)
                }
            })
        }

        /// Bit `k` set where lane `k` is below `limit` in size (never for a NaN).
        #[inline(always)]
        pub fn below(self, limit: f32) -> u32 {
            unsafe {
                let size = _mm_andnot_ps(_mm_set1_ps(-0.0), self.0);
                _mm_movemask_ps(_mm_cmplt_ps(size, _mm_set1_ps(limit))) as u32
            }
        }

        /// Lane 3 replaced by 1.0.
        #[inline(always)]
        pub fn opaque(self) -> F4 {
            unsafe {
                let rgb = _mm_castsi128_ps(_mm_setr_epi32(-1, -1, -1, 0));
                F4(_mm_or_ps(_mm_and_ps(self.0, rgb), _mm_setr_ps(0.0, 0.0, 0.0, 1.0)))
            }
        }

        #[inline(always)]
        pub fn lanes(self) -> [f32; 4] {
            unsafe {
                [
                    _mm_cvtss_f32(self.0),
                    _mm_cvtss_f32(_mm_shuffle_ps::<0b01_01_01_01>(self.0, self.0)),
                    _mm_cvtss_f32(_mm_movehl_ps(self.0, self.0)),
                    _mm_cvtss_f32(_mm_shuffle_ps::<0b11_11_11_11>(self.0, self.0)),
                ]
            }
        }
    }
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "sse2")))]
mod lanes {
    #[derive(Clone, Copy)]
    pub struct F4([f32; 4]);

    impl F4 {
        #[inline(always)]
        pub fn new(a: f32, b: f32, c: f32, d: f32) -> F4 {
            F4([a, b, c, d])
        }

        #[inline(always)]
        pub fn load(v: [f32; 4]) -> F4 {
            F4(v)
        }

        #[inline(always)]
        pub fn splat(x: f32) -> F4 {
            F4([x; 4])
        }

        #[inline(always)]
        fn zip(self, o: F4, f: impl Fn(f32, f32) -> f32) -> F4 {
            F4([f(self.0[0], o.0[0]), f(self.0[1], o.0[1]), f(self.0[2], o.0[2]), f(self.0[3], o.0[3])])
        }

        #[inline(always)]
        pub fn add(self, o: F4) -> F4 {
            self.zip(o, |a, b| a + b)
        }

        #[inline(always)]
        pub fn sub(self, o: F4) -> F4 {
            self.zip(o, |a, b| a - b)
        }

        #[inline(always)]
        pub fn mul(self, o: F4) -> F4 {
            self.zip(o, |a, b| a * b)
        }

        #[inline(always)]
        pub fn div(self, o: F4) -> F4 {
            self.zip(o, |a, b| a / b)
        }

        #[inline(always)]
        pub fn flip(self, sign: F4) -> F4 {
            self.zip(sign, |a, b| f32::from_bits(a.to_bits() ^ b.to_bits()))
        }

        #[inline(always)]
        pub fn lt(self, o: F4) -> u32 {
            (0..4).map(|k| ((self.0[k] < o.0[k]) as u32) << k).sum()
        }

        #[inline(always)]
        pub fn gt(self, o: F4) -> u32 {
            (0..4).map(|k| ((self.0[k] > o.0[k]) as u32) << k).sum()
        }

        #[inline(always)]
        pub fn floor_small(self) -> F4 {
            F4(self.0.map(|x| {
                let t = x as i32 as f32;
                if t > x { t - 1.0 } else { t }
            }))
        }

        #[inline(always)]
        pub fn ints(self) -> (i32, i32) {
            (self.0[0] as i32, self.0[1] as i32)
        }

        #[inline(always)]
        pub fn high(self) -> F4 {
            F4([self.0[2], self.0[3], self.0[2], self.0[3]])
        }

        #[inline(always)]
        pub fn spread<const K: usize>(self) -> F4 {
            F4([self.0[K]; 4])
        }

        #[inline(always)]
        pub fn below(self, limit: f32) -> u32 {
            (0..4).map(|k| ((self.0[k].abs() < limit) as u32) << k).sum()
        }

        #[inline(always)]
        pub fn opaque(self) -> F4 {
            F4([self.0[0], self.0[1], self.0[2], 1.0])
        }

        #[inline(always)]
        pub fn lanes(self) -> [f32; 4] {
            self.0
        }
    }
}

/// `x.floor()`, bit for bit, without the call to the C library it compiles to (no SSE4.1 by
/// default): below 2^23 a float goes to an integer and back exactly.
#[inline(always)]
fn floor(x: f32) -> f32 {
    if x.abs() < 8_388_608.0 {
        let t = x as i32 as f32;
        let r = if t > x { t - 1.0 } else { t };
        // zero keeps the sign of `x`, as `floor` does (floor(-0.0) is -0.0)
        if r == 0.0 { r.copysign(x) } else { r }
    } else {
        x.floor()
    }
}

/// `x.ceil()`, bit for bit.
#[inline(always)]
fn ceil(x: f32) -> f32 {
    -floor(-x)
}

/// `(a as i64).rem_euclid(n as i64) as usize`, without the division for the values a texture
/// coordinate gives (one period around 0..n).
#[inline(always)]
fn wrap(a: f32, n: usize) -> usize {
    let (i, n) = (a as i64, n as i64);
    let w = if i >= 0 && i < n {
        i
    } else if i >= -n && i < 0 {
        i + n
    } else if i >= n && i < 2 * n {
        i - n
    } else {
        i.rem_euclid(n)
    };
    w as usize
}

/// A texture ready to be sampled: what a sample needs of it, worked out once per batch.
#[derive(Clone, Copy)]
struct Source<'a> {
    image: &'a Image,
    /// (width, height, 0, 0).
    size: F4,
    /// Both sides below [`EXACT`] (and not zero): the usual case, where conversions are exact.
    small: bool,
}

impl<'a> Source<'a> {
    fn new(image: &'a Image) -> Source<'a> {
        let side = |n: usize| n > 0 && n < EXACT as usize;
        Source {
            image,
            size: F4::new(image.width as f32, image.height as f32, 0.0, 0.0),
            small: side(image.width) && side(image.height),
        }
    }
}

/// Where a sample falls in a texture, as `Image::sample` finds it: the offsets in `rgba` of the
/// four texels around it (a, b on one row, c, d on the next), and its weights fx, fy, 1 - fx,
/// 1 - fy (each on all lanes). It only depends on the coordinates and on the texture's size.
#[derive(Clone, Copy)]
struct Foot {
    at: [usize; 4],
    fx: F4,
    fy: F4,
    gx: F4,
    gy: F4,
}

/// The footprint in `source` of the coordinates in lanes 0 and 1 of `uv`.
#[inline(always)]
fn foot(source: &Source, uv: F4) -> Foot {
    let (width, height) = (source.image.width, source.image.height);
    let one = F4::splat(1.0);
    // the usual case: coordinates and sizes small enough for the conversions to be exact
    if source.small && uv.below(EXACT) & 3 == 3 {
        // (u - u.floor()) * w - 0.5: the sign of a zero `floor` does not survive the subtraction of 0.5
        let xy = uv.sub(uv.floor_small()).mul(source.size).sub(F4::splat(0.5));
        // xy is in -0.5 ..= size - 0.5 and never -0.0: its floor is exact, in -1 ..= size - 1
        let xy0 = xy.floor_small();
        let f = xy.sub(xy0);
        let (fx, fy) = (f.spread::<0>(), f.spread::<1>());
        let (x0, y0) = xy0.ints();
        let (w, h) = (width as i32, height as i32);
        // the wrap of x0 and of x0 + 1 into 0..w
        let xa = if x0 < 0 { x0 + w } else { x0 };
        let xb = if x0 + 1 == w { 0 } else { x0 + 1 };
        let ya = if y0 < 0 { y0 + h } else { y0 };
        let yb = if y0 + 1 == h { 0 } else { y0 + 1 };
        let (ra, rb) = (ya as usize * width, yb as usize * width);
        let at = [(ra + xa as usize) * 4, (ra + xb as usize) * 4, (rb + xa as usize) * 4, (rb + xb as usize) * 4];
        Foot { at, fx, fy, gx: one.sub(fx), gy: one.sub(fy) }
    } else {
        // anything else (huge, not a number): the reference's own operations
        let [u, v, ..] = uv.lanes();
        let (w, h) = (width as f32, height as f32);
        let x = (u - floor(u)) * w - 0.5;
        let y = (v - floor(v)) * h - 0.5;
        let (x0, y0) = (floor(x), floor(y));
        let (xa, xb, ya, yb) = (wrap(x0, width), wrap(x0 + 1.0, width), wrap(y0, height), wrap(y0 + 1.0, height));
        let at = [(ya * width + xa) * 4, (ya * width + xb) * 4, (yb * width + xa) * 4, (yb * width + xb) * 4];
        let (fx, fy) = (F4::splat(x - x0), F4::splat(y - y0));
        Foot { at, fx, fy, gx: one.sub(fx), gy: one.sub(fy) }
    }
}

/// The four texels of a footprint mixed: `(a * (1 - fx) + b * fx) * (1 - fy) + (c * (1 - fx) +
/// d * fx) * fy` on each channel, a texel's byte being `byte as f32 / 255.0`.
#[inline(always)]
fn texels(image: &Image, foot: &Foot) -> F4 {
    let px = |o: usize| -> F4 {
        let t = &image.rgba[o..o + 4];
        F4::new(UNIT[t[0] as usize], UNIT[t[1] as usize], UNIT[t[2] as usize], UNIT[t[3] as usize])
    };
    let (a, b, c, d) = (px(foot.at[0]), px(foot.at[1]), px(foot.at[2]), px(foot.at[3]));
    a.mul(foot.gx).add(b.mul(foot.fx)).mul(foot.gy).add(c.mul(foot.gx).add(d.mul(foot.fx)).mul(foot.fy))
}

/// `image.sample(u, v)`, bit for bit, `u` and `v` being lanes 0 and 1 of `uv`.
#[inline(always)]
fn sample(source: &Source, uv: F4) -> F4 {
    texels(source.image, &foot(source, uv))
}

/// The attributes of the three corners of a triangle.
struct Corners {
    tint: [F4; 3],
    /// First coordinates in lanes 0 and 1, second ones (the mask's) in lanes 2 and 3.
    uv: [F4; 3],
    weights: [F4; 3],
}

impl Corners {
    /// `b[0] * a[0] + b[1] * a[1] + b[2] * a[2]` on every lane, each `b` being the same on all lanes.
    #[inline(always)]
    fn mix(a: &[F4; 3], b: &[F4; 3]) -> F4 {
        b[0].mul(a[0]).add(b[1].mul(a[1])).add(b[2].mul(a[2]))
    }
}

/// What gives a pixel of a triangle its colour (RGBA), from its barycentric weights (each one on
/// all lanes).
trait Paint {
    fn colour(&self, b: &[F4; 3], corners: &Corners) -> F4;
}

/// No texture: the tint alone.
struct Flat;

impl Paint for Flat {
    #[inline(always)]
    fn colour(&self, b: &[F4; 3], corners: &Corners) -> F4 {
        Corners::mix(&corners.tint, b)
    }
}

/// One texture times the tint.
struct Textured<'a>(Source<'a>);

impl Paint for Textured<'_> {
    #[inline(always)]
    fn colour(&self, b: &[F4; 3], corners: &Corners) -> F4 {
        let tint = Corners::mix(&corners.tint, b);
        sample(&self.0, Corners::mix(&corners.uv, b)).mul(tint)
    }
}

/// Four layers mixed by the mask, or by the corners' weights.
struct Layered<'a> {
    layers: [Option<Source<'a>>; 4],
    /// For each layer, the first layer of the same size: they share their footprints.
    alike: [usize; 4],
    mask: Option<(Source<'a>, (usize, usize))>,
}

impl<'a> Layered<'a> {
    fn new(layers: [Option<&'a Image>; 4], mask: Option<(&'a Image, (usize, usize))>) -> Layered<'a> {
        let size = |k: usize| layers[k].map(|i| (i.width, i.height));
        let alike = [0, 1, 2, 3].map(|k| (0..k).find(|&j| size(j).is_some() && size(j) == size(k)).unwrap_or(k));
        Layered {
            layers: layers.map(|l| l.map(Source::new)),
            alike,
            mask: mask.map(|(m, channels)| (Source::new(m), channels)),
        }
    }
}

impl Paint for Layered<'_> {
    #[inline(always)]
    fn colour(&self, b: &[F4; 3], corners: &Corners) -> F4 {
        let tint = Corners::mix(&corners.tint, b);
        let uv = Corners::mix(&corners.uv, b);
        let weights: [f32; 4] = match &self.mask {
            Some((m, (ca, cb))) => {
                let s = sample(m, uv.high()).lanes();
                let (x, y) = (s[(*ca).min(3)], s[(*cb).min(3)]);
                [(1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y]
            }
            None => Corners::mix(&corners.weights, b).lanes(),
        };
        let total = F4::splat(weights.iter().sum::<f32>().max(1e-6));
        let mut c = F4::splat(0.0);
        // the footprints already found at this pixel, by layer
        let mut feet: [Option<Foot>; 4] = [None; 4];
        for (k, weight) in weights.into_iter().enumerate() {
            if weight <= 0.0 {
                continue;
            }
            let s = match &self.layers[k] {
                Some(source) => {
                    let first = self.alike[k];
                    let at = match feet[first] {
                        Some(at) => at,
                        None => *feet[first].insert(foot(source, uv)),
                    };
                    texels(source.image, &at)
                }
                None => F4::new(0.5, 0.5, 0.5, 1.0),
            };
            c = c.add(s.mul(F4::splat(weight)).div(total));
        }
        // the layers' colour times the tint, with the tint's own alpha
        c.opaque().mul(tint)
    }
}

/// The vertex `i` of a batch, as `triangle` and `triangle_layers` take it.
fn vertex(batch: &Batch, i: usize) -> Vertex {
    Vertex {
        pos: batch.positions[i],
        uv: batch.uvs[i],
        tint: batch.tints[i],
        weights: batch.weights.get(i).copied().unwrap_or([1.0, 0.0, 0.0, 0.0]),
        uv1: batch.uvs1.get(i).copied().unwrap_or([0.0, 0.0]),
    }
}

impl Raster {
    /// The world rectangle (x1, y1, x2, y2) outside which nothing reaches the picture (with a few
    /// pixels of margin), for a view from straight above.
    pub fn clip(&self) -> Option<[f32; 4]> {
        let Projection::Orthographic { centre, metres, .. } = self.projection;
        let (hw, hh) =
            ((self.width as f32 / 2.0 + CLIP_MARGIN) * metres, (self.height as f32 / 2.0 + CLIP_MARGIN) * metres);
        Some([centre[0] - hw, centre[1] - hh, centre[0] + hw, centre[1] + hh])
    }

    /// Draws a batch: all its triangles in order, to the same picture, bit for bit, as
    /// [`Raster::draw_reference`].
    pub fn draw(&mut self, batch: &Batch) {
        // a picture too wide for half-pixel coordinates to be exact: nothing is assumed
        if *REFERENCE || self.width >= EXACT as usize {
            return self.draw_reference(batch);
        }
        // heights only (the margin of the shading), as `triangle_layers` does it: nothing blended
        if self.heights_only && batch.mode == Mode::Blend {
            return;
        }
        PROJECTED.with_borrow_mut(|projected| {
            projected.clear();
            projected.extend(batch.positions.iter().map(|p| self.project(*p)));
            match &batch.material {
                // heights only: an opaque batch without its textures, the tint alone
                _ if self.heights_only && batch.mode == Mode::Opaque => self.fill(batch, projected, &Flat),
                Material::Single(None) => self.fill(batch, projected, &Flat),
                Material::Single(Some(texture)) => self.fill(batch, projected, &Textured(Source::new(&texture.image))),
                Material::Layers { layers, mask } => {
                    let layers = [0, 1, 2, 3].map(|k| layers[k].as_ref().map(|t| &*t.image));
                    self.fill(
                        batch,
                        projected,
                        &Layered::new(layers, mask.as_ref().map(|m| (&*m.texture.image, m.channels))),
                    )
                }
            }
        })
    }

    /// Draws a batch one triangle at a time through `triangle` and `triangle_layers`: the
    /// reference [`Raster::draw`] is checked against.
    pub fn draw_reference(&mut self, batch: &Batch) {
        for tri in batch.indices.as_chunks::<3>().0 {
            self.reference_triangle(batch, [tri[0] as usize, tri[1] as usize, tri[2] as usize]);
        }
    }

    fn reference_triangle(&mut self, batch: &Batch, i: [usize; 3]) {
        let v = i.map(|i| vertex(batch, i));
        match &batch.material {
            Material::Single(texture) => self.triangle(v, texture.as_ref().map(|t| &*t.image), batch.mode),
            Material::Layers { layers, mask } => {
                let layers: [Option<&Image>; 4] = [0, 1, 2, 3].map(|k| layers[k].as_ref().map(|t| &*t.image));
                self.triangle_layers(v, &layers, mask.as_ref().map(|m| (&*m.texture.image, m.channels)), batch.mode)
            }
        }
    }

    /// The triangles of `batch`, whose vertices are `projected`, painted by `paint`.
    fn fill<P: Paint>(&mut self, batch: &Batch, projected: &[[f32; 3]], paint: &P) {
        let width = self.width;
        let (wmax, hmax) = (self.width as f32 - 1.0, self.height as f32 - 1.0);
        let mode = batch.mode;
        let tolerance = if mode == Mode::Blend { 0.05 } else { 0.0 };
        let layered = !batch.weights.is_empty();
        let masked = !batch.uvs1.is_empty();
        let (zero, one, four) = (F4::splat(0.0), F4::splat(1.0), F4::splat(4.0));
        // what is drawn, for the profile
        let (mut kept, mut tested, mut covered, mut painted) = (0u64, 0u64, 0u64, 0u64);
        for tri in batch.indices.as_chunks::<3>().0 {
            let i = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
            let s = [projected[i[0]], projected[i[1]], projected[i[2]]];
            // the same triangles are left out as by the reference, for the same reasons
            if s.iter().any(|p| p[2] <= 0.1 || !p[0].is_finite() || !p[1].is_finite()) {
                continue;
            }
            let area = (s[1][0] - s[0][0]) * (s[2][1] - s[0][1]) - (s[2][0] - s[0][0]) * (s[1][1] - s[0][1]);
            if area.abs() < 1e-9 {
                continue;
            }
            let minx = floor(s[0][0].min(s[1][0]).min(s[2][0])).max(0.0) as i64;
            let maxx = ceil(s[0][0].max(s[1][0]).max(s[2][0])).min(wmax) as i64;
            let miny = floor(s[0][1].min(s[1][1]).min(s[2][1])).max(0.0) as i64;
            let maxy = ceil(s[0][1].max(s[1][1]).max(s[2][1])).min(hmax) as i64;
            if minx > maxx || miny > maxy {
                continue;
            }
            // the raster's filter (the margin of the shading), which `triangle_layers` asks first:
            // a triangle it refuses is not drawn
            if self.keep.as_ref().is_some_and(|keep| !keep(&i.map(|i| batch.positions[i]))) {
                continue;
            }
            kept += 1;
            tested += ((maxx - minx + 1) * (maxy - miny + 1)) as u64;
            // an area that is not a number, or beyond what the shortcuts below are proven for: as before
            if area.is_nan() || area.abs() > 1e20 {
                self.reference_triangle(batch, i);
                continue;
            }
            let corners = Corners {
                tint: i.map(|i| F4::load(batch.tints[i])),
                uv: i.map(|i| {
                    let (uv, uv1) = (batch.uvs[i], if masked { batch.uvs1[i] } else { [0.0; 2] });
                    F4::new(uv[0], uv[1], uv1[0], uv1[1])
                }),
                weights: i.map(|i| if layered { F4::load(batch.weights[i]) } else { F4::new(1.0, 0.0, 0.0, 0.0) }),
            };
            // the attributes are not divided by depth: the view from above is orthographic
            let inv = [F4::splat(1.0); 3];
            let (x0, x1, x2) = (F4::splat(s[0][0]), F4::splat(s[1][0]), F4::splat(s[2][0]));
            let (z0, z1, z2) = (F4::splat(s[0][2]), F4::splat(s[1][2]), F4::splat(s[2][2]));
            // A pixel is outside when one of its weights is negative: w0 = n0 / area, w1 = n1 / area,
            // w2 = 1 - w0 - w1. Telling so from the numerators spares the divisions for most pixels
            // outside: with m = n * sign(area), m < -1e-20 gives a quotient that is negative and not
            // zero (|area| <= 1e20), and m0 + m1 > 1.001 |area| gives w0 + w1 > 1.0008, so w2 < 0.
            // Whatever these tests do not settle (edges, numbers that are not numbers) goes through
            // the divisions and the reference's own test.
            let sign = F4::splat(if area > 0.0 { 0.0 } else { -0.0 });
            let (area4, limit, tiny) = (F4::splat(area), F4::splat(area.abs() * 1.001), F4::splat(-1e-20));
            // the centres of the first four pixels of a row: x + 0.5, exact, as is adding 4 to them
            let first =
                F4::new(minx as f32 + 0.5, (minx + 1) as f32 + 0.5, (minx + 2) as f32 + 0.5, (minx + 3) as f32 + 0.5);
            for y in miny..=maxy {
                let py = y as f32 + 0.5;
                let (e0, e1, e2) = (F4::splat(s[0][1] - py), F4::splat(s[1][1] - py), F4::splat(s[2][1] - py));
                let row = y as usize * width;
                let (mut next, mut centres) = (minx, first);
                while next <= maxx {
                    // four pixels at once: x .. x + 3, their centres in `px`
                    let (x, px) = (next, centres);
                    next += 4;
                    centres = centres.add(four);
                    let (d0, d1, d2) = (x0.sub(px), x1.sub(px), x2.sub(px));
                    let n0 = d1.mul(e2).sub(d2.mul(e1));
                    let n1 = d2.mul(e0).sub(d0.mul(e2));
                    let (m0, m1) = (n0.flip(sign), n1.flip(sign));
                    // those of the four that are in the row and not told outside
                    let mut inside =
                        ((1u32 << (maxx - x + 1).min(4)) - 1) & !(m0.lt(tiny) | m1.lt(tiny) | m0.add(m1).gt(limit));
                    if inside == 0 {
                        continue;
                    }
                    let w0 = n0.div(area4);
                    let w1 = n1.div(area4);
                    let w2 = one.sub(w0).sub(w1);
                    inside &= !(w0.lt(zero) | w1.lt(zero) | w2.lt(zero));
                    if inside == 0 {
                        continue;
                    }
                    covered += inside.count_ones() as u64;
                    let (b0, b1, b2) = (w0.mul(inv[0]), w1.mul(inv[1]), w2.mul(inv[2]));
                    let sum = b0.add(b1).add(b2);
                    let (b0, b1, b2) = (b0.div(sum), b1.div(sum), b2.div(sum));
                    let depth = b0.mul(z0).add(b1.mul(z1)).add(b2.mul(z2));
                    let (b0, b1, b2, depth) = (b0.lanes(), b1.lanes(), b2.lanes(), depth.lanes());
                    // the pixels left, from the first to the last
                    while inside != 0 {
                        let lane = inside.trailing_zeros() as usize;
                        inside &= inside - 1;
                        let at = row + x as usize + lane;
                        let depth = depth[lane];
                        if depth > self.depth[at] + tolerance {
                            continue;
                        }
                        painted += 1;
                        let c = paint
                            .colour(&[F4::splat(b0[lane]), F4::splat(b1[lane]), F4::splat(b2[lane])], &corners)
                            .lanes();
                        match mode {
                            Mode::Opaque => {
                                self.colour[at] = [c[0], c[1], c[2]];
                                self.depth[at] = depth;
                            }
                            Mode::Cutout => {
                                if c[3] >= 0.5 {
                                    self.colour[at] = [c[0], c[1], c[2]];
                                    self.depth[at] = depth;
                                }
                            }
                            Mode::Blend => {
                                let a = c[3].clamp(0.0, 1.0);
                                let o = self.colour[at];
                                self.colour[at] = [0, 1, 2].map(|k| o[k] * (1.0 - a) + c[k] * a);
                            }
                        }
                    }
                }
            }
        }
        if profile::on() {
            profile::count(Count::Triangles, (batch.indices.len() / 3) as u64);
            profile::count(Count::Kept, kept);
            profile::count(Count::Tested, tested);
            profile::count(Count::Covered, covered);
            let kind = match mode {
                Mode::Opaque if layered => Count::Layered,
                Mode::Opaque => Count::Opaque,
                Mode::Cutout => Count::Cutout,
                Mode::Blend => Count::Blended,
            };
            profile::count(kind, painted);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::scene::{Mask, Tex};
    use std::hint::black_box;

    /// A small deterministic generator (xorshift64*).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        /// In 0..1.
        fn unit(&mut self) -> f32 {
            (self.next() >> 40) as f32 / (1u64 << 24) as f32
        }

        fn range(&mut self, lo: f32, hi: f32) -> f32 {
            lo + (hi - lo) * self.unit()
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        fn image(&mut self, width: usize, height: usize) -> Image {
            Image { width, height, rgba: (0..width * height * 4).map(|_| (self.next() >> 56) as u8).collect() }
        }
    }

    #[test]
    fn table_holds_the_quotients() {
        for i in 0..=255u8 {
            assert_eq!(UNIT[i as usize].to_bits(), (black_box(i) as f32 / black_box(255.0f32)).to_bits(), "{i}");
        }
    }

    /// Values that stress rounding: integers, halves, their neighbours, zeros, huge, not numbers.
    fn odd_values(rng: &mut Rng) -> Vec<f32> {
        let mut values = vec![
            0.0,
            -0.0,
            0.5,
            -0.5,
            1.0,
            -1.0,
            8_388_607.5,
            -8_388_607.5,
            8_388_608.0,
            -8_388_608.0,
            1e9,
            -1e9,
            3e38,
            -3e38,
        ];
        values.extend([
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            1e-40,
            -1e-40,
        ]);
        for k in -70..70 {
            let x = k as f32;
            values.extend([
                x,
                f32::from_bits(x.to_bits() + 1),
                f32::from_bits(x.to_bits().wrapping_sub(1)),
                x + 0.5,
                x + 0.25,
            ]);
        }
        for _ in 0..200_000 {
            values.push(f32::from_bits(rng.next() as u32));
            values.push(rng.range(-3000.0, 3000.0));
        }
        values
    }

    #[test]
    fn floor_and_ceil_are_the_library_ones() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for x in odd_values(&mut rng) {
            assert_eq!(floor(x).to_bits(), black_box(x).floor().to_bits(), "floor({x:?})");
            assert_eq!(ceil(x).to_bits(), black_box(x).ceil().to_bits(), "ceil({x:?})");
        }
    }

    #[test]
    fn wrap_is_rem_euclid() {
        let mut rng = Rng(7);
        for n in [1usize, 2, 3, 4, 7, 16, 100, 1024] {
            for x in odd_values(&mut rng).into_iter().take(3000) {
                assert_eq!(wrap(x, n), (x as i64).rem_euclid(n as i64) as usize, "wrap({x:?}, {n})");
            }
        }
    }

    #[test]
    fn sample_is_the_reference_one() {
        let mut rng = Rng(0xD1B5_4A32_D192_ED03);
        for (w, h) in [(1, 1), (2, 2), (4, 4), (3, 5), (7, 3), (16, 16), (64, 32), (100, 60), (256, 256)] {
            let image = rng.image(w, h);
            let odd = odd_values(&mut rng);
            for n in 0..40_000 {
                let (u, v) = match n % 4 {
                    0 => (rng.range(-4.0, 4.0), rng.range(-4.0, 4.0)),
                    // on texel borders and centres
                    1 => {
                        (rng.below(4 * w) as f32 / (2 * w) as f32 - 0.5, rng.below(4 * h) as f32 / (2 * h) as f32 - 0.5)
                    }
                    2 => (odd[rng.below(odd.len())], rng.range(-2.0, 2.0)),
                    _ => (rng.range(-2.0, 2.0), odd[rng.below(odd.len())]),
                };
                let (a, b) = (sample(&Source::new(&image), F4::new(u, v, 0.0, 0.0)).lanes(), image.sample(u, v));
                assert_eq!(a.map(f32::to_bits), b.map(f32::to_bits), "{w}×{h} at ({u:?}, {v:?})");
            }
        }
    }

    /// A random batch around a 96 × 64 picture: triangles of all sizes, some outside, some
    /// degenerate, a few with coordinates that are not numbers.
    struct Random {
        positions: Vec<[f32; 3]>,
        uvs: Vec<[f32; 2]>,
        tints: Vec<[f32; 4]>,
        weights: Vec<[f32; 4]>,
        uvs1: Vec<[f32; 2]>,
        indices: Vec<u16>,
    }

    fn random_batch(rng: &mut Rng, layered: bool, masked: bool, wild: bool) -> Random {
        let n = 60;
        let mut r =
            Random { positions: vec![], uvs: vec![], tints: vec![], weights: vec![], uvs1: vec![], indices: vec![] };
        for _ in 0..n {
            // world: the picture covers x in -48..48, y in -32..32 (1 m per pixel), heights 0..40
            let spread = [1.0, 1.0, 1.0, 3.0, 30.0][rng.below(5)];
            let mut p = [rng.range(-60.0, 60.0) * spread, rng.range(-45.0, 45.0) * spread, rng.range(0.0, 40.0)];
            // on pixel centres and borders, where a weight is exactly zero
            if rng.below(3) == 0 {
                p = [(p[0] * 2.0).round() / 2.0, (p[1] * 2.0).round() / 2.0, p[2].round()];
            }
            if wild {
                match rng.below(40) {
                    0 => p[rng.below(3)] = f32::NAN,
                    1 => p[rng.below(3)] = f32::INFINITY,
                    2 => p[2] = 1e6, // above the camera
                    3 => p[rng.below(2)] = 3e30 * (rng.unit() - 0.5),
                    _ => {}
                }
            }
            r.positions.push(p);
            r.uvs.push([rng.range(-3.0, 3.0), rng.range(-3.0, 3.0)]);
            r.tints.push([rng.unit(), rng.unit(), rng.unit(), if rng.below(2) == 0 { 1.0 } else { rng.unit() }]);
            if layered {
                let (a, b) = ((rng.next() >> 56) as f32 / 255.0, (rng.next() >> 56) as f32 / 255.0);
                r.weights.push([(1.0 - a) * (1.0 - b), a * (1.0 - b), (1.0 - a) * b, a * b]);
                if masked {
                    r.uvs1.push([rng.range(-2.0, 2.0), rng.range(-2.0, 2.0)]);
                }
            }
        }
        for _ in 0..150 {
            let a = rng.below(n);
            // mostly small triangles (neighbours in the list are unrelated: all sizes), some degenerate
            let (b, c) = match rng.below(12) {
                0 => (a, rng.below(n)),
                _ => (rng.below(n), rng.below(n)),
            };
            r.indices.extend([a as u16, b as u16, c as u16]);
        }
        r
    }

    fn same(a: &Raster, b: &Raster, what: &str) {
        for (i, (x, y)) in a.colour.iter().zip(&b.colour).enumerate() {
            assert_eq!(x.map(f32::to_bits), y.map(f32::to_bits), "{what}: colour of pixel {i} ({x:?} / {y:?})");
        }
        for (i, (x, y)) in a.depth.iter().zip(&b.depth).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "{what}: depth of pixel {i} ({x:?} / {y:?})");
        }
    }

    #[test]
    fn draw_is_the_reference_one() {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let textures: Vec<Tex> =
            [(4, 4), (16, 8), (7, 5), (32, 32), (1, 1)].iter().map(|&(w, h)| Tex::new(rng.image(w, h))).collect();
        let projections = [
            Projection::Orthographic { centre: [0.0, 0.0], metres: 1.0, top: 100.0 },
            Projection::Orthographic { centre: [3.3, -7.1], metres: 0.37, top: 55.5 },
        ];
        let mut drawn = 0;
        for round in 0..90 {
            let projection = projections[round % projections.len()];
            let (mut fast, mut reference) =
                (Raster::new(96, 64, projection, [0.1, 0.2, 0.3]), Raster::new(96, 64, projection, [0.1, 0.2, 0.3]));
            // a picture is several batches over one another: opaque, cut out, blended
            for step in 0..9 {
                let mode = [Mode::Opaque, Mode::Opaque, Mode::Cutout, Mode::Blend]
                    [if step < 3 { step % 2 } else { 2 + step % 2 }];
                let kind = rng.below(5);
                let (layered, masked) = (kind >= 3, kind == 4);
                let pick = |rng: &mut Rng| (rng.below(4) != 0).then(|| textures[rng.below(textures.len())].clone());
                let material = match kind {
                    0 => Material::Single(None),
                    1 | 2 => Material::Single(Some(textures[rng.below(textures.len())].clone())),
                    _ => Material::Layers {
                        layers: [pick(&mut rng), pick(&mut rng), pick(&mut rng), pick(&mut rng)],
                        mask: masked.then(|| Mask {
                            texture: textures[rng.below(textures.len())].clone(),
                            channels: [(2, 1), (0, 3), (1, 1)][rng.below(3)],
                        }),
                    },
                };
                let r = random_batch(&mut rng, layered, masked, round % 5 == 4);
                let batch = Batch {
                    mode,
                    material,
                    positions: &r.positions,
                    uvs: &r.uvs,
                    tints: &r.tints,
                    weights: &r.weights,
                    uvs1: &r.uvs1,
                    indices: &r.indices,
                };
                fast.draw(&batch);
                reference.draw_reference(&batch);
                same(&fast, &reference, &format!("round {round}, batch {step}"));
            }
            drawn += fast.depth.iter().filter(|d| d.is_finite()).count();
        }
        // the test is worth something only if triangles did reach the pictures
        assert!(drawn > 90 * 96 * 64 / 4, "{drawn} pixels drawn");
    }

    /// The pass of the shading's margin (`Raster::set_pass`: a filter of triangles, heights only):
    /// `draw` follows it bit for bit as `triangle_layers` does, and with heights only the depths
    /// are those of the plain drawing (blended batches write none).
    #[test]
    fn draw_follows_the_pass_as_the_reference_does() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let mut rng = Rng(0x1234_5678_9ABC_DEF1);
        let textures: Vec<Tex> =
            [(4, 4), (16, 8), (7, 5), (32, 32), (1, 1)].iter().map(|&(w, h)| Tex::new(rng.image(w, h))).collect();
        let projections = [
            Projection::Orthographic { centre: [0.0, 0.0], metres: 1.0, top: 100.0 },
            Projection::Orthographic { centre: [3.3, -7.1], metres: 0.37, top: 55.5 },
        ];
        let (refused, accepted) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
        let (mut drawn, mut blended) = (0, 0);
        for round in 0..120 {
            let projection = projections[round % projections.len()];
            // the four passes: with or without a filter, colours or heights only
            let (filtered, heights_only) = (round % 2 == 1, (round / 2) % 2 == 1);
            let new = || Raster::new(96, 64, projection, [0.1, 0.2, 0.3]);
            let (mut fast, mut reference, mut plain) = (new(), new(), new());
            // the filter: a triangle wholly west of a line is refused (the margin's own filter
            // refuses by the triangle's box too)
            let limit = rng.range(-30.0, 30.0);
            for raster in [&mut fast, &mut reference] {
                let (refused, accepted) = (refused.clone(), accepted.clone());
                let keep = filtered.then(|| {
                    Box::new(move |t: &[[f32; 3]; 3]| {
                        let keep = !t.iter().all(|p| p[0] < limit);
                        if keep { &accepted } else { &refused }.fetch_add(1, Ordering::Relaxed);
                        keep
                    }) as crate::render::raster::Keep
                });
                raster.set_pass(keep, heights_only);
            }
            for step in 0..9 {
                let mode = [Mode::Opaque, Mode::Opaque, Mode::Cutout, Mode::Blend]
                    [if step < 3 { step % 2 } else { 2 + step % 2 }];
                let kind = rng.below(5);
                let (layered, masked) = (kind >= 3, kind == 4);
                let pick = |rng: &mut Rng| (rng.below(4) != 0).then(|| textures[rng.below(textures.len())].clone());
                let material = match kind {
                    0 => Material::Single(None),
                    1 | 2 => Material::Single(Some(textures[rng.below(textures.len())].clone())),
                    _ => Material::Layers {
                        layers: [pick(&mut rng), pick(&mut rng), pick(&mut rng), pick(&mut rng)],
                        mask: masked.then(|| Mask {
                            texture: textures[rng.below(textures.len())].clone(),
                            channels: [(2, 1), (0, 3), (1, 1)][rng.below(3)],
                        }),
                    },
                };
                let r = random_batch(&mut rng, layered, masked, round % 5 == 4);
                let batch = Batch {
                    mode,
                    material,
                    positions: &r.positions,
                    uvs: &r.uvs,
                    tints: &r.tints,
                    weights: &r.weights,
                    uvs1: &r.uvs1,
                    indices: &r.indices,
                };
                fast.draw(&batch);
                reference.draw_reference(&batch);
                same(
                    &fast,
                    &reference,
                    &format!("round {round} (filter {filtered}, heights only {heights_only}), batch {step}"),
                );
                // the plain drawing, without the blended batches
                if mode != Mode::Blend {
                    plain.draw_reference(&batch);
                } else if heights_only {
                    blended += 1;
                }
            }
            if heights_only && !filtered {
                for (i, (x, y)) in fast.depth.iter().zip(&plain.depth).enumerate() {
                    assert_eq!(x.to_bits(), y.to_bits(), "round {round}: height of pixel {i} ({x:?} / {y:?})");
                }
            }
            drawn += fast.depth.iter().filter(|d| d.is_finite()).count();
        }
        // the test is worth something only if the filter refused and accepted triangles (each is
        // asked once by each of the two rasters, the fast one only for those that reach the picture),
        // if blended batches came in heights only, and if triangles did reach the pictures
        let (refused, accepted) = (refused.load(Ordering::Relaxed), accepted.load(Ordering::Relaxed));
        assert!(refused > 2000 && accepted > 2000, "{refused} refused, {accepted} accepted");
        assert!(blended > 50, "{blended} blended batches in heights only");
        assert!(drawn > 120 * 96 * 64 / 8, "{drawn} pixels drawn");
    }

    #[test]
    fn nothing_beyond_the_clip_reaches_the_picture() {
        let mut rng = Rng(99);
        let projection = Projection::Orthographic { centre: [10.0, 20.0], metres: 0.5, top: 100.0 };
        let clip = Raster::new(64, 48, projection, [0.0; 3]).clip().unwrap();
        for _ in 0..200 {
            let mut r = random_batch(&mut rng, false, false, false);
            // moved wholly to one side of the rectangle
            let side = rng.below(4);
            for p in &mut r.positions {
                match side {
                    0 => p[0] = clip[0] - p[0].abs() - 1e-3,
                    1 => p[0] = clip[2] + p[0].abs() + 1e-3,
                    2 => p[1] = clip[1] - p[1].abs() - 1e-3,
                    _ => p[1] = clip[3] + p[1].abs() + 1e-3,
                }
            }
            let mut raster = Raster::new(64, 48, projection, [0.0; 3]);
            raster.draw_reference(&Batch {
                mode: Mode::Opaque,
                material: Material::Single(None),
                positions: &r.positions,
                uvs: &r.uvs,
                tints: &r.tints,
                weights: &r.weights,
                uvs1: &r.uvs1,
                indices: &r.indices,
            });
            assert!(raster.depth.iter().all(|d| d.is_infinite()));
        }
    }

    /// Not a check: times `draw` against `draw_reference` on a picture the size of a block, filled
    /// with as many small triangles as a downtown block has (about 40 million pixels of boxes, 10
    /// million covered), for each kind of material and mode; the best of three runs each.
    /// `cargo test --release -- --ignored --nocapture draw_speed`
    #[test]
    #[ignore]
    fn draw_speed() {
        use std::time::Instant;
        let mut rng = Rng(42);
        let side = 2048usize;
        let metres = 128.0 / side as f32;
        let projection = Projection::Orthographic { centre: [0.0, 0.0], metres, top: 3000.0 };
        let textures: Vec<Tex> = [(1024, 1024), (512, 512), (1024, 512), (256, 256), (64, 64)]
            .iter()
            .map(|&(w, h)| Tex::new(rng.image(w, h)))
            .collect();
        // sheets of small triangles over the whole picture, one above the other, drawn in an order
        // that hides some of them: neighbours in the list are neighbours in the picture, as in a model
        let (mut boxes, mut covered, mut count) = (0.0f64, 0.0f64, 0usize);
        let mut pieces: Vec<Random> = Vec::new();
        for (sheet, height) in [20.0f32, 0.0, 30.0, 10.0].into_iter().enumerate() {
            let cell = 13.0 * metres;
            let (sin, cos) = (sheet as f32 * 0.5).sin_cos();
            let cells = (1.5 * side as f32 * metres / cell) as i32;
            let corner = |rng: &mut Rng, i: i32, j: i32| -> [f32; 2] {
                // a jittered grid, turned by half a radian more for each sheet
                let (x, y) = ((i - cells / 2) as f32 * cell, (j - cells / 2) as f32 * cell);
                let (x, y) = (x * cos - y * sin, x * sin + y * cos);
                // the same corner for every triangle that shares it: the jitter comes from its place
                let mut local = Rng((i as u64) << 32 ^ j as u64 ^ rng.0);
                local.next();
                [x + cell * local.range(-0.3, 0.3), y + cell * local.range(-0.3, 0.3)]
            };
            let mut r = Random {
                positions: vec![],
                uvs: vec![],
                tints: vec![],
                weights: vec![],
                uvs1: vec![],
                indices: vec![],
            };
            for j in 0..cells {
                for i in 0..cells {
                    let quad = [
                        corner(&mut rng, i, j),
                        corner(&mut rng, i + 1, j),
                        corner(&mut rng, i + 1, j + 1),
                        corner(&mut rng, i, j + 1),
                    ];
                    for tri in [[0, 1, 2], [0, 2, 3]] {
                        let c = tri.map(|k| quad[k]);
                        let (lo, hi) = (
                            [0, 1].map(|k| c.iter().map(|c| c[k]).fold(f32::MAX, f32::min)),
                            [0, 1].map(|k| c.iter().map(|c| c[k]).fold(f32::MIN, f32::max)),
                        );
                        if hi[0] < -64.0 || lo[0] > 64.0 || hi[1] < -64.0 || lo[1] > 64.0 {
                            continue;
                        }
                        if r.positions.len() + 3 > 65535 {
                            pieces.push(std::mem::replace(
                                &mut r,
                                Random {
                                    positions: vec![],
                                    uvs: vec![],
                                    tints: vec![],
                                    weights: vec![],
                                    uvs1: vec![],
                                    indices: vec![],
                                },
                            ));
                        }
                        for c in c {
                            r.indices.push(r.positions.len() as u16);
                            r.positions.push([c[0], c[1], height + 0.01 * (c[0] + c[1])]);
                            // 8 texels per pixel of a 1024 texture, as ground textures have
                            r.uvs.push([c[0] * 0.125, c[1] * 0.125]);
                            r.tints.push([1.0, 1.0, 1.0, rng.unit()]);
                            let (a, b) = ((rng.next() >> 56) as f32 / 255.0, (rng.next() >> 56) as f32 / 255.0);
                            r.weights.push([(1.0 - a) * (1.0 - b), a * (1.0 - b), (1.0 - a) * b, a * b]);
                            r.uvs1.push([c[0] / 128.0, c[1] / 128.0]);
                        }
                        count += 1;
                        boxes += ((hi[0] - lo[0]) / metres + 1.0) as f64 * ((hi[1] - lo[1]) / metres + 1.0) as f64;
                        covered += (((c[1][0] - c[0][0]) * (c[2][1] - c[0][1])
                            - (c[2][0] - c[0][0]) * (c[1][1] - c[0][1]))
                            .abs()
                            / 2.0
                            / metres
                            / metres) as f64;
                    }
                }
            }
            pieces.push(r);
        }
        println!(
            "{count} triangles, about {:.0} million pixels of boxes, {:.0} million covered",
            boxes / 1e6,
            covered / 1e6
        );
        let single = |k: usize| Material::Single(Some(textures[k].clone()));
        let layers = |mask: bool| Material::Layers {
            layers: [Some(textures[0].clone()), Some(textures[1].clone()), None, Some(textures[2].clone())],
            mask: mask.then(|| Mask { texture: textures[3].clone(), channels: (2, 1) }),
        };
        let cases: Vec<(&str, Mode, Material)> = vec![
            ("no texture, opaque", Mode::Opaque, Material::Single(None)),
            ("one texture, opaque", Mode::Opaque, single(0)),
            ("one texture, cut out", Mode::Cutout, single(1)),
            ("one texture, blended", Mode::Blend, single(2)),
            ("four layers and a mask, opaque", Mode::Opaque, layers(true)),
            ("four layers by vertex weights, opaque", Mode::Opaque, layers(false)),
        ];
        for (name, mode, material) in cases {
            let layered = matches!(material, Material::Layers { .. });
            let masked = matches!(&material, Material::Layers { mask: Some(_), .. });
            let run = |reference: bool| {
                let mut best = f64::MAX;
                let mut raster = Raster::new(side, side, projection, [0.0; 3]);
                for _ in 0..3 {
                    raster = Raster::new(side, side, projection, [0.0; 3]);
                    let t = Instant::now();
                    for r in &pieces {
                        let batch = Batch {
                            mode,
                            material: material.clone(),
                            positions: &r.positions,
                            uvs: &r.uvs,
                            tints: &r.tints,
                            weights: if layered { &r.weights } else { &[] },
                            uvs1: if masked { &r.uvs1 } else { &[] },
                            indices: &r.indices,
                        };
                        if reference { raster.draw_reference(&batch) } else { raster.draw(&batch) }
                    }
                    best = best.min(t.elapsed().as_secs_f64());
                }
                (best, raster)
            };
            let ((slow, a), (fast, b)) = (run(true), run(false));
            same(&a, &b, name);
            println!(
                "{name:<46} reference {:>6.0} ms, draw {:>6.0} ms ({:.1} times faster)",
                slow * 1e3,
                fast * 1e3,
                slow / fast
            );
        }
    }
}
