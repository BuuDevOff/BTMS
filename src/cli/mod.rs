//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The command line of BTMS: the map of GTA V read from the game's own files (never from the
//! running game: nothing attaches to FiveM), to make tiles of it.
//!
//! `btms help` prints the full usage (`USAGE` below). In short:
//!
//! ```text
//! btms interface [--port <n>] [--no-window]       the window: settings, renders, their progress (also without a command)
//! btms verify [--game <folder>]                   the keys, a known text decrypted, the archives listed
//! btms import-keys <magic.dat>                    NG keys from CodeWalker's magic.dat
//! btms ls <path> [--game <folder>]                for example update/update.rpf/x64
//! btms extract <path> <file> [--game <folder>]
//! btms find <pattern> [--game <folder>]           the paths that hold a pattern, 200 at most
//! btms groups                                     the maps loaded on request: kept, left out
//! btms resources [--all] [--detail]               what the server's resources hold, replace and remove
//! btms probe <bx> <by> [--image <file.png>]       what one block is made of
//! btms mend [<folder>]                            the last holes of tiles already drawn, coloured in from around them
//! btms tiles [<folder>] [--zone x1,y1,x2,y2] [--threads auto|<n>] [--renderer auto|gpu|cpu] [--shading auto|gpu|cpu]
//!            [--ram <GB>] [--vram <GB>] [--format webp|jpeg] [--quality <0-100>|lossless] [--webp-effort fast|careful]
//!            [--dry-run] [--profile]   the map as tiles (tiles/options.rs, tiles/format.rs)
//! btms update [<folder>] (--zone x1,y1,x2,y2 | --changes | --list | --record) [options of tiles]
//!            a part of a map already drawn, drawn again in its own folder (update/mod.rs)
//! btms convert <folder> --to <folder> [--quality <0-100>|lossless] [--webp-effort fast|careful] [--threads auto|<n>]
//!            a folder of tiles already drawn written again as WebP into another one (tiles/convert.rs)
//! ```
//!
//! `help` (`-h`, `--help`) and `--version` (`-V`) are answered first, before `map.toml` or the
//! game is read, so that they work without GTA V and with a broken configuration.
//!
//! The light of the pictures seen from above (`[lighting]` of `map.toml`) can be set on the
//! command line: `--lighting on|off`, `--no-lighting`, `--azimuth <deg>`, `--elevation <deg>`,
//! `--strength <0-1>`, `--shadows on|off`, `--ambient-occlusion <0-1>`.
//!
//! The pictures are drawn by the graphics card when there is one, else by the processor
//! (`--renderer auto`, the default; `--renderer gpu`, `--renderer cpu`; or `BTMS_RENDERER`): see
//! `render/gpu`.

pub(crate) mod archive;

use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::render::gpu::{self, chain, shade as gpu_shade};
use crate::render::shade;
use crate::util::profile;
use crate::world::{self, groups, holes, resources};
use crate::{config, game, interface, probe, progress, tiles, update};

