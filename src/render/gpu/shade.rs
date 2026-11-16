//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The shading of a block on the graphics card: the factors of [`crate::render::shade::factors`] (slope,
//! cast shadows, foot of the walls…) computed by compute shaders (`shaders/shade.wgsl`), when the
//! card is used.
//!
//! `--shading auto|gpu|cpu` (or `BTMS_SHADING`), `auto` by default: the card when the drawing
//! uses one ([`crate::render::gpu`]), else the processor; `gpu`: the card whenever one opens, the drawing
//! on the processor or not; `cpu`: never. Whatever happens on the card (no card, a failure, a
//! result that cannot be read, a delay), [`factors`] gives None and the processor computes the
//! factors, as before; after a failure of the card itself, for the rest of the run (said once).
//!
//! How. The heights go to the card, ten small compute passes follow the processor's code step
//! by step (the patches and their averages, the cells of the wall feet and their sweeps in eight
//! directions, the sweep of the sun, the factors), and the factors of the window come back: one
//! submission per picture. The shader does what `render/shade.rs` does, operation for operation
//! and in the same order wherever it sums, so that most factors are equal to the bit; the sweeps
//! differ in how they find the occluders (see the shader), the divisions and roots in their last
//! bit.
//!
//! Which card. A device of its own, on the adapter the drawing uses (`gpu::drawing_card`), or on
//! the best adapter when the drawing is on the processor and `--shading gpu` asks for the card:
//! - `--shading gpu` must work without the drawing's device (`--renderer cpu`);
//! - wgpu holds a device's lock for as long as a thread waits for it (`Device::poll`), and the
//!   drawing waits for every picture: on a device of its own, the shading neither waits behind
//!   the drawing's locks nor makes the drawing wait behind its own;
//! - its own queue: the card can run the shading beside the drawing's render passes;
//! - an error of the shading does not make the drawing give the card up (`render/gpu/mod.rs`
//!   gives it up at any error of its device), nor the other way round.
//!
//! Many threads. The buffers of a picture (heights, patches, cells, factors, and the staging
//! memory on both ways) form a [`Set`]; the card keeps at most [`SETS`] of them, a thread takes a
//! free one (waiting for one only when they are all in use) and gives it back. Nothing is locked
//! while a thread waits for the card: it polls the device (which only looks at what is finished)
//! and sleeps between two looks.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::config::Lighting;

/// Where the factors are computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// The card when the drawing uses one, else the processor.
    Auto,
    Gpu,
    Cpu,
}

static CHOICE: OnceLock<Choice> = OnceLock::new();

/// Takes `--shading auto|gpu|cpu` out of the arguments and keeps it for the run. Without the
/// option: `BTMS_SHADING` (the same three words), else auto.
pub fn choose(args: &mut Vec<String>) -> Result<()> {
    let default = std::env::var("BTMS_SHADING").ok();
    let _ = CHOICE.set(chosen(args, default.as_deref())?);
    Ok(())
}

/// What [`choose`] keeps: `--shading` taken out of `args`, over `default` (`BTMS_SHADING`).
pub(crate) fn chosen(args: &mut Vec<String>, default: Option<&str>) -> Result<Choice> {
    let word = |w: Option<&str>, what: &str| -> Result<Choice> {
        Ok(match w {
            None | Some("auto") => Choice::Auto,
            Some("gpu") => Choice::Gpu,
            Some("cpu") => Choice::Cpu,
            Some(other) => bail!("{what}{other}: expected auto, gpu or cpu"),
        })
    };
    let mut choice = word(default, "BTMS_SHADING=")?;
    while let Some(i) = args.iter().position(|a| a == "--shading") {
        choice = word(Some(args.get(i + 1).map(String::as_str).unwrap_or("")), "--shading ")?;
        args.drain(i..(i + 2).min(args.len()));
    }
    Ok(choice)
}

/// The choice of the run (auto when [`choose`] was not called: the tests).
pub fn choice() -> Choice {
    *CHOICE.get_or_init(|| Choice::Auto)
}

/// The factors of [`crate::render::shade::factors`] (same arguments, same meaning, same result to the
/// tolerance of the card), computed by the card; None when the card does not shade (choice,
/// no card, failure): the processor computes them then.
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
) -> Option<Vec<f32>> {
    // (a block redone on the processor after the chain on the card failed: `gpu::on_processor`)
    if choice() == Choice::Cpu || crate::render::gpu::processor_only() || tests_on_processor() {
        return None;
    }
    let card = opened()?;
    if card.down.load(Ordering::Relaxed) {
        LEFT.fetch_add(1, Ordering::Relaxed);
        crate::render::gpu::chain::note(
            crate::render::gpu::chain::Kind::Shading,
            &crate::render::gpu::chain::with_cause(
                "the shading's graphics card failed earlier in the render",
                card.cause.get().map(String::as_str),
            ),
        );
        return None;
    }
    let picture = Picture { heights, width, height, origin, metres, light, reach, window };
    let started = Instant::now();
    // (a panic in wgpu or in the card's driver must not take the run with it)
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| card.factors(&picture)))
        .unwrap_or_else(|_| Err(Failure::Card("unexpected crash during the shading".to_string())));
    match result {
        Ok(factors) => {
            SHADED.fetch_add(1, Ordering::Relaxed);
            NANOS.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
            Some(factors)
        }
        Err(Failure::Picture(why)) => {
            LEFT.fetch_add(1, Ordering::Relaxed);
            crate::render::gpu::chain::note(
                crate::render::gpu::chain::Kind::Shading,
                &format!("a picture the card does not take: {why}"),
            );
            if !card.said_picture.swap(true, Ordering::Relaxed) {
                eprintln!(
                    "shading: a picture is left to the CPU ({why}); so are the next ones in this case, without saying it again"
                );
            }
            None
        }
        Err(Failure::Card(why)) => {
            LEFT.fetch_add(1, Ordering::Relaxed);
            // (a picture that failed after another one made the card give up: that one's cause)
            crate::render::gpu::chain::note(
                crate::render::gpu::chain::Kind::Shading,
                &crate::render::gpu::chain::with_cause(
                    &format!("the graphics card failed: {why}"),
                    card.cause.get().map(String::as_str),
                ),
            );
            card.give_up(&why);
            None
        }
    }
}

#[cfg(test)]
thread_local! {
    /// The tests of the chain shade on the processor what the card draws (`shading_on_processor`).
    static SHADE_ON_PROCESSOR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn tests_on_processor() -> bool {
    SHADE_ON_PROCESSOR.with(|s| s.get())
}

#[cfg(not(test))]
fn tests_on_processor() -> bool {
    false
}

/// For the tests of the chain: `f` with the shading of this thread on the processor (the drawing
/// as the run chose).
#[cfg(test)]
pub(crate) fn shading_on_processor<R>(f: impl FnOnce() -> R) -> R {
    SHADE_ON_PROCESSOR.with(|s| s.set(true));
    let r = f();
    SHADE_ON_PROCESSOR.with(|s| s.set(false));
    r
}

/// For the tests of the chain: the shading on a device of theirs (not the card of [`factors`]).
#[cfg(test)]
pub(crate) fn card_for_tests(device: &wgpu::Device, queue: &wgpu::Queue, name: &str) -> Option<&'static Card> {
    Card::with(device.clone(), queue.clone(), name.to_string(), false).ok().map(|card| &*Box::leak(Box::new(card)))
}

/// For the tests of the chain: the factors of a picture by `card` (heights on the processor).
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn factors_on(
    card: &Card,
    heights: &[f32],
    width: usize,
    height: usize,
    origin: [i64; 2],
    metres: f32,
    light: &Lighting,
    reach: f32,
    window: [usize; 4],
) -> Option<Vec<f32>> {
    card.factors(&Picture { heights, width, height, origin, metres, light, reach, window }).ok()
}

