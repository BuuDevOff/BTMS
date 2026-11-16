//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The chain of a block on the graphics card: when the card draws and shades, a block of the
//! tiles stays on it from its drawing to its zooms 9, 8 and 7, and comes back once, ready to be
//! encoded. Without the chain, a block drawn by the card comes back to the processor (colours and
//! depths, 84 MB), so do the heights around it (twice 28 MB), they go to the card again for the
//! shading (28 MB) and its factors come back (16 MB); then the processor makes the 8-bit picture,
//! closes the small holes, puts the water on it and halves it twice. Here (`shaders/chain.wgsl`):
//!
//! 1. the block is drawn into a picture kept on the card (`gpu::paint_kept`), then the heights
//!    around it (first pass, near the block);
//! 2. the lowest heights of its 64 parts are reduced on the card and read back (256 bytes): the
//!    processor needs them to choose what is tall enough to be drawn in the second pass of the
//!    margins (`shade::Frame::far_from`, the same choice as `Frame::far`);
//! 3. the second pass is drawn over the first, on the card (the nearer wins: the processor's
//!    merge);
//! 4. one submission: the heights the shading reads, composed on the card into the buffer of a
//!    set of the shading (`gpu_shade`, its ten passes on the same device), the colours times the
//!    factors in 8 bits, four passes of the holes, the ground under the water, the water and the
//!    sea, zooms 8 and 7; the three pictures come back in one copy (21 MB).
//!
//! Without the light, steps 1 to 3 are the block alone and step 4 has no shading. Without
//! `[water]`, or on a block no piece of water meets, no water.
//!
//! **One device.** Textures cannot go from one wgpu device to another: the chain runs on the
//! drawing's device (`render/gpu/mod.rs`), and so does its shading (`gpu_shade::on_drawing_device`,
//! the same shaders on that device). The shading has a device of its own for
//! `--renderer cpu --shading gpu` (the chain is not used then), so as not to wait behind the
//! drawing's locks (the `card_wait` measure saw no such wait: waiting only takes a read lock) and
//! so that an error of one does not give the other up (here any failure of the chain gives its
//! block to the processor, below).
//!
//! **Fallbacks** ([`Failure`]). Memory the card refuses, a picture it does not take, the water
//! not settled in the rounds of its sweeps: that block is made again whole on the processor
//! (drawing, shading, holes, water, as `--renderer cpu --shading cpu` makes it:
//! `gpu::on_processor`). An error of the card, its loss, a delay: the chain is given up, said
//! once, and the rest of the run takes the path without the chain (the card drawing and shading
//! if it still can, the processor doing the rest). Each is counted ([`fell_back`],
//! `progress::fallback`), and a line at the end of the render says how many blocks the processor
//! made instead of the card, and why ([`end`]).
//!
//! `--chain auto|gpu|cpu` (or `BTMS_CHAIN`): `auto` (the default) uses the chain when the card
//! draws and shades (or draws, with the light off), and not with the tools that need the
//! pictures on the processor (`BTMS_FINGERPRINTS`, `BTMS_GPU_COMPARE`); `gpu` the same, with those
//! tools too (they then see nothing of the blocks), refused with `--renderer cpu` or
//! `--shading cpu` (light on); `cpu`: never, the run goes without the chain.
//! `BTMS_CHAIN_FAULT=fail[:n]` makes the n-th block fail (its fallback), `abandon[:n]` makes the
//! chain give up there.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::render::gpu::shade as gpu_shade;
use crate::render::gpu::{self, Drawn};
use crate::util::profile::{Phase, time};
use crate::world::water::Spans;

/// Where the chain of a block runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// On the card when it draws and shades there, else without the chain.
    Auto,
    Gpu,
    Cpu,
}

static CHOICE: OnceLock<Choice> = OnceLock::new();

/// Takes `--chain auto|gpu|cpu` out of the arguments and keeps it for the run (`BTMS_CHAIN`
/// without it, else auto). After `gpu::choose`, `gpu_shade::choose` and the configuration (the
/// light): `gpu` is refused where the chain could not be on the card.
pub fn choose(args: &mut Vec<String>) -> Result<()> {
    let default = std::env::var("BTMS_CHAIN").ok();
    let choice = chosen(args, default.as_deref())?;
    if choice == Choice::Gpu {
        if gpu::choice() == gpu::Choice::Cpu {
            bail!("--chain gpu needs the drawing on the graphics card: not with --renderer cpu (or use --chain auto)");
        }
        if crate::render::shade::lit() && gpu_shade::choice() == gpu_shade::Choice::Cpu {
            bail!("--chain gpu needs the shading on the graphics card: not with --shading cpu (or use --chain auto)");
        }
    }
    let _ = CHOICE.set(choice);
    Ok(())
}

fn chosen(args: &mut Vec<String>, default: Option<&str>) -> Result<Choice> {
    let word = |w: Option<&str>, what: &str| -> Result<Choice> {
        Ok(match w {
            None | Some("auto") => Choice::Auto,
            Some("gpu") => Choice::Gpu,
            Some("cpu") => Choice::Cpu,
            Some(other) => bail!("{what}{other}: expected auto, gpu or cpu"),
        })
    };
    let mut choice = word(default, "BTMS_CHAIN=")?;
    while let Some(i) = args.iter().position(|a| a == "--chain") {
        // (no value, or another option in its place: said as `--threads` says it)
        let value = args.get(i + 1).map(String::as_str).filter(|v| !v.starts_with("--"));
        let Some(value) = value else {
            bail!("--chain without a value: expected auto, gpu or cpu");
        };
        choice = word(Some(value), "--chain ")?;
        args.drain(i..i + 2);
    }
    Ok(choice)
}

/// The choice of the run (auto when none was made: the tests).
pub fn choice() -> Choice {
    *CHOICE.get_or_init(|| Choice::Auto)
}

// ---------------------------------------------------------------------------------------------
// Fallbacks: blocks the processor made instead of the card, counted and told.

/// What the processor did instead of the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The whole chain of a block (made again on the processor).
    Chain,
    /// A picture of a block (the block itself or the heights around it).
    Drawing,
    /// The shading of a block.
    Shading,
}

impl Kind {
    /// The word of the `fallback` event.
    fn word(self) -> &'static str {
        match self {
            Kind::Chain => "chain",
            Kind::Drawing => "drawing",
            Kind::Shading => "shading",
        }
    }

    fn at(self) -> usize {
        self as usize
    }
}

/// Fallbacks of one kind so far.
#[derive(Default)]
struct Tally {
    blocks: usize,
    /// Of them (the chain), those that took the path without the chain once it was given up (the
    /// card drawing and shading if it still could): not wholly the processor's.
    before: usize,
    reason: String,
    /// When the event was last sent, and whether blocks were counted since.
    sent: Option<Instant>,
    unsent: bool,
}

static TALLY: Mutex<[Tally; 3]> =
    Mutex::new([const { Tally { blocks: 0, before: 0, reason: String::new(), sent: None, unsent: false } }; 3]);

/// The event is sent when the first block falls back, then at most this often while it goes on.
const TELL_EVERY: Duration = Duration::from_secs(10);

const KINDS: [Kind; 3] = [Kind::Chain, Kind::Drawing, Kind::Shading];

impl Tally {
    /// A block counted at `now` (`before`: the chain given up, it took the path without it): the
    /// event to send, the total so far and this reason, when it is due ([`Tally::due`]).
    fn count(&mut self, reason: &str, before: bool, now: Instant) -> Option<(usize, String)> {
        self.blocks += 1;
        self.before += before as usize;
        self.reason = reason.to_string();
        self.unsent = true;
        self.due(now)
    }

    /// The event to send at `now`: when blocks were counted since the last one, and it is the
    /// first or the last one sent is 10 s old (so a total that stopped growing is still told).
    fn due(&mut self, now: Instant) -> Option<(usize, String)> {
        if !self.unsent || self.sent.is_some_and(|at| now.saturating_duration_since(at) < TELL_EVERY) {
            return None;
        }
        self.sent = Some(now);
        self.unsent = false;
        Some((self.blocks, self.reason.clone()))
    }
}

thread_local! {
    /// What fell back on this thread during its block (drawing, shading): counted at its end.
    static PENDING: RefCell<[Option<String>; 3]> = const { RefCell::new([None, None, None]) };
}

/// The picture or the shading of the block this thread is making was left to the processor
/// (`render/gpu/mod.rs`, `render/gpu/shade.rs`): counted once for the block, when it ends ([`block_end`]), with
/// the first reason of the block (what happened first; what followed may only follow from it).
pub fn note(kind: Kind, reason: &str) {
    PENDING.with_borrow_mut(|p| {
        p[kind.at()].get_or_insert_with(|| reason.to_string());
    });
}

/// The block this thread was making is over: what fell back during it is counted; and the
/// totals that grew since their last event are told once it is 10 s old (when the fallbacks
/// stopped, no other block would tell them before the end).
pub fn block_end() {
    let pending = PENDING.with_borrow_mut(std::mem::take);
    for (kind, reason) in KINDS.into_iter().zip(pending) {
        if let Some(reason) = reason {
            fell_back(kind, &reason);
        }
    }
    let due: Vec<(Kind, (usize, String))> = {
        let mut tally = TALLY.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        KINDS.into_iter().filter_map(|kind| tally[kind.at()].due(now).map(|told| (kind, told))).collect()
    };
    for (kind, (blocks, reason)) in due {
        crate::progress::fallback(kind.word(), blocks, &reason);
    }
}

/// A failure seen after the card was given up (`what`: "… earlier in the render", "… during the
/// drawing"), with why it was given up (`cause`, the first failure) when that is known and not
/// said already: the reasons of the fallbacks then all say what happened to the card.
pub(crate) fn with_cause(what: &str, cause: Option<&str>) -> String {
    match cause {
        Some(cause) if !cause.is_empty() && !what.contains(cause) => format!("{what} ({cause})"),
        _ => what.to_string(),
    }
}

/// A block the processor made instead of the card: counted, and the `fallback` event sent when it
/// is the first, or when the last one sent is 10 s old.
pub fn fell_back(kind: Kind, reason: &str) {
    counted(kind, reason, false);
}

/// A block the chain did not make, that took the path without it (the chain given up).
pub fn went_before(reason: &str) {
    counted(Kind::Chain, reason, true);
}

fn counted(kind: Kind, reason: &str, before: bool) {
    let tell = TALLY.lock().unwrap_or_else(|e| e.into_inner())[kind.at()].count(reason, before, Instant::now());
    if let Some((blocks, reason)) = tell {
        crate::progress::fallback(kind.word(), blocks, &reason);
    }
}

/// The end of the blocks: the `fallback` event once more for each kind that fell back, with its
/// total, and the line that says what the processor did instead of the card (None when the card
/// was not to do anything: `card` false).
pub fn end(card: bool) -> Option<String> {
    let tally = TALLY.lock().unwrap_or_else(|e| e.into_inner());
    for kind in KINDS {
        let t = &tally[kind.at()];
        if t.blocks > 0 {
            crate::progress::fallback(kind.word(), t.blocks, &t.reason);
        }
    }
    line(&tally, card)
}

