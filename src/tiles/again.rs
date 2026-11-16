//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! Blocks of a map already drawn, drawn again in its own folder ([`redraw`], for `update`):
//! the render of the map itself, restricted to some of its blocks.
//!
//! Everything is done as the render that made the folder did it, so that nothing shows at the edge
//! of what is drawn again: the map's own zone (none for the whole map) for the holes of the ground
//! (`holes::fill`) and the placements; the same blocks drawn (those with entities, and with the
//! water the rectangle around the map: [`super::blocks_to_draw`]); the same drawing, light and water
//! (`super::block`); the folder's own format. Then, unlike a render into an empty folder:
//!
//! - a tile of a block drawn again that comes out empty is removed, and so are the 21 tiles of a
//!   block that is no longer drawn at all;
//! - the last holes are looked for from the blocks drawn again, the blocks around read from the
//!   folder as the flood reaches them (`mend::mend_with`), as the render of the whole map sees them;
//! - zooms 6 to 0 are made again above every block touched, a tile whose children are all gone
//!   removed, and the change carried up to zoom 0 ([`lower_zooms_pruned`]);
//! - `tiles.json` is not written here: the caller patches the map's own ([`Plan::finish`]).

use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::Result;
use rage_rpf::GtaKeys;

use super::{Clock, Options, Placements, Written, mend, options, parent_tile, tile_path, write, z_order};
use crate::render::assets::{Assets, generic_textures};
use crate::world::World;

/// The blocks to draw again, and the rectangle around them (the `zone` of the event `plan`).
pub struct Redraw {
    pub blocks: BTreeSet<(i32, i32)>,
    pub zone: [f32; 4],
}

/// What a [`redraw`] did.
pub struct Done {
    /// The blocks drawn again.
    pub drawn: Vec<(i32, i32)>,
    /// The blocks asked for that are not drawn (nothing there): their tiles removed.
    pub cleared: Vec<(i32, i32)>,
    /// Blocks whose tiles could not be written.
    pub errors: usize,
}

/// What decides the blocks of a [`redraw`] and ends it.
pub trait Plan {
    /// Whether the events of the render wait for [`Plan::blocks`] (which tells the event
    /// `changes` first).
    fn defers(&self) -> bool;
    /// The rectangle the `settings: …` line says, when it is known before the world is read.
    fn zone(&self) -> Option<[f32; 4]>;
    /// The blocks to draw again, once the world and the placements are read; None when there is
    /// nothing to draw (the render stops there).
    fn blocks(&mut self, world: &World, places: &Placements) -> Result<Option<Redraw>>;
    /// Once the tiles are written: the map's `tiles.json`, the record of the resources.
    fn finish(&mut self, world: &World, places: &Placements, done: &Done) -> Result<()>;
}

/// The tiles of a block, in the order `write::tiles` writes them: zoom 9 (4 × 4, the north row
/// first), zoom 8 (2 × 2), zoom 7.
pub fn block_tiles(bx: i32, by: i32) -> Vec<(i32, i32, i32)> {
    let mut out = Vec::with_capacity(21);
    for row in 0..4 {
        for col in 0..4 {
            out.push((9, bx * 4 + col, by * 4 + (3 - row)));
        }
    }
    for row in 0..2 {
        for col in 0..2 {
            out.push((8, bx * 2 + col, by * 2 + (1 - row)));
        }
    }
    out.push((7, bx, by));
    out
}

/// Removes the tile (z, tx, ty) of `out`, whatever its extension. Whether there was one.
pub fn remove_tile(out: &Path, z: i32, tx: i32, ty: i32) -> bool {
    let mut any = false;
    for ext in super::format::EXTENSIONS {
        any |= std::fs::remove_file(tile_path(out, z, tx, ty, ext)).is_ok();
    }
    any
}

/// Whether `out` has the tile (z, tx, ty), in its format.
pub fn has_tile(out: &Path, z: i32, tx: i32, ty: i32) -> bool {
    super::format::of(out).extensions().iter().any(|ext| tile_path(out, z, tx, ty, ext).is_file())
}

