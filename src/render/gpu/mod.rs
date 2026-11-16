//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The GPU half of the drawing: the [`Batch`]es of the list ([`crate::render::scene`]) drawn by the
//! graphics card (wgpu: DirectX 12 or Vulkan), into the same [`Raster`] the CPU half
//! ([`crate::render::draw`]) fills. What follows a drawing (the shading, the holes, the tiles) reads the
//! raster's colours and depths and does not know which half drew them.
//!
//! [`paint`] is called by `assets::paint` for every picture. It answers `None` when the picture is
//! left to the CPU: no card (`--renderer cpu`, no adapter, no device: said once, when the card is
//! opened), a picture that already holds colours, memory the card refuses (that picture only: the
//! card then keeps fewer textures, `Renderer::short_of_memory`), a failure of the card (said once;
//! the rest of the run is then drawn by the CPU). Whatever happens, the raster is only written
//! once the picture is back whole and the card raised no error: a picture left to the CPU is as
//! it was.
//!
//! What the card does as the CPU half does:
//!
//! - **the projection**: every vertex goes through `Raster::project`, here, on the CPU: the card
//!   gets screen positions (pixels) and depths (metres below the camera), and only turns pixels
//!   into its own clip space. In perspective the depth is the `w` of a vertex, so that everything
//!   is interpolated as the CPU half interpolates it;
//! - **the triangles left out**: one with a vertex at 0.1 m of the camera's plane or nearer, or
//!   with a position that is not a number, is not drawn at all (the CPU half does not clip);
//!   one the raster's filter refuses (`Raster::set_pass`, the margins of the shading) neither;
//! - **the order**: batches are drawn in the order they come, in one render pass; the depth test
//!   is "nearer or equal", so that at equal depth the later triangle wins;
//! - **the depth**: metres, written by the fragment shader as `metres / FAR` (a power of two:
//!   nothing is rounded) in a 32-bit float depth buffer; a blended triangle is tested 0.05 m
//!   nearer than it is and does not write its depth; a pixel nothing was drawn on comes back with
//!   an infinite depth;
//! - **the colour**: the texture's first level read at the four texels around the coordinates
//!   and mixed by the shader with the CPU half's own formula (not by the card's sampler, whose
//!   weights only have 8 bits), repeated, times the tint; a cut-out is dropped under alpha 1/2;
//!   the four layers of a terrain are mixed by the mask read at the second coordinates, or by the
//!   vertices' weights; colours stay 32-bit floats to the end, blending included;
//! - **texture coordinates that are not numbers** (a few models have some): the CPU half reads
//!   texels that are not numbers with them, and the pixels end black. A card makes what it wants
//!   of such coordinates, so they are sent as a marker and the shaders give those pixels the
//!   CPU's colour themselves (`LOST`, and `lost` in the shaders);
//! - **heights only** (`Raster::set_pass`): nothing blended, opaque batches without their
//!   textures. Only the depths are drawn and read back then, and merged into those the raster
//!   holds (the nearer one wins, as when everything is drawn in one go): the colours of such a
//!   raster are left as they are, nothing reads them.
//!
//! What it cannot do as the CPU half does, and why two pictures are not equal to the bit:
//!
//! - the card interpolates with its own rounding: on a texture read much finer than the picture
//!   (all of them are: only the first level is read) a coordinate one bit apart is enough to move
//!   a channel by 1/255. Its grid of 1/256 pixel for the vertices would move far more: that one is
//!   measured and taken back in the shaders (`slip`);
//! - a pixel whose centre lies exactly on an edge belongs to one triangle only (the card's
//!   top-left rule; the CPU half draws both, the later one wins), and a pixel a hair from an edge
//!   may fall on either side of it;
//! - two different surfaces at the same depth, a cut-out whose alpha is at the threshold: either
//!   half decides by its last bit.
//!
//! Measured on test pictures and on 64 blocks of downtown: 94 to 98 % of the pixels are
//! equal at 8 bits, 99.97 % within 2/255, 99.98 % within 8/255; the rest (0.01 to 0.02 %) are
//! single pixels on edges, in foliage and where surfaces coincide. `BTMS_GPU_COMPARE` draws every
//! picture both ways and counts the differences.
//!
//! How it is fed. One device for all threads. Each thread gathers its picture ([`Frame`]: the
//! vertices of all batches in a few arrays, the triangles kept, the draws), sends it and waits
//! for it: one submission per picture. A texture is uploaded once for all threads, by the first
//! one that needs it (`Tex::id`), and the least recently used ones leave the card beyond a budget.
//! Nothing large is copied while one of wgpu's own locks is held: textures and geometry go
//! through staging memory filled here (`Renderer::upload`, `send`).
//!
//! Environment: `BTMS_RENDERER=auto|gpu|cpu` is the choice when `--renderer` is not given;
//! `BTMS_GPU=<words>` picks the adapter whose backend and name hold all the words
//! (`vulkan`, `dx12 radeon`…; a software adapter can be named too); `WGPU_BACKEND` (wgpu's own)
//! restricts the backends looked at; `BTMS_GPU_MEMORY=<MB>` is the budget of the textures kept
//! on the card (`--vram <GB>` of `tiles` before it: [`texture_budget`]); `BTMS_GPU_COMPARE=<folder|1>` compares both halves on every picture (with a
//! folder: the pictures too); `BTMS_GPU_FAULT=device|fail|fail:<n>` makes the card unusable
//! or fail, to see the fallbacks ([`Trial`]).

pub mod chain;
pub mod shade;

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::render::raster::{Mode, Raster};
use crate::render::scene::{Batch, Mask, Material, Tex};
use crate::util::profile::{self, Count, Phase, time};

/// What hands the scene over: called with what draws each batch, it returns the totals.
pub type Stream<'a> = dyn FnMut(&mut dyn FnMut(&Batch)) -> Totals + 'a;

/// What `scene::stream` returns: (triangles, triangles per render bucket).
pub type Totals = (usize, [usize; 8]);

/// Depths are stored as `metres / FAR`: a power of two, so that the division and the product
/// that undoes it are exact, and far beyond anything the map holds.
pub(crate) const FAR: f32 = 65536.0;
/// A blended triangle is drawn where it is at most this much (metres) behind what is there: the
/// tolerance of `raster.rs`.
const BLEND_TOLERANCE: f32 = 0.05;
const COLOUR: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float;
const DEPTH: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Megabytes of textures kept on the card before the least recently used are dropped, when
/// neither `--vram` nor `BTMS_GPU_MEMORY` says otherwise.
const TEXTURE_BUDGET_MB: usize = 4000;
/// How long a picture may take to come back from the card.
const WAIT: Duration = Duration::from_secs(120);
/// Render targets kept by a thread (a block and the heights around it are two).
const TARGETS_KEPT: usize = 3;
/// When the card runs short of memory, the textures it keeps are cut ([`shrunk`]), no lower than
/// this: a card that cannot keep this much beside the pictures is full of something else.
const MIN_TEXTURE_BUDGET: usize = 256_000_000;
/// After a cut, what the textures gave back takes a moment to leave the card (the pictures of
/// the other threads still hold some): the shortages of that moment cut nothing more.
const SHORTAGE_PAUSE: Duration = Duration::from_secs(2);

/// Which half draws: `--renderer auto|gpu|cpu`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Choice {
    /// The graphics card when there is one, else the processor.
    Auto,
    /// The same, a software adapter being accepted when there is nothing else.
    Gpu,
    /// The processor.
    Cpu,
}

static CHOICE: OnceLock<Choice> = OnceLock::new();

/// The choice of the run (`--renderer`; the processor when none was made: the tests).
pub(crate) fn choice() -> Choice {
    CHOICE.get().copied().unwrap_or(Choice::Cpu)
}
static RENDERER: OnceLock<Option<Renderer>> = OnceLock::new();
/// `--vram` of `tiles`, in bytes.
static TEXTURE_BUDGET: OnceLock<usize> = OnceLock::new();

/// Keeps the budget of the textures on the card the run was given (`--vram`, in
/// bytes), over `BTMS_GPU_MEMORY`. Before the card is opened: it reads it then.
pub fn set_texture_budget(bytes: usize) {
    let _ = TEXTURE_BUDGET.set(bytes);
}

/// Bytes of textures kept on the card: `--vram`, else `BTMS_GPU_MEMORY` (megabytes),
/// else 4 GB.
pub fn texture_budget() -> usize {
    budget_of(TEXTURE_BUDGET.get().copied(), std::env::var("BTMS_GPU_MEMORY").ok().as_deref())
}

fn budget_of(option: Option<usize>, megabytes: Option<&str>) -> usize {
    option
        .or_else(|| megabytes.and_then(|v| v.trim().parse::<usize>().ok()).map(|mb| mb * 1_000_000))
        .unwrap_or(TEXTURE_BUDGET_MB * 1_000_000)
}

/// Takes `--renderer auto|gpu|cpu` out of the arguments and keeps it for the run. Without the
/// option: `BTMS_RENDERER` (the same three words; for scripts whose command lines are written
/// already), else auto.
pub fn choose(args: &mut Vec<String>) -> Result<()> {
    let default = std::env::var("BTMS_RENDERER").ok();
    let _ = CHOICE.set(chosen(args, default.as_deref())?);
    Ok(())
}

/// What [`choose`] keeps: `--renderer` taken out of `args`, over `default` (`BTMS_RENDERER`).
pub(crate) fn chosen(args: &mut Vec<String>, default: Option<&str>) -> Result<Choice> {
    let mut choice = match default {
        None | Some("auto") => Choice::Auto,
        Some("gpu") => Choice::Gpu,
        Some("cpu") => Choice::Cpu,
        Some(other) => bail!("BTMS_RENDERER={other}: expected auto, gpu or cpu"),
    };
    while let Some(i) = args.iter().position(|a| a == "--renderer") {
        choice = match args.get(i + 1).map(String::as_str) {
            Some("auto") => Choice::Auto,
            Some("gpu") => Choice::Gpu,
            Some("cpu") => Choice::Cpu,
            other => bail!("--renderer {}: expected auto, gpu or cpu", other.unwrap_or("")),
        };
        args.drain(i..i + 2);
    }
    Ok(choice)
}

/// Opens the card now, on a thread of its own, instead of at the first picture: a command that
/// will draw calls this first, and the card is ready (measured at 0.8 s on a desktop card)
/// by the time the map is loaded. Without it the first picture of every thread waits for it.
pub fn warm() {
    if CHOICE.get().is_some_and(|choice| *choice != Choice::Cpu) {
        std::thread::spawn(|| {
            renderer();
        });
    }
}

/// The renderer of the run, opened at the first picture (or by [`warm`]): None when the CPU
/// draws. Says once which card draws, or why none does.
fn renderer() -> Option<&'static Renderer> {
    RENDERER
        .get_or_init(|| {
            // (nothing chosen: the tests, which draw on the CPU)
            let choice = CHOICE.get().copied().unwrap_or(Choice::Cpu);
            if choice == Choice::Cpu {
                return None;
            }
            // the reference drawing is asked for: it is the processor's
            if choice == Choice::Auto && std::env::var("BTMS_REFERENCE_DRAWING").is_ok_and(|v| v != "0") {
                eprintln!("renderer: CPU (BTMS_REFERENCE_DRAWING asks for the reference drawing)");
                return None;
            }
            let started = Instant::now();
            // (a panic in the card's driver or in wgpu must not take the run with it)
            let opened = std::panic::catch_unwind(|| Renderer::open(choice == Choice::Gpu))
                .unwrap_or_else(|_| Err("unexpected crash while opening the card".to_string()));
            match opened {
                Ok(renderer) => {
                    eprintln!("renderer: graphics card {} ({:.1} s)", renderer.name, started.elapsed().as_secs_f64());
                    Some(renderer)
                }
                Err(why) => {
                    eprintln!("renderer: CPU (no usable graphics card: {why})");
                    None
                }
            }
        })
        .as_ref()
}

/// `BTMS_GPU_FAULT`, to see the fallbacks without breaking a card: `device` asks for a device
/// no adapter can give (the real refusal of wgpu); `fail` makes the card fail at its first
/// picture, `fail:<n>` at its n-th (a made-up failure, raised where a real one of the reading
/// back would be: everything is drawn and back, the raster not yet written).
enum Trial {
    None,
    Device,
    Picture(u64),
}

fn trial() -> &'static Trial {
    static TRIAL: OnceLock<Trial> = OnceLock::new();
    TRIAL.get_or_init(|| match std::env::var("BTMS_GPU_FAULT").as_deref() {
        Ok("device") => Trial::Device,
        Ok("fail") => Trial::Picture(1),
        Ok(other) => {
            other.strip_prefix("fail:").and_then(|n| n.parse().ok()).map(Trial::Picture).unwrap_or(Trial::None)
        }
        Err(_) => Trial::None,
    })
}

/// Why a picture is not drawn by the card.
#[derive(Clone, Debug)]
pub(crate) enum Failure {
    /// This picture only (a texture or a picture too large for the card): the card stays in use.
    Picture(String),
    /// The card refused memory: this picture is left to the CPU, the card keeps fewer textures
    /// from now on ([`Renderer::short_of_memory`]) and stays in use.
    Memory(String),
    /// The card itself: it is given up for the rest of the run.
    Card(String),
}

/// What an error the card raised means for the run: memory refused leaves the picture to the
/// CPU (a refused texture or buffer does not lose the device in wgpu); anything else gives the
/// card up.
fn verdict(error: &wgpu::Error) -> Failure {
    match error {
        wgpu::Error::OutOfMemory { .. } => Failure::Memory(error.to_string()),
        _ => Failure::Card(error.to_string()),
    }
}

/// A future that is ready at once, as wgpu's are outside a browser.
fn now<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        std::thread::yield_now();
    }
}

/// What both shaders share: the view, the placement of a vertex, the depth as stored, and the
/// texture read. `bilinear` is `Image::sample` (`raster.rs`), operation for operation.
const COMMON: &str = r#"
struct View {
    // 2 / width, 2 / height, 1 / FAR, 1 in perspective (0 from straight above)
    a: vec4<f32>,
    // tolerance of the blended triangles (metres), "not a number", then nothing
    b: vec4<f32>,
};
@group(0) @binding(0) var<uniform> view: View;

// A vertex comes projected (x, y in pixels, depth in metres). In perspective its depth is its w:
// what is interpolated is then interpolated as on the CPU (divided by depth).
fn place(screen: vec3<f32>) -> vec4<f32> {
    let w = select(1.0, screen.z, view.a.w > 0.5);
    let ndc = vec2<f32>(screen.x * view.a.x - 1.0, 1.0 - screen.y * view.a.y);
    return vec4<f32>(ndc * w, 0.5 * w, w);
}

fn stored(depth: f32) -> f32 {
    return depth * view.a.z;
}

// The card moves every vertex to a grid of 1/256 pixel before it interpolates: what it gives a
// pixel is what the triangle as given holds a little beside the pixel's centre. `screen` (the
// screen position as given, interpolated like the rest but straight across the screen) is that
// place: its distance to the centre, in pixels, is the slip, taken back from every value with
// the value's own slope (`back_*`). At most 1/32 pixel: beyond, something else is wrong.
fn slip(screen: vec2<f32>, position: vec4<f32>) -> vec2<f32> {
    return clamp(screen - position.xy, vec2<f32>(-0.03125), vec2<f32>(0.03125));
}

fn back_1(v: f32, s: vec2<f32>) -> f32 {
    return v - (dpdx(v) * s.x + dpdy(v) * s.y);
}

fn back_2(v: vec2<f32>, s: vec2<f32>) -> vec2<f32> {
    return v - (dpdx(v) * s.x + dpdy(v) * s.y);
}

fn back_4(v: vec4<f32>, s: vec2<f32>) -> vec4<f32> {
    return v - (dpdx(v) * s.x + dpdy(v) * s.y);
}

// The four texels around (u, v), repeated, mixed: (a (1 - fx) + b fx) (1 - fy) + (c (1 - fx) + d fx) fy.
fn bilinear(t: texture_2d<f32>, uv: vec2<f32>) -> vec4<f32> {
    let size = vec2<i32>(textureDimensions(t));
    let xy = (uv - floor(uv)) * vec2<f32>(size) - vec2<f32>(0.5);
    let xy0 = floor(xy);
    let f = xy - xy0;
    let g = vec2<f32>(1.0) - f;
    // xy0 is -1 ..= size - 1
    let p = vec2<i32>(xy0);
    let a = (p + size) % size;
    let b = (p + vec2<i32>(1) + size) % size;
    let ta = textureLoad(t, vec2<i32>(a.x, a.y), 0);
    let tb = textureLoad(t, vec2<i32>(b.x, a.y), 0);
    let tc = textureLoad(t, vec2<i32>(a.x, b.y), 0);
    let td = textureLoad(t, vec2<i32>(b.x, b.y), 0);
    return (ta * g.x + tb * f.x) * g.y + (tc * g.x + td * f.x) * f.y;
}

