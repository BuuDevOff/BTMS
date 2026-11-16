//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

// The factors of `shade::factors` (render/shade.rs) on the graphics card, one entry point per step
// of the processor's code, run in this order (render/gpu/shade.rs):
//
//   patches, cells, along_x, walls, rows_max, along_y, columns_max, relief_pass, shadows, shade
//
// Every step does what the processor does, operation for operation and in the same order where
// it sums (one thread per patch or cell walks its pixels row by row, as the processor's loop
// meets them), so that most results are equal to the bit. What differs:
//
// - the sweeps of the shadows and of the wall feet: the processor keeps a queue of falling
//   values along each line (one line after the other); here every pixel walks back along its
//   line over the window of the reach, and finds the same two occluders (the highest, the latest
//   among equals; the nearest that rises above the pixel). Their heights are compared relative to
//   the pixel in 32-bit floats (the processor: absolute, in 64-bit), a few micrometres apart;
// - divisions and square roots, which a card may round differently in the last bit.
//
// Nothing that is not a number goes through the card's arithmetic: heights are read as bits and
// tested (`finite`), and what the processor leaves as NaN or minus infinity (a patch or a cell
// with nothing drawn) is `NONE` here, a finite number below anything real.

struct Params {
    width: u32,
    height: u32,
    wx: u32,
    wy: u32,
    ww: u32,
    wh: u32,
    rx0: u32,
    ry0: u32,
    rx1: u32,
    ry1: u32,
    pw: u32,
    ph: u32,
    cw: u32,
    ch: u32,
    // 1: the slope (and with it the patches), 2: the cast shadows, 4: the wall feet
    flags: u32,
    canopy_radius: u32,
    metres: f32,
    limit: f32,
    kx: f32,
    ky: f32,
    strength: f32,
    foot_strength: f32,
    patch_metres: f32,
    wall: f32,
    // the sweep of the shadows: lines along x, toward the growing coordinates, steps, rise per step
    s_x_major: u32,
    s_plus: u32,
    s_window: u32,
    s_rise: f32,
    // where its fades start in the tables
    s_fade: u32,
    // the pixels the sweep computes: the window and one pixel around, within the picture
    ax0: u32,
    ay0: u32,
    aw: u32,
    ah: u32,
    foot_radius: u32,
    // where the eight directions of the wall feet start in the tables (6 words each)
    foot_dirs: u32,
    // where the offsets of the lines start in the tables
    s_offsets: u32,
    pad0: u32,
    pad1: u32,
    pad2: u32,
    pad3: u32,
}

@group(0) @binding(0) var<uniform> P: Params;
// the heights of the picture, as bits (the processor's NaN and infinities among them)
@group(0) @binding(1) var<storage, read> heights: array<u32>;
// the offsets and fades of the sweeps, the directions of the wall feet
@group(0) @binding(2) var<storage, read> tables: array<u32>;
// per patch: mean slope toward x and y, level, share of broken steps
@group(0) @binding(3) var<storage, read_write> own: array<vec4<f32>>;
// per patch: the same averaged along x, then along y (slope x, slope y, level)
@group(0) @binding(4) var<storage, read_write> along: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> smoothed: array<vec4<f32>>;
// per patch: the smooth slope (x, y), how much it is foliage, the highest level around
@group(0) @binding(6) var<storage, read_write> relief: array<vec4<f32>>;
// per cell: the mean height, how much it is hemmed in by walls, the highest within the radius
@group(0) @binding(7) var<storage, read_write> mean: array<f32>;
@group(0) @binding(8) var<storage, read_write> hemmed: array<f32>;
@group(0) @binding(9) var<storage, read_write> rows: array<f32>;
@group(0) @binding(10) var<storage, read_write> highest: array<f32>;
// per pixel of the window and one around: how much it is hidden from the sun
@group(0) @binding(11) var<storage, read_write> hidden: array<f32>;
// per pixel of the window: the factor
@group(0) @binding(12) var<storage, read_write> factors: array<f32>;

// What the processor leaves as NaN or minus infinity.
const NONE: f32 = -1e30f;

fn finite(bits: u32) -> bool {
    return (bits & 0x7f800000u) != 0x7f800000u;
}

