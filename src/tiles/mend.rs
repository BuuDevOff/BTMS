//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The last holes of the tiles: empty regions closed all around by what is drawn, that no
//! placement file fills ([`crate::world::holes`] keeps the files that do): an opening onto an interior
//! that is not drawn, a basin whose water is not drawn. Once the blocks are written, each such
//! region gets the colours around it, so that the map has no hole left.
//!
//! Read from the zoom 7 tiles (one per block, 0.25 m per pixel): a block with a JPEG tile (or a
//! WebP without alpha) is full, one with no tile is outside the map, a PNG tile (or a WebP with
//! alpha) may have empty pixels, grouped in regions
//! and joined across the blocks' edges. A region that reaches the outside is the outside (the
//! sea beyond the seabed). The others are holes: the zoom 9 picture of their blocks is filled
//! from the rim inwards, each pixel the mean of its neighbours already coloured, and the block's
//! tiles are written again. A hole larger than [`MAX_AREA`] m² is left and reported: colours
//! spread that far would not be a map.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;

use super::{BLOCK, BLOCK_PX, TILE, Written, crop, halve, read_tile, write_tile};

/// Metres per pixel of a zoom 7 tile.
const METRES: f32 = BLOCK / TILE as f32;
/// A hole larger than this many m² is not coloured in.
const MAX_AREA: f32 = 2000.0;

type BlockId = (i32, i32);

/// The empty pixels of a block's zoom 7 tile, in regions.
struct Partial {
    /// Region of each pixel: 0 where something is drawn.
    label: Vec<u32>,
    /// Pixels of each region (index = number − 1).
    sizes: Vec<usize>,
}

enum State {
    Full,
    Partial(Partial),
    /// No tile: nothing is drawn there.
    Outside,
}

/// The 4-connected regions of the pixels `empty` is true for, in a `TILE` × `TILE` picture.
fn regions(empty: &[bool]) -> Partial {
    let mut label = vec![0u32; TILE * TILE];
    let mut sizes = Vec::new();
    let mut stack = Vec::new();
    for start in 0..TILE * TILE {
        if !empty[start] || label[start] != 0 {
            continue;
        }
        let id = sizes.len() as u32 + 1;
        let mut n = 0;
        label[start] = id;
        stack.push(start);
        while let Some(i) = stack.pop() {
            n += 1;
            let (x, y) = (i % TILE, i / TILE);
            let mut visit = |j: usize| {
                if empty[j] && label[j] == 0 {
                    label[j] = id;
                    stack.push(j);
                }
            };
            if x > 0 {
                visit(i - 1);
            }
            if x + 1 < TILE {
                visit(i + 1);
            }
            if y > 0 {
                visit(i - TILE);
            }
            if y + 1 < TILE {
                visit(i + TILE);
            }
        }
        sizes.push(n);
    }
    Partial { label, sizes }
}

fn state(out: &Path, (bx, by): BlockId) -> State {
    // (an older `.jpg`, or a `.webp` without alpha: told without decoding it)
    if super::format::opaque_on_disk(out, 7, bx, by) {
        return State::Full;
    }
    match read_tile(out, 7, bx, by) {
        Some(rgba) => {
            let empty: Vec<bool> = rgba.as_chunks::<4>().0.iter().map(|p| p[3] == 0).collect();
            if empty.iter().any(|e| *e) { State::Partial(regions(&empty)) } else { State::Full }
        }
        None => State::Outside,
    }
}

