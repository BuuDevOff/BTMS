//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! `probe <bx> <by> [--image <file.png>]` (and `--renderer auto|gpu|cpu`, taken out by `cli`):
//! what a block of 128 m is made of, to understand a fault seen on the map (a hole, a black or
//! grey patch). It prints:
//!
//! 1. every model placed in the block as the tiles gather them (`tiles::placements`): the file of
//!    the model, the placement file, the entity's detail level, how many;
//! 2. what is not drawn, and why: entities the tiles leave out (detail levels other than HD,
//!    interiors, entities a server resource removes, unknown archetypes, files a multiplayer pack
//!    replaces, maps loaded on request that are left out), models that cannot be drawn (file not
//!    found, protected, unreadable), pieces of models left out (a render mask without the main
//!    pass: the shadow of a tree, a reflection; water and foam);
//! 3. the block drawn in memory as the tiles draw it before the light, the holes and the water,
//!    at 0.25 m per pixel, by the card or the processor as `--renderer` says, and its pixels that
//!    look wrong: empty, near black, plain grey (a piece without a texture). Near black and grey pixels
//!    are put on the models that paint them (each model drawn alone, its depths compared with the
//!    block's), with their suspicious pieces (texture coordinates that are not numbers, textures not
//!    found); empty pixels on what would cover them (a map loaded on request left out, a lower
//!    detail level, the box of a model that is not drawn, the water of the tiles).
//!
//! Nothing is written but the picture `--image` asks for. The search of `tiles` for the holes
//! that a map loaded on request closes (`fill_holes`) is not made here: it may draw
//! hundreds of blocks around a hole.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use image::ImageEncoder;
use rage_rpf::GtaKeys;

use crate::formats::resource::{Resource, joaat};
use crate::formats::ydr::Drawable;
use crate::formats::ymap::{
    ASSET_DRAWABLE, ASSET_DRAWABLE_DICTIONARY, ASSET_FRAGMENT, Archetype, Entity, LOD_NAMES, parse_ymap, world_box,
};
use crate::render::assets::{Assets, Draw, Generic, generic_textures, paint};
use crate::render::raster::{Projection, Raster};
use crate::world::World;
use crate::world::groups::Kept;
use crate::world::resources::{label, names};

/// A block: one zoom 7 tile, as the tiles draw them.
const BLOCK: f32 = 128.0;
/// Pixels along the block: 0.25 m per pixel (the tiles draw it at 6.25 cm).
const PX: usize = 512;
const METRES: f32 = BLOCK / PX as f32;
/// Height the block is seen from (that of the tiles).
const TOP: f32 = 3000.0;
/// Where a resource's root structure starts.
const ROOT: u64 = 0x5000_0000;
/// A pixel is near black below this sum of its three channels (out of 765).
const NEAR_BLACK: u32 = 24;
/// Plain grey: three channels within 1 of each other in this range, around 128 (a piece without a
/// texture is drawn with half its tint, 0.5 × 255 + 0.5).
const GREY: std::ops::RangeInclusive<u8> = 126..=130;
/// Two depths closer than this (m) are the same surface.
const SAME: f32 = 0.02;
/// Bytes of models and textures kept from one drawing to the next.
const BUDGET: usize = 2_000_000_000;
/// Lines of a list printed at most.
const LINES: usize = 40;
/// Pieces of one model printed at most.
const PIECES: usize = 12;
/// An empty region smaller than this many pixels (1 m²) is not listed on its own.
const SMALL_REGION: usize = 16;

const USAGE: &str = "probe <bx> <by> [--image <file.png>] [--renderer auto|gpu|cpu]: the 128 m block from x = bx·128 to (bx+1)·128 and from y = by·128 to (by+1)·128 (the numbers the interface shows under \"block\")";

/// What `probe` was asked.
#[derive(Debug, PartialEq)]
struct Asked {
    bx: i32,
    by: i32,
    image: Option<PathBuf>,
}

fn parse(args: &[String]) -> Result<Asked> {
    let mut numbers = Vec::new();
    let mut image = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--image" {
            let file = args
                .get(i + 1)
                .filter(|f| !f.starts_with("--"))
                .with_context(|| format!("--image without a file; {USAGE}"))?;
            image = Some(PathBuf::from(file));
            i += 2;
            continue;
        }
        if a.starts_with("--") {
            bail!("{a}: unknown probe option; {USAGE}");
        }
        // "16 40", "16,40" or "16, 40"
        for part in a.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            numbers.push(part.parse::<i32>().with_context(|| format!("\"{part}\" is not a block number; {USAGE}"))?);
        }
        i += 1;
    }
    let [bx, by] = numbers[..] else { bail!("{USAGE}") };
    if bx.abs() > 1000 || by.abs() > 1000 {
        bail!("block {bx},{by}: outside the world ({USAGE})");
    }
    Ok(Asked { bx, by, image })
}

/// Why an entity of the block is not drawn by the tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Skip {
    /// Its placement file is replaced by the copy of a multiplayer pack.
    Replaced,
    /// Its placement file is a map loaded on request that is left out.
    LeftOut,
    Interior,
    /// A server resource removes it.
    Removed,
    /// Its detail level is not HD (nor orphan HD).
    Level(u32),
    UnknownArchetype,
    /// Its box in the world is not a number.
    OddBox,
    /// Its box spans more than 64 blocks.
    HugeBox,
}

impl Skip {
    fn says(self) -> String {
        match self {
            Skip::Replaced => "file replaced by the copy of a multiplayer pack (the copy is what is drawn)".into(),
            Skip::LeftOut => "on-demand map left out".into(),
            Skip::Interior => "interior (the tiles do not draw interiors)".into(),
            Skip::Removed => "removed by a server resource".into(),
            Skip::Level(level) => format!("detail level {} (the tiles draw HD only)", level_name(level)),
            Skip::UnknownArchetype => "unknown archetype: no .ytyp file defines it (model not found)".into(),
            Skip::OddBox => "box that is not a number".into(),
            Skip::HugeBox => "box of more than 64 blocks (a sky, a broken placement)".into(),
        }
    }
}

fn level_name(level: u32) -> String {
    LOD_NAMES.get(level as usize).map(|n| n.to_string()).unwrap_or(format!("{level}"))
}

/// An entity whose box meets the block.
struct Placed {
    entity: Entity,
    /// Index of its placement file in `World::ymaps`.
    file: usize,
    /// Why its file is not drawn, then why the entity itself is not.
    file_skip: Option<Skip>,
    skip: Option<Skip>,
    lo: [f32; 3],
    hi: [f32; 3],
}

impl Placed {
    fn drawn(&self) -> bool {
        self.file_skip.is_none() && self.skip.is_none()
    }
}

/// The world rectangle of block (bx, by): x1, y1, x2, y2.
fn block_rect(bx: i32, by: i32) -> [f32; 4] {
    [bx as f32 * BLOCK, by as f32 * BLOCK, (bx + 1) as f32 * BLOCK, (by + 1) as f32 * BLOCK]
}

/// Whether a box is in the block as the tiles count it: the blocks of `floor(lo / 128)` to
/// `floor(hi / 128)`, both ends in.
fn in_block(lo: [f32; 3], hi: [f32; 3], rect: [f32; 4]) -> bool {
    lo[0] < rect[2] && hi[0] >= rect[0] && lo[1] < rect[3] && hi[1] >= rect[1]
}

