//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The options of `tiles`, as the interface writes them (its contract with the engine):
//!
//! ```text
//! btms tiles <folder> [--zone x1,y1,x2,y2] [--threads auto|<n>] [--renderer auto|gpu|cpu]
//!     [--shading auto|gpu|cpu] [--chain auto|gpu|cpu] [--ram <GB>] [--vram <GB>] [--dry-run]
//!     [--format webp|jpeg] [--quality <0-100>|lossless] [--webp-effort fast|careful]
//! ```
//!
//! [`parse`] takes them out of the arguments and checks them (a value refused gets a message that
//! says why); `--renderer`, `--shading` and `--chain` are taken out before, by `render/gpu/mod.rs`,
//! `render/gpu/shade.rs` and `render/gpu/chain.rs`.
//! [`resolve`] then gives what the run really uses ([`Settings`]): the threads `--threads auto`
//! stands for, who draws and on which card, who shades, the budgets. [`Settings::tell`] writes the
//! line `settings: …` and, with `BTMS_PROGRESS=1`, the event `settings`.
//!
//! `--threads auto` ([`threads`]): on the processor alone, one drawing thread per logical core. With
//! the card, half as many again: a thread waiting for its picture leaves its core to another one.
//! The tiles are written by threads of their own (`write.rs`), a quarter as many as those that draw;
//! with the chain on the card, which leaves the processor only the lists to draw and the encoding,
//! the rule is another one (see [`threads`]).

use std::path::PathBuf;

use anyhow::{Result, bail};

use super::Options;

/// Drawing threads at most (`--threads`).
pub const MAX_THREADS: usize = 256;
/// The budget of the models and textures kept by all threads (`--ram`), in GB.
pub const RAM_GB: f64 = 6.0;
/// Budgets above this many GB are refused (a typo, not a machine; the interface's bound too).
const MAX_GB: f64 = 4096.0;

/// `--threads`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Threads {
    /// As many as the machine needs to be kept busy ([`threads`]).
    Auto,
    Fixed(usize),
}

/// Takes the value of every `flag` out of `args` (the last one counts). A flag without a value, or
/// followed by another option, is refused.
fn take(args: &mut Vec<String>, flag: &str, expected: &str) -> Result<Option<String>> {
    let mut value = None;
    while let Some(i) = args.iter().position(|a| a == flag) {
        match args.get(i + 1) {
            Some(v) if !v.starts_with("--") => value = Some(v.clone()),
            _ => bail!("{flag} without a value: expected {expected}"),
        }
        args.drain(i..i + 2);
    }
    Ok(value)
}

/// A number of GB: decimals with a point or a comma (`12.5`, `12,5`), more than 0.
fn gigabytes(flag: &str, value: &str) -> Result<f64> {
    match value.trim().replace(',', ".").parse::<f64>() {
        Ok(gb) if gb.is_finite() && gb > 0.0 && gb <= MAX_GB => Ok(gb),
        _ => bail!("{flag} {value}: expected a number of GB greater than 0 (for example 6 or 12.5), {MAX_GB} at most"),
    }
}

fn bytes(gb: f64) -> usize {
    (gb * 1e9).round() as usize
}

