//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The map as a whole: every file ([`Index`]), every archetype, the box of every placement file
//! (the copy the game uses), and the parents of the texture dictionaries (`gtxd`).

pub mod fivem;
pub mod groups;
pub mod holes;
pub mod resources;
pub mod water;

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use rage_rpf::GtaKeys;

use crate::formats::resource::{Resource, joaat};
use crate::formats::ymap::{Archetype, parse_ymap, parse_ytyp};
use crate::game::index::{self, Index};

/// Prefixes of the multiplayer packs that replace placement files of the base map with their own
/// copy (Heists, Executives, Lowriders, Bikers, Diamond Casino), with 3 510, 584, 592, 145 and 198
/// such pairs. Their `content.xml` swaps the base area archive for theirs in their map
/// change set (`GROUP_MAP`, run whenever the map loads: `MPHEIST_GTA5_CITYE_DOWNTOWN_01`
/// invalidates the base `downtown_01_metadata.rpf` and enables its own).
pub const REPLACING_PREFIXES: [&str; 5] = ["hei_", "apa_", "lr_", "bkr_", "vw_"];

/// A placement file: where it is, its boxes.
pub struct YmapInfo {
    pub path: String,
    pub hash: u32,
    pub entities_min: [f32; 3],
    pub entities_max: [f32; 3],
    pub entity_count: usize,
    /// Hash of the placement file this one hangs from (its LOD parent), or 0.
    pub parent: u32,
    /// `flags` of its `CMapData` (bit 0: loaded on a script's request only, see [`crate::world::groups`]).
    pub flags: u32,
}

pub struct World {
    pub index: Index,
    pub archetypes: HashMap<u32, (Archetype, String)>,
    pub ymaps: Vec<YmapInfo>,
    /// Placement files a multiplayer pack replaces with its own prefixed copy (see
    /// [`REPLACING_PREFIXES`]): not to be drawn.
    pub replaced: std::collections::HashSet<u32>,
    /// Texture dictionary → its parent (hashes of the names).
    pub txd_parents: HashMap<u32, u32>,
    /// What the server's resources brought, and which of their entities are not shown
    /// ([`crate::world::fivem::shown`]).
    pub server: crate::world::fivem::Added,
    /// Every `_manifest.ymf` of the game.
    pub manifests: Vec<crate::world::groups::ManifestFile>,
    /// The maps loaded on request only, and those of them the white list keeps: the placement
    /// files of `groups.left_out` are not to be drawn.
    pub groups: crate::world::groups::Groups,
}

/// A file the loading reads, found by the walk of the archives.
struct Job {
    path: String,
    archive: crate::game::store::ArchiveRef,
    entry: usize,
    kind: Kind,
}

/// What a file is read for.
#[derive(Clone, Copy)]
enum Kind {
    Archetypes,
    Placements,
    Manifest,
    Island,
    TextureParents,
}

/// What a file read gave (each read and parsed on its own, on any thread).
enum Read {
    Archetypes(Option<Vec<Archetype>>),
    Placements(Option<YmapInfo>),
    Manifest(crate::world::groups::ManifestFile),
    Island(Option<crate::world::groups::Island>),
    TextureParents(Option<Vec<u8>>),
}

impl Job {
    fn read(&self, keys: &GtaKeys) -> Read {
        let bytes = || self.archive.read(self.entry, keys);
        match self.kind {
            Kind::Archetypes => {
                Read::Archetypes(bytes().and_then(|b| Resource::parse(&b)).and_then(|r| parse_ytyp(&r)).ok())
            }
            Kind::Placements => {
                let stem = self
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&self.path)
                    .rsplit_once('.')
                    .map(|(s, _)| s)
                    .unwrap_or_default();
                Read::Placements(bytes().and_then(|b| Resource::parse(&b)).and_then(|r| parse_ymap(&r)).ok().map(|m| {
                    YmapInfo {
                        path: self.path.clone(),
                        hash: joaat(stem),
                        entities_min: m.entities_min,
                        entities_max: m.entities_max,
                        entity_count: m.entities.len(),
                        parent: m.parent,
                        flags: m.flags,
                    }
                }))
            }
            Kind::Manifest => Read::Manifest(crate::world::groups::ManifestFile::read(&self.path, bytes())),
            Kind::Island => {
                let text = bytes().map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
                Read::Island(crate::world::groups::Island::parse(&self.path, &text))
            }
            Kind::TextureParents => Read::TextureParents(bytes().ok()),
        }
    }
}

