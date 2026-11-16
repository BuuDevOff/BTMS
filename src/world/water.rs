//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! Water.
//!
//! The game has two kinds of water, and neither is a model of a placement file like the rest:
//!
//! - **the sea and the lakes** are flat pieces listed in `water.xml` (`WaterQuads`): rectangles
//!   and right triangles (`Type` 1 to 4, [`MISSING`]) at a height `z`: 0 for the sea, 30 for the
//!   Alamo Sea, 160 and 196 for the two reservoirs. Cayo Perico has its own file. A tile shows
//!   the water wherever the ground is below such a piece ([`Water::flood`]): the deeper, the more
//!   opaque and the darker, so the shore fades in. Where there is no ground at all under a
//!   piece (beyond the seabed), it is deep water, as long as that empty region touches water or
//!   the edge of the block: a pocket ringed by dry ground stays empty (a hole of the map is not a
//!   pond). The pieces of a file only cover a rectangle around the land ([`Water::areas`]):
//!   beyond those rectangles it is the open sea, at height 0, as in the game;
//! - **rivers, pools and fountains** are pieces of models with a water shader (render bucket 6):
//!   [`mesh`] tells the surfaces (drawn as blended water, [`MESHES`]) from the foam and the
//!   decals (left out).
//!
//! Nothing here is drawn in a "heights only" pass: the shading and the search for holes see the
//! ground, not the water.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use rage_rpf::GtaKeys;

use crate::formats::resource::joaat;

/// The files of the water pieces: the map's, then Cayo Perico's.
pub const FILES: [&str; 2] =
    ["common.rpf/data/levels/gta5/water.xml", "update/update.rpf/common/data/levels/gta5/water_heistisland.xml"];

/// Whether the water pieces of models are drawn (set by the tiles when `[water]` is on).
pub static MESHES: AtomicBool = AtomicBool::new(false);

/// A corner of a rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    NorthEast,
    SouthEast,
    SouthWest,
    NorthWest,
}

/// The corner a triangle of `Type` 1, 2, 3, 4 leaves out of its rectangle (the right angle is at
/// the opposite one). Not written anywhere in the game's data: settled by the ground itself
/// (for every triangle, the half that has the ground under the water).
pub const MISSING: [Corner; 4] = [Corner::NorthEast, Corner::SouthEast, Corner::SouthWest, Corner::NorthWest];

/// A piece of water of `water.xml`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub min: [f32; 2],
    pub max: [f32; 2],
    /// 0: the whole rectangle; 1 to 4: a right triangle ([`MISSING`]).
    pub kind: u8,
    pub z: f32,
}

impl Quad {
    /// Whether (x, y) is on the half of the rectangle that leaves `missing` out.
    pub fn half(&self, x: f32, y: f32, missing: Corner) -> bool {
        let u = (x - self.min[0]) / (self.max[0] - self.min[0]);
        let v = (y - self.min[1]) / (self.max[1] - self.min[1]);
        match missing {
            Corner::NorthEast => u + v <= 1.0,
            Corner::SouthEast => v >= u,
            Corner::SouthWest => u + v >= 1.0,
            Corner::NorthWest => v <= u,
        }
    }

    /// Whether (x, y) is on this piece.
    pub fn holds(&self, x: f32, y: f32) -> bool {
        if x < self.min[0] || x >= self.max[0] || y < self.min[1] || y >= self.max[1] {
            return false;
        }
        match self.kind {
            1..=4 => self.half(x, y, MISSING[self.kind as usize - 1]),
            _ => true,
        }
    }
}

