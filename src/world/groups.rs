//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The maps the game loads only when asked ("on demand": yachts, UFOs, the states of a building a
//! mission changes…), and which of them are drawn.
//!
//! **What marks them**: bit 0 of `flags` in the `CMapData` of a placement file. Established on the
//! game's files (the `groups` command prints the counts):
//!
//! - every map group of the `_manifest.ymf` files (`MapDataGroups`) is named after a placement file
//!   that has the bit, and a file without the bit never hangs from one that has it;
//! - the maps an island switches on (`IPLsToEnable` of `islandhopper.meta`) all have it;
//! - the maps the scripts of the user's server ask for or remove by name all have it, except
//!   interior placements, which are loaded anyway.
//!
//! The map groups themselves are not the list: a group exists when a map comes with collision
//! files (`Bounds` names `.ybn` files, never placement files) or follows the hour or the weather.
//! On build 3889 of the game, 750 of the 1 296 on-demand files have no group.
//! The `content.xml` and `setup2.xml` of the packs name archives, never a map group or a
//! placement file, and none of their on-demand change sets mounts a map archive.
//!
//! **What is drawn of them** (the others are left out, [`Groups::left_out`]):
//!
//! - the files the white list names (`[maps] keep` of `map.toml`: a name, `*` standing for
//!   anything), and the files of the islands it names (`HeistIsland`: the maps
//!   `SET_ISLAND_ENABLED` loads);
//! - the files a client script of a server resource loads by name (`RequestIpl` with the name
//!   written out, [`crate::world::fivem::Server::requested`]);
//! - the files whose map group follows the hour or the weather, when it is shown at [`HOUR`] in
//!   [`WEATHER`] weather: the game switches those by itself, no script asks (so the day props of
//!   Vespucci beach are drawn, and not the night ones);
//! - the files a kept one hangs from (its LOD parents: the game loads them with it).
//!
//! The rule is about the game's own files. A placement file a server resource streams
//! ([`crate::world::fivem`]) is never left out, whatever its bit: the rule chosen for them, which the
//! files cannot confirm (what FiveM does with a streamed file that bears the bit is not checked).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use crate::formats::pso::{GROUP_TIME_DEPENDENT, GROUP_WEATHER_DEPENDENT, Manifest};
use crate::formats::resource::joaat;
use crate::game::index::Index;
use crate::world::{World, YmapInfo};

/// Bit of `CMapData.flags`: the placement file is loaded on request only.
pub const ON_DEMAND: u32 = 1;
/// Bit of `CMapData.flags`: other placement files hang from this one (it holds their LOD entities).
pub const LOD_PARENT: u32 = 2;
/// The hour the hour-dependent maps are taken at: noon (fixed, not a setting).
pub const HOUR: u32 = 12;
/// The weather the weather-dependent maps are taken in (fixed, not a setting).
pub const WEATHER: &str = "EXTRASUNNY";
/// Weather names, to show the hashes of a map group as text.
const WEATHERS: [&str; 15] = [
    "EXTRASUNNY",
    "CLEAR",
    "CLOUDS",
    "SMOG",
    "FOGGY",
    "OVERCAST",
    "RAIN",
    "THUNDER",
    "CLEARING",
    "NEUTRAL",
    "SNOW",
    "BLIZZARD",
    "SNOWLIGHT",
    "XMAS",
    "HALLOWEEN",
];

/// A `_manifest.ymf` as read: where it is, what it says (or why it could not be read).
pub struct ManifestFile {
    pub path: String,
    /// In the older RBF form (three manifests of the base game), not PSO.
    pub rbf: bool,
    pub manifest: Result<Manifest, String>,
}

impl ManifestFile {
    pub fn read(path: &str, bytes: anyhow::Result<Vec<u8>>) -> ManifestFile {
        let rbf = bytes.as_ref().is_ok_and(|b| crate::formats::rbf::is_rbf(b));
        let manifest = bytes
            .and_then(|b| if rbf { rbf_manifest(&b) } else { crate::formats::pso::manifest(&b) })
            .map_err(|e| format!("{e:#}"));
        ManifestFile { path: path.to_string(), rbf, manifest }
    }
}

/// A manifest in RBF form. Those of the game only bind high definition textures; map groups there
/// have never been seen, so they are an error rather than read on a guess or ignored.
fn rbf_manifest(bytes: &[u8]) -> anyhow::Result<Manifest> {
    let root = crate::formats::rbf::parse(bytes)?;
    if root.name != "CPackFileMetaData" {
        anyhow::bail!("RBF root {}, CPackFileMetaData expected", root.name);
    }
    if root.child("MapDataGroups").is_some_and(|g| !g.children.is_empty()) {
        anyhow::bail!("RBF manifest with map groups: not supported");
    }
    Ok(Manifest {
        hd_bindings: root.child("HDTxdBindingArray").map(|l| l.children.len()).unwrap_or(0),
        ..Default::default()
    })
}

