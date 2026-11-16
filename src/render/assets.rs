//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! What the pictures of the map are drawn from, for the tiles, the holes of the ground, the water
//! and `probe`: the textures known by name in the `generic` folders ([`generic_textures`]), the
//! models and textures each drawing thread asks for ([`Assets`]), and [`paint`], which draws a list
//! of entities ([`Draw`]) into a raster: each model at the detail level its distance calls for, the
//! diffuse texture of each shader, tinted by its palette when it has one, without light.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use rage_rpf::GtaKeys;

use crate::formats::resource::{Resource, joaat};
use crate::formats::ydr::{Drawable, parse_ydd, parse_ydr, parse_yft};
use crate::formats::ymap::{ASSET_DRAWABLE, ASSET_DRAWABLE_DICTIONARY, ASSET_FRAGMENT, Archetype, Entity};
use crate::formats::ytd::{Texture, parse_ytd};
use crate::render::raster::{Image, Raster};
use crate::render::scene::{self, Tex};
use crate::render::shared::{Holder, ModelDictionary, ModelKey, Shared};
use crate::util::profile::{Phase, time};
use crate::world::World;

/// Terrain layer weights: (vertex colour set 0 or 1, byte of the 0-1 / 2-3 lerp, byte of the final
/// lerp). `BTMS_TERRAIN=set,a,b` overrides it, to compare other choices.
pub(crate) static TERRAIN: std::sync::LazyLock<(usize, usize, usize)> = std::sync::LazyLock::new(|| {
    std::env::var("BTMS_TERRAIN")
        .ok()
        .and_then(|v| {
            let p: Vec<usize> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (p.len() == 3).then(|| (p[0], p[1], p[2]))
        })
        .unwrap_or((1, 2, 1))
});

/// Terrain blend mask channels (the 0-1 / 2-3 lerp, the final lerp), `BTMS_TERRAIN_MASK=a,b` to compare.
pub(crate) static MASK: std::sync::LazyLock<(usize, usize)> = std::sync::LazyLock::new(|| {
    std::env::var("BTMS_TERRAIN_MASK")
        .ok()
        .and_then(|v| {
            let p: Vec<usize> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (p.len() == 2).then(|| (p[0], p[1]))
        })
        .unwrap_or((2, 1))
});

/// Which byte of a vertex's first colour picks the column of a tint palette.
pub(crate) const TINT_BYTE: usize = 2;
/// Texture name → (the shared dictionary of a `generic` folder that holds it, how many such
/// dictionaries hold it): the last resort when the archetype's dictionary and its known parents
/// do not have it.
pub type Generic = HashMap<String, (u32, usize)>;

/// The names of the textures of every dictionary under a `generic` folder (not the `+hi` ones),
/// read once: system pages only.
pub fn generic_textures(game: &Path, keys: &GtaKeys, world: &World) -> Generic {
    let t = Instant::now();
    let mut map: HashMap<String, (usize, u32, usize)> = HashMap::new();
    let index = &world.index;
    let mut count = 0;
    // the dictionaries, in the order of the walk (the archives' tables are kept: `store`), read
    // on all the cores, then taken in that order: of two copies of a name at the same rank, the
    // first one met wins, as when they were read during the walk
    let mut found: Vec<(String, crate::game::store::ArchiveRef, usize)> = Vec::new();
    let _ =
        crate::game::store::for_each_file_named(game, keys, &|name| name.ends_with(".ytd"), &mut |path, archive, i| {
            if path.contains("/generic/") && path.ends_with(".ytd") && !path.contains("+hi") {
                found.push((path.to_string(), archive.clone(), i));
            }
            true
        });
    let read = crate::util::parallel::map(&found, |(_, archive, i)| {
        archive
            .read(*i, keys)
            .and_then(|b| Resource::parse_system(&b))
            .and_then(|r| crate::formats::ytd::texture_names(&r))
            .ok()
    });
    for ((path, _, _), names) in found.iter().zip(read) {
        let stem = path.rsplit('/').next().unwrap_or(path).trim_end_matches(".ytd");
        let rank = index.rank(path);
        if let Some(names) = names {
            count += 1;
            for n in names {
                let n = n.to_lowercase();
                let copies = map.get(&n).map(|e| e.2).unwrap_or(0) + 1;
                if map.get(&n).is_none_or(|(r, _, _)| *r < rank) {
                    map.insert(n, (rank, joaat(stem), copies));
                } else if let Some(e) = map.get_mut(&n) {
                    e.2 = copies;
                }
            }
        }
    }
    eprintln!(
        "shared textures: {} names in {count} dictionaries of the generic folders ({:.1} s)",
        map.len(),
        t.elapsed().as_secs_f64()
    );
    map.into_iter().map(|(n, (_, h, c))| (n, (h, c))).collect()
}

