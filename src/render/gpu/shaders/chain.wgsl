//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

// The chain of a block on the graphics card (render/gpu/chain.rs): what the processor does to a
// block once it is drawn, done on the card where the drawing left it. One entry point per step, in
// this order:
//
//   floor_block, floor_ring    the lowest heights of the 64 parts of the block (`Frame::far`)
//   compose                    the heights the shading reads (`Frame::shade`), into its buffer
//   (the ten passes of the shading, shade.wgsl)
//   rgba                       colours times the factors, to 8 bits (`Frame::shade`, `Raster::rgba`)
//   holes (4 times)            the small holes closed (`tiles::fill_holes`)
//   depths, spread (4 times)   the ground under the water (`water::spread`)
//   state, seed, rows, columns the water over the ground, and the sea where there is none (`Water::flood`)
//   water                      its colours
//   halve_8, halve_7           zooms 8 and 7 (`tiles::halve`)
//
// Every step does what the processor does, with the same operations in the same order. Integers
// (holes, halving) are equal to the bit; the floats are those of the processor but for what a
// card may round otherwise: the product and sum of the 8-bit conversion (fused or not), `exp` in
// the colours of the water, a division (done with a correction step, below). Nothing that is not
// a number goes through the arithmetic: depths, heights and ground are read as bits and tested.

struct Params {
    // side of the block's picture, in pixels
    px: u32,
    // the raster of heights around it (the shading's), and the block in it
    fw: u32,
    fh: u32,
    wx: u32,
    wy: u32,
    // 1: the colours are multiplied by the factors of the shading
    lit: u32,
    // pieces of water, rectangles of the files, the whole block open sea, the open sea test
    quads: u32,
    areas: u32,
    all_open: u32,
    open_test: u32,
    // the colour of deep water as the processor's bytes
    deep: u32,
    // 0, which the compiler cannot know (`add`)
    zero: u32,
    // looked down from (metres), the depth's scale (`gpu::FAR`)
    top: f32,
    far: f32,
    pad1: f32,
    pad2: f32,
    // per file's rectangle: its columns i0..i1, its rows j0..j1
    area: array<vec4<u32>, 4>,
    shallow: vec4<f32>,
    deep_colour: vec4<f32>,
};

@group(0) @binding(0) var<uniform> P: Params;
// the block as drawn: colours, depths (metres / far; 1 where nothing is drawn)
@group(0) @binding(1) var colour: texture_2d<f32>;
@group(0) @binding(2) var depth: texture_depth_2d;
// the heights around the block, as drawn (depths too)
@group(0) @binding(3) var around: texture_depth_2d;
// the RGBA picture (r | g << 8 | b << 16 | a << 24): read from one, written to the other
@group(0) @binding(4) var<storage, read> src: array<u32>;
@group(0) @binding(5) var<storage, read_write> dst: array<u32>;
// the shading's factors of the block, and the heights it reads (as bits)
@group(0) @binding(6) var<storage, read> factors: array<f32>;
@group(0) @binding(7) var<storage, read_write> heights: array<u32>;
// the ground under the water (depths as bits), read from one, written to the other
@group(0) @binding(8) var<storage, read> gsrc: array<u32>;
@group(0) @binding(9) var<storage, read_write> gdst: array<u32>;
// the height of the water over each pixel, and what the pixel is (0 dry, 1 water over the
// ground, 2 water with no ground under it, 3 that made sea)
@group(0) @binding(10) var<storage, read_write> surface: array<f32>;
@group(0) @binding(11) var<storage, read_write> state: array<u32>;
// the pieces of water: height (bits), first row, rows end, where their rows' ranges start
@group(0) @binding(12) var<storage, read> quads: array<vec4<u32>>;
@group(0) @binding(13) var<storage, read> rows: array<vec2<u32>>;
// [0]: the sea spread in this round of the sweeps; [1]: in the last round that ran
@group(0) @binding(14) var<storage, read_write> status: array<atomic<u32>>;
@group(0) @binding(15) var<storage, read_write> z8: array<u32>;
@group(0) @binding(16) var<storage, read_write> z7: array<u32>;
// the deepest depth (bits) drawn in each of the 8 × 8 parts of the block; 0: none
@group(0) @binding(17) var<storage, read_write> parts: array<atomic<u32>>;

const NEG_INF: u32 = 0xff800000u;
const INF: u32 = 0x7f800000u;
// no water over the pixel (the processor's minus infinity)
const NONE: f32 = -1e30f;