/// An island of an `islandhopper.meta` (`CIslandHopperData`): the maps `SET_ISLAND_ENABLED` of its
/// name loads.
#[derive(Clone, Debug)]
pub struct Island {
    pub name: String,
    pub path: String,
    /// Names of placement files, lowercase.
    pub maps: Vec<String>,
}

impl Island {
    pub fn parse(path: &str, text: &str) -> Option<Island> {
        let inner = |text: &'_ str, tag: &str| -> Option<String> {
            let rest = text.split(&format!("<{tag}>")).nth(1)?;
            Some(rest.split(&format!("</{tag}>")).next()?.to_string())
        };
        let body = inner(text, "CIslandHopperData")?;
        let name = inner(&body, "Name")?.trim().to_string();
        let list = inner(&body, "IPLsToEnable")?;
        let mut maps: Vec<String> =
            list.split("<Item>").skip(1).filter_map(|s| s.split('<').next()).map(|s| s.trim().to_lowercase()).collect();
        maps.sort();
        maps.dedup();
        Some(Island { name, path: path.to_string(), maps })
    }
}

/// Why an on-demand placement file is drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum Kept {
    /// An entry of the white list names it.
    Listed(String),
    /// An entry of the white list names an island that loads it: (entry, island).
    Island(String, String),
    /// A client script of a server resource loads it by name (`RequestIpl`).
    Script,
    /// Its map group is shown at [`HOUR`] in [`WEATHER`] weather.
    Hour,
    /// A kept file hangs from it.
    Parent(String),
    /// Without it the ground has a hole ([`crate::world::holes`]).
    Hole,
}

/// What the manifests say of the map group named after a placement file, all copies merged.
#[derive(Clone, Debug, Default)]
pub struct Group {
    pub flags: u32,
    pub hours: u32,
    pub weathers: Vec<u32>,
    /// The collision files that go with it.
    pub bounds: Vec<u32>,
    /// In how many manifests it is.
    pub copies: usize,
}

impl Group {
    /// The game shows it by itself at this hour, in this weather (None: no script-free rule).
    pub fn shown(&self, hour: u32, weather: &str) -> Option<bool> {
        if self.flags & (GROUP_TIME_DEPENDENT | GROUP_WEATHER_DEPENDENT) == 0 {
            return None;
        }
        let hour_ok = self.flags & GROUP_TIME_DEPENDENT == 0 || self.hours >> hour & 1 == 1;
        let weather_ok = self.flags & GROUP_WEATHER_DEPENDENT == 0 || self.weathers.contains(&joaat(weather));
        Some(hour_ok && weather_ok)
    }
}

/// An on-demand placement file.
#[derive(Clone, Debug)]
pub struct OnDemand {
    pub hash: u32,
    pub name: String,
    pub path: String,
    pub parent: u32,
    /// Other files hang from it ([`LOD_PARENT`]).
    pub lod: bool,
    pub entities: usize,
    /// A multiplayer pack replaces it with its own copy ([`World::replaced`]): not drawn anyway.
    pub replaced: bool,
    pub kept: Option<Kept>,
}

/// Counts of the checks the rule rests on.
#[derive(Clone, Debug, Default)]
pub struct Evidence {
    pub ymaps: usize,
    /// Distinct group names; those naming a placement file of the game; those whose file is among
    /// the files drawn here (not in an excluded folder); those of them with the on-demand bit.
    pub groups: [usize; 4],
    pub raw_groups: usize,
    /// `Bounds` of the groups: names in all, of collision files, of placement files, unknown.
    pub bounds: [usize; 4],
    /// Placement files without the bit hanging from one that has it.
    pub plain_under_on_demand: usize,
    /// On-demand files hanging from a file without the bit.
    pub on_demand_under_plain: usize,
    /// Files without the bit that a multiplayer pack replaces with an on-demand copy: leaving that
    /// copy out would leave a hole.
    pub replaced_by_on_demand: usize,
    /// Maps of the islands: in all, known placement files, with the bit.
    pub island_maps: [usize; 3],
    /// Groups following the hour, the weather.
    pub timed: [usize; 2],
    /// Placement files a server resource streams: in all, with the on-demand bit (never left out).
    pub streamed: [usize; 2],
}

