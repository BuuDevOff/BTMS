//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! Where the time of a render goes, and the fingerprints that prove a picture did not change.
//! Both are off unless asked for, and cost nothing then:
//!
//! - `--profile` (or `BTMS_PROFILE=1`): `tiles` ends with the table of the time its threads spent
//!   in each phase, per block ([`report`]);
//! - `BTMS_FINGERPRINTS=<file>`: every picture drawn appends one line to that file, the SHA-1 of
//!   its exact colours and depths ([`fingerprint`]); with the light on, also the heights read around
//!   it and the picture shaded. Two runs drew the same pictures when their sorted files are equal.
//!
//! A phase is timed where its work is done ([`time`]); the times are kept per thread and added to
//! the totals by [`flush`] (at the end of a block, by the thread that drew it and by the one that
//! wrote its tiles).

use std::cell::Cell;
use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What a thread can be busy with while it draws a block.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// A whole block, everything below included but the writing of its tiles (`Write`).
    Block,
    /// `read_file`: finding the file (the archives on its path and their tables) and extracting it.
    ReadFile,
    /// Inside `ReadFile`: the bytes of the file out of its archive (disk, copy, decryption).
    Extract,
    /// Inflating a resource (`Resource::parse`).
    Inflate,
    /// Parsing models (ydr, ydd, yft).
    Models,
    /// Parsing texture dictionaries (ytd).
    Dictionaries,
    /// Decoding textures to RGBA.
    Decode,
    /// Getting from the shared cache what another thread loaded (or is loading: the wait).
    Shared,
    /// `paint`, everything it triggers included (the loads above).
    Paint,
    /// Inside `Paint`: building the batches (vertices to world space, tints, weights), loads included.
    Scene,
    /// Inside `Paint`: drawing the batches (triangles to pixels).
    Raster,
    /// Creating the colour and depth buffers.
    Canvas,
    /// The picture to RGBA bytes.
    Rgba,
    /// The light, its drawing apart (the heights around a block are drawn by `paint`, and counted
    /// there): the entities around the block, its lowest points, the shading itself.
    Shade,
    /// Closing the small holes.
    Holes,
    /// Halving and cropping (zooms 8 and 7, the 21 tiles of a block).
    Shrink,
    /// Encoding the tiles (JPEG, PNG).
    Encode,
    /// The tile files: their folder, creating, writing, renaming them.
    Files,
    /// Inside `Paint`, on the graphics card (`render/gpu`): gathering the batches (vertices projected,
    /// triangles kept), the uploads below included.
    GpuPack,
    /// Inside `GpuPack`: sending textures to the card.
    GpuTextures,
    /// Sending the geometry, recording the drawing and handing it to the card.
    GpuDraw,
    /// Waiting for the picture and copying it back.
    GpuRead,
    /// Checking that nothing was drawn on the picture yet: the first reading of its buffers, when
    /// the system hands their memory over.
    GpuStart,
    /// The writing of a block's tiles, on a thread of its own (`tiles/write.rs`): `Shrink`,
    /// `Encode` and `Files` included.
    Write,
    /// A drawing thread waiting for room in the queue of the writing threads, its block drawn.
    Queue,
    /// The chain of a block on the card (`chain.rs`): the lowest heights of its parts, reduced on
    /// the card and read back (the wait included).
    ChainFloor,
    /// Recording its last steps and handing them to the card.
    ChainRecord,
    /// Waiting for the block to come back.
    ChainWait,
    /// Copying its three zooms out of the card's memory.
    ChainCopy,
    /// Waiting for a free set of buffers of the chain or of its shading (a new one made included).
    ChainSet,
}

const PHASES: usize = 30;

/// What the drawing of a block is made of ([`count`]).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Count {
    /// Triangles handed to the raster.
    Triangles,
    /// Those that are not left out at once (wholly outside the picture, flat, behind the camera).
    Kept,
    /// Pixels of the boxes of those triangles: each is tested.
    Tested,
    /// Pixels inside a triangle.
    Covered,
    /// Pixels painted (not hidden by something nearer), by kind: one texture or none.
    Opaque,
    /// Four layers of terrain.
    Layered,
    Cutout,
    Blended,
}

