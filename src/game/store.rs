//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The archives opened once: `crate::cli::archive::read_file` goes through here.
//!
//! Reading a file used to map its archive from disk, decrypt and parse the archive's table, then
//! do the same for every archive nested on the way, for each file read. Here an archive is opened
//! the first time a path goes through it and kept for the rest of the run (its mapping, its parsed
//! table, the entries of its folders by name), shared by all threads. What is read is unchanged:
//! the same entry, extracted by the same `rage-rpf` call.
//!
//! `BTMS_CHECK_READS=1` reads every file both ways (through the cache, and by opening
//! everything again as before) and stops on the first difference: the proof that the cache gives
//! the same bytes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, RwLock};

use anyhow::{Context, Result, bail};
use rage_rpf::{GtaKeys, RpfArchive, RpfEncryption, RpfEntryKind};

use crate::game::vfs::{Archive, Mapped};
use crate::util::profile::{Phase, time};

/// Where the bytes of an opened archive are.
enum Bytes {
    /// An archive of the install, mapped from disk.
    Disk(Mapped),
    /// An archive stored as is inside another: a slice of its parent (kept open with it).
    Inside { parent: Arc<Opened>, start: usize, len: usize },
    /// An archive compressed or encrypted as a whole inside another (unusual): its own bytes.
    Owned(Vec<u8>),
}

/// An archive kept open: its bytes, its table, its folders.
struct Opened {
    name: String,
    bytes: Bytes,
    rpf: RpfArchive,
    /// (folder index, lowercase name) → entry index: the first entry of that name, as a scan of
    /// the folder would find it.
    children: HashMap<(u32, String), u32>,
}

impl Opened {
    fn new(name: &str, bytes: Bytes, keys: &GtaKeys) -> Result<Opened> {
        // the same checks and the same parsing as an archive opened for one read
        let rpf = Archive::parse(slice(&bytes), name, keys)?.rpf;
        let mut children = HashMap::new();
        for (dir, entry) in rpf.entries.iter().enumerate() {
            if let RpfEntryKind::Directory { entries_index, entries_count } = entry.kind {
                let start = entries_index as usize;
                for i in start..(start + entries_count as usize).min(rpf.entries.len()) {
                    children.entry((dir as u32, rpf.entries[i].name_lower.clone())).or_insert(i as u32);
                }
            }
        }
        Ok(Opened { name: name.to_string(), bytes, rpf, children })
    }

    fn bytes(&self) -> &[u8] {
        slice(&self.bytes)
    }

    fn child(&self, dir: usize, lower: &str) -> Option<usize> {
        self.children.get(&(dir as u32, lower.to_string())).map(|&i| i as usize)
    }

    /// The bytes of a file, decrypted and decompressed: what `vfs::Archive::read` gives.
    fn read(&self, index: usize, keys: &GtaKeys) -> Result<Vec<u8>> {
        let entry = &self.rpf.entries[index];
        let encrypted = matches!(
            entry.kind,
            RpfEntryKind::BinaryFile { is_encrypted: true, .. } | RpfEntryKind::ResourceFile { is_encrypted: true, .. }
        );
        if encrypted && self.rpf.encryption == RpfEncryption::Ng && !crate::game::has_ng(keys) {
            bail!(
                "{} is NG encrypted: NG keys missing (run `btms import-keys <magic.dat>` with the magic.dat of your own copy of CodeWalker)",
                entry.name
            );
        }
        self.rpf
            .extract_entry(self.bytes(), entry, Some(keys))
            .with_context(|| format!("extracting {} from {}", entry.name, self.name))
    }

    /// Where the archive held by entry `index` is: what `vfs::Archive::nested` does.
    fn nested(self: &Arc<Opened>, index: usize, keys: &GtaKeys) -> Result<Opened> {
        Opened::new(&self.rpf.entries[index].name, self.nested_bytes(index, keys)?, keys)
    }

    /// The bytes of the archive held by entry `index`: a slice of this one when it is stored as is
    /// (the usual case), else its own bytes, read.
    fn nested_bytes(self: &Arc<Opened>, index: usize, keys: &GtaKeys) -> Result<Bytes> {
        let entry = &self.rpf.entries[index];
        let RpfEntryKind::BinaryFile { file_offset, file_size, uncompressed_size, is_encrypted } = entry.kind else {
            bail!("{} is not an archive", entry.name);
        };
        let len = if file_size > 0 { file_size } else { uncompressed_size } as usize;
        let start = self.rpf.start_offset + file_offset as usize * 512;
        let stored = !is_encrypted && (file_size == 0 || file_size == uncompressed_size);
        Ok(if stored {
            if start.checked_add(len).is_none_or(|end| end > self.bytes().len()) {
                bail!("{} lies outside its archive", entry.name);
            }
            Bytes::Inside { parent: self.clone(), start, len }
        } else {
            Bytes::Owned(self.read(index, keys)?)
        })
    }