pub struct Groups {
    /// On-demand placement files that are not kept: not to be drawn.
    pub left_out: HashSet<u32>,
    /// Every on-demand placement file, by pack then name.
    pub files: Vec<OnDemand>,
    pub groups: HashMap<u32, Group>,
    pub islands: Vec<Island>,
    /// The white list, and for each entry how many files it names.
    pub list: Vec<(String, usize)>,
    pub evidence: Evidence,
}

/// The name of a file without folders or extension.
pub fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name)
}

/// The pack a path belongs to.
pub fn pack(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("update/update.rpf/dlc_patch/") {
        format!("{} (patch)", rest.split('/').next().unwrap_or(""))
    } else if let Some(rest) = path.split("/dlcpacks/").nth(1) {
        rest.split('/').next().unwrap_or("").to_string()
    } else if path.starts_with("update/") {
        "update".to_string()
    } else {
        "base game".to_string()
    }
}

/// `name` matches `pattern`, where `*` stands for anything (both compared in lowercase).
pub fn matches(pattern: &str, name: &str) -> bool {
    fn rec(p: &[u8], n: &[u8]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some((b'*', rest)) => (0..=n.len()).any(|i| rec(rest, &n[i..])),
            Some((c, rest)) => n.split_first().is_some_and(|(d, more)| c == d && rec(rest, more)),
        }
    }
    rec(pattern.to_lowercase().as_bytes(), name.to_lowercase().as_bytes())
}