fn known(v: f32) -> bool {
    return v > -1e29f;
}

fn smooth_step(t: f32) -> f32 {
    let u = clamp(t, 0.0f, 1.0f);
    return u * u * (3.0f - 2.0f * u);
}

// `shade::gradient`: the mean of the steps to and from the pixel, or the one that is not a wall,
// or none; a step to nothing (`ok` false) is not within the limit either.
fn gradient(before: f32, before_ok: bool, after: f32, after_ok: bool, limit: f32) -> f32 {
    let b = before_ok && abs(before) <= limit;
    let a = after_ok && abs(after) <= limit;
    if b && a {
        return (before + after) * 0.5f;
    }
    if b {
        return before;
    }
    if a {
        return after;
    }
    return 0.0f;
}

// A patch, a cell of the region: the processor counts their pixels.
fn in_region(x0: u32, y0: u32) -> bool {
    return x0 >= P.rx0 && x0 < P.rx1 && y0 >= P.ry0 && y0 < P.ry1;
}

// `Relief::new`, first part: per patch, the lowest and highest heights, the steps to the next
// pixel (east, south) that are slopes and those that are broken, then the level.
@compute @workgroup_size(8, 8)
fn patches(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.pw || id.y >= P.ph {
        return;
    }
    let p = id.y * P.pw + id.x;
    let x0 = id.x * 8u;
    let y0 = id.y * 8u;
    if !in_region(x0, y0) {
        own[p] = vec4<f32>(0.0f, 0.0f, NONE, 0.0f);
        return;
    }
    let x1 = min(x0 + 8u, P.width);
    let y1 = min(y0 + 8u, P.height);
    var low = 3.0e38f;
    var high = -3.0e38f;
    var count = 0u;
    var steps = vec2<f32>(0.0f, 0.0f);
    var whole = vec2<u32>(0u, 0u);
    var broken = 0u;
    for (var y = y0; y < y1; y++) {
        for (var x = x0; x < x1; x++) {
            let i = y * P.width + x;
            let bits = heights[i];
            if !finite(bits) {
                continue;
            }
            let h = bitcast<f32>(bits);
            low = min(low, h);
            high = max(high, h);
            count += 1u;
            if x + 1u < P.width {
                let to = heights[i + 1u];
                if finite(to) {
                    let d = bitcast<f32>(to) - h;
                    if abs(d) <= P.limit {
                        steps.x += d;
                        whole.x += 1u;
                    } else {
                        broken += 1u;
                    }
                }
            }
            if y + 1u < P.height {
                let to = heights[i + P.width];
                if finite(to) {
                    let d = bitcast<f32>(to) - h;
                    if abs(d) <= P.limit {
                        steps.y += d;
                        whole.y += 1u;
                    } else {
                        broken += 1u;
                    }
                }
            }
        }
    }
    if count == 0u {
        own[p] = vec4<f32>(0.0f, 0.0f, NONE, 0.0f);
        return;
    }
    // the mean height; when the pixels are spread over more than a metre, of those on the more
    // crowded side of the middle
    let middle = (low + high) / 2.0f;
    let split = high - low > 1.0f;
    var under = 0.0f;
    var over = 0.0f;
    var n_under = 0u;
    var n_over = 0u;
    for (var y = y0; y < y1; y++) {
        for (var x = x0; x < x1; x++) {
            let bits = heights[y * P.width + x];
            if !finite(bits) {
                continue;
            }
            let h = bitcast<f32>(bits);
            if split && h > middle {
                over += h;
                n_over += 1u;
            } else {
                under += h;
                n_under += 1u;
            }
        }
    }
    var level = 0.0f;
    if n_over > n_under {
        level = over / f32(n_over);
    } else {
        level = under / f32(n_under);
    }
    var gx = 0.0f;
    if whole.x > 0u {
        gx = steps.x / f32(whole.x) / P.metres;
    }
    var gy = 0.0f;
    if whole.y > 0u {
        gy = steps.y / f32(whole.y) / P.metres;
    }
    own[p] = vec4<f32>(gx, gy, level, f32(broken) / f32(2u * count));
}

