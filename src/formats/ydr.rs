//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! A model (`.ydr`, `rage::rmcDrawable`, PC Legacy layout): its levels of detail, their pieces
//! (geometries: vertices, triangles, the shader that paints them), the shaders and the textures
//! they name. Offsets are CodeWalker's (`DrawableBase`, `DrawableModel`, `DrawableGeometry`,
//! `VertexBuffer`, `VertexDeclaration`, `IndexBuffer`, `ShaderGroup`, `ShaderFX`,
//! `ShaderParametersBlock`), and a reading is checked against what the file says of itself:
//! vertex size against the declaration, indices against the vertex count, vertices against the
//! model's box.

use anyhow::{Context, Result, bail};

use crate::formats::resource::Resource;
use crate::formats::ytd::{Texture, parse_texture_dictionary};

/// Root of a resource: its first system page.
const ROOT: u64 = 0x5000_0000;

pub struct Drawable {
    pub bb_min: [f32; 3],
    pub bb_max: [f32; 3],
    pub lod_dist: [f32; 4],
    /// Models per level: high, medium, low, very low.
    pub lods: [Vec<Model>; 4],
    pub shaders: Vec<Shader>,
    /// Textures held by the model itself (else in the archetype's `.ytd`).
    pub textures: Vec<Texture>,
}

pub struct Model {
    pub geometries: Vec<Geometry>,
}

pub struct Geometry {
    pub shader: usize,
    pub positions: Vec<[f32; 3]>,
    /// First texture coordinates (none when the declaration has none).
    pub uvs: Vec<[f32; 2]>,
    /// Second texture coordinates (the terrain's blend mask; none when absent).
    pub uvs1: Vec<[f32; 2]>,
    /// Vertex colours 0 and 1 as stored (4 bytes each; none when absent).
    pub colours0: Vec<[u8; 4]>,
    pub colours1: Vec<[u8; 4]>,
    pub indices: Vec<u16>,
}

pub struct Shader {
    pub name: u32,
    /// Render bucket (`ShaderFX.RenderBucket`): 0 opaque, 1 alpha, 2 decal, 3 cutout, 4 no splash,
    /// 5 no water, 6 water, 7 displacement alpha.
    pub bucket: u8,
    /// (parameter name hash, texture name) of the texture parameters.
    pub textures: Vec<(u32, String)>,
}

/// Size in bytes of a vertex component of type `t` (`rage::grcFvf` types, CodeWalker's
/// `VertexComponentType`).
fn component_size(t: u64) -> Option<usize> {
    Some(match t {
        1 => 4,      // half2
        2 => 4,      // float
        3 => 8,      // half4
        5 => 8,      // float2
        6 => 12,     // float3
        7 => 16,     // float4
        8..=10 => 4, // ubyte4, colour, rgba8 snorm
        _ => return None,
    })
}

fn half(bits: u16) -> f32 {
    let (sign, exp, frac) = ((bits >> 15) as u32, ((bits >> 10) & 0x1F) as i32, (bits & 0x3FF) as f32);
    let v = match exp {
        0 => frac / 1024.0 * 2f32.powi(-14),
        31 => f32::INFINITY,
        e => (1.0 + frac / 1024.0) * 2f32.powi(e - 15),
    };
    if sign == 1 { -v } else { v }
}

pub fn parse_ydr(res: &Resource) -> Result<Drawable> {
    if res.version != 165 {
        bail!("ydr of version {} (165 expected)", res.version);
    }
    parse_drawable(res, ROOT)
}

/// The drawables of a `.ydd` (`DrawableDictionary`: name hashes at 0x20, drawables at 0x30), by
/// name hash.
pub fn parse_ydd(res: &Resource) -> Result<Vec<(u32, Drawable)>> {
    if res.version != 165 {
        bail!("ydd of version {} (165 expected)", res.version);
    }
    let (hashes, count) = res.list(ROOT + 0x20)?;
    let drawables = res.pointers(ROOT + 0x30)?;
    if count != drawables.len() {
        bail!("{count} names for {} models", drawables.len());
    }
    let mut out = Vec::with_capacity(count);
    for (i, d) in drawables.into_iter().enumerate() {
        out.push((res.u32(hashes + i as u64 * 4)?, parse_drawable(res, d).with_context(|| format!("model {i}"))?));
    }
    Ok(out)
}

