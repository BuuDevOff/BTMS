# Command line

Everything the interface does can also be done from a terminal. This page lists every command,
every option with its values and default, the environment variables, and the exit codes. The
interface builds these same command lines for its jobs.

```text
btms <command> [arguments] [options]
```

- With the installer's PATH option, `btms` works from any terminal. Otherwise run it from the
  install folder as `.\btms.exe`, or from the folder of the portable zip.
- `btms.exe` is the command-line program. The Start menu and desktop shortcuts start
  `btms-gui.exe`, the same program built without a console window, which opens the interface.
- Commands read `map.toml` from the **current folder**. The installer creates
  `Documents\BTMS` with a copy of `map.toml`: open your terminal there (`cd ~\Documents\BTMS`) so
  that the commands find it. See [configuration.md](configuration.md).

## Contents

- [How options are read](#how-options-are-read)
- [What each command needs](#what-each-command-needs)
- [Global options](#global-options)
- [Lighting overrides](#lighting-overrides)
- Commands: [`interface`](#interface), [`verify`](#verify), [`import-keys`](#import-keys),
  [`ls`](#ls), [`extract`](#extract), [`find`](#find), [`tiles`](#tiles), [`update`](#update),
  [`convert`](#convert), [`groups`](#groups), [`mend`](#mend), [`probe`](#probe),
  [`resources`](#resources), [`help`](#help), [`--version`](#--version)
- [Environment variables](#environment-variables)
- [Exit codes](#exit-codes)
- [Examples](#examples)

## How options are read

- **An option and its value are two arguments:** `--zone 0,-1536,1024,-512`, `--threads 8`. The
  form `--zone=…` is refused by every command, because the option would otherwise be ignored and
  `tiles` would draw the whole map.
- **A value may not start with `--`** for the options of `tiles`, `update` and `convert`, for
  `--chain` and for `--image`: `--threads --zone 1,2,3,4` is refused as an option without a
  value.
- **Order does not matter.** The folder of `tiles` or `update` can come before, after or between
  the options.
- **The last one counts.** When an option of `tiles`, `update` or `convert`, or `--renderer`,
  `--shading` or `--chain`, is given twice, the last value is used.
- **Unknown options are refused** by `tiles`, `update`, `convert`, `probe` and `interface`, with
  the list of the options the command takes. This way a typo is never taken for a folder name.
  `resources` ignores the options it does not know.
- **Values are words of their own:** `on`/`off` (also `true`/`false`/`1`/`0`), `auto`/`gpu`/`cpu`,
  `webp`/`jpeg` (also `jpg`), `fast`/`careful`, `lossless`. Sizes are in GB, with a point or a
  comma for decimals (`12.5` or `12,5`).

## What each command needs

Every command except `interface`, `help` and `--version` first reads the configuration
(`map.toml` in the current folder, or `--config <file>`), and stops if it cannot be read.
Then most commands look for the GTA V folder: given with `--game`, or else the one FiveM uses
(`IVPath` in `%LOCALAPPDATA%\FiveM\FiveM.app\CitizenFX.ini`). The folder must hold `GTA5.exe`.

| Command | GTA V folder | Keys | Writes |
|---|---|---|---|
| `interface` | from its settings | from its settings | its settings, `%LOCALAPPDATA%\BTMS\interface.json` |
| `help`, `--version` | no | no | nothing |
| `verify` | yes | yes | nothing |
| `import-keys` | yes (the AES key is read from `GTA5.exe`) | builds them | the keys folder |
| `ls`, `extract`, `find` | yes | yes | `extract`: the file you name |
| `tiles`, `update` | yes, except with `--dry-run` | yes, except with `--dry-run` | the map folder |
| `convert` | no | no | the target folder |
| `groups`, `resources`, `probe` | yes | yes | `probe --image`: the picture |
| `mend` | it must be found, but is not read | no | the map folder |

The keys live in `%LOCALAPPDATA%\BTMS\keys`. The first command that needs them also saves there
the AES key it finds in `GTA5.exe`. Until the NG keys are imported (see
[`import-keys`](#import-keys)), every command that reads an NG-encrypted archive fails with an
error.

## Global options

These work with every command except `interface`, `help` and `--version`. The interface takes the
game folder, the configuration and the render settings from its own page.

| Option | Values | Default | What it does |
|---|---|---|---|
| `--game <folder>` | a GTA V folder | FiveM's `IVPath` | The game to read. It is only read, never written. |
| `--config <file>` | a TOML file | `map.toml` in the current folder, else the built-in defaults | What the tiles show. A file given here that does not exist is an error. See [configuration.md](configuration.md). |
| `--renderer <r>` | `auto\|gpu\|cpu` | `auto`, or `BTMS_RENDERER` | Who draws the pictures. `auto` takes the best graphics card: a discrete card before an integrated one, DirectX 12 before Vulkan, and never a software adapter. `gpu` also accepts a software adapter when nothing else is found. `cpu` never opens a card. When no card can be used, the processor draws and the log says why: neither `auto` nor `gpu` stops the run for that. |
| `--shading <s>` | `auto\|gpu\|cpu` | `auto`, or `BTMS_SHADING` | Who computes the shading. `auto` uses the card when the drawing does. On a failure the processor takes over. |
| `--chain <c>` | `auto\|gpu\|cpu` | `auto`, or `BTMS_CHAIN` | Whether a block stays on the card from its drawing to its tiles (shading, hole filling, water, zooms 8 and 7). `auto` uses it when the card draws and shades, or draws with the lighting off. `gpu` is refused with `--renderer cpu`, and with `--shading cpu` while the lighting is on. `cpu` never uses it. |
| `--profile` | no value | off, or `BTMS_PROFILE` | At the end of a render, prints a table of where the time went, phase by phase. |

How the two renderers compare, and what happens when the card fails, is described in
[architecture.md](architecture.md#8-the-gpu-renderer-and-the-chain-on-the-card). Only
`--renderer cpu` gives exactly the processor's pictures; the card matches them within 2/255 on
99.9 % of pixels.

## Lighting overrides

These override the `[lighting]` section of `map.toml` for one run. They are global options too.

| Option | Value | `map.toml` key | Default |
|---|---|---|---|
| `--lighting on\|off` | `on` or `off` | `enabled` | `on` |
| `--no-lighting` | no value | `enabled = false` | |
| `--azimuth <deg>` | degrees clockwise from north, where the sun comes from | `azimuth` | `315` (north-west) |
| `--elevation <deg>` | height of the sun above the horizon, in degrees | `elevation` | `35` |
| `--strength <0-1>` | strength of the shading | `strength` | `0.6` |
| `--shadows on\|off` | cast shadows | `shadows` | `on` |
| `--ambient-occlusion <0-1>` | darkening at the foot of walls | `ambient_occlusion` | `0.3` |

- `--no-lighting` wins over `--lighting on`.
- The numbers are taken as given: keep them within the ranges shown.
- With the lighting off, the tiles show the plain texture colours and nothing is shaded.
- `btms update --changes` compares these settings with the ones the map was drawn with, and
  refuses to redraw part of a map with different ones. Give `update` the same `map.toml` and the
  same lighting options as the render. See
  [configuration.md](configuration.md#command-line-overrides).

## Commands

### `interface`

```text
btms
btms interface [--port <n>] [--no-window]
btms interface --render-line [x1,y1,x2,y2]
```

The local web interface: settings, renders and their progress on a live map. It is also what runs
with no command at all. It starts even when the game or `map.toml` cannot be read, and its page
says what is wrong.

| Option | Value | Default | What it does |
|---|---|---|---|
| `--port <n>` | a port number | `8791` | The first port tried on `127.0.0.1`. When it is taken, the next 20 are tried. The address in use is printed. |
| `--no-window` | no value | a window opens | Opens no window: open the printed address in a browser yourself. |
| `--render-line [x1,y1,x2,y2]` | an optional zone | the whole map | Starts no server. Prints, as a JSON array, the arguments of the render the interface would start with its saved settings, writes the derived configuration they name (`%LOCALAPPDATA%\BTMS\interface\effective-map.toml`), and ends. Nothing is rendered. |

- The window is Microsoft Edge in app mode when Edge is installed, otherwise your default
  browser.
- One interface runs per user. Starting it again while one runs, even with its window closed,
  opens the running one instead of a second server.
- With a window, the interface quits by itself five minutes after the last request, once no job
  runs. With `--no-window` it runs until you press Ctrl+C.
- The global options are not accepted here.

Everything in the page is described in [interface.md](interface.md).

### `verify`

```text
btms verify [--game <folder>]
```

Checks the whole reading chain: the game folder, the keys and the archives. It opens four
archives (`common.rpf`, `update/update.rpf`, a Cayo Perico DLC archive and `x64c.rpf`), decrypts
them and counts their files. In the first two it decrypts an XML file and checks that plain text
comes out. It prints one line per archive and ends with an error if any check fails.

Run it once after importing the keys. In the interface: **Tools → Check the game and the keys**.

### `import-keys`

```text
btms import-keys <magic.dat> [--game <folder>]
```

Imports the NG decryption keys from CodeWalker's `magic.dat`. BTMS reads the AES key from your
own `GTA5.exe`, decodes the file with it, checks each of the 101 keys and 272 tables against its
SHA-1 digest, and keeps them in `%LOCALAPPDATA%\BTMS\keys`. A file that does not decode exactly is
refused. Once is enough.

- `magic.dat` is never shipped with BTMS. The Windows installer has a task, **Set up the keys
  now**, ticked by default: it downloads `magic.dat` from CodeWalker's official GitHub into the
  setup's temporary folder and runs `btms import-keys` on it. If that fails (no network, or GTA V
  not found through FiveM), BTMS is installed all the same and you import the file later.
- Give `--game` when FiveM's settings do not point at your GTA V folder.
- In the interface: **Import magic.dat…**.

### `ls`

```text
btms ls <path>
```

Lists what is inside the game's archives. `<path>` starts with an archive under the game folder
and goes on inside it, through archives within archives: `x64a.rpf`, `update/update.rpf/x64`,
`update/x64/dlcpacks/mpheist4/dlc1.rpf`. Both `/` and `\` work. For a folder, each entry is listed
with its kind and size (compressed size and encryption when they apply). For a file, that file
alone is described.

### `extract`

```text
btms extract <path> <file>
```

Writes one file from the archives to `<file>`, decrypted and decompressed. `<path>` is written as
for `ls`. A resource file (`.ydr`, `.ytd`, `.ymap`…) keeps its `RSC7` header.

### `find`

```text
btms find <pattern>
```

Prints the full path of every file in the game's archives whose path contains `<pattern>`,
ignoring case, archives within archives included. It stops after 200 paths. The paths it prints
can be given to `ls` and `extract`. The files of your server resources are not searched.

### `tiles`

```text
btms tiles [<folder>] [--zone x1,y1,x2,y2] [--threads auto|<n>] [--ram <GB>] [--vram <GB>]
           [--format webp|jpeg] [--quality <0-100>|lossless] [--webp-effort fast|careful]
           [--dry-run] [global options]
```

Renders the map, or a zone of it, as tiles. What it writes is described in
[tile-format.md](tile-format.md).

| Option | Value | Default | What it does |
|---|---|---|---|
| `<folder>` | one folder | `output/tiles` | Where the tiles go. A relative folder is taken from the current folder. |
| `--zone x1,y1,x2,y2` | four numbers, game metres | the whole map | The rectangle to draw (`x` east, `y` north, the corners in any order), widened outwards to whole 128 m blocks. |
| `--threads <t>` | `auto\|<n>`, 1 to 256 | `auto` | The drawing threads. `auto`: one per logical core on the processor alone, half as many again when the card draws, one per logical core with the chain on the card. The writer threads follow (see [architecture.md](architecture.md#11-from-a-block-to-tiles)). |
| `--ram <GB>` | more than 0, at most 4096 | `6` | Memory for the models and textures kept by all threads together. Past it, the cache is emptied and files are read again. |
| `--vram <GB>` | more than 0, at most 4096 | `BTMS_GPU_MEMORY`, else `4` | Memory for the textures kept on the graphics card. When the card refuses memory, BTMS lowers it by itself. |
| `--format <f>` | `webp\|jpeg` | `webp` | The tile files. `jpeg` writes `.jpg` (quality 82) for full tiles and lossless `.png` for partly empty ones. |
| `--quality <q>` | `<0-100>\|lossless` | `90` | The WebP quality, or lossless WebP. Refused with `--format jpeg`. |
| `--webp-effort <e>` | `fast\|careful` | `fast` | How hard the WebP encoder works. `fast`: method 0 with the plain RGB to YUV conversion. `careful`: method 4 with the "sharp" conversion, which keeps thin coloured lines such as road markings truer, about four times slower to encode. Refused with `--format jpeg`. |
| `--dry-run` | no value | | Checks the options and the configuration, prints the settings the render would use (threads, who draws, who shades, the chain, memory, format, zone, folder), and stops. The game is not read and nothing is drawn. The graphics card is opened, to say which one would draw. |

- **The folder.** A render writes over the tiles already in its folder, but never removes a tile
  that now comes out empty: render into an empty folder to start clean. A `--zone` into a folder
  that holds tiles of another format is refused before anything is read.
- **One writer at a time.** While it writes, the folder holds `write.lock`. A second `tiles` or
  `update` into the same folder is refused while the first one runs.
- **The resources record.** A whole-map render (not `--zone`) writes `resources.json` at the end,
  unless blocks failed. `btms update --changes` uses it.
- **Failed blocks** are listed at the end of the output (the first ten) and counted. Draw them
  again with `btms update --zone`.
- The interface renders zones into its preview folder (`output/preview` by default) and the whole
  map into `output/tiles`.

### `update`

```text
btms update [<folder>] --zone x1,y1,x2,y2 [drawing options] [--dry-run] [global options]
btms update [<folder>] --changes            [drawing options] [--dry-run] [global options]
btms update [<folder>] --list               [global options]
btms update [<folder>] --record             [global options]

drawing options: [--threads auto|<n>] [--ram <GB>] [--vram <GB>]
```

Redraws part of a map that was already drawn, in that map's own folder, after you add, change or
remove a resource. `<folder>` defaults to `output/tiles` and must hold a finished render of
`btms tiles` (`"complete": true` in its `tiles.json`), except for `--list`. Give exactly one of
the four modes.

| Mode | What it does |
|---|---|
| `--zone x1,y1,x2,y2` | Redraws the 128 m blocks of a rectangle, widened outwards. Only blocks inside the map are drawn; a zone wholly outside it is refused. When the map settings differ from the record, it says so and draws with the current ones. |
| `--changes` | Redraws the ground of every resource added, changed or removed since `resources.json` was written: its old and its new ground, plus any on-demand map now drawn differently, widened by one block (more when shadows reach farther). Then it writes `resources.json` again, unless blocks failed: the next `--changes` then draws the same ground again. Refused when the folder has no `resources.json`, or when the `[maps]`, `[lighting]` or `[water]` settings no longer match it. |
| `--list` | Says which resources were added, changed or removed and what `--changes` would redraw. Draws and writes nothing, and takes no lock. |
| `--record` | Writes `resources.json` for the resources as they are now, and draws nothing. Use it when the map is up to date but has no record, for example after a render with `--zone`. |

- **Options.** `--threads`, `--ram`, `--vram`, `--dry-run` and the global options work as for
  `tiles`. `--format` and `--quality` are accepted but not used: an update always keeps the map's
  own format, quality and WebP effort, read from its `tiles.json`, and says so when you asked for
  another format. `--webp-effort` is refused. With `--list` and `--record` the drawing options change nothing.
- **What changes in the folder.** A redrawn tile that comes out empty is deleted. Zooms 6 to 0 are
  made again above every block touched. `tiles.json` stays the map's own: its `bounds`, `blocks`
  and `resources` follow the change, and an entry is added to its `updates` list (the last 100 are
  kept).
- **`--dry-run`** prints the settings the update would use and the mode, reading only the map's
  `tiles.json`.
- Like `tiles`, an update holds `write.lock` in the folder while it writes (not `--list`).

When an update is refused, [troubleshooting.md](troubleshooting.md#an-update-is-refused) explains
each reason.

### `convert`

```text
btms convert <folder> --to <folder> [--quality <0-100>|lossless] [--webp-effort fast|careful]
             [--threads auto|<n>]
```

Writes a finished folder of tiles (JPEG and PNG, or WebP) again as WebP, into another folder, to
publish a map without drawing it again. It needs neither the game nor the keys.

| Option | Value | Default | What it does |
|---|---|---|---|
| `<folder>` | a folder of tiles | required | The folder to convert. It is only read. |
| `--to <folder>` | a folder | required | Where the WebP tiles go. It must be new, empty, or an earlier conversion. It may not be the source folder, hold it, or be inside it. A render's folder is refused, because its tiles would be replaced. |
| `--quality <q>` | `<0-100>\|lossless` | `90` | The WebP quality. |
| `--webp-effort <e>` | `fast\|careful` | `careful` | As for `tiles`. A publication is not in a hurry, so the default is `careful`. |
| `--threads <t>` | `auto\|<n>`, 1 to 256 | `auto` (one per logical core) | The encoding threads. |

- The target gets every tile that is not empty as `.webp`, the viewer page `index.html`, and a
  `tiles.json` made from the source's, with the new format's keys and a `converted_from` entry.
  The source folder's name is recorded, but never its path.
- A JPEG tile is decoded and encoded again, so it keeps its JPEG artefacts. For the full quality,
  render the map again in WebP.
- `--format` is refused: the target is always WebP.
- If any tile cannot be converted, the command ends with an error and the target's `tiles.json`
  says `"complete": false`.

### `groups`

```text
btms groups [global options]
```

Prints a report on the maps the game loads only on demand (yachts, UFOs, the states of a building
that a mission changes): how they are recognised, what `[maps] keep` matches, and every on-demand
map with whether it is drawn and why. With `[maps] fill_holes = true` (the default), it first
searches for the holes in the ground that such a map closes, as a render does, which can take
several minutes on the whole map. Use it to find the names to put in `keep`: see
[configuration.md](configuration.md#finding-names-btms-groups).

### `mend`

```text
btms mend [<folder>]
```

Colours in the last holes of a folder already drawn (`output/tiles` by default), then makes zooms
6 to 0 again above the blocks it touched. It is the pass a render runs at the end when
`[maps] fill_holes` is on: empty regions closed all around by drawn ground, filled from their rim
inwards. A region that reaches the outside is the open sea and stays empty; a region larger than
2,000 m² is left and reported. `mend` takes no lock: do not run it on a folder that a render is
writing.

### `probe`

```text
btms probe <bx> <by> [--image <file.png>] [global options]
```

Explains what one 128 m block is made of, to understand a fault seen on the map (a hole, a black
or grey patch). Block `(bx, by)` covers `x` from `bx·128` to `(bx+1)·128` and `y` from `by·128` to
`(by+1)·128`: these are the numbers the interface shows under **block**. Write them as `16 40` or
`16,40`; each must be between −1000 and 1000.

It prints:

1. every model placed in the block, as the tiles gather them: the model file, the placement file,
   the detail level and how many;
2. what is not drawn, and why: entities the tiles leave out, models that cannot be drawn (not
   found, protected, unreadable), pieces of models left out;
3. the block drawn in memory as the tiles draw it before the lighting, the hole filling and the
   water, at 0.25 m per pixel, and its pixels that look wrong (empty, near black, plain grey),
   each put on the models that paint or would cover it.

| Option | Value | What it does |
|---|---|---|
| `--image <file.png>` | a PNG file | Also writes that block picture (512 × 512 px, unlit, empty pixels transparent). Nothing else is written. |
| `--renderer <r>` | `auto\|gpu\|cpu` | Who draws the block, as for a render (a global option). |

`probe` does not run the search for holes that an on-demand map closes. See
[troubleshooting.md](troubleshooting.md#black-or-grey-patches).

### `resources`

```text
btms resources [--all] [--detail] [global options]
```

Reports on the FiveM resources of `[resources] folders`: for each resource, how many files it
streams and of which kinds, which of them replace a game file, what it removes, and what could not
be read. Protected content (files encrypted by FiveM's asset escrow, `FXAP`, and models with
scrambled vertices) is reported and left out, never decrypted. Without any resource configured,
it says so.

| Option | What it does |
|---|---|
| `--all` | Also reads every model and texture dictionary the resources stream, to find the ones that cannot be read. Slower. |
| `--detail` | Names every file that failed, instead of three per reason. |

### `help`

```text
btms help
btms -h
btms --help
```

Prints the usage of every command, the global options and the environment variables. It works
without GTA V and with a broken `map.toml`. An unknown command also prints the usage.

### `--version`

```text
btms --version
btms -V
```

Prints `btms <version>`, for example `btms 1.0.0`. Like `help`, it reads nothing.

## Environment variables

An environment variable applies to every command started from the same terminal. In PowerShell:

```powershell
$env:BTMS_RENDERER = "cpu"      # set it for this terminal
.\btms.exe tiles --dry-run
Remove-Item Env:BTMS_RENDERER   # and remove it
```

In `cmd.exe`: `set BTMS_RENDERER=cpu`, and `set BTMS_RENDERER=` to remove it. An option given on
the command line always wins over its variable.

### Everyday

| Variable | Value | What it does |
|---|---|---|
| `BTMS_RENDERER` | `auto\|gpu\|cpu` | The default of `--renderer`. Handy for scripts whose command lines are already written. |
| `BTMS_SHADING` | `auto\|gpu\|cpu` | The default of `--shading`. |
| `BTMS_CHAIN` | `auto\|gpu\|cpu` | The default of `--chain`. |
| `BTMS_GPU` | words, for example `vulkan` or `dx12 radeon` | Picks the graphics card whose API and name contain every one of the words, ignoring case. Naming a card this way also allows a software adapter. When no card matches, the processor draws and the log lists every card found. |
| `BTMS_GPU_MEMORY` | megabytes | Textures kept on the graphics card when `--vram` is not given. Default `4000`. Careful: this one is in **MB**, while `--vram` is in **GB**. |
| `BTMS_LOAD_THREADS` | 1 to 256 | Threads that read and parse the game's files when the world is loaded. Default: one per logical core. |
| `BTMS_PROFILE` | anything but `0` | The same as `--profile`. |
| `BTMS_BLOCK_TIMES` | set to anything | Prints the time per block at the end of a render. Without it, that line is printed only when the lighting is on or the chain is used. |
| `BTMS_INTERFACE_DIR` | a folder | The interface serves its page files from this folder instead of the copies built into the executable. For working on the page: point it at `web/interface`. |
| `BTMS_PROGRESS` | `1` | Set by the interface on every job it starts: the job writes progress events on its standard output as lines starting with `@@BTMS `, and ends when its standard input closes (exit code 3). Leave it unset in a terminal. |

`WGPU_BACKEND` is wgpu's own variable: `vulkan` or `dx12` limits the cards looked at to that API.
See [troubleshooting.md](troubleshooting.md#the-graphics-card-is-not-used).

### For checks and bug reports

These compare results or reproduce a fault. Some of them change the tiles or slow a render down a
lot: leave them unset for a normal render.

| Variable | Value | What it does |
|---|---|---|
| `BTMS_GPU_COMPARE` | `1`, or a folder | Draws every picture both on the processor and on the card and counts the differences. With a folder, also writes `image_<nnn>_cpu.png`, `image_<nnn>_gpu.png` and `image_<nnn>_diff.png` there. Keeps `--chain auto` from using the chain. `0` or empty: off. |
| `BTMS_FINGERPRINTS` | a file | Appends one line per picture drawn to that file: a label (`block <bx>,<by>`, with `heights` or `shaded`) and the SHA-1 of its exact colours and depths. Keeps `--chain auto` from using the chain; with `--chain gpu`, the blocks made on the card have no line. |
| `BTMS_REFERENCE_DRAWING` | anything but `0` | The processor draws through its reference rasterizer instead of the fast one: the same pictures, more slowly. `--renderer auto` then draws on the processor. |
| `BTMS_CHECK_READS` | anything but `0` | Every archive read through the cache is compared with the same read without it. A difference stops the program and names the file. |
| `BTMS_CHECK_JPEG` | anything but `0` | Every JPEG tile is also encoded by the `image` crate, the reference encoder, and must come out byte for byte the same. A difference stops the program. |
| `BTMS_RESOURCE_ORDER` | `first-wins` | Of two streamed files with the same name, the first one wins instead of the last. Any other value: the default order. Useful to find which resource a clash comes from. |
| `BTMS_HIDE` | `name,name,…` | Archetypes not drawn, by name: to look under a roof. |
| `BTMS_SHADOW_REACH` | metres | How far a shadow reaches, 1 to 512. Default `48`. It changes the tiles, and the value is written into `tiles.json` as `lighting.shadow_reach_m`. |
| `BTMS_SHADOW_MARGIN` | metres | Metres added to every shading margin, 0 to 256. It must not change the result: a check of the shading. |
| `BTMS_SHADOW_DRAW_ALL` | set to anything | Draws everything in the shading margins, not only what is tall enough to cast a shadow into the block. It must not change the result either. |
| `BTMS_TERRAIN` | `set,a,b` | Overrides the terrain layer weights: the vertex colour set (0 or 1), the byte of the first blend and the byte of the final blend. Default `1,2,1`. |
| `BTMS_TERRAIN_MASK` | `a,b` | Overrides the channels of the terrain blend mask. Default `2,1`. |
| `BTMS_GPU_FAULT` | `device`, `fail` or `fail:<n>` | Simulates a fault of the drawing card, to see the fallbacks: `device` makes the card impossible to open, `fail` makes it fail at its first picture, `fail:<n>` at its n-th. |
| `BTMS_CHAIN_FAULT` | `fail[:<n>]` or `abandon[:<n>]` | Simulates a fault of the chain on the card: `fail` makes the n-th block fail (it is made again on the processor), `abandon` makes the chain be given up at the n-th block. Without `:<n>`, the first. |

A few more variables are read only by the project's ignored tests; they are described next to
those tests in the source.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | The command succeeded. A `tiles` or `update` run in which some blocks failed also ends with 0: the failures are listed at the end of its output and counted in its progress events. |
| `1` | An error. The message is the last line on the error output and starts with `error: `. This includes a refused option, a failed `verify` check and a `convert` with tiles that could not be converted. |
| `3` | Only for a job started with `BTMS_PROGRESS=1`: its standard input closed because the interface that started it went away, so it stopped. |
| `101` | The program stopped on an unexpected internal error (a Rust panic). Please [report it](troubleshooting.md#reporting-a-problem) with the output. |

## Examples

The examples use PowerShell from the folder that holds your `map.toml`. Write `btms` instead of
`.\btms.exe` if BTMS is on your PATH.

**1. Import the keys by hand, then check everything.**

```powershell
.\btms.exe import-keys "$env:USERPROFILE\Downloads\magic.dat"
.\btms.exe verify
```

**2. Check the settings of a render without reading the game.**

```powershell
.\btms.exe tiles --dry-run
```

**3. Draw a 1 km² test area of downtown Los Santos into its own folder.**

```powershell
.\btms.exe tiles output\test --zone 0,-1536,1024,-512
```

**4. Draw the whole map with fixed threads and memory budgets.**

```powershell
.\btms.exe tiles --threads 12 --ram 12 --vram 6
```

**5. Try an evening sun on a test area, without editing `map.toml`.**

```powershell
.\btms.exe tiles output\evening --zone 0,-1536,1024,-512 --azimuth 270 --elevation 15 --strength 0.8
```

**6. After changing your server's resources, see what changed, then redraw it.**

```powershell
.\btms.exe update --list
.\btms.exe update --changes
```

**7. Make a lighter copy of a finished map for publishing (WebP quality 80, encoded with care).**

```powershell
.\btms.exe convert output\tiles --to output\web --quality 80
```

**8. Look into a black patch on block 16, 40.**

```powershell
.\btms.exe probe 16 40 --image block-16-40.png
```
