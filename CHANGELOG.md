# Changelog

All notable changes to BTMS are documented in this file.

BTMS follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html). From 1.0.0 on, each
release's section is written by the release workflow from the Conventional Commits merged into
`main` (see [CONTRIBUTING.md](CONTRIBUTING.md#releases)).

## [1.0.0]

First public release.

### Added

- **Whole-map tiles from the game's own files.** BTMS exports the GTA V map seen from above as web
  map tiles: zooms 0 to 9, 512 × 512 px, 6.25 cm per pixel at zoom 9, written as
  `<folder>/<z>/<x>/<y>.<extension>`. The placements of every map file the game uses are drawn with
  their most detailed models, including the updates and DLC content (Cayo Perico among them) and the
  on-demand maps kept in `map.toml`.
- **Offline and read-only.** BTMS reads the game's archives on disk. It never attaches to the game
  or to FiveM, never reads their memory, and never writes to the game folder.
- **FiveM server resources.** BTMS draws the server's own unprotected map resources (streamed
  `.ymap`, `.ytyp`, `.ydr`, `.ydd`, `.yft` and `.ytd`) from the folders listed under `[resources]`
  in `map.toml`. A streamed file replaces the game's file of the same name. Files protected by the
  FiveM asset escrow (`FXAP`) are detected, reported and left out. They are never decrypted.
- **Tile formats.** WebP by default (lossy, quality 90, or `--quality lossless`), with transparency
  where a tile is partly empty, or JPEG with PNG for partly empty tiles (`--format jpeg`). Empty
  tiles are not written, and each tile is written in one step, so a reader never sees half a file.
- **Tile manifest.** `tiles.json` (format `btms tiles v1`) describes the folder. It is written when
  a render starts and completed when it ends, so the folder can be viewed while it fills.
- **Standalone viewer page.** An `index.html` written next to `tiles.json` shows the map in a
  browser, in one self-contained file: the folder can be published as it is.
- **Lighting.** Sun shading with a configurable azimuth, elevation and strength, cast shadows, and
  ambient occlusion at the foot of walls, set in `[lighting]` of `map.toml` or on the command line
  (`--lighting on|off`, `--no-lighting`, `--azimuth`, `--elevation`, `--strength`,
  `--shadows on|off`, `--ambient-occlusion`).
- **Water** surfaces drawn on the map (`[water]` of `map.toml`).
- **Hole filling.** Small holes in the ground are filled from what surrounds them
  (`fill_holes` in `map.toml`), and `mend` fills the last holes of a folder already rendered.
- **GPU rendering.** Drawing and shading on the graphics card with wgpu (DirectX 12 or Vulkan),
  with an automatic fallback to the processor when no card can be used or the card fails during a
  render (`--renderer`, `--shading` and `--chain`: `auto|gpu|cpu`).
- **Zone updates.** `update` redraws only a zone (`--zone`), or only the areas that added, changed
  or removed resources affect (`--changes`), inside an existing map folder and without seams.
  `--list` says what changed without drawing, and `--record` notes the resources as they are now
  in `resources.json`.
- **Local interface.** `btms interface` (also the default with no command) opens a local web
  interface in an app window: settings, checks of the game folder, keys and `map.toml`, a live view
  of the machine's load, previews of a zone, full renders, zone updates, reports, and live progress
  (stage, blocks, rate, time left, tiles appearing on the map as they are written). It listens on
  `127.0.0.1` only and requires a per-run token.
- **Seven languages** in the interface, the viewer page and the installer: English, French,
  German, Dutch, Russian, Swedish and Spanish. The interface and the viewer page follow the
  browser's language by default, and each has a language selector.
- **Windows installer**, `BTMS-Setup-<version>-x64.exe`. It installs for the current user without
  administrator rights, or for all users. It adds a Start menu shortcut, and optionally a desktop
  icon and `btms` on the PATH. It creates the working folder `Documents\BTMS` with a copy of
  `map.toml` (kept on upgrades and on uninstall), where renders go by default. Each release also
  has a portable zip, `BTMS-<version>-x64-portable.zip`, and `SHA256SUMS.txt`.
- **Keys set up by the installer.** The installer's task "Set up the keys now: download magic.dat
  from CodeWalker's official GitHub", ticked by default, downloads `magic.dat` into the setup's
  temporary folder and runs `btms import-keys` on it. If that fails (no network, or GTA V not found
  through FiveM), BTMS is installed all the same, and the keys can be imported later with
  **Import magic.dat…** in the interface or `btms import-keys <magic.dat> --game <GTA V folder>`.
  `magic.dat` and the keys are never shipped with BTMS.
- **Commands:**
  - `tiles` renders the whole map, or a zone, as tiles;
  - `update` redraws part of an existing map;
  - `convert` writes a rendered folder again as WebP into another folder (`--to`);
  - `mend` fills the last holes of a rendered folder;
  - `groups` lists the on-demand maps that are kept or left out, and why;
  - `resources` reports what the server's resources hold, replace and remove;
  - `probe` explains what a 128 m block is made of, to understand a defect seen on the map;
  - `verify`, `ls`, `extract` and `find` inspect the game's archives;
  - `import-keys <magic.dat>` decodes the decryption keys from CodeWalker's `magic.dat` with your
    own copy of the game, checks every one, and stores them in `%LOCALAPPDATA%\BTMS\keys`;
  - `help` (also `-h`, `--help`) and `--version` (also `-V`).
- **`--dry-run`** for `tiles` and `update` checks the options, prints the settings the render would
  use, and stops before reading the game.

[1.0.0]: https://github.com/BuuDevOff/BTMS/releases/tag/v1.0.0