/// The shading on the device of the card that draws, for the chain of a block on the card
/// (`chain.rs`): the same shaders and sets of buffers, on that device; it is also the card of
/// [`factors`] from then on (a block the chain leaves goes through it), unless another one was
/// opened already. Made once.
pub(crate) fn on_drawing_device(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    name: &str,
) -> Result<&'static Card, String> {
    static SHARED: OnceLock<Result<&'static Card, String>> = OnceLock::new();
    SHARED
        .get_or_init(|| {
            let card = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Card::with(device.clone(), queue.clone(), name.to_string(), false)
            }))
            .unwrap_or_else(|_| Err("unexpected crash while opening the card".to_string()))?;
            // the card of `factors` too, when none was opened yet; else one of its own
            let mut mine = Some(card);
            let kept = CARD.get_or_init(|| mine.take());
            match (mine, kept) {
                (None, Some(card)) => Ok(card),
                (Some(card), _) => Ok(&*Box::leak(Box::new(card))),
                (None, None) => Err("no card for the shading".to_string()),
            }
        })
        .clone()
}

/// The card that shades, opened now if it is not yet (and with it the drawing's, for `auto`):
/// its name, None when the processor shades. For what is said of the run before it starts
/// (`--dry-run`, the `settings` event: `tiles::options::resolve`).
pub fn card() -> Option<&'static str> {
    opened().map(|card| card.name.as_str())
}

/// A line on what the card shaded during the run (blocks, time), None when it shaded nothing.
pub fn summary() -> Option<String> {
    let card = CARD.get()?.as_ref()?;
    let (shaded, left) = (SHADED.load(Ordering::Relaxed), LEFT.load(Ordering::Relaxed));
    if shaded == 0 && left == 0 {
        return None;
    }
    Some(format!(
        "  shading on the graphics card {}: {} blocks shaded, {:.1} ms per block on average (upload, compute, readback and waiting included), {} left to the CPU{}",
        card.name,
        shaded,
        NANOS.load(Ordering::Relaxed) as f64 / shaded.max(1) as f64 / 1e6,
        left,
        if card.down.load(Ordering::Relaxed) { "; it failed during the run" } else { "" }
    ))
}

/// Pictures shaded by the card, the time spent on them (nanoseconds), pictures the card was to
/// shade and left to the processor.
static SHADED: AtomicU64 = AtomicU64::new(0);
static NANOS: AtomicU64 = AtomicU64::new(0);
static LEFT: AtomicU64 = AtomicU64::new(0);

static CARD: OnceLock<Option<Card>> = OnceLock::new();

/// The card of the run, opened at the first call: None when the processor shades. Says once
/// which card shades, or why none does.
fn opened() -> Option<&'static Card> {
    CARD.get_or_init(|| {
        let choice = choice();
        // `auto`: the card only when the drawing uses one, and that very adapter
        let drawing = match choice {
            Choice::Cpu => return None,
            Choice::Auto => Some(crate::render::gpu::drawing_card()?),
            Choice::Gpu => crate::render::gpu::drawing_card(),
        };
        let started = Instant::now();
        let opened = std::panic::catch_unwind(|| Card::open(drawing))
            .unwrap_or_else(|_| Err("unexpected crash while opening the card".to_string()));
        match opened {
            Ok(card) => {
                eprintln!("shading: graphics card {} ({:.1} s)", card.name, started.elapsed().as_secs_f64());
                Some(card)
            }
            Err(why) => {
                eprintln!("shading: CPU (no usable graphics card: {why})");
                None
            }
        }
    })
    .as_ref()
}

// The constants of `render/shade.rs` (private there) that the work on the processor's side needs
// here. They must stay those of `render/shade.rs`: the tests compare the card with
// `shade::factors`.
const PATCH: usize = 8;
const CELL: usize = 4;
const SMOOTH_RADIUS: usize = 4;
const FOOT_RADIUS: f32 = 3.0;
const FOOT_SLOPE: f64 = 1.0;
const SLOPE_MAX: f32 = 3.0;
const DIRECT: f32 = 0.75;
const SHADOW_FADE: f32 = 0.5;
/// The eight directions of the wall feet, in the order `Foot::new` sums them.
const FOOT_DIRS: [[f64; 2]; 8] =
    [[1.0, 0.0], [-1.0, 0.0], [0.0, 1.0], [0.0, -1.0], [1.0, 1.0], [1.0, -1.0], [-1.0, 1.0], [-1.0, -1.0]];

/// The compute shaders.
const SHADER: &str = include_str!("shaders/shade.wgsl");
/// The passes, in the order they run, and the bindings of the shader each one uses.
const PASSES: [(&str, &[u32]); 10] = [
    ("patches", &[0, 1, 3]),
    ("cells", &[0, 1, 7]),
    ("along_x", &[0, 3, 4]),
    ("walls", &[0, 2, 7, 8]),
    ("rows_max", &[0, 7, 9]),
    ("along_y", &[0, 3, 4, 5]),
    ("columns_max", &[0, 9, 10]),
    ("relief_pass", &[0, 3, 5, 6]),
    ("shadows", &[0, 1, 2, 6, 11]),
    ("shade", &[0, 1, 6, 8, 10, 11, 12]),
];
/// Words of `Params` in the shader.
const PARAMS: usize = 40;
const PARAMS_BYTES: u64 = (PARAMS * 4) as u64;
/// Sets of buffers kept on the card: so many pictures shaded at once at most (about 75 MB of
/// the card's memory each for a block of the tiles).
const SETS: usize = 8;
/// How long a picture may take to come back.
const WAIT: Duration = Duration::from_secs(60);
/// Between two looks at the card, while waiting for a picture.
const PAUSE: Duration = Duration::from_micros(200);

/// What `factors` is given.
struct Picture<'a> {
    heights: &'a [f32],
    width: usize,
    height: usize,
    origin: [i64; 2],
    metres: f32,
    light: &'a Lighting,
    reach: f32,
    window: [usize; 4],
}

/// Why a picture is not shaded by the card.
#[derive(Debug)]
enum Failure {
    /// This picture only (too large for the card…): the card stays in use.
    Picture(String),
    /// The card itself: it is given up for the rest of the run.
    Card(String),
}

/// A future that is ready at once, as wgpu's are outside a browser.
fn now<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        std::thread::yield_now();
    }
}

// What `render/shade.rs` computes before its loops, the same way (it is private there).

fn toward_sun(light: &Lighting) -> [f64; 2] {
    let azimuth = (light.azimuth as f64).to_radians();
    [azimuth.sin(), -azimuth.cos()]
}

fn sun_height(light: &Lighting) -> f64 {
    (light.elevation as f64).clamp(5.0, 90.0).to_radians()
}

fn casts(light: &Lighting) -> bool {
    light.shadows && light.strength > 0.0 && light.elevation < 89.0
}

fn pad(light: &Lighting, metres: f32) -> usize {
    let patches = (SMOOTH_RADIUS + 3) * PATCH + 2;
    if light.ambient_occlusion > 0.0 { patches.max((FOOT_RADIUS / metres).ceil() as usize + 2 * CELL) } else { patches }
}

struct Lines {
    x_major: bool,
    slope: f64,
    step: f64,
    window: i64,
    toward_plus: bool,
}

fn lines(dir: [f64; 2], metres: f64, reach: f64) -> Lines {
    let x_major = dir[0].abs() >= dir[1].abs();
    let (along, across) = if x_major { (dir[0], dir[1]) } else { (dir[1], dir[0]) };
    let slope = across / along;
    let step = metres * (1.0 + slope * slope).sqrt();
    Lines { x_major, slope, step, window: (reach / step).floor() as i64, toward_plus: along > 0.0 }
}

