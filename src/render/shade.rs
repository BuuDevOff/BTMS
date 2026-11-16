//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! Shading of the pictures seen straight from above (the tiles), so that heights and buildings
//! can be seen: `[lighting]` of `map.toml`.
//!
//! The height of every pixel is known (an orthographic view looks down from a fixed height:
//! height = top − depth). From that height map, the colour of each pixel is multiplied by a factor
//! ([`factors`]) made of three things:
//!
//! - the slope: Lambert's law with the normal of the height map, relative to flat ground, which
//!   keeps its colour exactly; a step steeper than [`SLOPE_MAX`] is a wall, not a slope;
//! - cast shadows: a pixel is in shadow when something on the line toward the sun rises above the
//!   sun's ray, within the reach ([`REACH`] metres); beyond half the reach the shadow fades out,
//!   so that a tower's shadow ends softly instead of covering the district;
//! - the foot of walls: a light darkening where something rises steeply within [`FOOT_RADIUS`].
//!
//! One rule keeps the borders between the blocks of the tiles seamless: the factor of a pixel
//! depends only on the heights within a fixed distance of it, on grids fixed to the world (the
//! lines swept toward the sun, the coarse cells and patches are set by world pixel coordinates,
//! not by the picture). So the heights are needed around a picture too ([`Frame`]): toward the
//! sun over the reach of the shadows, over a few metres on the other sides. Two neighbouring
//! blocks then compute the same factors along their border.
//!
//! The picture itself is drawn exactly as without light; the heights around it go to a second
//! raster, for their height alone (no texture but for the cut-outs, nothing blended), and only
//! what matters is drawn there: everything within the few metres the wall feet read, and beyond
//! only what is high enough for its shadow to reach the picture.
//!
//! Foliage is shaded as a volume, not leaf by leaf (the leaves of a crown would shade one
//! another into darkness): where the heights are broken from one pixel to the next, the slope is
//! the canopy's and a leaf counts as high as the canopy around it. Elsewhere the slope of a pixel
//! gives way to the mean slope around it when it is close to it: the flat triangles of the ground
//! do not show, the ridge of a roof stays sharp.
//!
//! Shading comes before the holes of a tile are filled (`tiles/mod.rs`): a hole has no height,
//! and the filling copies the already shaded colours next to it.

use std::sync::LazyLock;

use anyhow::{Context, Result, bail};

use crate::config::Lighting;
use crate::render::raster::{Projection, Raster};

/// Longest cast shadow, in metres: how far toward the sun the heights are read around a picture.
/// At a sun height of 35° anything taller than 48 × tan 35° = 34 m casts a shadow cut short
/// (faded out) at 48 m; at 45°, anything taller than 48 m.
pub const REACH: f32 = 48.0;
/// Fraction of the reach after which a cast shadow fades out.
const SHADOW_FADE: f32 = 0.5;
/// Metres an occluder must rise above the sun's ray for a shadow to start, and to be full.
const SHADOW_SOFT: [f32; 2] = [0.03, 0.15];
/// Share of the light that comes straight from the sun at `strength = 1` (the rest is the sky's):
/// a full shadow keeps 1 − `strength` × this of the colour.
const DIRECT: f32 = 0.75;
/// Steepest slope (rise over run) shaded as a slope; a steeper step between two pixels is a wall
/// or an edge, and gives no slope.
const SLOPE_MAX: f32 = 3.0;
/// Most a slope facing the sun is lit, relative to flat ground.
const LAMBERT_MAX: f32 = 2.0;
/// Least the factor can be: nothing goes black.
const FLOOR: f32 = 0.2;
/// Pixels per side of a coarse cell (the wall feet are computed on the mean height of each cell).
const CELL: usize = 4;
/// How far from a wall its foot is darkened, in metres.
const FOOT_RADIUS: f32 = 3.0;
/// A wall darkens its foot where it rises above this slope from the pixel (1 = 45°).
const FOOT_SLOPE: f64 = 1.0;
/// Metres above that slope for the full effect of one direction.
const FOOT_FULL: f32 = 0.5;
/// Gain on the mean of the eight directions (a long straight wall fills three of them).
const FOOT_GAIN: f32 = 1.6;
/// Pixels per side of a patch: the cells of a second, coarser grid, for what is read at the
/// scale of a metre (the smooth slope of the ground, foliage).
const PATCH: usize = 8;
/// Patches averaged on each side of a patch for the smooth slope.
const SMOOTH_RADIUS: usize = 4;
/// Patches whose mean heights differ by more than this many metres are not averaged together:
/// there is a wall between them.
const SMOOTH_STEP: f32 = 1.0;
/// How far the slope of a pixel is from the smooth slope around it (rise over run): under the
/// first value the smooth slope is used (the flat triangles of the ground would show), over the
/// second the pixel's own (the ridge of a roof, a kerb).
const DETAIL: [f32; 2] = [0.15, 0.4];
/// Share of the steps between neighbouring pixels that are broken (an edge) around a patch:
/// under the first value nothing there is foliage, over the second everything is.
const ROUGH: [f32; 2] = [0.06, 0.18];
/// Metres a leaf may be under the canopy around it without being in its shadow: foliage is
/// shaded as a volume, not leaf by leaf.
const LIFT: f32 = 2.5;

/// The reach in use: [`REACH`], or `BTMS_SHADOW_REACH=<metres>` (to compare).
static REACH_IN_USE: LazyLock<f32> =
    LazyLock::new(|| env_metres("BTMS_SHADOW_REACH").unwrap_or(REACH).clamp(1.0, 512.0));
/// Metres added to every margin, `BTMS_SHADOW_MARGIN=<metres>`: a check that a picture's shading
/// does not depend on how far it was drawn (the result must not change).
static EXTRA_MARGIN: LazyLock<f32> =
    LazyLock::new(|| env_metres("BTMS_SHADOW_MARGIN").unwrap_or(0.0).clamp(0.0, 256.0));

/// `BTMS_SHADOW_DRAW_ALL=1`: everything is drawn in the margin, not only what is tall enough for
/// its shadow to reach the picture (a check: the result must not change).
static DRAW_ALL: LazyLock<bool> = LazyLock::new(|| std::env::var_os("BTMS_SHADOW_DRAW_ALL").is_some());

fn env_metres(name: &str) -> Option<f32> {
    std::env::var(name).ok().and_then(|v| v.trim().parse().ok()).filter(|v: &f32| v.is_finite())
}

/// The light of the run: `[lighting]`, None when it is off.
fn light() -> Option<&'static Lighting> {
    let l = &crate::config::get().lighting;
    l.enabled.then_some(l)
}

/// Whether the pictures are shaded.
pub fn lit() -> bool {
    light().is_some()
}

/// Command-line overrides of `[lighting]`, taken out of `args`: `--lighting on|off`,
/// `--no-lighting`, `--azimuth <deg>`, `--elevation <deg>`, `--strength <0-1>`,
/// `--shadows on|off`, `--ambient-occlusion <0-1>`.
pub fn overrides(args: &mut Vec<String>, light: &mut Lighting) -> Result<()> {
    let mut take = |flag: &str| -> Result<Option<String>> {
        let Some(i) = args.iter().position(|a| a == flag) else { return Ok(None) };
        let value = args.get(i + 1).cloned().with_context(|| format!("{flag} without a value"))?;
        args.drain(i..i + 2);
        Ok(Some(value))
    };
    let yes = |flag: &str, v: &str| -> Result<bool> {
        match v {
            "on" | "true" | "1" => Ok(true),
            "off" | "false" | "0" => Ok(false),
            _ => bail!("{flag}: \"on\" or \"off\" expected, not \"{v}\""),
        }
    };
    let number = |flag: &str, v: &str| -> Result<f32> {
        v.parse::<f32>()
            .ok()
            .filter(|x| x.is_finite())
            .with_context(|| format!("{flag}: a number expected, not \"{v}\""))
    };
    if let Some(v) = take("--lighting")? {
        light.enabled = yes("--lighting", &v)?;
    }
    if let Some(v) = take("--azimuth")? {
        light.azimuth = number("--azimuth", &v)?;
    }
    if let Some(v) = take("--elevation")? {
        light.elevation = number("--elevation", &v)?;
    }
    if let Some(v) = take("--strength")? {
        light.strength = number("--strength", &v)?;
    }
    if let Some(v) = take("--shadows")? {
        light.shadows = yes("--shadows", &v)?;
    }
    if let Some(v) = take("--ambient-occlusion")? {
        light.ambient_occlusion = number("--ambient-occlusion", &v)?;
    }
    if let Some(i) = args.iter().position(|a| a == "--no-lighting") {
        args.remove(i);
        light.enabled = false;
    }
    Ok(())
}

/// What the manifest of the tiles says of the light, None when it is off.
pub fn manifest() -> Option<serde_json::Value> {
    light().map(|l| {
        serde_json::json!({
            "azimuth": l.azimuth,
            "elevation": l.elevation,
            "strength": l.strength,
            "shadows": l.shadows,
            "ambient_occlusion": l.ambient_occlusion,
            "shadow_reach_m": *REACH_IN_USE,
        })
    })
}

/// The widest margin a picture of `metres` per pixel is drawn with, in metres (0 when the light
/// is off): how far around a zone the entities matter.
pub fn margin_metres(metres: f32) -> f32 {
    match light() {
        Some(l) => {
            margins(l, metres, *REACH_IN_USE).into_iter().max().unwrap_or(0) as f32 * metres
                + *EXTRA_MARGIN
                + PATCH as f32 * metres
        }
        None => 0.0,
    }
}