/// The drawable of a `.yft` (`FragType`: its drawable at 0x30).
pub fn parse_yft(res: &Resource) -> Result<Drawable> {
    if res.version != 162 {
        bail!("yft of version {} (162 expected)", res.version);
    }
    let d = res.u64(ROOT + 0x30)?;
    if d == 0 {
        bail!("fragment without a model");
    }
    parse_drawable(res, d)
}

/// Bit of a model's render mask: drawn in the main pass (0xFF: every pass; 0xFD: not in the
/// shadows; 0xE2: the shadows only, as the shadow model of every tree).
const MAIN_PASS: u16 = 1;

/// The drawable at `at` (the root of a `.ydr`, or an entry of a `.ydd`).
pub fn parse_drawable(res: &Resource, at: u64) -> Result<Drawable> {
    let shaders_ptr = res.u64(at + 0x10)?;
    let (bb_min, bb_max) = (res.vec3(at + 0x30)?, res.vec3(at + 0x40)?);
    let lod_dist = [res.f32(at + 0x70)?, res.f32(at + 0x74)?, res.f32(at + 0x78)?, res.f32(at + 0x7C)?];

    let (shaders, textures) = if shaders_ptr == 0 { (Vec::new(), Vec::new()) } else { shader_group(res, shaders_ptr)? };

    let mut lods: [Vec<Model>; 4] = Default::default();
    for (level, models) in lods.iter_mut().enumerate() {
        let list = res.u64(at + 0x50 + level as u64 * 8)?;
        if list == 0 {
            continue;
        }
        for m in res.pointers(list)? {
            // the render mask (low byte of the flags at 0x2C): bit 0, the model is drawn in the
            // main pass. A model without it only serves another pass of the game: the shadow of a
            // tree (an untextured crown, drawn grey here), a reflection or water proxy
            if res.u16(m + 0x2C)? & MAIN_PASS == 0 {
                continue;
            }
            models.push(model(res, m, shaders.len()).with_context(|| format!("level {level}"))?);
        }
    }
    Ok(Drawable { bb_min, bb_max, lod_dist, lods, shaders, textures })
}

fn model(res: &Resource, at: u64, shader_count: usize) -> Result<Model> {
    let (geoms, count) = (res.u64(at + 0x08)?, res.u16(at + 0x10)? as u64);
    let mapping = res.u64(at + 0x20)?;
    let mut geometries = Vec::with_capacity(count as usize);
    for i in 0..count {
        let shader = if mapping == 0 { 0 } else { res.u16(mapping + i * 2)? as usize };
        if shader_count > 0 && shader >= shader_count {
            bail!("piece {i}: shader {shader} of {shader_count}");
        }
        geometries.push(geometry(res, res.u64(geoms + i * 8)?, shader).with_context(|| format!("piece {i}"))?);
    }
    Ok(Model { geometries })
}

