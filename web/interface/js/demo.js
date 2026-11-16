//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

// Demo mode: a fake server in memory, for working on the page without the program and for
// screenshots. It follows the contract of the real one (same routes, same JSON, same status codes,
// the same English messages with the same `code` and `params`), renders nothing and touches
// nothing: a render is a timer.
//
//   ?demo=1            a render of the whole map draws a few hundred blocks around the city centre
//   ?demo=map          a render of the whole map draws the whole map (about 6,000 blocks)
//   &tiles=<base>      a static server of real tiles (<base>/<z>/<tx>/<ty>.<extension of <base>/tiles.json>,
//                      or .webp, .jpg, then .png when it cannot be read)
//   &running=1         a render of the whole map is already half done when the page opens
//   &game=fail  &keys=fail  &config=fail    a check that fails
//   &outage=<s>        the server stops answering after <s> seconds, for 6 seconds
//   &token=fail        the server refuses the token of the page (403), as after it was restarted
//   &speed=<n>         blocks per second of the simulated render
//   &card=none         a machine without a graphics card (a software adapter only)
//   &counters=off      the load of the card cannot be read (null), as without performance counters
//   &fallback=<kind>   the work the card leaves to the processor: drawing (the default), shading,
//                      chain, device (the card asked for does not open), none (the card does it all)
//   &record=none       the map folder has no record of the resources yet (`resources.json`)
//   &changes=none      nothing changed in the resources since the record
//   &output=empty      no map in the folder of the whole map: nothing to update
//   &update=fail       an update is refused before its first block (the folder is locked by another render)
//
// The jobs of `update` (`update`, `list-changes`, `record`) are simulated too: four resources
// changed since the record (one added, two changed, one of them whose ground cannot be read, one
// removed); an update draws their ground (or the zone given) into the map itself, whose facts
// stay those of the whole map; once it is over, or once the resources are recorded, nothing is
// left to change (what the engine is assumed to do: the record is written again).
//
// The machine (16 logical cores, 64 GB, an RTX 3090 Ti and an integrated card) is known a moment
// after the start, as on the real server; its load is read every second and follows the
// simulated render (its threads, who draws, who shades, its budgets).
//
// In the console: `btmsDemo.outage(6)` cuts the server for 6 seconds, `btmsDemo.calls` counts the
// requests it answered, `btmsDemo.request(…)` asks it something, `btmsDemo.map` is the map.

const VERSION = '1.0.0';
const CORES = 16;
const BLOCK = 128;
/** The bounds of the settings, the same as the server's (src/interface/settings.rs). */
const MAX_THREADS = 256;
const MEMORY_GB = [0.25, 4096];
const CARDS = [
  { name: 'NVIDIA GeForce RTX 3090 Ti', memory_gb: 23.74, type: 'dedicated' },
  { name: 'AMD Radeon(TM) Graphics', memory_gb: 0.47, type: 'integrated' },
  { name: 'Microsoft Basic Render Driver', memory_gb: null, type: 'software' },
];
/** The interface languages the server accepts for the setting `language` (null: auto), as settings.rs `LANGUAGES`. */
const LANGUAGES = ['en', 'fr', 'de', 'nl', 'ru', 'sv', 'es'];

// The blocks of the real map (the tiles of zoom 7 of a finished render), row by row from north
// (by = 65) to south, each row as runs "offset.length" from bx = −32.
const NORTH_ROW = 65;
const WEST_COLUMN = -32;
const ROWS =
  '29.4/26.9/26.13/24.17/24.18/23.21/22.25,67.1/22.26,50.5/19.36/18.43/18.44/17.46/17.46/16.47/16.47/16.48/' +
  '16.49/15.51/14.53/14.53/14.54/13.55/11.57/11.58/10.60/9.61/7.63/7.63/7.63/7.64/6.66/5.67/5.68/4.69/2.71/' +
  '1.72/1.72/2.71/2.70/2.70/2.68/2.68/2.68/3.67/4.65/4.64/4.64/3.65/3.64/2.65/1.66/1.65/2.64/1.65/1.64/0.63' +
  '/0.63/0.63/1.62/1.62/1.63/1.63/0.65/0.65/1.64/1.64/2.63/3.63/3.62/5.60/6.58/8.56/8.56/11.53/11.52/12.51/' +
  '12.51/13.50/6.1,14.49/15.47/14.48/13.49/12.50/11.51/11.51/10.51/10.51/10.50/10.49/10.47/9.47/9.46/10.44/' +
  '10.43/11.42,56.25/12.40,56.25/13.36,56.25/13.34,56.25/14.16,34.12,56.25/16.11,37.8,56.25/19.7,56.25/21.4' +
  ',55.26/1.1,55.26/24.2,55.26/56.25/56.25/56.25/56.25/56.25/56.25/56.25/56.25/56.25/56.25/56.25/56.25/56.2' +
  '5/56.25/56.25';

const KEEP = [
  'hei_carrier', 'hei_yacht_heist', 'HeistIsland', 'post_hiest_unload', 'refit_unload', 'finbank',
  'coroner_int_on', 'farm', 'farm_props', 'farm_lod', 'farmint', 'des_farmhouse', 'fiblobby',
  'atriumglmission', 'dt1_05_hc_remove', 'facelobby', 'v_tunnel_hole', 'carwash_with_spinners',
  'ch1_02_open', 'canyonrvrshallow', 'ferris_finale_anim', 'ld_rail_01_track', 'ld_rail_02_track',
  'dockcrane1', 'pcranecont', 'cs5_4_trains', 'shr_int', 'trevorstrailertidy', 'cargoship',
  'hei_carrier_int1', 'hei_carrier_int2', 'hei_carrier_lodlights', 'hei_yacht_heist_bar',
  'hei_yacht_heist_bridge', 'gr_heist_yacht2', 'vw_casino_main', 'vw_casino_garage',
  'vw_casino_carpark', 'vw_casino_penthouse', 'tr_tuner_meetup', 'xm_hatch_closed',
  'm23_1_legacy_fixes', 'm24_1_carrier', 'm25_1_carwash', 'm25_2_knoway_sign',
];

