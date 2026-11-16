//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! What to draw, for any renderer: the entities of a view as a list of [`Batch`]es.
//!
//! [`stream`] is the first half of what `render::assets::paint` did in one go: it walks the
//! entities, picks each model's detail level, finds the textures, and hands out one batch per piece
//! of geometry, with everything a renderer needs and nothing about how it draws: vertices in world
//! space, their texture coordinates, tints and layer weights, the triangles, the textures, the
//! drawing mode. The second half is the renderer: `Raster::draw` on the CPU
//! ([`crate::render::draw`]), or the graphics card's ([`crate::render::gpu`]).
//!
//! The contract of the list:
//!
//! - **Order matters.** Batches come in three passes, opaque, then cut out, then blended, and
//!   inside a pass in the order of the entities. A renderer must give the same result as drawing
//!   every triangle one after the other in that order: at equal depth the later triangle wins
//!   (opaque and cut out), and blending is done over what is already there.
//! - **Depth** is the distance below the camera (`top - z` from straight above); smaller is
//!   nearer. Opaque and cut-out triangles are tested with "nearer or equal" and write it; blended
//!   ones are tested with a tolerance of 0.05 m and do not write it (`raster::Mode`).
//! - **Colour** of a pixel: the texture sampled bilinearly, repeated, at the interpolated `uvs`
//!   (first level only, no mip-mapping), times the interpolated tint (RGB and alpha). Without a
//!   texture: the tint alone (the builder already made it mid grey). Cut out: kept where alpha is
//!   at least one half.
//! - **Terrain** ([`Material::Layers`]): four textures at the same `uvs`, mixed by weights that
//!   come per pixel from the mask sampled at `uvs1` when there is one, else from the interpolated
//!   `weights`; a missing layer counts as mid grey; the result is multiplied by the tint's RGB and
//!   takes the tint's alpha.
//! - Vertex arrays are parallel (`positions[i]`, `uvs[i]`, `tints[i]` are one vertex); `indices`
//!   are triangles (three per triangle) into them. `weights` and `uvs1` are empty when the
//!   material does not use them.
//! - A batch only lives during the call that hands it out: its arrays are reused for the next one.
//!   A texture is the same picture as long as its [`Tex::id`] is the same, across batches, blocks
//!   and threads (an id is never given twice): upload once, look up by id.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::formats::resource::joaat;
use crate::formats::ymap::rotate;
use crate::render::assets::{Assets, Draw, MASK, TERRAIN, TINT_BYTE};
use crate::render::raster::{Image, Mode};
use crate::util::profile::{Phase, time};

/// A decoded texture and its identity.
#[derive(Clone)]
pub struct Tex {
    /// Unique to this decoded picture for the whole run, never reused: the key of a renderer's
    /// own copy of it (a GPU texture). The CPU raster reads the picture itself and has no use for it.
    #[allow(dead_code)]
    pub id: u64,
    pub image: Arc<Image>,
}

impl Tex {
    pub fn new(image: Image) -> Tex {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Tex { id: NEXT.fetch_add(1, Ordering::Relaxed), image: Arc::new(image) }
    }
}

/// The blend mask of a terrain: a texture read at the second coordinates, and which of its
/// channels (0 red … 3 alpha) mix layers 0-1 and 2-3, then both results.
#[derive(Clone)]
pub struct Mask {
    pub texture: Tex,
    pub channels: (usize, usize),
}

/// What a batch is painted with.
#[derive(Clone)]
pub enum Material {
    /// One texture, or none: the tint alone.
    Single(Option<Tex>),
    /// Terrain: four layers (None: mid grey) mixed by the mask when there is one, else by the
    /// vertices' `weights`.
    Layers { layers: [Option<Tex>; 4], mask: Option<Mask> },
}

/// One piece of geometry to draw: the vertices of one model piece placed in the world, its
/// triangles, how to paint them.
pub struct Batch<'a> {
    pub mode: Mode,
    pub material: Material,
    /// World positions (metres; x east, y north, z up).
    pub positions: &'a [[f32; 3]],
    /// First texture coordinates.
    pub uvs: &'a [[f32; 2]],
    /// Colour multiplier (RGB) and alpha.
    pub tints: &'a [[f32; 4]],
    /// Weights of the four layers ([`Material::Layers`]; empty otherwise).
    pub weights: &'a [[f32; 4]],
    /// Second texture coordinates, of the mask ([`Material::Layers`] with a mask; empty otherwise).
    pub uvs1: &'a [[f32; 2]],
    /// Triangles: three indices each.
    pub indices: &'a [u16],
}

