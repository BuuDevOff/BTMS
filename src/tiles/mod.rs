//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The whole map as tiles (`tiles`), seen straight from above, on the viewer's grid: zoom 0 to
//! 9, tiles of 512 × 512 px, side `32 · 2^(9 − z)` m (6.25 cm per pixel at zoom 9), tile
//! `(z, tx, ty)` covering x ∈ [tx·W, (tx+1)·W), y ∈ [ty·W, (ty+1)·W), `ty` growing northwards.
//! Written as `<folder>/<z>/<tx>/<ty>.webp` (with its transparency when partly empty: sea,
//! edges), or with `--format jpeg` the older way, `.jpg` when fully opaque and `.png` when partly
//! empty (`format.rs`); nothing when empty; `tiles.json` describes it, and the page of the web
//! map (`index.html`) is written beside it. The manifest is written
//! first (`complete: false`), so that the folder can be opened (the viewer) while it fills, then
//! again at the end (`complete: true`).
//!
//! The map is drawn by blocks of 128 m (one zoom 7 tile, 4 × 4 zoom 9 tiles) on several threads,
//! which share the models and textures they load; a block's zooms 9, 8 and 7 come from its own
//! picture, zooms 6 to 0 from the zoom 7 tiles: every 30 s for the blocks finished meanwhile (the
//! map can be looked at from afar while it is drawn), then once more for all of them at the end.
//! A tile is written whole (a temporary file, then renamed): a reader never sees half of one. What a block shows: every HD
//! entity of the placement files drawn (the game's and the server resources', `fivem`; not the maps loaded on request
//! that are left out, `groups`) whose box meets it, each model at its most detailed level, shaded if `[lighting]` is on.
//!
//! The threads that draw do not write: a block's picture goes to threads of their own, which
//! encode and write its tiles while the next block is drawn (`write.rs`); a block is done once its
//! tiles are written. How many threads of each, and the memory budgets: `options.rs`.
//!
//! When the card draws and shades, a block goes through the chain on the card (`render/gpu/chain.rs`,
//! `chained.rs`): it stays there from its drawing to its zooms 9, 8 and 7, which come back once;
//! a block the chain could not make is made again by the processor ([`block_timed`]).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::{Context, Result};
use rage_rpf::GtaKeys;

use crate::formats::resource::Resource;
use crate::formats::ymap::{Entity, parse_ymap, world_box};
use crate::render::assets::{Assets, Draw, generic_textures, paint};
use crate::render::raster::{Projection, Raster};
use crate::render::shade;
use crate::util::profile::{Phase, time};
use crate::world::World;

pub(crate) mod again;
mod chained;
pub mod convert;
pub mod format;
mod jpeg;
mod mend;
pub mod options;
pub mod webp;
mod write;

pub const ZMAX: i32 = 9;
pub const TILE: usize = 512;
/// A block: one zoom 7 tile.
const BLOCK: f32 = 128.0;
const BLOCK_PX: usize = TILE * 4;
/// JPEG quality of the opaque tiles.
const JPEG_QUALITY: u8 = 82;
/// The encoder of the opaque tiles.
static JPEG: std::sync::LazyLock<crate::tiles::jpeg::Encoder> = std::sync::LazyLock::new(|| {
    crate::tiles::jpeg::Encoder::new(TILE, TILE, JPEG_QUALITY).expect("the tiles' JPEG encoder")
});
/// Height the orthographic view looks down from.
const TOP: f32 = 3000.0;

/// Side of a tile of zoom `z`, in metres.
pub fn side(z: i32) -> f32 {
    32.0 * 2f32.powi(ZMAX - z)
}

/// The options of `tiles` ([`options::parse`]).
pub struct Options {
    pub out: PathBuf,
    /// Only the blocks meeting this rectangle (x1, y1, x2, y2).
    pub zone: Option<[f32; 4]>,
    /// Drawing threads (`--threads`).
    pub threads: options::Threads,
    /// Bytes of models and textures kept, by all threads together (they share them), before the
    /// caches are emptied (`--ram`).
    pub ram_budget: usize,
    /// Bytes of textures kept on the card (`--vram`), when given.
    pub card_budget: Option<usize>,
    /// `--dry-run`: the options checked and told, nothing read nor drawn.
    pub dry_run: bool,
    /// What the tiles are written as (`--format`, `--quality`).
    pub format: format::Format,
    /// How hard the WebP encoder works (`--webp-effort`): fast for a render.
    pub effort: webp::Effort,
}

/// `--dry-run`: what the run would use, told (the card opened if it is to draw, to know which),
/// and nothing else: the world is not read, nothing is drawn.
pub fn dry_run(opts: &Options) -> Result<()> {
    if opts.zone.is_some() {
        format::refuse_mixed_zone(&opts.out, opts.format)?;
    }
    options::resolve(opts).tell(opts);
    Ok(())
}

/// What a tile became.
#[derive(Clone, Copy, PartialEq)]
enum Written {
    Empty,
    Jpeg,
    Png,
}

fn tile_path(out: &Path, z: i32, tx: i32, ty: i32, ext: &str) -> PathBuf {
    out.join(z.to_string()).join(tx.to_string()).join(format!("{ty}.{ext}"))
}

