//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! What the tiles of a folder are written as ([`Format`]): WebP, the default (`<z>/<tx>/<ty>.webp`,
//! with its transparency when the tile is partly empty), or the older JPEG and PNG (`.jpg` when
//! opaque, `.png` when partly empty). Chosen by `--format webp|jpeg` and `--quality <0-100>|lossless`
//! ([`from_options`]); one extension per folder. Beside it, how hard the WebP encoder works
//! ([`Effort`]: `--webp-effort fast|careful`), which changes the files a little and the time a lot.
//!
//! And what goes with it: the keys of `tiles.json` that say it ([`describe`]: `format`,
//! `extension`, `quality`, `effort`), the page of the web map written beside it ([`write_page`]: the
//! folder is published as it is on a static web server), the reading of a tile back ([`decode_file`]).
//!
//! The format is known by folder ([`set`], [`of`], [`effort`]): `write_tile` and `read_tile` keep
//! their arguments, and the tiles of a folder are all written the same way, whoever writes them
//! (the blocks, the lower zooms, the last holes). A folder nobody set is read from its
//! `tiles.json`; without one, it is an older folder (JPEG and PNG).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::webp::{self, Effort, Quality, Settings};

/// The quality of the lossy WebP tiles when `--quality` is not given. Chosen by measuring the
/// encoding of blocks drawn in memory.
pub const DEFAULT_QUALITY: u8 = 90;
/// The JPEG quality of the older opaque tiles (`--format jpeg`).
pub const JPEG_QUALITY: u8 = super::JPEG_QUALITY;
/// What the manifest's `format` says.
pub const MANIFEST_FORMAT: &str = "btms tiles v1";
/// Every extension a tile of this program can have: what a tile written replaces.
pub const EXTENSIONS: [&str; 3] = ["webp", "jpg", "png"];
/// The page of the web map, written beside `tiles.json` (`web/viewer/index.html`).
pub const PAGE: &[u8] = include_bytes!("../../web/viewer/index.html");
/// Its name in the folder of tiles.
pub const PAGE_NAME: &str = "index.html";

/// What the tiles of a folder are written as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// `.webp`, opaque or partly empty alike.
    Webp(Quality),
    /// `.jpg` (quality 82) when opaque, `.png` when partly empty: the older tiles.
    Jpeg,
}

impl Default for Format {
    fn default() -> Format {
        Format::Webp(Quality::Lossy(DEFAULT_QUALITY))
    }
}

