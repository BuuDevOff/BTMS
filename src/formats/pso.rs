//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! RAGE PSO files (`PSIN` …: `_manifest.ymf`, some `.ymt`, the PSO form of a few ymaps): a
//! sequence of sections, each `<4-byte name><big-endian u32 length>` then its bytes:
//!
//! - `PSIN`: the data, offsets counted from the start of this section;
//! - `PMAP`: the blocks of the data (root block number, then per block: name hash of the structure
//!   it holds (or a small number for plain values: 6 for 32-bit ones), offset, length);
//! - `PSCH`: the description of the structures (size, and per field: name hash, type, subtype,
//!   offset, reference) and of the enums (per value: name hash, number);
//! - `PSIG`, `STRF`, `STRS`, `STRE` (the names, encrypted: not read), `CHKS`: not needed here.
//!
//! Everything is big-endian. As for Meta files ([`crate::formats::meta`]), fields are read by name through
//! the file's own description, and by the type that description declares: a field of another type
//! or width than the one asked for is an error, not a guess.
//!
//! Arrays (`atArray`, 16 bytes: pointer, 4 unused bytes, count, capacity) point into a block like a
//! `MetaPOINTER`: bits 0-11 the block number from 1, bits 12-31 the byte offset in that block. The
//! elements of an array are described by another entry of the same structure, named
//! [`ARRAY_INFO`], whose index is the low half of the array entry's reference. Unlike in a Meta
//! resource, a block has no size limit: an array lies whole in one block (several arrays may share
//! a block, each at its offset), so elements that would run past the block are an error.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};

use crate::formats::resource::meta_hash;

/// Name of the entries that describe the elements of an array, or the enum of a flags field.
pub const ARRAY_INFO: u32 = 0x100;

// field types (`Field::kind`)
pub const BOOL: u8 = 0x00;
pub const SBYTE: u8 = 0x01;
pub const UBYTE: u8 = 0x02;
pub const SSHORT: u8 = 0x03;
pub const USHORT: u8 = 0x04;
pub const SINT: u8 = 0x05;
pub const UINT: u8 = 0x06;
/// Subtype 7 or 8: the 32-bit hash of the text.
pub const STRING: u8 = 0x0B;
/// Subtype 0: the structure itself (reference: its name hash).
pub const STRUCT: u8 = 0x0C;
/// Subtype 0: an `atArray` (16 bytes).
pub const ARRAY: u8 = 0x0D;
/// Subtype 0: 32 bits, 1: 16 bits, 2: 8 bits.
pub const ENUM: u8 = 0x0E;
/// Subtype 0: 32 bits, 1: 16 bits, 2: 8 bits.
pub const FLAGS: u8 = 0x0F;

pub struct Field {
    pub name: u32,
    pub kind: u8,
    pub sub: u8,
    pub offset: usize,
    /// A structure: its type. An array: the index of the entry describing its elements (low 16
    /// bits). Flags: the index of the entry naming their enum (low 16 bits).
    pub reference: u32,
}

pub struct Struct {
    pub size: usize,
    /// In file order: arrays refer to another entry by index.
    pub fields: Vec<Field>,
}

impl Struct {
    /// The field `name` (never an [`ARRAY_INFO`] entry).
    pub fn field(&self, name: u32) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name && name != ARRAY_INFO)
    }
}

pub struct Block {
    pub name: u32,
    pub offset: usize,
    pub len: usize,
}

pub struct Pso {
    /// The `PSIN` section, header included (block offsets count from its start).
    pub data: Vec<u8>,
    pub blocks: Vec<Block>,
    /// Index (from 0) of the root block.
    pub root: usize,
    pub structs: HashMap<u32, Struct>,
}

fn be32(b: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(b.get(at..at + 4).context("truncated PSO")?.try_into().unwrap()))
}

fn be16(b: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_be_bytes(b.get(at..at + 2).context("truncated PSO")?.try_into().unwrap()))
}

/// Where the elements of an array are: block (from 0), byte offset in it, count.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Span {
    pub block: usize,
    pub offset: usize,
    pub count: usize,
}