/// Every entity of the placement files whose box meets the block, classified as the tiles would
/// (the filter of `tiles::placements`, said instead of silent). Returns them and the placement
/// files that could not be read.
fn gather(game: &Path, keys: &GtaKeys, world: &World, rect: [f32; 4]) -> (Vec<Placed>, Vec<(String, String)>) {
    let mut placed = Vec::new();
    let mut unreadable = Vec::new();
    for (file, m) in world.ymaps.iter().enumerate() {
        if !(m.entities_min[0] <= rect[2]
            && m.entities_max[0] >= rect[0]
            && m.entities_min[1] <= rect[3]
            && m.entities_max[1] >= rect[1])
        {
            continue;
        }
        let file_skip = if world.replaced.contains(&m.hash) {
            Some(Skip::Replaced)
        } else if world.groups.left_out.contains(&m.hash) {
            Some(Skip::LeftOut)
        } else {
            None
        };
        let map = match crate::cli::archive::read_file(game, &m.path, keys)
            .and_then(|b| Resource::parse(&b))
            .and_then(|r| parse_ymap(&r))
        {
            Ok(map) => crate::world::fivem::shown(&world.server, &m.path, map),
            Err(e) => {
                unreadable.push((m.path.clone(), format!("{e:#}")));
                continue;
            }
        };
        for e in map.entities {
            let archetype = world.archetypes.get(&e.archetype).map(|(a, _)| a);
            let (lo, hi) = match archetype {
                Some(a) => world_box(&e, a, true),
                None => (e.position, e.position),
            };
            let finite = lo.iter().chain(&hi).all(|v| v.is_finite());
            let here = if finite { in_block(lo, hi, rect) } else { in_block(e.position, e.position, rect) };
            if !here {
                continue;
            }
            let skip = if e.interior {
                Some(Skip::Interior)
            } else if e.lod_level == crate::world::fivem::NOT_SHOWN {
                Some(Skip::Removed)
            } else if !matches!(e.lod_level, 0 | 5) {
                Some(Skip::Level(e.lod_level))
            } else if archetype.is_none() {
                Some(Skip::UnknownArchetype)
            } else if !finite {
                Some(Skip::OddBox)
            } else if ((hi[0] / BLOCK).floor() - (lo[0] / BLOCK).floor()) > 64.0
                || ((hi[1] / BLOCK).floor() - (lo[1] / BLOCK).floor()) > 64.0
            {
                Some(Skip::HugeBox)
            } else {
                None
            };
            placed.push(Placed { entity: e, file, file_skip, skip, lo, hi });
        }
    }
    (placed, unreadable)
}

/// The file of an archetype's model: (its name's hash, its extension).
fn model_file(a: &Archetype) -> Option<(u32, &'static str)> {
    match a.asset_type {
        ASSET_FRAGMENT => Some((a.asset_name, "yft")),
        ASSET_DRAWABLE => Some((a.asset_name, "ydr")),
        ASSET_DRAWABLE_DICTIONARY => Some((a.drawable_dictionary, "ydd")),
        _ => None,
    }
}

/// The name of a file of the game, or of a server resource (with the resource's name).
fn file_name(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path).to_string();
    if crate::world::fivem::is_streamed(path) {
        let described = crate::world::fivem::describe(path);
        let resource = described.split('/').next().unwrap_or("");
        format!("{name} (resource {resource})")
    } else {
        name
    }
}

/// What the model of an archetype is called: its file (and its entry, in a dictionary).
fn model_label(world: &World, names: &HashMap<u32, String>, a: &Archetype) -> String {
    match model_file(a) {
        Some((hash, ext)) => {
            let file = world
                .index
                .copies(hash, ext)
                .first()
                .map(|p| file_name(p))
                .unwrap_or_else(|| format!("{}.{ext}", label(names, hash)));
            if ext == "ydd" { format!("{file} [{}]", label(names, a.asset_name)) } else { file }
        }
        None => format!("{} (archetype without a model, type {})", label(names, a.name), a.asset_type),
    }
}

/// Why the model of an archetype could not be had (`Assets::drawable` gave none): not found,
/// protected, unreadable, absent from its dictionary.
fn missing_reason(game: &Path, keys: &GtaKeys, world: &World, names: &HashMap<u32, String>, a: &Archetype) -> String {
    let Some((hash, ext)) = model_file(a) else {
        return format!("the archetype has no model (type {})", a.asset_type);
    };
    let copies = world.index.copies(hash, ext);
    if copies.is_empty() {
        return format!("file {}.{ext} not found, neither in the game nor in the resources", label(names, hash));
    }
    let mut reasons = Vec::new();
    for path in copies {
        let said = match crate::cli::archive::read_file(game, path, keys)
            .and_then(|b| crate::world::fivem::resource(path, &b))
        {
            Err(e) => format!("unreadable ({e:#})"),
            Ok(res) if crate::world::fivem::is_streamed(path) && crate::world::fivem::is_scrambled(path, &res) => {
                "protected model (vertices scrambled, probably by FiveM escrow), not drawn".to_string()
            }
            Ok(res) => {
                let parsed = match ext {
                    "ydr" => crate::formats::ydr::parse_ydr(&res).map(|_| ()),
                    "yft" => crate::formats::ydr::parse_yft(&res).map(|_| ()),
                    _ => crate::formats::ydr::parse_ydd(&res).and_then(|list| {
                        if list.iter().any(|(h, _)| *h == a.asset_name) {
                            Ok(())
                        } else {
                            bail!("{} missing from the dictionary", label(names, a.asset_name))
                        }
                    }),
                };
                match parsed {
                    Ok(()) => "readable: left out for another reason".to_string(),
                    Err(e) => format!("does not read as a model ({e:#})"),
                }
            }
        };
        reasons.push(format!("{}: {said}", file_name(path)));
    }
    reasons.join("; ")
}

/// The render masks of the models of each detail level, read from the file (the parser leaves out
/// the models without the main pass): (mask, pieces) per model.
fn raw_masks(game: &Path, keys: &GtaKeys, world: &World, a: &Archetype) -> Result<[Vec<(u8, u16)>; 4]> {
    let (hash, ext) = model_file(a).context("no model")?;
    let res = crate::world::fivem::usable(game, keys, &world.index, hash, ext).context("unreadable model")?;
    let at = match ext {
        "ydr" => ROOT,
        "yft" => res.u64(ROOT + 0x30)?,
        _ => {
            let (hashes, count) = res.list(ROOT + 0x20)?;
            let drawables = res.pointers(ROOT + 0x30)?;
            let i = (0..count.min(drawables.len()))
                .find(|&i| res.u32(hashes + i as u64 * 4).ok() == Some(a.asset_name))
                .context("missing from the dictionary")?;
            drawables[i]
        }
    };
    let mut out: [Vec<(u8, u16)>; 4] = Default::default();
    for (level, models) in out.iter_mut().enumerate() {
        let list = res.u64(at + 0x50 + level as u64 * 8)?;
        if list == 0 {
            continue;
        }
        for m in res.pointers(list)? {
            models.push(((res.u16(m + 0x2C)? & 0xFF) as u8, res.u16(m + 0x10)?));
        }
    }
    Ok(out)
}

/// What a render mask without the main pass serves.
fn mask_meaning(mask: u8) -> &'static str {
    if mask & 0x02 != 0 {
        "the cast shadow only (the shadow of a tree, of an object)"
    } else {
        "a pass other than the main view (reflection, water)"
    }
}

/// The level the tiles draw of a model (distance 0, the scale of the LOD distances 3), as
/// `scene::stream` picks it.
fn level_drawn(d: &Drawable) -> Option<usize> {
    (0..4)
        .find(|&l| !d.lods[l].is_empty() && 0.0 <= d.lod_dist[l] * 3.0)
        .or_else(|| (0..4).rev().find(|&l| !d.lods[l].is_empty()))
}