/// The fades of a sweep along its window, as `shade::sweep` computes them.
fn fades(window: i64, fade_from: f32) -> impl Iterator<Item = u32> {
    (0..=window.max(0)).map(move |d| {
        let t = ((d as f32 / window as f32 - fade_from) / (1.0 - fade_from)).clamp(0.0, 1.0);
        (1.0 - t * t * (3.0 - 2.0 * t)).to_bits()
    })
}

/// What a picture needs on the card, in elements.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Room {
    /// Heights (the picture).
    pixels: u64,
    /// Factors (the window).
    window: u64,
    /// Pixels the sweep of the sun computes (the window and one around).
    hidden: u64,
    patches: u64,
    cells: u64,
    /// Words of the tables.
    tables: u64,
}

impl Room {
    fn holds(&self, other: &Room) -> bool {
        self.pixels >= other.pixels
            && self.window >= other.window
            && self.hidden >= other.hidden
            && self.patches >= other.patches
            && self.cells >= other.cells
            && self.tables >= other.tables
    }

    fn max(&self, other: &Room) -> Room {
        Room {
            pixels: self.pixels.max(other.pixels),
            window: self.window.max(other.window),
            hidden: self.hidden.max(other.hidden),
            patches: self.patches.max(other.patches),
            cells: self.cells.max(other.cells),
            tables: self.tables.max(other.tables),
        }
    }

    /// Bytes of the staging memory toward the card: the heights, the parameters, the tables.
    fn up(&self) -> u64 {
        self.pixels * 4 + PARAMS_BYTES + self.tables * 4
    }
}

/// A picture made ready for the card: the parameters and tables of the shader, the passes to run.
pub(crate) struct Plan {
    params: [u32; PARAMS],
    tables: Vec<u32>,
    room: Room,
    /// Workgroups of each pass ([`PASSES`]), (0, 0) for a pass that does not run.
    dispatch: [(u32, u32); 10],
}

impl Plan {
    /// The size of the picture of heights, and the window of the factors in it.
    pub(crate) fn width(&self) -> usize {
        self.params[0] as usize
    }

    pub(crate) fn height(&self) -> usize {
        self.params[1] as usize
    }

    pub(crate) fn window(&self) -> [usize; 4] {
        [2, 3, 4, 5].map(|k| self.params[k] as usize)
    }
}

/// What `shade::factors` sets up before it computes anything, for the shader. None: not a
/// picture `factors` can shade (the processor will say what is wrong with it).
fn plan(p: &Picture) -> Option<Plan> {
    if p.heights.len() != p.width * p.height {
        return None;
    }
    plan_for(p.width, p.height, p.origin, p.metres, p.light, p.reach, p.window)
}

/// [`plan`] for heights the card makes itself (the chain: they never were on the processor).
pub(crate) fn plan_for(
    width: usize,
    height: usize,
    origin: [i64; 2],
    metres: f32,
    light: &Lighting,
    reach: f32,
    window: [usize; 4],
) -> Option<Plan> {
    let [wx, wy, ww, wh] = window;
    if ww == 0 || wh == 0 || wx + ww > width || wy + wh > height || metres.is_nan() || metres <= 0.0 {
        return None;
    }
    let p = Picture { heights: &[], width, height, origin, metres, light, reach, window };
    let light = p.light;
    let dir = toward_sun(light);
    let sun = sun_height(light);
    let strength = light.strength.clamp(0.0, 1.0) * DIRECT;
    let foot_strength = light.ambient_occlusion.clamp(0.0, 1.0);
    let around = pad(light, p.metres);
    let region = [
        wx.saturating_sub(around) / PATCH * PATCH,
        wy.saturating_sub(around) / PATCH * PATCH,
        ((wx + ww + around).div_ceil(PATCH) * PATCH).min(width),
        ((wy + wh + around).div_ceil(PATCH) * PATCH).min(height),
    ];
    let (pw, ph) = (width.div_ceil(PATCH), height.div_ceil(PATCH));
    let (cw, ch) = (width.div_ceil(CELL), height.div_ceil(CELL));
    let relief = strength > 0.0;
    let foot = foot_strength > 0.0;
    let mut tables: Vec<u32> = Vec::new();

    // the sweep of the sun: the offsets of its lines from world coordinates, its fades
    let l = lines(dir, p.metres as f64, p.reach as f64);
    let shadow = relief && casts(light) && l.window >= 1;
    let (s_offsets, mut s_fade) = (0usize, 0usize);
    if shadow {
        let (la, oa) = if l.x_major { (width, p.origin[0]) } else { (height, p.origin[1]) };
        let offset = |a: usize| ((oa + a as i64) as f64 * l.slope).round() as i64;
        let first = offset(0);
        tables.extend((0..la).map(|a| (offset(a) - first) as i32 as u32));
        s_fade = tables.len();
        tables.extend(fades(l.window, SHADOW_FADE));
    }
    // the eight directions of the wall feet (their slopes are 0 or ±1: the offsets of a line are
    // its steps), then their fades
    let foot_dirs = tables.len();
    let cell_metres = (p.metres * CELL as f32) as f64;
    tables.resize(foot_dirs + 6 * FOOT_DIRS.len(), 0);
    for (k, dir) in FOOT_DIRS.iter().enumerate() {
        let l = lines(*dir, cell_metres, FOOT_RADIUS as f64);
        let fade = tables.len();
        tables.extend(fades(l.window, 0.0));
        let record = [
            l.x_major as u32,
            l.toward_plus as u32,
            (l.slope.round() as i32) as u32,
            l.window.max(0) as u32,
            ((l.step * FOOT_SLOPE) as f32).to_bits(),
            fade as u32,
        ];
        tables[foot_dirs + 6 * k..foot_dirs + 6 * (k + 1)].copy_from_slice(&record);
    }

    let (ax0, ay0) = (wx.saturating_sub(1), wy.saturating_sub(1));
    let (ax1, ay1) = ((wx + ww + 1).min(width), (wy + wh + 1).min(height));
    let patch_metres = p.metres * PATCH as f32;
    let (kx, ky) = ((dir[0] * sun.cos() / sun.sin()) as f32, (dir[1] * sun.cos() / sun.sin()) as f32);
    let flags = relief as u32 | (shadow as u32) << 1 | (foot as u32) << 2;
    let u = |v: usize| v as u32;
    let f = |v: f32| v.to_bits();
    let mut params = [0u32; PARAMS];
    params[..36].copy_from_slice(&[
        u(width),
        u(height),
        u(wx),
        u(wy),
        u(ww),
        u(wh),
        u(region[0]),
        u(region[1]),
        u(region[2]),
        u(region[3]),
        u(pw),
        u(ph),
        u(cw),
        u(ch),
        flags,
        (1.0 / patch_metres).ceil() as u32,
        f(p.metres),
        f(SLOPE_MAX * p.metres),
        f(kx),
        f(ky),
        f(strength),
        f(foot_strength),
        f(patch_metres),
        f(SLOPE_MAX * patch_metres),
        l.x_major as u32,
        l.toward_plus as u32,
        if shadow { l.window as u32 } else { 0 },
        f((l.step * sun.tan()) as f32),
        u(s_fade),
        u(ax0),
        u(ay0),
        u(ax1 - ax0),
        u(ay1 - ay0),
        (FOOT_RADIUS as f64 / cell_metres).ceil() as u32,
        u(foot_dirs),
        u(s_offsets),
    ]);
    let groups = |n: usize, size: usize| n.div_ceil(size) as u32;
    let (patch_groups, cell_groups) = ((groups(pw, 8), groups(ph, 8)), (groups(cw, 8), groups(ch, 8)));
    let none = (0, 0);
    let dispatch = [
        if relief { patch_groups } else { none },
        if foot { cell_groups } else { none },
        if relief { patch_groups } else { none },
        if foot { cell_groups } else { none },
        if foot { cell_groups } else { none },
        if relief { patch_groups } else { none },
        if foot { cell_groups } else { none },
        if relief { patch_groups } else { none },
        if shadow { (groups(ax1 - ax0, 64), groups(ay1 - ay0, 4)) } else { none },
        (groups(ww, 64), groups(wh, 4)),
    ];
    let room = Room {
        pixels: (width * height) as u64,
        window: (ww * wh) as u64,
        hidden: ((ax1 - ax0) * (ay1 - ay0)) as u64,
        patches: (pw * ph) as u64,
        cells: (cw * ch) as u64,
        tables: tables.len() as u64,
    };
    Some(Plan { params, tables, room, dispatch })
}