/// Gives every pixel of `mask` the colours around it: from the rim inwards, each the mean of its
/// neighbours already coloured (those drawn, then those filled before it).
pub(super) fn inpaint(rgba: &mut [u8], w: usize, h: usize, mask: &[bool]) -> usize {
    let mut coloured: Vec<bool> = rgba.as_chunks::<4>().0.iter().map(|p| p[3] > 0).collect();
    let neighbours = |i: usize| {
        let (x, y) = ((i % w) as i64, (i / w) as i64);
        [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)]
            .into_iter()
            .map(move |(dx, dy)| (x + dx, y + dy))
            .filter(move |(nx, ny)| *nx >= 0 && *ny >= 0 && (*nx as usize) < w && (*ny as usize) < h)
            .map(move |(nx, ny)| ny as usize * w + nx as usize)
    };
    // the first ring: masked pixels touching a coloured one
    let mut ring: Vec<usize> =
        (0..w * h).filter(|&i| mask[i] && !coloured[i] && neighbours(i).any(|j| coloured[j])).collect();
    let mut queued = vec![false; w * h];
    for &i in &ring {
        queued[i] = true;
    }
    let mut filled = 0;
    while !ring.is_empty() {
        // the colours of the whole ring from what was coloured before it
        let colours: Vec<[u8; 3]> = ring
            .iter()
            .map(|&i| {
                let (mut sum, mut n) = ([0u32; 3], 0u32);
                for j in neighbours(i).filter(|&j| coloured[j]) {
                    for k in 0..3 {
                        sum[k] += rgba[j * 4 + k] as u32;
                    }
                    n += 1;
                }
                [(sum[0] / n.max(1)) as u8, (sum[1] / n.max(1)) as u8, (sum[2] / n.max(1)) as u8]
            })
            .collect();
        for (&i, c) in ring.iter().zip(&colours) {
            rgba[i * 4..i * 4 + 3].copy_from_slice(c);
            rgba[i * 4 + 3] = 255;
        }
        let mut next = Vec::new();
        for &i in &ring {
            coloured[i] = true;
            filled += 1;
        }
        for &i in &ring {
            for j in neighbours(i) {
                if mask[j] && !coloured[j] && !queued[j] {
                    queued[j] = true;
                    next.push(j);
                }
            }
        }
        ring = next;
    }
    filled
}

/// What [`mend`] did.
pub struct Mended {
    /// Holes coloured in, and their area in m².
    pub filled: usize,
    pub area: f32,
    /// Holes too large to colour in: middle (x, y) and area.
    pub left: Vec<([f32; 2], f32)>,
    /// The blocks whose tiles were written again.
    pub blocks: Vec<BlockId>,
}

/// Colours in the holes left in the tiles of these blocks (see the module).
pub fn mend(out: &Path, blocks: &[BlockId]) -> Result<Mended> {
    mend_with(out, blocks, false)
}