/// Names of shaders and texture parameters, to show them instead of their hashes.
const SHADERS: &[&str] = &[
    "default",
    "normal",
    "normal_spec",
    "normal_spec_detail",
    "normal_detail",
    "normal_diffspec",
    "normal_diffspec_detail",
    "spec",
    "default_spec",
    "cutout",
    "cutout_fence",
    "cutout_fence_normal",
    "normal_cutout",
    "normal_spec_cutout",
    "decal",
    "normal_decal",
    "spec_decal",
    "normal_spec_decal",
    "decal_tnt",
    "decal_dirt",
    "decal_glue",
    "emissive",
    "emissivenight",
    "emissivestrong",
    "alpha",
    "glass",
    "glass_pv",
    "glass_env",
    "trees",
    "trees_lod",
    "trees_lod2",
    "trees_lod_tnt",
    "trees_normal",
    "trees_normal_spec",
    "trees_normal_diffspec",
    "trees_normal_diffspec_tnt",
    "trees_tnt",
    "trees_camera_aligned",
    "trees_camera_facing",
    "trees_shadow_proxy",
    "trees_normal_spec_tnt",
    "grass",
    "grass_batch",
    "grass_fur",
    "grass_fur_mask",
    "default_tnt",
    "normal_tnt",
    "normal_spec_tnt",
    "cutout_tnt",
    "normal_cutout_tnt",
    "normal_spec_cutout_tnt",
    "default_detail",
    "mirror_default",
    "parallax",
    "normal_spec_dpm",
    "terrain_cb_w_4lyr",
    "terrain_cb_w_4lyr_lod",
    "terrain_cb_w_4lyr_spec",
    "terrain_cb_w_4lyr_cm",
    "terrain_cb_w_4lyr_cm_tnt",
    "terrain_cb_w_4lyr_2tex",
    "terrain_cb_w_4lyr_2tex_blend",
    "terrain_cb_w_4lyr_2tex_blend_lod",
    "terrain_cb_4lyr",
    "terrain_cb_4lyr_lod",
    "terrain_cb_w_4lyr_pxm",
    "terrain_cb_w_4lyr_spec_pxm",
    "terrain_cb_w_4lyr_2tex_pxm",
    "terrain_cb_w_4lyr_2tex_blend_pxm",
    "water_river",
    "water_riverlod",
    "water_riverfoam",
    "water_terrainfoam",
    "water_fountain",
    "water_poolenv",
    "water_shallow",
    "vehicle_paint1",
    "ptfx_model",
    "cable",
    "billboard_nobump",
    "leaves",
    "radar",
    "sky_system",
];

fn shader_name(hash: u32) -> String {
    SHADERS
        .iter()
        .find(|n| joaat(n) == hash || joaat(&format!("{n}.sps")) == hash)
        .map(|n| n.to_string())
        .unwrap_or(format!("0x{hash:08X}"))
}

/// Each texture of a dictionary: (lowercase name, format, width, height).
type TextureList = Vec<(String, u32, usize, usize)>;

/// The texture dictionaries read to tell why a texture is not found: name → (lowercase name,
/// format, width, height) of each texture, None when the dictionary has no file.
struct Dictionaries<'a> {
    game: &'a Path,
    keys: &'a GtaKeys,
    world: &'a World,
    read: HashMap<u32, Option<TextureList>>,
}

impl Dictionaries<'_> {
    fn of(&mut self, dict: u32) -> Option<&Vec<(String, u32, usize, usize)>> {
        let (game, keys, world) = (self.game, self.keys, self.world);
        self.read
            .entry(dict)
            .or_insert_with(|| {
                let res = crate::world::fivem::usable(game, keys, &world.index, dict, "ytd")?;
                let list = crate::formats::ytd::parse_ytd(&res).ok()?;
                Some(list.into_iter().map(|t| (t.name.to_lowercase(), t.format, t.width, t.height)).collect())
            })
            .as_ref()
    }

    /// Where the texture `name` of a model is found, in the order the drawing looks: the model,
    /// the archetype's dictionary and its parents, then by its name alone in a shared dictionary.
    fn found_where(
        &mut self,
        name: &str,
        d: &Drawable,
        txd: u32,
        generic: &Generic,
        names: &HashMap<u32, String>,
    ) -> String {
        let lower = name.to_lowercase();
        if d.textures.iter().any(|t| t.name.eq_ignore_ascii_case(&lower)) {
            return "in the model".into();
        }
        let (mut dict, mut seen) = (txd, HashSet::new());
        while dict != 0 && seen.insert(dict) {
            if self.of(dict).is_some_and(|list| list.iter().any(|(n, ..)| *n == lower)) {
                return format!("in {}", label(names, dict));
            }
            dict = self.world.txd_parents.get(&dict).copied().unwrap_or(0);
        }
        match generic.get(&lower) {
            Some((dict, copies)) => format!(
                "by its name alone, in the shared dictionary {} (dictionaries of this name: {copies})",
                label(names, *dict)
            ),
            None => "not found".into(),
        }
    }

    /// Why the texture `name` of a model is not found where the drawing looks for it: present but
    /// not decoded (and its format), or found nowhere (and where it was looked for).
    fn why(&mut self, name: &str, d: &Drawable, txd: u32, generic: &Generic, names: &HashMap<u32, String>) -> String {
        let lower = name.to_lowercase();
        if let Some(t) = d.textures.iter().find(|t| t.name.eq_ignore_ascii_case(&lower)) {
            return format!(
                "in the model, but unreadable ({} {}×{})",
                crate::formats::ytd::format_name(t.format),
                t.width,
                t.height
            );
        }
        let mut chain = Vec::new();
        let (mut dict, mut seen) = (txd, HashSet::new());
        while dict != 0 && seen.insert(dict) {
            match self.of(dict) {
                None => chain.push(format!("{} (no file)", label(names, dict))),
                Some(list) => {
                    if let Some((_, format, w, h)) = list.iter().find(|(n, ..)| *n == lower) {
                        return format!(
                            "in {}, but unreadable ({} {w}×{h})",
                            label(names, dict),
                            crate::formats::ytd::format_name(*format)
                        );
                    }
                    chain.push(label(names, dict));
                }
            }
            dict = self.world.txd_parents.get(&dict).copied().unwrap_or(0);
        }
        if let Some((dict, copies)) = generic.get(&lower) {
            return format!("in the shared dictionary {} ({copies} in all), but unreadable", label(names, *dict));
        }
        let looked =
            if chain.is_empty() { "no dictionary (the archetype names none)".to_string() } else { chain.join(" → ") };
        format!("NOT FOUND: not in the model, nor in {looked}, nor by its name in the generic folders")
    }
}

/// A piece of a model that may draw wrong.
struct Suspect {
    /// Index of the piece in the level drawn, its shader, its triangles.
    piece: usize,
    shader: String,
    bucket: u8,
    triangles: usize,
    /// What is wrong with it, in words.
    faults: Vec<String>,
    /// It is drawn without a texture: plain grey (times its tint).
    grey: bool,
}

