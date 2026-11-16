//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! `convert`: a folder of tiles already drawn (JPEG and PNG, or WebP) written again as WebP into
//! another folder, to publish the map at once without drawing it again:
//!
//! ```text
//! btms convert <folder> --to <folder> [--quality <0-100>|lossless] [--threads auto|<n>]
//!     [--webp-effort careful|fast]
//! ```
//!
//! Never in place: the folder converted is only read, and the other one may not be it, hold it,
//! or be inside it; it must be new, empty, or an earlier conversion (a render's folder is
//! refused: its tiles would be replaced). That one gets `tiles.json` (the source's, with the keys of the new format
//! and `converted_from`), the page of the web map, and every tile that is not empty as `.webp`, on
//! every core, with care by default (`--webp-effort careful`: method 4 and the "sharp"
//! conversion; a publication, nothing is waiting). A JPEG tile keeps its defects (it is decoded,
//! then encoded again): the quality of a render, without them, comes back only by drawing the map
//! again (`tiles`, in WebP).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::format::{self, Format};
use super::webp::{self, Effort, Quality, Settings};
use super::{ZMAX, tile_path};

/// The options of `convert`.
#[derive(Debug, PartialEq)]
pub struct Options {
    pub from: PathBuf,
    pub to: PathBuf,
    pub quality: Quality,
    pub threads: usize,
    /// `--webp-effort`: careful by default.
    pub effort: Effort,
}

/// `convert <folder> --to <folder> [--quality …] [--threads …]` (`convert` first).
pub fn parse(args: &[String]) -> Result<Options> {
    let usage =
        "convert <folder> --to <folder> [--quality <0-100>|lossless] [--threads auto|<n>] [--webp-effort careful|fast]";
    let mut rest: Vec<String> = args.iter().skip(1).cloned().collect();
    let mut take = |flag: &str| -> Result<Option<String>> {
        let mut value = None;
        while let Some(i) = rest.iter().position(|a| a == flag) {
            match rest.get(i + 1) {
                Some(v) if !v.starts_with("--") => value = Some(v.clone()),
                _ => bail!("{flag} without a value: {usage}"),
            }
            rest.drain(i..i + 2);
        }
        Ok(value)
    };
    let to = take("--to")?.map(PathBuf::from);
    let quality = take("--quality")?
        .map(|q| format::parse_quality(&q))
        .transpose()?
        .unwrap_or(Quality::Lossy(format::DEFAULT_QUALITY));
    let threads = match take("--threads")?.as_deref() {
        None | Some("auto") => super::options::logical(),
        Some(v) => match v.trim().parse::<usize>() {
            Ok(n) if (1..=super::options::MAX_THREADS).contains(&n) => n,
            _ => bail!("--threads {v}: expected auto or a number of threads from 1 to {}", super::options::MAX_THREADS),
        },
    };
    let effort = take("--webp-effort")?.map(|w| Effort::parse(&w)).transpose()?.unwrap_or(Effort::Careful);
    if let Some(unknown) = rest.iter().find(|a| a.starts_with("--")) {
        bail!("convert: unknown option {unknown} ({usage})");
    }
    let Some(to) = to else { bail!("convert: --to <folder> missing (never in place): {usage}") };
    match rest.as_slice() {
        [from] => Ok(Options { from: PathBuf::from(from), to, quality, threads, effort }),
        [] => bail!("convert: the folder to convert is missing: {usage}"),
        many => bail!(
            "convert: one folder to convert expected, not {}",
            many.iter().map(|f| format!("\"{f}\"")).collect::<Vec<_>>().join(" and ")
        ),
    }
}

/// A tile of the folder converted.
struct Source {
    z: i32,
    tx: i32,
    ty: i32,
    path: PathBuf,
}

/// Every tile of `dir` (zooms 0 to 9), one file per tile: the first of the extensions its format
/// looks for (`format::of`), as a reader of that folder would take it.
fn tiles_of(dir: &Path) -> Vec<Source> {
    let extensions = format::of(dir).extensions();
    let mut tiles = Vec::new();
    for z in 0..=ZMAX {
        let Ok(columns) = std::fs::read_dir(dir.join(z.to_string())) else { continue };
        for column in columns.flatten() {
            let Some(tx) = column.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else { continue };
            let Ok(files) = std::fs::read_dir(column.path()) else { continue };
            let mut found: Vec<(i32, usize, PathBuf)> = Vec::new();
            for file in files.flatten() {
                let name = file.file_name();
                let Some((stem, ext)) = name.to_str().and_then(|n| n.rsplit_once('.')) else { continue };
                let (Ok(ty), Some(rank)) = (stem.parse::<i32>(), extensions.iter().position(|e| *e == ext)) else {
                    continue;
                };
                found.push((ty, rank, file.path()));
            }
            found.sort_by_key(|(ty, rank, _)| (*ty, *rank));
            found.dedup_by_key(|(ty, ..)| *ty);
            tiles.extend(found.into_iter().map(|(ty, _, path)| Source { z, tx, ty, path }));
        }
    }
    tiles
}

