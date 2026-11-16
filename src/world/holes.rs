//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! Holes: ground that only a map loaded on request draws.
//!
//! The packs of the game moved the ground of a few places (the sites of the mansions, a road
//! bend, a building of Paleto Bay) into placement files loaded on request, so that a script can
//! swap one state of the place for another. Left out like every other map loaded on request
//! ([`crate::world::groups`]), they leave a hole in the map. So, once the white list is applied, the
//! files left out that close a hole are kept after all ([`Kept::Hole`]):
//!
//! - every block of 128 m met by an entity of a file left out is drawn with what is kept, heights
//!   only, at [`METRES`] per pixel; its empty pixels are grouped in regions, joined across the
//!   blocks' edges;
//! - a region is a **hole** when it is closed all around by what is drawn. One that reaches a
//!   block where nothing at all is drawn (the open sea beyond the seabed) is not: a yacht moored
//!   there stays out;
//! - a hole of [`MIN_AREA`] m² or more is offered to the files left out that have entities in its
//!   blocks, each drawn alone the same way. It is closed by one file that covers [`CLOSED`] of
//!   it, the original state of the place before any other ([`rank`]), else by several together,
//!   the widest first; the states a mission leaves behind (burnt, destroyed) only when nothing
//!   else closes it. A hole no file closes is reported, and left.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use anyhow::Result;
use rage_rpf::GtaKeys;

use crate::formats::resource::Resource;
use crate::formats::ymap::{Entity, parse_ymap, world_box};
use crate::render::assets::{Assets, Draw, generic_textures, paint};
use crate::render::raster::{Projection, Raster};
use crate::world::groups::Kept;
use crate::world::{World, YmapInfo};

/// Side of a block, in metres (the blocks of the tiles).
const BLOCK: f32 = 128.0;
/// Pixels along a block: 0.25 m per pixel.
const PX: usize = 512;
const METRES: f32 = BLOCK / PX as f32;
/// Height the blocks are seen from.
const TOP: f32 = 3000.0;
/// A hole smaller than this many m² is left alone (seams between two pieces of ground, which
/// the tiles fill from their neighbours).
const MIN_AREA: f32 = 4.0;
/// Share of a hole the files kept for it must cover.
const CLOSED: f32 = 0.9;
/// A region spread over more blocks than this is not a hole (it is the outside).
const MAX_BLOCKS: usize = 600;

type BlockId = (i32, i32);

/// Entities by block, with the placement file each comes from.
pub struct Places {
    entities: Vec<Entity>,
    /// Hash of the placement file of each entity.
    owner: Vec<u32>,
    blocks: HashMap<BlockId, Vec<u32>>,
}

/// The HD entities of one placement file, as the tiles take them, each with the blocks its box
/// meets (bx0, by0, bx1, by1).
type FileEntities = Vec<(Entity, [i32; 4])>;

/// The HD entities of the placement files `pick` chooses (hash of the file, entities), in the order
/// of `world.ymaps`: read, decrypted and parsed on every thread, then put back in that order, so
/// that what comes out is what one thread would give. A file that does not read is left out.
fn read_files(
    game: &Path,
    keys: &GtaKeys,
    world: &World,
    pick: &(dyn Fn(&YmapInfo) -> bool + Sync),
) -> Vec<(u32, FileEntities)> {
    let chosen: Vec<&YmapInfo> = world.ymaps.iter().filter(|m| pick(m)).collect();
    let slots: Vec<Mutex<Option<FileEntities>>> = (0..chosen.len()).map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let threads = std::thread::available_parallelism().map(usize::from).unwrap_or(8).min(chosen.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(m) = chosen.get(i) else { break };
                    *slots[i].lock().unwrap() = read_one(game, keys, world, m);
                }
            });
        }
    });
    chosen
        .iter()
        .zip(slots)
        .filter_map(|(m, slot)| slot.into_inner().unwrap().map(|entities| (m.hash, entities)))
        .collect()
}

