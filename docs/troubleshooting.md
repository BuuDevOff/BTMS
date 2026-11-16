# Troubleshooting

Most problems explain themselves in the log: the terminal on the command line, or the log panel
of the interface. Find what it says, or what you see on the map, below.

- [Before anything else](#before-anything-else)
- [The keys are missing or refused](#the-keys-are-missing-or-refused)
- [The game folder is not found](#the-game-folder-is-not-found)
- [The graphics card is not used](#the-graphics-card-is-not-used)
- [An amber banner: the card left work to the processor](#an-amber-banner-the-card-left-work-to-the-processor)
- [Out of memory](#out-of-memory)
- [Black or grey patches](#black-or-grey-patches)
- [Holes and transparent areas](#holes-and-transparent-areas)
- [Protected resources are reported](#protected-resources-are-reported)
- [An update is refused](#an-update-is-refused)
- [A folder is being written by another job](#a-folder-is-being-written-by-another-job)
- [The interface does not open](#the-interface-does-not-open)
- [Antivirus or SmartScreen warnings](#antivirus-or-smartscreen-warnings)
- [The published map shows nothing](#the-published-map-shows-nothing)
- [Reporting a problem](#reporting-a-problem)

## Before anything else

Three quick checks narrow most problems down. None of them draws anything. The commands run in a
terminal in the BTMS folder; with the installer, type `btms` instead of `.\btms.exe` (see
[getting-started.md](getting-started.md#2-install)).

```powershell
.\btms.exe --version          # the version, for a bug report
.\btms.exe verify             # the game folder, the keys and the decryption, on a few real archives
.\btms.exe tiles --dry-run    # checks the options, opens the graphics card if it is to draw,
                              # prints the settings line, then stops before reading the game
```

In the interface, the three checks in the top bar (**Game**, **Keys**, **map.toml**) say whether
the game folder is found, the keys are imported and valid, and `map.toml` can be read.
**Tools → Check the game and the keys** runs `verify`, which goes further: it opens and decrypts real
archives.

Then try a small zone before the whole map: **Pick a zone** and **Preview the zone** in the
interface, or `btms tiles output\test --zone 0,-1536,1024,-512` (1 km² of downtown Los Santos).

## The keys are missing or refused

BTMS ships no key. The AES key comes from your own `GTA5.exe`, and the NG keys from CodeWalker's
`magic.dat`. Setting them up is [step 3 of getting started](getting-started.md#3-import-the-keys).

**The installer could not set up the keys.** Its message gives the two usual causes: no network
(the download from CodeWalker's GitHub failed), or GTA V not found through FiveM (the setup does
not ask for the game folder: it reads it from FiveM's settings). BTMS is installed all the same.
Import the keys by hand:

1. Download [`magic.dat` from CodeWalker's official repository](https://github.com/dexyfex/CodeWalker/blob/master/CodeWalker.Core/Resources/magic.dat)
   with the download button of that page ("Save link as" saves a web page instead).
2. Import it, in a terminal:

   ```powershell
   & "$env:LOCALAPPDATA\Programs\BTMS\btms.exe" import-keys "path\to\magic.dat" --game "<your GTA V folder>"
   ```

   That is the install folder for your user; with the PATH option, `btms import-keys …` is enough.
   `--game` is only needed when FiveM's settings do not give the game folder. In the interface,
   click the **Keys** check, then **Import magic.dat…**, and choose the game folder under the
   **Game** check first if it is red.

**The keys are missing.** The **Keys** check is red, or a command stops on an archive because it is
NG-encrypted and the NG keys are missing. Until they are imported, BTMS refuses every NG-encrypted
archive with an error rather than decrypting it with empty keys. Import them as above. The keys are
kept per Windows user, in `%LOCALAPPDATA%\BTMS\keys`: another user of the same PC imports them once
too.

**`import-keys` fails.** It decodes `magic.dat` the way CodeWalker does, then checks each of the
101 NG keys and 272 decrypt tables against its SHA-1 hash. No NG key is saved unless all of them
match.

| Error | What it means | What to do |
|---|---|---|
| `magic.dat` does not decompress | the file is not CodeWalker's `magic.dat` (for example a web page saved in its place), it is damaged, or your `GTA5.exe` does not have the AES key CodeWalker uses | download it again from [CodeWalker's repository](https://github.com/dexyfex/CodeWalker/blob/master/CodeWalker.Core/Resources/magic.dat) with the page's download button, and check that the game folder is your GTA V for PC |
| the decoded file is too short, or of an unexpected size | it decoded, but it is not the expected `magic.dat` | as above |
| some keys or tables do not have their SHA-1 hash | the decoded content is not the expected one | as above; a single wrong key stops the import |
| the AES key is not found in `GTA5.exe` | BTMS does not recognise this `GTA5.exe` | BTMS is tested with the Legacy edition, the one FiveM runs on; check the game folder |
| the GTA V folder is not found | FiveM's settings do not give it | add `--game`; see [the next section](#the-game-folder-is-not-found) |

**Keys that worked stop working.** The keys are checked against their SHA-1 hashes every time they
are read. If a key file is damaged, the error asks you to run `import-keys` again. To start over,
delete `%LOCALAPPDATA%\BTMS\keys` (it holds `gtav_aes_key.dat`, `gtav_ng_key.dat` and
`gtav_ng_decrypt_tables.dat`) and import again; the AES key is then searched for in `GTA5.exe`
once more.

Never attach the keys, `magic.dat` or game files to an issue. They are not needed to describe a
problem.

## The game folder is not found

BTMS reads the GTA V folder from FiveM's settings: the `IVPath=` line of
`%LOCALAPPDATA%\FiveM\FiveM.app\CitizenFX.ini`, accepted only if it holds `GTA5.exe`. This fails
when FiveM is not installed, when that file is somewhere else, or when the game has moved since
FiveM recorded it.

Give the folder yourself, the one that holds `GTA5.exe` (not the file itself):

- in the interface, click the **Game** check, then **Choose the game folder…**;
- on the command line, add `--game` to any command that reads the game:

  ```powershell
  .\btms.exe verify --game "C:\Program Files (x86)\Steam\steamapps\common\Grand Theft Auto V"
  ```

  Common folders are `…\Steam\steamapps\common\Grand Theft Auto V`,
  `C:\Program Files\Rockstar Games\Grand Theft Auto V` and `C:\Program Files\Epic Games\GTAV`.
  Quote a path that holds spaces.

When BTMS says that a folder is not a GTA V folder, `GTA5.exe` is not directly in it. BTMS only
reads the game folder, and skips a `mods/` folder inside it. The game does not need to run.

## The graphics card is not used

**How to tell.** The log says once who draws: the graphics card, with its name and its API (`Dx12`
or `Vulkan`), or the processor, followed by the reason each card was turned down. During and after
a render, the interface shows the same beside the gauges (**Drawing: graphics card \<name\>
(Dx12)**, or **processor**). `btms tiles --dry-run` opens the card the same way, without reading
the game.

| Situation | Why | What to do |
|---|---|---|
| No card is found at all | wgpu sees no DirectX 12 or Vulkan adapter | install the current driver from your card's maker (NVIDIA, AMD or Intel) |
| Only "Microsoft Basic Render Driver" is found | it is the Windows software renderer, which `auto` turns down: it runs on the processor, more slowly than BTMS's own processor renderer | the card's driver is usually missing: install it |
| A card is turned down because it cannot blend 32-bit float colours | BTMS needs that feature to draw on the card | update the driver; a card that still lacks it cannot be used |
| The card fails to open | the driver refused to create the device | update the driver, then try the other API (below) |
| The integrated card draws instead of the discrete one | `auto` takes a discrete card before an integrated one, and DirectX 12 before Vulkan, so the discrete card was turned down (the log says why) | fix that reason, or name the card with `BTMS_GPU` (below) |
| The processor draws although a card is there | **Who draws** is set to **Processor**, or the render was started with `--renderer cpu` | set **Who draws** to **Auto** |

**Choosing the card or the API.** Two environment variables, set in the same PowerShell window
before the command:

```powershell
# wgpu's own variable: Vulkan adapters only ("dx12" for DirectX 12 only)
$env:WGPU_BACKEND = "vulkan"
.\btms.exe tiles --dry-run
```

```powershell
# the adapter whose API and name contain every one of these words, for example "vulkan" or "dx12 radeon"
$env:BTMS_GPU = "dx12 radeon"
.\btms.exe tiles --dry-run
```

When `BTMS_GPU` matches no adapter, the reason given lists every adapter found with its API: a
quick way to see their names. `--renderer gpu` also accepts a software adapter when there is
nothing else, and `--renderer cpu` never opens a card.

**Is the processor a problem?** Only for speed. The processor renderer is the reference, and the
card matches it within 2/255 on 99.9 % of pixels.

## An amber banner: the card left work to the processor

The banner says how many blocks the processor drew, shaded or finished in the card's place, and
why. The render goes on, and those blocks are as good: the processor draws the reference picture,
which the card matches within a small tolerance. The same count is printed at the end of the
render.

| Reason | What BTMS does | What to do |
|---|---|---|
| The card refused memory | only that picture goes to the processor; the texture budget drops to three quarters of what it held, and the next pictures are the card's again. The card is given up only when it cannot keep even a minimum of textures | lower **Graphics card memory (textures)** (`--vram <GB>`), use fewer threads, close other programs that use the card |
| The card failed, or the device was lost | the rest of the run is drawn by the processor | update the driver; if it happens again, set **Who draws** to **Processor** |
| A block failed in the chain on the card | that block is made again whole on the processor, and the chain goes on | nothing, unless it happens often |
| The chain failed, was lost, or did not answer within 60 s | the chain is given up; the rest of the run goes on without it | `--chain cpu` turns the chain off while the card still draws and shades |

On the card, a render needs more than its texture budget: about 0.2 GB per drawing thread for its
pictures, and about 0.6 GB for the shading. `--shading cpu` (**Who shades: Processor**) leaves the
shading to the processor. The reasons in the banner and the log are in English in every language
of the interface.

See also [interface.md](interface.md#when-the-graphics-card-falls-back-to-the-processor).

## Out of memory

**RAM.** Two things use most of it:

- the models and textures already loaded, shared by all threads: 6 GB at most by default. Beyond
  that they are dropped and loaded again when needed. Set it with **RAM for the models** or
  `--ram <GB>` (decimals allowed, such as `3.5`). The search for holes in the ground keeps 2 GB at
  most;
- each drawing thread's own pictures and buffers, and the queue of pictures waiting for the writer
  threads (two per writer thread).

The second part grows with the number of threads: 10 drawing threads can peak at about 8 to 10 GB
in all. If Windows runs short of memory or the render stops with an allocation error:

1. use fewer threads: **Worker threads**, or `--threads <n>`;
2. then lower the model memory;
3. or choose the **Quiet** preset (half the cores, low priority).

**Graphics card memory.** Textures take up to 4 GB by default (`--vram <GB>`), plus the pictures of
the threads and the shading (see the section above). When the card refuses memory, BTMS lowers the
budget by itself and goes on. The **Graphics card memory (textures)** slider of the interface stops at what the card
can hold beside the rest, less 3 GB for Windows and other programs.

## Black or grey patches

Three things make a dark or grey patch:

- **Plain mid grey.** A piece whose texture cannot be found is drawn plain mid grey. A texture is
  looked for in the model, then in the archetype's texture dictionary and its parents, then by
  name in the game's shared dictionaries. With a server resource, the usual cause is a texture
  dictionary that lives in another resource you did not add to `[resources] folders` (a texture
  pack the map depends on), or one that is protected. Add the missing resource.
- **Near black.** A few models have texture coordinates that are not numbers. Both renderers draw
  those pixels black, which leaves small dark patches a few metres wide. This is a known limit.
  Some textures are also really dark: bushes, for example.
- **Dark because of the lighting.** A full shadow keeps `1 − 0.75 × strength` of the colour. Lower
  `[lighting] strength`, raise the sun (**High sun** preset), or set `shadows = false`. Props of a
  protected map standing over the seabed also cast long shadows on it.

**Find out what a block is made of** with `btms probe`:

```powershell
.\btms.exe probe 1 -8                           # block (1, -8): x 128 to 256, y -1024 to -896
.\btms.exe probe 1 -8 --image block.png         # also writes the block at 0.25 m per pixel
.\btms.exe probe 1 -8 --renderer cpu            # drawn by the processor instead of the card
```

The block numbers are `bx = floor(x / 128)` and `by = floor(y / 128)`; the interface shows them
under **block** for the point under the pointer. The probe prints:

1. the models placed in the block and drawn, by placement file;
2. what is not drawn, and why (detail level, interior, removed by a resource, unknown archetype,
   file replaced by a DLC pack, on-demand map left out, model missing, unreadable or protected),
   then the pieces with no texture (the plain grey ones);
3. the block drawn in memory, before lighting, hole filling and water, with its empty, near-black
   (channels summing to less than 24) and plain grey pixels;
4. the empty regions, and what would cover them;
5. each model drawn alone: its area seen from above, its mean colour, its share of dull or
   near-black pixels, and for the suspicious ones the main texture of each piece and where it was
   found;
6. the near-black and grey pixels attributed to models, with the cause for each suspicious piece:
   texture not found (and where it was looked for), present but unreadable (its format), a terrain
   layer or palette without texture, coordinates that are not numbers.

It writes nothing but the `--image` you ask for (no lighting, empty pixels transparent). It takes
about ten seconds when the game files are already in the system's cache, about a minute
otherwise. It does not run the search for holes in the ground (that can draw hundreds of blocks):
it only says which left-out on-demand maps touch the block.

## Holes and transparent areas

Small cracks between two pieces of geometry are always filled when the tiles are written. Larger
empty areas have a cause:

| What you see | Why | What to do |
|---|---|---|
| A hole in the ground where a mission changes a place | the ground is in an on-demand map that is not kept | keep `[maps] fill_holes = true` (the default), which finds such holes and keeps the map that closes them; `btms groups` says what was kept and why |
| A yacht, the carrier, a building state or an interior is missing | on-demand maps are left out unless listed | add the map to `[maps] keep` ([configuration.md](configuration.md#keep)) |
| An opening onto an interior, a basin without its water | no placement file fills it | the last pass of `fill_holes` colours such regions from their rim, up to 2,000 m²; a larger one is left and reported. `btms mend <folder>` runs that pass again on finished tiles |
| A resource's land is missing, or only its props show | its models are protected | see [the next section](#protected-resources-are-reported) |
| Something a resource removes is gone | entities moved below −2,000 m, or files removed with `RemoveIpl("name")`, are not drawn, as in the game | intended |
| North Yankton is not there | `[maps] exclude_folders` leaves it out by default | remove the entry if you want it |
| The sea is empty | `[water] enabled = false` draws no water, and nothing beyond the seabed | turn water on |
| A pocket of nothing ringed by land | an area with no ground at all is not taken for a pond | find what should be there with `btms probe` |
| Transparent around a zone at far zooms | a zone render draws only its blocks | normal; render the whole map |

The log reports the holes that no file closes, and the regions too large to fill. The full rules
are in [configuration.md](configuration.md#fill_holes).

## Protected resources are reported

BTMS never decrypts, unscrambles or works around a protection. Files encrypted by FiveM's asset
escrow (`FXAP` header), and models whose vertices are scrambled, are left out and reported. The
game's model of the same name is drawn instead when there is one. This is deliberate, and a
request to read escrowed files will not be accepted.

Where they are reported:

- in the lines printed when the map loads, and in a count at the end of the render;
- by `btms resources` (**Tools → Server resources inventory**), which lists every file left out
  with the reason; `btms resources --all` also reads every model and texture dictionary and counts
  the protected ones per resource;
- in `resources.json`, which marks a resource whose map files could not all be read.

On the tiles, a map whose terrain and buildings are protected shows as its unprotected props over
the game's own ground or seabed, or over nothing. Your options: leave it as it is, take the
resource out of `[resources] folders`, or ask its author for unprotected map files (models that a
manifest lists under `escrow_ignore` read normally). Details are in
[configuration.md](configuration.md#protected-resources-escrow-fxap-scrambled-models).

## An update is refused

`btms update` redraws part of a finished map in its own folder. It refuses rather than leave a seam:

| Refusal | Why | What to do |
|---|---|---|
| The map is not finished | its `tiles.json` does not say `"complete": true`: a render still writes it, or one stopped before its end | wait for it, or render the whole map again into the same folder |
| No record of the resources (`--changes` and `--list` only; `--zone` does not need it) | there is no `resources.json`: the map was drawn with `--zone`, or the render had failed blocks | if the map is up to date, record its state with `btms update <folder> --record` (draws nothing); otherwise render the whole map again |
| The map settings changed | `[maps]`, `[lighting]` or `[water]` differ from what the map was drawn with, so `--changes` is refused | render the whole map again; `--zone` only reports the difference, and draws |
| The zone is outside the map | the map was itself rendered with a `--zone`, and yours does not meet it | check the coordinates (x first, then y) |
| An unknown option, or `--zone=…` | an option and its value are two arguments | write `--zone x1,y1,x2,y2` |
| The folder is being written | another job holds its lock | see the next section |

An update always keeps the map's own tile format: a `--format` that differs is ignored, and the
log says so. `btms update <folder> --list` says what `--changes` would redraw, without drawing
anything.

The same "one format per folder" rule makes `btms tiles --zone` refuse a folder that holds tiles of
another format, before it reads anything: render the zone into another folder, in the folder's
format, or `btms convert` the folder to WebP first.

## A folder is being written by another job

`btms tiles` and `btms update` hold a lock file, `write.lock`, in the folder they write. It records
the process number, the program, the command and the start time. A second job in the same folder
is refused, with the command and process that hold it.

**A crash does not leave you locked out.** A lock whose process no longer runs, or whose process
number now belongs to another program, is stale: the next job takes it over and says so in the
log. So when a job is refused, a BTMS process really is writing the folder:

- a render started from the interface keeps running when you close its window. Start BTMS again
  (Start menu, or `btms-gui.exe`) to get the window back, then wait for it or click **Stop**;
- another terminal may run a render into the same folder.

Look for `btms.exe` or `btms-gui.exe` in the Task Manager (the lock's `program` field names the
one that holds it). Delete the lock file by hand only when neither of them runs at all.

**After a crash or a stop**, the folder holds the tiles written so far, `tiles.json` says
`"complete": false`, and a tile that was being written may have left a harmless `.tmp` file. A
stopped render does not resume: run it again into the same folder. Until then, `btms update`
refuses that folder.

**The interface's own lock** keeps a single interface running per user. Windows releases it when
the process ends, however it ends, so it is never stale. If BTMS says that another interface is
open but does not give its address, one is still starting or still running: wait a few seconds
and try again, or end its `btms.exe` or `btms-gui.exe` in the Task Manager.

## The interface does not open

- **See what BTMS says.** The **BTMS** shortcut and `btms-gui.exe` show no console window, so
  when no window appears, run `btms interface` in a terminal (see
  [Before anything else](#before-anything-else)). It prints the address it serves,
  `http://127.0.0.1:8791/` or the next free port, what window it opened, or the error that stopped
  it. You can always open that address in any browser yourself.
- **The window.** BTMS opens Microsoft Edge in app mode when it finds `msedge.exe` under
  `Program Files (x86)`, `Program Files` or `%LOCALAPPDATA%` (in `Microsoft\Edge\Application`),
  otherwise your default browser. If neither opens, `btms interface` says so in the terminal.
  `--no-window` opens no window at all.
- **The port.** BTMS tries port 8791, then up to 20 ports after it. If all of them are taken, it
  stops with an error: choose another one with `btms interface --port <n>`.
- **Use the address as printed.** The server listens on `127.0.0.1` only, and answers only when
  the address is `127.0.0.1:<port>` or `localhost:<port>`. The PC's network name, its LAN address
  or `[::1]` are refused (403), and other machines cannot reach it. This is on purpose (see
  [interface.md](interface.md#security)).
- **A second launch** does not start a second server: it opens a window on the running one (in a
  terminal, `btms interface` also prints its address).
- **"Connection lost."** The program stopped: **Quit** was clicked, the terminal that ran
  `btms interface` was closed, or it stopped by itself after five minutes with its window closed
  and no job running. Start BTMS again (Start menu, or `btms-gui.exe`). The page reloads itself
  once to pick up the new session; reload it by hand if it does not.

## Antivirus or SmartScreen warnings

BTMS releases are not code-signed yet. The first time you run a new version, Windows SmartScreen
may say "Windows protected your PC": if you downloaded it from the
[releases page](https://github.com/BuuDevOff/BTMS/releases), choose **More info**, then **Run
anyway**. Antivirus programs sometimes flag new unsigned executables that few people have run yet.

If yours flags BTMS:

1. Download only from the [releases page](https://github.com/BuuDevOff/BTMS/releases).
2. Compare the file you downloaded with its line in the release's `SHA256SUMS.txt`:

   ```powershell
   Get-FileHash ".\<the file you downloaded>" -Algorithm SHA256
   ```

3. Report the false positive to your antivirus vendor.
4. Allow that one file or folder in your antivirus rather than turning the protection off.
5. Or build BTMS yourself from the source code ([CONTRIBUTING.md](../CONTRIBUTING.md#building-from-source)).

For reference, what BTMS does on your machine: it reads the game folder (including `GTA5.exe`, for
the AES key) and never writes to it; it writes to its output folders and to `%LOCALAPPDATA%\BTMS`;
it starts copies of itself to run jobs; it listens on `127.0.0.1` only; and it opens Edge or your
browser on that address. BTMS itself downloads nothing. The installer goes online only for its keys
task, to download `magic.dat` from CodeWalker's GitHub.

## The published map shows nothing

- **Opened from the disk.** A page opened as a file (`file://`) cannot read `tiles.json`. Serve the
  folder over HTTP, as in [getting-started.md](getting-started.md#8-put-the-map-on-a-website).
- **`tiles.json` is not next to `index.html`.** The page says so and uses the default grid. Upload
  the folder as it is, with `tiles.json`, `index.html` and the zoom folders side by side.
- **The tiles are not served as pictures.** Check that `.webp` files are served as `image/webp`.
- **Too many files.** A whole map is about 240,000 files and 10 GB, more than many free page hosts
  allow. An upload that stops partway leaves holes in the map: compare the number of files on the
  server with the folder's.
- **An old browser.** A browser that cannot show WebP gets a message saying so.
- **The render is still running.** The page reads `tiles.json` again every 30 seconds while
  `"complete"` is `false`, and the far zooms fill in as the render goes.

The grid, the manifest and the viewer page are described in [tile-format.md](tile-format.md).

## Reporting a problem

Open an issue on [GitHub](https://github.com/BuuDevOff/BTMS/issues) with:

- the output of `btms --version`, your Windows version and your graphics card;
- what you ran: the command line, or the interface settings (`btms interface --render-line`
  prints the arguments of the render the interface would start);
- the log lines around the error;
- for a defect on the map: its coordinates or block numbers, a screenshot, and the output of
  `btms probe <bx> <by>`.

Never include decryption keys, `magic.dat`, game files or protected resource files. A security
problem goes through a private advisory instead: see [SECURITY.md](../SECURITY.md).