/// Writes a 512 × 512 RGBA tile as the folder's format says (`format::of`): WebP, or JPEG when
/// opaque and PNG when partly empty; nothing when empty.
fn write_tile(out: &Path, z: i32, tx: i32, ty: i32, rgba: &[u8]) -> Result<Written> {
    let alpha = rgba.as_chunks::<4>().0.iter().map(|p| p[3]);
    let (mut any, mut all) = (false, true);
    for a in alpha {
        any |= a > 0;
        all &= a == 255;
    }
    if !any {
        return Ok(Written::Empty);
    }
    let format = format::of(out);
    let ext = format.file_extension(all);
    // the whole file in memory first (the bytes are those an encoder writing straight to disk gives)
    let bytes = time(Phase::Encode, || -> Result<Vec<u8>> {
        if let Some(webp) = format.encode(rgba, TILE, all, format::effort(out)) {
            webp
        } else if all {
            // the file the `image` crate's JPEG encoder writes, byte for byte, made faster
            Ok(crate::tiles::jpeg::encode(&JPEG, rgba, JPEG_QUALITY))
        } else {
            let mut bytes = Vec::new();
            image::codecs::png::PngEncoder::new(&mut bytes).write_image(
                rgba,
                TILE as u32,
                TILE as u32,
                image::ExtendedColorType::Rgba8,
            )?;
            Ok(bytes)
        }
    })?;
    time(Phase::Files, || -> Result<()> {
        let path = tile_path(out, z, tx, ty, ext);
        // written beside, then renamed over: a reader (the viewer, the lower zooms) never gets half a tile
        let temp = path.with_extension(format!("{ext}.tmp"));
        // the folder is only made when the file cannot be written: the first tile of a folder
        if std::fs::write(&temp, &bytes).is_err() {
            std::fs::create_dir_all(path.parent().unwrap()).with_context(|| format!("creating {}", path.display()))?;
            std::fs::write(&temp, &bytes).with_context(|| format!("writing {}", temp.display()))?;
        }
        std::fs::rename(&temp, &path).with_context(|| format!("renaming {}", temp.display()))?;
        // a lower zoom tile drawn again may change format as its children fill (or the folder
        // held tiles of another format): the old one goes
        for other in format::EXTENSIONS.into_iter().filter(|e| *e != ext) {
            let _ = std::fs::remove_file(tile_path(out, z, tx, ty, other));
        }
        Ok(())
    })?;
    Ok(if all { Written::Jpeg } else { Written::Png })
}

use image::ImageEncoder;

/// Passes of [`fill_holes`]: holes up to about twice as many pixels wide are closed.
pub(crate) const HOLE_PASSES: usize = 4;

/// Closes the small holes between pieces of geometry (pixels nothing was drawn on, next to drawn
/// ones): each pass gives every empty pixel touching a drawn one the mean of its drawn neighbours.
/// A wide empty area (the sea) only loses a rim of `passes` pixels.
pub(crate) fn fill_holes(rgba: &mut [u8], w: usize, h: usize, passes: usize) {
    // no hole at all (most blocks): nothing to copy nor to look at
    if rgba.as_chunks::<4>().0.iter().all(|p| p[3] != 0) {
        return;
    }
    for _ in 0..passes {
        let src = rgba.to_vec();
        let mut changed = false;
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                if src[i + 3] != 0 {
                    continue;
                }
                let (mut sum, mut n) = ([0u32; 3], 0u32);
                for (dx, dy) in [(-1i32, -1i32), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)] {
                    let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    let j = (ny as usize * w + nx as usize) * 4;
                    if src[j + 3] != 0 {
                        for k in 0..3 {
                            sum[k] += src[j + k] as u32;
                        }
                        n += 1;
                    }
                }
                if n >= 3 {
                    for k in 0..3 {
                        rgba[i + k] = (sum[k] / n) as u8;
                    }
                    rgba[i + 3] = 255;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// Halves an RGBA picture (`w` × `h`, both even): each pixel the mean of 2 × 2, weighted by alpha.
pub(crate) fn halve(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    let (ow, oh) = (w / 2, h / 2);
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..oh {
        for x in 0..ow {
            let o = (y * ow + x) * 4;
            // four opaque pixels (the usual case): the weighted mean below is their plain mean,
            // 255 (a + b + c + d) / (4 × 255)
            let (top, bottom) = (&rgba[(y * 2 * w + x * 2) * 4..][..8], &rgba[((y * 2 + 1) * w + x * 2) * 4..][..8]);
            if top[3] & top[7] & bottom[3] & bottom[7] == 255 {
                for k in 0..3 {
                    out[o + k] =
                        ((top[k] as u32 + top[4 + k] as u32 + bottom[k] as u32 + bottom[4 + k] as u32) / 4) as u8;
                }
                out[o + 3] = 255;
                continue;
            }
            let mut sum = [0u32; 4];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let i = ((y * 2 + dy) * w + x * 2 + dx) * 4;
                let a = rgba[i + 3] as u32;
                for k in 0..3 {
                    sum[k] += rgba[i + k] as u32 * a;
                }
                sum[3] += a;
            }
            if let Some(alpha) = std::num::NonZeroU32::new(sum[3]) {
                for k in 0..3 {
                    out[o + k] = (sum[k] / alpha) as u8;
                }
                out[o + 3] = (alpha.get() / 4) as u8;
            }
        }
    }
    out
}

/// The square `[x0, x0 + n) × [y0, y0 + n)` of a `w`-wide RGBA picture.
fn crop(rgba: &[u8], w: usize, x0: usize, y0: usize, n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * n * 4);
    for y in y0..y0 + n {
        out.extend_from_slice(&rgba[(y * w + x0) * 4..(y * w + x0 + n) * 4]);
    }
    out
}

/// Every HD entity of the placement files the game uses, and for each block of 128 m the entities
/// whose box meets it.
pub(crate) struct Placements {
    pub(crate) entities: Vec<Entity>,
    pub(crate) blocks: HashMap<(i32, i32), Vec<u32>>,
}