// A texture coordinate that is not a number comes here as 1e30 (`LOST`), and stays huge wherever
// its vertex weighs. The CPU half reads texels that are not numbers with it: the colour is not a
// number either, and ends black. `nothing()` is that colour (the card is given it: a shader
// cannot write it down).
fn lost(uv: vec2<f32>) -> bool {
    return abs(uv.x) > 1e20 || abs(uv.y) > 1e20;
}

fn nothing() -> vec3<f32> {
    return vec3<f32>(view.b.y);
}

// The colour of a pixel, and whether it is not a number.
struct Shade {
    c: vec4<f32>,
    lost: bool,
};

struct Pixel {
    @location(0) colour: vec4<f32>,
    @builtin(frag_depth) depth: f32,
};
"#;

/// One texture times the tint, or the tint alone.
const SINGLE: &str = r#"
struct Material {
    // 1 with a texture, 0 for the tint alone
    textured: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
};
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var<uniform> material: Material;

struct Varyings {
    @builtin(position) @invariant position: vec4<f32>,
    @location(0) depth: f32,
    @location(1) uv: vec2<f32>,
    @location(2) tint: vec4<f32>,
    @location(5) @interpolate(linear) screen: vec2<f32>,
};

@vertex
fn vertex(@location(0) screen: vec3<f32>, @location(1) uv: vec2<f32>, @location(2) tint: vec4<f32>) -> Varyings {
    var out: Varyings;
    out.position = place(screen);
    out.depth = screen.z;
    out.uv = uv;
    out.tint = tint;
    out.screen = screen.xy;
    return out;
}

// What the triangle holds at the centre of the pixel.
struct Point {
    depth: f32,
    uv: vec2<f32>,
    tint: vec4<f32>,
};

fn point(in: Varyings) -> Point {
    let s = slip(in.screen, in.position);
    return Point(back_1(in.depth, s), back_2(in.uv, s), back_4(in.tint, s));
}

// The texture times the tint; without a texture, the tint alone. With coordinates that are not
// numbers neither the colour nor its alpha is one.
fn colour(p: Point) -> Shade {
    if (material.textured == 0u) {
        return Shade(p.tint, false);
    }
    if (lost(p.uv)) {
        return Shade(vec4<f32>(nothing(), 0.0), true);
    }
    return Shade(bilinear(image, p.uv) * p.tint, false);
}

@fragment
fn opaque(in: Varyings) -> Pixel {
    let p = point(in);
    let s = colour(p);
    return Pixel(vec4<f32>(s.c.rgb, 1.0), stored(p.depth));
}

// Kept where alpha is at least one half: not where it is not a number.
@fragment
fn cutout(in: Varyings) -> Pixel {
    let p = point(in);
    let s = colour(p);
    if (s.lost || !(s.c.a >= 0.5)) {
        discard;
    }
    return Pixel(vec4<f32>(s.c.rgb, 1.0), stored(p.depth));
}

// Mixed by alpha over what is there; a colour that is not a number replaces it whatever its alpha
// (on the CPU half the mix is not a number either).
@fragment
fn blend(in: Varyings) -> Pixel {
    let p = point(in);
    let s = colour(p);
    let alpha = select(clamp(s.c.a, 0.0, 1.0), 1.0, s.lost);
    return Pixel(vec4<f32>(s.c.rgb, alpha), stored(p.depth - view.b.x));
}

@fragment
fn height(in: Varyings) -> @builtin(frag_depth) f32 {
    let p = point(in);
    return stored(p.depth);
}

@fragment
fn height_cutout(in: Varyings) -> @builtin(frag_depth) f32 {
    let p = point(in);
    let s = colour(p);
    if (s.lost || !(s.c.a >= 0.5)) {
        discard;
    }
    return stored(p.depth);
}
"#;

/// Terrain: four layers mixed by the mask, or by the vertices' weights.
const LAYERS: &str = r#"
struct Material {
    // bit k set: layer k has a texture (else it counts as mid grey)
    present: u32,
    // 1: the weights come from the mask
    masked: u32,
    // channels of the mask: the 0-1 / 2-3 lerp, the final lerp
    first: u32,
    second: u32,
};
@group(1) @binding(0) var layer0: texture_2d<f32>;
@group(1) @binding(1) var layer1: texture_2d<f32>;
@group(1) @binding(2) var layer2: texture_2d<f32>;
@group(1) @binding(3) var layer3: texture_2d<f32>;
@group(1) @binding(4) var mask: texture_2d<f32>;
@group(1) @binding(5) var<uniform> material: Material;

struct Varyings {
    @builtin(position) @invariant position: vec4<f32>,
    @location(0) depth: f32,
    @location(1) uv: vec2<f32>,
    @location(2) tint: vec4<f32>,
    @location(3) weights: vec4<f32>,
    @location(4) uv1: vec2<f32>,
    @location(5) @interpolate(linear) screen: vec2<f32>,
};

@vertex
fn vertex(
    @location(0) screen: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tint: vec4<f32>,
    @location(3) weights: vec4<f32>,
    @location(4) uv1: vec2<f32>,
) -> Varyings {
    var out: Varyings;
    out.position = place(screen);
    out.depth = screen.z;
    out.uv = uv;
    out.tint = tint;
    out.weights = weights;
    out.uv1 = uv1;
    out.screen = screen.xy;
    return out;
}

// What the triangle holds at the centre of the pixel.
struct Point {
    depth: f32,
    uv: vec2<f32>,
    tint: vec4<f32>,
    weights: vec4<f32>,
    uv1: vec2<f32>,
};

fn point(in: Varyings) -> Point {
    let s = slip(in.screen, in.position);
    return Point(back_1(in.depth, s), back_2(in.uv, s), back_4(in.tint, s), back_4(in.weights, s), back_2(in.uv1, s));
}

// One layer's share of the colour: its texture, or mid grey when it has none, times its weight.
// A layer that weighs nothing is not read; one read with coordinates that are not numbers is
// not a number (`lost`).
struct Share {
    c: vec4<f32>,
    lost: bool,
};

fn share(t: texture_2d<f32>, bit: u32, uv: vec2<f32>, gone: bool, weight: f32, total: f32) -> Share {
    if (!(weight > 0.0)) {
        return Share(vec4<f32>(0.0), false);
    }
    if ((material.present & bit) == 0u) {
        return Share(vec4<f32>(0.5, 0.5, 0.5, 1.0) * weight / total, false);
    }
    if (gone) {
        return Share(vec4<f32>(0.0), true);
    }
    return Share(bilinear(t, uv) * weight / total, false);
}