/// Toward the sun in a picture (x right = east, y down = south), a unit vector.
fn toward_sun(light: &Lighting) -> [f64; 2] {
    let azimuth = (light.azimuth as f64).to_radians();
    [azimuth.sin(), -azimuth.cos()]
}

/// Height of the sun above the horizon, in radians, kept within what a march can follow.
fn sun_height(light: &Lighting) -> f64 {
    (light.elevation as f64).clamp(5.0, 90.0).to_radians()
}

/// Whether shadows are cast (a sun at the zenith casts none straight down).
fn casts(light: &Lighting) -> bool {
    light.shadows && light.strength > 0.0 && light.elevation < 89.0
}

/// The lines a sweep follows through a picture: one pixel per step along the axis the light is
/// most aligned with, each step moving `slope` pixels across it.
struct Lines {
    /// The lines run along x (else along y).
    x_major: bool,
    slope: f64,
    /// Metres per step.
    step: f64,
    /// Steps within the reach.
    window: i64,
    /// The light is toward the growing coordinates of the main axis.
    toward_plus: bool,
}

fn lines(dir: [f64; 2], metres: f64, reach: f64) -> Lines {
    let x_major = dir[0].abs() >= dir[1].abs();
    let (along, across) = if x_major { (dir[0], dir[1]) } else { (dir[1], dir[0]) };
    let slope = across / along;
    let step = metres * (1.0 + slope * slope).sqrt();
    Lines { x_major, slope, step, window: (reach / step).floor() as i64, toward_plus: along > 0.0 }
}

/// The margin every side of a picture needs, in pixels: the patches of the smooth slope and
/// the radius of the wall feet.
fn pad(light: &Lighting, metres: f32) -> usize {
    // the patches: two for the weights between them, one for the slope, the radius of the mean
    let patches = (SMOOTH_RADIUS + 3) * PATCH + 2;
    if light.ambient_occlusion > 0.0 { patches.max((FOOT_RADIUS / metres).ceil() as usize + 2 * CELL) } else { patches }
}

/// The margins a picture needs, in pixels: left (west), top (north), right (east), bottom (south).
/// Toward the sun, the reach of the shadows; on every side, the radius of the wall feet.
pub fn margins(light: &Lighting, metres: f32, reach: f32) -> [usize; 4] {
    let mut m = [pad(light, metres); 4];
    if casts(light) {
        let dir = toward_sun(light);
        let l = lines(dir, metres as f64, reach as f64);
        // the steps of the window, the rounding of a line's offsets, the 3 × 3 softening
        let along = l.window as usize + 3;
        let across = (l.window as f64 * l.slope.abs()).ceil() as usize + 4;
        let (x, y) = if l.x_major { (along, across) } else { (across, along) };
        let side = if dir[0] > 0.0 { 2 } else { 0 };
        m[side] = m[side].max(x);
        let side = if dir[1] > 0.0 { 3 } else { 1 };
        m[side] = m[side].max(y);
    }
    m
}

/// The shading of a picture seen from above: the picture is drawn as without light, then the
/// heights around it are drawn in a second raster (the picture and its margins), in two passes,
/// and [`Frame::shade`] multiplies the picture's colours:
///
/// ```text
/// paint(&mut picture, …);                         // as without light
/// if let Some(frame) = Frame::new(centre, metres, width, height) {
///     let mut around = frame.heights(top);        // heights only, what is near the picture
///     paint(&mut around, the entities `frame.near` keeps, …);
///     let floor = frame.far(&picture, &mut around);   // then what is farther and tall enough
///     paint(&mut around, the entities `frame.reaches` keeps, …);
///     frame.shade(&mut picture, &around);
/// }
/// ```
pub struct Frame {
    light: Lighting,
    reach: f32,
    /// Size of the raster of heights: the picture and its margins.
    width: usize,
    height: usize,
    /// Centre of the raster of heights.
    centre: [f32; 2],
    metres: f32,
    /// The picture in the raster of heights: x, y, width, height.
    window: [usize; 4],
    /// World pixel coordinates (x east, y south, in pixels from the world's origin) of the first
    /// pixel of the raster of heights.
    origin: [i64; 2],
    /// What decides in which pass something is drawn, if at all.
    caster: Caster,
}

/// The two corners of the box of a triangle.
fn corners(t: &[[f32; 3]; 3]) -> ([f32; 3], [f32; 3]) {
    ([0, 1, 2].map(|k| t[0][k].min(t[1][k]).min(t[2][k])), [0, 1, 2].map(|k| t[0][k].max(t[1][k]).max(t[2][k])))
}

/// What decides whether something around a picture is worth drawing for its height.
#[derive(Clone, Copy)]
struct Caster {
    /// The rectangle of the world the picture covers: (west, south), (east, north).
    inner: ([f32; 2], [f32; 2]),
    /// A quarter of a pixel, in metres: what stays within the picture by that much covers none
    /// of the pixels around it.
    edge: f32,
    /// Metres around the picture within which everything is drawn.
    pad: f32,
    /// Shadows are cast.
    casts: bool,
    /// The way shadows travel in the world (east, north), a unit vector.
    away: [f64; 2],
    /// Metres a shadow comes down per metre travelled.
    fall: f64,
    /// Metres a shadow travels at most.
    reach: f64,
    /// Metres of safety: the lines of a sweep are not exactly straight.
    slack: f64,
}

impl Caster {
    fn new(light: &Lighting, reach: f32, metres: f32, inner: ([f32; 2], [f32; 2])) -> Caster {
        // toward the sun in a picture is (east, south): in the world, north is up
        let sun = toward_sun(light);
        Caster {
            inner,
            edge: 0.25 * metres,
            pad: (pad(light, metres) + 1) as f32 * metres,
            casts: casts(light),
            away: [-sun[0], sun[1]],
            fall: sun_height(light).tan(),
            reach: reach as f64,
            slack: 4.0 * metres as f64 + 0.05,
        }
    }

    /// The box lies wholly in the picture: the picture's own raster has all of it.
    fn inside(&self, lo: [f32; 3], hi: [f32; 3]) -> bool {
        (0..2).all(|k| lo[k] >= self.inner.0[k] - self.edge && hi[k] <= self.inner.1[k] + self.edge)
    }

    /// The box touches the picture or the few metres around it where everything is drawn.
    fn near(&self, lo: [f32; 3], hi: [f32; 3]) -> bool {
        (0..2).all(|k| lo[k] <= self.inner.1[k] + self.pad && hi[k] >= self.inner.0[k] - self.pad)
    }

    /// The box lies wholly in the picture and those few metres around it.
    fn close(&self, lo: [f32; 3], hi: [f32; 3]) -> bool {
        (0..2).all(|k| lo[k] >= self.inner.0[k] - self.pad && hi[k] <= self.inner.1[k] + self.pad)
    }

    /// How far a box must travel the way shadows go to meet a rectangle of the world, None when
    /// it cannot within the reach: each axis gives an interval.
    fn travel(&self, lo: [f32; 3], hi: [f32; 3], rect: ([f64; 2], [f64; 2])) -> Option<f64> {
        let (mut from, mut to) = (0.0f64, self.reach + self.slack);
        for k in 0..2 {
            let (a, b) = (rect.0[k] - self.slack - hi[k] as f64, rect.1[k] + self.slack - lo[k] as f64);
            if self.away[k].abs() < 1e-9 {
                if a > 0.0 || b < 0.0 {
                    return None;
                }
            } else {
                let (t0, t1) = (a / self.away[k], b / self.away[k]);
                from = from.max(t0.min(t1));
                to = to.min(t0.max(t1));
            }
        }
        (from <= to).then_some(from)
    }

    /// The shadow of something in the box can reach the picture, whose lowest points are `floor`:
    /// the box is toward the sun within the reach of a part of the picture, and its top above
    /// the sun's ray that comes down to the lowest point of that part at its nearest.
    fn reaches(&self, lo: [f32; 3], hi: [f32; 3], floor: &Floor) -> bool {
        if *DRAW_ALL || floor.lowest == f32::NEG_INFINITY {
            return true;
        }
        if !self.casts || !floor.lowest.is_finite() {
            return false;
        }
        let high = |from: f64, floor: f32| (hi[2] as f64 - floor as f64) > (from - self.slack).max(0.0) * self.fall;
        // the whole picture first: most of what is around fails here
        let whole =
            ([self.inner.0[0] as f64, self.inner.0[1] as f64], [self.inner.1[0] as f64, self.inner.1[1] as f64]);
        if !self.travel(lo, hi, whole).is_some_and(|from| high(from, floor.lowest)) {
            return false;
        }
        // then part by part: a pit at one end of the picture says nothing of the other end
        let side = [(whole.1[0] - whole.0[0]) / FLOORS as f64, (whole.1[1] - whole.0[1]) / FLOORS as f64];
        for (i, part) in floor.parts.iter().enumerate() {
            if !part.is_finite() || hi[2] <= *part {
                continue;
            }
            // the first row of parts is the north of the picture
            let (column, row) = ((i % FLOORS) as f64, (i / FLOORS) as f64);
            let rect = (
                [whole.0[0] + column * side[0], whole.1[1] - (row + 1.0) * side[1]],
                [whole.0[0] + (column + 1.0) * side[0], whole.1[1] - row * side[1]],
            );
            if self.travel(lo, hi, rect).is_some_and(|from| high(from, *part)) {
                return true;
            }
        }
        false
    }
}