/// Models and textures. They are loaded once for all threads ([`Shared`]); each thread keeps here
/// what it already asked for, and what it did not find.
pub struct Assets<'a> {
    game: &'a Path,
    keys: &'a GtaKeys,
    world: &'a World,
    generic: &'a Generic,
    shared: &'static Shared,
    /// The generation of `shared` the maps below were filled in.
    generation: u64,
    drawables: HashMap<ModelKey, Option<Arc<Drawable>>>,
    dictionaries: HashMap<u32, Option<ModelDictionary>>,
    txds: HashMap<u32, Option<Arc<Vec<Texture>>>>,
    /// (model, texture dictionary of the archetype, lowercase name) → the texture found for it.
    images: HashMap<(ModelKey, u32, String), Option<Tex>>,
    /// About how many bytes the shared caches hold.
    pub bytes: usize,
}

impl<'a> Assets<'a> {
    pub fn new(game: &'a Path, keys: &'a GtaKeys, world: &'a World, generic: &'a Generic) -> Assets<'a> {
        let shared = Shared::of(world);
        Assets {
            game,
            keys,
            world,
            generic,
            shared,
            generation: shared.generation(),
            drawables: HashMap::new(),
            dictionaries: HashMap::new(),
            txds: HashMap::new(),
            images: HashMap::new(),
            bytes: 0,
        }
    }

    /// Empties the caches when the models and textures loaded, by all threads together, hold more
    /// than `budget` bytes.
    pub fn trim(&mut self, budget: usize) {
        self.shared.trim(budget);
        // emptied (by this thread or another): what this thread kept of it goes too
        if self.generation != self.shared.generation() {
            self.generation = self.shared.generation();
            self.drawables.clear();
            self.dictionaries.clear();
            self.txds.clear();
            self.images.clear();
        }
        self.bytes = self.shared.bytes();
    }

    fn read(&self, hash: u32, ext: &str) -> Option<Resource> {
        // (a file of a server resource that cannot be used gives way to the next copy; each copy
        // read is counted and its inflating timed there, for `--profile`)
        crate::world::fivem::usable(self.game, self.keys, &self.world.index, hash, ext)
    }

    /// The model of an archetype: (its key, which is also that of its embedded textures; the drawable).
    pub(crate) fn drawable(&mut self, a: &Archetype) -> Option<(ModelKey, Arc<Drawable>)> {
        let dictionary = if a.asset_type == ASSET_DRAWABLE_DICTIONARY { a.drawable_dictionary } else { 0 };
        let key: ModelKey = (a.asset_type, a.asset_name, dictionary);
        if !self.drawables.contains_key(&key) {
            // a model of its own file: read once for all threads
            let own_file = |ext: &str, parse: fn(&Resource) -> Result<Drawable>| {
                self.shared.model((a.asset_type, a.asset_name), || {
                    let d =
                        self.read(a.asset_name, ext).and_then(|r| time(Phase::Models, || parse(&r).ok())).map(Arc::new);
                    let bytes = d.as_deref().map(drawable_bytes).unwrap_or(0);
                    (d, bytes)
                })
            };
            let d = match a.asset_type {
                ASSET_DRAWABLE => own_file("ydr", parse_ydr),
                ASSET_FRAGMENT => own_file("yft", parse_yft),
                ASSET_DRAWABLE_DICTIONARY => {
                    let dict = self.dictionary(a.drawable_dictionary);
                    dict.and_then(|d| d.iter().find(|(h, _)| *h == a.asset_name).map(|(_, d)| d.clone()))
                }
                _ => None,
            };
            self.drawables.insert(key, d);
        }
        self.drawables[&key].clone().map(|d| (key, d))
    }

    fn dictionary(&mut self, hash: u32) -> Option<ModelDictionary> {
        if !self.dictionaries.contains_key(&hash) {
            let d = self.shared.dictionary(hash, || {
                let d = self
                    .read(hash, "ydd")
                    .and_then(|r| time(Phase::Models, || parse_ydd(&r).ok()))
                    .map(|list| Arc::new(list.into_iter().map(|(h, d)| (h, Arc::new(d))).collect::<Vec<_>>()));
                let bytes =
                    d.as_ref().map(|list| list.iter().map(|(_, d)| drawable_bytes(d)).sum::<usize>()).unwrap_or(0);
                (d, bytes)
            });
            self.dictionaries.insert(hash, d);
        }
        self.dictionaries[&hash].clone()
    }

    fn txd(&mut self, hash: u32) -> Option<Arc<Vec<Texture>>> {
        if !self.txds.contains_key(&hash) {
            let t = self.shared.txd(hash, || {
                let t =
                    self.read(hash, "ytd").and_then(|r| time(Phase::Dictionaries, || parse_ytd(&r).ok())).map(Arc::new);
                let bytes = t.as_ref().map(|list| list.iter().map(|t| t.data.len()).sum::<usize>()).unwrap_or(0);
                (t, bytes)
            });
            self.txds.insert(hash, t);
        }
        self.txds[&hash].clone()
    }

    /// The texture `name`: in the model, else in the archetype's dictionary and its parents.
    pub(crate) fn image(&mut self, name: &str, model: ModelKey, embedded: &[Texture], txd: u32) -> Option<Tex> {
        let key = (model, txd, name.to_lowercase());
        if let Some(i) = self.images.get(&key) {
            return i.clone();
        }
        let lower = &key.2;
        let shared = self.shared;
        // decoded once for all threads, by what holds the texture and its name
        let decode = |holder: Holder, t: &Texture| {
            shared.image(holder, lower, || {
                let tex = time(Phase::Decode, || t.rgba().ok())
                    .map(|rgba| Tex::new(Image { width: t.width, height: t.height, rgba }));
                let bytes = tex.as_ref().map(|t| t.image.rgba.len()).unwrap_or(0);
                (tex, bytes)
            })
        };
        let mut found =
            embedded.iter().find(|t| t.name.eq_ignore_ascii_case(lower)).and_then(|t| decode(Holder::Model(model), t));
        let mut dict = txd;
        let mut seen = HashSet::new();
        while found.is_none() && dict != 0 && seen.insert(dict) {
            if let Some(list) = self.txd(dict) {
                found = list
                    .iter()
                    .find(|t| t.name.eq_ignore_ascii_case(lower))
                    .and_then(|t| decode(Holder::Dictionary(dict), t));
            }
            dict = self.world.txd_parents.get(&dict).copied().unwrap_or(0);
        }
        if found.is_none()
            && let Some(&(dict, _)) = self.generic.get(lower)
            && let Some(list) = self.txd(dict)
        {
            found = list
                .iter()
                .find(|t| t.name.eq_ignore_ascii_case(lower))
                .and_then(|t| decode(Holder::Dictionary(dict), t));
        }
        self.images.insert(key, found.clone());
        found
    }
}