impl Format {
    /// The manifest's `extension`: `webp`, or `jpg` for the older JPEG and PNG.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Webp(_) => "webp",
            Format::Jpeg => "jpg",
        }
    }

    /// The extension of a tile's file.
    pub fn file_extension(self, opaque: bool) -> &'static str {
        match self {
            Format::Webp(_) => "webp",
            Format::Jpeg if opaque => "jpg",
            Format::Jpeg => "png",
        }
    }

    /// The extensions a tile of such a folder may have, in the order they are looked for.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Format::Webp(_) => &["webp"],
            Format::Jpeg => &["jpg", "png"],
        }
    }

    /// The manifest's `quality`: what the encoder did.
    pub fn quality_value(self) -> Value {
        match self {
            Format::Webp(Quality::Lossless) => json!("lossless"),
            Format::Webp(Quality::Lossy(q)) => json!(q),
            Format::Jpeg => json!(JPEG_QUALITY),
        }
    }

    /// For the `settings: …` line and the reports.
    pub fn label(self) -> String {
        match self {
            Format::Webp(Quality::Lossless) => "WebP lossless".to_string(),
            Format::Webp(Quality::Lossy(q)) if q == DEFAULT_QUALITY => format!("WebP quality {q} (default)"),
            Format::Webp(Quality::Lossy(q)) => format!("WebP quality {q}"),
            Format::Jpeg => format!("JPEG quality {JPEG_QUALITY} and PNG (the older format)"),
        }
    }

    /// The encoder, for the manifest (`effort`: the WebP encoder's).
    pub fn encoder(self, effort: Effort) -> String {
        match self {
            Format::Webp(q) => {
                let s = Settings::tiles(q, effort);
                match q {
                    Quality::Lossless => format!("libwebp {} lossless, level {}", webp::version(), s.effort),
                    Quality::Lossy(_) => format!(
                        "libwebp {} lossy, method {}{}",
                        webp::version(),
                        s.effort,
                        if s.sharp_yuv { ", sharp YUV conversion" } else { ", plain YUV conversion" }
                    ),
                }
            }
            Format::Jpeg => format!(
                "JPEG quality {JPEG_QUALITY} (4:4:4) when the tile is full, lossless PNG when it is partly empty"
            ),
        }
    }

    /// The effort, for the `settings: …` line: `, fast encoding (method 0)`; nothing for the
    /// older JPEG and PNG.
    pub fn effort_label(self, effort: Effort) -> String {
        let Format::Webp(q) = self else { return String::new() };
        let s = Settings::tiles(q, effort);
        let how = match q {
            Quality::Lossless => format!("level {}", s.effort),
            Quality::Lossy(_) => format!("method {}{}", s.effort, if s.sharp_yuv { ", sharp YUV" } else { "" }),
        };
        let word = match effort {
            Effort::Fast => "fast",
            Effort::Careful => "careful",
        };
        format!(", {word} encoding ({how})")
    }

    /// The file of a WebP tile, encoded with `effort` (None for the older JPEG and PNG: the
    /// caller writes them).
    pub fn encode(self, rgba: &[u8], side: usize, opaque: bool, effort: Effort) -> Option<Result<Vec<u8>>> {
        match self {
            Format::Webp(q) => Some(webp::encode(rgba, side, side, opaque, &Settings::tiles(q, effort))),
            Format::Jpeg => None,
        }
    }

    /// The tiles written: `12 WebP tiles (10 full, 2 partly empty), 3 empty`.
    pub fn counts(self, full: usize, partial: usize, empty: usize) -> String {
        match self {
            Format::Webp(_) => {
                format!("{} WebP tiles ({full} full, {partial} partly empty), {empty} empty", full + partial)
            }
            Format::Jpeg => format!("{full} JPEG tiles, {partial} PNG, {empty} empty"),
        }
    }
}

/// `--format` and `--quality`, as `options.rs` took them out of the arguments.
pub fn from_options(format: Option<&str>, quality: Option<&str>) -> Result<Format> {
    let webp = match format.map(str::trim) {
        None | Some("webp") => true,
        Some("jpeg" | "jpg") => false,
        Some(other) => bail!("--format {other}: expected webp (the default) or jpeg (JPEG and PNG, the older format)"),
    };
    let Some(quality) = quality else {
        return Ok(if webp { Format::default() } else { Format::Jpeg });
    };
    if !webp {
        bail!(
            "--quality only applies to --format webp: JPEG tiles keep their fixed quality {JPEG_QUALITY} (and their PNG are lossless)"
        );
    }
    Ok(Format::Webp(parse_quality(quality)?))
}

/// `--quality`: a whole number from 0 to 100, or `lossless`.
pub fn parse_quality(text: &str) -> Result<Quality> {
    match text.trim() {
        "lossless" => Ok(Quality::Lossless),
        t => match t.parse::<u8>() {
            Ok(q) if q <= 100 => Ok(Quality::Lossy(q)),
            _ => {
                bail!("--quality {text}: expected a whole number from 0 to 100 (default {DEFAULT_QUALITY}) or lossless")
            }
        },
    }
}

/// The format of each folder written or read in this run, and the WebP encoder's effort.
static FOLDERS: RwLock<Vec<(PathBuf, Format, Effort)>> = RwLock::new(Vec::new());

/// The tiles of `out` are written as `format`, with `effort`, from now on.
pub fn set(out: &Path, format: Format, effort: Effort) {
    let mut folders = FOLDERS.write().unwrap_or_else(|e| e.into_inner());
    folders.retain(|(dir, ..)| dir != out);
    folders.push((out.to_path_buf(), format, effort));
}