/// A picture is cut in this many parts each way, for the lowest height drawn in each.
pub const FLOORS: usize = 8;

/// The lowest heights drawn in a picture: of the whole (infinite when nothing is drawn; minus
/// infinity when nothing is known, so that everything counts), and of each of its
/// [`FLOORS`] × [`FLOORS`] parts, row by row from the north (infinite where nothing is drawn).
#[derive(Clone, Copy)]
pub struct Floor {
    lowest: f32,
    parts: [f32; FLOORS * FLOORS],
}

impl Floor {
    fn empty() -> Floor {
        Floor { lowest: f32::INFINITY, parts: [f32::INFINITY; FLOORS * FLOORS] }
    }

    /// Counts a height drawn at the pixel (x, y) of a picture of `width` × `height` pixels; a
    /// pixel just outside counts for the nearest part.
    fn add(&mut self, x: isize, y: isize, width: usize, height: usize, h: f32) {
        let part = |p: isize, n: usize| (p.clamp(0, n as isize - 1) as usize * FLOORS / n).min(FLOORS - 1);
        self.lower(part(y, height) * FLOORS + part(x, width), h);
    }

    /// Counts a height drawn in a part (a height that is not finite: nothing drawn).
    fn lower(&mut self, part: usize, h: f32) {
        if h.is_finite() {
            self.parts[part] = self.parts[part].min(h);
            self.lowest = self.lowest.min(h);
        }
    }
}

impl Frame {
    /// The shading of a picture of `width` × `height` pixels of `metres`, centred on `centre`, as
    /// `[lighting]` says; None when the light is off.
    pub fn new(centre: [f32; 2], metres: f32, width: usize, height: usize) -> Option<Frame> {
        light().map(|l| Frame::with(l, *REACH_IN_USE, *EXTRA_MARGIN, centre, metres, width, height))
    }

    /// The same with a given light, reach of the shadows and extra margin (metres).
    pub fn with(
        light: &Lighting,
        reach: f32,
        extra: f32,
        centre: [f32; 2],
        metres: f32,
        width: usize,
        height: usize,
    ) -> Frame {
        let (half_w, half_h) = (width as f32 / 2.0 * metres, height as f32 / 2.0 * metres);
        let inner = ([centre[0] - half_w, centre[1] - half_h], [centre[0] + half_w, centre[1] + half_h]);
        let m = metres as f64;
        // world pixel of the picture's first pixel
        let x0 = ((centre[0] as f64 - width as f64 / 2.0 * m) / m).round() as i64;
        let y0 = ((-(centre[1] as f64) - height as f64 / 2.0 * m) / m).round() as i64;
        let extra = (extra / metres).ceil() as usize;
        let [mut left, mut top, mut right, mut bottom] = margins(light, metres, reach).map(|p| p + extra);
        // the raster of heights starts and ends on the world grid of the patches (and so of the cells)
        let patch = PATCH as i64;
        left += (x0 - left as i64).rem_euclid(patch) as usize;
        top += (y0 - top as i64).rem_euclid(patch) as usize;
        right += (PATCH - (left + width + right) % PATCH) % PATCH;
        bottom += (PATCH - (top + height + bottom) % PATCH) % PATCH;
        Frame {
            light: light.clone(),
            reach,
            width: left + width + right,
            height: top + height + bottom,
            // the top margin is to the north, the right one to the east
            centre: [
                centre[0] + (right as f32 - left as f32) / 2.0 * metres,
                centre[1] + (top as f32 - bottom as f32) / 2.0 * metres,
            ],
            metres,
            window: [left, top, width, height],
            origin: [x0 - left as i64, y0 - top as i64],
            caster: Caster::new(light, reach, metres, inner),
        }
    }

    /// The margins around the picture, in pixels: left (west), top (north), right, bottom.
    pub fn margin(&self) -> [usize; 4] {
        let [x, y, w, h] = self.window;
        [x, y, self.width - x - w, self.height - y - h]
    }

    /// The size of the raster of heights, in pixels.
    pub fn size(&self) -> [usize; 2] {
        [self.width, self.height]
    }

    /// The rectangle of the world the raster of heights covers: (west, south), (east, north).
    pub fn bounds(&self) -> ([f32; 2], [f32; 2]) {
        let (w, h) = (self.width as f32 / 2.0 * self.metres, self.height as f32 / 2.0 * self.metres);
        ([self.centre[0] - w, self.centre[1] - h], [self.centre[0] + w, self.centre[1] + h])
    }

    /// The raster of the heights around the picture, looking down from `top` as the picture does.
    /// Ready for its first pass: of what is given to it, it draws, for their height alone, the
    /// triangles that touch the few metres around the picture (the wall feet need them all) and
    /// are not wholly in the picture (the picture's own raster has those).
    pub fn heights(&self, top: f32) -> Raster {
        let mut raster = Raster::new(
            self.width,
            self.height,
            Projection::Orthographic { centre: self.centre, metres: self.metres, top },
            [0.0; 3],
        );
        let caster = self.caster;
        raster.set_pass(
            Some(Box::new(move |t: &[[f32; 3]; 3]| {
                let (lo, hi) = corners(t);
                !caster.inside(lo, hi) && caster.near(lo, hi)
            })),
            true,
        );
        raster
    }

    /// First pass: whether an entity, by its box in the world (its two corners), touches the
    /// picture or the few metres around it, and is not wholly in the picture (none of its
    /// triangles would be drawn).
    pub fn near(&self, lo: [f32; 3], hi: [f32; 3]) -> bool {
        self.caster.near(lo, hi) && !self.caster.inside(lo, hi)
    }

    /// Between the two passes: the lowest heights drawn in the picture (and in the two pixels
    /// around it); from now on the raster of heights draws the triangles farther than the first
    /// pass's whose shadow can reach the picture. What cannot is not worth drawing: a pixel that
    /// hides nothing changes no factor.
    pub fn far(&self, picture: &Raster, around: &mut Raster) -> Floor {
        let [x0, y0, w, h] = self.window;
        let mut floor = Floor::empty();
        match (picture.projection, around.projection) {
            (Projection::Orthographic { top, .. }, Projection::Orthographic { top: top_around, .. })
                if picture.width == w && picture.height == h =>
            {
                for (y, row) in picture.depths().chunks_exact(w).enumerate() {
                    for i in 0..FLOORS {
                        let deepest = row[i * w / FLOORS..(i + 1) * w / FLOORS]
                            .iter()
                            .filter(|d| d.is_finite())
                            .fold(f32::NEG_INFINITY, |a, d| a.max(*d));
                        floor.lower((y * FLOORS / h) * FLOORS + i, top - deepest);
                    }
                }
                // the two pixels around the picture, which the softening of the shadows reads
                let depths = around.depths();
                for y in y0.saturating_sub(2)..(y0 + h + 2).min(self.height) {
                    for x in x0.saturating_sub(2)..(x0 + w + 2).min(self.width) {
                        if x < x0 || x >= x0 + w || y < y0 || y >= y0 + h {
                            floor.add(
                                x as isize - x0 as isize,
                                y as isize - y0 as isize,
                                w,
                                h,
                                top_around - depths[y * self.width + x],
                            );
                        }
                    }
                }
            }
            // not the views from above this is made for: nothing is known, everything counts
            _ => floor.lowest = f32::NEG_INFINITY,
        }
        let caster = self.caster;
        around.set_pass(
            Some(Box::new(move |t: &[[f32; 3]; 3]| {
                let (lo, hi) = corners(t);
                !caster.near(lo, hi) && caster.reaches(lo, hi, &floor)
            })),
            true,
        );
        floor
    }

    /// [`Frame::far`] for a picture whose depths are not on the processor (the chain of a block
    /// on the card, `chain.rs`): `parts` are the lowest heights [`Frame::far`] would find in each
    /// part (row by row from the north; infinite where nothing is drawn), the card having looked
    /// at the same pixels.
    pub fn far_from(&self, parts: &[f32; FLOORS * FLOORS], around: &mut Raster) -> Floor {
        let mut floor = Floor::empty();
        for (part, h) in parts.iter().enumerate() {
            floor.lower(part, *h);
        }
        let caster = self.caster;
        around.set_pass(
            Some(Box::new(move |t: &[[f32; 3]; 3]| {
                let (lo, hi) = corners(t);
                !caster.near(lo, hi) && caster.reaches(lo, hi, &floor)
            })),
            true,
        );
        floor
    }

    /// What the shading of the chain needs (`chain.rs`): the light, the reach of the shadows, the
    /// metres of a pixel, the world pixel of the raster of heights' first pixel, and the picture
    /// in it (x, y, width, height).
    pub fn shading(&self) -> (&Lighting, f32, f32, [i64; 2], [usize; 4]) {
        (&self.light, self.reach, self.metres, self.origin, self.window)
    }

    /// Second pass: whether the shadow of an entity, by its box in the world, can reach the
    /// picture, whose lowest heights are `floor` ([`Frame::far`]).
    pub fn reaches(&self, lo: [f32; 3], hi: [f32; 3], floor: &Floor) -> bool {
        // wholly within the few metres around the picture: the first pass drew all of it
        !self.caster.close(lo, hi) && self.caster.reaches(lo, hi, floor)
    }

