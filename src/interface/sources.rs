//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The folders of tiles the page shows: what one holds (its `tiles.json`, the ground it covers),
//! the path of a tile asked for by its address, and the emptying of the preview folder before a
//! zone is rendered into it. The server only ever deletes there, and only what the render of a
//! zone wrote: `tiles.json`, the page `index.html` beside it and the folders `0` to `9` (a folder
//! that holds a render of the whole map is refused).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use serde_json::Value;

use super::Message;
use super::job::Source;
use crate::tiles::format::PAGE_NAME;

/// Name of the manifest of a folder of tiles.
const MANIFEST: &str = "tiles.json";
/// What the `format` of a BTMS manifest starts with (a prefix of `format::MANIFEST_FORMAT`).
const FORMAT: &str = "btms tiles";
/// Side of a block in metres: a zoom 7 tile.
const BLOCK: f64 = 128.0;

/// What a folder of tiles holds, for the page.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Facts {
    pub folder: String,
    /// It has a BTMS manifest.
    pub exists: bool,
    pub complete: bool,
    pub blocks: Option<u64>,
    /// The ground its zoom 7 tiles cover, in metres: x1, y1, x2, y2.
    pub bounds: Option<[f64; 4]>,
    /// Changes whenever the manifest is written again: what the server compares to tell that the
    /// tiles changed without it.
    #[serde(skip)]
    pub stamp: Option<(SystemTime, u64)>,
}

/// The manifest of a folder, when it is a BTMS one.
fn manifest(dir: &Path) -> Option<Value> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(MANIFEST)).ok()?).ok()?;
    value.get("format")?.as_str()?.starts_with(FORMAT).then_some(value)
}

/// The blocks a folder has a zoom 7 tile of: the least and the greatest, in x and in y.
fn block_range(dir: &Path) -> Option<[i32; 4]> {
    let mut range: Option<[i32; 4]> = None;
    for column in std::fs::read_dir(dir.join("7")).ok()?.flatten() {
        let Some(x) = column.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else { continue };
        let Ok(tiles) = std::fs::read_dir(column.path()) else { continue };
        for tile in tiles.flatten() {
            let name = tile.file_name();
            let Some((stem, ext)) = name.to_str().and_then(|n| n.rsplit_once('.')) else { continue };
            let Some(y) = stem.parse::<i32>().ok().filter(|_| ext == "webp" || ext == "jpg" || ext == "png") else {
                continue;
            };
            let r = range.get_or_insert([x, y, x, y]);
            *r = [r[0].min(x), r[1].min(y), r[2].max(x), r[3].max(y)];
        }
    }
    range
}

pub fn facts(dir: &Path) -> Facts {
    let mut facts = Facts { folder: dir.display().to_string(), ..Facts::default() };
    facts.stamp = std::fs::metadata(dir.join(MANIFEST)).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
    let Some(manifest) = manifest(dir) else { return facts };
    facts.exists = true;
    // a manifest from before the key was written while the folder fills: it was only written at the end
    facts.complete = manifest.get("complete").and_then(Value::as_bool).unwrap_or(true);
    facts.blocks = manifest.get("blocks").and_then(Value::as_u64);
    facts.bounds = match block_range(dir) {
        Some([x1, y1, x2, y2]) => {
            Some([x1 as f64 * BLOCK, y1 as f64 * BLOCK, (x2 + 1) as f64 * BLOCK, (y2 + 1) as f64 * BLOCK])
        }
        // no tile yet: the zone asked for, widened to whole blocks
        None => manifest.get("zone").and_then(|z| serde_json::from_value::<[f64; 4]>(z.clone()).ok()).map(|z| {
            [
                (z[0] / BLOCK).floor() * BLOCK,
                (z[1] / BLOCK).floor() * BLOCK,
                (z[2] / BLOCK).ceil() * BLOCK,
                (z[3] / BLOCK).ceil() * BLOCK,
            ]
        }),
    };
    facts
}

/// Whether the folder of the whole map holds a BTMS map (its `tiles.json`), the one a job of
/// `update` draws again in part: else refused, with the reason. Nothing more is asked of the
/// manifest here: the engine says itself what it cannot update (a folder another render is
/// writing, a map of another kind), and its reason reaches the page.
pub fn map_present(map: &Path) -> Result<(), Message> {
    if manifest(map).is_some() {
        return Ok(());
    }
    let folder = map.display().to_string();
    Err(Message::new(
        "no_map_to_update",
        format!("no map to update: the whole-map folder ({folder}) has no BTMS tiles.json. Render the whole map first, or choose the folder of a map already drawn."),
    )
    .with("folder", folder))
}

