//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The commands on the game's archives alone, and how the rest of the program reads a file.
//!
//! - `verify` ([`verify`]): the keys, a known text decrypted byte-clean, archives listed with
//!   what they hold;
//! - `ls` ([`ls`]), `extract` ([`extract`]) and `find` ([`find`]): a folder of the archives
//!   listed, one file written out, the paths that hold a pattern;
//! - [`read_file`]: the bytes of any file of the game or of a server resource, through the
//!   archives opened once ([`store`]); [`read_file_uncached`] is its reference.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use rage_rpf::{GtaKeys, RpfEntryKind};

use crate::game::vfs::{self, Archive, Mapped, Nested};
use crate::game::{self, index, store};
use crate::util::profile;
use crate::world::fivem;

/// The bytes of the file at `path` (decrypted, decompressed; a resource keeps its RSC7 header).
/// The archives on the path are opened once and kept ([`store`]).
pub(crate) fn read_file(game: &Path, path: &str, keys: &GtaKeys) -> Result<Vec<u8>> {
    // a file streamed by a server resource, or any file on disk given by its full path: the same
    // bytes an archive gives. Before the archives: `store` only knows the game's `.rpf` paths (a
    // streamed file is timed as an extraction, for `--profile`)
    if fivem::is_streamed(path) {
        return profile::time(profile::Phase::ReadFile, || {
            profile::time(profile::Phase::Extract, || fivem::read(path))
        });
    }
    if Path::new(path).is_absolute() && Path::new(path).is_file() {
        return fivem::read_disk(Path::new(path));
    }
    store::read(game, path, keys)
}

/// The same for a file of the game, every archive on the path opened again for this one read: what
/// `read_file` did before [`store`], kept as the reference its check (`BTMS_CHECK_READS`)
/// compares with.
pub(crate) fn read_file_uncached(game: &Path, path: &str, keys: &GtaKeys) -> Result<Vec<u8>> {
    let mut out = None;
    at_path(game, path, keys, &mut |archive, index| {
        out = Some(archive.read(index, keys)?);
        Ok(())
    })?;
    out.context("empty file")
}

/// `ls <path>`: the entries of the folder at `path` in the archives, or the one file there.
pub(super) fn ls(game: &Path, path: &str) -> Result<()> {
    let keys = game::keys(game)?;
    at_path(game, path, &keys, &mut |archive, index| list(archive, index))
}

/// `extract <path> <file>`: the file at `path`, written to `out` as the game reads it.
pub(super) fn extract(game: &Path, path: &str, out: &Path) -> Result<()> {
    let keys = game::keys(game)?;
    at_path(game, path, &keys, &mut |archive, index| {
        let bytes = archive.read(index, &keys)?;
        std::fs::write(out, &bytes).with_context(|| format!("writing {}", out.display()))?;
        println!("{}: {} bytes → {}", archive.entry(index).name, bytes.len(), out.display());
        Ok(())
    })
}

/// `find <pattern>`: the paths of the game's files that hold `pattern` (already lowercase), 200 at
/// most.
pub(super) fn find(game: &Path, pattern: &str) -> Result<()> {
    let keys = game::keys(game)?;
    let mut n = 0;
    index::for_each_file(game, &keys, &mut |p, _, _| {
        if p.contains(pattern) {
            println!("{p}");
            n += 1;
        }
        n < 200
    })?;
    Ok(())
}

/// Calls `action` on the entry at `path` (a folder: the folder's index; the root of an archive: 0).
fn at_path(
    game: &Path,
    path: &str,
    keys: &GtaKeys,
    action: &mut dyn FnMut(&Archive<'_>, usize) -> Result<()>,
) -> Result<()> {
    let (disk, inner) = vfs::split_disk_path(game, path)?;
    let mapped = Mapped::open(&disk)?;
    let name = disk.file_name().unwrap().to_string_lossy().to_string();
    let archive = Archive::parse(mapped.bytes(), &name, keys)?;
    descend(&archive, 0, &inner, keys, action)
}

fn descend(
    archive: &Archive<'_>,
    dir: usize,
    rest: &[String],
    keys: &GtaKeys,
    action: &mut dyn FnMut(&Archive<'_>, usize) -> Result<()>,
) -> Result<()> {
    let Some(first) = rest.first() else { return action(archive, dir) };
    let index = archive.child(dir, first).with_context(|| format!("{first} not found in {}", archive.name))?;
    let entry = archive.entry(index);
    if entry.is_directory() {
        descend(archive, index, &rest[1..], keys, action)
    } else if entry.name_lower.ends_with(".rpf") {
        match archive.nested(index, keys)? {
            Nested::Borrowed(inner) => descend(&inner, 0, &rest[1..], keys, action),
            Nested::Owned(bytes, name) => {
                let inner = Archive::parse(&bytes, &name, keys)?;
                descend(&inner, 0, &rest[1..], keys, action)
            }
        }
    } else if rest.len() == 1 {
        action(archive, index)
    } else {
        bail!("{} is a file", entry.name)
    }
}

fn list(archive: &Archive<'_>, index: usize) -> Result<()> {
    let entry = archive.entry(index);
    if !entry.is_directory() {
        println!("{}", describe(archive, index));
        return Ok(());
    }
    for i in archive.children(index) {
        println!("{}", describe(archive, i));
    }
    Ok(())
}

fn describe(archive: &Archive<'_>, i: usize) -> String {
    let e = archive.entry(i);
    match &e.kind {
        RpfEntryKind::Directory { entries_count, .. } => {
            format!("{:<48} folder ({entries_count})", format!("{}/", e.name))
        }
        RpfEntryKind::BinaryFile { file_size, uncompressed_size, is_encrypted, .. } => format!(
            "{:<48} file {uncompressed_size} B{}{}",
            e.name,
            if *file_size > 0 && file_size < uncompressed_size {
                format!(" (compressed {file_size})")
            } else {
                String::new()
            },
            if *is_encrypted { ", encrypted" } else { "" }
        ),
        RpfEntryKind::ResourceFile { file_size, is_encrypted, .. } => {
            format!("{:<48} resource {file_size} B{}", e.name, if *is_encrypted { ", encrypted" } else { "" })
        }
    }
}

/// Text check: the bytes are an XML document (after an optional BOM and blanks).
fn looks_like_xml(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes)).ok()?;
    let t = text.trim_start();
    t.starts_with('<').then(|| t.lines().next().unwrap_or("").chars().take(80).collect())
}