    /// Shades the picture: its colours multiplied by the factors of the height map, the picture's
    /// own heights in the middle of those drawn around it.
    pub fn shade(&self, picture: &mut Raster, around: &Raster) {
        let [x0, y0, w, h] = self.window;
        let (Projection::Orthographic { top, .. }, Projection::Orthographic { top: top_around, .. }) =
            (picture.projection, around.projection);
        if picture.width != w || picture.height != h || around.width != self.width || around.height != self.height {
            return;
        }
        // nothing drawn: an infinite depth, so a height that is not finite
        let mut heights: Vec<f32> = around.depths().iter().map(|d| top_around - d).collect();
        for (y, row) in picture.depths().chunks_exact(w).enumerate() {
            for (to, d) in heights[(y0 + y) * self.width + x0..][..w].iter_mut().zip(row) {
                *to = top - d;
            }
        }
        // on the card when it shades (`--shading`), else on the processor
        let factor = crate::render::gpu::shade::factors(
            &heights,
            self.width,
            self.height,
            self.origin,
            self.metres,
            &self.light,
            self.reach,
            self.window,
        )
        .unwrap_or_else(|| {
            factors(&heights, self.width, self.height, self.origin, self.metres, &self.light, self.reach, self.window)
        });
        for (c, f) in picture.colours_mut().iter_mut().zip(&factor) {
            if *f != 1.0 {
                *c = c.map(|v| v * f);
            }
        }
    }
}

/// The factor each pixel of `window` (x, y, width, height) is multiplied by, from the height map
/// of a picture of `width` × `height` pixels of `metres` (a height that is not finite: nothing
/// there, the factor is 1). `origin` is the world pixel of the picture's first pixel. The window
/// must lie within the picture by the [`margins`] for the result not to depend on the picture.
#[allow(clippy::too_many_arguments)]
pub fn factors(
    heights: &[f32],
    width: usize,
    height: usize,
    origin: [i64; 2],
    metres: f32,
    light: &Lighting,
    reach: f32,
    window: [usize; 4],
) -> Vec<f32> {
    let [wx, wy, ww, wh] = window;
    let dir = toward_sun(light);
    let sun = sun_height(light);
    let strength = light.strength.clamp(0.0, 1.0) * DIRECT;
    let foot_strength = light.ambient_occlusion.clamp(0.0, 1.0);

    // the coarse grids are only read around the window: by the margin every side has
    let around = pad(light, metres);
    let region = [
        wx.saturating_sub(around) / PATCH * PATCH,
        wy.saturating_sub(around) / PATCH * PATCH,
        ((wx + ww + around).div_ceil(PATCH) * PATCH).min(width),
        ((wy + wh + around).div_ceil(PATCH) * PATCH).min(height),
    ];
    let relief = (strength > 0.0).then(|| Relief::new(heights, width, height, metres, region));
    let shadow = casts(light).then(|| {
        let mut amount = vec![0.0f32; width * height];
        let cast = Sweep { dir, rise: sun.tan(), reach: reach as f64, soft: SHADOW_SOFT, fade_from: SHADOW_FADE };
        sweep(heights, width, height, origin, metres as f64, &cast, relief.as_ref(), &mut amount);
        amount
    });
    let foot = (foot_strength > 0.0).then(|| Foot::new(heights, width, height, origin, metres, region));
    // where a pixel falls among the cells of a coarse grid: two cells and the weight of the second
    let coarse = |p: usize, cells: usize, size: usize| -> (usize, usize, f32) {
        let f = (p as f32 + 0.5) / size as f32 - 0.5;
        let a = f.floor();
        ((a.max(0.0) as usize).min(cells - 1), ((a + 1.0).max(0.0) as usize).min(cells - 1), f - a)
    };
    let columns: Vec<(usize, usize, f32)> = match &foot {
        Some(f) => (0..ww).map(|x| coarse(wx + x, f.width, CELL)).collect(),
        None => Vec::new(),
    };
    let patch_columns: Vec<(usize, usize, f32)> = match &relief {
        Some(r) => (0..ww).map(|x| coarse(wx + x, r.width, PATCH)).collect(),
        None => Vec::new(),
    };
    // the coarse grids along the row of pixels being shaded: their two rows of cells around it
    // mixed, once per row
    let span = |columns: &[(usize, usize, f32)]| match (columns.first(), columns.last()) {
        (Some(first), Some(last)) => first.0..last.1 + 1,
        _ => 0..0,
    };
    let (cell_span, patch_span) = (span(&columns), span(&patch_columns));
    let mut hemmed_row = vec![0.0f32; foot.as_ref().map_or(0, |f| f.width)];
    let mut relief_row = vec![[0.0f32; 3]; relief.as_ref().map_or(0, |r| r.width)];

    // Lambert relative to flat ground: (n · l) / l.z with n = (−gx, −gy, 1) normalised
    let (kx, ky) = ((dir[0] * sun.cos() / sun.sin()) as f32, (dir[1] * sun.cos() / sun.sin()) as f32);
    let limit = SLOPE_MAX * metres;
    let at = |x: isize, y: isize| -> f32 {
        if x < 0 || y < 0 || x >= width as isize || y >= height as isize {
            f32::NAN
        } else {
            heights[y as usize * width + x as usize]
        }
    };
    let mut out = vec![1.0f32; ww * wh];
    for y in 0..wh {
        let py = wy + y;
        if let Some(f) = &foot {
            let (ya, yb, ty) = coarse(py, f.height, CELL);
            for c in cell_span.clone() {
                let (a, b) = (f.hemmed[ya * f.width + c], f.hemmed[yb * f.width + c]);
                hemmed_row[c] = a + ty * (b - a);
            }
        }
        if let Some(r) = &relief {
            let (ya, yb, ty) = coarse(py, r.height, PATCH);
            for c in patch_span.clone() {
                let (a, b) = (r.slope[ya * r.width + c], r.slope[yb * r.width + c]);
                let (ra, rb) = (r.rough[ya * r.width + c], r.rough[yb * r.width + c]);
                relief_row[c] = [a[0] + ty * (b[0] - a[0]), a[1] + ty * (b[1] - a[1]), ra + ty * (rb - ra)];
            }
        }
        for x in 0..ww {
            let px = wx + x;
            let i = py * width + px;
            let h = heights[i];
            if !h.is_finite() {
                continue;
            }
            let inside = px > 0 && px + 1 < width && py > 0 && py + 1 < height;
            let mut factor = 1.0f32;
            if strength > 0.0 {
                let (west, east, north, south) = if inside {
                    (heights[i - 1], heights[i + 1], heights[i - width], heights[i + width])
                } else {
                    let (x, y) = (px as isize, py as isize);
                    (at(x - 1, y), at(x + 1, y), at(x, y - 1), at(x, y + 1))
                };
                let mut gx = gradient(h - west, east - h, limit) / metres;
                let mut gy = gradient(h - north, south - h, limit) / metres;
                if relief.is_some() {
                    // the smooth slope around, but where the pixel's own stands out of it (and is
                    // not a leaf's): a ridge, a kerb, a step
                    let (xa, xb, tx) = patch_columns[x];
                    let (a, b) = (relief_row[xa], relief_row[xb]);
                    let (sx, sy, rough) =
                        (a[0] + tx * (b[0] - a[0]), a[1] + tx * (b[1] - a[1]), a[2] + tx * (b[2] - a[2]));
                    let (dx, dy) = (gx - sx, gy - sy);
                    let apart = dx * dx + dy * dy;
                    let own = if apart <= DETAIL[0] * DETAIL[0] {
                        0.0
                    } else {
                        smooth_step((apart.sqrt() - DETAIL[0]) / (DETAIL[1] - DETAIL[0])) * (1.0 - rough)
                    };
                    gx = sx + own * dx;
                    gy = sy + own * dy;
                }
                let lambert = if gx == 0.0 && gy == 0.0 {
                    1.0
                } else {
                    ((1.0 - gx * kx - gy * ky) / (1.0 + gx * gx + gy * gy).sqrt()).clamp(0.0, LAMBERT_MAX)
                };
                let hidden = match &shadow {
                    Some(s) if inside => {
                        // softened over 3 × 3 pixels: the lines of a sweep are one pixel wide
                        let (a, b, c) = (i - width, i, i + width);
                        (s[a - 1]
                            + s[a + 1]
                            + s[c - 1]
                            + s[c + 1]
                            + 2.0 * (s[a] + s[c] + s[b - 1] + s[b + 1])
                            + 4.0 * s[b])
                            / 16.0
                    }
                    Some(s) => s[i],
                    None => 0.0,
                };
                factor = 1.0 + strength * (lambert * (1.0 - hidden) - 1.0);
            }
            if let Some(f) = &foot {
                let (xa, xb, tx) = columns[x];
                let hemmed = hemmed_row[xa] + tx * (hemmed_row[xb] - hemmed_row[xa]);
                if hemmed > 0.0 {
                    // only below what stands around: the edge of a roof is not at the foot of its own wall
                    let below = ((f.highest[(py / CELL) * f.width + px / CELL] - h) / FOOT_FULL).clamp(0.0, 1.0);
                    factor *= 1.0 - foot_strength * hemmed * below;
                }
            }
            out[y * ww + x] = factor.max(FLOOR);
        }
    }
    out
}