fn colour(p: Point) -> Shade {
    var weights = p.weights;
    if (material.masked != 0u) {
        // a mask read with coordinates that are not numbers: no weight is a number, nor the colour
        if (lost(p.uv1)) {
            return Shade(vec4<f32>(nothing(), p.tint.a), true);
        }
        let s = bilinear(mask, p.uv1);
        let x = s[material.first];
        let y = s[material.second];
        weights = vec4<f32>((1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y);
    }
    let total = max(weights.x + weights.y + weights.z + weights.w, 1e-6);
    let gone = lost(p.uv);
    let s0 = share(layer0, 1u, p.uv, gone, weights.x, total);
    let s1 = share(layer1, 2u, p.uv, gone, weights.y, total);
    let s2 = share(layer2, 4u, p.uv, gone, weights.z, total);
    let s3 = share(layer3, 8u, p.uv, gone, weights.w, total);
    if (s0.lost || s1.lost || s2.lost || s3.lost) {
        return Shade(vec4<f32>(nothing(), p.tint.a), true);
    }
    let c = vec4<f32>(0.0) + s0.c + s1.c + s2.c + s3.c;
    // the layers' colour times the tint, with the tint's own alpha
    return Shade(vec4<f32>(c.rgb * p.tint.rgb, p.tint.a), false);
}

@fragment
fn opaque(in: Varyings) -> Pixel {
    let p = point(in);
    let s = colour(p);
    return Pixel(vec4<f32>(s.c.rgb, 1.0), stored(p.depth));
}

@fragment
fn cutout(in: Varyings) -> Pixel {
    let p = point(in);
    let s = colour(p);
    if (!(s.c.a >= 0.5)) {
        discard;
    }
    return Pixel(vec4<f32>(s.c.rgb, 1.0), stored(p.depth));
}

// Mixed by alpha over what is there; a colour that is not a number replaces it whatever its alpha
// (on the CPU half the mix is not a number either).
@fragment
fn blend(in: Varyings) -> Pixel {
    let p = point(in);
    let s = colour(p);
    let alpha = select(clamp(s.c.a, 0.0, 1.0), 1.0, s.lost);
    return Pixel(vec4<f32>(s.c.rgb, alpha), stored(p.depth - view.b.x));
}

@fragment
fn height_cutout(in: Varyings) -> @builtin(frag_depth) f32 {
    let p = point(in);
    let s = colour(p);
    if (!(s.c.a >= 0.5)) {
        discard;
    }
    return stored(p.depth);
}
"#;

/// The pipelines, by (material, mode), then those that draw heights only.
const SINGLE_OPAQUE: usize = 0;
const LAYERS_OPAQUE: usize = 3;
const HEIGHT: usize = 6;
const HEIGHT_SINGLE_CUTOUT: usize = 7;
const HEIGHT_LAYERS_CUTOUT: usize = 8;

/// The pipeline of a batch.
fn pipeline_of(layered: bool, mode: Mode, heights_only: bool) -> usize {
    let step = match mode {
        Mode::Opaque => 0,
        Mode::Cutout => 1,
        Mode::Blend => 2,
    };
    match (heights_only, layered, mode) {
        (false, false, _) => SINGLE_OPAQUE + step,
        (false, true, _) => LAYERS_OPAQUE + step,
        (true, true, Mode::Cutout) => HEIGHT_LAYERS_CUTOUT,
        (true, false, Mode::Cutout) => HEIGHT_SINGLE_CUTOUT,
        (true, _, _) => HEIGHT,
    }
}

/// Whether a pipeline takes the vertices of terrain (weights and second coordinates).
fn takes_layers(pipeline: usize) -> bool {
    (LAYERS_OPAQUE..LAYERS_OPAQUE + 3).contains(&pipeline) || pipeline == HEIGHT_LAYERS_CUTOUT
}

/// A texture on the card.
struct Texture {
    view: wgpu::TextureView,
    /// Bound alone, for a batch with this one texture.
    group: wgpu::BindGroup,
    bytes: usize,
}

type Uploaded = Result<Arc<Texture>, Failure>;

/// A texture whose bytes wait in a buffer of the card: (the buffer, bytes per row, the texture,
/// its size).
type Waiting = (wgpu::Buffer, u32, wgpu::Texture, wgpu::Extent3d);

/// A texture of the cache: uploaded once for all threads, by the first one that asks.
#[derive(Default)]
struct Entry {
    cell: Arc<OnceLock<Uploaded>>,
    /// The picture (their count) that used it last.
    used: u64,
}

/// The textures on the card, by `Tex::id`.
#[derive(Default)]
struct Textures {
    map: HashMap<u64, Entry>,
    /// Pictures begun so far.
    tick: u64,
    /// When the textures were last cut for want of memory.
    shrunk_at: Option<Instant>,
}

/// The card, opened once for all threads.
struct Renderer {
    /// Name and backend, for the user.
    name: String,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: Vec<wgpu::RenderPipeline>,
    view_layout: wgpu::BindGroupLayout,
    single_layout: wgpu::BindGroupLayout,
    layers_layout: wgpu::BindGroupLayout,
    /// Stands for a texture that is not there (never read: the material says so).
    blank: wgpu::TextureView,
    /// The tint alone.
    flat: wgpu::BindGroup,
    /// The material of a batch with one texture.
    textured: wgpu::Buffer,
    /// The materials of terrain, by (layers present, masked, channels).
    materials: Mutex<HashMap<[u32; 4], wgpu::Buffer>>,
    textures: Mutex<Textures>,
    /// Textures whose bytes are in a buffer of the card and are still to be copied into them:
    /// the next picture submitted, of any thread, has them copied first.
    waiting: Mutex<Vec<Waiting>>,
    /// Bytes of textures on the card, and how many may stay (lowered when the card runs short of
    /// memory).
    held: AtomicUsize,
    budget: AtomicUsize,
    max_texture: u32,
    max_buffer: u64,
    /// The card failed: the CPU draws from now on.
    down: AtomicBool,
    /// Why (the first failure, on one line), for the reasons of the fallbacks that follow.
    cause: OnceLock<String>,
    said: AtomicBool,
    said_picture: AtomicBool,
    said_memory: AtomicBool,
    // what was done, for the profile
    pictures: AtomicU64,
    left: AtomicU64,
    uploads: AtomicU64,
    uploaded: AtomicU64,
    dropped: AtomicU64,
    /// Pictures the card refused memory for.
    short: AtomicU64,
    /// [`MIN_TEXTURE_BUDGET`] (lower in the tests, which hold few textures).
    floor: usize,
}

impl Renderer {
    /// The best adapter that can draw: a discrete card before an integrated one; a software one
    /// (the processor again, slower than `draw.rs`) only when `software` allows it.
    fn open(software: bool) -> Result<Renderer, String> {
        let descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        let backends = descriptor.backends;
        // The instance and every adapter are kept until the end of the run (never dropped), used or
        // not. Destroying an adapter that is not used, once the device of another one exists, hung
        // the first run of every new executable on a machine whose card is listed twice by
        // DirectX 12: the destruction of its second entry never came back.
        let instance: &'static wgpu::Instance = Box::leak(Box::new(wgpu::Instance::new(descriptor)));
        let mut adapters: Vec<&'static wgpu::Adapter> =
            now(instance.enumerate_adapters(backends)).leak().iter().collect();
        if adapters.is_empty() {
            return Err(format!("no graphics card found (backends searched: {backends:?})"));
        }
        let label = |a: &&'static wgpu::Adapter| {
            let info = a.get_info();
            format!("{} ({:?})", info.name, info.backend)
        };
        // BTMS_GPU: every word must be in the backend and the name
        let wanted: Vec<String> = std::env::var("BTMS_GPU")
            .map(|v| v.to_lowercase().split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        if !wanted.is_empty() {
            let all: Vec<String> = adapters.iter().map(label).collect();
            adapters.retain(|a| {
                let name = label(a).to_lowercase();
                wanted.iter().all(|w| name.contains(w))
            });
            if adapters.is_empty() {
                return Err(format!("BTMS_GPU={} matches none of: {}", wanted.join(" "), all.join(", ")));
            }
        }
        let rank = |a: &&'static wgpu::Adapter| {
            let info = a.get_info();
            let kind = match info.device_type {
                wgpu::DeviceType::DiscreteGpu => 0,
                wgpu::DeviceType::IntegratedGpu => 1,
                wgpu::DeviceType::VirtualGpu => 2,
                wgpu::DeviceType::Other => 3,
                wgpu::DeviceType::Cpu => 4,
            };
            (kind, if info.backend == wgpu::Backend::Dx12 { 0 } else { 1 })
        };
        adapters.sort_by_key(rank);
        let mut refused: Vec<String> = Vec::new();
        for adapter in &adapters {
            let name = label(adapter);
            if refused.iter().any(|r| r.starts_with(&name)) {
                continue; // listed twice (one per screen)
            }
            if adapter.get_info().device_type == wgpu::DeviceType::Cpu && !software && wanted.is_empty() {
                refused.push(format!("{name}: software rendering, skipped"));
                continue;
            }
            if !adapter.features().contains(wgpu::Features::FLOAT32_BLENDABLE) {
                refused.push(format!("{name}: does not blend 32-bit float colours"));
                continue;
            }
            match Renderer::on(adapter, name.clone()) {
                Ok(renderer) => return Ok(renderer),
                Err(why) => refused.push(format!("{name}: {why}")),
            }
        }
        Err(refused.join("; "))
    }

    /// The device of an adapter and everything drawn with: shaders, pipelines, the blank texture.
    fn on(adapter: &wgpu::Adapter, name: String) -> Result<Renderer, String> {
        let limits = adapter.limits();
        // (the storage limits of the adapter: the chain of a block on the card, `chain.rs`, binds
        // more storage buffers to its passes than the default allows; the drawing uses none)
        let mut required = wgpu::Limits {
            max_texture_dimension_2d: limits.max_texture_dimension_2d,
            max_buffer_size: limits.max_buffer_size,
            max_storage_buffers_per_shader_stage: limits.max_storage_buffers_per_shader_stage,
            max_storage_buffer_binding_size: limits.max_storage_buffer_binding_size,
            ..wgpu::Limits::default()
        };
        // (to see the fallback: a device no adapter can give)
        if matches!(trial(), Trial::Device) {
            required.max_texture_dimension_2d = u32::MAX;
        }
        let (device, queue) = now(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("btms"),
            required_features: wgpu::Features::FLOAT32_BLENDABLE,
            required_limits: required,
            ..Default::default()
        }))
        .map_err(|e| format!("device refused ({e})"))?;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);

        let texture = |ty: wgpu::TextureSampleType| wgpu::BindingType::Texture {
            sample_type: ty,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        };
        let float = wgpu::TextureSampleType::Float { filterable: false };
        let uniform = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        };
        let entry = |binding: u32, visibility: wgpu::ShaderStages, ty: wgpu::BindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty,
            count: None,
        };
        let fragment = wgpu::ShaderStages::FRAGMENT;
        let view_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("view"),
            entries: &[entry(0, wgpu::ShaderStages::VERTEX_FRAGMENT, uniform)],
        });
        let single_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("one texture"),
            entries: &[entry(0, fragment, texture(float)), entry(1, fragment, uniform)],
        });
        let layers_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("four layers"),
            entries: &[
                entry(0, fragment, texture(float)),
                entry(1, fragment, texture(float)),
                entry(2, fragment, texture(float)),
                entry(3, fragment, texture(float)),
                entry(4, fragment, texture(float)),
                entry(5, fragment, uniform),
            ],
        });
        let module = |label: &str, source: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(format!("{COMMON}{source}").into()),
            })
        };
        let (single, layers) = (module("one texture", SINGLE), module("four layers", LAYERS));
        let layout = |label: &str, material: &wgpu::BindGroupLayout| {
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: &[Some(&view_layout), Some(material)],
                immediate_size: 0,
            })
        };
        let (single_pipeline, layers_pipeline) =
            (layout("one texture", &single_layout), layout("four layers", &layers_layout));
        // one buffer per attribute: screen position, coordinates, tint, then weights and second
        // coordinates for terrain
        let attributes = [
            wgpu::vertex_attr_array![0 => Float32x3],
            wgpu::vertex_attr_array![1 => Float32x2],
            wgpu::vertex_attr_array![2 => Float32x4],
            wgpu::vertex_attr_array![3 => Float32x4],
            wgpu::vertex_attr_array![4 => Float32x2],
        ];
        let strides = [12, 8, 16, 16, 8];
        let buffers: Vec<Option<wgpu::VertexBufferLayout>> = (0..5)
            .map(|k| {
                Some(wgpu::VertexBufferLayout {
                    array_stride: strides[k],
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes[k],
                })
            })
            .collect();
        let mixing = wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::SrcAlpha,
                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                operation: wgpu::BlendOperation::Add,
            },
            alpha: wgpu::BlendComponent {
                src_factor: wgpu::BlendFactor::Zero,
                dst_factor: wgpu::BlendFactor::One,
                operation: wgpu::BlendOperation::Add,
            },
        };
        let pipeline = |layered: bool, entry: &str, colour: bool, mode: Mode| {
            let targets = [Some(wgpu::ColorTargetState {
                format: COLOUR,
                blend: (mode == Mode::Blend).then_some(mixing),
                write_mask: wgpu::ColorWrites::ALL,
            })];
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(if layered { &layers_pipeline } else { &single_pipeline }),
                vertex: wgpu::VertexState {
                    module: if layered { &layers } else { &single },
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &buffers[..if layered { 5 } else { 3 }],
                },
                // both faces, as the CPU half draws them
                primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH,
                    depth_write_enabled: Some(mode != Mode::Blend),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: if layered { &layers } else { &single },
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: if colour { &targets } else { &[] },
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        // (the order of the constants SINGLE_OPAQUE … HEIGHT_LAYERS_CUTOUT)
        let pipelines = vec![
            pipeline(false, "opaque", true, Mode::Opaque),
            pipeline(false, "cutout", true, Mode::Cutout),
            pipeline(false, "blend", true, Mode::Blend),
            pipeline(true, "opaque", true, Mode::Opaque),
            pipeline(true, "cutout", true, Mode::Cutout),
            pipeline(true, "blend", true, Mode::Blend),
            pipeline(false, "height", false, Mode::Opaque),
            pipeline(false, "height_cutout", false, Mode::Cutout),
            pipeline(true, "height_cutout", false, Mode::Cutout),
        ];

        // one white texel: bound where a texture is missing, never read
        let blank = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("blank"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            blank.as_image_copy(),
            &[255; 4],
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
            wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
        );
        let blank = blank.create_view(&Default::default());
        let constant = |label: &str, words: [u32; 4]| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 16,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&words));
            buffer
        };
        let (untextured, textured) = (constant("no texture", [0; 4]), constant("one texture", [1, 0, 0, 0]));
        let flat = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("no texture"),
            layout: &single_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&blank) },
                wgpu::BindGroupEntry { binding: 1, resource: untextured.as_entire_binding() },
            ],
        });
        if let Some(e) = now(scope.pop()) {
            return Err(format!("shaders or pipelines refused ({e})"));
        }
        let budget = texture_budget();
        Ok(Renderer {
            name,
            pipelines,
            view_layout,
            single_layout,
            layers_layout,
            blank,
            flat,
            textured,
            materials: Mutex::new(HashMap::new()),
            textures: Mutex::new(Textures::default()),
            waiting: Mutex::new(Vec::new()),
            held: AtomicUsize::new(0),
            budget: AtomicUsize::new(budget),
            max_texture: limits.max_texture_dimension_2d,
            max_buffer: limits.max_buffer_size,
            down: AtomicBool::new(false),
            cause: OnceLock::new(),
            said: AtomicBool::new(false),
            said_picture: AtomicBool::new(false),
            said_memory: AtomicBool::new(false),
            pictures: AtomicU64::new(0),
            left: AtomicU64::new(0),
            uploads: AtomicU64::new(0),
            uploaded: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            short: AtomicU64::new(0),
            floor: MIN_TEXTURE_BUDGET,
            device,
            queue,
        })
    }

    /// Errors raised outside a picture's own watch, and the loss of the device: the card is
    /// given up. (Set once the renderer has its final address.)
    fn watch(&'static self) {
        self.device.on_uncaptured_error(Arc::new(move |e: wgpu::Error| self.give_up(&e.to_string())));
        self.device.set_device_lost_callback(move |_, why| self.give_up(&format!("device lost: {why}")));
    }

    /// The card failed: said once, and the CPU draws from now on.
    fn give_up(&self, why: &str) {
        let why = why.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        // (kept before the card is marked down: the first failure is the cause)
        let _ = self.cause.set(why.clone());
        self.down.store(true, Ordering::Relaxed);
        if !self.said.swap(true, Ordering::Relaxed) {
            eprintln!("renderer: the graphics card failed ({why}); the rest is drawn by the CPU");
        }
    }

    /// A picture begins: counted, and the textures the card holds beyond its budget are dropped,
    /// the least recently used first, down to three quarters of it.
    fn begin(&self) {
        let mut cache = self.textures.lock().unwrap();
        cache.tick += 1;
        self.trim(&mut cache, self.budget.load(Ordering::Relaxed));
    }

    /// Drops the textures held beyond `budget`, the least recently used first, down to three
    /// quarters of it ([`to_drop`]).
    fn trim(&self, cache: &mut Textures, budget: usize) {
        if self.held.load(Ordering::Relaxed) <= budget {
            return;
        }
        let entries: Vec<(u64, u64, usize)> = cache
            .map
            .iter()
            .filter_map(|(id, e)| e.cell.get().and_then(|t| t.as_ref().ok()).map(|t| (e.used, *id, t.bytes)))
            .collect();
        let (gone, freed) = to_drop(entries, self.held.load(Ordering::Relaxed), budget);
        for id in &gone {
            cache.map.remove(id);
        }
        self.dropped.fetch_add(gone.len() as u64, Ordering::Relaxed);
        self.held.fetch_sub(freed.min(self.held.load(Ordering::Relaxed)), Ordering::Relaxed);
    }

    /// The card refused memory for a picture ([`Failure::Memory`]): the budget of the textures
    /// is cut to three quarters of what they hold ([`shrunk`]) and those beyond leave the card
    /// now; the picture is left to the CPU, the next ones are drawn by the card again. Said once.
    /// The shortages that come within [`SHORTAGE_PAUSE`] of a cut cut nothing more (the memory
    /// given back is still leaving the card). The card is given up only when its textures are
    /// too few to give any back: something else fills it. Nothing when the card is given up
    /// already: wgpu loses the device for memory refused where it cannot go on (its own staging
    /// memory, a mapping, a submission), and still reports the refusal.
    fn short_of_memory(&self, why: &str) {
        if self.down.load(Ordering::Relaxed) {
            return;
        }
        self.short.fetch_add(1, Ordering::Relaxed);
        let mut cache = self.textures.lock().unwrap();
        if cache.shrunk_at.is_some_and(|at| at.elapsed() < SHORTAGE_PAUSE) {
            return;
        }
        let held = self.held.load(Ordering::Relaxed);
        let Some(budget) = shrunk(held, self.budget.load(Ordering::Relaxed), self.floor) else {
            drop(cache);
            self.give_up(&format!("the card ran out of memory with only {} MB of textures: {why}", held / 1_000_000));
            return;
        };
        self.budget.store(budget, Ordering::Relaxed);
        cache.shrunk_at = Some(Instant::now());
        self.trim(&mut cache, budget);
        drop(cache);
        if !self.said_memory.swap(true, Ordering::Relaxed) {
            let why = why.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
            eprintln!(
                "renderer: the graphics card is short of memory ({why}); from now on it keeps at most {} MB of textures (less again if this happens again), the picture is left to the CPU and the next ones stay on the card",
                budget / 1_000_000
            );
        }
    }

    /// Makes something on the card, watching its memory alone: refused, it is a
    /// [`Failure::Memory`], and what wgpu handed back (an object that cannot be used) is dropped,
    /// never kept. Other errors go to the scopes around, as before.
    fn made<T>(&self, what: &str, make: impl FnOnce() -> T) -> Result<T, Failure> {
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let made = make();
        let refused = now(scope.pop()).map(|error| error.to_string());
        // (the tests refuse one, as a full card would)
        #[cfg(test)]
        let refused =
            refused.or_else(|| tests::REFUSE.with(|r| r.replace(false)).then(|| "refused by the tests".to_string()));
        match refused {
            None => Ok(made),
            Some(error) => Err(Failure::Memory(format!("{what}: {error}"))),
        }
    }

    /// The texture on the card, uploaded by the first thread that asks for it.
    fn texture(&self, texture: &Tex) -> Uploaded {
        let cell = {
            let mut cache = self.textures.lock().unwrap();
            let tick = cache.tick;
            let entry = cache.map.entry(texture.id).or_default();
            entry.used = tick;
            entry.cell.clone()
        };
        let uploaded = cell.get_or_init(|| time(Phase::GpuTextures, || self.upload(texture))).clone();
        if let Err(Failure::Memory(_)) = &uploaded {
            // not kept: it is sent again when asked for next, once the card has room
            let mut cache = self.textures.lock().unwrap();
            if cache.map.get(&texture.id).is_some_and(|e| Arc::ptr_eq(&e.cell, &cell)) {
                cache.map.remove(&texture.id);
            }
        }
        uploaded
    }

    fn upload(&self, texture: &Tex) -> Uploaded {
        let image = &*texture.image;
        let (width, height) = (image.width, image.height);
        if width == 0 || height == 0 || image.rgba.len() != width * height * 4 {
            return Err(Failure::Picture(format!("a {width}×{height} px texture has {} bytes", image.rgba.len())));
        }
        if width.max(height) > self.max_texture as usize {
            return Err(Failure::Picture(format!(
                "a {width}×{height} px texture is larger than the card accepts ({} px)",
                self.max_texture
            )));
        }
        let size = wgpu::Extent3d { width: width as u32, height: height as u32, depth_or_array_layers: 1 };
        // Through a buffer of its own, filled here and copied by the card, and not through
        // `Queue::write_texture`: that one copies the bytes while it holds the queue's lock, which
        // every thread's uploads and submissions wait for (measured: 107 ms of a block at 10
        // threads, 10 ms at one). The copy itself is not submitted here either (a submission per
        // texture cost half of the upload): it waits for the next picture (`Renderer::submit`).
        // A row of such a copy is a multiple of 256 bytes.
        let row = (4 * width).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize);
        // (memory refused: nothing of it is kept, the texture is asked for again later)
        let (on_card, staging) = self.made(&format!("{width}×{height} px texture"), || {
            // the bytes as they are: no colour space, the first level only
            let on_card = self.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: (row * height) as u64,
                usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: true,
            });
            (on_card, staging)
        })?;
        match staging.get_mapped_range_mut(..) {
            Ok(mut mapped) if row == 4 * width => mapped.slice(..).copy_from_slice(&image.rgba),
            Ok(mut mapped) => {
                for (y, texels) in image.rgba.chunks_exact(4 * width).enumerate() {
                    mapped.slice(y * row..y * row + 4 * width).copy_from_slice(texels);
                }
            }
            Err(e) => return Err(Failure::Card(format!("texture upload: {e}"))),
        }
        staging.unmap();
        let view = on_card.create_view(&Default::default());
        // listed before the texture is given to anyone: whoever draws with it has it copied first
        self.waiting.lock().unwrap().push((staging, row as u32, on_card, size));
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.single_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: self.textured.as_entire_binding() },
            ],
        });
        let bytes = image.rgba.len();
        self.held.fetch_add(bytes, Ordering::Relaxed);
        self.uploads.fetch_add(1, Ordering::Relaxed);
        self.uploaded.fetch_add(bytes as u64, Ordering::Relaxed);
        Ok(Arc::new(Texture { view, group, bytes }))
    }

    /// Hands a picture's commands to the card, after the copies of the textures that wait for
    /// one. The list is held until the card has them all: no thread can draw with a texture
    /// before its bytes are in it.
    fn submit(&self, picture: wgpu::CommandBuffer) -> wgpu::SubmissionIndex {
        let mut waiting = self.waiting.lock().unwrap();
        if waiting.is_empty() {
            return self.queue.submit([picture]);
        }
        let mut encoder = self.device.create_command_encoder(&Default::default());
        for (staging, row, texture, size) in waiting.drain(..) {
            encoder.copy_buffer_to_texture(
                wgpu::TexelCopyBufferInfo {
                    buffer: &staging,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row),
                        rows_per_image: Some(size.height),
                    },
                },
                texture.as_image_copy(),
                size,
            );
        }
        self.queue.submit([encoder.finish(), picture])
    }

    /// The four layers of a terrain and its mask, bound together.
    fn layers(&self, layers: &[Option<Tex>; 4], mask: Option<&Mask>) -> Result<wgpu::BindGroup, Failure> {
        let mut present = 0u32;
        let mut views: Vec<Arc<Texture>> = Vec::with_capacity(5);
        let mut at = [usize::MAX; 5];
        for (k, layer) in layers.iter().enumerate() {
            if let Some(texture) = layer {
                present |= 1 << k;
                at[k] = views.len();
                views.push(self.texture(texture)?);
            }
        }
        let mut words = [present, 0, 0, 0];
        if let Some(mask) = mask {
            words = [present, 1, mask.channels.0.min(3) as u32, mask.channels.1.min(3) as u32];
            at[4] = views.len();
            views.push(self.texture(&mask.texture)?);
        }
        let material = {
            let mut materials = self.materials.lock().unwrap();
            match materials.get(&words) {
                Some(buffer) => buffer.clone(),
                // (kept for all threads: never one the card refused)
                None => {
                    let buffer = self.made("material", || {
                        self.device.create_buffer(&wgpu::BufferDescriptor {
                            label: None,
                            size: 16,
                            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        })
                    })?;
                    self.queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&words));
                    materials.insert(words, buffer.clone());
                    buffer
                }
            }
        };
        let view = |k: usize| {
            wgpu::BindingResource::TextureView(if at[k] == usize::MAX { &self.blank } else { &views[at[k]].view })
        };
        Ok(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.layers_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: view(0) },
                wgpu::BindGroupEntry { binding: 1, resource: view(1) },
                wgpu::BindGroupEntry { binding: 2, resource: view(2) },
                wgpu::BindGroupEntry { binding: 3, resource: view(3) },
                wgpu::BindGroupEntry { binding: 4, resource: view(4) },
                wgpu::BindGroupEntry { binding: 5, resource: material.as_entire_binding() },
            ],
        }))
    }
}

/// The textures to drop from the card, of `entries` (last use, id, bytes), when `held` bytes pass
/// `budget`: the least recently used first, down to three quarters of the budget. Their ids, and
/// the bytes freed.
fn to_drop(mut entries: Vec<(u64, u64, usize)>, held: usize, budget: usize) -> (Vec<u64>, usize) {
    if held <= budget {
        return (Vec::new(), 0);
    }
    entries.sort_unstable();
    let (mut held, mut freed, mut gone) = (held, 0usize, Vec::new());
    for (_, id, bytes) in entries {
        if held <= budget / 4 * 3 {
            break;
        }
        gone.push(id);
        held = held.saturating_sub(bytes);
        freed += bytes;
    }
    (gone, freed)
}