// The patches on the same level (within a metre) averaged, along x.
@compute @workgroup_size(8, 8)
fn along_x(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.pw || id.y >= P.ph {
        return;
    }
    let p = id.y * P.pw + id.x;
    let centre = own[p].z;
    if !known(centre) {
        along[p] = vec4<f32>(0.0f, 0.0f, NONE, 0.0f);
        return;
    }
    var total = vec3<f32>(0.0f, 0.0f, 0.0f);
    var n = 0u;
    let first = select(0u, id.x - 4u, id.x >= 4u);
    let last = min(id.x + 5u, P.pw);
    for (var j = first; j < last; j++) {
        let v = own[id.y * P.pw + j];
        if known(v.z) && abs(v.z - centre) <= 1.0f {
            total += v.xyz;
            n += 1u;
        }
    }
    along[p] = vec4<f32>(total / f32(n), 0.0f);
}

// Then along y (the levels compared are still the patches' own).
@compute @workgroup_size(8, 8)
fn along_y(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.pw || id.y >= P.ph {
        return;
    }
    let p = id.y * P.pw + id.x;
    let centre = own[p].z;
    if !known(centre) {
        smoothed[p] = vec4<f32>(0.0f, 0.0f, NONE, 0.0f);
        return;
    }
    var total = vec3<f32>(0.0f, 0.0f, 0.0f);
    var n = 0u;
    let first = select(0u, id.y - 4u, id.y >= 4u);
    let last = min(id.y + 5u, P.ph);
    for (var j = first; j < last; j++) {
        let q = j * P.pw + id.x;
        let level = own[q].z;
        if known(level) && abs(level - centre) <= 1.0f {
            total += along[q].xyz;
            n += 1u;
        }
    }
    smoothed[p] = vec4<f32>(total / f32(n), 0.0f);
}

// The averaged level of a patch, NONE beyond the grid.
fn surface(x: i32, y: i32) -> f32 {
    if x < 0 || y < 0 || x >= i32(P.pw) || y >= i32(P.ph) {
        return NONE;
    }
    return smoothed[u32(y) * P.pw + u32(x)].z;
}

// `Relief::new`, last part: foliage from the broken steps around, the smooth slope (the canopy's
// in foliage), the highest level within a metre.
@compute @workgroup_size(8, 8)
fn relief_pass(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.pw || id.y >= P.ph {
        return;
    }
    let p = id.y * P.pw + id.x;
    var total = 0.0f;
    var n = 0u;
    for (var j = select(0u, id.y - 1u, id.y > 0u); j < min(id.y + 2u, P.ph); j++) {
        for (var i = select(0u, id.x - 1u, id.x > 0u); i < min(id.x + 2u, P.pw); i++) {
            total += own[j * P.pw + i].w;
            n += 1u;
        }
    }
    let rough = smooth_step((total / f32(n) - 0.06f) / (0.18f - 0.06f));
    let s = smoothed[p];
    var slope = vec2<f32>(0.0f, 0.0f);
    if known(s.z) {
        slope = s.xy;
        if rough > 0.0f {
            let h = s.z;
            let x = i32(id.x);
            let y = i32(id.y);
            let west = surface(x - 1, y);
            let east = surface(x + 1, y);
            let north = surface(x, y - 1);
            let south = surface(x, y + 1);
            var cx = gradient(h - west, known(west), east - h, known(east), P.wall) / P.patch_metres;
            var cy = gradient(h - north, known(north), south - h, known(south), P.wall) / P.patch_metres;
            if abs(cx) < 1e-4f {
                cx = 0.0f;
            }
            if abs(cy) < 1e-4f {
                cy = 0.0f;
            }
            slope = vec2<f32>(s.x + rough * (cx - s.x), s.y + rough * (cy - s.y));
        }
    }
    var top = NONE;
    let r = P.canopy_radius;
    for (var j = select(0u, id.y - r, id.y >= r); j < min(id.y + r + 1u, P.ph); j++) {
        for (var i = select(0u, id.x - r, id.x >= r); i < min(id.x + r + 1u, P.pw); i++) {
            top = max(top, own[j * P.pw + i].z);
        }
    }
    relief[p] = vec4<f32>(slope, rough, top);
}