fn finite(bits: u32) -> bool {
    return (bits & 0x7f800000u) != 0x7f800000u;
}

fn is_nan(bits: u32) -> bool {
    return (bits & 0x7fffffffu) > 0x7f800000u;
}

// a / b correctly rounded, as the processor divides: the card's quotient and one correction
// (`inverse` is 1 / b rounded, written as a constant expression where `b` is one)
fn quotient(a: f32, b: f32, inverse: f32) -> f32 {
    let q = a * inverse;
    let r = fma(-q, b, a);
    return fma(r, inverse, q);
}

// `x / 3` correctly rounded, as the processor divides, by integers: a card's `fma` need not be
// fused (HLSL's `mad`), so the correction of `quotient` is not exact. For a positive normal x =
// M · 2^(E − 150) (M of 24 bits): M · 2 / 3 or M · 4 / 3 has 24 bits, its remainder (0, 1 or 2
// thirds) never a half: up when it is 2.
fn third(x: f32) -> f32 {
    let b = bitcast<u32>(x);
    let e = (b >> 23u) & 255u;
    if (b >> 31u) != 0u || e <= 2u || e >= 254u {
        return x / 3.0f;
    }
    let m = (b & 0x7fffffu) | 0x800000u;
    var n = m * 4u;
    var shift = 2u;
    if m * 2u >= 3u * 0x800000u {
        n = m * 2u;
        shift = 1u;
    }
    var q = n / 3u;
    var exponent = e - shift;
    if n % 3u == 2u {
        q += 1u;
    }
    if q == 0x1000000u {
        q = 0x800000u;
        exponent += 1u;
    }
    return bitcast<f32>((exponent << 23u) | (q & 0x7fffffu));
}

// `sum / n` of the processor for the n ≤ 4 neighbours of `spread`
fn mean(sum: f32, n: u32) -> f32 {
    if n == 1u {
        return sum;
    }
    if n == 2u {
        return sum * 0.5f;
    }
    if n == 4u {
        return sum * 0.25f;
    }
    return third(sum);
}

// `(c.clamp(0, 1) * 255 + 0.5) as u8`: a colour that is not a number ends 0
fn byte(c: f32) -> u32 {
    if is_nan(bitcast<u32>(c)) {
        return 0u;
    }
    return min(u32(clamp(c, 0.0f, 1.0f) * 255.0f + 0.5f), 255u);
}

fn pack(r: u32, g: u32, b: u32, a: u32) -> u32 {
    return r | (g << 8u) | (b << 16u) | (a << 24u);
}

fn channel(p: u32, k: u32) -> u32 {
    return (p >> (8u * k)) & 255u;
}

// ---------------------------------------------------------------------------------------------
// The lowest heights of the parts (`Frame::far`): the deepest depth drawn in each part of the
// block, then in the two pixels around it (each counting for the nearest part). A 16 × 16 group
// lies in one part (a part is 256 pixels wide for a block of 2 048).

var<workgroup> deepest: atomic<u32>;

@compute @workgroup_size(16, 16)
fn floor_block(@builtin(global_invocation_id) id: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    if li == 0u {
        atomicStore(&deepest, 0u);
    }
    workgroupBarrier();
    if id.x < P.px && id.y < P.px {
        let d = textureLoad(depth, vec2<i32>(i32(id.x), i32(id.y)), 0);
        if d < 1.0f {
            atomicMax(&deepest, bitcast<u32>(d * P.far));
        }
    }
    workgroupBarrier();
    if li == 0u && id.x < P.px && id.y < P.px {
        let b = atomicLoad(&deepest);
        if b != 0u {
            atomicMax(&parts[(id.y * 8u / P.px) * 8u + id.x * 8u / P.px], b);
        }
    }
}

@compute @workgroup_size(16, 16)
fn floor_ring(@builtin(global_invocation_id) id: vec3<u32>) {
    let n = i32(P.px);
    if id.x >= P.px + 4u || id.y >= P.px + 4u {
        return;
    }
    // relative to the block
    let x = i32(id.x) - 2;
    let y = i32(id.y) - 2;
    if x >= 0 && x < n && y >= 0 && y < n {
        return;
    }
    let fx = i32(P.wx) + x;
    let fy = i32(P.wy) + y;
    if fx < 0 || fy < 0 || fx >= i32(P.fw) || fy >= i32(P.fh) {
        return;
    }
    let d = textureLoad(around, vec2<i32>(fx, fy), 0);
    if d < 1.0f {
        let px = min(u32(clamp(x, 0, n - 1)) * 8u / P.px, 7u);
        let py = min(u32(clamp(y, 0, n - 1)) * 8u / P.px, 7u);
        atomicMax(&parts[py * 8u + px], bitcast<u32>(d * P.far));
    }
}

