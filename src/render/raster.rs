//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! A software rasterizer for views from above: triangles with a depth buffer, textures sampled
//! bilinearly (repeated), colours unlit (the textures' own colour, without the game's light).
//!
//! The camera looks straight down, north up and east right, orthographic (square pixels of a
//! given size in metres, as the tiles are).

/// An RGBA image, 8 bits per channel.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Image {
    /// Bilinear sample at (u, v), repeated, in 0..1 floats.
    pub fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let (w, h) = (self.width as f32, self.height as f32);
        let x = (u - u.floor()) * w - 0.5;
        let y = (v - v.floor()) * h - 0.5;
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let wrap = |a: f32, n: usize| (a as i64).rem_euclid(n as i64) as usize;
        let (xa, xb, ya, yb) =
            (wrap(x0, self.width), wrap(x0 + 1.0, self.width), wrap(y0, self.height), wrap(y0 + 1.0, self.height));
        let px = |x: usize, y: usize| {
            let o = (y * self.width + x) * 4;
            [0, 1, 2, 3].map(|c| self.rgba[o + c] as f32 / 255.0)
        };
        let (a, b, c, d) = (px(xa, ya), px(xb, ya), px(xa, yb), px(xb, yb));
        [0, 1, 2, 3].map(|k| (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy)
    }
}

#[derive(Clone, Copy)]
pub enum Projection {
    /// Square pixels of `metres` per pixel, image centred on `centre`, seen from `top`.
    Orthographic { centre: [f32; 2], metres: f32, top: f32 },
}

/// How a triangle is drawn.
#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    /// Writes colour and depth.
    Opaque,
    /// Writes where alpha ≥ ½, colour and depth.
    Cutout,
    /// Blended by alpha over what is there, depth tested, not written.
    Blend,
}

/// A vertex: world position, texture coordinates, colour multiplier, and the weights of the
/// layers when the triangle is drawn with several textures.
#[derive(Clone, Copy)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub uv: [f32; 2],
    pub tint: [f32; 4],
    pub weights: [f32; 4],
    /// Second texture coordinates (a blend mask).
    pub uv1: [f32; 2],
}

/// Decides, from the three world positions of a triangle, whether it is drawn (`Send` and `Sync`,
/// so that a raster can still go from one thread to another).
pub type Keep = Box<dyn Fn(&[[f32; 3]; 3]) -> bool + Send + Sync>;

pub struct Raster {
    pub width: usize,
    pub height: usize,
    pub projection: Projection,
    pub(crate) colour: Vec<[f32; 3]>,
    pub(crate) depth: Vec<f32>,
    focal: f32,
    /// When set, only the triangles it accepts are drawn (the margin of the shading).
    /// (`pub(crate)`, like `heights_only`: `draw.rs` draws without `triangle_layers` and follows both.)
    pub(crate) keep: Option<Keep>,
    /// Only the heights matter (the margin of the shading): textures are left out, but for the
    /// cut-outs, whose shape they give, and blended triangles are not drawn.
    pub(crate) heights_only: bool,
}

impl Raster {
    pub fn new(width: usize, height: usize, projection: Projection, background: [f32; 3]) -> Raster {
        let Projection::Orthographic { metres, .. } = projection;
        let focal = 1.0 / metres;
        Raster {
            width,
            height,
            projection,
            colour: vec![background; width * height],
            depth: vec![f32::INFINITY; width * height],
            focal,
            keep: None,
            heights_only: false,
        }
    }

    /// Screen position (x right, y down) and depth (metres below the camera) of a world point.
    pub fn project(&self, p: [f32; 3]) -> [f32; 3] {
        let (cw, ch) = (self.width as f32 / 2.0, self.height as f32 / 2.0);
        let Projection::Orthographic { centre, top, .. } = self.projection;
        [cw + self.focal * (p[0] - centre[0]), ch - self.focal * (p[1] - centre[1]), top - p[2]]
    }

    /// Draws a triangle; `texture` None draws the tint alone.
    pub fn triangle(&mut self, v: [Vertex; 3], texture: Option<&Image>, mode: Mode) {
        self.triangle_layers(v, &[texture], None, mode)
    }

