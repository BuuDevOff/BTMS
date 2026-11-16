//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! What a job tells the interface that launched it ([`crate::interface`]), on its standard output:
//! lines `@@BTMS <one JSON object>`, one per event. Only with `BTMS_PROGRESS=1`: without it nothing
//! is written and nothing is watched, the program is the command-line one, byte for byte.
//!
//! ```text
//! @@BTMS {"e":"phase","name":"world"}                      world | placements | blocks | zooms
//! @@BTMS {"e":"plan","blocks":[[3,-8],…],"threads":2,"folder":"…","zone":[384.0,-1024.0,640.0,-768.0]}
//! @@BTMS {"e":"block","x":3,"y":-8,"state":"started"}
//! @@BTMS {"e":"block","x":3,"y":-8,"state":"done","ms":812,"jpg":21,"png":0,"empty":0}
//! @@BTMS {"e":"block","x":3,"y":-8,"state":"failed","message":"…"}
//! @@BTMS {"e":"zooms","blocks":4}                          a pass over zooms 6 to 0 just ended
//! @@BTMS {"e":"end","blocks":4,"errors":0,"seconds":7.3}
//! ```
//!
//! With `BTMS_PROGRESS=1` the job also ends by itself when its standard input closes: the interface
//! keeps that pipe open as long as it lives, so a job never outlives it ([`watch_stdin`]).

use std::io::{Read, Write};
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// What a progress line starts with.
pub const PREFIX: &str = "@@BTMS ";