impl Pso {
    pub fn is_pso(bytes: &[u8]) -> bool {
        bytes.starts_with(b"PSIN")
    }

    pub fn parse(bytes: &[u8]) -> Result<Pso> {
        if !Self::is_pso(bytes) {
            bail!("not a PSO file (no PSIN section at the start)");
        }
        let (mut data, mut blocks, mut root) = (None, Vec::new(), 0usize);
        let mut structs = HashMap::new();
        let mut pos = 0;
        while pos + 8 <= bytes.len() {
            let ident = &bytes[pos..pos + 4];
            let len = be32(bytes, pos + 4)? as usize;
            if len < 8 || pos + len > bytes.len() {
                bail!("PSO section {} of invalid length {len}", String::from_utf8_lossy(ident));
            }
            let s = &bytes[pos..pos + len];
            match ident {
                b"PSIN" => data = Some(s.to_vec()),
                b"PMAP" => {
                    root = be32(s, 8)? as usize;
                    let count = be16(s, 12)? as usize;
                    for i in 0..count {
                        let e = 16 + i * 16;
                        blocks.push(Block {
                            name: be32(s, e)?,
                            offset: be32(s, e + 4)? as usize,
                            len: be32(s, e + 12)? as usize,
                        });
                    }
                }
                b"PSCH" => {
                    let count = be32(s, 8)? as usize;
                    for i in 0..count {
                        let (name, at) = (be32(s, 12 + i * 8)?, be32(s, 16 + i * 8)? as usize);
                        let head = be32(s, at)?;
                        match head >> 24 {
                            // a structure: entry count, size, then 12 bytes per entry
                            0 => {
                                let entries = (head & 0xFFFF) as usize;
                                let size = be32(s, at + 4)? as usize;
                                let mut fields = Vec::with_capacity(entries);
                                for j in 0..entries {
                                    let e = at + 12 + j * 12;
                                    let field = be32(s, e)?;
                                    let (kind, sub) = (
                                        *s.get(e + 4).context("truncated PSO")?,
                                        *s.get(e + 5).context("truncated PSO")?,
                                    );
                                    let offset = be16(s, e + 6)? as usize;
                                    if offset > size {
                                        bail!(
                                            "PSO structure 0x{name:08X}: field 0x{field:08X} at {offset}, outside its {size} bytes"
                                        );
                                    }
                                    fields.push(Field { name: field, kind, sub, offset, reference: be32(s, e + 8)? });
                                }
                                structs.insert(name, Struct { size, fields });
                            }
                            // an enum: value count, then 8 bytes per value (name hash, number). Not
                            // used, but read all the same: an enum cut short is refused
                            1 => {
                                let entries = (head & 0xFF_FFFF) as usize;
                                for j in 0..entries {
                                    be32(s, at + 4 + j * 8)?;
                                    be32(s, at + 8 + j * 8)?;
                                }
                            }
                            other => bail!("PSO: description 0x{name:08X} of unknown type {other}"),
                        }
                    }
                }
                _ => {}
            }
            pos += len;
        }
        let data = data.context("PSO without a PSIN section")?;
        if root == 0 || root > blocks.len() {
            bail!("PSO: root block {root} of {}", blocks.len());
        }
        for b in &blocks {
            if b.offset + b.len > data.len() {
                bail!("PSO: block 0x{:08X} outside the data", b.name);
            }
        }
        Ok(Pso { data, blocks, root: root - 1, structs })
    }