/// The buffers of one picture on the card.
pub(crate) struct Set {
    room: Room,
    params: wgpu::Buffer,
    heights: wgpu::Buffer,
    tables: wgpu::Buffer,
    own: wgpu::Buffer,
    along: wgpu::Buffer,
    smoothed: wgpu::Buffer,
    relief: wgpu::Buffer,
    mean: wgpu::Buffer,
    hemmed: wgpu::Buffer,
    rows: wgpu::Buffer,
    highest: wgpu::Buffer,
    hidden: wgpu::Buffer,
    factors: wgpu::Buffer,
    /// Staging memory toward the card, mapped whenever the set is free.
    up: wgpu::Buffer,
    /// And back.
    down: wgpu::Buffer,
    /// One per pass.
    groups: Vec<wgpu::BindGroup>,
}

impl Set {
    /// The heights of the picture (as bits: `f32`, NaN and infinities included), which the chain
    /// writes on the card itself.
    pub(crate) fn heights_buffer(&self) -> &wgpu::Buffer {
        &self.heights
    }

    /// The factors of the window, row by row, once the passes ran.
    pub(crate) fn factors_buffer(&self) -> &wgpu::Buffer {
        &self.factors
    }

    /// The buffer of a binding of the shader.
    fn binding(&self, binding: u32) -> &wgpu::Buffer {
        match binding {
            0 => &self.params,
            1 => &self.heights,
            2 => &self.tables,
            3 => &self.own,
            4 => &self.along,
            5 => &self.smoothed,
            6 => &self.relief,
            7 => &self.mean,
            8 => &self.hemmed,
            9 => &self.rows,
            10 => &self.highest,
            11 => &self.hidden,
            _ => &self.factors,
        }
    }
}

/// The sets of a card: those free, and how many there are.
#[derive(Default)]
struct Pool {
    free: Vec<Set>,
    made: usize,
}

/// The card that shades, opened once for all threads.
pub(crate) struct Card {
    /// Name and backend, for the user.
    name: String,
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: Vec<wgpu::ComputePipeline>,
    layouts: Vec<wgpu::BindGroupLayout>,
    max_binding: u64,
    max_buffer: u64,
    max_groups: u32,
    pool: Mutex<Pool>,
    freed: Condvar,
    /// The card failed: the processor shades from now on.
    down: Arc<AtomicBool>,
    /// Why (the first failure, on one line), for the reasons of the fallbacks that follow.
    cause: Arc<OnceLock<String>>,
    said: Arc<AtomicBool>,
    said_picture: AtomicBool,
}

/// The card failed: said once.
fn give_up(down: &AtomicBool, said: &AtomicBool, cause: &OnceLock<String>, why: &str) {
    let why = why.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
    // (kept before the card is marked down: the first failure is the cause)
    let _ = cause.set(why.clone());
    down.store(true, Ordering::Relaxed);
    if !said.swap(true, Ordering::Relaxed) {
        eprintln!("shading: the graphics card failed ({why}); the rest is shaded by the CPU");
    }
}

