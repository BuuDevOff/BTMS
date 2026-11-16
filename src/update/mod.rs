//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! `update [<folder>] (--zone x1,y1,x2,y2 | --changes | --list | --record) [tile
//! options]`: a part of a map already drawn, drawn again in the map's own folder (not a
//! preview), after a resource was added, changed or removed.
//!
//! ```text
//! btms update [<folder>] --zone x1,y1,x2,y2   the blocks of the zone, rounded outwards
//! btms update [<folder>] --changes            the ground of every resource added, changed or removed
//! btms update [<folder>] --list               the same, told only: nothing drawn nor written
//! btms update [<folder>] --record             the resources as they are, recorded without drawing
//!     [--threads …] [--renderer …] [--shading …] [--chain …] [--ram …] [--vram …]
//!     [--format …] [--quality …] [--dry-run] [--config …] [--game …] [lighting options]
//! ```
//!
//! The folder (`output/tiles` by default) holds a map drawn by `tiles` (its `tiles.json`,
//! finished): the blocks are drawn as its render drew them (`tiles::again`), in its own format, and
//! its `tiles.json` stays the map's, patched ([`patch_manifest`]: `bounds`, `blocks`, `resources`,
//! an entry of `updates`). One writer at a time ([`lock`]).
//!
//! What the server's resources were when the map was drawn is in `<folder>/resources.json`
//! ([`record`]), written at the end of `tiles` (whole map), of `--changes` and by `--record`.
//! `--changes` compares it with the resources as they are: the ground of each resource added,
//! changed or removed ([`ground`]: its own and the one it had), with the maps loaded on request that
//! the holes of the ground keep otherwise, widened by a block (the light's shadows, the last holes),
//! is drawn again, then the record is written anew.

mod ground;
pub mod lock;
pub mod record;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rage_rpf::GtaKeys;
use serde_json::{Value, json};

use crate::progress::ResourceChange;
use crate::tiles::Placements;
use crate::tiles::again::{self, Done, Plan, Redraw};
use crate::tiles::format;
use crate::world::World;
use ground::{Blocks, Ground};
use record::{Change, Entry, Print, Record, State};

/// What `update` is asked to do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Zone([f32; 4]),
    Changes,
    List,
    Record,
}

/// The options of `update`: those of `tiles` (`zone` left out: it is the mode's), and the mode.
pub struct Parsed {
    pub tiles: crate::tiles::Options,
    pub mode: Mode,
    /// `--format` or `--quality` given.
    pub format_asked: bool,
}

/// The options of `tiles` that `update` takes too (those with a value, then the others).
const WITH_VALUE: [&str; 6] = ["--zone", "--threads", "--ram", "--vram", "--format", "--quality"];
const ALONE: [&str; 4] = ["--changes", "--list", "--record", "--dry-run"];

/// Reads the arguments that follow `update` (`--game`, `--config`, `--renderer`, `--shading`,
/// `--chain`, `--profile` and the lighting options already taken out by `cli`).
pub fn parse(args: &[String]) -> Result<Parsed> {
    let mut rest: Vec<String> = args.to_vec();
    let mut i = 0;
    let mut folders = Vec::new();
    while i < rest.len() {
        let a = &rest[i];
        if WITH_VALUE.contains(&a.as_str()) {
            i += 2;
        } else if ALONE.contains(&a.as_str()) {
            i += 1;
        } else if a.starts_with("--") {
            bail!(
                "update: unknown option {a} (those of update: --zone, --changes, --list, --record, --threads, --renderer, --shading, --chain, --ram, --vram, --format, --quality, --dry-run, --profile, --config, --game and the lighting options)"
            );
        } else {
            folders.push(a.clone());
            i += 1;
        }
    }
    if folders.len() > 1 {
        let all: Vec<String> = folders.iter().map(|f| format!("\"{f}\"")).collect();
        bail!("update: one folder expected, not {}", all.join(" and "));
    }
    let mut modes = Vec::new();
    for (flag, mode) in [("--changes", Mode::Changes), ("--list", Mode::List), ("--record", Mode::Record)] {
        if let Some(at) = rest.iter().position(|a| a == flag) {
            rest.remove(at);
            modes.push((flag, mode));
            while let Some(again) = rest.iter().position(|a| a == flag) {
                rest.remove(again);
            }
        }
    }
    let format_asked = rest.iter().any(|a| a == "--format" || a == "--quality");
    let mut tiles_args = vec!["tiles".to_string()];
    tiles_args.extend(rest);
    let mut tiles = crate::tiles::options::parse(&mut tiles_args)?;
    if let Some(z) = tiles.zone.take() {
        modes.push(("--zone", Mode::Zone(z)));
    }
    match modes.as_slice() {
        [(_, mode)] => Ok(Parsed { tiles, mode: *mode, format_asked }),
        [] => bail!(
            "update: say what to do: --zone x1,y1,x2,y2 (a zone), --changes (what the resources changed), --list (say it without drawing anything) or --record (record the state of the resources without drawing)"
        ),
        many => {
            bail!("update: one thing at a time, not {}", many.iter().map(|(f, _)| *f).collect::<Vec<_>>().join(" and "))
        }
    }
}