/// The visible pieces of a `water.xml`.
pub fn parse(xml: &str) -> Vec<Quad> {
    let Some(start) = xml.find("<WaterQuads>") else { return Vec::new() };
    let end = xml[start..].find("</WaterQuads>").map(|e| start + e).unwrap_or(xml.len());
    xml[start..end]
        .split("<Item>")
        .skip(1)
        .filter_map(|item| {
            let value = |name: &str| -> Option<&str> {
                let rest = &item[item.find(&format!("<{name} "))?..];
                let from = rest.find("value=\"")? + 7;
                let to = rest[from..].find('"')?;
                Some(rest[from..from + to].trim())
            };
            let number = |name: &str| value(name)?.parse::<f32>().ok();
            if value("IsInvisible") == Some("true") {
                return None;
            }
            let quad = Quad {
                min: [number("minX")?, number("minY")?],
                max: [number("maxX")?, number("maxY")?],
                kind: number("Type")? as u8,
                z: number("z")?,
            };
            (quad.max[0] > quad.min[0] && quad.max[1] > quad.min[1]).then_some(quad)
        })
        .collect()
}

/// Colours of the water, as sRGB fractions (the chain on the card is given them: `chain.rs`).
pub(crate) const SHALLOW: [f32; 3] = [0.29, 0.62, 0.64];
pub(crate) const DEEP: [f32; 3] = [0.085, 0.225, 0.36];
/// Tint and opacity of a river's surface, of a pool's.
const RIVER: [f32; 4] = [0.20, 0.38, 0.40, 0.72];
const POOL: [f32; 4] = [0.30, 0.66, 0.78, 0.85];

/// Colour and opacity of water `depth` metres deep: nothing at the shore, nearly opaque from a
/// few metres, from the shallow colour to the deep one.
fn tone(depth: f32) -> ([f32; 3], f32) {
    let opacity = 0.94 * (1.0 - (-(depth + 0.15) / 2.5).exp());
    let k = 1.0 - (-depth / 12.0).exp();
    ([0, 1, 2].map(|i| SHALLOW[i] + (DEEP[i] - SHALLOW[i]) * k), opacity.clamp(0.0, 1.0))
}

const SURFACES: [&str; 6] =
    ["water_river", "water_riverlod", "water_riverocean", "water_rivershallow", "water_shallow", "water_mesh"];
const POOLS: [&str; 2] = ["water_poolenv", "water_fountain"];
const FROTH: [&str; 4] = ["water_riverfoam", "water_terrainfoam", "water_decal", "water_foam"];

fn named(shader: u32, names: &[&str]) -> bool {
    names.iter().any(|n| joaat(n) == shader || joaat(&format!("{n}.sps")) == shader)
}

/// What the piece of a model with this shader (render bucket 6) is drawn with: the tint and
/// opacity of a surface, or None for froth (foam, decals). A name that is not one of the game's
/// water shaders is drawn as a river.
pub fn mesh(shader: u32) -> Option<[f32; 4]> {
    if named(shader, &FROTH) {
        None
    } else if named(shader, &POOLS) {
        Some(POOL)
    } else {
        Some(RIVER)
    }
}

static SEEN: Mutex<BTreeMap<u32, usize>> = Mutex::new(BTreeMap::new());

/// Counts a piece drawn with this water shader ([`shaders`]).
pub fn note(shader: u32) {
    *SEEN.lock().unwrap().entry(shader).or_default() += 1;
}

/// The water shaders drawn since the start: (name hash, pieces).
pub fn shaders() -> Vec<(u32, usize)> {
    SEEN.lock().unwrap().iter().map(|(k, v)| (*k, *v)).collect()
}

/// The name of a water shader, when it is one of the game's.
pub fn shader_name(hash: u32) -> Option<&'static str> {
    SURFACES.iter().chain(&POOLS).chain(&FROTH).find(|n| named(hash, &[**n])).copied()
}

/// The pieces of water of the map.
#[derive(Default)]
pub struct Water {
    pub quads: Vec<Quad>,
    /// The rectangle (x1, y1, x2, y2) the pieces of each file cover: inside one, there is water
    /// only on a piece (the land is in there); outside all of them, it is the open sea.
    pub areas: Vec<[f32; 4]>,
}

/// Height of the open sea.
const SEA: f32 = 0.0;