const OUTPUTS = {
  verify: [
    'game folder: D:\\Games\\Grand Theft Auto V',
    'AES key: found in GTA5.exe by its fingerprint (1.3 s)',
    'NG keys: 101 keys and 272 tables, fingerprints checked',
    'common.rpf: data/levels/gta5/gta5.meta read back as plain text',
    'update/update.rpf: content.xml read back as plain text',
    'update/x64/dlcpacks/mpheist4/dlc1.rpf: Cayo Perico map files read',
    'x64c.rpf: read',
    'verification passed: the archives decrypt',
  ],
  groups: [
    'manifests: 1792 read, 953 map groups, 578 distinct names',
    'on-demand placement files: 1296 of 11015 (75203 entities)',
    'keep list of map.toml: 184 entries',
    '',
    'KEPT     hei_carrier                      keep list                     412 entities',
    'KEPT     hei_carrier_int1                 keep list                      96 entities',
    'KEPT     hei_yacht_heist                  keep list                     131 entities',
    'KEPT     h4_islandx_terrain_01            island HeistIsland           1204 entities',
    'KEPT     h4_islandx_mansion               island HeistIsland            873 entities',
    'KEPT     vb_30_beach_day                  time: shown at noon            58 entities',
    'left out vb_30_beach_night                time: not at noon              61 entities',
    'left out apa_yacht_grp04_1                no script asks for it         214 entities',
    'left out ufo                              no script asks for it           1 entity',
    'left out sunkcargoship                    no script asks for it          37 entities',
    'left out redcarpet                        no script asks for it          22 entities',
    'left out bnkheist_apt_norm                no script asks for it          48 entities',
    '',
    'kept: 511 files (58560 entities); left out: 785 files (16643 entities)',
    'streamed by the resources: 0 on-demand files',
  ],
  'list-changes': [
    'resources: 4 folders, 23 resources, 1,108 files streamed',
    'record of the map: resources.json (23 resources recorded)',
    'comparison: 1 added, 2 changed, 1 removed',
  ],
  record: [
    'resources: 4 folders, 23 resources, 1,108 files streamed',
    'record written: resources.json',
  ],
  resources: [
    'resources: 1 folder, 1 resource, 247 files streamed',
    '',
    'my-police-station',
    '  36 placement files, 30 of them in place of a game file',
    '  211 models read, none protected',
    '  533 game entities moved out of the world (z ≈ −222,230 m), not drawn',
    '  1 interior: 4444 entities drawn',
    '  scripts: no RemoveIpl, no RequestIpl',
    '',
    'names duplicated between resources: none',
    'files left out: none (no FXAP file)',
  ],
  'import-keys': [
    'magic.dat: 154069 bytes',
    'decoding: four masks, AES-256, DEFLATE',
    'NG keys: 101 of 101 match their fingerprint',
    'NG tables: 272 of 272 match their fingerprint',
    'keys written to C:\\Users\\demo\\AppData\\Local\\BTMS\\keys',
  ],
};

// What changed in the resources since the map recorded them (the `changes` event of the engine).
const CHANGES = [
  { name: '[maps]/sandy-airfield', folder: 'C:\\Server\\resources\\[maps]\\sandy-airfield', state: 'added', zone: [1588.5, 3115.2, 1871.4, 3402.9] },
  { name: '[mlo]/my-police-station', folder: 'C:\\Server\\resources\\[mlo]\\my-police-station', state: 'changed', zone: [402.7, -1061.3, 515.8, -941.6] },
  { name: '[mlo]/pillbox-hospital', folder: 'C:\\Server\\resources\\[mlo]\\pillbox-hospital', state: 'changed', zone: null, reason: 'protected files (FXAP): its placements cannot be read' },
  { name: '[maps]/old-paleto-garage', folder: 'C:\\Server\\resources\\[maps]\\old-paleto-garage', state: 'removed', zone: [-223.9, 6201.4, -61.2, 6342.7] },
];

const ERRORS = [
  'dt1_19_lspd02.ydr: pointer outside the resource',
  'textures of hei_dt1_05: unreadable dictionary',
  'cs2_08_grass.ydr: unexpected vertex size',
];

/** The refusal of an unknown job kind, as the server words it (src/interface/mod.rs, `UNKNOWN_JOB`). */
const UNKNOWN_JOB = 'unknown job: zone, map, update, list-changes, record, verify, groups, resources or import-keys expected';
/** The job kinds the server launches (src/interface/job.rs, `Kind`). */
const JOB_KINDS = ['zone', 'map', 'update', 'list-changes', 'record', 'verify', 'groups', 'resources', 'import-keys'];
/** The folders and the map.toml of the settings when none is chosen (an empty path is the default too). */
const DEFAULT_PATHS = {
  config: 'C:\\Users\\demo\\BTMS\\map.toml',
  output: 'C:\\Users\\demo\\BTMS\\output\\tiles',
  preview: 'C:\\Users\\demo\\BTMS\\output\\preview',
};

const copy = (v) => JSON.parse(JSON.stringify(v));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
/** Jobs that draw tiles: a preview, the whole map, an update of the map. */
const isRender = (kind) => kind === 'zone' || kind === 'map' || kind === 'update';
/** A check as the server sends it: the English `detail`, and its `code` and `params`. */
const check = (ok, path, code, detail, params = {}) => ({ ok, path, detail, code, params });
/** An error the page shows by its code, as the server sends it: `{error, code, params}`. */
const refusal = (status, code, error, params = {}) => [status, { error, code, params }];

/**
 * The zone of a render asked for, as the server reads it (src/interface/mod.rs, `zone_asked`):
 * four numbers, put in order, not empty. `[zone, null]`, or `[null, refusal]`.
 */
function zoneAsked(value) {
  if (!Array.isArray(value) || value.length !== 4 || value.some((v) => typeof v !== 'number')) {
    return [null, refusal(400, 'zone_not_four_numbers', 'zone: four numbers expected (x1, y1, x2, y2)')];
  }
  if (value.some((v) => !Number.isFinite(v) || Math.abs(v) > 100000)) return [null, refusal(400, 'zone_outside_world', 'zone: coordinates outside the world')];
  const z = [Math.min(value[0], value[2]), Math.min(value[1], value[3]), Math.max(value[0], value[2]), Math.max(value[1], value[3])];
  if (z[2] - z[0] <= 0 || z[3] - z[1] <= 0) return [null, refusal(400, 'zone_empty', 'empty zone: it needs a width and a height')];
  return [z, null];
}

/** A duration as the server writes it in the last line of a job (src/interface/job.rs, `duration`): `7 s`, `3 min 05 s`. */
function durationText(seconds) {
  const s = Math.round(seconds);
  return s < 60 ? `${s} s` : `${Math.floor(s / 60)} min ${String(s % 60).padStart(2, '0')} s`;
}

/** A size in GB as the `settings:` line of the engine writes it (tiles/options.rs, `gb`): `6`, `12.5`. */
const gbText = (gb) => String(Number(gb.toFixed(2)));

/** The blocks of the whole map, as [bx, by]. */
function worldBlocks() {
  const list = [];
  ROWS.split('/').forEach((row, j) => {
    if (!row) return;
    for (const run of row.split(',')) {
      const [start, n] = run.split('.').map(Number);
      for (let i = 0; i < n; i++) list.push([WEST_COLUMN + start + i, NORTH_ROW - j]);
    }
  });
  return list;
}

/** Position of a block along the Z curve the renderer follows. */
function zOrder([bx, by]) {
  const x = bx + 512;
  const y = by + 512;
  let r = 0;
  for (let i = 0; i < 11; i++) r |= (((x >> i) & 1) << (2 * i)) | (((y >> i) & 1) << (2 * i + 1));
  return r;
}

/**
 * The fake server. `params`: the query of the page. Returns `request` (the same shape as the
 * real transport of app.js) and `tileUrls` (where the tiles are, for the map).
 */