    /// The entries of a folder (0 = root): what `vfs::Archive::children` gives.
    fn children(&self, dir: usize) -> std::ops::Range<usize> {
        match self.rpf.entries.get(dir).map(|e| &e.kind) {
            Some(RpfEntryKind::Directory { entries_index, entries_count }) => {
                let start = *entries_index as usize;
                start..(start + *entries_count as usize).min(self.rpf.entries.len())
            }
            _ => 0..0,
        }
    }
}

fn slice(bytes: &Bytes) -> &[u8] {
    match bytes {
        Bytes::Disk(mapped) => mapped.bytes(),
        Bytes::Inside { parent, start, len } => &parent.bytes()[*start..*start + *len],
        Bytes::Owned(bytes) => bytes,
    }
}

/// The archives opened so far, by path: the path on disk of an archive of the install, followed
/// for a nested one by its path inside (all lowercase, `/` between the parts).
static OPEN: LazyLock<RwLock<HashMap<String, Arc<Opened>>>> = LazyLock::new(Default::default);

/// The archive `key`, opened by `open` the first time.
fn opened(key: &str, open: impl FnOnce() -> Result<Opened>) -> Result<Arc<Opened>> {
    if let Some(found) = OPEN.read().unwrap().get(key) {
        return Ok(found.clone());
    }
    // opened outside the lock: two threads may open the same archive at once, the first one kept
    let archive = Arc::new(open()?);
    Ok(OPEN.write().unwrap().entry(key.to_string()).or_insert(archive).clone())
}

/// The archive that holds the file at `path` (`update/update.rpf/x64/…`), and the file's entry
/// in it. A path that ends on a folder or on an archive gives that folder's entry (0: the root),
/// which cannot be read, as before.
fn locate(game: &Path, path: &str, keys: &GtaKeys) -> Result<(Arc<Opened>, usize)> {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    // the archive on disk: the first part ending in .rpf that is a file
    let mut disk = game.to_path_buf();
    let mut found = None;
    for (i, part) in parts.iter().enumerate() {
        disk.push(part);
        if part.to_lowercase().ends_with(".rpf") {
            let key = disk.to_string_lossy().to_lowercase();
            if OPEN.read().unwrap().contains_key(&key) || disk.is_file() {
                let archive = opened(&key, || {
                    let name = disk.file_name().unwrap().to_string_lossy().to_string();
                    Opened::new(&name, Bytes::Disk(Mapped::open(&disk)?), keys)
                })?;
                found = Some((archive, key, i + 1));
                break;
            }
        }
    }
    let Some((mut archive, mut key, first)) = found else { bail!("no .rpf archive in the path {path}") };
    let mut dir = 0;
    for (i, part) in parts.iter().enumerate().skip(first) {
        let lower = part.to_lowercase();
        let index = archive.child(dir, &lower).with_context(|| format!("{part} not found in {}", archive.name))?;
        key.push('/');
        key.push_str(&lower);
        let entry = &archive.rpf.entries[index];
        if entry.is_directory() {
            dir = index;
        } else if entry.name_lower.ends_with(".rpf") {
            archive = opened(&key, || archive.nested(index, keys))?;
            dir = 0;
        } else if i + 1 == parts.len() {
            return Ok((archive, index));
        } else {
            bail!("{} is a file", entry.name)
        }
    }
    Ok((archive, dir))
}

/// An archive kept open, as [`for_each_file`] gives it: the files it holds can be read from any
/// thread, after the walk too.
#[derive(Clone)]
pub struct ArchiveRef(Arc<Opened>);

impl ArchiveRef {
    /// The bytes of its file `index` (decrypted, decompressed): what `vfs::Archive::read` gives.
    pub fn read(&self, index: usize, keys: &GtaKeys) -> Result<Vec<u8>> {
        self.0.read(index, keys)
    }
}