const COUNTS: usize = 8;

static ON: AtomicBool = AtomicBool::new(false);
static NANOS: [AtomicU64; PHASES] = [const { AtomicU64::new(0) }; PHASES];
static CALLS: [AtomicU64; PHASES] = [const { AtomicU64::new(0) }; PHASES];
static COUNTED: [AtomicU64; COUNTS] = [const { AtomicU64::new(0) }; COUNTS];
/// Files read (`Assets::read`), and which ones.
static READS: AtomicU64 = AtomicU64::new(0);
static FILES: Mutex<Option<HashSet<String>>> = Mutex::new(None);

thread_local! {
    static LOCAL_NANOS: [Cell<u64>; PHASES] = const { [const { Cell::new(0) }; PHASES] };
    static LOCAL_CALLS: [Cell<u64>; PHASES] = const { [const { Cell::new(0) }; PHASES] };
    static LOCAL_COUNTED: [Cell<u64>; COUNTS] = const { [const { Cell::new(0) }; COUNTS] };
}

/// Turns the timing on (`--profile`).
pub fn enable() {
    ON.store(true, Ordering::Relaxed);
}

/// Whether the phases are timed: `--profile`, or `BTMS_PROFILE` set to anything but 0.
pub fn on() -> bool {
    static ENV: OnceLock<bool> = OnceLock::new();
    ON.load(Ordering::Relaxed) || *ENV.get_or_init(|| std::env::var("BTMS_PROFILE").is_ok_and(|v| v != "0"))
}

/// Runs `f`, its time counted in `phase` when the timing is on.
#[inline]
pub fn time<R>(phase: Phase, f: impl FnOnce() -> R) -> R {
    if !on() {
        return f();
    }
    let t = Instant::now();
    let r = f();
    add(phase, t.elapsed());
    r
}

/// Counts `d` in `phase` (one call).
pub fn add(phase: Phase, d: Duration) {
    LOCAL_NANOS.with(|n| n[phase as usize].set(n[phase as usize].get() + d.as_nanos() as u64));
    LOCAL_CALLS.with(|c| c[phase as usize].set(c[phase as usize].get() + 1));
}

/// Adds `n` to a count of what is drawn (the caller checks [`on`] first: the counting is its own).
pub fn count(what: Count, n: u64) {
    LOCAL_COUNTED.with(|c| c[what as usize].set(c[what as usize].get() + n));
}

/// Counts a file read from the game (`path`), to tell how many are read more than once.
pub fn file_read(path: &str) {
    if on() {
        READS.fetch_add(1, Ordering::Relaxed);
        FILES.lock().unwrap().get_or_insert_with(HashSet::new).insert(path.to_string());
    }
}

/// Adds this thread's times to the totals (at the end of a block, and before [`report`]).
pub fn flush() {
    if !on() {
        return;
    }
    for i in 0..PHASES {
        NANOS[i].fetch_add(LOCAL_NANOS.with(|n| n[i].replace(0)), Ordering::Relaxed);
        CALLS[i].fetch_add(LOCAL_CALLS.with(|c| c[i].replace(0)), Ordering::Relaxed);
    }
    for i in 0..COUNTS {
        COUNTED[i].fetch_add(LOCAL_COUNTED.with(|c| c[i].replace(0)), Ordering::Relaxed);
    }
}