/// The line of [`end`].
fn line(tally: &[Tally; 3], card: bool) -> Option<String> {
    let mut parts = Vec::new();
    for kind in KINDS {
        let t = &tally[kind.at()];
        if t.blocks == 0 {
            continue;
        }
        let count = |n: usize| if n == 1 { "1 block".to_string() } else { format!("{n} blocks") };
        let blocks = count(t.blocks);
        parts.push(match kind {
            Kind::Chain => {
                let whole = t.blocks - t.before;
                let mut ways = Vec::new();
                if whole > 0 {
                    ways.push(format!("{} made again entirely by the CPU", count(whole)));
                }
                if t.before > 0 {
                    ways.push(format!("{} through the path without the chain (the card draws and shades if it still can, the CPU does the rest)", count(t.before)));
                }
                format!("{blocks} not made by the chain on the card: {} (last reason: {})", ways.join(", "), t.reason)
            }
            Kind::Drawing => format!("{blocks} with at least one picture drawn by the CPU instead of the card (last reason: {})", t.reason),
            Kind::Shading => format!("{blocks} shaded by the CPU instead of the card (last reason: {})", t.reason),
        });
    }
    if !card && parts.is_empty() {
        return None;
    }
    Some(if parts.is_empty() {
        "CPU fallback: no blocks, the graphics card did all the work it was given".to_string()
    } else {
        format!("CPU fallback: {}", parts.join("; "))
    })
}

// ---------------------------------------------------------------------------------------------
// The chain on the card.

/// Why a block does not come back from the chain.
#[derive(Clone, Debug)]
pub enum Failure {
    /// This block only (memory refused, a picture the card does not take, the water not settled):
    /// it is made again on the processor; the chain goes on.
    Block(String),
    /// The card failed (an error, its loss, a delay): the chain is given up for the rest of the
    /// run.
    Card(String),
}

impl Failure {
    pub fn reason(&self) -> &str {
        match self {
            Failure::Block(why) | Failure::Card(why) => why,
        }
    }
}

impl From<gpu::Failure> for Failure {
    fn from(failure: gpu::Failure) -> Failure {
        match failure {
            gpu::Failure::Card(_) => Failure::Card(failure.reason()),
            _ => Failure::Block(failure.reason()),
        }
    }
}

/// The zooms 9, 8 and 7 of a block (RGBA, row 0 its north edge): `BLOCK_PX`², a half and a
/// quarter of it a side.
pub struct Zooms {
    pub z9: Vec<u8>,
    pub z8: Vec<u8>,
    pub z7: Vec<u8>,
}

/// The shader's entry points, in the order of [`Chain::pipelines`].
const ENTRIES: [&str; 15] = [
    "floor_block",
    "floor_ring",
    "compose",
    "rgba",
    "holes",
    "depths",
    "spread",
    "water_state",
    "water_seed",
    "water_rows",
    "water_columns",
    "water",
    "halve_8",
    "halve_7",
    "water_next",
];
const FLOOR_BLOCK: usize = 0;
const FLOOR_RING: usize = 1;
const COMPOSE: usize = 2;
const RGBA: usize = 3;
const HOLES: usize = 4;
const DEPTHS: usize = 5;
const SPREAD: usize = 6;
const WATER_STATE: usize = 7;
const WATER_SEED: usize = 8;
const WATER_ROWS: usize = 9;
const WATER_COLUMNS: usize = 10;
const WATER: usize = 11;
const HALVE_8: usize = 12;
const HALVE_7: usize = 13;
const WATER_NEXT: usize = 14;
const SHADER: &str = include_str!("shaders/chain.wgsl");
/// Passes of the holes (`tiles::HOLE_PASSES`) and of the ground under the water (`water::spread`).
const HOLE_PASSES: usize = 4;
const SPREAD_PASSES: usize = 4;
/// Rounds of the sweeps that spread the sea, at most (they stop once one spreads nothing): the last
/// one must change nothing, else the block is left to the processor.
const ROUNDS: usize = 12;
/// Words of `Params` in the shader.
const PARAMS: usize = 40;
/// Storage buffers the passes bind (the device must allow as many).
const STORAGE_BINDINGS: u32 = 15;
/// Sets of buffers kept on the card, at least: so many blocks in their last steps at once (a run
/// keeps one per drawing thread: [`Chain::reserve`]). A thread holds one from the lowest heights of
/// its block to its zooms back, the second pass of the margins included: with fewer sets than
/// threads, they waited for one (78 ms a block, 16 threads and 8 sets).
const SETS: usize = 8;
/// How long a block may take to come back.
const WAIT: Duration = Duration::from_secs(60);
/// Between two looks at the card, while waiting for a block.
const PAUSE: Duration = Duration::from_micros(200);

/// `BTMS_CHAIN_FAULT`: the n-th block of the chain fails (`fail:n`) or makes the chain give up
/// (`abandon:n`), to see the fallbacks.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Trial {
    None,
    Block(u64),
    Card(u64),
}

fn trial_of(value: Option<&str>) -> Trial {
    let at = |rest: &str| if rest.is_empty() { Some(1) } else { rest.strip_prefix(':').and_then(|n| n.parse().ok()) };
    let Some(v) = value else { return Trial::None };
    if let Some(rest) = v.strip_prefix("fail") {
        at(rest).map(Trial::Block).unwrap_or(Trial::None)
    } else if let Some(rest) = v.strip_prefix("abandon") {
        at(rest).map(Trial::Card).unwrap_or(Trial::None)
    } else {
        Trial::None
    }
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

/// The water buffers of a set (made at its first block with water).
struct Wet {
    /// The ground, both ways of the passes; the surface; the state of each pixel.
    g0: wgpu::Buffer,
    g1: wgpu::Buffer,
    surface: wgpu::Buffer,
    state: wgpu::Buffer,
}

/// The buffers of one block on the card.
pub struct Set {
    px: usize,
    params: wgpu::Buffer,
    /// The RGBA picture and the other one of the holes' passes.
    a: wgpu::Buffer,
    b: wgpu::Buffer,
    z8: wgpu::Buffer,
    z7: wgpu::Buffer,
    status: wgpu::Buffer,
    parts: wgpu::Buffer,
    parts_read: wgpu::Buffer,
    /// The zooms and the status, back to the processor.
    read: wgpu::Buffer,
    wet: Option<Wet>,
    /// The workgroups of the sweeps of the sea (indirect: none once it is settled), and its group.
    turns: wgpu::Buffer,
    turns_group: wgpu::BindGroup,
    /// The pieces of water and their rows' ranges, made larger when a block needs it.
    quads: wgpu::Buffer,
    rows: wgpu::Buffer,
    /// Stand for what a block without light or water does not bind (each its own: a buffer bound
    /// twice, once for writing, would be refused).
    spare: [wgpu::Buffer; 6],
}

/// Bytes of a set's readback: zoom 9, zoom 8, zoom 7, the status.
fn read_size(px: usize) -> u64 {
    (px * px * 4 + px * px + px * px / 4 + 16) as u64
}

/// The sets of the chain: those free, and how many there are.
#[derive(Default)]
struct Pool {
    free: Vec<Set>,
    made: usize,
}

/// The chain, made once for all threads on the drawing's device.
pub struct Chain {
    /// The card's name and backend, as the drawing names it.
    pub name: String,
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// The shading on the same device (None when the light is off).
    shading: Option<&'static gpu_shade::Card>,
    layout: wgpu::BindGroupLayout,
    /// The group of `water_next` (the workgroups of the sweeps).
    turns_layout: wgpu::BindGroupLayout,
    pipelines: Vec<wgpu::ComputePipeline>,
    pool: Mutex<Pool>,
    freed: Condvar,
    /// Sets that may be made ([`SETS`] at least, one per drawing thread).
    sets: std::sync::atomic::AtomicUsize,
    /// The chain was given up: the rest of the run takes the path without it.
    down: AtomicBool,
    /// Why it was given up (the first failure).
    cause: OnceLock<String>,
    /// The drawing's card is the run's (`render/gpu/mod.rs`): its failure is the chain's (the tests
    /// have a card of their own).
    run_card: bool,
    said: AtomicBool,
    trial: Trial,
    /// Blocks made, and the time they spent in the chain's steps on the card (nanoseconds).
    blocks: AtomicU64,
    nanos: AtomicU64,
    started: AtomicU64,
    /// Time the threads waited for a free set of the chain, and of the shading (nanoseconds).
    set_wait: AtomicU64,
    shade_wait: AtomicU64,
}

static CHAIN: OnceLock<Result<Chain, String>> = OnceLock::new();

/// The chain of the run, decided (and made) at the first call: None when blocks are made without
/// it (the choice, no card, the light on and the shading on the processor…; [`why_not`]).
/// Opens the drawing's card if it is not yet.
pub fn active() -> Option<&'static Chain> {
    CHAIN.get_or_init(open_chain).as_ref().ok()
}

/// Why the chain is not used, once decided ([`active`]).
pub fn why_not() -> Option<&'static str> {
    CHAIN.get_or_init(open_chain).as_ref().err().map(String::as_str)
}

fn open_chain() -> Result<Chain, String> {
    let choice = choice();
    if choice == Choice::Cpu {
        return Err("--chain cpu".to_string());
    }
    if choice == Choice::Auto {
        for tool in ["BTMS_FINGERPRINTS", "BTMS_GPU_COMPARE"] {
            if std::env::var_os(tool).is_some_and(|v| !v.is_empty() && v != "0") {
                return Err(format!("{tool} wants the pictures on the CPU"));
            }
        }
    } else if std::env::var_os("BTMS_FINGERPRINTS").is_some() {
        eprintln!("chain: BTMS_FINGERPRINTS gets no picture of the blocks made on the card (--chain gpu)");
    }
    let Some((device, queue, name)) = gpu::shared_device() else {
        return Err("the drawing is on the CPU".to_string());
    };
    let lit = crate::render::shade::lit();
    if lit && gpu_shade::choice() == gpu_shade::Choice::Cpu {
        return Err("--shading cpu: the shading stays on the CPU".to_string());
    }
    let started = Instant::now();
    let shading = if lit {
        Some(
            gpu_shade::on_drawing_device(&device, &queue, &name)
                .map_err(|e| format!("no shading possible on the drawing's card: {e}"))?,
        )
    } else {
        None
    };
    let chain =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Chain::open(device, queue, name, shading, true)))
            .unwrap_or_else(|_| Err("unexpected panic while opening".to_string()));
    match &chain {
        Ok(chain) => eprintln!("chain: on the graphics card {} ({:.1} s)", chain.name, started.elapsed().as_secs_f64()),
        Err(why) => eprintln!("chain: not used, blocks are made without it ({why})"),
    }
    chain
}

/// A line on what the chain did during the run, None when it made nothing.
pub fn summary() -> Option<String> {
    let chain = CHAIN.get()?.as_ref().ok()?;
    let blocks = chain.blocks.load(Ordering::Relaxed);
    if blocks == 0 {
        return None;
    }
    let per_block = |n: &AtomicU64| n.load(Ordering::Relaxed) as f64 / blocks as f64 / 1e6;
    Some(format!(
        "  chain on the graphics card {}: {blocks} blocks made on the card, {:.1} ms per block for its last steps (lowest heights, shading, 8 bits, holes, water, halvings, readback, waiting included); waiting for a free set of buffers: {:.1} ms per block (chain), {:.1} ms (shading){}",
        chain.name,
        per_block(&chain.nanos),
        per_block(&chain.set_wait),
        per_block(&chain.shade_wait),
        if chain.down.load(Ordering::Relaxed) { "; given up along the way" } else { "" }
    ))
}

/// What a picture of the chain is drawn with: none of its buffers are used, only its size, its
/// projection and the filter and "heights only" of its pass (`gpu::paint_kept`). Made without
/// the memory a raster of that size takes.
pub fn canvas(
    width: usize,
    height: usize,
    projection: crate::render::raster::Projection,
) -> crate::render::raster::Raster {
    // (the focal of a view from above does not depend on the size: only `project` uses it, with
    // the width and height set below)
    let mut raster = crate::render::raster::Raster::new(1, 1, projection, [0.0; 3]);
    raster.width = width;
    raster.height = height;
    raster
}