/// What the walk found at an entry `.rpf` of an archive.
enum Inner {
    /// The archive, opened: its place in [`Tree::nodes`].
    Open(usize),
    /// It cannot be opened: said each time the walk comes to it, then skipped (`vfs::walk`).
    Unreadable(anyhow::Error),
    /// Compressed or encrypted as a whole, its bytes read, but they are not an archive: the walk
    /// stops there with this error (`vfs::walk` does the same).
    Fatal(Option<anyhow::Error>),
}

/// An archive opened by the walk, and the archives it holds.
struct Node {
    archive: ArchiveRef,
    inner: HashMap<usize, Inner>,
}

/// Every archive of the install opened, before the walk goes through them in order.
struct Tree {
    nodes: Vec<Node>,
    /// The archives on disk, in the order of `index::disk_archives`: their node, or why they
    /// could not be opened (not an archive: said and skipped; not mapped: the walk stops).
    disks: Vec<(std::path::PathBuf, Inner)>,
}

/// The archives held by a folder of `archive` and its sub-folders, in the order of the walk:
/// (entry, cache key of the archive).
fn nested_entries(archive: &Opened, dir: usize, key: &str, out: &mut Vec<(usize, String)>) {
    for i in archive.children(dir) {
        let entry = &archive.rpf.entries[i];
        let inner = format!("{key}/{}", entry.name_lower);
        if entry.is_directory() {
            nested_entries(archive, i, &inner, out);
        } else if entry.name_lower.ends_with(".rpf") {
            out.push((i, inner));
        }
    }
}

/// Opens every archive of the install, those inside others too, on all the logical cores: the
/// archives on disk at once, then the archives they hold, then those these hold… Each one is
/// opened through the cache (kept for the rest of the run, found there when it was already open):
/// the tables are decrypted once for all the walks and reads of the run.
fn open_all(game: &Path, keys: &GtaKeys) -> Tree {
    let disks = crate::game::index::disk_archives(game);
    let opened_disks = crate::util::parallel::map(&disks, |disk| {
        let key = disk.to_string_lossy().to_lowercase();
        // (a file that cannot be mapped stops the walk; one that is not an archive is skipped)
        let mut mapped = true;
        opened(&key, || {
            let name = disk.file_name().unwrap().to_string_lossy().to_string();
            let bytes = Bytes::Disk(Mapped::open(disk).inspect_err(|_| mapped = false)?);
            Opened::new(&name, bytes, keys)
        })
        .map(|a| (a, key))
        .map_err(|e| if mapped { Inner::Unreadable(e) } else { Inner::Fatal(Some(e)) })
    });
    let mut tree = Tree { nodes: Vec::new(), disks: Vec::new() };
    let mut level = Vec::new();
    for (disk, opened) in disks.into_iter().zip(opened_disks) {
        let at = match opened {
            Ok((archive, key)) => {
                tree.nodes.push(Node { archive: ArchiveRef(archive), inner: HashMap::new() });
                level.push((tree.nodes.len() - 1, key));
                Inner::Open(tree.nodes.len() - 1)
            }
            Err(why) => why,
        };
        tree.disks.push((disk, at));
    }
    while !level.is_empty() {
        let mut jobs: Vec<(usize, usize, String)> = Vec::new();
        for (node, key) in &level {
            let mut found = Vec::new();
            nested_entries(&tree.nodes[*node].archive.0, 0, key, &mut found);
            jobs.extend(found.into_iter().map(|(entry, key)| (*node, entry, key)));
        }
        let parents: Vec<Arc<Opened>> = tree.nodes.iter().map(|n| n.archive.0.clone()).collect();
        let done = crate::util::parallel::map(&jobs, |(node, entry, key)| {
            let parent = &parents[*node];
            // (an archive read whole that is not one stops the walk; any other failure is skipped)
            let mut owned = false;
            let archive = opened(key, || {
                let bytes = parent.nested_bytes(*entry, keys)?;
                owned = matches!(bytes, Bytes::Owned(_));
                Opened::new(&parent.rpf.entries[*entry].name, bytes, keys)
            });
            (archive, owned)
        });
        level = Vec::new();
        for ((node, entry, key), (archive, owned)) in jobs.into_iter().zip(done) {
            let inner = match archive {
                Ok(archive) => {
                    tree.nodes.push(Node { archive: ArchiveRef(archive), inner: HashMap::new() });
                    level.push((tree.nodes.len() - 1, key));
                    Inner::Open(tree.nodes.len() - 1)
                }
                Err(e) if owned => Inner::Fatal(Some(e)),
                Err(e) => Inner::Unreadable(e),
            };
            tree.nodes[node].inner.insert(entry, inner);
        }
    }
    tree
}