/// `--dry-run`: what the update would use, told; nothing read nor drawn.
pub fn dry_run(p: &Parsed) -> Result<()> {
    // the line says the zone asked and the folder's own format (its `tiles.json` alone is read)
    let o = &p.tiles;
    let zone = match p.mode {
        Mode::Zone(z) => ground::around(&ground::zone_blocks(z)),
        _ => None,
    };
    let format = format::from_manifest(&o.out).unwrap_or(o.format);
    let effort = format::effort_from_manifest(&o.out);
    let said = crate::tiles::Options {
        out: o.out.clone(),
        zone,
        threads: o.threads,
        ram_budget: o.ram_budget,
        card_budget: o.card_budget,
        dry_run: true,
        format,
        effort,
    };
    crate::tiles::options::resolve(&said).tell(&said);
    let what = match p.mode {
        Mode::Zone(z) => format!("--zone {},{},{},{}", z[0], z[1], z[2], z[3]),
        Mode::Changes => "--changes".into(),
        Mode::List => "--list".into(),
        Mode::Record => "--record".into(),
    };
    println!("update: {what} of the map {} (dry run: nothing is read or drawn)", p.tiles.out.display());
    Ok(())
}

/// The map in a folder: its own zone (None for the whole map) and format, from its `tiles.json`.
struct Map {
    zone: Option<[f32; 4]>,
    format: format::Format,
}

fn map(out: &Path, finished: bool) -> Result<Map> {
    let none =
        || anyhow::anyhow!("no map in {}: render the whole map first (btms tiles {})", out.display(), out.display());
    let text = std::fs::read_to_string(out.join("tiles.json")).map_err(|_| none())?;
    let manifest: Value = serde_json::from_str(&text).map_err(|_| none())?;
    let format = format::from_manifest(out).ok_or_else(none)?;
    if finished && manifest.get("complete") != Some(&Value::Bool(true)) {
        bail!(
            "the map in {} is not finished (tiles.json: \"complete\" is not true): a render is still writing it, or it stopped before the end; wait for it to finish, or render the whole map again",
            out.display()
        );
    }
    let zone = manifest.get("zone").and_then(|z| serde_json::from_value::<[f32; 4]>(z.clone()).ok());
    Ok(Map { zone, format })
}

/// Blocks of margin around the ground of a resource: one at least, more when the shadows of the
/// light reach farther than a block.
fn margin() -> i32 {
    let metres =
        crate::render::shade::margin_metres(crate::tiles::side(crate::tiles::ZMAX) / crate::tiles::TILE as f32);
    ((metres / ground::BLOCK).ceil() as i32).max(1)
}

pub fn run(game: &Path, keys: &GtaKeys, args: &[String]) -> Result<()> {
    let p = parse(args)?;
    // (read when the card is opened)
    if let Some(bytes) = p.tiles.card_budget {
        crate::render::gpu::set_texture_budget(bytes);
    }
    if p.tiles.dry_run {
        return dry_run(&p);
    }
    let out = p.tiles.out.clone();
    let m = map(&out, p.mode != Mode::List)?;
    let mut opts = p.tiles;
    if p.format_asked && opts.format != m.format {
        println!(
            "format: the map's own, {} (the format asked, {}, is not taken: an update keeps the map's format)",
            m.format.label(),
            opts.format.label()
        );
    }
    opts.format = m.format;
    // the folder's own encoding effort too (`tiles.json`), so that the tiles drawn again are
    // made as the rest of the map
    opts.effort = format::effort_from_manifest(&out);
    opts.zone = m.zone;
    let _lock = match p.mode {
        Mode::List => None,
        _ => Some(lock::Lock::take(&out, "update")?),
    };
    match p.mode {
        Mode::Zone(z) => {
            let blocks: Blocks = ground::zone_blocks(z).into_iter().filter(|b| ground::in_zone(m.zone, *b)).collect();
            if blocks.is_empty() {
                let within = m
                    .zone
                    .map(|c| format!(" (its zone: x {} to {}, y {} to {})", c[0], c[2], c[1], c[3]))
                    .unwrap_or_default();
                bail!("--zone {},{},{},{}: outside the map in {}{within}", z[0], z[1], z[2], z[3], out.display());
            }
            if let Some(remark) = record::read(&out).ok().flatten().as_ref().and_then(record::settings_changed) {
                println!("warning: {remark}; the zone is drawn with the current settings");
            }
            let mut plan = ZonePlan { out: &out, blocks, had: HashMap::new() };
            again::redraw(game, keys, &opts, &mut plan)
        }
        Mode::Changes => changes(game, keys, &opts, &m),
        Mode::List => list(game, keys, &opts, &m),
        Mode::Record => record_resources(game, keys, &opts, &m),
    }
}