// ---------------------------------------------------------------------------------------------
// The heights the shading reads (`Frame::shade`): those drawn around the block, the block's own
// in the middle; nothing drawn: minus infinity.

@compute @workgroup_size(16, 16)
fn compose(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.fw || id.y >= P.fh {
        return;
    }
    var d: f32;
    if id.x >= P.wx && id.x < P.wx + P.px && id.y >= P.wy && id.y < P.wy + P.px {
        d = textureLoad(depth, vec2<i32>(i32(id.x - P.wx), i32(id.y - P.wy)), 0);
    } else {
        d = textureLoad(around, vec2<i32>(i32(id.x), i32(id.y)), 0);
    }
    var h = NEG_INF;
    if d < 1.0f {
        h = bitcast<u32>(P.top - d * P.far);
    }
    heights[id.y * P.fw + id.x] = h;
}

// ---------------------------------------------------------------------------------------------
// The block in 8 bits: its colours times the factors (`Frame::shade`), opaque where something is
// drawn (`Raster::rgba`). Written to `dst`.

@compute @workgroup_size(16, 16)
fn rgba(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.px || id.y >= P.px {
        return;
    }
    let xy = vec2<i32>(i32(id.x), i32(id.y));
    let i = id.y * P.px + id.x;
    var c = textureLoad(colour, xy, 0).rgb;
    let d = textureLoad(depth, xy, 0);
    if P.lit != 0u {
        let f = factors[i];
        c = vec3<f32>(c.x * f, c.y * f, c.z * f);
    }
    var a = 0u;
    if d < 1.0f {
        a = 255u;
    }
    dst[i] = pack(byte(c.x), byte(c.y), byte(c.z), a);
}

// ---------------------------------------------------------------------------------------------
// One pass of `fill_holes`: an empty pixel with three drawn neighbours or more (of eight) takes
// their mean. From `src` to `dst`.

@compute @workgroup_size(16, 16)
fn holes(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.px || id.y >= P.px {
        return;
    }
    let i = id.y * P.px + id.x;
    let p = src[i];
    if (p >> 24u) != 0u {
        dst[i] = p;
        return;
    }
    var sum = vec3<u32>(0u, 0u, 0u);
    var n = 0u;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            if dx == 0 && dy == 0 {
                continue;
            }
            let x = i32(id.x) + dx;
            let y = i32(id.y) + dy;
            if x < 0 || y < 0 || x >= i32(P.px) || y >= i32(P.px) {
                continue;
            }
            let q = src[u32(y) * P.px + u32(x)];
            if (q >> 24u) != 0u {
                sum += vec3<u32>(channel(q, 0u), channel(q, 1u), channel(q, 2u));
                n += 1u;
            }
        }
    }
    if n >= 3u {
        dst[i] = pack(sum.x / n, sum.y / n, sum.z / n, 255u);
    } else {
        dst[i] = p;
    }
}

// ---------------------------------------------------------------------------------------------
// The ground under the water (`water::spread`): the block's depths (metres, infinite where
// nothing is), then four passes in which a pixel without one takes the mean of its four
// neighbours that have one, in the processor's order (west, east, north, south). Bits.

@compute @workgroup_size(16, 16)
fn depths(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.px || id.y >= P.px {
        return;
    }
    let d = textureLoad(depth, vec2<i32>(i32(id.x), i32(id.y)), 0);
    var g = INF;
    if d < 1.0f {
        g = bitcast<u32>(d * P.far);
    }
    gdst[id.y * P.px + id.x] = g;
}

// `a + b` rounded there, before anything else is added: a card's compiler may reorder a sum of
// floats (HLSL has no strict order), the processor adds one value after the other. The integers
// between two additions keep them apart.
fn add(a: f32, b: f32) -> f32 {
    return bitcast<f32>(bitcast<u32>(a + b) | P.zero);
}