/// The pieces of the level drawn of a model that may draw wrong: without a texture (none named,
/// or named and not found, or not decoded), a terrain layer without its texture, a tint palette not
/// found, texture coordinates or positions that are not numbers. With the choice of `scene.rs`.
fn suspects(
    d: &Drawable,
    key: crate::render::shared::ModelKey,
    a: &Archetype,
    assets: &mut Assets,
    dicts: &mut Dictionaries,
    generic: &Generic,
    names: &HashMap<u32, String>,
) -> Vec<Suspect> {
    let Some(level) = level_drawn(d) else { return Vec::new() };
    let (diffuse, palette, lookup) = (joaat("DiffuseSampler"), joaat("TintPaletteSampler"), joaat("lookupSampler"));
    let layer: [u32; 4] = [0, 1, 2, 3].map(|k| joaat(&format!("TextureSampler_layer{k}")));
    let mut out = Vec::new();
    for (piece, g) in d.lods[level].iter().flat_map(|m| &m.geometries).enumerate() {
        let Some(shader) = d.shaders.get(g.shader) else { continue };
        // (water and foam: drawn as water or not at all, never with a texture)
        if shader.bucket == 6 {
            continue;
        }
        let param = |hash: u32| shader.textures.iter().find(|(p, _)| *p == hash).map(|(_, t)| t.clone());
        let mut faults = Vec::new();
        let mut grey = false;
        let look =
            |name: &str, assets: &mut Assets| assets.image(name, key, &d.textures, a.texture_dictionary).is_some();
        let layered = param(layer[1]).is_some() && crate::render::assets::TERRAIN.0 <= 1;
        if layered {
            for (k, l) in layer.iter().enumerate() {
                match param(*l) {
                    None => faults.push(format!("terrain layer {k} without a named texture (grey)")),
                    Some(name) if !look(&name, assets) => faults.push(format!(
                        "layer {k} \"{}\" {} (grey)",
                        name.to_lowercase(),
                        dicts.why(&name, d, a.texture_dictionary, generic, names)
                    )),
                    Some(_) => {}
                }
            }
            if let Some(mask) = param(lookup).filter(|_| !g.uvs1.is_empty())
                && !look(&mask, assets)
            {
                faults.push(format!(
                    "mask \"{}\" {} (the layers are then blended by the vertex colours)",
                    mask.to_lowercase(),
                    dicts.why(&mask, d, a.texture_dictionary, generic, names)
                ));
            }
        } else {
            let name =
                param(diffuse).or_else(|| param(layer[0])).or_else(|| shader.textures.first().map(|(_, t)| t.clone()));
            match name {
                None => {
                    grey = true;
                    faults.push("the shader names no texture: plain grey".to_string());
                }
                Some(name) if !look(&name, assets) => {
                    grey = true;
                    faults.push(format!(
                        "texture \"{}\" {}: plain grey",
                        name.to_lowercase(),
                        dicts.why(&name, d, a.texture_dictionary, generic, names)
                    ));
                }
                Some(_) => {}
            }
        }
        if let Some(name) = param(palette)
            && !look(&name, assets)
        {
            faults.push(format!(
                "tint palette \"{}\" {} (no tint)",
                name.to_lowercase(),
                dicts.why(&name, d, a.texture_dictionary, generic, names)
            ));
        }
        let odd = |uv: &&[f32; 2]| !(uv[0].is_finite() && uv[1].is_finite());
        let nan = g.uvs.iter().filter(odd).count();
        if nan > 0 {
            faults.push(format!("{nan} texture coordinates out of {} are not numbers (read as texel 0)", g.uvs.len()));
        }
        let nan1 = g.uvs1.iter().filter(odd).count();
        if nan1 > 0 && layered {
            faults.push(format!("{nan1} mask coordinates out of {} are not numbers (read as texel 0)", g.uvs1.len()));
        }
        let lost = g.positions.iter().filter(|p| !p.iter().all(|v| v.is_finite())).count();
        if lost > 0 {
            faults.push(format!("{lost} vertices out of {} whose position is not a number", g.positions.len()));
        }
        if !faults.is_empty() {
            out.push(Suspect {
                piece,
                shader: shader_name(shader.name),
                bucket: shader.bucket,
                triangles: g.indices.len() / 3,
                faults,
                grey,
            });
        }
    }
    out
}

/// What a pixel of the block looks like that may be wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sort {
    /// Nothing is drawn there.
    Empty,
    NearBlack,
    /// Plain grey, around 128 on the three channels: a piece without a texture, likely.
    Grey,
}

const SORTS: [Sort; 3] = [Sort::Empty, Sort::NearBlack, Sort::Grey];

impl Sort {
    fn says(self) -> &'static str {
        match self {
            Sort::Empty => "empty (nothing is drawn there)",
            Sort::NearBlack => "near black",
            Sort::Grey => "plain grey (≈ 128, 128, 128: a piece without a texture, most often)",
        }
    }
}

/// The sort of each pixel (None: it looks like a drawn colour), from the RGBA picture and the depths.
fn sorts(rgba: &[u8], depths: &[f32]) -> Vec<Option<Sort>> {
    depths
        .iter()
        .zip(rgba.as_chunks::<4>().0)
        .map(|(d, p)| {
            if !d.is_finite() {
                return Some(Sort::Empty);
            }
            let (r, g, b) = (p[0], p[1], p[2]);
            if (r as u32 + g as u32 + b as u32) < NEAR_BLACK {
                Some(Sort::NearBlack)
            } else if GREY.contains(&r) && r.abs_diff(g) <= 1 && g.abs_diff(b) <= 1 && r.abs_diff(b) <= 1 {
                Some(Sort::Grey)
            } else {
                None
            }
        })
        .collect()
}

/// A region of pixels of one sort, 4-connected.
#[derive(Debug, PartialEq)]
struct Region {
    pixels: usize,
    /// Mean column and row.
    centre: [f32; 2],
    /// Columns and rows it spans: x0, y0, x1, y1 (both ends in).
    span: [usize; 4],
    /// It touches the edge of the block (it may go on beyond).
    edge: bool,
    /// Pixels of it that `extra` marks (the water of the tiles).
    marked: usize,
}

/// The 4-connected regions of the pixels `on` of a `w`-wide picture, the largest first.
fn regions(on: &[bool], w: usize, extra: &[bool]) -> Vec<Region> {
    let h = on.len() / w.max(1);
    let mut seen = vec![false; on.len()];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..on.len() {
        if !on[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        stack.push(start);
        let (mut n, mut sx, mut sy, mut marked) = (0usize, 0f64, 0f64, 0usize);
        let mut span = [usize::MAX, usize::MAX, 0, 0];
        let mut edge = false;
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            n += 1;
            sx += x as f64;
            sy += y as f64;
            marked += extra.get(i).copied().unwrap_or(false) as usize;
            span = [span[0].min(x), span[1].min(y), span[2].max(x), span[3].max(y)];
            edge |= x == 0 || y == 0 || x + 1 == w || y + 1 == h;
            let mut visit = |j: usize| {
                if on[j] && !seen[j] {
                    seen[j] = true;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < w {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - w);
            }
            if y + 1 < h {
                visit(i + w);
            }
        }
        out.push(Region { pixels: n, centre: [(sx / n as f64) as f32, (sy / n as f64) as f32], span, edge, marked });
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.pixels));
    out
}

/// The pixels (x0, y0, x1, y1, both ends in) of the block a world box covers, None outside it.
fn pixel_rect(lo: [f32; 3], hi: [f32; 3], rect: [f32; 4]) -> Option<[usize; 4]> {
    let x0 = ((lo[0] - rect[0]) / METRES).floor();
    let x1 = ((hi[0] - rect[0]) / METRES).floor();
    let y0 = ((rect[3] - hi[1]) / METRES).floor();
    let y1 = ((rect[3] - lo[1]) / METRES).floor();
    if !(x0.is_finite() && x1.is_finite() && y0.is_finite() && y1.is_finite())
        || x1 < 0.0
        || y1 < 0.0
        || x0 >= PX as f32
        || y0 >= PX as f32
    {
        return None;
    }
    let clamp = |v: f32| v.clamp(0.0, PX as f32 - 1.0) as usize;
    Some([clamp(x0), clamp(y0), clamp(x1), clamp(y1)])
}

/// Square metres of so many pixels.
fn m2(pixels: usize) -> String {
    let area = pixels as f32 * METRES * METRES;
    if area < 10.0 { format!("{area:.1} m²") } else { format!("{} m²", thousands(area.round() as usize)) }
}

/// A whole number with a comma between thousands, the English way.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The world point of the middle of a pixel.
fn world_of(column: f32, row: f32, rect: [f32; 4]) -> [f32; 2] {
    [rect[0] + (column + 0.5) * METRES, rect[3] - (row + 0.5) * METRES]
}

/// A pixel is dull (grey) when its channels differ by less than this share of the strongest.
const DULL: f32 = 0.15;
/// A model whose pixels seen from above are this many percent dull or more looks grey.
const GREYISH_MODEL: f64 = 50.0;

/// Whether a pixel looks grey: neither dark nor coloured.
fn dull(p: &[u8]) -> bool {
    let (max, min) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
    max >= 40 && ((max - min) as f32) <= DULL * max as f32
}

/// What one model shows of the block from above.
#[derive(Default)]
struct Seen {
    hash: u32,
    /// Pixels where it is the surface, the sum of their colours, how many look grey.
    pixels: usize,
    sum: [u64; 3],
    dull: usize,
    /// Of those, the pixels of each [`Sort`], and those where it is of that sort alone.
    hits: [usize; 3],
    itself: [usize; 3],
}

impl Seen {
    fn looks_grey(&self) -> bool {
        self.dull as f64 * 100.0 / self.pixels.max(1) as f64 >= GREYISH_MODEL
    }

    fn looks_dark(&self) -> bool {
        self.hits[Sort::NearBlack as usize] as f64 * 100.0 / self.pixels.max(1) as f64 >= GREYISH_MODEL
    }
}

