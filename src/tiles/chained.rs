//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! A block through the chain on the card (`chain.rs`): what `block_timed` does, the drawing and
//! every step after it kept on the card. The processor makes the lists to draw (the block, the
//! heights around it in two passes, as `block_timed`), chooses the second pass from the lowest
//! heights the card found, and gets the three zooms back.

use std::sync::atomic::Ordering;
use std::time::Instant;

use super::{BLOCK, BLOCK_PX, Clock, Placements, TOP, entities_around};
use crate::render::assets::{Assets, Draw};
use crate::render::gpu::chain::{Chain, Failure, Set, Zooms};
use crate::render::gpu::{self, Drawn, Keep};
use crate::render::raster::{Projection, Raster};
use crate::render::shade;
use crate::util::profile::{Phase, time};
use crate::world::World;
use crate::world::water::Water;

/// `assets::paint` for a picture kept on the card.
fn kept(raster: &Raster, keep: Keep, draws: &[Draw], assets: &mut Assets) -> Result<Drawn, Failure> {
    time(Phase::Paint, || {
        let options = crate::render::scene::Options { inverse: true, lod_scale: 3.0, clip: raster.clip() };
        gpu::paint_kept(raster, keep, &mut |sink| crate::render::scene::stream(draws, assets, &options, sink))
            .map(|(_, drawn)| drawn)
            .map_err(Failure::from)
    })
}

/// A set of the chain, given back if the block stops before its last step.
struct Taken<'a> {
    chain: &'a Chain,
    set: Option<Set>,
}

impl Drop for Taken<'_> {
    fn drop(&mut self) {
        if let Some(set) = self.set.take() {
            self.chain.give(set);
        }
    }
}