/// [`mend`], from the regions of `blocks`. `reach`: a region that reaches a block not among them
/// reads that block from the folder and goes on there (`update`, a few blocks of a whole
/// map: as the render of the whole map sees them); otherwise such a block is the outside.
pub fn mend_with(out: &Path, blocks: &[BlockId], reach: bool) -> Result<Mended> {
    let known: HashSet<BlockId> = blocks.iter().copied().collect();
    let mut states: HashMap<BlockId, State> = HashMap::new();
    for &b in blocks {
        states.insert(b, state(out, b));
    }
    let mut done: HashSet<(BlockId, u32)> = HashSet::new();
    let mut result = Mended { filled: 0, area: 0.0, left: Vec::new(), blocks: Vec::new() };
    // the regions to colour in, by block
    let mut to_fill: HashMap<BlockId, Vec<u32>> = HashMap::new();
    let mut order: Vec<BlockId> = blocks.to_vec();
    order.sort_unstable();
    for &start in &order {
        let count = match &states[&start] {
            State::Partial(p) => p.sizes.len() as u32,
            _ => 0,
        };
        for region in 1..=count {
            if !done.insert((start, region)) {
                continue;
            }
            let mut parts: Vec<(BlockId, u32)> = vec![(start, region)];
            let mut open = false;
            let mut at = 0;
            while at < parts.len() {
                let (b, r) = parts[at];
                at += 1;
                let State::Partial(part) = &states[&b] else { continue };
                let mut facing: Vec<(BlockId, usize)> = Vec::new();
                for k in 0..TILE {
                    if part.label[k] == r {
                        facing.push(((b.0, b.1 + 1), (TILE - 1) * TILE + k)); // north
                    }
                    if part.label[(TILE - 1) * TILE + k] == r {
                        facing.push(((b.0, b.1 - 1), k)); // south
                    }
                    if part.label[k * TILE] == r {
                        facing.push(((b.0 - 1, b.1), k * TILE + TILE - 1)); // west
                    }
                    if part.label[k * TILE + TILE - 1] == r {
                        facing.push(((b.0 + 1, b.1), k * TILE)); // east
                    }
                }
                for (n, pixel) in facing {
                    if reach && !states.contains_key(&n) {
                        states.insert(n, state(out, n));
                    }
                    match states.get(&n) {
                        // a block that was not drawn, or where nothing is: the outside
                        None | Some(State::Outside) => open = true,
                        Some(State::Full) => {}
                        Some(State::Partial(beyond)) => {
                            let r2 = beyond.label[pixel];
                            if r2 != 0 && done.insert((n, r2)) {
                                parts.push((n, r2));
                            }
                        }
                    }
                }
            }
            debug_assert!(reach || parts.iter().all(|(b, _)| known.contains(b)));
            if open {
                continue;
            }
            let (mut pixels, mut sx, mut sy) = (0usize, 0f64, 0f64);
            for (b, r) in &parts {
                let State::Partial(part) = &states[b] else { continue };
                pixels += part.sizes[*r as usize - 1];
                for (i, l) in part.label.iter().enumerate() {
                    if l == r {
                        sx += b.0 as f64 * BLOCK as f64 + ((i % TILE) as f64 + 0.5) * METRES as f64;
                        sy += (b.1 as f64 + 1.0) * BLOCK as f64 - ((i / TILE) as f64 + 0.5) * METRES as f64;
                    }
                }
            }
            let area = pixels as f32 * METRES * METRES;
            if area > MAX_AREA {
                result.left.push(([(sx / pixels as f64) as f32, (sy / pixels as f64) as f32], area));
                continue;
            }
            result.filled += 1;
            result.area += area;
            for (b, r) in parts {
                to_fill.entry(b).or_default().push(r);
            }
        }
    }
    // the blocks with holes: their zoom 9 picture filled, their tiles written again
    let mut blocks: Vec<BlockId> = to_fill.keys().copied().collect();
    blocks.sort_unstable();
    for (bx, by) in blocks {
        let State::Partial(part) = &states[&(bx, by)] else { continue };
        let wanted: HashSet<u32> = to_fill[&(bx, by)].iter().copied().collect();
        // the zoom 7 pixels of the holes, and those touching them (a zoom 7 pixel partly drawn
        // still has empty zoom 9 pixels)
        let mut cells = vec![false; TILE * TILE];
        for (i, l) in part.label.iter().enumerate() {
            if wanted.contains(l) {
                let (x, y) = ((i % TILE) as i64, (i / TILE) as i64);
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let (nx, ny) = (x + dx, y + dy);
                        if nx >= 0 && ny >= 0 && (nx as usize) < TILE && (ny as usize) < TILE {
                            cells[ny as usize * TILE + nx as usize] = true;
                        }
                    }
                }
            }
        }
        // the block at zoom 9: 4 × 4 tiles, the first row the north edge
        let mut rgba = vec![0u8; BLOCK_PX * BLOCK_PX * 4];
        for row in 0..4 {
            for col in 0..4 {
                if let Some(tile) = read_tile(out, 9, bx * 4 + col as i32, by * 4 + (3 - row as i32)) {
                    for y in 0..TILE {
                        let dst = ((row * TILE + y) * BLOCK_PX + col * TILE) * 4;
                        rgba[dst..dst + TILE * 4].copy_from_slice(&tile[y * TILE * 4..(y + 1) * TILE * 4]);
                    }
                }
            }
        }
        let scale = BLOCK_PX / TILE;
        let mask: Vec<bool> = (0..BLOCK_PX * BLOCK_PX)
            .map(|i| rgba[i * 4 + 3] == 0 && cells[(i / BLOCK_PX / scale) * TILE + (i % BLOCK_PX) / scale])
            .collect();
        if inpaint(&mut rgba, BLOCK_PX, BLOCK_PX, &mask) == 0 {
            continue;
        }
        for row in 0..4 {
            for col in 0..4 {
                write_tile(
                    out,
                    9,
                    bx * 4 + col as i32,
                    by * 4 + (3 - row as i32),
                    &crop(&rgba, BLOCK_PX, col * TILE, row * TILE, TILE),
                )?;
            }
        }
        let half = halve(&rgba, BLOCK_PX, BLOCK_PX);
        for row in 0..2 {
            for col in 0..2 {
                write_tile(
                    out,
                    8,
                    bx * 2 + col as i32,
                    by * 2 + (1 - row as i32),
                    &crop(&half, BLOCK_PX / 2, col * TILE, row * TILE, TILE),
                )?;
            }
        }
        let quarter = halve(&half, BLOCK_PX / 2, BLOCK_PX / 2);
        let _: Written = write_tile(out, 7, bx, by, &quarter)?;
        result.blocks.push((bx, by));
    }
    Ok(result)
}