pub(crate) fn placements(game: &Path, keys: &GtaKeys, world: &World, zone: Option<[f32; 4]>) -> Result<Placements> {
    let t = Instant::now();
    // the shading of a block reads the heights around it: the entities around the zone count
    // too, in blocks that are not drawn themselves (`drawn` is the zone asked for)
    let (drawn, margin) = (zone, shade::margin_metres(BLOCK / BLOCK_PX as f32));
    let zone = zone.map(|z| [z[0] - margin, z[1] - margin, z[2] + margin, z[3] + margin]);
    let meets = |lo: [f32; 3], hi: [f32; 3]| {
        zone.is_none_or(|z| lo[0] <= z[2] && hi[0] >= z[0] && lo[1] <= z[3] && hi[1] >= z[1])
    };
    let mut entities = Vec::new();
    let mut blocks: HashMap<(i32, i32), Vec<u32>> = HashMap::new();
    let mut files = 0;
    // the files the game uses: not those a pack replaces, nor the maps loaded on request that are left out
    let used =
        |m: &crate::world::YmapInfo| !world.replaced.contains(&m.hash) && !world.groups.left_out.contains(&m.hash);
    let wanted: Vec<&crate::world::YmapInfo> =
        world.ymaps.iter().filter(|m| used(m) && meets(m.entities_min, m.entities_max)).collect();
    // each file read, parsed and sorted on any thread: its HD entities that meet the zone, with
    // the blocks of their boxes (None: the file cannot be read)
    let read = crate::util::parallel::map(&wanted, |m| -> Option<Vec<(Entity, [i32; 4])>> {
        let map = crate::cli::archive::read_file(game, &m.path, keys)
            .and_then(|b| Resource::parse(&b))
            .and_then(|r| parse_ymap(&r))
            .ok()?;
        let map = crate::world::fivem::shown(&world.server, &m.path, map);
        let mut kept = Vec::new();
        for e in map.entities.into_iter().filter(|e| !e.interior && matches!(e.lod_level, 0 | 5)) {
            let Some((a, _)) = world.archetypes.get(&e.archetype) else { continue };
            let (lo, hi) = world_box(&e, a, true);
            if !meets(lo, hi) || !(lo[0].is_finite() && hi[0].is_finite()) {
                continue;
            }
            let (bx0, bx1) = ((lo[0] / BLOCK).floor() as i32, (hi[0] / BLOCK).floor() as i32);
            let (by0, by1) = ((lo[1] / BLOCK).floor() as i32, (hi[1] / BLOCK).floor() as i32);
            // a huge box (a sky dome, a broken placement) would fill thousands of blocks
            if (bx1 - bx0) > 64 || (by1 - by0) > 64 {
                continue;
            }
            kept.push((e, [bx0, bx1, by0, by1]));
        }
        Some(kept)
    });
    // then numbered in the order of the files, as when they were read one after the other
    for kept in read.into_iter().flatten() {
        files += 1;
        for (e, [bx0, bx1, by0, by1]) in kept {
            let id = entities.len() as u32;
            for by in by0..=by1 {
                for bx in bx0..=bx1 {
                    let inside = zone.is_none_or(|z| {
                        (bx + 1) as f32 * BLOCK > z[0]
                            && (bx as f32) * BLOCK < z[2]
                            && (by + 1) as f32 * BLOCK > z[1]
                            && (by as f32) * BLOCK < z[3]
                    });
                    if inside {
                        blocks.entry((bx, by)).or_default().push(id);
                    }
                }
            }
            entities.push(e);
        }
    }
    eprintln!(
        "placements: {files} files, {} HD entities, {} blocks of 128 m to draw ({:.1} s)",
        entities.len(),
        blocks.keys().filter(|b| in_zone(drawn, **b)).count(),
        t.elapsed().as_secs_f64()
    );
    Ok(Placements { entities, blocks })
}

/// Whether a block meets the zone asked for (every block without a zone).
fn in_zone(zone: Option<[f32; 4]>, (bx, by): (i32, i32)) -> bool {
    zone.is_none_or(|z| {
        (bx + 1) as f32 * BLOCK > z[0]
            && (bx as f32) * BLOCK < z[2]
            && (by + 1) as f32 * BLOCK > z[1]
            && (by as f32) * BLOCK < z[3]
    })
}

/// The blocks a render draws (in no order): those of the zone (every one without a zone) that
/// entities meet and, with the water (`wet`), every block of the rectangle around them and the water
/// pieces (the zone's, when there is one): beyond the land it is the sea. `again.rs` draws again
/// some of the same.
fn blocks_to_draw(
    places: &Placements,
    water: &crate::world::water::Water,
    zone: Option<[f32; 4]>,
    wet: bool,
) -> Vec<(i32, i32)> {
    let mut order: Vec<(i32, i32)> = places.blocks.keys().copied().filter(|b| in_zone(zone, *b)).collect();
    if wet {
        let mut seen: std::collections::HashSet<(i32, i32)> = order.iter().copied().collect();
        let known: Vec<(i32, i32)> = seen.iter().copied().chain(water.blocks(BLOCK)).collect();
        let span = |k: usize| (known.iter().map(|b| [b.0, b.1][k]).min(), known.iter().map(|b| [b.0, b.1][k]).max());
        let (x, y) = match zone {
            Some(z) => (
                (Some((z[0] / BLOCK).floor() as i32), Some(((z[2] - 1e-3) / BLOCK).floor() as i32)),
                (Some((z[1] / BLOCK).floor() as i32), Some(((z[3] - 1e-3) / BLOCK).floor() as i32)),
            ),
            None => {
                let (x, y) = (span(0), span(1));
                ((x.0.map(|v| v - 1), x.1.map(|v| v + 1)), (y.0.map(|v| v - 1), y.1.map(|v| v + 1)))
            }
        };
        if let ((Some(x0), Some(x1)), (Some(y0), Some(y1))) = (x, y) {
            for by in y0..=y1 {
                for bx in x0..=x1 {
                    if in_zone(zone, (bx, by)) && seen.insert((bx, by)) {
                        order.push((bx, by));
                    }
                }
            }
        }
    }
    order
}