/// What `btms help` prints (and a command BTMS does not know).
const USAGE: &str = concat!(
    "BTMS ",
    env!("CARGO_PKG_VERSION"),
    " · Buu's Tiles Map Scanner
The GTA V map as web map tiles, from your own game files and the unprotected map resources of
a FiveM server. The game folder is only read.

Usage: btms [<command>] [<arguments>] [<options>]

With no command, btms starts the interface. An option and its value are two arguments:
--zone x1,y1,x2,y2, not --zone=x1,y1,x2,y2.

Commands:
  interface [--port <n>] [--no-window] [--render-line [x1,y1,x2,y2]]
      The local web interface: settings, renders and their progress (the default command).
      --port: the first port tried (default 8791, then the next 20); --no-window: open no
      app window; --render-line: print the arguments of the render the interface would
      launch with its settings (the whole map, or a zone) as JSON, and start nothing.
  tiles [<folder>] [<tile options>]
      Render the map as tiles into <folder> (default output/tiles): the whole map, or a zone.
  update [<folder>] (--zone x1,y1,x2,y2 | --changes | --list | --record) [<tile options>]
      Draw part of a map already rendered again, in that map's folder (default output/tiles).
      One mode at a time:
        --zone x1,y1,x2,y2  the blocks of the zone, rounded outwards
        --changes           the ground of every resource added, changed or removed since the
                            folder's resources.json was written, then the record written anew
        --list              what --changes would draw, only told: nothing drawn or written
        --record            write resources.json as the resources are now, draw nothing
  convert <folder> --to <folder> [--quality <0-100>|lossless] [--webp-effort fast|careful]
          [--threads auto|<n>]
      Write a folder of tiles already drawn again as WebP into another folder, without
      drawing and never in place (--webp-effort is careful by default here).
  mend [<folder>]
      Colour in the last holes of tiles already drawn from the pixels around them
      (default output/tiles).
  groups
      Report the maps the game loads on demand: what marks them, which are kept, which are
      left out.
  resources [--all] [--detail]
      What the server's resources (the folders of [resources] in map.toml) hold, replace and
      remove, and what could not be read. --all: also read every model and texture dictionary
      they stream; --detail: name every failing file, not three per reason.
  probe <bx> <by> [--image <file.png>]
      What one 128 m block is made of, to understand a fault seen on the map (x from bx*128
      to (bx+1)*128, y the same with by: the numbers the interface shows under \"block\").
      --image: also write the block as drawn to <file.png>.
  verify
      Check the game folder and the keys, and that the archives are read and decrypted.
  import-keys <magic.dat>
      Import the NG keys from CodeWalker's magic.dat: each one checked against its SHA-1,
      then kept in the BTMS data folder.
  ls <path>
      List a folder of the game's archives (for example update/update.rpf/x64).
  extract <path> <file>
      Write one file of the archives to <file>, as the game reads it.
  find <pattern>
      Print the paths of the game's files that contain <pattern> (200 at most).
  help, -h, --help
      Print this help.
  --version, -V
      Print the version.

Tile options (tiles and update):
  --zone x1,y1,x2,y2          Only this rectangle, in metres (game coordinates).
  --threads auto|<n>          Drawing threads, 1 to 256. auto (default): one per logical core;
                              one and a half per core when the graphics card draws without the
                              chain (--chain).
  --ram <GB>                  Models and textures kept in memory by all threads (default 6).
  --vram <GB>                 Textures kept on the graphics card (default: BTMS_GPU_MEMORY,
                              else 4).
  --format webp|jpeg          Tile format (default webp; jpeg: JPEG and PNG tiles).
  --quality <0-100>|lossless  WebP quality (default 90).
  --webp-effort fast|careful  How hard the WebP encoder works (default fast). Not accepted by
                              update, which keeps the map's own.
  --dry-run                   Print the settings and stop, before the game is read.

Global options (every command but interface):
  --game <folder>             The GTA V folder (default: the one FiveM records in CitizenFX.ini).
  --config <file>             The map configuration (default: map.toml in the current folder,
                              else the built-in defaults).
  --renderer auto|gpu|cpu     Who draws the pictures: auto (default) the graphics card when
                              there is one, else the CPU.
  --shading auto|gpu|cpu      Who shades them (the lighting), the same way.
  --chain auto|gpu|cpu        A block kept on the graphics card from its drawing to its zooms
                              (auto: when the card draws and shades).
  --profile                   End a render with a table of where the time went.

Lighting options, for one run (over the [lighting] section of map.toml):
  --lighting on|off           Shade the tiles, or not.
  --no-lighting               The same as --lighting off.
  --azimuth <deg>             Where the sun is, clockwise from north (315: north-west).
  --elevation <deg>           Height of the sun above the horizon, 5 to 90.
  --strength <0-1>            Strength of the slope shading and of the cast shadows.
  --shadows on|off            Cast shadows.
  --ambient-occlusion <0-1>   Darkening at the foot of walls.

Environment variables:
  BTMS_RENDERER, BTMS_SHADING, BTMS_CHAIN
                              auto|gpu|cpu: the defaults of --renderer, --shading and --chain.
  BTMS_GPU=<words>            Pick the graphics adapter by backend and name (\"vulkan\",
                              \"dx12 radeon\"); a software adapter only when named.
  BTMS_GPU_MEMORY=<MB>        Textures kept on the card when --vram is not given, in MB
                              (--vram is in GB); default 4000.
  BTMS_LOAD_THREADS=<n>       Threads that load the world, 1 to 256 (default: one per logical core).
  BTMS_RESOURCE_ORDER=first-wins
                              Of two resources that stream a file of the same name, the first
                              one wins (default: the later one).
  BTMS_PROFILE=1              The same as --profile.
  BTMS_FINGERPRINTS=<file>    Append the SHA-1 of every picture drawn to <file> (a check).
  BTMS_GPU_COMPARE=1|<folder> Draw every picture both ways, on the card and on the CPU,
                              and count the differences; with a folder, the pictures go there.
  BTMS_INTERFACE_DIR=<folder> Serve the interface's page files from <folder> (web/interface)
                              instead of the copies built into btms.
  BTMS_PROGRESS=1             Write the progress lines the interface reads (it sets this on
                              the jobs it launches).
  More variables, for checks and bug reports, are listed in docs/cli.md.

Documentation: https://github.com/BuuDevOff/BTMS/tree/main/docs
"
);

/// The program: the command of the command line, run; when it fails, the error is written and
/// the process ends with status 1.
pub fn main() {
    let done = run();
    // (the total of `BTMS_GPU_COMPARE`, when the pictures were drawn both ways)
    gpu::finish();
    if let Err(e) = done {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `help` and `--version` first: they read nothing (neither map.toml nor the game), so that
    // they work without GTA V and with a broken map.toml
    if args.first().is_some_and(|a| a == "help") || args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{USAGE}");
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--version" || a == "-V") {
        println!("btms {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // an option and its value are two arguments (`--zone x1,y1,x2,y2`). Written `--zone=…` the
    // option would be taken for nothing, and `tiles` would draw the whole map; the same goes for
    // `--config=…`, `--threads=…` and the light's options
    if let Some(a) = args.iter().find(|a| a.starts_with("--") && a.contains('=')) {
        bail!(
            "{a}: write the option and its value as two arguments, separated by a space (for example --zone x1,y1,x2,y2, --threads n)"
        );
    }
    // the interface (and the program without a command): before map.toml and the game folder
    // are read, so that it starts even when they cannot be (its page says what is wrong)
    if args.first().is_none_or(|a| a == "interface") {
        return interface::run(&args);
    }
    // a job the interface launched (BTMS_PROGRESS=1) ends by itself when the interface is gone (but
    // not `tiles --dry-run`, over in a moment: it would be stopped before it says anything when its
    // standard input is not kept open)
    let dry_run = args.first().is_some_and(|a| a == "tiles" || a == "update") && args.iter().any(|a| a == "--dry-run");
    if !dry_run {
        progress::watch_stdin();
    }
    let (game_arg, config_arg) = take_game_and_config(&mut args)?;
    let mut config = config::Config::load(config_arg.as_deref())?;
    // the light of the tiles, from the command line (--lighting, --azimuth, --elevation, --strength, --shadows, --ambient-occlusion)
    shade::overrides(&mut args, &mut config.lighting)?;
    config::init(config);
    // which half draws: the graphics card when there is one (--renderer auto, the default), or as asked
    // (--renderer gpu, --renderer cpu)
    gpu::choose(&mut args)?;
    gpu_shade::choose(&mut args)?;
    // the chain of a block on the card (--chain auto|gpu|cpu): after the two above and the light
    chain::choose(&mut args)?;
    // where the time goes: a table at the end of `tiles`
    if let Some(i) = args.iter().position(|a| a == "--profile") {
        args.remove(i);
        profile::enable();
    }
    // the options of `tiles` (threads, memory, `--dry-run`), checked before the game is looked
    // for: `--dry-run` stops once they are told, without the game folder nor its keys
    let tiles_options = match args.first().map(String::as_str) {
        Some("tiles") => Some(tiles::options::parse(&mut args)?),
        _ => None,
    };
    if let Some(options) = &tiles_options {
        // (read when the card is opened)
        if let Some(bytes) = options.card_budget {
            gpu::set_texture_budget(bytes);
        }
        if options.dry_run {
            return tiles::dry_run(options);
        }
    }
    // `update --dry-run`, like `tiles --dry-run`: without the game folder nor its keys
    if args.first().is_some_and(|a| a == "update") && args.iter().any(|a| a == "--dry-run") {
        let parsed = update::parse(&args[1..])?;
        if let Some(bytes) = parsed.tiles.card_budget {
            gpu::set_texture_budget(bytes);
        }
        return update::dry_run(&parsed);
    }
    // commands on folders of tiles alone: neither the game nor its keys
    if args.first().is_some_and(|a| a == "convert") {
        return tiles::convert::run(&tiles::convert::parse(&args)?);
    }
    let game = game::game_dir(game_arg.as_deref())?;
    match args.first().map(String::as_str) {
        Some("verify") => archive::verify(&game),
        Some("import-keys") => {
            let magic = PathBuf::from(args.get(1).context("import-keys <magic.dat>")?);
            let t = Instant::now();
            let dir = game::import_magic(&game, &magic)?;
            println!(
                "NG keys: 101 keys and 272 tables decoded, each one matching its SHA-1 ({:.1} s), kept in {}",
                t.elapsed().as_secs_f64(),
                dir.display()
            );
            Ok(())
        }
        Some("ls") => {
            let path = args.get(1).context("ls <path>")?;
            archive::ls(&game, path)
        }
        Some("extract") => {
            let path = args.get(1).context("extract <path> <file>")?;
            let out = PathBuf::from(args.get(2).context("extract <path> <file>")?);
            archive::extract(&game, path, &out)
        }
        Some("find") => {
            let pattern = args.get(1).context("find <pattern>")?.to_lowercase();
            archive::find(&game, &pattern)
        }
        Some("probe") => probe::run(&game, &args[1..]),
        Some("update") => {
            // a zone of a map already drawn, drawn again in its own folder (update/mod.rs)
            let keys = game::keys(&game)?;
            update::run(&game, &keys, &args[1..])
        }
        Some("mend") => {
            // the last holes of a folder of tiles already drawn, coloured in from around them
            let out = args.get(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("output/tiles"));
            tiles::mend_folder(&out, 8)
        }
        Some("tiles") => {
            // the whole map (or a zone) as tiles, with the options read above (an option written
            // `--zone=…` would be taken for nothing, and the whole map drawn: it is refused at the
            // top of `run`, for every command)
            let keys = game::keys(&game)?;
            tiles::run(&game, &keys, tiles_options.as_ref().context("options of tiles")?)
        }
        Some("groups") => {
            // the maps loaded on request only: what marks them, those kept, those left out
            let keys = game::keys(&game)?;
            let mut world = world::World::load(&game, &keys, false)?;
            // the files that close a hole of the map are kept too (as `tiles` does)
            let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8).min(12);
            // (minutes on the whole map, before the report: a job the interface follows says so)
            if config::get().maps.fill_holes && !world.groups.left_out.is_empty() {
                progress::log("holes: looking for holes in the ground that an on-demand map closes…");
            }
            holes::fill(&game, &keys, &mut world, None, threads, holes::BUDGET)?;
            print!("{}", groups::report(&world));
            Ok(())
        }
        Some("resources") => resources::resources(
            &game,
            &game::keys(&game)?,
            args.iter().any(|a| a == "--all"),
            args.iter().any(|a| a == "--detail"),
        ),
        // a command BTMS does not know, or options alone: the usage
        _ => {
            print!("{USAGE}");
            Ok(())
        }
    }
}

/// Takes the global `--game <folder>` and `--config <file>` out of the arguments, as every command
/// but `interface` gets them: (the game folder, the map configuration), None when not given.
pub(crate) fn take_game_and_config(args: &mut Vec<String>) -> Result<(Option<PathBuf>, Option<PathBuf>)> {
    let mut game = None;
    if let Some(i) = args.iter().position(|a| a == "--game") {
        game = Some(PathBuf::from(args.get(i + 1).context("--game without a folder")?));
        args.drain(i..i + 2);
    }
    // what the tiles show: map.toml of the current folder, or --config <file>
    let mut config = None;
    if let Some(i) = args.iter().position(|a| a == "--config") {
        config = Some(PathBuf::from(args.get(i + 1).context("--config without a file")?));
        args.drain(i..i + 2);
    }
    Ok((game, config))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The commands [`run`] knows, as the first argument (`help` and `--version` aside).
    const COMMANDS: [&str; 13] = [
        "interface",
        "tiles",
        "update",
        "convert",
        "mend",
        "groups",
        "resources",
        "probe",
        "verify",
        "import-keys",
        "ls",
        "extract",
        "find",
    ];

    #[test]
    fn the_help_names_every_command_and_option() {
        for command in COMMANDS {
            assert!(
                USAGE.contains(&format!("\n  {command} ")) || USAGE.contains(&format!("\n  {command}\n")),
                "{command}"
            );
        }
        for flag in [
            "--game",
            "--config",
            "--profile",
            "--renderer",
            "--shading",
            "--chain",
            "--lighting",
            "--no-lighting",
            "--azimuth",
            "--elevation",
            "--strength",
            "--shadows",
            "--ambient-occlusion",
            "--zone",
            "--threads",
            "--ram",
            "--vram",
            "--format",
            "--quality",
            "--webp-effort",
            "--dry-run",
            "--changes",
            "--list",
            "--record",
            "--to",
            "--port",
            "--no-window",
            "--render-line",
            "--all",
            "--detail",
            "--image",
            "-h, --help",
            "--version, -V",
        ] {
            assert!(USAGE.contains(flag), "{flag}");
        }
        assert!(USAGE.starts_with(concat!("BTMS ", env!("CARGO_PKG_VERSION"), " ")));
        assert!(USAGE.contains("https://github.com/BuuDevOff/BTMS/tree/main/docs"));
        // laid out for a terminal: no line wider than 100 columns
        for line in USAGE.lines() {
            assert!(line.chars().count() <= 100, "too wide: {line}");
        }
    }
}