/// What [`set`] said for `out`, else what its `tiles.json` says, else JPEG and PNG (an older
/// folder).
fn entry(out: &Path) -> (Format, Effort) {
    if let Some((_, f, e)) = FOLDERS.read().unwrap_or_else(|e| e.into_inner()).iter().find(|(dir, ..)| dir == out) {
        return (*f, *e);
    }
    let found = (from_manifest(out).unwrap_or(Format::Jpeg), effort_from_manifest(out));
    let mut folders = FOLDERS.write().unwrap_or_else(|e| e.into_inner());
    match folders.iter().find(|(dir, ..)| dir == out) {
        Some((_, f, e)) => (*f, *e),
        None => {
            folders.push((out.to_path_buf(), found.0, found.1));
            found
        }
    }
}

/// What the tiles of `out` are written as: what [`set`] said, else what its `tiles.json` says,
/// else JPEG and PNG (an older folder).
pub fn of(out: &Path) -> Format {
    entry(out).0
}

/// How hard the WebP encoder works on the tiles of `out`: what [`set`] said, else the manifest's
/// `effort`, else [`Effort::Fast`] (a render's).
pub fn effort(out: &Path) -> Effort {
    entry(out).1
}

/// The format `tiles.json` of `dir` says, None when it has none: `extension` and `quality` (a
/// manifest without `extension` is an older one: JPEG and PNG).
pub fn from_manifest(dir: &Path) -> Option<Format> {
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("tiles.json")).ok()?).ok()?;
    manifest.get("format")?.as_str()?.starts_with("btms tiles").then_some(())?;
    Some(match manifest.get("extension").and_then(Value::as_str) {
        Some("webp") => Format::Webp(match manifest.get("quality") {
            Some(Value::String(s)) if s == "lossless" => Quality::Lossless,
            Some(v) => v
                .as_u64()
                .filter(|q| *q <= 100)
                .map(|q| Quality::Lossy(q as u8))
                .unwrap_or(Quality::Lossy(DEFAULT_QUALITY)),
            None => Quality::Lossy(DEFAULT_QUALITY),
        }),
        _ => Format::Jpeg,
    })
}