/// The budget of the textures once the card refused memory while `held` bytes of them were on
/// it (`budget` until then): three quarters of what they held. None when that would be under
/// `floor` ([`MIN_TEXTURE_BUDGET`]): the textures have too little to give back, something else
/// fills the card.
fn shrunk(held: usize, budget: usize, floor: usize) -> Option<usize> {
    let lowered = held.min(budget) / 4 * 3;
    (lowered >= floor).then_some(lowered)
}

/// The vertices of a picture, one array per attribute (weights and second coordinates: terrain
/// only), and their buffers on the card, kept from one picture to the next.
#[derive(Default)]
struct Vertices {
    positions: Vec<[f32; 3]>,
    uvs: Vec<[f32; 2]>,
    tints: Vec<[f32; 4]>,
    weights: Vec<[f32; 4]>,
    uvs1: Vec<[f32; 2]>,
    buffers: [Option<wgpu::Buffer>; 5],
}

impl Vertices {
    fn clear(&mut self) {
        self.positions.clear();
        self.uvs.clear();
        self.tints.clear();
        self.weights.clear();
        self.uvs1.clear();
    }

    /// Sends the arrays to the card; false when there is nothing to send.
    fn send(&mut self, renderer: &Renderer) -> Result<bool, Failure> {
        if self.positions.is_empty() {
            return Ok(false);
        }
        let arrays: [&[u8]; 5] = [
            bytemuck::cast_slice(&self.positions),
            bytemuck::cast_slice(&self.uvs),
            bytemuck::cast_slice(&self.tints),
            bytemuck::cast_slice(&self.weights),
            bytemuck::cast_slice(&self.uvs1),
        ];
        for (buffer, bytes) in self.buffers.iter_mut().zip(arrays) {
            send(renderer, buffer, wgpu::BufferUsages::VERTEX, bytes)?;
        }
        Ok(true)
    }
}

/// Writes `bytes` in a buffer of the card, made (larger) when it is too small.
fn send(
    renderer: &Renderer,
    buffer: &mut Option<wgpu::Buffer>,
    usage: wgpu::BufferUsages,
    bytes: &[u8],
) -> Result<(), Failure> {
    if bytes.is_empty() {
        return Ok(());
    }
    let needed = bytes.len() as u64;
    if buffer.as_ref().is_none_or(|b| b.size() < needed) {
        // (a multiple of 4, as copies ask)
        let size = needed.next_power_of_two().max(1 << 20).min(renderer.max_buffer & !3);
        if size < needed {
            return Err(Failure::Picture(format!(
                "{} MB of geometry is more than the card accepts in one buffer",
                needed / 1_000_000
            )));
        }
        // (refused: the thread keeps the one it had)
        let made = renderer.made(&format!("{} MB buffer", size / 1_000_000), || {
            renderer.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })
        })?;
        *buffer = Some(made);
    }
    // (copied here into the card's staging memory; `Queue::write_buffer` would copy while it holds
    // the queue's lock, which the other threads wait for)
    let size = wgpu::BufferSize::new(needed).expect("not empty");
    match renderer.queue.write_buffer_with(buffer.as_ref().unwrap(), 0, size) {
        Some(mut staging) => staging.copy_from_slice(bytes),
        None => return Err(Failure::Card(format!("upload of {} MB of geometry refused", needed / 1_000_000))),
    }
    Ok(())
}

/// What a texture coordinate that is not a number is sent as: the shaders know it by its size
/// (see `lost` there), and give the pixels it reaches the colour the CPU half gives them.
const LOST: f32 = 1e30;

/// Replaces the coordinates that are not numbers by [`LOST`].
fn mark_lost(coordinates: &mut [[f32; 2]]) {
    for pair in coordinates {
        for value in pair {
            if !value.is_finite() {
                *value = LOST;
            }
        }
    }
}

/// Consecutive triangles drawn with the same pipeline and the same textures.
struct Run {
    pipeline: usize,
    /// In `Frame::groups`.
    group: usize,
    first: u32,
    count: u32,
}

/// What a picture is drawn into, and what brings it back.
struct Target {
    width: u32,
    height: u32,
    /// None for heights only.
    colour: Option<(wgpu::Texture, wgpu::TextureView, wgpu::Buffer, usize)>,
    depth: (wgpu::Texture, wgpu::TextureView, wgpu::Buffer, usize),
}

impl Target {
    /// (memory refused: nothing of it is kept)
    fn new(renderer: &Renderer, width: u32, height: u32, colour: bool) -> Result<Target, Failure> {
        renderer.made(&format!("{width}×{height} px picture"), || Target::make(renderer, width, height, colour))
    }

    fn make(renderer: &Renderer, width: u32, height: u32, colour: bool) -> Target {
        let make = |format: wgpu::TextureFormat, bytes: u32| {
            let texture = renderer.device.create_texture(&wgpu::TextureDescriptor {
                label: None,
                size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            // a row of a copy to a buffer is a multiple of 256 bytes
            let row = (width * bytes).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let read = renderer.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: row as u64 * height as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            (texture, view, read, row as usize)
        };
        Target { width, height, colour: colour.then(|| make(COLOUR, 16)), depth: make(DEPTH, 4) }
    }
}

/// What a picture of the chain is drawn into ([`paint_kept`]): kept on the card for the compute
/// passes that follow (`chain.rs`), never read back, so without the buffers a [`Target`] has.
struct Held {
    width: u32,
    height: u32,
    colour: Option<wgpu::TextureView>,
    depth: wgpu::TextureView,
}

impl Held {
    /// (memory refused: nothing of it is kept)
    fn new(renderer: &Renderer, width: u32, height: u32, colour: bool) -> Result<Held, Failure> {
        renderer.made(&format!("{width}×{height} px kept picture"), || {
            let make = |format: wgpu::TextureFormat| {
                renderer
                    .device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("kept picture"),
                        size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            };
            Held { width, height, colour: colour.then(|| make(COLOUR)), depth: make(DEPTH) }
        })
    }
}

/// How a picture of the chain begins ([`paint_kept`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Keep {
    /// Colours and depths, from this background (nothing drawn: an infinite depth).
    Colour([f32; 3]),
    /// Depths only, from nothing ("heights only", the first pass of the margins).
    Heights,
    /// Depths only, over those the last `Heights` picture of this size left (the second pass of
    /// the margins: the nearer wins, as when the CPU half merges them).
    HeightsOver,
}

/// A picture of the chain drawn on the card and kept there: its colours (none for heights) and
/// depths (`metres / FAR`, 1 where nothing was drawn), for the compute passes of `chain.rs`. Valid
/// until the thread that drew it draws another picture of the same size.
#[derive(Clone)]
pub(crate) struct Drawn {
    pub colour: Option<wgpu::TextureView>,
    pub depth: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

/// The picture a thread is drawing: what it gathers from the batches, and what it keeps on the
/// card from one picture to the next (buffers, targets).
#[derive(Default)]
struct Frame {
    /// Vertices of the batches with one texture or none, then of the terrain.
    plain: Vertices,
    layered: Vertices,
    /// Triangles, as indices in `plain` or `layered` (by the pipeline of their run).
    indices: Vec<u32>,
    index_buffer: Option<wgpu::Buffer>,
    runs: Vec<Run>,
    /// The bind groups of the runs; the first one is the tint alone.
    groups: Vec<wgpu::BindGroup>,
    /// Where the group of a texture is in `groups`, by `Tex::id`; of a terrain, by the ids of
    /// its layers and mask (0: none) and the mask's channels.
    single: HashMap<u64, usize>,
    layers: HashMap<([u64; 5], (usize, usize)), usize>,
    view: Option<(wgpu::Buffer, wgpu::BindGroup)>,
    targets: Vec<Target>,
    /// The pictures of the chain, kept on the card ([`paint_kept`]).
    held: Vec<Held>,
    failure: Option<Failure>,
}

thread_local! {
    static FRAME: RefCell<Frame> = RefCell::new(Frame::default());
}

impl Frame {
    fn begin(&mut self, renderer: &Renderer) {
        self.plain.clear();
        self.layered.clear();
        self.indices.clear();
        self.runs.clear();
        self.groups.clear();
        self.groups.push(renderer.flat.clone());
        self.single.clear();
        self.layers.clear();
        self.failure = None;
    }

    /// The bind group of a batch's material: its place in `groups`. `flat`: the tint alone,
    /// whatever the material.
    fn group(&mut self, renderer: &Renderer, material: &Material, flat: bool) -> Result<usize, Failure> {
        match material {
            _ if flat => Ok(0),
            Material::Single(None) => Ok(0),
            Material::Single(Some(texture)) => {
                if let Some(&at) = self.single.get(&texture.id) {
                    return Ok(at);
                }
                self.groups.push(renderer.texture(texture)?.group.clone());
                self.single.insert(texture.id, self.groups.len() - 1);
                Ok(self.groups.len() - 1)
            }
            Material::Layers { layers, mask } => {
                let id = |t: Option<&Tex>| t.map(|t| t.id).unwrap_or(0);
                let key = (
                    [
                        id(layers[0].as_ref()),
                        id(layers[1].as_ref()),
                        id(layers[2].as_ref()),
                        id(layers[3].as_ref()),
                        id(mask.as_ref().map(|m| &m.texture)),
                    ],
                    mask.as_ref().map(|m| m.channels).unwrap_or((0, 0)),
                );
                if let Some(&at) = self.layers.get(&key) {
                    return Ok(at);
                }
                self.groups.push(renderer.layers(layers, mask.as_ref())?);
                self.layers.insert(key, self.groups.len() - 1);
                Ok(self.groups.len() - 1)
            }
        }
    }

    /// Gathers a batch: its vertices projected, its triangles that can reach the picture.
    fn add(&mut self, renderer: &Renderer, raster: &Raster, batch: &Batch) {
        if self.failure.is_some() {
            return;
        }
        let heights_only = raster.heights_only;
        // heights only, as the CPU half does it: nothing blended, an opaque batch without its
        // textures (the tint alone)
        if heights_only && batch.mode == Mode::Blend {
            return;
        }
        let flat = heights_only && batch.mode == Mode::Opaque;
        let layered = !flat && matches!(batch.material, Material::Layers { .. });
        let pipeline = pipeline_of(layered, batch.mode, heights_only);
        let vertices = if layered { &mut self.layered } else { &mut self.plain };
        let (count, base, first) = (batch.positions.len(), vertices.positions.len(), self.indices.len());
        vertices.positions.extend(batch.positions.iter().map(|p| raster.project(*p)));
        let projected = &vertices.positions[base..];
        let (width, height) = (raster.width as f32, raster.height as f32);
        for triangle in batch.indices.as_chunks::<3>().0 {
            let i = [triangle[0] as usize, triangle[1] as usize, triangle[2] as usize];
            if i.iter().any(|&i| i >= count) {
                continue;
            }
            let s = [projected[i[0]], projected[i[1]], projected[i[2]]];
            // left out as by the CPU half: a vertex at the camera's plane or behind it, a
            // coordinate that is not a number (a depth that is not one would be drawn there)
            if s.iter().any(|p| p[2] <= 0.1 || !p[2].is_finite() || !p[0].is_finite() || !p[1].is_finite()) {
                continue;
            }
            // wholly beside the picture
            if s[0][0].max(s[1][0]).max(s[2][0]) < 0.0
                || s[0][0].min(s[1][0]).min(s[2][0]) > width
                || s[0][1].max(s[1][1]).max(s[2][1]) < 0.0
                || s[0][1].min(s[1][1]).min(s[2][1]) > height
            {
                continue;
            }
            // the raster's filter (the margins of the shading)
            if raster.keep.as_ref().is_some_and(|keep| !keep(&i.map(|i| batch.positions[i]))) {
                continue;
            }
            self.indices.extend(i.map(|i| (base + i) as u32));
        }
        if profile::on() {
            profile::count(Count::Triangles, (batch.indices.len() / 3) as u64);
            profile::count(Count::Kept, ((self.indices.len() - first) / 3) as u64);
        }
        if self.indices.len() == first {
            // nothing of it is drawn: its textures are not even looked for
            vertices.positions.truncate(base);
            return;
        }
        let group = match self.group(renderer, &batch.material, flat) {
            Ok(group) => group,
            Err(failure) => {
                self.failure = Some(failure);
                return;
            }
        };
        let vertices = if layered { &mut self.layered } else { &mut self.plain };
        if batch.uvs.len() == count && batch.tints.len() == count {
            vertices.uvs.extend_from_slice(batch.uvs);
            vertices.tints.extend_from_slice(batch.tints);
        } else {
            vertices.uvs.extend((0..count).map(|i| batch.uvs.get(i).copied().unwrap_or([0.0; 2])));
            vertices.tints.extend((0..count).map(|i| batch.tints.get(i).copied().unwrap_or([1.0; 4])));
        }
        mark_lost(&mut vertices.uvs[base..]);
        if layered {
            // (as `draw.rs` takes them when a batch has none)
            if batch.weights.len() == count {
                vertices.weights.extend_from_slice(batch.weights);
            } else {
                vertices
                    .weights
                    .extend((0..count).map(|i| batch.weights.get(i).copied().unwrap_or([1.0, 0.0, 0.0, 0.0])));
            }
            if batch.uvs1.len() == count {
                vertices.uvs1.extend_from_slice(batch.uvs1);
            } else {
                vertices.uvs1.extend((0..count).map(|i| batch.uvs1.get(i).copied().unwrap_or([0.0; 2])));
            }
            mark_lost(&mut vertices.uvs1[base..]);
        }
        let added = (self.indices.len() - first) as u32;
        match self.runs.last_mut() {
            Some(run) if run.pipeline == pipeline && run.group == group => run.count += added,
            _ => self.runs.push(Run { pipeline, group, first: first as u32, count: added }),
        }
    }

    /// The target of this size, with or without colours, made when the thread has none.
    fn target(&mut self, renderer: &Renderer, width: u32, height: u32, colour: bool) -> Result<usize, Failure> {
        if let Some(at) =
            self.targets.iter().position(|t| t.width == width && t.height == height && t.colour.is_some() == colour)
        {
            return Ok(at);
        }
        if self.targets.len() >= TARGETS_KEPT {
            self.targets.remove(0);
        }
        self.targets.push(Target::new(renderer, width, height, colour)?);
        Ok(self.targets.len() - 1)
    }

    /// Lets go of all the thread holds on the card (buffers, targets, the bind groups of its last
    /// picture), after the card refused memory: made again by the next picture.
    fn forget(&mut self) {
        self.plain.buffers = Default::default();
        self.layered.buffers = Default::default();
        self.index_buffer = None;
        self.view = None;
        self.targets.clear();
        self.held.clear();
        self.groups.clear();
        self.failure = None;
    }

    /// The kept picture of this size, with or without colours, made when the thread has none.
    fn held(&mut self, renderer: &Renderer, width: u32, height: u32, colour: bool) -> Result<usize, Failure> {
        if let Some(at) =
            self.held.iter().position(|t| t.width == width && t.height == height && t.colour.is_some() == colour)
        {
            return Ok(at);
        }
        if self.held.len() >= TARGETS_KEPT {
            self.held.remove(0);
        }
        self.held.push(Held::new(renderer, width, height, colour)?);
        Ok(self.held.len() - 1)
    }

    /// Records the runs gathered into a render pass: their pipelines, vertices and bind groups.
    fn record(&self, renderer: &Renderer, pass: &mut wgpu::RenderPass, plain: bool, layered: bool) {
        let (mut pipeline, mut group, mut terrain) = (usize::MAX, usize::MAX, None);
        for run in &self.runs {
            if run.pipeline != pipeline {
                pass.set_pipeline(&renderer.pipelines[run.pipeline]);
                pipeline = run.pipeline;
                // the vertices this pipeline takes, and its own kind of bind group
                let takes = takes_layers(pipeline);
                if terrain != Some(takes) {
                    terrain = Some(takes);
                    group = usize::MAX;
                    let (vertices, sent) = if takes { (&self.layered, layered) } else { (&self.plain, plain) };
                    if sent {
                        for (slot, buffer) in vertices.buffers.iter().enumerate().take(if takes { 5 } else { 3 }) {
                            pass.set_vertex_buffer(slot as u32, buffer.as_ref().unwrap().slice(..));
                        }
                    }
                }
            }
            if run.group != group {
                pass.set_bind_group(1, &self.groups[run.group], &[]);
                group = run.group;
            }
            pass.draw_indexed(run.first..run.first + run.count, 0, 0..1);
        }
    }