/// Takes the options of `tiles` out of `args` (`tiles` first, `--renderer`, `--shading`, `--game`,
/// `--config`, `--profile` and the lighting options already taken out) and checks them: what is
/// left is the folder, alone. An option the command does not know is refused, rather than its value
/// being taken for the folder.
pub fn parse(args: &mut Vec<String>) -> Result<Options> {
    let zone = match take(args, "--zone", "x1,y1,x2,y2")? {
        None => None,
        Some(v) => {
            let p: Vec<f32> =
                v.split(',').filter_map(|x| x.trim().parse::<f32>().ok()).filter(|x| x.is_finite()).collect();
            if p.len() != 4 || v.split(',').count() != 4 {
                bail!("--zone {v}: expected x1,y1,x2,y2, four numbers in metres separated by commas");
            }
            Some([p[0].min(p[2]), p[1].min(p[3]), p[0].max(p[2]), p[1].max(p[3])])
        }
    };
    let expected = format!("auto or a number of threads from 1 to {MAX_THREADS}");
    let threads = match take(args, "--threads", &expected)?.as_deref() {
        None | Some("auto") => Threads::Auto,
        Some(v) => match v.trim().parse::<usize>() {
            Ok(n) if (1..=MAX_THREADS).contains(&n) => Threads::Fixed(n),
            _ => bail!("--threads {v}: expected {expected}"),
        },
    };
    let ram = match take(args, "--ram", "a number of GB")? {
        Some(v) => gigabytes("--ram", &v)?,
        None => RAM_GB,
    };
    let card = take(args, "--vram", "a number of GB")?.map(|v| gigabytes("--vram", &v)).transpose()?;
    // what the tiles are written as (`format.rs`), and how hard the WebP encoder works
    let format = take(args, "--format", "webp or jpeg")?;
    let quality = take(args, "--quality", "a whole number from 0 to 100 or lossless")?;
    let effort = take(args, "--webp-effort", "fast or careful")?;
    let format = super::format::from_options(format.as_deref(), quality.as_deref())?;
    let effort = match effort {
        None => super::webp::Effort::default(),
        Some(_) if format == super::format::Format::Jpeg => {
            bail!("--webp-effort only applies to WebP tiles: the older JPEG and PNG tiles are written in one way only")
        }
        Some(word) => super::webp::Effort::parse(&word)?,
    };
    let mut dry_run = false;
    while let Some(i) = args.iter().position(|a| a == "--dry-run") {
        args.remove(i);
        dry_run = true;
    }
    if let Some(unknown) = args.iter().skip(1).find(|a| a.starts_with("--")) {
        bail!(
            "tiles: unknown option {unknown} (tiles accepts --zone, --threads, --renderer, --shading, --chain, --ram, --vram, --format, --quality, --webp-effort, --dry-run, --profile, --config, --game and the lighting options)"
        );
    }
    let folders: Vec<&String> = args.iter().skip(1).collect();
    if folders.len() > 1 {
        let all: Vec<String> = folders.iter().map(|f| format!("\"{f}\"")).collect();
        bail!("tiles: one folder expected, not {}", all.join(" and "));
    }
    let out = folders.first().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("output/tiles"));
    Ok(Options { out, zone, threads, ram_budget: bytes(ram), card_budget: card.map(bytes), dry_run, format, effort })
}

/// (drawing threads, writing threads) for `asked`, on a machine of `logical` cores, the card
/// drawing or not. Auto: on the processor alone, one drawing thread per logical core; with the
/// card, one and a half: about a third of a drawing thread's time can be spent waiting for the
/// card (its picture, its heights, its shading), and a thread that waits leaves its core to
/// another one. Writers: a quarter of the drawing threads (encoding and writing the 21 tiles of a
/// block is about a sixth of its drawing on the processor, a quarter once the card shades), at
/// least one, never more than the cores; the queue between them (bounded) makes the drawing wait
/// when the writing lags, which hands their cores to the writers.
///
/// With the chain on the card (`chain`), a block leaves the processor two things: making its lists
/// to draw (reading and decoding the game's files, placing the vertices, gathering them for the
/// card), on a drawing thread, and encoding and writing its 21 tiles, on a writing thread. The
/// drawing thread also waits for the card twice a block (the lowest heights, then the block
/// itself). Measured on blocks drawn in memory (docs/architecture.md, the chain on the card): a
/// drawing thread spends some 60 ms of processor per block (with the files already loaded: 40 to
/// 200 ms more in a run) and waits for the card the rest of its time; encoding and writing the
/// tiles take about 150 ms; with both on 16 threads the processor is full (100 %) and the card
/// nearly (96 %), 24 threads do no better. So: one drawing thread per logical core (their waits
/// leave cores to the writers), and as many writers as drawing threads, never more than the cores
/// (a writer with nothing to write costs nothing); the queue between them keeps the memory bounded.
pub fn threads(asked: Threads, logical: usize, card: bool, chain: bool) -> (usize, usize) {
    let logical = logical.max(1);
    let drawers = match asked {
        Threads::Fixed(n) => n.clamp(1, MAX_THREADS),
        Threads::Auto if chain => logical.min(MAX_THREADS),
        Threads::Auto if card => (logical + logical / 2).min(MAX_THREADS),
        Threads::Auto => logical.min(MAX_THREADS),
    };
    let writers = if chain { drawers } else { drawers.div_ceil(4) };
    (drawers, writers.clamp(1, logical))
}

