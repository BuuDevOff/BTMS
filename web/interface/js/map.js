//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

// The map of the interface: one canvas, drawn by hand, on demand.
//
// - Tiles written by BTMS: zooms 0 to 9, 512 px, side W = 32 · 2^(9 − z) m; the tile (z, tx, ty)
//   covers x in [tx·W, (tx+1)·W) and y in [ty·W, (ty+1)·W), y growing north (up the screen).
// - A continuous zoom: the tiles of the nearest level are drawn, and while they load, what is
//   already known of the coarser levels (or of the next finer one).
// - Over the tiles: the mosaic of a render (one square per 128 m block of its plan), marks the
//   caller points at (the ground of a resource that changed) and the rectangle of the zone the
//   user picks, snapped to those blocks.
//
// The component knows nothing of the server: the caller gives the addresses of the tiles
// (`tileUrls`), tells what changed (`invalidate…`) and listens (`onView`, `onHover`, `onZone`,
// `onMode`). Pixels are never read back from the canvas: tiles may come from another origin.
// Its few words come from ./i18n.js.

import { t, onLanguageChange } from './i18n.js';

/** Side of a block of the render, in metres (one tile of zoom 7). */
export const BLOCK = 128;
/** Finest zoom of the tiles. */
export const ZMAX = 9;
/** Side of a tile, in pixels. */
const PX = 512;
/** Half side of the world the tiles can cover (the four tiles of zoom 0), in metres. */
const WORLD = 16384;
/** CSS pixels per metre: from the whole world in a small window to 4 × the finest tiles. */
const K_MIN = 0.008;
const K_MAX = 64;
/** San Andreas, when nothing better is known. */
export const SAN_ANDREAS = [-4096, -4096, 4608, 8192];

/** Tile states. */
const UNLOADED = 0;
const READY = 1;
const MISSING = 2;
/** Block states of the mosaic. */
const WAITING = 0;
const RUNNING = 1;
const DONE = 2;
const FAILED = 3;
const STATE_NAMES = ['waiting', 'running', 'done', 'failed'];

/** Images kept (a decoded tile is about 1 MB), and loads at the same time. */
const MAX_IMAGES = 280;
const MAX_LOADS = 8;
/** Times a tile is asked again when the server fails to answer for it. */
const MAX_TRIES = 4;
/** How long the veil of a finished block takes to fade, in ms. */
const FADE_MS = 1100;
/**
 * A block just finished is not in the far zooms until their next pass (every 30 s during a
 * render). Until then it shows through its own tile of zoom 7 when blocks are at least this wide
 * on the screen (CSS px), through a thumbnail of that tile below.
 */
const BLOCK_TILE_PX = 72;
/** Side of a thumbnail in pixels at most, and of the sheet that holds those of a render. */
const THUMB_PX = 32;
const SHEET_PX = 4096;
/** Thumbnails loading at the same time, and times one is asked again when the server fails. */
const MAX_THUMB_LOADS = 4;
const MAX_THUMB_TRIES = 3;
/** How long the blocks of the pass before still show fresh, while the far zooms load again, in ms. */
const ZOOMS_GRACE_MS = 4000;
/** Pause between two frames of the mosaic animation, in ms (the render needs the processor). */
const ANIMATION_STEP_MS = 90;

const COLORS = {
  grid: 'rgba(148, 163, 184, 0.07)',
  axes: 'rgba(148, 163, 184, 0.13)',
  waiting: 'rgba(9, 14, 21, 0.62)',
  waitingSolid: 'rgba(52, 70, 92, 0.88)',
  waitingEdge: 'rgba(143, 179, 217, 0.2)',
  running: [208, 168, 92],
  done: [95, 174, 138],
  failed: 'rgba(201, 79, 85, 0.62)',
  failedEdge: 'rgba(233, 163, 166, 0.95)',
  zone: 'rgba(143, 179, 217, 0.1)',
  zoneEdge: '#b9d1ea',
  zoneShadow: 'rgba(3, 5, 8, 0.75)',
  // marks: what changed in a resource (added, changed, removed), and the ground to draw again
  marks: { added: [95, 174, 138], changed: [208, 168, 92], removed: [201, 79, 85], zone: [143, 179, 217] },
};

const clamp = (v, a, b) => Math.min(b, Math.max(a, v));

/**
 * Mounts the map in `host` (a positioned element that the canvas fills).
 *
 * `options.tileUrls(source, z, tx, ty, rev)`: the addresses to try for a tile, in order (an empty
 * list or null: the tile does not exist). `options.extents`: { source: [x1, y1, x2, y2] }, the
 * ground each source covers to begin with (see `setExtent`). `options.onView(view)`,
 * `options.onHover(point | null)`, `options.onZone(zone | null)`, `options.onMode(active)`:
 * what the map tells.
 */