// `Foot::new`, first part: the mean height of each cell.
@compute @workgroup_size(8, 8)
fn cells(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.cw || id.y >= P.ch {
        return;
    }
    let c = id.y * P.cw + id.x;
    let x0 = id.x * 4u;
    let y0 = id.y * 4u;
    if !in_region(x0, y0) {
        mean[c] = NONE;
        return;
    }
    var sum = 0.0f;
    var n = 0u;
    for (var y = y0; y < min(y0 + 4u, P.height); y++) {
        for (var x = x0; x < min(x0 + 4u, P.width); x++) {
            let bits = heights[y * P.width + x];
            if finite(bits) {
                sum += bitcast<f32>(bits);
                n += 1u;
            }
        }
    }
    if n > 0u {
        mean[c] = sum / f32(n);
    } else {
        mean[c] = NONE;
    }
}

// `Foot::new`: from eight directions, does something rise above the slope within the radius?
@compute @workgroup_size(8, 8)
fn walls(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.cw || id.y >= P.ch {
        return;
    }
    let c = id.y * P.cw + id.x;
    let here = mean[c];
    if !known(here) {
        hemmed[c] = 0.0f;
        return;
    }
    var sum = 0.0f;
    for (var k = 0u; k < 8u; k++) {
        let at = P.foot_dirs + k * 6u;
        let x_major = tables[at] != 0u;
        let plus = tables[at + 1u] != 0u;
        let slope = bitcast<i32>(tables[at + 2u]);
        let window = tables[at + 3u];
        let rise = bitcast<f32>(tables[at + 4u]);
        let fade = tables[at + 5u];
        var a = i32(id.x);
        var b = i32(id.y);
        var la = i32(P.cw);
        var lb = i32(P.ch);
        if !x_major {
            a = i32(id.y);
            b = i32(id.x);
            la = i32(P.ch);
            lb = i32(P.cw);
        }
        var has_best = false;
        var best = 0.0f;
        var d_best = 0u;
        var has_near = false;
        var near = 0.0f;
        var d_near = 0u;
        for (var d = 1u; d <= window; d++) {
            let a2 = select(a - i32(d), a + i32(d), plus);
            if a2 < 0 || a2 >= la {
                break;
            }
            let b2 = b + slope * (a2 - a);
            if b2 < 0 || b2 >= lb {
                break;
            }
            var q = u32(a2) * P.cw + u32(b2);
            if x_major {
                q = u32(b2) * P.cw + u32(a2);
            }
            let m = mean[q];
            if !known(m) {
                continue;
            }
            let g = (m - here) - f32(d) * rise;
            if !has_best || g > best {
                has_best = true;
                best = g;
                d_best = d;
            }
            if !has_near && g > 0.0f {
                has_near = true;
                near = g;
                d_near = d;
            }
        }
        var amount = 0.0f;
        if has_best {
            amount = clamp((best - 0.0f) / (0.5f - 0.0f), 0.0f, 1.0f) * bitcast<f32>(tables[fade + d_best]);
        }
        if has_near {
            amount = max(amount, clamp((near - 0.0f) / (0.5f - 0.0f), 0.0f, 1.0f) * bitcast<f32>(tables[fade + d_near]));
        }
        sum += amount;
    }
    hemmed[c] = min(sum * 1.6f / 8.0f, 1.0f);
}

// The highest mean within the radius of each cell (a square): along x…
@compute @workgroup_size(8, 8)
fn rows_max(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.cw || id.y >= P.ch {
        return;
    }
    let r = P.foot_radius;
    var top = NONE;
    for (var i = select(0u, id.x - r, id.x >= r); i < min(id.x + r + 1u, P.cw); i++) {
        top = max(top, mean[id.y * P.cw + i]);
    }
    rows[id.y * P.cw + id.x] = top;
}

// … then along y.
@compute @workgroup_size(8, 8)
fn columns_max(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.cw || id.y >= P.ch {
        return;
    }
    let r = P.foot_radius;
    var top = NONE;
    for (var j = select(0u, id.y - r, id.y >= r); j < min(id.y + r + 1u, P.ch); j++) {
        top = max(top, rows[j * P.cw + id.x]);
    }
    highest[id.y * P.cw + id.x] = top;
}

