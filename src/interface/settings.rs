//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The settings of the interface: `interface.json` in the program's local data folder
//! (`%LOCALAPPDATA%\BTMS`). Only what the user changed is kept there: an absent key
//! is the default (the game folder FiveM uses, `map.toml` and `output/` of the current folder).
//!
//! `map.toml` is never written: its comments would be lost. What the page changes of it is kept
//! here as overrides (`overrides`), and a job is given `--config <derived file>`, the base
//! configuration with the overrides applied, written beside the settings ([`write_derived`]).
//!
//! The performance of a render (`threads`, `renderer`, `shading`, `ram_gb`, `vram_gb`,
//! `priority`) is kept here too, and becomes options of its command line
//! ([`Resolved::render_args`]): absent, the engine's own value. So is what its tiles are written
//! as (`format`: `webp` or `jpeg`; `quality`: 0 to 100 or `lossless`, for WebP only), given as
//! `--format jpeg` and `--quality` when they are not the engine's default (WebP at its quality).
//!
//! The language of the page (`language`: one of [`LANGUAGES`]) is kept here as well, not by the
//! page: its address (the port) may change from one run to the next, and its storage with it.
//! Absent, the page takes the first of the browser's languages it has, else English.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::config::Config;

/// Name of the settings file, in the data folder.
pub const FILE: &str = "interface.json";
/// Where the derived configuration goes, under the data folder: a folder, and the file in it.
pub const DERIVED: [&str; 2] = ["interface", "effective-map.toml"];
/// The most threads a render may be given (`--threads`).
pub const MAX_THREADS: usize = 256;
/// The memory budgets a render may be given, in GB (`--ram`, `--vram`).
pub const MEMORY_GB: (f64, f64) = (0.25, 4096.0);
/// The languages of the page (`language`): English, French, German, Dutch, Russian, Swedish and
/// Spanish.
pub const LANGUAGES: [&str; 7] = ["en", "fr", "de", "nl", "ru", "sv", "es"];

/// How many worker threads a render has: the engine's choice (`auto`), or a number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Threads {
    #[default]
    Auto,
    Count(usize),
}

impl Serialize for Threads {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Threads::Auto => s.serialize_str("auto"),
            Threads::Count(n) => s.serialize_u64(*n as u64),
        }
    }
}

impl<'de> Deserialize<'de> for Threads {
    /// A number, or `"auto"`.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Threads, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Count(u64),
            Word(String),
        }
        match Raw::deserialize(d)? {
            Raw::Count(n) => Ok(Threads::Count(n as usize)),
            Raw::Word(w) if w == "auto" => Ok(Threads::Auto),
            Raw::Word(w) => Err(serde::de::Error::custom(format!("threads: \"{w}\" (auto or a number expected)"))),
        }
    }
}

/// Who draws (`renderer`) or shades (`shading`): the engine's choice, the graphics card, the processor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Unit {
    #[default]
    Auto,
    Gpu,
    Cpu,
}

impl Unit {
    pub fn word(self) -> &'static str {
        match self {
            Unit::Auto => "auto",
            Unit::Gpu => "gpu",
            Unit::Cpu => "cpu",
        }
    }
}

/// The priority of the process of a render: `low` leaves the computer usable meanwhile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    #[default]
    Normal,
    Low,
}

/// What the tiles are written as (`--format`): WebP, the engine's default, or the older JPEG and PNG.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TileFormat {
    #[default]
    Webp,
    Jpeg,
}

/// The quality of the WebP tiles (`--quality`): a number from 0 to 100, or `"lossless"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileQuality {
    Lossy(u8),
    Lossless,
}

impl Default for TileQuality {
    fn default() -> TileQuality {
        TileQuality::Lossy(crate::tiles::format::DEFAULT_QUALITY)
    }
}

impl TileQuality {
    /// What `--quality` is given.
    pub fn word(self) -> String {
        match self {
            TileQuality::Lossy(q) => q.to_string(),
            TileQuality::Lossless => "lossless".to_string(),
        }
    }

    /// From what the page or the file says: a whole number from 0 to 100, or `"lossless"`.
    fn read(value: &Value) -> Option<TileQuality> {
        match value {
            Value::String(w) if w == "lossless" => Some(TileQuality::Lossless),
            v => v.as_u64().filter(|q| *q <= 100).map(|q| TileQuality::Lossy(q as u8)),
        }
    }
}

impl Serialize for TileQuality {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            TileQuality::Lossy(q) => s.serialize_u8(*q),
            TileQuality::Lossless => s.serialize_str("lossless"),
        }
    }
}

impl<'de> Deserialize<'de> for TileQuality {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<TileQuality, D::Error> {
        let value = Value::deserialize(d)?;
        TileQuality::read(&value)
            .ok_or_else(|| serde::de::Error::custom(format!("quality: {value} (0 to 100 or lossless expected)")))
    }
}

/// The settings as they are kept: None is the default.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stored {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub game: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threads: Option<Threads>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renderer: Option<Unit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shading: Option<Unit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram_gb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vram_gb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<TileFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<TileQuality>,
    /// Overrides of `map.toml`: section → field → value.
    #[serde(skip_serializing_if = "Map::is_empty")]
    pub overrides: Map<String, Value>,
    /// The language of the page, one of [`LANGUAGES`]; None: the browser's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// The settings as the page gets them and as a job uses them: every path absolute.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Resolved {
    /// None: the game folder FiveM uses.
    pub game: Option<String>,
    pub config: String,
    pub output: String,
    pub preview: String,
    pub threads: Threads,
    pub renderer: Unit,
    pub shading: Unit,
    /// None: the engine's budget (6 GB, 4 GB on the card).
    pub ram_gb: Option<f64>,
    pub vram_gb: Option<f64>,
    pub priority: Priority,
    pub format: TileFormat,
    /// The quality of the WebP tiles (that of the engine when none was set).
    pub quality: TileQuality,
    pub overrides: Map<String, Value>,
    /// One of [`LANGUAGES`]; None (`null` for the page): the browser's language, as the page
    /// chooses it.
    pub language: Option<String>,
}

