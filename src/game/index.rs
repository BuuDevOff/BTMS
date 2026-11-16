//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! Every file of the game, all archives together, and the one the game uses for a name.
//!
//! The same name exists in several archives (the base `x64*.rpf`, the patch `update/update.rpf`,
//! the DLC packs). Which copy wins ([`Index::rank`]): the base game, under the patch `update.rpf`,
//! under the DLC packs in the order the game loads them (`dlclist.xml` of the patch: a pack loaded
//! later wins), each pack under its own patch (`update.rpf/dlc_patch/<pack>`). A pack absent from
//! the list ranks just above the patch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rage_rpf::GtaKeys;

use crate::formats::resource::joaat;
use crate::game::vfs::{self, Archive, Mapped};

/// The `.rpf` archives of the install (not `mods/`), in a stable order.
pub fn disk_archives(game: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            if p.is_dir() {
                if name != "mods" {
                    walk(&p, out);
                }
            } else if name.ends_with(".rpf") {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(game, &mut out);
    out
}

/// The path of `disk` under the game folder, with `/`.
pub(crate) fn relative(game: &Path, disk: &Path) -> String {
    disk.strip_prefix(game).unwrap_or(disk).to_string_lossy().replace('\\', "/").to_lowercase()
}

/// Calls `visit` with (full path, archive, entry) for every file of every archive.
pub fn for_each_file(
    game: &Path,
    keys: &GtaKeys,
    visit: &mut dyn FnMut(&str, &Archive<'_>, usize) -> bool,
) -> Result<()> {
    for disk in disk_archives(game) {
        let mapped = Mapped::open(&disk)?;
        let name = disk.file_name().unwrap().to_string_lossy().to_string();
        let archive = match Archive::parse(mapped.bytes(), &name, keys) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("  unreadable archive {}: {e:#}", disk.display());
                continue;
            }
        };
        let prefix = relative(game, &disk);
        if !vfs::walk(&archive, 0, &prefix, keys, visit)? {
            break;
        }
    }
    Ok(())
}

/// The DLC packs in the order the game loads them: the `dlcpacks:/<name>/` items of `dlclist.xml`.
pub fn dlc_order(dlclist: &str) -> Vec<String> {
    dlclist
        .split("dlcpacks:/")
        .skip(1)
        .filter_map(|s| s.split(['/', '<']).next())
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Adds the file `path` to the files of an index being built ([`Index::of_files`]), after the
/// copies of its name already there.
pub fn add_file(files: &mut HashMap<(u32, String), Vec<String>>, path: &str) {
    let name = path.rsplit('/').next().unwrap_or(path);
    if let Some((stem, ext)) = name.rsplit_once('.') {
        files.entry((joaat(stem), ext.to_string())).or_default().push(path.to_string());
    }
}

/// Every file of the game by (hash of its name without extension, extension): the paths of its
/// copies, the one preferred first.
pub struct Index {
    pub files: HashMap<(u32, String), Vec<String>>,
    /// DLC packs, in load order.
    pub dlcs: Vec<String>,
    /// The order reversed (the base game first): a check that the order matters.
    pub base_first: bool,
}

impl Index {
    /// How much a copy of a file is preferred (higher wins), from its path.
    pub fn rank(&self, path: &str) -> usize {
        // a file streamed by a server resource wins over every copy of the game, in any order
        if let Some(r) = crate::world::fivem::rank(path) {
            return r;
        }
        let r = self.rank_patch_first(path);
        if self.base_first { 1000 - r } else { r }
    }

    fn rank_patch_first(&self, path: &str) -> usize {
        let pack = |rest: &str| -> usize {
            let name = rest.split('/').next().unwrap_or("");
            self.dlcs.iter().position(|d| d == name).map(|i| 10 + 2 * i).unwrap_or(5)
        };
        if let Some(rest) = path.strip_prefix("update/update.rpf/dlc_patch/") {
            pack(rest) + 1
        } else if let Some(rest) = path.split("/dlcpacks/").nth(1) {
            pack(rest)
        } else if path.starts_with("update/") {
            1
        } else {
            0
        }
    }

    /// The index of the game's files given in the order of the walk ([`add_file`]): the files of
    /// the server's resources added, the copies of each name ordered.
    pub fn of_files(mut files: HashMap<(u32, String), Vec<String>>, dlclist: &str, base_first: bool) -> Index {
        // the files the server's resources stream (`[resources]` of map.toml)
        for f in &crate::world::fivem::server().streamed {
            add_file(&mut files, &f.path);
        }
        let mut index = Index { files, dlcs: dlc_order(dlclist), base_first };
        let mut files = std::mem::take(&mut index.files);
        for copies in files.values_mut() {
            copies.sort_by_key(|p| std::cmp::Reverse(index.rank(p)));
        }
        index.files = files;
        index
    }

    /// The copies of the file `hash`.`ext`, the preferred first.
    pub fn copies(&self, hash: u32, ext: &str) -> &[String] {
        self.files.get(&(hash, ext.to_string())).map(Vec::as_slice).unwrap_or(&[])
    }
}