/// The slope at a pixel from the steps to it and from it (metres per pixel): their mean, or the
/// one that is not a wall, or none.
fn gradient(before: f32, after: f32, limit: f32) -> f32 {
    // a step to nothing is infinite or NaN: not within the limit either
    match (before.abs() <= limit, after.abs() <= limit) {
        (true, true) => (before + after) * 0.5,
        (true, false) => before,
        (false, true) => after,
        (false, false) => 0.0,
    }
}

/// A sweep of a height map along the lines toward a light.
struct Sweep {
    /// Toward the light in the picture (x right, y down).
    dir: [f64; 2],
    /// Metres the light's ray rises per metre travelled.
    rise: f64,
    /// How far an occluder counts, in metres.
    reach: f64,
    /// Metres an occluder rises above the ray: where it starts to count, where it counts fully.
    soft: [f32; 2],
    /// Fraction of the reach after which an occluder counts less and less.
    fade_from: f32,
}

/// For every pixel with a height, how much something toward the light hides it (0 to 1), written
/// in `out` (untouched elsewhere).
///
/// Along a line, walking away from the light, a pixel `p` is hidden by an earlier one `q` when
/// `h(q) − rise · (p − q) > h(p)`, that is `g(q) > g(p)` with `g = h + rise · position`: a maximum
/// of `g` over the window of the reach, kept in a queue of falling values (constant time per
/// pixel). Two occluders are weighed: the highest above the ray and the nearest. With `relief`,
/// a pixel of foliage is hidden as if it were as high as the canopy around it (up to [`LIFT`]
/// metres higher): the leaves of a crown do not shade one another.
#[allow(clippy::too_many_arguments)]
fn sweep(
    heights: &[f32],
    width: usize,
    height: usize,
    origin: [i64; 2],
    metres: f64,
    s: &Sweep,
    relief: Option<&Relief>,
    out: &mut [f32],
) {
    let l = lines(s.dir, metres, s.reach);
    if l.window < 1 {
        return;
    }
    // `a` runs along the main axis of the lines, `b` across it
    let (la, lb, sa, sb, oa, ob) = if l.x_major {
        (width, height, 1, width, origin[0], origin[1])
    } else {
        (height, width, width, 1, origin[1], origin[0])
    };
    let rise = l.step * s.rise;
    // offset across for each position along, from world coordinates: every picture cuts the same lines
    let offsets: Vec<i64> = (0..la).map(|a| ((oa + a as i64) as f64 * l.slope).round() as i64).collect();
    let (low, high) = offsets.iter().fold((i64::MAX, i64::MIN), |(lo, hi), &o| (lo.min(o), hi.max(o)));
    let fade: Vec<f32> = (0..=l.window)
        .map(|d| {
            let t = ((d as f32 / l.window as f32 - s.fade_from) / (1.0 - s.fade_from)).clamp(0.0, 1.0);
            1.0 - t * t * (3.0 - 2.0 * t)
        })
        .collect();
    let soft = |excess: f64| ((excess as f32 - s.soft[0]) / (s.soft[1] - s.soft[0])).clamp(0.0, 1.0);
    // the queue: positions and values of g, falling from head to tail
    let (mut qt, mut qg) = (vec![0i64; la], vec![0f64; la]);
    // a line: the pixels whose world row minus the offset of their column is `line`
    for line in (ob - high)..(ob + lb as i64 - low) {
        let (mut head, mut tail) = (0usize, 0usize);
        for k in 0..la {
            let a = if l.toward_plus { la - 1 - k } else { k };
            let b = line - ob + offsets[a];
            if b < 0 || b >= lb as i64 {
                continue;
            }
            let i = a * sa + b as usize * sb;
            let h = heights[i];
            if !h.is_finite() {
                continue;
            }
            // position along the walk, growing away from the light, in world pixels
            let t = if l.toward_plus { -(oa + a as i64) } else { oa + a as i64 };
            let g = h as f64 + t as f64 * rise;
            // what hides the pixel must rise above `seen`: its own height, or the canopy's
            let seen = match relief {
                Some(r) => {
                    let (x, y) = if l.x_major { (a, b as usize) } else { (b as usize, a) };
                    let patch = (y / PATCH) * r.width + x / PATCH;
                    g + (r.rough[patch] * (r.canopy[patch] - h).clamp(0.0, LIFT)) as f64
                }
                None => g,
            };
            while head < tail && qt[head] < t - l.window {
                head += 1;
            }
            let mut amount = 0.0f32;
            if head < tail {
                amount = soft(qg[head] - seen) * fade[(t - qt[head]) as usize];
            }
            while tail > head && qg[tail - 1] <= g {
                tail -= 1;
            }
            if head < tail {
                amount = amount.max(soft(qg[tail - 1] - seen) * fade[(t - qt[tail - 1]) as usize]);
            }
            qt[tail] = t;
            qg[tail] = g;
            tail += 1;
            out[i] = amount;
        }
    }
}