// `shade::sweep` of the sun: how much something toward the sun hides each pixel of the window
// (and of one pixel around it). Along its line, back toward the sun over the reach: the highest
// occluder (the latest among equals, as the processor's queue keeps it) and the nearest one that
// rises above the pixel; a leaf is hidden only by what rises above the canopy around it.
@compute @workgroup_size(64, 4)
fn shadows(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.aw || id.y >= P.ah {
        return;
    }
    let x = P.ax0 + id.x;
    let y = P.ay0 + id.y;
    let k = id.y * P.aw + id.x;
    let bits = heights[y * P.width + x];
    if !finite(bits) {
        hidden[k] = 0.0f;
        return;
    }
    let h = bitcast<f32>(bits);
    let r = relief[(y / 8u) * P.pw + x / 8u];
    let lift = r.z * clamp(r.w - h, 0.0f, 2.5f);
    let x_major = P.s_x_major != 0u;
    let plus = P.s_plus != 0u;
    var a = i32(x);
    var b = i32(y);
    var la = i32(P.width);
    var lb = i32(P.height);
    if !x_major {
        a = i32(y);
        b = i32(x);
        la = i32(P.height);
        lb = i32(P.width);
    }
    let offset = bitcast<i32>(tables[P.s_offsets + u32(a)]);
    var has_best = false;
    var best = 0.0f;
    var d_best = 0u;
    var has_near = false;
    var near = 0.0f;
    var d_near = 0u;
    for (var d = 1u; d <= P.s_window; d++) {
        let a2 = select(a - i32(d), a + i32(d), plus);
        if a2 < 0 || a2 >= la {
            break;
        }
        let b2 = b + (bitcast<i32>(tables[P.s_offsets + u32(a2)]) - offset);
        if b2 < 0 || b2 >= lb {
            break;
        }
        var q = u32(a2) * P.width + u32(b2);
        if x_major {
            q = u32(b2) * P.width + u32(a2);
        }
        let other = heights[q];
        if !finite(other) {
            continue;
        }
        let g = (bitcast<f32>(other) - h) - f32(d) * P.s_rise;
        if !has_best || g > best {
            has_best = true;
            best = g;
            d_best = d;
        }
        if !has_near && g > 0.0f {
            has_near = true;
            near = g;
            d_near = d;
        }
    }
    var amount = 0.0f;
    if has_best {
        amount = clamp((best - lift - 0.03f) / (0.15f - 0.03f), 0.0f, 1.0f) * bitcast<f32>(tables[P.s_fade + d_best]);
    }
    if has_near {
        amount = max(amount, clamp((near - lift - 0.03f) / (0.15f - 0.03f), 0.0f, 1.0f) * bitcast<f32>(tables[P.s_fade + d_near]));
    }
    hidden[k] = amount;
}

// Where a pixel falls among the cells of a coarse grid: two cells and the weight of the second.
struct Coarse {
    a: u32,
    b: u32,
    t: f32,
}

fn coarse(p: u32, cells: u32, size: f32) -> Coarse {
    let f = (f32(p) + 0.5f) / size - 0.5f;
    let a = floor(f);
    return Coarse(min(u32(max(a, 0.0f)), cells - 1u), min(u32(max(a + 1.0f, 0.0f)), cells - 1u), f - a);
}

