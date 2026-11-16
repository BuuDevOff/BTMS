//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! `resources [--all] [--detail]`: what the server's resources hold, replace and remove, and what
//! could not be read. Also the names of the files by hash ([`names`], [`label`]), for `probe`.

use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use rage_rpf::GtaKeys;

use crate::formats::resource::{Resource, joaat};
use crate::formats::ymap::{parse_ymap, parse_ytyp};
use crate::game::index::Index;
use crate::world::World;

/// Hash → name, from the names of the files (models, dictionaries, placement and archetype files).
pub fn names(index: &Index) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    for ((hash, ext), paths) in &index.files {
        if matches!(ext.as_str(), "ydr" | "yft" | "ydd" | "ymap" | "ytyp" | "ytd")
            && let Some(p) = paths.first()
        {
            let name = p.rsplit('/').next().unwrap_or(p);
            out.entry(*hash).or_insert_with(|| name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name).to_string());
        }
    }
    out
}

/// `0xHASH` or the name when a file bears it.
pub fn label(names: &HashMap<u32, String>, hash: u32) -> String {
    names.get(&hash).cloned().unwrap_or(format!("0x{hash:08X}"))
}

/// What is said of a model whose vertices are scrambled.
const PROTECTED: &str = "protected model (vertices scrambled, probably by FiveM escrow), not drawn";

/// How an error reads without what is particular to a file (numbers, the name before the last
/// ": "): to count alike ones together.
fn reason(e: &anyhow::Error) -> String {
    let text = format!("{e:#}");
    let mut out = String::new();
    let mut digits = false;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            if !digits {
                out.push('#');
            }
            digits = true;
        } else {
            digits = false;
            out.push(ch);
        }
    }
    match out.rsplit_once(": ") {
        Some((_, tail)) if tail.len() > 12 => tail.to_string(),
        _ => out,
    }
}