impl Card {
    /// A device on the adapter named `drawing` (the drawing's, as `render/gpu/mod.rs` names it), or
    /// on the best one: a discrete card before an integrated one, DirectX 12 before Vulkan, a
    /// software adapter (slower than the processor's own code) only when `BTMS_GPU` names it.
    fn open(drawing: Option<&str>) -> Result<Card, String> {
        let descriptor = wgpu::InstanceDescriptor::new_without_display_handle_from_env();
        let backends = descriptor.backends;
        // Kept to the end of the run, used or not, as `render/gpu/mod.rs` keeps its own: destroying
        // an adapter that is not used, once a device of another one exists, can hang (see there).
        let instance: &'static wgpu::Instance = Box::leak(Box::new(wgpu::Instance::new(descriptor)));
        let mut adapters: Vec<&'static wgpu::Adapter> =
            now(instance.enumerate_adapters(backends)).leak().iter().collect();
        if adapters.is_empty() {
            return Err(format!("no card found (backends searched: {backends:?})"));
        }
        let label = |a: &&'static wgpu::Adapter| {
            let info = a.get_info();
            format!("{} ({:?})", info.name, info.backend)
        };
        let wanted: Vec<String> = std::env::var("BTMS_GPU")
            .map(|v| v.to_lowercase().split_whitespace().map(str::to_string).collect())
            .unwrap_or_default();
        match drawing {
            Some(name) => adapters.retain(|a| label(a) == name),
            None => {
                adapters.retain(|a| {
                    let name = label(a).to_lowercase();
                    wanted.iter().all(|w| name.contains(w))
                        && (a.get_info().device_type != wgpu::DeviceType::Cpu || !wanted.is_empty())
                });
                adapters.sort_by_key(|a| {
                    let info = a.get_info();
                    let kind = match info.device_type {
                        wgpu::DeviceType::DiscreteGpu => 0,
                        wgpu::DeviceType::IntegratedGpu => 1,
                        wgpu::DeviceType::VirtualGpu => 2,
                        wgpu::DeviceType::Other => 3,
                        wgpu::DeviceType::Cpu => 4,
                    };
                    (kind, if info.backend == wgpu::Backend::Dx12 { 0 } else { 1 })
                });
            }
        }
        if adapters.is_empty() {
            return Err(match drawing {
                Some(name) => format!("the drawing's adapter, {name}, is not found"),
                None => "no card that is not a software renderer".to_string(),
            });
        }
        let mut refused: Vec<String> = Vec::new();
        for adapter in &adapters {
            let name = label(adapter);
            if refused.iter().any(|r| r.starts_with(&name)) {
                continue; // listed twice (one per screen)
            }
            match Card::on(adapter, name.clone()) {
                Ok(card) => return Ok(card),
                Err(why) => refused.push(format!("{name}: {why}")),
            }
        }
        Err(refused.join("; "))
    }

    /// The device of an adapter, the shaders, a first set of buffers (which checks them all).
    fn on(adapter: &wgpu::Adapter, name: String) -> Result<Card, String> {
        let limits = adapter.limits();
        let required = wgpu::Limits {
            max_storage_buffer_binding_size: limits.max_storage_buffer_binding_size,
            max_buffer_size: limits.max_buffer_size,
            ..wgpu::Limits::default()
        };
        let (device, queue) = now(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("btms shading"),
            required_limits: required.clone(),
            ..Default::default()
        }))
        .map_err(|e| format!("device refused ({e})"))?;
        Card::with(device, queue, name, true)
    }

    /// The shaders and a first set of buffers on a device. `own`: the device is this card's own,
    /// whose errors and loss give the card up; else it is another's (the drawing's, for the
    /// chain: `render/gpu/mod.rs` watches it, and a failure there is seen through it).
    fn with(device: wgpu::Device, queue: wgpu::Queue, name: String, own: bool) -> Result<Card, String> {
        let required = device.limits();
        let down = Arc::new(AtomicBool::new(false));
        let said = Arc::new(AtomicBool::new(false));
        let cause = Arc::new(OnceLock::new());
        if own {
            {
                let (down, said, cause) = (down.clone(), said.clone(), cause.clone());
                device
                    .on_uncaptured_error(Arc::new(move |e: wgpu::Error| give_up(&down, &said, &cause, &e.to_string())));
            }
            {
                let (down, said, cause) = (down.clone(), said.clone(), cause.clone());
                device.set_device_lost_callback(move |_, why| {
                    give_up(&down, &said, &cause, &format!("device lost: {why}"))
                });
            }
        }
        let scopes = [wgpu::ErrorFilter::Validation, wgpu::ErrorFilter::OutOfMemory, wgpu::ErrorFilter::Internal]
            .map(|filter| device.push_error_scope(filter));
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("shading"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let pipelines: Vec<wgpu::ComputePipeline> = PASSES
            .iter()
            .map(|(entry, _)| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: None,
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    cache: None,
                })
            })
            .collect();
        let layouts = pipelines.iter().map(|p| p.get_bind_group_layout(0)).collect();
        let card = Card {
            name,
            device,
            queue,
            pipelines,
            layouts,
            max_binding: required.max_storage_buffer_binding_size,
            max_buffer: required.max_buffer_size,
            max_groups: required.max_compute_workgroups_per_dimension,
            pool: Mutex::new(Pool::default()),
            freed: Condvar::new(),
            down,
            cause,
            said,
            said_picture: AtomicBool::new(false),
        };
        // a first, small set: every bind group is checked against its pass
        let first =
            card.set(&Room { pixels: 64 * 64, window: 16 * 16, hidden: 18 * 18, patches: 64, cells: 256, tables: 256 });
        let mut raised = None;
        for scope in scopes.into_iter().rev() {
            if let Some(e) = now(scope.pop()) {
                raised.get_or_insert(e.to_string());
            }
        }
        if let Some(e) = raised {
            return Err(format!("shaders refused ({e})"));
        }
        {
            let mut pool = card.pool.lock().unwrap_or_else(|e| e.into_inner());
            pool.free.push(first);
            pool.made = 1;
        }
        Ok(card)
    }

    fn give_up(&self, why: &str) {
        give_up(&self.down, &self.said, &self.cause, why);
        self.freed.notify_all();
    }

    /// A new set of buffers of `room`; its staging memory toward the card is mapped.
    fn set(&self, room: &Room) -> Set {
        let device = &self.device;
        let buffer = |label: &str, elements: u64, bytes: u64, usage: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (elements * bytes).max(16),
                usage,
                mapped_at_creation: false,
            })
        };
        let storage = wgpu::BufferUsages::STORAGE;
        let input = storage | wgpu::BufferUsages::COPY_DST;
        let mut set = Set {
            room: *room,
            params: buffer("params", 1, PARAMS_BYTES, wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST),
            // (read back by the tests of the chain, which writes it on the card)
            heights: buffer("heights", room.pixels, 4, input | wgpu::BufferUsages::COPY_SRC),
            tables: buffer("tables", room.tables, 4, input),
            own: buffer("patches", room.patches, 16, storage),
            along: buffer("patches along x", room.patches, 16, storage),
            smoothed: buffer("smoothed patches", room.patches, 16, storage),
            relief: buffer("relief", room.patches, 16, storage),
            mean: buffer("cells", room.cells, 4, storage),
            hemmed: buffer("wall feet", room.cells, 4, storage),
            rows: buffer("highest along x", room.cells, 4, storage),
            highest: buffer("highest", room.cells, 4, storage),
            hidden: buffer("shadows", room.hidden, 4, storage),
            factors: buffer("factors", room.window, 4, storage | wgpu::BufferUsages::COPY_SRC),
            up: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("upload"),
                size: room.up(),
                usage: wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: true,
            }),
            down: buffer("readback", room.window, 4, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST),
            groups: Vec::new(),
        };
        set.groups = PASSES
            .iter()
            .zip(&self.layouts)
            .map(|((entry, bindings), layout)| {
                let entries: Vec<wgpu::BindGroupEntry> = bindings
                    .iter()
                    .map(|b| wgpu::BindGroupEntry { binding: *b, resource: set.binding(*b).as_entire_binding() })
                    .collect();
                device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(entry), layout, entries: &entries })
            })
            .collect();
        set
    }

    /// A free set that holds `room`; a free one rebuilt larger, or a new one while there are fewer
    /// than [`SETS`]; else the first one given back.
    fn take(&self, room: &Room) -> Result<Set, Failure> {
        let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if self.down.load(Ordering::Relaxed) {
                return Err(Failure::Card("the card failed".to_string()));
            }
            if let Some(i) = pool.free.iter().position(|s| s.room.holds(room)) {
                return Ok(pool.free.swap_remove(i));
            }
            if let Some(old) = pool.free.pop() {
                drop(pool);
                return Ok(self.set(&old.room.max(room)));
            }
            if pool.made < SETS {
                pool.made += 1;
                drop(pool);
                return Ok(self.set(room));
            }
            pool = self.freed.wait_timeout(pool, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    fn give(&self, set: Set) {
        self.pool.lock().unwrap_or_else(|e| e.into_inner()).free.push(set);
        self.freed.notify_one();
    }

    /// The factors of a picture, computed by the card.
    fn factors(&self, p: &Picture) -> Result<Vec<f32>, Failure> {
        if self.down.load(Ordering::Relaxed) {
            return Err(Failure::Card("the card failed".to_string()));
        }
        let plan =
            plan(p).ok_or_else(|| Failure::Picture("a picture or a window the shading does not take".to_string()))?;
        let room = plan.room;
        let largest = (room.pixels * 4).max(room.patches * 16).max(room.hidden * 4);
        if largest > self.max_binding || room.up() > self.max_buffer {
            return Err(Failure::Picture(format!(
                "a picture of {}×{} px is larger than the card accepts",
                p.width, p.height
            )));
        }
        if plan.dispatch.iter().any(|(x, y)| *x > self.max_groups || *y > self.max_groups) {
            return Err(Failure::Picture(format!(
                "a picture of {}×{} px asks the card for too many workgroups",
                p.width, p.height
            )));
        }
        // the errors of this thread's calls to the card, up to the submission
        let scopes = [wgpu::ErrorFilter::Validation, wgpu::ErrorFilter::OutOfMemory, wgpu::ErrorFilter::Internal]
            .map(|filter| self.device.push_error_scope(filter));
        let sent = self.send(p, &plan);
        let mut raised = None;
        for scope in scopes.into_iter().rev() {
            if let Some(e) = now(scope.pop()) {
                raised.get_or_insert(e.to_string());
            }
        }
        let (set, back) = match (sent, raised) {
            (Err(failure), _) => return Err(failure),
            (Ok(_), Some(error)) => return Err(Failure::Card(error)),
            (Ok(sent), None) => sent,
        };
        // (a set whose picture did not come back is not given back: its state is unknown)
        self.wait(&back)?;
        let bytes = room.window * 4;
        let factors = {
            let view = set
                .down
                .get_mapped_range(0..bytes)
                .map_err(|e| Failure::Card(format!("reading back the factors: {e}")))?;
            match bytemuck::try_cast_slice::<u8, f32>(&view) {
                Ok(floats) => floats.to_vec(),
                Err(_) => view.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect(),
            }
        };
        set.down.unmap();
        self.give(set);
        Ok(factors)
    }

    /// Sends a picture: heights, parameters and tables through the staging memory, the passes,
    /// the factors copied back; once submitted, both staging buffers are to be mapped again (the
    /// one back to be read, the one toward the card for the next picture).
    fn send(&self, p: &Picture, plan: &Plan) -> Result<(Set, Receiver<Result<(), wgpu::BufferAsyncError>>), Failure> {
        let set = self.take(&plan.room)?;
        let heights = (p.heights.len() * 4) as u64;
        let params_at = set.room.pixels * 4;
        let tables_at = params_at + PARAMS_BYTES;
        let tables = (plan.tables.len() * 4) as u64;
        {
            let mut view =
                set.up.get_mapped_range_mut(..).map_err(|e| Failure::Card(format!("uploading the heights: {e}")))?;
            view.slice(..heights as usize).copy_from_slice(bytemuck::cast_slice(p.heights));
            view.slice(params_at as usize..tables_at as usize).copy_from_slice(bytemuck::cast_slice(&plan.params));
            view.slice(tables_at as usize..(tables_at + tables) as usize)
                .copy_from_slice(bytemuck::cast_slice(&plan.tables));
        }
        set.up.unmap();
        let mut encoder = self.device.create_command_encoder(&Default::default());
        encoder.copy_buffer_to_buffer(&set.up, 0, &set.heights, 0, heights);
        encoder.copy_buffer_to_buffer(&set.up, params_at, &set.params, 0, PARAMS_BYTES);
        if tables > 0 {
            encoder.copy_buffer_to_buffer(&set.up, tables_at, &set.tables, 0, tables);
        }
        {
            let mut pass = encoder
                .begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("shading"), timestamp_writes: None });
            for (k, (x, y)) in plan.dispatch.iter().enumerate() {
                if *x == 0 || *y == 0 {
                    continue;
                }
                pass.set_pipeline(&self.pipelines[k]);
                pass.set_bind_group(0, &set.groups[k], &[]);
                pass.dispatch_workgroups(*x, *y, 1);
            }
        }
        let bytes = plan.room.window * 4;
        encoder.copy_buffer_to_buffer(&set.factors, 0, &set.down, 0, bytes);
        self.queue.submit([encoder.finish()]);
        let (sender, receiver) = channel();
        let again = sender.clone();
        set.down.map_async(wgpu::MapMode::Read, 0..bytes, move |mapped| {
            let _ = sender.send(mapped);
        });
        set.up.map_async(wgpu::MapMode::Write, .., move |mapped| {
            let _ = again.send(mapped);
        });
        Ok((set, receiver))
    }

    /// For the chain (`chain.rs`, on the drawing's device): a set for `plan`, taken from the pool
    /// as [`Card::factors`] takes one (waiting when all are in use), to be given back with
    /// [`Card::give_back`] once the block is back. Its heights are to be written by the chain.
    pub(crate) fn take_for(&self, plan: &Plan) -> Result<Set, String> {
        let largest = (plan.room.pixels * 4).max(plan.room.patches * 16).max(plan.room.hidden * 4);
        if largest > self.max_binding
            || plan.room.up() > self.max_buffer
            || plan.dispatch.iter().any(|(x, y)| *x > self.max_groups || *y > self.max_groups)
        {
            return Err("a picture the shading on the card does not take".to_string());
        }
        self.take(&plan.room).map_err(|f| match f {
            Failure::Picture(why) | Failure::Card(why) => why,
        })
    }

    pub(crate) fn give_back(&self, set: Set) {
        self.give(set);
    }

    /// For the chain: a set whose block did not come back (its state is not known) is let go, and
    /// another may be made in its place. Kept, it would be counted for ever: on the drawing's
    /// device, nothing gives this card up, and the threads waiting for a set would wait for ever.
    pub(crate) fn lose(&self, set: Set) {
        drop(set);
        let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        pool.made = pool.made.saturating_sub(1);
        drop(pool);
        self.freed.notify_one();
    }

    /// Records the shading of `plan` into `encoder`: its parameters and tables written (through
    /// the queue: a few kilobytes), then the passes, whose factors stay in the set
    /// ([`Set::factors_buffer`]). The heights must be in [`Set::heights_buffer`] by then.
    pub(crate) fn record(&self, set: &Set, plan: &Plan, encoder: &mut wgpu::CommandEncoder) {
        self.queue.write_buffer(&set.params, 0, bytemuck::cast_slice(&plan.params));
        if !plan.tables.is_empty() {
            self.queue.write_buffer(&set.tables, 0, bytemuck::cast_slice(&plan.tables));
        }
        let mut pass =
            encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("shading"), timestamp_writes: None });
        for (k, (x, y)) in plan.dispatch.iter().enumerate() {
            if *x == 0 || *y == 0 {
                continue;
            }
            pass.set_pipeline(&self.pipelines[k]);
            pass.set_bind_group(0, &set.groups[k], &[]);
            pass.dispatch_workgroups(*x, *y, 1);
        }
    }

    /// Waits for both staging buffers of a picture, looking at the card and sleeping in turn.
    fn wait(&self, back: &Receiver<Result<(), wgpu::BufferAsyncError>>) -> Result<(), Failure> {
        let deadline = Instant::now() + WAIT;
        let mut mapped = 0;
        loop {
            self.device.poll(wgpu::PollType::Poll).map_err(|e| Failure::Card(format!("waiting for the card: {e}")))?;
            while let Ok(result) = back.try_recv() {
                result.map_err(|e| Failure::Card(format!("readback: {e}")))?;
                mapped += 1;
            }
            if mapped == 2 {
                return Ok(());
            }
            if self.down.load(Ordering::Relaxed) {
                return Err(Failure::Card("the card failed during the shading".to_string()));
            }
            if Instant::now() > deadline {
                return Err(Failure::Card(format!("no answer within {} s", WAIT.as_secs())));
            }
            std::thread::sleep(PAUSE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_option_is_read_and_taken_out() {
        let mut args: Vec<String> = ["tiles", "--shading", "cpu", "output"].iter().map(|s| s.to_string()).collect();
        assert_eq!(chosen(&mut args, None).unwrap(), Choice::Cpu);
        assert_eq!(args, ["tiles", "output"]);
        let mut args: Vec<String> = vec!["tiles".into()];
        assert_eq!(chosen(&mut args, Some("gpu")).unwrap(), Choice::Gpu);
        assert!(chosen(&mut vec!["--shading".into(), "turbo".into()], None).is_err());
    }

    /// The card of the tests, or None on a machine without one (the tests then check nothing).
    fn card() -> Option<&'static Card> {
        static CARD: OnceLock<Option<Card>> = OnceLock::new();
        CARD.get_or_init(|| match Card::open(None) {
            Ok(card) => Some(card),
            Err(why) => {
                eprintln!("no graphics card for the tests: {why}");
                None
            }
        })
        .as_ref()
    }

    /// A small deterministic hash of a pixel, 0 to 1.
    fn noise(x: i64, y: i64, seed: u64) -> f32 {
        let mut h =
            (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ seed;
        h ^= h >> 29;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 32;
        (h >> 40) as f32 / (1u64 << 24) as f32
    }

    /// A made-up world, in metres from its origin (x east, y south): hills and a steep slope, a
    /// cliff, towers, houses with pitched roofs, low walls and kerbs, isolated trees and a wood,
    /// holes (single pixels, a pond with nothing drawn, a few NaN).
    fn world(xm: f64, ym: f64, px: i64, py: i64) -> f32 {
        // (in metres from the north-west corner of the block of the tests)
        let (x, y) = ((xm - ANCHOR[0]) as f32, (ym - ANCHOR[1]) as f32);
        let mut h = 30.0 + 6.0 * (x / 23.0).sin() * (y / 31.0).cos() + 0.12 * x - 0.05 * y;
        // a steep hill
        h += 22.0 * (-((x - 40.0).powi(2) + (y - 95.0).powi(2)) / 400.0).exp();
        // a cliff, oblique
        if x + 0.35 * y > 150.0 {
            h += 14.0;
        }
        // kerbs and low walls
        if (y - 20.0).abs() < 0.15 && x > 0.0 && x < 120.0 {
            h += 0.15;
        }
        if (x - 75.0).abs() < 0.2 && y > 30.0 && y < 60.0 {
            h += 1.2;
        }
        // buildings: (centre x, centre y, half width, half depth, height, pitch)
        for (cx, cy, hw, hd, top, pitch) in [
            (20.0f32, 30.0f32, 8.0f32, 12.0f32, 6.0f32, 0.6f32),
            (60.0, 15.0, 5.0, 5.0, 4.5, 0.0),
            (100.0, 40.0, 10.0, 10.0, 120.0, 0.0),
            (-20.0, 60.0, 9.0, 9.0, 70.0, 0.0),
            (64.0, -30.0, 12.0, 6.0, 45.0, 0.0),
            (90.0, 110.0, 6.0, 10.0, 8.0, 0.8),
            (140.0, 80.0, 4.0, 4.0, 25.0, 0.0),
            (30.0, 128.5, 7.0, 7.0, 3.0, 0.0),
        ] {
            if (x - cx).abs() <= hw && (y - cy).abs() <= hd {
                h += top - pitch * (x - cx).abs();
            }
        }
        // isolated trees and a wood: crowns of broken heights
        for (cx, cy, r) in [(45.0f32, 60.0f32, 3.0f32), (52.0, 64.0, 2.5), (110.0, 90.0, 4.0), (5.0, 100.0, 3.5)] {
            let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
            if d < r {
                h += 7.0 * (1.0 - (d / r).powi(2)) + 1.5 * noise(px, py, 1);
            }
        }
        if x > 70.0 && x < 95.0 && y > 65.0 && y < 85.0 {
            h += 9.0 + 2.5 * noise(px, py, 2);
        }
        // nothing drawn: single pixels, a pond, a few that are not numbers
        let n = noise(px, py, 3);
        if n < 0.004 || ((x - 115.0).powi(2) + (y - 15.0).powi(2)) < 36.0 {
            return f32::NEG_INFINITY;
        }
        if n > 0.9995 {
            return f32::NAN;
        }
        h
    }

    /// Where the made-up world is, in metres (x east, y south): the block of the world pixel
    /// (6144, 10240) at 6.25 cm.
    const ANCHOR: [f64; 2] = [384.0, 640.0];

    /// The picture of the world around a window of `w` × `h` pixels at the world pixel (x0, y0),
    /// with the margins a frame gives it, on the grid of the patches; `inset` pixels taken off
    /// the margins of the west and north (0: a frame's picture; more: a window that touches the
    /// edges of the picture). (heights, width, height, origin, window)
    #[allow(clippy::too_many_arguments)]
    fn picture(
        light: &Lighting,
        metres: f32,
        reach: f32,
        x0: i64,
        y0: i64,
        w: usize,
        h: usize,
        inset: usize,
    ) -> (Vec<f32>, usize, usize, [i64; 2], [usize; 4]) {
        let [left, top, right, bottom] = crate::render::shade::margins(light, metres, reach);
        let left = left + (x0 - left as i64).rem_euclid(PATCH as i64) as usize;
        let top = top + (y0 - top as i64).rem_euclid(PATCH as i64) as usize;
        let right = right + (PATCH - (left + w + right) % PATCH) % PATCH;
        let bottom = bottom + (PATCH - (top + h + bottom) % PATCH) % PATCH;
        let (left, top) = (left.saturating_sub(inset), top.saturating_sub(inset));
        let (width, height) = (left + w + right, top + h + bottom);
        let origin = [x0 - left as i64, y0 - top as i64];
        let heights = (0..width * height)
            .map(|i| {
                let (px, py) = (origin[0] + (i % width) as i64, origin[1] + (i / width) as i64);
                world(px as f64 * metres as f64, py as f64 * metres as f64, px, py)
            })
            .collect();
        (heights, width, height, origin, [left, top, w, h])
    }

    /// How far the card's factors are from the processor's, in 255ths.
    #[derive(Default, Debug)]
    struct Gap {
        pixels: u64,
        /// Equal to the bit.
        same: u64,
        within1: u64,
        within4: u64,
        beyond: u64,
        /// The largest difference, in 255ths.
        worst: f32,
        /// Factors under 0.95 and over 1.05 on the processor: what the comparison is worth.
        dark: u64,
        lit: u64,
    }

    impl Gap {
        fn add(&mut self, cpu: &[f32], card: &[f32]) {
            assert_eq!(cpu.len(), card.len());
            for (c, g) in cpu.iter().zip(card) {
                let d = (c - g).abs() * 255.0;
                self.pixels += 1;
                self.same += (c.to_bits() == g.to_bits()) as u64;
                self.dark += (*c < 0.95) as u64;
                self.lit += (*c > 1.05) as u64;
                if d <= 1.0 {
                    self.within1 += 1;
                } else if d <= 4.0 {
                    self.within4 += 1;
                } else {
                    self.beyond += 1;
                }
                self.worst = self.worst.max(if d.is_nan() { f32::INFINITY } else { d });
            }
        }

        fn say(&self) -> String {
            format!(
                "{} pixels ({} under 0.95, {} over 1.05): {} equal to the bit, {:.4} % within 1/255, {} between 1 and 4/255, {} beyond ({:.5} %), largest difference {:.4}/255",
                self.pixels,
                self.dark,
                self.lit,
                self.same,
                100.0 * self.within1 as f64 / self.pixels.max(1) as f64,
                self.within4,
                self.beyond,
                100.0 * self.beyond as f64 / self.pixels.max(1) as f64,
                self.worst
            )
        }
    }

    fn sun(azimuth: f32, elevation: f32, shadows: bool, ambient_occlusion: f32) -> Lighting {
        Lighting { enabled: true, azimuth, elevation, strength: 0.6, shadows, ambient_occlusion }
    }

    /// The card against `shade::factors`: blocks of the tiles' size (2 048 px of 6.25 cm in a
    /// picture of some 2 660 px), suns from every side and height, shadows or not, wall feet or
    /// not; then smaller pictures at other scales, with windows on the edges of the picture.
    #[test]
    fn the_card_shades_as_the_processor() {
        let Some(card) = card() else { return };
        let mut total = Gap::default();
        let blocks = [
            sun(315.0, 35.0, true, 0.3),
            sun(200.0, 20.0, true, 1.0),
            sun(90.0, 60.0, true, 0.0),
            sun(10.0, 45.0, false, 1.0),
            sun(270.0, 35.0, true, 0.0),
            sun(135.0, 8.0, true, 0.3),
            Lighting { strength: 1.0, ..sun(240.0, 50.0, true, 1.0) },
            Lighting { strength: 0.0, ..sun(315.0, 35.0, true, 1.0) },
            sun(45.0, 89.5, true, 0.3),
        ];
        let (x0, y0) = (6144i64, 10240i64);
        for light in &blocks {
            let (heights, width, height, origin, window) = picture(light, 0.0625, 48.0, x0, y0, 2048, 2048, 0);
            let cpu = crate::render::shade::factors(&heights, width, height, origin, 0.0625, light, 48.0, window);
            let picture =
                Picture { heights: &heights, width, height, origin, metres: 0.0625, light, reach: 48.0, window };
            let gpu = card.factors(&picture).expect("the card did not shade");
            let mut gap = Gap::default();
            gap.add(&cpu, &gpu);
            println!(
                "block {width}×{height}, sun {}° at {}°, shadows {}, ambient occlusion {}: {}",
                light.azimuth,
                light.elevation,
                light.shadows,
                light.ambient_occlusion,
                gap.say()
            );
            // something shaded: the test checks something
            assert!(gap.dark > 10_000 || light.elevation > 89.0, "nothing shaded: {}", gap.say());
            total.add(&cpu, &gpu);
            // and the comparison sees a sun 2° away
            if light.azimuth == 315.0 && light.strength > 0.0 {
                let turned = Lighting { azimuth: 317.0, ..light.clone() };
                let gpu = card.factors(&Picture { light: &turned, ..picture }).expect("the card did not shade");
                let mut gap = Gap::default();
                gap.add(&cpu, &gpu);
                println!("  (the sun at 317°: {})", gap.say());
                assert!((gap.within1 as f64) < 0.99 * gap.pixels as f64 && gap.beyond > 1000, "{}", gap.say());
            }
        }
        // smaller pictures, other scales, windows on the picture's edges (inset: no margins there)
        let mut small = Gap::default();
        for (k, (metres, inset)) in
            [(0.25f32, 0usize), (0.5, 0), (1.0, 0), (0.25, 10_000), (0.0625, 10_000), (0.125, 30)]
                .into_iter()
                .enumerate()
        {
            for light in &blocks[..6] {
                let side = (160.0 / metres).min(700.0) as usize;
                let (x0, y0) = (
                    (ANCHOR[0] / metres as f64) as i64 - side as i64 / 3 + k as i64,
                    (ANCHOR[1] / metres as f64) as i64 + side as i64 / 5,
                );
                let (heights, width, height, origin, window) =
                    picture(light, metres, 48.0, x0, y0, side, side * 3 / 4, inset);
                let cpu = crate::render::shade::factors(&heights, width, height, origin, metres, light, 48.0, window);
                let picture = Picture { heights: &heights, width, height, origin, metres, light, reach: 48.0, window };
                let gpu = card.factors(&picture).expect("the card did not shade");
                small.add(&cpu, &gpu);
            }
        }
        println!("small pictures: {}", small.say());
        println!("blocks: {}", total.say());
        for gap in [&total, &small] {
            assert!(gap.within1 as f64 >= 0.999 * gap.pixels as f64, "{}", gap.say());
            assert!(gap.beyond * 20_000 <= gap.pixels, "more than 0.005 % beyond 4/255: {}", gap.say());
        }
    }

    /// Several threads at once, more than there are sets: every picture comes back, right.
    #[test]
    fn threads_share_the_card() {
        let Some(card) = card() else { return };
        let light = sun(315.0, 35.0, true, 0.3);
        let (heights, width, height, origin, window) = picture(&light, 0.25, 48.0, 100, 200, 512, 512, 0);
        let cpu = crate::render::shade::factors(&heights, width, height, origin, 0.25, &light, 48.0, window);
        std::thread::scope(|s| {
            for _ in 0..SETS + 4 {
                s.spawn(|| {
                    for _ in 0..3 {
                        let picture = Picture {
                            heights: &heights,
                            width,
                            height,
                            origin,
                            metres: 0.25,
                            light: &light,
                            reach: 48.0,
                            window,
                        };
                        let gpu = card.factors(&picture).expect("the card did not shade");
                        let mut gap = Gap::default();
                        gap.add(&cpu, &gpu);
                        assert!(gap.within1 as f64 >= 0.999 * gap.pixels as f64, "{}", gap.say());
                    }
                });
            }
        });
        assert!(card.pool.lock().unwrap().made <= SETS);
    }

    /// A card that failed shades nothing more (on a device of its own: the other tests keep
    /// theirs), and a thread waiting for a set of buffers does not wait for ever.
    #[test]
    fn a_card_that_failed_is_left() {
        if card().is_none() {
            return;
        }
        let card = Card::open(None).expect("a second card");
        let light = sun(315.0, 35.0, true, 0.3);
        let (heights, width, height, origin, window) = picture(&light, 1.0, 48.0, 384, 640, 64, 64, 0);
        let picture =
            Picture { heights: &heights, width, height, origin, metres: 1.0, light: &light, reach: 48.0, window };
        assert!(card.factors(&picture).is_ok());
        card.give_up("failure simulated by the test");
        card.give_up("another failure, after it");
        assert_eq!(card.cause.get().map(String::as_str), Some("failure simulated by the test"));
        assert!(matches!(card.factors(&picture), Err(Failure::Card(_))));
        assert!(matches!(card.take(&plan(&picture).unwrap().room), Err(Failure::Card(_))));
    }

    /// What is not a picture the card takes is left to the processor.
    #[test]
    fn a_wrong_picture_is_left_to_the_processor() {
        let light = sun(315.0, 35.0, true, 0.3);
        let heights = vec![1.0f32; 100];
        let picture = |window: [usize; 4], width: usize| Picture {
            heights: &heights,
            width,
            height: 10,
            origin: [0, 0],
            metres: 1.0,
            light: &light,
            reach: 48.0,
            window,
        };
        assert!(plan(&picture([0, 0, 10, 10], 10)).is_some());
        assert!(plan(&picture([5, 0, 10, 10], 10)).is_none());
        assert!(plan(&picture([0, 0, 0, 10], 10)).is_none());
        assert!(plan(&picture([0, 0, 10, 10], 11)).is_none());
    }

    /// Time of a block of the tiles' size, on the processor and on the card (alone, then several
    /// threads at once): `cargo test --release -- --ignored --nocapture time_of_a_block`.
    #[test]
    #[ignore]
    fn time_of_a_block() {
        let t = Instant::now();
        let Some(card) = card() else { return };
        println!(
            "card {} opened in {:.2} s (device, shaders, a first set of buffers)",
            card.name,
            t.elapsed().as_secs_f64()
        );
        let light = Lighting::default();
        let (heights, width, height, origin, window) = picture(&light, 0.0625, 48.0, 6144, 10240, 2048, 2048, 0);
        let picture =
            Picture { heights: &heights, width, height, origin, metres: 0.0625, light: &light, reach: 48.0, window };
        let rounds = 5;
        let t = Instant::now();
        for _ in 0..rounds {
            std::hint::black_box(crate::render::shade::factors(
                &heights, width, height, origin, 0.0625, &light, 48.0, window,
            ));
        }
        let cpu = t.elapsed().as_secs_f64() / rounds as f64;
        // the first one opens sets and compiles: not counted
        card.factors(&picture).unwrap();
        let t = Instant::now();
        for _ in 0..rounds * 4 {
            std::hint::black_box(card.factors(&picture).unwrap());
        }
        let alone = t.elapsed().as_secs_f64() / (rounds * 4) as f64;
        // the parts of it: filling the staging memory, reading back
        let t = Instant::now();
        let mut copy = vec![0f32; heights.len()];
        copy.copy_from_slice(&heights);
        std::hint::black_box(&copy);
        let fill = t.elapsed().as_secs_f64();
        println!(
            "block of {width}×{height} px (window of 2048×2048): processor {:.1} ms; card {:.1} ms per block, alone (one copy of the heights in memory: {:.1} ms)",
            cpu * 1e3,
            alone * 1e3,
            fill * 1e3
        );
        for threads in [4usize, 8, 16] {
            let per = 4;
            let t = Instant::now();
            std::thread::scope(|s| {
                for _ in 0..threads {
                    s.spawn(|| {
                        for _ in 0..per {
                            let picture = Picture {
                                heights: &heights,
                                width,
                                height,
                                origin,
                                metres: 0.0625,
                                light: &light,
                                reach: 48.0,
                                window,
                            };
                            std::hint::black_box(card.factors(&picture).unwrap());
                        }
                    });
                }
            });
            let wall = t.elapsed().as_secs_f64();
            println!(
                "  {threads} threads at once, {} blocks: {:.1} ms per block in all ({:.1} blocks per second), {:.1} ms of waiting per block and per thread",
                threads * per,
                wall / (threads * per) as f64 * 1e3,
                (threads * per) as f64 / wall,
                wall / per as f64 * 1e3
            );
        }
    }
}