    /// The root structure, checked to be `name`.
    pub fn root(&self, name: &str) -> Result<Item<'_>> {
        let b = &self.blocks[self.root];
        if b.name != meta_hash(name) {
            bail!("PSO root 0x{:08X}, {name} expected", b.name);
        }
        self.item(self.root, 0)
    }

    /// The structure at `offset` in block `index`.
    pub fn item(&self, index: usize, offset: usize) -> Result<Item<'_>> {
        let b = self.blocks.get(index).with_context(|| format!("PSO block {} missing", index + 1))?;
        let info =
            self.structs.get(&b.name).with_context(|| format!("PSO structure 0x{:08X} not described", b.name))?;
        let start = b.offset + offset;
        if offset + info.size > b.len {
            bail!("PSO structure outside its block");
        }
        Ok(Item { pso: self, name: b.name, bytes: &self.data[start..start + info.size], info })
    }

    /// Where the 16 bytes of an `atArray` point (None when it is empty).
    fn span(&self, array: &[u8]) -> Result<Option<Span>> {
        let ptr = be32(array, 0)?;
        let count = be16(array, 8)? as usize;
        if count == 0 {
            return Ok(None);
        }
        let (block, offset) = ((ptr & 0xFFF) as usize, ((ptr >> 12) & 0xF_FFFF) as usize);
        if block == 0 || block > self.blocks.len() {
            bail!("PSO array of {count} elements: block {block} of {}", self.blocks.len());
        }
        Ok(Some(Span { block: block - 1, offset, count }))
    }

    /// The `count` elements of `size` bytes of a span: all inside its block.
    fn elements(&self, span: Span, size: usize) -> Result<Vec<&[u8]>> {
        let b = &self.blocks[span.block];
        if span.offset + span.count * size > b.len {
            bail!(
                "PSO array: {} elements of {size} bytes at {} in block {} (0x{:08X}) of {} bytes",
                span.count,
                span.offset,
                span.block + 1,
                b.name,
                b.len
            );
        }
        let start = b.offset + span.offset;
        Ok((0..span.count).map(|i| &self.data[start + i * size..start + (i + 1) * size]).collect())
    }
}

/// A structure read in a PSO block.
#[derive(Clone, Copy)]
pub struct Item<'a> {
    pso: &'a Pso,
    pub name: u32,
    pub bytes: &'a [u8],
    pub info: &'a Struct,
}

impl<'a> Item<'a> {
    fn field(&self, name: &str) -> Result<&'a Field> {
        self.info.field(meta_hash(name)).with_context(|| format!("PSO field {name} missing from 0x{:08X}", self.name))
    }

    /// A 32-bit field: a number, the hash of a name, 32-bit flags.
    pub fn u32(&self, name: &str) -> Result<u32> {
        let f = self.field(name)?;
        match (f.kind, f.sub) {
            (SINT | UINT, _) | (STRING, 7 | 8) | (ENUM | FLAGS, 0) => be32(self.bytes, f.offset),
            _ => bail!("PSO field {name} of type {:02X}/{:02X}: 32 bits expected", f.kind, f.sub),
        }
    }

    /// A whole number or a set of flags, read by the width its description declares (8, 16 or 32
    /// bits).
    pub fn bits(&self, name: &str) -> Result<u32> {
        let f = self.field(name)?;
        match (f.kind, f.sub) {
            (SINT | UINT, _) | (ENUM | FLAGS, 0) => be32(self.bytes, f.offset),
            (SSHORT | USHORT, _) | (ENUM | FLAGS, 1) => Ok(be16(self.bytes, f.offset)? as u32),
            (BOOL | SBYTE | UBYTE, _) | (ENUM | FLAGS, 2) => {
                Ok(*self.bytes.get(f.offset).context("truncated PSO")? as u32)
            }
            _ => bail!("PSO field {name} of type {:02X}/{:02X}: a number or flags expected", f.kind, f.sub),
        }
    }

    /// An `atArray` field: (the entry describing its elements, where they are).
    fn array(&self, name: &str) -> Result<(&'a Field, Option<Span>)> {
        let f = self.field(name)?;
        if (f.kind, f.sub) != (ARRAY, 0) {
            bail!("PSO field {name} of type {:02X}/{:02X}: an array expected", f.kind, f.sub);
        }
        let element = self
            .info
            .fields
            .get((f.reference & 0xFFFF) as usize)
            .filter(|e| e.name == ARRAY_INFO)
            .with_context(|| format!("PSO array {name}: elements not described"))?;
        let array = self.bytes.get(f.offset..f.offset + 16).context("PSO array outside its structure")?;
        Ok((element, self.pso.span(array)?))
    }

    /// Where the elements of an array field are (None when it is empty).
    pub fn span(&self, name: &str) -> Result<Option<Span>> {
        Ok(self.array(name)?.1)
    }

    /// The structures of an array field. The block they lie in must be of the type the array
    /// declares.
    pub fn structs(&self, name: &str) -> Result<Vec<Item<'a>>> {
        let (element, span) = self.array(name)?;
        if (element.kind, element.sub) != (STRUCT, 0) {
            bail!("PSO array {name}: elements of type {:02X}/{:02X}, structures expected", element.kind, element.sub);
        }
        let Some(span) = span else { return Ok(Vec::new()) };
        let kind = element.reference;
        let block = &self.pso.blocks[span.block];
        if block.name != kind {
            bail!("PSO array {name}: block of 0x{:08X}, 0x{kind:08X} expected", block.name);
        }
        let info = self
            .pso
            .structs
            .get(&kind)
            .with_context(|| format!("PSO array {name}: structure 0x{kind:08X} not described"))?;
        let pso = self.pso;
        Ok(pso.elements(span, info.size)?.into_iter().map(|bytes| Item { pso, name: kind, bytes, info }).collect())
    }

    /// The 32-bit values (hashes of names, numbers) of an array field.
    pub fn u32s(&self, name: &str) -> Result<Vec<u32>> {
        let (element, span) = self.array(name)?;
        if !matches!((element.kind, element.sub), (SINT | UINT, _) | (STRING, 7 | 8) | (ENUM | FLAGS, 0)) {
            bail!(
                "PSO array {name}: elements of type {:02X}/{:02X}, 32-bit values expected",
                element.kind,
                element.sub
            );
        }
        let Some(span) = span else { return Ok(Vec::new()) };
        self.pso.elements(span, 4)?.into_iter().map(|b| be32(b, 0)).collect()
    }
}