/// One event of a job. The field names are those of the lines.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "e")]
pub enum Event {
    /// A stage begins: `world`, `placements`, `blocks`, `zooms`.
    #[serde(rename = "phase")]
    Phase { name: String },
    /// Once: the blocks, in the order they are drawn.
    #[serde(rename = "plan")]
    Plan { blocks: Vec<[i32; 2]>, threads: usize, folder: String, zone: Option<[f32; 4]> },
    /// A block begins, ends (its time and its tiles) or fails (why).
    #[serde(rename = "block")]
    Block {
        x: i32,
        y: i32,
        state: BlockState,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
        /// The block's full tiles (`.jpg` in the older format, `.webp` without alpha).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        jpg: Option<usize>,
        /// Its partly empty tiles (`.png` in the older format, `.webp` with alpha).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        png: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        empty: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    /// A pass over zooms 6 to 0 just ended, above this many blocks: the far tiles changed.
    #[serde(rename = "zooms")]
    LowerZooms { blocks: usize },
    /// The render is over.
    #[serde(rename = "end")]
    End { blocks: usize, errors: usize, seconds: f64 },
    /// Work meant for the card that the processor did instead (`kind`: "drawing", "shading" or
    /// "chain"), how many blocks so far, and why, in English. Sent when it first happens, then
    /// at most every 10 s while it goes on, and once more at the end with the total.
    #[serde(rename = "fallback")]
    Fallback { kind: String, blocks: usize, reason: String },
    /// `update`, once, before the plan: what changed in the server's resources since the
    /// map folder last recorded them. `known`: a record exists (false for a map made before
    /// records: nothing can be compared, `resources` and `zones` are empty). `resources`: each
    /// resource added, changed or removed. `zones`: the ground to draw again ([x1, y1, x2, y2] in
    /// metres, whole 128 m blocks), `blocks`: how many blocks that is. `note`: what else the
    /// user must know, in English (the settings of the map changed, a resource whose ground cannot
    /// be read…), or None.
    #[serde(rename = "changes")]
    Changes {
        known: bool,
        resources: Vec<ResourceChange>,
        zones: Vec<[f32; 4]>,
        blocks: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// What the run really uses, once, before the world is loaded: worker threads, who draws
    /// (`gpu` or `cpu`) and on which card, who shades, the memory budgets in GB.
    #[serde(rename = "settings")]
    Settings { threads: usize, renderer: String, card: Option<String>, shading: String, ram_gb: f64, vram_gb: f64 },
}

/// One resource of [`Event::Changes`]. `name`: its folder under the resource folders of the
/// settings (`[my-island]/my-island-map`); `folder`: its full path; `state`: `added`,
/// `changed` or `removed`; `zone`: the ground its placements cover ([x1, y1, x2, y2] in metres;
/// for a removed one, the ground recorded), None when it cannot be read (`reason`, in English).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceChange {
    pub name: String,
    pub folder: String,
    pub state: String,
    pub zone: Option<[f32; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockState {
    #[serde(rename = "started")]
    Started,
    #[serde(rename = "done")]
    Done,
    #[serde(rename = "failed")]
    Failed,
}

/// Whether this run reports its progress (`BTMS_PROGRESS=1`, read once).
pub fn active() -> bool {
    static ACTIVE: OnceLock<bool> = OnceLock::new();
    *ACTIVE.get_or_init(|| std::env::var("BTMS_PROGRESS").is_ok_and(|v| v == "1"))
}

/// The line of an event, without its end of line.
pub fn line(event: &Event) -> String {
    format!("{PREFIX}{}", serde_json::to_string(event).unwrap_or_else(|_| "{}".into()))
}

/// The event of a line of standard output, None when the line is not a progress line.
pub fn parse(line: &str) -> Option<Event> {
    serde_json::from_str(line.strip_prefix(PREFIX)?.trim()).ok()
}

/// Writes one event: the whole line at once, under the lock of the standard output, so that the
/// lines of two threads never mix. Nothing without `BTMS_PROGRESS=1`.
pub fn emit(event: &Event) {
    if !active() {
        return;
    }
    let text = format!("{}\n", line(event));
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// A plain line of text for the interface's log (not an event): what a long step without events
/// is doing. Written whole, like the events. Nothing without `BTMS_PROGRESS=1`: the command-line
/// program writes what it always wrote.
pub fn log(text: &str) {
    if !active() {
        return;
    }
    let text = format!(
        "{text}
"
    );
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

pub fn phase(name: &str) {
    if active() {
        emit(&Event::Phase { name: name.to_string() });
    }
}

pub fn plan(order: &[(i32, i32)], threads: usize, out: &Path, zone: Option<[f32; 4]>) {
    if active() {
        let blocks = order.iter().map(|&(x, y)| [x, y]).collect();
        emit(&Event::Plan { blocks, threads, folder: out.display().to_string(), zone });
    }
}

pub fn block_started(x: i32, y: i32) {
    if active() {
        emit(&Event::Block {
            x,
            y,
            state: BlockState::Started,
            ms: None,
            jpg: None,
            png: None,
            empty: None,
            message: None,
        });
    }
}

pub fn block_done(x: i32, y: i32, ms: u128, jpg: usize, png: usize, empty: usize) {
    if active() {
        emit(&Event::Block {
            x,
            y,
            state: BlockState::Done,
            ms: Some(ms as u64),
            jpg: Some(jpg),
            png: Some(png),
            empty: Some(empty),
            message: None,
        });
    }
}

pub fn block_failed(x: i32, y: i32, message: &str) {
    if active() {
        emit(&Event::Block {
            x,
            y,
            state: BlockState::Failed,
            ms: None,
            jpg: None,
            png: None,
            empty: None,
            message: Some(message.to_string()),
        });
    }
}

pub fn lower_zooms(blocks: usize) {
    if active() {
        emit(&Event::LowerZooms { blocks });
    }
}

pub fn settings(threads: usize, renderer: &str, card: Option<&str>, shading: &str, ram_gb: f64, vram_gb: f64) {
    if active() {
        emit(&Event::Settings {
            threads,
            renderer: renderer.into(),
            card: card.map(str::to_string),
            shading: shading.into(),
            ram_gb,
            vram_gb,
        });
    }
}

pub fn fallback(kind: &str, blocks: usize, reason: &str) {
    if active() {
        emit(&Event::Fallback { kind: kind.into(), blocks, reason: reason.into() });
    }
}

pub fn changes(known: bool, resources: Vec<ResourceChange>, zones: Vec<[f32; 4]>, blocks: usize, note: Option<String>) {
    if active() {
        emit(&Event::Changes { known, resources, zones, blocks, note });
    }
}

pub fn end(blocks: usize, errors: usize, seconds: f64) {
    if active() {
        emit(&Event::End { blocks, errors, seconds: (seconds * 10.0).round() / 10.0 });
    }
}

/// With `BTMS_PROGRESS=1`: a thread that reads the standard input and ends the process when it
/// closes, that is when the interface that launched this job is gone (it never writes there, it
/// only keeps the pipe open). A tile is written whole or not at all, so stopping here leaves no
/// half tile. Nothing without `BTMS_PROGRESS=1`.
pub fn watch_stdin() {
    if !active() {
        return;
    }
    std::thread::spawn(|| {
        let mut stdin = std::io::stdin().lock();
        let mut buffer = [0u8; 256];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        // (written without `eprintln!`, which panics when the error output is closed too: it is
        // the interface's pipe as well, and the process would then go on to its end)
        let _ = writeln!(std::io::stderr(), "stopped: the interface that launched this job has closed");
        std::process::exit(3);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_those_of_the_contract() {
        let started = Event::Block {
            x: 3,
            y: -8,
            state: BlockState::Started,
            ms: None,
            jpg: None,
            png: None,
            empty: None,
            message: None,
        };
        assert_eq!(line(&started), r#"@@BTMS {"e":"block","x":3,"y":-8,"state":"started"}"#);
        let done = Event::Block {
            x: 3,
            y: -8,
            state: BlockState::Done,
            ms: Some(812),
            jpg: Some(20),
            png: Some(1),
            empty: Some(0),
            message: None,
        };
        assert_eq!(
            line(&done),
            r#"@@BTMS {"e":"block","x":3,"y":-8,"state":"done","ms":812,"jpg":20,"png":1,"empty":0}"#
        );
        let failed = Event::Block {
            x: 0,
            y: 0,
            state: BlockState::Failed,
            ms: None,
            jpg: None,
            png: None,
            empty: None,
            message: Some("write\nrefused".into()),
        };
        assert_eq!(line(&failed), r#"@@BTMS {"e":"block","x":0,"y":0,"state":"failed","message":"write\nrefused"}"#);
        assert_eq!(line(&Event::Phase { name: "world".into() }), r#"@@BTMS {"e":"phase","name":"world"}"#);
        assert_eq!(line(&Event::LowerZooms { blocks: 4 }), r#"@@BTMS {"e":"zooms","blocks":4}"#);
        assert_eq!(
            line(&Event::End { blocks: 4, errors: 0, seconds: 7.3 }),
            r#"@@BTMS {"e":"end","blocks":4,"errors":0,"seconds":7.3}"#
        );
        let plan = Event::Plan { blocks: vec![[3, -8], [4, -8]], threads: 2, folder: "C:\\a b".into(), zone: None };
        assert_eq!(
            line(&plan),
            r#"@@BTMS {"e":"plan","blocks":[[3,-8],[4,-8]],"threads":2,"folder":"C:\\a b","zone":null}"#
        );
    }

    #[test]
    fn every_line_reads_back() {
        let events = [
            Event::Phase { name: "placements".into() },
            Event::Plan {
                blocks: vec![[-32, 61], [0, 0]],
                threads: 10,
                folder: "output/tiles".into(),
                zone: Some([384.0, -1024.0, 640.0, -768.0]),
            },
            Event::Block {
                x: -1,
                y: 2,
                state: BlockState::Started,
                ms: None,
                jpg: None,
                png: None,
                empty: None,
                message: None,
            },
            Event::Block {
                x: -1,
                y: 2,
                state: BlockState::Done,
                ms: Some(5),
                jpg: Some(1),
                png: Some(2),
                empty: Some(18),
                message: None,
            },
            Event::Block {
                x: -1,
                y: 2,
                state: BlockState::Failed,
                ms: None,
                jpg: None,
                png: None,
                empty: None,
                message: Some("\"no\" — 2 m²".into()),
            },
            Event::LowerZooms { blocks: 6097 },
            Event::End { blocks: 6097, errors: 3, seconds: 612.5 },
        ];
        for event in events {
            let text = line(&event);
            assert!(!text.contains('\n'), "{text}");
            assert_eq!(parse(&text), Some(event));
        }
    }

    #[test]
    fn other_lines_are_not_events() {
        assert_eq!(parse("zooms 9 to 7: 4 blocks in 3 s"), None);
        assert_eq!(parse("@@BTMS"), None);
        assert_eq!(parse("@@BTMS not JSON"), None);
        assert_eq!(parse(r#"@@BTMS {"e":"unknown"}"#), None);
        assert_eq!(parse(r#" @@BTMS {"e":"zooms","blocks":1}"#), None);
        // the end of line a reader may leave
        assert_eq!(parse("@@BTMS {\"e\":\"zooms\",\"blocks\":1}\r"), Some(Event::LowerZooms { blocks: 1 }));
    }
}