impl Resolved {
    /// The arguments of the process of a render, after the executable: a zone into the preview
    /// folder, or the whole map into its folder, with the performance settings and the derived
    /// configuration (`derived`). What the server launches, and nothing else: no check, no file.
    ///
    /// `tiles <folder> [--zone x1,y1,x2,y2] --threads auto|<n> --renderer <r> --shading <s>
    /// [--ram <GB>] [--vram <GB>] [--format jpeg | --quality <q>|lossless]
    /// --config <file> [--game <folder>]`
    pub fn render_args(&self, zone: Option<[f64; 4]>, derived: &Path) -> Vec<String> {
        let mut args: Vec<String> = vec!["tiles".into()];
        match zone {
            Some(z) => args.extend([self.preview.clone(), "--zone".into(), zone_arg(z)]),
            None => args.push(self.output.clone()),
        }
        self.tile_args(&mut args);
        self.job_args(&mut args, derived);
        args
    }

    /// The arguments of a job of `update`, after the executable: always on the folder of the
    /// whole map (`output`), never on the preview's. Drawing (a zone, or the ground of what
    /// changed) takes the performance and format settings, as a render of the whole map; listing
    /// and recording the resources only the configuration and the game.
    ///
    /// `update <output> (--zone x1,y1,x2,y2 | --changes | --list | --record)
    /// [options of a render] --config <file> [--game <folder>]`
    pub fn update_args(&self, what: Update, derived: &Path) -> Vec<String> {
        let mut args: Vec<String> = vec!["update".into(), self.output.clone()];
        match what {
            Update::Zone(z) => args.extend(["--zone".into(), zone_arg(z)]),
            Update::Changes => args.push("--changes".into()),
            Update::List => args.push("--list".into()),
            Update::Record => args.push("--record".into()),
        }
        if matches!(what, Update::Zone(_) | Update::Changes) {
            self.tile_args(&mut args);
        }
        self.job_args(&mut args, derived);
        args
    }

    /// The performance and format options of a job that draws tiles.
    fn tile_args(&self, args: &mut Vec<String>) {
        let threads = match self.threads {
            Threads::Auto => "auto".to_string(),
            Threads::Count(n) => n.to_string(),
        };
        args.extend([
            "--threads".into(),
            threads,
            "--renderer".into(),
            self.renderer.word().into(),
            "--shading".into(),
            self.shading.word().into(),
        ]);
        // (a budget as the shortest decimal that reads back: 12, 12.5)
        if let Some(gb) = self.ram_gb {
            args.extend(["--ram".into(), gb.to_string()]);
        }
        if let Some(gb) = self.vram_gb {
            args.extend(["--vram".into(), gb.to_string()]);
        }
        // the tiles: WebP at the engine's quality unless told otherwise (a quality is WebP's only)
        match self.format {
            TileFormat::Jpeg => args.extend(["--format".into(), "jpeg".into()]),
            TileFormat::Webp if self.quality != TileQuality::default() => {
                args.extend(["--quality".into(), self.quality.word()])
            }
            TileFormat::Webp => {}
        }
    }

    /// What every job of the interface is given: the derived configuration, the game chosen.
    fn job_args(&self, args: &mut Vec<String>, derived: &Path) {
        args.extend(["--config".into(), derived.display().to_string()]);
        if let Some(game) = &self.game {
            args.extend(["--game".into(), game.clone()]);
        }
    }
}

/// What a job of `update` is asked: draw a zone again, or the ground of what changed in the
/// resources; list those changes; record the resources as they are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Update {
    Zone([f64; 4]),
    Changes,
    List,
    Record,
}

/// A zone as the command line takes it: `x1,y1,x2,y2`, each number as the shortest decimal that
/// reads back.
fn zone_arg(z: [f64; 4]) -> String {
    format!("{},{},{},{}", z[0], z[1], z[2], z[3])
}

/// `given` as an absolute path, a relative one taken from `cwd`.
fn absolute(cwd: &Path, given: &str) -> String {
    let joined = if Path::new(given).is_absolute() { PathBuf::from(given) } else { cwd.join(given) };
    std::path::absolute(&joined).unwrap_or(joined).display().to_string()
}

impl Stored {
    pub fn resolve(&self, cwd: &Path) -> Resolved {
        let path = |given: &Option<String>, default: &str| absolute(cwd, given.as_deref().unwrap_or(default));
        // (a file written by hand may hold anything: what is out of bounds is taken back in them)
        let budget = |gb: Option<f64>| gb.filter(|x| x.is_finite()).map(|x| x.clamp(MEMORY_GB.0, MEMORY_GB.1));
        Resolved {
            game: self.game.as_deref().map(|p| absolute(cwd, p)),
            config: path(&self.config, crate::config::FILE),
            output: path(&self.output, "output/tiles"),
            preview: path(&self.preview, "output/preview"),
            threads: match self.threads {
                None | Some(Threads::Auto) => Threads::Auto,
                Some(Threads::Count(n)) => Threads::Count(n.clamp(1, MAX_THREADS)),
            },
            renderer: self.renderer.unwrap_or_default(),
            shading: self.shading.unwrap_or_default(),
            ram_gb: budget(self.ram_gb),
            vram_gb: budget(self.vram_gb),
            priority: self.priority.unwrap_or_default(),
            format: self.format.unwrap_or_default(),
            quality: self.quality.unwrap_or_default(),
            overrides: self.overrides.clone(),
            // (and a language the page does not have is the browser's)
            language: self.language.clone().filter(|code| LANGUAGES.contains(&code.as_str())),
        }
    }

