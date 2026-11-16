//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! What the tiles show, as the user sets it: `map.toml` (in the current folder, or `--config
//! <file>`). Every key has a default, so the file may hold only what changes; without a file
//! the defaults below apply. Read once at startup ([`init`]), then [`get`] anywhere.
//!
//! - `[maps]`: which placement files are drawn (maps a script loads on demand, folders the game
//!   never loads with the map);
//! - `[resources]`: the FiveM resources of the server, which win over the game's files;
//! - `[lighting]`: the shading of the tiles;
//! - `[water]`: the water of the tiles.

use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Name of the file looked for in the current folder.
pub const FILE: &str = "map.toml";

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub maps: Maps,
    pub resources: Resources,
    pub lighting: Lighting,
    pub water: Water,
}

/// The water of the tiles.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Water {
    /// The sea, the lakes, the rivers and the pools are drawn. False: the ground under them.
    pub enabled: bool,
}

impl Default for Water {
    fn default() -> Self {
        Water { enabled: true }
    }
}

/// Which placement files are drawn.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Maps {
    /// Maps loaded on a script's request are left out, except these: names of map groups or of
    /// placement files, `*` standing for anything (`h4_*`), or names of islands of the game
    /// (`HeistIsland`).
    pub keep: Vec<String>,
    /// Folders of the game whose placement files are never loaded with the map.
    pub exclude_folders: Vec<String>,
    /// Keeps, besides the white list, the maps loaded on request without which the ground has a
    /// hole (the original state of a place a pack rebuilt).
    pub fill_holes: bool,
}

impl Default for Maps {
    fn default() -> Self {
        Maps {
            keep: vec!["hei_carrier".into(), "hei_yacht_heist".into(), "HeistIsland".into()],
            exclude_folders: vec!["/levels/gta5/_prologue/".into()],
            fill_holes: true,
        }
    }
}

/// The FiveM resources of the server: their files win over the game's files of the same name.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Resources {
    /// Resource folders (or folders of resources, like `[maps]`), the last one winning.
    pub folders: Vec<String>,
}

/// The shading of the tiles.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Lighting {
    /// No shading at all when false: the textures' own colour.
    pub enabled: bool,
    /// Where the sun is, in degrees clockwise from north (0 north, 90 east, 315 north-west).
    pub azimuth: f32,
    /// Height of the sun above the horizon, in degrees.
    pub elevation: f32,
    /// Strength of the shading: 0 none, 1 strong.
    pub strength: f32,
    /// Cast shadows.
    pub shadows: bool,
    /// Darkening at the foot of walls: 0 none, 1 strong.
    pub ambient_occlusion: f32,
}

impl Default for Lighting {
    fn default() -> Self {
        Lighting {
            enabled: true,
            azimuth: 315.0,
            elevation: 35.0,
            strength: 0.6,
            shadows: true,
            ambient_occlusion: 0.3,
        }
    }
}

impl Config {
    /// The configuration of `path`, or the defaults when `path` is None and there is no
    /// `map.toml` in the current folder.
    pub fn load(path: Option<&Path>) -> Result<Config> {
        let file = match path {
            Some(p) => p.to_path_buf(),
            None if Path::new(FILE).is_file() => Path::new(FILE).to_path_buf(),
            None => return Ok(Config::default()),
        };
        let text = std::fs::read_to_string(&file).with_context(|| format!("reading {}", file.display()))?;
        toml::from_str(&text).with_context(|| format!("{} is unreadable", file.display()))
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();

/// Sets the configuration for the whole run (once, at startup).
pub fn init(config: Config) {
    let _ = CONFIG.set(config);
}

/// The configuration ([`init`]), or the defaults when nothing was set (tests).
pub fn get() -> &'static Config {
    CONFIG.get_or_init(Config::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_partial_file() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c.maps.keep, ["hei_carrier", "hei_yacht_heist", "HeistIsland"]);
        assert!(c.lighting.enabled && c.lighting.shadows);
        let c: Config =
            toml::from_str("[lighting]\nshadows = false\nazimuth = 90\n[resources]\nfolders = ['C:\\a\\[b]']").unwrap();
        assert!(!c.lighting.shadows);
        assert_eq!(c.lighting.azimuth, 90.0);
        assert_eq!(c.lighting.elevation, 35.0);
        assert_eq!(c.resources.folders, ["C:\\a\\[b]"]);
        assert!(toml::from_str::<Config>("[lighting]\nunknown = 1").is_err());
    }

    #[test]
    fn the_repository_map_toml_holds_the_defaults() {
        // the file shipped with BTMS documents every key with its default value: the two must not drift apart
        let shipped: Config = toml::from_str(include_str!("../map.toml")).expect("map.toml of the repository");
        assert_eq!(shipped, Config::default());
    }
}