/// The main texture of each piece of the level drawn of a model (the choice of `scene.rs`; the
/// four layers of a terrain), where it is found and its mean colour.
fn textures_of(
    d: &Drawable,
    key: crate::render::shared::ModelKey,
    a: &Archetype,
    assets: &mut Assets,
    dicts: &mut Dictionaries,
    generic: &Generic,
    names: &HashMap<u32, String>,
) -> Vec<String> {
    let Some(level) = level_drawn(d) else { return vec!["no level to draw".into()] };
    let diffuse = joaat("DiffuseSampler");
    let layer: [u32; 4] = [0, 1, 2, 3].map(|k| joaat(&format!("TextureSampler_layer{k}")));
    let mut out = Vec::new();
    for (piece, g) in d.lods[level].iter().flat_map(|m| &m.geometries).enumerate() {
        let Some(shader) = d.shaders.get(g.shader) else { continue };
        let param = |hash: u32| shader.textures.iter().find(|(p, _)| *p == hash).map(|(_, t)| t.clone());
        let layered = param(layer[1]).is_some() && crate::render::assets::TERRAIN.0 <= 1;
        let names_used: Vec<String> = if layered {
            layer.iter().filter_map(|l| param(*l)).collect()
        } else {
            param(diffuse)
                .or_else(|| param(layer[0]))
                .or_else(|| shader.textures.first().map(|(_, t)| t.clone()))
                .into_iter()
                .collect()
        };
        let mut said = Vec::new();
        for name in &names_used {
            let colour = match assets.image(name, key, &d.textures, a.texture_dictionary) {
                None => "not found: plain grey".to_string(),
                Some(tex) => {
                    let im = &tex.image;
                    let (mut sum, mut n) = ([0u64; 3], 0u64);
                    for px in im.rgba.as_chunks::<4>().0.iter().filter(|px| px[3] >= 128) {
                        for k in 0..3 {
                            sum[k] += px[k] as u64;
                        }
                        n += 1;
                    }
                    let opaque = n as f64 * 100.0 / (im.width * im.height).max(1) as f64;
                    match std::num::NonZeroU64::new(n) {
                        None => format!("{}×{}, no opaque texel", im.width, im.height),
                        Some(n) => format!(
                            "{}×{}, mean colour ({}, {}, {}) over its opaque texels ({opaque:.0}%)",
                            im.width,
                            im.height,
                            sum[0] / n,
                            sum[1] / n,
                            sum[2] / n
                        ),
                    }
                }
            };
            said.push(format!(
                "\"{}\" {}: {colour}",
                name.to_lowercase(),
                dicts.found_where(name, d, a.texture_dictionary, generic, names)
            ));
        }
        if said.is_empty() {
            said.push("no texture named: plain grey".into());
        }
        // the colours of the vertices, which the drawing does not multiply in (but for the alpha of
        // what is blended, and a palette's column): a tint the game may give there
        let mean = |set: &[[u8; 4]]| {
            let n = set.len().max(1) as u64;
            let sum = set.iter().fold([0u64; 4], |mut acc, c| {
                for k in 0..4 {
                    acc[k] += c[k] as u64;
                }
                acc
            });
            format!("({}, {}, {}, {})", sum[0] / n, sum[1] / n, sum[2] / n, sum[3] / n)
        };
        let colours = match (g.colours0.is_empty(), g.colours1.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!("; vertex colours, mean {}", mean(&g.colours0)),
            (true, false) => format!("; second vertex colours, mean {}", mean(&g.colours1)),
            (false, false) => format!("; vertex colours, mean {} and {}", mean(&g.colours0), mean(&g.colours1)),
        };
        out.push(format!(
            "piece {piece} (shader {}, bucket {}, {} triangles): {}{colours}",
            shader_name(shader.name),
            shader.bucket,
            thousands(g.indices.len() / 3),
            said.join("; ")
        ));
    }
    out
}

/// Draws entities into a fresh picture of the block: heights only for what only covers.
fn draw(entities: &[&Placed], world: &World, assets: &mut Assets, centre: [f32; 2], heights_only: bool) -> Raster {
    let draws: Vec<Draw> = entities
        .iter()
        .filter_map(|p| {
            world.archetypes.get(&p.entity.archetype).map(|(a, _)| Draw {
                entity: p.entity.clone(),
                archetype: a.clone(),
                distance: 0.0,
            })
        })
        .collect();
    let mut raster = Raster::new(PX, PX, Projection::Orthographic { centre, metres: METRES, top: TOP }, [0.0; 3]);
    if heights_only {
        raster.set_pass(None, true);
    }
    paint(&mut raster, &draws, assets, true, 3.0);
    assets.trim(BUDGET);
    raster
}

/// Why a map loaded on request that has entities in the block is kept.
fn kept_says(kept: &Kept) -> String {
    match kept {
        Kept::Listed(entry) => format!("\"{entry}\" of [maps] keep names it"),
        Kept::Island(entry, island) => format!("\"{entry}\" of [maps] keep names the island {island}, which loads it"),
        Kept::Script => "a script of a server resource loads it".into(),
        Kept::Hour => "its map group is shown at the hour and in the weather of the scan".into(),
        Kept::Parent(child) => format!("the kept map {child} depends on it"),
        Kept::Hole => "it fills a hole in the ground".into(),
    }
}