/// `resources [--all] [--detail]`: what the server's resources hold, what they replace and
/// remove, what could not be read. `--all` also reads every model and texture dictionary they
/// stream; `--detail` names every file in failure instead of three per reason.
pub fn resources(game: &Path, keys: &GtaKeys, full: bool, detail: bool) -> Result<()> {
    use crate::world::fivem::{self, is_streamed};
    let world = World::load(game, keys, false)?;
    let s = fivem::server();
    let names = names(&world.index);
    if s.resources.is_empty() {
        println!("no server resource ([resources] folders in map.toml)");
        return Ok(());
    }
    let parts = |path: &str| -> Option<(u32, String)> {
        let name = path.rsplit('/').next().unwrap_or(path);
        name.rsplit_once('.').map(|(stem, ext)| (joaat(stem), ext.to_string()))
    };
    let in_game =
        |path: &str| parts(path).is_some_and(|(h, ext)| world.index.copies(h, &ext).iter().any(|p| !is_streamed(p)));
    let wins = |path: &str| {
        parts(path).is_some_and(|(h, ext)| world.index.copies(h, &ext).first().map(String::as_str) == Some(path))
    };
    let is = |path: &str, ext: &str| path.ends_with(&format!(".{ext}"));

    // the resources
    println!("{} resources, {} streamed files", s.resources.len(), s.streamed.len());
    for (i, r) in s.resources.iter().enumerate() {
        let files: Vec<&str> = s.streamed.iter().filter(|f| f.resource == i).map(|f| f.path.as_str()).collect();
        let count = |ext: &str| files.iter().filter(|p| is(p, ext)).count();
        let known: usize = fivem::READ.iter().map(|e| count(e)).sum();
        println!(
            "  {:<26} {} {:>5} files: ymap {}, ytyp {}, ydr {}, ydd {}, yft {}, ytd {}, others {}; {} bear the name of a game file",
            r.name,
            if r.map { "map" } else { "   " },
            files.len(),
            count("ymap"),
            count("ytyp"),
            count("ydr"),
            count("ydd"),
            count("yft"),
            count("ytd"),
            files.len() - known,
            files.iter().filter(|p| fivem::READ.iter().any(|e| is(p, e)) && in_game(p)).count()
        );
    }

    // where each resource places things: the box of the entities its placement files show
    println!("extent of the placements (entities shown):");
    let resource_of: HashMap<&str, usize> = s.streamed.iter().map(|f| (f.path.as_str(), f.resource)).collect();
    for (i, r) in s.resources.iter().enumerate() {
        let (mut lo, mut hi, mut n, mut entities) = ([f32::MAX; 2], [f32::MIN; 2], 0usize, 0usize);
        for m in world.ymaps.iter().filter(|m| resource_of.get(m.path.as_str()) == Some(&i) && m.entity_count > 0) {
            let hidden = world.server.not_shown.get(&m.path).map(|l| l.len()).unwrap_or(0);
            if hidden == m.entity_count {
                continue;
            }
            n += 1;
            entities += m.entity_count - hidden;
            for k in 0..2 {
                lo[k] = lo[k].min(m.entities_min[k]);
                hi[k] = hi[k].max(m.entities_max[k]);
            }
        }
        if n > 0 {
            println!(
                "  {:<26} x {:.0} to {:.0}, y {:.0} to {:.0} ({n} placement files, {entities} entities)",
                r.name, lo[0], hi[0], lo[1], hi[1]
            );
        }
    }

    // what takes the place of a file of the game, by kind
    let winners: Vec<&str> = s.streamed.iter().map(|f| f.path.as_str()).filter(|p| wins(p)).collect();
    println!(
        "in place of a game file: {}",
        fivem::READ
            .iter()
            .map(|e| format!("{e} {}", winners.iter().filter(|p| is(p, e) && in_game(p)).count()))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // several streamed files of one name
    let by_name = s.by_name();
    let mut clashes: Vec<(&&str, &Vec<&str>)> =
        by_name.iter().filter(|(n, v)| v.len() > 1 && fivem::READ.iter().any(|e| is(n, e))).collect();
    clashes.sort();
    let differ = |v: &Vec<&str>| {
        let first = fivem::read(v[0]).ok();
        v.iter().skip(1).any(|p| fivem::read(p).ok() != first)
    };
    println!(
        "names streamed more than once (the last one wins: order of the folders, then of the names): {}",
        clashes.len()
    );
    for (name, copies) in &clashes {
        println!("  {name} ({}):", if differ(copies) { "different contents" } else { "identical" });
        for (k, p) in copies.iter().enumerate() {
            println!("      {} {}", if k + 1 == copies.len() { "kept    " } else { "left out" }, fivem::describe(p));
        }
    }

    // placement files: what they replace, what they move out of the world, their boxes
    println!("placement files of the resources:");
    let mut stale: Vec<(String, f32)> = Vec::new();
    let (mut unknown, mut hd_total): (HashMap<u32, usize>, usize) = (HashMap::new(), 0);
    let mut placed: Vec<(u32, String)> = Vec::new();
    let (mut moved_total, mut shadows_total, mut interiors, mut hidden_by_variant) =
        (0usize, 0usize, 0usize, Vec::new());
    let lowest_game = world
        .ymaps
        .iter()
        .filter(|m| !is_streamed(&m.path) && m.entity_count > 0 && m.entities_min[2].is_finite())
        .map(|m| m.entities_min[2])
        .fold(f32::MAX, f32::min);
    for m in world.ymaps.iter().filter(|m| is_streamed(&m.path)) {
        let Ok(map) = crate::cli::archive::read_file(game, &m.path, keys)
            .and_then(|b| Resource::parse(&b))
            .and_then(|r| parse_ymap(&r))
        else {
            continue;
        };
        let stem = label(&names, m.hash);
        let moved = map.entities.iter().filter(|e| fivem::out_of_world(e)).count();
        let shadows = world.server.not_shown.get(&m.path).map(|l| l.len()).unwrap_or(0) - moved;
        for e in map
            .entities
            .iter()
            .filter(|e| !e.interior && matches!(e.lod_level, 0 | 5) && !world.archetypes.contains_key(&e.archetype))
        {
            *unknown.entry(e.archetype).or_default() += 1;
        }
        hd_total += map.entities.iter().filter(|e| !e.interior && matches!(e.lod_level, 0 | 5)).count();
        moved_total += moved;
        shadows_total += shadows;
        interiors += map.entities.iter().filter(|e| e.interior).count();
        for e in map.entities.iter().filter(|e| e.interior && world.server.interiors.contains_key(&e.archetype)) {
            placed.push((
                e.archetype,
                format!("{stem} at ({:.0}, {:.0}, {:.0})", e.position[0], e.position[1], e.position[2]),
            ));
        }
        if world.replaced.contains(&m.hash) {
            hidden_by_variant.push(stem.clone());
        }
        // the box of the entities shown (the map's) against the one the file declares
        let out = (0..3)
            .map(|k| (map.entities_min[k] - m.entities_min[k]).max(m.entities_max[k] - map.entities_max[k]))
            .fold(0f32, f32::max);
        if out > 1.0 {
            stale.push((stem.clone(), out));
        }
        let game_copy = world.index.copies(m.hash, "ymap").iter().find(|p| !is_streamed(p));
        if let Some(g) = game_copy {
            let before = crate::cli::archive::read_file(game, g, keys)
                .and_then(|b| Resource::parse(&b))
                .and_then(|r| parse_ymap(&r))
                .map(|x| x.entities.len());
            let emptied = map.entities.is_empty() && before.as_ref().is_ok_and(|n| *n > 0);
            println!(
                "  {stem:<40} replaces the game's: {} entities → {}{}{}",
                before.map(|n| n.to_string()).unwrap_or("?".into()),
                map.entities.len(),
                if moved + shadows > 0 {
                    format!(
                        ", {moved} of them moved out of the world and {shadows} that only cast a shadow (not drawn)"
                    )
                } else {
                    String::new()
                },
                if emptied { ": emptied, it only removes" } else { "" }
            );
        } else if moved + shadows > 0 {
            println!(
                "  {stem:<40} new: {} entities, {moved} of them moved out of the world and {shadows} that only cast a shadow (not drawn)",
                map.entities.len()
            );
        }
    }
    let streamed_ymaps = world.ymaps.iter().filter(|m| is_streamed(&m.path)).count();
    println!(
        "  {streamed_ymaps} placement files, {moved_total} entities moved out of the world (below {} m; the lowest placement of the game is at {lowest_game:.0} m), {shadows_total} that only cast a shadow, {interiors} interior placements",
        fivem::OUT_OF_WORLD_Z
    );
    println!(
        "  left out by the hei_/apa_/lr_/bkr_/vw_ variant rule (a base file whose prefixed copy exists): {}",
        if hidden_by_variant.is_empty() { "none".to_string() } else { hidden_by_variant.join(", ") }
    );
    let mut unknown: Vec<(u32, usize)> = unknown.into_iter().collect();
    unknown.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    println!(
        "  HD entities without an archetype (no .ytyp file of the game or of the resources defines it: not drawn): {} of {hd_total}, from {} archetypes{}",
        unknown.iter().map(|u| u.1).sum::<usize>(),
        unknown.len(),
        if unknown.is_empty() {
            String::new()
        } else {
            format!(
                " (most placed: {})",
                unknown
                    .iter()
                    .take(6)
                    .map(|(h, n)| format!("{} ×{n}", label(&names, *h)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    );
    println!(
        "  interiors: {} defined by the resources, {} of them from resources with protected models (left out); {} placements of the others, {} entities inside added to the map",
        world.server.interiors.len() + world.server.interiors_protected,
        world.server.interiors_protected,
        world.server.interiors_placed,
        world.server.inside
    );
    for mlo in world.server.interiors.values() {
        let places: Vec<&str> = placed.iter().filter(|(a, _)| *a == mlo.name).map(|(_, p)| p.as_str()).collect();
        println!(
            "      interior kept: {} ({} entities), placed by {}",
            label(&names, mlo.name),
            mlo.entities.len(),
            if places.is_empty() { "no placement file".to_string() } else { places.join(", ") }
        );
    }
    stale.sort_by(|a, b| b.1.total_cmp(&a.1));
    println!(
        "  declared box smaller than that of their HD entities (by more than 1 m), widened: {}",
        if stale.is_empty() {
            "none".to_string()
        } else {
            stale.iter().take(12).map(|(n, d)| format!("{n} ({d:.0} m)")).collect::<Vec<_>>().join(", ")
                + &format!("; {} in all", stale.len())
        }
    );

    // scripts
    println!("client scripts:");
    for c in &s.calls {
        println!(
            "  {} line {}: {} {}",
            c.file.display(),
            c.line,
            if c.remove { "RemoveIpl" } else { "RequestIpl" },
            c.name
                .as_deref()
                .map(|n| format!("\"{n}\""))
                .unwrap_or("(name computed by the script: not applied)".into())
        );
    }
    for (n, file, line) in &s.other_hides {
        println!("  {} line {line}: {n} (not applied)", file.display());
    }
    if s.calls.is_empty() && s.other_hides.is_empty() {
        println!("  no RemoveIpl, RequestIpl or CreateModelHide in the readable scripts");
    }

    // left out
    println!("files left out: {}", s.skipped.len());
    for (file, why) in &s.skipped {
        println!("  {}: {why}", file.display());
    }

    if !full {
        println!("(--all: also reads every model and every texture dictionary)");
        return Ok(());
    }
    let protected = |yes: bool| if yes { Err(anyhow::anyhow!("{PROTECTED}")) } else { Ok(()) };
    // every streamed file read whole, on 8 threads
    let files: Vec<&str> =
        s.streamed.iter().map(|f| f.path.as_str()).filter(|p| fivem::READ.iter().any(|e| is(p, e))).collect();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failures = std::sync::Mutex::new(Vec::<(String, String, String)>::new());
    let stray = std::sync::Mutex::new(Vec::<String>::new());
    let t = std::time::Instant::now();
    std::thread::scope(|sc| {
        for _ in 0..8 {
            sc.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(path) = files.get(i) else { break };
                    let ext = path.rsplit('.').next().unwrap_or("");
                    let result = crate::cli::archive::read_file(game, path, keys)
                        .and_then(|b| fivem::resource(path, &b))
                        .and_then(|r| match ext {
                            "ymap" => parse_ymap(&r).map(|_| ()),
                            "ytyp" => parse_ytyp(&r).map(|_| ()),
                            "ydr" => crate::formats::ydr::parse_ydr(&r).and_then(|d| {
                                if fivem::outside_its_box(&d) {
                                    stray.lock().unwrap().push(fivem::describe(path));
                                }
                                protected(fivem::scrambled(&d))
                            }),
                            "ydd" => crate::formats::ydr::parse_ydd(&r)
                                .and_then(|l| protected(l.iter().any(|(_, d)| fivem::scrambled(d)))),
                            "yft" => crate::formats::ydr::parse_yft(&r).and_then(|d| protected(fivem::scrambled(&d))),
                            _ => crate::formats::ytd::parse_ytd(&r)
                                .and_then(|l| l.iter().try_for_each(|t| t.rgba().map(|_| ()))),
                        });
                    if let Err(e) = result {
                        failures.lock().unwrap().push((ext.to_string(), reason(&e), fivem::describe(path)));
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    println!("full read of {} files in {:.0} s:", files.len(), t.elapsed().as_secs_f64());
    let mut stray = stray.into_inner().unwrap();
    stray.sort();
    println!(
        "  unscrambled .ydr models with vertices outside their box (box out of date: drawn): {}{}",
        stray.len(),
        if stray.is_empty() {
            String::new()
        } else {
            format!(
                ": {}",
                stray.iter().take(if detail { usize::MAX } else { 4 }).cloned().collect::<Vec<_>>().join(", ")
            )
        }
    );
    println!("  protected models by resource (vertices scrambled):");
    for (i, r) in s.resources.iter().enumerate() {
        let models: Vec<&str> = s
            .streamed
            .iter()
            .filter(|f| f.resource == i)
            .map(|f| f.path.as_str())
            .filter(|p| is(p, "ydr") || is(p, "ydd") || is(p, "yft"))
            .collect();
        if models.is_empty() {
            continue;
        }
        let prefix = format!("{}/", r.name);
        let locked = failures.iter().filter(|f| f.1 == PROTECTED && f.2.starts_with(&prefix)).count();
        println!("      {:<26} {locked} of {} models", r.name, models.len());
    }
    for ext in fivem::READ {
        let total = files.iter().filter(|p| is(p, ext)).count();
        let failed: Vec<&(String, String, String)> = failures.iter().filter(|f| f.0 == ext).collect();
        println!("  {ext}: {} read, {} failed", total - failed.len(), failed.len());
        let mut by: HashMap<&str, Vec<&str>> = HashMap::new();
        for f in &failed {
            by.entry(f.1.as_str()).or_default().push(f.2.as_str());
        }
        let mut by: Vec<_> = by.into_iter().collect();
        by.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
        for (why, mut list) in by {
            list.sort();
            println!("      ×{} {why}", list.len());
            for file in list.iter().take(if detail { usize::MAX } else { 3 }) {
                println!("          {file}");
            }
        }
    }
    Ok(())
}
