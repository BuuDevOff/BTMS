//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! A RAGE resource (`RSC7`: ymap, ytyp, ydr, ytd…) as the game loads it: two memory areas, the
//! system pages then the graphics pages, that its pointers address (0x5xxxxxxx: system,
//! 0x6xxxxxxx: graphics, as CodeWalker's `ResourceDataReader`).
//!
//! `extract_entry` of rage-rpf gives the 16-byte header (magic, version, system flags, graphics
//! flags) followed by the body still deflated: it is inflated here, and its length must be exactly
//! the sizes the flags give (rage-rpf's `resource_size_from_flags`, CodeWalker's
//! `GetSizeFromFlags`). Every read is bounds checked: a wrong pointer is an error, never a panic.

use std::io::Read;

use anyhow::{Context, Result, bail};
use rage_rpf::archive::resource_size_from_flags;

pub struct Resource {
    /// The resource type's version (ymap and ytyp 2, ydr 165, ytd 13…).
    pub version: u32,
    /// Made by a modding tool (a file a server resource streams), not by the game's own tools:
    /// what the game does not read in it may be wrong and is not checked (a texture's row size).
    pub modded: bool,
    data: Vec<u8>,
    system_len: usize,
}

impl Resource {
    /// `rsc`: the bytes `extract_entry` gives for a resource entry.
    pub fn parse(rsc: &[u8]) -> Result<Resource> {
        Self::parse_pages(rsc, false)
    }

    /// The system pages only (names, structures), without inflating the graphics pages (pixels,
    /// vertices): a pointer to those is then an error.
    pub fn parse_system(rsc: &[u8]) -> Result<Resource> {
        Self::parse_pages(rsc, true)
    }

    fn parse_pages(rsc: &[u8], system_only: bool) -> Result<Resource> {
        if rsc.len() < 16 || rsc[0..4] != *b"RSC7" {
            bail!("not an RSC7 resource");
        }
        let word = |o: usize| u32::from_le_bytes(rsc[o..o + 4].try_into().unwrap());
        let (version, system_flags, graphics_flags) = (word(4), word(8), word(12));
        let system_len = resource_size_from_flags(system_flags);
        let graphics_len = resource_size_from_flags(graphics_flags);
        let mut data = Vec::with_capacity(system_len + graphics_len);
        if system_only {
            data.resize(system_len, 0);
            flate2::read::DeflateDecoder::new(&rsc[16..])
                .read_exact(&mut data)
                .context("unreadable system pages (DEFLATE)")?;
            return Ok(Resource { version, modded: false, data, system_len });
        }
        flate2::read::DeflateDecoder::new(&rsc[16..])
            .read_to_end(&mut data)
            .context("unreadable resource body (DEFLATE)")?;
        if data.len() != system_len + graphics_len {
            bail!(
                "resource of {} bytes once inflated, {} expected ({} system + {} graphics)",
                data.len(),
                system_len + graphics_len,
                system_len,
                graphics_len
            );
        }
        Ok(Resource { version, modded: false, data, system_len })
    }

    /// The same resource, said to be made by a modding tool or not (see [`Resource::modded`]).
    pub fn modded(mut self, yes: bool) -> Resource {
        self.modded = yes;
        self
    }

    /// `len` bytes at pointer `ptr`, inside the area the pointer names.
    pub fn bytes(&self, ptr: u64, len: usize) -> Result<&[u8]> {
        let off = (ptr & 0x0FFF_FFFF) as usize;
        let (start, end) = match ptr >> 28 {
            5 => (off, self.system_len),
            6 => (self.system_len + off, self.data.len()),
            _ => bail!("pointer 0x{ptr:X} outside the resource"),
        };
        if start.checked_add(len).is_none_or(|e| e > end) {
            bail!("pointer 0x{ptr:X} (+{len} bytes) outside its area");
        }
        Ok(&self.data[start..start + len])
    }

    pub fn u8(&self, ptr: u64) -> Result<u8> {
        Ok(self.bytes(ptr, 1)?[0])
    }

    pub fn u16(&self, ptr: u64) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(ptr, 2)?.try_into().unwrap()))
    }

    pub fn u32(&self, ptr: u64) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(ptr, 4)?.try_into().unwrap()))
    }

    pub fn u64(&self, ptr: u64) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(ptr, 8)?.try_into().unwrap()))
    }

    pub fn f32(&self, ptr: u64) -> Result<f32> {
        Ok(f32::from_le_bytes(self.bytes(ptr, 4)?.try_into().unwrap()))
    }

    pub fn vec3(&self, ptr: u64) -> Result<[f32; 3]> {
        Ok([self.f32(ptr)?, self.f32(ptr + 4)?, self.f32(ptr + 8)?])
    }

    /// The zero-terminated string at `ptr` (at most 256 bytes).
    pub fn cstr(&self, ptr: u64) -> Result<String> {
        let off = (ptr & 0x0FFF_FFFF) as usize;
        let avail = match ptr >> 28 {
            5 => self.system_len.saturating_sub(off),
            6 => self.data.len().saturating_sub(self.system_len + off),
            _ => bail!("text pointer 0x{ptr:X} outside the resource"),
        };
        let bytes = self.bytes(ptr, avail.min(256))?;
        let end = bytes.iter().position(|&b| b == 0).context("unterminated text")?;
        Ok(String::from_utf8_lossy(&bytes[..end]).into_owned())
    }

    /// A `ResourcePointerList64` / `ResourceSimpleList64` at `ptr`: (entries pointer, count).
    pub fn list(&self, ptr: u64) -> Result<(u64, usize)> {
        Ok((self.u64(ptr)?, self.u16(ptr + 8)? as usize))
    }

    /// The pointers of a `ResourcePointerList64` at `ptr`.
    pub fn pointers(&self, ptr: u64) -> Result<Vec<u64>> {
        let (entries, count) = self.list(ptr)?;
        (0..count as u64).map(|i| self.u64(entries + i * 8)).collect()
    }
}

/// The hash RAGE gives a file name: Jenkins one-at-a-time over the lowercase name.
pub fn joaat(name: &str) -> u32 {
    rage_rpf::rage_joaat(&name.to_lowercase())
}

/// The hash of a Meta structure or field name: the same, case kept (`archetypeName` and
/// `archetypename` differ; the structure infos of a ymap only match the first).
pub fn meta_hash(name: &str) -> u32 {
    rage_rpf::rage_joaat(name)
}
