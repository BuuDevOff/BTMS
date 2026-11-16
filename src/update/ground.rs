//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The ground a server resource changes on the map ([`Ground`]), in 128 m blocks (those of the
//! tiles), and the sets of blocks the update draws (rounded, joined, widened: [`Blocks`]).
//!
//! The ground of a resource ([`own`], then [`placed_elsewhere`]):
//!
//! - its placements: every entity the tiles draw of the placement files it streams (the winning
//!   copy of each name), boxed by its archetype (`world_box`), its interiors' entities included
//!   (`fivem::shown`); and the entities of the game's file it takes the place of, which go;
//! - the files its client scripts remove or load by name (`RemoveIpl`, `RequestIpl`);
//! - wherever, in the map, the models and textures it streams are placed: an archetype whose model
//!   (`.ydr`, `.yft`, `.ydd`) or texture dictionary (`.ytd`, with its parents) bears the name of a
//!   streamed file, or that its archetype files (`.ytyp`) define.
//!
//! A file of the resource that is protected (`FXAP`) or does not read leaves it `readable: false`,
//! with the reason; what reads still counts.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use rage_rpf::GtaKeys;

use crate::formats::resource::{Resource, joaat};
use crate::formats::ymap::{ASSET_DRAWABLE, ASSET_DRAWABLE_DICTIONARY, ASSET_FRAGMENT, Entity, parse_ymap, parse_ytyp};
use crate::world::World;
use crate::world::fivem::Server;

/// Side of a block, in metres (a zoom 7 tile).
pub const BLOCK: f32 = 128.0;
/// A box wider than this many blocks is not counted (a sky dome, a broken placement): the tiles
/// leave it out too.
const HUGE: i32 = 64;

pub type BlockId = (i32, i32);
pub type Blocks = BTreeSet<BlockId>;

/// The blocks a box (x, y in metres) meets, as the tiles count them; None for a box that is not
/// finite or is too wide ([`HUGE`]).
pub fn box_blocks(lo: [f32; 3], hi: [f32; 3]) -> Option<[i32; 4]> {
    if !(lo[0].is_finite() && lo[1].is_finite() && hi[0].is_finite() && hi[1].is_finite()) {
        return None;
    }
    let (bx0, bx1) = ((lo[0] / BLOCK).floor() as i32, (hi[0] / BLOCK).floor() as i32);
    let (by0, by1) = ((lo[1] / BLOCK).floor() as i32, (hi[1] / BLOCK).floor() as i32);
    ((bx1 - bx0) <= HUGE && (by1 - by0) <= HUGE).then_some([bx0, by0, bx1, by1])
}

/// The blocks of a zone in metres (x1, y1, x2, y2), rounded outwards: every block it meets.
pub fn zone_blocks(z: [f32; 4]) -> Blocks {
    let mut out = Blocks::new();
    for by in (z[1] / BLOCK).floor() as i32..=((z[3] / BLOCK).ceil() as i32 - 1).max((z[1] / BLOCK).floor() as i32) {
        for bx in (z[0] / BLOCK).floor() as i32..=((z[2] / BLOCK).ceil() as i32 - 1).max((z[0] / BLOCK).floor() as i32)
        {
            out.insert((bx, by));
        }
    }
    out
}

/// Whether a block meets a zone (every block without one): the rule of the tiles.
pub fn in_zone(zone: Option<[f32; 4]>, (bx, by): BlockId) -> bool {
    zone.is_none_or(|z| {
        (bx + 1) as f32 * BLOCK > z[0]
            && (bx as f32) * BLOCK < z[2]
            && (by + 1) as f32 * BLOCK > z[1]
            && (by as f32) * BLOCK < z[3]
    })
}

/// `blocks` and every block within `margin` blocks of one (the 8 around for a margin of 1).
pub fn widen(blocks: &Blocks, margin: i32) -> Blocks {
    let mut out = Blocks::new();
    for &(bx, by) in blocks {
        for dy in -margin..=margin {
            for dx in -margin..=margin {
                out.insert((bx + dx, by + dy));
            }
        }
    }
    out
}