/// The HD entities of one placement file and their blocks; None when it does not read.
fn read_one(game: &Path, keys: &GtaKeys, world: &World, m: &YmapInfo) -> Option<FileEntities> {
    let map = crate::cli::archive::read_file(game, &m.path, keys)
        .and_then(|b| Resource::parse(&b))
        .and_then(|r| parse_ymap(&r))
        .ok()?;
    let map = crate::world::fivem::shown(&world.server, &m.path, map);
    let mut out = Vec::new();
    for e in map.entities.into_iter().filter(|e| !e.interior && matches!(e.lod_level, 0 | 5)) {
        let Some((a, _)) = world.archetypes.get(&e.archetype) else { continue };
        let (lo, hi) = world_box(&e, a, true);
        if !(lo[0].is_finite() && hi[0].is_finite() && lo[1].is_finite() && hi[1].is_finite()) {
            continue;
        }
        let (bx0, bx1) = ((lo[0] / BLOCK).floor() as i32, (hi[0] / BLOCK).floor() as i32);
        let (by0, by1) = ((lo[1] / BLOCK).floor() as i32, (hi[1] / BLOCK).floor() as i32);
        // a huge box (a sky dome, a broken placement) would fill thousands of blocks
        if (bx1 - bx0) > 64 || (by1 - by0) > 64 {
            continue;
        }
        out.push((e, [bx0, by0, bx1, by1]));
    }
    Some(out)
}

/// The entities of the files read that `keep` takes (by the hash of their file), by block, in the
/// order they were read.
fn places_of(files: &[(u32, FileEntities)], keep: &dyn Fn(u32) -> bool) -> Places {
    let mut places = Places { entities: Vec::new(), owner: Vec::new(), blocks: HashMap::new() };
    for (hash, entities) in files.iter().filter(|(h, _)| keep(*h)) {
        for (e, [bx0, by0, bx1, by1]) in entities {
            let id = places.entities.len() as u32;
            for by in *by0..=*by1 {
                for bx in *bx0..=*bx1 {
                    places.blocks.entry((bx, by)).or_default().push(id);
                }
            }
            places.entities.push(e.clone());
            places.owner.push(*hash);
        }
    }
    places
}

/// Which pixels of a block these entities cover (row 0 is the north edge), heights only.
fn coverage(world: &World, entities: &[&Entity], assets: &mut Assets, (bx, by): BlockId) -> Vec<bool> {
    let draws: Vec<Draw> = entities
        .iter()
        .filter_map(|e| {
            world.archetypes.get(&e.archetype).map(|(a, _)| Draw {
                entity: (*e).clone(),
                archetype: a.clone(),
                distance: 0.0,
            })
        })
        .collect();
    let centre = [(bx as f32 + 0.5) * BLOCK, (by as f32 + 0.5) * BLOCK];
    let mut raster = Raster::new(PX, PX, Projection::Orthographic { centre, metres: METRES, top: TOP }, [0.0; 3]);
    raster.set_pass(None, true);
    paint(&mut raster, &draws, assets, true, 3.0);
    raster.depths().iter().map(|d| d.is_finite()).collect()
}

/// What a block shows once drawn with what is kept.
struct Seen {
    /// Region of each pixel: 0 where something is drawn, else the number of its empty region.
    label: Vec<u32>,
    /// Pixels of each region (index = number − 1).
    sizes: Vec<usize>,
    /// The files left out with entities here, each with the pixels it covers.
    files: Vec<(u32, Vec<bool>)>,
}