/// How the list is built.
pub struct Options {
    /// Entities are turned by the inverse of their stored quaternion (the validated sense).
    pub inverse: bool,
    /// Multiplies the models' own LOD distances when picking a detail level.
    pub lod_scale: f32,
    /// World rectangle (x1, y1, x2, y2) outside which nothing can be seen: a piece of geometry
    /// wholly outside is left out (and its textures are not even loaded). None: everything.
    pub clip: Option<[f32; 4]>,
}

/// A texture coordinate that is not a number (some models store 0xFFC00000 where a vertex does
/// not use the coordinate: the mask coordinates of the fields of Grapeseed) reads the texel at 0,
/// as the game's card does with it, instead of a colour that is not a number (a black pixel).
fn finite(uv: [f32; 2]) -> [f32; 2] {
    uv.map(|c| if c.is_finite() { c } else { 0.0 })
}

/// The vertex arrays of the batch being built, reused from one batch to the next.
#[derive(Default)]
struct Arrays {
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    tints: Vec<[f32; 4]>,
    weights: Vec<[f32; 4]>,
    uvs1: Vec<[f32; 2]>,
}

/// Hands every batch of `draws` to `sink`, in drawing order: opaque, then cut out, then blended
/// (decals). Returns (triangles, triangles per render bucket), geometry left out by
/// [`Options::clip`] included.
pub fn stream(
    draws: &[Draw],
    assets: &mut Assets,
    options: &Options,
    sink: &mut dyn FnMut(&Batch),
) -> (usize, [usize; 8]) {
    let mut triangles = 0usize;
    let mut by_bucket = [0usize; 8];
    let mut arrays = Arrays::default();
    // names of the texture parameters of a shader
    let (diffuse, palette, lookup) = (joaat("DiffuseSampler"), joaat("TintPaletteSampler"), joaat("lookupSampler"));
    let layer: [u32; 4] = [0, 1, 2, 3].map(|k| joaat(&format!("TextureSampler_layer{k}")));
    for pass in [Mode::Opaque, Mode::Cutout, Mode::Blend] {
        for draw in draws {
            let Some((model, d)) = time(Phase::Scene, || assets.drawable(&draw.archetype)) else { continue };
            // the detail level the distance calls for
            let level = (0..4)
                .find(|&l| !d.lods[l].is_empty() && draw.distance <= d.lod_dist[l] * options.lod_scale)
                .or_else(|| (0..4).rev().find(|&l| !d.lods[l].is_empty()));
            let Some(level) = level else { continue };
            let e = &draw.entity;
            let place = |p: &[f32; 3]| {
                let s = [p[0] * e.scale_xy, p[1] * e.scale_xy, p[2] * e.scale_z];
                let r = rotate(e.rotation, s, options.inverse);
                [r[0] + e.position[0], r[1] + e.position[1], r[2] + e.position[2]]
            };
            for g in d.lods[level].iter().flat_map(|m| &m.geometries) {
                let Some(shader) = d.shaders.get(g.shader) else { continue };
                // water: the surface of a river, a pool, a fountain, when the tiles ask for it
                // (`water`: its own tint, no texture); foam and decals are left out
                let water = if shader.bucket == 6 && crate::world::water::MESHES.load(Ordering::Relaxed) {
                    crate::world::water::mesh(shader.name)
                } else {
                    None
                };
                let mode = match shader.bucket {
                    0 | 4 | 5 => Mode::Opaque,
                    3 => Mode::Cutout,
                    6 if water.is_some() => Mode::Blend,
                    6 => continue,
                    _ => Mode::Blend,
                };
                if mode != pass {
                    continue;
                }
                by_bucket[shader.bucket.min(7) as usize] += g.indices.len() / 3;
                triangles += g.indices.len() / 3;
                let ready = time(Phase::Scene, || {
                    // world positions; a piece wholly outside the view is left out
                    arrays.positions.clear();
                    arrays.positions.extend(g.positions.iter().map(place));
                    if let Some(clip) = options.clip {
                        let (mut lo, mut hi) = ([f32::INFINITY; 2], [f32::NEG_INFINITY; 2]);
                        let mut odd = false;
                        for p in &arrays.positions {
                            for k in 0..2 {
                                lo[k] = lo[k].min(p[k]);
                                hi[k] = hi[k].max(p[k]);
                                odd |= !p[k].is_finite();
                            }
                        }
                        // a coordinate that is not a number is left to the renderer, as before
                        if !odd && (hi[0] < clip[0] || lo[0] > clip[2] || hi[1] < clip[1] || lo[1] > clip[3]) {
                            return None;
                        }
                    }
                    if let Some(tint) = water {
                        crate::world::water::note(shader.name);
                        let n = g.positions.len();
                        arrays.uvs.clear();
                        arrays.uvs.resize(n, [0.0, 0.0]);
                        arrays.tints.clear();
                        arrays.tints.resize(n, tint);
                        arrays.weights.clear();
                        arrays.uvs1.clear();
                        return Some(Material::Single(None));
                    }
                    let param = |hash: u32| shader.textures.iter().find(|(p, _)| *p == hash).map(|(_, t)| t.as_str());
                    let mut texture =
                        |name: &str| assets.image(name, model, &d.textures, draw.archetype.texture_dictionary);
                    let name = param(diffuse)
                        .or_else(|| param(layer[0]))
                        .or_else(|| shader.textures.first().map(|(_, t)| t.as_str()));
                    let image = name.and_then(&mut texture);
                    let grey = if image.is_none() { 0.5 } else { 1.0 };
                    // tinted shaders: the colour is multiplied by a palette entry, its column given by
                    // a vertex colour byte, its row by the entity's tint
                    let palette = param(palette).and_then(&mut texture);
                    // terrain: four layers mixed by a vertex colour (two channels: a lerp between
                    // layers 0-1 and 2-3, then between both)
                    let layers: Option<[Option<Tex>; 4]> =
                        param(layer[1]).is_some().then(|| layer.map(|l| param(l).and_then(&mut texture)));
                    let layered = layers.is_some() && TERRAIN.0 <= 1;
                    // a blend mask texture (lookupSampler) read with the second coordinates gives the
                    // weights per pixel instead
                    let mask =
                        if layers.is_none() || g.uvs1.is_empty() { None } else { param(lookup).and_then(&mut texture) };

                    let n = g.positions.len();
                    arrays.uvs.clear();
                    arrays.uvs.extend((0..n).map(|i| finite(g.uvs.get(i).copied().unwrap_or([0.0, 0.0]))));
                    arrays.tints.clear();
                    arrays.tints.extend((0..n).map(|i| {
                        let t: [f32; 3] = match (&palette, g.colours0.get(i)) {
                            (Some(p), Some(c)) => {
                                let p = &p.image;
                                let x = (c[TINT_BYTE] as usize).min(p.width - 1);
                                let y = (draw.entity.tint as usize).min(p.height - 1);
                                let o = (y * p.width + x) * 4;
                                [0, 1, 2].map(|k| p.rgba[o + k] as f32 / 255.0)
                            }
                            _ => [1.0, 1.0, 1.0],
                        };
                        let alpha = if mode == Mode::Blend {
                            g.colours0.get(i).map(|c| c[3] as f32 / 255.0).unwrap_or(1.0)
                        } else {
                            1.0
                        };
                        [grey * t[0], grey * t[1], grey * t[2], alpha]
                    }));
                    arrays.weights.clear();
                    arrays.uvs1.clear();
                    if layered {
                        let set = if TERRAIN.0 == 0 { &g.colours0 } else { &g.colours1 };
                        arrays.weights.extend((0..n).map(|i| {
                            let Some(c) = set.get(i) else { return [1.0, 0.0, 0.0, 0.0] };
                            let (a, b) = (c[TERRAIN.1] as f32 / 255.0, c[TERRAIN.2] as f32 / 255.0);
                            [(1.0 - a) * (1.0 - b), a * (1.0 - b), (1.0 - a) * b, a * b]
                        }));
                        if mask.is_some() {
                            arrays.uvs1.extend((0..n).map(|i| finite(g.uvs1.get(i).copied().unwrap_or([0.0, 0.0]))));
                        }
                    }
                    Some(match layers {
                        Some(layers) if layered => {
                            Material::Layers { layers, mask: mask.map(|texture| Mask { texture, channels: *MASK }) }
                        }
                        _ => Material::Single(image),
                    })
                });
                let Some(material) = ready else { continue };
                sink(&Batch {
                    mode,
                    material,
                    positions: &arrays.positions,
                    uvs: &arrays.uvs,
                    tints: &arrays.tints,
                    weights: &arrays.weights,
                    uvs1: &arrays.uvs1,
                    indices: &g.indices,
                });
            }
        }
    }
    (triangles, by_bucket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_that_are_not_numbers_read_texel_zero() {
        // the bytes 00 00 c0 ff of the fields of Grapeseed
        let nan = f32::from_le_bytes([0x00, 0x00, 0xc0, 0xff]);
        assert!(nan.is_nan());
        assert_eq!(finite([nan, nan]), [0.0, 0.0]);
        assert_eq!(finite([0.845, f32::INFINITY]), [0.845, 0.0]);
        assert_eq!(finite([-13.5, 1190.25]), [-13.5, 1190.25]);
    }
}