/// The blocks as rectangles (bx1, by1, bx2, by2, both ends included) that cover exactly them:
/// the runs of each row, joined with the identical runs of the rows above.
pub fn rectangles(blocks: &Blocks) -> Vec<[i32; 4]> {
    // the runs of each row (row after row, west to east)
    let mut rows: Vec<(i32, i32)> = blocks.iter().map(|&(bx, by)| (by, bx)).collect();
    rows.sort_unstable();
    let mut runs: Vec<(i32, i32, i32)> = Vec::new();
    for (by, bx) in rows {
        match runs.last_mut() {
            Some((y, _, x1)) if *y == by && *x1 + 1 == bx => *x1 = bx,
            _ => runs.push((by, bx, bx)),
        }
    }
    // a run continues a rectangle that ends on the row below with the same columns
    let mut out: Vec<[i32; 4]> = Vec::new();
    let mut open: HashMap<(i32, i32), usize> = HashMap::new();
    for (y, x0, x1) in runs {
        match open.get(&(x0, x1)).copied().filter(|&i| out[i][3] + 1 == y) {
            Some(i) => out[i][3] = y,
            None => {
                open.insert((x0, x1), out.len());
                out.push([x0, y, x1, y]);
            }
        }
    }
    out
}

/// The blocks of rectangles ([`rectangles`]).
pub fn from_rectangles(rects: &[[i32; 4]]) -> Blocks {
    let mut out = Blocks::new();
    for r in rects {
        for by in r[1]..=r[3] {
            for bx in r[0]..=r[2] {
                out.insert((bx, by));
            }
        }
    }
    out
}

/// A rectangle of blocks in metres (x1, y1, x2, y2).
pub fn metres(r: [i32; 4]) -> [f32; 4] {
    [r[0] as f32 * BLOCK, r[1] as f32 * BLOCK, (r[2] + 1) as f32 * BLOCK, (r[3] + 1) as f32 * BLOCK]
}

/// The rectangle around blocks, in metres; None when there are none.
pub fn around(blocks: &Blocks) -> Option<[f32; 4]> {
    let first = blocks.iter().next()?;
    let mut r = [first.0, first.1, first.0, first.1];
    for &(x, y) in blocks {
        r = [r[0].min(x), r[1].min(y), r[2].max(x), r[3].max(y)];
    }
    Some(metres(r))
}

/// The ground a resource changes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ground {
    pub blocks: Blocks,
    /// The boxes it is made of, joined (x1, y1, x2, y2 in metres).
    pub zone: Option<[f32; 4]>,
    /// Map files that are protected or do not read, and the reason told.
    pub unreadable: usize,
    pub reason: Option<String>,
}

impl Ground {
    /// Adds a box (in metres): its blocks and the zone.
    pub fn add(&mut self, lo: [f32; 3], hi: [f32; 3]) {
        let Some([bx0, by0, bx1, by1]) = box_blocks(lo, hi) else { return };
        for by in by0..=by1 {
            for bx in bx0..=bx1 {
                self.blocks.insert((bx, by));
            }
        }
        let z = self.zone.get_or_insert([lo[0], lo[1], hi[0], hi[1]]);
        *z = [z[0].min(lo[0]), z[1].min(lo[1]), z[2].max(hi[0]), z[3].max(hi[1])];
    }
}