/// A tile asked for: `/tiles/<source>/<z>/<tx>/<ty>`, `source` being `map` or `preview`, `z` one
/// digit, `tx` and `ty` whole numbers that may be negative. Anything else is refused: the path of
/// the file is made from the numbers read, never from the text given.
pub fn tile_request(path: &str) -> Option<(Source, u8, i32, i32)> {
    let mut parts = path.strip_prefix("/tiles/")?.split('/');
    let source = match parts.next()? {
        "map" => Source::Map,
        "preview" => Source::Preview,
        _ => return None,
    };
    let z = parts.next()?;
    let z = (z.len() == 1 && z.as_bytes()[0].is_ascii_digit()).then(|| z.as_bytes()[0] - b'0')?;
    let whole = |text: &str| -> Option<i32> {
        let digits = text.strip_prefix('-').unwrap_or(text);
        (!digits.is_empty() && digits.len() <= 7 && digits.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse().ok())?
    };
    let (tx, ty) = (whole(parts.next()?)?, whole(parts.next()?)?);
    parts.next().is_none().then_some((source, z, tx, ty))
}

/// The file of a tile in `dir` and its media type, None when there is none: its `.webp`, else
/// its `.jpg` or `.png` (an older folder; during a WebP render into such a folder, a tile not
/// written again yet shows as it was).
pub fn tile_file(dir: &Path, z: u8, tx: i32, ty: i32) -> Option<(PathBuf, &'static str)> {
    let column = dir.join(z.to_string()).join(tx.to_string());
    [("webp", "image/webp"), ("jpg", "image/jpeg"), ("png", "image/png")]
        .into_iter()
        .map(|(ext, kind)| (column.join(format!("{ty}.{ext}")), kind))
        .find(|(file, _)| file.is_file())
}

/// A path as it is compared: absolute, links followed in the part of it that exists, case
/// folded on Windows.
fn comparable(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let (mut existing, mut rest) = (absolute.as_path(), Vec::new());
    let mut full = loop {
        if let Ok(real) = std::fs::canonicalize(existing) {
            break real;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent;
            }
            _ => break existing.to_path_buf(),
        }
    };
    full.extend(rest.iter().rev());
    if cfg!(windows) { PathBuf::from(full.to_string_lossy().to_lowercase()) } else { full }
}

/// Whether two folders are the same one, or one inside the other.
pub fn related(a: &Path, b: &Path) -> bool {
    let (a, b) = (comparable(a), comparable(b));
    a.starts_with(&b) || b.starts_with(&a)
}