export function createDemo(params) {
  const wholeWorld = params.get('demo') === 'map';
  const tilesBase = (params.get('tiles') || '').replace(/\/+$/, '');
  // the extensions to try for a tile of `&tiles=`, as the readers do: the `extension` of its
  // `tiles.json` (`webp` alone; `jpg` or none: `.jpg` then `.png`); all three while it is not
  // read, and from a server of another origin (it would not let the page read it)
  let tileExtensions = ['webp', 'jpg', 'png'];
  let sameOrigin = false;
  try {
    sameOrigin = Boolean(tilesBase) && new URL(tilesBase, location.href).origin === location.origin;
  } catch {
    // not an address: the tiles will not load anyway
  }
  if (sameOrigin) {
    fetch(`${tilesBase}/tiles.json`, { cache: 'no-cache' })
      .then((response) => (response.ok ? response.json() : null))
      .then((m) => {
        if (!m || typeof m !== 'object') return;
        const ext = typeof m.extension === 'string' ? m.extension.trim().toLowerCase().replace(/^\./, '') : '';
        tileExtensions = /^[a-z0-9]{2,5}$/.test(ext) && ext !== 'jpg' ? [ext] : ['jpg', 'png'];
      })
      .catch(() => {});
  }
  const speedWanted = Number(params.get('speed')) || 0;
  const tokenRefused = params.get('token') === 'fail';
  // the work the card leaves to the processor: `drawing` (the default: from 40 % of the blocks on,
  // one in three drawn by the processor), `shading` (the same for the shading), `chain` (the same
  // for the whole chain of a block), `device` (the card asked for does not open: the processor
  // draws from the start), `none` (the card does it all)
  const fallbackWanted = ['drawing', 'shading', 'chain', 'device', 'none'].includes(params.get('fallback')) ? params.get('fallback') : 'drawing';
  const world = worldBlocks();
  const blockKeys = new Set(world.map(([x, y]) => x + ',' + y));

  // ------------------------------------------------------------------------------------- state

  const base = {
    maps: { keep: KEEP, exclude_folders: ['/levels/gta5/_prologue/'], fill_holes: true },
    resources: { folders: ['C:\\Server\\resources\\my-police-station'] },
    lighting: { enabled: true, azimuth: 315, elevation: 35, strength: 0.6, shadows: true, ambient_occlusion: 0.3 },
    water: { enabled: true },
  };
  const settings = {
    game: null,
    ...DEFAULT_PATHS,
    threads: 'auto',
    renderer: 'auto',
    shading: 'auto',
    ram_gb: null,
    vram_gb: null,
    priority: 'normal',
    format: 'webp',
    quality: 90,
    overrides: {},
    language: null,
  };
  const machine = {
    cores: CORES,
    memory_gb: 63.21,
    cards: params.get('card') === 'none' ? CARDS.filter((c) => c.type === 'software') : CARDS,
  };
  const cardCounters = params.get('counters') !== 'off';
  // (the checks as the server words them, src/interface/mod.rs: check_game, check_keys, check_config)
  const gameOfFiveM = () => check(true, 'D:\\Games\\Grand Theft Auto V', 'game_fivem', 'the folder FiveM uses (CitizenFX.ini)');
  const keysPresent = (path) => check(true, path, 'keys_present', 'AES key, 101 NG keys and 272 NG tables present');
  const checks = {
    game: gameOfFiveM(),
    keys: keysPresent('C:\\Users\\demo\\AppData\\Local\\BTMS\\keys'),
    config: check(true, settings.config, 'config_read_lighting_on', `${KEEP.length} on-demand maps kept, 1 resource folder(s), lighting on`, { kept: KEEP.length, folders: 1 }),
  };
  if (params.get('game') === 'fail') {
    checks.game = check(false, null, 'game_not_found', 'GTA V folder not found: FiveM does not record it in CitizenFX.ini. Choose it.');
  }
  if (params.get('keys') === 'fail') {
    const files = 'gtav_ng_key.dat, gtav_ng_decrypt_tables.dat';
    checks.keys = check(false, checks.keys.path, 'keys_ng_missing', `${files} missing: import the NG keys from CodeWalker's magic.dat`, { files });
  }
  if (params.get('config') === 'fail') {
    const error = 'map.toml cannot be read: line 87, unknown key "lightin" (the defaults are used)';
    checks.config = check(false, settings.config, 'config_unreadable', error, { error });
  }
  const sources = {
    map: params.get('output') === 'empty'
      ? { folder: settings.output, exists: false, complete: false, blocks: null, bounds: null, rev: 1 }
      : { folder: settings.output, exists: true, complete: true, blocks: 6097, bounds: [-4096, -6784, 6272, 8448], rev: 1 },
    preview: { folder: settings.preview, exists: false, complete: false, blocks: null, bounds: null, rev: 0 },
  };

  // the record of the resources in the map folder, and what changed since it
  let recorded = params.get('record') !== 'none';
  const updateFails = params.get('update') === 'fail';
  let changedResources = params.get('changes') === 'none' ? [] : copy(CHANGES);

  /** The `changes` event of the engine, as the server keeps it (`job.changes`). */
  function changesEvent() {
    if (!recorded) return { known: false, resources: [], zones: [], blocks: 0, note: null };
    // the ground of each resource that can be read, in whole blocks
    const zones = changedResources.filter((r) => r.zone).map(({ zone: z }) => [Math.floor(z[0] / BLOCK) * BLOCK, Math.floor(z[1] / BLOCK) * BLOCK, Math.ceil(z[2] / BLOCK) * BLOCK, Math.ceil(z[3] / BLOCK) * BLOCK]);
    const blocks = zones.reduce((n, z) => n + ((z[2] - z[0]) / BLOCK) * ((z[3] - z[1]) / BLOCK), 0);
    const unreadable = changedResources.filter((r) => !r.zone).length;
    return {
      known: true,
      resources: copy(changedResources),
      zones,
      blocks,
      note: unreadable ? `${unreadable} resource whose zone cannot be read: its place is not redrawn. Draw it on the map and use "Update the map here".` : null,
    };
  }

  /** The blocks of some zones, once each. */
  function blocksOfZones(zones) {
    const seen = new Set();
    const list = [];
    for (const z of zones) {
      for (let x = Math.floor(z[0] / BLOCK); x < Math.ceil(z[2] / BLOCK); x++) {
        for (let y = Math.floor(z[1] / BLOCK); y < Math.ceil(z[3] / BLOCK); y++) {
          if (seen.has(x + ',' + y)) continue;
          seen.add(x + ',' + y);
          list.push([x, y]);
        }
      }
    }
    return list;
  }

  let job = null;
  let nextId = 1;
  /** Events of the shown job, and the cursor of the first of them. */
  let events = [];
  let cursorStart = 0;
  /** The running simulation, or null. */
  let sim = null;
  let timer = 0;
  /** Blocks whose tiles the preview source can show (their keys), and those the far zooms include. */
  const previewBlocks = new Set();
  const previewZooms = new Set();
  let dead = false;
  let outageUntil = 0;
  let foldersChosen = 0;
  /** Requests answered, by route. */
  const calls = {};

  // ------------------------------------------------------------------------------------- settings

  function effective() {
    const s = settings.overrides || {};
    const e = copy(base);
    for (const section of ['maps', 'resources', 'lighting', 'water']) {
      for (const [field, value] of Object.entries(s[section] || {})) {
        if (value !== null && value !== undefined) e[section][field] = value;
      }
    }
    return e;
  }

  /** Merges a partial object: objects are merged, null erases, the rest replaces. */
  function merge(target, patch) {
    for (const [key, value] of Object.entries(patch)) {
      if (value === null) delete target[key];
      else if (typeof value === 'object' && !Array.isArray(value)) {
        if (typeof target[key] !== 'object' || target[key] === null || Array.isArray(target[key])) target[key] = {};
        merge(target[key], value);
      } else target[key] = value;
    }
  }

  /** The reason a performance setting is refused, as the server words it (settings.rs, `merge`), or ''. */
  function perfRefusal(key, v) {
    if (v === null) return '';
    if (key === 'threads') return v === 'auto' || (Number.isInteger(v) && v >= 1 && v <= MAX_THREADS) ? '' : `threads: "auto" or a whole number from 1 to ${MAX_THREADS} expected`;
    if (key === 'renderer' || key === 'shading') return ['auto', 'gpu', 'cpu'].includes(v) ? '' : `${key}: auto, gpu or cpu expected`;
    if (key === 'ram_gb' || key === 'vram_gb') {
      return typeof v === 'number' && Number.isFinite(v) && v >= MEMORY_GB[0] && v <= MEMORY_GB[1] ? '' : `${key}: a number of GB from ${MEMORY_GB[0]} to ${MEMORY_GB[1]} expected, or null`;
    }
    if (key === 'priority') return v === 'normal' || v === 'low' ? '' : 'priority: normal or low expected';
    if (key === 'format') return v === 'webp' || v === 'jpeg' ? '' : 'format: webp or jpeg expected';
    if (key === 'quality') return v === 'lossless' || (Number.isInteger(v) && v >= 0 && v <= 100) ? '' : 'quality: a whole number from 0 to 100 or "lossless" expected, or null';
    return '';
  }

  /** The reason a language is refused, as the server words it (settings.rs, `merge`), or '': one of LANGUAGES, or null for the browser's. */
  function languageRefusal(v) {
    return v === null || LANGUAGES.includes(v) ? '' : `language: one of ${LANGUAGES.join(', ')} expected, or null`;
  }

  /** The reason a path is refused, as the server words it (settings.rs, `text`), or '': text, or null. */
  function pathRefusal(key, v) {
    if (v === null) return '';
    if (typeof v !== 'string') return `${key}: a path (text) or null expected`;
    return v.includes('\0') ? `${key}: path refused` : '';
  }

  /** A path setting as the server keeps it: null, or empty, is the default (`game`: the folder FiveM uses). */
  const pathOf = (key, v) => (typeof v === 'string' && v.trim() ? v.trim() : key === 'game' ? null : DEFAULT_PATHS[key]);

  /** The overrides once a patch is merged in: objects into objects, null removes, sections left empty go (`merge_into`). */
  function overridesAfter(patch) {
    if (patch === null) return {};
    const after = copy(settings.overrides || {});
    merge(after, patch);
    for (const section of Object.keys(after)) {
      if (after[section] && typeof after[section] === 'object' && !Array.isArray(after[section]) && !Object.keys(after[section]).length) delete after[section];
    }
    return after;
  }

  /**
   * The reason overrides are refused, as the server words it (settings.rs, `effective`), or ''.
   * Like the server, what is checked is the overrides once the patch is merged in.
   */
  function overridesRefusal(patch) {
    if (patch === null) return '';
    if (typeof patch !== 'object' || Array.isArray(patch)) return 'overrides: an object expected (maps, resources, lighting, water)';
    for (const [section, fields] of Object.entries(overridesAfter(patch))) {
      if (!base[section]) return `overrides: unknown section "${section}" (maps, resources, lighting, water)`;
      if (fields === null) continue;
      if (typeof fields !== 'object' || Array.isArray(fields)) return `overrides.${section}: an object expected`;
      for (const [field, v] of Object.entries(fields)) {
        const name = `overrides.${section}.${field}`;
        if (!(field in base[section])) return `${name}: unknown field`;
        const old = base[section][field];
        if (v === null) continue;
        if (typeof old === 'boolean' && typeof v !== 'boolean') return `${name}: true or false expected`;
        if (typeof old === 'number' && (typeof v !== 'number' || !Number.isFinite(v))) return `${name}: a number expected`;
        if (Array.isArray(old) && !(Array.isArray(v) && v.every((x) => typeof x === 'string'))) return `${name}: a list of strings expected`;
      }
    }
    return '';
  }

  // (a refusal of the settings is the settings module's own English text, without a code, as the server sends it)
  function changeSettings(patch) {
    if (!patch || typeof patch !== 'object' || Array.isArray(patch)) return [400, { error: 'the settings must be a JSON object' }];
    const PERF = ['threads', 'renderer', 'shading', 'ram_gb', 'vram_gb', 'priority', 'format', 'quality'];
    const PATHS = ['game', 'config', 'output', 'preview'];
    for (const key of Object.keys(patch)) {
      if (![...PATHS, 'overrides', 'language', ...PERF].includes(key)) return [400, { error: `unknown setting: ${key}` }];
      const refused = PERF.includes(key)
        ? perfRefusal(key, patch[key])
        : PATHS.includes(key) ? pathRefusal(key, patch[key]) : key === 'overrides' ? overridesRefusal(patch[key]) : languageRefusal(patch[key]);
      if (refused) return [400, { error: refused }];
    }
    // the job that runs writes into the folder it was given: the page must not be shown another one meanwhile
    const moved = ['output', 'preview'].some((key) => key in patch && pathOf(key, patch[key]) !== settings[key]);
    if (moved && job && job.status === 'running') return refusal(409, 'folders_busy', 'a job is running: the tile folders can only be changed after it');
    // the default of each is the engine's own value
    const DEFAULTS = { threads: 'auto', renderer: 'auto', shading: 'auto', ram_gb: null, vram_gb: null, priority: 'normal', format: 'webp', quality: 90 };
    for (const key of PERF) if (key in patch) settings[key] = patch[key] === null ? DEFAULTS[key] : patch[key];
    if ('game' in patch) {
      settings.game = pathOf('game', patch.game);
      checks.game = settings.game ? check(true, settings.game, 'game_chosen', 'folder chosen in the interface') : gameOfFiveM();
    }
    for (const key of ['config', 'output', 'preview']) if (key in patch) settings[key] = pathOf(key, patch[key]);
    if ('overrides' in patch) settings.overrides = overridesAfter(patch.overrides);
    if ('language' in patch) settings.language = patch.language;
    if (sources.map.folder !== settings.output) Object.assign(sources.map, { folder: settings.output, exists: false, complete: false, blocks: null, bounds: null, rev: sources.map.rev + 1 });
    if (sources.preview.folder !== settings.preview) {
      Object.assign(sources.preview, { folder: settings.preview, exists: false, complete: false, blocks: null, bounds: null, rev: sources.preview.rev + 1 });
      previewBlocks.clear();
      previewZooms.clear();
    }
    checks.config.path = settings.config;
    return [200, stateView(null)];
  }

  // ------------------------------------------------------------------------------------- jobs

  const emit = (ev) => events.push(ev);
  const log = (text) => emit({ t: 'log', text });

  /** What a render takes of the settings (the `settings` event of the engine): `auto` resolved, the card if it opens. */
  function runSettings() {
    const card = machine.cards.find((c) => c.type !== 'software') || null;
    // (`&fallback=device`: the card is asked for, and does not open)
    const cardDraws = Boolean(card) && settings.renderer !== 'cpu' && fallbackWanted !== 'device';
    const cardShades = Boolean(card) && (settings.shading === 'gpu' || (settings.shading === 'auto' && cardDraws));
    // (`--chain auto`, `render/gpu/chain.rs`: the chain on the card when it draws, and shades or the light is off)
    const chain = cardDraws && (cardShades || effective().lighting.enabled === false);
    return {
      // (the engine's own choice, `tiles::options::threads`: one drawing thread per logical core;
      // without the chain, half as many again when the card draws, to cover their waits)
      threads: settings.threads === 'auto' ? (cardDraws && !chain ? Math.min(CORES + Math.floor(CORES / 2), 256) : CORES) : settings.threads,
      renderer: cardDraws ? 'gpu' : 'cpu',
      // (the card that draws, named as the engine names it; null when the processor draws)
      card: cardDraws ? `${card.name} (Dx12)` : null,
      shading: cardShades ? 'gpu' : 'cpu',
      ram_gb: settings.ram_gb === null ? 6 : settings.ram_gb,
      vram_gb: settings.vram_gb === null ? 4 : settings.vram_gb,
    };
  }

  /** The `settings: …` line the engine writes before it loads the world (tiles/options.rs, `Settings::line`). */
  function settingsLine(r) {
    const lit = effective().lighting.enabled !== false;
    // (the chain on the card when it draws, and shades or the light is off, as in runSettings)
    const chain = r.renderer === 'gpu' && (r.shading === 'gpu' || !lit);
    // (writers: as many as the drawing threads with the chain, else a quarter; WebP wants half of them at least)
    let writers = Math.min(Math.max(chain ? r.threads : Math.ceil(r.threads / 4), 1), CORES);
    if (sim.format !== 'jpeg') writers = Math.min(Math.max(Math.ceil(r.threads / 2), writers), Math.max(CORES, writers));
    const q = settings.quality;
    const tiles = sim.format === 'jpeg'
      ? 'JPEG quality 82 and PNG (the older format)'
      : q === 'lossless' ? 'WebP lossless, fast encoding (level 0)' : `WebP quality ${q}${q === 90 ? ' (default)' : ''}, fast encoding (method 0)`;
    const where = job.zone ? `zone ${job.zone.join(',')}` : 'the whole map';
    return `settings: ${r.threads} drawing${settings.threads === 'auto' ? ' (auto)' : ''} and ${writers} writing threads; `
      + `drawing: ${r.renderer === 'gpu' ? `graphics card ${r.card}` : 'processor'}; `
      + `shading: ${!lit ? 'none (lighting off)' : r.shading === 'gpu' ? 'graphics card' : 'processor'}; `
      + `chain: ${chain ? 'graphics card (8 bits, holes, water, downscaling)' : 'processor'}; `
      + `memory: ${gbText(r.ram_gb)} GB of models and textures${r.renderer === 'gpu' ? `, ${gbText(r.vram_gb)} GB of textures on the card` : ''}; `
      + `tiles: ${tiles}; ${where} → ${sources[job.source].folder}`;
  }

  function startJob(kind, extra) {
    cursorStart += events.length;
    events = [];
    job = {
      id: nextId++,
      kind,
      status: 'running',
      phase: 'starting',
      started_ms: Date.now(),
      duration_ms: 0,
      source: null,
      zone: null,
      // "auto": known once the run says what it took
      threads: settings.threads === 'auto' ? 0 : settings.threads,
      blocks_total: null,
      blocks_done: 0,
      blocks_running: 0,
      errors: 0,
      speed: null,
      remaining_s: null,
      tiles: { jpg: 0, png: 0, empty: 0 },
      message: null,
      settings: null,
      // what the settings asked of a render when it was launched, and what the card left
      asked: isRender(kind) ? { renderer: settings.renderer, shading: settings.shading, lighting: effective().lighting.enabled !== false } : null,
      fallbacks: [],
      changes: null,
      ...extra,
    };
    const now = performance.now();
    // (`format`: what the tiles are written as, kept from the launch as the engine does)
    sim = { start: now, phaseStart: now, lines: 0, format: settings.format };
    if (isRender(kind)) {
      const src = sources[job.source];
      src.rev += 1;
      // an update draws into the whole map: what its folder holds stays what it was
      if (kind !== 'update') {
        src.exists = true;
        src.complete = false;
      }
      if (kind === 'zone') {
        previewBlocks.clear();
        previewZooms.clear();
        src.blocks = null;
        src.bounds = job.zone;
      }
      const z = job.zone;
      let plan = world;
      // (an update draws every block of its ground: a new mapping may be where nothing was)
      if (kind === 'update') plan = z ? blocksOfZones([z]) : [];
      else if (z) plan = world.filter(([x, y]) => x * BLOCK >= z[0] && (x + 1) * BLOCK <= z[2] && y * BLOCK >= z[1] && (y + 1) * BLOCK <= z[3]);
      else if (!wholeWorld) plan = world.filter(([x, y]) => x >= -10 && x < 16 && y >= -24 && y < -2);
      plan = [...plan].sort((a, b) => zOrder(a) - zOrder(b));
      Object.assign(sim, {
        plan,
        next: 0,
        workers: Array.from({ length: runSettings().threads }, () => ({ block: -1, end: 0 })),
        ends: [],
        pace: speedWanted || (z ? 8 : wholeWorld ? 90 : 30),
        zooms: 0,
        blocksStart: 0,
      });
    }
    clearInterval(timer);
    timer = setInterval(advance, 100);
    return job.id;
  }

  /**
   * The job is over. `message`: why it failed, the last `error: …` line of the engine, as the server
   * keeps it (else the server's own words for a process that exited).
   */
  function finishJob(finalStatus, message) {
    if (!job || job.status !== 'running') return;
    job.status = finalStatus;
    job.message = finalStatus === 'failed' ? message || 'the program exited with code 1' : null;
    job.duration_ms = Math.round(performance.now() - sim.start);
    // (the last line of every job, written by the server: src/interface/job.rs, `exited`)
    const took = durationText(job.duration_ms / 1000);
    log(finalStatus === 'done' ? `job done in ${took}` : finalStatus === 'stopped' ? `job stopped after ${took}` : `job failed after ${took}`);
    job.blocks_running = 0;
    job.speed = null;
    job.remaining_s = null;
    if (finalStatus === 'done') job.phase = 'done';
    if (job.source) {
      const src = sources[job.source];
      src.rev += 1;
      // an update of the map: its folder still holds the whole map; what it drew is recorded
      if (job.kind === 'update' && finalStatus === 'done' && job.changes) {
        recorded = true;
        changedResources = [];
      }
      if (finalStatus === 'done' && job.kind !== 'update') {
        src.complete = true;
        src.blocks = sim.plan.length;
        if (sim.plan.length) {
          const xs = sim.plan.map((b) => b[0]);
          const ys = sim.plan.map((b) => b[1]);
          src.bounds = [Math.min(...xs) * BLOCK, Math.min(...ys) * BLOCK, (Math.max(...xs) + 1) * BLOCK, (Math.max(...ys) + 1) * BLOCK];
        }
      }
    }
    clearInterval(timer);
    timer = 0;
    sim = null;
  }

  /** Brings the simulation up to now. */
  function advance() {
    if (!sim || !job || job.status !== 'running') return;
    const t = performance.now();
    job.duration_ms = Math.round(t - sim.start);
    if (isRender(job.kind)) advanceRender(t);
    else advanceTool(t);
  }

  function advanceTool(t) {
    const lines = OUTPUTS[job.kind] || [];
    if (job.kind === 'verify' && !checks.game.ok) {
      if (t - sim.start > 500) {
        // (the engine's words: game/mod.rs, `game_dir`)
        const error = 'error: GTA V folder not found: give it with --game (FiveM records it in CitizenFX.ini, IVPath)';
        log(error);
        finishJob('failed', error);
      }
      return;
    }
    // (no record yet: there is nothing to compare, the search says so and fails)
    if (job.kind === 'list-changes' && !recorded) {
      if (t - sim.start > 700) noRecord();
      return;
    }
    const wanted = Math.min(Math.floor((t - sim.start) / 140), lines.length);
    while (sim.lines < wanted) log(lines[sim.lines++]);
    if (sim.lines >= lines.length && t - sim.start > lines.length * 140 + 300) {
      if (job.kind === 'import-keys') checks.keys = keysPresent(checks.keys.path);
      // what changed since the record (for `record`, the record then written: nothing left)
      if (job.kind === 'list-changes' || job.kind === 'record') {
        job.changes = changesEvent();
        // (the engine's words for a first record: update/mod.rs, `record_resources`)
        if (!recorded) job.changes.note = 'no record before this one: the current state is recorded without drawing anything';
      }
      if (job.kind === 'record') {
        recorded = true;
        changedResources = [];
      }
      finishJob('done');
    }
  }

  /**
   * `update --list` or `--changes` without a record of the resources: the engine says so in its
   * `changes` event (`known: false`, the reason in `note`), then fails with the same words
   * (update/mod.rs, `record_or_refuse`).
   */
  function noRecord() {
    const out = settings.output;
    const why = `${out} has no record of the resources (resources.json): the map was drawn before records existed, or by tiles --zone. If it is up to date, record the current state without drawing anything with "update ${out} --record"; otherwise render the whole map again (tiles records it at the end)`;
    job.changes = { known: false, resources: [], zones: [], blocks: 0, note: why };
    log(`error: ${why}`);
    finishJob('failed', `error: ${why}`);
  }

  function enterPhase(phase, t) {
    job.phase = phase;
    sim.phaseStart = t;
  }

  function advanceRender(t) {
    const elapsed = t - sim.phaseStart;
    const n = sim.plan.length;
    // `&update=fail`: refused before the first block, once its changes are said (as by a lock)
    if (job.kind === 'update' && updateFails && !job.blocks_total && (!sim.byChanges || job.changes) && t - sim.start > 700) {
      // (the engine's words: update/lock.rs, `Lock::take`; the other render is made up)
      const message = `error: ${settings.output} is being written by another job (tiles by process 4242, since ${new Date(Date.now() - 600000).toISOString().replace(/\.\d+Z$/, 'Z')}): wait for it to end (lock ${settings.output}\\write.lock)`;
      log(message);
      finishJob('failed', message);
      return;
    }
    if (job.phase === 'starting' && sim.byChanges && !job.changes) {
      // an update of what changed: the changes first, refused without a record, nothing to do without a zone
      if (elapsed > 500) {
        if (!recorded) {
          noRecord();
          return;
        }
        job.changes = changesEvent();
        const c = job.changes;
        log(`changes: ${c.resources.length} resources, ${c.zones.length} zones, ${c.blocks} blocks to redraw`);
        if (!c.zones.length) {
          log('to draw again: nothing');
          finishJob('done');
          return;
        }
        sim.plan = [...blocksOfZones(c.zones)].sort((a, b) => zOrder(a) - zOrder(b));
        sim.phaseStart = t;
      }
      return;
    }
    if (job.phase === 'starting') {
      if (elapsed > 400) {
        job.settings = runSettings();
        job.threads = job.settings.threads;
        log(settingsLine(job.settings));
        enterPhase('world', t);
      }
    } else if (job.phase === 'world') {
      if (elapsed > 1400) {
        log('world: 159380 archetypes, 11015 placement files, 26 texture links (4 gtxd files), 5.6 s');
        log('variants: 5029 placement files replaced by the copy of a multiplayer pack');
        log('shared textures: 4167 names in 1532 dictionaries of the generic folders (2.5 s)');
        enterPhase('placements', t);
      }
    } else if (job.phase === 'placements') {
      if (elapsed > 1200) {
        log(`placements: ${Math.max(Math.round(n * 0.98), 1)} files, ${n * 115} HD entities, ${n} blocks of 128 m to draw (8.2 s)`);
        emit({ t: 'plan', blocks: copy(sim.plan) });
        job.blocks_total = n;
        if (job.kind !== 'update') sources[job.source].blocks = n;
        enterPhase('blocks', t);
        sim.blocksStart = t;
        sim.zooms = t;
      }
    } else if (job.phase === 'blocks') {
      advanceBlocks(t);
    } else if (job.phase === 'zooms') {
      if (elapsed > 1000) {
        log(`zooms 6 to 0: z6 ${Math.ceil(n / 4)}, z5 ${Math.ceil(n / 16)}, z4 ${Math.ceil(n / 64)}, z3 ${Math.ceil(n / 256)}, z2 ${Math.ceil(n / 1024)}, z1 ${Math.min(n, 4)}, z0 ${Math.min(n, 4)} in 1 s`);
        zoomPass();
        log(`tiles: ${sources[job.source].folder} in ${Math.round((t - sim.start) / 60000)} min`);
        finishJob('done');
      }
    }
  }

  function advanceBlocks(t) {
    const n = sim.plan.length;
    const src = sources[job.source];
    // how long a block takes on one thread, around the wanted pace
    const duration = () => (sim.workers.length / sim.pace) * 1000 * (0.55 + Math.random() * 0.9);
    for (const w of sim.workers) {
      // a worker may finish several blocks between two ticks
      for (let guard = 0; guard < 50; guard++) {
        if (w.block >= 0) {
          if (w.end > t) break;
          finishBlock(w.block, w.end, false);
          w.block = -1;
        }
        if (sim.next >= n) break;
        const begin = w.end && w.end > sim.blocksStart ? w.end : t;
        w.block = sim.next++;
        w.end = begin + duration();
        const [x, y] = sim.plan[w.block];
        emit({ t: 'block', x, y, state: 'started' });
      }
    }
    job.blocks_running = sim.workers.filter((w) => w.block >= 0).length;
    // blocks per second over the last 20 seconds
    const span = Math.min(20000, t - sim.blocksStart);
    while (sim.ends.length && sim.ends[0] < t - 20000) sim.ends.shift();
    job.speed = sim.ends.length && span > 300 ? sim.ends.length / (span / 1000) : null;
    job.remaining_s = job.speed ? (n - job.blocks_done) / job.speed : null;
    // the far zooms of what is drawn, every 5 seconds here (30 in the real program)
    if (t - sim.zooms > 5000 && job.blocks_done < n) {
      sim.zooms = t;
      src.rev += 1;
      zoomPass();
    }
    if (job.blocks_done >= n) {
      const s = (t - sim.blocksStart) / 1000;
      // (the words of the engine, `Format::counts` in tiles/format.rs)
      const { jpg, png, empty } = job.tiles;
      const written = sim.format === 'jpeg' ? `${jpg} JPEG tiles, ${png} PNG, ${empty} empty` : `${jpg + png} WebP tiles (${jpg} full, ${png} partly empty), ${empty} empty`;
      log(`zooms 9 to 7: ${n} blocks in ${s.toFixed(0)} s (${((s * sim.workers.length) / Math.max(n, 1)).toFixed(2)} s per block and per thread); ${written}`);
      job.blocks_running = 0;
      enterPhase('zooms', t);
    }
  }

  /** A pass of the far zooms: they now include every block drawn so far. */
  function zoomPass() {
    if (job.source === 'preview') for (const key of previewBlocks) previewZooms.add(key);
    emit({ t: 'zooms' });
  }

  /** A block is over: its tiles, or its error. `before`: before the page opened (no pace kept). */
  function finishBlock(i, when, before) {
    const n = sim.plan.length;
    const [x, y] = sim.plan[i];
    const failed = n >= 40 && (i === Math.floor(n * 0.31) || i === Math.floor(n * 0.64) || i === Math.floor(n * 0.9));
    job.blocks_done += 1;
    if (!before) sim.ends.push(when);
    fallback(i, n);
    if (failed) {
      const message = ERRORS[i % ERRORS.length];
      job.errors += 1;
      emit({ t: 'block', x, y, state: 'failed', message });
      log(`  block ${x},${y}: ${message}`);
    } else {
      // a block is 1 + 4 + 16 tiles; near the sea some are partly empty (PNG) or empty
      const edge = [[1, 0], [-1, 0], [0, 1], [0, -1]].some(([dx, dy]) => !blockKeys.has(x + dx + ',' + (y + dy)));
      const part = edge ? { jpg: 9, png: 8, empty: 4 } : { jpg: 21, png: 0, empty: 0 };
      job.tiles.jpg += part.jpg;
      job.tiles.png += part.png;
      job.tiles.empty += part.empty;
      // (the page's event has no times nor counts: the server keeps those in the job)
      emit({ t: 'block', x, y, state: 'done' });
      if (job.source === 'preview') previewBlocks.add(x + ',' + y);
    }
    sources[job.source].rev += 1;
    if (job.blocks_done % 50 === 0 || job.blocks_done === n) {
      const s = Math.max((performance.now() - sim.blocksStart) / 1000, 0.1);
      const v = before ? sim.pace : job.blocks_done / s;
      log(`  ${job.blocks_done}/${n} blocks, ${v.toFixed(1)} per second, ${Math.round((n - job.blocks_done) / v / 60)} min to go`);
    }
  }

  /**
   * The work the card leaves to the processor (`&fallback=`), as the engine tells it (its `fallback`
   * events: the total so far each time): from 40 % of the blocks on, one block in three.
   */
  function fallback(i, n) {
    const r = job.settings;
    const kind = ['drawing', 'shading', 'chain'].includes(fallbackWanted) ? fallbackWanted : null;
    if (!kind || !r || r[kind === 'shading' ? 'shading' : 'renderer'] !== 'gpu' || i < Math.floor(n * 0.4) || i % 3 !== 0) return;
    let x = job.fallbacks.find((f) => f.kind === kind);
    if (!x) {
      const reason = {
        drawing: 'the card refused memory for a picture (simulated by the demo)',
        shading: 'the shading device was lost (simulated by the demo)',
        // (a block of the chain that failed alone: made again whole on the processor, nothing said in the log)
        chain: 'card memory refused for this block (simulated by the demo)',
      }[kind];
      x = { kind, blocks: 0, reason };
      job.fallbacks.push(x);
      // (the engine's words: render/gpu/mod.rs `left_behind`, render/gpu/shade.rs)
      if (kind !== 'chain') {
        log(kind === 'drawing'
          ? `renderer: a picture is left to the CPU (${reason}); so are the next ones in this case, without saying it again`
          : `shading: the graphics card failed (${reason}); the rest is shaded by the CPU`);
      }
    }
    x.blocks += 1;
  }

  /** `&running=1`: the page opens on a render of the whole map that is already half done. */
  function startRunning() {
    startJob('map', { source: 'map' });
    job.settings = runSettings();
    job.threads = job.settings.threads;
    const t = performance.now();
    const back = 95000;
    sim.start = t - back;
    job.started_ms = Date.now() - back;
    log(settingsLine(job.settings));
    log('world: 159380 archetypes, 11015 placement files, 26 texture links (4 gtxd files), 5.6 s');
    log('variants: 5029 placement files replaced by the copy of a multiplayer pack');
    log('shared textures: 4167 names in 1532 dictionaries of the generic folders (2.5 s)');
    const n = sim.plan.length;
    log(`placements: ${Math.round(n * 0.98)} files, ${n * 115} HD entities, ${n} blocks of 128 m to draw (8.2 s)`);
    emit({ t: 'plan', blocks: copy(sim.plan) });
    job.blocks_total = n;
    job.phase = 'blocks';
    sim.phaseStart = t;
    sim.blocksStart = t - 80000;
    sim.zooms = t;
    const done = Math.floor(n * 0.42);
    for (let i = 0; i < done; i++) {
      const [x, y] = sim.plan[i];
      emit({ t: 'block', x, y, state: 'started' });
      finishBlock(i, 0, true);
    }
    zoomPass();
    sim.next = done;
    // the pace from before the page opened, so that a speed is known at once
    for (let i = 0; i < 200; i++) sim.ends.push(t - 20000 + i * 100);
    sim.blocksStart = t - 20000;
  }

  // ------------------------------------------------------------------------------------- the machine and its load

  let knownMachine = null;
  let loadView = null;
  const serverStart = performance.now();
  const read = { cpu: 6, gpu: 3, ram: 14.2, vram: 1.1 };
  const history = [];
  const round = (x, digits) => Math.round(x * 10 ** digits) / 10 ** digits;

  /** Where the load goes now: idle, or what the simulated render takes. */
  function targets() {
    const rendering = job && job.status === 'running' && isRender(job.kind) && job.settings;
    if (!rendering) return { cpu: job && job.status === 'running' ? 14 : 5, gpu: 2, ram: 14.2, vram: 1.1 };
    const r = job.settings;
    const low = settings.priority === 'low';
    if (job.phase !== 'blocks') return { cpu: 16, gpu: 3, ram: 16 + Math.min(r.ram_gb, 30) * 0.2, vram: r.renderer === 'gpu' ? 1.6 : 1.1 };
    const busy = Math.min(r.threads / CORES, 1);
    const shadesOnCard = r.shading === 'gpu';
    return {
      cpu: Math.min(8 + 90 * busy - (shadesOnCard ? 8 : 0), low ? 90 : 99),
      gpu: r.renderer === 'gpu' ? (shadesOnCard ? 91 : 64) * Math.min(busy * 1.15, 1) : shadesOnCard ? 40 : 3,
      ram: 14.2 + Math.min(r.ram_gb, 60) * 0.92,
      vram: r.renderer === 'gpu' || shadesOnCard ? 1.1 + Math.min(r.vram_gb, 22.5) * 0.9 : 1.1,
    };
  }

  function sample() {
    advance();
    const c = targets();
    const noise = (a) => (Math.random() - 0.5) * a;
    read.cpu = Math.min(Math.max(read.cpu + (c.cpu - read.cpu) * 0.35 + noise(6), 0.5), 100);
    read.gpu = Math.min(Math.max(read.gpu + (c.gpu - read.gpu) * 0.4 + noise(5), 0), 100);
    read.ram += (c.ram - read.ram) * 0.12 + noise(0.08);
    read.vram = Math.max(read.vram + (c.vram - read.vram) * 0.2 + noise(0.04), 0.3);
    const gpu = cardCounters && knownMachine && knownMachine.cards.some((k) => k.type !== 'software') ? round(read.gpu, 1) : null;
    history.push([round((performance.now() - serverStart) / 1000, 1), round(read.cpu, 1), gpu]);
    while (history.length > 120) history.shift();
    loadView = {
      cpu: round(read.cpu, 1),
      gpu,
      ram_used_gb: round(read.ram, 2),
      vram_used_gb: gpu === null ? null : round(read.vram, 2),
      history: copy(history),
    };
  }

  // the machine is listed a moment after the start, the load read every second
  setTimeout(() => {
    knownMachine = copy(machine);
  }, 800);
  setInterval(sample, 1000);

  // ------------------------------------------------------------------------------------- routes

  function stateView(since) {
    advance();
    const cursor = cursorStart + events.length;
    const evs = since === null ? events : events.slice(Math.max(since - cursorStart, 0));
    return copy({
      version: VERSION,
      cores: CORES,
      machine: knownMachine,
      load: loadView,
      settings,
      base,
      effective: effective(),
      checks,
      sources,
      job,
      cursor,
      events: evs,
    });
  }

  // (a job that needs the game, refused as the server refuses it, src/interface/mod.rs `start`)
  const noGame = () => refusal(400, 'launch_game_not_found', 'game folder: GTA V folder not found: FiveM does not record it in CitizenFX.ini. Choose it.');
  const noKeys = () => refusal(400, 'ng_keys_missing', 'NG keys missing: import them first from CodeWalker\'s magic.dat');

  // (the checks in the server's order, src/interface/mod.rs: `order`, then `launch` and `start`)
  function launch(body) {
    advance();
    // what the body asks: the kind, and the zone or the changes it needs
    const kind = body && typeof body.kind === 'string' && JOB_KINDS.includes(body.kind) ? body.kind : null;
    if (!kind) return refusal(400, 'unknown_job', UNKNOWN_JOB);
    let zone = null;
    let byChanges = false;
    if (kind === 'zone') {
      const [z, refused] = zoneAsked(body.zone);
      if (refused) return refused;
      zone = z;
    } else if (kind === 'update') {
      // (null and false are as good as absent)
      const given = body.zone !== undefined && body.zone !== null;
      const changes = body.changes !== undefined && body.changes !== null && body.changes !== false;
      if (given && changes) return refusal(400, 'update_zone_and_changes', 'update: a zone or the changes of the resources, not both');
      if (given) {
        const [z, refused] = zoneAsked(body.zone);
        if (refused) return refused;
        zone = z;
      } else if (body.changes === true) byChanges = true;
      else return refusal(400, 'update_zone_or_changes', 'update: "zone" [x1, y1, x2, y2] or "changes": true expected');
    }
    if (job && job.status === 'running') return refusal(409, 'job_running', 'a job is already running');
    // a job that depends on map.toml is not run on the defaults when the file is broken
    if (!checks.config.ok && kind !== 'verify' && kind !== 'import-keys') {
      const detail = checks.config.detail;
      return refusal(400, 'config_broken', `map.toml cannot be read, nothing is launched: ${detail}`, { detail });
    }
    // `update`: on the folder of the whole map, which must hold one
    const updates = kind === 'update' || kind === 'list-changes' || kind === 'record';
    if (updates && !sources.map.exists) {
      const folder = sources.map.folder;
      return refusal(400, 'no_map_to_update', `no map to update: the whole-map folder (${folder}) has no BTMS tiles.json. Render the whole map first, or choose the folder of a map already drawn.`, { folder });
    }
    if (isRender(kind) || updates) {
      if (!checks.game.ok) return noGame();
      if (!checks.keys.ok) return noKeys();
    }
    if (kind === 'import-keys' && (typeof body.file !== 'string' || !body.file.trim())) {
      return refusal(400, 'import_keys_no_file', 'import-keys: the magic.dat file must be given ("file")');
    }
    if (kind === 'zone') return [200, { job_id: startJob('zone', { source: 'preview', zone }) }];
    if (kind === 'map') return [200, { job_id: startJob('map', { source: 'map' }) }];
    if (kind === 'update') {
      const id = startJob('update', { source: 'map', zone });
      sim.byChanges = byChanges;
      return [200, { job_id: id }];
    }
    return [200, { job_id: startJob(kind) }];
  }

  /** A fake dialog: the answer follows what the dialog is for (`purpose`), never its translated title. */
  async function choose(body) {
    await wait(350);
    if (!body || (body.kind !== 'folder' && body.kind !== 'file')) return refusal(400, 'choose_unknown_kind', 'kind: folder or file expected');
    if (body.kind === 'file') return [200, { path: 'C:\\Users\\demo\\Downloads\\magic.dat' }];
    foldersChosen += 1;
    if (body.purpose === 'game') return [200, { path: 'E:\\SteamLibrary\\steamapps\\common\\Grand Theft Auto V' }];
    if (body.purpose === 'resources') return [200, { path: `C:\\Server\\resources\\[maps]\\example-${foldersChosen}` }];
    return [200, { path: `D:\\Tiles\\folder-${foldersChosen}` }];
  }

  async function router(method, path, headers, body) {
    const [route, query = ''] = path.split('?');
    if (!route.startsWith('/api/')) return refusal(404, 'unknown_page', 'unknown page');
    if (!headers || !headers['X-BTMS-Token'] || tokenRefused) return refusal(403, 'bad_token', 'token missing or wrong: only the page served by this interface can command it');
    if (route === '/api/state') {
      if (method !== 'GET') return refusal(405, 'method_not_allowed', 'method not allowed');
      const s = new URLSearchParams(query).get('since');
      return [200, stateView(s === null || s === '' ? null : Number(s))];
    }
    // (the server's order: a route it does not know is a 404 whatever the method)
    const posted = ['/api/settings', '/api/choose', '/api/launch', '/api/stop', '/api/open', '/api/quit'];
    if (!posted.includes(route)) return refusal(404, 'unknown_route', 'unknown route');
    if (method !== 'POST') return refusal(405, 'method_not_allowed', 'method not allowed');
    switch (route) {
      case '/api/settings':
        return changeSettings(body);
      case '/api/choose':
        return choose(body);
      case '/api/launch':
        return launch(body);
      case '/api/stop':
        advance();
        finishJob('stopped');
        return [200, { ok: true }];
      case '/api/open':
        if (!body || !['output', 'preview', 'config'].includes(body.target)) return refusal(400, 'open_unknown_target', 'to open: output, preview or config');
        return [200, { ok: true }];
      case '/api/quit':
        finishJob('stopped');
        dead = true;
        return [200, { ok: true }];
      default:
        return refusal(404, 'unknown_route', 'unknown route');
    }
  }

  /** The transport of app.js: resolves to { status, json }, rejects when the server is away. */
  async function request(method, path, headers, body) {
    await wait(25 + Math.random() * 40);
    if (dead || performance.now() < outageUntil) throw new TypeError('Failed to fetch');
    const key = `${method} ${path.split('?')[0]}`;
    calls[key] = (calls[key] || 0) + 1;
    const [status, json] = await router(method, path, headers, body === undefined ? undefined : copy(body));
    return { status, json };
  }

  /** Where the tiles are: nowhere without `&tiles=`. */
  function tileUrls(source, z, tx, ty, rev) {
    if (!tilesBase) return null;
    if (source === 'preview') {
      // The preview shows the tiles of the blocks its last render drew, and the far zooms once a
      // pass has made them. Those come from the whole map here: they show more than the zone.
      if (z >= 7) {
        const f = 2 ** (z - 7);
        if (!previewBlocks.has(Math.floor(tx / f) + ',' + Math.floor(ty / f))) return null;
      } else {
        const f = 2 ** (7 - z);
        let inside = false;
        for (const key of previewZooms) {
          const [bx, by] = key.split(',').map(Number);
          if (Math.floor(bx / f) === tx && Math.floor(by / f) === ty) {
            inside = true;
            break;
          }
        }
        if (!inside) return null;
      }
    }
    const address = `${tilesBase}/${z}/${tx}/${ty}`;
    return tileExtensions.map((ext) => `${address}.${ext}?v=${rev}`);
  }

  function outage(seconds) {
    outageUntil = performance.now() + seconds * 1000;
  }

  if (params.get('running')) startRunning();
  const delay = Number(params.get('outage'));
  if (delay > 0) setTimeout(() => outage(6), delay * 1000);
  // for the console: cut the server, count the requests it got, ask it something
  window.btmsDemo = { outage, calls, request };

  return { request, tileUrls };
}