/// The manifest's `effort` (`fast` or `careful`); [`Effort::Fast`] when it says none (an older
/// folder, or none at all).
pub fn effort_from_manifest(dir: &Path) -> Effort {
    std::fs::read_to_string(dir.join("tiles.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|m| m.get("effort").and_then(Value::as_str).and_then(|w| Effort::parse(w).ok()))
        .unwrap_or_default()
}

/// Sets the keys of `tiles.json` that say how the tiles are written (`effort` for WebP only).
pub fn describe(manifest: &mut Value, format: Format, effort: Effort) {
    manifest["format"] = json!(MANIFEST_FORMAT);
    manifest["extension"] = json!(format.extension());
    manifest["quality"] = format.quality_value();
    manifest["encoder"] = json!(format.encoder(effort));
    match format {
        Format::Webp(_) => manifest["effort"] = json!(effort.word()),
        Format::Jpeg => {
            manifest.as_object_mut().map(|m| m.remove("effort"));
        }
    }
    manifest["files"] = json!(match format {
        Format::Webp(_) => "<z>/<tx>/<ty>.webp (with its transparency when the tile is partly empty); missing = empty",
        Format::Jpeg => "<z>/<tx>/<ty>.jpg (opaque) or .png (partly empty); missing = empty",
    });
    manifest["page"] = json!(PAGE_NAME);
}

/// The ground the zoom 7 tiles of `dir` cover, in metres, `[x1, y1, x2, y2]`: the manifest's
/// `bounds`, which the web page frames the whole map on. None when it has none.
pub fn bounds(dir: &Path) -> Option<[f64; 4]> {
    let mut range: Option<[i32; 4]> = None;
    for column in std::fs::read_dir(dir.join("7")).ok()?.flatten() {
        let Some(x) = column.file_name().to_str().and_then(|n| n.parse::<i32>().ok()) else { continue };
        let Ok(tiles) = std::fs::read_dir(column.path()) else { continue };
        for tile in tiles.flatten() {
            let name = tile.file_name();
            let Some((stem, ext)) = name.to_str().and_then(|n| n.rsplit_once('.')) else { continue };
            let Some(y) = stem.parse::<i32>().ok().filter(|_| EXTENSIONS.contains(&ext)) else { continue };
            let r = range.get_or_insert([x, y, x, y]);
            *r = [r[0].min(x), r[1].min(y), r[2].max(x), r[3].max(y)];
        }
    }
    let side = super::side(7) as f64;
    range.map(|[x1, y1, x2, y2]| [x1 as f64 * side, y1 as f64 * side, (x2 + 1) as f64 * side, (y2 + 1) as f64 * side])
}

/// Writes `bytes` to `file` whole: beside it first, then renamed over.
pub fn write_whole(file: &Path, bytes: &[u8]) -> Result<()> {
    let temp = file.with_extension("tmp");
    std::fs::write(&temp, bytes).with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, file).with_context(|| format!("renaming {}", temp.display()))
}

/// Writes the page of the web map beside `tiles.json` (`index.html`).
pub fn write_page(out: &Path) -> Result<()> {
    write_whole(&out.join(PAGE_NAME), PAGE)
}

/// Says when `out` holds tiles of another format than the one about to be written: those the
/// run writes again replace them, the others stay (a reader follows `extension` and skips them).
pub fn tell_if_mixed(out: &Path, format: Format) {
    if let Some(before) = from_manifest(out).filter(|b| b.extension() != format.extension()) {
        println!(
            "format: {} held .{} tiles: each tile written again as .{} replaces its own; those this render does not write again stay (readers follow \"extension\" in tiles.json and skip them)",
            out.display(),
            before.extensions().join(", ."),
            format.extension()
        );
    }
}

/// The extension of a zoom 7 tile of `out` that is not one of `format`'s, None when there is
/// none (a new or empty folder, or one of the same format).
fn other_tiles(out: &Path, format: Format) -> Option<&'static str> {
    for column in std::fs::read_dir(out.join("7")).ok()?.flatten() {
        let Ok(tiles) = std::fs::read_dir(column.path()) else { continue };
        for tile in tiles.flatten() {
            let name = tile.file_name();
            let Some((stem, ext)) = name.to_str().and_then(|n| n.rsplit_once('.')) else { continue };
            if stem.parse::<i32>().is_err() || format.extensions().contains(&ext) {
                continue;
            }
            if let Some(other) = EXTENSIONS.into_iter().find(|e| *e == ext) {
                return Some(other);
            }
        }
    }
    None
}

/// A zone is not drawn in one format into a folder that holds tiles of another: its manifest
/// would then send the readers to the zone's extension, and the rest of the map would vanish
/// for them; the zooms 6 to 0 above the zone, made again from its tiles alone, would lose the
/// rest too. (The whole map may be: every tile is drawn again, and the run says what stays.)
pub fn refuse_mixed_zone(out: &Path, format: Format) -> Result<()> {
    let Some(other) = other_tiles(out, format) else { return Ok(()) };
    let keep = if other == "webp" { "--format webp" } else { "--format jpeg" };
    bail!(
        "tiles: {} already holds .{other} tiles: a zone in .{} would mix two formats there (readers, which follow \"extension\" in tiles.json, would then see only the zone, and zooms 6 to 0 above it would lose the rest). Render the zone into another folder, or in the folder's format ({keep}){}",
        out.display(),
        format.extension(),
        if other == "webp" {
            ""
        } else {
            ", or convert the folder to WebP first (convert <folder> --to <another folder>)"
        }
    )
}

/// A tile's file decoded to RGBA: WebP by libwebp, JPEG and PNG by `image`. None when it cannot
/// be read.
pub fn decode_file(path: &Path) -> Option<Vec<u8>> {
    if path.extension().is_some_and(|e| e == "webp") {
        return webp::decode(&std::fs::read(path).ok()?).ok().map(|(_, _, rgba)| rgba);
    }
    image::open(path).ok().map(|i| i.to_rgba8().into_raw())
}