/// About how many bytes a drawable holds.
fn drawable_bytes(d: &Drawable) -> usize {
    let geometry: usize =
        d.lods.iter().flatten().flat_map(|m| &m.geometries).map(|g| g.positions.len() * 48 + g.indices.len() * 2).sum();
    geometry + d.textures.iter().map(|t| t.data.len()).sum::<usize>()
}

/// An entity to draw.
pub struct Draw {
    pub entity: Entity,
    pub archetype: Archetype,
    /// From the camera: picks the model's detail level (0 for the most detailed).
    pub distance: f32,
}

/// Draws the entities: opaque, then cut out, then blended (decals). Returns (triangles, triangles
/// per render bucket).
///
/// In two halves: [`scene::stream`] builds the list of what to draw, one batch per piece of
/// geometry (world-space vertices, textures, mode), and the raster draws each batch as it comes
/// ([`Raster::draw`]). The graphics card takes the same list first, when there is one
/// ([`crate::render::gpu::paint`]); what it leaves is drawn here, by the processor.
pub fn paint(
    raster: &mut Raster,
    draws: &[Draw],
    assets: &mut Assets,
    inverse: bool,
    lod_scale: f32,
) -> (usize, [usize; 8]) {
    time(Phase::Paint, || {
        let options = scene::Options { inverse, lod_scale, clip: raster.clip() };
        if let Some(done) = crate::render::gpu::paint(raster, &mut |sink| scene::stream(draws, assets, &options, sink))
        {
            return done;
        }
        scene::stream(draws, assets, &options, &mut |batch| time(Phase::Raster, || raster.draw(batch)))
    })
}