impl Groups {
    /// `ymaps`: the placement files drawn (the preferred copy of each); `replaced`: those a pack
    /// replaces; `islands`: every `islandhopper.meta` read, the best ranked first; `keep`: the white
    /// list; `requested`: the names (hashes) the client scripts of the server's resources load.
    #[allow(clippy::too_many_arguments)]
    pub fn build(
        index: &Index,
        ymaps: &[YmapInfo],
        replaced: &HashSet<u32>,
        manifests: &[ManifestFile],
        islands: Vec<Island>,
        keep: &[String],
        requested: &HashSet<u32>,
    ) -> Groups {
        let by_hash: HashMap<u32, &YmapInfo> = ymaps.iter().map(|m| (m.hash, m)).collect();
        let mut evidence = Evidence { ymaps: ymaps.len(), ..Default::default() };
        // the files a server resource streams: the rule below is not theirs
        let streamed = |m: &YmapInfo| crate::world::fivem::is_streamed(&m.path);
        evidence.streamed = [
            ymaps.iter().filter(|m| streamed(m)).count(),
            ymaps.iter().filter(|m| streamed(m) && m.flags & ON_DEMAND != 0).count(),
        ];

        // the map groups: the copies of a name say different things (one manifest ties it to its
        // collision files, another gives its hours), so they are merged; of the hours and weathers,
        // those of the best ranked manifest that gives any
        let mut groups: HashMap<u32, Group> = HashMap::new();
        let mut timed_rank: HashMap<u32, usize> = HashMap::new();
        for m in manifests {
            let Ok(x) = &m.manifest else { continue };
            for g in &x.groups {
                evidence.raw_groups += 1;
                let e = groups.entry(g.name).or_default();
                e.copies += 1;
                for b in &g.bounds {
                    if !e.bounds.contains(b) {
                        e.bounds.push(*b);
                    }
                }
                let rank = index.rank(&m.path);
                if g.flags != 0 && timed_rank.get(&g.name).is_none_or(|r| *r <= rank) {
                    timed_rank.insert(g.name, rank);
                    (e.flags, e.hours, e.weathers) = (g.flags, g.hours, g.weathers.clone());
                }
            }
        }
        for (name, g) in &groups {
            evidence.groups[0] += 1;
            if !index.copies(*name, "ymap").is_empty() {
                evidence.groups[1] += 1;
            }
            if let Some(m) = by_hash.get(name) {
                evidence.groups[2] += 1;
                evidence.groups[3] += usize::from(m.flags & ON_DEMAND != 0);
            }
            for b in &g.bounds {
                evidence.bounds[0] += 1;
                match (!index.copies(*b, "ybn").is_empty(), !index.copies(*b, "ymap").is_empty()) {
                    (true, _) => evidence.bounds[1] += 1,
                    (false, true) => evidence.bounds[2] += 1,
                    (false, false) => evidence.bounds[3] += 1,
                }
            }
            evidence.timed[0] += usize::from(g.flags & GROUP_TIME_DEPENDENT != 0);
            evidence.timed[1] += usize::from(g.flags & GROUP_WEATHER_DEPENDENT != 0);
        }
        for m in ymaps {
            if let Some(p) = by_hash.get(&m.parent) {
                match (m.flags & ON_DEMAND != 0, p.flags & ON_DEMAND != 0) {
                    (false, true) => evidence.plain_under_on_demand += 1,
                    (true, false) => evidence.on_demand_under_plain += 1,
                    _ => {}
                }
            }
        }

        let by_name: HashMap<&str, &YmapInfo> = ymaps.iter().map(|m| (stem(&m.path), m)).collect();
        for m in ymaps.iter().filter(|m| replaced.contains(&m.hash) && m.flags & ON_DEMAND == 0) {
            let mut copies = crate::world::REPLACING_PREFIXES
                .iter()
                .filter_map(|p| by_name.get(format!("{p}{}", stem(&m.path)).as_str()));
            evidence.replaced_by_on_demand +=
                usize::from(copies.any(|c| !replaced.contains(&c.hash) && c.flags & ON_DEMAND != 0));
        }

        // one island per name: the first read (the best ranked)
        let mut seen = HashSet::new();
        let islands: Vec<Island> = islands.into_iter().filter(|i| seen.insert(i.name.to_lowercase())).collect();
        for island in &islands {
            for map in &island.maps {
                evidence.island_maps[0] += 1;
                if let Some(m) = by_hash.get(&joaat(map)) {
                    evidence.island_maps[1] += 1;
                    evidence.island_maps[2] += usize::from(m.flags & ON_DEMAND != 0);
                }
            }
        }

        // the on-demand files (of the game: a file a resource streams is always drawn), and those kept
        let mut files: Vec<OnDemand> = ymaps
            .iter()
            .filter(|m| m.flags & ON_DEMAND != 0 && !streamed(m))
            .map(|m| OnDemand {
                hash: m.hash,
                name: stem(&m.path).to_string(),
                path: m.path.clone(),
                parent: m.parent,
                lod: m.flags & LOD_PARENT != 0,
                entities: m.entity_count,
                replaced: replaced.contains(&m.hash),
                kept: None,
            })
            .collect();
        files.sort_by(|a, b| (pack(&a.path), &a.name).cmp(&(pack(&b.path), &b.name)));
        let mut list: Vec<(String, usize)> = keep.iter().map(|e| (e.clone(), 0)).collect();
        for f in &mut files {
            for (entry, count) in &mut list {
                let island = islands.iter().find(|i| matches(entry, &i.name) && i.maps.contains(&f.name));
                let kept = if matches(entry, &f.name) {
                    Some(Kept::Listed(entry.clone()))
                } else {
                    island.map(|i| Kept::Island(entry.clone(), i.name.clone()))
                };
                if kept.is_some() {
                    *count += 1;
                    f.kept = f.kept.take().or(kept);
                }
            }
            if f.kept.is_none() && requested.contains(&f.hash) {
                f.kept = Some(Kept::Script);
            }
            if f.kept.is_none() && groups.get(&f.hash).and_then(|g| g.shown(HOUR, WEATHER)) == Some(true) {
                f.kept = Some(Kept::Hour);
            }
        }
        // the files a kept one hangs from
        let position: HashMap<u32, usize> = files.iter().enumerate().map(|(i, f)| (f.hash, i)).collect();
        for i in 0..files.len() {
            if files[i].kept.is_none() || matches!(files[i].kept, Some(Kept::Parent(_))) {
                continue;
            }
            let (child, mut parent) = (files[i].name.clone(), files[i].parent);
            let mut guard = 0;
            while let Some(&p) = position.get(&parent) {
                if files[p].kept.is_none() {
                    files[p].kept = Some(Kept::Parent(child.clone()));
                }
                parent = files[p].parent;
                guard += 1;
                if guard > 16 {
                    break;
                }
            }
        }
        let left_out = files.iter().filter(|f| f.kept.is_none()).map(|f| f.hash).collect();
        Groups { left_out, files, groups, islands, list, evidence }
    }
}

fn weather_name(hash: u32) -> String {
    WEATHERS.iter().find(|w| joaat(w) == hash).map(|w| w.to_string()).unwrap_or(format!("0x{hash:08X}"))
}

/// The hours of a mask as ranges: `7:00 to 18:00`.
fn hours_text(mask: u32) -> String {
    let on = |h: u32| mask >> (h % 24) & 1 == 1;
    if (0..24).all(on) {
        return "all day".to_string();
    }
    // start at an hour that is off, so that a range crossing midnight comes out whole
    let start = (0..24).find(|h| !on(*h)).unwrap_or(0);
    let mut ranges = Vec::new();
    let mut h = start;
    while h < start + 24 {
        if on(h) {
            let from = h;
            while h < start + 24 && on(h) {
                h += 1;
            }
            ranges.push(format!("{}:00 to {}:00", from % 24, h % 24));
        } else {
            h += 1;
        }
    }
    ranges.join(", ")
}