/// 0 under 0, 1 over 1, smooth between.
fn smooth_step(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// What is read at the scale of a metre, on patches of [`PATCH`] pixels: the smooth slope of the
/// ground and of the roofs, and where there is foliage.
struct Relief {
    /// Patches per row and per column.
    width: usize,
    height: usize,
    /// The smooth slope around each patch (rise over run, toward x and y): the mean slope of the
    /// pixels around; in foliage, the slope of the canopy.
    slope: Vec<[f32; 2]>,
    /// How much each patch is foliage: 0 to 1, from how broken the heights are around it.
    rough: Vec<f32>,
    /// The highest level of the patches within a metre of each patch.
    canopy: Vec<f32>,
}

impl Relief {
    /// Of the patches of `region` (pixels: x0, y0, x1, y1, on the grid of the patches).
    fn new(heights: &[f32], width: usize, height: usize, metres: f32, region: [usize; 4]) -> Relief {
        let (pw, ph) = (width.div_ceil(PATCH), height.div_ceil(PATCH));
        let limit = SLOPE_MAX * metres;
        // per patch: the lowest and the highest of what is drawn, how many pixels; and of the
        // steps to the next pixel (east, south), the sum of those that are a slope and the
        // number of those that are broken (a wall, an edge)
        let mut low = vec![f32::INFINITY; pw * ph];
        let mut high = vec![f32::NEG_INFINITY; pw * ph];
        let mut count = vec![0u32; pw * ph];
        let mut steps = vec![[0.0f32; 2]; pw * ph];
        let mut whole = vec![[0u32; 2]; pw * ph];
        let mut broken = vec![0u32; pw * ph];
        for y in region[1]..region[3] {
            let row = &heights[y * width..(y + 1) * width];
            let below = (y + 1 < height).then(|| &heights[(y + 1) * width..(y + 2) * width]);
            let patches = (y / PATCH) * pw;
            for x in region[0]..region[2] {
                let h = row[x];
                if !h.is_finite() {
                    continue;
                }
                let p = patches + x / PATCH;
                low[p] = low[p].min(h);
                high[p] = high[p].max(h);
                count[p] += 1;
                // a step to nothing is neither a slope nor a broken step
                let mut step = |k: usize, to: f32| {
                    let d = to - h;
                    if d.abs() <= limit {
                        steps[p][k] += d;
                        whole[p][k] += 1;
                    } else if to.is_finite() {
                        broken[p] += 1;
                    }
                };
                if x + 1 < width {
                    step(0, row[x + 1]);
                }
                if let Some(below) = below {
                    step(1, below[x]);
                }
            }
        }
        // the level of a patch: the mean height of its pixels; when they are spread over more
        // than SMOOTH_STEP (an edge runs through it, or leaves), of those on the more crowded
        // side of the middle
        let mut level = vec![f32::NAN; pw * ph];
        for py in region[1] / PATCH..region[3].div_ceil(PATCH) {
            for px in region[0] / PATCH..region[2].div_ceil(PATCH) {
                let p = py * pw + px;
                if count[p] == 0 {
                    continue;
                }
                let (middle, split) = ((low[p] + high[p]) / 2.0, high[p] - low[p] > SMOOTH_STEP);
                let (mut under, mut over, mut n_under, mut n_over) = (0.0f32, 0.0f32, 0u32, 0u32);
                for y in py * PATCH..((py + 1) * PATCH).min(height) {
                    for h in &heights[y * width + px * PATCH..y * width + ((px + 1) * PATCH).min(width)] {
                        if !h.is_finite() {
                        } else if split && *h > middle {
                            over += h;
                            n_over += 1;
                        } else {
                            under += h;
                            n_under += 1;
                        }
                    }
                }
                level[p] = if n_over > n_under { over / n_over as f32 } else { under / n_under as f32 };
            }
        }
        // the mean slope of the pixels of each patch (the steps that are slopes: a wall adds
        // nothing), the mean level, and the share of broken steps
        let own: Vec<[f32; 3]> = (0..pw * ph)
            .map(|p| [0, 1].map(|k| if whole[p][k] > 0 { steps[p][k] / whole[p][k] as f32 / metres } else { 0.0 }))
            .zip(&level)
            .map(|(g, l)| [g[0], g[1], *l])
            .collect();
        let share: Vec<f32> =
            broken.iter().zip(&count).map(|(b, n)| if *n > 0 { *b as f32 / (2 * *n) as f32 } else { 0.0 }).collect();

        // averaged over the patches around, along x then along y; a patch on another level
        // (beyond a wall) is left out
        let average = |src: &[[f32; 3]], dst: &mut [[f32; 3]], stride: usize, len: usize, start: usize| {
            for k in 0..len {
                let centre = level[start + k * stride];
                if !centre.is_finite() {
                    continue;
                }
                let (mut total, mut n) = ([0.0f32; 3], 0u32);
                for j in k.saturating_sub(SMOOTH_RADIUS)..(k + SMOOTH_RADIUS + 1).min(len) {
                    if (level[start + j * stride] - centre).abs() <= SMOOTH_STEP {
                        let v = src[start + j * stride];
                        total = [total[0] + v[0], total[1] + v[1], total[2] + v[2]];
                        n += 1;
                    }
                }
                dst[start + k * stride] = total.map(|t| t / n as f32);
            }
        };
        let mut along = vec![[0.0f32, 0.0, f32::NAN]; pw * ph];
        for y in 0..ph {
            average(&own, &mut along, 1, pw, y * pw);
        }
        let mut smooth = vec![[0.0f32, 0.0, f32::NAN]; pw * ph];
        for x in 0..pw {
            average(&along, &mut smooth, pw, ph, x);
        }
        let patch_metres = metres * PATCH as f32;
        let wall = SLOPE_MAX * patch_metres;
        let surface = |x: isize, y: isize| {
            if x < 0 || y < 0 || x >= pw as isize || y >= ph as isize {
                f32::NAN
            } else {
                smooth[y as usize * pw + x as usize][2]
            }
        };
        let mut slope = vec![[0.0f32; 2]; pw * ph];
        let mut rough = vec![0.0f32; pw * ph];
        for y in 0..ph as isize {
            for x in 0..pw as isize {
                let p = y as usize * pw + x as usize;
                // the share of broken steps over the patch and those around it: foliage
                let (mut total, mut n) = (0.0f32, 0u32);
                for j in (y - 1).max(0)..(y + 2).min(ph as isize) {
                    for i in (x - 1).max(0)..(x + 2).min(pw as isize) {
                        total += share[j as usize * pw + i as usize];
                        n += 1;
                    }
                }
                rough[p] = smooth_step((total / n as f32 - ROUGH[0]) / (ROUGH[1] - ROUGH[0]));
                let [gx, gy, h] = smooth[p];
                if !h.is_finite() {
                    continue;
                }
                slope[p] = [gx, gy];
                if rough[p] > 0.0 {
                    // foliage: the slope of the canopy, read on the averaged levels (the slopes
                    // of single leaves say nothing of it)
                    let canopy = [
                        gradient(h - surface(x - 1, y), surface(x + 1, y) - h, wall) / patch_metres,
                        gradient(h - surface(x, y - 1), surface(x, y + 1) - h, wall) / patch_metres,
                    ]
                    .map(|g| if g.abs() < 1e-4 { 0.0 } else { g });
                    slope[p] = [gx + rough[p] * (canopy[0] - gx), gy + rough[p] * (canopy[1] - gy)];
                }
            }
        }
        let radius = (1.0 / patch_metres).ceil() as usize;
        Relief { width: pw, height: ph, slope, rough, canopy: highest(&level, pw, ph, radius) }
    }
}

/// The darkening at the foot of walls, on coarse cells of [`CELL`] pixels.
struct Foot {
    /// Cells per row and per column.
    width: usize,
    height: usize,
    /// How much each cell is hemmed in by walls, 0 to 1.
    hemmed: Vec<f32>,
    /// The highest cell within the radius of each cell.
    highest: Vec<f32>,
}

impl Foot {
    /// Of the cells of `region` (pixels: x0, y0, x1, y1, on the grid of the cells).
    fn new(heights: &[f32], width: usize, height: usize, origin: [i64; 2], metres: f32, region: [usize; 4]) -> Foot {
        let (cw, ch) = (width.div_ceil(CELL), height.div_ceil(CELL));
        // mean height of what is drawn in each cell
        let mut sum = vec![0.0f32; cw * ch];
        let mut count = vec![0u32; cw * ch];
        for y in region[1]..region[3] {
            let row = (y / CELL) * cw;
            for x in region[0]..region[2] {
                let h = heights[y * width + x];
                if h.is_finite() {
                    sum[row + x / CELL] += h;
                    count[row + x / CELL] += 1;
                }
            }
        }
        let mean: Vec<f32> =
            sum.iter().zip(&count).map(|(s, n)| if *n > 0 { s / *n as f32 } else { f32::NAN }).collect();
        // from eight directions: does something rise above the slope within the radius?
        let cell_origin = [origin[0].div_euclid(CELL as i64), origin[1].div_euclid(CELL as i64)];
        let cell_metres = (metres * CELL as f32) as f64;
        let mut hemmed = vec![0.0f32; cw * ch];
        let mut one = vec![0.0f32; cw * ch];
        for dir in
            [[1.0, 0.0], [-1.0, 0.0], [0.0, 1.0], [0.0, -1.0], [1.0, 1.0], [1.0, -1.0], [-1.0, 1.0], [-1.0, -1.0]]
        {
            one.fill(0.0);
            let wall =
                Sweep { dir, rise: FOOT_SLOPE, reach: FOOT_RADIUS as f64, soft: [0.0, FOOT_FULL], fade_from: 0.0 };
            sweep(&mean, cw, ch, cell_origin, cell_metres, &wall, None, &mut one);
            for (h, o) in hemmed.iter_mut().zip(&one) {
                *h += o;
            }
        }
        for h in &mut hemmed {
            *h = (*h * FOOT_GAIN / 8.0).min(1.0);
        }
        let radius = (FOOT_RADIUS as f64 / cell_metres).ceil() as usize;
        Foot { width: cw, height: ch, highest: highest(&mean, cw, ch, radius), hemmed }
    }
}

/// The highest value within `radius` cells of each cell (a square), cells without data left out.
fn highest(values: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    let clean: Vec<f32> = values.iter().map(|v| if v.is_finite() { *v } else { f32::NEG_INFINITY }).collect();
    let mut rows = vec![f32::NEG_INFINITY; width * height];
    let mut queue = Vec::new();
    for y in 0..height {
        sliding_max(&clean, &mut rows, y * width, 1, width, radius, &mut queue);
    }
    let mut out = vec![f32::NEG_INFINITY; width * height];
    for x in 0..width {
        sliding_max(&rows, &mut out, x, width, height, radius, &mut queue);
    }
    out
}

/// `dst[k]` = the highest of `src[k − radius ..= k + radius]` along a line of `len` values
/// starting at `start`, `stride` apart.
fn sliding_max(
    src: &[f32],
    dst: &mut [f32],
    start: usize,
    stride: usize,
    len: usize,
    radius: usize,
    queue: &mut Vec<usize>,
) {
    queue.clear();
    let mut head = 0;
    let value = |k: usize| src[start + k * stride];
    for j in 0..len + radius {
        if j < len {
            while queue.len() > head && value(queue[queue.len() - 1]) <= value(j) {
                queue.pop();
            }
            queue.push(j);
        }
        if j >= radius {
            let k = j - radius;
            while queue[head] + radius < k {
                head += 1;
            }
            dst[start + k * stride] = value(queue[head]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sun(azimuth: f32, elevation: f32) -> Lighting {
        Lighting { enabled: true, azimuth, elevation, strength: 0.6, shadows: true, ambient_occlusion: 0.0 }
    }

    /// A flat field of `w` × `h` pixels with a box of `top` metres on [x0, x1) × [y0, y1).
    fn field(w: usize, h: usize, boxes: &[([usize; 4], f32)]) -> Vec<f32> {
        let mut f = vec![0.0f32; w * h];
        for ([x0, y0, x1, y1], top) in boxes {
            for y in *y0..*y1 {
                for x in *x0..*x1 {
                    f[y * w + x] = *top;
                }
            }
        }
        f
    }

    #[test]
    fn flat_ground_keeps_its_colour() {
        let h = vec![12.5f32; 64 * 48];
        let light = Lighting { ambient_occlusion: 0.3, ..sun(315.0, 35.0) };
        let f = factors(&h, 64, 48, [0, 0], 0.25, &light, REACH, [0, 0, 64, 48]);
        assert!(f.iter().all(|v| *v == 1.0));
    }

    #[test]
    fn nothing_drawn_is_left_alone() {
        let mut h = field(40, 40, &[([10, 10, 20, 20], 8.0)]);
        for i in [0, 41, 455, 1599] {
            h[i] = f32::NEG_INFINITY;
        }
        let light = Lighting { ambient_occlusion: 0.3, ..sun(315.0, 35.0) };
        let f = factors(&h, 40, 40, [0, 0], 0.5, &light, REACH, [0, 0, 40, 40]);
        for i in [0, 41, 455, 1599] {
            assert_eq!(f[i], 1.0);
        }
        assert!(f.iter().all(|v| v.is_finite() && *v >= FLOOR));
    }

    #[test]
    fn a_box_casts_its_shadow_away_from_the_sun() {
        // 1 m per pixel, a box of 10 m on columns 40..44, rows 40..44; the sun in the west at 45°:
        // the shadow goes 10 m to the east
        let h = field(100, 100, &[([40, 40, 44, 44], 10.0)]);
        let f = factors(&h, 100, 100, [0, 0], 1.0, &sun(270.0, 45.0), REACH, [0, 0, 100, 100]);
        let dark = 1.0 - 0.6 * DIRECT;
        let at = |x: usize, y: usize| f[y * 100 + x];
        assert!((at(48, 42) - dark).abs() < 1e-4, "in the shadow: {}", at(48, 42));
        assert_eq!(at(58, 42), 1.0, "beyond the shadow");
        assert_eq!(at(30, 42), 1.0, "on the sun's side");
        assert_eq!(at(48, 30), 1.0, "north of the shadow");
        assert_eq!(at(48, 54), 1.0, "south of the shadow");
        assert_eq!(at(42, 42), 1.0, "the roof");
        // the sun in the north-west: the shadow goes to the south-east (right and down)
        let f = factors(&h, 100, 100, [0, 0], 1.0, &sun(315.0, 45.0), REACH, [0, 0, 100, 100]);
        let at = |x: usize, y: usize| f[y * 100 + x];
        assert!(at(48, 48) < 0.8, "to the south-east: {}", at(48, 48));
        assert_eq!(at(36, 36), 1.0, "to the north-west");
        assert_eq!(at(48, 36), 1.0, "to the north-east");
        assert_eq!(at(36, 48), 1.0, "to the south-west");
    }

    #[test]
    fn a_tower_s_shadow_stops_at_the_reach() {
        // 1 m per pixel, a tower of 300 m; the sun in the west at 35°: 428 m of shadow, cut at the reach
        let h = field(200, 20, &[([10, 5, 20, 15], 300.0)]);
        let f = factors(&h, 200, 20, [0, 0], 1.0, &sun(270.0, 35.0), REACH, [0, 0, 200, 20]);
        let at = |x: usize| f[10 * 200 + x];
        let dark = 1.0 - 0.6 * DIRECT;
        assert!((at(19 + (REACH * SHADOW_FADE) as usize - 1) - dark).abs() < 1e-4, "full shadow up to half the reach");
        let fading = at(19 + (REACH * 0.8) as usize);
        assert!(fading > dark + 0.05 && fading < 0.95, "the shadow fading out: {fading}");
        assert_eq!(at(19 + REACH as usize + 2), 1.0, "no shadow left beyond the reach");
    }

    #[test]
    fn slopes_face_the_sun_or_not() {
        // a roof ridge running north-south, 1 in 2 on both sides; the sun in the west
        let (w, hgt) = (60, 20);
        let mut h = vec![0.0f32; w * hgt];
        for y in 0..hgt {
            for x in 0..w {
                h[y * w + x] = 10.0 - (x as f32 - 30.0).abs() * 0.5;
            }
        }
        let light = Lighting { shadows: false, ..sun(270.0, 45.0) };
        let f = factors(&h, w, hgt, [0, 0], 1.0, &light, REACH, [0, 0, w, hgt]);
        let (west, east) = (f[10 * w + 20], f[10 * w + 40]);
        assert!(west > 1.05 && east < 0.95, "west side {west}, east side {east}");
        // a wall is not a slope: nothing but flat ground and a flat roof here
        let h = field(40, 40, &[([10, 10, 30, 30], 20.0)]);
        let f = factors(&h, 40, 40, [0, 0], 1.0, &light, REACH, [0, 0, 40, 40]);
        assert!(f.iter().all(|v| *v == 1.0));
    }

    #[test]
    fn the_foot_of_a_wall_is_darker_but_not_its_roof() {
        // 6.25 cm per pixel (the tiles), a wall of 6 m along x = 160 (10 m); no sun effect
        let (w, hgt) = (240, 96);
        let h = field(w, hgt, &[([160, 0, 240, 96], 6.0)]);
        let light = Lighting { strength: 0.0, ambient_occlusion: 0.5, ..sun(315.0, 35.0) };
        let f = factors(&h, w, hgt, [0, 0], 0.0625, &light, REACH, [0, 0, w, hgt]);
        let at = |x: usize| f[48 * w + x];
        assert!(at(158) < 0.8, "at the foot of the wall: {}", at(158));
        assert!(
            at(158) < at(150) && at(150) < at(140) && at(140) < 1.0,
            "less and less dark farther away: {} {} {}",
            at(158),
            at(150),
            at(140)
        );
        assert_eq!(at(80), 1.0, "5 m from the wall");
        assert_eq!(at(161), 1.0, "the edge of the roof");
        assert_eq!(at(200), 1.0, "the roof");
    }

    /// A field with boxes, a slope, holes: its heights from world pixel coordinates.
    fn world_height(x: i64, y: i64) -> f32 {
        let (fx, fy) = (x as f32, y as f32);
        let mut h = 20.0 + 0.08 * fx + 6.0 * (fy * 0.05).sin();
        for (cx, cy, half, top) in [
            (-70i64, 30i64, 9i64, 25.0f32),
            (40, -20, 14, 60.0),
            (150, 90, 6, 9.0),
            (10, 120, 20, 300.0),
            (-140, -60, 11, 14.0),
            (200, -40, 3, 30.0),
        ] {
            if (x - cx).abs() <= half && (y - cy).abs() <= half {
                h += top;
            }
        }
        // a patch of leaves: broken heights, from one pixel to the next
        if (x + 90).abs() <= 25 && (y - 60).abs() <= 20 {
            h += 4.0 + (x * 31 + y * 17).rem_euclid(7) as f32 * 0.7;
        }
        if (x * 7 + y * 13).rem_euclid(97) == 0 {
            return f32::NEG_INFINITY;
        }
        h
    }

    /// The factors of the world rectangle [x0, x0 + w) × [y0, y0 + h), computed in a picture that
    /// holds it with its margins and `extra` pixels more.
    #[allow(clippy::too_many_arguments)]
    fn shaded(
        light: &Lighting,
        metres: f32,
        reach: f32,
        x0: i64,
        y0: i64,
        w: usize,
        h: usize,
        extra: usize,
    ) -> Vec<f32> {
        let [left, top, right, bottom] = margins(light, metres, reach).map(|m| m + extra);
        // on the grid of the patches, as a frame is
        let left = left + (x0 - left as i64).rem_euclid(PATCH as i64) as usize;
        let top = top + (y0 - top as i64).rem_euclid(PATCH as i64) as usize;
        let (pw, ph) = (left + w + right, top + h + bottom);
        let origin = [x0 - left as i64, y0 - top as i64];
        let heights: Vec<f32> =
            (0..pw * ph).map(|i| world_height(origin[0] + (i % pw) as i64, origin[1] + (i / pw) as i64)).collect();
        factors(&heights, pw, ph, origin, metres, light, reach, [left, top, w, h])
    }

    #[test]
    fn blocks_shaded_apart_match_the_whole() {
        // whatever the sun: the factors of a rectangle do not depend on the picture they are
        // computed in, so two blocks meet without a seam
        for (azimuth, elevation) in [(315.0, 35.0), (200.0, 20.0), (90.0, 60.0), (10.0, 45.0)] {
            let light = Lighting { ambient_occlusion: 0.3, ..sun(azimuth, elevation) };
            let (metres, reach) = (0.5, 24.0);
            let (x0, y0, w, h) = (-160i64, -96i64, 320usize, 192usize);
            let whole = shaded(&light, metres, reach, x0, y0, w, h, 0);
            assert!(whole.iter().any(|v| *v < 0.7) && whole.contains(&1.0));
            // a wider picture changes nothing
            assert_eq!(whole, shaded(&light, metres, reach, x0, y0, w, h, 40), "azimuth {azimuth}");
            // four blocks, each with its own margins
            let (hw, hh) = (w / 2, h / 2);
            for (bx, by) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let part = shaded(&light, metres, reach, x0 + (bx * hw) as i64, y0 + (by * hh) as i64, hw, hh, 0);
                for y in 0..hh {
                    for x in 0..hw {
                        assert_eq!(
                            part[y * hw + x],
                            whole[(by * hh + y) * w + bx * hw + x],
                            "azimuth {azimuth}, block {bx},{by}, pixel {x},{y}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn margins_go_toward_the_sun() {
        let light = sun(315.0, 35.0);
        let [left, top, right, bottom] = margins(&light, 0.0625, 48.0);
        // 48 m along the diagonal: 34 m, 543 pixels, to the west and to the north
        assert!((543..=550).contains(&left) && (543..=550).contains(&top), "{left} {top}");
        let pad = (SMOOTH_RADIUS + 3) * PATCH + 2;
        assert_eq!((right, bottom), (pad, pad));
        let [left, top, right, bottom] = margins(&sun(90.0, 35.0), 0.0625, 48.0);
        assert!(
            (768..=772).contains(&right) && left == pad && top == pad && bottom == pad,
            "{left} {top} {right} {bottom}"
        );
        // a frame: its raster of heights on the grid of the patches, the picture where it says
        let f =
            Frame::with(&Lighting { ambient_occlusion: 0.3, ..light }, 48.0, 0.0, [448.0, -960.0], 0.0625, 2048, 2048);
        let [left, top, right, bottom] = f.margin();
        assert_eq!(
            (
                f.width % PATCH,
                f.height % PATCH,
                f.origin[0].rem_euclid(PATCH as i64),
                f.origin[1].rem_euclid(PATCH as i64)
            ),
            (0, 0, 0, 0)
        );
        assert_eq!(f.origin, [(384.0 / 0.0625) as i64 - left as i64, (896.0 / 0.0625) as i64 - top as i64]);
        assert_eq!(f.size(), [left + 2048 + right, top + 2048 + bottom]);
        let (lo, hi) = f.bounds();
        assert!(
            (lo[0] - (384.0 - left as f32 * 0.0625)).abs() < 1e-3
                && (hi[0] - (512.0 + right as f32 * 0.0625)).abs() < 1e-3
        );
        assert!(
            (lo[1] - (-1024.0 - bottom as f32 * 0.0625)).abs() < 1e-3
                && (hi[1] - (-896.0 + top as f32 * 0.0625)).abs() < 1e-3
        );
    }

    #[test]
    fn a_picture_is_shaded_from_the_heights_around_it() {
        use crate::render::raster::{Mode, Vertex};
        let light = Lighting { ambient_occlusion: 0.0, ..sun(270.0, 45.0) };
        // the tiles' 6.25 cm per pixel, a picture of 32 × 32 pixels (x 0 to 2 m, y −2 to 0), shadows of 12 m at most
        let frame = Frame::with(&light, 12.0, 0.0, [1.0, -1.0], 0.0625, 32, 32);
        let [left, top, _, _] = frame.margin();
        assert!(left >= 192 && top >= 58);
        let mut picture =
            Raster::new(32, 32, Projection::Orthographic { centre: [1.0, -1.0], metres: 0.0625, top: 100.0 }, [0.0; 3]);
        let mut around = frame.heights(100.0);
        let v = |x: f32, y: f32, z: f32| Vertex {
            pos: [x, y, z],
            uv: [0.0; 2],
            tint: [0.8, 0.8, 0.8, 1.0],
            weights: [1.0, 0.0, 0.0, 0.0],
            uv1: [0.0; 2],
        };
        let quad = |raster: &mut Raster, [x0, y0, x1, y1, z]: [f32; 5]| {
            raster.triangle([v(x0, y0, z), v(x1, y0, z), v(x1, y1, z)], None, Mode::Opaque);
            raster.triangle([v(x0, y0, z), v(x1, y1, z), v(x0, y1, z)], None, Mode::Opaque);
        };
        let quads = [
            // the ground at 5 m, under the picture and far to the west, but for a strip left empty in the picture
            [-20.0, -1.875, 3.0, 1.0, 5.0],
            [-20.0, -3.0, 3.0, -1.9375, 5.0],
            // 5 m to the west, a box 20 m above the ground: far from the picture, tall enough
            [-6.0, -1.5, -5.0, -0.5, 25.0],
            // farther, a low wall: its shadow cannot reach the picture
            [-8.0, -1.4, -7.0, -0.6, 5.5],
            // in the picture, a small box
            [1.0, -1.0, 1.25, -0.75, 6.0],
        ];
        // the picture as without light; around it, what is near, then what is far and tall enough
        for q in quads {
            quad(&mut picture, q);
            quad(&mut around, q);
        }
        let plain = picture.rgba();
        let floor = frame.far(&picture, &mut around);
        // the ground everywhere, but in the part the small box fills
        assert!((floor.lowest - 5.0).abs() < 1e-3, "the lowest point: {}", floor.lowest);
        assert!(
            floor.parts.iter().enumerate().all(|(i, p)| (p - if i == 3 * FLOORS + 4 { 6.0 } else { 5.0 }).abs() < 1e-3),
            "{:?}",
            floor.parts
        );
        for q in quads {
            quad(&mut around, q);
        }
        let height = |x: f32, y: f32| {
            let (lo, hi) = frame.bounds();
            let (px, py) = (((x - lo[0]) / 0.0625) as usize, ((hi[1] - y) / 0.0625) as usize);
            100.0 - around.depths()[py * frame.width + px]
        };
        assert!((height(-5.5, -1.0) - 25.0).abs() < 1e-3, "the tall box, far away, is drawn");
        assert!((height(-7.5, -1.0) - 5.0).abs() < 1e-3, "the low wall, farther, is not: the ground alone");
        assert!((height(-1.0, -1.0) - 5.0).abs() < 1e-3, "the ground near the picture is");
        assert!(
            (height(1.1, -0.9) - 5.0).abs() < 1e-3,
            "what fits in the picture is not (the picture has it): the ground alone"
        );
        frame.shade(&mut picture, &around);
        let shaded = picture.rgba();
        let px = |rgba: &[u8], x: usize, y: usize| [rgba[(y * 32 + x) * 4], rgba[(y * 32 + x) * 4 + 3]];
        assert_eq!(px(&plain, 3, 15), [204, 255]);
        // the shadow of the tall box, 5 m to the west
        assert_eq!(
            px(&shaded, 3, 15),
            [(204.0 * (1.0 - 0.6 * DIRECT) + 0.5) as u8, 255],
            "in the shadow cast from the margin"
        );
        assert_eq!(px(&shaded, 3, 2), [204, 255], "north of the shadow");
        // nothing drawn stays empty
        assert_eq!(px(&shaded, 5, 30)[1], 0);
        assert_eq!(px(&plain, 5, 30)[1], 0);
    }

    #[test]
    fn what_cannot_shade_the_picture_changes_nothing() {
        // a box by its two corners, (west, south, bottom) and (east, north, top)
        type Corners = ([f32; 3], [f32; 3]);
        // boxes scattered over a picture and its margins, on a slight slope: without those that
        // are neither near nor tall enough, the factors of the picture are exactly the same
        let mut seed = 20261114u64;
        let mut random = move |n: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % n
        };
        let mut refused = 0;
        for (azimuth, elevation) in [(315.0, 35.0), (200.0, 20.0), (90.0, 60.0), (10.0, 45.0), (270.0, 35.0)] {
            let light = Lighting { ambient_occlusion: 0.3, ..sun(azimuth, elevation) };
            let (metres, reach) = (0.5f32, 24.0f32);
            let frame = Frame::with(&light, reach, 0.0, [40.0, -24.0], metres, 96, 64);
            let (lo, hi) = frame.bounds();
            // a slope, and a pit in a corner of the picture: the lowest point of the whole is
            // not the lowest of each of its parts
            let ground = |x: f32, y: f32| 10.0 + 0.02 * x - 0.01 * y - if x > 60.0 && y < -36.0 { 6.0 } else { 0.0 };
            let boxes: Vec<Corners> = (0..80)
                .map(|_| {
                    let (w, d) = (1.0 + random(8) as f32, 1.0 + random(8) as f32);
                    let x = lo[0] - 4.0 + random(((hi[0] - lo[0] + 8.0) * 2.0) as u64) as f32 / 2.0;
                    let y = lo[1] - 4.0 + random(((hi[1] - lo[1] + 8.0) * 2.0) as u64) as f32 / 2.0;
                    let top = 10.5 + random(24) as f32 * 0.5 + if random(5) == 0 { 40.0 } else { 0.0 };
                    ([x, y, 8.0], [x + w, y + d, top])
                })
                .collect();
            let heights = |keep: &dyn Fn(&Corners) -> bool| -> Vec<f32> {
                let kept: Vec<_> = boxes.iter().filter(|b| keep(b)).collect();
                (0..frame.width * frame.height)
                    .map(|i| {
                        let (x, y) = (
                            lo[0] + ((i % frame.width) as f32 + 0.5) * metres,
                            hi[1] - ((i / frame.width) as f32 + 0.5) * metres,
                        );
                        kept.iter()
                            .filter(|b| x >= b.0[0] && x < b.1[0] && y >= b.0[1] && y < b.1[1])
                            .fold(ground(x, y), |z, b| z.max(b.1[2]))
                    })
                    .collect()
            };
            let shade =
                |h: &[f32]| factors(h, frame.width, frame.height, frame.origin, metres, &light, reach, frame.window);
            // as a block is drawn: what is near the picture first, which gives the floor, then
            // what is tall enough
            let near = heights(&|b| frame.caster.inside(b.0, b.1) || frame.near(b.0, b.1));
            let [x0, y0, w, h] = frame.window;
            let mut floor = Floor::empty();
            for y in y0 - 2..y0 + h + 2 {
                for x in x0 - 2..x0 + w + 2 {
                    floor.add(x as isize - x0 as isize, y as isize - y0 as isize, w, h, near[y * frame.width + x]);
                }
            }
            // what lies wholly in the picture is the picture's own
            let keep =
                |b: &Corners| frame.caster.inside(b.0, b.1) || frame.near(b.0, b.1) || frame.reaches(b.0, b.1, &floor);
            refused += boxes.iter().filter(|b| !keep(b)).count();
            let (all, kept) = (shade(&heights(&|_| true)), shade(&heights(&keep)));
            assert!(all.iter().any(|v| *v < 0.7));
            assert_eq!(all, kept, "azimuth {azimuth}");
        }
        assert!(refused > 40, "{refused} boxes left out");
    }

    #[test]
    fn overrides_are_taken_out_of_the_arguments() {
        let mut args: Vec<String> = [
            "tiles",
            "--azimuth",
            "120",
            "output",
            "--shadows",
            "off",
            "--zone",
            "0,0,1,1",
            "--strength",
            "0.4",
            "--no-lighting",
        ]
        .map(String::from)
        .to_vec();
        let mut light = Lighting::default();
        overrides(&mut args, &mut light).unwrap();
        assert_eq!(args, ["tiles", "output", "--zone", "0,0,1,1"]);
        assert_eq!((light.azimuth, light.strength, light.shadows, light.enabled), (120.0, 0.4, false, false));
        let mut args: Vec<String> = ["tiles", "--lighting", "maybe"].map(String::from).to_vec();
        assert!(overrides(&mut args, &mut light).is_err());
        let mut args: Vec<String> = ["tiles", "--elevation"].map(String::from).to_vec();
        assert!(overrides(&mut args, &mut light).is_err());
    }
}