impl Water {
    /// The pieces of the game's files ([`FILES`]); a file that cannot be read gives none.
    pub fn load(game: &Path, keys: &GtaKeys) -> Water {
        let mut water = Water::default();
        for path in FILES {
            if let Ok(bytes) = crate::cli::archive::read_file(game, path, keys) {
                water.add(parse(&String::from_utf8_lossy(&bytes)));
            }
        }
        water
    }

    /// Adds the pieces of one file, and the rectangle they cover.
    pub fn add(&mut self, quads: Vec<Quad>) {
        if quads.is_empty() {
            return;
        }
        let mut area = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
        for q in &quads {
            area = [area[0].min(q.min[0]), area[1].min(q.min[1]), area[2].max(q.max[0]), area[3].max(q.max[1])];
        }
        self.areas.push(area);
        self.quads.extend(quads);
    }

    /// Whether (x, y) is beyond the rectangles of every file: the open sea.
    fn open(&self, x: f32, y: f32) -> bool {
        !self.areas.iter().any(|a| x >= a[0] && x < a[2] && y >= a[1] && y < a[3])
    }

    /// The blocks of `side` metres a piece of water meets.
    pub fn blocks(&self, side: f32) -> HashSet<(i32, i32)> {
        let mut out = HashSet::new();
        for q in &self.quads {
            let (x0, x1) = ((q.min[0] / side).floor() as i32, ((q.max[0] - 1e-3) / side).floor() as i32);
            let (y0, y1) = ((q.min[1] / side).floor() as i32, ((q.max[1] - 1e-3) / side).floor() as i32);
            for by in y0..=y1 {
                for bx in x0..=x1 {
                    out.insert((bx, by));
                }
            }
        }
        out
    }

    /// The height of the water over each pixel of a square picture (row 0 its north edge; -inf
    /// where there is none): `west`, `north` its corner, `metres` per pixel, `px` pixels a side.
    /// None when no piece meets it.
    pub fn surface(&self, west: f32, north: f32, metres: f32, px: usize) -> Option<Vec<f32>> {
        if self.quads.is_empty() {
            return None;
        }
        let (east, south) = (west + px as f32 * metres, north - px as f32 * metres);
        // a picture wholly beyond every file's rectangle is all open sea; one that only reaches
        // beyond is open sea where it does
        let apart = |a: &[f32; 4]| east <= a[0] || west >= a[2] || north <= a[1] || south >= a[3];
        if self.areas.iter().all(apart) {
            return Some(vec![SEA; px * px]);
        }
        let within = self.areas.iter().any(|a| west >= a[0] && east <= a[2] && south >= a[1] && north <= a[3]);
        let mut out: Option<Vec<f32>> = None;
        if !within {
            let mut surface = vec![f32::NEG_INFINITY; px * px];
            for j in 0..px {
                let y = north - (j as f32 + 0.5) * metres;
                for i in 0..px {
                    if self.open(west + (i as f32 + 0.5) * metres, y) {
                        surface[j * px + i] = SEA;
                    }
                }
            }
            out = Some(surface);
        }
        for q in
            self.quads.iter().filter(|q| q.min[0] < east && q.max[0] > west && q.min[1] < north && q.max[1] > south)
        {
            let surface = out.get_or_insert_with(|| vec![f32::NEG_INFINITY; px * px]);
            // the pixels whose centre can be on the piece
            let x0 = (((q.min[0] - west) / metres - 0.5).ceil().max(0.0)) as usize;
            let x1 = (((q.max[0] - west) / metres - 0.5).ceil().max(0.0) as usize).min(px);
            let y0 = (((north - q.max[1]) / metres - 0.5).floor().max(0.0)) as usize;
            let y1 = ((((north - q.min[1]) / metres - 0.5).floor() + 1.0).max(0.0) as usize).min(px);
            for j in y0..y1 {
                let y = north - (j as f32 + 0.5) * metres;
                for i in x0..x1 {
                    let x = west + (i as f32 + 0.5) * metres;
                    if q.holds(x, y) {
                        let s = &mut surface[j * px + i];
                        *s = s.max(q.z);
                    }
                }
            }
        }
        out
    }