/// Whether the tile `(z, tx, ty)` of `out` is on disk and opaque, told without decoding it: its
/// `.jpg` (the older JPEG and PNG), or its `.webp` whose header says no alpha.
pub fn opaque_on_disk(out: &Path, z: i32, tx: i32, ty: i32) -> bool {
    match of(out) {
        Format::Jpeg => super::tile_path(out, z, tx, ty, "jpg").is_file(),
        Format::Webp(_) => {
            let Ok(file) = std::fs::File::open(super::tile_path(out, z, tx, ty, "webp")) else { return false };
            // the header of any kind of WebP (VP8, VP8L, VP8X) is within its first 64 bytes
            let mut head = Vec::with_capacity(64);
            if file.take(64).read_to_end(&mut head).is_err() {
                return false;
            }
            webp::has_alpha(&head) == Some(false)
        }
    }
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

    #[test]
    fn the_options_give_the_format() {
        assert_eq!(from_options(None, None).unwrap(), Format::Webp(Quality::Lossy(DEFAULT_QUALITY)));
        assert_eq!(from_options(Some("webp"), Some("75")).unwrap(), Format::Webp(Quality::Lossy(75)));
        assert_eq!(from_options(None, Some("lossless")).unwrap(), Format::Webp(Quality::Lossless));
        assert_eq!(from_options(None, Some("0")).unwrap(), Format::Webp(Quality::Lossy(0)));
        assert_eq!(from_options(None, Some("100")).unwrap(), Format::Webp(Quality::Lossy(100)));
        assert_eq!(from_options(Some("jpeg"), None).unwrap(), Format::Jpeg);
        let refused = |f: Option<&str>, q: Option<&str>| format!("{:#}", from_options(f, q).unwrap_err());
        assert_eq!(
            refused(Some("png"), None),
            "--format png: expected webp (the default) or jpeg (JPEG and PNG, the older format)"
        );
        assert!(refused(Some("jpeg"), Some("90")).starts_with("--quality only applies to --format webp"));
        for bad in ["101", "-1", "90.5", "high", "", "loss_less"] {
            assert_eq!(
                refused(None, Some(bad)),
                format!(
                    "--quality {bad}: expected a whole number from 0 to 100 (default {DEFAULT_QUALITY}) or lossless"
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_manifest_says_the_format_and_is_read_back() {
        let dir = scratch("format-manifest");
        for (format, effort) in [
            (Format::Webp(Quality::Lossy(90)), Effort::Fast),
            (Format::Webp(Quality::Lossy(63)), Effort::Careful),
            (Format::Webp(Quality::Lossless), Effort::Fast),
            (Format::Webp(Quality::Lossless), Effort::Careful),
            (Format::Jpeg, Effort::Careful),
        ] {
            // (a manifest of another version of the format: `describe` writes this one's)
            let mut manifest = json!({"format": "btms tiles v0", "zmax": 9, "files": "old", "effort": "careful"});
            describe(&mut manifest, format, effort);
            assert_eq!(manifest["format"], MANIFEST_FORMAT);
            assert_eq!(manifest["zmax"], 9);
            assert_eq!(manifest["page"], "index.html");
            std::fs::write(dir.join("tiles.json"), serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
            assert_eq!(from_manifest(&dir), Some(format));
            // the effort is said for WebP only: a JPEG folder's is none, the default
            match format {
                Format::Webp(_) => {
                    assert_eq!((&manifest["effort"], effort_from_manifest(&dir)), (&json!(effort.word()), effort))
                }
                Format::Jpeg => assert_eq!((manifest.get("effort"), effort_from_manifest(&dir)), (None, Effort::Fast)),
            }
        }
        let mut manifest = json!({});
        describe(&mut manifest, Format::Webp(Quality::Lossless), Effort::Fast);
        assert_eq!((&manifest["extension"], &manifest["quality"]), (&json!("webp"), &json!("lossless")));
        assert!(manifest["encoder"].as_str().unwrap().ends_with("lossless, level 0"), "{}", manifest["encoder"]);
        describe(&mut manifest, Format::Webp(Quality::Lossy(90)), Effort::Fast);
        assert_eq!((&manifest["extension"], &manifest["quality"]), (&json!("webp"), &json!(90)));
        assert!(manifest["encoder"].as_str().unwrap().starts_with("libwebp "));
        assert!(
            manifest["encoder"].as_str().unwrap().ends_with("lossy, method 0, plain YUV conversion"),
            "{}",
            manifest["encoder"]
        );
        describe(&mut manifest, Format::Webp(Quality::Lossy(90)), Effort::Careful);
        assert!(
            manifest["encoder"].as_str().unwrap().ends_with("lossy, method 4, sharp YUV conversion"),
            "{}",
            manifest["encoder"]
        );
        describe(&mut manifest, Format::Jpeg, Effort::Fast);
        assert_eq!((&manifest["extension"], &manifest["quality"]), (&json!("jpg"), &json!(82)));
        // what the `settings: …` line says of the effort
        assert_eq!(Format::Webp(Quality::Lossy(90)).effort_label(Effort::Fast), ", fast encoding (method 0)");
        assert_eq!(
            Format::Webp(Quality::Lossy(90)).effort_label(Effort::Careful),
            ", careful encoding (method 4, sharp YUV)"
        );
        assert_eq!(Format::Webp(Quality::Lossless).effort_label(Effort::Fast), ", fast encoding (level 0)");
        assert_eq!(Format::Jpeg.effort_label(Effort::Careful), "");
        // a manifest of this program without `extension`: JPEG and PNG; someone else's: nothing
        std::fs::write(dir.join("tiles.json"), r#"{"format": "btms tiles v1", "zone": null}"#).unwrap();
        assert_eq!(from_manifest(&dir), Some(Format::Jpeg));
        assert_eq!(effort_from_manifest(&dir), Effort::Fast);
        std::fs::write(dir.join("tiles.json"), r#"{"format": "other", "extension": "webp"}"#).unwrap();
        assert_eq!(from_manifest(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_s_format_is_the_one_set_else_its_manifest_s() {
        let (a, b, c) = (scratch("format-a"), scratch("format-b"), scratch("format-c"));
        std::fs::write(
            b.join("tiles.json"),
            r#"{"format": "btms tiles v1", "extension": "webp", "quality": "lossless"}"#,
        )
        .unwrap();
        // no manifest: an older folder; the manifest's; what is set wins, for that folder only
        assert_eq!(of(&a), Format::Jpeg);
        assert_eq!((of(&b), effort(&b)), (Format::Webp(Quality::Lossless), Effort::Fast));
        set(&a, Format::Webp(Quality::Lossy(80)), Effort::Fast);
        set(&c, Format::Webp(Quality::Lossy(70)), Effort::Careful);
        assert_eq!(
            (of(&a), of(&b), of(&c)),
            (Format::Webp(Quality::Lossy(80)), Format::Webp(Quality::Lossless), Format::Webp(Quality::Lossy(70)))
        );
        assert_eq!((effort(&a), effort(&c)), (Effort::Fast, Effort::Careful));
        set(&c, Format::Jpeg, Effort::Fast);
        assert_eq!((of(&c), effort(&c)), (Format::Jpeg, Effort::Fast));
        // a folder converted with care, read in another run (its lower zooms, its last holes)
        let d = scratch("format-d");
        std::fs::write(
            d.join("tiles.json"),
            r#"{"format": "btms tiles v1", "extension": "webp", "quality": 90, "effort": "careful"}"#,
        )
        .unwrap();
        assert_eq!((of(&d), effort(&d)), (Format::Webp(Quality::Lossy(90)), Effort::Careful));
        let _ = std::fs::remove_dir_all(&d);
        for d in [a, b, c] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// Every file under `dir`: its path from there.
    fn files(dir: &Path) -> Vec<String> {
        let mut all = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    all.push(p.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/"));
                }
            }
        }
        all.sort();
        all
    }

    #[test]
    fn a_webp_folder_is_written_and_read_back_by_the_blocks_and_the_lower_zooms() {
        use super::super::{BLOCK_PX, TILE, Written, halve, lower_zooms, read_tile, tile_path, write};
        let mut seed = 0x00C0_FFEE_u64;
        for (quality, effort) in [
            (Quality::Lossless, Effort::Fast),
            (Quality::Lossless, Effort::Careful),
            (Quality::Lossy(DEFAULT_QUALITY), Effort::Fast),
            (Quality::Lossy(DEFAULT_QUALITY), Effort::Careful),
        ] {
            let out = scratch(&format!("format-folder-{quality:?}-{effort:?}"));
            set(&out, Format::Webp(quality), effort);
            // an older tile where a block is about to be written: it goes
            std::fs::create_dir_all(out.join("9").join("0")).unwrap();
            std::fs::write(tile_path(&out, 9, 0, 3, "jpg"), b"old").unwrap();
            // a block opaque, one with its western third empty
            let full = webp::tests::ground(BLOCK_PX, BLOCK_PX, false, &mut seed);
            let coast = webp::tests::ground(BLOCK_PX, BLOCK_PX, true, &mut seed);
            let a = write::tiles(&out, 0, 0, &write::Pixels::Whole(full.clone())).unwrap();
            let b = write::tiles(&out, 1, 0, &write::Pixels::Whole(coast.clone())).unwrap();
            assert!(a.iter().all(|w| *w == Written::Jpeg), "an opaque block gives opaque tiles");
            assert!(b.contains(&Written::Png) && b.contains(&Written::Jpeg));
            lower_zooms(&out, vec![(0, 0), (1, 0)], 2);
            let names = files(&out);
            assert!(names.iter().all(|n| n.ends_with(".webp")), "{names:?}");
            // the tiles of the blocks that are not empty (the coast's western column is), and zooms 6 to 0 above them
            let written = a.iter().chain(&b).filter(|w| **w != Written::Empty).count();
            assert_eq!(b.iter().filter(|w| **w == Written::Empty).count(), 4);
            assert_eq!(names.len(), written + 7, "{names:?}");
            // read back as written; partly empty tiles keep their transparency, opaque ones have no alpha
            let back = read_tile(&out, 9, 0, 3).unwrap();
            let first = super::super::crop(&full, BLOCK_PX, 0, 0, TILE);
            if quality == Quality::Lossless {
                assert!(back == first, "{effort:?}: the lossless tile read back differs");
            } else {
                assert!(webp::tests::psnr(&first, &back) > 30.0);
            }
            // the file is the one of the folder's effort
            let file = std::fs::read(tile_path(&out, 9, 0, 3, "webp")).unwrap();
            assert!(
                file == webp::encode(&first, TILE, TILE, true, &Settings::tiles(quality, effort)).unwrap(),
                "{quality:?} {effort:?}"
            );
            assert!(opaque_on_disk(&out, 9, 0, 3));
            // the zoom 7 tile of the coast: alpha exactly the halved block's
            let quarter = halve(&halve(&coast, BLOCK_PX, BLOCK_PX), BLOCK_PX / 2, BLOCK_PX / 2);
            let z7 = read_tile(&out, 7, 1, 0).unwrap();
            assert!(!opaque_on_disk(&out, 7, 1, 0));
            assert!(z7.as_chunks::<4>().0.iter().map(|p| p[3]).eq(quarter.as_chunks::<4>().0.iter().map(|p| p[3])));
            // the zoom 0 tile holds both blocks
            assert!(read_tile(&out, 0, 0, 0).is_some());
            let _ = std::fs::remove_dir_all(&out);
        }
    }

    /// What a zone drawn in WebP into an older folder would do: the rest of the map hidden from
    /// the readers, and the zooms above the zone without it. Refused, rather.
    #[test]
    fn a_zone_in_another_format_is_refused_rather_than_mixed() {
        use super::super::{BLOCK_PX, TILE, lower_zooms, read_tile, write};
        let mut seed = 0x0DD_BA11u64;
        let (west, east) = (
            webp::tests::ground(BLOCK_PX, BLOCK_PX, false, &mut seed),
            webp::tests::ground(BLOCK_PX, BLOCK_PX, false, &mut seed),
        );
        let before = scratch("format-mixed");
        set(&before, Format::Jpeg, Effort::Fast);
        write::tiles(&before, 0, 0, &write::Pixels::Whole(west.clone())).unwrap();
        write::tiles(&before, 1, 0, &write::Pixels::Whole(east.clone())).unwrap();
        lower_zooms(&before, vec![(0, 0), (1, 0)], 2);
        // the zone's format: refused, saying what to do; the folder's own is not
        let refused = format!("{:#}", refuse_mixed_zone(&before, Format::default()).unwrap_err());
        assert!(refused.contains("already holds .jpg tiles: a zone in .webp would mix two formats"), "{refused}");
        assert!(refused.contains("--format jpeg") && refused.contains("convert <folder> --to"), "{refused}");
        assert!(refuse_mixed_zone(&before, Format::Jpeg).is_ok());
        // a new folder, an empty one, one of the same format: nothing to refuse
        let new = std::env::temp_dir().join(format!("btms-tests-{}-format-new", std::process::id()));
        assert!(refuse_mixed_zone(&new, Format::default()).is_ok());
        let webp_folder = scratch("format-mixed-webp");
        set(&webp_folder, Format::default(), Effort::Fast);
        write::tiles(&webp_folder, 0, 0, &write::Pixels::Whole(west.clone())).unwrap();
        assert!(refuse_mixed_zone(&webp_folder, Format::Webp(Quality::Lossless)).is_ok());
        assert!(
            format!("{:#}", refuse_mixed_zone(&webp_folder, Format::Jpeg).unwrap_err())
                .contains("already holds .webp tiles: a zone in .jpg")
        );
        // what the refusal spares: the eastern block drawn again in WebP over the older folder
        set(&before, Format::default(), Effort::Fast);
        write::tiles(&before, 1, 0, &write::Pixels::Whole(east)).unwrap();
        lower_zooms(&before, vec![(1, 0)], 2);
        // the western block is still on disk, but a reader following the folder's format no longer sees it
        assert!(super::super::tile_path(&before, 7, 0, 0, "jpg").is_file());
        assert!(read_tile(&before, 7, 0, 0).is_none());
        // and the zoom 6 tile above both, made again from the WebP alone, lost it: its western half is empty
        let z6 = read_tile(&before, 6, 0, 0).unwrap();
        let west_half_empty = (TILE / 2..TILE).all(|y| (0..TILE / 2).all(|x| z6[(y * TILE + x) * 4 + 3] == 0));
        assert!(west_half_empty);
        assert!(!super::super::tile_path(&before, 6, 0, 0, "jpg").is_file());
        for d in [before, webp_folder] {
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn the_bounds_are_those_of_the_zoom_7_tiles() {
        let dir = scratch("format-bounds");
        assert_eq!(bounds(&dir), None);
        for (x, y, ext) in [(3, -8, "webp"), (-2, 5, "jpg"), (4, -9, "png")] {
            std::fs::create_dir_all(dir.join("7").join(x.to_string())).unwrap();
            std::fs::write(dir.join("7").join(x.to_string()).join(format!("{y}.{ext}")), b"t").unwrap();
        }
        // what is not a tile does not count
        std::fs::write(dir.join("7").join("3").join("40.webp.tmp"), b"t").unwrap();
        std::fs::write(dir.join("7").join("3").join("notes.txt"), b"t").unwrap();
        assert_eq!(bounds(&dir), Some([-256.0, -1152.0, 640.0, 768.0]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_page_is_written_whole_beside_the_manifest() {
        let dir = scratch("format-page");
        write_page(&dir).unwrap();
        assert_eq!(std::fs::read(dir.join(PAGE_NAME)).unwrap(), PAGE);
        assert!(!dir.join("index.tmp").exists());
        assert!(String::from_utf8_lossy(PAGE).contains("<html"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
