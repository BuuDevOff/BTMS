//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! Interiors (`CMloArchetypeDef` of a `.ytyp`): the entities an interior is made of, placed
//! relative to it. Its rooms, its portals (the openings between rooms, and between a room and the
//! outside) and its entity sets are not used, but read all the same: an interior whose ones do not
//! read is refused.
//!
//! Read like the rest of a `.ytyp`, by field name through the file's own description
//! ([`crate::formats::meta`]).

use anyhow::{Context, Result};

use crate::formats::meta::{Item, Meta};
use crate::formats::resource::Resource;
use crate::formats::ymap::Entity;

pub struct Mlo {
    /// Hash of the archetype's name.
    pub name: u32,
    /// The entities, placed relative to the interior.
    pub entities: Vec<Entity>,
}

/// A `MetaPOINTER` / array pointer: (block index from 0, byte offset).
fn split(ptr: u32) -> Option<(usize, usize)> {
    let block = (ptr & 0xFFF) as usize;
    (block != 0).then(|| (block - 1, ((ptr >> 12) & 0xF_FFFF) as usize))
}

/// (pointer, count) of a 16-byte array field.
fn array(bytes: &[u8]) -> (u32, usize) {
    (u32::from_le_bytes(bytes[0..4].try_into().unwrap()), u16::from_le_bytes(bytes[8..10].try_into().unwrap()) as usize)
}

/// The structures of an `Array_Structure` field: stored one after the other in a block.
fn structs<'a>(meta: &'a Meta, bytes: &[u8]) -> Result<Vec<Item<'a>>> {
    let (ptr, count) = array(bytes);
    if count == 0 {
        return Ok(Vec::new());
    }
    let (index, offset) = split(ptr).context("structure array: null pointer")?;
    let block = meta.blocks.get(index).context("structure array: block missing")?;
    let size = meta.structs.get(&block.name).context("structure array: structure not described")?.size;
    (0..count).map(|i| meta.item(index, offset + i * size)).collect()
}

/// The numbers of an `Array_uint` field.
fn u32s(meta: &Meta, bytes: &[u8]) -> Result<Vec<u32>> {
    let (ptr, count) = array(bytes);
    if count == 0 {
        return Ok(Vec::new());
    }
    let (index, offset) = split(ptr).context("number array: null pointer")?;
    let data = &meta.blocks.get(index).context("number array: block missing")?.data;
    let raw = data.get(offset..offset + count * 4).context("number array: outside the block")?;
    Ok(raw.as_chunks::<4>().0.iter().map(|b| u32::from_le_bytes(*b)).collect())
}

fn entity(e: &Item) -> Result<Entity> {
    // (not used, but read all the same: an entity without them is refused)
    e.i32("parentIndex")?;
    e.f32("lodDist")?;
    e.f32("childLodDist")?;
    e.u32("flags")?;
    Ok(Entity {
        archetype: e.u32("archetypeName")?,
        position: e.vec3("position")?,
        rotation: e.vec4("rotation")?,
        scale_xy: e.f32("scaleXY")?,
        scale_z: e.f32("scaleZ")?,
        tint: e.u32("tintValue")?,
        lod_level: e.u32("lodLevel")?,
        interior: !e.is("CEntityDef"),
    })
}

/// The interiors a `.ytyp` defines.
pub fn parse_mlos(res: &Resource) -> Result<Vec<Mlo>> {
    let meta = Meta::parse(res)?;
    let types = meta.root("CMapTypes")?;
    let mut out = Vec::new();
    for a in meta.pointer_array(types.array("archetypes")?)? {
        if !a.is("CMloArchetypeDef") {
            continue;
        }
        let entities = meta.pointer_array(a.array("entities")?)?.iter().map(entity).collect::<Result<Vec<_>>>()?;
        // the rooms (their name, box, flags, and which entities each holds), the portals and the
        // entity sets: read as checks only (see above)
        for r in structs(&meta, a.array("rooms")?)? {
            r.array("name")?;
            r.vec3("bbMin")?;
            r.vec3("bbMax")?;
            r.u32("flags")?;
            u32s(&meta, r.array("attachedObjects")?)?;
        }
        for p in structs(&meta, a.array("portals")?)? {
            p.u32("roomFrom")?;
            p.u32("roomTo")?;
            p.u32("flags")?;
            u32s(&meta, p.array("attachedObjects")?)?;
        }
        for s in structs(&meta, a.array("entitySets")?)? {
            s.u32("name")?;
            for e in meta.pointer_array(s.array("entities")?)? {
                entity(&e)?;
            }
        }
        out.push(Mlo { name: a.u32("name")?, entities });
    }
    Ok(out)
}