    /// What [`Water::surface`] gives, said so that the card can make it without a float
    /// operation of its own (`chain.rs`): which pixels are the open sea (per file's rectangle, its
    /// columns and rows), and for each piece, per row, the pixels it holds. Every float is computed
    /// here, by the very expressions of `surface` (the pixel centres, `Quad::holds`), so that the
    /// card's surface is that one to the bit ([`Spans::surface`] is it, on the processor, for the
    /// tests). None exactly when `surface` gives None.
    pub fn spans(&self, west: f32, north: f32, metres: f32, px: usize) -> Option<Spans> {
        if self.quads.is_empty() {
            return None;
        }
        let (east, south) = (west + px as f32 * metres, north - px as f32 * metres);
        let apart = |a: &[f32; 4]| east <= a[0] || west >= a[2] || north <= a[1] || south >= a[3];
        if self.areas.iter().all(apart) {
            return Some(Spans {
                all_open: true,
                open_test: false,
                areas: Vec::new(),
                quads: Vec::new(),
                rows: Vec::new(),
            });
        }
        let within = self.areas.iter().any(|a| west >= a[0] && east <= a[2] && south >= a[1] && north <= a[3]);
        let column = |i: usize| west + (i as f32 + 0.5) * metres;
        let row = |j: usize| north - (j as f32 + 0.5) * metres;
        // the first index of 0..n where a test that is false, then true along the index, holds
        let first = |n: usize, test: &dyn Fn(usize) -> bool| -> usize {
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                if test(mid) { hi = mid } else { lo = mid + 1 }
            }
            lo
        };
        let mut spans =
            Spans { all_open: false, open_test: !within, areas: Vec::new(), quads: Vec::new(), rows: Vec::new() };
        if !within {
            // a pixel is in a file's rectangle when its column and its row are: x grows with the
            // column, y falls with the row, so each is a range
            for a in &self.areas {
                let (i0, i1) = (
                    first(px, &|i| column(i) >= a[0]),
                    first(px, &|i| column(i).partial_cmp(&a[2]).is_none_or(std::cmp::Ordering::is_ge)),
                );
                let (j0, j1) = (
                    first(px, &|j| row(j) < a[3]),
                    first(px, &|j| row(j).partial_cmp(&a[1]).is_none_or(std::cmp::Ordering::is_lt)),
                );
                spans.areas.push([i0 as u32, i1.max(i0) as u32, j0 as u32, j1.max(j0) as u32]);
            }
        }
        let mut any = !within;
        for q in
            self.quads.iter().filter(|q| q.min[0] < east && q.max[0] > west && q.min[1] < north && q.max[1] > south)
        {
            any = true;
            // the pixels whose centre can be on the piece, as `surface` takes them
            let x0 = (((q.min[0] - west) / metres - 0.5).ceil().max(0.0)) as usize;
            let x1 = (((q.max[0] - west) / metres - 0.5).ceil().max(0.0) as usize).min(px);
            let y0 = (((north - q.max[1]) / metres - 0.5).floor().max(0.0)) as usize;
            let y1 = ((((north - q.min[1]) / metres - 0.5).floor() + 1.0).max(0.0) as usize).min(px);
            let at = spans.rows.len() as u32;
            for j in y0..y1.max(y0) {
                let y = row(j);
                let n = x1.saturating_sub(x0);
                // along a row, `holds` is true on one range of the columns: x grows with the
                // column, and with it u = (x − min) / (max − min); a triangle keeps the columns on
                // one side of its diagonal
                let held = |k: usize| q.holds(column(x0 + k), y);
                let (from, to) = if y < q.min[1] || y >= q.max[1] {
                    (0, 0)
                } else {
                    let (mut from, mut to) = (
                        first(n, &|k| column(x0 + k) >= q.min[0]),
                        first(n, &|k| column(x0 + k).partial_cmp(&q.max[0]).is_none_or(std::cmp::Ordering::is_ge)),
                    );
                    if (1..=4).contains(&q.kind) {
                        let missing = MISSING[q.kind as usize - 1];
                        let half = |k: usize| q.half(column(x0 + k), y, missing);
                        match missing {
                            // u + v ≤ 1, v ≥ u: the columns west of the diagonal
                            Corner::NorthEast | Corner::SouthEast => to = to.min(first(n, &|k| !half(k))),
                            // u + v ≥ 1, v ≤ u: those east of it
                            Corner::SouthWest | Corner::NorthWest => from = from.max(first(n, &half)),
                        }
                    }
                    (from, to.max(from))
                };
                debug_assert!((from..to).all(held));
                spans.rows.push([(x0 + from) as u32, (x0 + to) as u32]);
            }
            spans.quads.push(SpanQuad { z: q.z, y0: y0 as u32, y1: y1.max(y0) as u32, at });
        }
        any.then_some(spans)
    }

    /// Puts the sea and the lakes on the picture of a block (RGBA, `px` × `px`, row 0 north, its
    /// west and north edges at `west`, `north`): `depths` are the depths the block was drawn with
    /// (`top` − height; infinite where nothing is). Returns the pixels that got water.
    pub fn flood(
        &self,
        rgba: &mut [u8],
        depths: &[f32],
        top: f32,
        [west, north]: [f32; 2],
        metres: f32,
        px: usize,
    ) -> usize {
        let Some(surface) = self.surface(west, north, metres, px) else { return 0 };
        let ground = spread(depths, px, px, 4);
        // 0 dry, 1 water over the ground, 2 water piece with no ground under it, 3 that last made sea
        let mut state = vec![0u8; px * px];
        let mut stack = Vec::new();
        for i in 0..px * px {
            if surface[i] == f32::NEG_INFINITY {
                continue;
            }
            if !ground[i].is_finite() {
                state[i] = 2;
            } else if top - ground[i] < surface[i] - 0.02 {
                state[i] = 1;
            }
        }
        // an empty region under water is sea when it touches water or the edge of the block
        for i in 0..px * px {
            if state[i] != 2 {
                continue;
            }
            let (x, y) = (i % px, i / px);
            let edge = x == 0 || y == 0 || x + 1 == px || y + 1 == px;
            let wet = |j: usize| state[j] == 1;
            if edge || wet(i - 1) || wet(i + 1) || wet(i - px) || wet(i + px) {
                state[i] = 3;
                stack.push(i);
            }
        }
        while let Some(i) = stack.pop() {
            let (x, y) = (i % px, i / px);
            let mut visit = |j: usize| {
                if state[j] == 2 {
                    state[j] = 3;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < px {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - px);
            }
            if y + 1 < px {
                visit(i + px);
            }
        }
        let mut wet = 0;
        for i in 0..px * px {
            let p = &mut rgba[i * 4..i * 4 + 4];
            match state[i] {
                1 => {
                    let (colour, opacity) = tone(surface[i] - (top - ground[i]));
                    for k in 0..3 {
                        let under = if p[3] > 0 { p[k] as f32 / 255.0 } else { colour[k] };
                        p[k] = ((under + (colour[k] - under) * opacity).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    }
                    p[3] = 255;
                    wet += 1;
                }
                3 => {
                    for k in 0..3 {
                        p[k] = (DEEP[k] * 255.0 + 0.5) as u8;
                    }
                    p[3] = 255;
                    wet += 1;
                }
                _ => {}
            }
        }
        wet
    }
}

/// A piece of water over a block, for the card: its height, the rows of the picture it may hold
/// pixels on (`y0..y1`), and where the ranges of those rows start in [`Spans::rows`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpanQuad {
    pub z: f32,
    pub y0: u32,
    pub y1: u32,
    pub at: u32,
}