/// The empty regions of a block (4-connected): the region number of each pixel, and their sizes.
fn regions(covered: &[bool]) -> (Vec<u32>, Vec<usize>) {
    let mut label = vec![0u32; PX * PX];
    let mut sizes = Vec::new();
    let mut stack = Vec::new();
    for start in 0..PX * PX {
        if covered[start] || label[start] != 0 {
            continue;
        }
        let id = sizes.len() as u32 + 1;
        let mut n = 0;
        label[start] = id;
        stack.push(start);
        while let Some(i) = stack.pop() {
            n += 1;
            let (x, y) = (i % PX, i / PX);
            let mut visit = |j: usize| {
                if !covered[j] && label[j] == 0 {
                    label[j] = id;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < PX {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - PX);
            }
            if y + 1 < PX {
                visit(i + PX);
            }
        }
        sizes.push(n);
    }
    (label, sizes)
}

/// Where a state of a place stands among the others: the original one first, then the normal
/// one, then any other, and last ([`WRECKED`]) what a mission leaves behind. Told from the name
/// of the file, which is all the game's data says (the river of Raton Canyon starts shallow).
pub fn rank(name: &str) -> u8 {
    let name = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    if has(&["original", "_orig"]) {
        0
    } else if has(&["_norm", "normal", "default", "unburnt", "intact", "closed", "tidy", "shallow"]) {
        1
    } else if has(&[
        "burnt", "burned", "_dest", "destroy", "damage", "wreck", "sunk", "crash", "ruin", "trash", "_after",
    ]) {
        WRECKED
    } else {
        2
    }
}

/// [`rank`] of the states a mission leaves behind.
const WRECKED: u8 = 3;

/// A hole and what was done about it.
pub struct Hole {
    /// Its middle, in metres, and its area in m².
    pub centre: [f32; 2],
    pub area: f32,
    /// Names of the files kept to close it (none: no file closes it).
    pub closed_by: Vec<String>,
}

/// Bytes of models and textures the search keeps at most (it draws heights only, block after
/// block: more would not make it faster).
pub const BUDGET: usize = 2_000_000_000;

/// Rounds of [`fill`] at most: a hole closed to [`CLOSED`] may leave a smaller one beside it (a road
/// along the ground just put back), which the next round closes.
const ROUNDS: usize = 4;

/// Keeps the files left out that close a hole of the map (see the module). `zone`: only the
/// blocks meeting it are looked at (the tiles of a zone). `budget`: bytes of models and textures
/// the run keeps (`--ram` of `tiles`); the search keeps [`BUDGET`] at most. Returns the
/// holes found, those no file closes last (`closed_by` empty).
pub fn fill(
    game: &Path,
    keys: &GtaKeys,
    world: &mut World,
    zone: Option<[f32; 4]>,
    threads: usize,
    budget: usize,
) -> Result<Vec<Hole>> {
    if !crate::config::get().maps.fill_holes || world.groups.left_out.is_empty() {
        return Ok(Vec::new());
    }
    let t = Instant::now();
    let mut closed: Vec<Hole> = Vec::new();
    let mut open: Vec<Hole> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    // read once for every round: the placement files that may be drawn (all but those a pack
    // replaces), on every thread, and the textures shared by the models
    let files = read_files(game, keys, world, &|m: &YmapInfo| !world.replaced.contains(&m.hash));
    let generic = generic_textures(game, keys, world);
    // the blocks drawn by a round, kept for the next one but those a file kept since meets
    let mut cache = Cache::default();
    for _ in 0..ROUNDS {
        let before: HashSet<u32> = world.groups.left_out.clone();
        let holes = find(game, keys, world, zone, threads.max(1), budget.min(BUDGET), &files, &generic, &mut cache);
        // the files that close a hole are kept, with the files they hang from
        let names: HashSet<&str> = holes.iter().flat_map(|h| h.closed_by.iter().map(String::as_str)).collect();
        let position: HashMap<u32, usize> = world.groups.files.iter().enumerate().map(|(i, f)| (f.hash, i)).collect();
        let chosen: Vec<usize> = world
            .groups
            .files
            .iter()
            .enumerate()
            .filter(|(_, f)| f.kept.is_none() && names.contains(f.name.as_str()))
            .map(|(i, _)| i)
            .collect();
        for &i in &chosen {
            world.groups.files[i].kept = Some(Kept::Hole);
            kept.push(world.groups.files[i].name.clone());
            let (child, mut parent) = (world.groups.files[i].name.clone(), world.groups.files[i].parent);
            let mut guard = 0;
            while let Some(&p) = position.get(&parent) {
                if world.groups.files[p].kept.is_none() {
                    world.groups.files[p].kept = Some(Kept::Parent(child.clone()));
                }
                parent = world.groups.files[p].parent;
                guard += 1;
                if guard > 16 {
                    break;
                }
            }
        }
        world.groups.left_out = world.groups.files.iter().filter(|f| f.kept.is_none()).map(|f| f.hash).collect();
        // what the files kept now meet is drawn again by the next round; the rest is as it was
        let now_kept: HashSet<u32> = before.difference(&world.groups.left_out).copied().collect();
        for (_, entities) in files.iter().filter(|(h, _)| now_kept.contains(h)) {
            for (_, [bx0, by0, bx1, by1]) in entities {
                for by in *by0..=*by1 {
                    for bx in *bx0..=*bx1 {
                        cache.seen.remove(&(bx, by));
                        cache.with_files.remove(&(bx, by));
                    }
                }
            }
        }
        let again = !chosen.is_empty();
        open.clear();
        for h in holes {
            if h.closed_by.is_empty() { open.push(h) } else { closed.push(h) }
        }
        if !again {
            break;
        }
    }
    kept.sort_unstable();
    kept.dedup();
    println!(
        "holes: {} hole(s) in the ground ({:.0} m²) closed by {} on-demand map(s) kept for that{}; {} hole(s) ({:.0} m²) that no map closes ({:.1} s)",
        closed.len(),
        closed.iter().map(|h| h.area).sum::<f32>(),
        kept.len(),
        if kept.is_empty() { String::new() } else { format!(" ({})", kept.join(", ")) },
        open.len(),
        open.iter().map(|h| h.area).sum::<f32>(),
        t.elapsed().as_secs_f64()
    );
    for h in open.iter().filter(|h| h.area >= 25.0) {
        println!("  hole left: {:.0} m² near x {:.0}, y {:.0}", h.area, h.centre[0], h.centre[1]);
    }
    closed.extend(open);
    Ok(closed)
}

/// The blocks a round of [`fill`] drew, for the next: what each shows ([`Seen`], None when nothing
/// is empty in it), and those drawn with the files left out too (the blocks those meet).
#[derive(Default)]
struct Cache {
    seen: HashMap<BlockId, Option<Seen>>,
    with_files: HashSet<BlockId>,
}

/// The holes of the map as it is drawn, each with the files left out that close it. `files`: the
/// placement files that may be drawn, read once ([`read_files`]); `cache`: the blocks drawn by the
/// rounds before, still true (the caller forgets those a file kept since meets).
#[allow(clippy::too_many_arguments)]
fn find(
    game: &Path,
    keys: &GtaKeys,
    world: &World,
    zone: Option<[f32; 4]>,
    threads: usize,
    budget: usize,
    files: &[(u32, FileEntities)],
    generic: &crate::render::assets::Generic,
    cache: &mut Cache,
) -> Vec<Hole> {
    let out = places_of(files, &|h| world.groups.left_out.contains(&h));
    let in_zone = |(bx, by): BlockId| {
        zone.is_none_or(|z| {
            (bx + 1) as f32 * BLOCK > z[0]
                && (bx as f32) * BLOCK < z[2]
                && (by + 1) as f32 * BLOCK > z[1]
                && (by as f32) * BLOCK < z[3]
        })
    };
    let mut wanted: Vec<BlockId> = out.blocks.keys().copied().filter(|b| in_zone(*b)).collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    wanted.sort_unstable();
    let kept = places_of(files, &|h| !world.groups.left_out.contains(&h));
    let names: HashMap<u32, &str> = world.groups.files.iter().map(|f| (f.hash, f.name.as_str())).collect();

    // a block drawn with what is kept: None when nothing is empty in it
    let see = |assets: &mut Assets, b: BlockId, with_files: bool| -> Option<Seen> {
        let entities: Vec<&Entity> = kept
            .blocks
            .get(&b)
            .map(|ids| ids.iter().map(|&i| &kept.entities[i as usize]).collect())
            .unwrap_or_default();
        let covered = if entities.is_empty() { vec![false; PX * PX] } else { coverage(world, &entities, assets, b) };
        if covered.iter().all(|c| *c) {
            return None;
        }
        let (label, sizes) = regions(&covered);
        let mut files = Vec::new();
        if with_files {
            let mut by_file: BTreeMap<u32, Vec<&Entity>> = BTreeMap::new();
            for &i in out.blocks.get(&b).map(Vec::as_slice).unwrap_or(&[]) {
                by_file.entry(out.owner[i as usize]).or_default().push(&out.entities[i as usize]);
            }
            for (file, entities) in by_file {
                let mask = coverage(world, &entities, assets, b);
                if mask.iter().zip(&covered).any(|(m, c)| *m && !*c) {
                    files.push((file, mask));
                }
            }
        }
        Some(Seen { label, sizes, files })
    };

    // the blocks where a file left out has entities, on every thread (those a round before drew
    // with them, and that nothing kept since meets, are as they were)
    let todo: Vec<BlockId> = wanted.iter().copied().filter(|b| !cache.with_files.contains(b)).collect();
    let drawn: Mutex<Vec<(BlockId, Option<Seen>)>> = Mutex::new(Vec::new());
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads.min(todo.len().max(1)) {
            s.spawn(|| {
                let mut assets = Assets::new(game, keys, world, generic);
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&b) = todo.get(i) else { break };
                    let block = see(&mut assets, b, true);
                    drawn.lock().unwrap().push((b, block));
                    assets.trim(budget);
                }
            });
        }
    });
    for (b, block) in drawn.into_inner().unwrap() {
        cache.seen.insert(b, block);
        cache.with_files.insert(b);
    }
    let seen = &mut cache.seen;

    // the regions joined across the blocks' edges; the blocks around are drawn as they are met
    let mut assets = Assets::new(game, keys, world, generic);
    let mut done: HashSet<(BlockId, u32)> = HashSet::new();
    let mut holes = Vec::new();
    for &start in &wanted {
        let count = seen.get(&start).and_then(|s| s.as_ref()).map(|s| s.sizes.len()).unwrap_or(0);
        for region in 1..=count as u32 {
            if !done.insert((start, region)) {
                continue;
            }
            // every part of this region, block after block
            let mut parts: Vec<(BlockId, u32)> = vec![(start, region)];
            let mut blocks: HashSet<BlockId> = HashSet::from([start]);
            let mut open = false;
            let mut at = 0;
            while at < parts.len() {
                let (b, r) = parts[at];
                at += 1;
                // the pixels of this part along each edge, and the pixel facing each in the block beyond
                let mut facing: Vec<(BlockId, usize)> = Vec::new();
                {
                    let part = seen[&b].as_ref().unwrap();
                    for k in 0..PX {
                        if part.label[k] == r {
                            facing.push(((b.0, b.1 + 1), (PX - 1) * PX + k)); // north
                        }
                        if part.label[(PX - 1) * PX + k] == r {
                            facing.push(((b.0, b.1 - 1), k)); // south
                        }
                        if part.label[k * PX] == r {
                            facing.push(((b.0 - 1, b.1), k * PX + PX - 1)); // west
                        }
                        if part.label[k * PX + PX - 1] == r {
                            facing.push(((b.0 + 1, b.1), k * PX)); // east
                        }
                    }
                }
                for (n, pixel) in facing {
                    // nothing at all is drawn there: the region is the outside
                    if !kept.blocks.contains_key(&n) {
                        open = true;
                        continue;
                    }
                    if let std::collections::hash_map::Entry::Vacant(e) = seen.entry(n) {
                        let block = see(&mut assets, n, false);
                        e.insert(block);
                        assets.trim(budget);
                    }
                    let Some(beyond) = seen[&n].as_ref() else { continue };
                    let r2 = beyond.label[pixel];
                    if r2 != 0 && done.insert((n, r2)) {
                        parts.push((n, r2));
                        blocks.insert(n);
                    }
                }
                if blocks.len() > MAX_BLOCKS {
                    open = true;
                }
            }
            if open {
                continue;
            }
            let pixels: usize = parts.iter().map(|(b, r)| seen[b].as_ref().unwrap().sizes[*r as usize - 1]).sum();
            let area = pixels as f32 * METRES * METRES;
            if area < MIN_AREA {
                continue;
            }
            // its middle, and what each file left out covers of it
            let (mut sx, mut sy) = (0f64, 0f64);
            let mut cover: BTreeMap<u32, usize> = BTreeMap::new();
            for (b, r) in &parts {
                let part = seen[b].as_ref().unwrap();
                for (i, l) in part.label.iter().enumerate() {
                    if l == r {
                        sx += (b.0 as f64) * BLOCK as f64 + ((i % PX) as f64 + 0.5) * METRES as f64;
                        sy += (b.1 as f64 + 1.0) * BLOCK as f64 - ((i / PX) as f64 + 0.5) * METRES as f64;
                    }
                }
                for (file, mask) in &part.files {
                    let n = part.label.iter().zip(mask).filter(|(l, m)| *l == r && **m).count();
                    if n > 0 {
                        *cover.entry(*file).or_default() += n;
                    }
                }
            }
            let centre = [(sx / pixels as f64) as f32, (sy / pixels as f64) as f32];
            let need = (CLOSED * pixels as f32).ceil() as usize;
            let name = |file: u32| names.get(&file).copied().unwrap_or("?").to_string();
            // the files that close it, among those ranked `limit` at most: one alone, the original
            // state of the place before the others, else several together, the widest first
            // (each must bring something)
            let close = |limit: u8| -> Vec<String> {
                let within: Vec<(u32, usize)> =
                    cover.iter().map(|(f, n)| (*f, *n)).filter(|(f, _)| rank(&name(*f)) <= limit).collect();
                let mut alone: Vec<(u32, usize)> = within.iter().copied().filter(|(_, n)| *n >= need).collect();
                alone.sort_by(|a, b| {
                    (rank(&name(a.0)), std::cmp::Reverse(a.1), name(a.0)).cmp(&(
                        rank(&name(b.0)),
                        std::cmp::Reverse(b.1),
                        name(b.0),
                    ))
                });
                if let Some((file, _)) = alone.first() {
                    return vec![name(*file)];
                }
                let mut order = within;
                order.sort_by(|a, b| {
                    (std::cmp::Reverse(a.1), rank(&name(a.0)), name(a.0)).cmp(&(
                        std::cmp::Reverse(b.1),
                        rank(&name(b.0)),
                        name(b.0),
                    ))
                });
                let mut filled: HashSet<(BlockId, usize)> = HashSet::new();
                let mut together = Vec::new();
                for (file, _) in order {
                    let before = filled.len();
                    for (b, r) in &parts {
                        let part = seen[b].as_ref().unwrap();
                        if let Some((_, mask)) = part.files.iter().find(|(f, _)| *f == file) {
                            for (i, l) in part.label.iter().enumerate() {
                                if l == r && mask[i] {
                                    filled.insert((*b, i));
                                }
                            }
                        }
                    }
                    if filled.len() > before + pixels / 100 {
                        together.push(name(file));
                    }
                    if filled.len() >= need {
                        return together;
                    }
                }
                Vec::new()
            };
            // what a mission leaves behind (burnt, destroyed) only when nothing else closes the hole
            let mut closed_by = close(WRECKED - 1);
            if closed_by.is_empty() {
                closed_by = close(WRECKED);
            }
            holes.push(Hole { centre, area, closed_by });
        }
    }
    holes.sort_by(|a, b| b.area.total_cmp(&a.area));
    holes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_of_a_block() {
        // everything drawn but a square of 3 × 2 pixels and one pixel apart
        let mut covered = vec![true; PX * PX];
        for y in 10..12 {
            for x in 20..23 {
                covered[y * PX + x] = false;
            }
        }
        covered[100 * PX + 100] = false;
        let (label, sizes) = regions(&covered);
        assert_eq!(sizes, [6, 1]);
        assert_eq!(label[10 * PX + 20], 1);
        assert_eq!(label[11 * PX + 22], 1);
        assert_eq!(label[100 * PX + 100], 2);
        assert_eq!(label[0], 0);
        // nothing drawn: one region, the whole block
        let (_, sizes) = regions(&vec![false; PX * PX]);
        assert_eq!(sizes, [PX * PX]);
    }

    #[test]
    fn original_state_first() {
        assert_eq!(rank("ch1_02_mansion_original"), 0);
        assert_eq!(rank("bnkheist_apt_norm"), 1);
        assert_eq!(rank("bnkheist_apt_dest"), WRECKED);
        assert_eq!(rank("farm_burnt"), WRECKED);
        assert_eq!(rank("bh1_47_joshhse_unburnt"), 1);
        assert_eq!(rank("farm"), 2);
        assert!(rank("hei_ch1_roads_original") < rank("m25_2_mansion"));
    }
}