/// The writing threads for tiles of `format`: those of [`threads`] for the older JPEG and PNG;
/// twice as many for WebP (half the drawing threads, never more than the cores), whose encoding
/// takes 9.3 ms a tile with a render's effort (quality 90, method 0, measured on one thread on
/// blocks drawn in memory) instead of 2.8 for JPEG: about 0.2 s of a core for the 21 tiles of a
/// block. A rule to start from, to be measured on a real render (`--profile`); with the chain on
/// the card the writers are as many as the drawing threads already.
pub fn writers_for(format: super::format::Format, drawers: usize, writers: usize, logical: usize) -> usize {
    match format {
        super::format::Format::Jpeg => writers,
        super::format::Format::Webp(_) => drawers.div_ceil(2).clamp(writers, logical.max(writers)),
    }
}

/// The logical cores of the machine.
pub fn logical() -> usize {
    std::thread::available_parallelism().map(usize::from).unwrap_or(8)
}

/// What a run of `tiles` really uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub drawers: usize,
    pub writers: usize,
    /// `--threads auto`.
    pub auto: bool,
    /// The card that draws (its name), None when the processor does.
    pub card: Option<String>,
    /// The light is on (`[lighting]`), and the card shades (else the processor does).
    pub lit: bool,
    pub card_shades: bool,
    /// The blocks go through the chain on the card (`chain.rs`); else why not, when the card
    /// draws (with the processor drawing, that says it).
    pub chain: bool,
    pub chain_why: Option<String>,
    /// Bytes of models and textures kept by all threads; of textures kept on the card.
    pub ram_budget: usize,
    pub card_budget: usize,
}

/// What the run will use: the card is opened now when it is to draw (it is waited for, if
/// `gpu::warm` is opening it), to know whether it does; so is the one that shades, when the
/// light is on (`gpu::shade::card`).
pub fn resolve(opts: &Options) -> Settings {
    let card = crate::render::gpu::drawing_card().map(str::to_string);
    // the chain first: when it is used, the shading is on the drawing's device (its card is then
    // that of `gpu::shade` too, and no other device is opened)
    let chain = crate::render::gpu::chain::active().is_some();
    let chain_why = match (chain, card.is_some()) {
        (false, true) => crate::render::gpu::chain::why_not().map(str::to_string),
        _ => None,
    };
    let (drawers, writers) = threads(opts.threads, logical(), card.is_some(), chain);
    let writers = writers_for(opts.format, drawers, writers, logical());
    let lit = crate::render::shade::lit();
    let card_shades = lit && crate::render::gpu::shade::card().is_some();
    Settings {
        drawers,
        writers,
        auto: opts.threads == Threads::Auto,
        card,
        lit,
        card_shades,
        chain,
        chain_why,
        ram_budget: opts.ram_budget,
        card_budget: crate::render::gpu::texture_budget(),
    }
}

/// GB as the line writes them: `6`, `12.5`.
fn gb(bytes: usize) -> String {
    let text = format!("{:.2}", bytes as f64 / 1e9);
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}

impl Settings {
    /// The line `settings: …`.
    pub fn line(&self, opts: &Options) -> String {
        let drawing = match &self.card {
            Some(name) => format!("graphics card {name}"),
            None => "processor".to_string(),
        };
        let shading = match (self.lit, self.card_shades) {
            (false, _) => "none (lighting off)",
            (true, true) => "graphics card",
            (true, false) => "processor",
        };
        let chain = match (self.chain, &self.chain_why) {
            (true, _) => "graphics card (8 bits, holes, water, downscaling)".to_string(),
            (false, Some(why)) => format!("processor ({why})"),
            (false, None) => "processor".to_string(),
        };
        let card_memory = if self.card.is_some() {
            format!(", {} GB of textures on the card", gb(self.card_budget))
        } else {
            String::new()
        };
        let zone = match opts.zone {
            Some(z) => format!("zone {},{},{},{}", z[0], z[1], z[2], z[3]),
            None => "the whole map".to_string(),
        };
        format!(
            "settings: {} drawing{} and {} writing threads; drawing: {drawing}; shading: {shading}; chain: {chain}; memory: {} GB of models and textures{card_memory}; tiles: {}{}; {zone} → {}{}",
            self.drawers,
            if self.auto { " (auto)" } else { "" },
            self.writers,
            gb(self.ram_budget),
            opts.format.label(),
            opts.format.effort_label(opts.effort),
            opts.out.display(),
            if opts.dry_run { " (dry run: the world is not read, nothing is drawn)" } else { "" }
        )
    }

