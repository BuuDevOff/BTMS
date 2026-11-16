//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The game's files as one tree: folders of the install, `.rpf` archives, folders inside them and
//! archives inside archives (`update/x64/dlcpacks/mpheist4/dlc.rpf/x64/levels/…`).
//!
//! An archive on disk is mapped in memory, read only (`x64*.rpf` weigh up to 2.7 GB: nothing is
//! copied, the system reads the pages asked for). An archive inside an archive is parsed from the
//! exact slice it occupies: the NG key of its table depends on its own length.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use memmap2::Mmap;
use rage_rpf::{GtaKeys, RpfArchive, RpfEntry, RpfEntryKind};

/// Encryption field of an RPF7 header for the NG cipher (CodeWalker `RpfEncryption.NG`; 0x0FFFFFF9 is AES).
const NG_ENCRYPTION: u32 = 0x0FEF_FFFF;

/// An archive mapped from disk.
pub struct Mapped {
    map: Mmap,
}

impl Mapped {
    pub fn open(path: &Path) -> Result<Mapped> {
        // std opens with read and write sharing on Windows: the game and FiveM may hold the file
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        // SAFETY: the mapping is read only; the game files are not changed while the tool runs
        let map = unsafe { Mmap::map(&file) }.with_context(|| format!("mapping {}", path.display()))?;
        Ok(Mapped { map })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.map
    }
}

/// An archive (on disk or inside another) and the bytes it lives in.
pub struct Archive<'a> {
    pub name: String,
    pub data: &'a [u8],
    pub rpf: RpfArchive,
}

impl<'a> Archive<'a> {
    /// The archive held by `data` (exactly its bytes).
    pub fn parse(data: &'a [u8], name: &str, keys: &GtaKeys) -> Result<Archive<'a>> {
        // an NG archive would be decrypted with empty keys (and panic): refused before
        if data.len() >= 16
            && data[0..4] == *b"7FPR"
            && data[12..16] == NG_ENCRYPTION.to_le_bytes()
            && !crate::game::has_ng(keys)
        {
            bail!(
                "archive {name} is NG encrypted: NG keys missing (only the AES key is known; run `btms import-keys <magic.dat>` with the magic.dat of your own copy of CodeWalker)"
            );
        }
        let rpf = RpfArchive::parse(data, name, Some(keys)).with_context(|| format!("reading the archive {name}"))?;
        Ok(Archive { name: name.to_string(), data, rpf })
    }

    pub fn entry(&self, index: usize) -> &RpfEntry {
        &self.rpf.entries[index]
    }

    /// Indices of the entries of a folder (0 = root).
    pub fn children(&self, dir: usize) -> std::ops::Range<usize> {
        match self.rpf.entries.get(dir).map(|e| &e.kind) {
            Some(RpfEntryKind::Directory { entries_index, entries_count }) => {
                let start = *entries_index as usize;
                start..(start + *entries_count as usize).min(self.rpf.entries.len())
            }
            _ => 0..0,
        }
    }

    pub fn child(&self, dir: usize, name: &str) -> Option<usize> {
        let lower = name.to_lowercase();
        self.children(dir).find(|&i| self.rpf.entries[i].name_lower == lower)
    }

    /// The bytes of a file, decrypted and decompressed (a resource keeps its RSC7 header).
    pub fn read(&self, index: usize, keys: &GtaKeys) -> Result<Vec<u8>> {
        let entry = &self.rpf.entries[index];
        let encrypted = matches!(
            entry.kind,
            RpfEntryKind::BinaryFile { is_encrypted: true, .. } | RpfEntryKind::ResourceFile { is_encrypted: true, .. }
        );
        if encrypted && self.rpf.encryption == rage_rpf::RpfEncryption::Ng && !crate::game::has_ng(keys) {
            bail!(
                "{} is NG encrypted: NG keys missing (run `btms import-keys <magic.dat>` with the magic.dat of your own copy of CodeWalker)",
                entry.name
            );
        }
        self.rpf
            .extract_entry(self.data, entry, Some(keys))
            .with_context(|| format!("extracting {} from {}", entry.name, self.name))
    }

    /// The archive held by entry `index` (a `.rpf` inside this one), without copying it when it is
    /// stored as is (the usual case).
    pub fn nested(&self, index: usize, keys: &GtaKeys) -> Result<Nested<'a>> {
        let entry = &self.rpf.entries[index];
        let RpfEntryKind::BinaryFile { file_offset, file_size, uncompressed_size, is_encrypted } = entry.kind else {
            bail!("{} is not an archive", entry.name);
        };
        let size = if file_size > 0 { file_size } else { uncompressed_size } as usize;
        let start = self.rpf.start_offset + file_offset as usize * 512;
        let stored = !is_encrypted && (file_size == 0 || file_size == uncompressed_size);
        if stored {
            let slice = self
                .data
                .get(start..start + size)
                .with_context(|| format!("{} lies outside its archive", entry.name))?;
            Ok(Nested::Borrowed(Archive::parse(slice, &entry.name, keys)?))
        } else {
            Ok(Nested::Owned(self.read(index, keys)?, entry.name.clone()))
        }
    }
}

/// An archive inside an archive: a slice of its parent, or (compressed or encrypted as a whole,
/// unusual) its own bytes.
pub enum Nested<'a> {
    Borrowed(Archive<'a>),
    Owned(Vec<u8>, String),
}

/// Every file under a folder of an archive, archives inside included: (path, archive, index),
/// `visit` returning false stops the walk.
pub fn walk<'a>(
    archive: &Archive<'a>,
    dir: usize,
    prefix: &str,
    keys: &GtaKeys,
    visit: &mut dyn FnMut(&str, &Archive<'_>, usize) -> bool,
) -> Result<bool> {
    for i in archive.children(dir) {
        let entry = archive.entry(i);
        let path = if prefix.is_empty() { entry.name_lower.clone() } else { format!("{prefix}/{}", entry.name_lower) };
        if entry.is_directory() {
            if !walk(archive, i, &path, keys, visit)? {
                return Ok(false);
            }
        } else if entry.name_lower.ends_with(".rpf") {
            match archive.nested(i, keys) {
                Ok(Nested::Borrowed(inner)) => {
                    if !walk(&inner, 0, &path, keys, visit)? {
                        return Ok(false);
                    }
                }
                Ok(Nested::Owned(bytes, name)) => {
                    let inner = Archive::parse(&bytes, &name, keys)?;
                    if !walk(&inner, 0, &path, keys, visit)? {
                        return Ok(false);
                    }
                }
                Err(e) => eprintln!("  unreadable archive {path}: {e:#}"),
            }
        } else if !visit(&path, archive, i) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Splits `update/update.rpf/x64/…` into the archive on disk and the path inside it.
pub fn split_disk_path(game: &Path, path: &str) -> Result<(PathBuf, Vec<String>)> {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    let mut disk = game.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        disk.push(part);
        if part.to_lowercase().ends_with(".rpf") && disk.is_file() {
            return Ok((disk, parts[i + 1..].iter().map(|s| s.to_string()).collect()));
        }
    }
    bail!("no .rpf archive in the path {path}")
}