/// `verify`: the keys, a known text decrypted byte-clean, archives listed with what they hold.
pub(super) fn verify(game: &Path) -> Result<()> {
    println!("game: {}", game.display());
    let t = Instant::now();
    let keys = game::keys(game)?;
    println!("keys: ready ({:.1} s, kept in {})", t.elapsed().as_secs_f64(), game::keys_dir()?.display());
    let mut failures = 0;

    for (label, path, want_xml) in [
        ("common.rpf", "common.rpf", true),
        ("update.rpf", "update/update.rpf", true),
        ("Cayo Perico (mpheist4, dlc1.rpf)", "update/x64/dlcpacks/mpheist4/dlc1.rpf", false),
        ("x64c.rpf", "x64c.rpf", false),
    ] {
        let t = Instant::now();
        type Summary = (usize, usize, BTreeMap<String, usize>, Option<(String, String)>, String);
        let result = (|| -> Result<Summary> {
            let (disk, _) = vfs::split_disk_path(game, path)?;
            let mapped = Mapped::open(&disk)?;
            let archive = Archive::parse(mapped.bytes(), &disk.file_name().unwrap().to_string_lossy(), &keys)?;
            let encryption = format!("{:?}", archive.rpf.encryption);
            let (mut files, mut nested) = (0usize, 0usize);
            let mut by_ext: BTreeMap<String, usize> = BTreeMap::new();
            let mut xml: Option<(String, String)> = None;
            let mut xml_error: Option<String> = None;
            vfs::walk(&archive, 0, "", &keys, &mut |p, a, i| {
                files += 1;
                if p.matches(".rpf/").count() > 0 && p.split('/').filter(|s| s.ends_with(".rpf")).count() > nested {
                    nested = p.split('/').filter(|s| s.ends_with(".rpf")).count();
                }
                let ext = p.rsplit('.').next().unwrap_or("").to_string();
                *by_ext.entry(ext.clone()).or_default() += 1;
                if want_xml
                    && xml.is_none()
                    && (ext == "xml" || ext == "meta")
                    && let RpfEntryKind::BinaryFile { .. } = a.entry(i).kind
                {
                    match a.read(i, &keys) {
                        Ok(bytes) => match looks_like_xml(&bytes) {
                            Some(first) => xml = Some((p.to_string(), first)),
                            None => xml_error = Some(format!("{p}: not XML after decryption")),
                        },
                        Err(e) => xml_error = Some(format!("{p}: {e:#}")),
                    }
                }
                true
            })?;
            if let Some(e) = xml_error.filter(|_| xml.is_none()) {
                bail!(e);
            }
            Ok((files, nested, by_ext, xml, encryption))
        })();
        match result {
            Ok((files, depth, by_ext, xml, encryption)) => {
                let map_files: Vec<String> = ["ymap", "ytyp", "ydr", "ydd", "yft", "ytd", "ybn"]
                    .iter()
                    .map(|e| format!("{e} {}", by_ext.get(*e).copied().unwrap_or(0)))
                    .collect();
                println!(
                    "ok   {label}: encryption {encryption}, {files} files (archives nested {depth} level(s) deep), {:.1} s",
                    t.elapsed().as_secs_f64()
                );
                println!("     map files: {}", map_files.join(", "));
                match (want_xml, xml) {
                    (true, Some((p, first))) => println!("     plain text: {p} → {first}"),
                    (true, None) => {
                        println!("FAIL {label}: no XML found to check");
                        failures += 1;
                    }
                    _ => {}
                }
            }
            Err(e) => {
                println!("FAIL {label}: {e:#}");
                failures += 1;
            }
        }
    }
    if failures > 0 {
        bail!("{failures} check(s) failed");
    }
    println!("ok: the archives are read and decrypted");
    Ok(())
}