    /// The view uniform of a picture of this raster (made once per thread), and its bind group.
    fn view_group(&mut self, renderer: &Renderer, raster: &Raster) -> wgpu::BindGroup {
        let (device, queue) = (&renderer.device, &renderer.queue);
        let (width, height) = (raster.width as u32, raster.height as u32);
        let view: [f32; 8] =
            [2.0 / width as f32, 2.0 / height as f32, 1.0 / FAR, 0.0, BLEND_TOLERANCE, f32::NAN, 0.0, 0.0];
        let (view_buffer, view_group) = self.view.get_or_insert_with(|| {
            let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &renderer.view_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
            });
            (buffer, group)
        });
        queue.write_buffer(view_buffer, 0, bytemuck::cast_slice(&view));
        view_group.clone()
    }

    /// Draws what was gathered into a picture kept on the card ([`Held`]) and hands it over
    /// without waiting for it: the chain's compute passes read it next (`chain.rs`). Unlike
    /// [`Frame::draw`], a picture with nothing to draw is still begun (cleared), but for `HeightsOver`.
    fn draw_kept(&mut self, renderer: &Renderer, raster: &Raster, keep: Keep) -> Result<Drawn, Failure> {
        if let Some(failure) = self.failure.take() {
            return Err(failure);
        }
        let (width, height) = (raster.width as u32, raster.height as u32);
        if width.max(height) > renderer.max_texture {
            return Err(Failure::Picture(format!(
                "a {width}×{height} px picture is larger than the card accepts ({} px)",
                renderer.max_texture
            )));
        }
        let colour = matches!(keep, Keep::Colour(_));
        time(Phase::GpuDraw, || -> Result<Drawn, Failure> {
            let at = self.held(renderer, width, height, colour)?;
            let drawn = {
                let held = &self.held[at];
                Drawn { colour: held.colour.clone(), depth: held.depth.clone(), width, height }
            };
            if self.runs.is_empty() && keep == Keep::HeightsOver {
                return Ok(drawn);
            }
            let plain = self.plain.send(renderer)?;
            let layered = self.layered.send(renderer)?;
            send(renderer, &mut self.index_buffer, wgpu::BufferUsages::INDEX, bytemuck::cast_slice(&self.indices))?;
            let view_group = self.view_group(renderer, raster);
            let mut encoder = renderer.device.create_command_encoder(&Default::default());
            {
                let background = match keep {
                    Keep::Colour(c) => wgpu::Color { r: c[0] as f64, g: c[1] as f64, b: c[2] as f64, a: 0.0 },
                    _ => wgpu::Color::default(),
                };
                let colours = [drawn.colour.as_ref().map(|view| wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(background), store: wgpu::StoreOp::Store },
                })];
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: if colour { &colours } else { &[] },
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &drawn.depth,
                        // 1: nothing drawn; over the last picture: what it left
                        depth_ops: Some(wgpu::Operations {
                            load: if keep == Keep::HeightsOver { wgpu::LoadOp::Load } else { wgpu::LoadOp::Clear(1.0) },
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                if !self.runs.is_empty() {
                    pass.set_bind_group(0, &view_group, &[]);
                    pass.set_index_buffer(self.index_buffer.as_ref().unwrap().slice(..), wgpu::IndexFormat::Uint32);
                    self.record(renderer, &mut pass, plain, layered);
                }
            }
            renderer.submit(encoder.finish());
            Ok(drawn)
        })
    }

    /// Draws what was gathered and brings it back into the raster. `background`: the colour of
    /// a picture nothing was drawn on yet; None for heights only (the depths are merged into
    /// those the raster holds). `raised` gives the error the card raised since the picture was
    /// begun, if any: it is asked once everything is back, and the raster is only written when
    /// there is none and nothing can fail any more.
    fn draw(
        &mut self,
        renderer: &Renderer,
        raster: &mut Raster,
        background: Option<[f32; 3]>,
        raised: &mut dyn FnMut() -> Option<Failure>,
    ) -> Result<(), Failure> {
        if let Some(failure) = self.failure.take() {
            return Err(failure);
        }
        // nothing to draw: the picture stays as it is
        if self.runs.is_empty() {
            return Ok(());
        }
        let (device, queue) = (&renderer.device, &renderer.queue);
        let (width, height) = (raster.width as u32, raster.height as u32);
        if width.max(height) > renderer.max_texture {
            return Err(Failure::Picture(format!(
                "a {width}×{height} px picture is larger than the card accepts ({} px)",
                renderer.max_texture
            )));
        }
        let card = |e: String| Failure::Card(e);
        let mut at = 0;
        let submitted = time(Phase::GpuDraw, || -> Result<wgpu::SubmissionIndex, Failure> {
            at = self.target(renderer, width, height, background.is_some())?;
            let plain = self.plain.send(renderer)?;
            let layered = self.layered.send(renderer)?;
            send(renderer, &mut self.index_buffer, wgpu::BufferUsages::INDEX, bytemuck::cast_slice(&self.indices))?;
            let view: [f32; 8] =
                [2.0 / width as f32, 2.0 / height as f32, 1.0 / FAR, 0.0, BLEND_TOLERANCE, f32::NAN, 0.0, 0.0];
            let (view_buffer, view_group) = self.view.get_or_insert_with(|| {
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 32,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &renderer.view_layout,
                    entries: &[wgpu::BindGroupEntry { binding: 0, resource: buffer.as_entire_binding() }],
                });
                (buffer, group)
            });
            queue.write_buffer(view_buffer, 0, bytemuck::cast_slice(&view));

            let target = &self.targets[at];
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let colours = [target.colour.as_ref().map(|(_, view, ..)| wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(
                            background
                                .map(|c| wgpu::Color { r: c[0] as f64, g: c[1] as f64, b: c[2] as f64, a: 0.0 })
                                .unwrap_or_default(),
                        ),
                        store: wgpu::StoreOp::Store,
                    },
                })];
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: if target.colour.is_some() { &colours } else { &[] },
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &target.depth.1,
                        // 1: nothing drawn (an infinite depth once back)
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_bind_group(0, &*view_group, &[]);
                pass.set_index_buffer(self.index_buffer.as_ref().unwrap().slice(..), wgpu::IndexFormat::Uint32);
                let (mut pipeline, mut group, mut terrain) = (usize::MAX, usize::MAX, None);
                for run in &self.runs {
                    if run.pipeline != pipeline {
                        pass.set_pipeline(&renderer.pipelines[run.pipeline]);
                        pipeline = run.pipeline;
                        // the vertices this pipeline takes, and its own kind of bind group
                        let takes = takes_layers(pipeline);
                        if terrain != Some(takes) {
                            terrain = Some(takes);
                            group = usize::MAX;
                            let (vertices, sent) = if takes { (&self.layered, layered) } else { (&self.plain, plain) };
                            if sent {
                                for (slot, buffer) in
                                    vertices.buffers.iter().enumerate().take(if takes { 5 } else { 3 })
                                {
                                    pass.set_vertex_buffer(slot as u32, buffer.as_ref().unwrap().slice(..));
                                }
                            }
                        }
                    }
                    if run.group != group {
                        pass.set_bind_group(1, &self.groups[run.group], &[]);
                        group = run.group;
                    }
                    pass.draw_indexed(run.first..run.first + run.count, 0, 0..1);
                }
            }
            let extent = wgpu::Extent3d { width, height, depth_or_array_layers: 1 };
            let rows = |row: usize| wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row as u32),
                rows_per_image: Some(height),
            };
            if let Some((texture, _, read, row)) = &target.colour {
                encoder.copy_texture_to_buffer(
                    texture.as_image_copy(),
                    wgpu::TexelCopyBufferInfo { buffer: read, layout: rows(*row) },
                    extent,
                );
            }
            let (texture, _, read, row) = &target.depth;
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::DepthOnly,
                },
                wgpu::TexelCopyBufferInfo { buffer: read, layout: rows(*row) },
                extent,
            );
            Ok(renderer.submit(encoder.finish()))
        })?;

        time(Phase::GpuRead, || -> Result<(), Failure> {
            let target = &self.targets[at];
            let reads: Vec<&wgpu::Buffer> = target.colour.iter().map(|c| &c.2).chain([&target.depth.2]).collect();
            let (sender, receiver) = std::sync::mpsc::channel();
            for read in &reads {
                let sender = sender.clone();
                read.map_async(wgpu::MapMode::Read, .., move |mapped| {
                    let _ = sender.send(mapped);
                });
            }
            // from here on the buffers are mapped, or will be: whatever happens they are unmapped
            let result = (|| -> Result<(), Failure> {
                device
                    .poll(wgpu::PollType::Wait { submission_index: Some(submitted), timeout: Some(WAIT) })
                    .map_err(|e| card(format!("waiting for the picture: {e}")))?;
                let deadline = Instant::now() + WAIT;
                for _ in &reads {
                    loop {
                        match receiver.try_recv() {
                            Ok(mapped) => break mapped.map_err(|e| card(format!("reading the picture back: {e}")))?,
                            Err(_) if Instant::now() > deadline => {
                                return Err(card("reading the picture back: timed out".to_string()));
                            }
                            Err(_) => {
                                let _ = device.poll(wgpu::PollType::Poll);
                                std::thread::yield_now();
                            }
                        }
                    }
                }
                if renderer.down.load(Ordering::Relaxed) {
                    return Err(card("the card failed during the drawing".to_string()));
                }
                let (w, h) = (width as usize, height as usize);
                let depths =
                    target.depth.2.get_mapped_range(..).map_err(|e| card(format!("reading the depths back: {e}")))?;
                let colours = match &target.colour {
                    Some((_, _, read, _)) => {
                        Some(read.get_mapped_range(..).map_err(|e| card(format!("reading the colours back: {e}")))?)
                    }
                    None => None,
                };
                // (a mapping is aligned for floats; should one not be, nothing is written)
                let misaligned = || card("reading back: misaligned memory".to_string());
                let first_depths: &[f32] = bytemuck::try_cast_slice(&depths[..w * 4]).map_err(|_| misaligned())?;
                if let Some(colours) = &colours {
                    let _: &[[f32; 4]] = bytemuck::try_cast_slice(&colours[..w * 16]).map_err(|_| misaligned())?;
                }
                let _ = first_depths;
                if let Some(failure) = raised() {
                    return Err(failure);
                }
                if let Trial::Picture(n) = trial()
                    && renderer.pictures.load(Ordering::Relaxed) + 1 >= *n
                {
                    return Err(card("failure simulated by BTMS_GPU_FAULT".to_string()));
                }
                // nothing can fail any more: the raster is written
                if let (Some(colours), Some((.., row))) = (&colours, &target.colour) {
                    for (from, to) in colours.chunks(*row).zip(raster.colour.chunks_exact_mut(w)).take(h) {
                        let from: &[[f32; 4]] = bytemuck::cast_slice(&from[..w * 16]);
                        for (to, from) in to.iter_mut().zip(from) {
                            *to = [from[0], from[1], from[2]];
                        }
                    }
                }
                let merge = background.is_none();
                for (from, to) in depths.chunks(target.depth.3).zip(raster.depth.chunks_exact_mut(w)).take(h) {
                    let from: &[f32] = bytemuck::cast_slice(&from[..w * 4]);
                    if merge {
                        // heights only: the nearer of what is there and what was drawn
                        for (to, from) in to.iter_mut().zip(from) {
                            if *from < 1.0 && *from * FAR <= *to {
                                *to = *from * FAR;
                            }
                        }
                    } else {
                        for (to, from) in to.iter_mut().zip(from) {
                            *to = if *from < 1.0 { *from * FAR } else { f32::INFINITY };
                        }
                    }
                }
                Ok(())
            })();
            for read in &reads {
                read.unmap();
            }
            result
        })
    }
}

/// The colour of a picture nothing was drawn on yet (every pixel at an infinite depth and of
/// the same colour): what the card can start from. None: the picture already holds something.
fn untouched(raster: &Raster) -> Option<[f32; 3]> {
    let background = *raster.colour.first()?;
    let bits = background.map(f32::to_bits);
    let empty = raster.depth.chunks(4096).all(|c| c.iter().fold(true, |all, d| all & (*d == f32::INFINITY)));
    (empty && raster.colour.chunks(4096).all(|c| c.iter().fold(true, |all, p| all & (p.map(f32::to_bits) == bits))))
        .then_some(background)
}

/// Draws the batches `stream` hands out into `raster`, on the graphics card. None when the
/// picture is left to the CPU half: `stream` may then have been called, and is to be called
/// again; the raster is as it was.
pub fn paint(raster: &mut Raster, stream: &mut Stream<'_>) -> Option<Totals> {
    // (a block redone on the processor after the chain on the card failed: `on_processor`)
    if processor_only() {
        return None;
    }
    let renderer = renderer()?;
    if renderer.down.load(Ordering::Relaxed) {
        crate::render::gpu::chain::note(
            crate::render::gpu::chain::Kind::Drawing,
            &crate::render::gpu::chain::with_cause(
                "the graphics card failed earlier in the render",
                renderer.cause.get().map(String::as_str),
            ),
        );
        return None;
    }
    if raster.width == 0 || raster.height == 0 {
        return None;
    }
    match comparing() {
        Some(folder) => compared(renderer, raster, stream, folder),
        None => card_paint(renderer, raster, stream),
    }
}

fn card_paint(renderer: &'static Renderer, raster: &mut Raster, stream: &mut Stream<'_>) -> Option<Totals> {
    // heights only are merged into what the raster holds; colours need a picture nothing was
    // drawn on (what is blended is blended over what the card itself drew)
    let background = if raster.heights_only {
        None
    } else {
        // (timed apart: this first reading of the picture's buffers is when the system hands
        // their memory over, which costs more than the reading)
        let Some(background) = time(Phase::GpuStart, || untouched(raster)) else {
            renderer.left.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        Some(background)
    };
    static WATCHED: std::sync::Once = std::sync::Once::new();
    WATCHED.call_once(|| renderer.watch());
    renderer.begin();
    // the errors of this thread's calls to the card, from here to the picture read back
    let mut scopes = Some(
        [wgpu::ErrorFilter::Validation, wgpu::ErrorFilter::OutOfMemory, wgpu::ErrorFilter::Internal]
            .map(|filter| renderer.device.push_error_scope(filter)),
    );
    let mut raised = || -> Option<Failure> {
        let mut first: Option<Failure> = None;
        // (popped in the reverse order of their creation)
        for scope in scopes.take()?.into_iter().rev() {
            if let Some(error) = now(scope.pop()) {
                // memory refused first: what else went wrong may only follow from it
                let failure = verdict(&error);
                if first.is_none() || matches!(failure, Failure::Memory(_)) {
                    first = Some(failure);
                }
            }
        }
        first
    };
    let done = FRAME.with_borrow_mut(|frame| {
        frame.begin(renderer);
        let totals = stream(&mut |batch| time(Phase::GpuPack, || frame.add(renderer, raster, batch)));
        frame.draw(renderer, raster, background, &mut raised).map(|()| totals)
    });
    // (when nothing was drawn, or when the drawing failed before asking)
    let done = match (done, raised()) {
        (_, Some(memory @ Failure::Memory(_))) => Err(memory),
        (Ok(_), Some(failure)) => Err(failure),
        (done, _) => done,
    };
    match done {
        Ok(totals) => {
            renderer.pictures.fetch_add(1, Ordering::Relaxed);
            Some(totals)
        }
        Err(failure) => {
            // (a picture that failed after another one made the card give up: that one's cause)
            crate::render::gpu::chain::note(
                crate::render::gpu::chain::Kind::Drawing,
                &crate::render::gpu::chain::with_cause(&failure.reason(), renderer.cause.get().map(String::as_str)),
            );
            left_behind(renderer, failure);
            None
        }
    }
}

impl Failure {
    /// Why, in English on one line, for the fallbacks' lines and events.
    pub(crate) fn reason(&self) -> String {
        let one_line = |why: &str| why.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        match self {
            Failure::Picture(why) => format!("a picture the card does not take: {}", one_line(why)),
            Failure::Memory(why) => format!("card memory refused: {}", one_line(why)),
            Failure::Card(why) => format!("the graphics card failed: {}", one_line(why)),
        }
    }
}

/// What a picture the card did not draw means for the card: said once for a picture it does not
/// take; for memory refused, the thread lets go of what it holds there and the textures kept are
/// cut; for a failure of the card, it is given up.
fn left_behind(renderer: &Renderer, failure: Failure) {
    match failure {
        Failure::Picture(why) => {
            renderer.left.fetch_add(1, Ordering::Relaxed);
            if !renderer.said_picture.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "renderer: a picture is left to the CPU ({why}); so are the next ones in this case, without saying it again"
                );
            }
        }
        Failure::Memory(why) => {
            renderer.left.fetch_add(1, Ordering::Relaxed);
            // what the thread holds on the card may be what was refused: all of it is made again
            FRAME.with_borrow_mut(Frame::forget);
            renderer.short_of_memory(&why);
        }
        Failure::Card(why) => renderer.give_up(&why),
    }
}