/// The order blocks are drawn in: along a Z curve, so that a thread's next block is near its last
/// (its caches still hold most of the models).
fn z_order((bx, by): (i32, i32)) -> u64 {
    let (x, y) = ((bx + 32768) as u64, (by + 32768) as u64);
    let mut z = 0u64;
    for i in 0..16 {
        z |= ((x >> i) & 1) << (2 * i) | ((y >> i) & 1) << (2 * i + 1);
    }
    z
}

pub fn run(game: &Path, keys: &GtaKeys, opts: &Options) -> Result<()> {
    let t0 = Instant::now();
    // a zone in another format than the tiles the folder holds: refused before anything is read
    if opts.zone.is_some() {
        format::refuse_mixed_zone(&opts.out, opts.format)?;
    }
    // (the graphics card, when it draws, is opened while the map loads)
    crate::render::gpu::warm();
    // what the run uses (threads, who draws and shades, budgets): for the interface, before the
    // world is loaded (the card is waited for, to know whether it opened); else once it is
    // loaded, the card opened meanwhile
    let told = crate::progress::active().then(|| options::resolve(opts).tell(opts));
    crate::progress::phase("world");
    let mut world = World::load(game, keys, false)?;
    // the whole map records the resources it is drawn with (`resources.json`, for `update`):
    // as they are now, at the start of the render
    let prints = opts.zone.is_none().then(|| crate::update::record::prints(crate::world::fivem::server()));
    let settings = told.unwrap_or_else(|| options::resolve(opts).tell(opts));
    let threads = settings.drawers;
    // the chain of a block on the card, when the run uses it (decided by `options::resolve`), with
    // a set of buffers per drawing thread
    let chain = if settings.chain { crate::render::gpu::chain::active() } else { None };
    if let Some(chain) = chain {
        chain.reserve(threads);
    }
    // the maps loaded on request that close a hole of the map are kept after all
    // (the search draws blocks before any block is reported: a job the interface follows says so)
    if crate::config::get().maps.fill_holes && !world.groups.left_out.is_empty() {
        crate::progress::log("holes: looking for holes in the ground that an on-demand map closes…");
    }
    crate::world::holes::fill(game, keys, &mut world, opts.zone, threads, settings.ram_budget)?;
    let world = world;
    let generic = generic_textures(game, keys, &world);
    crate::progress::phase("placements");
    let places = placements(game, keys, &world, opts.zone)?;
    // the water (`[water]`): the blocks of the sea and the lakes are drawn too, with nothing else in them
    let wet = crate::config::get().water.enabled;
    let water = if wet { crate::world::water::Water::load(game, keys) } else { crate::world::water::Water::default() };
    crate::world::water::draw_meshes(wet);
    let mut order = blocks_to_draw(&places, &water, opts.zone, wet);
    order.sort_by_key(|b| z_order(*b));
    std::fs::create_dir_all(&opts.out).with_context(|| format!("creating {}", opts.out.display()))?;
    // one writer at a time in a folder (`update` too): let go at the end of the render
    let _lock = crate::update::lock::Lock::take(&opts.out, "tiles")?;
    format::tell_if_mixed(&opts.out, opts.format);
    format::set(&opts.out, opts.format, opts.effort);
    write_manifest(opts, order.len(), false)?;
    crate::progress::plan(&order, threads, &opts.out, opts.zone);
    crate::progress::phase("blocks");

    // zooms 9, 8, 7 block by block: drawn on several threads, written on others
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let counts = Mutex::new([0usize; 3]);
    // blocks written since the lower zooms were last made
    let finished = Mutex::new(Vec::<(i32, i32)>::new());
    let drawing = AtomicBool::new(true);
    let errors = Mutex::new(Vec::<String>::new());
    let clock = Clock::default();
    let t = Instant::now();
    let total = order.len();
    let queued = std::thread::scope(|s| {
        // the lower zooms of what is written, every 30 s, so that the map can be seen from afar
        s.spawn(|| {
            let mut last = Instant::now();
            while drawing.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if last.elapsed().as_secs_f64() < LOWER_ZOOMS_EVERY_S {
                    continue;
                }
                last = Instant::now();
                let blocks = std::mem::take(&mut *finished.lock().unwrap());
                if !blocks.is_empty() {
                    let passed = blocks.len();
                    lower_zooms(&opts.out, blocks, 2);
                    crate::progress::lower_zooms(passed);
                }
            }
        });
        // (the thread above ends with the blocks, even when a thread of theirs panics)
        let _over = write::OnDrop(|| drawing.store(false, Ordering::Relaxed));
        write::pipeline(
            &opts.out,
            threads,
            settings.writers,
            |hand| {
                let mut assets = Assets::new(game, keys, &world, &generic);
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(bx, by)) = order.get(i) else { break };
                    crate::progress::block_started(bx, by);
                    let started = Instant::now();
                    let pixels = block(chain, &places, &world, &water, &mut assets, bx, by, &clock);
                    hand(write::Picture { bx, by, pixels, started });
                    crate::util::profile::flush();
                    assets.trim(settings.ram_budget);
                }
            },
            // a block is done once its tiles are written
            |report, result| {
                let (bx, by) = (report.bx, report.by);
                clock.rest.fetch_add(report.spent.as_nanos() as u64, Ordering::Relaxed);
                match result {
                    Ok(w) => {
                        if crate::progress::active() {
                            let count = |kind: Written| w.iter().filter(|x| **x == kind).count();
                            crate::progress::block_done(
                                bx,
                                by,
                                report.started.elapsed().as_millis(),
                                count(Written::Jpeg),
                                count(Written::Png),
                                count(Written::Empty),
                            );
                        }
                        let mut c = counts.lock().unwrap();
                        for x in w {
                            match x {
                                Written::Jpeg => c[0] += 1,
                                Written::Png => c[1] += 1,
                                Written::Empty => c[2] += 1,
                            }
                        }
                    }
                    Err(e) => {
                        crate::progress::block_failed(bx, by, &format!("{e:#}"));
                        errors.lock().unwrap().push(format!("block {bx},{by}: {e:#}"))
                    }
                }
                crate::util::profile::flush();
                finished.lock().unwrap().push((bx, by));
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_multiple_of(50) || n == total {
                    let rate = n as f64 / t.elapsed().as_secs_f64();
                    eprintln!(
                        "  {n}/{total} blocks, {:.1} per second, {:.0} min to go",
                        rate,
                        (total - n) as f64 / rate.max(1e-9) / 60.0
                    );
                }
            },
        )
    });
    let wall = t.elapsed();
    let [jpeg, png, empty] = *counts.lock().unwrap();
    println!(
        "zooms 9 to 7: {total} blocks in {:.0} s ({:.2} s per block and per thread); {}",
        t.elapsed().as_secs_f64(),
        t.elapsed().as_secs_f64() * threads as f64 / total.max(1) as f64,
        opts.format.counts(jpeg, png, empty)
    );
    // what the light costs, per block: the heights of the margin and the shading
    if shade::lit() || chain.is_some() || std::env::var_os("BTMS_BLOCK_TIMES").is_some() {
        let per_block = |ns: &AtomicU64| ns.load(Ordering::Relaxed) as f64 / 1e6 / total.max(1) as f64;
        if chain.is_some() {
            println!(
                "per block: drawing {:.0} ms, heights around the block {:.0} ms, end of the chain on the card (shading, 8 bits, holes, water, downscaling, read-back) {:.0} ms, writing the tiles {:.0} ms",
                per_block(&clock.paint),
                per_block(&clock.margin),
                per_block(&clock.shade),
                per_block(&clock.rest)
            );
        } else {
            println!(
                "per block: drawing {:.0} ms, heights around the block {:.0} ms, shading {:.0} ms, the rest (holes, tiles) {:.0} ms",
                per_block(&clock.paint),
                per_block(&clock.margin),
                per_block(&clock.shade),
                per_block(&clock.rest)
            );
        }
        if let Some(frame) = shade::Frame::new([BLOCK / 2.0, BLOCK / 2.0], BLOCK / BLOCK_PX as f32, BLOCK_PX, BLOCK_PX)
        {
            let ([width, height], [left, top, right, bottom]) = (frame.size(), frame.margin());
            println!(
                "lighting: heights read over {width}×{height} px around each block (margins west {left}, north {top}, east {right}, south {bottom}); of the {} entities of these margins, {} are tall enough to cast a shadow on their block",
                clock.around.load(Ordering::Relaxed),
                clock.tall.load(Ordering::Relaxed)
            );
        }
    }
    for e in errors.lock().unwrap().iter().take(10) {
        println!("  {e}");
    }
    if let Some(table) = crate::util::profile::report(wall, threads, settings.writers) {
        println!("{table}");
        // (at the bound, often: the writing is what holds the drawing back)
        println!(
            "  write queue: up to {queued} block pictures waiting at once, out of {} slots",
            write::bound(settings.writers)
        );
    }
    // what the card did: drawn, shaded, the chain; and what the processor did in its place
    for line in
        [crate::render::gpu::summary(), crate::render::gpu::shade::summary(), crate::render::gpu::chain::summary()]
            .into_iter()
            .flatten()
    {
        println!("{line}");
    }
    if let Some(line) = crate::render::gpu::chain::end(settings.card.is_some() || settings.card_shades) {
        println!("{line}");
    }

    // the last holes, which no placement file fills, get the colours around them
    if crate::config::get().maps.fill_holes {
        mend::report(&mend::mend(&opts.out, &order)?);
    }

    // zooms 6 to 0 of every block, once more now that all are drawn (the processor's work only:
    // as many threads as it has, the drawing threads at most)
    crate::progress::phase("zooms");
    let t = Instant::now();
    let lower_counts = lower_zooms(&opts.out, order.clone(), threads.min(options::logical()));
    println!("zooms 6 to 0: {} in {:.0} s", lower_counts.join(", "), t.elapsed().as_secs_f64());
    crate::progress::lower_zooms(total);

    write_manifest(opts, total, true)?;
    if wet {
        let shaders: Vec<String> = crate::world::water::shaders()
            .iter()
            .map(|(hash, n)| {
                format!(
                    "{} × {n}",
                    crate::world::water::shader_name(*hash)
                        .map(str::to_string)
                        .unwrap_or(format!("0x{hash:08X} (unknown, drawn as a river)"))
                )
            })
            .collect();
        println!(
            "water: {} pieces of sea and lakes; water pieces of the models drawn: {}",
            water.quads.len(),
            if shaders.is_empty() { "none".to_string() } else { shaders.join(", ") }
        );
    }
    crate::world::fivem::say_scrambled(&world.index);
    // what `update --changes` compares the resources with (the whole map only)
    if let Some(prints) = prints {
        crate::update::after_render(game, keys, &opts.out, &world, &places, prints, errors.lock().unwrap().len());
    }
    println!("tiles: {} in {:.0} min", opts.out.display(), t0.elapsed().as_secs_f64() / 60.0);
    crate::progress::end(total, errors.lock().unwrap().len(), t0.elapsed().as_secs_f64());
    Ok(())
}