/// Says what [`mend`] did.
pub fn report(m: &Mended) {
    if m.filled > 0 || !m.left.is_empty() {
        println!(
            "last holes: {} hole(s) that no map fills ({:.0} m²) filled with the colours around them, in {} block(s); {} too large, left",
            m.filled,
            m.area,
            m.blocks.len(),
            m.left.len()
        );
        for (centre, area) in &m.left {
            println!("  hole left: {area:.0} m² near x {:.0}, y {:.0}", centre[0], centre[1]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inpaint_fills_from_the_rim() {
        // a 9 × 9 picture, red on the left half, blue on the right, a 3 × 3 hole in the middle
        let (w, h) = (9, 9);
        let mut rgba = vec![0u8; w * h * 4];
        let mut mask = vec![false; w * h];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if (3..6).contains(&x) && (3..6).contains(&y) {
                    mask[i] = true;
                } else {
                    rgba[i * 4..i * 4 + 4].copy_from_slice(if x < 4 { &[200, 0, 0, 255] } else { &[0, 0, 200, 255] });
                }
            }
        }
        assert_eq!(inpaint(&mut rgba, w, h, &mask), 9);
        // every pixel opaque, the hole a mix of its two sides
        assert!(rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
        let centre = &rgba[(4 * w + 4) * 4..][..4];
        assert!(centre[0] > 0 && centre[2] > 0 && centre[1] == 0);
        // a pixel out of the mask is never touched, even empty
        let mut rgba = vec![0u8; 4 * 4];
        rgba[0..4].copy_from_slice(&[9, 9, 9, 255]);
        assert_eq!(inpaint(&mut rgba, 4, 1, &[false, true, true, false]), 2);
        assert_eq!(rgba[12..16], [0, 0, 0, 0]);
        assert_eq!(rgba[4..8], [9, 9, 9, 255]);
    }

    /// A hole closed all around, in a folder of WebP tiles: read back from them, coloured in, and
    /// the block's tiles written again as WebP, without alpha now.
    #[test]
    fn a_hole_of_a_webp_folder_is_mended_in_webp() {
        use crate::tiles::format::{self, Format};
        use crate::tiles::webp::{Effort, Quality};
        for (quality, effort) in [
            (Quality::Lossless, Effort::Fast),
            (Quality::Lossy(90), Effort::Fast),
            (Quality::Lossy(90), Effort::Careful),
        ] {
            let out = std::env::temp_dir()
                .join(format!("btms-tests-{}-holes-webp-{quality:?}-{effort:?}", std::process::id()));
            let _ = std::fs::remove_dir_all(&out);
            format::set(&out, Format::Webp(quality), effort);
            let mut seed = 11u64;
            let mut rgba = crate::tiles::webp::tests::ground(BLOCK_PX, BLOCK_PX, false, &mut seed);
            // 40 × 40 px of zoom 9 (2.5 m) empty, astride four zoom 9 tiles
            for y in 1000..1040 {
                for x in 1000..1040 {
                    rgba[(y * BLOCK_PX + x) * 4..][..4].fill(0);
                }
            }
            crate::tiles::write::tiles(&out, 0, 0, &crate::tiles::write::Pixels::Whole(rgba)).unwrap();
            assert!(!format::opaque_on_disk(&out, 7, 0, 0));
            let mended = mend(&out, &[(0, 0)]).unwrap();
            assert_eq!(
                (mended.filled, mended.left.len(), mended.blocks.clone()),
                (1, 0, vec![(0, 0)]),
                "{quality:?} {effort:?}"
            );
            // every tile opaque again, still WebP, nothing else in the folder
            assert!(format::opaque_on_disk(&out, 7, 0, 0));
            for (col, row) in [(1, 1), (2, 1), (1, 2), (2, 2)] {
                let tile = read_tile(&out, 9, col, 3 - row).unwrap();
                assert!(
                    tile.as_chunks::<4>().0.iter().all(|p| p[3] == 255),
                    "{quality:?} {effort:?}: tile {col},{}",
                    3 - row
                );
            }
            let mut stack = vec![out.clone()];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d).unwrap().flatten() {
                    if e.path().is_dir() {
                        stack.push(e.path());
                    } else {
                        assert!(e.path().extension().is_some_and(|x| x == "webp"), "{}", e.path().display());
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&out);
        }
    }

    /// A hole at the edge of a block drawn again, against a neighbour already mended: closed when
    /// the neighbour is read from the folder (`reach`), as the render of the whole map closes it;
    /// the outside when it is not, or when the neighbour has no tile.
    #[test]
    fn a_hole_at_the_edge_reads_the_neighbour_from_the_folder() {
        use super::super::{format, write};
        let run = |reach: bool, neighbour: bool| -> (usize, bool) {
            let out =
                std::env::temp_dir().join(format!("btms-tests-{}-hole-edge-{reach}-{neighbour}", std::process::id()));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            format::set(
                &out,
                format::Format::Webp(crate::tiles::webp::Quality::Lossless),
                crate::tiles::webp::Effort::Fast,
            );
            // block (0, 0): opaque, but a hole of 32 × 32 px at its eastern edge; block (1, 0): opaque
            let mut a = vec![0u8; BLOCK_PX * BLOCK_PX * 4];
            for (i, p) in a.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let (x, y) = (i % BLOCK_PX, i / BLOCK_PX);
                if !(x >= BLOCK_PX - 32 && (1000..1032).contains(&y)) {
                    p.copy_from_slice(&[90, 120, 60, 255]);
                }
            }
            write::tiles(&out, 0, 0, &write::Pixels::Whole(a)).unwrap();
            if neighbour {
                write::tiles(&out, 1, 0, &write::Pixels::Whole(vec![200u8; BLOCK_PX * BLOCK_PX * 4])).unwrap();
            }
            let m = mend_with(&out, &[(0, 0)], reach).unwrap();
            let z9 = read_tile(&out, 9, 3, 2).unwrap();
            let _ = std::fs::remove_dir_all(&out);
            (m.filled, z9.as_chunks::<4>().0.iter().all(|p| p[3] == 255))
        };
        assert_eq!(run(true, true), (1, true));
        assert_eq!(run(false, true), (0, false));
        assert_eq!(run(true, false), (0, false));
    }

    #[test]
    fn regions_are_four_connected() {
        let mut empty = vec![false; TILE * TILE];
        empty[0] = true;
        empty[1] = true;
        empty[TILE + 1 + 1] = true; // touches (1, 0) by a corner only
        let p = regions(&empty);
        assert_eq!(p.sizes, [2, 1]);
        assert_eq!(p.label[0], 1);
        assert_eq!(p.label[TILE + 2], 2);
    }
}