/// One block through the chain: its three zooms, or why not (the block is then made again,
/// `tiles::run`). `stages`: the steps of its end measured apart (slower: the measures).
#[allow(clippy::too_many_arguments)]
pub(super) fn block(
    chain: &Chain,
    places: &Placements,
    world: &World,
    water: &Water,
    assets: &mut Assets,
    bx: i32,
    by: i32,
    clock: &Clock,
    stages: Option<&mut Vec<(&'static str, f64)>>,
) -> Result<Zooms, Failure> {
    // (a block of the open sea has no entity at all)
    let draw = |i: u32| {
        let e = &places.entities[i as usize];
        world.archetypes.get(&e.archetype).map(|(a, _)| Draw { entity: e.clone(), archetype: a.clone(), distance: 0.0 })
    };
    let draws: Vec<Draw> =
        places.blocks.get(&(bx, by)).map(Vec::as_slice).unwrap_or(&[]).iter().filter_map(|&i| draw(i)).collect();
    let centre = [(bx as f32 + 0.5) * BLOCK, (by as f32 + 0.5) * BLOCK];
    let metres = BLOCK / BLOCK_PX as f32;
    let t = Instant::now();
    let raster =
        crate::render::gpu::chain::canvas(BLOCK_PX, BLOCK_PX, Projection::Orthographic { centre, metres, top: TOP });
    let picture = kept(&raster, Keep::Colour([0.0; 3]), &draws, assets)?;
    clock.paint.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    let mut taken = Taken { chain, set: None };
    // the light, as `block_timed` has it: the heights around the block in two passes, the second
    // chosen from the lowest heights of the block's parts (found on the card)
    let mut shading = None;
    if let Some(frame) =
        shade::Frame::new(centre, metres, BLOCK_PX, BLOCK_PX).filter(|_| places.blocks.contains_key(&(bx, by)))
    {
        let t = Instant::now();
        let around = time(Phase::Shade, || entities_around(places, world, &frame));
        let mut heights = time(Phase::Canvas, || frame.heights(TOP));
        // (only its size, projection and passes are used: the heights stay on the card)
        heights.colour = Vec::new();
        heights.depth = Vec::new();
        let near: Vec<Draw> =
            around.iter().filter(|(_, lo, hi)| frame.near(*lo, *hi)).filter_map(|(i, ..)| draw(*i)).collect();
        let first = kept(&heights, Keep::Heights, &near, assets)?;
        let (light, reach, metres, origin, window) = frame.shading();
        let [width, height] = frame.size();
        let set = taken.set.insert(chain.take(BLOCK_PX)?);
        let parts = chain.floor(set, &picture, &first, window, [width, height], TOP)?;
        let floor = time(Phase::Shade, || frame.far_from(&parts, &mut heights));
        let far: Vec<Draw> = around
            .iter()
            .filter(|(_, lo, hi)| frame.reaches(*lo, *hi, &floor))
            .filter_map(|(i, ..)| draw(*i))
            .collect();
        let second = kept(&heights, Keep::HeightsOver, &far, assets)?;
        clock.around.fetch_add(around.len() as u64, Ordering::Relaxed);
        clock.tall.fetch_add(far.len() as u64, Ordering::Relaxed);
        clock.margin.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
        let plan = crate::render::gpu::shade::plan_for(width, height, origin, metres, light, reach, window)
            .ok_or_else(|| Failure::Block("a picture the shading cannot take".to_string()))?;
        shading = Some((second, plan));
    }
    let t = Instant::now();
    let set = match taken.set.take() {
        Some(set) => set,
        None => chain.take(BLOCK_PX)?,
    };
    let spans = time(Phase::Holes, || water.spans(bx as f32 * BLOCK, (by + 1) as f32 * BLOCK, metres, BLOCK_PX));
    let zooms =
        chain.finish(set, &picture, shading.as_ref().map(|(drawn, plan)| (drawn, plan)), spans.as_ref(), TOP, stages);
    clock.shade.fetch_add(t.elapsed().as_nanos() as u64, Ordering::Relaxed);
    zooms
}

/// Blocks of the game drawn in memory, through the chain on the card and through the processor:
/// nothing is written anywhere (`cargo test --release -- --ignored --nocapture blocks_in_memory`,
/// alone). `BTMS_BLOCKS="6,-8 16,40 …"` (8 at most), `BTMS_BURST=<threads>x<blocks>` (a burst
/// after the comparison: each thread makes the blocks again and again through the chain),
/// `BTMS_BURST_PATH=chain|no-chain|cpu` (the way of the burst: the chain, the way before the chain
/// with the card drawing and shading, or the processor).
#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    use sha1::{Digest, Sha1};

    /// SHA-1 of a block's three zooms.
    fn digest(zooms: &Zooms) -> [u8; 20] {
        let mut h = Sha1::new();
        for z in [&zooms.z9, &zooms.z8, &zooms.z7] {
            h.update(z);
        }
        h.finalize().into()
    }

    use super::*;
    use crate::render::assets::{generic_textures, paint};
    use crate::tiles::{HOLE_PASSES, fill_holes, halve};

    /// Pixels of two RGBA pictures: equal, within 2/255 (alpha equal), within 8/255, alpha apart, largest gap.
    fn apart(a: &[u8], b: &[u8]) -> [usize; 5] {
        let mut out = [0usize; 5];
        for (p, q) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
            let gap = (0..3).map(|k| p[k].abs_diff(q[k])).max().unwrap() as usize;
            out[0] += (p == q) as usize;
            out[1] += (gap <= 2 && p[3] == q[3]) as usize;
            out[2] += (gap <= 8 && p[3] == q[3]) as usize;
            out[3] += (p[3] != q[3]) as usize;
            out[4] = out[4].max(gap);
        }
        out
    }

    fn say(name: &str, gap: [usize; 5], pixels: usize) -> String {
        let share = |n: usize| 100.0 * n as f64 / pixels.max(1) as f64;
        format!(
            "{name}: identical {:.3} %, within 2/255 {:.4} %, within 8/255 {:.4} %, different transparency {} px, largest gap {}/255",
            share(gap[0]),
            share(gap[1]),
            share(gap[2]),
            gap[3],
            gap[4]
        )
    }

    /// `block_timed` on the processor, each step timed (milliseconds): its copy, for the measures.
    #[allow(clippy::too_many_arguments)]
    fn processor_steps(
        places: &Placements,
        world: &World,
        water: &Water,
        assets: &mut Assets,
        bx: i32,
        by: i32,
    ) -> (Vec<u8>, Vec<(&'static str, f64)>) {
        let mut steps = Vec::new();
        let mut t = Instant::now();
        let mut lap = |name: &'static str, t: &mut Instant| {
            steps.push((name, t.elapsed().as_secs_f64() * 1e3));
            *t = Instant::now();
        };
        let draw = |i: u32| {
            let e = &places.entities[i as usize];
            world.archetypes.get(&e.archetype).map(|(a, _)| Draw {
                entity: e.clone(),
                archetype: a.clone(),
                distance: 0.0,
            })
        };
        let draws: Vec<Draw> =
            places.blocks.get(&(bx, by)).map(Vec::as_slice).unwrap_or(&[]).iter().filter_map(|&i| draw(i)).collect();
        let centre = [(bx as f32 + 0.5) * BLOCK, (by as f32 + 0.5) * BLOCK];
        let mut raster = Raster::new(
            BLOCK_PX,
            BLOCK_PX,
            Projection::Orthographic { centre, metres: BLOCK / BLOCK_PX as f32, top: TOP },
            [0.0; 3],
        );
        paint(&mut raster, &draws, assets, true, 3.0);
        lap("block drawing", &mut t);
        if let Some(frame) = shade::Frame::new(centre, BLOCK / BLOCK_PX as f32, BLOCK_PX, BLOCK_PX)
            .filter(|_| places.blocks.contains_key(&(bx, by)))
        {
            let around = entities_around(places, world, &frame);
            let mut heights = frame.heights(TOP);
            let near: Vec<Draw> =
                around.iter().filter(|(_, lo, hi)| frame.near(*lo, *hi)).filter_map(|(i, ..)| draw(*i)).collect();
            paint(&mut heights, &near, assets, true, 3.0);
            let floor = frame.far(&raster, &mut heights);
            let far: Vec<Draw> = around
                .iter()
                .filter(|(_, lo, hi)| frame.reaches(*lo, *hi, &floor))
                .filter_map(|(i, ..)| draw(*i))
                .collect();
            paint(&mut heights, &far, assets, true, 3.0);
            lap("heights around (two passes)", &mut t);
            frame.shade(&mut raster, &heights);
            lap("shading", &mut t);
        }
        let mut rgba = raster.rgba();
        lap("8 bits", &mut t);
        fill_holes(&mut rgba, BLOCK_PX, BLOCK_PX, HOLE_PASSES);
        lap("holes", &mut t);
        if !water.quads.is_empty() {
            water.flood(
                &mut rgba,
                raster.depths(),
                TOP,
                [bx as f32 * BLOCK, (by + 1) as f32 * BLOCK],
                BLOCK / BLOCK_PX as f32,
                BLOCK_PX,
            );
        }
        lap("water", &mut t);
        let half = halve(&rgba, BLOCK_PX, BLOCK_PX);
        let _quarter = halve(&half, BLOCK_PX / 2, BLOCK_PX / 2);
        lap("zooms 8 and 7", &mut t);
        (rgba, steps)
    }

    /// What the writing threads do with a block, without the files: the 21 tiles cut and encoded
    /// (milliseconds).
    fn encode(zooms: &Zooms) -> f64 {
        let t = Instant::now();
        let mut bytes = 0usize;
        let mut one = |rgba: &[u8]| {
            let (any, all) = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .fold((false, true), |(any, all), p| (any | (p[3] > 0), all & (p[3] == 255)));
            if !any {
                return;
            }
            if all {
                bytes += crate::tiles::jpeg::encode(&super::super::JPEG, rgba, super::super::JPEG_QUALITY).len();
            } else {
                let mut out = Vec::new();
                use image::ImageEncoder;
                image::codecs::png::PngEncoder::new(&mut out)
                    .write_image(
                        rgba,
                        super::super::TILE as u32,
                        super::super::TILE as u32,
                        image::ExtendedColorType::Rgba8,
                    )
                    .unwrap();
                bytes += out.len();
            }
        };
        let tile = super::super::TILE;
        for row in 0..4 {
            for col in 0..4 {
                one(&super::super::crop(&zooms.z9, BLOCK_PX, col * tile, row * tile, tile));
            }
        }
        for row in 0..2 {
            for col in 0..2 {
                one(&super::super::crop(&zooms.z8, BLOCK_PX / 2, col * tile, row * tile, tile));
            }
        }
        one(&zooms.z7);
        std::hint::black_box(bytes);
        t.elapsed().as_secs_f64() * 1e3
    }

    /// The processor time of this thread so far (user and kernel), in seconds.
    #[cfg(windows)]
    fn thread_cpu() -> f64 {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
        let (mut a, mut b, mut k, mut u) =
            (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
        // SAFETY: four FILETIME of this frame, the pseudo-handle of the current thread
        let ok = unsafe { GetThreadTimes(GetCurrentThread(), &mut a, &mut b, &mut k, &mut u) }.is_ok();
        let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64) as f64 * 1e-7;
        if ok { t(k) + t(u) } else { 0.0 }
    }

    #[cfg(not(windows))]
    fn thread_cpu() -> f64 {
        0.0
    }

    #[test]
    #[ignore = "blocks of the game drawn in memory: cargo test --release -- --ignored --nocapture blocks_in_memory"]
    fn blocks_in_memory() {
        let mut config = crate::config::Config::load(None).expect("map.toml");
        // (`BTMS_TEST_NO_LIGHTING`: the blocks without the light)
        if std::env::var_os("BTMS_TEST_NO_LIGHTING").is_some() {
            config.lighting.enabled = false;
        }
        crate::config::init(config);
        crate::render::gpu::choose(&mut Vec::new()).unwrap();
        crate::render::gpu::shade::choose(&mut Vec::new()).unwrap();
        crate::render::gpu::chain::choose(&mut Vec::new()).unwrap();
        let blocks: Vec<(i32, i32)> = std::env::var("BTMS_BLOCKS")
            .unwrap_or_else(|_| "6,-8 16,40".to_string())
            .split_whitespace()
            .filter_map(|b| b.split_once(',').and_then(|(x, y)| Some((x.parse().ok()?, y.parse().ok()?))))
            .collect();
        assert!(!blocks.is_empty() && blocks.len() <= 8, "1 to 8 blocks");
        let game = crate::game::game_dir(None).expect("the game");
        let keys = crate::game::keys(&game).expect("the keys");
        let t = Instant::now();
        let world = World::load(&game, &keys, false).expect("the world");
        let generic = generic_textures(&game, &keys, &world);
        let wet = crate::config::get().water.enabled;
        let water = if wet { Water::load(&game, &keys) } else { Water::default() };
        crate::world::water::draw_meshes(wet);
        let chain = crate::render::gpu::chain::active().expect("the chain on the card");
        println!("world read in {:.1} s; chain on {}", t.elapsed().as_secs_f64(), chain.name);
        // each block's placements (its zone and the margins of the light)
        let places: Vec<Placements> = blocks
            .iter()
            .map(|&(bx, by)| {
                super::super::placements(
                    &game,
                    &keys,
                    &world,
                    Some([bx as f32 * BLOCK, by as f32 * BLOCK, (bx + 1) as f32 * BLOCK, (by + 1) as f32 * BLOCK]),
                )
                .expect("placements")
            })
            .collect();
        let clock = Clock::default();
        let mut assets = Assets::new(&game, &keys, &world, &generic);
        let (mut all_cpu, mut all_card, mut pixels) = ([0usize; 5], [0usize; 5], 0usize);
        let compare = std::env::var_os("BTMS_BURST_ONLY").is_none();
        // each block's zooms as one thread made them: every block of the burst must be the same
        let mut alone: Vec<[u8; 20]> = Vec::new();
        for (k, &(bx, by)) in blocks.iter().enumerate() {
            let p = &places[k];
            // (once each, unmeasured: the files and textures loaded, the card's caches filled)
            alone.push(digest(
                &block(chain, p, &world, &water, &mut assets, bx, by, &clock, None)
                    .expect("the block through the chain"),
            ));
            if !compare {
                continue;
            }
            let entities = p.blocks.get(&(bx, by)).map_or(0, Vec::len);
            // the processor's chain, the way before the chain (the card draws and shades), the chain
            let t = Instant::now();
            let cpu = crate::render::gpu::on_processor(|| {
                super::super::block_timed(p, &world, &water, &mut assets, bx, by, &clock)
            });
            let cpu_ms = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let before = super::super::block_timed(p, &world, &water, &mut assets, bx, by, &clock);
            let before_ms = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let zooms = block(chain, p, &world, &water, &mut assets, bx, by, &clock, None)
                .expect("the block through the chain");
            let chain_ms = t.elapsed().as_secs_f64() * 1e3;
            let cpu8 = halve(&cpu, BLOCK_PX, BLOCK_PX);
            let cpu7 = halve(&cpu8, BLOCK_PX / 2, BLOCK_PX / 2);
            let n = BLOCK_PX * BLOCK_PX;
            let (g9, g8, g7) = (apart(&cpu, &zooms.z9), apart(&cpu8, &zooms.z8), apart(&cpu7, &zooms.z7));
            let b9 = apart(&before, &zooms.z9);
            // where the gap to the processor comes from: the card's drawing alone (shaded by the
            // processor), against the processor's
            let drawn = crate::render::gpu::shade::shading_on_processor(|| {
                super::super::block_timed(p, &world, &water, &mut assets, bx, by, &clock)
            });
            println!(
                "  {}",
                say(
                    "drawn by the card, shaded by the processor, against the processor, zoom 9",
                    apart(&cpu, &drawn),
                    n
                )
            );
            println!(
                "  {}",
                say(
                    "drawn by the card, shaded by the card, against drawn by the card, shaded by the processor, zoom 9",
                    apart(&drawn, &zooms.z9),
                    n
                )
            );
            let empty = cpu.as_chunks::<4>().0.iter().filter(|p| p[3] == 0).count();
            println!("block {bx},{by}: {entities} entities, {empty} empty pixels (processor)");
            println!("  {}", say("chain against the processor, zoom 9", g9, n));
            println!("  {}", say("chain against the processor, zoom 8", g8, n / 4));
            println!("  {}", say("chain against the processor, zoom 7", g7, n / 16));
            println!(
                "  {}",
                say(
                    "chain against the way before it (the card draws and shades, the rest on the processor), zoom 9",
                    b9,
                    n
                )
            );
            assert!(
                zooms.z8 == halve(&zooms.z9, BLOCK_PX, BLOCK_PX)
                    && zooms.z7 == halve(&zooms.z8, BLOCK_PX / 2, BLOCK_PX / 2)
            );
            for k in 0..3 {
                all_cpu[k] += g9[k];
                all_card[k] += b9[k];
            }
            all_cpu[3] += g9[3];
            all_card[3] += b9[3];
            all_cpu[4] = all_cpu[4].max(g9[4]);
            all_card[4] = all_card[4].max(b9[4]);
            pixels += n;
            // times: whole blocks, the steps of each way, the encoding
            let (check, steps) =
                crate::render::gpu::on_processor(|| processor_steps(p, &world, &water, &mut assets, bx, by));
            assert!(check == cpu, "the copy of block_timed differs from it");
            let mut card_steps = Vec::new();
            let t = Instant::now();
            let measured = block(chain, p, &world, &water, &mut assets, bx, by, &clock, Some(&mut card_steps))
                .expect("the measured block");
            let measured_ms = t.elapsed().as_secs_f64() * 1e3;
            assert!(measured.z9 == zooms.z9);
            let encode_ms = encode(&zooms);
            println!(
                "  time of a block, one thread: processor {cpu_ms:.0} ms, the way before the chain {before_ms:.0} ms, chain {chain_ms:.0} ms; encoding the 21 tiles (without the files) {encode_ms:.0} ms"
            );
            println!(
                "  processor, by step: {}",
                steps.iter().map(|(s, ms)| format!("{s} {ms:.1}")).collect::<Vec<_>>().join(", ")
            );
            println!(
                "  chain, steps on the card measured one by one ({measured_ms:.0} ms in all, drawings included): {}",
                card_steps.iter().map(|(s, ms)| format!("{s} {ms:.2}")).collect::<Vec<_>>().join(", ")
            );
            crate::render::gpu::chain::block_end();
        }
        if compare {
            println!(
                "{}",
                say(&format!("in all ({} blocks), chain against the processor, zoom 9", blocks.len()), all_cpu, pixels)
            );
            println!("{}", say("in all, chain against the way before it, zoom 9", all_card, pixels));
        }
        // the burst: threads × rounds over the blocks, each thread its own assets
        if let Some((threads, rounds)) = std::env::var("BTMS_BURST").ok().and_then(|v| {
            v.split_once('x').and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)))
        }) {
            let way = std::env::var("BTMS_BURST_PATH").unwrap_or_else(|_| "chain".to_string());
            // (as `tiles::run`: a set of the chain per thread, unless `BTMS_BURST_SETS` says how many)
            chain.reserve(std::env::var("BTMS_BURST_SETS").ok().and_then(|v| v.parse().ok()).unwrap_or(threads));
            let next = AtomicUsize::new(0);
            // (`<threads>x<blocks>`: so many blocks in all, the same 8 or fewer again and again)
            let total = rounds;
            let busy = Mutex::new(Vec::new());
            let (same, other) = (AtomicUsize::new(0), AtomicUsize::new(0));
            let unix = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
            println!("burst: start {:.3}", unix());
            let t = Instant::now();
            std::thread::scope(|s| {
                for _ in 0..threads {
                    s.spawn(|| {
                        let mut assets = Assets::new(&game, &keys, &world, &generic);
                        let clock = Clock::default();
                        let mut mine = 0.0;
                        let cpu = thread_cpu();
                        let mut encoding = 0.0;
                        let encode_too = std::env::var_os("BTMS_BURST_ENCODE").is_some();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            if i >= total {
                                break;
                            }
                            let k = i % blocks.len();
                            let (bx, by) = blocks[k];
                            let t = Instant::now();
                            let zooms = match way.as_str() {
                                "cpu" => {
                                    let z9 = crate::render::gpu::on_processor(|| {
                                        super::super::block_timed(
                                            &places[k],
                                            &world,
                                            &water,
                                            &mut assets,
                                            bx,
                                            by,
                                            &clock,
                                        )
                                    });
                                    let z8 = halve(&z9, BLOCK_PX, BLOCK_PX);
                                    let z7 = halve(&z8, BLOCK_PX / 2, BLOCK_PX / 2);
                                    Zooms { z9, z8, z7 }
                                }
                                "no-chain" => {
                                    let z9 = super::super::block_timed(
                                        &places[k],
                                        &world,
                                        &water,
                                        &mut assets,
                                        bx,
                                        by,
                                        &clock,
                                    );
                                    let z8 = halve(&z9, BLOCK_PX, BLOCK_PX);
                                    let z7 = halve(&z8, BLOCK_PX / 2, BLOCK_PX / 2);
                                    Zooms { z9, z8, z7 }
                                }
                                // as `tiles::run` makes a block: the chain, or what replaces it
                                _ => match super::super::block(
                                    Some(chain),
                                    &places[k],
                                    &world,
                                    &water,
                                    &mut assets,
                                    bx,
                                    by,
                                    &clock,
                                ) {
                                    crate::tiles::write::Pixels::Zooms(zooms) => zooms,
                                    crate::tiles::write::Pixels::Whole(z9) => {
                                        let z8 = halve(&z9, BLOCK_PX, BLOCK_PX);
                                        let z7 = halve(&z8, BLOCK_PX / 2, BLOCK_PX / 2);
                                        Zooms { z9, z8, z7 }
                                    }
                                },
                            };
                            mine += t.elapsed().as_secs_f64();
                            // (a block the processor made instead is not the chain's: only the chain's are held to it)
                            if digest(&zooms) == alone[k] {
                                same.fetch_add(1, Ordering::Relaxed)
                            } else {
                                other.fetch_add(1, Ordering::Relaxed)
                            };
                            if encode_too {
                                encoding += encode(&zooms) / 1e3;
                            }
                            std::hint::black_box(&zooms);
                            crate::render::gpu::chain::block_end();
                        }
                        busy.lock().unwrap().push((mine, thread_cpu() - cpu, encoding));
                    });
                }
            });
            let wall = t.elapsed().as_secs_f64();
            println!("burst: end {:.3}", unix());
            println!(
                "burst: {} blocks bit-identical to the same block made alone, {} others",
                same.load(Ordering::Relaxed),
                other.load(Ordering::Relaxed)
            );
            if way == "chain"
                && std::env::var_os("BTMS_CHAIN_FAULT").is_none()
                && std::env::var_os("BTMS_GPU_FAULT").is_none()
            {
                assert_eq!(other.load(Ordering::Relaxed), 0, "some blocks of the burst are not the ones made alone");
            }
            let busy = busy.into_inner().unwrap();
            let sum = |k: usize| busy.iter().map(|b| [b.0, b.1, b.2][k]).sum::<f64>() / total as f64 * 1e3;
            println!(
                "burst ({way}): {total} blocks on {threads} threads in {wall:.2} s, {:.1} blocks per second ({}); per block: {:.0} ms of thread time, {:.0} ms of it on the processor (the rest: waiting for the card, for the locks){}",
                total as f64 / wall,
                if std::env::var_os("BTMS_BURST_ENCODE").is_some() {
                    "tile encoding included, on the same threads"
                } else {
                    "without the encoding"
                },
                sum(0) + sum(2),
                sum(1),
                if std::env::var_os("BTMS_BURST_ENCODE").is_some() {
                    format!("; the encoding alone: {:.0} ms per block", sum(2))
                } else {
                    String::new()
                }
            );
        }
        // what the blocks hold of water: the rectangles of the files, the water pieces of models drawn
        println!(
            "water: rectangles of the files {:?}; water pieces of the models drawn: {:?}",
            water.areas,
            crate::world::water::shaders()
                .iter()
                .map(|(h, n)| (crate::world::water::shader_name(*h).unwrap_or("?"), *n))
                .collect::<Vec<_>>()
        );
        if let Some(line) = crate::render::gpu::chain::summary() {
            println!("{line}");
        }
        if let Some(line) = crate::render::gpu::chain::end(true) {
            println!("{line}");
        }
    }
}