/// Writes `tiles.json`: what the folder holds, and whether the render is finished.
fn write_manifest(opts: &Options, blocks: usize, complete: bool) -> Result<()> {
    let mut manifest = serde_json::json!({
        "format": format::MANIFEST_FORMAT,
        "source": "GTA V game files (placements, models, textures), seen from straight above, unlit",
        "zmax": ZMAX,
        "tile": TILE,
        "side_m": "32 · 2^(9 − z)",
        "metres_per_pixel_zmax": side(ZMAX) / TILE as f32,
        "y": "ty grows northwards; tile (z, tx, ty) covers x ∈ [tx·W, (tx+1)·W), y ∈ [ty·W, (ty+1)·W)",
        "zone": opts.zone,
        "blocks": blocks,
        "complete": complete,
    });
    if let Some(light) = shade::manifest() {
        manifest["source"] = "GTA V game files (placements, models, textures), seen from straight above, shaded (slope, cast shadows, ambient occlusion)".into();
        manifest["lighting"] = light;
    }
    // `format`, `extension`, `quality`, and the page of the web map beside the manifest; once
    // every block is written, the ground they cover (`bounds`, what the page frames on)
    format::describe(&mut manifest, opts.format, opts.effort);
    if let Some(bounds) = format::bounds(&opts.out).filter(|_| complete) {
        manifest["bounds"] = serde_json::json!(bounds);
    }
    // the server's resources drawn over the game's files (`[resources]`), when there are any
    let server = crate::world::fivem::server();
    if !server.resources.is_empty() {
        manifest["resources"] = server.resources.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().into();
    }
    let path = opts.out.join("tiles.json");
    std::fs::write(&path, serde_json::to_string_pretty(&manifest)?)
        .with_context(|| format!("writing {}", path.display()))?;
    format::write_page(&opts.out)
}