/// The archetypes of the world by the files that make them up: model (`ydr`, `yft`), model
/// dictionary (`ydd`), texture dictionary (`ytd`, any of its parents too).
pub struct Names {
    by_file: HashMap<(&'static str, u32), Vec<u32>>,
}

impl Names {
    pub fn new(world: &World) -> Names {
        let mut by_file: HashMap<(&'static str, u32), Vec<u32>> = HashMap::new();
        for (name, (a, _)) in &world.archetypes {
            let model = match a.asset_type {
                ASSET_DRAWABLE => Some(("ydr", a.asset_name)),
                ASSET_FRAGMENT => Some(("yft", a.asset_name)),
                ASSET_DRAWABLE_DICTIONARY => Some(("ydd", a.drawable_dictionary)),
                _ => None,
            };
            if let Some(key) = model {
                by_file.entry(key).or_default().push(*name);
            }
            // the dictionary and its parents, as the drawing looks for a texture
            let (mut dict, mut seen) = (a.texture_dictionary, HashSet::new());
            while dict != 0 && seen.insert(dict) {
                by_file.entry(("ytd", dict)).or_default().push(*name);
                dict = world.txd_parents.get(&dict).copied().unwrap_or(0);
            }
        }
        Names { by_file }
    }

    /// The archetypes made with the file `hash`.`ext`.
    pub fn archetypes(&self, ext: &str, hash: u32) -> &[u32] {
        let ext: &'static str = match ext {
            "ydr" => "ydr",
            "yft" => "yft",
            "ydd" => "ydd",
            "ytd" => "ytd",
            _ => return &[],
        };
        self.by_file.get(&(ext, hash)).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// The entities of a placement file the tiles draw (as `tiles::placements` takes them).
fn drawn(map: crate::formats::ymap::Ymap) -> impl Iterator<Item = Entity> {
    map.entities.into_iter().filter(|e| !e.interior && matches!(e.lod_level, 0 | 5))
}

/// Whether a file of the server's (`resource:…` path, or a path on disk) is in the folder of
/// resource `dir`.
fn under(path: &str, dir: &Path) -> bool {
    let root = format!("{}/", dir.to_string_lossy().replace('\\', "/").to_lowercase().trim_end_matches('/'));
    let lower = path
        .strip_prefix(crate::world::fivem::PREFIX)
        .and_then(|p| p.split_once(':'))
        .map(|(_, p)| p.to_string())
        .unwrap_or_else(|| path.replace('\\', "/").to_lowercase());
    lower.starts_with(&root)
}

/// The ground of resource `r` of `server` that its own files give (its placements, the game's files
/// they replace, the files its scripts remove or load), and the archetypes whose placements
/// anywhere count too ([`placed_elsewhere`]): those made with a model or textures it streams, or
/// that its archetype files define.
pub fn own(
    game: &Path,
    keys: &GtaKeys,
    world: &World,
    server: &Server,
    names: &Names,
    r: usize,
) -> (Ground, HashSet<u32>) {
    let mut ground = Ground::default();
    let mut affected: HashSet<u32> = HashSet::new();
    let dir = &server.resources[r].dir;
    let mut unreadable: Vec<String> = Vec::new();
    for f in server.streamed.iter().filter(|f| f.resource == r) {
        let name = f.path.rsplit('/').next().unwrap_or(&f.path);
        let Some((stem, ext)) = name.rsplit_once('.') else { continue };
        let hash = joaat(stem);
        // a copy another resource wins over is not on the map
        if world.index.copies(hash, ext).first() != Some(&f.path) {
            continue;
        }
        match ext {
            "ymap" => {
                let map = match crate::cli::archive::read_file(game, &f.path, keys)
                    .and_then(|b| Resource::parse(&b))
                    .and_then(|r| parse_ymap(&r))
                {
                    Ok(m) => m,
                    Err(_) => {
                        unreadable.push(name.to_string());
                        continue;
                    }
                };
                // its entities as the tiles show them (those removed left out, its interiors' added)
                for e in drawn(crate::world::fivem::shown(&world.server, &f.path, map)) {
                    if let Some((a, _)) = world.archetypes.get(&e.archetype) {
                        let (lo, hi) = crate::formats::ymap::world_box(&e, a, true);
                        ground.add(lo, hi);
                    }
                }
                // the game's file of that name, whose entities go
                if let Some(copy) =
                    world.index.copies(hash, "ymap").iter().find(|p| !crate::world::fivem::is_streamed(p))
                    && let Ok(m) = crate::cli::archive::read_file(game, copy, keys)
                        .and_then(|b| Resource::parse(&b))
                        .and_then(|r| parse_ymap(&r))
                {
                    for e in drawn(m) {
                        if let Some((a, _)) = world.archetypes.get(&e.archetype) {
                            let (lo, hi) = crate::formats::ymap::world_box(&e, a, true);
                            ground.add(lo, hi);
                        }
                    }
                }
            }
            "ytyp" => match crate::cli::archive::read_file(game, &f.path, keys)
                .and_then(|b| Resource::parse(&b))
                .and_then(|r| parse_ytyp(&r))
            {
                Ok(list) => affected.extend(list.iter().map(|a| a.name)),
                Err(_) => unreadable.push(name.to_string()),
            },
            "ydr" | "yft" | "ydd" | "ytd" => affected.extend(names.archetypes(ext, hash)),
            _ => {}
        }
    }
    // what its client scripts remove or load by name
    for c in server.calls.iter().filter(|c| c.file.starts_with(dir)) {
        let Some(name) = c.name.as_deref() else { continue };
        let hash = joaat(name);
        for m in world.ymaps.iter().filter(|m| m.hash == hash) {
            ground.add(m.entities_min, m.entities_max);
        }
    }
    // its map files left out: protected, not a resource, unreadable (its scripts and other files
    // left out do not count here)
    let map_file = |file: &Path| {
        file.starts_with(dir)
            && file
                .extension()
                .is_some_and(|e| crate::world::fivem::READ.contains(&e.to_string_lossy().to_lowercase().as_str()))
    };
    let mut protected = 0;
    for (file, why) in server.skipped.iter().filter(|(f, _)| map_file(f)) {
        unreadable.push(file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        protected += why.contains("FXAP") as usize;
    }
    for (path, _) in &world.server.failures {
        if under(path, dir) {
            let name = path.rsplit('/').next().unwrap_or(path).to_string();
            if !unreadable.contains(&name) {
                unreadable.push(name);
            }
        }
    }
    ground.unreadable = unreadable.len();
    if !unreadable.is_empty() {
        ground.reason = Some(format!(
            "{} unreadable map file(s), {protected} of them protected by FiveM escrow (FXAP): what they place or draw is not known{}",
            unreadable.len(),
            if protected == unreadable.len() { " (the tiles do not draw them either)" } else { "" }
        ));
    }
    (ground, affected)
}

/// Adds to `ground` the blocks where the entities of `entities` (those the tiles draw, the whole
/// map) are of an archetype of `affected`.
pub fn placed_elsewhere(ground: &mut Ground, affected: &HashSet<u32>, world: &World, entities: &[Entity]) {
    if affected.is_empty() {
        return;
    }
    for e in entities.iter().filter(|e| affected.contains(&e.archetype)) {
        if let Some((a, _)) = world.archetypes.get(&e.archetype) {
            let (lo, hi) = crate::formats::ymap::world_box(e, a, true);
            ground.add(lo, hi);
        }
    }
}

/// The reason told for a ground without any block.
pub fn finish(ground: &mut Ground) {
    if ground.blocks.is_empty() {
        ground.zone = None;
        if ground.reason.is_none() {
            ground.reason = Some("places nothing the tiles draw".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(list: &[(i32, i32)]) -> Blocks {
        list.iter().copied().collect()
    }

    #[test]
    fn zones_are_rounded_outwards_to_whole_blocks() {
        // exactly on the edges: the blocks inside only
        assert_eq!(zone_blocks([0.0, 0.0, 256.0, 128.0]), set(&[(0, 0), (1, 0)]));
        // a metre past an edge takes the next block, on every side
        assert_eq!(zone_blocks([-1.0, 127.0, 129.0, 129.0]), set(&[(-1, 0), (0, 0), (1, 0), (-1, 1), (0, 1), (1, 1)]));
        // a point is its block
        assert_eq!(zone_blocks([10.0, -10.0, 10.0, -10.0]), set(&[(0, -1)]));
        // the rule of the tiles
        for b in zone_blocks([-300.0, 50.0, 1.0, 300.0]) {
            assert!(in_zone(Some([-300.0, 50.0, 1.0, 300.0]), b));
        }
        assert!(!in_zone(Some([0.0, 0.0, 256.0, 128.0]), (2, 0)) && in_zone(None, (99, -99)));
    }

    #[test]
    fn boxes_give_the_blocks_the_tiles_give() {
        assert_eq!(box_blocks([-1.0, 0.0, 0.0], [1.0, 127.9, 5.0]), Some([-1, 0, 0, 0]));
        assert_eq!(box_blocks([0.0, 0.0, 0.0], [f32::INFINITY, 1.0, 0.0]), None);
        assert_eq!(box_blocks([0.0, 0.0, 0.0], [65.0 * 128.0, 1.0, 0.0]), None);
        assert_eq!(box_blocks([0.0, 0.0, 0.0], [64.5 * 128.0, 1.0, 0.0]), Some([0, 0, 64, 0]));
        let mut g = Ground::default();
        g.add([10.0, 10.0, 0.0], [20.0, 140.0, 1.0]);
        g.add([-300.0, 5.0, 0.0], [-290.0, 6.0, 1.0]);
        g.add([0.0, 0.0, 0.0], [f32::NAN, 1.0, 1.0]);
        assert_eq!(g.blocks, set(&[(0, 0), (0, 1), (-3, 0)]));
        assert_eq!(g.zone, Some([-300.0, 5.0, 20.0, 140.0]));
        let mut empty = Ground::default();
        finish(&mut empty);
        assert_eq!((empty.zone, empty.reason.as_deref()), (None, Some("places nothing the tiles draw")));
    }

    #[test]
    fn rectangles_cover_the_blocks_exactly() {
        let mut seed = 0x1234_5678u64;
        let mut random = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..200 {
            let mut blocks = Blocks::new();
            for _ in 0..(random() % 60) {
                blocks.insert(((random() % 12) as i32 - 6, (random() % 9) as i32 - 4));
            }
            let rects = rectangles(&blocks);
            assert_eq!(from_rectangles(&rects), blocks);
            // no block twice
            let total: i32 = rects.iter().map(|r| (r[2] - r[0] + 1) * (r[3] - r[1] + 1)).sum();
            assert_eq!(total as usize, blocks.len());
        }
        // a square of 3 × 3 is one rectangle, an L two
        let square = from_rectangles(&[[2, -1, 4, 1]]);
        assert_eq!(rectangles(&square), [[2, -1, 4, 1]]);
        assert_eq!(rectangles(&set(&[(0, 0), (1, 0), (0, 1)])), [[0, 0, 1, 0], [0, 1, 0, 1]]);
        assert_eq!(around(&square), Some([256.0, -128.0, 640.0, 256.0]));
        assert_eq!(around(&Blocks::new()), None);
    }

    #[test]
    fn the_margin_takes_the_blocks_around() {
        let w = widen(&set(&[(0, 0), (5, 5)]), 1);
        assert_eq!(w.len(), 18);
        assert!(w.contains(&(-1, -1)) && w.contains(&(1, 1)) && w.contains(&(6, 4)) && !w.contains(&(2, 2)));
        // the union of two grounds side by side, widened, is one rectangle
        let both: Blocks = set(&[(0, 0), (1, 0)]).union(&set(&[(2, 0)])).copied().collect();
        assert_eq!(rectangles(&widen(&both, 1)), [[-1, -1, 3, 1]]);
        assert_eq!(widen(&both, 0), both);
    }

    /// The archetypes a streamed model or texture dictionary changes, and where they are placed.
    #[test]
    fn what_a_model_or_texture_changes_is_where_it_is_placed() {
        use crate::formats::ymap::Archetype;
        let archetype = |name: &str, asset_type: u32, dictionary: &str, txd: &str| Archetype {
            name: joaat(name),
            texture_dictionary: if txd.is_empty() { 0 } else { joaat(txd) },
            drawable_dictionary: if dictionary.is_empty() { 0 } else { joaat(dictionary) },
            asset_name: joaat(name),
            asset_type,
            bb_min: [-1.0, -1.0, 0.0],
            bb_max: [1.0, 1.0, 3.0],
        };
        let list = [
            archetype("a_lamp", ASSET_DRAWABLE, "", "child_txd"),
            archetype("b_frag", ASSET_FRAGMENT, "", "other_txd"),
            archetype("c_in_dict", ASSET_DRAWABLE_DICTIONARY, "dict", ""),
        ];
        let world = World {
            index: crate::game::index::Index { files: HashMap::new(), dlcs: Vec::new(), base_first: false },
            archetypes: list.iter().map(|a| (a.name, (a.clone(), "x.ytyp".to_string()))).collect(),
            ymaps: Vec::new(),
            replaced: HashSet::new(),
            txd_parents: HashMap::from([
                (joaat("child_txd"), joaat("parent_txd")),
                (joaat("parent_txd"), joaat("child_txd")),
            ]),
            server: crate::world::fivem::Added::default(),
            manifests: Vec::new(),
            groups: crate::world::groups::Groups {
                left_out: HashSet::new(),
                files: Vec::new(),
                groups: HashMap::new(),
                islands: Vec::new(),
                list: Vec::new(),
                evidence: Default::default(),
            },
        };
        let names = Names::new(&world);
        let a = |ext: &str, n: &str| names.archetypes(ext, joaat(n)).to_vec();
        // a model by its kind of file; a texture dictionary, its parents too (a loop of parents ends)
        assert_eq!(a("ydr", "a_lamp"), [joaat("a_lamp")]);
        assert_eq!(a("yft", "b_frag"), [joaat("b_frag")]);
        assert!(a("ydr", "b_frag").is_empty() && a("ydr", "c_in_dict").is_empty());
        assert_eq!(a("ydd", "dict"), [joaat("c_in_dict")]);
        assert_eq!((a("ytd", "child_txd"), a("ytd", "parent_txd")), (vec![joaat("a_lamp")], vec![joaat("a_lamp")]));
        assert!(a("ymap", "a_lamp").is_empty());
        // its placements in the map: the blocks of each entity's box
        let entity = |name: &str, position: [f32; 3]| Entity {
            archetype: joaat(name),
            position,
            rotation: [0.0, 0.0, 0.0, 1.0],
            scale_xy: 1.0,
            scale_z: 1.0,
            tint: 0,
            lod_level: 0,
            interior: false,
        };
        let placed = [
            entity("a_lamp", [1000.0, 1000.0, 0.0]),
            entity("b_frag", [-500.0, 20.0, 0.0]),
            entity("a_lamp", [-0.5, 300.0, 0.0]),
        ];
        let mut g = Ground::default();
        placed_elsewhere(&mut g, &a("ytd", "parent_txd").into_iter().collect(), &world, &placed);
        assert_eq!(g.blocks, set(&[(7, 7), (-1, 2), (0, 2)]));
        assert_eq!(g.zone, Some([-1.5, 299.0, 1001.0, 1001.0]));
        let mut none = Ground::default();
        placed_elsewhere(&mut none, &HashSet::new(), &world, &placed);
        assert!(none.blocks.is_empty());
    }

    #[test]
    fn files_of_a_resource_are_found_by_their_path() {
        let dir = Path::new("C:\\Srv\\[My-Island]\\island-map");
        assert!(under("resource:000012:c:/srv/[my-island]/island-map/stream/a.ymap", dir));
        assert!(!under("resource:000012:c:/srv/[my-island]/island-map-2/stream/a.ymap", dir));
        assert!(under("C:\\Srv\\[My-Island]\\island-map\\data\\gtxd.meta", dir));
        assert!(!under("x64a.rpf/levels/a.ymap", dir));
    }
}