/// Takes back the maps loaded on request the render of the map in `out` drew (`on_demand` of
/// its record, `resources.json`) when the record knows them and was made with the settings of
/// now: those left out now are kept, as its search of the holes kept them. Whether it was done.
fn reuse_on_request(out: &Path, world: &mut World) -> bool {
    if !crate::config::get().maps.fill_holes || world.groups.left_out.is_empty() {
        return false;
    }
    let Ok(Some(record)) = crate::update::record::read(out) else { return false };
    let Some(drawn) = record.on_demand.as_ref().filter(|_| crate::update::record::settings_changed(&record).is_none())
    else {
        return false;
    };
    let drawn: HashSet<&str> = drawn.iter().map(String::as_str).collect();
    let mut taken = 0;
    for f in world.groups.files.iter_mut().filter(|f| f.kept.is_none() && drawn.contains(f.name.as_str())) {
        f.kept = Some(crate::world::groups::Kept::Hole);
        taken += 1;
    }
    world.groups.left_out = world.groups.files.iter().filter(|f| f.kept.is_none()).map(|f| f.hash).collect();
    println!(
        "holes: the map's on-demand maps taken back from its record of {} ({taken} kept to fill holes); holes are only looked for around what is drawn again",
        record.date
    );
    true
}

/// The blocks of the rectangle a map in `out` covers: `bounds` of its `tiles.json` (what its
/// render drew, sea included), else the extent of its tiles of zoom 7 on disk; none when neither is
/// there.
pub fn map_rectangle(out: &Path) -> Vec<(i32, i32)> {
    let written = std::fs::read_to_string(out.join("tiles.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|m| m.get("bounds").and_then(|l| serde_json::from_value::<[f64; 4]>(l.clone()).ok()));
    let Some([x1, y1, x2, y2]) = written.or_else(|| super::format::bounds(out)) else { return Vec::new() };
    let side = super::BLOCK as f64;
    let (bx0, bx1) = ((x1 / side).floor() as i32, ((x2 / side).ceil() as i32) - 1);
    let (by0, by1) = ((y1 / side).floor() as i32, ((y2 / side).ceil() as i32) - 1);
    // (a rectangle out of reason is not trusted: the whole world is 256 blocks a side)
    if bx1 < bx0 || by1 < by0 || bx1 - bx0 > 256 || by1 - by0 > 256 {
        return Vec::new();
    }
    (by0..=by1).flat_map(|by| (bx0..=bx1).map(move |bx| (bx, by))).collect()
}

/// Makes zooms 6 to 0 again above these blocks (zoom 7), each tile from the four below it: a tile
/// whose children are all gone (or empty) is removed, and every tile looked at carries the change
/// to its parent, up to zoom 0. Returns how many were written at each zoom.
pub fn lower_zooms_pruned(out: &Path, blocks: Vec<(i32, i32)>, threads: usize) -> Vec<String> {
    let mut level = blocks;
    let mut counts = Vec::new();
    for z in (0..7).rev() {
        let mut parents: Vec<(i32, i32)> = level.iter().map(|&(x, y)| (x.div_euclid(2), y.div_euclid(2))).collect();
        parents.sort_unstable();
        parents.dedup();
        let next = AtomicUsize::new(0);
        let (written, removed) = (AtomicUsize::new(0), AtomicUsize::new(0));
        std::thread::scope(|s| {
            for _ in 0..threads.max(1) {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(tx, ty)) = parents.get(i) else { break };
                        match parent_tile(out, z, tx, ty) {
                            Ok(Written::Empty) => {
                                if remove_tile(out, z, tx, ty) {
                                    removed.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            Ok(_) => {
                                written.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(_) => {}
                        }
                    }
                });
            }
        });
        let removed = removed.into_inner();
        counts.push(format!(
            "z{z} {}{}",
            written.into_inner(),
            if removed > 0 { format!(" ({removed} removed)") } else { String::new() }
        ));
        level = parents;
    }
    counts
}

/// Draws again the blocks `plan` gives of the map in `opts.out` (see the module). `opts.zone` is
/// the map's own zone (None for the whole map), `opts.format` its own format.
pub fn redraw(game: &Path, keys: &GtaKeys, opts: &Options, plan: &mut dyn Plan) -> Result<()> {
    let t0 = Instant::now();
    crate::render::gpu::warm();
    let defer = plan.defers();
    // the `settings: …` line says what is drawn again, not the map's own zone
    let said = |zone: Option<[f32; 4]>| Options {
        out: opts.out.clone(),
        zone,
        threads: opts.threads,
        ram_budget: opts.ram_budget,
        card_budget: opts.card_budget,
        dry_run: false,
        format: opts.format,
        effort: opts.effort,
    };
    let told = (crate::progress::active() && !defer).then(|| options::resolve(opts).tell(&said(plan.zone())));
    if defer {
        crate::progress::log("update: reading the world (game files and server resources)…");
    } else {
        crate::progress::phase("world");
    }
    let mut world = World::load(game, keys, false)?;
    let settings = match told {
        Some(s) => s,
        None if defer => options::resolve(opts),
        None => options::resolve(opts).tell(&said(plan.zone())),
    };
    let threads = settings.drawers;
    let chain = if settings.chain { crate::render::gpu::chain::active() } else { None };
    if let Some(chain) = chain {
        chain.reserve(threads);
    }
    // the holes of the ground as the render of the map found them: over its own zone
    if crate::config::get().maps.fill_holes && !world.groups.left_out.is_empty() {
        crate::progress::log("holes: looking for holes in the ground that an on-demand map closes…");
    }
    // the maps loaded on request that the render of the map kept to close holes of the ground:
    // taken back from the record of the folder when it knows them and its settings are those of
    // now (the search over the whole map takes a minute or more), the holes then looked for only
    // around what is drawn again, once that is known (below); else the whole search, as the render
    // of the map made it
    let reused = reuse_on_request(&opts.out, &mut world);
    if !reused {
        crate::world::holes::fill(game, keys, &mut world, opts.zone, threads, settings.ram_budget)?;
    }
    let generic = generic_textures(game, keys, &world);
    if !defer {
        crate::progress::phase("placements");
    }
    let mut places = super::placements(game, keys, &world, opts.zone)?;
    let wet = crate::config::get().water.enabled;
    let water = if wet { crate::world::water::Water::load(game, keys) } else { crate::world::water::Water::default() };
    crate::world::water::draw_meshes(wet);
    // what the render of the map draws, and of that what the plan asks for
    let drawn_by_map = |places: &Placements| {
        let mut all: HashSet<(i32, i32)> = super::blocks_to_draw(places, &water, opts.zone, wet).into_iter().collect();
        // the sea of the map as its render drew it: every block of the map's own rectangle, even
        // when what is read now ends sooner (a resource gone, other settings), so that a block of
        // its sea is never taken for nothing and its tiles removed
        if wet {
            all.extend(map_rectangle(&opts.out).into_iter().filter(|b| super::in_zone(opts.zone, *b)));
        }
        all
    };
    let mut all = drawn_by_map(&places);
    let Some(mut asked) = plan.blocks(&world, &places)? else { return Ok(()) };
    // the choice of the record taken back: the holes around what is drawn again (a block more on
    // every side). A map loaded on request that closes one there now is kept, the placements read
    // again with it, and its own ground drawn again too
    if reused {
        let before = world.groups.left_out.clone();
        let z = asked.zone;
        let around = [z[0] - super::BLOCK, z[1] - super::BLOCK, z[2] + super::BLOCK, z[3] + super::BLOCK];
        crate::world::holes::fill(game, keys, &mut world, Some(around), threads, settings.ram_budget)?;
        let now_kept: HashSet<u32> = before.difference(&world.groups.left_out).copied().collect();
        if !now_kept.is_empty() {
            places = super::placements(game, keys, &world, opts.zone)?;
            all = drawn_by_map(&places);
            for m in world.ymaps.iter().filter(|m| now_kept.contains(&m.hash)) {
                let range = |k: usize| {
                    (
                        (m.entities_min[k] / super::BLOCK).floor() as i32,
                        (m.entities_max[k] / super::BLOCK).floor() as i32,
                    )
                };
                let ((x0, x1), (y0, y1)) = (range(0), range(1));
                if x1 - x0 > 64 || y1 - y0 > 64 {
                    continue;
                }
                asked.blocks.extend(
                    (y0..=y1)
                        .flat_map(|by| (x0..=x1).map(move |bx| (bx, by)))
                        .filter(|b| super::in_zone(opts.zone, *b)),
                );
            }
            let (bx0, by0) = (
                asked.blocks.iter().map(|b| b.0).min().unwrap_or(0),
                asked.blocks.iter().map(|b| b.1).min().unwrap_or(0),
            );
            let (bx1, by1) = (
                asked.blocks.iter().map(|b| b.0).max().unwrap_or(0),
                asked.blocks.iter().map(|b| b.1).max().unwrap_or(0),
            );
            asked.zone = [
                bx0 as f32 * super::BLOCK,
                by0 as f32 * super::BLOCK,
                (bx1 + 1) as f32 * super::BLOCK,
                (by1 + 1) as f32 * super::BLOCK,
            ];
        }
    }
    let settings = if defer {
        let s = settings.tell(&said(Some(asked.zone)));
        crate::progress::phase("world");
        crate::progress::phase("placements");
        s
    } else {
        settings
    };
    let mut order: Vec<(i32, i32)> = asked.blocks.iter().copied().filter(|b| all.contains(b)).collect();
    order.sort_by_key(|b| z_order(*b));
    // the blocks asked for where the map draws nothing: their tiles go
    let cleared: Vec<(i32, i32)> = asked.blocks.iter().copied().filter(|b| !all.contains(b)).collect();
    let mut removed = 0;
    for &(bx, by) in &cleared {
        for (z, tx, ty) in block_tiles(bx, by) {
            removed += remove_tile(&opts.out, z, tx, ty) as usize;
        }
    }
    super::format::set(&opts.out, opts.format, opts.effort);
    println!(
        "update: {} block(s) to draw again in {} ({}){}",
        order.len(),
        opts.out.display(),
        opts.format.label(),
        if cleared.is_empty() {
            String::new()
        } else {
            format!("; {} block(s) where the map draws nothing, {removed} tile(s) removed", cleared.len())
        }
    );
    crate::progress::plan(&order, threads, &opts.out, Some(asked.zone));
    crate::progress::phase("blocks");

    // zooms 9, 8, 7 of the blocks, as the render draws them; a tile that comes out empty is removed
    let next = AtomicUsize::new(0);
    let counts = Mutex::new([0usize; 4]);
    let finished = Mutex::new(Vec::<(i32, i32)>::new());
    let drawing = AtomicBool::new(true);
    let errors = Mutex::new(Vec::<String>::new());
    let clock = Clock::default();
    let t = Instant::now();
    let total = order.len();
    std::thread::scope(|s| {
        s.spawn(|| {
            let mut last = Instant::now();
            while drawing.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(500));
                if last.elapsed().as_secs_f64() < super::LOWER_ZOOMS_EVERY_S {
                    continue;
                }
                last = Instant::now();
                let blocks = std::mem::take(&mut *finished.lock().unwrap());
                if !blocks.is_empty() {
                    let passed = blocks.len();
                    lower_zooms_pruned(&opts.out, blocks, 2);
                    crate::progress::lower_zooms(passed);
                }
            }
        });
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
                    let pixels = super::block(chain, &places, &world, &water, &mut assets, bx, by, &clock);
                    hand(write::Picture { bx, by, pixels, started });
                    crate::util::profile::flush();
                    assets.trim(settings.ram_budget);
                }
            },
            |report, result| {
                let (bx, by) = (report.bx, report.by);
                match result {
                    Ok(w) => {
                        let ids = block_tiles(bx, by);
                        let mut gone = 0;
                        for (x, &(z, tx, ty)) in w.iter().zip(&ids) {
                            if *x == Written::Empty {
                                gone += remove_tile(&opts.out, z, tx, ty) as usize;
                            }
                        }
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
                        for x in &w {
                            match x {
                                Written::Jpeg => c[0] += 1,
                                Written::Png => c[1] += 1,
                                Written::Empty => c[2] += 1,
                            }
                        }
                        c[3] += gone;
                    }
                    Err(e) => {
                        crate::progress::block_failed(bx, by, &format!("{e:#}"));
                        errors.lock().unwrap().push(format!("block {bx},{by}: {e:#}"))
                    }
                }
                crate::util::profile::flush();
                finished.lock().unwrap().push((bx, by));
            },
        );
    });
    let [full, partial, empty, gone] = *counts.lock().unwrap();
    println!(
        "zooms 9 to 7: {total} block(s) in {:.0} s; {}{}",
        t.elapsed().as_secs_f64(),
        opts.format.counts(full, partial, empty),
        if gone > 0 { format!("; {gone} tile(s) that became empty removed") } else { String::new() }
    );
    let errors = errors.into_inner().unwrap();
    for e in errors.iter().take(10) {
        println!("  {e}");
    }
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

    // the last holes, from the blocks drawn again, the others read from the folder as reached
    let mut lower: BTreeSet<(i32, i32)> = order.iter().chain(&cleared).copied().collect();
    if crate::config::get().maps.fill_holes {
        let mended = mend::mend_with(&opts.out, &order, true)?;
        mend::report(&mended);
        lower.extend(mended.blocks);
    }
    crate::progress::phase("zooms");
    let t = Instant::now();
    let lower_counts = lower_zooms_pruned(&opts.out, lower.into_iter().collect(), threads.min(options::logical()));
    println!("zooms 6 to 0: {} in {:.0} s", lower_counts.join(", "), t.elapsed().as_secs_f64());
    crate::progress::lower_zooms(total);

    plan.finish(&world, &places, &Done { drawn: order.clone(), cleared, errors: errors.len() })?;
    crate::world::fivem::say_scrambled(&world.index);
    println!("update: {} in {:.1} min", opts.out.display(), t0.elapsed().as_secs_f64() / 60.0);
    crate::progress::end(total, errors.len(), t0.elapsed().as_secs_f64());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{BLOCK_PX, TILE, format, read_tile};

    /// The map's own rectangle, from `bounds` (the sea a render drew beyond the land): every block
    /// of it, so that an update never takes a block of the map's sea for nothing.
    #[test]
    fn the_rectangle_of_the_map_is_its_own() {
        let out = std::env::temp_dir().join(format!("btms-tests-{}-rectangle", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        assert!(super::map_rectangle(&out).is_empty(), "no manifest, no tile: nothing");
        std::fs::write(out.join("tiles.json"), r#"{"bounds": [-6272.0, 7296.0, -5760.0, 7552.0]}"#).unwrap();
        let blocks = super::map_rectangle(&out);
        assert_eq!(blocks.len(), 4 * 2);
        assert!(
            blocks.contains(&(-49, 57))
                && blocks.contains(&(-46, 58))
                && !blocks.contains(&(-45, 58))
                && !blocks.contains(&(-49, 59))
        );
        // a rectangle out of reason is not trusted
        std::fs::write(out.join("tiles.json"), r#"{"bounds": [-1e9, 0.0, 1e9, 128.0]}"#).unwrap();
        assert!(super::map_rectangle(&out).is_empty());
        let _ = std::fs::remove_dir_all(&out);
    }
    use super::*;
    use crate::tiles::webp::Quality;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("btms-tests-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A block's picture: opaque, of one colour, or empty.
    fn picture(colour: Option<[u8; 3]>) -> Vec<u8> {
        let mut rgba = vec![0u8; BLOCK_PX * BLOCK_PX * 4];
        if let Some(c) = colour {
            for p in rgba.as_chunks_mut::<4>().0 {
                p.copy_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        rgba
    }

    #[test]
    fn the_tiles_of_a_block_are_those_written_in_their_order() {
        let out = scratch("block-tiles");
        format::set(&out, format::Format::Webp(Quality::Lossless), crate::tiles::webp::Effort::Fast);
        let w = write::tiles(&out, -3, 5, &write::Pixels::Whole(picture(Some([10, 20, 30])))).unwrap();
        assert_eq!(w.len(), 21);
        let ids = block_tiles(-3, 5);
        assert_eq!(ids.len(), 21);
        for &(z, tx, ty) in &ids {
            assert!(has_tile(&out, z, tx, ty), "{z}/{tx}/{ty}");
        }
        // what is written is what is listed: nothing else
        let mut count = 0;
        for z in [7, 8, 9] {
            for col in std::fs::read_dir(out.join(z.to_string())).unwrap().flatten() {
                count += std::fs::read_dir(col.path()).unwrap().count();
            }
        }
        assert_eq!(count, 21);
        let _ = std::fs::remove_dir_all(&out);
    }

    /// A block that comes out empty loses its tiles, and the zooms above it that only it fed; the
    /// others are made again from what is left.
    #[test]
    fn an_emptied_block_and_the_zooms_above_it_are_cleaned() {
        let out = scratch("emptied-block");
        format::set(&out, format::Format::Webp(Quality::Lossless), crate::tiles::webp::Effort::Fast);
        // two blocks side by side in the same zoom 6 tile (0 and 1), and one far away
        for (b, c) in [((0, 0), [200, 0, 0]), ((1, 0), [0, 0, 200]), ((40, 40), [0, 200, 0])] {
            write::tiles(&out, b.0, b.1, &write::Pixels::Whole(picture(Some(c)))).unwrap();
        }
        super::super::lower_zooms(&out, vec![(0, 0), (1, 0), (40, 40)], 2);
        let z6 = read_tile(&out, 6, 0, 0).unwrap();
        // the western half has the red block, the eastern the blue one (row 0 is the north edge: empty)
        let at = |rgba: &[u8], x: usize, y: usize| rgba[(y * TILE + x) * 4..][..4].to_vec();
        assert_eq!(
            (at(&z6, 10, TILE - 10), at(&z6, TILE - 10, TILE - 10)),
            (vec![200, 0, 0, 255], vec![0, 0, 200, 255])
        );
        // block (1, 0) drawn again empty: its tiles go
        let w = write::tiles(&out, 1, 0, &write::Pixels::Whole(picture(None))).unwrap();
        assert!(w.iter().all(|x| *x == Written::Empty));
        for (x, &(z, tx, ty)) in w.iter().zip(&block_tiles(1, 0)) {
            if *x == Written::Empty {
                remove_tile(&out, z, tx, ty);
            }
        }
        assert!(block_tiles(1, 0).iter().all(|&(z, tx, ty)| !has_tile(&out, z, tx, ty)));
        let counts = lower_zooms_pruned(&out, vec![(1, 0)], 2);
        assert_eq!(counts.len(), 7);
        // zoom 6 keeps the red block only
        let z6 = read_tile(&out, 6, 0, 0).unwrap();
        assert_eq!((at(&z6, 10, TILE - 10), at(&z6, TILE - 10, TILE - 10)), (vec![200, 0, 0, 255], vec![0, 0, 0, 0]));
        // block (0, 0) emptied too: zooms 6 to 2 above them go; zooms 1 and 0, which the far block
        // feeds too, stay
        for (z, tx, ty) in block_tiles(0, 0) {
            remove_tile(&out, z, tx, ty);
        }
        let counts = lower_zooms_pruned(&out, vec![(0, 0)], 2);
        assert!(counts[0].starts_with("z6 0 (1 removed)"), "{counts:?}");
        for z in 2..=6 {
            assert!(!has_tile(&out, z, 0, 0), "zoom {z}");
        }
        assert!(has_tile(&out, 1, 0, 0) && has_tile(&out, 0, 0, 0) && has_tile(&out, 6, 20, 20));
        let z0 = read_tile(&out, 0, 0, 0).unwrap();
        assert_eq!(
            z0.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count(),
            z0.as_chunks::<4>().0.iter().filter(|p| p[1] > 100).count()
        );
        // and with the far block gone as well, zoom 0 goes
        for (z, tx, ty) in block_tiles(40, 40) {
            remove_tile(&out, z, tx, ty);
        }
        lower_zooms_pruned(&out, vec![(40, 40)], 1);
        assert!(!has_tile(&out, 0, 0, 0));
        let _ = std::fs::remove_dir_all(&out);
    }
}