/// Time spent on the blocks, in nanoseconds, all threads together, and what the margins held.
#[derive(Default)]
struct Clock {
    paint: AtomicU64,
    /// The heights around the block (the shading's margins).
    margin: AtomicU64,
    shade: AtomicU64,
    rest: AtomicU64,
    /// Entities of the blocks and their margins, and those of them tall enough to cast a
    /// shadow on their block.
    around: AtomicU64,
    tall: AtomicU64,
}

/// An entity and its box in the world.
type Boxed = (u32, [f32; 3], [f32; 3]);

/// The entities whose box meets the rectangle a block's shading reads (the block and its
/// margins), from the blocks it covers, in the order of the placement files.
fn entities_around(places: &Placements, world: &World, frame: &shade::Frame) -> Vec<Boxed> {
    let (lo, hi) = frame.bounds();
    let mut ids: Vec<u32> = Vec::new();
    for ny in (lo[1] / BLOCK).floor() as i32..=(hi[1] / BLOCK).floor() as i32 {
        for nx in (lo[0] / BLOCK).floor() as i32..=(hi[0] / BLOCK).floor() as i32 {
            if let Some(list) = places.blocks.get(&(nx, ny)) {
                ids.extend_from_slice(list);
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .filter_map(|i| {
            let e = &places.entities[i as usize];
            let (a, _) = world.archetypes.get(&e.archetype)?;
            let (a, b) = world_box(e, a, true);
            (a[0] <= hi[0] && b[0] >= lo[0] && a[1] <= hi[1] && b[1] >= lo[1]).then_some((i, a, b))
        })
        .collect()
}

/// Makes one block, through the chain on the card when the run uses it (`chain`), else as before
/// ([`block_timed`]); a block the chain could not make is made again: whole on the processor when
/// the chain only left this block, the way before the chain when it was given up. What fell back
/// to the processor is counted (`chain::block_end`).
#[allow(clippy::too_many_arguments)]
fn block(
    chain: Option<&crate::render::gpu::chain::Chain>,
    places: &Placements,
    world: &World,
    water: &crate::world::water::Water,
    assets: &mut Assets,
    bx: i32,
    by: i32,
    clock: &Clock,
) -> write::Pixels {
    let pixels = time(Phase::Block, || match chain {
        Some(chain) if chain.usable() => match chained::block(chain, places, world, water, assets, bx, by, clock, None)
        {
            Ok(zooms) => write::Pixels::Zooms(zooms),
            Err(failure) => {
                chain.failed(&failure);
                match failure {
                    crate::render::gpu::chain::Failure::Block(_) => {
                        crate::render::gpu::chain::fell_back(crate::render::gpu::chain::Kind::Chain, failure.reason())
                    }
                    crate::render::gpu::chain::Failure::Card(_) => {
                        crate::render::gpu::chain::went_before(failure.reason())
                    }
                }
                write::Pixels::Whole(match failure {
                    // this block only: made again whole on the processor
                    crate::render::gpu::chain::Failure::Block(_) => {
                        crate::render::gpu::on_processor(|| block_timed(places, world, water, assets, bx, by, clock))
                    }
                    // the chain is given up: the way before it (the card if it still draws)
                    crate::render::gpu::chain::Failure::Card(_) => {
                        block_timed(places, world, water, assets, bx, by, clock)
                    }
                })
            }
        },
        Some(chain) => {
            crate::render::gpu::chain::went_before(&chain.given_up());
            write::Pixels::Whole(block_timed(places, world, water, assets, bx, by, clock))
        }
        None => write::Pixels::Whole(block_timed(places, world, water, assets, bx, by, clock)),
    });
    // (what fell back to the processor during the block)
    crate::render::gpu::chain::block_end();
    pixels
}

/// Draws one block as before the chain (its time counted by the caller, `Phase::Block`): its
/// picture, from which its zoom 9, 8 and 7 tiles are written ([`write::tiles`]).
fn block_timed(
    places: &Placements,
    world: &World,
    water: &crate::world::water::Water,
    assets: &mut Assets,
    bx: i32,
    by: i32,
    clock: &Clock,
) -> Vec<u8> {
    // (a block of the open sea has no entity at all)
    let draws: Vec<Draw> = places
        .blocks
        .get(&(bx, by))
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|&i| {
            let e = &places.entities[i as usize];
            world.archetypes.get(&e.archetype).map(|(a, _)| Draw {
                entity: e.clone(),
                archetype: a.clone(),
                distance: 0.0,
            })
        })
        .collect();
    let centre = [(bx as f32 + 0.5) * BLOCK, (by as f32 + 0.5) * BLOCK];
    let t = Instant::now();
    let mut raster = time(Phase::Canvas, || {
        Raster::new(
            BLOCK_PX,
            BLOCK_PX,
            Projection::Orthographic { centre, metres: BLOCK / BLOCK_PX as f32, top: TOP },
            [0.0; 3],
        )
    });
    paint(&mut raster, &draws, assets, true, 3.0);
    clock.paint.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    crate::util::profile::fingerprint(&format!("block {bx},{by}"), &raster);
    // the light (`[lighting]`): the block above is drawn as without it; the heights around it go
    // to a second raster, and the block is shaded from both. Shading comes before the holes are
    // filled: a hole has no height to shade, and filling it copies the shaded colours next to it.
    if let Some(frame) = shade::Frame::new(centre, BLOCK / BLOCK_PX as f32, BLOCK_PX, BLOCK_PX)
        .filter(|_| places.blocks.contains_key(&(bx, by)))
    {
        let t = Instant::now();
        let around = time(Phase::Shade, || entities_around(places, world, &frame));
        let draw = |i: u32| {
            let e = &places.entities[i as usize];
            world.archetypes.get(&e.archetype).map(|(a, _)| Draw {
                entity: e.clone(),
                archetype: a.clone(),
                distance: 0.0,
            })
        };
        // first what is within a few metres of the block: the wall feet need it all
        let mut heights = time(Phase::Canvas, || frame.heights(TOP));
        let near: Vec<Draw> =
            around.iter().filter(|(_, lo, hi)| frame.near(*lo, *hi)).filter_map(|(i, ..)| draw(*i)).collect();
        paint(&mut heights, &near, assets, true, 3.0);
        // then, farther, only what is high enough for its shadow to reach the block, now that
        // the block's lowest points are known: the ground around costs nothing
        let floor = time(Phase::Shade, || frame.far(&raster, &mut heights));
        let far: Vec<Draw> = around
            .iter()
            .filter(|(_, lo, hi)| frame.reaches(*lo, *hi, &floor))
            .filter_map(|(i, ..)| draw(*i))
            .collect();
        paint(&mut heights, &far, assets, true, 3.0);
        clock.around.fetch_add(around.len() as u64, Ordering::Relaxed);
        clock.tall.fetch_add(far.len() as u64, Ordering::Relaxed);
        clock.margin.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        let t = Instant::now();
        time(Phase::Shade, || frame.shade(&mut raster, &heights));
        clock.shade.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        // (beside the fingerprint of the block as drawn: those of the heights read and of the block shaded)
        crate::util::profile::fingerprint(&format!("block {bx},{by} heights"), &heights);
        crate::util::profile::fingerprint(&format!("block {bx},{by} shaded"), &raster);
    }
    let t = Instant::now();
    let mut rgba = time(Phase::Rgba, || raster.rgba());
    time(Phase::Holes, || fill_holes(&mut rgba, BLOCK_PX, BLOCK_PX, HOLE_PASSES));
    // the sea and the lakes, over the ground that lies under them
    if !water.quads.is_empty() {
        time(Phase::Holes, || {
            water.flood(
                &mut rgba,
                raster.depths(),
                TOP,
                [bx as f32 * BLOCK, (by + 1) as f32 * BLOCK],
                BLOCK / BLOCK_PX as f32,
                BLOCK_PX,
            )
        });
    }
    // (the writing of its tiles is counted by the thread that writes them)
    clock.rest.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    rgba
}

/// Colours in the last holes of a folder of tiles already drawn (what [`run`] does once its
/// blocks are written), then makes the lower zooms above the blocks touched again.
pub fn mend_folder(out: &Path, threads: usize) -> Result<()> {
    let mut blocks = Vec::new();
    let level = out.join("7");
    for column in std::fs::read_dir(&level).with_context(|| format!("reading {}", level.display()))?.flatten() {
        let Some(tx) = column.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else { continue };
        for tile in std::fs::read_dir(column.path())?.flatten() {
            let name = tile.file_name();
            let Some((ty, ext)) = name.to_str().and_then(|s| s.rsplit_once('.')) else { continue };
            if let (Ok(ty), "jpg" | "png" | "webp") = (ty.parse::<i32>(), ext) {
                blocks.push((tx, ty));
            }
        }
    }
    blocks.sort_unstable();
    blocks.dedup();
    let mended = mend::mend(out, &blocks)?;
    mend::report(&mended);
    if mended.filled == 0 && mended.left.is_empty() {
        println!("last holes: none in the {} blocks of {}", blocks.len(), out.display());
    }
    lower_zooms(out, mended.blocks.clone(), threads);
    Ok(())
}

/// Seconds between two passes over the lower zooms while the blocks are drawn.
const LOWER_ZOOMS_EVERY_S: f64 = 30.0;

/// Makes the zoom 6 to 0 tiles above these blocks (zoom 7 tiles), each from the four below it, on
/// `threads` threads. Returns how many were written at each zoom.
fn lower_zooms(out: &Path, blocks: Vec<(i32, i32)>, threads: usize) -> Vec<String> {
    let mut level = blocks;
    let mut counts = Vec::new();
    for z in (0..7).rev() {
        let mut parents: Vec<(i32, i32)> = level.iter().map(|&(x, y)| (x.div_euclid(2), y.div_euclid(2))).collect();
        parents.sort_unstable();
        parents.dedup();
        let next = AtomicUsize::new(0);
        let written = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..threads.max(1) {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(tx, ty)) = parents.get(i) else { break };
                        if let Ok(w) = parent_tile(out, z, tx, ty)
                            && w != Written::Empty
                        {
                            written.lock().unwrap().push((tx, ty));
                        }
                    }
                });
            }
        });
        level = written.into_inner().unwrap();
        counts.push(format!("z{z} {}", level.len()));
    }
    counts
}

