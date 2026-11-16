//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! What a map is made of: the placements (`.ymap`, `CMapData`: entities placed in the world) and
//! the archetypes they name (`.ytyp`, `CMapTypes`: which model and which textures). Read by field
//! name through the file's own description ([`crate::formats::meta`]).

use anyhow::{Result, bail};

use crate::formats::meta::Meta;
use crate::formats::resource::Resource;

/// `lodLevel` of an entity (`rage__eLodType`).
pub const LOD_NAMES: [&str; 7] = ["HD", "LOD", "SLOD1", "SLOD2", "SLOD3", "ORPHANHD", "SLOD4"];

#[derive(Clone, Debug)]
pub struct Entity {
    pub archetype: u32,
    pub position: [f32; 3],
    /// Quaternion (x, y, z, w) as stored (CodeWalker inverts it for a world entity; see the drawing).
    pub rotation: [f32; 4],
    pub scale_xy: f32,
    pub scale_z: f32,
    /// Row of the tint palette of its model's tinted shaders.
    pub tint: u32,
    pub lod_level: u32,
    /// An interior placement (`CMloInstanceDef`), not a plain `CEntityDef`.
    pub interior: bool,
}

pub struct Ymap {
    pub parent: u32,
    pub flags: u32,
    pub entities_min: [f32; 3],
    pub entities_max: [f32; 3],
    pub entities: Vec<Entity>,
}

pub fn parse_ymap(res: &Resource) -> Result<Ymap> {
    if res.version != 2 {
        bail!("ymap of version {} (2 expected)", res.version);
    }
    let meta = Meta::parse(res)?;
    let map = meta.root("CMapData")?;
    let mut entities = Vec::new();
    for e in meta.pointer_array(map.array("entities")?)? {
        let interior = e.is("CMloInstanceDef");
        if !interior && !e.is("CEntityDef") {
            bail!("entity of type 0x{:08X}", e.name);
        }
        // (not used, but read all the same: an entity without them is refused)
        e.i32("parentIndex")?;
        e.f32("lodDist")?;
        e.f32("childLodDist")?;
        e.u32("flags")?;
        entities.push(Entity {
            archetype: e.u32("archetypeName")?,
            position: e.vec3("position")?,
            rotation: e.vec4("rotation")?,
            scale_xy: e.f32("scaleXY")?,
            scale_z: e.f32("scaleZ")?,
            tint: e.u32("tintValue")?,
            lod_level: e.u32("lodLevel")?,
            interior,
        });
    }
    // (not used, but read all the same: a file without them is refused)
    map.u32("name")?;
    map.u32("contentFlags")?;
    map.vec3("streamingExtentsMin")?;
    map.vec3("streamingExtentsMax")?;
    Ok(Ymap {
        parent: map.u32("parent")?,
        flags: map.u32("flags")?,
        entities_min: map.vec3("entitiesExtentsMin")?,
        entities_max: map.vec3("entitiesExtentsMax")?,
        entities,
    })
}

/// `v` turned by the quaternion `q` (x, y, z, w), or by its inverse.
pub fn rotate(q: [f32; 4], v: [f32; 3], inverse: bool) -> [f32; 3] {
    let s = if inverse { -1.0 } else { 1.0 };
    let (u, w) = ([q[0] * s, q[1] * s, q[2] * s], q[3]);
    let cross =
        |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
    let t = cross(u, v).map(|c| 2.0 * c);
    let ut = cross(u, t);
    [0, 1, 2].map(|k| v[k] + w * t[k] + ut[k])
}

/// The world box of an entity: its archetype's box scaled, turned (by the stored quaternion or its
/// inverse) and moved to its position.
pub fn world_box(e: &Entity, a: &Archetype, inverse: bool) -> ([f32; 3], [f32; 3]) {
    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
    for c in 0..8 {
        let corner = [0, 1, 2].map(|k| if c & (1 << k) == 0 { a.bb_min[k] } else { a.bb_max[k] });
        let scaled = [corner[0] * e.scale_xy, corner[1] * e.scale_xy, corner[2] * e.scale_z];
        let r = rotate(e.rotation, scaled, inverse);
        for k in 0..3 {
            lo[k] = lo[k].min(r[k] + e.position[k]);
            hi[k] = hi[k].max(r[k] + e.position[k]);
        }
    }
    (lo, hi)
}

/// `assetType` of an archetype (`fwArchetypeDef__eAssetType`).
pub const ASSET_FRAGMENT: u32 = 1;
pub const ASSET_DRAWABLE: u32 = 2;
pub const ASSET_DRAWABLE_DICTIONARY: u32 = 3;

#[derive(Clone, Debug)]
pub struct Archetype {
    pub name: u32,
    pub texture_dictionary: u32,
    pub drawable_dictionary: u32,
    pub asset_name: u32,
    pub asset_type: u32,
    pub bb_min: [f32; 3],
    pub bb_max: [f32; 3],
}

pub fn parse_ytyp(res: &Resource) -> Result<Vec<Archetype>> {
    if res.version != 2 {
        bail!("ytyp of version {} (2 expected)", res.version);
    }
    let meta = Meta::parse(res)?;
    let types = meta.root("CMapTypes")?;
    let mut out = Vec::new();
    for a in meta.pointer_array(types.array("archetypes")?)? {
        // the base type, `CTimeArchetypeDef` (shown at some hours) or `CMloArchetypeDef` (an interior)
        if !a.is("CBaseArchetypeDef") && !a.is("CTimeArchetypeDef") && !a.is("CMloArchetypeDef") {
            bail!("archetype of type 0x{:08X}", a.name);
        }
        // older files (destruction props, some doors) describe a 128-byte CBaseArchetypeDef without
        // assetName / assetType: the model then bears the archetype's name
        let name = a.u32("name")?;
        let drawable_dictionary = a.u32("drawableDictionary")?;
        out.push(Archetype {
            name,
            texture_dictionary: a.u32("textureDictionary")?,
            drawable_dictionary,
            asset_name: a.u32_opt("assetName")?.unwrap_or(name),
            asset_type: a.u32_opt("assetType")?.unwrap_or(if drawable_dictionary != 0 {
                ASSET_DRAWABLE_DICTIONARY
            } else {
                ASSET_DRAWABLE
            }),
            bb_min: a.vec3("bbMin")?,
            bb_max: a.vec3("bbMax")?,
        });
        // (not used, but read all the same: an archetype without it is refused)
        a.f32("lodDist")?;
    }
    Ok(out)
}