thread_local! {
    /// This thread draws on the processor for now ([`on_processor`]).
    static PROCESSOR_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs `f` with everything this thread draws and shades done by the processor, whatever the
/// run chose (`gpu::paint` and `gpu::shade::factors` give None at once): a block the chain on the
/// card could not make is made again this way, as with `--renderer cpu --shading cpu`.
pub(crate) fn on_processor<R>(f: impl FnOnce() -> R) -> R {
    struct Back(bool);
    impl Drop for Back {
        fn drop(&mut self) {
            PROCESSOR_ONLY.with(|p| p.set(self.0));
        }
    }
    let _back = Back(PROCESSOR_ONLY.with(|p| p.replace(true)));
    f()
}

/// Whether this thread is in [`on_processor`].
pub(crate) fn processor_only() -> bool {
    PROCESSOR_ONLY.with(|p| p.get())
}

/// For the chain (`chain.rs`): the device and queue of the card that draws, and its name, None
/// when the processor draws or the card was given up. Opens the card first if it is to draw.
pub(crate) fn shared_device() -> Option<(wgpu::Device, wgpu::Queue, String)> {
    renderer().filter(|r| !r.down.load(Ordering::Relaxed)).map(|r| (r.device.clone(), r.queue.clone(), r.name.clone()))
}

/// Whether the card that draws was given up during the run (or never opened).
pub(crate) fn card_down() -> bool {
    RENDERER.get().and_then(Option::as_ref).is_none_or(|r| r.down.load(Ordering::Relaxed))
}

/// Why the card that draws was given up (its first failure), None while it was not.
pub(crate) fn cause() -> Option<&'static str> {
    RENDERER.get().and_then(Option::as_ref).and_then(|r| r.cause.get()).map(String::as_str)
}

/// Draws the batches `stream` hands out on the card, into a picture kept there for the chain
/// (`chain.rs`): nothing is read back, nothing is left to the CPU half. `raster` gives the size,
/// the projection, the filter and "heights only" (`keep` says how the picture begins; `Heights`
/// and `HeightsOver` want a raster in "heights only"); its own buffers are not used (they may be
/// empty). An error: the card did not draw it (memory refused, a picture it does not take, a
/// failure of the card: what it means for the card is done here, as for any picture), and the
/// chain makes its block again on the processor.
pub(crate) fn paint_kept(raster: &Raster, keep: Keep, stream: &mut Stream<'_>) -> Result<(Totals, Drawn), Failure> {
    let renderer = renderer().ok_or_else(|| Failure::Card("no graphics card".to_string()))?;
    paint_kept_on(renderer, raster, keep, stream)
}

fn paint_kept_on(
    renderer: &'static Renderer,
    raster: &Raster,
    keep: Keep,
    stream: &mut Stream<'_>,
) -> Result<(Totals, Drawn), Failure> {
    if renderer.down.load(Ordering::Relaxed) {
        return Err(Failure::Card(crate::render::gpu::chain::with_cause(
            "the graphics card failed earlier in the render",
            renderer.cause.get().map(String::as_str),
        )));
    }
    if raster.heights_only == matches!(keep, Keep::Colour(_)) || raster.width == 0 || raster.height == 0 {
        return Err(Failure::Picture("a picture the chain does not take".to_string()));
    }
    static WATCHED: std::sync::Once = std::sync::Once::new();
    WATCHED.call_once(|| renderer.watch());
    renderer.begin();
    let scopes = [wgpu::ErrorFilter::Validation, wgpu::ErrorFilter::OutOfMemory, wgpu::ErrorFilter::Internal]
        .map(|filter| renderer.device.push_error_scope(filter));
    let done = FRAME.with_borrow_mut(|frame| {
        frame.begin(renderer);
        let totals = stream(&mut |batch| time(Phase::GpuPack, || frame.add(renderer, raster, batch)));
        frame.draw_kept(renderer, raster, keep).map(|drawn| (totals, drawn))
    });
    let mut raised: Option<Failure> = None;
    for scope in scopes.into_iter().rev() {
        if let Some(error) = now(scope.pop()) {
            let failure = verdict(&error);
            if raised.is_none() || matches!(failure, Failure::Memory(_)) {
                raised = Some(failure);
            }
        }
    }
    let done = match (done, raised) {
        (_, Some(memory @ Failure::Memory(_))) => Err(memory),
        (Ok(_), Some(failure)) => Err(failure),
        (done, _) => done,
    };
    match done {
        Ok(done) => {
            renderer.pictures.fetch_add(1, Ordering::Relaxed);
            Ok(done)
        }
        Err(failure) => {
            left_behind(renderer, failure.clone());
            Err(failure)
        }
    }
}

/// What the card did, for the table of `--profile`: None when it drew nothing.
pub fn summary() -> Option<String> {
    RENDERER.get()?.as_ref().map(summary_of)
}

fn summary_of(renderer: &Renderer) -> String {
    let count = |n: &AtomicU64| n.load(Ordering::Relaxed);
    let short = match count(&renderer.short) {
        0 => String::new(),
        n => format!("; {n} pictures left for want of memory on the card (texture budget lowered)"),
    };
    format!(
        "  graphics card {}: {} pictures drawn, {} left to the CPU; {} textures uploaded ({:.0} MB), {} dropped to fit in {} MB{short}{}",
        renderer.name,
        count(&renderer.pictures),
        count(&renderer.left),
        count(&renderer.uploads),
        count(&renderer.uploaded) as f64 / 1e6,
        count(&renderer.dropped),
        renderer.budget.load(Ordering::Relaxed) / 1_000_000,
        if renderer.down.load(Ordering::Relaxed) { "; it failed during the run" } else { "" }
    )
}

/// `BTMS_GPU_COMPARE`: the folder the compared pictures go to (None with `1`: numbers only).
fn comparing() -> Option<&'static Option<PathBuf>> {
    static FOLDER: OnceLock<Option<Option<PathBuf>>> = OnceLock::new();
    FOLDER
        .get_or_init(|| {
            let value = std::env::var("BTMS_GPU_COMPARE").ok().filter(|v| !v.is_empty() && v != "0")?;
            Some((value != "1").then(|| PathBuf::from(value)))
        })
        .as_ref()
}

/// How far the card's pictures are from the CPU's.
#[derive(Default, Clone)]
struct Gap {
    pictures: u64,
    /// Pixels of the pictures with colours, by the largest difference of a channel, in 255ths.
    pixels: u64,
    same: u64,
    within2: u64,
    within8: u64,
    beyond: u64,
    largest: u32,
    /// Of the pixels more than 2 apart: those where both drew the same surface (depths within a
    /// millimetre, or nothing on both), and those where each drew another one.
    apart_same_surface: u64,
    apart_other_surface: u64,
    /// Pixels of all pictures, by the difference of their depths.
    depths: u64,
    depth_1mm: u64,
    depth_1cm: u64,
    depth_beyond: u64,
    /// Pixels only one of them drew on.
    only_cpu: u64,
    only_gpu: u64,
}

impl Gap {
    /// The gap between two pictures (colours and depths of every pixel; the colours are not
    /// looked at with `heights_only`). `each` is told every pixel: its place, how far apart its
    /// colours are in 255ths (0 with `heights_only`), and whether both drew the same surface there
    /// (depths within a millimetre, or nothing on both).
    fn measure(
        cpu: (&[[f32; 3]], &[f32]),
        card: (&[[f32; 3]], &[f32]),
        heights_only: bool,
        mut each: impl FnMut(usize, u32, bool),
    ) -> Gap {
        let mut gap = Gap { pictures: 1, ..Gap::default() };
        for (i, ((cc, cd), (gc, gd))) in cpu.0.iter().zip(cpu.1).zip(card.0.iter().zip(card.1)).enumerate() {
            gap.depths += 1;
            let same_surface = match (cd.is_finite(), gd.is_finite()) {
                (false, false) => {
                    gap.depth_1mm += 1;
                    true
                }
                (true, false) => {
                    gap.only_cpu += 1;
                    false
                }
                (false, true) => {
                    gap.only_gpu += 1;
                    false
                }
                (true, true) => {
                    let d = (cd - gd).abs();
                    if d <= 0.001 {
                        gap.depth_1mm += 1;
                    } else if d <= 0.01 {
                        gap.depth_1cm += 1;
                    } else {
                        gap.depth_beyond += 1;
                    }
                    d <= 0.001
                }
            };
            let mut apart = 0;
            if !heights_only {
                gap.pixels += 1;
                apart = (0..3).map(|k| (byte(cc[k]) as i32 - byte(gc[k]) as i32).unsigned_abs()).max().unwrap_or(0);
                gap.largest = gap.largest.max(apart);
                match apart {
                    0 => gap.same += 1,
                    1..=2 => gap.within2 += 1,
                    3..=8 => gap.within8 += 1,
                    _ => gap.beyond += 1,
                }
                if apart > 2 {
                    if same_surface {
                        gap.apart_same_surface += 1;
                    } else {
                        gap.apart_other_surface += 1;
                    }
                }
            }
            each(i, apart, same_surface);
        }
        gap
    }

    fn add(&mut self, other: &Gap) {
        self.pictures += other.pictures;
        self.pixels += other.pixels;
        self.same += other.same;
        self.within2 += other.within2;
        self.within8 += other.within8;
        self.beyond += other.beyond;
        self.largest = self.largest.max(other.largest);
        self.apart_same_surface += other.apart_same_surface;
        self.apart_other_surface += other.apart_other_surface;
        self.depths += other.depths;
        self.depth_1mm += other.depth_1mm;
        self.depth_1cm += other.depth_1cm;
        self.depth_beyond += other.depth_beyond;
        self.only_cpu += other.only_cpu;
        self.only_gpu += other.only_gpu;
    }

    fn say(&self) -> String {
        let share = |n: u64, of: u64| 100.0 * n as f64 / of.max(1) as f64;
        let mut out = String::new();
        if self.pixels > 0 {
            out += &format!(
                "colours ({} pixels): equal at 8 bits {:.4}%, within 2/255 {:.4}%, within 8/255 {:.4}%, beyond {} pixels ({:.4}%), largest gap {}/255; of the {} pixels more than 2/255 apart: {} on the same surface (depths within 1 mm), {} on another surface. ",
                self.pixels,
                share(self.same, self.pixels),
                share(self.same + self.within2, self.pixels),
                share(self.same + self.within2 + self.within8, self.pixels),
                self.beyond,
                share(self.beyond, self.pixels),
                self.largest,
                self.within8 + self.beyond,
                self.apart_same_surface,
                self.apart_other_surface
            );
        }
        out += &format!(
            "depths ({} pixels): within 1 mm {:.4}%, within 1 cm {:.4}%, beyond {}; drawn by the CPU only {}, by the card only {}",
            self.depths,
            share(self.depth_1mm, self.depths),
            share(self.depth_1mm + self.depth_1cm, self.depths),
            self.depth_beyond,
            self.only_cpu,
            self.only_gpu
        );
        out
    }
}

static GAP: Mutex<Option<Gap>> = Mutex::new(None);
static COMPARED: AtomicU64 = AtomicU64::new(0);