/// `args`: what follows `probe` on the command line.
pub fn run(game: &Path, args: &[String]) -> Result<()> {
    let asked = parse(args)?;
    let (bx, by) = (asked.bx, asked.by);
    let rect = block_rect(bx, by);
    let centre = [rect[0] + BLOCK / 2.0, rect[1] + BLOCK / 2.0];
    let t0 = Instant::now();
    // (the card, when it draws, opens while the world loads)
    crate::render::gpu::warm();
    let keys = crate::game::keys(game)?;
    let world = World::load(game, &keys, false)?;
    let config = crate::config::get();
    crate::world::water::draw_meshes(config.water.enabled);
    let names = names(&world.index);
    let server = crate::world::fivem::server();
    println!(
        "block {bx},{by}: x {} to {} m, y {} to {} m (tile 7/{bx}/{by}); configuration: {} server resource(s){}, water {}, ground hole filling {}",
        rect[0],
        rect[2],
        rect[1],
        rect[3],
        server.resources.len(),
        if server.resources.is_empty() {
            String::new()
        } else {
            format!(" ({})", server.resources.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", "))
        },
        if config.water.enabled { "on" } else { "off" },
        if config.maps.fill_holes { "on" } else { "off" }
    );

    // 1. what is placed in the block
    let (placed, unreadable) = gather(game, &keys, &world, rect);
    let generic = generic_textures(game, &keys, &world);
    let mut assets = Assets::new(game, &keys, &world, &generic);
    let mut dicts = Dictionaries { game, keys: &keys, world: &world, read: HashMap::new() };
    let stem = |file: usize| file_name(&world.ymaps[file].path);
    let drawn: Vec<&Placed> = placed.iter().filter(|p| p.drawn()).collect();
    // each model drawn, once: its drawable and the level the tiles draw, or why there is none
    let mut by_archetype: BTreeMap<u32, Vec<&Placed>> = BTreeMap::new();
    for p in &drawn {
        by_archetype.entry(p.entity.archetype).or_default().push(p);
    }
    struct Model {
        label: String,
        drawable: Option<(crate::render::shared::ModelKey, Arc<Drawable>)>,
        missing: Option<String>,
    }
    let mut models: HashMap<u32, Model> = HashMap::new();
    for &hash in by_archetype.keys() {
        let a = &world.archetypes[&hash].0;
        let drawable = assets.drawable(a);
        let missing = drawable.is_none().then(|| missing_reason(game, &keys, &world, &names, a));
        models.insert(hash, Model { label: model_label(&world, &names, a), drawable, missing });
    }
    let mut rows: BTreeMap<(String, String, u32), usize> = BTreeMap::new();
    for p in &drawn {
        *rows.entry((stem(p.file), models[&p.entity.archetype].label.clone(), p.entity.lod_level)).or_default() += 1;
    }
    println!(
        "\nmodels placed in the block and drawn by the tiles: {} entities, {} different models, from {} placement files",
        drawn.len(),
        by_archetype.len(),
        drawn.iter().map(|p| p.file).collect::<HashSet<_>>().len()
    );
    let mut last_file = String::new();
    for ((file, model, level), n) in &rows {
        if *file != last_file {
            println!("  {file}");
            last_file = file.clone();
        }
        println!("      {model} · {} · × {n}", level_name(*level));
    }

    // 2. what is not drawn, and why
    println!("\nwhat is not drawn, and why:");
    let mut nothing = true;
    for (path, e) in &unreadable {
        nothing = false;
        println!("  unreadable placement file: {} ({e})", file_name(path));
    }
    // entities, by reason then file
    let mut skipped: BTreeMap<(Skip, String), usize> = BTreeMap::new();
    for p in placed.iter().filter(|p| !p.drawn()) {
        *skipped.entry((p.file_skip.or(p.skip).unwrap(), stem(p.file))).or_default() += 1;
    }
    let mut last = None;
    for ((skip, file), n) in &skipped {
        nothing = false;
        if last != Some(*skip) {
            println!("  entities: {}", skip.says());
            last = Some(*skip);
        }
        println!("      {file}: {n}");
    }
    // the maps loaded on request of the block: left out, or kept and why
    let on_demand: HashMap<u32, &crate::world::groups::OnDemand> =
        world.groups.files.iter().map(|f| (f.hash, f)).collect();
    let files_here: HashSet<usize> = placed.iter().map(|p| p.file).collect();
    let mut files_here: Vec<usize> = files_here.into_iter().collect();
    files_here.sort_by_key(|f| world.ymaps[*f].path.clone());
    for &f in &files_here {
        let m = &world.ymaps[f];
        let Some(od) = on_demand.get(&m.hash) else { continue };
        match &od.kept {
            None if !od.replaced => {
                nothing = false;
                println!(
                    "  on-demand map left out: {} (pack {}){}",
                    od.name,
                    crate::world::groups::pack(&od.path),
                    if config.maps.fill_holes {
                        "; `tiles` keeps it if it fills a hole in the ground (search not made here)"
                    } else {
                        ""
                    }
                );
            }
            Some(kept) => println!("  (on-demand map kept: {}: {})", od.name, kept_says(kept)),
            None => {}
        }
    }
    // the models themselves
    for (&hash, list) in &by_archetype {
        let model = &models[&hash];
        let a = &world.archetypes[&hash].0;
        if let Some(why) = &model.missing {
            nothing = false;
            println!("  model not drawn: {} ({} entities): {why}", model.label, list.len());
            continue;
        }
        let Some((_, d)) = &model.drawable else { continue };
        let level = level_drawn(d);
        // pieces left out by their render mask, at the levels up to the one drawn
        if let Ok(masks) = raw_masks(game, &keys, &world, a) {
            let upto = level.unwrap_or(3);
            for (l, list_l) in masks.iter().enumerate().take(upto + 1) {
                let hidden: Vec<&(u8, u16)> = list_l.iter().filter(|(m, _)| m & 1 == 0).collect();
                if hidden.is_empty() {
                    continue;
                }
                nothing = false;
                let masks_said: BTreeMap<u8, usize> = hidden.iter().fold(BTreeMap::new(), |mut acc, (m, _)| {
                    *acc.entry(*m).or_default() += 1;
                    acc
                });
                println!(
                    "  {} ({} entities), level {l}{}: {} model(s) out of {} not drawn, {} piece(s): render mask without the main view ({})",
                    model.label,
                    list.len(),
                    if Some(l) == level { ", the one the tiles draw" } else { "" },
                    hidden.len(),
                    list_l.len(),
                    hidden.iter().map(|(_, n)| *n as usize).sum::<usize>(),
                    masks_said
                        .iter()
                        .map(|(m, n)| format!("0x{m:02X} × {n}: {}", mask_meaning(*m)))
                        .collect::<Vec<_>>()
                        .join("; ")
                );
            }
        }
        if level.is_none() {
            nothing = false;
            println!("  {}: no detail level to draw", model.label);
            continue;
        }
        // water and foam
        let pieces = d.lods[level.unwrap()].iter().flat_map(|m| &m.geometries);
        let wet = pieces
            .filter(|g| {
                d.shaders.get(g.shader).is_some_and(|s| {
                    s.bucket == 6 && !(config.water.enabled && crate::world::water::mesh(s.name).is_some())
                })
            })
            .count();
        if wet > 0 {
            nothing = false;
            println!("  {}: {wet} water or foam piece(s) (render bucket 6) not drawn", model.label);
        }
    }
    if nothing {
        println!("  nothing: every entity of the block is drawn whole");
    }

    // pieces without a texture, in the whole block (drawn grey, seen or covered)
    let mut suspects_of: HashMap<u32, Vec<Suspect>> = HashMap::new();
    for &hash in by_archetype.keys() {
        let a = &world.archetypes[&hash].0;
        if let Some((key, d)) = models[&hash].drawable.clone() {
            let list = suspects(&d, key, a, &mut assets, &mut dicts, &generic, &names);
            if !list.is_empty() {
                suspects_of.insert(hash, list);
            }
        }
    }
    let mut grey_models: Vec<(u32, usize, usize)> = suspects_of
        .iter()
        .map(|(h, l)| {
            (*h, l.iter().filter(|s| s.grey).count(), l.iter().filter(|s| s.grey).map(|s| s.triangles).sum::<usize>())
        })
        .filter(|(_, n, _)| *n > 0)
        .collect();
    grey_models.sort_by_key(|a| std::cmp::Reverse(a.2));
    println!(
        "\npieces without a texture (drawn plain grey) of the block's models: {} model(s){}",
        grey_models.len(),
        if grey_models.is_empty() { String::new() } else { ":".into() }
    );
    for (hash, n, triangles) in grey_models.iter().take(LINES) {
        println!(
            "  {} ({} entities): {n} piece(s), {} triangles",
            models[hash].label,
            by_archetype[hash].len(),
            thousands(*triangles)
        );
    }

    // 3. the block drawn in memory
    let t = Instant::now();
    let full = draw(&drawn, &world, &mut assets, centre, false);
    let device = match crate::render::gpu::drawing_card() {
        Some(card) => format!("the graphics card {card}"),
        None => "the processor".to_string(),
    };
    let rgba = full.rgba();
    let depths = full.depths().to_vec();
    let sort = sorts(&rgba, &depths);
    let count = |s: Sort| sort.iter().filter(|x| **x == Some(s)).count();
    println!(
        "\nthe block drawn in memory by {device} ({:.1} s), at {METRES} m per pixel ({PX} × {PX} px), without the light, the hole filling or the water:",
        t.elapsed().as_secs_f64()
    );
    for s in SORTS {
        let n = count(s);
        let share = format!("{:.2}", n as f64 * 100.0 / (PX * PX) as f64);
        println!("  {} pixels {} ({}, {share}%)", thousands(n), s.says(), m2(n));
    }

    // the water the tiles lay over the block (`[water]`): over the ground under it, and over the
    // empty pixels of the sea; a patch of ground taken for water would look like a hole
    let water = if config.water.enabled {
        crate::world::water::Water::load(game, &keys)
    } else {
        crate::world::water::Water::default()
    };
    let mut wet = rgba.clone();
    if !water.quads.is_empty() {
        water.flood(&mut wet, &depths, TOP, [rect[0], rect[3]], METRES, PX);
    }
    let flooded: Vec<bool> =
        (0..PX * PX).map(|i| depths[i].is_finite() && wet[i * 4..i * 4 + 3] != rgba[i * 4..i * 4 + 3]).collect();
    let n_flooded = flooded.iter().filter(|f| **f).count();
    if n_flooded > 0 {
        let found = regions(&flooded, PX, &[]);
        println!(
            "\ndrawn pixels the tiles' water covers (the ground is under the surface of a piece of water): {} ({})",
            thousands(n_flooded),
            m2(n_flooded)
        );
        for r in found.iter().filter(|r| r.pixels >= SMALL_REGION).take(8) {
            let [x, y] = world_of(r.centre[0], r.centre[1], rect);
            println!(
                "  {} around x {x:.0}, y {y:.0}{}",
                m2(r.pixels),
                if r.edge { ", touches the edge of the block" } else { "" }
            );
        }
    }

    // empty pixels: the regions, what would cover them
    let empty: Vec<bool> = sort.iter().map(|s| *s == Some(Sort::Empty)).collect();
    let n_empty = count(Sort::Empty);
    if n_empty > 0 {
        let under_water: Vec<bool> = (0..PX * PX).map(|i| empty[i] && wet[i * 4 + 3] > 0).collect();
        let n_water = under_water.iter().filter(|w| **w).count();
        let found = regions(&empty, PX, &under_water);
        let big: Vec<&Region> = found.iter().filter(|r| r.pixels >= SMALL_REGION).collect();
        println!(
            "\nempty pixels: {} region(s), {} of them of 1 m² or more; {} of these pixels are under the water the tiles draw{}",
            found.len(),
            big.len(),
            thousands(n_water),
            if config.water.enabled { "" } else { " (water off in map.toml)" }
        );
        for r in big.iter().take(12) {
            let [x, y] = world_of(r.centre[0], r.centre[1], rect);
            let (w, h) = ((r.span[2] - r.span[0] + 1) as f32 * METRES, (r.span[3] - r.span[1] + 1) as f32 * METRES);
            println!(
                "  {} around x {x:.0}, y {y:.0} ({w:.0} × {h:.0} m){}{}",
                m2(r.pixels),
                if r.edge { ", touches the edge of the block (may go on next to it)" } else { "" },
                if r.marked == r.pixels {
                    ", wholly under water: not a hole in the tiles".to_string()
                } else if r.marked > 0 {
                    format!(", {} of it under water", m2(r.marked))
                } else {
                    String::new()
                }
            );
        }
        let holes: Vec<bool> = (0..PX * PX).map(|i| empty[i] && !under_water[i]).collect();
        let n_holes = holes.iter().filter(|h| **h).count();
        if n_holes > 0 {
            println!("  what would cover the {} empty pixels out of the water ({}):", thousands(n_holes), m2(n_holes));
            let mut covers: Vec<(usize, String)> = Vec::new();
            // the maps loaded on request left out, each alone; then the lower detail levels, by file
            let mut groups: BTreeMap<(u8, usize, u32), Vec<&Placed>> = BTreeMap::new();
            for p in &placed {
                match (p.file_skip, p.skip) {
                    (Some(Skip::LeftOut), None) => groups.entry((0, p.file, 0)).or_default().push(p),
                    (None, Some(Skip::Level(level))) => groups.entry((1, p.file, level)).or_default().push(p),
                    _ => {}
                }
            }
            for ((kind, file, level), list) in &groups {
                let solo = draw(list, &world, &mut assets, centre, true);
                let covered = solo.depths().iter().zip(&holes).filter(|(d, h)| **h && d.is_finite()).count();
                if covered > 0 {
                    let what = if *kind == 0 {
                        format!("the on-demand map left out {} ({} HD entities)", stem(*file), list.len())
                    } else {
                        format!(
                            "the {} {} entities of {} (not drawn by the tiles)",
                            list.len(),
                            level_name(*level),
                            stem(*file)
                        )
                    };
                    covers.push((covered, what));
                }
            }
            // the boxes of the models that are not drawn
            for (&hash, list) in &by_archetype {
                let model = &models[&hash];
                if model.missing.is_none() {
                    continue;
                }
                let mut inside = vec![false; PX * PX];
                for p in list {
                    if let Some([x0, y0, x1, y1]) = pixel_rect(p.lo, p.hi, rect) {
                        for y in y0..=y1 {
                            for x in x0..=x1 {
                                inside[y * PX + x] = true;
                            }
                        }
                    }
                }
                let covered = inside.iter().zip(&holes).filter(|(i, h)| **i && **h).count();
                if covered > 0 {
                    covers.push((
                        covered,
                        format!("the box of the model not drawn {} ({} entities)", model.label, list.len()),
                    ));
                }
            }
            covers.sort_by_key(|a| std::cmp::Reverse(a.0));
            if covers.is_empty() {
                println!(
                    "      nothing the files place here (the hole is in the game's files, or beyond the last ground)"
                );
            }
            for (covered, what) in covers.iter().take(LINES) {
                println!(
                    "      {what}: {} ({:.0}% of the empty pixels)",
                    m2(*covered),
                    *covered as f64 * 100.0 / n_holes as f64
                );
            }
        }
    }

    // every model drawn alone, its depths compared with the block's: the pixels it shows, their
    // colour, and the near black and grey ones it paints
    let mut seen_models: Vec<Seen> = Vec::new();
    for (&hash, list) in &by_archetype {
        if models[&hash].drawable.is_none() {
            continue;
        }
        let solo = draw(list, &world, &mut assets, centre, false);
        let (own_rgba, own_depths) = (solo.rgba(), solo.depths());
        let own = sorts(&own_rgba, own_depths);
        let mut seen = Seen { hash, ..Seen::default() };
        for k in 0..PX * PX {
            if !(own_depths[k].is_finite() && (own_depths[k] - depths[k]).abs() < SAME) {
                continue;
            }
            seen.pixels += 1;
            let p = &rgba[k * 4..k * 4 + 3];
            for (c, &v) in p.iter().enumerate() {
                seen.sum[c] += v as u64;
            }
            seen.dull += dull(p) as usize;
            if let Some(s) = sort[k] {
                seen.hits[s as usize] += 1;
                seen.itself[s as usize] += (own[k] == Some(s)) as usize;
            }
        }
        if seen.pixels > 0 {
            seen_models.push(seen);
        }
    }
    seen_models.sort_by_key(|a| std::cmp::Reverse(a.pixels));
    let files_of = |hash: u32| {
        let files: BTreeMap<String, usize> = by_archetype[&hash].iter().fold(BTreeMap::new(), |mut acc, p| {
            *acc.entry(stem(p.file)).or_default() += 1;
            acc
        });
        files.iter().map(|(f, n)| format!("{f} × {n}")).collect::<Vec<_>>().join(", ")
    };
    let say_pieces = |hash: u32| match suspects_of.get(&hash) {
        None => println!("      no suspicious piece: this colour is that of its textures"),
        Some(list) => {
            for sus in list {
                println!(
                    "      piece {} (shader {}, bucket {}, {} triangles): {}",
                    sus.piece,
                    sus.shader,
                    sus.bucket,
                    thousands(sus.triangles),
                    sus.faults.join("; ")
                );
            }
        }
    };
    println!(
        "\nmodels seen from above (each drawn alone, depths compared): visible surface, mean colour, share of dull pixels (grey: channels apart by less than {}% of the strongest):",
        (DULL * 100.0) as u32
    );
    for m in seen_models.iter().filter(|m| m.pixels >= SMALL_REGION).take(LINES) {
        let mean = m.sum.map(|v| (v as f64 / m.pixels as f64).round() as u32);
        let share = m.dull as f64 * 100.0 / m.pixels as f64;
        println!(
            "  {}: {} visible, mean colour ({}, {}, {}), {share:.0}% dull{}",
            models[&m.hash].label,
            m2(m.pixels),
            mean[0],
            mean[1],
            mean[2],
            if m.looks_grey() {
                "  ← grey"
            } else if m.looks_dark() {
                "  ← near black"
            } else {
                ""
            }
        );
    }
    // the textures of the models that look grey or near black from above: which, where they are
    // found, their colour
    let odd: Vec<&Seen> =
        seen_models.iter().filter(|m| m.pixels >= SMALL_REGION && (m.looks_grey() || m.looks_dark())).collect();
    if !odd.is_empty() {
        println!("\ntextures of the models that look grey or near black (level drawn; main texture of each piece):");
    }
    for m in odd.iter().take(LINES) {
        let a = &world.archetypes[&m.hash].0;
        let Some((key, d)) = models[&m.hash].drawable.clone() else { continue };
        println!("  {} ({})", models[&m.hash].label, files_of(m.hash));
        let lines = textures_of(&d, key, a, &mut assets, &mut dicts, &generic, &names);
        for line in lines.iter().take(PIECES) {
            println!("      {line}");
        }
        if lines.len() > PIECES {
            println!("      … and {} other pieces", lines.len() - PIECES);
        }
    }
    for s in [Sort::NearBlack, Sort::Grey] {
        let n = count(s);
        if n == 0 {
            continue;
        }
        println!("\npixels {} ({}): the models at their surface:", s.says(), thousands(n));
        let mut rows: Vec<&Seen> = seen_models.iter().filter(|m| m.hits[s as usize] > 0).collect();
        rows.sort_by(|a, b| b.hits[s as usize].cmp(&a.hits[s as usize]));
        let put: usize = rows.iter().map(|m| m.hits[s as usize]).sum();
        if rows.is_empty() {
            println!("  no model is at their surface (a blended marking, which writes no depth, may paint them)");
        }
        for m in rows.iter().take(LINES) {
            println!(
                "  {} ({}): at the surface of {} of these pixels ({}), {} of its own colour",
                models[&m.hash].label,
                files_of(m.hash),
                thousands(m.hits[s as usize]),
                m2(m.hits[s as usize]),
                thousands(m.itself[s as usize])
            );
            say_pieces(m.hash);
        }
        if put < n {
            println!(
                "  ({} of these pixels with no model at their surface: under a blended marking, or at a depth another model shares)",
                thousands(n - put)
            );
        }
    }

    if let Some(path) = &asked.image {
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes).write_image(
            &rgba,
            PX as u32,
            PX as u32,
            image::ExtendedColorType::Rgba8,
        )?;
        std::fs::write(path, &bytes).with_context(|| format!("writing {}", path.display()))?;
        println!("\nimage of the block (empty pixels transparent, no light): {}", path.display());
    }
    if let Some(line) = crate::render::gpu::summary() {
        println!("\n{line}");
    }
    println!("\nprobe: {:.0} s", t0.elapsed().as_secs_f64());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_block_is_asked_in_any_of_its_spellings() {
        for given in [&["16", "40"][..], &["16,40"], &["16,", "40"], &["16", ",40"], &["16, 40"]] {
            assert_eq!(parse(&args(given)).unwrap(), Asked { bx: 16, by: 40, image: None }, "{given:?}");
        }
        assert_eq!(
            parse(&args(&["-12", "2", "--image", r"C:\a b\block.png"])).unwrap(),
            Asked { bx: -12, by: 2, image: Some(PathBuf::from(r"C:\a b\block.png")) }
        );
        assert_eq!(parse(&args(&["--image", "x.png", "-12", "2"])).unwrap().bx, -12);
        for wrong in [
            &[][..],
            &["16"],
            &["16", "40", "3"],
            &["x", "2"],
            &["16", "40", "--image"],
            &["16", "40", "--image", "--zone"],
            &["16", "40", "--zone", "1"],
            &["5000", "0"],
        ] {
            let e = parse(&args(wrong)).unwrap_err().to_string();
            assert!(e.contains("probe <bx> <by>"), "{wrong:?}: {e}");
        }
    }

    #[test]
    fn pixels_are_sorted_as_they_look() {
        let rgba = [
            128, 128, 128, 0, // nothing drawn, whatever its colour
            5, 6, 7, 255, // near black
            128, 128, 128, 255, // the grey of a piece without a texture
            127, 128, 128, 255, // the same, rounded otherwise by the card
            40, 90, 30, 255, // a colour
            140, 140, 140, 255, // a lighter grey: a texture's own
        ];
        let depths = [f32::INFINITY, 10.0, 10.0, 10.0, 10.0, 10.0];
        assert_eq!(
            sorts(&rgba, &depths),
            [Some(Sort::Empty), Some(Sort::NearBlack), Some(Sort::Grey), Some(Sort::Grey), None, None]
        );
        // the indices the counts of a model use
        assert_eq!([Sort::Empty as usize, Sort::NearBlack as usize, Sort::Grey as usize], [0, 1, 2]);
    }

    #[test]
    fn dull_pixels_are_the_grey_ones() {
        assert!(dull(&[100, 100, 100]));
        assert!(dull(&[95, 105, 110]));
        assert!(!dull(&[60, 76, 43]), "a green");
        assert!(!dull(&[20, 20, 20]), "too dark to tell");
        assert!(!dull(&[150, 120, 100]), "a brown");
    }

    #[test]
    fn empty_regions_are_joined_by_their_sides_only() {
        // 5 × 4, x: empty
        // x x . . .
        // . x . . x
        // . . x . x
        // . . . . x
        let on: Vec<bool> = ["xx...", ".x..x", "..x.x", "....x"].concat().chars().map(|c| c == 'x').collect();
        let marked = vec![false; 20];
        let found = regions(&on, 5, &marked);
        let sizes: Vec<usize> = found.iter().map(|r| r.pixels).collect();
        assert_eq!(sizes, [3, 3, 1], "{found:?}");
        // the largest first; a corner does not join (the pixel at 2,2 stays alone)
        let first = &found[0];
        assert!(first.edge);
        let lone = found.iter().find(|r| r.pixels == 1).unwrap();
        assert_eq!((lone.centre, lone.span, lone.edge), ([2.0, 2.0], [2, 2, 2, 2], false));
        // the pixels marked (under water) are counted in their region
        let mut wet = vec![false; 20];
        wet[9] = true;
        wet[14] = true;
        let found = regions(&on, 5, &wet);
        let column = found.iter().find(|r| r.span[0] == 4).unwrap();
        assert_eq!((column.pixels, column.marked), (3, 2));
    }

    #[test]
    fn boxes_fall_on_the_pixels_of_the_block() {
        let rect = block_rect(16, 40);
        assert_eq!(rect, [2048.0, 5120.0, 2176.0, 5248.0]);
        assert_eq!(pixel_rect([2048.0, 5120.0, 0.0], [2176.0, 5248.0, 0.0], rect), Some([0, 0, 511, 511]));
        assert_eq!(pixel_rect([2050.0, 5240.0, 0.0], [2051.0, 5247.0, 9.0], rect), Some([8, 4, 12, 32]));
        assert_eq!(pixel_rect([1000.0, 5120.0, 0.0], [2000.0, 5248.0, 0.0], rect), None);
        assert_eq!(pixel_rect([f32::NAN, 5120.0, 0.0], [2100.0, 5248.0, 0.0], rect), None);
        assert_eq!(world_of(0.0, 0.0, rect), [2048.125, 5247.875]);
        // the blocks of a box are those of floor(lo / 128) to floor(hi / 128), as the tiles count them
        assert!(in_block([2000.0, 5130.0, 0.0], [2048.0, 5140.0, 0.0], rect), "ends on the west edge");
        assert!(!in_block([2176.0, 5130.0, 0.0], [2200.0, 5140.0, 0.0], rect), "starts on the east edge");
        assert!(!in_block([2100.0, f32::NAN, 0.0], [2110.0, 5140.0, 0.0], rect));
    }

    #[test]
    fn figures_are_written_the_english_way() {
        assert_eq!(
            [thousands(0), thousands(999), thousands(1000), thousands(1_234_567)],
            ["0", "999", "1,000", "1,234,567"]
        );
        assert_eq!([m2(16), m2(3), m2(160), m2(16_000)], ["1.0 m²", "0.2 m²", "10 m²", "1,000 m²"]);
    }

    #[test]
    fn names_and_masks_are_told() {
        assert_eq!(shader_name(joaat("trees_normal_spec")), "trees_normal_spec");
        assert_eq!(shader_name(joaat("normal_spec_detail.sps")), "normal_spec_detail");
        assert_eq!(shader_name(0x1234_5678), "0x12345678");
        assert!(mask_meaning(0xE2).contains("shadow"));
        assert!(mask_meaning(0x04).contains("reflection"));
        assert!(Skip::Level(1).says().contains("LOD"));
        assert!(Skip::Level(99).says().contains("99"));
    }
}