/// The table of the time spent per phase, per block (all threads added); None when
/// the timing is off or no block was drawn. `wall`: how long the blocks took, `drawers` and
/// `writers`: on how many threads they were drawn and their tiles written. (What the card did is
/// said after it by `tiles::run`: `gpu::summary`, `gpu::shade::summary`.)
pub fn report(wall: Duration, drawers: usize, writers: usize) -> Option<String> {
    flush();
    let ns = |p: Phase| NANOS[p as usize].load(Ordering::Relaxed) as f64;
    let calls = |p: Phase| CALLS[p as usize].load(Ordering::Relaxed);
    let blocks = calls(Phase::Block);
    if !on() || blocks == 0 {
        return None;
    }
    use Phase::*;
    // loads are timed where they happen, inside `Paint` (and inside `Scene` when the batches are built apart)
    let loads = ns(ReadFile) + ns(Inflate) + ns(Models) + ns(Dictionaries) + ns(Decode) + ns(Shared);
    let mut rows: Vec<(&str, f64, u64)> = vec![
        ("archives: opening, table, finding the file", ns(ReadFile) - ns(Extract), calls(ReadFile)),
        ("extraction (disk, copy, decryption)", ns(Extract), calls(Extract)),
        ("inflating resources (DEFLATE)", ns(Inflate), calls(Inflate)),
        ("reading models (ydr, ydd, yft)", ns(Models), calls(Models)),
        ("reading texture dictionaries (ytd)", ns(Dictionaries), calls(Dictionaries)),
        ("decoding textures (RGBA)", ns(Decode), calls(Decode)),
    ];
    if calls(Shared) > 0 {
        rows.push(("shared cache: waiting for another thread, reuse", ns(Shared), calls(Shared)));
    }
    if calls(Scene) > 0 {
        rows.push(("transform (list to draw: vertices, tints)", ns(Scene) - loads, calls(Scene)));
        rows.push(("drawing (triangles → pixels)", ns(Raster), calls(Raster)));
        // the graphics card (when it draws): its rows in place of the drawing's
        let card = ns(GpuStart) + ns(GpuPack) + ns(GpuDraw) + ns(GpuRead);
        if calls(GpuPack) > 0 {
            rows.push(("card: picture empty? (first access to its memory)", ns(GpuStart), calls(GpuStart)));
            rows.push(("card: vertices projected, triangles kept", ns(GpuPack) - ns(GpuTextures), calls(GpuPack)));
            rows.push(("card: sending the textures", ns(GpuTextures), calls(GpuTextures)));
            rows.push(("card: sending the geometry, drawing", ns(GpuDraw), calls(GpuDraw)));
            rows.push(("card: waiting for the picture, reading it back", ns(GpuRead), calls(GpuRead)));
        }
        rows.push(("walking the entities (rest of paint)", ns(Paint) - ns(Scene) - ns(Raster) - card, calls(Paint)));
    } else {
        rows.push(("transform + drawing (triangle by triangle)", ns(Paint) - loads, calls(Paint)));
    }
    rows.push(("picture: creating the buffers", ns(Canvas), calls(Canvas)));
    // the chain of the blocks on the card, when it made them
    let chain = ns(ChainFloor) + ns(ChainRecord) + ns(ChainWait) + ns(ChainCopy) + ns(ChainSet);
    if calls(ChainRecord) > 0 {
        rows.push(("chain on the card: waiting for a free set of buffers", ns(ChainSet), calls(ChainSet)));
        rows.push(("chain on the card: lowest points (reduce, wait, read back)", ns(ChainFloor), calls(ChainFloor)));
        rows.push(("chain on the card: its last steps handed to the card", ns(ChainRecord), calls(ChainRecord)));
        rows.push(("chain on the card: waiting for the block", ns(ChainWait), calls(ChainWait)));
        rows.push(("chain on the card: copying the three zooms read back", ns(ChainCopy), calls(ChainCopy)));
    }
    // the light (when it is on): its drawing is in the rows above, with the block's own
    if calls(Shade) > 0 {
        rows.push(("lighting: entities around, lowest points, shading", ns(Shade), calls(Shade)));
    }
    rows.extend([
        ("picture: conversion to RGBA", ns(Rgba), calls(Rgba)),
        ("closing the holes", ns(Holes), calls(Holes)),
    ]);
    let accounted = ns(Paint) + ns(Canvas) + ns(Shade) + ns(Rgba) + ns(Holes) + chain;
    rows.push(("rest of the block", ns(Block) - accounted, 0));
    // the writing of the tiles, on threads of their own
    let writing: Vec<(&str, f64, u64)> = vec![
        ("halving and cropping (zooms 8 and 7)", ns(Shrink), calls(Shrink)),
        ("encoding the tiles (WebP, JPEG, PNG)", ns(Encode), calls(Encode)),
        ("tile files (creating, writing, renaming)", ns(Files), calls(Files)),
        ("rest of the writing", ns(Write) - ns(Shrink) - ns(Encode) - ns(Files), 0),
    ];
    let total = ns(Block) + ns(Write);
    let b = blocks as f64;
    let mut out = format!(
        "profile: {blocks} blocks drawn on {drawers} threads and written on {writers} in {:.1} s, {:.2} blocks per second; thread times added up, per block\n",
        wall.as_secs_f64(),
        b / wall.as_secs_f64().max(1e-9)
    );
    out += &format!("  {:<54} {:>9} {:>6} {:>12}\n", "phase", "ms/block", "%", "calls/block");
    let row = |name: &str, t: f64, n: u64| {
        format!("  {:<54} {:>9.1} {:>6.1} {:>12.1}\n", name, t / b / 1e6, 100.0 * t / total.max(1.0), n as f64 / b)
    };
    for (name, t, n) in rows {
        out += &row(name, t, n);
    }
    let sum = |name: &str, t: f64| format!("  {:<54} {:>9.1} {:>6.1}\n", name, t / b / 1e6, 100.0 * t / total.max(1.0));
    out += &sum("drawing a block (per drawing thread)", ns(Block));
    for (name, t, n) in writing {
        out += &row(name, t, n);
    }
    out += &sum("writing a block (per writing thread)", ns(Write));
    out += &sum("total per block", total);
    let busy = |t: f64, threads: usize| 100.0 * t / (wall.as_nanos() as f64 * threads.max(1) as f64).max(1.0);
    out += &format!(
        "  threads busy drawing blocks: {:.0} % of the time; writing their tiles: {:.0} %; waiting for room in the writing queue: {:.1} ms per block\n",
        busy(ns(Block), drawers),
        busy(ns(Write), writers),
        ns(Queue) / b / 1e6
    );
    let counted = |c: Count| COUNTED[c as usize].load(Ordering::Relaxed) as f64 / b / 1e6;
    out += &format!(
        "  drawing a block, in millions: {:.2} triangles, {:.2} of them kept; pixels tested {:.1}, covered {:.1}, painted {:.1} (opaque {:.2}, 4-layer terrain {:.2}, cut out {:.2}, blended {:.2})
",
        counted(Count::Triangles),
        counted(Count::Kept),
        counted(Count::Tested),
        counted(Count::Covered),
        counted(Count::Opaque) + counted(Count::Layered) + counted(Count::Cutout) + counted(Count::Blended),
        counted(Count::Opaque),
        counted(Count::Layered),
        counted(Count::Cutout),
        counted(Count::Blended),
    );
    let distinct = FILES.lock().unwrap().as_ref().map(|f| f.len()).unwrap_or(0);
    out += &format!(
        "  files read (models, dictionaries): {} reads of {distinct} distinct files",
        READS.load(Ordering::Relaxed)
    );
    Some(out)
}

/// Appends `<label> <SHA-1 of the picture's exact colours and depths>` to the file named by
/// `BTMS_FINGERPRINTS` (nothing without it).
pub fn fingerprint(label: &str, raster: &crate::render::raster::Raster) {
    static FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();
    let file = FILE.get_or_init(|| {
        let path = std::env::var_os("BTMS_FINGERPRINTS")?;
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => Some(Mutex::new(f)),
            Err(e) => {
                eprintln!("BTMS_FINGERPRINTS: {}: {e}", std::path::Path::new(&path).display());
                None
            }
        }
    });
    if let Some(file) = file {
        let line = format!("{label} {}\n", raster.digest());
        let _ = file.lock().unwrap().write_all(line.as_bytes());
    }
}