fn geometry(res: &Resource, at: u64, shader: usize) -> Result<Geometry> {
    let (vb, ib) = (res.u64(at + 0x18)?, res.u64(at + 0x38)?);
    let (index_count, triangles) = (res.u32(at + 0x58)? as usize, res.u32(at + 0x5C)? as usize);
    let vertex_count = res.u16(at + 0x60)? as usize;
    let stride = res.u16(at + 0x70)? as usize;

    // vertex buffer and its declaration
    let vb_stride = res.u16(vb + 0x08)? as usize;
    let mut data = res.u64(vb + 0x10)?;
    let vb_count = res.u32(vb + 0x18)? as usize;
    if data == 0 {
        data = res.u64(vb + 0x20)?;
    }
    let info = res.u64(vb + 0x30)?;
    let (flags, decl_stride, types) = (res.u32(info)?, res.u16(info + 4)? as usize, res.u64(info + 8)?);
    if vb_stride != stride || decl_stride != stride || vb_count != vertex_count {
        bail!("vertex buffer: size {vb_stride}/{decl_stride}/{stride}, count {vb_count}/{vertex_count}");
    }
    let mut offsets = [None; 16];
    let mut size = 0;
    for (c, offset) in offsets.iter_mut().enumerate() {
        if flags & (1 << c) != 0 {
            let t = (types >> (c * 4)) & 0xF;
            let s = component_size(t).with_context(|| format!("component {c} of unknown type {t}"))?;
            *offset = Some((size, t));
            size += s;
        }
    }
    if size != stride {
        bail!("declaration 0x{flags:X} / 0x{types:016X}: {size} bytes, the vertex has {stride}");
    }
    let vertices = res.bytes(data, vertex_count * stride)?;
    let f = |b: &[u8], o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let h = |b: &[u8], o: usize| half(u16::from_le_bytes(b[o..o + 2].try_into().unwrap()));
    let positions = match offsets[0] {
        Some((o, 6)) => vertices.chunks_exact(stride).map(|v| [f(v, o), f(v, o + 4), f(v, o + 8)]).collect(),
        Some((o, 3)) => vertices.chunks_exact(stride).map(|v| [h(v, o), h(v, o + 2), h(v, o + 4)]).collect(),
        other => bail!("position of type {other:?}"),
    };
    let uvs = match offsets[6] {
        Some((o, 5)) => vertices.chunks_exact(stride).map(|v| [f(v, o), f(v, o + 4)]).collect(),
        Some((o, 1)) => vertices.chunks_exact(stride).map(|v| [h(v, o), h(v, o + 2)]).collect(),
        _ => Vec::new(),
    };
    let uvs1 = match offsets[7] {
        Some((o, 5)) => vertices.chunks_exact(stride).map(|v| [f(v, o), f(v, o + 4)]).collect(),
        Some((o, 1)) => vertices.chunks_exact(stride).map(|v| [h(v, o), h(v, o + 2)]).collect(),
        _ => Vec::new(),
    };
    let colours = |c: usize| match offsets[c] {
        Some((o, 8..=10)) => vertices.chunks_exact(stride).map(|v| [v[o], v[o + 1], v[o + 2], v[o + 3]]).collect(),
        _ => Vec::new(),
    };
    let (colours0, colours1) = (colours(4), colours(5));

    // indices (triangle list)
    let ib_count = res.u32(ib + 0x08)? as usize;
    let ib_data = res.u64(ib + 0x10)?;
    if ib_count != index_count || triangles * 3 != index_count {
        bail!("indices: {ib_count} in the buffer, {index_count} declared, {triangles} triangles");
    }
    let indices: Vec<u16> = res
        .bytes(ib_data, index_count * 2)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    if let Some(bad) = indices.iter().find(|&&i| i as usize >= vertex_count) {
        bail!("index {bad} for {vertex_count} vertices");
    }
    Ok(Geometry { shader, positions, uvs, uvs1, colours0, colours1, indices })
}

fn shader_group(res: &Resource, at: u64) -> Result<(Vec<Shader>, Vec<Texture>)> {
    let dict = res.u64(at + 0x08)?;
    let (list, count) = (res.u64(at + 0x10)?, res.u16(at + 0x18)? as u64);
    let textures =
        if dict == 0 { Vec::new() } else { parse_texture_dictionary(res, dict).context("the model's textures")? };
    let mut shaders = Vec::with_capacity(count as usize);
    for i in 0..count {
        let s = res.u64(list + i * 8)?;
        let (params, name, param_count) = (res.u64(s)?, res.u32(s + 0x08)?, res.u8(s + 0x10)? as u64);
        let bucket = res.u8(s + 0x11)?;
        // parameters (16 bytes each), their vector data, then one name hash per parameter
        let mut kinds = Vec::with_capacity(param_count as usize);
        let mut data_len = 0;
        for p in 0..param_count {
            let kind = res.u8(params + p * 16)?;
            data_len += match kind {
                0 => 0,
                1 => 16,
                n => 16 * n as u64,
            };
            kinds.push((kind, res.u64(params + p * 16 + 8)?));
        }
        let hashes = params + param_count * 16 + data_len;
        let mut textures = Vec::new();
        for (p, (kind, ptr)) in kinds.into_iter().enumerate() {
            if kind == 0 && ptr != 0 {
                let param = res.u32(hashes + p as u64 * 4)?;
                textures.push((param, res.cstr(res.u64(ptr + 0x28)?)?));
            }
        }
        shaders.push(Shader { name, bucket, textures });
    }
    Ok((shaders, textures))
}