/// What `groups` prints: the sources read, the checks of the rule, the white list, then every
/// on-demand map with what becomes of it.
pub fn report(world: &World) -> String {
    let mut out = String::new();
    write_report(world, &mut out).expect("writing to a String");
    out
}

fn write_report(world: &World, out: &mut String) -> std::fmt::Result {
    let g = &world.groups;
    let e = &g.evidence;
    let index = &world.index;

    writeln!(out, "MAPS LOADED ON DEMAND")?;
    writeln!(out)?;
    writeln!(out, "Sources read")?;
    let failed: Vec<&ManifestFile> = world.manifests.iter().filter(|m| m.manifest.is_err()).collect();
    let read: Vec<&Manifest> = world.manifests.iter().filter_map(|m| m.manifest.as_ref().ok()).collect();
    let rbf = world.manifests.iter().filter(|m| m.rbf && m.manifest.is_ok()).count();
    writeln!(
        out,
        "  manifests (_manifest.ymf): {} files, {} read ({} in PSO form, {} in RBF form, the latter without groups), {} not read",
        world.manifests.len(),
        read.len(),
        read.len() - rbf,
        rbf,
        failed.len()
    )?;
    for m in &failed {
        writeln!(out, "    not read: {} ({})", m.path, m.manifest.as_ref().err().map(String::as_str).unwrap_or(""))?;
    }
    let arrays: usize = read.iter().map(|m| m.arrays.len()).sum();
    let exact: usize = read.iter().map(|m| m.arrays.iter().filter(|a| **a).count()).sum();
    writeln!(
        out,
        "    {} arrays read, each whole inside its block ({} fill it exactly, {} share it with others)",
        arrays,
        exact,
        arrays - exact
    )?;
    writeln!(
        out,
        "    {} placement file dependencies, {} interiors, {} HD texture bindings",
        read.iter().map(|m| m.imap_dependencies.len()).sum::<usize>(),
        read.iter().map(|m| m.interiors.len()).sum::<usize>(),
        read.iter().map(|m| m.hd_bindings).sum::<usize>()
    )?;
    writeln!(
        out,
        "  map groups (MapDataGroups): {} in {} manifests, {} distinct names",
        e.raw_groups,
        read.iter().filter(|m| !m.groups.is_empty()).count(),
        e.groups[0]
    )?;
    writeln!(out, "    following the hour: {}; following the weather: {}", e.timed[0], e.timed[1])?;
    for island in &g.islands {
        writeln!(out, "  island {}: {} maps ({})", island.name, island.maps.len(), island.path)?;
    }
    let on_demand = g.files.len();
    writeln!(out, "  placement files: {}; on demand (bit 0 of CMapData.flags): {}", e.ymaps, on_demand)?;
    // (said only when the server has resources: without them the report is that of the game alone)
    if e.streamed[0] > 0 {
        writeln!(
            out,
            "  of which {} streamed by the server's resources ({} bear the bit): never left out, the rule only applies to the game's files",
            e.streamed[0], e.streamed[1]
        )?;
    }
    writeln!(out)?;

    writeln!(out, "Checks of the rule \"on demand = bit 0 of CMapData.flags\"")?;
    writeln!(
        out,
        "  groups named after a placement file: {} of {} ({} among the files drawn, {} in an excluded folder)",
        e.groups[1],
        e.groups[0],
        e.groups[2],
        e.groups[1] - e.groups[2]
    )?;
    writeln!(out, "  of these {} files, with the bit: {}", e.groups[2], e.groups[3])?;
    writeln!(
        out,
        "  files tied to the groups (Bounds): {} names, of which {} collision files (.ybn), {} placement files, {} unknown",
        e.bounds[0], e.bounds[1], e.bounds[2], e.bounds[3]
    )?;
    writeln!(out, "  files without the bit hanging from a parent that has it: {}", e.plain_under_on_demand)?;
    writeln!(out, "  files with the bit hanging from a parent that does not have it: {}", e.on_demand_under_plain)?;
    writeln!(
        out,
        "  files without the bit that a multiplayer pack replaces with a copy that has it: {}",
        e.replaced_by_on_demand
    )?;
    writeln!(
        out,
        "  maps of the islands: {} names, {} known placement files, {} with the bit",
        e.island_maps[0], e.island_maps[1], e.island_maps[2]
    )?;
    let with_group = g.files.iter().filter(|f| g.groups.contains_key(&f.hash)).count();
    writeln!(
        out,
        "  on-demand files named by a group: {} of {} (the groups alone are not enough)",
        with_group, on_demand
    )?;
    writeln!(out)?;

    writeln!(out, "Keep list ([maps] keep in map.toml): {} entries", g.list.len())?;
    // the entries naming one file each are counted together; the others (an island, a pattern, a
    // name that matches nothing) are shown one by one
    let mut plain = 0;
    for (entry, count) in &g.list {
        let island = g.islands.iter().find(|i| matches(entry, &i.name));
        if *count == 1 && island.is_none() && !entry.contains('*') {
            plain += 1;
            continue;
        }
        writeln!(
            out,
            "  {:<44} {}",
            entry,
            match (count, island) {
                (0, _) => "NO on-demand map of this name".to_string(),
                (n, Some(i)) => format!("{n} files (island {})", i.name),
                (n, None) => format!("{n} file{}", if *n > 1 { "s" } else { "" }),
            }
        )?;
    }
    if plain > 0 {
        writeln!(out, "  {plain} file names, one file each (marked \"keep list\" in the detail)")?;
    }
    writeln!(out, "Maps following the hour or the weather: those shown at {HOUR}:00 in {WEATHER} weather are kept")?;
    writeln!(out)?;

    // totals
    let count = |f: &dyn Fn(&OnDemand) -> bool| -> (usize, usize) {
        let list: Vec<&OnDemand> = g.files.iter().filter(|x| f(x)).collect();
        (list.len(), list.iter().map(|x| x.entities).sum())
    };
    let (kept_n, kept_e) = count(&|f| f.kept.is_some());
    let (out_n, out_e) = count(&|f| f.kept.is_none());
    let (listed_n, listed_e) = count(&|f| matches!(f.kept, Some(Kept::Listed(_))));
    let (island_n, island_e) = count(&|f| matches!(f.kept, Some(Kept::Island(..))));
    let (script_n, script_e) = count(&|f| matches!(f.kept, Some(Kept::Script)));
    let (hour_n, hour_e) = count(&|f| matches!(f.kept, Some(Kept::Hour)));
    let (parent_n, parent_e) = count(&|f| matches!(f.kept, Some(Kept::Parent(_))));
    let (hole_n, hole_e) = count(&|f| matches!(f.kept, Some(Kept::Hole)));
    let (replaced_n, _) = count(&|f| f.kept.is_some() && f.replaced);
    writeln!(out, "Summary")?;
    writeln!(out, "  on demand: {on_demand} placement files, {} entities", kept_e + out_e)?;
    writeln!(out, "  kept:      {kept_n} files, {kept_e} entities")?;
    writeln!(out, "      named by the keep list:          {listed_n} files, {listed_e} entities")?;
    writeln!(out, "      from an island in the list:      {island_n} files, {island_e} entities")?;
    if script_n > 0 {
        writeln!(
            out,
            "      loaded by a script of a server resource (RequestIpl): {script_n} files, {script_e} entities"
        )?;
    }
    writeln!(out, "      at {HOUR}:00 in {WEATHER} weather:  {hour_n} files, {hour_e} entities")?;
    writeln!(out, "      parents of a kept file:          {parent_n} files, {parent_e} entities")?;
    if hole_n > 0 {
        writeln!(out, "      that close a hole in the ground: {hole_n} files, {hole_e} entities")?;
    }
    writeln!(out, "      (of which {replaced_n} replaced by the copy of a multiplayer pack, so not drawn)")?;
    writeln!(out, "  left out:  {out_n} files, {out_e} entities")?;
    writeln!(out)?;

    // by pack
    let mut packs: BTreeMap<String, [usize; 4]> = BTreeMap::new();
    for f in &g.files {
        let p = packs.entry(pack(&f.path)).or_default();
        let k = if f.kept.is_some() { 0 } else { 2 };
        p[k] += 1;
        p[k + 1] += f.entities;
    }
    writeln!(out, "By pack                            kept (entities)   left out (entities)")?;
    for (name, c) in &packs {
        writeln!(out, "  {:<30} {:>6} {:>10}    {:>7} {:>10}", name, c[0], c[1], c[2], c[3])?;
    }
    writeln!(out)?;

    // every file
    writeln!(out, "Detail: every on-demand file (LOD: other files hang from it)")?;
    let mut last = String::new();
    for f in &g.files {
        let p = pack(&f.path);
        if p != last {
            writeln!(out, "  — {p}")?;
            last = p;
        }
        let state = match &f.kept {
            None => "left out".to_string(),
            Some(Kept::Listed(entry)) if entry.eq_ignore_ascii_case(&f.name) => "KEPT  keep list".to_string(),
            Some(Kept::Listed(entry)) => format!("KEPT  keep list ({entry})"),
            Some(Kept::Island(_, island)) => format!("KEPT  island {island}"),
            Some(Kept::Script) => "KEPT  RequestIpl of a resource".to_string(),
            Some(Kept::Hour) => format!("KEPT  {HOUR}:00, {WEATHER}"),
            Some(Kept::Parent(child)) => format!("KEPT  parent of {child}"),
            Some(Kept::Hole) => "KEPT  closes a hole in the ground".to_string(),
        };
        let mut notes = Vec::new();
        if f.lod {
            notes.push("LOD".to_string());
        }
        if f.replaced {
            notes.push("replaced by the copy of a pack".to_string());
        }
        if let Some(group) = g.groups.get(&f.hash) {
            let mut text = format!(
                "group: {} collision file{}",
                group.bounds.len(),
                if group.bounds.len() == 1 { "" } else { "s" }
            );
            if group.flags & GROUP_TIME_DEPENDENT != 0 {
                write!(text, ", {}", hours_text(group.hours))?;
            }
            if group.flags & GROUP_WEATHER_DEPENDENT != 0 {
                write!(
                    text,
                    ", weather {}",
                    group.weathers.iter().map(|w| weather_name(*w)).collect::<Vec<_>>().join(" ")
                )?;
            }
            notes.push(text);
        }
        if f.parent != 0 {
            notes.push(format!("parent {}", index.copies(f.parent, "ymap").first().map(|p| stem(p)).unwrap_or("?")));
        }
        let plural = if f.entities == 1 { "y  " } else { "ies" };
        writeln!(out, "    {:<52} {:>5} entit{plural}  {:<34} {}", f.name, f.entities, state, notes.join("; "))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(matches("hei_carrier", "HEI_Carrier"));
        assert!(!matches("hei_carrier", "hei_carrier_lod"));
        assert!(matches("h4_*", "h4_islandx_terrain_01"));
        assert!(!matches("h4_*", "xh4_beach"));
        assert!(matches("*", "anything") && matches("*_lod", "farm_lod") && matches("a*b*c", "a1b22c"));
        assert!(!matches("a*b*c", "a1b22") && !matches("", "x") && matches("", ""));
    }

    #[test]
    fn island_file() {
        let text = "<?xml version=\"1.0\"?>\n<CIslandHopperData>\n <Name>HeistIsland</Name>\n <Cullbox>HeistIsland</Cullbox>\n <IPLsToEnable>\n  <Item>h4_beach</Item>\n  <Item>H4_islandx_terrain_01</Item>\n  <Item>h4_beach</Item>\n </IPLsToEnable>\n <IMAPsToPreempt>\n  <Item>h4_grass_0</Item>\n </IMAPsToPreempt>\n</CIslandHopperData>";
        let island = Island::parse("x/islandhopper.meta", text).unwrap();
        assert_eq!(island.name, "HeistIsland");
        assert_eq!(island.maps, ["h4_beach", "h4_islandx_terrain_01"]);
        assert!(Island::parse("x", "<Other/>").is_none());
    }

    #[test]
    fn hours_and_weather() {
        // the day props of Vespucci beach: 7 h to 18 h, three weathers
        let day = Group {
            flags: 3,
            hours: 0x03FF80,
            weathers: vec![joaat("EXTRASUNNY"), joaat("CLEAR"), joaat("SMOG")],
            ..Default::default()
        };
        assert_eq!(day.shown(12, "EXTRASUNNY"), Some(true));
        assert_eq!(day.shown(6, "EXTRASUNNY"), Some(false));
        assert_eq!(day.shown(18, "EXTRASUNNY"), Some(false));
        assert_eq!(day.shown(12, "RAIN"), Some(false));
        // hours only; then a group no hour or weather drives
        let stand = Group { flags: 1, hours: 0x0FFFC0, ..Default::default() };
        assert_eq!(stand.shown(12, "RAIN"), Some(true));
        assert_eq!(Group::default().shown(12, "EXTRASUNNY"), None);
        assert_eq!(hours_text(0x03FF80), "7:00 to 18:00");
        assert_eq!(hours_text(0x80001F), "23:00 to 5:00");
        assert_eq!(hours_text(0xFFFFFF), "all day");
        assert_eq!(weather_name(joaat("extrasunny")), "EXTRASUNNY");
    }

    fn ymap(name: &str, parent: &str, flags: u32) -> YmapInfo {
        YmapInfo {
            path: format!("x64a.rpf/levels/gta5/{name}.ymap"),
            hash: joaat(name),
            entities_min: [0.0; 3],
            entities_max: [0.0; 3],
            entity_count: 1,
            parent: if parent.is_empty() { 0 } else { joaat(parent) },
            flags,
        }
    }

    #[test]
    fn what_is_kept() {
        let ymaps = [
            ymap("city", "", 0),
            ymap("yacht", "yacht_lod", 1),
            ymap("yacht_lod", "yacht_slod", 3),
            ymap("yacht_slod", "", 3),
            ymap("ufo", "ufo_lod", 1),
            ymap("ufo_lod", "", 3),
            ymap("isle_terrain", "", 1),
            ymap("isle_gate_broken", "", 1),
            ymap("beach_day", "", 1),
            ymap("beach_night", "", 1),
        ];
        let group = |name: &str, hours: u32| crate::formats::pso::MapGroup {
            name: joaat(name),
            bounds: vec![],
            flags: 1,
            weathers: vec![],
            hours,
        };
        let manifests = [ManifestFile {
            path: "x64a.rpf/levels/gta5/_manifest.ymf".into(),
            rbf: false,
            manifest: Ok(Manifest {
                groups: vec![group("beach_day", 0x03FF80), group("beach_night", 0x80001F)],
                ..Default::default()
            }),
        }];
        let islands =
            vec![Island { name: "Isle".into(), path: "islandhopper.meta".into(), maps: vec!["isle_terrain".into()] }];
        let index = Index { files: HashMap::new(), dlcs: Vec::new(), base_first: false };
        let keep = ["YACHT".to_string(), "isle".to_string(), "nothing_*".to_string()];
        let g = Groups::build(&index, &ymaps, &HashSet::new(), &manifests, islands, &keep, &HashSet::new());
        let state = |name: &str| g.files.iter().find(|f| f.name == name).map(|f| f.kept.clone());
        assert_eq!(state("city"), None, "not on demand: not in the list at all");
        assert_eq!(state("yacht"), Some(Some(Kept::Listed("YACHT".into()))));
        assert_eq!(state("yacht_lod"), Some(Some(Kept::Parent("yacht".into()))));
        assert_eq!(state("yacht_slod"), Some(Some(Kept::Parent("yacht".into()))));
        assert_eq!(state("ufo"), Some(None));
        assert_eq!(state("ufo_lod"), Some(None));
        assert_eq!(state("isle_terrain"), Some(Some(Kept::Island("isle".into(), "Isle".into()))));
        assert_eq!(state("isle_gate_broken"), Some(None), "not in the island's list");
        assert_eq!(state("beach_day"), Some(Some(Kept::Hour)));
        assert_eq!(state("beach_night"), Some(None));
        let mut left: Vec<u32> = g.left_out.iter().copied().collect();
        left.sort();
        let mut want = vec![joaat("ufo"), joaat("ufo_lod"), joaat("isle_gate_broken"), joaat("beach_night")];
        want.sort();
        assert_eq!(left, want);
        assert_eq!(g.list, [("YACHT".to_string(), 1), ("isle".to_string(), 1), ("nothing_*".to_string(), 0)]);
        // "*" keeps everything
        let all =
            Groups::build(&index, &ymaps, &HashSet::new(), &manifests, Vec::new(), &["*".to_string()], &HashSet::new());
        assert!(all.left_out.is_empty());
    }

    /// With the server's resources: a map a client script loads by name is kept (and what it hangs
    /// from), and a placement file a resource streams is never on the list, whatever its bit.
    #[test]
    fn what_the_server_resources_change() {
        let streamed = |name: &str, flags: u32| YmapInfo {
            path: format!("resource:000001:c:/srv/res/stream/{name}.ymap"),
            ..ymap(name, "", flags)
        };
        let ymaps = [
            ymap("city", "", 0),
            ymap("ufo", "ufo_lod", 1),
            ymap("ufo_lod", "", 3),
            ymap("yacht", "", 1),
            streamed("new_island", 1),
            streamed("new_town", 0),
        ];
        let index = Index { files: HashMap::new(), dlcs: Vec::new(), base_first: false };
        let requested = HashSet::from([joaat("ufo"), joaat("unknown")]);
        let g = Groups::build(&index, &ymaps, &HashSet::new(), &[], Vec::new(), &[], &requested);
        let state = |name: &str| g.files.iter().find(|f| f.name == name).map(|f| f.kept.clone());
        assert_eq!(state("ufo"), Some(Some(Kept::Script)));
        assert_eq!(state("ufo_lod"), Some(Some(Kept::Parent("ufo".into()))));
        assert_eq!(state("yacht"), Some(None));
        assert_eq!(state("new_island"), None, "streamed by a resource: not on the list");
        assert_eq!(g.left_out, HashSet::from([joaat("yacht")]));
        assert_eq!(g.evidence.streamed, [2, 1]);
    }
}