impl Chain {
    /// The shaders, their layout, and a first block made on the card (a small one, with water):
    /// whatever the card refuses is refused now, not at the first block.
    fn open(
        device: wgpu::Device,
        queue: wgpu::Queue,
        name: String,
        shading: Option<&'static gpu_shade::Card>,
        run_card: bool,
    ) -> Result<Chain, String> {
        let limits = device.limits();
        if limits.max_storage_buffers_per_shader_stage < STORAGE_BINDINGS {
            return Err(format!(
                "the card binds only {} storage buffers to a compute shader ({STORAGE_BINDINGS} needed)",
                limits.max_storage_buffers_per_shader_stage
            ));
        }
        let scopes = [wgpu::ErrorFilter::Validation, wgpu::ErrorFilter::OutOfMemory, wgpu::ErrorFilter::Internal]
            .map(|filter| device.push_error_scope(filter));
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("chain"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let compute = wgpu::ShaderStages::COMPUTE;
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: compute,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let texture = |binding: u32, sample_type: wgpu::TextureSampleType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: compute,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let mut entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: compute,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            texture(1, wgpu::TextureSampleType::Float { filterable: false }),
            texture(2, wgpu::TextureSampleType::Depth),
            texture(3, wgpu::TextureSampleType::Depth),
        ];
        // (read only: the source of the holes, the factors, the source of the ground, the pieces, their rows)
        for binding in 4..18 {
            entries.push(storage(binding, matches!(binding, 4 | 6 | 8 | 12 | 13)));
        }
        let layout = device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("chain"), entries: &entries });
        let turns_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("chain: turns"),
            entries: &[storage(0, false)],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("chain"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        // (`water_next` alone has the group of the workgroups: the sweeps read them as such, which
        // a buffer bound for writing in their own groups would forbid)
        let next_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("chain: turns"),
            bind_group_layouts: &[Some(&layout), Some(&turns_layout)],
            immediate_size: 0,
        });
        let pipelines: Vec<wgpu::ComputePipeline> = ENTRIES
            .iter()
            .map(|entry| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(entry),
                    layout: Some(if *entry == "water_next" { &next_layout } else { &pipeline_layout }),
                    module: &module,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    cache: None,
                })
            })
            .collect();
        let mut raised = None;
        for scope in scopes.into_iter().rev() {
            if let Some(e) = now(scope.pop()) {
                raised.get_or_insert(e.to_string());
            }
        }
        if let Some(e) = raised {
            return Err(format!("shaders refused ({e})"));
        }
        let trial = trial_of(std::env::var("BTMS_CHAIN_FAULT").ok().as_deref());
        let chain = Chain {
            name,
            device,
            queue,
            shading,
            layout,
            turns_layout,
            pipelines,
            pool: Mutex::new(Pool::default()),
            freed: Condvar::new(),
            sets: std::sync::atomic::AtomicUsize::new(SETS),
            down: AtomicBool::new(false),
            cause: OnceLock::new(),
            run_card,
            said: AtomicBool::new(false),
            trial: Trial::None,
            blocks: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
            started: AtomicU64::new(0),
            set_wait: AtomicU64::new(0),
            shade_wait: AtomicU64::new(0),
        };
        chain.try_out().map_err(|f| format!("try-out of a small block refused ({})", f.reason()))?;
        // (the set of the try is not kept: the blocks are of another size)
        *chain.pool.lock().unwrap_or_else(|e| e.into_inner()) = Pool::default();
        Ok(Chain {
            trial,
            blocks: AtomicU64::new(0),
            nanos: AtomicU64::new(0),
            started: AtomicU64::new(0),
            set_wait: AtomicU64::new(0),
            shade_wait: AtomicU64::new(0),
            ..chain
        })
    }

    /// A block of 128 px made through every step (water and sea included), on pictures of its
    /// own: checks the pipelines and the bind groups against the card.
    fn try_out(&self) -> Result<(), Failure> {
        let px = 128;
        let texture = |format: wgpu::TextureFormat| {
            self.device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("try-out"),
                    size: wgpu::Extent3d { width: px as u32, height: px as u32, depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let block = Drawn {
            colour: Some(texture(wgpu::TextureFormat::Rgba32Float)),
            depth: texture(wgpu::TextureFormat::Depth32Float),
            width: px as u32,
            height: px as u32,
        };
        let water = Spans { all_open: true, open_test: false, areas: Vec::new(), quads: Vec::new(), rows: Vec::new() };
        let set = self.take(px)?;
        let parts = self.floor(&set, &block, &block, [0, 0, px, px], [px, px], 3000.0);
        let zooms = parts.and_then(|_| self.finish(set, &block, None, Some(&water), 3000.0, None))?;
        // (nothing drawn in the pictures of the try: zero depths, so the ground everywhere, dry)
        if zooms.z9.len() != px * px * 4 || zooms.z8.len() != px * px || zooms.z7.len() != px * px / 4 {
            return Err(Failure::Card("try-out: wrong sizes read back".to_string()));
        }
        Ok(())
    }

    /// One set of buffers may be made per drawing thread of the run (at least [`SETS`]), so that no
    /// thread waits for one.
    pub fn reserve(&self, drawers: usize) {
        self.sets.fetch_max(drawers, Ordering::Relaxed);
    }

    /// Whether blocks still go through the chain (it was not given up, the card still draws).
    pub fn usable(&self) -> bool {
        !self.down.load(Ordering::Relaxed) && !self.card_down()
    }

    /// The drawing's card failed.
    fn card_down(&self) -> bool {
        self.run_card && gpu::card_down()
    }

    /// Why the chain is not used any more: its own first failure, else that of the drawing's card.
    fn cause(&self) -> Option<&str> {
        self.cause.get().map(String::as_str).or_else(|| if self.run_card { gpu::cause() } else { None })
    }

    /// The reason of a block that took the path without the chain, once it was given up.
    pub fn given_up(&self) -> String {
        with_cause("the chain on the card was given up earlier in the render", self.cause())
    }

    /// The chain is given up: said once.
    fn give_up(&self, why: &str) {
        // (on one line: a reason of the fallbacks after it)
        let _ = self.cause.set(why.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" "));
        self.down.store(true, Ordering::Relaxed);
        self.freed.notify_all();
        if !self.said.swap(true, Ordering::Relaxed) {
            eprintln!(
                "chain: the graphics card failed ({why}); the remaining blocks take the path without the chain (the card draws and shades if it still can, the CPU does the rest)"
            );
        }
    }

    /// What a failure means for the chain: given up for a failure of the card.
    pub fn failed(&self, failure: &Failure) {
        if let Failure::Card(why) = failure {
            self.give_up(why);
        }
    }

    /// Makes something on the card, watching its memory: refused, the block is left.
    fn made<T>(&self, what: &str, make: impl FnOnce() -> T) -> Result<T, Failure> {
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let made = make();
        match now(scope.pop()) {
            None => Ok(made),
            Some(error) => Err(Failure::Block(format!("card memory refused: {what}: {error}"))),
        }
    }

    fn buffer(&self, label: &str, bytes: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: bytes.max(16).next_multiple_of(4),
            usage,
            mapped_at_creation: false,
        })
    }

    /// A new set for blocks of `px` pixels a side.
    fn set(&self, px: usize) -> Result<Set, Failure> {
        let pixels = (px * px) as u64;
        let storage = wgpu::BufferUsages::STORAGE;
        let copied = storage | wgpu::BufferUsages::COPY_SRC;
        let read = wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST;
        self.made(&format!("buffers of a {px} px block"), || {
            let turns =
                self.buffer("chain: turns", 16, storage | wgpu::BufferUsages::INDIRECT | wgpu::BufferUsages::COPY_DST);
            let turns_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("chain: turns"),
                layout: &self.turns_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: turns.as_entire_binding() }],
            });
            Set {
                px,
                turns,
                turns_group,
                params: self.buffer(
                    "chain: parameters",
                    (PARAMS * 4) as u64,
                    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                ),
                a: self.buffer("chain: zoom 9", pixels * 4, copied),
                b: self.buffer("chain: holes", pixels * 4, storage),
                z8: self.buffer("chain: zoom 8", pixels, copied),
                z7: self.buffer("chain: zoom 7", pixels / 4, copied),
                status: self.buffer("chain: status", 16, copied | wgpu::BufferUsages::COPY_DST),
                parts: self.buffer("chain: lowest heights", 256, copied | wgpu::BufferUsages::COPY_DST),
                parts_read: self.buffer("chain: lowest heights read back", 256, read),
                read: self.buffer("chain: readback", read_size(px), read),
                wet: None,
                quads: self.buffer("chain: water pieces", 16 * 64, storage | wgpu::BufferUsages::COPY_DST),
                rows: self.buffer("chain: water rows", 8 * 4096, storage | wgpu::BufferUsages::COPY_DST),
                spare: [0, 1, 2, 3, 4, 5].map(|_| self.buffer("chain: unused", 16, storage)),
            }
        })
    }

    /// A free set for blocks of `px`; a new one while there are fewer than [`SETS`]; else the
    /// first one given back.
    pub fn take(&self, px: usize) -> Result<Set, Failure> {
        let t = Instant::now();
        let taken = time(Phase::ChainSet, || self.take_in(px));
        self.set_wait.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        taken
    }

    fn take_in(&self, px: usize) -> Result<Set, Failure> {
        let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if self.down.load(Ordering::Relaxed) {
                return Err(Failure::Card(with_cause("the chain was given up", self.cause())));
            }
            if let Some(i) = pool.free.iter().position(|s| s.px == px) {
                return Ok(pool.free.swap_remove(i));
            }
            if pool.made < self.sets.load(Ordering::Relaxed) {
                pool.made += 1;
                drop(pool);
                let set = self.set(px);
                if set.is_err() {
                    self.pool.lock().unwrap_or_else(|e| e.into_inner()).made -= 1;
                }
                return set;
            }
            if let Some(old) = pool.free.pop() {
                // (a set of another size: made again at this one)
                drop(pool);
                drop(old);
                let set = self.set(px);
                if set.is_err() {
                    let mut pool = self.pool.lock().unwrap_or_else(|e| e.into_inner());
                    pool.made -= 1;
                }
                return set;
            }
            pool = self.freed.wait_timeout(pool, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Gives a set back, its block back on the processor.
    pub fn give(&self, set: Set) {
        self.pool.lock().unwrap_or_else(|e| e.into_inner()).free.push(set);
        self.freed.notify_one();
    }

    /// A set whose block did not come back: its state is not known, it is not kept.
    fn lose(&self, set: Set) {
        drop(set);
        self.pool.lock().unwrap_or_else(|e| e.into_inner()).made -= 1;
        self.freed.notify_one();
    }

    /// The water buffers of a set, made at its first block with water.
    fn wet(&self, set: &mut Set) -> Result<(), Failure> {
        if set.wet.is_none() {
            let pixels = (set.px * set.px) as u64;
            let storage = wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC;
            set.wet = Some(self.made("water buffers of a block", || Wet {
                g0: self.buffer("chain: ground", pixels * 4, storage),
                g1: self.buffer("chain: ground (other)", pixels * 4, storage),
                surface: self.buffer("chain: water surface", pixels * 4, storage),
                state: self.buffer("chain: water state", pixels * 4, storage),
            })?);
        }
        Ok(())
    }

    /// The parameters of the shader for a block.
    #[allow(clippy::too_many_arguments)]
    fn params(
        &self,
        set: &Set,
        frame: [usize; 2],
        window: [usize; 4],
        lit: bool,
        water: Option<&Spans>,
        top: f32,
    ) -> [u32; PARAMS] {
        let mut p = [0u32; PARAMS];
        let deep = crate::world::water::DEEP.map(|c| (c * 255.0 + 0.5) as u8 as u32);
        p[..12].copy_from_slice(&[
            set.px as u32,
            frame[0] as u32,
            frame[1] as u32,
            window[0] as u32,
            window[1] as u32,
            lit as u32,
            water.map_or(0, |w| w.quads.len() as u32),
            water.map_or(0, |w| w.areas.len().min(4) as u32),
            water.is_some_and(|w| w.all_open) as u32,
            water.is_some_and(|w| w.open_test) as u32,
            deep[0] | deep[1] << 8 | deep[2] << 16 | 255 << 24,
            0,
        ]);
        p[12] = top.to_bits();
        p[13] = gpu::FAR.to_bits();
        if let Some(w) = water {
            for (k, a) in w.areas.iter().take(4).enumerate() {
                p[16 + 4 * k..20 + 4 * k].copy_from_slice(a);
            }
        }
        for k in 0..3 {
            p[32 + k] = crate::world::water::SHALLOW[k].to_bits();
            p[36 + k] = crate::world::water::DEEP[k].to_bits();
        }
        p
    }

    /// The bind group of a block: `flip` swaps the two pictures of the holes and the two grounds
    /// (the passes go from one to the other).
    fn group(
        &self,
        set: &Set,
        block: &Drawn,
        around: &Drawn,
        shading: Option<&gpu_shade::Set>,
        flip: bool,
    ) -> wgpu::BindGroup {
        let colour = block.colour.as_ref().expect("a block has colours");
        let (src, dst) = if flip { (&set.a, &set.b) } else { (&set.b, &set.a) };
        let (factors, heights) = match shading {
            Some(s) => (s.factors_buffer(), s.heights_buffer()),
            None => (&set.spare[0], &set.spare[1]),
        };
        let (gsrc, gdst, surface, state) = match &set.wet {
            Some(w) if flip => (&w.g0, &w.g1, &w.surface, &w.state),
            Some(w) => (&w.g1, &w.g0, &w.surface, &w.state),
            None => (&set.spare[2], &set.spare[3], &set.spare[4], &set.spare[5]),
        };
        fn buffer(binding: u32, b: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry { binding, resource: b.as_entire_binding() }
        }
        fn view(binding: u32, v: &wgpu::TextureView) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry { binding, resource: wgpu::BindingResource::TextureView(v) }
        }
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chain"),
            layout: &self.layout,
            entries: &[
                buffer(0, &set.params),
                view(1, colour),
                view(2, &block.depth),
                view(3, &around.depth),
                buffer(4, src),
                buffer(5, dst),
                buffer(6, factors),
                buffer(7, heights),
                buffer(8, gsrc),
                buffer(9, gdst),
                buffer(10, surface),
                buffer(11, state),
                buffer(12, &set.quads),
                buffer(13, &set.rows),
                buffer(14, &set.status),
                buffer(15, &set.z8),
                buffer(16, &set.z7),
                buffer(17, &set.parts),
            ],
        })
    }

    /// Runs the passes `(entry, workgroups x, y, flip)` in one compute pass.
    fn passes(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        groups: &[wgpu::BindGroup; 2],
        passes: &[(usize, u32, u32, bool)],
    ) {
        let mut pass =
            encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("chain"), timestamp_writes: None });
        for &(entry, x, y, flip) in passes {
            pass.set_pipeline(&self.pipelines[entry]);
            pass.set_bind_group(0, &groups[flip as usize], &[]);
            pass.dispatch_workgroups(x, y, 1);
        }
    }

    /// The rounds of the sweeps of the sea, in one compute pass: each its rows and columns (as many
    /// workgroups as `turns` holds), then `water_next`, which stops the next ones once one spreads
    /// nothing.
    fn sweeps(&self, encoder: &mut wgpu::CommandEncoder, groups: &[wgpu::BindGroup; 2], set: &Set) {
        let mut pass = encoder
            .begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("chain: sea"), timestamp_writes: None });
        for _ in 0..ROUNDS {
            for entry in [WATER_ROWS, WATER_COLUMNS] {
                pass.set_pipeline(&self.pipelines[entry]);
                pass.set_bind_group(0, &groups[0], &[]);
                pass.dispatch_workgroups_indirect(&set.turns, 0);
            }
            pass.set_pipeline(&self.pipelines[WATER_NEXT]);
            pass.set_bind_group(0, &groups[0], &[]);
            pass.set_bind_group(1, &set.turns_group, &[]);
            pass.dispatch_workgroups(1, 1, 1);
        }
    }

    /// Hands commands to the card under error scopes: an error raised while they are recorded
    /// and handed over is the card's (memory refused: the block's).
    fn submit(&self, record: impl FnOnce(&mut wgpu::CommandEncoder) -> Result<(), Failure>) -> Result<(), Failure> {
        let scopes = [wgpu::ErrorFilter::Validation, wgpu::ErrorFilter::OutOfMemory, wgpu::ErrorFilter::Internal]
            .map(|filter| self.device.push_error_scope(filter));
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let recorded = record(&mut encoder);
        if recorded.is_ok() {
            self.queue.submit([encoder.finish()]);
        }
        let mut raised: Option<Failure> = None;
        for (scope, filter) in scopes.into_iter().zip([0, 1, 2]).rev() {
            if let Some(e) = now(scope.pop()) {
                let failure = if filter == 1 {
                    Failure::Block(format!("card memory refused: {e}"))
                } else {
                    Failure::Card(e.to_string())
                };
                if raised.is_none() || matches!(failure, Failure::Block(_)) {
                    raised = Some(failure);
                }
            }
        }
        match (recorded, raised) {
            (_, Some(failure)) => Err(failure),
            (recorded, None) => recorded,
        }
    }

    /// Waits for `count` mappings, looking at the card and sleeping in turn.
    fn wait(&self, back: &Receiver<Result<(), wgpu::BufferAsyncError>>, count: usize) -> Result<(), Failure> {
        let deadline = Instant::now() + WAIT;
        let mut mapped = 0;
        loop {
            self.device.poll(wgpu::PollType::Poll).map_err(|e| Failure::Card(format!("waiting for the card: {e}")))?;
            while let Ok(result) = back.try_recv() {
                result.map_err(|e| Failure::Card(format!("readback: {e}")))?;
                mapped += 1;
            }
            if mapped == count {
                return Ok(());
            }
            if self.card_down() {
                return Err(Failure::Card(with_cause("the card failed during the chain", gpu::cause())));
            }
            if Instant::now() > deadline {
                return Err(Failure::Card(format!("no response in {} s", WAIT.as_secs())));
            }
            std::thread::sleep(PAUSE);
        }
    }

    /// Waits for everything handed to the card so far (the measures of [`Chain::finish`]).
    fn idle(&self) -> Result<(), Failure> {
        self.device
            .poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(WAIT) })
            .map(|_| ())
            .map_err(|e| Failure::Card(format!("waiting for the card: {e}")))
    }

    /// Step 2 of the light: the lowest heights of the 64 parts of the block (row by row from the
    /// north; infinite where nothing is drawn), from the block and from the two pixels around it in
    /// the first pass of the margins (`around`, `frame` pixels, the block at `window`), as
    /// `Frame::far` finds them. The set is the block's until it is given back.
    pub fn floor(
        &self,
        set: &Set,
        block: &Drawn,
        around: &Drawn,
        window: [usize; 4],
        frame: [usize; 2],
        top: f32,
    ) -> Result<[f32; 64], Failure> {
        time(Phase::ChainFloor, || {
            let px = set.px;
            if block.width as usize != px || window[2] != px || window[3] != px || !px.is_multiple_of(128) {
                return Err(Failure::Block("a block the chain does not take".to_string()));
            }
            self.queue.write_buffer(
                &set.params,
                0,
                bytemuck::cast_slice(&self.params(set, frame, window, false, None, top)),
            );
            let groups = [self.group(set, block, around, None, false), self.group(set, block, around, None, true)];
            let n = px as u32;
            self.submit(|encoder| {
                encoder.clear_buffer(&set.parts, 0, None);
                self.passes(
                    encoder,
                    &groups,
                    &[
                        (FLOOR_BLOCK, n / 16, n / 16, false),
                        (FLOOR_RING, (n + 4).div_ceil(16), (n + 4).div_ceil(16), false),
                    ],
                );
                encoder.copy_buffer_to_buffer(&set.parts, 0, &set.parts_read, 0, 256);
                Ok(())
            })?;
            let (sender, receiver) = channel();
            set.parts_read.map_async(wgpu::MapMode::Read, .., move |mapped| {
                let _ = sender.send(mapped);
            });
            let waited = self.wait(&receiver, 1);
            let parts = waited.and_then(|()| {
                let view = set
                    .parts_read
                    .get_mapped_range(..)
                    .map_err(|e| Failure::Card(format!("readback of the lowest heights: {e}")))?;
                let bits: Vec<u32> = view.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect();
                let mut parts = [f32::INFINITY; 64];
                for (part, b) in parts.iter_mut().zip(bits) {
                    if b != 0 {
                        *part = top - f32::from_bits(b);
                    }
                }
                Ok(parts)
            });
            set.parts_read.unmap();
            parts
        })
    }

    /// Step 4: the rest of the block on the card, back as its three zooms. `shading`: the heights
    /// around the block (both passes drawn) and the plan of the shading; `water`: the water over
    /// it ([`crate::world::water::Water::spans`]). The set is given back (or let go when the block did not
    /// come back). `stages`: each step handed to the card and waited for apart, its time noted
    /// (the measures; slower).
    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        &self,
        mut set: Set,
        block: &Drawn,
        shading: Option<(&Drawn, &gpu_shade::Plan)>,
        water: Option<&Spans>,
        top: f32,
        stages: Option<&mut Vec<(&'static str, f64)>>,
    ) -> Result<Zooms, Failure> {
        let started = Instant::now();
        let number = self.started.fetch_add(1, Ordering::Relaxed) + 1;
        let result = self.finish_in(&mut set, block, shading, water, top, stages, number);
        match &result {
            Ok(_) => {
                self.give(set);
                self.blocks.fetch_add(1, Ordering::Relaxed);
                self.nanos.fetch_add(started.elapsed().as_nanos() as u64, Ordering::Relaxed);
            }
            Err(Failure::Block(_)) => self.give(set),
            Err(Failure::Card(_)) => self.lose(set),
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_in(
        &self,
        set: &mut Set,
        block: &Drawn,
        shading: Option<(&Drawn, &gpu_shade::Plan)>,
        water: Option<&Spans>,
        top: f32,
        mut stages: Option<&mut Vec<(&'static str, f64)>>,
        number: u64,
    ) -> Result<Zooms, Failure> {
        let px = set.px;
        if block.width as usize != px
            || block.height as usize != px
            || !px.is_multiple_of(128)
            || water.is_some_and(|w| w.areas.len() > 4)
        {
            return Err(Failure::Block("a block the chain does not take".to_string()));
        }
        if water.is_some() {
            self.wet(set)?;
        }
        // the pieces of water and their rows' ranges
        if let Some(w) = water {
            let quads: Vec<[u32; 4]> = w.quads.iter().map(|q| [q.z.to_bits(), q.y0, q.y1, q.at]).collect();
            for (buffer, bytes, label) in [
                (&mut set.quads, bytemuck::cast_slice::<[u32; 4], u8>(&quads), "chain: water pieces"),
                (&mut set.rows, bytemuck::cast_slice::<[u32; 2], u8>(&w.rows), "chain: water rows"),
            ] {
                if bytes.len() as u64 > buffer.size() {
                    let size = (bytes.len() as u64).next_power_of_two();
                    *buffer = self.made(label, || {
                        self.buffer(label, size, wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST)
                    })?;
                }
                if !bytes.is_empty() {
                    self.queue.write_buffer(buffer, 0, bytes);
                }
            }
        }
        let set: &Set = set;
        // the shading's set, the heights written into it by the chain
        let shade = match (shading, self.shading) {
            (Some((around, plan)), Some(card)) => {
                let t = Instant::now();
                let set = time(Phase::ChainSet, || card.take_for(plan)).map_err(Failure::Block)?;
                self.shade_wait.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
                Some((around, plan, card, set))
            }
            (Some(_), None) => return Err(Failure::Block("no shading on the card".to_string())),
            _ => None,
        };
        let result = (|| -> Result<Zooms, Failure> {
            let (around, frame, window) = match &shade {
                Some((around, plan, ..)) => (*around, [plan.width(), plan.height()], plan.window()),
                None => (block, [px, px], [0, 0, px, px]),
            };
            self.queue.write_buffer(
                &set.params,
                0,
                bytemuck::cast_slice(&self.params(set, frame, window, shade.is_some(), water, top)),
            );
            // (the sweeps of the sea begin with a workgroup per 64 rows)
            self.queue.write_buffer(&set.turns, 0, bytemuck::cast_slice(&[(px as u32).div_ceil(64), 1, 1, 0]));
            let shade_set = shade.as_ref().map(|(_, _, _, s)| s);
            let groups =
                [self.group(set, block, around, shade_set, false), self.group(set, block, around, shade_set, true)];
            let (g, n) = (&groups, px as u32);
            let tiles = n / 16;
            let pixels = (px * px) as u64;
            // the steps, in their order: handed to the card in one submission, unless each is
            // measured (`stages`)
            type Step<'a> = (&'static str, Box<dyn Fn(&mut wgpu::CommandEncoder) + 'a>);
            let mut steps: Vec<Step<'_>> = Vec::new();
            steps.push((
                "start",
                Box::new(move |encoder: &mut wgpu::CommandEncoder| encoder.clear_buffer(&set.status, 0, None)),
            ));
            if let Some((_, plan, card, s)) = &shade {
                let (fw, fh, plan, card) = (frame[0] as u32, frame[1] as u32, *plan, *card);
                steps.push((
                    "heights for the shading",
                    Box::new(move |encoder: &mut wgpu::CommandEncoder| {
                        self.passes(encoder, g, &[(COMPOSE, fw.div_ceil(16), fh.div_ceil(16), false)])
                    }),
                ));
                steps.push((
                    "shading (ten passes)",
                    Box::new(move |encoder: &mut wgpu::CommandEncoder| card.record(s, plan, encoder)),
                ));
            }
            steps.push((
                "8 bits (and the shading's factors)",
                Box::new(move |encoder: &mut wgpu::CommandEncoder| {
                    self.passes(encoder, g, &[(RGBA, tiles, tiles, false)])
                }),
            ));
            // the holes: from a to b, b to a… (4 passes: back in a)
            steps.push((
                "holes (4 passes)",
                Box::new(move |encoder: &mut wgpu::CommandEncoder| {
                    let holes: Vec<(usize, u32, u32, bool)> =
                        (0..HOLE_PASSES).map(|k| (HOLES, tiles, tiles, k % 2 == 0)).collect();
                    self.passes(encoder, g, &holes);
                }),
            ));
            if water.is_some() {
                steps.push((
                    "water: ground, state, sea, colours",
                    Box::new(move |encoder: &mut wgpu::CommandEncoder| {
                        // the ground: into g0, then g0 to g1, g1 to g0… (4 passes: back in g0)
                        let mut first: Vec<(usize, u32, u32, bool)> = vec![(DEPTHS, tiles, tiles, false)];
                        first.extend((0..SPREAD_PASSES).map(|k| (SPREAD, tiles, tiles, k % 2 == 0)));
                        first.extend([(WATER_STATE, tiles, tiles, false), (WATER_SEED, tiles, tiles, false)]);
                        self.passes(encoder, g, &first);
                        // the sea spreads, round after round, until a round spreads nothing
                        self.sweeps(encoder, g, set);
                        self.passes(encoder, g, &[(WATER, tiles, tiles, false)]);
                    }),
                ));
            }
            steps.push((
                "zooms 8 and 7",
                Box::new(move |encoder: &mut wgpu::CommandEncoder| {
                    self.passes(
                        encoder,
                        g,
                        &[(HALVE_8, tiles / 2, tiles / 2, false), (HALVE_7, tiles / 4, tiles / 4, false)],
                    )
                }),
            ));
            steps.push((
                "copy to the readback",
                Box::new(move |encoder: &mut wgpu::CommandEncoder| {
                    encoder.copy_buffer_to_buffer(&set.a, 0, &set.read, 0, pixels * 4);
                    encoder.copy_buffer_to_buffer(&set.z8, 0, &set.read, pixels * 4, pixels);
                    encoder.copy_buffer_to_buffer(&set.z7, 0, &set.read, pixels * 5, pixels / 4);
                    encoder.copy_buffer_to_buffer(&set.status, 0, &set.read, pixels * 5 + pixels / 4, 16);
                }),
            ));
            match stages.as_deref_mut() {
                Some(stages) => {
                    for (name, record) in &steps {
                        let t = Instant::now();
                        self.submit(|encoder| {
                            record(encoder);
                            Ok(())
                        })?;
                        self.idle()?;
                        stages.push((name, t.elapsed().as_secs_f64() * 1e3));
                    }
                }
                None => time(Phase::ChainRecord, || {
                    self.submit(|encoder| {
                        for (_, record) in &steps {
                            record(encoder);
                        }
                        Ok(())
                    })
                })?,
            }
            drop(steps);
            // back, once
            let t = Instant::now();
            let (sender, receiver) = channel();
            set.read.map_async(wgpu::MapMode::Read, .., move |mapped| {
                let _ = sender.send(mapped);
            });
            let waited = time(Phase::ChainWait, || self.wait(&receiver, 1));
            let zooms = waited.and_then(|()| {
                time(Phase::ChainCopy, || {
                    let view = set
                        .read
                        .get_mapped_range(..)
                        .map_err(|e| Failure::Card(format!("readback of the block: {e}")))?;
                    let (p, q) = ((pixels * 4) as usize, (pixels * 5) as usize);
                    let r = q + (pixels / 4) as usize;
                    let still = u32::from_le_bytes([view[r + 4], view[r + 5], view[r + 6], view[r + 7]]);
                    if water.is_some() && still != 0 {
                        return Err(Failure::Block(format!(
                            "water: the sea was still spreading after {ROUNDS} rounds of sweeps"
                        )));
                    }
                    Ok(Zooms { z9: view[..p].to_vec(), z8: view[p..q].to_vec(), z7: view[q..r].to_vec() })
                })
            });
            set.read.unmap();
            if let Some(stages) = stages.as_deref_mut() {
                stages.push(("wait and readback", t.elapsed().as_secs_f64() * 1e3));
            }
            let zooms = zooms?;
            match self.trial {
                Trial::Block(at) if number == at => {
                    Err(Failure::Block("failure simulated by BTMS_CHAIN_FAULT".to_string()))
                }
                Trial::Card(at) if number == at => {
                    Err(Failure::Card("abandon simulated by BTMS_CHAIN_FAULT".to_string()))
                }
                _ => Ok(zooms),
            }
        })();
        if let Some((_, _, card, s)) = shade {
            // (a block that did not come back: the shading's set is in an unknown state too, let
            // go, so that another can be made)
            match &result {
                Err(Failure::Card(_)) => card.lose(s),
                _ => card.give_back(s),
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_option_is_read_and_taken_out() {
        let mut args: Vec<String> = ["tiles", "--chain", "cpu", "output"].iter().map(|s| s.to_string()).collect();
        assert_eq!(chosen(&mut args, None).unwrap(), Choice::Cpu);
        assert_eq!(args, ["tiles", "output"]);
        assert_eq!(chosen(&mut vec!["tiles".into()], Some("gpu")).unwrap(), Choice::Gpu);
        assert_eq!(
            chosen(&mut vec!["tiles".into(), "--chain".into(), "auto".into()], Some("cpu")).unwrap(),
            Choice::Auto
        );
        assert_eq!(
            format!("{:#}", chosen(&mut vec!["--chain".into(), "turbo".into()], None).unwrap_err()),
            "--chain turbo: expected auto, gpu or cpu"
        );
        assert_eq!(
            format!("{:#}", chosen(&mut vec![], Some("turbo")).unwrap_err()),
            "BTMS_CHAIN=turbo: expected auto, gpu or cpu"
        );
        assert!(chosen(&mut vec!["--chain".into()], None).is_err());
        assert_eq!(
            format!("{:#}", chosen(&mut vec!["--chain".into()], None).unwrap_err()),
            "--chain without a value: expected auto, gpu or cpu"
        );
        assert_eq!(
            format!("{:#}", chosen(&mut vec!["--chain".into(), "--dry-run".into()], None).unwrap_err()),
            "--chain without a value: expected auto, gpu or cpu"
        );
    }

    #[test]
    fn the_trials_are_read() {
        assert_eq!(trial_of(None), Trial::None);
        assert_eq!(trial_of(Some("fail")), Trial::Block(1));
        assert_eq!(trial_of(Some("fail:5")), Trial::Block(5));
        assert_eq!(trial_of(Some("abandon:2")), Trial::Card(2));
        assert_eq!(trial_of(Some("abandon")), Trial::Card(1));
        assert_eq!(trial_of(Some("fails")), Trial::None);
        assert_eq!(trial_of(Some("abandons")), Trial::None);
        assert_eq!(trial_of(Some("fail:x")), Trial::None);
    }

    #[test]
    fn a_failure_of_the_card_gives_the_chain_up_and_the_rest_leaves_the_block() {
        assert!(matches!(Failure::from(gpu::Failure::Memory("x".into())), Failure::Block(_)));
        assert!(matches!(Failure::from(gpu::Failure::Picture("x".into())), Failure::Block(_)));
        assert!(matches!(Failure::from(gpu::Failure::Card("x".into())), Failure::Card(_)));
        assert!(Failure::from(gpu::Failure::Memory("x".into())).reason().starts_with("card memory refused"));
    }

    /// The events of the fallbacks: the first at once, then at most every 10 s while blocks fall
    /// back; when they stop, the total once more 10 s after the last event (at the end of any
    /// block: `block_end`); nothing when nothing was counted since.
    #[test]
    fn a_total_that_stops_growing_is_still_told() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut t = Tally::default();
        assert_eq!(t.due(at(0)), None);
        assert_eq!(t.count("memory refused", false, at(0)), Some((1, "memory refused".to_string())));
        // 15 more within 2 s: none told
        for k in 1..16 {
            assert_eq!(t.count("memory refused again", false, at(k * 130)), None);
        }
        assert_eq!(t.due(at(9_999)), None);
        // nothing falls back any more: the total, once, 10 s after the first event
        assert_eq!(t.due(at(10_000)), Some((16, "memory refused again".to_string())));
        assert_eq!(t.due(at(30_000)), None);
        // falling back again, long after: told at once, then 10 s later
        assert_eq!(
            t.count("the chain was given up", true, at(31_000)),
            Some((17, "the chain was given up".to_string()))
        );
        assert_eq!(t.count("the chain was given up earlier", true, at(32_000)), None);
        assert_eq!(t.due(at(40_999)), None);
        assert_eq!(t.due(at(41_000)), Some((18, "the chain was given up earlier".to_string())));
        assert_eq!((t.blocks, t.before), (18, 2));
    }

    /// What follows from a failure of the card says why the card was given up, in the events and
    /// in the line of the end, whatever fell back before it (memory refused, here).
    #[test]
    fn what_follows_a_failure_of_the_card_says_its_cause() {
        let cause = "device lost: timed out";
        assert_eq!(
            with_cause("the graphics card failed earlier in the render", Some(cause)),
            "the graphics card failed earlier in the render (device lost: timed out)"
        );
        // (said already, or not known: as it is)
        assert_eq!(
            with_cause(&format!("the graphics card failed: {cause}"), Some(cause)),
            format!("the graphics card failed: {cause}")
        );
        assert_eq!(with_cause("the card failed during the chain", None), "the card failed during the chain");
        // memory refused for a block, the card lost later, a block after it 10 s later
        let mut tally: [Tally; 3] = Default::default();
        let now = Instant::now();
        let drawing = &mut tally[Kind::Drawing.at()];
        assert_eq!(
            drawing.count("card memory refused: 80 MB", false, now).map(|told| told.1).as_deref(),
            Some("card memory refused: 80 MB")
        );
        assert_eq!(drawing.count(&format!("the graphics card failed: {cause}"), false, now), None);
        let after = drawing.count(
            &with_cause("the graphics card failed earlier in the render", Some(cause)),
            false,
            now + TELL_EVERY,
        );
        assert!(after.as_ref().is_some_and(|(blocks, why)| *blocks == 3 && why.contains(cause)), "{after:?}");
        let given_up = "abandon simulated by BTMS_CHAIN_FAULT";
        let chain = &mut tally[Kind::Chain.at()];
        chain.count(given_up, true, now);
        chain.count(&with_cause("the chain on the card was given up earlier in the render", Some(given_up)), true, now);
        assert_eq!(
            line(&tally, true).as_deref(),
            Some(
                "CPU fallback: 2 blocks not made by the chain on the card: 2 blocks through the path without the chain (the card draws and shades if it still can, the CPU does the rest) (last reason: the chain on the card was given up earlier in the render (abandon simulated by BTMS_CHAIN_FAULT)); 3 blocks with at least one picture drawn by the CPU instead of the card (last reason: the graphics card failed earlier in the render (device lost: timed out))"
            )
        );
        assert_eq!(
            line(&Default::default(), true).as_deref(),
            Some("CPU fallback: no blocks, the graphics card did all the work it was given")
        );
        assert_eq!(line(&Default::default(), false), None);
    }

    /// The chain given up says why to the blocks after it, and to a block that asks for a set
    /// then: its first failure of the card (a failure of a block does not give it up).
    #[test]
    fn the_chain_given_up_says_why() {
        let Some((device, queue, name)) = gpu::testing::device() else { return };
        let chain = Chain::open(device, queue, name, None, false).expect("the chain");
        assert_eq!(chain.given_up(), "the chain on the card was given up earlier in the render");
        chain.failed(&Failure::Block("card memory refused: 58 MB".to_string()));
        assert!(chain.usable());
        chain.failed(&Failure::Card("no response in 60 s".to_string()));
        chain.failed(&Failure::Card("readback: device lost".to_string()));
        assert!(!chain.usable());
        assert_eq!(chain.given_up(), "the chain on the card was given up earlier in the render (no response in 60 s)");
        assert!(
            matches!(chain.take(128), Err(Failure::Card(why)) if why == "the chain was given up (no response in 60 s)")
        );
    }
    // ---------------------------------------------------------------------------------------------
    // The chain on the card against the processor's steps, on blocks drawn by the card (a card of
    // the tests' own: `gpu::testing`). Each picture is drawn twice by the card, kept for the chain
    // and read back for the processor: the same colours and depths, so that every step after the
    // drawing is compared alone.

    use crate::config::Lighting;
    use crate::render::raster::{Image, Mode, Projection, Raster};
    use crate::render::scene::{Batch, Mask, Material, Tex};
    use crate::world::water::{Quad, Water};

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

        /// A texture of soft patches; `cut`: an alpha that crosses ½ (foliage).
        // 6.28 is the test pattern's period, deliberately not exactly τ: the test pictures stay the same
        #[allow(clippy::approx_constant)]
        fn image(&mut self, width: usize, height: usize, cut: bool) -> Image {
            let phase: Vec<f32> = (0..12).map(|_| self.range(0.0, 6.28)).collect();
            let mut rgba = Vec::with_capacity(width * height * 4);
            for y in 0..height {
                for x in 0..width {
                    let (u, v) = (x as f32 / width as f32 * 6.28, y as f32 / height as f32 * 6.28);
                    for k in 0..4 {
                        let wave = (u * (1 + k) as f32 + phase[k * 3]).sin()
                            * (v * (2 - k % 2) as f32 + phase[k * 3 + 1]).cos();
                        let value = if k == 3 && !cut { 255.0 } else { 127.5 + 120.0 * wave };
                        rgba.push(value as u8);
                    }
                }
            }
            Image { width, height, rgba }
        }
    }

    /// The vertices of a batch.
    #[derive(Default)]
    struct Arrays {
        positions: Vec<[f32; 3]>,
        uvs: Vec<[f32; 2]>,
        tints: Vec<[f32; 4]>,
        weights: Vec<[f32; 4]>,
        uvs1: Vec<[f32; 2]>,
        indices: Vec<u16>,
    }

    impl Arrays {
        fn vertex(&mut self, p: [f32; 3], uv: [f32; 2], tint: [f32; 4]) -> u16 {
            self.positions.push(p);
            self.uvs.push(uv);
            self.tints.push(tint);
            (self.positions.len() - 1) as u16
        }
    }

    /// The ground of the made-up world (metres): hills that go under the sea (z 0) and the lake
    /// (z 12), a few metres of relief.
    fn ground(x: f32, y: f32) -> f32 {
        6.0 + 9.0 * (x / 17.0).sin() * (y / 23.0).cos() + 0.04 * (x - 960.0)
    }

    /// A made-up block and what is around it: the ground in cells of 2 m (none in a corner of the
    /// block and in a few cells: the sea beyond the seabed, holes; a crack between the cells),
    /// towers toward the sun beyond the block, houses, foliage cut out, blended pieces, terrain of
    /// four layers. In the order of the drawing: opaque, cut out, blended.
    fn world(
        rng: &mut Rng,
        textures: &[Tex],
        west: f32,
        north: f32,
        side: f32,
        margin: f32,
    ) -> Vec<(Mode, Material, Arrays)> {
        let mut out = Vec::new();
        // the ground, a batch per row of cells
        let cell = 2.0f32;
        let (x0, x1, y0, y1) = (west - margin, west + side + margin, north - side - margin, north + margin);
        let mut y = y0;
        let mut row = 0;
        while y < y1 {
            let mut a = Arrays::default();
            let mut x = x0;
            while x < x1 {
                let sea = x < west + 30.0 && y < north - side + 40.0;
                if !sea && rng.below(60) != 0 {
                    // (a crack of 3 cm between the cells: single pixels missed, the holes to close)
                    let (xa, xb, ya, yb) = (x + 0.03, x + cell, y + 0.03, y + cell);
                    let tint = [0.9, 0.85, 0.8, 1.0];
                    let p = |x: f32, y: f32| [x, y, ground(x, y)];
                    let i = [
                        a.vertex(p(xa, ya), [xa / 8.0, ya / 8.0], tint),
                        a.vertex(p(xb, ya), [xb / 8.0, ya / 8.0], tint),
                        a.vertex(p(xb, yb), [xb / 8.0, yb / 8.0], tint),
                        a.vertex(p(xa, yb), [xa / 8.0, yb / 8.0], tint),
                    ];
                    a.indices.extend([i[0], i[1], i[2], i[0], i[2], i[3]]);
                }
                x += cell;
            }
            if !a.indices.is_empty() {
                out.push((Mode::Opaque, Material::Single(Some(textures[row % 2].clone())), a));
            }
            y += cell;
            row += 1;
        }
        // boxes: towers beyond the block toward the sun (north-west), houses everywhere
        let mut boxes = Arrays::default();
        for k in 0..40 {
            let tall = k < 8;
            let (cx, cy) = if tall {
                (west - rng.range(4.0, margin - 6.0), north + rng.range(-side, margin - 6.0))
            } else {
                (rng.range(x0 + 5.0, x1 - 5.0), rng.range(y0 + 5.0, y1 - 5.0))
            };
            let (hw, hd) = (rng.range(2.0, 7.0), rng.range(2.0, 7.0));
            let top = ground(cx, cy) + if tall { rng.range(30.0, 90.0) } else { rng.range(3.0, 12.0) };
            let tint = [rng.range(0.4, 1.0), rng.range(0.4, 1.0), rng.range(0.4, 1.0), 1.0];
            let i = [
                boxes.vertex([cx - hw, cy - hd, top], [0.0, 0.0], tint),
                boxes.vertex([cx + hw, cy - hd, top], [1.0, 0.0], tint),
                boxes.vertex([cx + hw, cy + hd, top - 0.5], [1.0, 1.0], tint),
                boxes.vertex([cx - hw, cy + hd, top - 0.5], [0.0, 1.0], tint),
            ];
            boxes.indices.extend([i[0], i[1], i[2], i[0], i[2], i[3]]);
        }
        out.push((Mode::Opaque, Material::Single(Some(textures[2].clone())), boxes));
        // terrain of four layers with a mask, in the block
        let mut terrain = Arrays::default();
        for _ in 0..30 {
            let (cx, cy) = (rng.range(west, west + side), rng.range(north - side, north));
            let s = rng.range(2.0, 9.0);
            for (dx, dy) in [(-s, -s), (s, -s), (0.0, s)] {
                let (x, y) = (cx + dx, cy + dy);
                let i = terrain.vertex([x, y, ground(x, y) + 0.3], [x / 5.0, y / 5.0], [1.0, 1.0, 1.0, 1.0]);
                terrain.indices.push(i);
                terrain.uvs1.push([x / 30.0, y / 30.0]);
                let (a, b) = (rng.unit(), rng.unit());
                terrain.weights.push([(1.0 - a) * (1.0 - b), a * (1.0 - b), (1.0 - a) * b, a * b]);
            }
        }
        out.push((
            Mode::Opaque,
            Material::Layers {
                layers: [Some(textures[0].clone()), None, Some(textures[1].clone()), Some(textures[2].clone())],
                mask: Some(Mask { texture: textures[3].clone(), channels: (2, 1) }),
            },
            terrain,
        ));
        // foliage, cut out: crowns of broken heights, in the block and around it
        for _ in 0..3 {
            let mut leaves = Arrays::default();
            for _ in 0..400 {
                let (cx, cy) = (rng.range(x0 + 2.0, x1 - 2.0), rng.range(y0 + 2.0, y1 - 2.0));
                let h = ground(cx, cy) + rng.range(2.0, 9.0);
                let s = rng.range(0.5, 2.5);
                for _ in 0..3 {
                    let i = leaves.vertex(
                        [cx + rng.range(-s, s), cy + rng.range(-s, s), h + rng.range(-0.5, 0.5)],
                        [rng.range(0.0, 1.0), rng.range(0.0, 1.0)],
                        [0.6, 0.9, 0.5, 1.0],
                    );
                    leaves.indices.push(i);
                }
            }
            out.push((Mode::Cutout, Material::Single(Some(textures[4].clone())), leaves));
        }
        // blended pieces (decals, glass)
        let mut blended = Arrays::default();
        for _ in 0..80 {
            let (cx, cy) = (rng.range(west, west + side), rng.range(north - side, north));
            let h = ground(cx, cy) + rng.range(0.0, 0.04);
            let s = rng.range(0.5, 4.0);
            for _ in 0..3 {
                let i = blended.vertex(
                    [cx + rng.range(-s, s), cy + rng.range(-s, s), h],
                    [rng.range(0.0, 2.0), rng.range(0.0, 2.0)],
                    [1.0, 0.9, 0.8, rng.range(0.1, 0.9)],
                );
                blended.indices.push(i);
            }
        }
        out.push((Mode::Blend, Material::Single(Some(textures[1].clone())), blended));
        out
    }

    /// Pieces of water over the block: the sea (a rectangle of the first file, which leaves the
    /// east of the block out of its rectangle: the open sea there), a lake higher up, and
    /// triangles of the four kinds whose diagonals go through the centres of pixels.
    fn water(west: f32, north: f32, side: f32, metres: f32) -> Water {
        let mut water = Water::default();
        let centre = |i: f32| i * metres + metres / 2.0;
        let mut quads = vec![
            Quad { min: [west - 50.0, north - side - 50.0], max: [west + side - 20.0, north + 50.0], kind: 0, z: 0.0 },
            Quad { min: [west + 70.0, north - 60.0], max: [west + 100.0, north - 30.0], kind: 0, z: 12.0 },
        ];
        for (k, (i, j)) in [(20.0, 30.0), (120.0, 40.0), (40.0, 140.0), (150.0, 150.0)].into_iter().enumerate() {
            // a square of 32 m whose diagonal goes through pixel centres
            let (x, y) = (west + centre(i), north - centre(j) - 32.0);
            quads.push(Quad { min: [x, y], max: [x + 32.0, y + 32.0], kind: k as u8 + 1, z: 3.0 + k as f32 });
        }
        water.add(quads);
        // a second file far away: a rectangle of its own
        water.add(vec![Quad {
            min: [west + 5000.0, north + 5000.0],
            max: [west + 5100.0, north + 5100.0],
            kind: 0,
            z: 0.0,
        }]);
        water
    }

    /// `Raster::rgba` of colours times factors (where they are not 1), as `Frame::shade` and
    /// `Raster::rgba` make it.
    fn rgba_of(raster: &Raster, factors: Option<&[f32]>) -> Vec<u8> {
        let mut out = vec![0u8; raster.colour.len() * 4];
        for (i, (c, d)) in raster.colour.iter().zip(&raster.depth).enumerate() {
            let c = match factors {
                Some(f) if f[i] != 1.0 => c.map(|v| v * f[i]),
                _ => *c,
            };
            for k in 0..3 {
                out[i * 4 + k] = (c[k].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            out[i * 4 + 3] = if d.is_finite() { 255 } else { 0 };
        }
        out
    }

    /// Pixels of two RGBA pictures: (equal, within 2/255, alpha apart, largest gap).
    fn apart(a: &[u8], b: &[u8]) -> (usize, usize, usize, u8) {
        let (mut same, mut within, mut alpha, mut worst) = (0, 0, 0, 0u8);
        for (p, q) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
            let gap = (0..3).map(|k| p[k].abs_diff(q[k])).max().unwrap();
            same += (p == q) as usize;
            within += (gap <= 2 && p[3] == q[3]) as usize;
            alpha += (p[3] != q[3]) as usize;
            worst = worst.max(gap);
        }
        (same, within, alpha, worst)
    }

    /// The lowest heights of the parts, as `Frame::far` finds them (its code, for the test).
    fn parts_of(block: &Raster, around: &Raster, window: [usize; 4], width: usize, top: f32) -> [f32; 64] {
        let [x0, y0, w, h] = window;
        let mut parts = [f32::INFINITY; 64];
        let mut lower = |part: usize, v: f32| {
            if v.is_finite() {
                parts[part] = parts[part].min(v);
            }
        };
        for (y, row) in block.depths().chunks_exact(w).enumerate() {
            for i in 0..8 {
                let deepest = row[i * w / 8..(i + 1) * w / 8]
                    .iter()
                    .filter(|d| d.is_finite())
                    .fold(f32::NEG_INFINITY, |a, d| a.max(*d));
                lower((y * 8 / h) * 8 + i, top - deepest);
            }
        }
        let height = around.height;
        let part = |p: isize, n: usize| (p.clamp(0, n as isize - 1) as usize * 8 / n).min(7);
        for y in y0.saturating_sub(2)..(y0 + h + 2).min(height) {
            for x in x0.saturating_sub(2)..(x0 + w + 2).min(width) {
                if x < x0 || x >= x0 + w || y < y0 || y >= y0 + h {
                    lower(
                        part(y as isize - y0 as isize, h) * 8 + part(x as isize - x0 as isize, w),
                        top - around.depths()[y * width + x],
                    );
                }
            }
        }
        parts
    }

    impl Chain {
        /// The bytes of a buffer of the card (the tests).
        fn read_back(&self, buffer: &wgpu::Buffer) -> Vec<u8> {
            let size = buffer.size();
            let read = self.buffer("test: readback", size, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
            self.submit(|encoder| {
                encoder.copy_buffer_to_buffer(buffer, 0, &read, 0, size);
                Ok(())
            })
            .unwrap();
            let (sender, receiver) = channel();
            read.map_async(wgpu::MapMode::Read, .., move |m| {
                let _ = sender.send(m);
            });
            self.wait(&receiver, 1).unwrap();
            let bytes = read.get_mapped_range(..).unwrap().to_vec();
            read.unmap();
            bytes
        }
    }

    fn bits(bytes: &[u8]) -> Vec<u32> {
        bytes.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect()
    }

    /// Made-up blocks, with the light and without, with water and without: every step of the chain
    /// against the processor's, from the same pictures drawn by the card. To the bit: the heights the
    /// shading reads, the lowest heights of the parts, the ground under the water, the holes (from the
    /// same 8-bit picture), the halvings; the 8-bit picture all but the rounding of a product and a
    /// sum; the water's colours to `exp`. Against the processor's own shading (`shade::factors`):
    /// 99.9 % of the pixels within 2/255, and the same transparency.
    #[test]
    fn the_chain_makes_what_the_processor_makes() {
        let Some((device, queue, name)) = gpu::testing::device() else { return };
        let Some(shading) = gpu_shade::card_for_tests(&device, &queue, &name) else { return };
        let chain = Chain::open(device, queue, name, Some(shading), false).expect("the chain");
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let textures: Vec<Tex> = [(64, 64, false), (128, 32, false), (37, 53, false), (32, 32, false), (64, 64, true)]
            .iter()
            .map(|&(w, h, cut)| Tex::new(rng.image(w, h, cut)))
            .collect();
        let (px, metres, top) = (256usize, 0.5f32, 300.0f32);
        let light = Lighting::default();
        let (mut total, mut against_cpu, mut pixels) =
            ((0usize, 0usize, 0usize, 0u8), (0usize, 0usize, 0usize, 0u8), 0usize);
        let mut seen = [0usize; 4];
        for (round, (bx, by, lit, wet)) in [
            (7, -4, true, true),
            (-3, 5, true, true),
            (2, 2, false, true),
            (11, -9, true, false),
            (-6, -6, false, false),
        ]
        .into_iter()
        .enumerate()
        {
            let side = px as f32 * metres;
            let (west, north) = (bx as f32 * side, (by + 1) as f32 * side);
            let centre = [west + side / 2.0, north - side / 2.0];
            let frame = crate::render::shade::Frame::with(&light, 48.0, 0.0, centre, metres, px, px);
            let margin = frame.margin().into_iter().max().unwrap() as f32 * metres + 4.0;
            let batches = world(&mut rng, &textures, west, north, side, margin);
            let mut stream = |sink: &mut dyn FnMut(&Batch)| -> gpu::Totals {
                for (mode, material, a) in &batches {
                    sink(&Batch {
                        mode: *mode,
                        material: material.clone(),
                        positions: &a.positions,
                        uvs: &a.uvs,
                        tints: &a.tints,
                        weights: &a.weights,
                        uvs1: &a.uvs1,
                        indices: &a.indices,
                    });
                }
                (0, [0; 8])
            };
            let water = if wet { water(west, north, side, metres) } else { Water::default() };
            let projection = Projection::Orthographic { centre, metres, top };
            // the block, kept and read back
            let (_, block) =
                gpu::testing::paint_kept(&canvas(px, px, projection), gpu::Keep::Colour([0.0; 3]), &mut stream)
                    .expect("the block");
            let mut read = Raster::new(px, px, projection, [0.0; 3]);
            assert!(gpu::testing::paint_read(&mut read, &mut stream));
            let mut set = chain.take(px).unwrap();
            let mut shading_of = None;
            let mut factors = None;
            if lit {
                let (light, reach, metres, origin, window) = frame.shading();
                let [width, height] = frame.size();
                // the first pass, kept and read back; the lowest heights of the parts
                let mut kept = frame.heights(top);
                let (_, first) = gpu::testing::paint_kept(&kept, gpu::Keep::Heights, &mut stream).unwrap();
                let mut around = frame.heights(top);
                assert!(
                    gpu::testing::paint_read(&mut around, &mut stream)
                        || around.depths().iter().all(|d| d.is_infinite())
                );
                let parts = chain.floor(&set, &block, &first, window, [width, height], top).unwrap();
                let expected = parts_of(&read, &around, window, width, top);
                assert_eq!(parts.map(f32::to_bits), expected.map(f32::to_bits), "block {bx},{by}: lowest heights");
                assert!(parts.iter().any(|p| p.is_finite()));
                // the second pass over the first
                frame.far_from(&parts, &mut kept);
                let _ = frame.far(&read, &mut around);
                let (_, second) = gpu::testing::paint_kept(&kept, gpu::Keep::HeightsOver, &mut stream).unwrap();
                gpu::testing::paint_read(&mut around, &mut stream);
                // the heights `Frame::shade` composes
                let mut heights: Vec<f32> = around.depths().iter().map(|d| top - d).collect();
                for (y, row) in read.depths().chunks_exact(px).enumerate() {
                    for (to, d) in heights[(window[1] + y) * width + window[0]..][..px].iter_mut().zip(row) {
                        *to = top - d;
                    }
                }
                let plan = gpu_shade::plan_for(width, height, origin, metres, light, reach, window).unwrap();
                factors = Some((
                    gpu_shade::factors_on(shading, &heights, width, height, origin, metres, light, reach, window)
                        .unwrap(),
                    crate::render::shade::factors(&heights, width, height, origin, metres, light, reach, window),
                    heights,
                ));
                shading_of = Some((second, plan));
            }
            let spans = water.spans(west, north, metres, px);
            assert_eq!(spans.is_some(), wet);
            let zooms = chain
                .finish_in(&mut set, &block, shading_of.as_ref().map(|(d, p)| (d, p)), spans.as_ref(), top, None, 1)
                .expect("the chain");
            // the heights the shading read, to the bit
            if let (Some((_, plan)), Some((_, _, heights))) = (&shading_of, &factors) {
                let s = shading.take_for(plan).unwrap();
                let card = bits(&chain.read_back(s.heights_buffer()));
                shading.give_back(s);
                let apart = heights.iter().zip(&card).filter(|(h, c)| h.to_bits() != **c).count();
                assert_eq!(apart, 0, "block {bx},{by}: {apart} different heights");
            }
            // the ground under the water, to the bit
            if wet {
                let ground = bits(&chain.read_back(&set.wet.as_ref().unwrap().g0));
                let expected = crate::world::water::spread(read.depths(), px, px, 4);
                let apart = expected.iter().zip(&ground).filter(|(e, g)| e.to_bits() != **g).count();
                assert_eq!(apart, 0, "block {bx},{by}: {apart} different pixels of the ground under the water");
                let state = bits(&chain.read_back(&set.wet.as_ref().unwrap().state));
                for s in state {
                    seen[s as usize] += 1;
                }
            }
            chain.give(set);
            // the processor's steps, from the picture read back
            let reference = |f: Option<&[f32]>| {
                let mut rgba = rgba_of(&read, f);
                crate::tiles::fill_holes(&mut rgba, px, px, crate::tiles::HOLE_PASSES);
                if !water.quads.is_empty() {
                    water.flood(&mut rgba, read.depths(), top, [west, north], metres, px);
                }
                rgba
            };
            let (card_factors, cpu_factors) = match &factors {
                Some((card, cpu, _)) => (Some(card.as_slice()), Some(cpu.as_slice())),
                None => (None, None),
            };
            let same_factors = reference(card_factors);
            let processor = reference(cpu_factors);
            // the halvings: the card's own, from its zoom 9, to the bit
            assert!(zooms.z8 == crate::tiles::halve(&zooms.z9, px, px), "block {bx},{by}: zoom 8");
            assert!(zooms.z7 == crate::tiles::halve(&zooms.z8, px / 2, px / 2), "block {bx},{by}: zoom 7");
            let a = apart(&same_factors, &zooms.z9);
            let b = apart(&processor, &zooms.z9);
            println!(
                "block {bx},{by} (light {lit}, water {wet}): with the card's factors, {} of {} pixels equal, {} within 2/255, transparency different {}, largest gap {}; with the processor's shading, {} equal, {} within 2/255, transparency different {}, largest gap {}",
                a.0,
                px * px,
                a.1,
                a.2,
                a.3,
                b.0,
                b.1,
                b.2,
                b.3
            );
            // (the light did shade: the unshaded picture is another one)
            if lit {
                let unshaded = apart(&reference(None), &zooms.z9);
                assert!(
                    unshaded.0 < px * px * 9 / 10,
                    "block {bx},{by}: the shading changes almost nothing ({unshaded:?})"
                );
                println!("  (without the shading, only {} pixels would be equal)", unshaded.0);
            }
            assert_eq!(a.2, 0, "round {round}: transparency");
            assert_eq!(b.2, 0, "round {round}: transparency against the processor's shading");
            for (t, x) in [(&mut total, a), (&mut against_cpu, b)] {
                *t = (t.0 + x.0, t.1 + x.1, t.2 + x.2, t.3.max(x.3));
            }
            pixels += px * px;
        }
        println!(
            "in all, {pixels} pixels: same factors {total:?}; processor's shading {against_cpu:?}; water states (dry, over the ground, no ground, sea) {seen:?}"
        );
        // the test is worth something: water over the ground, sea, and pixels still dry
        assert!(seen[1] > 1000 && seen[3] > 1000 && seen[0] > 1000, "{seen:?}");
        assert!(total.0 as f64 >= 0.999 * pixels as f64, "{total:?}");
        assert!(against_cpu.1 as f64 >= 0.999 * pixels as f64, "{against_cpu:?}");
    }
    /// The sea through a serpentine: corridors with no ground under the water, between walls of
    /// dry ground, open to the edge of the block at one end only. Few walls: the sweeps settle and
    /// the sea is the processor's; more walls than rounds: the block is left (to the processor).
    #[test]
    fn the_sea_spreads_as_far_as_the_processor_s_or_the_block_is_left() {
        let Some((device, queue, name)) = gpu::testing::device() else { return };
        let chain = Chain::open(device, queue, name, None, false).expect("the chain");
        let (px, metres, top) = (384usize, 0.5f32, 300.0f32);
        let side = px as f32 * metres;
        let (west, north) = (side * 3.0, side * 2.0);
        let centre = [west + side / 2.0, north - side / 2.0];
        let projection = Projection::Orthographic { centre, metres, top };
        // a piece of water far away: the block is beyond every file's rectangle, all open sea
        let mut water = Water::default();
        water.add(vec![Quad {
            min: [west + 9000.0, north + 9000.0],
            max: [west + 9100.0, north + 9100.0],
            kind: 0,
            z: 0.0,
        }]);
        for (walls, settles) in [(3usize, true), (16, false)] {
            // ground at 5 m (dry): bands along the top, the bottom and the right, and the walls,
            // in pixels; the corridors between them 12 px wide, the first one open to the west edge
            let mut a = Arrays::default();
            let mut rect = |x0: f32, y0: f32, x1: f32, y1: f32| {
                let p = |x: f32, y: f32| [west + x * metres, north - y * metres, 5.0];
                let tint = [0.6, 0.5, 0.4, 1.0];
                let i = [
                    a.vertex(p(x0, y0), [0.0; 2], tint),
                    a.vertex(p(x1, y0), [0.0; 2], tint),
                    a.vertex(p(x1, y1), [0.0; 2], tint),
                    a.vertex(p(x0, y1), [0.0; 2], tint),
                ];
                a.indices.extend([i[0], i[1], i[2], i[0], i[2], i[3]]);
            };
            let n = px as f32;
            rect(0.0, 0.0, n, 8.0);
            rect(0.0, n - 8.0, n, n);
            rect(n - 8.0, 0.0, n, n);
            for k in 0..walls {
                let x = 12.0 + 20.0 * k as f32;
                if k % 2 == 0 { rect(x, 8.0, x + 8.0, n - 28.0) } else { rect(x, 28.0, x + 8.0, n - 8.0) }
            }
            // the rest of the block east of the last wall: ground
            rect(12.0 + 20.0 * walls as f32 + 8.0, 8.0, n - 8.0, n - 8.0);
            let batches = [(Mode::Opaque, Material::Single(None), a)];
            let mut stream = |sink: &mut dyn FnMut(&Batch)| -> gpu::Totals {
                for (mode, material, a) in &batches {
                    sink(&Batch {
                        mode: *mode,
                        material: material.clone(),
                        positions: &a.positions,
                        uvs: &a.uvs,
                        tints: &a.tints,
                        weights: &a.weights,
                        uvs1: &a.uvs1,
                        indices: &a.indices,
                    });
                }
                (0, [0; 8])
            };
            let (_, block) =
                gpu::testing::paint_kept(&canvas(px, px, projection), gpu::Keep::Colour([0.0; 3]), &mut stream)
                    .unwrap();
            let mut read = Raster::new(px, px, projection, [0.0; 3]);
            assert!(gpu::testing::paint_read(&mut read, &mut stream));
            let spans = water.spans(west, north, metres, px).expect("some water");
            assert!(spans.all_open);
            let mut set = chain.take(px).unwrap();
            let made = chain.finish_in(&mut set, &block, None, Some(&spans), top, None, 1);
            chain.give(set);
            let mut rgba = rgba_of(&read, None);
            crate::tiles::fill_holes(&mut rgba, px, px, crate::tiles::HOLE_PASSES);
            let wet = water.flood(&mut rgba, read.depths(), top, [west, north], metres, px);
            let sea = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[3] == 255 && p[..3] == crate::world::water::DEEP.map(|c| (c * 255.0 + 0.5) as u8)[..])
                .count();
            println!(
                "{walls} walls: {wet} water pixels on the processor, {sea} of them sea; the chain: {}",
                match &made {
                    Ok(_) => "made".to_string(),
                    Err(f) => f.reason().to_string(),
                }
            );
            assert!(sea > 10_000, "{sea} sea pixels: the test checks nothing");
            if settles {
                let zooms = made.expect("the sea should have spread in enough rounds");
                assert!(zooms.z9 == rgba, "the chain's sea is not the processor's");
            } else {
                assert!(
                    matches!(&made, Err(Failure::Block(why)) if why.starts_with("water: the sea was still spreading")),
                    "{:?}",
                    made.as_ref().err()
                );
            }
        }
    }

    /// The card fails in the last step of 16 blocks at once (`Failure::Card`, as when the
    /// drawing's card is lost while they wait for it), more blocks than the shading has sets (8):
    /// the sets those blocks held are let go, not lost. Every block comes back (with its failure),
    /// a block after them gets a set, and so does the shading of the path without the chain on the
    /// same card (`gpu_shade::factors`). Each waits on a thread of its own: a set never given back
    /// would make it wait for ever, the test fails after 20 s.
    #[test]
    fn a_failure_of_the_card_lets_the_shading_s_sets_go() {
        let Some((device, queue, name)) = gpu::testing::device() else { return };
        let Some(shading) = gpu_shade::card_for_tests(&device, &queue, &name) else { return };
        let mut chain = Chain::open(device, queue, name, Some(shading), false).expect("the chain");
        // (a block numbered 1 fails as a failure of the card would, after its zooms came back)
        chain.trial = Trial::Card(1);
        let chain: &'static Chain = Box::leak(Box::new(chain));
        let blocks = 16;
        chain.reserve(blocks);
        let (px, metres, top) = (128usize, 1.0f32, 300.0f32);
        let centre = [px as f32 * 2.5, px as f32 * 3.5];
        let frame = crate::render::shade::Frame::with(&Lighting::default(), 48.0, 0.0, centre, metres, px, px);
        let (light, reach, metres, origin, window) = frame.shading();
        let light = light.clone();
        let [width, height] = frame.size();
        let plan = gpu_shade::plan_for(width, height, origin, metres, &light, reach, window).expect("the shading plan");
        let mut nothing = |_: &mut dyn FnMut(&Batch)| -> gpu::Totals { (0, [0; 8]) };
        let projection = Projection::Orthographic { centre, metres, top };
        let (_, block) =
            gpu::testing::paint_kept(&canvas(px, px, projection), gpu::Keep::Colour([0.0; 3]), &mut nothing)
                .expect("the block");
        let (_, around) =
            gpu::testing::paint_kept(&frame.heights(top), gpu::Keep::Heights, &mut nothing).expect("the margins");
        let pictures: &'static (Drawn, Drawn, gpu_shade::Plan) = Box::leak(Box::new((block, around, plan)));
        // blocks on threads of their own, numbered `number`: what came back, in the order it came
        let start = |count: usize, number: u64| {
            let (sender, receiver) = channel();
            for _ in 0..count {
                let sender = sender.clone();
                std::thread::spawn(move || {
                    let (block, around, plan) = pictures;
                    let mut set = chain.take(px).expect("a set of the chain");
                    let made =
                        chain.finish_in(&mut set, block, Some((around, plan)), None, top, None, number).map(|_| ());
                    chain.give(set);
                    let _ = sender.send(made);
                });
            }
            receiver
        };
        let wait = Duration::from_secs(20);
        let back = start(blocks, 1);
        for k in 0..blocks {
            let made = back.recv_timeout(wait).unwrap_or_else(|_| panic!("{k} of {blocks} blocks came back: the others are still waiting for a set of the shading after {} s", wait.as_secs()));
            assert!(
                matches!(&made, Err(Failure::Card(why)) if why == "abandon simulated by BTMS_CHAIN_FAULT"),
                "{made:?}"
            );
        }
        let made = start(1, 2)
            .recv_timeout(wait)
            .expect("a block after the failures is still waiting for a set of the shading");
        assert!(made.is_ok(), "{made:?}");
        // the path without the chain: the factors of a picture by the same card
        let (sender, receiver) = channel();
        std::thread::spawn(move || {
            let heights = vec![0.0f32; width * height];
            let _ = sender.send(
                gpu_shade::factors_on(shading, &heights, width, height, origin, metres, &light, reach, window)
                    .is_some(),
            );
        });
        assert!(
            receiver.recv_timeout(wait).expect("the shading of the path without the chain is still waiting for a set"),
            "the shading of the path without the chain did not shade"
        );
    }
}