/// Reads a tile back (RGBA), or None when there is none: only the extensions of the folder's
/// format (`format::of`).
fn read_tile(out: &Path, z: i32, tx: i32, ty: i32) -> Option<Vec<u8>> {
    for ext in format::of(out).extensions() {
        let p = tile_path(out, z, tx, ty, ext);
        if p.is_file() {
            return format::decode_file(&p);
        }
    }
    None
}

/// The tile (z, tx, ty) from its four children at z + 1.
fn parent_tile(out: &Path, z: i32, tx: i32, ty: i32) -> Result<Written> {
    let mut big = vec![0u8; TILE * 2 * TILE * 2 * 4];
    let mut any = false;
    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        // child (2tx + dx, 2ty + dy); ty grows north, rows go south: dy = 1 is the top half
        if let Some(c) = read_tile(out, z + 1, tx * 2 + dx, ty * 2 + dy) {
            any = true;
            let (x0, y0) = (dx as usize * TILE, (1 - dy) as usize * TILE);
            for y in 0..TILE {
                let dst = ((y0 + y) * TILE * 2 + x0) * 4;
                big[dst..dst + TILE * 4].copy_from_slice(&c[y * TILE * 4..(y + 1) * TILE * 4]);
            }
        }
    }
    if !any {
        return Ok(Written::Empty);
    }
    write_tile(out, z, tx, ty, &halve(&big, TILE * 2, TILE * 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random(seed: &mut u64) -> u64 {
        *seed ^= *seed >> 12;
        *seed ^= *seed << 25;
        *seed ^= *seed >> 27;
        seed.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A picture whose pixels are opaque, empty or in between, in patches.
    fn picture(w: usize, h: usize, seed: &mut u64) -> Vec<u8> {
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let r = random(seed);
                let alpha = match (x / 5 + y / 3 + r.is_multiple_of(7) as usize) % 4 {
                    0 | 1 => 255,
                    2 => 0,
                    _ => (r >> 40) as u8,
                };
                rgba[(y * w + x) * 4..][..4].copy_from_slice(&[r as u8, (r >> 8) as u8, (r >> 16) as u8, alpha]);
            }
        }
        rgba
    }

    /// `halve` as it was written first: the mean of 2 × 2 weighted by alpha, for every pixel.
    fn halve_plain(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
        let (ow, oh) = (w / 2, h / 2);
        let mut out = vec![0u8; ow * oh * 4];
        for y in 0..oh {
            for x in 0..ow {
                let mut sum = [0u32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let i = ((y * 2 + dy) * w + x * 2 + dx) * 4;
                    let a = rgba[i + 3] as u32;
                    for k in 0..3 {
                        sum[k] += rgba[i + k] as u32 * a;
                    }
                    sum[3] += a;
                }
                let o = (y * ow + x) * 4;
                if let Some(alpha) = std::num::NonZeroU32::new(sum[3]) {
                    for k in 0..3 {
                        out[o + k] = (sum[k] / alpha) as u8;
                    }
                    out[o + 3] = (alpha.get() / 4) as u8;
                }
            }
        }
        out
    }

    #[test]
    fn halving_opaque_pixels_is_the_weighted_mean() {
        let mut seed = 0xABCD_EF01_2345_6789u64;
        for (w, h) in [(2, 2), (64, 48), (130, 66)] {
            for _ in 0..20 {
                let rgba = picture(w, h, &mut seed);
                assert!(halve(&rgba, w, h) == halve_plain(&rgba, w, h), "{w}×{h}");
            }
        }
        // every sum of four opaque values
        for s in 0..=1020u32 {
            let v = [
                s.min(255),
                s.saturating_sub(255).min(255),
                s.saturating_sub(510).min(255),
                s.saturating_sub(765).min(255),
            ];
            let rgba: Vec<u8> = v.iter().flat_map(|&c| [c as u8, 0, 255, 255]).collect();
            assert_eq!(halve(&rgba, 2, 2), halve_plain(&rgba, 2, 2), "{s}");
        }
    }

    #[test]
    fn a_picture_without_holes_is_left_alone() {
        let mut seed = 5u64;
        let mut rgba = picture(32, 32, &mut seed);
        for p in rgba.as_chunks_mut::<4>().0 {
            p[3] = p[3].max(1);
        }
        let before = rgba.clone();
        fill_holes(&mut rgba, 32, 32, HOLE_PASSES);
        assert!(rgba == before);
        // and one hole among drawn pixels is closed
        rgba[(5 * 32 + 5) * 4 + 3] = 0;
        fill_holes(&mut rgba, 32, 32, HOLE_PASSES);
        assert_eq!(rgba[(5 * 32 + 5) * 4 + 3], 255);
    }
}
