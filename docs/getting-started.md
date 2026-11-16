# Getting started

From the download to a map on your website: install BTMS, set up the keys, render a zone and then
the whole map, publish the tiles, and keep them up to date when your resources change.

- [1. What you need](#1-what-you-need)
- [2. Install](#2-install)
- [3. Import the keys](#3-import-the-keys)
- [4. Add your server's resources (optional)](#4-add-your-servers-resources-optional)
- [5. First render with the interface](#5-first-render-with-the-interface)
- [6. First render from the command line](#6-first-render-from-the-command-line)
- [7. Where things are written](#7-where-things-are-written)
- [8. Put the map on a website](#8-put-the-map-on-a-website)
- [9. Update the map after resource changes](#9-update-the-map-after-resource-changes)
- [Next steps](#next-steps)

## 1. What you need

- **Windows 10 or 11, 64-bit.**
- **A legal copy of GTA V for PC, installed.** BTMS is tested with the Legacy edition, the one
  FiveM runs on. It only reads the game folder and never changes it.
- **CodeWalker's `magic.dat`**, for the decryption keys. BTMS does not ship it, but the installer
  can download it for you ([step 3](#3-import-the-keys)).
- **A graphics card with DirectX 12 or Vulkan** is recommended. Without one, the processor draws
  the same picture, more slowly.
- **About 10 GB of disk space** for the whole map at the default quality (about 240,000 files).
- **Optional: your server's resource folders** on this PC, to draw your mappings. FiveM itself is
  not required.

## 2. Install

The [latest release](https://github.com/BuuDevOff/BTMS/releases/latest) has two downloads:

- **`BTMS-Setup-<version>-x64.exe`, the installer** (recommended, in 7 languages).
  - It installs for your user without administrator rights, in `%LOCALAPPDATA%\Programs\BTMS`.
    Its first page also offers an install for all users.
  - It adds a **BTMS** shortcut to the Start menu. A desktop icon, and a `btms` entry in the PATH
    to run it from any terminal, are optional.
  - Its keys task, ticked by default, sets up the decryption keys for you ([step 3](#3-import-the-keys)).
  - Your working folder is `Documents\BTMS`. The shortcut starts BTMS there, and the installer
    copies a documented `map.toml` there once. Upgrades and uninstalls never touch that copy.
  - To upgrade, run a newer installer. Uninstall from Windows Settings → Apps.
- **`BTMS-<version>-x64-portable.zip`.** Extract it into a folder of its own and double-click
  `btms-gui.exe` to open the interface (`btms.exe` is the command line). Nothing is installed, and
  you set up the keys by hand ([step 3](#3-import-the-keys)).

Each release also lists the SHA-256 checksums of its files in `SHA256SUMS.txt`. The executables are
not code-signed yet, so Windows SmartScreen may warn you the first time: choose **More info**, then
**Run anyway**.

When BTMS starts, the interface opens in its own window (Microsoft Edge in app mode, or your
default browser).

BTMS reads `map.toml` from the folder it runs in, and writes its tiles to `output\tiles` under that
folder by default. That folder is `Documents\BTMS` for the installed shortcut, the zip's folder for
a portable `btms-gui.exe`, and the current folder in a terminal.

**From a terminal.** The examples in these guides run `.\btms.exe` in a terminal opened in the
BTMS folder, as with the portable zip. With the installer, tick its PATH option, open the terminal
in `Documents\BTMS` and type `btms` instead of `.\btms.exe`. Without the PATH option, give the full
path: `& "$env:LOCALAPPDATA\Programs\BTMS\btms.exe"` in PowerShell, for an install for your user.

## 3. Import the keys

The game's archives are encrypted, and BTMS ships no key. It reads the AES key from your own
`GTA5.exe` and decodes the other keys (the NG keys) from CodeWalker's `magic.dat`. You do this once
per Windows user.

### With the installer

Leave the task **Set up the keys now: download magic.dat from CodeWalker's official GitHub** ticked
(it is by default). When you click **Install**, the setup downloads `magic.dat` from
[CodeWalker's official repository](https://github.com/dexyfex/CodeWalker), then runs
`btms import-keys` on it as your user. If no message appears, the keys are in place, and the
**Keys** check is green when BTMS opens.

If it fails, a message says so (no network, or GTA V not found through FiveM), and BTMS is
installed anyway: import the keys by hand, as below. In an install for all users, the keys are set
up only for the Windows user who ran the setup.

### By hand (portable zip, or when the installer could not)

1. **Get `magic.dat`** from CodeWalker's official repository:
   [CodeWalker.Core/Resources/magic.dat](https://github.com/dexyfex/CodeWalker/blob/master/CodeWalker.Core/Resources/magic.dat).
   Use the download button of that page: "Save link as" saves a web page instead of the file.
   CodeWalker's own release keeps `magic.dat` inside its DLL, so you will not find it in a
   CodeWalker folder.
2. **Import it**, either way:
   - in the interface, click the **Keys** check in the top bar (or the red line under it when the
     keys are missing), then **Import magic.dat…**, and pick the file. The result shows in the log;
   - in a terminal ([`btms` with the installer](#2-install)):

     ```powershell
     .\btms.exe import-keys "path\to\magic.dat"
     ```

You can delete `magic.dat` afterwards: the keys are kept in `%LOCALAPPDATA%\BTMS\keys`.

### Finding the game

BTMS finds GTA V through FiveM's settings: the `IVPath` line of
`%LOCALAPPDATA%\FiveM\FiveM.app\CitizenFX.ini`. Without FiveM, or to use another copy of the game,
give the folder that holds `GTA5.exe`:

- in the interface, click the **Game** check, then **Choose the game folder…**;
- on the command line, add `--game` to the command:

  ```powershell
  .\btms.exe import-keys "path\to\magic.dat" --game "D:\Games\Grand Theft Auto V"
  ```

### What the import does

1. It finds the AES key in your `GTA5.exe` by its SHA-1 hash (the first time only).
2. It decodes `magic.dat` with it and checks each of the 101 NG keys and 272 decrypt tables against
   its SHA-1 hash. A file that does not decode, or a single wrong key, stops the import with an
   error, and no NG key is saved.
3. It saves the keys in `%LOCALAPPDATA%\BTMS\keys`. They are checked again every time they are read.

**Check it.** `.\btms.exe verify` (or **Tools → Check the game and the keys** in the interface) opens a
few archives of the game, the Cayo Perico DLC among them, and decrypts files from them. If
something goes wrong, see [troubleshooting](troubleshooting.md#the-keys-are-missing-or-refused).

## 4. Add your server's resources (optional)

BTMS draws what your resources stream (`ymap`, `ytyp`, `ydr`, `ydd`, `yft`, `ytd` under their
`stream/` folder) over the game's files: a file with the same name as a game file replaces it, and
a new name is added.

- **In the interface:** in the settings column, open **What is drawn**, then **Server resources**
  and **Add a folder…**. Untick a folder to leave it out of the next render without forgetting it.
- **In `map.toml`:**

  ```toml
  [resources]
  folders = ['C:\FXServer\resources\[maps]', 'C:\FXServer\resources\[local]\my-island']
  ```

Each entry is a resource folder, or a folder of resources such as `[maps]`. Every resource found is
used, whether your server starts it or not. When two resources stream a file of the same name, the
later one wins. Write Windows paths in single quotes.

Protected resources (FiveM asset escrow) are never decrypted: their protected files are left out
and reported. `.\btms.exe resources` (or **Tools → Server resources inventory**) shows what BTMS
makes of your resources before you render. Every key of `map.toml` is in
[configuration.md](configuration.md).

The interface never rewrites `map.toml`. What you change in its settings is kept apart as
overrides, applied to a copy of `map.toml` for each render. A section that differs from the file
says so and offers **Back to map.toml**.

## 5. First render with the interface

1. **Try a zone first.** Click **Pick a zone** in the map toolbar and draw a rectangle (or hold
   Shift and drag). It snaps to the 128 m blocks of the render. Click **Preview the zone**: the
   tiles go to the preview folder and appear on the map block by block. This is the quick way to
   try the **Lighting** settings and check your resources.
2. **Choose the folder and the format.** In **Folders and format**, **Whole map folder** is
   `output\tiles` by default. The tiles are **WebP** (the default, quality 90, or **Lossless**) or
   **JPEG and PNG**.
3. **Choose how hard the PC works.** **Performance** has three presets: **Maximum**, **Balanced**
   and **Quiet** (low priority, the PC stays usable). Every setting can also be set by hand.
4. **Render the whole map.** Click **Whole map** and confirm. The right column follows the steps
   (**Loading the game**, **Placements**, **Blocks**, **Zoomed-out levels**, **Done**), the speed, the time
   left, the tiles written and the errors. The far zooms are rebuilt about every 30 seconds, so the
   whole map fills in as you watch.
5. **When it is done,** the summary gives the blocks, tiles, errors and duration, and **Open the
   folder** shows the result.

Good to know:

- **Stop** ends the running job. The tiles already written stay, but a stopped render does not
  resume where it was.
- If the graphics card leaves work to the processor (it failed, or ran out of memory), an amber
  banner says how many blocks and why. The render goes on.
- Closing the window does not stop a render: start BTMS again (Start menu, or `btms-gui.exe`) to
  reopen it. With its window
  closed and no job running, BTMS quits by itself after five minutes.
- Every part of the interface is described in [interface.md](interface.md).

## 6. First render from the command line

The same work without the interface. Open a terminal in the BTMS folder (with the installer, use
`btms` from `Documents\BTMS`, as in [step 2](#2-install)):

```powershell
# check the settings: prints them and stops, without reading the game
.\btms.exe tiles --dry-run

# a 1 km² test area of downtown Los Santos, into its own folder
.\btms.exe tiles output\test --zone 0,-1536,1024,-512

# the whole map, into .\output\tiles
.\btms.exe tiles

# another configuration file, without lighting for this run
.\btms.exe tiles --config "path\to\map.toml" --no-lighting
```

- `--zone x1,y1,x2,y2` takes game metres (`x` east, `y` north) and is widened to whole 128 m
  blocks. An option and its value are two separate arguments: `--zone=…` is refused.
- The render prints its settings, its progress and a summary at the end.
- `.\btms.exe help` lists every command. Every option (threads, who draws, memory budgets, tile
  format and quality) is in [cli.md](cli.md).

## 7. Where things are written

| What | Where |
|---|---|
| Whole-map tiles | `output\tiles` under the current folder, or the folder you give |
| Zone previews (interface) | `output\preview` under the current folder, or the folder you choose |
| Map settings | `map.toml` in the current folder, or `--config <file>`; only read, never written |
| Keys | `%LOCALAPPDATA%\BTMS\keys` |
| Interface settings and overrides | `%LOCALAPPDATA%\BTMS` |
| Game folder | only read, never written |

A finished tiles folder holds:

```text
output/tiles/
├── index.html       the viewer page
├── tiles.json       the manifest: grid, format, covered area, render state
├── resources.json   the record of your resources, for updates (after a whole-map render)
└── 0/ … 9/          one folder per zoom: <z>/<tx>/<ty>.webp
```

`tiles.json` says `"complete": false` while the render runs, and `"complete": true` at the end.
`resources.json` holds folder names but no path from your disk, so the folder can be published as
it is. While a render or an update writes a folder, a lock keeps a second one out of it.

Before each preview, BTMS empties the preview folder, carefully: only when it is empty or holds
BTMS tiles, and only the files BTMS writes (`tiles.json`, `index.html` and the zoom folders `0` to
`9`). A preview folder that is, contains or sits inside the whole map folder, or that holds a
whole-map render, is refused and nothing is deleted.

## 8. Put the map on a website

The tiles folder is a complete static website. Upload it as it is, at the root of a site or in a
sub-folder, and open its address: `index.html` shows the map, with pan, zoom, game coordinates
under the pointer and a language selector.

**Look at it locally first.** A page opened from a file (`file://`) cannot load `tiles.json`, so
serve the folder over HTTP. Any static server will do, for example:

```powershell
python -m http.server 8000 --directory output\tiles
```

Then open <http://localhost:8000/>.

**Hosting.** A whole map is about 240,000 files and 10 GB. Many free page hosts limit the number of
files or the size of a site, so your own web server, or an object storage bucket behind a CDN, suits
it better. Check that `.webp` files are served as `image/webp` (most servers do this out of the box).

**Publishing while it renders.** The viewer reads `tiles.json` again every 30 seconds while
`"complete"` is `false`, so a folder that is still being rendered can already be online.

**In your own page.** Embed the viewer in an `<iframe>`:

```html
<iframe src="https://maps.example.com/" title="Server map" style="width: 100%; height: 600px; border: 0"></iframe>
```

Or load the tiles in your own map library. The grid is a flat grid in game metres, not the usual
web-mercator scheme: [tile-format.md](tile-format.md#using-the-tiles-in-your-own-map) has Leaflet
and OpenLayers examples, and describes the grid and `tiles.json` in full.

## 9. Update the map after resource changes

A new mapping, a resource changed or removed: you do not need to render the whole map again. BTMS
redraws only the blocks that change, inside the existing map, with the same lighting and water and
no seams at the edges.

**How it knows what changed.** At the end of a whole-map render, BTMS writes `resources.json` into
the map folder. It records, for each resource, the files that can change the map (by path, size and
date, not their content) and the blocks of the map it covers. An update compares your resources
with that record.

**In the interface,** in the **Resource changes** section of the right column:

1. **Look for changes** lists every resource added, changed or removed since the record, with the place
   it covers on the map. Nothing is drawn. Hover a row to see its zone on the map.
2. **Update the map (N zones, M blocks)** redraws those zones in the whole map folder, after a
   confirmation. The page follows the update like a render, and the map refreshes block by block.
3. If the folder has no record yet, for example a map rendered before you added resources, the
   section offers **Record the current state**. It writes the record without redrawing anything:
   use it when the map already shows your resources as they are.

To redraw an area by hand, pick a zone and click **Update the map here**.

**From the command line:**

```powershell
.\btms.exe update --list                 # list what changed; draws nothing
.\btms.exe update --changes              # redraw what changed, in .\output\tiles
.\btms.exe update --zone x1,y1,x2,y2     # redraw an area
.\btms.exe update --record               # record the current state without drawing
```

Give the map folder as the first argument when it is not `output\tiles`:
`.\btms.exe update "D:\maps\tiles" --changes`.

Rules:

- The folder must hold a finished whole-map render (`"complete": true` in `tiles.json`).
- Updated tiles keep the format and quality of the folder.
- The redrawn area is the old and the new place of each changed resource, plus a margin of one
  block (more when long shadows reach further). Tiles that become empty are deleted, and the far
  zooms above the redrawn blocks are rebuilt.
- If `[maps]`, `[lighting]` or `[water]` changed since the record, `--changes` is refused, because
  the new settings would leave seams: render the whole map again. `--zone` only warns about it.
- Only one render or update writes a folder at a time.

**Online.** An update replaces tiles in place. If a CDN or a browser cache sits in front of your
tiles, purge it after an update so that visitors see the new tiles.

## Next steps

- [configuration.md](configuration.md): every key of `map.toml`.
- [cli.md](cli.md): every command and option.
- [interface.md](interface.md): every part of the interface.
- [tile-format.md](tile-format.md): the grid, the files and `tiles.json`.
- [troubleshooting.md](troubleshooting.md): when something goes wrong.
- [architecture.md](architecture.md): how BTMS reads the game and draws the map.