/// The water over a block's picture without a float operation left ([`Water::spans`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Spans {
    /// The whole picture is the open sea.
    pub all_open: bool,
    /// Pixels in none of the files' rectangles are the open sea (else none is).
    pub open_test: bool,
    /// Per file's rectangle: the columns `i0..i1` and the rows `j0..j1` it covers.
    pub areas: Vec<[u32; 4]>,
    pub quads: Vec<SpanQuad>,
    /// Per piece, per row from its `y0`: the columns `i0..i1` it holds.
    pub rows: Vec<[u32; 2]>,
}

impl Spans {
    /// The surface these spans give (what the card computes, here on the processor): the height
    /// of the water over each pixel, -inf where there is none.
    #[cfg(test)]
    pub fn surface(&self, px: usize) -> Vec<f32> {
        let mut out = vec![if self.all_open { SEA } else { f32::NEG_INFINITY }; px * px];
        if self.all_open {
            return out;
        }
        for j in 0..px {
            for i in 0..px {
                let s = &mut out[j * px + i];
                let (i, j) = (i as u32, j as u32);
                if self.open_test && !self.areas.iter().any(|a| i >= a[0] && i < a[1] && j >= a[2] && j < a[3]) {
                    *s = SEA;
                }
                for q in &self.quads {
                    if j >= q.y0 && j < q.y1 {
                        let r = self.rows[(q.at + j - q.y0) as usize];
                        if i >= r[0] && i < r[1] {
                            *s = s.max(q.z);
                        }
                    }
                }
            }
        }
        out
    }
}