/// What happened to a tile.
enum Done {
    Written { before: u64, after: u64 },
    Empty,
}

/// Converts one tile: decoded, encoded as WebP (without alpha when opaque), written whole.
fn convert_tile(tile: &Source, to: &Path, settings: &Settings) -> Result<Done> {
    let bytes = std::fs::read(&tile.path).with_context(|| format!("reading {}", tile.path.display()))?;
    let rgba = if webp::is_webp(&bytes) {
        webp::decode(&bytes)?.2
    } else {
        image::load_from_memory(&bytes)
            .with_context(|| format!("{} is unreadable", tile.path.display()))?
            .to_rgba8()
            .into_raw()
    };
    let side = ((rgba.len() / 4) as f64).sqrt() as usize;
    if side * side * 4 != rgba.len() {
        bail!("{} is not a square tile", tile.path.display());
    }
    let (mut any, mut all) = (false, true);
    for p in rgba.as_chunks::<4>().0 {
        any |= p[3] > 0;
        all &= p[3] == 255;
    }
    if !any {
        return Ok(Done::Empty);
    }
    let out = webp::encode(&rgba, side, side, all, settings).with_context(|| format!("{}", tile.path.display()))?;
    let path = tile_path(to, tile.z, tile.tx, tile.ty, "webp");
    let temp = path.with_extension("webp.tmp");
    if std::fs::write(&temp, &out).is_err() {
        std::fs::create_dir_all(path.parent().unwrap()).with_context(|| format!("creating {}", path.display()))?;
        std::fs::write(&temp, &out).with_context(|| format!("writing {}", temp.display()))?;
    }
    std::fs::rename(&temp, &path).with_context(|| format!("renaming {}", temp.display()))?;
    // the folder may hold older tiles (a conversion into a render's folder): one extension
    for other in ["jpg", "png"] {
        let _ = std::fs::remove_file(tile_path(to, tile.z, tile.tx, tile.ty, other));
    }
    Ok(Done::Written { before: bytes.len() as u64, after: out.len() as u64 })
}

/// The manifest of the folder written: the source's (or a new one), saying the new format and
/// where it comes from.
fn manifest(opts: &Options, source: Option<&Value>, before: Format, complete: bool) -> Value {
    let mut manifest = match source {
        Some(v @ Value::Object(_)) => v.clone(),
        _ => json!({
            "zmax": ZMAX,
            "tile": super::TILE,
            "side_m": "32 · 2^(9 − z)",
            "metres_per_pixel_zmax": super::side(ZMAX) / super::TILE as f32,
            "y": "ty grows northwards; tile (z, tx, ty) covers x ∈ [tx·W, (tx+1)·W), y ∈ [ty·W, (ty+1)·W)",
        }),
    };
    // a render that was not finished stays one
    let finished = source.and_then(|s| s.get("complete")).and_then(Value::as_bool).unwrap_or(true);
    manifest["complete"] = json!(complete && finished);
    format::describe(&mut manifest, Format::Webp(opts.quality), opts.effort);
    match format::bounds(&opts.to).filter(|_| complete) {
        Some(bounds) => manifest["bounds"] = json!(bounds),
        None => {
            manifest.as_object_mut().map(|m| m.remove("bounds"));
        }
    }
    // the folder's name only: the manifest is published, and a whole path would tell the machine's
    // folders (the user's name among them)
    let name = opts
        .from
        .file_name()
        .map(PathBuf::from)
        .or_else(|| std::fs::canonicalize(&opts.from).ok().and_then(|p| p.file_name().map(PathBuf::from)));
    manifest["converted_from"] = json!({
        "folder": name.map(|n| n.to_string_lossy().into_owned()),
        "extension": before.extension(),
        "quality": before.quality_value(),
        "note": "tiles decoded then encoded again: those from a JPEG keep its defects",
    });
    manifest
}