/// The record of the folder, or the refusal that explains `--record` (and the event `changes`
/// with `known: false`).
fn record_or_refuse(out: &Path) -> Result<Record> {
    match record::read(out)? {
        Some(r) => Ok(r),
        None => {
            let why = format!(
                "{} has no record of the resources ({}): the map was drawn before records existed, or by tiles --zone. If it is up to date, record the current state without drawing anything with \"update {} --record\"; otherwise render the whole map again (tiles records it at the end)",
                out.display(),
                record::FILE,
                out.display()
            );
            crate::progress::changes(false, Vec::new(), Vec::new(), 0, Some(why.clone()));
            bail!("{why}")
        }
    }
}

/// `--zone`: the blocks of the zone, nothing else decided.
struct ZonePlan<'a> {
    out: &'a Path,
    blocks: Blocks,
    had: HashMap<(i32, i32), bool>,
}

impl Plan for ZonePlan<'_> {
    fn defers(&self) -> bool {
        false
    }

    fn zone(&self) -> Option<[f32; 4]> {
        ground::around(&self.blocks)
    }

    fn blocks(&mut self, _: &World, _: &Placements) -> Result<Option<Redraw>> {
        self.had = self.blocks.iter().map(|&(bx, by)| ((bx, by), again::has_tile(self.out, 7, bx, by))).collect();
        let zone = ground::around(&self.blocks).context("empty zone")?;
        Ok(Some(Redraw { blocks: self.blocks.clone(), zone }))
    }

    fn finish(&mut self, _: &World, _: &Placements, done: &Done) -> Result<()> {
        patch_manifest(self.out, &self.blocks, &self.had, json!({"by": "zone"}), done)
    }
}

