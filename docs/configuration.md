# Configuration: `map.toml`

`map.toml` decides **what the tiles show**: which of the game's placement files are drawn, which
map resources of your FiveM server are added, how the tiles are lit, and whether water is drawn.

How BTMS renders and writes the tiles (threads, GPU or CPU, memory, WebP or JPEG) is not in this
file: those are command-line options, described in [cli.md](cli.md).

The repository ships a [`map.toml`](../map.toml) that holds every key at its default value, each
with a short comment. Copy it to the folder you run BTMS from and change what you need.

## Contents

- [Where BTMS reads it](#where-btms-reads-it)
- [All keys at a glance](#all-keys-at-a-glance)
- [`[maps]`](#maps): [`keep`](#keep), [`exclude_folders`](#exclude_folders), [`fill_holes`](#fill_holes)
- [Maps loaded on demand: what you can keep or remove](#maps-loaded-on-demand-what-you-can-keep-or-remove)
- [`[resources]`](#resources)
- [`[lighting]`](#lighting)
- [`[water]`](#water)
- [Changing the configuration after a render](#changing-the-configuration-after-a-render)
- [A complete example for a FiveM server](#a-complete-example-for-a-fivem-server)

## Where BTMS reads it

| How BTMS runs | Configuration used |
|---|---|
| `btms <command>` | `map.toml` in the **current folder** (the folder the command runs in, not the folder of `btms.exe`). Without that file: the defaults, and no error. |
| `btms <command> --config <file>` | That file. A missing file is an error. |
| The interface | The file chosen in its settings (by default `map.toml` in the folder BTMS was started from), plus what you changed in the page. See [The interface](#the-interface). |

The Windows installer copies `map.toml` once into its working folder, `Documents\BTMS` (it never
overwrites your copy), and its shortcut starts BTMS there. The portable zip has `map.toml` next to
`btms.exe` and `btms-gui.exe`.

Rules for every configuration file:

- **Every key has a default.** A file may hold only the keys you change. A missing section takes
  the defaults of all its keys, and an empty file is the defaults.
- **Unknown names are errors.** An unknown section or key (`azimut` instead of `azimuth`) is
  refused, and so is a value of the wrong type. The command stops before it reads the game, so a
  typo never silently gives you the defaults.
- **Numbers** may be integers (`azimuth = 90`) or decimals (`azimuth = 90.0`).
- **Windows paths:** write them in single quotes. These are TOML literal strings, in which `\` is a
  plain character: `'C:\FXServer\resources\[maps]'`. In double quotes every backslash must be
  doubled (`"C:\\FXServer\\resources\\[maps]"`). Forward slashes work too.
- **Relative paths** (in `[resources] folders`) are taken from the current folder, not from the
  folder of the configuration file. Absolute paths avoid surprises.
- The file is read **once**, when a command starts (every command but `interface`, `help` and
  `--version`). The `[lighting]` keys can also be overridden for one run (see
  [Command-line overrides](#command-line-overrides)).

To check a file without rendering, run `btms tiles --dry-run`. An error in the configuration stops
it. Otherwise it prints the render settings line (threads, renderer, shading or lighting off,
memory, tile format, zone) and stops before reading the game. It does not print the other values of
the file.

### The interface

The interface **never writes `map.toml`**, because its comments would be lost. What you change in
the page (kept maps, excluded folders, hole filling, resource folders, lighting, water) is stored as
overrides in its own settings file, `%LOCALAPPDATA%\BTMS\interface.json`. Every job it launches gets
`--config` with a derived file, `%LOCALAPPDATA%\BTMS\interface\effective-map.toml`: `map.toml` as
it is read, with your overrides applied. That file is written again for every job, so do not edit
it. Edit `map.toml`, or change the setting in the page.

A setting that differs from `map.toml` is marked in the page, which can also drop the override and
go back to the file's value. If `map.toml` cannot be read (a typo, an unknown key), the page shows
the error and starts no render: a render with the defaults in place of your settings would be worse
than none. Checking the game and importing the keys still work. See [interface.md](interface.md).

## All keys at a glance

| Section | Key | Type | Default | Values |
|---|---|---|---|---|
| `[maps]` | [`keep`](#keep) | list of strings | `["hei_carrier", "hei_yacht_heist", "HeistIsland"]` | file names, `*` patterns, island names |
| `[maps]` | [`exclude_folders`](#exclude_folders) | list of strings | `["/levels/gta5/_prologue/"]` | parts of paths inside the game's archives |
| `[maps]` | [`fill_holes`](#fill_holes) | boolean | `true` | `true`, `false` |
| `[resources]` | [`folders`](#folders) | list of strings | `[]` | folders on disk |
| `[lighting]` | [`enabled`](#lighting) | boolean | `true` | `true`, `false` |
| `[lighting]` | [`azimuth`](#lighting) | number, degrees | `315.0` | any angle: 0 north, 90 east, 180 south, 270 west |
| `[lighting]` | [`elevation`](#lighting) | number, degrees | `35.0` | 5 to 90 |
| `[lighting]` | [`strength`](#lighting) | number | `0.6` | 0 to 1 |
| `[lighting]` | [`shadows`](#lighting) | boolean | `true` | `true`, `false` |
| `[lighting]` | [`ambient_occlusion`](#lighting) | number | `0.3` | 0 to 1 |
| `[water]` | [`enabled`](#water) | boolean | `true` | `true`, `false` |

A number outside its range is not an error: it is taken back to the nearest bound when the tiles
are shaded.

## `[maps]`

Which of the game's placement files (`.ymap`) are drawn.

### `keep`

- **Type:** list of strings
- **Default:** `["hei_carrier", "hei_yacht_heist", "HeistIsland"]`

The maps that the game loads only on request and that BTMS should still draw. Everything about them
is in [Maps loaded on demand](#maps-loaded-on-demand-what-you-can-keep-or-remove), just below. In
short:

- an entry is a placement file's name (`"hei_carrier"`), a pattern with `*` (`"h4_*"`), or an
  island of the game (`"HeistIsland"`);
- `keep = []` keeps none of them beyond what the fixed rules keep;
- `keep = ["*"]` keeps all of them;
- a kept map is drawn like any other placement file, with its shadows. A map left out is not drawn
  and casts no shadow.

### `exclude_folders`

- **Type:** list of strings
- **Default:** `["/levels/gta5/_prologue/"]`

Every placement file of the game has a path inside the game folder and its archives: lowercase,
with `/` between the parts, and archives appearing like folders (for example
`update/update.rpf/…`). A placement file whose path **contains** one of these texts is never read
and never drawn, whatever `keep` says.

**What it is for: North Yankton.** The prologue's level is in the game's archives, but only the
prologue's scripts load it, so it is never on the map of a free-mode or FiveM session. Without this
entry, a snowy area appears south-east of Los Santos.

**To leave out other folders**, add a part of their path:

```toml
[maps]
exclude_folders = [
  "/levels/gta5/_prologue/",   # North Yankton
  "/dlcpacks/<pack>/",          # every placement file of one DLC pack
]
```

The pack names are those that `btms groups` prints in its table per pack. `btms ls` and `btms find`
show the paths inside the archives (see [cli.md](cli.md)).

Notes:

- It applies to **the game's placement files only**, not to archetype files, models, textures or
  the files of your resources. To leave out a resource, take its folder out of
  [`[resources] folders`](#folders).
- The match is plain, case-sensitive text on a lowercase path: write the entries in lowercase, with
  `/`. A short text can match more than you meant (`"_prologue"` matches any path that contains it).
- BTMS reads only the copy of a placement file that the game uses (the one in the pack loaded
  last). If that copy is in an excluded folder, the file is left out: an older copy in another
  folder is **not** drawn in its place.
- `exclude_folders = []` draws every folder, North Yankton included.

### `fill_holes`

- **Type:** boolean
- **Default:** `true`

Game updates moved the ground of a few places (mansion sites, a road bend, a building of Paleto
Bay) into on-demand maps, so that a script can swap one state of the place for another. When no
script asks for either state, both are left out like every other on-demand map, and the tiles show
a hole there. `fill_holes = true` deals with that in three steps:

1. **Search.** After the other rules, every 128 m block that a left-out map touches is drawn with
   what is kept, heights only, at 0.25 m per pixel. Its empty regions are joined across the edges
   of the blocks. A region closed all around by what is drawn is a hole. A region that reaches a
   block where nothing is kept at all (the open sea beyond the seabed) is not, so a yacht moored at
   sea stays out.
2. **Choice.** A hole of 4 m² or more is offered to the left-out maps that have entities in its
   blocks. One map that covers 90% of the hole closes it alone, in this order of preference: the
   original state of the place (names containing `original` or `_orig`), then a normal state
   (`_norm`, `normal`, `default`, `unburnt`, `intact`, `closed`, `tidy`, `shallow`), then any other.
   A wrecked state (`burnt`, `burned`, `_dest`, `destroy`, `damage`, `wreck`, `sunk`, `crash`,
   `ruin`, `trash`, `_after`) is taken only when nothing else closes the hole. When no single map
   closes it, several close it together, the widest first. Up to four rounds run, because closing
   a hole can leave a smaller one beside it. The maps chosen are kept, with their LOD parents.
3. **Last holes.** Once the tiles are written, the holes that no map closes (an opening onto an
   interior that is not drawn, a basin whose water is not drawn) are coloured in from the pixels
   around them, from the rim inwards, up to 2,000 m². A larger hole is left as it is and reported.

With `fill_holes = false`, none of the three steps runs: only what `keep` and the fixed rules keep
is drawn, and these holes stay empty (transparent).

**How it works with `keep`:**

- `keep` chooses the state, and `fill_holes` only fills what is still empty. If a map you list
  already closes a place's ground, no hole is left there, and the state you listed is drawn.
- Taking an entry out of `keep` does not guarantee that the map disappears: if it closes a hole,
  `fill_holes` keeps it again. `btms groups` says why each map is kept.
- With `keep = ["*"]`, nothing is left out, so there is nothing to search.

Also:

- The search draws blocks before the render starts. On the whole map it took about a minute in
  measurements on an 8-core machine. `btms groups` runs the same search before its report;
  `btms probe` does not run it.
- `btms update` takes back the maps that the full render kept for holes (recorded in
  `resources.json`) when the settings have not changed since, and searches only around what it
  redraws.
- `btms mend [folder]` runs step 3 alone on tiles that are already drawn, whatever this setting.
- While a tile is written, the small seams between two pieces of geometry are always filled from
  the pixels next to them. That has nothing to do with this setting.

## Maps loaded on demand: what you can keep or remove

The game loads some maps only when a script asks for them: yachts, the UFO, the states of a building
that a mission changes (a cargo ship afloat or sunk, a red carpet in Vinewood), multiplayer
interiors. FiveM scripters know them as IPLs: `RequestIpl('hei_carrier')` loads the aircraft
carrier. Drawing all of them would show things a player never sees, sometimes several states of
the same place on top of each other. BTMS leaves them out, except the ones you keep.

### How BTMS tells that a map is on demand

The game marks them itself: **bit 0 of the `flags` field** of the `CMapData` structure that starts
every placement file says that the file is loaded only on request. BTMS reads that bit in every
placement file of the game. On one recent build of the game, 1,296 of the 11,015 placement files
have it.

`btms groups` checks the rule against the rest of your game files: every map group of the
`_manifest.ymf` files (`MapDataGroups`) bears the name of a placement file, and every such file
that is drawn has the bit; no file hangs (as a LOD child) from a file whose bit differs from its
own; and every map an island switches on (`IPLsToEnable` in its `islandhopper.meta`) has the bit.

The map groups alone are not the list. A group exists only when a map comes with collision files or
follows the hour or the weather: on the same build, 750 of the 1,296 on-demand files have none.

### What is drawn of them

An on-demand placement file of the game is drawn when one of these holds. `btms groups` shows the
first one that applies:

1. an entry of `keep` matches its name, or names an island whose list holds it (the first such
   entry of the list is the one shown);
2. a client script of one of your resources loads it by name: `RequestIpl('name')`, with the name
   written out (see [Which files are used](#which-files-are-used));
3. its map group follows the hour or the weather, and is shown at **12:00 in `EXTRASUNNY`
   weather**. The game switches those maps by itself, with no script: the day props of Vespucci
   Beach are drawn, the evening and night ones are not. The hour and the weather are fixed, not
   settings;
4. a kept file hangs from it: the game loads a file's LOD parents with it, so BTMS draws them too;
5. with [`fill_holes`](#fill_holes), it closes a hole in the ground.

Every other on-demand file is left out.

Three more rules apply to every placement file, on demand or not:

- a placement file that a client script of your resources removes by name (`RemoveIpl('name')`) is
  not drawn, **even if `keep` names it**;
- a placement file that a multiplayer pack replaces with its own copy (named with a prefix such as
  `hei_` or `apa_`) is not drawn, and the pack's copy is drawn instead;
- the placement files that your resources stream are **never** left out by the on-demand rule,
  whatever their bit. This is BTMS's own choice: what FiveM itself does with a streamed file that
  has the bit has not been verified.

### How an entry of `keep` matches

- It is compared with the name of the file, without folder or extension (`hei_carrier` for
  `…/hei_carrier.ymap`). **Case does not matter.**
- An entry without `*` matches that one name, whole: `hei_carrier` does not match
  `hei_carrier_lod`. (The LOD parents of a kept file come with it anyway.)
- `*` stands for any run of characters, the empty one included, anywhere in the entry: `h4_*`,
  `*_original`, `*`. A pattern can match more than you expect: `*_original` keeps the original
  state of every place that has one.
- The same entry is also compared, by the same rules, with the names of the game's **islands**,
  read from its `islandhopper.meta` files (`CIslandHopperData`, field `Name`). When it matches an
  island, every map in that island's `IPLsToEnable` list is kept, which is what `SET_ISLAND_ENABLED`
  loads in the game. `HeistIsland` is Cayo Perico. `*` matches every island as well.
- `btms groups` prints how many files each entry keeps, and flags an entry that keeps none.

### Catalog of useful entries

These names come from the game's own files. Counts are those of one recent build, and yours may
differ. `btms groups` gives the list for your game files.

| Entry | What it adds | Where | Notes |
|---|---|---|---|
| `hei_carrier` | The aircraft carrier | At sea | In the default list |
| `hei_yacht_heist` | The heist yacht | Off the funfair of Del Perro Pier | In the default list. Its story-mode copy stays out. |
| `HeistIsland` | Cayo Perico: the 94 maps the game enables for the island | The island | In the default list. This is an island name, not a file name. |
| `h4_*` | Every `h4_` file: the island, plus 35 files the island does not load, among them three yachts along the San Andreas coast and the variants of the missions | Cayo Perico and the coast | Use it instead of `HeistIsland` only if you want those extras too. |
| `*_mansion_original`, `*_props_original` | Three mansion sites in their original state, before a game update built mansions there | Three sites | The update (pack `mp2025_02`) stores both states as on-demand maps. `fill_holes` already keeps what closes a hole, the original state first. |
| `hei_ch1_roads_original` | A road bend in its original state | One bend | Same update, same note. |
| `bnkheist_apt_norm` | The apartment building with a pool next to the bank, in its normal state | Paleto Bay, around x -158, y 6,426 | `bnkheist_apt_dest` is the same building destroyed, after the story-mode heist. Without either, the tiles have a hole the size of the lot. `fill_holes` takes the normal state. |
| `cargoship` | The cargo ship moored at the quay | The port | |
| `apa_yacht_grp04_1` | One of the player yachts of the multiplayer packs | At sea | A yacht can have more than one file (its interior, its LOD files). LOD parents come with it, and `btms groups` lists the other files. |
| `*` | Every on-demand map: every yacht, the UFO, and every state of every place, all at once | Everywhere | Good for a check, not for a map |

With the default list, these are left out, among others: the UFO above Fort Zancudo, the red carpet
in Vinewood, the sunk cargo ship, slabs laid on the roofs of the
Arcadius tower, the player yachts, and the evening and night props of Vespucci Beach. Their names
are in the output of `btms groups`.

### Finding names: `btms groups`

`btms groups` reads your game files and your resources with your configuration, and prints, without
drawing anything:

- the sources it read: manifests, map groups (and how many follow the hour or the weather), islands
  and their maps, placement files, how many are on demand and how many your resources stream;
- the checks of the rule above;
- your `keep` list, with the number of files each entry keeps, flagging any entry that keeps none;
- totals of what is kept (by reason) and left out, in files and entities, then a table per pack;
- every on-demand file, by pack: its entity count, what happens to it (left out, or kept and why),
  whether it is a LOD parent, whether a pack replaces it, its map group (collision files, hours,
  weathers), and its parent.

The output is long, so save it to a file: `btms groups > groups.txt`. With `fill_holes = true`, it
runs the hole search first.

A typical round: find a name in that list, add it to `keep`, and render only the area around it to
look at it, for example `btms tiles output/preview --zone x1,y1,x2,y2` (coordinates in metres, see
[cli.md](cli.md)).

### Recipes

```toml
[maps]
keep = ["hei_yacht_heist", "HeistIsland"]   # no aircraft carrier
```

```toml
[maps]
keep = ["hei_carrier", "hei_yacht_heist"]   # no Cayo Perico
```

```toml
[maps]
keep = []        # only what the fixed rules keep (scripts, noon maps, LOD parents, holes)
```

```toml
[maps]
keep = ["*"]     # every on-demand map, for a check
```

## `[resources]`

The map resources of your FiveM server: MLOs, roads, new areas, fixes. A file that a resource
streams **wins over the game's file of the same name**, and a name the game does not have is added
to the map.

### `folders`

- **Type:** list of strings
- **Default:** `[]`, the game alone

Each entry is a folder on disk:

- **a resource folder**, which holds `fxmanifest.lua` or `__resource.lua`: that resource;
- **any other folder**, which BTMS searches for resources: its subfolders in the order of their
  names (case ignored), down to 6 levels. A folder that holds a manifest is a resource, and the
  search does not go inside it. So a folder of resources such as `[maps]` works, and so does a
  whole `resources` folder.

```toml
[resources]
folders = [
  'C:\FXServer\resources\[maps]',              # every resource in [maps]
  'C:\FXServer\resources\[local]\my-island',   # one resource
]
```

If an entry does not exist, or holds no resource, BTMS reports it when the map loads and goes on
without it.

**Every resource that BTMS finds is used**, whether your server starts it or not: BTMS does not
read `server.cfg`. List the folders of the resources your server actually runs, or list the
resources one by one.

### Which files are used

BTMS only reads files: nothing in a resource is ever run.

- **Everything under the resource's `stream/` folder**, subfolders included. FiveM knows a streamed
  file by its name alone, and so does BTMS. Six kinds of files are drawn: placement files `.ymap`,
  archetype files `.ytyp`, models `.ydr`, `.ydd` and `.yft`, and texture dictionaries `.ytd`. They
  must be RSC7 resources, the same bytes a game archive holds: a file of one of these kinds that is
  not is reported and left out. An `.rpf` archive inside `stream/` is reported and not opened.
  Other kinds of files (collisions, for example) do not change the tiles.
- **The manifest** (`fxmanifest.lua` or `__resource.lua`). BTMS reads the texture parent files
  declared with `data_file 'GTXD_PARENTING_DATA' '<file>'` (a declared file that is missing is
  reported), and `this_is_a_map`, which `btms resources` only displays. The streamed files do not
  need to be declared: every file under `stream/` counts.
- **The client and shared scripts** declared with `client_script`, `client_scripts`,
  `shared_script` or `shared_scripts`. Patterns with `*` and `**` are followed, and
  `@other_resource/…` entries are skipped. These Lua or JavaScript files are read as text
  (comments ignored), only to find calls that name a map with a literal string:
  - `RemoveIpl('name')`: that placement file is not drawn;
  - `RequestIpl('name')`: that on-demand map is kept;
  - the same two calls written as `Citizen.InvokeNative(0xEE6C5AD3ECE0A82D, 'name')` (remove) and
    `Citizen.InvokeNative(0x41B4893843BBDB74, 'name')` (request).

  A name the script computes (`RemoveIpl(name)`, `RemoveIpl('a' .. x)`) cannot be known: such calls
  are counted, not applied. So are `CreateModelHide`, `CreateModelHideExcludingScriptObjects`,
  `CreateModelSwap` and `SetBuildingState`.

### Which copy wins

- A streamed file wins over the game's file of the same name and kind (placement file, archetype
  file, model, texture dictionary). An archetype that a resource defines replaces the game's
  archetype of the same name.
- When two streamed files have the same name, **the later one wins**. The order is: the entries in
  the order of `folders`, then the resources in the order of their folder names, then the files of
  a resource in the order of their paths (a folder's files before its subfolders). This is BTMS's
  own rule: FiveM's order between two resources that stream the same name has not been verified.
  `btms resources` lists the names streamed more than once, so that you can check them. To compare,
  `BTMS_RESOURCE_ORDER=first-wins` makes the first one win instead.

### What resources remove and add

- A placement file with the same name as one of the game's replaces it.
- In the placement files of a resource, two kinds of entities are not drawn: entities moved out of
  the world, below -2,000 m (the usual way to remove an entity without deleting it), and entities
  whose archetype only casts a shadow (archetype flag `0x800`; for an archetype of the game, this is
  checked only in a file that replaces one of the game's).
- A placement file that a client script removes (`RemoveIpl` with a literal name) is not drawn.
- **Interiors.** The entities of a resource's interiors (a `CMloInstanceDef` whose archetype the
  resource defines) are drawn as plain entities. They show where the roof is open or glazed:
  resources use interiors for whole buildings. The entity sets that scripts switch on are left out,
  since which ones are on cannot be known. The game's own interiors are not drawn, because they are
  under a roof.
- Placement files that your resources stream are drawn whatever their on-demand bit (see
  [What is drawn of them](#what-is-drawn-of-them)).

### Protected resources (escrow, FXAP, scrambled models)

**BTMS never decrypts, unscrambles or works around a protection.** Whatever is protected is reported
and left out:

| What | How BTMS tells | What happens |
|---|---|---|
| A file encrypted by FiveM's asset escrow | It starts with the four bytes `FXAP` | Left out and reported. For a streamed file, the game's file of the same name, if there is one, is used instead. |
| A client script encrypted the same way | It starts with `FXAP` | Left out and reported: what it removes or requests is unknown. |
| A client script that is neither text nor `FXAP` | It is not valid UTF-8 | Left out and reported: what it removes or requests is unknown. |
| A model (`.ydr`, `.ydd`, `.yft`) with scrambled vertices | The file reads as a plain RSC7 resource, but at least one vertex cannot be a position: it is not a number, it lies beyond 100 km, or it lies more than 1 km outside the model's own box | Not drawn. The game's model of the same name is drawn instead, when there is one. Counted at the end of the render. |
| The interiors of a resource whose models are protected | One of the first three models the resource streams for one of its interiors is scrambled | None of that resource's interiors is placed: their furniture would stand alone on the ground. |

In practice, the models scrambled this way are those of resources under escrow. The models that a
manifest takes out of escrow (`escrow_ignore`) read normally. A model whose box is only out of date
(vertices a few metres outside it) is kept. Textures, placement files and archetype files read
normally, unless they are `FXAP` files.

**What to expect on the tiles:** if the terrain and buildings of a map resource are protected, that
map comes out as its unprotected props, standing over the game's own ground or seabed, or over
nothing. With lighting on, those props also cast shadows on the seabed far below them. If that
misleads, take the resource out of `folders`, or ask its author for unprotected map files.

**Where it is reported:**

- **When the map loads**, in a few lines: the resources and their streamed files; placement files,
  with how many replace one of the game's and how many of those are emptied; archetypes; texture
  parents; interiors; entities not drawn; files left out; and which `RemoveIpl` and `RequestIpl`
  calls were applied.
- **At the end of a render:** how many models were protected, and for how many of them the game's
  model of the same name was drawn instead.
- **`btms resources`** prints the full inventory: resources, files that replace the game's, names
  streamed twice, replaced placement files, removed entities, interiors, scripts, and every file left
  out with the reason. `btms resources --all` also reads every model and texture dictionary and
  counts the protected and unreadable ones per resource; `--detail` names every file that fails.
- **The resources record**, `resources.json`, is written next to the tiles. It marks a resource whose
  map files are protected or unreadable, with the reason. It names resources by their folder only
  and never holds a full disk path, so the tile folder can be published as it is.

## `[lighting]`

The tiles are shaded so that relief and buildings can be read. Seen straight from above, every
pixel's height is known, and the colour of each pixel is multiplied by a factor made of:

- **the slope**: Lambert's law relative to flat ground, which keeps its exact colour. A step steeper
  than 3 to 1 between two pixels is a wall, not a slope. Where a pixel's slope is close to the mean
  slope around it, the mean is used, so the flat triangles of the terrain do not show while the
  ridge of a roof stays sharp;
- **cast shadows**: a pixel is in shadow when something on the line toward the sun rises above the
  sun's ray, within 48 m. The shadow fades out over the second half of that reach;
- **the foot of walls** (`ambient_occlusion`): a light darkening where something rises steeper than
  45° within 3 m;
- **foliage** is shaded as a volume, not leaf by leaf, so that a tree's crown does not shade itself
  into black.

The factor never goes below 0.2: nothing turns black. The borders between blocks are seamless,
because a pixel's factor depends only on the heights within a fixed distance of it, on grids fixed
to the world. Shading sees the ground, not the water.

| Key | Type | Default | Range | Effect |
|---|---|---|---|---|
| `enabled` | boolean | `true` | | `false`: no shading at all, only the textures' own colours. The render is faster too: lighting made a block about 40 to 50% slower in measurements. |
| `azimuth` | degrees | `315.0` | any | Where the sun is, clockwise from north: 0 north, 90 east, 180 south, 270 west. Any angle works (360 is 0, -45 is 315). Shadows fall on the opposite side. 315 (north-west) is the usual convention of maps: relief lit from the south looks inverted. |
| `elevation` | degrees | `35.0` | 5 to 90 | Height of the sun above the horizon. Lower: longer shadows and stronger slopes. At 89 and above, nothing casts a shadow. The 48 m reach cuts short the shadow of anything taller than 48 × tan(elevation): 34 m at 35°, 48 m at 45°. |
| `strength` | number | `0.6` | 0 to 1 | Weight of the slope shading and of the cast shadows. Flat ground keeps its colour. A full shadow keeps 1 − 0.75 × strength of it (55% at 0.6), and a slope facing the sun gets up to 1 + 0.75 × strength. At 0, there is no slope shading and no shadow (`ambient_occlusion` still applies). |
| `shadows` | boolean | `true` | | Cast shadows. `false`: slopes only. |
| `ambient_occlusion` | number | `0.3` | 0 to 1 | Darkening at the foot of walls, only below what stands around (the edge of a roof is not the foot of its own wall). 0: none. |

With `strength = 0` and `ambient_occlusion = 0`, the colours are exactly those of an unlit render,
but the shading still takes its time. To turn the light off, use `enabled = false`.

### Presets

| Preset | `enabled` | `azimuth` | `elevation` | `strength` | `shadows` | `ambient_occlusion` | Result |
|---|---|---|---|---|---|---|---|
| Flat map | `false` | | | | | | Texture colours only, the fastest render |
| Soft relief | `true` | `315.0` | `45.0` | `0.5` | `true` | `0.3` | Relief still reads, and streets in shadow stay readable |
| Default | `true` | `315.0` | `35.0` | `0.6` | `true` | `0.3` | Very marked relief |

On a downtown block, the default darkens 32% of the pixels by more than 45% of their brightness;
soft relief darkens 8% of them that much.

```toml
[lighting]   # soft relief
elevation = 45.0
strength = 0.5
```

### Command-line overrides

Each key can be set for one run, after the configuration file is read:

| Flag | Sets | Values |
|---|---|---|
| `--lighting on\|off` | `enabled` | `on`, `off` (also `true`, `false`, `1`, `0`) |
| `--no-lighting` | `enabled = false` | |
| `--azimuth <deg>` | `azimuth` | a number |
| `--elevation <deg>` | `elevation` | a number |
| `--strength <0-1>` | `strength` | a number |
| `--shadows on\|off` | `shadows` | `on`, `off` (also `true`, `false`, `1`, `0`) |
| `--ambient-occlusion <0-1>` | `ambient_occlusion` | a number |

```
btms tiles output/preview --zone 0,-1536,1024,-512 --elevation 45 --strength 0.5
```

A flag and its value are two arguments: `--azimuth 200` works, and `--azimuth=200` is refused. The
overrides apply to every command that reads the configuration. `tiles.json` records the lighting a
render used. The interface sets the same keys from its page (see [interface.md](interface.md)).

## `[water]`

- **Type:** boolean (`enabled`)
- **Default:** `true`

The game has two kinds of water, and neither is a model in a placement file like the rest.

With `enabled = true`:

- **The sea and the lakes** are flat pieces listed in the game's `water.xml`, plus Cayo Perico's own
  file, each at its height: 0 for the sea, 30 m for the Alamo Sea, 160 m and 196 m for the two
  reservoirs. Wherever the ground lies below such a piece, the tile shows water over it, more
  opaque and darker where it is deeper, so the shores fade in. Where there is no ground at all under
  a piece (beyond the seabed), it is deep water, as long as that empty region touches water or the
  edge of the block. A pocket ringed by dry ground stays empty: a hole in the map is not a pond.
  Beyond the rectangles that the water files cover, it is the open sea at height 0, as in the game.
- **Rivers, pools and fountains** are parts of models with a water shader. Their surfaces are drawn
  as blended water, and their foam and decals are left out.
- **The sea fills the whole rectangle around the map**, plus one block on each side, so there are
  more tiles than without water.

With `enabled = false`, there is no water at all. The tiles show the ground under it: the seabed and
the beds of lakes, rivers and pools. Beyond the seabed nothing is drawn (transparent), and only the
blocks that hold something get tiles.

Water never changes the shading or the hole search: both see the ground.

```toml
[water]
enabled = false   # the seabed instead of the sea
```

## Changing the configuration after a render

`[maps]`, `[lighting]` and `[water]` change the whole map. A full render (`btms tiles` without
`--zone`) records them next to the tiles, in `resources.json`. If they differ when you run
`btms update`:

- `btms update --changes` stops with an error when a resource changed, and draws nothing: the
  blocks it would redraw would not match the ones around them. Render the whole map again with
  `btms tiles`, or record the current settings without drawing anything with
  `btms update --record`. When no resource changed, it has nothing to draw and only prints a remark.
- `btms update --zone x1,y1,x2,y2` warns you, then draws the zone with the current settings.

Lighting overrides given on the command line count as settings for this comparison.

`[resources] folders` works differently. The record holds a fingerprint of each resource: its
manifest, everything under `stream/`, and its `.lua`, `.js`, `.meta`, `.xml` and `.ide` files, by
path, size and date. `btms update --changes` redraws only the ground of the resources added,
changed or removed since that record. Adding a folder to the list adds its resources. See
[cli.md](cli.md).

## A complete example for a FiveM server

A server whose map resources live in two places:

```
C:\FXServer\resources\
├─ [maps]\
│  ├─ my-island\        fxmanifest.lua, stream\…
│  └─ my-garage\        fxmanifest.lua, stream\…
└─ [local]\
   └─ map-fixes\        fxmanifest.lua, client.lua (RemoveIpl / RequestIpl), stream\…
```

```toml
# map.toml: the game, the server's map resources, and a softer light.

[maps]
# The aircraft carrier, the heist yacht and Cayo Perico. A map that a client script of the
# resources below loads with RequestIpl('name') is kept without being listed here, and one it
# removes with RemoveIpl('name') is never drawn.
keep = ["hei_carrier", "hei_yacht_heist", "HeistIsland"]
# North Yankton stays out.
exclude_folders = ["/levels/gta5/_prologue/"]
# Close the holes left by the game's on-demand ground.
fill_holes = true

[resources]
# Every resource in [maps], then one resource on its own. When two of them stream a file of
# the same name, the later one wins, so map-fixes comes last on purpose.
folders = [
  'C:\FXServer\resources\[maps]',
  'C:\FXServer\resources\[local]\map-fixes',
]

[lighting]
# Soft relief: streets in shadow stay readable.
# enabled, azimuth, shadows and ambient_occlusion keep their defaults (true, 315, true, 0.3).
elevation = 45.0
strength = 0.5

[water]
enabled = true
```

Then:

1. `btms tiles --dry-run`: the file reads (an error in it would stop here), and the render
   settings are printed.
2. `btms resources`: what each resource brings, what replaces the game's files, and what was left
   out as protected or unreadable.
3. `btms groups > groups.txt`: which on-demand maps are kept, and why.
4. `btms tiles output/preview --zone x1,y1,x2,y2`: a preview of one area, such as the island.
5. `btms tiles`: the whole map, into `output/tiles`.
6. After you change a resource: `btms update --changes` redraws only what it touches.

## See also

- [cli.md](cli.md): every command and option
- [interface.md](interface.md): the local interface, and how it handles `map.toml`
- [tile-format.md](tile-format.md): the tiles, `tiles.json` and `resources.json`
- [troubleshooting.md](troubleshooting.md): common problems