    /// Draws a triangle with one texture, or with up to four layers mixed by the vertex weights
    /// (terrain); a missing layer counts as mid grey.
    /// With `mask` (image, channel of the 0-1 / 2-3 lerp, channel of the final lerp), the weights
    /// come from that image at the second coordinates, per pixel.
    pub fn triangle_layers(
        &mut self,
        v: [Vertex; 3],
        layers: &[Option<&Image>],
        mask: Option<(&Image, (usize, usize))>,
        mode: Mode,
    ) {
        if self.keep.as_ref().is_some_and(|keep| !keep(&v.map(|x| x.pos))) {
            return;
        }
        type MaskImage<'a> = Option<(&'a Image, (usize, usize))>;
        let (layers, mask): (&[Option<&Image>], MaskImage<'_>) = match mode {
            Mode::Blend if self.heights_only => return,
            Mode::Opaque if self.heights_only => (&[None], None),
            _ => (layers, mask),
        };
        let s = v.map(|x| self.project(x.pos));
        if s.iter().any(|p| p[2] <= 0.1 || !p[0].is_finite() || !p[1].is_finite()) {
            return;
        }
        let area = (s[1][0] - s[0][0]) * (s[2][1] - s[0][1]) - (s[2][0] - s[0][0]) * (s[1][1] - s[0][1]);
        if area.abs() < 1e-9 {
            return;
        }
        let minx = s.iter().map(|p| p[0]).fold(f32::MAX, f32::min).floor().max(0.0) as i64;
        let maxx = s.iter().map(|p| p[0]).fold(f32::MIN, f32::max).ceil().min(self.width as f32 - 1.0) as i64;
        let miny = s.iter().map(|p| p[1]).fold(f32::MAX, f32::min).floor().max(0.0) as i64;
        let maxy = s.iter().map(|p| p[1]).fold(f32::MIN, f32::max).ceil().min(self.height as f32 - 1.0) as i64;
        if minx > maxx || miny > maxy {
            return;
        }
        // the attributes are not divided by depth: the view from above is orthographic
        let inv = [1.0f32; 3];
        for y in miny..=maxy {
            for x in minx..=maxx {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let w0 = ((s[1][0] - px) * (s[2][1] - py) - (s[2][0] - px) * (s[1][1] - py)) / area;
                let w1 = ((s[2][0] - px) * (s[0][1] - py) - (s[0][0] - px) * (s[2][1] - py)) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                let b = [w0 * inv[0], w1 * inv[1], w2 * inv[2]];
                let sum = b[0] + b[1] + b[2];
                let b = b.map(|x| x / sum);
                let depth = b[0] * s[0][2] + b[1] * s[1][2] + b[2] * s[2][2];
                let i = y as usize * self.width + x as usize;
                let tolerance = if mode == Mode::Blend { 0.05 } else { 0.0 };
                if depth > self.depth[i] + tolerance {
                    continue;
                }
                let tint: [f32; 4] =
                    [0, 1, 2, 3].map(|k| b[0] * v[0].tint[k] + b[1] * v[1].tint[k] + b[2] * v[2].tint[k]);
                let u = b[0] * v[0].uv[0] + b[1] * v[1].uv[0] + b[2] * v[2].uv[0];
                let w = b[0] * v[0].uv[1] + b[1] * v[1].uv[1] + b[2] * v[2].uv[1];
                let c = if layers.len() == 1 {
                    match layers[0] {
                        Some(t) => {
                            let c = t.sample(u, w);
                            [c[0] * tint[0], c[1] * tint[1], c[2] * tint[2], c[3] * tint[3]]
                        }
                        None => tint,
                    }
                } else {
                    let weights: [f32; 4] = match mask {
                        Some((m, (ca, cb))) => {
                            let mu = b[0] * v[0].uv1[0] + b[1] * v[1].uv1[0] + b[2] * v[2].uv1[0];
                            let mv = b[0] * v[0].uv1[1] + b[1] * v[1].uv1[1] + b[2] * v[2].uv1[1];
                            let s = m.sample(mu, mv);
                            let (x, y) = (s[ca.min(3)], s[cb.min(3)]);
                            [(1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y]
                        }
                        None => [0, 1, 2, 3]
                            .map(|k| b[0] * v[0].weights[k] + b[1] * v[1].weights[k] + b[2] * v[2].weights[k]),
                    };
                    let total: f32 = weights.iter().sum::<f32>().max(1e-6);
                    let mut c = [0.0f32; 4];
                    for (layer, weight) in layers.iter().zip(weights) {
                        if weight <= 0.0 {
                            continue;
                        }
                        let s = layer.map(|t| t.sample(u, w)).unwrap_or([0.5, 0.5, 0.5, 1.0]);
                        for k in 0..4 {
                            c[k] += s[k] * weight / total;
                        }
                    }
                    [c[0] * tint[0], c[1] * tint[1], c[2] * tint[2], tint[3]]
                };
                match mode {
                    Mode::Opaque => {
                        self.colour[i] = [c[0], c[1], c[2]];
                        self.depth[i] = depth;
                    }
                    Mode::Cutout => {
                        if c[3] >= 0.5 {
                            self.colour[i] = [c[0], c[1], c[2]];
                            self.depth[i] = depth;
                        }
                    }
                    Mode::Blend => {
                        let a = c[3].clamp(0.0, 1.0);
                        let o = self.colour[i];
                        self.colour[i] = [0, 1, 2].map(|k| o[k] * (1.0 - a) + c[k] * a);
                    }
                }
            }
        }
    }

    /// SHA-1 of the picture's exact colours and depths (the bits of every value): two renders are
    /// the same picture when their digests are equal.
    pub fn digest(&self) -> String {
        use sha1::{Digest, Sha1};
        let mut h = Sha1::new();
        let mut bytes = Vec::with_capacity(4096 * 16);
        for (colours, depths) in self.colour.chunks(4096).zip(self.depth.chunks(4096)) {
            bytes.clear();
            for (c, d) in colours.iter().zip(depths) {
                for x in c {
                    bytes.extend_from_slice(&x.to_bits().to_le_bytes());
                }
                bytes.extend_from_slice(&d.to_bits().to_le_bytes());
            }
            h.update(&bytes);
        }
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The picture in RGBA: opaque where something was drawn, transparent elsewhere.
    pub fn rgba(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.colour.len() * 4];
        for ((px, c), d) in out.as_chunks_mut::<4>().0.iter_mut().zip(&self.colour).zip(&self.depth) {
            for k in 0..3 {
                px[k] = (c[k].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            px[3] = if d.is_finite() { 255 } else { 0 };
        }
        out
    }

    /// From now on, only the triangles `keep` accepts are drawn (None: all of them), and with
    /// `heights_only` for their height alone (no texture but for the cut-outs, nothing blended).
    pub fn set_pass(&mut self, keep: Option<Keep>, heights_only: bool) {
        self.keep = keep;
        self.heights_only = heights_only;
    }

    /// The depth of every pixel, row by row (metres below the camera, or below `top` for an
    /// orthographic view; infinite where nothing was drawn): the height map the shading reads.
    pub fn depths(&self) -> &[f32] {
        &self.depth
    }

    /// The colour of every pixel, row by row, to be changed in place (the shading).
    pub fn colours_mut(&mut self) -> &mut [[f32; 3]] {
        &mut self.colour
    }
}