// The end of `shade::factors`: the factor of each pixel of the window.
@compute @workgroup_size(64, 4)
fn shade(@builtin(global_invocation_id) id: vec3<u32>) {
    if id.x >= P.ww || id.y >= P.wh {
        return;
    }
    let px = P.wx + id.x;
    let py = P.wy + id.y;
    let o = id.y * P.ww + id.x;
    let i = py * P.width + px;
    let bits = heights[i];
    if !finite(bits) {
        factors[o] = 1.0f;
        return;
    }
    let h = bitcast<f32>(bits);
    let inside = px > 0u && px + 1u < P.width && py > 0u && py + 1u < P.height;
    var factor = 1.0f;
    if (P.flags & 1u) != 0u {
        // the four neighbours; beyond the picture, nothing
        var west = 0.0f;
        var west_ok = false;
        if px > 0u {
            let b = heights[i - 1u];
            west_ok = finite(b);
            if west_ok {
                west = bitcast<f32>(b);
            }
        }
        var east = 0.0f;
        var east_ok = false;
        if px + 1u < P.width {
            let b = heights[i + 1u];
            east_ok = finite(b);
            if east_ok {
                east = bitcast<f32>(b);
            }
        }
        var north = 0.0f;
        var north_ok = false;
        if py > 0u {
            let b = heights[i - P.width];
            north_ok = finite(b);
            if north_ok {
                north = bitcast<f32>(b);
            }
        }
        var south = 0.0f;
        var south_ok = false;
        if py + 1u < P.height {
            let b = heights[i + P.width];
            south_ok = finite(b);
            if south_ok {
                south = bitcast<f32>(b);
            }
        }
        var gx = gradient(h - west, west_ok, east - h, east_ok, P.limit) / P.metres;
        var gy = gradient(h - north, north_ok, south - h, south_ok, P.limit) / P.metres;
        // the smooth slope around, but where the pixel's own stands out of it (and is not a
        // leaf's): the two rows of patches around mixed first, then the two columns
        let cx = coarse(px, P.pw, 8.0f);
        let cy = coarse(py, P.ph, 8.0f);
        let ra = relief[cy.a * P.pw + cx.a].xyz;
        let rb = relief[cy.b * P.pw + cx.a].xyz;
        let rc = relief[cy.a * P.pw + cx.b].xyz;
        let rd = relief[cy.b * P.pw + cx.b].xyz;
        let first = ra + cy.t * (rb - ra);
        let second = rc + cy.t * (rd - rc);
        let v = first + cx.t * (second - first);
        let dx = gx - v.x;
        let dy = gy - v.y;
        let apart = dx * dx + dy * dy;
        var mine = 0.0f;
        if !(apart <= 0.15f * 0.15f) {
            mine = smooth_step((sqrt(apart) - 0.15f) / (0.4f - 0.15f)) * (1.0f - v.z);
        }
        gx = v.x + mine * dx;
        gy = v.y + mine * dy;
        var lambert = 1.0f;
        if !(gx == 0.0f && gy == 0.0f) {
            lambert = clamp((1.0f - gx * P.kx - gy * P.ky) / sqrt(1.0f + gx * gx + gy * gy), 0.0f, 2.0f);
        }
        var shadow = 0.0f;
        if (P.flags & 2u) != 0u {
            let k = (py - P.ay0) * P.aw + (px - P.ax0);
            if inside {
                // softened over 3 × 3 pixels: the lines of a sweep are one pixel wide
                let a = k - P.aw;
                let c = k + P.aw;
                shadow = (hidden[a - 1u] + hidden[a + 1u] + hidden[c - 1u] + hidden[c + 1u] + 2.0f * (hidden[a] + hidden[c] + hidden[k - 1u] + hidden[k + 1u]) + 4.0f * hidden[k]) / 16.0f;
            } else {
                shadow = hidden[k];
            }
        }
        factor = 1.0f + P.strength * (lambert * (1.0f - shadow) - 1.0f);
    }
    if (P.flags & 4u) != 0u {
        let cx = coarse(px, P.cw, 4.0f);
        let cy = coarse(py, P.ch, 4.0f);
        let ha = hemmed[cy.a * P.cw + cx.a];
        let hb = hemmed[cy.b * P.cw + cx.a];
        let hc = hemmed[cy.a * P.cw + cx.b];
        let hd = hemmed[cy.b * P.cw + cx.b];
        let first = ha + cy.t * (hb - ha);
        let second = hc + cy.t * (hd - hc);
        let hem = first + cx.t * (second - first);
        if hem > 0.0f {
            // only below what stands around: the edge of a roof is not at the foot of its own wall
            let below = clamp((highest[(py / 4u) * P.cw + px / 4u] - h) / 0.5f, 0.0f, 1.0f);
            factor *= 1.0f - P.foot_strength * hem * below;
        }
    }
    factors[o] = max(factor, 0.2f);
}