/// The maps loaded on request that the render draws (names, in order).
fn on_request(world: &World) -> Vec<String> {
    let mut names: Vec<String> = world
        .groups
        .files
        .iter()
        .filter(|f| !world.groups.left_out.contains(&f.hash) && !world.replaced.contains(&f.hash))
        .map(|f| f.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The grounds of resources `which` of the server as they are now (see `ground`): their own, then
/// where what they stream is placed (`entities`: every entity the tiles draw, read here when not
/// given and needed).
fn grounds_now(
    game: &Path,
    keys: &GtaKeys,
    world: &World,
    zone: Option<[f32; 4]>,
    entities: Option<&[crate::formats::ymap::Entity]>,
    which: &[usize],
) -> Result<HashMap<usize, Ground>> {
    let server = crate::world::fivem::server();
    let names = ground::Names::new(world);
    let mut found: Vec<(usize, Ground, HashSet<u32>)> = which
        .iter()
        .map(|&r| {
            let (g, affected) = ground::own(game, keys, world, server, &names, r);
            (r, g, affected)
        })
        .collect();
    if found.iter().any(|(_, _, a)| !a.is_empty()) {
        let read;
        let entities = match entities {
            Some(e) => e,
            None => {
                crate::progress::log(
                    "update: reading the map's placements (where the models and textures of the resources are placed)…",
                );
                read = crate::tiles::placements(game, keys, world, zone)?;
                &read.entities
            }
        };
        for (_, g, affected) in &mut found {
            ground::placed_elsewhere(g, affected, world, entities);
        }
    }
    Ok(found
        .into_iter()
        .map(|(r, mut g, _)| {
            ground::finish(&mut g);
            (r, g)
        })
        .collect())
}

/// An entry of the record for resource `r` as it is now.
fn entry(print: &Print, g: &Ground) -> Entry {
    Entry {
        name: print.name.clone(),
        folder: print.dir.display().to_string(),
        fingerprint: print.digest.clone(),
        files: print.files,
        zone: g.zone,
        blocks: ground::rectangles(&g.blocks),
        readable: g.unreadable == 0,
        reason: g.reason.clone(),
    }
}

/// What changed, as the event and the lines tell it, and the blocks to draw again.
struct Told {
    resources: Vec<ResourceChange>,
    blocks: Blocks,
    /// Maps loaded on request drawn now and not before, or the other way round.
    on_request: Vec<String>,
}

/// The blocks to draw again for `changes`: the ground each had and has, the maps loaded on request
/// that changed (`on_request`: names of maps drawn before or now but not both), widened by the
/// margin, within the map's zone.
fn told(
    record: &Record,
    prints: &[Print],
    changes: &[Change],
    now: &HashMap<usize, Ground>,
    world: Option<&World>,
    on_request: &[String],
    zone: Option<[f32; 4]>,
) -> Told {
    let mut union = Blocks::new();
    let mut resources = Vec::new();
    for c in changes {
        let old = c.old.map(|i| &record.resources[i]);
        let new = c.now.and_then(|i| now.get(&i));
        if let Some(e) = old {
            union.extend(ground::from_rectangles(&e.blocks));
        }
        if let Some(g) = new {
            union.extend(g.blocks.iter().copied());
        }
        let (zone, reason) = match (new, old) {
            (Some(g), _) => (g.zone, g.reason.clone()),
            (None, Some(e)) => (e.zone, e.reason.clone()),
            (None, None) => (None, None),
        };
        let folder = match (c.now, old) {
            (Some(i), _) => prints[i].dir.display().to_string(),
            (None, Some(e)) => e.folder.clone(),
            (None, None) => String::new(),
        };
        resources.push(ResourceChange { name: c.name.clone(), folder, state: c.state.word().into(), zone, reason });
    }
    // the maps loaded on request that are drawn now and were not, or the other way round
    let mut changed = Vec::new();
    if let (Some(before), Some(world)) = (record.on_demand.as_ref(), world) {
        let before: BTreeSet<&str> = before.iter().map(String::as_str).collect();
        let now: BTreeSet<&str> = on_request.iter().map(String::as_str).collect();
        for name in before.symmetric_difference(&now) {
            let hash = crate::formats::resource::joaat(name);
            for m in world.ymaps.iter().filter(|m| m.hash == hash) {
                let mut g = Ground::default();
                g.add(m.entities_min, m.entities_max);
                union.extend(g.blocks);
            }
            changed.push(name.to_string());
        }
    }
    let blocks: Blocks = ground::widen(&union, margin()).into_iter().filter(|b| ground::in_zone(zone, *b)).collect();
    Told { resources, blocks, on_request: changed }
}

/// Says what changed and sends the event `changes`.
fn tell(record: &Record, t: &Told, remark: Option<String>) {
    println!("changes since {} ({}): {} resource(s)", record.date, record.by, t.resources.len());
    for r in &t.resources {
        let zone = match r.zone {
            Some(z) => format!("zone x {:.0} to {:.0}, y {:.0} to {:.0}", z[0], z[2], z[1], z[3]),
            None => "unknown zone".to_string(),
        };
        let state = match r.state.as_str() {
            "added" => State::Added,
            "changed" => State::Changed,
            _ => State::Removed,
        };
        println!(
            "  {:<9} {}: {zone}{}",
            state.label(),
            r.name,
            r.reason.as_deref().map(|w| format!(" ({w})")).unwrap_or_default()
        );
    }
    if !t.on_request.is_empty() {
        println!(
            "  on-demand maps drawn differently than before (holes in the ground, RequestIpl): {}",
            t.on_request.join(", ")
        );
    }
    let rects: Vec<[f32; 4]> = ground::rectangles(&t.blocks).into_iter().map(ground::metres).collect();
    if t.blocks.is_empty() {
        println!("to draw again: nothing");
    } else {
        println!(
            "to draw again: {} block(s) of 128 m, a margin of {} block(s) included, in {} rectangle(s): {}",
            t.blocks.len(),
            margin(),
            rects.len(),
            rects
                .iter()
                .take(8)
                .map(|z| format!("{:.0},{:.0},{:.0},{:.0}", z[0], z[1], z[2], z[3]))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    if let Some(r) = &remark {
        println!("note: {r}");
    }
    crate::progress::changes(true, t.resources.clone(), rects, t.blocks.len(), remark);
}

/// `--list`: the changes told, nothing drawn nor written.
fn list(game: &Path, keys: &GtaKeys, opts: &crate::tiles::Options, m: &Map) -> Result<()> {
    let record = record_or_refuse(&opts.out)?;
    let server = crate::world::fivem::server();
    let prints = record::prints(server);
    let changes = record::diff(&record, &prints);
    let remark = record::settings_changed(&record);
    if changes.is_empty() {
        println!("changes since {} ({}): no resource added, changed or removed", record.date, record.by);
        if let Some(r) = &remark {
            println!("note: {r}");
        }
        crate::progress::changes(true, Vec::new(), Vec::new(), 0, remark);
        return Ok(());
    }
    let which: Vec<usize> = changes.iter().filter_map(|c| c.now).collect();
    let (now, world) = if which.is_empty() {
        (HashMap::new(), None)
    } else {
        crate::progress::log("update: reading the world (for the zone of the resources added or changed)…");
        let world = World::load(game, keys, false)?;
        (grounds_now(game, keys, &world, m.zone, None, &which)?, Some(world))
    };
    // (the holes of the ground are not looked for here: the maps loaded on request are those of
    // the white list, compared only by --changes)
    let t = told(&record, &prints, &changes, &now, None, &[], m.zone);
    drop(world);
    tell(&record, &t, remark);
    Ok(())
}

/// `--record`: the resources as they are recorded, nothing drawn.
fn record_resources(game: &Path, keys: &GtaKeys, opts: &crate::tiles::Options, m: &Map) -> Result<()> {
    let before = record::read(&opts.out)?;
    let server = crate::world::fivem::server();
    let prints = record::prints(server);
    crate::progress::log("update: reading the world (for the zone of each resource)…");
    let mut world = World::load(game, keys, false)?;
    let all: Vec<usize> = (0..prints.len()).collect();
    let now = grounds_now(game, keys, &world, m.zone, None, &all)?;
    // the holes of the ground looked for once, as the render of the map does (over its zone): the
    // maps loaded on request they keep are recorded, so that an update takes them back instead of
    // looking over the whole map again
    if crate::config::get().maps.fill_holes && !world.groups.left_out.is_empty() {
        crate::progress::log(
            "holes: looking for holes in the ground that an on-demand map closes (once, for the updates)…",
        );
    }
    let threads = std::thread::available_parallelism().map(usize::from).unwrap_or(8);
    crate::world::holes::fill(game, keys, &mut world, m.zone, threads, opts.ram_budget)?;
    let record = Record {
        format: record::FORMAT.into(),
        date: record::now(),
        by: "update --record".into(),
        settings: record::settings(),
        folders: record::folder_names(&crate::config::get().resources.folders),
        on_demand: Some(on_request(&world)),
        resources: prints.iter().enumerate().map(|(i, p)| entry(p, &now[&i])).collect(),
    };
    // what changed since the record before, told
    match &before {
        Some(b) => {
            let changes = record::diff(b, &prints);
            let t = told(b, &prints, &changes, &now, None, &[], m.zone);
            let note = format!(
                "state of the resources recorded without drawing anything{}",
                if changes.is_empty() {
                    String::new()
                } else {
                    format!(": the {} block(s) of these changes are not drawn again", t.blocks.len())
                }
            );
            let remark = match record::settings_changed(b) {
                Some(r) => format!("{note}; {r} (the current settings are recorded)"),
                None => note,
            };
            tell(b, &t, Some(remark));
        }
        None => crate::progress::changes(
            false,
            Vec::new(),
            Vec::new(),
            0,
            Some("no record before this one: the current state is recorded without drawing anything".into()),
        ),
    }
    record::write(&opts.out, &record)?;
    for (p, e) in prints.iter().zip(&record.resources) {
        println!(
            "  {}: {}{}",
            p.name,
            match e.zone {
                Some(z) => format!(
                    "zone x {:.0} to {:.0}, y {:.0} to {:.0} ({} block(s))",
                    z[0],
                    z[2],
                    z[1],
                    z[3],
                    ground::from_rectangles(&e.blocks).len()
                ),
                None => "unknown zone".into(),
            },
            e.reason.as_deref().map(|w| format!(" ({w})")).unwrap_or_default()
        );
    }
    println!("record: {} resource(s) recorded in {}", record.resources.len(), opts.out.join(record::FILE).display());
    Ok(())
}

/// `--changes`: the ground of the resources added, changed or removed, drawn again.
fn changes(game: &Path, keys: &GtaKeys, opts: &crate::tiles::Options, m: &Map) -> Result<()> {
    let record = record_or_refuse(&opts.out)?;
    let server = crate::world::fivem::server();
    let prints = record::prints(server);
    let changes = record::diff(&record, &prints);
    let remark = record::settings_changed(&record);
    if changes.is_empty() {
        println!(
            "changes since {} ({}): no resource added, changed or removed: nothing to draw",
            record.date, record.by
        );
        if let Some(r) = &remark {
            println!("note: {r}");
        }
        crate::progress::changes(true, Vec::new(), Vec::new(), 0, remark);
        return Ok(());
    }
    // the map was drawn with other settings: what is drawn now would not join what is around it
    if let Some(r) = remark {
        let resources = changes
            .iter()
            .map(|c| {
                let old = c.old.map(|i| &record.resources[i]);
                ResourceChange {
                    name: c.name.clone(),
                    folder: c
                        .now
                        .map(|i| prints[i].dir.display().to_string())
                        .or(old.map(|e| e.folder.clone()))
                        .unwrap_or_default(),
                    state: c.state.word().into(),
                    zone: old.and_then(|e| e.zone),
                    reason: None,
                }
            })
            .collect();
        let why = format!(
            "{r}; nothing is drawn (\"update --record\" records the current settings and resources without drawing)"
        );
        crate::progress::changes(true, resources, Vec::new(), 0, Some(why.clone()));
        bail!("{why}");
    }
    crate::progress::log(&format!(
        "update: {} resource(s) added, changed or removed: {}",
        changes.len(),
        changes.iter().map(|c| format!("{} ({})", c.name, c.state.label())).collect::<Vec<_>>().join(", ")
    ));
    let mut plan = ChangesPlan {
        game,
        keys,
        out: &opts.out,
        zone: m.zone,
        record,
        prints,
        changes,
        now: HashMap::new(),
        blocks: Blocks::new(),
        had: HashMap::new(),
        t0: Instant::now(),
    };
    again::redraw(game, keys, opts, &mut plan)
}

/// `--changes` once the world is read.
struct ChangesPlan<'a> {
    game: &'a Path,
    keys: &'a GtaKeys,
    out: &'a Path,
    zone: Option<[f32; 4]>,
    record: Record,
    prints: Vec<Print>,
    changes: Vec<Change>,
    now: HashMap<usize, Ground>,
    blocks: Blocks,
    had: HashMap<(i32, i32), bool>,
    t0: Instant,
}

impl Plan for ChangesPlan<'_> {
    fn defers(&self) -> bool {
        true
    }

    fn zone(&self) -> Option<[f32; 4]> {
        None
    }

    fn blocks(&mut self, world: &World, places: &Placements) -> Result<Option<Redraw>> {
        let which: Vec<usize> = self.changes.iter().filter_map(|c| c.now).collect();
        self.now = grounds_now(self.game, self.keys, world, self.zone, Some(&places.entities), &which)?;
        let t = told(&self.record, &self.prints, &self.changes, &self.now, Some(world), &on_request(world), self.zone);
        tell(&self.record, &t, None);
        self.blocks = t.blocks;
        if self.blocks.is_empty() {
            // nothing on the map to draw again: the new state is recorded all the same
            self.write(world)?;
            crate::progress::end(0, 0, self.t0.elapsed().as_secs_f64());
            return Ok(None);
        }
        self.had = self.blocks.iter().map(|&(bx, by)| ((bx, by), again::has_tile(self.out, 7, bx, by))).collect();
        let zone = ground::around(&self.blocks).context("no block")?;
        Ok(Some(Redraw { blocks: self.blocks.clone(), zone }))
    }

    fn finish(&mut self, world: &World, _: &Placements, done: &Done) -> Result<()> {
        let names: Vec<&str> = self.changes.iter().map(|c| c.name.as_str()).collect();
        patch_manifest(self.out, &self.blocks, &self.had, json!({"by": "changes", "resources": names}), done)?;
        if done.errors > 0 {
            println!(
                "record: not updated, {} block(s) failed: run update --changes again (the same zones will be drawn again)",
                done.errors
            );
            return Ok(());
        }
        self.write(world)
    }
}

impl ChangesPlan<'_> {
    /// The record as the resources are now: the ground of those changed as just found, of the
    /// others as recorded.
    fn write(&self, world: &World) -> Result<()> {
        let by_name: HashMap<&str, &Entry> = self.record.resources.iter().map(|e| (e.name.as_str(), e)).collect();
        let resources = self
            .prints
            .iter()
            .enumerate()
            .map(|(i, p)| match (self.now.get(&i), by_name.get(p.name.as_str())) {
                (Some(g), _) => entry(p, g),
                (None, Some(e)) => Entry { folder: p.dir.display().to_string(), ..(*e).clone() },
                // (not changed, and not recorded: cannot be)
                (None, None) => entry(p, &Ground::default()),
            })
            .collect();
        let record = Record {
            format: record::FORMAT.into(),
            date: record::now(),
            by: "update --changes".into(),
            settings: self.record.settings.clone(),
            folders: record::folder_names(&crate::config::get().resources.folders),
            on_demand: Some(on_request(world)),
            resources,
        };
        record::write(self.out, &record)?;
        println!(
            "record: {} resource(s) recorded in {}",
            record.resources.len(),
            self.out.join(record::FILE).display()
        );
        Ok(())
    }
}

/// The map's own `tiles.json` after blocks were drawn again: the ground its zoom 7 tiles cover
/// (`bounds`), its count of blocks (those that gained or lost their tiles), the server's resources,
/// and an entry of `updates` (date, zones, blocks, what asked: `extra`). Its zone, format and
/// lighting stay. Written whole.
fn patch_manifest(
    out: &Path,
    blocks: &Blocks,
    had: &HashMap<(i32, i32), bool>,
    extra: Value,
    done: &Done,
) -> Result<()> {
    let path = out.join("tiles.json");
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut manifest: Value =
        serde_json::from_str(&text).with_context(|| format!("{} is unreadable", path.display()))?;
    let delta: i64 = blocks
        .iter()
        .map(|&(bx, by)| again::has_tile(out, 7, bx, by) as i64 - had.get(&(bx, by)).copied().unwrap_or(false) as i64)
        .sum();
    if let Some(n) = manifest.get("blocks").and_then(Value::as_i64) {
        manifest["blocks"] = json!((n + delta).max(0));
    }
    match format::bounds(out) {
        Some(l) => manifest["bounds"] = json!(l),
        None => {
            if let Some(o) = manifest.as_object_mut() {
                o.remove("bounds");
            }
        }
    }
    let server = crate::world::fivem::server();
    if server.resources.is_empty() {
        if let Some(o) = manifest.as_object_mut() {
            o.remove("resources");
        }
    } else {
        manifest["resources"] = server.resources.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().into();
    }
    let mut entry = json!({
        "date": record::now(),
        "zones": ground::rectangles(blocks).into_iter().map(ground::metres).collect::<Vec<_>>(),
        "blocks": done.drawn.len(),
        "cleared": done.cleared.len(),
        "errors": done.errors,
    });
    if let (Some(e), Some(x)) = (entry.as_object_mut(), extra.as_object()) {
        e.extend(x.clone());
    }
    let mut list = manifest.get("updates").and_then(Value::as_array).cloned().unwrap_or_default();
    list.push(entry);
    // the last ones are enough
    let keep = list.len().saturating_sub(MAX_UPDATES);
    manifest["updates"] = Value::Array(list.split_off(keep));
    format::write_whole(&path, serde_json::to_string_pretty(&manifest)?.as_bytes())
}

/// Entries of `updates` kept in `tiles.json`.
const MAX_UPDATES: usize = 100;

/// At the end of `tiles` over the whole map: the record of the resources it was drawn with
/// (`prints`, taken at its start), each one's ground found in the map's placements. Not written
/// when blocks failed (`errors`): said, never an error of the render.
pub fn after_render(
    game: &Path,
    keys: &GtaKeys,
    out: &Path,
    world: &World,
    places: &Placements,
    prints: Vec<Print>,
    errors: usize,
) {
    if errors > 0 {
        println!(
            "record of the resources: not written, {errors} block(s) failed (update --zone on them, then --record)"
        );
        return;
    }
    let all: Vec<usize> = (0..prints.len()).collect();
    let done = (|| -> Result<usize> {
        let now = grounds_now(game, keys, world, None, Some(&places.entities), &all)?;
        let record = Record {
            format: record::FORMAT.into(),
            date: record::now(),
            by: "tiles".into(),
            settings: record::settings(),
            folders: record::folder_names(&crate::config::get().resources.folders),
            on_demand: Some(on_request(world)),
            resources: prints.iter().enumerate().map(|(i, p)| entry(p, &now[&i])).collect(),
        };
        record::write(out, &record)?;
        Ok(record.resources.len())
    })();
    match done {
        Ok(n) => println!(
            "record of the resources: {n} resource(s) recorded in {} (for update --changes)",
            out.join(record::FILE).display()
        ),
        Err(e) => println!("record of the resources: not written ({e:#})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    fn refused(list: &[&str]) -> String {
        match parse(&args(list)) {
            Ok(_) => panic!("{list:?} accepted"),
            Err(e) => format!("{e:#}"),
        }
    }

    #[test]
    fn the_modes_and_options_of_the_contract_are_read() {
        let p = parse(&args(&["output/x", "--zone", "640,-768,384,-1024", "--threads", "4", "--quality", "lossless"]))
            .unwrap();
        assert_eq!(p.mode, Mode::Zone([384.0, -1024.0, 640.0, -768.0]));
        assert_eq!(
            (p.tiles.out, p.tiles.zone, p.tiles.threads, p.format_asked),
            (PathBuf::from("output/x"), None, crate::tiles::options::Threads::Fixed(4), true)
        );
        let p = parse(&args(&["--changes"])).unwrap();
        assert_eq!(
            (p.mode, p.tiles.out, p.format_asked, p.tiles.dry_run),
            (Mode::Changes, PathBuf::from("output/tiles"), false, false)
        );
        assert_eq!(parse(&args(&["--list", "d"])).unwrap().mode, Mode::List);
        let p = parse(&args(&["d", "--record", "--dry-run", "--ram", "2"])).unwrap();
        assert_eq!((p.mode, p.tiles.dry_run, p.tiles.ram_budget), (Mode::Record, true, 2_000_000_000));
        // refused, with the reason
        assert!(refused(&["d"]).starts_with("update: say what to do: --zone x1,y1,x2,y2"));
        assert_eq!(refused(&["--list", "--record"]), "update: one thing at a time, not --list and --record");
        assert_eq!(
            refused(&["--changes", "--zone", "0,0,1,1"]),
            "update: one thing at a time, not --changes and --zone"
        );
        assert!(refused(&["--changes", "--quick"]).starts_with("update: unknown option --quick"));
        assert_eq!(refused(&["a", "b", "--list"]), "update: one folder expected, not \"a\" and \"b\"");
        assert!(refused(&["--zone", "1,2,3"]).starts_with("--zone 1,2,3: expected x1,y1,x2,y2"));
        assert!(refused(&["--changes", "--threads", "0"]).starts_with("--threads 0: expected"));
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("btms-tests-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_folder_without_a_finished_map_is_refused() {
        let dir = scratch("no-map");
        let e = format!("{:#}", map(&dir, true).err().unwrap());
        assert!(e.starts_with("no map in ") && e.contains("render the whole map first"), "{e}");
        let unfinished = json!({"format": format::MANIFEST_FORMAT, "extension": "webp", "quality": "lossless", "zone": null, "complete": false});
        std::fs::write(dir.join("tiles.json"), unfinished.to_string()).unwrap();
        let e = format!("{:#}", map(&dir, true).err().unwrap());
        assert!(e.contains("is not finished"), "{e}");
        // listed all the same
        let m = map(&dir, false).unwrap();
        assert_eq!((m.zone, m.format), (None, format::Format::Webp(crate::tiles::webp::Quality::Lossless)));
        let finished = json!({"format": format::MANIFEST_FORMAT, "extension": "webp", "quality": 90, "zone": [-2432.0, 0.0, -1920.0, 512.0], "complete": true});
        std::fs::write(dir.join("tiles.json"), finished.to_string()).unwrap();
        let m = map(&dir, true).unwrap();
        assert_eq!(m.zone, Some([-2432.0, 0.0, -1920.0, 512.0]));
        // without a record: the refusal explains --record
        let e = format!("{:#}", record_or_refuse(&dir).err().unwrap());
        assert!(e.contains("has no record of the resources (resources.json)") && e.contains("--record"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The map's manifest stays its own: zone, format, lighting kept; counts, bounds and the list of
    /// updates follow what the folder holds.
    #[test]
    fn the_manifest_of_the_map_is_patched_not_replaced() {
        let dir = scratch("manifest-kept");
        let manifest = json!({
            "format": format::MANIFEST_FORMAT, "extension": "webp", "quality": 90, "zone": null, "complete": true,
            "blocks": 10, "lighting": {"azimuth": 315.0}, "bounds": [0.0, 0.0, 128.0, 128.0], "resources": ["old"],
        });
        std::fs::write(dir.join("tiles.json"), serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
        // block (0, 0) had its tile and lost it; (3, -2) gained one; (1, 1) kept its own
        for (x, y) in [(3, -2), (1, 1)] {
            std::fs::create_dir_all(dir.join("7").join(x.to_string())).unwrap();
            std::fs::write(dir.join("7").join(x.to_string()).join(format!("{y}.{}", format::EXTENSIONS[0])), b"t")
                .unwrap();
        }
        let blocks: Blocks = [(0, 0), (3, -2), (1, 1)].into_iter().collect();
        let had = HashMap::from([((0, 0), true), ((3, -2), false), ((1, 1), true)]);
        let done = Done { drawn: vec![(3, -2), (1, 1)], cleared: vec![(0, 0)], errors: 0 };
        patch_manifest(&dir, &blocks, &had, json!({"by": "zone"}), &done).unwrap();
        patch_manifest(&dir, &blocks, &HashMap::new(), json!({"by": "changes", "resources": ["a"]}), &done).unwrap();
        let after: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("tiles.json")).unwrap()).unwrap();
        for key in ["format", "extension", "quality", "zone", "complete", "lighting"] {
            assert_eq!(after[key], manifest[key], "{key}");
        }
        // 10, then −1 + 1 + 0, then (nothing had) + 2
        assert_eq!(after["blocks"], 12);
        assert_eq!(after["bounds"], json!([128.0, -256.0, 512.0, 256.0]));
        assert!(after.get("resources").is_none(), "no resource in the settings of the tests");
        let list = after["updates"].as_array().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(
            (&list[0]["by"], &list[0]["blocks"], &list[0]["cleared"], &list[0]["errors"]),
            (&json!("zone"), &json!(2), &json!(1), &json!(0))
        );
        assert_eq!(
            list[0]["zones"],
            json!([[384.0, -256.0, 512.0, -128.0], [0.0, 0.0, 128.0, 128.0], [128.0, 128.0, 256.0, 256.0]])
        );
        assert_eq!(list[1]["resources"], json!(["a"]));
        assert!(!dir.join("tiles.tmp").exists());
        // the list is bounded
        for _ in 0..MAX_UPDATES + 5 {
            patch_manifest(&dir, &blocks, &had, json!({}), &done).unwrap();
        }
        let after: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("tiles.json")).unwrap()).unwrap();
        assert_eq!(after["updates"].as_array().unwrap().len(), MAX_UPDATES);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_blocks_drawn_again_are_both_grounds_widened() {
        let e = |name: &str, blocks: Vec<[i32; 4]>| Entry {
            name: name.into(),
            folder: format!("C:/srv/{name}"),
            fingerprint: "1".into(),
            files: 1,
            zone: Some(ground::metres(blocks[0])),
            blocks,
            readable: true,
            reason: None,
        };
        let record = Record {
            format: record::FORMAT.into(),
            date: "2026-11-15T10:00:00Z".into(),
            by: "tiles".into(),
            settings: record::settings(),
            folders: vec![],
            on_demand: None,
            resources: vec![e("moved", vec![[0, 0, 0, 0]]), e("gone", vec![[10, 10, 11, 10]])],
        };
        let prints = vec![
            Print { name: "moved".into(), dir: PathBuf::from("C:/srv/moved"), digest: "2".into(), files: 1 },
            Print { name: "new".into(), dir: PathBuf::from("C:/srv/new"), digest: "3".into(), files: 1 },
        ];
        let changes = record::diff(&record, &prints);
        assert_eq!(
            changes.iter().map(|c| (c.name.as_str(), c.state)).collect::<Vec<_>>(),
            [("gone", State::Removed), ("moved", State::Changed), ("new", State::Added)]
        );
        let mut moved = Ground::default();
        moved.add([256.0 + 1.0, 1.0, 0.0], [256.0 + 2.0, 2.0, 0.0]);
        let mut unreadable = Ground { unreadable: 3, reason: Some("protected".into()), ..Ground::default() };
        ground::finish(&mut unreadable);
        let now = HashMap::from([(0, moved), (1, unreadable)]);
        let t = told(&record, &prints, &changes, &now, None, &[], None);
        // old (0, 0) and new (2, 0) of "moved", the two blocks of "gone", each widened by one
        let expected: Blocks = ground::widen(&[(0, 0), (2, 0), (10, 10), (11, 10)].into_iter().collect(), 1);
        assert_eq!(t.blocks, expected);
        assert_eq!(t.resources.len(), 3);
        assert_eq!(
            (t.resources[0].state.as_str(), t.resources[0].zone),
            ("removed", Some([1280.0, 1280.0, 1536.0, 1408.0]))
        );
        assert_eq!(t.resources[2].zone, None);
        assert_eq!(t.resources[2].reason.as_deref(), Some("protected"));
        // within the map's own zone only
        let t = told(&record, &prints, &changes, &now, None, &[], Some([0.0, 0.0, 384.0, 128.0]));
        assert_eq!(t.blocks, [(0, 0), (1, 0), (2, 0)].into_iter().collect());
    }
}