    /// These settings with a partial set merged in (the body of `POST /api/settings`): a key that
    /// is absent stays, `null` goes back to the default, an override set to `null` is removed.
    /// Refused as a whole, with the reason, when what results is not a configuration.
    pub fn merge(&self, patch: &Value) -> Result<Stored, String> {
        let Value::Object(patch) = patch else { return Err("the settings must be a JSON object".into()) };
        let mut next = self.clone();
        for (key, value) in patch {
            match key.as_str() {
                "game" => next.game = text(key, value)?,
                "config" => next.config = text(key, value)?,
                "output" => next.output = text(key, value)?,
                "preview" => next.preview = text(key, value)?,
                // the default of each is the engine's own value: it is not kept
                "threads" => {
                    next.threads = match value {
                        Value::Null => None,
                        Value::String(w) if w == "auto" => None,
                        v => Some(Threads::Count(
                            v.as_u64().map(|n| n as usize).filter(|n| (1..=MAX_THREADS).contains(n)).ok_or(format!(
                                "threads: \"auto\" or a whole number from 1 to {MAX_THREADS} expected"
                            ))?,
                        )),
                    }
                }
                "renderer" | "shading" => {
                    let unit = match value {
                        Value::Null => None,
                        Value::String(w) => match w.as_str() {
                            "auto" => None,
                            "gpu" => Some(Unit::Gpu),
                            "cpu" => Some(Unit::Cpu),
                            _ => return Err(format!("{key}: auto, gpu or cpu expected")),
                        },
                        _ => return Err(format!("{key}: auto, gpu or cpu expected")),
                    };
                    if key == "renderer" { next.renderer = unit } else { next.shading = unit }
                }
                "ram_gb" | "vram_gb" => {
                    let gb = match value {
                        Value::Null => None,
                        v => Some(
                            v.as_f64().filter(|x| x.is_finite() && (MEMORY_GB.0..=MEMORY_GB.1).contains(x)).ok_or(
                                format!(
                                    "{key}: a number of GB from {} to {} expected, or null",
                                    MEMORY_GB.0, MEMORY_GB.1
                                ),
                            )?,
                        ),
                    };
                    if key == "ram_gb" { next.ram_gb = gb } else { next.vram_gb = gb }
                }
                // the engine's default (WebP, its quality) is not kept either
                "format" => {
                    next.format = match value {
                        Value::Null => None,
                        Value::String(w) if w == "webp" => None,
                        Value::String(w) if w == "jpeg" => Some(TileFormat::Jpeg),
                        _ => return Err("format: webp or jpeg expected".into()),
                    }
                }
                "quality" => {
                    next.quality = match value {
                        Value::Null => None,
                        v => match TileQuality::read(v) {
                            Some(q) if q == TileQuality::default() => None,
                            Some(q) => Some(q),
                            None => {
                                return Err(
                                    "quality: a whole number from 0 to 100 or \"lossless\" expected, or null".into()
                                );
                            }
                        },
                    }
                }
                "priority" => {
                    next.priority = match value {
                        Value::Null => None,
                        Value::String(w) if w == "normal" => None,
                        Value::String(w) if w == "low" => Some(Priority::Low),
                        _ => return Err("priority: normal or low expected".into()),
                    }
                }
                "overrides" => match value {
                    Value::Null => next.overrides.clear(),
                    Value::Object(sections) => merge_into(&mut next.overrides, sections),
                    _ => return Err("overrides: an object expected (maps, resources, lighting, water)".into()),
                },
                // the language of the page: one of LANGUAGES, or null for the browser's
                "language" => {
                    next.language = match value {
                        Value::Null => None,
                        Value::String(code) if LANGUAGES.contains(&code.as_str()) => Some(code.clone()),
                        _ => return Err(format!("language: one of {} expected, or null", LANGUAGES.join(", "))),
                    }
                }
                _ => return Err(format!("unknown setting: {key}")),
            }
        }
        effective(&Config::default(), &next.overrides)?;
        Ok(next)
    }
}

/// A path setting: text, `null` or empty for the default.
fn text(key: &str, value: &Value) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(s) if s.contains('\0') => Err(format!("{key}: path refused")),
        Value::String(s) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
        _ => Err(format!("{key}: a path (text) or null expected")),
    }
}

/// Merges `patch` into `target`, objects into objects; `null` removes the key. Sections left
/// empty are dropped.
fn merge_into(target: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (key, value) in patch {
        match value {
            Value::Null => {
                target.remove(key);
            }
            Value::Object(inner) => {
                let slot = target.entry(key.clone()).or_insert_with(|| Value::Object(Map::new()));
                if !slot.is_object() {
                    *slot = Value::Object(Map::new());
                }
                if let Some(slot) = slot.as_object_mut() {
                    merge_into(slot, inner);
                }
            }
            other => {
                target.insert(key.clone(), other.clone());
            }
        }
    }
    target.retain(|_, v| !v.as_object().is_some_and(Map::is_empty));
}

/// What a render launched from the interface uses: `base` (`map.toml` as it is read) with the
/// overrides applied. An override must name a field `map.toml` has, with a value of its kind.
pub fn effective(base: &Config, overrides: &Map<String, Value>) -> Result<Config, String> {
    let mut value = serde_json::to_value(base).map_err(|e| e.to_string())?;
    for (section, fields) in overrides {
        let unknown = || format!("overrides: unknown section \"{section}\" (maps, resources, lighting, water)");
        let target = value.get_mut(section).and_then(Value::as_object_mut).ok_or_else(unknown)?;
        let Value::Object(fields) = fields else { return Err(format!("overrides.{section}: an object expected")) };
        for (key, new) in fields {
            let name = format!("overrides.{section}.{key}");
            let Some(old) = target.get(key) else { return Err(format!("{name}: unknown field")) };
            match (old, new) {
                (_, Value::Null) => continue,
                (Value::Bool(_), Value::Bool(_)) => {}
                (Value::Number(_), Value::Number(n)) => {
                    if !n.as_f64().is_some_and(|x| (x as f32).is_finite()) {
                        return Err(format!("{name}: number refused"));
                    }
                }
                (Value::Array(_), Value::Array(items)) => {
                    if !items.iter().all(Value::is_string) {
                        return Err(format!("{name}: a list of strings expected"));
                    }
                }
                (Value::Bool(_), _) => return Err(format!("{name}: true or false expected")),
                (Value::Number(_), _) => return Err(format!("{name}: a number expected")),
                _ => return Err(format!("{name}: a list of strings expected")),
            }
            target.insert(key.clone(), new.clone());
        }
    }
    serde_json::from_value(value).map_err(|e| format!("overrides: {e}"))
}

/// The settings kept in `dir`; the defaults when there is no file, or when it cannot be read
/// (said on the error output: the next change writes a sound file over it).
pub fn load(dir: &Path) -> Stored {
    let file = dir.join(FILE);
    let Ok(text) = std::fs::read_to_string(&file) else { return Stored::default() };
    match serde_json::from_str::<Stored>(&text) {
        Ok(stored) if effective(&Config::default(), &stored.overrides).is_ok() => stored,
        Ok(stored) => {
            eprintln!("interface: overrides of {} cannot be read, ignored", file.display());
            Stored { overrides: Map::new(), ..stored }
        }
        Err(e) => {
            eprintln!("interface: {} cannot be read ({e}), default settings", file.display());
            Stored::default()
        }
    }
}