/// `Flags` of a map group: shown at some hours only ([`MapGroup::hours`]).
pub const GROUP_TIME_DEPENDENT: u32 = 1;
/// `Flags` of a map group: shown in some weathers only ([`MapGroup::weathers`]).
pub const GROUP_WEATHER_DEPENDENT: u32 = 2;

/// A map group of a `_manifest.ymf` (`CMapDataGroup`).
#[derive(Clone, Debug)]
pub struct MapGroup {
    /// Hash of its name: the name of a placement file.
    pub name: u32,
    /// Hashes of the names of the files that go with it (see [`crate::world::groups`] for what they are).
    pub bounds: Vec<u32>,
    pub flags: u32,
    pub weathers: Vec<u32>,
    /// One bit per hour of the day (bit 0: midnight to 1 a.m.) when [`GROUP_TIME_DEPENDENT`].
    pub hours: u32,
}

/// What a `_manifest.ymf` (`CPackFileMetaData`) says about the placement files of its archive.
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub groups: Vec<MapGroup>,
    /// `imapDependencies_2`: (placement file, its flags, the archetype files it needs).
    pub imap_dependencies: Vec<(u32, u32, Vec<u32>)>,
    /// `Interiors`: (interior, its collision files).
    pub interiors: Vec<(u32, Vec<u32>)>,
    /// How many texture dictionaries get a high definition one (`HDTxdBindingArray`).
    pub hd_bindings: usize,
    /// For each array read: does it fill its block exactly, from its start?
    pub arrays: Vec<bool>,
}

