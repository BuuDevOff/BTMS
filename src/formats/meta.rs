//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! RAGE "Meta" resources (ymap, ytyp): data blocks, each an array of one structure type, and the
//! description of those structures carried by the file itself (`StructureInfos`: size, and for
//! every field its name hash and offset).
//!
//! Fields are read by name through the file's own description, never at offsets assumed here: a
//! layout this code does not expect is an error ("field … missing"), not garbage. The header layout
//! is CodeWalker's `Meta` (0x70 bytes), the block pointers its `MetaPOINTER` / `Array_Structure`
//! (bits 0-11: block number from 1, bits 12-31: byte offset in the block).

use std::collections::HashMap;

use anyhow::{Context, Result, bail};

use crate::formats::resource::{Resource, meta_hash};

pub struct Block {
    /// Hash of the name of the structure the block holds (or of a basic type: pointers, hashes…).
    pub name: u32,
    pub data: Vec<u8>,
}

pub struct Field {
    pub offset: usize,
}

pub struct Struct {
    pub size: usize,
    pub fields: HashMap<u32, Field>,
}

pub struct Meta {
    pub blocks: Vec<Block>,
    pub structs: HashMap<u32, Struct>,
    /// Index (from 0) of the root block.
    pub root: usize,
}

/// A structure read in a block: its type, its bytes.
#[derive(Clone, Copy)]
pub struct Item<'a> {
    pub name: u32,
    pub bytes: &'a [u8],
    pub info: &'a Struct,
}

impl Meta {
    pub fn parse(res: &Resource) -> Result<Meta> {
        const BASE: u64 = 0x5000_0000;
        let root = res.u32(BASE + 0x1C)? as usize;
        let (structs_ptr, blocks_ptr) = (res.u64(BASE + 0x20)?, res.u64(BASE + 0x30)?);
        let (structs_count, blocks_count) = (res.u16(BASE + 0x48)? as u64, res.u16(BASE + 0x4C)? as u64);
        if root == 0 || root as u64 > blocks_count {
            bail!("root block {root} outside the {blocks_count} blocks");
        }
        let mut structs = HashMap::new();
        for i in 0..structs_count {
            let p = structs_ptr + i * 0x20;
            let name = res.u32(p)?;
            let (entries, size, count) = (res.u64(p + 0x10)?, res.u32(p + 0x18)? as usize, res.u16(p + 0x1E)? as u64);
            let mut fields = HashMap::new();
            for j in 0..count {
                let e = entries + j * 0x10;
                let (field, offset) = (res.u32(e)?, res.u32(e + 4)? as usize);
                // (the field's type is not used, but read all the same: an entry cut short is refused)
                res.u8(e + 8)?;
                if offset > size {
                    bail!("structure 0x{name:08X}: field 0x{field:08X} at {offset}, outside its {size} bytes");
                }
                fields.insert(field, Field { offset });
            }
            structs.insert(name, Struct { size, fields });
        }
        let mut blocks = Vec::with_capacity(blocks_count as usize);
        for i in 0..blocks_count {
            let p = blocks_ptr + i * 0x10;
            let (name, len, data) = (res.u32(p)?, res.u32(p + 4)? as usize, res.u64(p + 8)?);
            blocks.push(Block { name, data: res.bytes(data, len)?.to_vec() });
        }
        Ok(Meta { blocks, structs, root: root - 1 })
    }

    /// The root structure (`CMapData` of a ymap, `CMapTypes` of a ytyp), checked to be `name`.
    pub fn root(&self, name: &str) -> Result<Item<'_>> {
        let block = &self.blocks[self.root];
        if block.name != meta_hash(name) {
            bail!("root 0x{:08X}, {name} expected", block.name);
        }
        self.item(self.root, 0)
    }

    /// The structure at `offset` in block `index`.
    pub fn item(&self, index: usize, offset: usize) -> Result<Item<'_>> {
        let block = self.blocks.get(index).with_context(|| format!("block {} missing", index + 1))?;
        let info = self
            .structs
            .get(&block.name)
            .with_context(|| format!("block {}: structure 0x{:08X} not described", index + 1, block.name))?;
        let bytes = block
            .data
            .get(offset..offset + info.size)
            .with_context(|| format!("block {}: structure outside the block", index + 1))?;
        Ok(Item { name: block.name, bytes, info })
    }

    /// The items of an `Array_StructurePointer` field: each element of the array is a pointer to a
    /// structure (`CEntityDef`, `CMloInstanceDef`, `CBaseArchetypeDef`…).
    pub fn pointer_array(&self, array: &[u8]) -> Result<Vec<Item<'_>>> {
        let (ptr, count) = (
            u32::from_le_bytes(array[0..4].try_into().unwrap()),
            u16::from_le_bytes(array[8..10].try_into().unwrap()) as usize,
        );
        if count == 0 {
            return Ok(Vec::new());
        }
        let (index, offset) = split(ptr)?;
        let block = self.blocks.get(index).context("pointer array: block missing")?;
        let mut items = Vec::with_capacity(count);
        for i in 0..count {
            let at = offset + i * 8;
            let p = block.data.get(at..at + 4).context("pointer array: outside the block")?;
            let (index, offset) = split(u32::from_le_bytes(p.try_into().unwrap()))?;
            items.push(self.item(index, offset)?);
        }
        Ok(items)
    }
}

impl<'a> Item<'a> {
    fn field(&self, name: &str) -> Result<&'a Field> {
        self.info
            .fields
            .get(&meta_hash(name))
            .with_context(|| format!("field {name} missing from structure 0x{:08X}", self.name))
    }

    fn at(&self, name: &str, len: usize) -> Result<&'a [u8]> {
        let f = self.field(name)?;
        self.bytes.get(f.offset..f.offset + len).with_context(|| format!("field {name} outside its structure"))
    }

    pub fn u32(&self, name: &str) -> Result<u32> {
        Ok(u32::from_le_bytes(self.at(name, 4)?.try_into().unwrap()))
    }

    /// A field that older versions of the structure do not have.
    pub fn u32_opt(&self, name: &str) -> Result<Option<u32>> {
        if self.info.fields.contains_key(&meta_hash(name)) { self.u32(name).map(Some) } else { Ok(None) }
    }

    pub fn i32(&self, name: &str) -> Result<i32> {
        Ok(self.u32(name)? as i32)
    }

    pub fn f32(&self, name: &str) -> Result<f32> {
        Ok(f32::from_le_bytes(self.at(name, 4)?.try_into().unwrap()))
    }

    pub fn vec3(&self, name: &str) -> Result<[f32; 3]> {
        let b = self.at(name, 12)?;
        Ok([0, 4, 8].map(|o| f32::from_le_bytes(b[o..o + 4].try_into().unwrap())))
    }

    pub fn vec4(&self, name: &str) -> Result<[f32; 4]> {
        let b = self.at(name, 16)?;
        Ok([0, 4, 8, 12].map(|o| f32::from_le_bytes(b[o..o + 4].try_into().unwrap())))
    }

    /// The 16 bytes of an array field (`Array_Structure`, `Array_StructurePointer`…).
    pub fn array(&self, name: &str) -> Result<&'a [u8]> {
        self.at(name, 16)
    }

    pub fn is(&self, name: &str) -> bool {
        self.name == meta_hash(name)
    }
}

/// A `MetaPOINTER`: (block index from 0, byte offset).
fn split(ptr: u32) -> Result<(usize, usize)> {
    let block = (ptr & 0xFFF) as usize;
    if block == 0 {
        bail!("null pointer");
    }
    Ok((block - 1, ((ptr >> 12) & 0xF_FFFF) as usize))
}