/// Writes `text` to `file` whole: beside it first, then renamed over.
fn write_whole(file: &Path, text: &str) -> Result<(), String> {
    let dir = file.parent().ok_or("path without a folder")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    let temp = file.with_extension("tmp");
    std::fs::write(&temp, text).map_err(|e| format!("writing {}: {e}", temp.display()))?;
    std::fs::rename(&temp, file).map_err(|e| format!("writing {}: {e}", file.display()))
}

/// Keeps the settings in `dir`.
pub fn save(dir: &Path, stored: &Stored) -> Result<(), String> {
    let text = serde_json::to_string_pretty(stored).map_err(|e| e.to_string())?;
    write_whole(&dir.join(FILE), &text)
}

/// Writes the configuration a job is given (`--config`), under `dir`. Returns the file.
pub fn write_derived(dir: &Path, config: &Config) -> Result<PathBuf, String> {
    let file = dir.join(DERIVED[0]).join(DERIVED[1]);
    let body = toml::to_string(config).map_err(|e| format!("derived configuration: {e}"))?;
    let text = format!(
        "# BTMS · derived configuration, written by the interface for every job.\n# Do not edit it: it is made again from map.toml and the interface settings.\n\n{body}"
    );
    write_whole(&file, &text)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A folder of its own under the system's temporary folder, emptied.
    pub fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("btms-tests-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Takes out of a command line the interface wrote what the engine's command line takes out
    /// before `tiles` or `update` reads the rest, with the engine's own code (`cli`, then the
    /// light's options, `--renderer` and `--shading`), and checks that each value is the setting's
    /// (for a job that `draws`; one that does not is given neither, and gets the engine's own
    /// choice). What is left starts with the command.
    fn take_global_options(resolved: &Resolved, derived: &Path, draws: bool, args: &mut Vec<String>) {
        let line = args.clone();
        let (game, config) = crate::cli::take_game_and_config(args).unwrap_or_else(|e| panic!("{e:#} in {line:?}"));
        assert_eq!(game, resolved.game.as_ref().map(PathBuf::from), "--game in {line:?}");
        assert_eq!(config.as_deref(), Some(derived), "--config in {line:?}");
        // the light comes from the derived file, never from the line
        let mut light = Config::default().lighting;
        crate::render::shade::overrides(args, &mut light).unwrap_or_else(|e| panic!("{e:#} in {line:?}"));
        assert_eq!(light, Config::default().lighting, "{line:?}");
        // (the variables BTMS_RENDERER and BTMS_SHADING left out: the line alone)
        let unit = |unit: Unit| if draws { unit } else { Unit::Auto };
        let renderer = crate::render::gpu::chosen(args, None).unwrap_or_else(|e| panic!("{e:#} in {line:?}"));
        let expected = match unit(resolved.renderer) {
            Unit::Auto => crate::render::gpu::Choice::Auto,
            Unit::Gpu => crate::render::gpu::Choice::Gpu,
            Unit::Cpu => crate::render::gpu::Choice::Cpu,
        };
        assert_eq!(renderer, expected, "--renderer in {line:?}");
        let shading = crate::render::gpu::shade::chosen(args, None).unwrap_or_else(|e| panic!("{e:#} in {line:?}"));
        let expected = match unit(resolved.shading) {
            Unit::Auto => crate::render::gpu::shade::Choice::Auto,
            Unit::Gpu => crate::render::gpu::shade::Choice::Gpu,
            Unit::Cpu => crate::render::gpu::shade::Choice::Cpu,
        };
        assert_eq!(shading, expected, "--shading in {line:?}");
        for taken in ["--game", "--config", "--renderer", "--shading"] {
            assert!(!args.iter().any(|a| a == taken), "{taken} twice in {line:?}");
        }
    }

    /// What `tiles` (and `update`, which gives its options to `tiles`) reads of the settings of
    /// a render: the threads, the budgets, the format.
    fn assert_tile_options(resolved: &Resolved, opts: &crate::tiles::Options, line: &[String]) {
        let threads = match resolved.threads {
            Threads::Auto => crate::tiles::options::Threads::Auto,
            Threads::Count(n) => crate::tiles::options::Threads::Fixed(n),
        };
        assert_eq!(opts.threads, threads, "--threads in {line:?}");
        let bytes = |gb: f64| (gb * 1e9).round() as usize;
        assert_eq!(
            opts.ram_budget,
            bytes(resolved.ram_gb.unwrap_or(crate::tiles::options::RAM_GB)),
            "--ram in {line:?}"
        );
        assert_eq!(opts.card_budget, resolved.vram_gb.map(bytes), "--vram in {line:?}");
        let expected = match resolved {
            Resolved { format: TileFormat::Jpeg, .. } => crate::tiles::format::Format::Jpeg,
            Resolved { quality: TileQuality::Lossless, .. } => {
                crate::tiles::format::Format::Webp(crate::tiles::webp::Quality::Lossless)
            }
            Resolved { quality: TileQuality::Lossy(q), .. } => {
                crate::tiles::format::Format::Webp(crate::tiles::webp::Quality::Lossy(*q))
            }
        };
        assert_eq!(opts.format, expected, "--format and --quality in {line:?}");
        assert!(!opts.dry_run, "{line:?}");
    }

    #[test]
    fn defaults_come_from_the_current_folder() {
        let cwd = std::env::temp_dir().join("btms-cwd");
        let r = Stored::default().resolve(&cwd);
        assert_eq!(r.game, None);
        assert_eq!(Path::new(&r.config), cwd.join("map.toml"));
        assert_eq!(Path::new(&r.output), cwd.join("output").join("tiles"));
        assert_eq!(Path::new(&r.preview), cwd.join("output").join("preview"));
        // the performance: what the engine chooses by itself
        assert_eq!(
            (r.threads, r.renderer, r.shading, r.priority),
            (Threads::Auto, Unit::Auto, Unit::Auto, Priority::Normal)
        );
        assert_eq!((r.ram_gb, r.vram_gb), (None, None));
        // the tiles: WebP at the engine's quality
        assert_eq!(
            (r.format, r.quality),
            (TileFormat::Webp, TileQuality::Lossy(crate::tiles::format::DEFAULT_QUALITY))
        );
        assert!(r.overrides.is_empty());
        // the language: the browser's
        assert_eq!(r.language, None);
        // as the page gets them
        let page = serde_json::to_value(&r).unwrap();
        assert_eq!(
            [
                &page["threads"],
                &page["renderer"],
                &page["shading"],
                &page["ram_gb"],
                &page["vram_gb"],
                &page["priority"],
                &page["language"]
            ],
            [
                &json!("auto"),
                &json!("auto"),
                &json!("auto"),
                &json!(null),
                &json!(null),
                &json!("normal"),
                &json!(null)
            ]
        );
    }

    #[test]
    fn a_partial_set_is_merged() {
        let cwd = std::env::temp_dir().join("btms-cwd");
        let elsewhere = std::env::temp_dir().join("btms-elsewhere");
        let a = Stored::default().merge(&json!({"threads": 2, "preview": elsewhere, "game": "D:/GTA V"})).unwrap();
        assert_eq!((a.threads, a.game.as_deref()), (Some(Threads::Count(2)), Some("D:/GTA V")));
        // an absent key stays, null and the empty text go back to the default
        let b = a.merge(&json!({"output": "tiles-test", "game": null, "preview": "  "})).unwrap();
        assert_eq!((b.threads, b.game.clone(), b.preview.clone()), (Some(Threads::Count(2)), None, None));
        let r = b.resolve(&cwd);
        assert_eq!(Path::new(&r.output), cwd.join("tiles-test"));
        assert_eq!(Path::new(&r.preview), cwd.join("output").join("preview"));
        assert_eq!(r.threads, Threads::Count(2));
        assert_eq!(b.merge(&json!({"threads": null})).unwrap().threads, None);
        assert_eq!(b.merge(&json!({"threads": "auto"})).unwrap().threads, None);
        assert_eq!(b.merge(&json!({"threads": 256})).unwrap().threads, Some(Threads::Count(256)));
        // refused as a whole
        for bad in [
            json!({"threads": 0}),
            json!({"threads": 257}),
            json!({"threads": "two"}),
            json!({"threads": 2.5}),
            json!({"unknown": 1}),
            json!({"output": 3}),
            json!([1]),
            json!({"overrides": 4}),
            json!({"renderer": "card"}),
            json!({"shading": true}),
            json!({"ram_gb": 0}),
            json!({"vram_gb": "8"}),
            json!({"ram_gb": 1e6}),
            json!({"priority": "high"}),
            json!({"threads": 4, "renderer": "turbo"}),
        ] {
            assert!(b.merge(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_performance_is_merged_and_kept_only_when_it_is_not_the_engine_s() {
        let a = Stored::default()
            .merge(&json!({"threads": 14, "renderer": "gpu", "shading": "cpu", "ram_gb": 12.5, "vram_gb": 18, "priority": "low"}))
            .unwrap();
        assert_eq!(
            (a.threads, a.renderer, a.shading, a.priority),
            (Some(Threads::Count(14)), Some(Unit::Gpu), Some(Unit::Cpu), Some(Priority::Low))
        );
        assert_eq!((a.ram_gb, a.vram_gb), (Some(12.5), Some(18.0)));
        // "auto", "normal" and null are the engine's value: nothing is kept
        let b = a.merge(&json!({"threads": "auto", "renderer": "auto", "shading": null, "ram_gb": null, "vram_gb": null, "priority": "normal"})).unwrap();
        assert_eq!(b, Stored::default());
        assert_eq!(serde_json::to_string(&b).unwrap(), "{}");
    }

    #[test]
    fn the_format_of_the_tiles_is_kept_and_given_to_a_render() {
        let cwd = std::env::temp_dir().join("btms-cwd");
        let derived = PathBuf::from("C:\\d\\effective-map.toml");
        let default = crate::tiles::format::DEFAULT_QUALITY;
        // the engine's default is not kept, and adds nothing to the command line
        let a = Stored::default().merge(&json!({"format": "webp", "quality": default})).unwrap();
        assert_eq!(a, Stored::default());
        assert!(!a.resolve(&cwd).render_args(None, &derived).iter().any(|x| x == "--format" || x == "--quality"));
        // a quality, lossless, the older format
        let b = a.merge(&json!({"quality": 75})).unwrap();
        assert_eq!(b.quality, Some(TileQuality::Lossy(75)));
        let args = b.resolve(&cwd).render_args(None, &derived);
        assert_eq!(&args[args.len() - 4..args.len() - 2], ["--quality", "75"]);
        let c = b.merge(&json!({"quality": "lossless"})).unwrap();
        let args = c.resolve(&cwd).render_args(Some([0.0, 0.0, 128.0, 128.0]), &derived);
        assert_eq!(&args[args.len() - 4..args.len() - 2], ["--quality", "lossless"]);
        let d = c.merge(&json!({"format": "jpeg"})).unwrap();
        let args = d.resolve(&cwd).render_args(None, &derived);
        assert_eq!(&args[args.len() - 4..args.len() - 2], ["--format", "jpeg"]);
        assert!(!args.iter().any(|x| x == "--quality"), "a quality is WebP's only: {args:?}");
        // kept, read back, and as the page gets them
        assert_eq!(serde_json::to_value(&d).unwrap(), json!({"format": "jpeg", "quality": "lossless"}));
        assert_eq!(serde_json::from_value::<Stored>(json!({"format": "jpeg", "quality": "lossless"})).unwrap(), d);
        let page = serde_json::to_value(b.resolve(&cwd)).unwrap();
        assert_eq!((&page["format"], &page["quality"]), (&json!("webp"), &json!(75)));
        assert_eq!(d.merge(&json!({"format": null, "quality": null})).unwrap(), Stored::default());
        // what the engine would refuse is refused here
        for bad in [
            json!({"format": "png"}),
            json!({"format": 1}),
            json!({"quality": 101}),
            json!({"quality": -1}),
            json!({"quality": 90.5}),
            json!({"quality": "high"}),
            json!({"quality": "loss-less"}),
        ] {
            assert!(Stored::default().merge(&bad).is_err(), "{bad}");
        }
        // a hand-written file with a quality out of bounds: refused as a whole, the defaults
        let dir = scratch("tile-format");
        std::fs::write(dir.join(FILE), r#"{"quality": 300}"#).unwrap();
        assert_eq!(load(&dir), Stored::default());
        // the command line the interface writes is one the engine reads, every option of it
        // (--threads, --renderer, --shading, --ram, --vram, --format, --quality, --zone, --config,
        // --game): the global options taken out first by the engine's own code, as its command line
        // does, then `tiles` reads the rest and refuses any option it does not know
        let every = Stored::default()
            .merge(&json!({"threads": 14, "renderer": "gpu", "shading": "cpu", "ram_gb": 12.5, "vram_gb": 18, "quality": 75, "game": "E:\\GTA V"}))
            .unwrap();
        let other_end = Stored::default()
            .merge(&json!({"threads": 1, "renderer": "cpu", "shading": "gpu", "ram_gb": 0.25, "vram_gb": 4096, "format": "jpeg", "game": "D:\\Games\\GTA V"}))
            .unwrap();
        for stored in [Stored::default(), every, other_end, b, c, d] {
            let resolved = stored.resolve(&cwd);
            for zone in [None, Some([384.0, -1024.0, 640.0, -768.5])] {
                let line = resolved.render_args(zone, &derived);
                let mut args = line.clone();
                take_global_options(&resolved, &derived, true, &mut args);
                assert_eq!(args[0], "tiles", "{line:?}");
                let opts = crate::tiles::options::parse(&mut args).unwrap_or_else(|e| panic!("{e:#} in {line:?}"));
                let folder = if zone.is_some() { &resolved.preview } else { &resolved.output };
                assert_eq!(opts.out, PathBuf::from(folder));
                assert_eq!(opts.zone, zone.map(|z| z.map(|x| x as f32)));
                assert_tile_options(&resolved, &opts, &line);
                // nothing left but the command and its folder
                assert_eq!(args, ["tiles", folder.as_str()], "{line:?}");
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_settings_file_written_by_hand_is_read() {
        let dir = scratch("by-hand");
        let cwd = std::env::temp_dir().join("btms-cwd");
        // a few settings: the others are the defaults
        std::fs::write(
            dir.join(FILE),
            r#"{"threads": 10, "output": "D:\\tiles", "overrides": {"lighting": {"strength": 0.5}}}"#,
        )
        .unwrap();
        let stored = load(&dir);
        assert_eq!(stored.threads, Some(Threads::Count(10)));
        assert_eq!(stored.output.as_deref(), Some("D:\\tiles"));
        let r = stored.resolve(&cwd);
        assert_eq!(
            (r.threads, r.renderer, r.shading, r.priority, r.ram_gb),
            (Threads::Count(10), Unit::Auto, Unit::Auto, Priority::Normal, None)
        );
        // the word, and numbers a hand put out of bounds
        std::fs::write(dir.join(FILE), r#"{"threads": "auto", "ram_gb": 99999, "vram_gb": 0.01}"#).unwrap();
        let r = load(&dir).resolve(&cwd);
        assert_eq!((r.threads, r.ram_gb, r.vram_gb), (Threads::Auto, Some(MEMORY_GB.1), Some(MEMORY_GB.0)));
        std::fs::write(dir.join(FILE), r#"{"threads": 900}"#).unwrap();
        assert_eq!(load(&dir).resolve(&cwd).threads, Threads::Count(MAX_THREADS));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_command_line_of_a_render() {
        let cwd = std::env::temp_dir().join("btms-cwd");
        let derived = PathBuf::from("C:\\data\\BTMS\\interface\\effective-map.toml");
        let path = |p: &str| absolute(&cwd, p);
        // the defaults: the engine chooses the threads, who draws and who shades, and its budgets
        let r = Stored::default().resolve(&cwd);
        assert_eq!(
            r.render_args(None, &derived),
            [
                "tiles",
                &path("output/tiles"),
                "--threads",
                "auto",
                "--renderer",
                "auto",
                "--shading",
                "auto",
                "--config",
                &derived.display().to_string()
            ]
        );
        // a zone goes into the preview folder; every setting given
        let set = Stored::default()
            .merge(&json!({"threads": 14, "renderer": "gpu", "shading": "cpu", "ram_gb": 31, "vram_gb": 17.5, "priority": "low", "game": "E:\\GTA V"}))
            .unwrap()
            .resolve(&cwd);
        let args = set.render_args(Some([384.0, -1024.0, 640.0, -768.5]), &derived);
        assert_eq!(
            args,
            [
                "tiles",
                &path("output/preview"),
                "--zone",
                "384,-1024,640,-768.5",
                "--threads",
                "14",
                "--renderer",
                "gpu",
                "--shading",
                "cpu",
                "--ram",
                "31",
                "--vram",
                "17.5",
                "--config",
                &derived.display().to_string(),
                "--game",
                "E:\\GTA V"
            ]
        );
        // every option and its value are two arguments: none would be refused as `--threads=8`
        assert!(args.iter().all(|a| !(a.starts_with("--") && a.contains('='))));
        // the priority is not an option: it is how the process is started
        assert!(!args.iter().any(|a| a.contains("priority") || a == "low"));
    }

    #[test]
    fn the_command_line_of_an_update() {
        let cwd = std::env::temp_dir().join("btms-cwd");
        let derived = PathBuf::from("C:\\data\\BTMS\\interface\\effective-map.toml");
        let d = derived.display().to_string();
        let map = absolute(&cwd, "output/tiles");
        // the defaults: into the folder of the whole map, never the preview's
        let r = Stored::default().resolve(&cwd);
        assert_eq!(
            r.update_args(Update::Zone([384.0, -1024.0, 640.0, -768.5]), &derived),
            [
                "update",
                &map,
                "--zone",
                "384,-1024,640,-768.5",
                "--threads",
                "auto",
                "--renderer",
                "auto",
                "--shading",
                "auto",
                "--config",
                &d
            ]
        );
        assert_eq!(
            r.update_args(Update::Changes, &derived),
            [
                "update",
                &map,
                "--changes",
                "--threads",
                "auto",
                "--renderer",
                "auto",
                "--shading",
                "auto",
                "--config",
                &d
            ]
        );
        // listing and recording draw nothing: neither performance nor format
        assert_eq!(r.update_args(Update::List, &derived), ["update", &map, "--list", "--config", &d]);
        assert_eq!(r.update_args(Update::Record, &derived), ["update", &map, "--record", "--config", &d]);
        // every setting given: an update draws as the whole map would (performance and format)
        let set = Stored::default()
            .merge(&json!({"output": "D:\\Maps\\tiles", "preview": "D:\\Maps\\preview", "threads": 14, "renderer": "gpu", "shading": "cpu", "ram_gb": 31, "vram_gb": 17.5, "priority": "low", "quality": 80, "game": "E:\\GTA V"}))
            .unwrap()
            .resolve(&cwd);
        let tail = [
            "--threads",
            "14",
            "--renderer",
            "gpu",
            "--shading",
            "cpu",
            "--ram",
            "31",
            "--vram",
            "17.5",
            "--quality",
            "80",
            "--config",
            &d,
            "--game",
            "E:\\GTA V",
        ];
        let whole = set.render_args(None, &derived);
        assert_eq!(whole[..2], ["tiles", "D:\\Maps\\tiles"]);
        assert_eq!(whole[2..], tail);
        let zone = set.update_args(Update::Zone([0.0, 0.0, 128.0, 256.0]), &derived);
        assert_eq!(zone[..4], ["update", "D:\\Maps\\tiles", "--zone", "0,0,128,256"]);
        assert_eq!(zone[4..], tail);
        let changes = set.update_args(Update::Changes, &derived);
        assert_eq!(changes[..3], ["update", "D:\\Maps\\tiles", "--changes"]);
        assert_eq!(changes[3..], tail);
        assert_eq!(
            set.update_args(Update::Record, &derived),
            ["update", "D:\\Maps\\tiles", "--record", "--config", &d, "--game", "E:\\GTA V"]
        );
        // the older format
        let jpeg = Stored::default().merge(&json!({"format": "jpeg"})).unwrap().resolve(&cwd);
        let args = jpeg.update_args(Update::Changes, &derived);
        assert!(args.windows(2).any(|w| w == ["--format", "jpeg"]), "{args:?}");
        assert!(!jpeg.update_args(Update::List, &derived).iter().any(|a| a == "--format"));
        // never the preview folder, never an option glued to its value, never the priority
        for what in [Update::Zone([0.0, 0.0, 128.0, 128.0]), Update::Changes, Update::List, Update::Record] {
            let args = set.update_args(what, &derived);
            assert!(!args.iter().any(|a| a.contains("preview")), "{args:?}");
            assert!(args.iter().all(|a| !(a.starts_with("--") && a.contains('='))), "{args:?}");
            assert!(!args.iter().any(|a| a.contains("priority") || a == "low"), "{args:?}");
        }
        // and each one is a command line the engine reads, every mode with every setting: the
        // global options taken out by the engine's own code, then `update` reads the rest (it gives
        // the options of a render to `tiles`, which refuses any option it does not know)
        let lossless = Stored::default()
            .merge(&json!({"quality": "lossless", "threads": 3, "game": "E:\\GTA V"}))
            .unwrap()
            .resolve(&cwd);
        let engine_s = Stored::default().resolve(&cwd);
        for resolved in [&r, &set, &jpeg, &lossless] {
            for what in [Update::Zone([384.0, -1024.0, 640.0, -768.5]), Update::Changes, Update::List, Update::Record] {
                let draws = matches!(what, Update::Zone(_) | Update::Changes);
                let line = resolved.update_args(what, &derived);
                let mut args = line.clone();
                take_global_options(resolved, &derived, draws, &mut args);
                assert_eq!(args[0], "update", "{line:?}");
                let parsed = crate::update::parse(&args[1..]).unwrap_or_else(|e| panic!("{e:#} in {line:?}"));
                let mode = match what {
                    Update::Zone(z) => crate::update::Mode::Zone(z.map(|x| x as f32)),
                    Update::Changes => crate::update::Mode::Changes,
                    Update::List => crate::update::Mode::List,
                    Update::Record => crate::update::Mode::Record,
                };
                assert_eq!(parsed.mode, mode, "{line:?}");
                assert_eq!(
                    (&parsed.tiles.out, parsed.tiles.zone),
                    (&PathBuf::from(&resolved.output), None),
                    "{line:?}"
                );
                if draws {
                    // drawing: the performance and format settings, as a render of the whole map
                    assert_tile_options(resolved, &parsed.tiles, &line);
                    let format_given =
                        resolved.format == TileFormat::Jpeg || resolved.quality != TileQuality::default();
                    assert_eq!(parsed.format_asked, format_given, "{line:?}");
                } else {
                    // listing, recording: nothing of a render, the engine's own values
                    assert_tile_options(&engine_s, &parsed.tiles, &line);
                    assert!(!parsed.format_asked, "{line:?}");
                }
            }
        }
    }

    #[test]
    fn the_language_of_the_page_is_kept() {
        let cwd = std::env::temp_dir().join("btms-cwd");
        // none set: the browser's, null for the page
        assert_eq!(Stored::default().resolve(&cwd).language, None);
        // each of the seven, and null goes back to the browser's
        for code in LANGUAGES {
            let set = Stored::default().merge(&json!({"language": code})).unwrap();
            assert_eq!(set.language.as_deref(), Some(code));
            assert_eq!(serde_json::to_value(set.resolve(&cwd)).unwrap()["language"], json!(code));
            assert_eq!(set.merge(&json!({"language": null})).unwrap(), Stored::default());
        }
        // any other value is refused, with the reason
        for bad in
            [json!("auto"), json!(""), json!("EN"), json!("en-US"), json!("pt"), json!(1), json!(true), json!(["fr"])]
        {
            let refused = Stored::default().merge(&json!({"language": bad})).unwrap_err();
            assert_eq!(refused, "language: one of en, fr, de, nl, ru, sv, es expected, or null", "{bad}");
        }
        // kept in the file and read back; it is on no command line
        let dir = scratch("language");
        let stored = Stored::default().merge(&json!({"language": "de"})).unwrap();
        save(&dir, &stored).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), "{\n  \"language\": \"de\"\n}");
        assert_eq!(load(&dir), stored);
        let r = stored.resolve(&cwd);
        let derived = PathBuf::from("C:\\d\\effective-map.toml");
        for args in [
            r.render_args(None, &derived),
            r.update_args(Update::Changes, &derived),
            r.update_args(Update::Record, &derived),
        ] {
            assert!(!args.iter().any(|a| a.contains("language") || a == "de"), "{args:?}");
        }
        // a code written by hand that the page does not have: the browser's language, the other
        // settings kept
        std::fs::write(dir.join(FILE), r#"{"language": "xx", "threads": 4}"#).unwrap();
        let by_hand = load(&dir);
        assert_eq!(by_hand.threads, Some(Threads::Count(4)));
        assert_eq!(by_hand.resolve(&cwd).language, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overrides_are_merged_and_removed() {
        let a =
            Stored::default().merge(&json!({"overrides": {"lighting": {"elevation": 45, "strength": 0.5}}})).unwrap();
        let b = a
            .merge(
                &json!({"overrides": {"lighting": {"strength": null, "shadows": false}, "resources": {"folders": []}}}),
            )
            .unwrap();
        assert_eq!(
            Value::Object(b.overrides.clone()),
            json!({"lighting": {"elevation": 45, "shadows": false}, "resources": {"folders": []}})
        );
        // a section emptied goes; null clears a section, or everything
        let c = b.merge(&json!({"overrides": {"lighting": {"elevation": null, "shadows": null}}})).unwrap();
        assert_eq!(Value::Object(c.overrides.clone()), json!({"resources": {"folders": []}}));
        assert!(b.merge(&json!({"overrides": {"resources": null, "lighting": null}})).unwrap().overrides.is_empty());
        assert!(b.merge(&json!({"overrides": null})).unwrap().overrides.is_empty());
        // what is not a field of map.toml, or not of its kind, is refused
        for bad in [
            json!({"overrides": {"lighting": {"colour": 1}}}),
            json!({"overrides": {"sun": {"azimuth": 1}}}),
            json!({"overrides": {"lighting": {"enabled": "on"}}}),
            json!({"overrides": {"lighting": {"azimuth": "north"}}}),
            json!({"overrides": {"lighting": {"azimuth": 1e300}}}),
            json!({"overrides": {"maps": {"keep": "h4_*"}}}),
            json!({"overrides": {"maps": {"keep": [1, 2]}}}),
            json!({"overrides": {"lighting": 3}}),
        ] {
            assert!(Stored::default().merge(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_effective_configuration_is_the_base_with_the_overrides() {
        let base: Config =
            toml::from_str("[maps]\nkeep = ['a', 'b']\n[lighting]\nazimuth = 200\nstrength = 0.6").unwrap();
        assert_eq!(effective(&base, &Map::new()).unwrap(), base);
        let over = json!({"lighting": {"elevation": 45, "enabled": false}, "maps": {"keep": ["*"]}, "resources": {"folders": ["C:\\a\\[b]"]}});
        let c = effective(&base, over.as_object().unwrap()).unwrap();
        assert_eq!(c.maps.keep, ["*"]);
        assert_eq!(c.maps.exclude_folders, base.maps.exclude_folders);
        assert_eq!(c.resources.folders, ["C:\\a\\[b]"]);
        assert_eq!(
            (c.lighting.enabled, c.lighting.azimuth, c.lighting.elevation, c.lighting.strength),
            (false, 200.0, 45.0, 0.6)
        );
    }

    #[test]
    fn settings_are_kept_and_read_back() {
        let dir = scratch("settings");
        assert_eq!(load(&dir), Stored::default());
        let stored = Stored::default()
            .merge(&json!({"threads": 3, "output": "D:\\maps\\Ж 日本", "renderer": "cpu", "vram_gb": 6.5, "priority": "low", "overrides": {"lighting": {"strength": 0.25}}, "language": "sv"}))
            .unwrap();
        save(&dir, &stored).unwrap();
        assert_eq!(load(&dir), stored);
        // only what was changed is in the file
        let text = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(!text.contains("preview") && !text.contains("game") && text.contains("Ж 日本"), "{text}");
        // a broken file gives the defaults
        std::fs::write(dir.join(FILE), "{ not JSON").unwrap();
        assert_eq!(load(&dir), Stored::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_derived_file_reads_back_as_the_same_configuration() {
        let dir = scratch("derived");
        let mut configs = vec![Config::default()];
        // the package's own map.toml (the test runs in the package folder)
        if let Ok(text) = std::fs::read_to_string("map.toml") {
            configs.push(toml::from_str(&text).unwrap());
        }
        let mut odd = Config::default();
        odd.maps.keep =
            vec!["h4_*".into(), "with \"quotes\" and 'apostrophes'".into(), "Ж 日本 · ☃".into(), String::new()];
        odd.maps.exclude_folders.clear();
        odd.resources.folders =
            vec!["C:\\Users\\x\\[my-island]".into(), "\\\\server\\share\\a b".into(), "line\nnext\ttab".into()];
        odd.lighting = crate::config::Lighting {
            enabled: false,
            azimuth: 0.1,
            elevation: 33.333_332,
            strength: 1.0 / 3.0,
            shadows: false,
            ambient_occlusion: f32::MIN_POSITIVE,
        };
        configs.push(odd);
        for config in configs {
            let file = write_derived(&dir, &config).unwrap();
            assert_eq!(file, dir.join("interface").join("effective-map.toml"));
            assert_eq!(Config::load(Some(&file)).unwrap(), config);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_water_and_the_holes_are_part_of_what_a_job_is_given() {
        // `[water]` and `fill_holes` too: an override may name them, and the derived file
        // carries them
        let dir = scratch("water");
        let base = Config::default();
        assert!(base.water.enabled && base.maps.fill_holes);
        let over = json!({"water": {"enabled": false}, "maps": {"fill_holes": false}});
        let off = effective(&base, over.as_object().unwrap()).unwrap();
        assert!(!off.water.enabled && !off.maps.fill_holes);
        assert_eq!(off.maps.keep, base.maps.keep);
        for (config, value) in [(&base, "true"), (&off, "false")] {
            let file = write_derived(&dir, config).unwrap();
            let text = std::fs::read_to_string(&file).unwrap();
            assert!(text.contains(&format!("fill_holes = {value}")), "{text}");
            let water = text.split("[water]").nth(1).unwrap_or_else(|| panic!("no [water] section: {text}"));
            assert!(water.contains(&format!("enabled = {value}")), "{text}");
            assert_eq!(&Config::load(Some(&file)).unwrap(), config);
        }
        for bad in [
            json!({"overrides": {"water": {"enabled": "on"}}}),
            json!({"overrides": {"water": {"level": 1}}}),
            json!({"overrides": {"maps": {"fill_holes": 1}}}),
        ] {
            assert!(Stored::default().merge(&bad).is_err(), "{bad}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