/// Reads a `_manifest.ymf`.
pub fn manifest(bytes: &[u8]) -> Result<Manifest> {
    let pso = Pso::parse(bytes)?;
    let root = pso.root("CPackFileMetaData")?;
    let mut out = Manifest::default();
    let mut note = |item: &Item, field: &str, size: usize| -> Result<()> {
        if let Some(s) = item.span(field)? {
            out.arrays.push(s.offset == 0 && s.count * size == pso.blocks[s.block].len);
        }
        Ok(())
    };
    let size_of = |name: &str| pso.structs.get(&meta_hash(name)).map(|s| s.size).unwrap_or(0);
    note(&root, "MapDataGroups", size_of("CMapDataGroup"))?;
    for g in root.structs("MapDataGroups")? {
        note(&g, "Bounds", 4)?;
        note(&g, "WeatherTypes", 4)?;
        out.groups.push(MapGroup {
            name: g.u32("Name")?,
            bounds: g.u32s("Bounds")?,
            flags: g.bits("Flags")?,
            weathers: g.u32s("WeatherTypes")?,
            hours: g.u32("HoursOnOff")?,
        });
    }
    note(&root, "imapDependencies_2", size_of("CImapDependencies"))?;
    for d in root.structs("imapDependencies_2")? {
        note(&d, "itypDepArray", 4)?;
        out.imap_dependencies.push((d.u32("imapName")?, d.bits("manifestFlags")?, d.u32s("itypDepArray")?));
    }
    note(&root, "Interiors", size_of("CInteriorBoundsFiles"))?;
    for i in root.structs("Interiors")? {
        note(&i, "Bounds", 4)?;
        out.interiors.push((i.u32("Name")?, i.u32s("Bounds")?));
    }
    out.hd_bindings = root.span("HDTxdBindingArray")?.map(|s| s.count).unwrap_or(0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be(v: u32) -> [u8; 4] {
        v.to_be_bytes()
    }

    fn section(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
        [&name[..], &be(body.len() as u32 + 8), body].concat()
    }

    fn entry(name: u32, kind: u8, sub: u8, offset: u16, reference: u32) -> Vec<u8> {
        [&be(name)[..], &[kind, sub], &offset.to_be_bytes(), &be(reference)].concat()
    }

    fn array(block: u32, offset: u32, count: u16) -> Vec<u8> {
        [&be(block | offset << 12)[..], &[0; 4], &count.to_be_bytes(), &count.to_be_bytes(), &[0; 4]].concat()
    }

    /// A manifest with two map groups, laid out as the game's are: the groups in a block of their
    /// own, the hashes of both `Bounds` arrays sharing one block of plain 32-bit values.
    fn sample(second_bounds: u16) -> Vec<u8> {
        let (root, group) = (meta_hash("CPackFileMetaData"), meta_hash("CMapDataGroup"));
        // data, after the 8 bytes of the section header and 8 of padding
        let mut data = vec![0x70; 8];
        // block 1 at 16: two groups of 56 bytes
        for (name, bounds, flags, hours) in
            [(0xAAAA_0001u32, array(2, 0, 2), 0u16, 0u32), (0xAAAA_0002, array(2, 8, second_bounds), 1, 0x00FF_F03F)]
        {
            data.extend(be(name));
            data.extend([0; 4]);
            data.extend(bounds);
            // 16-bit flags followed by bytes that are not theirs
            data.extend(flags.to_be_bytes());
            data.extend([0xEE; 6]);
            data.extend(array(0, 0, 0));
            data.extend(be(hours));
            data.extend([0; 4]);
        }
        // block 2 at 128: three hashes
        for h in [0xB000_0001u32, 0xB000_0002, 0xB000_0003] {
            data.extend(be(h));
        }
        data.extend([0x70; 4]);
        // block 3 at 144: the root, 96 bytes, only its first array filled
        data.extend(array(1, 0, 2));
        data.extend([0; 80]);
        let psin = section(b"PSIN", &data);

        let mut pmap = [&be(3)[..], &3u16.to_be_bytes(), &[0x70, 0x70]].concat();
        for (name, offset, len) in [(group, 16u32, 112u32), (6, 128, 12), (root, 144, 96)] {
            pmap.extend([be(name), be(offset), be(0), be(len)].concat());
        }

        let mut root_info = [be(12), be(96), be(0)].concat();
        for (i, (field, kind)) in [
            ("MapDataGroups", "CMapDataGroup"),
            ("HDTxdBindingArray", "CHDTxdAssetBinding"),
            ("imapDependencies", "CImapDependency"),
            ("imapDependencies_2", "CImapDependencies"),
            ("itypDependencies_2", "CItypDependencies"),
            ("Interiors", "CInteriorBoundsFiles"),
        ]
        .into_iter()
        .enumerate()
        {
            root_info.extend(entry(ARRAY_INFO, STRUCT, 0, 0, meta_hash(kind)));
            root_info.extend(entry(meta_hash(field), ARRAY, 0, i as u16 * 16, i as u32 * 2));
        }
        let group_info = [
            &[be(8), be(56), be(0)].concat()[..],
            &entry(meta_hash("Name"), STRING, 7, 0, 0),
            &entry(ARRAY_INFO, STRING, 7, 0, 0),
            &entry(meta_hash("Bounds"), ARRAY, 0, 8, 1),
            &entry(ARRAY_INFO, ENUM, 0, 0, 0x1234_5678),
            &entry(meta_hash("Flags"), FLAGS, 1, 24, 0x0020_0003),
            &entry(ARRAY_INFO, STRING, 7, 0, 0),
            &entry(meta_hash("WeatherTypes"), ARRAY, 0, 32, 5),
            &entry(meta_hash("HoursOnOff"), UINT, 0, 48, 0),
        ]
        .concat();
        let enum_info =
            [be(0x0100_0002), be(meta_hash("TIME_DEPENDENT")), be(0), be(meta_hash("WEATHER_DEPENDENT")), be(1)]
                .concat();
        // index (name, offset from the section start), then the descriptions
        let first = 12 + 3 * 8;
        let offsets = [first, first + root_info.len(), first + root_info.len() + group_info.len()];
        let mut psch = be(3).to_vec();
        for (name, at) in [(root, offsets[0]), (group, offsets[1]), (0x1234_5678, offsets[2])] {
            psch.extend([be(name), be(at as u32)].concat());
        }
        psch.extend([root_info, group_info, enum_info].concat());

        [psin, section(b"PMAP", &pmap), section(b"PSCH", &psch), section(b"STRE", &[1, 2, 3, 4])].concat()
    }

    #[test]
    fn manifest_groups() {
        let m = manifest(&sample(1)).unwrap();
        assert_eq!(m.groups.len(), 2);
        assert_eq!((m.groups[0].name, m.groups[0].flags, m.groups[0].hours), (0xAAAA_0001, 0, 0));
        assert_eq!(m.groups[0].bounds, [0xB000_0001, 0xB000_0002]);
        // the 16-bit flags alone, not the bytes after them; the second array starts at its offset
        assert_eq!(
            (m.groups[1].name, m.groups[1].flags, m.groups[1].hours),
            (0xAAAA_0002, GROUP_TIME_DEPENDENT, 0x00FF_F03F)
        );
        assert_eq!(m.groups[1].bounds, [0xB000_0003]);
        assert!(m.groups[1].weathers.is_empty());
        // the groups fill their block; neither Bounds array fills the shared block of hashes
        assert_eq!(m.arrays, [true, false, false]);
        assert!(m.imap_dependencies.is_empty() && m.interiors.is_empty() && m.hd_bindings == 0);
    }

    #[test]
    fn array_past_its_block_is_an_error() {
        // two hashes asked for where the block holds one more: nothing is read from the next block
        let e = manifest(&sample(2)).unwrap_err();
        assert!(format!("{e:#}").contains("PSO array"), "{e:#}");
    }

    #[test]
    fn wrong_type_is_an_error() {
        let bytes = sample(1);
        let pso = Pso::parse(&bytes).unwrap();
        let group = pso.item(0, 56).unwrap();
        assert_eq!(group.bits("Flags").unwrap(), 1);
        // 16-bit flags are not a 32-bit value, a hash is not an array, an array is not a number
        assert!(group.u32("Flags").is_err());
        assert!(group.u32s("Name").is_err());
        assert!(group.bits("Bounds").is_err());
        assert!(group.structs("Bounds").is_err());
        assert!(pso.root("CMapData").is_err());
        assert!(!Pso::is_pso(b"RSC7") && Pso::parse(b"RSC7\0\0\0\0").is_err());
    }
}