@compute @workgroup_size(16, 16)
fn spread(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.px || id.y >= P.px {
        return;
    }
    let w = P.px;
    let i = id.y * w + id.x;
    let v = gsrc[i];
    if finite(v) {
        gdst[i] = v;
        return;
    }
    var sum = 0.0f;
    var n = 0u;
    if id.x > 0u && finite(gsrc[i - 1u]) {
        sum = add(sum, bitcast<f32>(gsrc[i - 1u]));
        n += 1u;
    }
    if id.x + 1u < w && finite(gsrc[i + 1u]) {
        sum = add(sum, bitcast<f32>(gsrc[i + 1u]));
        n += 1u;
    }
    if id.y > 0u && finite(gsrc[i - w]) {
        sum = add(sum, bitcast<f32>(gsrc[i - w]));
        n += 1u;
    }
    if id.y + 1u < w && finite(gsrc[i + w]) {
        sum = add(sum, bitcast<f32>(gsrc[i + w]));
        n += 1u;
    }
    if n > 0u {
        gdst[i] = bitcast<u32>(mean(sum, n));
    } else {
        gdst[i] = v;
    }
}

// ---------------------------------------------------------------------------------------------
// `Water::flood`. The surface over each pixel from the ranges the processor made (no float
// computed here: `Water::spans`), and what the pixel is; the ground is in `gdst`.

@compute @workgroup_size(16, 16)
fn water_state(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.px || id.y >= P.px {
        return;
    }
    let i = id.y * P.px + id.x;
    var s = NONE;
    if P.all_open != 0u {
        s = 0.0f;
    } else {
        if P.open_test != 0u {
            var inside = false;
            for (var a = 0u; a < P.areas; a++) {
                let r = P.area[a];
                if id.x >= r.x && id.x < r.y && id.y >= r.z && id.y < r.w {
                    inside = true;
                }
            }
            if !inside {
                s = 0.0f;
            }
        }
        for (var q = 0u; q < P.quads; q++) {
            let piece = quads[q];
            if id.y >= piece.y && id.y < piece.z {
                let r = rows[piece.w + id.y - piece.y];
                if id.x >= r.x && id.x < r.y {
                    s = max(s, bitcast<f32>(piece.x));
                }
            }
        }
    }
    surface[i] = s;
    var st = 0u;
    if s > -1e29f {
        let g = gdst[i];
        if !finite(g) {
            st = 2u;
        } else if P.top - bitcast<f32>(g) < s - 0.02f {
            st = 1u;
        }
    }
    state[i] = st;
}

// The water with no ground under it that touches the edge of the block or water over the ground
// is the sea (3): the first ones.
@compute @workgroup_size(16, 16)
fn water_seed(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.px || id.y >= P.px {
        return;
    }
    let w = P.px;
    let i = id.y * w + id.x;
    if state[i] != 2u {
        return;
    }
    let edge = id.x == 0u || id.y == 0u || id.x + 1u == w || id.y + 1u == w;
    if edge || state[i - 1u] == 1u || state[i + 1u] == 1u || state[i - w] == 1u || state[i + w] == 1u {
        state[i] = 3u;
    }
}

// The sea spreads through the water with no ground under it: along each row, both ways (one
// thread per row), then along each column. A round of both spreads it around one turn of a
// path; the rounds are repeated (`water_next` stops them once one spreads nothing), and the last
// one says whether it still spread (`status[1]`).
@compute @workgroup_size(64)
fn water_rows(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.px {
        return;
    }
    let w = P.px;
    let start = id.x * w;
    var changed = false;
    var sea = false;
    for (var x = 0u; x < w; x++) {
        let s = state[start + x];
        if s == 3u {
            sea = true;
        } else if s == 2u {
            if sea {
                state[start + x] = 3u;
                changed = true;
            }
        } else {
            sea = false;
        }
    }
    sea = false;
    for (var x = w; x > 0u; x--) {
        let s = state[start + x - 1u];
        if s == 3u {
            sea = true;
        } else if s == 2u {
            if sea {
                state[start + x - 1u] = 3u;
                changed = true;
            }
        } else {
            sea = false;
        }
    }
    if changed {
        atomicOr(&status[0], 1u);
    }
}