/// A colour as the pictures store it: 8 bits per channel.
fn byte(c: f32) -> u8 {
    (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// `BTMS_GPU_COMPARE`: the picture drawn by the CPU half first (the raster is put back as it
/// was), then by the card, and both compared; the raster ends as the card drew it.
fn compared(
    renderer: &'static Renderer,
    raster: &mut Raster,
    stream: &mut Stream<'_>,
    folder: &Option<PathBuf>,
) -> Option<Totals> {
    let before = (raster.colour.clone(), raster.depth.clone());
    stream(&mut |batch| raster.draw(batch));
    let cpu = (std::mem::replace(&mut raster.colour, before.0), std::mem::replace(&mut raster.depth, before.1));
    let done = card_paint(renderer, raster, stream)?;
    let number = COMPARED.fetch_add(1, Ordering::Relaxed) + 1;
    let (width, heights_only) = (raster.width, raster.heights_only);
    // where the differences are: the picture dimmed where there is none, then by size and by cause
    let mut marks = vec![0u8; raster.colour.len() * 3];
    let mut worst: Vec<(u32, usize)> = Vec::new();
    let gap =
        Gap::measure((&cpu.0, &cpu.1), (&raster.colour, &raster.depth), heights_only, |i, apart, same_surface| {
            let mark: [u8; 3] = match (apart, same_surface) {
                (_, false) if heights_only => [255, 0, 255],
                (_, _) if heights_only => [0, 0, 0],
                (0, _) => {
                    let grey = (cpu.0[i].map(|c| byte(c) as u32).iter().sum::<u32>() / 12) as u8;
                    [grey, grey, grey]
                }
                (1..=2, _) => [0, 70, 170],
                (3..=8, _) => [255, 220, 0],
                (_, true) => [255, 120, 0],
                (_, false) => [255, 0, 0],
            };
            marks[i * 3..i * 3 + 3].copy_from_slice(&mark);
            if apart > 2 {
                worst.push((apart, i));
            }
        });
    let kind = if heights_only { "heights only" } else { "top-down" };
    eprintln!("comparison {number} ({}×{} px, {kind}): {}", raster.width, raster.height, gap.say());
    worst.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (apart, i) in worst.iter().take(4) {
        let (x, y) = (i % width, i / width);
        eprintln!(
            "  gap {apart}/255 at ({x}, {y}): CPU {:?} at {:.3} m, card {:?} at {:.3} m",
            cpu.0[*i].map(byte),
            cpu.1[*i],
            raster.colour[*i].map(byte),
            raster.depth[*i]
        );
    }
    if let Some(folder) = folder {
        let save = |name: &str, rgb: &[u8]| -> Result<()> {
            std::fs::create_dir_all(folder).with_context(|| format!("creating {}", folder.display()))?;
            save_rgb(&folder.join(format!("image_{number:03}_{name}.png")), raster.width, raster.height, rgb)
        };
        let rgb = |colours: &[[f32; 3]]| -> Vec<u8> { colours.iter().flat_map(|c| c.map(byte)).collect() };
        // depths as greys between the nearest and the farthest drawn (heights only have no colours)
        let greys = |depths: &[f32]| -> Vec<u8> {
            let (lo, hi) = depths
                .iter()
                .filter(|d| d.is_finite())
                .fold((f32::MAX, f32::MIN), |(lo, hi), d| (lo.min(*d), hi.max(*d)));
            depths
                .iter()
                .flat_map(|d| {
                    [if d.is_finite() { (255.0 - 235.0 * (d - lo) / (hi - lo).max(1e-6)) as u8 } else { 0 }; 3]
                })
                .collect()
        };
        let pictures =
            if heights_only { (greys(&cpu.1), greys(&raster.depth)) } else { (rgb(&cpu.0), rgb(&raster.colour)) };
        let saved =
            save("cpu", &pictures.0).and_then(|()| save("gpu", &pictures.1)).and_then(|()| save("diff", &marks));
        if let Err(e) = saved {
            eprintln!("comparison {number}: {e:#}");
        }
    }
    GAP.lock().unwrap().get_or_insert_with(Gap::default).add(&gap);
    Some(done)
}

fn save_rgb(path: &Path, width: usize, height: usize, rgb: &[u8]) -> Result<()> {
    let file = std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(rgb)?;
    Ok(())
}

/// At the end of a run: the total of `BTMS_GPU_COMPARE`, when it was asked for.
pub fn finish() {
    if let Some(gap) = GAP.lock().unwrap().as_ref() {
        println!("comparison of the CPU and the graphics card, {} pictures: {}", gap.pictures, gap.say());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::raster::{Image, Projection};

    /// The card of the tests, or None on a machine without one (the tests then check nothing).
    fn card() -> Option<&'static Renderer> {
        static CARD: OnceLock<Option<Renderer>> = OnceLock::new();
        CARD.get_or_init(|| match Renderer::open(false) {
            Ok(renderer) => Some(renderer),
            Err(why) => {
                eprintln!("no graphics card for the tests: {why}");
                None
            }
        })
        .as_ref()
    }

    /// A small deterministic generator (xorshift64*).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn unit(&mut self) -> f32 {
            (self.next() >> 40) as f32 / (1u64 << 24) as f32
        }

        fn range(&mut self, lo: f32, hi: f32) -> f32 {
            lo + (hi - lo) * self.unit()
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }

        /// A texture of soft patches: neighbouring texels are close, as in a real one.
        // 6.28 is the test pattern's period, deliberately not exactly τ: the test pictures stay the same
        #[allow(clippy::approx_constant)]
        fn image(&mut self, width: usize, height: usize) -> Image {
            let phase: Vec<f32> = (0..12).map(|_| self.range(0.0, 6.28)).collect();
            let mut rgba = Vec::with_capacity(width * height * 4);
            for y in 0..height {
                for x in 0..width {
                    let (u, v) = (x as f32 / width as f32 * 6.28, y as f32 / height as f32 * 6.28);
                    for k in 0..4 {
                        let wave = (u * (1 + k) as f32 + phase[k * 3]).sin()
                            * (v * (2 - k % 2) as f32 + phase[k * 3 + 1]).cos();
                        rgba.push((127.5 + 120.0 * wave) as u8);
                    }
                }
            }
            Image { width, height, rgba }
        }
    }

    struct Arrays {
        positions: Vec<[f32; 3]>,
        uvs: Vec<[f32; 2]>,
        tints: Vec<[f32; 4]>,
        weights: Vec<[f32; 4]>,
        uvs1: Vec<[f32; 2]>,
        indices: Vec<u16>,
    }

    /// Triangles of a few pixels to a third of the picture, at random heights, none on a pixel
    /// centre (where the two halves are known to differ).
    fn random_batch(rng: &mut Rng, layered: bool, masked: bool) -> Arrays {
        let mut r =
            Arrays { positions: vec![], uvs: vec![], tints: vec![], weights: vec![], uvs1: vec![], indices: vec![] };
        for _ in 0..60 {
            let centre = [rng.range(-70.0, 70.0), rng.range(-45.0, 45.0)];
            let size = [2.0, 6.0, 20.0, 60.0][rng.below(4)];
            let height = rng.range(0.0, 40.0);
            for _ in 0..3 {
                r.indices.push(r.positions.len() as u16);
                r.positions.push([
                    centre[0] + rng.range(-size, size),
                    centre[1] + rng.range(-size, size),
                    height + rng.range(-3.0, 3.0),
                ]);
                r.uvs.push([rng.range(-2.0, 2.0), rng.range(-2.0, 2.0)]);
                r.tints.push([rng.range(0.3, 1.0), rng.range(0.3, 1.0), rng.range(0.3, 1.0), rng.unit()]);
                if layered {
                    let (a, b) = (rng.unit(), rng.unit());
                    r.weights.push([(1.0 - a) * (1.0 - b), a * (1.0 - b), (1.0 - a) * b, a * b]);
                    if masked {
                        r.uvs1.push([rng.range(-1.0, 1.0), rng.range(-1.0, 1.0)]);
                    }
                }
            }
        }
        r
    }

    /// Random pictures drawn by both halves: opaque, cut out and blended batches of every
    /// material over one another, seen from above. The card must give the CPU's
    /// picture but for a few pixels (edges, ties): the bound is far above what is measured, and
    /// far below what a wrong formula gives.
    #[test]
    fn the_card_draws_what_the_processor_draws() {
        let Some(renderer) = card() else { return };
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let textures: Vec<Tex> = [(64, 64), (128, 32), (37, 53), (256, 256), (1, 1)]
            .iter()
            .map(|&(w, h)| Tex::new(rng.image(w, h)))
            .collect();
        let projections = [
            Projection::Orthographic { centre: [0.0, 0.0], metres: 0.5, top: 100.0 },
            Projection::Orthographic { centre: [3.3, -7.1], metres: 0.37, top: 55.5 },
        ];
        let mut total = Gap::default();
        for round in 0..24 {
            let projection = projections[round % projections.len()];
            let new = || Raster::new(320, 200, projection, [0.1, 0.2, 0.3]);
            let (mut cpu, mut gpu) = (new(), new());
            let mut batches: Vec<(Mode, Material, Arrays)> = Vec::new();
            for step in 0..9 {
                let mode = [
                    Mode::Opaque,
                    Mode::Opaque,
                    Mode::Opaque,
                    Mode::Cutout,
                    Mode::Cutout,
                    Mode::Cutout,
                    Mode::Blend,
                    Mode::Blend,
                    Mode::Blend,
                ][step];
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
                batches.push((mode, material, random_batch(&mut rng, layered, masked)));
            }
            let mut stream = |sink: &mut dyn FnMut(&Batch)| -> Totals {
                for (mode, material, r) in &batches {
                    sink(&Batch {
                        mode: *mode,
                        material: material.clone(),
                        positions: &r.positions,
                        uvs: &r.uvs,
                        tints: &r.tints,
                        weights: &r.weights,
                        uvs1: &r.uvs1,
                        indices: &r.indices,
                    });
                }
                (0, [0; 8])
            };
            stream(&mut |batch| cpu.draw(batch));
            assert!(card_paint(renderer, &mut gpu, &mut stream).is_some(), "round {round}: the card did not draw");
            total.add(&Gap::measure((&cpu.colour, &cpu.depth), (&gpu.colour, &gpu.depth), false, |_, _, _| {}));
        }
        println!("{}", total.say());
        // the test is worth something only if triangles did reach the pictures
        assert!(total.depth_1mm > total.depths / 2, "{}", total.say());
        let (apart, depths_apart) =
            (total.within8 + total.beyond, total.depth_1cm + total.depth_beyond + total.only_cpu + total.only_gpu);
        assert!(apart * 200 < total.pixels, "more than 0.5 % of the pixels over 2/255 apart: {}", total.say());
        assert!(depths_apart * 200 < total.depths, "more than 0.5 % of the depths apart: {}", total.say());
    }

    /// Heights only: nothing blended, opaque batches without their textures, the raster's
    /// filter applied, and a second drawing merged into the first (the nearer depth wins).
    #[test]
    fn heights_are_merged_as_the_processor_draws_them() {
        let Some(renderer) = card() else { return };
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let textures: Vec<Tex> = [(64, 64), (32, 32)].iter().map(|&(w, h)| Tex::new(rng.image(w, h))).collect();
        let projection = Projection::Orthographic { centre: [0.0, 0.0], metres: 0.5, top: 100.0 };
        let (mut apart, mut pixels, mut drawn) = (0u64, 0u64, 0u64);
        for round in 0..12 {
            let new = || {
                let mut raster = Raster::new(320, 200, projection, [0.0; 3]);
                // the filter: triangles wholly west of a line are refused
                let limit = -20.0 + round as f32 * 4.0;
                raster.set_pass(Some(Box::new(move |t: &[[f32; 3]; 3]| !t.iter().all(|p| p[0] < limit))), true);
                raster
            };
            let (mut cpu, mut gpu) = (new(), new());
            // two drawings into the same raster, as the shading does (near, then far)
            for _ in 0..2 {
                let mut batches: Vec<(Mode, Material, Arrays)> = Vec::new();
                for step in 0..6 {
                    let mode = [Mode::Opaque, Mode::Cutout, Mode::Blend][step % 3];
                    let material = if step < 3 {
                        Material::Single(Some(textures[step % 2].clone()))
                    } else {
                        Material::Single(None)
                    };
                    batches.push((mode, material, random_batch(&mut rng, false, false)));
                }
                let mut stream = |sink: &mut dyn FnMut(&Batch)| -> Totals {
                    for (mode, material, r) in &batches {
                        sink(&Batch {
                            mode: *mode,
                            material: material.clone(),
                            positions: &r.positions,
                            uvs: &r.uvs,
                            tints: &r.tints,
                            weights: &r.weights,
                            uvs1: &r.uvs1,
                            indices: &r.indices,
                        });
                    }
                    (0, [0; 8])
                };
                stream(&mut |batch| cpu.draw(batch));
                assert!(card_paint(renderer, &mut gpu, &mut stream).is_some(), "round {round}: the card did not draw");
            }
            for (c, g) in cpu.depth.iter().zip(&gpu.depth) {
                pixels += 1;
                drawn += c.is_finite() as u64;
                if c.is_finite() != g.is_finite() || (c.is_finite() && (c - g).abs() > 0.001) {
                    apart += 1;
                }
            }
        }
        println!("{apart} depths apart of {pixels} ({drawn} drawn)");
        assert!(drawn > pixels / 4, "{drawn} pixels drawn of {pixels}");
        assert!(apart * 200 < pixels, "{apart} depths apart of {pixels}");
    }

    /// Texture coordinates that are not numbers: the CPU half reads texels that are not numbers
    /// with them, and the pixels end black (drawn when opaque, not drawn when cut out, black over
    /// whatever is there when blended; a terrain's mask the same). The card must give the same
    /// pixels: every pixel that is not a number in one picture is not one in the other, but for
    /// the edges.
    #[test]
    fn coordinates_that_are_not_numbers_end_as_on_the_processor() {
        let Some(renderer) = card() else { return };
        let mut rng = Rng(0xD1B5_4A32_D192_ED03);
        let textures: Vec<Tex> =
            [(64, 64), (32, 32), (16, 16)].iter().map(|&(w, h)| Tex::new(rng.image(w, h))).collect();
        let projection = Projection::Orthographic { centre: [0.0, 0.0], metres: 0.5, top: 100.0 };
        let (mut pixels, mut lost, mut apart) = (0u64, 0u64, 0u64);
        for round in 0..18 {
            let new = || Raster::new(320, 200, projection, [0.1, 0.2, 0.3]);
            let (mut cpu, mut gpu) = (new(), new());
            let mut batches: Vec<(Mode, Material, Arrays)> = Vec::new();
            // a sound opaque ground, then batches of which a few vertices have lost their coordinates
            for step in 0..5 {
                let mode = [Mode::Opaque, Mode::Opaque, Mode::Cutout, Mode::Blend, Mode::Blend][step];
                let kind = (round + step) % 3;
                let (layered, masked) = (kind >= 1, kind == 2);
                let material = match kind {
                    0 => Material::Single(Some(textures[step % 3].clone())),
                    _ => Material::Layers {
                        layers: [Some(textures[0].clone()), None, Some(textures[1].clone()), Some(textures[2].clone())],
                        mask: masked.then(|| Mask { texture: textures[step % 3].clone(), channels: (2, 1) }),
                    },
                };
                let mut r = random_batch(&mut rng, layered, masked);
                if step > 0 {
                    for i in 0..r.uvs.len() {
                        if rng.below(6) == 0 {
                            let bad = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY][rng.below(3)];
                            // the first coordinates, or the mask's
                            if masked && rng.below(2) == 0 {
                                r.uvs1[i][rng.below(2)] = bad
                            } else {
                                r.uvs[i][rng.below(2)] = bad
                            }
                        }
                    }
                }
                batches.push((mode, material, r));
            }
            let mut stream = |sink: &mut dyn FnMut(&Batch)| -> Totals {
                for (mode, material, r) in &batches {
                    sink(&Batch {
                        mode: *mode,
                        material: material.clone(),
                        positions: &r.positions,
                        uvs: &r.uvs,
                        tints: &r.tints,
                        weights: &r.weights,
                        uvs1: &r.uvs1,
                        indices: &r.indices,
                    });
                }
                (0, [0; 8])
            };
            stream(&mut |batch| cpu.draw(batch));
            assert!(card_paint(renderer, &mut gpu, &mut stream).is_some(), "round {round}: the card did not draw");
            for (c, g) in cpu.colour.iter().zip(&gpu.colour) {
                pixels += 1;
                lost += c[0].is_nan() as u64;
                apart += (c[0].is_nan() != g[0].is_nan()) as u64;
            }
        }
        println!("{lost} pixels that are not numbers of {pixels}, {apart} of them on one side only");
        assert!(lost > pixels / 50, "{lost} pixels lost of {pixels}: the test checks nothing");
        assert!(apart * 100 < lost, "{apart} pixels are not numbers on one side only, of {lost}");
    }

    /// A picture that already holds colours is left to the CPU half, untouched.
    #[test]
    fn a_picture_already_drawn_on_is_left_to_the_processor() {
        let Some(renderer) = card() else { return };
        let mut raster = Raster::new(
            64,
            48,
            Projection::Orthographic { centre: [0.0, 0.0], metres: 1.0, top: 100.0 },
            [0.1, 0.2, 0.3],
        );
        assert_eq!(untouched(&raster), Some([0.1, 0.2, 0.3]));
        raster.colour[100] = [0.5; 3];
        assert_eq!(untouched(&raster), None);
        let before = raster.digest();
        let mut called = 0;
        let mut stream = |_: &mut dyn FnMut(&Batch)| -> Totals {
            called += 1;
            (0, [0; 8])
        };
        assert!(card_paint(renderer, &mut raster, &mut stream).is_none());
        assert_eq!((called, raster.digest()), (0, before));
        raster.colour[100] = [0.1, 0.2, 0.3];
        raster.depth[7] = 3.0;
        assert_eq!(untouched(&raster), None);
    }

    #[test]
    fn the_pipelines_are_those_of_the_modes() {
        assert_eq!(pipeline_of(false, Mode::Opaque, false), SINGLE_OPAQUE);
        assert_eq!(pipeline_of(false, Mode::Blend, false), SINGLE_OPAQUE + 2);
        assert_eq!(pipeline_of(true, Mode::Cutout, false), LAYERS_OPAQUE + 1);
        assert_eq!(pipeline_of(true, Mode::Opaque, true), HEIGHT);
        assert_eq!(pipeline_of(false, Mode::Cutout, true), HEIGHT_SINGLE_CUTOUT);
        assert_eq!(pipeline_of(true, Mode::Cutout, true), HEIGHT_LAYERS_CUTOUT);
        for pipeline in 0..9 {
            assert_eq!(takes_layers(pipeline), matches!(pipeline, 3 | 4 | 5 | 8), "{pipeline}");
        }
    }

    #[test]
    fn the_option_is_taken_out_of_the_arguments() {
        let parse = |args: &[&str]| {
            let mut args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
            chosen(&mut args, None).ok().map(|choice| (choice, args))
        };
        assert_eq!(
            parse(&["tiles", "--renderer", "cpu", "output"]),
            Some((Choice::Cpu, vec!["tiles".to_string(), "output".to_string()]))
        );
        assert_eq!(parse(&["tiles", "--renderer", "gpu", "--zone", "0,0,1,1"]).map(|p| p.0), Some(Choice::Gpu));
        assert_eq!(parse(&["tiles", "output"]).map(|p| p.0), Some(Choice::Auto));
        assert_eq!(parse(&["tiles", "--renderer", "turbo"]), None);
        assert_eq!(parse(&["tiles", "--renderer"]), None);
        // the default from outside, and the option over it
        let with = |default: &str, args: &[&str]| {
            chosen(&mut args.iter().map(|a| a.to_string()).collect(), Some(default)).ok()
        };
        assert_eq!(with("cpu", &["tiles"]), Some(Choice::Cpu));
        assert_eq!(with("cpu", &["tiles", "--renderer", "gpu"]), Some(Choice::Gpu));
        assert_eq!(with("turbo", &["tiles"]), None);
    }

    #[test]
    fn the_budget_of_the_textures_is_the_option_then_the_variable() {
        assert_eq!(budget_of(None, None), 4_000_000_000);
        assert_eq!(budget_of(None, Some("2048")), 2_048_000_000);
        assert_eq!(budget_of(None, Some(" 512 ")), 512_000_000);
        assert_eq!(budget_of(None, Some("plenty")), 4_000_000_000);
        assert_eq!(budget_of(Some(12_500_000_000), Some("2048")), 12_500_000_000);
    }

    #[test]
    fn the_textures_on_the_card_keep_within_the_budget() {
        // (last use, id, bytes)
        let entries = vec![(5, 50, 400), (1, 10, 300), (3, 30, 200), (2, 20, 100), (4, 40, 500)];
        // within the budget: nothing goes
        assert_eq!(to_drop(entries.clone(), 1_500, 1_500), (vec![], 0));
        // beyond it: the least recently used first, down to three quarters of it
        let (gone, freed) = to_drop(entries.clone(), 1_500, 1_000);
        assert_eq!((gone, freed), (vec![10, 20, 30, 40], 1_100));
        assert!(1_500 - freed <= 1_000 / 4 * 3);
        // no more than it takes: at 1,300 held, two go (300 and 100 bytes) and 900 stay, over 750…
        // then the third (200) brings it to 700
        assert_eq!(
            to_drop(vec![(1, 10, 300), (2, 20, 100), (3, 30, 200), (4, 40, 700)], 1_300, 1_000),
            (vec![10, 20, 30], 600)
        );
        // everything, when that is what it takes
        assert_eq!(to_drop(entries, 1_500, 4).1, 1_500);
    }

    thread_local! {
        /// The next thing this thread makes on the card is refused, as by a full card
        /// (`Renderer::made`).
        pub(super) static REFUSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    #[test]
    fn memory_refused_leaves_the_picture_and_anything_else_the_card() {
        let source = || -> wgpu::ErrorSource { Box::new(std::io::Error::other("test")) };
        assert!(matches!(verdict(&wgpu::Error::OutOfMemory { source: source() }), Failure::Memory(_)));
        assert!(matches!(
            verdict(&wgpu::Error::Validation { source: source(), description: "test".into() }),
            Failure::Card(_)
        ));
        assert!(matches!(
            verdict(&wgpu::Error::Internal { source: source(), description: "test".into() }),
            Failure::Card(_)
        ));
    }

    #[test]
    fn a_card_short_of_memory_keeps_three_quarters_of_its_textures() {
        // three quarters of what was held, or of the budget when less was allowed
        assert_eq!(shrunk(16_000_000_000, 17_500_000_000, MIN_TEXTURE_BUDGET), Some(12_000_000_000));
        assert_eq!(shrunk(20_000_000_000, 17_500_000_000, MIN_TEXTURE_BUDGET), Some(13_125_000_000));
        // then again, lower each time
        assert_eq!(shrunk(12_000_000_000, 12_000_000_000, MIN_TEXTURE_BUDGET), Some(9_000_000_000));
        // too little to give back: the card is full of something else
        assert_eq!(shrunk(300_000_000, 4_000_000_000, MIN_TEXTURE_BUDGET), None);
        assert_eq!(shrunk(0, 4_000_000_000, MIN_TEXTURE_BUDGET), None);
        assert_eq!(shrunk(342_000_000, 4_000_000_000, MIN_TEXTURE_BUDGET), Some(256_500_000));
    }

    /// A card of its own for a test that cuts its budget or gives it up (the others share
    /// [`card`]): kept to the end, watched as a run's is.
    fn own_card(floor: usize) -> Option<&'static Renderer> {
        card()?;
        let mut renderer = Renderer::open(false).ok()?;
        renderer.floor = floor;
        let renderer: &'static Renderer = Box::leak(Box::new(renderer));
        renderer.watch();
        Some(renderer)
    }

    /// Pictures of textured batches over one another, one texture per batch.
    fn textured(rng: &mut Rng, textures: &[Tex]) -> Vec<(Mode, Material, Arrays)> {
        textures
            .iter()
            .enumerate()
            .map(|(k, t)| {
                (
                    [Mode::Opaque, Mode::Cutout, Mode::Blend][k % 3],
                    Material::Single(Some(t.clone())),
                    random_batch(rng, false, false),
                )
            })
            .collect()
    }

    /// Draws `batches` on both halves; the card's picture, or None when it left it.
    fn both(renderer: &'static Renderer, batches: &[(Mode, Material, Arrays)]) -> Option<Gap> {
        let projection = Projection::Orthographic { centre: [0.0, 0.0], metres: 0.5, top: 100.0 };
        let new = || Raster::new(320, 200, projection, [0.1, 0.2, 0.3]);
        let (mut cpu, mut gpu) = (new(), new());
        let mut stream = |sink: &mut dyn FnMut(&Batch)| -> Totals {
            for (mode, material, r) in batches {
                sink(&Batch {
                    mode: *mode,
                    material: material.clone(),
                    positions: &r.positions,
                    uvs: &r.uvs,
                    tints: &r.tints,
                    weights: &r.weights,
                    uvs1: &r.uvs1,
                    indices: &r.indices,
                });
            }
            (0, [0; 8])
        };
        stream(&mut |batch| cpu.draw(batch));
        let before = gpu.digest();
        if card_paint(renderer, &mut gpu, &mut stream).is_none() {
            // left: the raster is as it was
            assert_eq!(gpu.digest(), before);
            return None;
        }
        Some(Gap::measure((&cpu.colour, &cpu.depth), (&gpu.colour, &gpu.depth), false, |_, _, _| {}))
    }

    fn close(gap: &Gap) {
        assert!(gap.depth_1mm > gap.depths / 2, "{}", gap.say());
        let (apart, depths_apart) =
            (gap.within8 + gap.beyond, gap.depth_1cm + gap.depth_beyond + gap.only_cpu + gap.only_gpu);
        assert!(apart * 200 < gap.pixels, "more than 0.5 % of the pixels over 2/255 apart: {}", gap.say());
        assert!(depths_apart * 200 < gap.depths, "more than 0.5 % of the depths apart: {}", gap.say());
    }

    /// The card refuses memory for a texture (as a full card does; the refusal is made up: filling
    /// the card for real would take it from whatever else runs on it): that picture is left to
    /// the processor, untouched; the thread lets go of what it held on the card, the refused
    /// texture is not kept, the budget of the textures falls to three quarters of what they held
    /// and those beyond leave the card. The card stays in use: the next pictures are its own
    /// again, as close to the processor's as ever. A second refusal within the pause cuts nothing
    /// more.
    #[test]
    fn a_card_short_of_memory_keeps_drawing() {
        let Some(renderer) = own_card(1) else { return };
        let mut rng = Rng(0x5851_F42D_4C95_7F2D);
        let textures: Vec<Tex> = (0..10).map(|k| Tex::new(rng.image(64 + 16 * k, 64))).collect();
        let first = textured(&mut rng, &textures[..8]);
        close(&both(renderer, &first).expect("the card did not draw"));
        let held = renderer.held.load(Ordering::Relaxed);
        assert!(held > 0);
        assert!(FRAME.with_borrow(|f| !f.targets.is_empty()));

        // the ninth texture is new, and refused
        let second = textured(&mut rng, &textures[..9]);
        REFUSE.with(|r| r.set(true));
        assert!(both(renderer, &second).is_none(), "a picture the card refused memory for was drawn");
        assert!(!renderer.down.load(Ordering::Relaxed), "the card was given up");
        assert_eq!(renderer.short.load(Ordering::Relaxed), 1);
        let budget = renderer.budget.load(Ordering::Relaxed);
        assert_eq!(budget, held / 4 * 3);
        assert!(renderer.held.load(Ordering::Relaxed) <= budget / 4 * 3);
        assert!(renderer.dropped.load(Ordering::Relaxed) > 0);
        assert!(!renderer.textures.lock().unwrap().map.contains_key(&textures[8].id), "the refused texture is kept");
        assert!(FRAME.with_borrow(|f| f.targets.is_empty()
            && f.index_buffer.is_none()
            && f.plain.buffers.iter().all(Option::is_none)));

        // drawn by the card again, the refused texture sent this time
        close(&both(renderer, &second).expect("the card did not draw after the shortage"));
        assert!(renderer.textures.lock().unwrap().map.contains_key(&textures[8].id));

        // within the pause: left, nothing more cut
        let cut = renderer.textures.lock().unwrap().shrunk_at.expect("no cut");
        let third = textured(&mut rng, &textures);
        REFUSE.with(|r| r.set(true));
        assert!(both(renderer, &third).is_none());
        assert_eq!(renderer.short.load(Ordering::Relaxed), 2);
        if cut.elapsed() < SHORTAGE_PAUSE {
            assert_eq!(renderer.budget.load(Ordering::Relaxed), budget, "cut again within the pause");
        } else {
            println!("(the card was slow: the second refusal came after the pause, and may have cut again)");
        }
        assert!(!renderer.down.load(Ordering::Relaxed));
        close(&both(renderer, &third).expect("the card did not draw after the second shortage"));
        assert_eq!(renderer.pictures.load(Ordering::Relaxed), 3);
        println!("{}", summary_of(renderer));
    }

    /// Memory refused while the textures hold too little to give any back: the card is full of
    /// something else, and given up (the rest is drawn by the processor).
    #[test]
    fn a_card_full_of_something_else_is_given_up() {
        let Some(renderer) = own_card(MIN_TEXTURE_BUDGET) else { return };
        let mut rng = Rng(0x2127_599B_F432_5C37);
        let textures: Vec<Tex> = (0..3).map(|_| Tex::new(rng.image(32, 32))).collect();
        close(&both(renderer, &textured(&mut rng, &textures[..2])).expect("the card did not draw"));
        REFUSE.with(|r| r.set(true));
        assert!(both(renderer, &textured(&mut rng, &textures)).is_none());
        assert!(renderer.down.load(Ordering::Relaxed), "a card too full to give anything back was kept");
        // a refusal reported once the card is given up (as with a device wgpu lost) cuts nothing
        let before = (renderer.short.load(Ordering::Relaxed), renderer.budget.load(Ordering::Relaxed));
        renderer.short_of_memory("after giving up");
        assert_eq!((renderer.short.load(Ordering::Relaxed), renderer.budget.load(Ordering::Relaxed)), before);
    }

    /// Many threads drawing at once, as `--threads auto` has them with a card (one and a half per
    /// core), some uploading textures of their own while the others draw: each picture is still
    /// the processor's, to the bounds of the test above.
    #[test]
    fn threads_drawing_at_once_draw_what_one_draws() {
        let Some(renderer) = card() else { return };
        let mut rng = Rng(0x1234_5678_9ABC_DEF1);
        let shared: Vec<Tex> =
            [(64, 64), (128, 32), (37, 53)].iter().map(|&(w, h)| Tex::new(rng.image(w, h))).collect();
        let total = Mutex::new(Gap::default());
        std::thread::scope(|s| {
            for t in 0..12u64 {
                let (shared, total) = (&shared, &total);
                s.spawn(move || {
                    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ (t + 1).wrapping_mul(0xBF58_476D_1CE4_E5B9));
                    let own: Vec<Tex> = (0..2).map(|k| Tex::new(rng.image(16 + 8 * k, 24))).collect();
                    let projection = Projection::Orthographic { centre: [0.0, 0.0], metres: 0.5, top: 100.0 };
                    let mut gap = Gap::default();
                    for _ in 0..6 {
                        let new = || Raster::new(320, 200, projection, [0.1, 0.2, 0.3]);
                        let (mut cpu, mut gpu) = (new(), new());
                        let mut batches: Vec<(Mode, Material, Arrays)> = Vec::new();
                        for step in 0..6 {
                            let mode =
                                [Mode::Opaque, Mode::Opaque, Mode::Cutout, Mode::Cutout, Mode::Blend, Mode::Blend]
                                    [step];
                            let texture = if rng.below(2) == 0 {
                                shared[rng.below(shared.len())].clone()
                            } else {
                                own[rng.below(own.len())].clone()
                            };
                            batches.push((mode, Material::Single(Some(texture)), random_batch(&mut rng, false, false)));
                        }
                        let mut stream = |sink: &mut dyn FnMut(&Batch)| -> Totals {
                            for (mode, material, r) in &batches {
                                sink(&Batch {
                                    mode: *mode,
                                    material: material.clone(),
                                    positions: &r.positions,
                                    uvs: &r.uvs,
                                    tints: &r.tints,
                                    weights: &r.weights,
                                    uvs1: &r.uvs1,
                                    indices: &r.indices,
                                });
                            }
                            (0, [0; 8])
                        };
                        stream(&mut |batch| cpu.draw(batch));
                        assert!(
                            card_paint(renderer, &mut gpu, &mut stream).is_some(),
                            "thread {t}: the card did not draw"
                        );
                        gap.add(&Gap::measure(
                            (&cpu.colour, &cpu.depth),
                            (&gpu.colour, &gpu.depth),
                            false,
                            |_, _, _| {},
                        ));
                    }
                    total.lock().unwrap().add(&gap);
                });
            }
        });
        let total = total.into_inner().unwrap();
        println!("{}", total.say());
        assert!(total.depth_1mm > total.depths / 2, "{}", total.say());
        let (apart, depths_apart) =
            (total.within8 + total.beyond, total.depth_1cm + total.depth_beyond + total.only_cpu + total.only_gpu);
        assert!(apart * 200 < total.pixels, "more than 0.5 % of the pixels over 2/255 apart: {}", total.say());
        assert!(depths_apart * 200 < total.depths, "more than 0.5 % of the depths apart: {}", total.say());
    }

    /// A thread that waits for its picture must not hold anything the others need: while one
    /// thread draws pictures the card is long at (2048 × 2048, blended over and over), another
    /// uploads textures and hands the card commands, which take the locks of this file and those
    /// of wgpu's queue. Its times are to stay far below the first one's.
    #[test]
    #[ignore = "a measure: cargo test --release -- --ignored card_wait"]
    fn card_wait_blocks_no_other_thread() {
        let Some(renderer) = card() else { return };
        let stop = AtomicBool::new(false);
        let (pictures, uploads, submits) = (Mutex::new(Vec::new()), Mutex::new(Vec::new()), Mutex::new(Vec::new()));
        std::thread::scope(|s| {
            s.spawn(|| {
                // 600 blended triangles over the whole picture
                let mut r = Arrays {
                    positions: vec![],
                    uvs: vec![],
                    tints: vec![],
                    weights: vec![],
                    uvs1: vec![],
                    indices: vec![],
                };
                for k in 0..600u16 {
                    for p in [[-100.0, -100.0], [300.0, -100.0], [-100.0, 300.0]] {
                        r.indices.push(r.positions.len() as u16);
                        r.positions.push([p[0], p[1], 10.0 + k as f32 * 0.01]);
                        r.uvs.push([0.0, 0.0]);
                        r.tints.push([0.5, 0.6, 0.7, 0.1]);
                    }
                }
                let mut stream = |sink: &mut dyn FnMut(&Batch)| -> Totals {
                    sink(&Batch {
                        mode: Mode::Blend,
                        material: Material::Single(None),
                        positions: &r.positions,
                        uvs: &r.uvs,
                        tints: &r.tints,
                        weights: &r.weights,
                        uvs1: &r.uvs1,
                        indices: &r.indices,
                    });
                    (0, [0; 8])
                };
                for _ in 0..24 {
                    let mut raster = Raster::new(
                        2048,
                        2048,
                        Projection::Orthographic { centre: [0.0, 0.0], metres: 0.0625, top: 100.0 },
                        [0.0; 3],
                    );
                    let t = Instant::now();
                    assert!(card_paint(renderer, &mut raster, &mut stream).is_some());
                    pictures.lock().unwrap().push(t.elapsed());
                }
                stop.store(true, Ordering::Relaxed);
            });
            s.spawn(|| {
                let mut rng = Rng(6);
                while !stop.load(Ordering::Relaxed) {
                    let texture = Tex::new(rng.image(64, 64));
                    let t = Instant::now();
                    assert!(renderer.texture(&texture).is_ok());
                    uploads.lock().unwrap().push(t.elapsed());
                    let t = Instant::now();
                    renderer.submit(renderer.device.create_command_encoder(&Default::default()).finish());
                    submits.lock().unwrap().push(t.elapsed());
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
        });
        let sorted = |m: Mutex<Vec<Duration>>| {
            let mut v = m.into_inner().unwrap();
            v.sort_unstable();
            v
        };
        let (pictures, uploads, submits) = (sorted(pictures), sorted(uploads), sorted(submits));
        let at = |v: &[Duration], q: f64| v[((v.len() - 1) as f64 * q) as usize].as_secs_f64() * 1e3;
        println!(
            "2048 px pictures: median {:.1} ms; meanwhile, {} texture uploads: median {:.3} ms, 99% under {:.3} ms, at most {:.3} ms; {} submissions: median {:.3} ms, 99% under {:.3} ms, at most {:.3} ms",
            at(&pictures, 0.5),
            uploads.len(),
            at(&uploads, 0.5),
            at(&uploads, 0.99),
            at(&uploads, 1.0),
            submits.len(),
            at(&submits, 0.5),
            at(&submits, 0.99),
            at(&submits, 1.0)
        );
        assert!(at(&uploads, 0.99) < at(&pictures, 0.5) / 4.0 && at(&submits, 0.99) < at(&pictures, 0.5) / 4.0);
    }
}

/// For the tests of the chain (`chain.rs`): a card of their own (the run's is never opened in the
/// tests), its device, and its two ways of drawing (kept on the card, read back).
#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    fn card() -> Option<&'static Renderer> {
        static CARD: OnceLock<Option<&'static Renderer>> = OnceLock::new();
        *CARD.get_or_init(|| match Renderer::open(false) {
            Ok(renderer) => Some(&*Box::leak(Box::new(renderer))),
            Err(why) => {
                eprintln!("no graphics card for the tests: {why}");
                None
            }
        })
    }

    pub(crate) fn device() -> Option<(wgpu::Device, wgpu::Queue, String)> {
        card().map(|r| (r.device.clone(), r.queue.clone(), r.name.clone()))
    }

    pub(crate) fn paint_kept(raster: &Raster, keep: Keep, stream: &mut Stream<'_>) -> Result<(Totals, Drawn), Failure> {
        paint_kept_on(card().expect("a graphics card"), raster, keep, stream)
    }

    /// Draws on the card and reads back into `raster`, as a run without the chain does.
    pub(crate) fn paint_read(raster: &mut Raster, stream: &mut Stream<'_>) -> bool {
        card_paint(card().expect("a graphics card"), raster, stream).is_some()
    }
}

/// For the shading on the card (`render/gpu/shade.rs`): the adapter the drawing uses, named as when it
/// opened (name and backend), None when the processor draws or the card was given up. Opens the
/// card first if the drawing is to use one and it is not open yet.
pub fn drawing_card() -> Option<&'static str> {
    renderer().filter(|r| !r.down.load(Ordering::Relaxed)).map(|r| r.name.as_str())
}