export function createMap(host, options) {
  const canvas = document.createElement('canvas');
  canvas.className = 'map-canvas';
  canvas.tabIndex = 0;
  canvas.setAttribute('role', 'application');
  canvas.setAttribute('aria-label', t('map.ariaLabel'));
  // so that applyTranslations() of ./i18n.js gives the label again in another language
  canvas.setAttribute('data-i18n-aria-label', 'map.ariaLabel');
  const tooltip = document.createElement('div');
  tooltip.className = 'map-tooltip';
  tooltip.hidden = true;
  host.append(canvas, tooltip);
  const ctx = canvas.getContext('2d');

  // ------------------------------------------------------------------------------------- state

  /** Centre of the view in metres, and CSS pixels per metre. */
  const view = { cx: 256, cy: 2048, k: 0.05 };
  /** Size of the canvas in CSS pixels. */
  let width = 1;
  let height = 1;
  let dpr = 1;

  let source = 'map';
  let background = null;
  const revs = Object.create(null);
  /**
   * The ground the tiles of a source cover, when it is known: no tile is asked outside. The caller
   * may give it from the start (`options.extents`), before the first frame asks anything.
   */
  const extents = Object.assign(Object.create(null), options.extents);
  /**
   * key → { key, source, z, tx, ty, img, state, stale, loading, changedWhileLoading, seen, dead,
   * rank, tries, waitUntil }
   */
  const tiles = new Map();
  let loads = 0;
  let frameCount = 0;

  /** The plan of the render shown as a mosaic, or null. */
  let plan = null;
  let mosaicShown = true;
  /** Plan indexes of finished blocks whose thumbnail is to load, and the loads under way. */
  let thumbQueue = [];
  let thumbLoads = 0;

  /** Marks drawn over the tiles: [{ zone: [x1, y1, x2, y2], kind, name }] (see `setMarks`). */
  let marks = [];
  /** The picked zone [x1, y1, x2, y2] (metres, multiples of 128), or null. */
  let zone = null;
  let pickMode = false;
  /** What the pointer is dragging: the view, a new zone, an edge of the zone. */
  let drag = null;
  let hover = null;

  let dirty = false;
  let frameRequest = 0;
  let timer = 0;
  let animation = null;
  /** A framing asked before the canvas had a size. */
  let pendingFit = null;

  // ------------------------------------------------------------------------------------- geometry

  const zoomOf = (k) => Math.log2(k) + 5;
  const worldX = (px) => view.cx + (px - width / 2) / view.k;
  const worldY = (py) => view.cy - (py - height / 2) / view.k;
  const screenX = (x) => width / 2 + (x - view.cx) * view.k;
  const screenY = (y) => height / 2 - (y - view.cy) * view.k;

  function constrain() {
    view.k = clamp(view.k, K_MIN, K_MAX);
    view.cx = clamp(view.cx, -WORLD, WORLD);
    view.cy = clamp(view.cy, -WORLD, WORLD);
  }

  function tellView() {
    if (options.onView) options.onView(readView());
  }

  function readView() {
    return { cx: view.cx, cy: view.cy, k: view.k, zoom: zoomOf(view.k) };
  }

  /** Zooms to `k` (CSS pixels per metre) while the point under (px, py) stays where it is. */
  function zoomAround(px, py, k) {
    const x = worldX(px);
    const y = worldY(py);
    view.k = clamp(k, K_MIN, K_MAX);
    view.cx = x - (px - width / 2) / view.k;
    view.cy = y + (py - height / 2) / view.k;
    constrain();
  }

  // ------------------------------------------------------------------------------------- drawing

  /** Asks for a frame; nothing is drawn while nothing changes. */
  function requestDraw() {
    if (dirty) return;
    dirty = true;
    frameRequest = requestAnimationFrame(draw);
  }

  function draw() {
    dirty = false;
    if (!width || !height) return;
    frameCount += 1;
    const w = canvas.width;
    const h = canvas.height;
    const kd = view.k * dpr;
    const g = {
      w,
      h,
      kd,
      X: (x) => w / 2 + (x - view.cx) * kd,
      Y: (y) => h / 2 - (y - view.cy) * kd,
      x0: view.cx - w / 2 / kd,
      x1: view.cx + w / 2 / kd,
      y0: view.cy - h / 2 / kd,
      y1: view.cy + h / 2 / kd,
    };
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, w, h);
    drawGrid(g);

    // the nearest level, a little in favour of the finer one (a tile is rather shrunk than blown up)
    const z = clamp(Math.round(Math.log2(kd) + 5 + 0.2), 0, ZMAX);
    const queue = [];
    ctx.imageSmoothingEnabled = true;
    ctx.imageSmoothingQuality = 'medium';
    if (background && background !== source) {
      ctx.globalAlpha = 0.3;
      drawLayer(background, z, g, queue, 1);
      ctx.globalAlpha = 1;
    }
    drawLayer(source, z, g, queue, 0);
    if (z < 7) drawFreshBlocks(g, queue);
    drawThumbnails(g);

    const animating = drawMosaic(g);
    drawMarks(g);
    drawZone(g);

    prune();
    startLoads(queue);
    if (animating && !document.hidden && !timer) {
      timer = setTimeout(() => {
        timer = 0;
        requestDraw();
      }, ANIMATION_STEP_MS);
    }
  }

  /** Faint lines of the world: every block when they are wide enough, else every 1024 m and more. */
  function drawGrid(g) {
    let step = BLOCK;
    for (const p of [BLOCK, 1024, 4096, WORLD]) {
      step = p;
      if (p * g.kd >= 36 * dpr) break;
    }
    ctx.beginPath();
    for (let x = Math.ceil(g.x0 / step) * step; x <= g.x1; x += step) {
      if (x === 0) continue;
      const sx = Math.round(g.X(x)) + 0.5;
      ctx.moveTo(sx, 0);
      ctx.lineTo(sx, g.h);
    }
    for (let y = Math.ceil(g.y0 / step) * step; y <= g.y1; y += step) {
      if (y === 0) continue;
      const sy = Math.round(g.Y(y)) + 0.5;
      ctx.moveTo(0, sy);
      ctx.lineTo(g.w, sy);
    }
    ctx.lineWidth = 1;
    ctx.strokeStyle = COLORS.grid;
    ctx.stroke();
    // the axes of the world
    ctx.beginPath();
    const ax = Math.round(g.X(0)) + 0.5;
    const ay = Math.round(g.Y(0)) + 0.5;
    ctx.moveTo(ax, 0);
    ctx.lineTo(ax, g.h);
    ctx.moveTo(0, ay);
    ctx.lineTo(g.w, ay);
    ctx.strokeStyle = COLORS.axes;
    ctx.stroke();
  }

  const keyOf = (src, z, tx, ty) => `${src}/${z}/${tx}/${ty}`;

  function entry(src, z, tx, ty) {
    const key = keyOf(src, z, tx, ty);
    let e = tiles.get(key);
    if (!e) {
      e = { key, source: src, z, tx, ty, img: null, state: UNLOADED, stale: false, loading: false, changedWhileLoading: false, seen: 0, dead: false, rank: 0, tries: 0, waitUntil: 0 };
      tiles.set(key, e);
    }
    return e;
  }

  /** Destination of a tile on the canvas: its edges are rounded, not its size, so tiles never gap. */
  function destRect(z, tx, ty, g) {
    const W = 32 * 2 ** (ZMAX - z);
    const xa = Math.round(g.X(tx * W));
    const xb = Math.round(g.X((tx + 1) * W));
    const ya = Math.round(g.Y((ty + 1) * W));
    const yb = Math.round(g.Y(ty * W));
    return [xa, ya, xb - xa, yb - ya];
  }

  function drawLayer(src, z, g, queue, rank) {
    const W = 32 * 2 ** (ZMAX - z);
    const lim = WORLD / W;
    const tx0 = Math.max(Math.floor(g.x0 / W), -lim);
    const tx1 = Math.min(Math.floor(g.x1 / W), lim - 1);
    const ty0 = Math.max(Math.floor(g.y0 / W), -lim);
    const ty1 = Math.min(Math.floor(g.y1 / W), lim - 1);
    const mx = (g.x0 + g.x1) / 2 / W - 0.5;
    const my = (g.y0 + g.y1) / 2 / W - 0.5;
    const now = performance.now();
    const extent = extents[src];
    for (let ty = ty0; ty <= ty1; ty++) {
      for (let tx = tx0; tx <= tx1; tx++) {
        // outside what the source covers: known to be empty, never asked
        if (extent && ((tx + 1) * W <= extent[0] || tx * W >= extent[2] || (ty + 1) * W <= extent[1] || ty * W >= extent[3])) continue;
        const e = entry(src, z, tx, ty);
        e.seen = frameCount;
        if ((e.state === UNLOADED || e.stale) && !e.loading && now >= e.waitUntil) {
          e.rank = rank * 1e6 + (tx - mx) ** 2 + (ty - my) ** 2;
          queue.push(e);
        }
        if (e.img) {
          const [x, y, w, h] = destRect(z, tx, ty, g);
          ctx.drawImage(e.img, x, y, w, h);
          continue;
        }
        if (e.state === MISSING) continue;
        // not known yet: the part of a coarser tile that covers it...
        let covered = false;
        for (let za = z - 1; za >= 0 && !covered; za--) {
          const f = 2 ** (z - za);
          const atx = Math.floor(tx / f);
          const aty = Math.floor(ty / f);
          const a = tiles.get(keyOf(src, za, atx, aty));
          if (!a || !a.img) continue;
          a.seen = frameCount;
          const c = PX / f;
          const [x, y, w, h] = destRect(z, tx, ty, g);
          ctx.drawImage(a.img, (tx - atx * f) * c, (f - 1 - (ty - aty * f)) * c, c, c, x, y, w, h);
          covered = true;
        }
        // ...or its four finer tiles
        if (!covered && z < ZMAX) {
          for (let j = 0; j < 2; j++) {
            for (let i = 0; i < 2; i++) {
              const c = tiles.get(keyOf(src, z + 1, 2 * tx + i, 2 * ty + j));
              if (!c || !c.img) continue;
              c.seen = frameCount;
              const [x, y, w, h] = destRect(z + 1, 2 * tx + i, 2 * ty + j, g);
              ctx.drawImage(c.img, x, y, w, h);
            }
          }
        }
      }
    }
  }

  /**
   * Whether block i of the plan is finished and not yet in the far zooms shown: finished since
   * their last pass, or during the one before while that pass's tiles load again.
   */
  function isFresh(p, i, now) {
    if (p.state[i] !== DONE) return false;
    const pass = p.passes[i];
    return pass === p.pass || (pass === p.pass - 1 && now - p.passAt < ZOOMS_GRACE_MS);
  }

  /**
   * The far zooms (6 to 0) are only made again every 30 s during a render: until then, a block
   * that was just drawn shows through its own tile of zoom 7, when blocks are wide enough on the
   * screen for that not to mean hundreds of pictures (else through its thumbnail).
   */
  function drawFreshBlocks(g, queue) {
    if (!plan || plan.source !== source || BLOCK * g.kd < BLOCK_TILE_PX * dpr) return;
    const bx0 = Math.floor(g.x0 / BLOCK);
    const bx1 = Math.floor(g.x1 / BLOCK);
    const by0 = Math.floor(g.y0 / BLOCK);
    const by1 = Math.floor(g.y1 / BLOCK);
    const { bx, by } = plan;
    const now = performance.now();
    for (let i = 0; i < plan.n; i++) {
      if (!isFresh(plan, i, now)) continue;
      const x = bx[i];
      const y = by[i];
      if (x < bx0 || x > bx1 || y < by0 || y > by1) continue;
      const e = entry(source, 7, x, y);
      e.seen = frameCount;
      if ((e.state === UNLOADED || e.stale) && !e.loading && performance.now() >= e.waitUntil) {
        e.rank = 0;
        queue.push(e);
      }
      if (e.img) {
        const [px, py, w, h] = destRect(7, x, y, g);
        ctx.drawImage(e.img, px, py, w, h);
      }
    }
  }

  // ------------------------------------------------------------------------------------- thumbnails

  /**
   * Blocks too small on the screen for their own tiles: a block just finished shows through a
   * thumbnail of its tile of zoom 7, loaded once when it finishes, until the far zooms include it.
   * The thumbnails of a render are on one sheet (a canvas of at most 4096 px a side).
   */
  function drawThumbnails(g) {
    const p = plan;
    if (!p || !p.sheet || p.source !== source || BLOCK * g.kd >= BLOCK_TILE_PX * dpr) return;
    const { canvas: c, R, bx0: fx, by1: fy } = p.sheet;
    const bx0 = Math.floor(g.x0 / BLOCK);
    const bx1 = Math.floor(g.x1 / BLOCK);
    const by0 = Math.floor(g.y0 / BLOCK);
    const by1 = Math.floor(g.y1 / BLOCK);
    const now = performance.now();
    for (let i = 0; i < p.n; i++) {
      if (!p.onSheet[i] || !isFresh(p, i, now)) continue;
      const x = p.bx[i];
      const y = p.by[i];
      if (x < bx0 || x > bx1 || y < by0 || y > by1) continue;
      const xa = Math.round(g.X(x * BLOCK));
      const xb = Math.round(g.X((x + 1) * BLOCK));
      const ya = Math.round(g.Y((y + 1) * BLOCK));
      const yb = Math.round(g.Y(y * BLOCK));
      ctx.drawImage(c, (x - fx) * R, (fy - 1 - y) * R, R, R, xa, ya, Math.max(xb - xa, 1), Math.max(yb - ya, 1));
    }
  }

  /** The sheet of the thumbnails of a plan, made when the first one comes; null: too many blocks. */
  function sheetOf(p) {
    if (p.sheet) return p.sheet;
    const [x1, y1, x2, y2] = p.bounds;
    const nx = Math.round((x2 - x1) / BLOCK);
    const ny = Math.round((y2 - y1) / BLOCK);
    const R = Math.min(THUMB_PX, Math.floor(SHEET_PX / Math.max(nx, ny, 1)));
    if (R < 2) return null;
    const c = document.createElement('canvas');
    c.width = nx * R;
    c.height = ny * R;
    p.sheet = { canvas: c, ctx: c.getContext('2d'), R, bx0: Math.round(x1 / BLOCK), by1: Math.round(y2 / BLOCK) };
    return p.sheet;
  }

  /** Gives back the memory of a plan's sheet. */
  function dropSheet(p) {
    if (!p || !p.sheet) return;
    p.sheet.canvas.width = 0;
    p.sheet.canvas.height = 0;
    p.sheet = null;
  }

  /** Starts the thumbnails waiting, oldest first; those the far zooms already include are skipped. */
  function pumpThumbnails() {
    const p = plan;
    while (p && thumbLoads < MAX_THUMB_LOADS && thumbQueue.length) {
      const i = thumbQueue.shift();
      if (p.onSheet[i] || p.state[i] !== DONE || p.passes[i] !== p.pass) continue;
      loadThumbnail(p, i);
    }
  }

  function loadThumbnail(p, i) {
    const sheet = sheetOf(p);
    const urls = sheet && options.tileUrls(p.source, 7, p.bx[i], p.by[i], revs[p.source] || 0);
    if (!urls || !urls.length) return;
    const R = sheet.R;
    thumbLoads += 1;
    loadImage(urls, R, (img, failed) => {
      thumbLoads -= 1;
      if (plan === p) {
        if (img) {
          sheet.ctx.drawImage(img, (p.bx[i] - sheet.bx0) * R, (sheet.by1 - 1 - p.by[i]) * R, R, R);
          p.onSheet[i] = 1;
          requestDraw();
        } else if (failed && p.thumbTries[i] + 1 < MAX_THUMB_TRIES) {
          // the server did not answer: asked again a little later
          p.thumbTries[i] += 1;
          setTimeout(() => {
            if (plan !== p) return;
            thumbQueue.push(i);
            pumpThumbnails();
          }, 1200 * p.thumbTries[i]);
        }
      }
      if (img && img.close) img.close();
      pumpThumbnails();
    });
  }

  /**
   * Loads the first picture of `urls` that exists, at `size` px a side: `finish(img, failed)`, img
   * null when none exists or when the server did not answer (`failed`).
   */
  function loadImage(urls, size, finish) {
    const next = (i) => (i + 1 < urls.length ? attempt(i + 1) : finish(null, false));
    const attempt = (i) => {
      const url = urls[i];
      if (sameOrigin(url)) {
        fetch(url, { cache: 'no-store' })
          .then((response) => {
            if (response.status === 404) return next(i);
            if (!response.ok) return finish(null, true);
            return response
              .blob()
              .then((blob) =>
                // decoded small, off the main thread (a browser without the resize options decodes it whole)
                createImageBitmap(blob, { resizeWidth: size, resizeHeight: size, resizeQuality: 'medium' }).catch(() =>
                  createImageBitmap(blob),
                ),
              )
              .then((bitmap) => finish(bitmap, false));
          })
          .catch(() => finish(null, true));
        return;
      }
      const img = new Image();
      img.decoding = 'async';
      img.onload = () => finish(img, false);
      img.onerror = () => next(i);
      img.src = url;
    };
    attempt(0);
  }

  /** Starts the loads of the frame: the shown source first, the centre first. */
  function startLoads(queue) {
    if (!queue.length || loads >= MAX_LOADS) return;
    queue.sort((a, b) => a.rank - b.rank);
    for (const e of queue) {
      if (loads >= MAX_LOADS) break;
      load(e);
    }
  }

  function load(e) {
    const urls = options.tileUrls(e.source, e.z, e.tx, e.ty, revs[e.source] || 0);
    if (!urls || !urls.length) {
      freeImage(e);
      e.img = null;
      e.state = MISSING;
      e.stale = false;
      return;
    }
    e.loading = true;
    e.changedWhileLoading = false;
    loads += 1;
    // `img`: the picture, or null when the tile does not exist; `failed`: no answer to trust
    const finish = (img, failed) => {
      loads -= 1;
      e.loading = false;
      if (e.dead) {
        if (img && img.close) img.close();
        return;
      }
      if (failed) {
        // The server did not answer, or not as it should: this says nothing of the tile. What is
        // shown stays, and the tile is asked again a little later, a few times.
        e.tries += 1;
        if (e.tries < MAX_TRIES) {
          e.waitUntil = performance.now() + 1200 * e.tries;
          if (e.state !== UNLOADED) e.stale = true;
          setTimeout(requestDraw, 1200 * e.tries + 20);
        } else {
          e.tries = 0;
          e.stale = false;
          if (!e.img) e.state = MISSING;
        }
        requestDraw();
        return;
      }
      // the picture shown until now stays until this point: no blink when a tile is redrawn
      if (e.img !== img) freeImage(e);
      e.img = img;
      e.state = img ? READY : MISSING;
      e.stale = e.changedWhileLoading;
      e.changedWhileLoading = false;
      e.tries = 0;
      requestDraw();
    };
    const next = (i) => (i + 1 < urls.length ? attempt(i + 1) : finish(null, false));
    const attempt = (i) => {
      const url = urls[i];
      if (sameOrigin(url)) {
        // our own server: its status tells an absent tile (404) from a failure, and the picture
        // is decoded off the main thread
        fetch(url, { cache: 'no-store' })
          .then((response) => {
            if (response.status === 404) return next(i);
            if (!response.ok) return finish(null, true);
            return response
              .blob()
              .then((blob) => createImageBitmap(blob))
              .then((bitmap) => finish(bitmap, false));
          })
          .catch(() => finish(null, true));
        return;
      }
      // another origin (tiles of the demonstration mode): a plain image, whose failure can only
      // be read as "not there"
      const img = new Image();
      img.decoding = 'async';
      img.onload = () => {
        // decoding ahead only spares the first drawing a pause; refused, the tile is still there
        if (img.decode) img.decode().then(() => finish(img, false), () => finish(img, false));
        else finish(img, false);
      };
      img.onerror = () => next(i);
      img.src = url;
    };
    attempt(0);
  }

  function sameOrigin(url) {
    try {
      return new URL(url, location.href).origin === location.origin;
    } catch {
      return false;
    }
  }

  /** Gives back the memory of a decoded picture. */
  function freeImage(e) {
    if (e.img && e.img.close) e.img.close();
  }

  /** Bounds the cache: the pictures seen longest ago go first. */
  function prune() {
    if (tiles.size <= MAX_IMAGES) return;
    let images = 0;
    for (const e of tiles.values()) if (e.img) images += 1;
    if (images <= MAX_IMAGES && tiles.size <= 12000) return;
    const old = [...tiles.values()].filter((e) => e.seen !== frameCount && !e.loading);
    old.sort((a, b) => a.seen - b.seen);
    let toRemove = Math.max(images - Math.floor(MAX_IMAGES * 0.8), 0);
    const tooManyKeys = tiles.size > 12000;
    for (const e of old) {
      if (e.img) {
        if (toRemove <= 0) continue;
        toRemove -= 1;
      } else if (!tooManyKeys) {
        continue;
      }
      e.dead = true;
      freeImage(e);
      tiles.delete(e.key);
    }
  }

  // ------------------------------------------------------------------------------------- mosaic

  /** Draws the mosaic; true when it has to be drawn again soon (a block running or fading). */
  function drawMosaic(g) {
    if (!plan || !mosaicShown) return false;
    const side = BLOCK * g.kd;
    const solid = side < 3 * dpr;
    const outlined = side >= 7 * dpr;
    const now = performance.now();
    const bx0 = Math.floor(g.x0 / BLOCK);
    const bx1 = Math.floor(g.x1 / BLOCK);
    const by0 = Math.floor(g.y0 / BLOCK);
    const by1 = Math.floor(g.y1 / BLOCK);
    const { bx, by, state, doneAt } = plan;
    const running = [];
    const fading = [];
    const failed = [];
    let animating = false;

    // blocks waiting: one path, filled once (and outlined once when they are wide enough)
    ctx.beginPath();
    for (let i = 0; i < plan.n; i++) {
      const x = bx[i];
      const y = by[i];
      const s = state[i];
      if (s === RUNNING) animating = true;
      else if (s === DONE && now - doneAt[i] < FADE_MS) animating = true;
      if (x < bx0 || x > bx1 || y < by0 || y > by1) continue;
      if (s === DONE && now - doneAt[i] >= FADE_MS) continue;
      const xa = Math.round(g.X(x * BLOCK));
      const xb = Math.round(g.X((x + 1) * BLOCK));
      const ya = Math.round(g.Y((y + 1) * BLOCK));
      const yb = Math.round(g.Y(y * BLOCK));
      const w = Math.max(xb - xa, 1);
      const h = Math.max(yb - ya, 1);
      if (s === WAITING) ctx.rect(xa, ya, w, h);
      else if (s === RUNNING) running.push(xa, ya, w, h);
      else if (s === DONE) fading.push(xa, ya, w, h, 1 - (now - doneAt[i]) / FADE_MS);
      else failed.push(xa, ya, w, h);
    }
    ctx.fillStyle = solid ? COLORS.waitingSolid : COLORS.waiting;
    ctx.fill();
    if (outlined) {
      ctx.save();
      ctx.translate(0.5, 0.5);
      ctx.lineWidth = 1;
      ctx.strokeStyle = COLORS.waitingEdge;
      ctx.stroke();
      ctx.restore();
    }

    // blocks just finished: a veil that fades and lets the tile show
    const [fr, fg, fb] = COLORS.done;
    for (let i = 0; i < fading.length; i += 5) {
      ctx.fillStyle = `rgba(${fr}, ${fg}, ${fb}, ${(0.6 * fading[i + 4]).toFixed(3)})`;
      ctx.fillRect(fading[i], fading[i + 1], fading[i + 2], fading[i + 3]);
    }

    // blocks being drawn: bright, breathing
    if (running.length) {
      const [cr, cg, cb] = COLORS.running;
      const breath = 0.5 + 0.5 * Math.sin(now / 240);
      ctx.fillStyle = `rgba(${cr}, ${cg}, ${cb}, ${(solid ? 1 : 0.42 + 0.3 * breath).toFixed(3)})`;
      for (let i = 0; i < running.length; i += 4) ctx.fillRect(running[i], running[i + 1], running[i + 2], running[i + 3]);
      if (!solid) {
        ctx.lineWidth = Math.max(1, Math.round(dpr));
        ctx.strokeStyle = `rgba(${cr + 30}, ${cg + 30}, ${cb + 40}, ${(0.75 + 0.25 * breath).toFixed(3)})`;
        for (let i = 0; i < running.length; i += 4) {
          ctx.strokeRect(running[i] + 0.5, running[i + 1] + 0.5, running[i + 2] - 1, running[i + 3] - 1);
        }
      }
    }

    // blocks that failed
    if (failed.length) {
      ctx.fillStyle = solid ? COLORS.failedEdge : COLORS.failed;
      for (let i = 0; i < failed.length; i += 4) {
        // a failed block must not vanish among thousands: at least 3 px
        const w = Math.max(failed[i + 2], 3 * dpr);
        const h = Math.max(failed[i + 3], 3 * dpr);
        ctx.fillRect(failed[i] - (w - failed[i + 2]) / 2, failed[i + 1] - (h - failed[i + 3]) / 2, w, h);
      }
      if (!solid) {
        ctx.lineWidth = Math.max(1, Math.round(dpr));
        ctx.strokeStyle = COLORS.failedEdge;
        for (let i = 0; i < failed.length; i += 4) {
          ctx.strokeRect(failed[i] + 0.5, failed[i + 1] + 0.5, failed[i + 2] - 1, failed[i + 3] - 1);
        }
      }
    }
    return animating;
  }

  // ------------------------------------------------------------------------------------- marks

  /** A rounded label over the top edge of a rectangle (under it when the top is out of sight). */
  function label(text, xa, xb, ya, yb, g) {
    ctx.font = `600 ${12 * dpr}px Bahnschrift, "Segoe UI", system-ui, sans-serif`;
    const lw = ctx.measureText(text).width + 14 * dpr;
    const lh = 20 * dpr;
    let tx = clamp(xa, 4 * dpr, g.w - lw - 4 * dpr);
    let ty = ya - lh - 8 * dpr;
    if (ty < 4 * dpr) ty = Math.min(yb + 8 * dpr, g.h - lh - 4 * dpr);
    if (xb < 0 || xa > g.w) tx = -1e6;
    ctx.fillStyle = 'rgba(10, 14, 20, 0.9)';
    ctx.beginPath();
    ctx.roundRect ? ctx.roundRect(tx, ty, lw, lh, 5 * dpr) : ctx.rect(tx, ty, lw, lh);
    ctx.fill();
    ctx.fillStyle = '#e6ecf2';
    ctx.textBaseline = 'middle';
    ctx.fillText(text, tx + 7 * dpr, ty + lh / 2 + dpr);
  }

  /** The marks: a tinted rectangle, a dashed edge, never smaller than a few pixels; one alone has its name. */
  function drawMarks(g) {
    if (!marks.length) return;
    const min = 8 * dpr;
    for (const mark of marks) {
      const [cr, cg, cb] = COLORS.marks[mark.kind] || COLORS.marks.zone;
      let xa = Math.round(g.X(mark.zone[0]));
      let xb = Math.round(g.X(mark.zone[2]));
      let ya = Math.round(g.Y(mark.zone[3]));
      let yb = Math.round(g.Y(mark.zone[1]));
      // a zone far smaller than the screen still shows: widened around its middle
      if (xb - xa < min) {
        const m = (xa + xb) / 2;
        xa = Math.round(m - min / 2);
        xb = xa + min;
      }
      if (yb - ya < min) {
        const m = (ya + yb) / 2;
        ya = Math.round(m - min / 2);
        yb = ya + min;
      }
      const w = xb - xa;
      const h = yb - ya;
      ctx.fillStyle = `rgba(${cr}, ${cg}, ${cb}, 0.16)`;
      ctx.fillRect(xa, ya, w, h);
      ctx.lineWidth = 4 * dpr;
      ctx.strokeStyle = COLORS.zoneShadow;
      ctx.strokeRect(xa, ya, w, h);
      ctx.setLineDash([7 * dpr, 4 * dpr]);
      ctx.lineWidth = 2 * dpr;
      ctx.strokeStyle = `rgb(${cr}, ${cg}, ${cb})`;
      ctx.strokeRect(xa, ya, w, h);
      ctx.setLineDash([]);
      if (marks.length === 1 && mark.name) label(mark.name, xa, xb, ya, yb, g);
    }
  }

  // ------------------------------------------------------------------------------------- zone

  function drawZone(g) {
    const z = drag && drag.type === 'zone' ? drag.zone : zone;
    if (!z) return;
    const xa = Math.round(g.X(z[0]));
    const xb = Math.round(g.X(z[2]));
    const ya = Math.round(g.Y(z[3]));
    const yb = Math.round(g.Y(z[1]));
    const w = xb - xa;
    const h = yb - ya;
    ctx.fillStyle = COLORS.zone;
    ctx.fillRect(xa, ya, w, h);
    ctx.lineWidth = 4 * dpr;
    ctx.strokeStyle = COLORS.zoneShadow;
    ctx.strokeRect(xa, ya, w, h);
    ctx.lineWidth = 2 * dpr;
    ctx.strokeStyle = COLORS.zoneEdge;
    ctx.strokeRect(xa, ya, w, h);

    // handles: corners and middles of the edges
    const c = 4 * dpr;
    ctx.fillStyle = COLORS.zoneEdge;
    ctx.strokeStyle = COLORS.zoneShadow;
    ctx.lineWidth = dpr;
    for (const [px, py] of [
      [xa, ya], [xa + w / 2, ya], [xb, ya],
      [xa, ya + h / 2], [xb, ya + h / 2],
      [xa, yb], [xa + w / 2, yb], [xb, yb],
    ]) {
      ctx.fillRect(px - c, py - c, 2 * c, 2 * c);
      ctx.strokeRect(px - c, py - c, 2 * c, 2 * c);
    }

    // its size, over its top edge (under it when the top is out of sight)
    const nx = Math.round((z[2] - z[0]) / BLOCK);
    const ny = Math.round((z[3] - z[1]) / BLOCK);
    const n = nx * ny;
    label(t('map.zoneSize', { count: n, nx, ny }), xa, xb, ya, yb, g);
  }

  /** The handle of the zone under a point (CSS pixels): 'n', 'se'…, or null. */
  function handleAt(px, py) {
    if (!zone) return null;
    const xa = screenX(zone[0]);
    const xb = screenX(zone[2]);
    const ya = screenY(zone[3]);
    const yb = screenY(zone[1]);
    const m = 7;
    if (px < xa - m || px > xb + m || py < ya - m || py > yb + m) return null;
    let p = '';
    if (Math.abs(py - ya) <= m) p += 'n';
    else if (Math.abs(py - yb) <= m) p += 's';
    if (Math.abs(px - xa) <= m) p += 'w';
    else if (Math.abs(px - xb) <= m) p += 'e';
    return p || null;
  }

  const CURSORS = { n: 'ns-resize', s: 'ns-resize', e: 'ew-resize', w: 'ew-resize', nw: 'nwse-resize', se: 'nwse-resize', ne: 'nesw-resize', sw: 'nesw-resize' };

  /** A zone with whole blocks inside the world, or null. */
  function validZone(z) {
    if (!Array.isArray(z) || z.length !== 4 || z.some((v) => !Number.isFinite(v))) return null;
    const x1 = clamp(Math.floor(Math.min(z[0], z[2]) / BLOCK) * BLOCK, -WORLD, WORLD - BLOCK);
    const y1 = clamp(Math.floor(Math.min(z[1], z[3]) / BLOCK) * BLOCK, -WORLD, WORLD - BLOCK);
    const x2 = clamp(Math.ceil(Math.max(z[0], z[2]) / BLOCK) * BLOCK, x1 + BLOCK, WORLD);
    const y2 = clamp(Math.ceil(Math.max(z[1], z[3]) / BLOCK) * BLOCK, y1 + BLOCK, WORLD);
    return [x1, y1, x2, y2];
  }

  function applyZone(z, tell) {
    const before = zone ? zone.join() : '';
    zone = z ? validZone(z) : null;
    if (tell && options.onZone && (zone ? zone.join() : '') !== before) options.onZone(zone ? [...zone] : null);
    requestDraw();
  }

  function applyPickMode(active, tell) {
    if (pickMode === active) return;
    pickMode = active;
    updateCursor();
    if (tell && options.onMode) options.onMode(active);
  }

  // ------------------------------------------------------------------------------------- pointer

  function pointOf(ev) {
    const r = canvas.getBoundingClientRect();
    return [ev.clientX - r.left, ev.clientY - r.top];
  }

  function updateCursor(px, py) {
    let c = 'crosshair';
    if (drag && drag.type === 'view') c = 'grabbing';
    else if (drag && drag.type === 'resize') c = CURSORS[drag.handle];
    else if (px !== undefined && handleAt(px, py)) c = CURSORS[handleAt(px, py)];
    else if (pickMode || (drag && drag.type === 'zone')) c = 'cell';
    canvas.style.cursor = c;
  }

  canvas.addEventListener('pointerdown', (ev) => {
    if (ev.button !== 0 && ev.button !== 1) return;
    stopAnimation();
    const [px, py] = pointOf(ev);
    const handle = ev.button === 0 ? handleAt(px, py) : null;
    if (handle) {
      drag = { type: 'resize', handle };
    } else if (ev.button === 0 && (pickMode || ev.shiftKey)) {
      const ax = Math.floor(worldX(px) / BLOCK);
      const ay = Math.floor(worldY(py) / BLOCK);
      drag = { type: 'zone', ax, ay, zone: [ax * BLOCK, ay * BLOCK, (ax + 1) * BLOCK, (ay + 1) * BLOCK] };
    } else {
      drag = { type: 'view', px, py, cx: view.cx, cy: view.cy };
    }
    try {
      canvas.setPointerCapture(ev.pointerId);
    } catch {
      // a pointer that cannot be captured still drags while it stays over the map
    }
    hideTooltip();
    updateCursor(px, py);
    requestDraw();
    ev.preventDefault();
    canvas.focus({ preventScroll: true });
  });

  canvas.addEventListener('pointermove', (ev) => {
    const [px, py] = pointOf(ev);
    if (!drag) {
      updateCursor(px, py);
      hoverAt(px, py);
      return;
    }
    if (drag.type === 'view') {
      view.cx = drag.cx - (px - drag.px) / view.k;
      view.cy = drag.cy + (py - drag.py) / view.k;
      constrain();
      tellView();
    } else if (drag.type === 'zone') {
      const bx = Math.floor(worldX(px) / BLOCK);
      const by = Math.floor(worldY(py) / BLOCK);
      drag.zone = validZone([
        Math.min(bx, drag.ax) * BLOCK,
        Math.min(by, drag.ay) * BLOCK,
        (Math.max(bx, drag.ax) + 1) * BLOCK,
        (Math.max(by, drag.ay) + 1) * BLOCK,
      ]);
    } else if (drag.type === 'resize' && zone) {
      const z = [...zone];
      const x = Math.round(worldX(px) / BLOCK) * BLOCK;
      const y = Math.round(worldY(py) / BLOCK) * BLOCK;
      if (drag.handle.includes('w')) z[0] = Math.min(x, z[2] - BLOCK);
      if (drag.handle.includes('e')) z[2] = Math.max(x, z[0] + BLOCK);
      if (drag.handle.includes('s')) z[1] = Math.min(y, z[3] - BLOCK);
      if (drag.handle.includes('n')) z[3] = Math.max(y, z[1] + BLOCK);
      applyZone(z, true);
    }
    hoverAt(px, py);
    requestDraw();
  });

  function release(ev) {
    if (!drag) return;
    const d = drag;
    drag = null;
    if (canvas.hasPointerCapture(ev.pointerId)) canvas.releasePointerCapture(ev.pointerId);
    if (d.type === 'zone' && ev.type === 'pointerup') {
      applyZone(d.zone, true);
      applyPickMode(false, true);
    }
    const [px, py] = pointOf(ev);
    updateCursor(px, py);
    requestDraw();
  }
  canvas.addEventListener('pointerup', release);
  canvas.addEventListener('pointercancel', release);

  canvas.addEventListener('pointerleave', () => {
    if (drag) return;
    hover = null;
    hideTooltip();
    if (options.onHover) options.onHover(null);
  });

  canvas.addEventListener(
    'wheel',
    (ev) => {
      ev.preventDefault();
      stopAnimation();
      const [px, py] = pointOf(ev);
      // a notch of a mouse wheel is about 100: a fifth of a level
      const step = ev.deltaMode === 1 ? ev.deltaY * 33 : ev.deltaY;
      zoomAround(px, py, view.k * Math.exp(-clamp(step, -400, 400) * 0.0022));
      tellView();
      hoverAt(px, py);
      requestDraw();
    },
    { passive: false },
  );

  canvas.addEventListener('dblclick', (ev) => {
    const [px, py] = pointOf(ev);
    if (pickMode || handleAt(px, py)) return;
    animateZoom(px, py, view.k * (ev.shiftKey ? 0.5 : 2));
  });

  canvas.addEventListener('keydown', (ev) => {
    const step = 80 / view.k;
    let handled = true;
    if (ev.key === '+' || ev.key === '=') animateZoom(width / 2, height / 2, view.k * 2);
    else if (ev.key === '-' || ev.key === '_') animateZoom(width / 2, height / 2, view.k / 2);
    else if (ev.key === 'ArrowLeft') view.cx -= step;
    else if (ev.key === 'ArrowRight') view.cx += step;
    else if (ev.key === 'ArrowUp') view.cy += step;
    else if (ev.key === 'ArrowDown') view.cy -= step;
    else if (ev.key === 'Escape' && (pickMode || (drag && drag.type === 'zone'))) {
      drag = null;
      applyPickMode(false, true);
    } else handled = false;
    if (!handled) return;
    ev.preventDefault();
    constrain();
    tellView();
    requestDraw();
  });

  /** What is under the pointer: world coordinates, block, state of the block in the mosaic. */
  function hoverAt(px, py) {
    const x = worldX(px);
    const y = worldY(py);
    const bx = Math.floor(x / BLOCK);
    const by = Math.floor(y / BLOCK);
    let state = null;
    let message = null;
    if (plan && mosaicShown) {
      const i = plan.index.get(bx + ',' + by);
      if (i !== undefined) {
        state = STATE_NAMES[plan.state[i]];
        message = plan.messages.get(i) || null;
      }
    }
    hover = { x, y, bx, by, state, message };
    if (options.onHover) options.onHover(hover);
    if (state === 'failed' && !drag) {
      // block coordinates as strings: t() would write numbers the way the language writes them
      tooltip.textContent = t('map.failedBlock', { bx: String(bx), by: String(by), message: message || t('map.failed') });
      tooltip.hidden = false;
      const tw = tooltip.offsetWidth;
      const th = tooltip.offsetHeight;
      tooltip.style.left = `${clamp(px + 14, 4, Math.max(width - tw - 4, 4))}px`;
      tooltip.style.top = `${py + 18 + th > height ? py - th - 10 : py + 18}px`;
    } else {
      hideTooltip();
    }
  }

  function hideTooltip() {
    if (!tooltip.hidden) tooltip.hidden = true;
  }

  // ------------------------------------------------------------------------------------- animation

  function stopAnimation() {
    if (animation) cancelAnimationFrame(animation.frameRequest);
    animation = null;
  }

  /** Runs `step(f)` with f from 0 to 1 over `ms`, eased. */
  function animate(ms, step) {
    stopAnimation();
    const start = performance.now();
    const a = { frameRequest: 0 };
    animation = a;
    const tick = (now) => {
      if (animation !== a) return;
      const f = clamp((now - start) / ms, 0, 1);
      step(1 - (1 - f) ** 3);
      constrain();
      tellView();
      requestDraw();
      if (f < 1) a.frameRequest = requestAnimationFrame(tick);
      else animation = null;
    };
    a.frameRequest = requestAnimationFrame(tick);
  }

  function animateZoom(px, py, k) {
    const k0 = view.k;
    const k1 = clamp(k, K_MIN, K_MAX);
    if (k1 === k0) return;
    animate(200, (f) => zoomAround(px, py, k0 * (k1 / k0) ** f));
  }

  /** Frames [x1, y1, x2, y2] with a margin; `margins` are CSS pixels kept free (top, right, bottom, left). */
  function fitBounds(bounds, { animate: animated = true, margins = [24, 24, 24, 24] } = {}) {
    // no size yet (the window is still hidden): framed when the canvas gets one
    if (width < 8 || height < 8) {
      pendingFit = { bounds, margins };
      return;
    }
    pendingFit = null;
    const [x1, y1, x2, y2] = bounds;
    const fw = Math.max(width - margins[1] - margins[3], 60);
    const fh = Math.max(height - margins[0] - margins[2], 60);
    const k1 = clamp(Math.min(fw / Math.max(x2 - x1, 1), fh / Math.max(y2 - y1, 1)), K_MIN, K_MAX);
    const cx1 = (x1 + x2) / 2 - (margins[3] - margins[1]) / 2 / k1;
    const cy1 = (y1 + y2) / 2 + (margins[0] - margins[2]) / 2 / k1;
    if (!animated) {
      stopAnimation();
      Object.assign(view, { cx: cx1, cy: cy1, k: k1 });
      constrain();
      tellView();
      requestDraw();
      return;
    }
    const { cx: cx0, cy: cy0, k: k0 } = view;
    animate(320, (f) => {
      view.k = k0 * (k1 / k0) ** f;
      // the centre moves on the screen at a steady pace: follow the zoom, not the time
      const u = k1 === k0 ? f : (1 / k0 - 1 / view.k) / (1 / k0 - 1 / k1);
      view.cx = cx0 + (cx1 - cx0) * u;
      view.cy = cy0 + (cy1 - cy0) * u;
    });
  }

  // ------------------------------------------------------------------------------------- size

  function measure() {
    const r = host.getBoundingClientRect();
    const d = window.devicePixelRatio || 1;
    const w = Math.max(Math.round(r.width), 1);
    const h = Math.max(Math.round(r.height), 1);
    if (w === width && h === height && d === dpr) return;
    width = w;
    height = h;
    dpr = d;
    canvas.width = Math.round(w * d);
    canvas.height = Math.round(h * d);
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;
    if (pendingFit && w >= 8 && h >= 8) fitBounds(pendingFit.bounds, { animate: false, margins: pendingFit.margins });
    dirty = false;
    cancelAnimationFrame(frameRequest);
    draw();
  }

  const observer = new ResizeObserver(measure);
  observer.observe(host);
  window.addEventListener('resize', measure);
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) requestDraw();
  });
  // the labels drawn on the canvas follow the language (the aria-label is filled by i18n.js)
  onLanguageChange(() => requestDraw());
  measure();

  // ------------------------------------------------------------------------------------- API

  return {
    /** The source whose tiles are shown ('map' or 'preview'). */
    setSource(name) {
      if (source === name) return;
      source = name;
      requestDraw();
    },
    source: () => source,

    /** A second source drawn faintly under the first (null: none). */
    setBackground(name) {
      if (background === name) return;
      background = name;
      requestDraw();
    },

    /** The revision put in the addresses of the tiles of a source asked from now on. */
    setRevision(name, rev) {
      revs[name] = rev;
    },

    /**
     * The ground [x1, y1, x2, y2] the tiles of a source cover, when it is known for good (a
     * finished render); null while it may still grow.
     */
    setExtent(name, bounds) {
      const before = extents[name] ? extents[name].join() : '';
      extents[name] = Array.isArray(bounds) && bounds.length === 4 && bounds.every(Number.isFinite) ? [...bounds] : null;
      if ((extents[name] ? extents[name].join() : '') !== before) requestDraw();
    },

    /** Everything known of a source may have changed: asked again, the pictures shown meanwhile. */
    invalidate(name) {
      for (const e of tiles.values()) {
        if (e.source !== name) continue;
        if (e.loading) e.changedWhileLoading = true;
        else e.stale = true;
      }
      requestDraw();
    },

    /** The tiles of zooms 7 to 9 of a block were just written. */
    invalidateBlock(name, bx, by) {
      for (let z = 7; z <= ZMAX; z++) {
        const f = 2 ** (z - 7);
        for (let j = 0; j < f; j++) {
          for (let i = 0; i < f; i++) {
            const e = tiles.get(keyOf(name, z, bx * f + i, by * f + j));
            if (!e) continue;
            if (e.loading) e.changedWhileLoading = true;
            else e.stale = true;
          }
        }
      }
      requestDraw();
    },

    /** The tiles of zooms 6 to 0 were just made again. */
    invalidateFarZooms(name) {
      if (plan && plan.source === name) {
        plan.pass += 1;
        plan.passAt = performance.now();
        // the blocks of the pass before stop showing as fresh after a while
        setTimeout(requestDraw, ZOOMS_GRACE_MS + 20);
      }
      for (const e of tiles.values()) {
        if (e.source !== name || e.z > 6) continue;
        if (e.loading) e.changedWhileLoading = true;
        else e.stale = true;
      }
      requestDraw();
    },

    /** Tiles that could not be had are asked again (the server was away). */
    retryMissing() {
      for (const e of tiles.values()) {
        e.waitUntil = 0;
        if (e.state === MISSING && !e.loading) e.stale = true;
      }
      requestDraw();
    },

    /**
     * The plan of a render: [[bx, by], …] in drawing order, and the source its tiles go to; null
     * to remove the mosaic.
     */
    setPlan(blocks, name) {
      dropSheet(plan);
      thumbQueue = [];
      if (!blocks || !blocks.length) {
        plan = null;
        requestDraw();
        return;
      }
      const n = blocks.length;
      const p = {
        n,
        bx: new Int32Array(n),
        by: new Int32Array(n),
        state: new Uint8Array(n),
        doneAt: new Float64Array(n),
        // the pass of far zooms during which each block was finished (-1: not finished)
        passes: new Int32Array(n).fill(-1),
        pass: 0,
        // when the last pass of the far zooms ended (performance.now())
        passAt: -Infinity,
        // the thumbnails: on the sheet (1) or not, and the times each was asked again
        onSheet: new Uint8Array(n),
        thumbTries: new Uint8Array(n),
        sheet: null,
        source: name || null,
        index: new Map(),
        messages: new Map(),
        bounds: [Infinity, Infinity, -Infinity, -Infinity],
      };
      for (let i = 0; i < n; i++) {
        const [x, y] = blocks[i];
        p.bx[i] = x;
        p.by[i] = y;
        p.index.set(x + ',' + y, i);
        p.bounds[0] = Math.min(p.bounds[0], x * BLOCK);
        p.bounds[1] = Math.min(p.bounds[1], y * BLOCK);
        p.bounds[2] = Math.max(p.bounds[2], (x + 1) * BLOCK);
        p.bounds[3] = Math.max(p.bounds[3], (y + 1) * BLOCK);
      }
      plan = p;
      requestDraw();
    },

    /**
     * A block of the plan changes state: 'started', 'done' or 'failed' (`message`: why it failed,
     * if the server said). `old` (an event read again): no fading veil.
     */
    markBlock(bx, by, state, message, old) {
      if (!plan) return;
      const i = plan.index.get(bx + ',' + by);
      if (i === undefined) return;
      if (state === 'started') {
        if (plan.state[i] === WAITING) plan.state[i] = RUNNING;
      } else if (state === 'done') {
        if (plan.state[i] !== FAILED) {
          plan.state[i] = DONE;
          plan.doneAt[i] = old ? -1e9 : performance.now();
          plan.passes[i] = plan.pass;
          // its thumbnail, for when it is small on the screen (asked once the events of the
          // moment are read: a replayed pass may show the far zooms already include it)
          if (plan.source && !plan.onSheet[i]) {
            thumbQueue.push(i);
            queueMicrotask(pumpThumbnails);
          }
        }
      } else if (state === 'failed') {
        plan.state[i] = FAILED;
        // without a message, the tooltip says "failed" in the language shown at that moment
        plan.messages.set(i, message || null);
      }
      requestDraw();
    },

    /** The render is over: blocks that were being drawn are not any more. */
    freezePlan() {
      if (!plan) return;
      for (let i = 0; i < plan.n; i++) if (plan.state[i] === RUNNING) plan.state[i] = WAITING;
      requestDraw();
    },

    planBounds: () => (plan ? [...plan.bounds] : null),

    showMosaic(visible) {
      mosaicShown = visible;
      requestDraw();
    },

    /**
     * Marks over the tiles: [{ zone: [x1, y1, x2, y2] in metres, kind: 'added' | 'changed' |
     * 'removed' | 'zone', name }], or null / [] for none. A mark alone is drawn with its name.
     */
    setMarks(list) {
      const valid = (Array.isArray(list) ? list : []).filter(
        (m) => m && Array.isArray(m.zone) && m.zone.length === 4 && m.zone.every(Number.isFinite) && m.zone[2] > m.zone[0] && m.zone[3] > m.zone[1],
      );
      const before = JSON.stringify(marks);
      marks = valid.map((m) => ({ zone: [...m.zone], kind: String(m.kind || 'zone'), name: m.name ? String(m.name) : '' }));
      if (JSON.stringify(marks) !== before) requestDraw();
    },

    zone: () => (zone ? [...zone] : null),
    setZone(z) {
      applyZone(z, true);
    },
    /** Picking mode: the next drag draws the zone. */
    setPickMode(active) {
      applyPickMode(active, false);
    },

    fitBounds,
    /** Zooms by a factor around the middle of the map. */
    zoomBy(factor) {
      animateZoom(width / 2, height / 2, view.k * factor);
    },
    view: readView,
    setView(v) {
      if (!v || ![v.cx, v.cy, v.k].every(Number.isFinite)) return;
      // a view given outright wins over a framing still waiting for the canvas to have a size
      pendingFit = null;
      stopAnimation();
      Object.assign(view, { cx: v.cx, cy: v.cy, k: v.k });
      constrain();
      tellView();
      requestDraw();
    },

    /** A round length that fits in `maxPx` CSS pixels: { metres, px }. */
    scale(maxPx) {
      const m = maxPx / view.k;
      const p = 10 ** Math.floor(Math.log10(m));
      const metres = (m >= 5 * p ? 5 : m >= 2 * p ? 2 : 1) * p;
      return { metres, px: metres * view.k };
    },

    redraw: requestDraw,

    /** Draws one frame right now and returns the milliseconds it took (to tune the mosaic). */
    timeFrame() {
      const start = performance.now();
      draw();
      return performance.now() - start;
    },
  };
}