fn write_manifest(dir: &Path, manifest: &Value) -> Result<()> {
    format::write_whole(&dir.join("tiles.json"), serde_json::to_string_pretty(manifest)?.as_bytes())
}

/// Megabytes: `12.3 MB`.
fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1e6)
}

pub fn run(opts: &Options) -> Result<()> {
    let (from, to) = (&opts.from, &opts.to);
    if !from.is_dir() {
        bail!("convert: {} is not a folder", from.display());
    }
    if crate::interface::sources::related(from, to) {
        bail!(
            "convert: {} is the folder converted, holds it or is inside it: never in place, choose another folder",
            to.display()
        );
    }
    let source_manifest: Option<Value> =
        std::fs::read_to_string(from.join("tiles.json")).ok().and_then(|t| serde_json::from_str(&t).ok());
    let before = format::of(from);
    // the folder written: new, empty, or an earlier conversion (its tiles are written over); never
    // a render, whose tiles would be replaced
    if let Ok(mut entries) = std::fs::read_dir(to)
        && entries.next().is_some()
    {
        let converted = std::fs::read_to_string(to.join("tiles.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .is_some_and(|m| m.get("converted_from").is_some() && format::from_manifest(to).is_some());
        if !converted {
            bail!(
                "convert: {} is neither empty nor an earlier conversion (its tiles.json has no \"converted_from\"): nothing is written there, choose an empty or new folder",
                to.display()
            );
        }
    }
    let tiles = tiles_of(from);
    if tiles.is_empty() {
        bail!("convert: no tile in {} (folders 0 to 9)", from.display());
    }
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    format::set(to, Format::Webp(opts.quality), opts.effort);
    write_manifest(to, &manifest(opts, source_manifest.as_ref(), before, false))?;
    format::write_page(to)?;
    let settings = Settings::tiles(opts.quality, opts.effort);
    println!(
        "convert: {} tiles from {} ({}) → {} ({}, {}), {} threads",
        tiles.len(),
        from.display(),
        before.label(),
        to.display(),
        Format::Webp(opts.quality).label(),
        Format::Webp(opts.quality).encoder(opts.effort),
        opts.threads
    );
    if matches!(before, Format::Jpeg) {
        println!(
            "  (JPEG tiles keep their defects: the quality of a render without them comes back only by drawing the map again, in WebP)"
        );
    }
    let t = Instant::now();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let (written, empty) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let (bytes_before, bytes_after) = (AtomicU64::new(0), AtomicU64::new(0));
    let errors = Mutex::new(Vec::<String>::new());
    std::thread::scope(|s| {
        for _ in 0..opts.threads.max(1) {
            s.spawn(|| {
                while let Some(tile) = tiles.get(next.fetch_add(1, Ordering::Relaxed)) {
                    match convert_tile(tile, to, &settings) {
                        Ok(Done::Written { before, after }) => {
                            written.fetch_add(1, Ordering::Relaxed);
                            bytes_before.fetch_add(before, Ordering::Relaxed);
                            bytes_after.fetch_add(after, Ordering::Relaxed);
                        }
                        Ok(Done::Empty) => {
                            empty.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(e) => errors.lock().unwrap().push(format!("{e:#}")),
                    }
                    done.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        // the progress, every two seconds
        s.spawn(|| {
            let total = tiles.len();
            let mut last = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let n = done.load(Ordering::Relaxed);
                if n >= total {
                    break;
                }
                if last.elapsed() >= Duration::from_secs(2) {
                    last = Instant::now();
                    let rate = n as f64 / t.elapsed().as_secs_f64().max(1e-9);
                    eprintln!(
                        "  {n}/{total} tiles, {rate:.0} per second, {:.1} min left",
                        (total - n) as f64 / rate.max(1e-9) / 60.0
                    );
                }
            }
        });
    });
    let errors = errors.into_inner().unwrap();
    let (before_b, after_b) = (bytes_before.load(Ordering::Relaxed), bytes_after.load(Ordering::Relaxed));
    write_manifest(to, &manifest(opts, source_manifest.as_ref(), before, errors.is_empty()))?;
    println!(
        "convert: {} WebP tiles written in {:.0} s ({:.0} per second), {} empty ones skipped; {} → {} ({:+.0} %)",
        written.load(Ordering::Relaxed),
        t.elapsed().as_secs_f64(),
        tiles.len() as f64 / t.elapsed().as_secs_f64().max(1e-9),
        empty.load(Ordering::Relaxed),
        mb(before_b),
        mb(after_b),
        if before_b > 0 { 100.0 * (after_b as f64 / before_b as f64 - 1.0) } else { 0.0 }
    );
    for e in errors.iter().take(10) {
        println!("  error: {e}");
    }
    if !errors.is_empty() {
        bail!("convert: {} tiles not converted (tiles.json says \"complete\": false)", errors.len());
    }
    println!("convert: {} can be published as it is (index.html, tiles.json, folders 0 to 9)", to.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::webp::tests::{ground, psnr};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("btms-tests-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn the_options_are_read_and_checked() {
        let o = parse(&args(&["convert", "output/tiles", "--to", "web", "--quality", "lossless", "--threads", "3"]))
            .unwrap();
        assert_eq!(
            o,
            Options {
                from: "output/tiles".into(),
                to: "web".into(),
                quality: Quality::Lossless,
                threads: 3,
                effort: Effort::Careful
            }
        );
        let o = parse(&args(&["convert", "--to", "web", "a", "--webp-effort", "fast"])).unwrap();
        assert_eq!(
            (o.from, o.quality, o.effort),
            (PathBuf::from("a"), Quality::Lossy(format::DEFAULT_QUALITY), Effort::Fast)
        );
        let refused = |list: &[&str]| format!("{:#}", parse(&args(list)).unwrap_err());
        assert!(refused(&["convert", "a"]).starts_with("convert: --to <folder> missing"));
        assert!(refused(&["convert", "--to", "b"]).starts_with("convert: the folder to convert is missing"));
        assert!(refused(&["convert", "a", "c", "--to", "b"]).starts_with("convert: one folder to convert expected"));
        assert!(refused(&["convert", "a", "--to", "b", "--quality", "120"]).starts_with("--quality 120: expected"));
        assert!(
            refused(&["convert", "a", "--to", "b", "--format", "webp"]).starts_with("convert: unknown option --format")
        );
        assert!(refused(&["convert", "a", "--to"]).starts_with("--to without a value"));
        assert!(
            refused(&["convert", "a", "--to", "b", "--webp-effort", "strong"])
                .starts_with("--webp-effort strong: expected fast")
        );
    }

    /// An older folder: JPEG tiles (opaque), PNG tiles (partly empty), an empty PNG, its manifest.
    fn older_folder(dir: &Path) -> Vec<(i32, i32, i32, Vec<u8>)> {
        let mut seed = 31u64;
        let mut written = Vec::new();
        for (z, tx, ty, holes) in [(9, 3, -8, false), (9, 4, -8, true), (7, 0, -2, false), (0, 0, 0, true)] {
            let rgba = ground(512, 512, holes, &mut seed);
            let column = dir.join(z.to_string()).join(tx.to_string());
            std::fs::create_dir_all(&column).unwrap();
            if holes {
                image::save_buffer(column.join(format!("{ty}.png")), &rgba, 512, 512, image::ExtendedColorType::Rgba8)
                    .unwrap();
            } else {
                std::fs::write(
                    column.join(format!("{ty}.jpg")),
                    crate::tiles::jpeg::encode(&super::super::JPEG, &rgba, super::super::JPEG_QUALITY),
                )
                .unwrap();
            }
            written.push((z, tx, ty, rgba));
        }
        // empty: no file is written for it
        std::fs::create_dir_all(dir.join("8").join("1")).unwrap();
        image::save_buffer(
            dir.join("8").join("1").join("1.png"),
            &vec![0u8; 512 * 512 * 4],
            512,
            512,
            image::ExtendedColorType::Rgba8,
        )
        .unwrap();
        std::fs::write(dir.join("tiles.json"), r#"{"format": "btms tiles v1", "blocks": 2, "complete": true, "zone": null, "lighting": {"azimuth": 315.0}}"#).unwrap();
        std::fs::write(dir.join("7").join("notes.txt"), b"not a tile").unwrap();
        written
    }

    #[test]
    fn an_older_folder_is_converted_into_another_one() {
        let root = scratch("convert");
        let (from, to) = (root.join("before"), root.join("web"));
        let originals = older_folder(&from);
        let before: Vec<(PathBuf, Vec<u8>)> = walk(&from);
        for quality in [Quality::Lossy(90), Quality::Lossless] {
            let opts = Options { from: from.clone(), to: to.clone(), quality, threads: 3, effort: Effort::Careful };
            run(&opts).unwrap();
            // the source untouched
            assert_eq!(walk(&from), before);
            // one .webp per tile that is not empty, the manifest and the page
            let mut names: Vec<String> = walk(&to)
                .into_iter()
                .map(|(p, _)| p.strip_prefix(&to).unwrap().to_string_lossy().replace('\\', "/"))
                .collect();
            names.sort();
            assert_eq!(names, ["0/0/0.webp", "7/0/-2.webp", "9/3/-8.webp", "9/4/-8.webp", "index.html", "tiles.json"]);
            let manifest: Value =
                serde_json::from_str(&std::fs::read_to_string(to.join("tiles.json")).unwrap()).unwrap();
            assert_eq!(
                (&manifest["format"], &manifest["extension"], &manifest["complete"]),
                (&json!("btms tiles v1"), &json!("webp"), &json!(true))
            );
            assert_eq!(manifest["quality"], Format::Webp(quality).quality_value());
            assert_eq!((&manifest["effort"], format::effort_from_manifest(&to)), (&json!("careful"), Effort::Careful));
            assert_eq!(
                (&manifest["converted_from"]["extension"], &manifest["converted_from"]["quality"]),
                (&json!("jpg"), &json!(82))
            );
            // the source's name only, never its path: the manifest is published
            assert_eq!(manifest["converted_from"]["folder"], json!("before"));
            let text = std::fs::read_to_string(to.join("tiles.json")).unwrap();
            assert!(
                !text.contains(&*root.to_string_lossy())
                    && !text.contains(&*root.to_string_lossy().replace('\\', "\\\\")),
                "{text}"
            );
            // what the source said is kept; the ground of its zoom 7 tiles is said
            assert_eq!((&manifest["blocks"], &manifest["lighting"]["azimuth"]), (&json!(2), &json!(315.0)));
            assert_eq!(manifest["bounds"], json!([0.0, -256.0, 128.0, -128.0]));
            assert_eq!(format::from_manifest(&to), Some(Format::Webp(quality)));
            for (z, tx, ty, rgba) in &originals {
                let bytes = std::fs::read(tile_path(&to, *z, *tx, *ty, "webp")).unwrap();
                let opaque = rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
                assert_eq!(webp::has_alpha(&bytes), Some(!opaque));
                let back = webp::decode(&bytes).unwrap().2;
                let source =
                    format::decode_file(&tile_path(&from, *z, *tx, *ty, if opaque { "jpg" } else { "png" })).unwrap();
                if quality == Quality::Lossless {
                    // the source's pixels exactly (a JPEG's included: its defects with them)
                    assert!(back == source, "{z}/{tx}/{ty}");
                } else {
                    // (the synthetic noise of `ground` is harder than the game's: see webp.rs)
                    assert!(psnr(&source, &back) > 30.0, "{z}/{tx}/{ty}: {:.1} dB", psnr(&source, &back));
                    let alpha = |v: &[u8]| v.as_chunks::<4>().0.iter().map(|p| p[3]).collect::<Vec<u8>>();
                    assert!(alpha(&back) == alpha(&source));
                }
            }
        }
        // never in place, nor into the folder converted or one inside it, nor into a stranger's folder
        for bad in [from.clone(), from.join("9"), root.clone()] {
            let e = run(&Options {
                from: from.clone(),
                to: bad.clone(),
                quality: Quality::Lossy(90),
                threads: 1,
                effort: Effort::Careful,
            })
            .unwrap_err();
            assert!(format!("{e:#}").contains("never in place"), "{}: {e:#}", bad.display());
        }
        let stranger = root.join("other");
        std::fs::create_dir_all(&stranger).unwrap();
        std::fs::write(stranger.join("photo.txt"), b"a").unwrap();
        // nor into a render's folder (a manifest of this program without `converted_from`): its tiles stay
        let render = root.join("render");
        older_folder(&render);
        let kept = walk(&render);
        for bad in [stranger.clone(), render.clone()] {
            let e = run(&Options {
                from: from.clone(),
                to: bad.clone(),
                quality: Quality::Lossy(90),
                threads: 1,
                effort: Effort::Careful,
            })
            .unwrap_err();
            assert!(format!("{e:#}").contains("neither empty nor an earlier conversion"), "{}: {e:#}", bad.display());
        }
        assert_eq!(walk(&render), kept);
        assert_eq!(walk(&from), before);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Every file under `dir`, sorted, with its bytes.
    fn walk(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut all = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let bytes = std::fs::read(&p).unwrap();
                    all.push((p, bytes));
                }
            }
        }
        all.sort();
        all
    }
}