@compute @workgroup_size(64)
fn water_columns(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.px {
        return;
    }
    let w = P.px;
    var changed = false;
    var sea = false;
    for (var y = 0u; y < w; y++) {
        let k = y * w + id.x;
        let s = state[k];
        if s == 3u {
            sea = true;
        } else if s == 2u {
            if sea {
                state[k] = 3u;
                changed = true;
            }
        } else {
            sea = false;
        }
    }
    sea = false;
    for (var y = w; y > 0u; y--) {
        let k = (y - 1u) * w + id.x;
        let s = state[k];
        if s == 3u {
            sea = true;
        } else if s == 2u {
            if sea {
                state[k] = 3u;
                changed = true;
            }
        } else {
            sea = false;
        }
    }
    if changed {
        atomicOr(&status[0], 1u);
    }
}

// After a round of the sweeps: the next one runs only if this one spread the sea (the workgroups of
// the next dispatches, `turns`: none once it is settled), and whether it did is kept.
@group(1) @binding(0) var<storage, read_write> turns: array<u32>;

@compute @workgroup_size(1)
fn water_next() {
    let spread = atomicLoad(&status[0]);
    atomicStore(&status[0], 0u);
    atomicStore(&status[1], spread);
    if spread == 0u {
        turns[0] = 0u;
    }
}

// The colours of the water (`water::tone`): over the ground, the deeper the more opaque and the
// darker; the sea, deep water. Into `dst`.
@compute @workgroup_size(16, 16)
fn water(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.px || id.y >= P.px {
        return;
    }
    let i = id.y * P.px + id.x;
    let st = state[i];
    if st == 3u {
        dst[i] = P.deep;
        return;
    }
    if st != 1u {
        return;
    }
    let deep = surface[i] - (P.top - bitcast<f32>(gdst[i]));
    let opacity = clamp(0.94f * (1.0f - exp(-quotient(deep + 0.15f, 2.5f, 1.0f / 2.5f))), 0.0f, 1.0f);
    let k = 1.0f - exp(-quotient(deep, 12.0f, 1.0f / 12.0f));
    let p = dst[i];
    var out = vec3<u32>(0u, 0u, 0u);
    for (var c = 0u; c < 3u; c++) {
        let tone = P.shallow[c] + (P.deep_colour[c] - P.shallow[c]) * k;
        var under = tone;
        if (p >> 24u) > 0u {
            under = quotient(f32(channel(p, c)), 255.0f, 1.0f / 255.0f);
        }
        out[c] = byte(under + (tone - under) * opacity);
    }
    dst[i] = pack(out.x, out.y, out.z, 255u);
}

// ---------------------------------------------------------------------------------------------
// `tiles::halve`: each pixel the mean of 2 × 2, weighted by alpha (the plain mean when all four
// are opaque). Zoom 8 from the block (`dst`), zoom 7 from zoom 8.

fn mean4(a: u32, b: u32, c: u32, d: u32) -> u32 {
    if ((a >> 24u) & (b >> 24u) & (c >> 24u) & (d >> 24u)) == 255u {
        var out = 255u << 24u;
        for (var k = 0u; k < 3u; k++) {
            out |= ((channel(a, k) + channel(b, k) + channel(c, k) + channel(d, k)) / 4u) << (8u * k);
        }
        return out;
    }
    let alpha = vec4<u32>(a >> 24u, b >> 24u, c >> 24u, d >> 24u);
    let total = alpha.x + alpha.y + alpha.z + alpha.w;
    if total == 0u {
        return 0u;
    }
    var out = (total / 4u) << 24u;
    for (var k = 0u; k < 3u; k++) {
        let sum = channel(a, k) * alpha.x + channel(b, k) * alpha.y + channel(c, k) * alpha.z + channel(d, k) * alpha.w;
        out |= (sum / total) << (8u * k);
    }
    return out;
}

@compute @workgroup_size(16, 16)
fn halve_8(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = P.px;
    let half = w / 2u;
    if id.x >= half || id.y >= half {
        return;
    }
    let top = id.y * 2u * w + id.x * 2u;
    let bottom = top + w;
    z8[id.y * half + id.x] = mean4(dst[top], dst[top + 1u], dst[bottom], dst[bottom + 1u]);
}

@compute @workgroup_size(16, 16)
fn halve_7(@builtin(global_invocation_id) id: vec3<u32>) {
    let w = P.px / 2u;
    let half = w / 2u;
    if id.x >= half || id.y >= half {
        return;
    }
    let top = id.y * 2u * w + id.x * 2u;
    let bottom = top + w;
    z7[id.y * half + id.x] = mean4(z8[top], z8[top + 1u], z8[bottom], z8[bottom + 1u]);
}