impl World {
    /// `base_first`: the order of the copies reversed (a check).
    ///
    /// The archives are walked once (their tables decrypted on all the cores and kept: `store`),
    /// which gives the index and the files to read; these are read and parsed on all the cores
    /// ([`crate::util::parallel::map`]), then taken in the order of the walk, as when they were read
    /// during it: the same archetypes win, the placement files and manifests come in the same order.
    pub fn load(game: &Path, keys: &GtaKeys, base_first: bool) -> Result<World> {
        let t = Instant::now();
        let dlclist = String::from_utf8_lossy(&crate::cli::archive::read_file(
            game,
            "update/update.rpf/common/data/dlclist.xml",
            keys,
        )?)
        .into_owned();
        // every file of every archive (the index), and those to read; the server's resources are
        // looked for meanwhile (on disk, once: `fivem::server`)
        let mut files: HashMap<(u32, String), Vec<String>> = HashMap::new();
        let mut jobs: Vec<Job> = Vec::new();
        std::thread::scope(|s| -> Result<()> {
            let scan = s.spawn(crate::world::fivem::server);
            let walked = crate::game::store::for_each_file(game, keys, &mut |path, archive, entry| {
                index::add_file(&mut files, path);
                let name = path.rsplit('/').next().unwrap_or(path);
                let Some((stem, ext)) = name.rsplit_once('.') else { return true };
                let kind = match ext {
                    "ytyp" => Kind::Archetypes,
                    "ymap" => Kind::Placements,
                    "ymf" => Kind::Manifest,
                    "meta" if stem == "islandhopper" => Kind::Island,
                    "ide" | "meta" if stem == "gtxd" => Kind::TextureParents,
                    _ => return true,
                };
                jobs.push(Job { path: path.to_string(), archive: archive.clone(), entry, kind });
                true
            });
            scan.join().unwrap_or_else(|e| std::panic::resume_unwind(e));
            walked
        })?;
        let index = Index::of_files(files, &dlclist, base_first);
        // the files read: the preferred copy of each placement file, every archetype file
        // (but one a server resource streams under the same name: never loaded)
        jobs.retain(|job| {
            let name = job.path.rsplit('/').next().unwrap_or(&job.path);
            let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or_default();
            match job.kind {
                Kind::Archetypes => !crate::world::fivem::replaces(&index, joaat(stem), "ytyp"),
                // folders the game does not load with the map (`exclude_folders`: North Yankton, the
                // prologue's level, which only its scripts load)
                Kind::Placements => {
                    !crate::config::get().maps.exclude_folders.iter().any(|f| job.path.contains(f.as_str()))
                        && index.copies(joaat(stem), "ymap").first() == Some(&job.path)
                }
                _ => true,
            }
        });
        let read = crate::util::parallel::map(&jobs, |job| job.read(keys));

        // archetypes (the preferred copy of each wins) and placement files (preferred copies only),
        // in the order of the walk
        let mut archetypes: HashMap<u32, (Archetype, String)> = HashMap::new();
        let mut ymaps = Vec::new();
        let mut gtxd: Vec<(usize, String, Vec<u8>)> = Vec::new();
        let mut manifests = Vec::new();
        let mut islands: Vec<(usize, crate::world::groups::Island)> = Vec::new();
        for (job, read) in jobs.iter().zip(read) {
            let path = job.path.as_str();
            match read {
                Read::Archetypes(Some(list)) => {
                    let p = index.rank(path);
                    for a in list {
                        if archetypes.get(&a.name).is_none_or(|(_, old)| index.rank(old) < p) {
                            archetypes.insert(a.name, (a, path.to_string()));
                        }
                    }
                }
                Read::Placements(Some(m)) => ymaps.push(m),
                Read::Manifest(m) => manifests.push(m),
                Read::Island(island) => islands.extend(island.map(|island| (index.rank(path), island))),
                Read::TextureParents(Some(b)) => gtxd.push((index.rank(path), path.to_string(), b)),
                Read::Archetypes(None) | Read::Placements(None) | Read::TextureParents(None) => {}
            }
        }
        // the server's resources: their archetypes, placement files and texture parents
        let added = crate::world::fivem::add_to_world(game, keys, &index, &mut archetypes, &mut ymaps, &mut gtxd);

        // texture parents: gtxd.ide (`child, parent` lines) and gtxd.meta (`<parent>`/`<child>`
        // items), the lowest ranked first so that a later pack overrides
        gtxd.sort_by_key(|(rank, _, _)| *rank);
        let mut txd_parents = HashMap::new();
        for (_, path, bytes) in &gtxd {
            let text = String::from_utf8_lossy(bytes);
            if path.ends_with(".ide") {
                for line in text.lines() {
                    if let Some((child, parent)) = line.split_once(',') {
                        txd_parents.insert(joaat(child.trim()), joaat(parent.trim()));
                    }
                }
            } else {
                for item in text.split("<Item>").skip(1) {
                    let tag =
                        |t: &str| item.split(&format!("<{t}>")).nth(1).and_then(|s| s.split('<').next()).map(str::trim);
                    if let (Some(parent), Some(child)) = (tag("parent"), tag("child")) {
                        txd_parents.insert(joaat(child), joaat(parent));
                    }
                }
            }
        }
        eprintln!(
            "world: {} archetypes, {} placement files, {} texture links ({} gtxd files), {:.1} s",
            archetypes.len(),
            ymaps.len(),
            txd_parents.len(),
            gtxd.len(),
            t.elapsed().as_secs_f64()
        );
        // a pack whose map change set swaps a base area for its own copy names its placement files
        // after the base ones with a prefix (hei_dt1_19_strm_0 for dt1_19_strm_0): of a name and its
        // prefixed copies, only the copy of the pack loaded last stays
        let stem = |p: &str| p.rsplit('/').next().unwrap_or(p).trim_end_matches(".ymap").to_string();
        let by_name: HashMap<String, &YmapInfo> = ymaps.iter().map(|m| (stem(&m.path), m)).collect();
        let mut replaced = std::collections::HashSet::new();
        for (name, m) in &by_name {
            if REPLACING_PREFIXES.iter().any(|p| name.starts_with(p)) {
                continue;
            }
            let copies: Vec<&&YmapInfo> =
                REPLACING_PREFIXES.iter().filter_map(|p| by_name.get(&format!("{p}{name}"))).collect();
            if let Some(last) = copies.iter().max_by_key(|c| index.rank(&c.path)) {
                replaced.insert(m.hash);
                replaced.extend(copies.iter().filter(|c| c.hash != last.hash).map(|c| c.hash));
            }
        }
        eprintln!("variants: {} placement files replaced by the copy of a multiplayer pack", replaced.len());
        // the maps loaded on request only (yachts, UFOs, the states of a building…): left out, except
        // those the white list keeps (`[maps] keep`), those a client script of a server resource
        // loads by name (`RequestIpl`) and those the game shows by itself at noon. Built before the
        // scripts' removals join `replaced`: what it says of `replaced` is about the packs' copies.
        islands.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
        let islands = islands.into_iter().map(|(_, island)| island).collect();
        let requested = crate::world::fivem::server().requested();
        let groups = crate::world::groups::Groups::build(
            &index,
            &ymaps,
            &replaced,
            &manifests,
            islands,
            &crate::config::get().maps.keep,
            &requested,
        );
        eprintln!(
            "on demand: {} placement files loaded on request, {} kept, {} left out",
            groups.files.len(),
            groups.files.len() - groups.left_out.len(),
            groups.left_out.len()
        );
        // the placement files a client script of the server removes (`RemoveIpl`) are not drawn either
        crate::world::fivem::report(&added, &ymaps, &mut replaced);
        Ok(World { index, archetypes, ymaps, replaced, txd_parents, server: added, manifests, groups })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use sha1::{Digest, Sha1};
    use std::fmt::Write as _;

    /// A SHA-1 fed by `write!`.
    struct Hasher(Sha1);

    impl std::fmt::Write for Hasher {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            self.0.update(s.as_bytes());
            Ok(())
        }
    }

