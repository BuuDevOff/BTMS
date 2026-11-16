# Tile format

This page describes what a render writes: the tile grid, how GTA V world coordinates map to tiles,
the files and their transparency, the manifest `tiles.json`, the resources record
`resources.json`, and the viewer page. It ends with ready-to-paste code for Leaflet and
OpenLayers.

The grid is **not** the usual web-mercator XYZ scheme. It is a flat grid in game metres, so a
position from the game goes onto the map with no projection at all.

- [The folder](#the-folder)
- [The grid](#the-grid)
- [From GTA coordinates to tiles, and back](#from-gta-coordinates-to-tiles-and-back)
- [Files and transparency](#files-and-transparency)
- [The manifest: `tiles.json`](#the-manifest-tilesjson)
- [The resources record: `resources.json`](#the-resources-record-resourcesjson)
- [The lock file](#the-lock-file)
- [The viewer page](#the-viewer-page)
- [Using the tiles in your own map](#using-the-tiles-in-your-own-map)

## The folder

```text
output/tiles/
├── index.html                 the viewer page
├── tiles.json                 the manifest: grid, format, covered area, render state
├── resources.json             the record of your resources (after a whole-map render)
├── write.lock                 present only while a render or an update writes the folder
├── 0/
│   ├── -1/
│   │   ├── -1.webp
│   │   └── 0.webp
│   └── 0/
│       ├── -1.webp
│       └── 0.webp
├── 1/ … 8/
└── 9/
    └── <tx>/<ty>.webp
```

- One folder per zoom, one sub-folder per tile column `tx`, one file per tile row `ty`:
  **`<z>/<tx>/<ty>.<extension>`**. The numbers are plain decimal integers, often negative
  (`9/6/-30.webp`), with no padding.
- A tile with nothing on it has **no file**.
- The folder is a complete static website: upload it as it is, at the root of a site or in a
  sub-folder. Every address the viewer uses is relative to `tiles.json`.
- The whole map is about 6,100 blocks of 128 m, or about 240,000 tiles and 10 GB at the default
  quality.

## The grid

| Property | Value |
|---|---|
| Zooms | `0` to `zmax`, with `zmax = 9` |
| Tile size | `tile = 512` px square |
| Side of a tile at zoom `z` | `W = 32 · 2^(zmax − z)` metres |
| One pixel at zoom `zmax` | `metres_per_pixel_zmax = 0.0625` m (6.25 cm) |
| Axes | `x` grows east, `y` grows north: the game's own world coordinates |
| Tile `(z, tx, ty)` | covers `x ∈ [tx·W, (tx + 1)·W)` and `y ∈ [ty·W, (ty + 1)·W)` |

`ty` grows **northwards**, the opposite of most web tile schemes, and both indices can be negative.
The coordinates are the ones your scripts already use (for example what `GetEntityCoords` returns),
in metres. The height (`z` in the game) plays no part: the tiles are a view from straight above.

| Zoom | Side of a tile `W` | One pixel | Note |
|---:|---:|---:|---|
| 0 | 16,384 m | 32 m | four tiles around the origin cover everything |
| 1 | 8,192 m | 16 m | |
| 2 | 4,096 m | 8 m | |
| 3 | 2,048 m | 4 m | |
| 4 | 1,024 m | 2 m | |
| 5 | 512 m | 1 m | |
| 6 | 256 m | 0.5 m | |
| 7 | 128 m | 0.25 m | one render block |
| 8 | 64 m | 0.125 m | |
| 9 | 32 m | 0.0625 m | the most detailed zoom |

**Zoom 0** holds four tiles: `(−1, −1)`, `(0, −1)`, `(−1, 0)` and `(0, 0)`. Together they cover
`[−16,384, 16,384)` m on both axes, which holds San Andreas and Cayo Perico with room to spare.

**Parents and children.** The four children of `(z, tx, ty)` are `(z + 1, 2·tx + a, 2·ty + b)` with
`a, b ∈ {0, 1}`: `a = 0` is the western half, `b = 1` the northern half. The parent of a tile is
`(z − 1, floor(tx / 2), floor(ty / 2))`. Use a real floor: `floor(−3 / 2) = −2`, while a division
that truncates towards zero would give `−1`.

**Inside a tile**, row 0 of the picture is the **north** edge and column 0 the **west** edge. In a
parent tile, the north-western child fills the top-left quarter.

**Blocks.** A render draws the map in blocks of 128 m, and a block is exactly one zoom 7 tile:
block `(bx, by)` is tile `(7, bx, by)`. It is also the 4 × 4 zoom 9 tiles `tx = 4·bx … 4·bx + 3`,
`ty = 4·by … 4·by + 3`, and the 2 × 2 zoom 8 tiles above them. Zooms 6 to 0 are made from the
zoom 7 tiles. A `--zone x1,y1,x2,y2` is widened outwards to whole blocks, and `btms probe <bx> <by>`
takes block numbers (the interface shows them under **block**).

## From GTA coordinates to tiles, and back

With `W` the side of a tile at zoom `z` and `p` the size of one of its pixels:

```text
W  = metres_per_pixel_zmax · tile · 2^(zmax − z)       = 32 · 2^(9 − z) m
p  = W / tile                                          (metres per pixel)

tx = floor(x / W)
ty = floor(y / W)
i  = floor((x − tx·W) / p)              column in the tile, 0 = west edge
j  = floor(((ty + 1)·W − y) / p)        row in the tile,    0 = north edge
```

And back:

```text
tile (z, tx, ty) covers   x ∈ [tx·W, (tx + 1)·W)   and   y ∈ [ty·W, (ty + 1)·W)
centre of pixel (i, j)    x = tx·W + (i + ½)·p     and   y = (ty + 1)·W − (j + ½)·p
```

A point exactly on the south edge of a tile (`y = ty·W`) belongs to that tile; the row formula
then gives `j = tile`, which you clamp to `tile − 1`.

**Worked example.** A point in downtown Los Santos: `x = 195.2`, `y = −933.8`.

| Zoom | `W` | `p` | `tx` | `ty` | File | Pixel `(i, j)` |
|---:|---:|---:|---:|---:|---|---|
| 9 | 32 m | 0.0625 m | `floor(6.1) = 6` | `floor(−29.18) = −30` | `9/6/-30.webp` | `(51, 92)` |
| 7 | 128 m | 0.25 m | `floor(1.53) = 1` | `floor(−7.30) = −8` | `7/1/-8.webp` (block `1, −8`) | `(268, 151)` |
| 0 | 16,384 m | 32 m | `floor(0.012) = 0` | `floor(−0.057) = −1` | `0/0/-1.webp` | `(6, 29)` |

At zoom 9: `i = floor((195.2 − 192) / 0.0625) = floor(51.2) = 51` and
`j = floor((−928 + 933.8) / 0.0625) = floor(92.8) = 92`. Back again, tile `9/6/-30` covers
`x ∈ [192, 224)` and `y ∈ [−960, −928)`, and the centre of its pixel `(51, 92)` is
`(195.22, −933.78)`. The parent of `9/6/-30` is `8/3/-15`, and its block is `(floor(6 / 4), floor(−30 / 4)) = (1, −8)`.

The same in JavaScript, reading the grid from the manifest:

```js
/** The tile and pixel of GTA coordinates (x, y) at zoom z. */
function tileOf(manifest, x, y, z) {
  const W = manifest.metres_per_pixel_zmax * manifest.tile * 2 ** (manifest.zmax - z);
  const p = W / manifest.tile;
  const tx = Math.floor(x / W);
  const ty = Math.floor(y / W);
  const i = Math.floor((x - tx * W) / p);
  const j = Math.min(manifest.tile - 1, Math.floor(((ty + 1) * W - y) / p));
  return { z, tx, ty, i, j, path: `${z}/${tx}/${ty}.${manifest.extension}` };
}

/** The GTA coordinates of the centre of pixel (i, j) of tile (z, tx, ty). */
function centreOf(manifest, z, tx, ty, i, j) {
  const W = manifest.metres_per_pixel_zmax * manifest.tile * 2 ** (manifest.zmax - z);
  const p = W / manifest.tile;
  return { x: tx * W + (i + 0.5) * p, y: (ty + 1) * W - (j + 0.5) * p };
}
```

## Files and transparency

A folder holds tiles of **one format**, which the manifest's `extension` names.

| A tile that is… | WebP (the default) | `--format jpeg` |
|---|---|---|
| empty: every pixel transparent | no file | no file |
| full: every pixel opaque | `.webp` with no alpha plane | `.jpg`, quality 82, 4:4:4 |
| partly empty | `.webp` with its transparency | `.png`, lossless |

- **WebP, lossy** (the default, `--quality` 90): VP8. On a partly empty tile the colours are
  lossy and the alpha plane is lossless.
- **WebP, lossless** (`--quality lossless`): VP8L, every pixel exactly as drawn, several times
  heavier.
- **Encoding effort.** A render encodes fast (method 0, plain RGB to YUV conversion);
  `btms convert` encodes with care (method 4, "sharp" YUV conversion). `--webp-effort` chooses.
- **JPEG and PNG.** With `--format jpeg`, `extension` is `"jpg"`, and partly empty tiles are
  stored as `.png` beside the `.jpg` ones. A reader of such a folder tries `.jpg` first, then
  `.png`. `btms convert <folder> --to <other folder>` rewrites a finished folder as WebP into
  another folder, never in place.

**Where tiles are transparent.** Only where nothing is drawn:

- beyond the drawn area. With `[water] enabled = true` (the default), the sea is drawn over every
  block of the rectangle around the map, so the tiles end at the edge of that rectangle. With water
  off, the tiles show the seabed where the game has one, and nothing beyond it;
- in a hole that remains, for example the ground of a protected resource, or an empty region
  larger than 2,000 m² that the last pass of hole filling leaves (see
  [troubleshooting.md](troubleshooting.md#holes-and-transparent-areas));
- in a zone render, around the zone at zooms 6 to 0, whose tiles cover more ground than the zone.

Halving a tile takes the mean of each 2 × 2 pixels, weighted by alpha, and their mean alpha. Along
an edge the pixels of the lower zooms are therefore partly transparent, and a lone speck a few
pixels wide fades out of the far zooms.

**A missing file means an empty tile.** Because each lower zoom is made from its four children, a
finished render has no file under a tile that has none, apart from such specks. The viewer relies
on this: in a finished render it does not ask for the tiles under one that has no file.

**Writing.** Every tile is written whole: first beside its final name, with `.tmp` appended
(`-30.webp.tmp`), then renamed over it. A reader never sees half a tile. `btms tiles` writes over
the tiles already in a folder (a tile written in one format replaces the same tile in another),
but never removes a tile that now comes out empty; render into an empty folder to start clean.
`btms update` does delete the tiles that come out empty.

**One format per folder.** Rendering a `--zone` into a folder that holds tiles of another format is
refused before anything is read: readers follow `extension`, and would then see only the zone.

## The manifest: `tiles.json`

`tiles.json` is written at the start of a render with `"complete": false`, and again at the end
with `"complete": true`. `btms update` rewrites it after each update, and `btms convert` writes the
one of the folder it creates.

**What a reader needs:** `zmax`, `tile`, `metres_per_pixel_zmax`, `extension`, `complete` and
`bounds`. Read them rather than hard-coding the numbers above. The other keys describe the render
for people.

An example, from a finished render of 1 km² of downtown Los Santos
(`--zone 0,-1536,1024,-512`, 8 × 8 blocks) with the default lighting and one resource. The free
text is abridged. BTMS writes the keys in alphabetical order, as here; the table below takes them
in a more useful order.

```json
{
  "blocks": 64,
  "bounds": [0.0, -1536.0, 1024.0, -512.0],
  "complete": true,
  "effort": "fast",
  "encoder": "libwebp … lossy, method 0, plain YUV conversion",
  "extension": "webp",
  "files": "<z>/<tx>/<ty>.webp …",
  "format": "btms tiles v1",
  "lighting": {
    "ambient_occlusion": 0.3,
    "azimuth": 315.0,
    "elevation": 35.0,
    "shadow_reach_m": 48.0,
    "shadows": true,
    "strength": 0.6
  },
  "metres_per_pixel_zmax": 0.0625,
  "page": "index.html",
  "quality": 90,
  "resources": ["my-island"],
  "side_m": "32 · 2^(9 − z)",
  "source": "GTA V game files (placements, models, textures), seen from straight above, shaded …",
  "tile": 512,
  "y": "ty grows northwards; tile (z, tx, ty) covers …",
  "zmax": 9,
  "zone": [0.0, -1536.0, 1024.0, -512.0]
}
```

| Key | Type | Meaning |
|---|---|---|
| `format` | string | `"btms tiles v1"`: says that the file is a BTMS manifest |
| `source` | string | what the tiles show, in words; for people, do not parse |
| `zmax` | integer | the most detailed zoom: `9` |
| `tile` | integer | the side of a tile in pixels: `512` |
| `side_m` | string | the side of a tile in metres as a formula, `"32 · 2^(9 − z)"`; for people |
| `metres_per_pixel_zmax` | number | metres per pixel at zoom `zmax`: `0.0625` |
| `y` | string | the orientation of the grid in words; for people |
| `zone` | `[x1, y1, x2, y2]` or `null` | the rectangle given with `--zone`, in metres, with `x1 ≤ x2` and `y1 ≤ y2`; `null` for the whole map |
| `blocks` | integer | the number of 128 m blocks of the render; an update adds the blocks that gained tiles and removes those that lost them |
| `complete` | boolean | `false` while a render writes the folder, or after one that stopped before its end; `true` once it finished |
| `lighting` | object | the lighting the tiles were shaded with; absent when the lighting is off (see below) |
| `extension` | string | the extension of the tiles: `"webp"`, or `"jpg"` for a JPEG and PNG folder |
| `quality` | integer or string | the WebP quality (0 to 100), `"lossless"` for lossless WebP, or `82` for JPEG |
| `encoder` | string | the encoder and its settings (libwebp version, method, YUV conversion); for people |
| `effort` | string | WebP only: `"fast"` (a render's default) or `"careful"` (the default of `btms convert`) |
| `files` | string | the file layout in words; for people |
| `page` | string | the viewer page beside the manifest: `"index.html"` |
| `bounds` | `[x1, y1, x2, y2]` | the ground the zoom 7 tiles cover, in metres, on whole 128 m blocks. Written once the render is complete; absent while it runs |
| `resources` | array of strings | the folder names of the server resources drawn over the game; absent without resources |
| `converted_from` | object | only in a folder written by `btms convert` (see below) |
| `updates` | array | only after `btms update`: one entry per update, the last 100 kept (see below) |

**`lighting`**, when the tiles are shaded:

| Key | Meaning |
|---|---|
| `azimuth` | where the sun comes from, in degrees (`[lighting] azimuth`) |
| `elevation` | the height of the sun above the horizon, in degrees (`[lighting] elevation`) |
| `strength` | the shading strength, 0 to 1 (`[lighting] strength`) |
| `shadows` | whether shadows are cast (`[lighting] shadows`) |
| `ambient_occlusion` | the darkening at the foot of walls, 0 to 1 (`[lighting] ambient_occlusion`) |
| `shadow_reach_m` | how far a shadow reaches, in metres: `48` |

**`converted_from`**, in a folder written by `btms convert`. The rest of the manifest is the
source folder's, with the new format's keys.

| Key | Meaning |
|---|---|
| `folder` | the name of the source folder, without its path (the manifest is meant to be published) |
| `extension` | the source's extension |
| `quality` | the source's quality |
| `note` | a reminder that the tiles were decoded and encoded again, so that those of a JPEG keep its artefacts |

**`updates`**, one entry per `btms update`:

| Key | Meaning |
|---|---|
| `date` | when it ran, in UTC (`2026-11-15T14:03:22Z`) |
| `zones` | the rectangles redrawn, in metres, `[x1, y1, x2, y2]` each |
| `blocks` | how many blocks were drawn again |
| `cleared` | how many blocks were asked for but hold nothing any more (their tiles removed) |
| `errors` | how many blocks could not be written |
| `by` | what asked for it: `"zone"` (`--zone`) or `"changes"` (`--changes`) |
| `resources` | with `--changes`: the resources added, changed or removed |

An update keeps the map's `zone`, format and lighting. It sets `bounds`, `blocks` and
`resources` to the map as it now is.

## The resources record: `resources.json`

`resources.json` is what `btms update --changes` compares your resources with. It is written
into the map folder at the end of a whole-map `btms tiles` (not with `--zone`), and by
`btms update --changes` and `btms update --record`. It is not written when blocks failed. It never
holds a full disk path, so the folder can be published as it is.

| Key | Meaning |
|---|---|
| `format` | `"btms resources v1"`: says that the file is a BTMS resources record |
| `date` | when it was written, in UTC |
| `by` | what wrote it: `"tiles"`, `"update --changes"` or `"update --record"` |
| `settings` | the `[maps]`, `[lighting]` and `[water]` settings the map was drawn with. When they no longer match, `--changes` is refused: draw the whole map again |
| `folders` | the last name of each folder of `[resources] folders` |
| `on_demand` | the on-demand maps the render drew, or `null` when not known (`--record` does not search for holes) |
| `resources` | one entry per resource (below) |

Each resource entry:

| Key | Meaning |
|---|---|
| `name` | its folder under the resource folders, for example `[my-island]/my-island-map` |
| `fingerprint` | a quick fingerprint of the files that can change the map: the manifest, everything under `stream/`, and the `.lua`, `.js`, `.meta`, `.xml` and `.ide` files, by relative path, size and date (not their content) |
| `files` | how many files the fingerprint covers |
| `zone` | the ground the resource touches, in metres `[x1, y1, x2, y2]`, or `null` |
| `blocks` | the same ground in 128 m blocks, as rectangles `[bx1, by1, bx2, by2]`, both ends included |
| `readable` | whether all its map files could be read (none protected or unreadable) |
| `reason` | why not, or why it has no ground; `null` otherwise |

## The lock file

While `btms tiles` or `btms update` writes a folder, the folder holds `write.lock`. It is a small
JSON file: `pid` (the process number), `program` (the executable's file name, `btms.exe` or `btms-gui.exe`),
`command` (`tiles` or `update`) and `since` (when it was taken, in UTC). A second render or update
into the same folder is refused while that process runs. A lock whose process is gone, or whose
process number now belongs to another program, is stale and is taken over. The file goes away when
the writer ends. It does no harm on a web server, but there is no reason to upload it.
See [troubleshooting.md](troubleshooting.md#a-folder-is-being-written-by-another-job).

## The viewer page

`index.html` is written next to `tiles.json` at the start of a render, and by `btms convert`. It is
one self-contained file: it loads nothing from the network but the folder's own files.

- **Where it looks.** It reads `tiles.json` next to itself. When its address has no final slash
  (`https://example.com/map`), it also tries `map/tiles.json`. Tile addresses are relative to the
  manifest.
- **What it shows.** Pan and zoom with the mouse, touch or keyboard; the game coordinates under the
  pointer (`x`, `y`, in metres); the zoom and a scale bar; an information panel (format and quality,
  the most detailed zoom and its centimetres per pixel, the render state and block count, the
  lighting, the number of server resources). It goes two zooms past `zmax`, enlarging the zoom 9
  tiles, and two zooms below 0.
- **Languages.** A selector offers English, Français, Deutsch, Nederlands, Русский, Svenska and
  Español. It starts with the browser's language and remembers the choice in `localStorage`
  (`btms.language`).
- **A link to a view.** The address keeps the view as `#<zoom>/<x>/<y>`, for example
  `index.html#7.00/195.2/-933.8`: copy it to share a place.
- **While a render runs.** As long as `complete` is `false`, the page reads `tiles.json` again every
  30 seconds (while the tab is visible). It then asks again for the tiles it has not found yet,
  and for the far zooms (6 to 0), which the render rebuilds as it goes. A folder can therefore be
  online while it fills.
- **Caching.** Each tile address carries `?v=<fingerprint of tiles.json>`, so a browser or a CDN
  never mixes the tiles of a new render with those of an older one. Static servers ignore the
  query string.
- **Missing tiles.** A `404`, `403` or `410` answer counts as an empty tile, and so does a `text/…`
  page answered with `200` (the "not found" page some hosts send).
- **What "show all" frames.** The manifest's `bounds`; otherwise its `zone`, widened to whole
  blocks; otherwise the ground the four zoom 0 tiles show something on; otherwise San Andreas.
  When the manifest gives `bounds` or a `zone`, or once the limits of a finished render are read
  from its zoom 0 tiles, no tile outside them is asked for.
- **Opened from the disk** (`file://`), the page cannot read `tiles.json`. It falls back to the
  default grid and shows what it can. Serve the folder over HTTP instead, as in
  [getting-started.md](getting-started.md#8-put-the-map-on-a-website).
- **WebP.** A browser that cannot show WebP gets a message saying so.

To embed the map in a page of your own, put `index.html` in an `<iframe>`, or load the tiles in a
map library as below.

## Using the tiles in your own map

These examples load the folder's manifest and set the grid from it. `base` is the folder that holds
`tiles.json`, relative to your page. Both place a marker at GTA coordinates and show the
coordinates under the pointer.

Both assume one file extension per folder, which is the case for WebP, the default. For a JPEG and
PNG folder, convert it first: `btms convert <folder> --to <new folder>`.

Some tile requests end with a `404`: those are empty tiles, which have no file. Limiting the layer
to `bounds`, as below, avoids most of them.

### Leaflet

Leaflet's `CRS.Simple` with this transformation puts one CSS pixel on 32 m at zoom 0, so Leaflet's
zoom levels are BTMS's zoom levels. Game coordinates go in as `[y, x]` (latitude is `y`, longitude
is `x`). Leaflet counts tile rows southwards from the origin, so row `c.y` is BTMS row `−c.y − 1`.

```html
<link rel="stylesheet" href="https://unpkg.com/leaflet@1.9.4/dist/leaflet.css">
<script src="https://unpkg.com/leaflet@1.9.4/dist/leaflet.js"></script>
<div id="map" style="height: 100vh"></div>
<script>
  const base = 'tiles/'; // the folder that holds tiles.json, relative to this page

  /** GTA world coordinates (x east, y north, metres) to a Leaflet position. */
  const gta = (x, y) => L.latLng(y, x);

  fetch(base + 'tiles.json').then((r) => r.json()).then((manifest) => {
    // metres per CSS pixel at zoom 0 (32 with the default grid)
    const unit = manifest.metres_per_pixel_zmax * 2 ** manifest.zmax;
    const crs = L.extend({}, L.CRS.Simple, {
      transformation: L.transformation(1 / unit, 0, -1 / unit, 0),
    });
    const map = L.map('map', { crs, minZoom: 0, maxZoom: manifest.zmax + 2 });

    // BTMS counts tile rows northwards, Leaflet southwards
    const BtmsTiles = L.TileLayer.extend({
      getTileUrl: (c) => `${base}${c.z}/${c.x}/${-c.y - 1}.${manifest.extension}`,
    });
    // [x1, y1, x2, y2] in metres, once the render is complete
    const area = manifest.bounds;
    const bounds = area && L.latLngBounds(gta(area[0], area[1]), gta(area[2], area[3]));
    new BtmsTiles('', {
      tileSize: manifest.tile,
      maxNativeZoom: manifest.zmax,
      maxZoom: manifest.zmax + 2,
      bounds,
    }).addTo(map);

    if (bounds) map.fitBounds(bounds);
    else map.setView(gta(0, 2000), 2);

    // a marker at GTA coordinates
    L.marker(gta(195.2, -933.8)).addTo(map).bindPopup('x 195.2, y -933.8');

    // the GTA coordinates under the pointer
    map.on('mousemove', (e) => {
      document.title = `x ${e.latlng.lng.toFixed(1)}  y ${e.latlng.lat.toFixed(1)}`;
    });
  });
</script>
```

### OpenLayers

OpenLayers takes a projection in metres, so game coordinates go in as `[x, y]`. The tile grid
has its origin at `(0, 0)` and one resolution per zoom. OpenLayers counts tile rows southwards from
the origin, so row `y` is BTMS row `−y − 1`.

```html
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/ol@10.5.0/ol.css">
<script src="https://cdn.jsdelivr.net/npm/ol@10.5.0/dist/ol.js"></script>
<div id="map" style="height: 100vh"></div>
<script>
  const base = 'tiles/'; // the folder that holds tiles.json, relative to this page

  fetch(base + 'tiles.json').then((r) => r.json()).then((m) => {
    const mpp = m.metres_per_pixel_zmax;
    const half = m.tile * mpp * 2 ** m.zmax; // zoom 0 is 2 × 2 tiles around (0, 0)
    const extent = [-half, -half, half, half];
    const resolutions = Array.from({ length: m.zmax + 1 }, (_, z) => mpp * 2 ** (m.zmax - z));
    const projection = new ol.proj.Projection({ code: 'BTMS', units: 'm', extent });
    const source = new ol.source.TileImage({
      projection,
      tileGrid: new ol.tilegrid.TileGrid({ origin: [0, 0], extent, resolutions, tileSize: m.tile }),
      // OpenLayers counts tile rows southwards from the origin, BTMS northwards
      tileUrlFunction: ([z, x, y]) => `${base}${z}/${x}/${-y - 1}.${m.extension}`,
    });
    // [x1, y1, x2, y2] in metres, once the render is complete: the layer asks for nothing outside
    const area = m.bounds;

    // a marker at GTA coordinates
    const markers = new ol.layer.Vector({
      source: new ol.source.Vector({
        features: [new ol.Feature(new ol.geom.Point([195.2, -933.8]))],
      }),
      style: new ol.style.Style({
        image: new ol.style.Circle({
          radius: 7,
          fill: new ol.style.Fill({ color: '#e5484d' }),
          stroke: new ol.style.Stroke({ color: '#ffffff', width: 2 }),
        }),
      }),
    });

    // two more zooms than the tiles have: the zoom 9 tiles are enlarged
    const viewResolutions = Array.from({ length: m.zmax + 3 }, (_, z) => mpp * 2 ** (m.zmax - z));
    const map = new ol.Map({
      target: 'map',
      layers: [new ol.layer.Tile({ source, extent: area || undefined }), markers],
      view: new ol.View({ projection, resolutions: viewResolutions, center: [0, 2000], zoom: 2 }),
    });
    if (area) map.getView().fit(area);

    // the GTA coordinates under the pointer
    map.on('pointermove', (e) => {
      const [x, y] = e.coordinate;
      document.title = `x ${x.toFixed(1)}  y ${y.toFixed(1)}`;
    });
  });
</script>
```

### Other libraries

A library that lets you give a tile URL function and a flat (non-geographic) coordinate system can
use the formulas of [From GTA coordinates to tiles, and back](#from-gta-coordinates-to-tiles-and-back)
directly.

A library that only knows the standard XYZ scheme (one tile at zoom 0, rows counted from the top)
can still load the tiles if you treat the square `[−16,384, 16,384)` m as its whole world. Its
zoom `k` is BTMS zoom `z = k − 1`, and its tiles map to BTMS tiles one to one:

```text
z  = k − 1                        (k ≥ 1; XYZ zoom 0 has no BTMS tile)
tx = X − 2^z
ty = 2^z − 1 − Y
```

For the tile of the worked example, XYZ `10/518/541` is BTMS `9/6/-30`. Positions then map
linearly from the world square: `x = −16,384 + 32,768 · u` and `y = 16,384 − 32,768 · v`, with
`(u, v)` from 0 to 1 across the world, `v` from the top. A library that only works in web-mercator
latitude and longitude does not place game coordinates linearly; use one with a flat coordinate
system.