/// The walk of `vfs::walk` over the opened archives; `named` picks the files visited by their
/// name (lowercase), before their path is made.
fn walk_node(
    tree: &mut Tree,
    node: usize,
    dir: usize,
    prefix: &str,
    named: &dyn Fn(&str) -> bool,
    visit: &mut dyn FnMut(&str, &ArchiveRef, usize) -> bool,
) -> Result<bool> {
    let archive = tree.nodes[node].archive.clone();
    let path = |name: &str| if prefix.is_empty() { name.to_string() } else { format!("{prefix}/{name}") };
    for i in archive.0.children(dir) {
        let entry = &archive.0.rpf.entries[i];
        if entry.is_directory() {
            if !walk_node(tree, node, i, &path(&entry.name_lower), named, visit)? {
                return Ok(false);
            }
        } else if entry.name_lower.ends_with(".rpf") {
            let path = path(&entry.name_lower);
            match tree.nodes[node].inner.get_mut(&i) {
                Some(Inner::Open(inner)) => {
                    let inner = *inner;
                    if !walk_node(tree, inner, 0, &path, named, visit)? {
                        return Ok(false);
                    }
                }
                Some(Inner::Unreadable(e)) => eprintln!("  unreadable archive {path}: {e:#}"),
                Some(Inner::Fatal(e)) => {
                    return Err(e.take().unwrap_or_else(|| anyhow::anyhow!("unreadable archive {path}")));
                }
                None => eprintln!("  unreadable archive {path}: not opened"),
            }
        } else if named(&entry.name_lower) && !visit(&path(&entry.name_lower), &archive, i) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Calls `visit` with (full path, archive, entry) for every file of every archive: the same files,
/// in the same order, with the same messages and errors as `index::for_each_file`, but the
/// archives are opened on all the cores and kept ([`open_all`]), so that a second walk costs
/// little, and the archive given can be read from any thread ([`ArchiveRef`]).
pub fn for_each_file(
    game: &Path,
    keys: &GtaKeys,
    visit: &mut dyn FnMut(&str, &ArchiveRef, usize) -> bool,
) -> Result<()> {
    for_each_file_named(game, keys, &|_| true, visit)
}

/// [`for_each_file`] for the files whose name (lowercase, without folders) `named` accepts only:
/// the others are passed over without making their path.
pub fn for_each_file_named(
    game: &Path,
    keys: &GtaKeys,
    named: &dyn Fn(&str) -> bool,
    visit: &mut dyn FnMut(&str, &ArchiveRef, usize) -> bool,
) -> Result<()> {
    let mut tree = open_all(game, keys);
    for d in 0..tree.disks.len() {
        let (disk, at) = &mut tree.disks[d];
        let node = match at {
            Inner::Open(node) => *node,
            // a file that cannot be mapped: the walk stops (`Mapped::open(..)?`)
            Inner::Fatal(e) => return Err(e.take().unwrap_or_else(|| anyhow::anyhow!("unreadable archive"))),
            Inner::Unreadable(e) => {
                eprintln!("  unreadable archive {}: {e:#}", disk.display());
                continue;
            }
        };
        let prefix = crate::game::index::relative(game, &tree.disks[d].0);
        if !walk_node(&mut tree, node, 0, &prefix, named, visit)? {
            break;
        }
    }
    Ok(())
}

/// The bytes of the file at `path` (decrypted, decompressed; a resource keeps its RSC7 header).
pub fn read(game: &Path, path: &str, keys: &GtaKeys) -> Result<Vec<u8>> {
    static CHECK: LazyLock<bool> = LazyLock::new(|| std::env::var("BTMS_CHECK_READS").is_ok_and(|v| v != "0"));
    let bytes = time(Phase::ReadFile, || {
        let (archive, index) = locate(game, path, keys)?;
        time(Phase::Extract, || archive.read(index, keys))
    });
    if *CHECK {
        let before = crate::cli::archive::read_file_uncached(game, path, keys);
        match (&bytes, &before) {
            (Ok(a), Ok(b)) if a == b => {}
            (Err(_), Err(_)) => {}
            _ => panic!(
                "BTMS_CHECK_READS: {path} read differently through the cache ({}) and without it ({})",
                bytes.as_ref().map(|b| format!("{} bytes", b.len())).unwrap_or_else(|e| format!("error: {e:#}")),
                before.as_ref().map(|b| format!("{} bytes", b.len())).unwrap_or_else(|e| format!("error: {e:#}")),
            ),
        }
    }
    bytes
}