    /// The SHA-1 of each part of what loading the game gives (the world, the shared textures,
    /// the placements of the whole map): equal when two loadings gave the same things in the same
    /// order (the maps by sorted key, everything else in its own order).
    pub(crate) fn digests(
        world: &World,
        generic: &crate::render::assets::Generic,
        places: &crate::tiles::Placements,
    ) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        let mut part = |name: &'static str, fill: &mut dyn FnMut(&mut Hasher) -> std::fmt::Result| {
            let mut h = Hasher(Sha1::new());
            fill(&mut h).expect("hashing");
            out.push((name, h.0.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>()));
        };
        part("index", &mut |h| {
            let mut keys: Vec<_> = world.index.files.keys().collect();
            keys.sort();
            for k in keys {
                write!(h, "{k:?}{:?};", world.index.files[k])?;
            }
            write!(h, "{:?}{}", world.index.dlcs, world.index.base_first)
        });
        part("archetypes", &mut |h| {
            let mut keys: Vec<_> = world.archetypes.keys().collect();
            keys.sort();
            for k in keys {
                write!(h, "{k}{:?};", world.archetypes[k])?;
            }
            Ok(())
        });
        part("placement files", &mut |h| {
            for m in &world.ymaps {
                write!(
                    h,
                    "{} {} {:?} {:?} {} {} {};",
                    m.path, m.hash, m.entities_min, m.entities_max, m.entity_count, m.parent, m.flags
                )?;
            }
            Ok(())
        });
        part("replaced", &mut |h| {
            let mut v: Vec<_> = world.replaced.iter().collect();
            v.sort();
            write!(h, "{v:?}")
        });
        part("texture parents", &mut |h| {
            let mut v: Vec<_> = world.txd_parents.iter().collect();
            v.sort();
            write!(h, "{v:?}")
        });
        part("manifests", &mut |h| {
            for m in &world.manifests {
                write!(h, "{} {} {:?};", m.path, m.rbf, m.manifest)?;
            }
            Ok(())
        });
        part("on-demand maps", &mut |h| {
            let g = &world.groups;
            let mut left: Vec<_> = g.left_out.iter().collect();
            left.sort();
            let mut groups: Vec<_> = g.groups.iter().collect();
            groups.sort_by_key(|(k, _)| **k);
            write!(h, "{left:?}{:?}{groups:?}{:?}{:?}{:?}", g.files, g.islands, g.list, g.evidence)
        });
        part("resources", &mut |h| {
            let a = &world.server;
            write!(
                h,
                "{} {} {} {} {} {} {} {} {} {} {} {} {} {} {:?}",
                a.ytyps,
                a.archetypes,
                a.archetypes_replaced,
                a.ymaps,
                a.ymaps_replaced,
                a.ymaps_emptied,
                a.gtxd,
                a.interiors_placed,
                a.inside,
                a.interiors_protected,
                a.moved_out,
                a.shadow_only,
                a.widened,
                a.interiors.len(),
                a.failures
            )?;
            let mut hidden: Vec<_> = a.not_shown.iter().collect();
            hidden.sort();
            write!(h, "{hidden:?}")?;
            let mut interiors: Vec<_> = a.interiors.iter().collect();
            interiors.sort_by_key(|(k, _)| **k);
            for (k, m) in interiors {
                write!(h, "{k} {} {:?};", m.name, m.entities)?;
            }
            Ok(())
        });
        part("shared textures", &mut |h| {
            let mut v: Vec<_> = generic.iter().collect();
            v.sort();
            write!(h, "{v:?}")
        });
        part("placement entities", &mut |h| {
            for e in &places.entities {
                write!(h, "{e:?};")?;
            }
            Ok(())
        });
        part("placement blocks", &mut |h| {
            let mut v: Vec<_> = places.blocks.iter().collect();
            v.sort_by_key(|(k, _)| **k);
            write!(h, "{v:?}")
        });
        out
    }

    /// The configuration of the tests that read the game: `BTMS_TEST_CONFIG`, else `map.toml`.
    pub(crate) fn game() -> (std::path::PathBuf, GtaKeys) {
        let config = crate::config::Config::load(std::env::var_os("BTMS_TEST_CONFIG").as_deref().map(Path::new))
            .expect("the configuration");
        crate::config::init(config);
        let game = crate::game::game_dir(None).expect("the game");
        let keys = crate::game::keys(&game).expect("the keys");
        (game, keys)
    }

    /// The time of each step of the loading, and the SHA-1 of what it gives (to compare two
    /// versions of the program): `cargo test --release -- --ignored --nocapture loading_steps`.
    #[test]
    #[ignore]
    fn loading_steps() {
        let (game, keys) = game();
        let t = Instant::now();
        let world = World::load(&game, &keys, false).expect("the world");
        let world_s = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let generic = crate::render::assets::generic_textures(&game, &keys, &world);
        let generic_s = t.elapsed().as_secs_f64();
        let t = Instant::now();
        let again = crate::render::assets::generic_textures(&game, &keys, &world);
        let again_s = t.elapsed().as_secs_f64();
        assert_eq!(generic, again);
        let t = Instant::now();
        let places = crate::tiles::placements(&game, &keys, &world, None).expect("the placements");
        let places_s = t.elapsed().as_secs_f64();
        println!(
            "steps: world {world_s:.2} s; shared textures {generic_s:.2} s, a second time {again_s:.2} s; placements of the whole map {places_s:.2} s ({} entities)",
            places.entities.len()
        );
        for (name, digest) in digests(&world, &generic, &places) {
            println!("digest {name}: {digest}");
        }
    }

    /// The game loaded one file after the other and on all the cores: the same world, the same
    /// shared textures, the same placements, for the whole map and for a zone
    /// (`cargo test --release -- --ignored one_thread_or_all`).
    #[test]
    #[ignore]
    fn one_thread_or_all() {
        let (game, keys) = game();
        let load = || {
            let world = World::load(&game, &keys, false).expect("the world");
            let generic = crate::render::assets::generic_textures(&game, &keys, &world);
            let places = crate::tiles::placements(&game, &keys, &world, None).expect("the placements");
            let mut all = digests(&world, &generic, &places);
            let zone = crate::tiles::placements(&game, &keys, &world, Some([-2304.0, 128.0, -1792.0, 640.0]))
                .expect("the placements");
            all.extend(digests(&world, &generic, &zone).into_iter().filter(|(name, _)| name.contains("placement")));
            all
        };
        let alone = crate::util::parallel::with_threads(1, load);
        assert!(crate::util::parallel::threads() > 1, "several threads");
        let together = load();
        for ((name, a), (_, b)) in alone.iter().zip(&together) {
            println!("{name}: one thread {a}, all {b}");
        }
        assert_eq!(alone, together);
    }
}