/// Empties the preview folder before a zone is rendered into it. Only when it is empty or holds
/// a BTMS manifest, and then only `tiles.json`, the page beside it and the folders `0` to `9` go:
/// anything else is refused with the reason, and nothing is deleted. Refused too when the folder
/// is the one of the whole map (`map`), or holds it, or is in it: the finished map would pass the
/// check. And when its manifest is not the one of a zone: a render of the whole map kept
/// elsewhere (a copy, an older output folder) and chosen as the preview folder to look at it
/// would be emptied by the next preview.
pub fn empty_preview(preview: &Path, map: &Path) -> Result<(), Message> {
    let folder = preview.display().to_string();
    if related(preview, map) {
        return Err(Message::new(
            "preview_is_map",
            format!("the preview folder ({folder}) is the whole map's folder, or one holds the other: it would be emptied before the render. Choose another preview folder."),
        )
        .with("folder", folder));
    }
    let Ok(meta) = std::fs::symlink_metadata(preview) else { return Ok(()) };
    if !meta.is_dir() {
        return Err(Message::new("preview_not_folder", format!("the preview folder ({folder}) is not a folder"))
            .with("folder", folder));
    }
    let mut entries = std::fs::read_dir(preview).map_err(|e| {
        Message::new("folder_unreadable", format!("reading {folder}: {e}"))
            .with("folder", folder.as_str())
            .with("error", e.to_string())
    })?;
    if entries.next().is_none() {
        return Ok(());
    }
    let Some(manifest) = manifest(preview) else {
        return Err(Message::new(
            "preview_not_tiles",
            format!("the preview folder ({folder}) holds something other than BTMS tiles (no tiles.json): nothing is deleted there. Choose an empty folder."),
        )
        .with("folder", folder));
    };
    // only the render of a zone is a preview: the manifest of a whole map has no zone
    if manifest.get("zone").and_then(|z| serde_json::from_value::<[f64; 4]>(z.clone()).ok()).is_none() {
        return Err(Message::new(
            "preview_holds_map",
            format!("the preview folder ({folder}) holds a render of the whole map: it cannot serve as the preview folder, it would be emptied. Nothing is deleted there. Choose another preview folder."),
        )
        .with("folder", folder));
    }
    let delete_failed = |path: &Path, e: std::io::Error| {
        let path = path.display().to_string();
        Message::new("delete_failed", format!("deleting {path}: {e}")).with("path", path).with("error", e.to_string())
    };
    for z in 0..=9 {
        let dir = preview.join(z.to_string());
        // a real folder only: a link is left alone
        if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_dir()) {
            std::fs::remove_dir_all(&dir).map_err(|e| delete_failed(&dir, e))?;
        }
    }
    // the page of the web map the render wrote beside its manifest (a file of that name only)
    let page = preview.join(PAGE_NAME);
    if std::fs::symlink_metadata(&page).is_ok_and(|m| m.is_file()) {
        std::fs::remove_file(&page).map_err(|e| delete_failed(&page, e))?;
    }
    // the manifest last: if a folder could not be removed, the next try still finds it
    let file = preview.join(MANIFEST);
    std::fs::remove_file(&file).map_err(|e| delete_failed(&file, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("btms-tests-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tile(dir: &Path, z: i32, tx: i32, ty: i32, ext: &str) {
        let column = dir.join(z.to_string()).join(tx.to_string());
        std::fs::create_dir_all(&column).unwrap();
        std::fs::write(column.join(format!("{ty}.{ext}")), b"tile").unwrap();
    }

    const OURS: &str =
        r#"{"format": "btms tiles v1", "blocks": 4, "complete": false, "zone": [384.0, -1024.0, 640.0, -768.0]}"#;

    #[test]
    fn a_manifest_written_by_btms_is_recognised() {
        assert!(crate::tiles::format::MANIFEST_FORMAT.starts_with(FORMAT), "{}", crate::tiles::format::MANIFEST_FORMAT);
    }

    #[test]
    fn tile_addresses() {
        assert_eq!(tile_request("/tiles/map/7/3/-8"), Some((Source::Map, 7, 3, -8)));
        assert_eq!(tile_request("/tiles/preview/9/-125/249"), Some((Source::Preview, 9, -125, 249)));
        assert_eq!(tile_request("/tiles/map/0/-1/0"), Some((Source::Map, 0, -1, 0)));
        assert_eq!(tile_request("/tiles/map/7/007/-0"), Some((Source::Map, 7, 7, 0)));
        for bad in [
            "/tiles/map/7/3",
            "/tiles/map/7/3/-8/",
            "/tiles/map/7/3/-8/9",
            "/tiles/map/7/3/-8.jpg",
            "/tiles/map/10/3/8",
            "/tiles/map/-1/3/8",
            "/tiles/map/7/+3/8",
            "/tiles/map/7/3/--8",
            "/tiles/map/7/3/ 8",
            "/tiles/map/7/3/8e2",
            "/tiles/map/7/12345678/8",
            "/tiles/map/7//8",
            "/tiles/map/7/../8",
            "/tiles/map/../../7/3/8",
            "/tiles/map/7/..%2f..%2f/8",
            "/tiles/map/7/3\\..\\..\\x/8",
            "/tiles/other/7/3/8",
            "/tiles/Map/7/3/8",
            "/tiles//7/3/8",
            "/tiles/map/7/3/٣",
            "/tiles",
            "/tiles/",
            "tiles/map/7/3/8",
            "/api/tiles/map/7/3/8",
        ] {
            assert_eq!(tile_request(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_tile_is_found_by_its_numbers() {
        let dir = scratch("tiles");
        tile(&dir, 7, 3, -8, "jpg");
        tile(&dir, 7, -4, 0, "png");
        tile(&dir, 7, 5, 1, "webp");
        assert_eq!(tile_file(&dir, 7, 3, -8), Some((dir.join("7").join("3").join("-8.jpg"), "image/jpeg")));
        assert_eq!(tile_file(&dir, 7, -4, 0), Some((dir.join("7").join("-4").join("0.png"), "image/png")));
        assert_eq!(tile_file(&dir, 7, 5, 1), Some((dir.join("7").join("5").join("1.webp"), "image/webp")));
        // a WebP tile written over an older folder is the one served
        tile(&dir, 7, 3, -8, "webp");
        assert_eq!(tile_file(&dir, 7, 3, -8), Some((dir.join("7").join("3").join("-8.webp"), "image/webp")));
        assert_eq!(tile_file(&dir, 7, 3, 8), None);
        assert_eq!(tile_file(&dir, 6, 3, -8), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_a_folder_holds() {
        let dir = scratch("facts");
        // no manifest: nothing to show
        let f = facts(&dir);
        assert_eq!((f.exists, f.complete, f.blocks, f.bounds, f.stamp), (false, false, None, None, None));
        // a manifest and no tile yet: the zone, in whole blocks
        std::fs::write(dir.join(MANIFEST), OURS.replace("384.0", "400.5")).unwrap();
        let f = facts(&dir);
        assert_eq!((f.exists, f.complete, f.blocks), (true, false, Some(4)));
        assert_eq!(f.bounds, Some([384.0, -1024.0, 640.0, -768.0]));
        assert!(f.stamp.is_some());
        // with tiles: what the zoom 7 tiles cover
        for (x, y, ext) in [(3, -8, "jpg"), (4, -8, "webp"), (3, -7, "png"), (-2, 5, "webp")] {
            tile(&dir, 7, x, y, ext);
        }
        std::fs::write(dir.join("7").join("3").join("-9.jpg.tmp"), b"being written").unwrap();
        std::fs::write(dir.join("7").join("notes.txt"), b"").unwrap();
        assert_eq!(facts(&dir).bounds, Some([-256.0, -1024.0, 640.0, 768.0]));
        // a manifest from before `complete` existed is a finished one; whole map: no zone
        std::fs::write(dir.join(MANIFEST), r#"{"format": "btms tiles v1", "blocks": 6097, "zone": null}"#).unwrap();
        let f = facts(&dir);
        assert_eq!((f.exists, f.complete, f.blocks), (true, true, Some(6097)));
        // someone else's tiles.json, or a broken one
        for other in [r#"{"format": "other"}"#, "{", "[]"] {
            std::fs::write(dir.join(MANIFEST), other).unwrap();
            assert!(!facts(&dir).exists, "{other}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_preview_folder_is_emptied_only_of_tiles() {
        let root = scratch("emptying");
        let (preview, map) = (root.join("preview"), root.join("map"));
        std::fs::create_dir_all(&map).unwrap();
        // not there yet, or empty: nothing to do
        assert_eq!(empty_preview(&preview, &map), Ok(()));
        std::fs::create_dir_all(&preview).unwrap();
        assert_eq!(empty_preview(&preview, &map), Ok(()));
        // a BTMS render: its manifest, its page and its zoom folders go, the rest stays
        std::fs::write(preview.join(MANIFEST), OURS).unwrap();
        for z in 0..=9 {
            tile(&preview, z, 1, -1, "jpg");
        }
        tile(&preview, 10, 0, 0, "jpg");
        std::fs::create_dir_all(preview.join("notes")).unwrap();
        std::fs::write(preview.join("notes").join("a.txt"), b"a").unwrap();
        std::fs::write(preview.join("readme.txt"), b"b").unwrap();
        std::fs::write(preview.join("07"), b"a file").unwrap();
        std::fs::write(preview.join("index.html"), b"<html>").unwrap();
        assert_eq!(empty_preview(&preview, &map), Ok(()));
        let mut left: Vec<String> = std::fs::read_dir(&preview)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["07", "10", "notes", "readme.txt"]);
        assert!(preview.join("notes").join("a.txt").is_file() && preview.join("10").join("0").join("0.jpg").is_file());
        // what is left is not a render any more: refused, and nothing goes
        let refusal = empty_preview(&preview, &map).unwrap_err();
        assert!(
            refusal.text.contains("nothing is deleted there") && refusal.code == "preview_not_tiles",
            "{refusal:?}"
        );
        assert_eq!(std::fs::read_dir(&preview).unwrap().count(), 4);
        // zoom folders without a manifest, or with someone else's: refused
        let other = root.join("other");
        tile(&other, 7, 0, 0, "jpg");
        assert!(empty_preview(&other, &map).is_err());
        std::fs::write(other.join(MANIFEST), r#"{"format": "something else"}"#).unwrap();
        assert!(empty_preview(&other, &map).is_err());
        assert!(other.join("7").join("0").join("0.jpg").is_file());
        // a file in place of the folder
        assert!(empty_preview(&preview.join("readme.txt"), &map).is_err());
        // a render of the whole map kept in another folder than the output one (no zone in its
        // manifest, or a manifest from before the key, or a zone that is not one): refused, whole
        let whole = root.join("kept-map");
        for kept in [
            r#"{"format": "btms tiles v1", "blocks": 6094, "complete": true, "zone": null}"#,
            r#"{"format": "btms tiles v1", "blocks": 6094}"#,
            r#"{"format": "btms tiles v1", "blocks": 6094, "zone": [0, 0, 128]}"#,
        ] {
            tile(&whole, 7, 3, -8, "jpg");
            tile(&whole, 0, 0, 0, "png");
            std::fs::write(whole.join(MANIFEST), kept).unwrap();
            let refusal = empty_preview(&whole, &map).unwrap_err();
            assert!(
                refusal.text.contains("whole map") && refusal.text.contains("Nothing is deleted there"),
                "{refusal:?}"
            );
            assert_eq!(
                (refusal.code.as_str(), refusal.params.get("folder")),
                ("preview_holds_map", Some(&Value::from(whole.display().to_string())))
            );
            assert_eq!(std::fs::read_to_string(whole.join(MANIFEST)).unwrap(), kept);
            assert!(
                whole.join("7").join("3").join("-8.jpg").is_file() && whole.join("0").join("0").join("0.png").is_file()
            );
        }
        // the same folder with the manifest of a zone is a preview
        std::fs::write(whole.join(MANIFEST), OURS).unwrap();
        assert_eq!(empty_preview(&whole, &map), Ok(()));
        assert_eq!(std::fs::read_dir(&whole).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_folder_of_the_whole_map_is_never_emptied() {
        let root = scratch("kept-map");
        let map = root.join("output").join("tiles");
        std::fs::create_dir_all(&map).unwrap();
        std::fs::write(map.join(MANIFEST), OURS.replace("false", "true")).unwrap();
        tile(&map, 7, 3, -8, "jpg");
        // the same folder, however it is written
        for same in [
            map.clone(),
            map.join("."),
            root.join("output").join("other").join("..").join("tiles"),
            PathBuf::from(map.to_string_lossy().to_uppercase()),
        ] {
            if cfg!(windows) || same.exists() {
                assert!(empty_preview(&same, &map).is_err(), "{}", same.display());
            }
        }
        // a folder that holds it, and a folder inside it
        std::fs::write(root.join("output").join(MANIFEST), OURS).unwrap();
        assert!(empty_preview(&root.join("output"), &map).is_err());
        assert!(empty_preview(&map.join("7"), &map).is_err());
        assert!(map.join(MANIFEST).is_file() && map.join("7").join("3").join("-8.jpg").is_file());
        // a folder inside it that does not exist yet is in it all the same
        assert!(related(&map.join("preview").join("test"), &map));
        // a neighbour is not it
        assert!(!related(&root.join("output").join("tiles-2"), &map));
        assert!(!related(&root.join("output").join("preview"), &map));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// What reading the extent of a whole map costs (the page asks the state every second when
    /// nothing runs, and the folders are read again at most once a second): 6 100 empty tiles of
    /// zoom 7, as many as a whole map has. By hand: `cargo test --release extent_of_a_whole_map -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_extent_of_a_whole_map_is_read_quickly() {
        let dir = scratch("extent");
        let mut n = 0;
        for x in -32..48 {
            let column = dir.join("7").join(x.to_string());
            std::fs::create_dir_all(&column).unwrap();
            for y in -60..16 {
                std::fs::write(column.join(format!("{y}.jpg")), b"").unwrap();
                n += 1;
            }
        }
        let mut times = Vec::new();
        for _ in 0..10 {
            let from = std::time::Instant::now();
            assert_eq!(block_range(&dir), Some([-32, -60, 47, 15]));
            times.push(from.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        println!("{n} tiles: {:.1} ms median, {:.1} ms at worst", times[5], times[9]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