    /// Writes the line, and the event `settings` for the interface (with `BTMS_PROGRESS=1`).
    pub fn tell(self, opts: &Options) -> Settings {
        println!("{}", self.line(opts));
        crate::progress::settings(
            self.drawers,
            if self.card.is_some() { "gpu" } else { "cpu" },
            self.card.as_deref(),
            if self.card_shades { "gpu" } else { "cpu" },
            self.ram_budget as f64 / 1e9,
            self.card_budget as f64 / 1e9,
        );
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    fn parsed(list: &[&str]) -> Result<Options> {
        parse(&mut args(list))
    }

    fn refused(list: &[&str]) -> String {
        match parsed(list) {
            Ok(_) => panic!("{list:?} accepted"),
            Err(e) => format!("{e:#}"),
        }
    }

    #[test]
    fn the_options_of_the_contract_are_read() {
        let o = parsed(&[
            "tiles",
            "output/x",
            "--zone",
            "640,-768,384,-1024",
            "--threads",
            "12",
            "--ram",
            "12.5",
            "--vram",
            "3",
            "--dry-run",
        ])
        .unwrap();
        assert_eq!(o.out, PathBuf::from("output/x"));
        assert_eq!(o.zone, Some([384.0, -1024.0, 640.0, -768.0]));
        assert_eq!(o.threads, Threads::Fixed(12));
        assert_eq!((o.ram_budget, o.card_budget, o.dry_run), (12_500_000_000, Some(3_000_000_000), true));
        // the defaults, and the folder anywhere among the options
        let o = parsed(&["tiles"]).unwrap();
        assert_eq!(
            (o.out, o.zone, o.threads, o.ram_budget, o.card_budget, o.dry_run),
            (PathBuf::from("output/tiles"), None, Threads::Auto, 6_000_000_000, None, false)
        );
        let o = parsed(&["tiles", "--threads", "auto", "--ram", "0,5", "elsewhere"]).unwrap();
        assert_eq!((o.out, o.threads, o.ram_budget), (PathBuf::from("elsewhere"), Threads::Auto, 500_000_000));
        // the last one counts
        assert_eq!(parsed(&["tiles", "--threads", "2", "--threads", "256"]).unwrap().threads, Threads::Fixed(256));
    }

    #[test]
    fn the_format_of_the_tiles_is_read() {
        use crate::tiles::format::{DEFAULT_QUALITY, Format};
        use crate::tiles::webp::Quality;
        assert_eq!(parsed(&["tiles"]).unwrap().format, Format::Webp(Quality::Lossy(DEFAULT_QUALITY)));
        assert_eq!(
            parsed(&["tiles", "x", "--quality", "70", "--dry-run"]).unwrap().format,
            Format::Webp(Quality::Lossy(70))
        );
        assert_eq!(
            parsed(&["tiles", "--format", "webp", "--quality", "lossless"]).unwrap().format,
            Format::Webp(Quality::Lossless)
        );
        assert_eq!(parsed(&["tiles", "--format", "jpeg", "x"]).unwrap().format, Format::Jpeg);
        assert_eq!(
            refused(&["tiles", "--format", "png"]),
            "--format png: expected webp (the default) or jpeg (JPEG and PNG, the older format)"
        );
        assert!(
            refused(&["tiles", "--format", "jpeg", "--quality", "90"])
                .starts_with("--quality only applies to --format webp")
        );
        assert!(
            refused(&["tiles", "--quality", "101"]).starts_with("--quality 101: expected a whole number from 0 to 100")
        );
        assert_eq!(
            refused(&["tiles", "--quality"]),
            "--quality without a value: expected a whole number from 0 to 100 or lossless"
        );
        // the effort: fast by default, careful when asked, for WebP only
        use crate::tiles::webp::Effort;
        assert_eq!(parsed(&["tiles"]).unwrap().effort, Effort::Fast);
        assert_eq!(parsed(&["tiles", "--webp-effort", "careful"]).unwrap().effort, Effort::Careful);
        assert_eq!(parsed(&["tiles", "--webp-effort", "fast", "--quality", "lossless"]).unwrap().effort, Effort::Fast);
        assert!(refused(&["tiles", "--webp-effort", "turbo"]).starts_with("--webp-effort turbo: expected fast"));
        assert_eq!(refused(&["tiles", "--webp-effort"]), "--webp-effort without a value: expected fast or careful");
        assert!(
            refused(&["tiles", "--format", "jpeg", "--webp-effort", "careful"])
                .starts_with("--webp-effort only applies to WebP tiles")
        );
        // and the line `settings: …` says it
        let opts = parsed(&["tiles", "--quality", "lossless", "--dry-run"]).unwrap();
        let s = Settings {
            drawers: 1,
            writers: 1,
            auto: false,
            card: None,
            lit: false,
            card_shades: false,
            chain: false,
            chain_why: None,
            ram_budget: 1,
            card_budget: 1,
        };
        assert!(s.line(&opts).contains("; tiles: WebP lossless, fast encoding (level 0); "), "{}", s.line(&opts));
        let opts = parsed(&["tiles", "--webp-effort", "careful"]).unwrap();
        assert!(
            s.line(&opts).contains("; tiles: WebP quality 90 (default), careful encoding (method 4, sharp YUV); "),
            "{}",
            s.line(&opts)
        );
        let opts = parsed(&["tiles", "--format", "jpeg"]).unwrap();
        assert!(s.line(&opts).contains("; tiles: JPEG quality 82 and PNG (the older format); "), "{}", s.line(&opts));
    }

    #[test]
    fn refused_values_say_why() {
        assert_eq!(
            refused(&["tiles", "--threads", "0"]),
            "--threads 0: expected auto or a number of threads from 1 to 256"
        );
        assert_eq!(
            refused(&["tiles", "--threads", "257"]),
            "--threads 257: expected auto or a number of threads from 1 to 256"
        );
        assert_eq!(
            refused(&["tiles", "--threads", "eight"]),
            "--threads eight: expected auto or a number of threads from 1 to 256"
        );
        assert_eq!(
            refused(&["tiles", "--threads"]),
            "--threads without a value: expected auto or a number of threads from 1 to 256"
        );
        assert_eq!(
            refused(&["tiles", "--threads", "--dry-run"]),
            "--threads without a value: expected auto or a number of threads from 1 to 256"
        );
        assert_eq!(
            refused(&["tiles", "--ram", "-1"]),
            "--ram -1: expected a number of GB greater than 0 (for example 6 or 12.5), 4096 at most"
        );
        assert_eq!(parsed(&["tiles", "--ram", "4096", "--vram", "0.25"]).unwrap().card_budget, Some(250_000_000));
        for bad in ["0", "nan", "inf", "5000", "six", ""] {
            assert!(
                refused(&["tiles", "--vram", bad]).starts_with(&format!("--vram {bad}: expected a number of GB")),
                "{bad}"
            );
        }
        assert!(refused(&["tiles", "--zone", "1,2,3"]).starts_with("--zone 1,2,3: expected x1,y1,x2,y2"));
        assert!(refused(&["tiles", "--zone", "1,2,3,nan"]).starts_with("--zone 1,2,3,nan: expected"));
        assert!(refused(&["tiles", "--zone", "1,2,3,4,5"]).starts_with("--zone 1,2,3,4,5: expected"));
        // an option the command does not know, rather than its value taken for the folder
        assert!(refused(&["tiles", "--dry_run", "8"]).starts_with("tiles: unknown option --dry_run"));
        assert_eq!(refused(&["tiles", "output", "8"]), "tiles: one folder expected, not \"output\" and \"8\"");
    }

    #[test]
    fn auto_keeps_the_cores_busy_while_threads_wait_for_the_card() {
        // a machine of 8 cores, 16 logical
        assert_eq!(threads(Threads::Auto, 16, false, false), (16, 4));
        assert_eq!(threads(Threads::Auto, 16, true, false), (24, 6));
        assert_eq!(threads(Threads::Auto, 1, false, false), (1, 1));
        assert_eq!(threads(Threads::Auto, 1, true, false), (1, 1));
        assert_eq!(threads(Threads::Auto, 4, true, false), (6, 2));
        assert_eq!(threads(Threads::Auto, 0, true, false), (1, 1));
        assert_eq!(threads(Threads::Auto, 512, true, false), (256, 64));
        // a number given is kept, card or not; the writers follow it, never beyond the cores
        assert_eq!(threads(Threads::Fixed(10), 16, true, false), (10, 3));
        assert_eq!(threads(Threads::Fixed(1), 16, false, false), (1, 1));
        assert_eq!(threads(Threads::Fixed(256), 16, false, false), (256, 16));
        // the chain on the card: one drawing thread per logical core, as many writers
        assert_eq!(threads(Threads::Auto, 16, true, true), (16, 16));
        assert_eq!(threads(Threads::Auto, 1, true, true), (1, 1));
        assert_eq!(threads(Threads::Auto, 6, true, true), (6, 6));
        assert_eq!(threads(Threads::Fixed(10), 16, true, true), (10, 10));
        assert_eq!(threads(Threads::Fixed(64), 16, true, true), (64, 16));
    }

    #[test]
    fn webp_tiles_get_more_writing_threads() {
        use crate::tiles::format::Format;
        use crate::tiles::webp::Quality;
        let webp = Format::Webp(Quality::Lossy(90));
        // 16 logical cores, with the card (24 drawing threads) and without (16)
        assert_eq!(writers_for(webp, 24, 6, 16), 12);
        assert_eq!(writers_for(webp, 16, 4, 16), 8);
        assert_eq!(writers_for(Format::Jpeg, 24, 6, 16), 6);
        // never more than the cores, never fewer than before
        assert_eq!(writers_for(webp, 256, 64, 16), 64);
        assert_eq!(writers_for(webp, 256, 16, 16), 16);
        assert_eq!(writers_for(webp, 1, 1, 1), 1);
        assert_eq!(writers_for(webp, 3, 1, 16), 2);
    }

    #[test]
    fn the_line_and_the_event_say_what_is_used() {
        let opts = parsed(&["tiles", "output/preview", "--zone", "384,-1024,640,-768", "--dry-run"]).unwrap();
        let mut s = Settings {
            drawers: 24,
            writers: 6,
            auto: true,
            card: Some("Example Graphics Card (Dx12)".into()),
            lit: true,
            card_shades: true,
            chain: true,
            chain_why: None,
            ram_budget: 12_500_000_000,
            card_budget: 4_000_000_000,
        };
        assert_eq!(
            s.line(&opts),
            "settings: 24 drawing (auto) and 6 writing threads; drawing: graphics card Example Graphics Card (Dx12); shading: graphics card; chain: graphics card (8 bits, holes, water, downscaling); memory: 12.5 GB of models and textures, 4 GB of textures on the card; tiles: WebP quality 90 (default), fast encoding (method 0); zone 384,-1024,640,-768 → output/preview (dry run: the world is not read, nothing is drawn)"
        );
        (s.chain, s.chain_why) = (false, Some("--chain cpu".into()));
        assert!(s.line(&opts).contains("; chain: processor (--chain cpu); "), "{}", s.line(&opts));
        (s.card, s.card_shades, s.lit, s.auto, s.drawers, s.writers, s.chain_why) =
            (None, false, false, false, 3, 1, None);
        let opts = parsed(&["tiles"]).unwrap();
        assert_eq!(
            s.line(&opts),
            "settings: 3 drawing and 1 writing threads; drawing: processor; shading: none (lighting off); chain: processor; memory: 12.5 GB of models and textures; tiles: WebP quality 90 (default), fast encoding (method 0); the whole map → output/tiles"
        );
        // the event of the contract
        let event = crate::progress::Event::Settings {
            threads: 24,
            renderer: "gpu".into(),
            card: Some("RTX".into()),
            shading: "cpu".into(),
            ram_gb: 6.0,
            vram_gb: 4.0,
        };
        assert_eq!(
            crate::progress::line(&event),
            r#"@@BTMS {"e":"settings","threads":24,"renderer":"gpu","card":"RTX","shading":"cpu","ram_gb":6.0,"vram_gb":4.0}"#
        );
        let event = crate::progress::Event::Settings {
            threads: 16,
            renderer: "cpu".into(),
            card: None,
            shading: "cpu".into(),
            ram_gb: 12.5,
            vram_gb: 4.0,
        };
        assert_eq!(crate::progress::parse(&crate::progress::line(&event)), Some(event));
        assert_eq!(
            (gb(6_000_000_000), gb(12_500_000_000), gb(4_096_000_000), gb(500_000_000)),
            ("6".into(), "12.5".into(), "4.1".into(), "0.5".into())
        );
    }

    #[test]
    fn the_budget_of_the_run_empties_the_shared_cache() {
        // what the drawing threads call after each block (`Assets::trim`), with the budget read
        let budget = parsed(&["tiles", "--ram", "0.000002"]).unwrap().ram_budget;
        assert_eq!(budget, 2_000);
        let shared = crate::render::shared::Shared::default();
        shared.txd(1, || (None, 1_500));
        shared.trim(budget);
        assert_eq!((shared.bytes(), shared.generation()), (1_500, 0));
        shared.txd(2, || (None, 1_000));
        shared.trim(budget);
        assert_eq!((shared.bytes(), shared.generation()), (0, 1));
    }
}