/// The depths with the seams filled: each pass gives every pixel without a depth the mean of its
/// neighbours that have one (as the tiles fill the colours of the same seams).
pub(crate) fn spread(depths: &[f32], w: usize, h: usize, passes: usize) -> Vec<f32> {
    let mut out = depths.to_vec();
    for _ in 0..passes {
        let before = out.clone();
        let mut any = false;
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if before[i].is_finite() {
                    continue;
                }
                let (mut sum, mut n) = (0.0f32, 0u32);
                let mut add = |j: usize| {
                    if before[j].is_finite() {
                        sum += before[j];
                        n += 1;
                    }
                };
                if x > 0 {
                    add(i - 1);
                }
                if x + 1 < w {
                    add(i + 1);
                }
                if y > 0 {
                    add(i - w);
                }
                if y + 1 < h {
                    add(i + w);
                }
                if n > 0 {
                    out[i] = sum / n as f32;
                    any = true;
                }
            }
        }
        if !any {
            break;
        }
    }
    out
}

/// Turns the water pieces of models on or off ([`MESHES`]).
pub fn draw_meshes(on: bool) {
    MESHES.store(on, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0"?><WaterData><WaterQuads>
      <Item><minX value="0" /><maxX value="100" /><minY value="0" /><maxY value="50" /><Type value="0" /><IsInvisible value="false" /><z value="0.0" /></Item>
      <Item><minX value="100" /><maxX value="200" /><minY value="0" /><maxY value="100" /><Type value="1" /><IsInvisible value="false" /><z value="30.0" /></Item>
      <Item><minX value="0" /><maxX value="10" /><minY value="0" /><maxY value="10" /><Type value="0" /><IsInvisible value="true" /><z value="5.0" /></Item>
    </WaterQuads><CalmingQuads><Item><minX value="1" /></Item></CalmingQuads></WaterData>"#;

    #[test]
    fn pieces_of_a_file() {
        let quads = parse(XML);
        assert_eq!(quads.len(), 2);
        assert_eq!(quads[0], Quad { min: [0.0, 0.0], max: [100.0, 50.0], kind: 0, z: 0.0 });
        assert_eq!((quads[1].kind, quads[1].z), (1, 30.0));
        // the rectangle holds its whole area, the triangle of type 1 its south-west half
        assert!(quads[0].holds(99.0, 49.0) && !quads[0].holds(100.0, 10.0) && !quads[0].holds(50.0, 50.0));
        assert!(quads[1].holds(110.0, 10.0) && !quads[1].holds(190.0, 90.0));
        // each half leaves its own corner out
        let q = Quad { min: [0.0, 0.0], max: [10.0, 10.0], kind: 0, z: 0.0 };
        assert!(!q.half(9.0, 9.0, Corner::NorthEast) && q.half(1.0, 1.0, Corner::NorthEast));
        assert!(!q.half(9.0, 1.0, Corner::SouthEast) && q.half(1.0, 9.0, Corner::SouthEast));
        assert!(!q.half(1.0, 1.0, Corner::SouthWest) && q.half(9.0, 9.0, Corner::SouthWest));
        assert!(!q.half(1.0, 9.0, Corner::NorthWest) && q.half(9.0, 1.0, Corner::NorthWest));
    }

    #[test]
    fn water_where_the_ground_is_under_it() {
        // 8 × 8 pixels of 1 m over x 0..8, y 0..8: the sea (z 0) on the west half
        let mut water = Water::default();
        water.add(vec![
            Quad { min: [0.0, 0.0], max: [4.0, 8.0], kind: 0, z: 0.0 },
            Quad { min: [0.0, 0.0], max: [8.0, 8.0], kind: 0, z: -50.0 },
        ]);
        let top = 100.0;
        let px = 8;
        let mut depths = vec![f32::INFINITY; px * px];
        let mut rgba = vec![0u8; px * px * 4];
        for y in 0..px {
            for x in 0..px {
                let i = y * px + x;
                // column 0: no ground; column 1: 5 m under water; column 2: 1 m above; the rest dry land
                let height = match x {
                    0 => None,
                    1 => Some(-5.0),
                    _ => Some(1.0),
                };
                if let Some(z) = height {
                    depths[i] = top - z;
                    rgba[i * 4..i * 4 + 4].copy_from_slice(&[200, 180, 120, 255]);
                }
            }
        }
        // (the seams are filled first: column 0 takes the depth of column 1)
        let wet = water.flood(&mut rgba, &depths, top, [0.0, 8.0], 1.0, px);
        assert_eq!(wet, 2 * px);
        let pixel = |x: usize| &rgba[(3 * px + x) * 4..][..4];
        assert!(pixel(1)[2] > pixel(1)[0] && pixel(1)[3] == 255, "water over the ground is blue");
        assert_eq!(pixel(2), [200, 180, 120, 255], "the ground above the water is left");
        assert_eq!(pixel(5), [200, 180, 120, 255], "nothing happens outside the piece");
        // an empty pocket ringed by dry ground stays empty; one that touches water becomes sea
        let mut depths = vec![top - 1.0; px * px];
        let mut rgba = vec![255u8; px * px * 4];
        let pocket = 4 * px + 2;
        depths[pocket] = f32::INFINITY;
        rgba[pocket * 4 + 3] = 0;
        // (no spreading here: a pocket of one pixel is a seam, so make it 6 × 3)
        for y in 2..5 {
            for x in 1..3 {
                depths[y * px + x] = f32::INFINITY;
                rgba[(y * px + x) * 4 + 3] = 0;
            }
        }
        let before = rgba.clone();
        let mut far = Water::default();
        far.add(vec![Quad { min: [0.0, 0.0], max: [8.0, 8.0], kind: 0, z: 0.0 }]);
        // every pixel of the pocket is within 4 seams of the ground: it is filled as ground, dry
        assert_eq!(far.flood(&mut rgba, &depths, top, [0.0, 8.0], 1.0, px), 0);
        assert_eq!(rgba, before);
    }

    #[test]
    fn open_sea_beyond_the_pieces() {
        // one file whose pieces cover x 0..100, y 0..100, with water on its west half only
        let mut water = Water::default();
        water.add(vec![
            Quad { min: [0.0, 0.0], max: [50.0, 100.0], kind: 0, z: 30.0 },
            Quad { min: [50.0, 0.0], max: [100.0, 100.0], kind: 1, z: 30.0 },
        ]);
        assert_eq!(water.areas, [[0.0, 0.0, 100.0, 100.0]]);
        // a picture far away: all open sea, at height 0
        let far = water.surface(1000.0, 1000.0, 1.0, 4).unwrap();
        assert!(far.iter().all(|z| *z == 0.0));
        // a picture across the east edge (x 96..104, y 60..68): inside the rectangle the triangle
        // has no water there (north-east corner left out), beyond it the open sea
        let edge = water.surface(96.0, 68.0, 1.0, 8).unwrap();
        assert_eq!(edge[0], f32::NEG_INFINITY);
        assert_eq!(edge[7], 0.0);
        // a picture wholly inside the rectangle, on no piece's water: no water at any pixel
        assert!(water.surface(90.0, 99.0, 1.0, 4).is_none_or(|s| s.iter().all(|z| *z == f32::NEG_INFINITY)));
        // no file at all (water off): none anywhere
        assert!(Water::default().surface(1000.0, 1000.0, 1.0, 4).is_none());
    }

    /// The spans the card is given make `surface` to the bit: blocks inside a file's rectangle,
    /// across its edges, beyond every one, at random places and on a grid where the diagonals of the
    /// triangles go through the centres of pixels.
    #[test]
    fn the_spans_give_the_surface() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (px, metres) = (128usize, 0.25f32);
        let side = px as f32 * metres;
        let mut checked = (0usize, 0usize);
        for round in 0..60 {
            let mut water = Water::default();
            for _file in 0..1 + round % 2 {
                let mut quads = Vec::new();
                for _ in 0..8 {
                    // on the pixel grid (a quarter of the time), else anywhere
                    let grid = next() % 4 == 0;
                    let at = |v: u64, m: f32| {
                        if grid { (v % 200) as f32 * m + m / 2.0 - 50.0 } else { (v % 20_000) as f32 / 100.0 - 50.0 }
                    };
                    let (x, y) = (at(next(), metres), at(next(), metres));
                    let (w, h) = if grid {
                        let s = (1 + next() % 40) as f32 * metres * 4.0;
                        (s, s)
                    } else {
                        ((next() % 6000) as f32 / 100.0 + 0.5, (next() % 6000) as f32 / 100.0 + 0.5)
                    };
                    quads.push(Quad {
                        min: [x, y],
                        max: [x + w, y + h],
                        kind: (next() % 5) as u8,
                        z: (next() % 50) as f32 - 10.0,
                    });
                }
                water.add(quads);
            }
            for (west, north) in [
                (-20.0f32, 30.0f32),
                (0.0, side),
                (40.0, 100.0),
                (-60.0, -10.0),
                (1000.0, 1000.0),
                (((next() % 200) as f32) - 100.0, ((next() % 200) as f32) - 100.0),
            ] {
                let surface = water.surface(west, north, metres, px);
                let spans = water.spans(west, north, metres, px);
                assert_eq!(surface.is_some(), spans.is_some(), "round {round} at {west},{north}");
                if let (Some(surface), Some(spans)) = (surface, spans) {
                    let made = spans.surface(px);
                    let apart = surface.iter().zip(&made).filter(|(a, b)| a.to_bits() != b.to_bits()).count();
                    assert_eq!(apart, 0, "round {round} at {west},{north}: {apart} pixels apart");
                    checked.0 += 1;
                    checked.1 += surface.iter().filter(|s| **s > f32::NEG_INFINITY).count();
                }
            }
        }
        // (the test checks something: pictures with water, pixels under it)
        assert!(checked.0 > 100 && checked.1 > 100_000, "{checked:?}");
    }

    #[test]
    fn shaders_of_the_water() {
        assert_eq!(mesh(joaat("water_riverfoam")), None);
        assert_eq!(mesh(joaat("water_poolenv")), Some(POOL));
        assert_eq!(mesh(joaat("water_river")), Some(RIVER));
        assert_eq!(shader_name(joaat("water_fountain")), Some("water_fountain"));
        assert_eq!(shader_name(joaat("default")), None);
    }

    #[test]
    fn tone_of_the_water() {
        let (_, shore) = tone(0.0);
        let (deep, opaque) = tone(40.0);
        assert!(shore < 0.1 && opaque > 0.9);
        assert!(deep[2] > deep[0], "deep water is blue");
    }
}
