//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! A job of the interface as the server follows it: the lines its process writes become the
//! state the page shows (stage, blocks, speed, time left) and the events it replays. Nothing here
//! knows of a process or of the clock: a line comes with the instant it was read, so that the
//! whole of it is tested without launching anything.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::progress::{self, BlockState};

/// Lines of text kept for the page: the last ones.
pub const LOG_LINES: usize = 2000;
/// Characters kept of one line of text.
const LOG_LINE_CHARS: usize = 4000;
/// Events kept besides the text (a whole map is 12 000 of them): beyond, the oldest go.
const EVENTS: usize = 200_000;
/// The speed is that of this many last seconds.
const SPEED_WINDOW: Duration = Duration::from_secs(20);

/// What a job does. The names are those of the page (`kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Kind {
    #[serde(rename = "zone")]
    Zone,
    #[serde(rename = "map")]
    Map,
    #[serde(rename = "verify")]
    Verify,
    #[serde(rename = "groups")]
    Groups,
    #[serde(rename = "resources")]
    Resources,
    #[serde(rename = "import-keys")]
    ImportKeys,
    /// `update <map> --zone … | --changes`: a part of the whole map drawn again in its own
    /// folder.
    #[serde(rename = "update")]
    Update,
    /// `update <map> --list`: what changed in the resources since the map recorded them.
    #[serde(rename = "list-changes")]
    ListChanges,
    /// `update <map> --record`: the resources as they are now, recorded without drawing.
    #[serde(rename = "record")]
    Record,
}

impl Kind {
    pub fn parse(name: &str) -> Option<Kind> {
        Some(match name {
            "zone" => Kind::Zone,
            "map" => Kind::Map,
            "verify" => Kind::Verify,
            "groups" => Kind::Groups,
            "resources" => Kind::Resources,
            "import-keys" => Kind::ImportKeys,
            "update" => Kind::Update,
            "list-changes" => Kind::ListChanges,
            "record" => Kind::Record,
            _ => return None,
        })
    }

    /// The tiles a job of this kind writes, if any.
    pub fn source(self) -> Option<Source> {
        match self {
            Kind::Zone => Some(Source::Preview),
            Kind::Map | Kind::Update => Some(Source::Map),
            _ => None,
        }
    }

    /// Whether a job of this kind draws tiles: it is given the performance settings, the priority
    /// asked, and what it really took is shown.
    pub fn renders(self) -> bool {
        matches!(self, Kind::Zone | Kind::Map | Kind::Update)
    }

    /// Whether it is a job of `update`: it works on the folder of the whole map, which must hold
    /// a map already.
    pub fn updates(self) -> bool {
        matches!(self, Kind::Update | Kind::ListChanges | Kind::Record)
    }
}

/// One of the two folders of tiles the page can show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Source {
    #[serde(rename = "map")]
    Map,
    #[serde(rename = "preview")]
    Preview,
}

impl Source {
    pub fn index(self) -> usize {
        match self {
            Source::Map => 0,
            Source::Preview => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Status {
    #[serde(rename = "running")]
    Running,
    #[serde(rename = "done")]
    Done,
    #[serde(rename = "failed")]
    Failed,
    #[serde(rename = "stopped")]
    Stopped,
}

/// An event as the page gets it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "t")]
pub enum PageEvent {
    #[serde(rename = "plan")]
    Plan { blocks: Vec<[i32; 2]> },
    #[serde(rename = "block")]
    Block {
        x: i32,
        y: i32,
        state: BlockState,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    #[serde(rename = "zooms")]
    LowerZooms,
    #[serde(rename = "log")]
    Log { text: String },
}

/// A job as the page gets it (the state's `job`).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct View {
    pub id: u64,
    pub kind: Kind,
    pub status: Status,
    pub phase: String,
    pub started_ms: u64,
    pub duration_ms: u64,
    pub source: Option<Source>,
    pub zone: Option<[f64; 4]>,
    pub threads: usize,
    pub blocks_total: Option<usize>,
    /// Blocks over, those that failed among them.
    pub blocks_done: usize,
    pub blocks_running: usize,
    pub errors: usize,
    pub speed: Option<f64>,
    pub remaining_s: Option<f64>,
    pub tiles: Tiles,
    pub message: Option<String>,
    /// What the render really took (its `settings` event), None before it came.
    pub settings: Option<RunSettings>,
    /// What the settings asked of a render when it was launched (None for a report).
    pub asked: Option<Asked>,
    /// Work meant for the card that the processor did instead: the last `fallback` event of each
    /// kind, in the order they first came (empty when the card did it all).
    pub fallbacks: Vec<Fallback>,
    /// What changed in the server's resources since the map recorded them: the last `changes`
    /// event of the job (`update`), None before it came or for another job.
    pub changes: Option<Changes>,
}

/// What changed in the resources since the map folder recorded them, as the run said it
/// ([`progress::Event::Changes`]): whether a record exists, each resource added, changed or removed,
/// the ground to draw again (whole blocks) and how many blocks, and what else to know.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Changes {
    pub known: bool,
    pub resources: Vec<progress::ResourceChange>,
    pub zones: Vec<[f32; 4]>,
    pub blocks: usize,
    pub note: Option<String>,
}

/// What the settings asked of a render when it was launched: who draws and who shades (`auto`,
/// `gpu`, `cpu`), whether the light is on. Kept with the job: the settings may change while it
/// runs, and after.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Asked {
    pub renderer: String,
    pub shading: String,
    pub lighting: bool,
}

/// Work meant for the card that the processor did instead, as the run last said it
/// ([`progress::Event::Fallback`]): what (`drawing`, `shading`, `chain`), how many blocks so far, why.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Fallback {
    pub kind: String,
    pub blocks: usize,
    pub reason: String,
}

/// What a render really uses, as it said once before loading the world: worker threads, who
/// draws (`gpu` only when the card opened) and on which card, who shades, the memory budgets.
/// The names are those of the event ([`progress::Event::Settings`]).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunSettings {
    pub threads: usize,
    pub renderer: String,
    pub card: Option<String>,
    pub shading: String,
    pub ram_gb: f64,
    pub vram_gb: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Tiles {
    pub jpg: usize,
    pub png: usize,
    pub empty: usize,
}

pub struct Job {
    pub id: u64,
    pub kind: Kind,
    pub status: Status,
    phase: String,
    /// When it was launched: milliseconds since 1970 for the page, the instant for the durations.
    started_ms: u64,
    started: Instant,
    ended: Option<Instant>,
    zone: Option<[f64; 4]>,
    threads: usize,
    total: Option<usize>,
    done: usize,
    running: usize,
    errors: usize,
    tiles: Tiles,
    message: Option<String>,
    settings: Option<RunSettings>,
    asked: Option<Asked>,
    fallbacks: Vec<Fallback>,
    changes: Option<Changes>,
    /// When the first block began, and when the last one was over.
    blocks_from: Option<Instant>,
    blocks_until: Option<Instant>,
    /// When each of the last blocks was over (those of the speed's window).
    finishes: VecDeque<Instant>,
    /// What the page replays, each with its sequence number: the events, and the text apart
    /// (it alone is cut to its last lines).
    events: VecDeque<(u64, PageEvent)>,
    log: VecDeque<(u64, String)>,
    /// The last line of the error output that looks like the reason of a failure.
    last_error: Option<String>,
    /// The user asked for it to stop.
    pub stop_requested: bool,
}

/// What a line changed besides the job itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Effect {
    /// Tiles were written: the pictures the page shows of this job's folder are stale.
    pub tiles_changed: bool,
}

impl Job {
    pub fn new(id: u64, kind: Kind, zone: Option<[f64; 4]>, threads: usize, started_ms: u64, now: Instant) -> Job {
        Job {
            id,
            kind,
            status: Status::Running,
            phase: "starting".into(),
            started_ms,
            started: now,
            ended: None,
            zone,
            threads,
            total: None,
            done: 0,
            running: 0,
            errors: 0,
            tiles: Tiles::default(),
            message: None,
            settings: None,
            asked: None,
            fallbacks: Vec::new(),
            changes: None,
            blocks_from: None,
            blocks_until: None,
            finishes: VecDeque::new(),
            events: VecDeque::new(),
            log: VecDeque::new(),
            last_error: None,
            stop_requested: false,
        }
    }

    pub fn is_running(&self) -> bool {
        self.status == Status::Running
    }

    /// What the settings asked of this render when it was launched.
    pub fn ask(&mut self, asked: Asked) {
        self.asked = Some(asked);
    }

    fn push(&mut self, seq: &mut u64, event: PageEvent) {
        *seq += 1;
        self.events.push_back((*seq, event));
        if self.events.len() > EVENTS {
            self.events.pop_front();
        }
    }

    /// A line of text for the page.
    pub fn say(&mut self, seq: &mut u64, text: &str) {
        *seq += 1;
        let text = if text.chars().count() > LOG_LINE_CHARS {
            text.chars().take(LOG_LINE_CHARS).chain(['…']).collect()
        } else {
            text.to_string()
        };
        self.log.push_back((*seq, text));
        if self.log.len() > LOG_LINES {
            self.log.pop_front();
        }
    }

    /// A line the process wrote, read at `now`: a progress line changes the state, any other is
    /// text. `error_output`: the line comes from the error output.
    pub fn line(&mut self, seq: &mut u64, text: &str, error_output: bool, now: Instant) -> Effect {
        let mut effect = Effect::default();
        let Some(event) = (if error_output { None } else { progress::parse(text) }) else {
            if error_output && text.trim_start().starts_with("error") {
                self.last_error = Some(text.trim().to_string());
            }
            self.say(seq, text);
            return effect;
        };
        match event {
            progress::Event::Phase { name } => {
                if matches!(name.as_str(), "world" | "placements" | "blocks" | "zooms") {
                    if name == "zooms" && self.blocks_from.is_some() {
                        self.blocks_until.get_or_insert(now);
                    }
                    self.phase = name;
                }
            }
            progress::Event::Plan { blocks, threads, .. } => {
                self.total = Some(blocks.len());
                self.threads = threads;
                self.push(seq, PageEvent::Plan { blocks });
            }
            progress::Event::Block { x, y, state, jpg, png, empty, message, .. } => {
                match state {
                    BlockState::Started => {
                        self.blocks_from.get_or_insert(now);
                        self.running += 1;
                    }
                    BlockState::Done | BlockState::Failed => {
                        self.running = self.running.saturating_sub(1);
                        self.done += 1;
                        self.finishes.push_back(now);
                        while self.finishes.front().is_some_and(|t| now.saturating_duration_since(*t) > SPEED_WINDOW) {
                            self.finishes.pop_front();
                        }
                        if state == BlockState::Failed {
                            self.errors += 1;
                        } else {
                            self.tiles.jpg += jpg.unwrap_or(0);
                            self.tiles.png += png.unwrap_or(0);
                            self.tiles.empty += empty.unwrap_or(0);
                            effect.tiles_changed = true;
                        }
                        if self.total.is_some_and(|total| self.done >= total) {
                            self.blocks_until.get_or_insert(now);
                        }
                    }
                }
                self.push(seq, PageEvent::Block { x, y, state, message });
            }
            progress::Event::LowerZooms { .. } => {
                effect.tiles_changed = true;
                self.push(seq, PageEvent::LowerZooms);
            }
            progress::Event::End { .. } => self.phase = "done".into(),
            // work meant for the card that the processor did instead: the last word of each kind
            // (the run says it again with the total so far, then once more at the end)
            progress::Event::Fallback { kind, blocks, reason } => {
                match self.fallbacks.iter_mut().find(|f| f.kind == kind) {
                    Some(kept) => {
                        kept.blocks = blocks;
                        kept.reason = reason;
                    }
                    None => self.fallbacks.push(Fallback { kind, blocks, reason }),
                }
            }
            // what changed in the resources since the map recorded them: the last word is kept,
            // for the page (neither an event it replays nor a line of its log)
            progress::Event::Changes { known, resources, zones, blocks, note } => {
                self.changes = Some(Changes { known, resources, zones, blocks, note });
            }
            // what the run really uses: its threads once `auto` is resolved, and the rest for the page
            progress::Event::Settings { threads, renderer, card, shading, ram_gb, vram_gb } => {
                self.threads = threads;
                self.settings = Some(RunSettings { threads, renderer, card, shading, ram_gb, vram_gb });
            }
        }
        effect
    }

    /// The process is over. `success`: it ended by itself without an error.
    pub fn exited(&mut self, seq: &mut u64, success: bool, code: Option<i32>, now: Instant) {
        self.ended = Some(now);
        self.running = 0;
        if self.blocks_from.is_some() {
            self.blocks_until.get_or_insert(now);
        }
        let seconds = now.saturating_duration_since(self.started).as_secs_f64();
        if success {
            self.status = Status::Done;
            self.phase = "done".into();
            self.say(seq, &format!("job done in {}", duration(seconds)));
        } else if self.stop_requested {
            self.status = Status::Stopped;
            self.say(seq, &format!("job stopped after {}", duration(seconds)));
        } else {
            self.status = Status::Failed;
            let reason = self.last_error.clone().unwrap_or_else(|| match code {
                Some(code) => format!("the program exited with code {code}"),
                None => "the program exited".into(),
            });
            self.say(seq, &format!("job failed after {}", duration(seconds)));
            self.message = Some(reason);
        }
    }

    /// Blocks per second: over the last 20 seconds while it runs, over all the blocks once it is
    /// over. None when there is nothing to measure yet.
    fn speed(&self, now: Instant) -> Option<f64> {
        let from = self.blocks_from?;
        if self.is_running() && self.blocks_until.is_none() {
            let window = now.saturating_duration_since(from).min(SPEED_WINDOW).as_secs_f64();
            let recent = self.finishes.iter().filter(|t| now.saturating_duration_since(**t) <= SPEED_WINDOW).count();
            (recent > 0 && window >= 0.5).then(|| recent as f64 / window)
        } else {
            let seconds = self.blocks_until?.saturating_duration_since(from).as_secs_f64();
            (self.done > 0 && seconds > 0.0).then(|| self.done as f64 / seconds)
        }
    }

    pub fn view(&self, now: Instant) -> View {
        let speed = self.speed(now);
        let left = match (self.is_running(), self.total, speed) {
            (true, Some(total), _) if self.done >= total => Some(0.0),
            (true, Some(total), Some(speed)) if self.blocks_until.is_none() => Some((total - self.done) as f64 / speed),
            _ => None,
        };
        let round = |x: f64, digits: i32| (x * 10f64.powi(digits)).round() / 10f64.powi(digits);
        View {
            id: self.id,
            kind: self.kind,
            status: self.status,
            phase: self.phase.clone(),
            started_ms: self.started_ms,
            duration_ms: self.ended.unwrap_or(now).saturating_duration_since(self.started).as_millis() as u64,
            source: self.kind.source(),
            zone: self.zone,
            threads: self.threads,
            blocks_total: self.total,
            blocks_done: self.done,
            blocks_running: self.running,
            errors: self.errors,
            speed: speed.map(|v| round(v, 2)),
            remaining_s: left.map(|s| round(s, 1)),
            tiles: self.tiles,
            message: self.message.clone(),
            settings: self.settings.clone(),
            asked: self.asked.clone(),
            fallbacks: self.fallbacks.clone(),
            changes: self.changes.clone(),
        }
    }

    /// What the page replays, in order: everything kept (`after` None), or what came after the
    /// cursor it gives back.
    pub fn events_after(&self, after: Option<u64>) -> Vec<PageEvent> {
        let after = after.unwrap_or(0);
        let (mut events, mut log) = (
            self.events.iter().filter(|(n, _)| *n > after).peekable(),
            self.log.iter().filter(|(n, _)| *n > after).peekable(),
        );
        let mut out = Vec::new();
        loop {
            match (events.peek(), log.peek()) {
                (Some((a, _)), Some((b, _))) if a < b => out.push(events.next().unwrap().1.clone()),
                (_, Some(_)) => out.push(PageEvent::Log { text: log.next().unwrap().1.clone() }),
                (Some(_), None) => out.push(events.next().unwrap().1.clone()),
                (None, None) => break,
            }
        }
        out
    }
}

/// A duration in words: "7 s", "3 min 05 s".
fn duration(seconds: f64) -> String {
    let s = seconds.round() as u64;
    if s < 60 { format!("{s} s") } else { format!("{} min {:02} s", s / 60, s % 60) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::{Event, ResourceChange, line};

    fn block(x: i32, y: i32, state: BlockState) -> String {
        let done = state == BlockState::Done;
        line(&Event::Block {
            x,
            y,
            state,
            ms: done.then_some(900),
            jpg: done.then_some(19),
            png: done.then_some(1),
            empty: done.then_some(1),
            message: (state == BlockState::Failed).then(|| "write refused".to_string()),
        })
    }

    #[test]
    fn a_render_is_followed_from_its_lines() {
        let t0 = Instant::now();
        let at = |s: f64| t0 + Duration::from_secs_f64(s);
        let mut seq = 100;
        let mut job = Job::new(7, Kind::Zone, Some([384.0, -1024.0, 640.0, -768.0]), 2, 1_700_000_000_000, t0);
        let v = job.view(at(0.2));
        assert_eq!((v.id, v.status, v.phase.as_str(), v.duration_ms), (7, Status::Running, "starting", 200));
        assert_eq!((v.source, v.blocks_total, v.speed, v.remaining_s), (Some(Source::Preview), None, None, None));

        assert!(!job.line(&mut seq, &line(&Event::Phase { name: "world".into() }), false, at(0.3)).tiles_changed);
        job.line(&mut seq, "world: 159380 archetypes", true, at(3.0));
        job.line(&mut seq, &line(&Event::Phase { name: "placements".into() }), false, at(3.1));
        let plan = Event::Plan {
            blocks: vec![[3, -8], [4, -8], [3, -7], [4, -7]],
            threads: 3,
            folder: "x".into(),
            zone: None,
        };
        job.line(&mut seq, &line(&plan), false, at(4.0));
        job.line(&mut seq, &line(&Event::Phase { name: "blocks".into() }), false, at(4.0));
        let v = job.view(at(4.0));
        assert_eq!(
            (v.phase.as_str(), v.blocks_total, v.blocks_done, v.threads, v.speed),
            ("blocks", Some(4), 0, 3, None)
        );

        job.line(&mut seq, &block(3, -8, BlockState::Started), false, at(4.0));
        job.line(&mut seq, &block(4, -8, BlockState::Started), false, at(4.0));
        assert_eq!(job.view(at(4.5)).blocks_running, 2);
        assert!(job.line(&mut seq, &block(3, -8, BlockState::Done), false, at(5.0)).tiles_changed);
        job.line(&mut seq, &block(3, -7, BlockState::Started), false, at(5.0));
        // a block that fails is over too, and writes no tile
        assert!(!job.line(&mut seq, &block(4, -8, BlockState::Failed), false, at(6.0)).tiles_changed);
        let v = job.view(at(6.0));
        assert_eq!((v.blocks_done, v.blocks_running, v.errors), (2, 1, 1));
        assert_eq!(v.tiles, Tiles { jpg: 19, png: 1, empty: 1 });
        // two blocks in the two seconds since the first began: one a second, two left
        assert_eq!((v.speed, v.remaining_s), (Some(1.0), Some(2.0)));

        job.line(&mut seq, &block(4, -7, BlockState::Started), false, at(6.0));
        job.line(&mut seq, &block(3, -7, BlockState::Done), false, at(7.0));
        job.line(&mut seq, &block(4, -7, BlockState::Done), false, at(8.0));
        let v = job.view(at(8.0));
        assert_eq!((v.blocks_done, v.blocks_running, v.remaining_s), (4, 0, Some(0.0)));
        job.line(&mut seq, &line(&Event::Phase { name: "zooms".into() }), false, at(8.1));
        assert!(job.line(&mut seq, &line(&Event::LowerZooms { blocks: 4 }), false, at(9.0)).tiles_changed);
        job.line(&mut seq, &line(&Event::End { blocks: 4, errors: 1, seconds: 9.0 }), false, at(9.1));
        assert_eq!(job.view(at(9.1)).phase, "done");
        assert!(job.is_running());
        job.exited(&mut seq, true, Some(0), at(9.5));
        let v = job.view(at(60.0));
        assert_eq!((v.status, v.duration_ms, v.message.clone(), v.remaining_s), (Status::Done, 9500, None, None));
        // once over: four blocks in the four seconds the blocks took
        assert_eq!(v.speed, Some(1.0));

        // what the page replays: the plan, the blocks and the text, in the order they came
        let all = job.events_after(None);
        assert_eq!(all.first(), Some(&PageEvent::Log { text: "world: 159380 archetypes".into() }));
        assert_eq!(all[1], PageEvent::Plan { blocks: vec![[3, -8], [4, -8], [3, -7], [4, -7]] });
        assert_eq!(all[2], PageEvent::Block { x: 3, y: -8, state: BlockState::Started, message: None });
        assert!(all.contains(&PageEvent::Block {
            x: 4,
            y: -8,
            state: BlockState::Failed,
            message: Some("write refused".into())
        }));
        assert_eq!(all.iter().filter(|e| matches!(e, PageEvent::Block { .. })).count(), 8);
        assert_eq!(all[all.len() - 2], PageEvent::LowerZooms);
        assert_eq!(all.last(), Some(&PageEvent::Log { text: "job done in 10 s".into() }));
        // and from a cursor: only what came after it (the sequence went on from 100)
        assert_eq!(job.events_after(Some(seq)), []);
        assert_eq!(job.events_after(Some(seq - 2)), all[all.len() - 2..]);
        assert_eq!(job.events_after(Some(100)), all);
        assert_eq!(serde_json::to_string(&all[all.len() - 2]).unwrap(), r#"{"t":"zooms"}"#);
        assert_eq!(serde_json::to_string(&all[2]).unwrap(), r#"{"t":"block","x":3,"y":-8,"state":"started"}"#);
    }

    #[test]
    fn what_a_render_really_took_is_kept() {
        let t0 = Instant::now();
        let mut seq = 0;
        // launched with `--threads auto`: the threads are not known before the run says them
        let mut job = Job::new(1, Kind::Map, None, 0, 0, t0);
        let v = job.view(t0);
        assert_eq!((v.threads, v.settings.clone()), (0, None));
        assert_eq!(serde_json::to_value(&v).unwrap()["settings"], serde_json::Value::Null);
        let said = r#"@@BTMS {"e":"settings","threads":14,"renderer":"gpu","card":"NVIDIA GeForce RTX 4070","shading":"gpu","ram_gb":31.0,"vram_gb":17.5}"#;
        let effect = job.line(&mut seq, said, false, t0);
        assert!(!effect.tiles_changed);
        let v = job.view(t0);
        assert_eq!(v.threads, 14);
        let taken = RunSettings {
            threads: 14,
            renderer: "gpu".into(),
            card: Some("NVIDIA GeForce RTX 4070".into()),
            shading: "gpu".into(),
            ram_gb: 31.0,
            vram_gb: 17.5,
        };
        assert_eq!(v.settings, Some(taken));
        assert_eq!(
            serde_json::to_value(&v).unwrap()["settings"],
            serde_json::json!({"threads": 14, "renderer": "gpu", "card": "NVIDIA GeForce RTX 4070", "shading": "gpu", "ram_gb": 31.0, "vram_gb": 17.5})
        );
        // it is no event of the page, nor a line of its log
        assert_eq!(job.events_after(None), []);
        // the processor, no card: `card` null
        job.line(&mut seq, r#"@@BTMS {"e":"settings","threads":8,"renderer":"cpu","card":null,"shading":"cpu","ram_gb":6.0,"vram_gb":4.0}"#, false, t0);
        let v = job.view(t0);
        assert_eq!(
            (v.threads, v.settings.as_ref().map(|r| (r.renderer.as_str(), r.card.clone()))),
            (8, Some(("cpu", None)))
        );
        // and the plan that comes after does not lose it
        job.line(
            &mut seq,
            &line(&Event::Plan { blocks: vec![[0, 0]], threads: 8, folder: "x".into(), zone: None }),
            false,
            t0,
        );
        assert!(job.view(t0).settings.is_some());
        // still after the end
        job.exited(&mut seq, true, Some(0), t0 + Duration::from_secs(1));
        assert_eq!(job.view(t0 + Duration::from_secs(9)).settings.map(|r| r.threads), Some(8));
    }

    #[test]
    fn the_work_the_card_left_is_kept() {
        let t0 = Instant::now();
        let mut seq = 0;
        let mut job = Job::new(1, Kind::Map, None, 0, 0, t0);
        // nothing left yet: an empty list; nothing asked (a job launched without it)
        let page = serde_json::to_value(job.view(t0)).unwrap();
        assert_eq!((&page["fallbacks"], &page["asked"]), (&serde_json::json!([]), &serde_json::Value::Null));
        job.ask(Asked { renderer: "auto".into(), shading: "gpu".into(), lighting: true });
        // the line of the contract (`progress::fallback`)
        let said =
            line(&Event::Fallback { kind: "drawing".into(), blocks: 1, reason: "the card refused memory".into() });
        assert_eq!(said, r#"@@BTMS {"e":"fallback","kind":"drawing","blocks":1,"reason":"the card refused memory"}"#);
        assert!(!job.line(&mut seq, &said, false, t0).tiles_changed);
        job.line(&mut seq, r#"@@BTMS {"e":"fallback","kind":"shading","blocks":3,"reason":"device lost"}"#, false, t0);
        // the same kind again: its last word (the total so far) replaces the first, in its place
        job.line(
            &mut seq,
            r#"@@BTMS {"e":"fallback","kind":"drawing","blocks":25,"reason":"the card was abandoned"}"#,
            false,
            t0,
        );
        let v = job.view(t0);
        assert_eq!(
            v.fallbacks,
            [
                Fallback { kind: "drawing".into(), blocks: 25, reason: "the card was abandoned".into() },
                Fallback { kind: "shading".into(), blocks: 3, reason: "device lost".into() },
            ]
        );
        let page = serde_json::to_value(&v).unwrap();
        assert_eq!(
            page["fallbacks"],
            serde_json::json!([
                {"kind": "drawing", "blocks": 25, "reason": "the card was abandoned"},
                {"kind": "shading", "blocks": 3, "reason": "device lost"}
            ])
        );
        assert_eq!(page["asked"], serde_json::json!({"renderer": "auto", "shading": "gpu", "lighting": true}));
        // neither an event of the page nor a line of its log
        assert_eq!(job.events_after(None), []);
        // a third kind; all kept after the end
        job.line(&mut seq, &line(&Event::Fallback { kind: "chain".into(), blocks: 2, reason: "x".into() }), false, t0);
        job.exited(&mut seq, true, Some(0), t0 + Duration::from_secs(1));
        let kinds: Vec<(String, usize)> =
            job.view(t0 + Duration::from_secs(5)).fallbacks.into_iter().map(|f| (f.kind, f.blocks)).collect();
        assert_eq!(kinds, [("drawing".to_string(), 25), ("shading".to_string(), 3), ("chain".to_string(), 2)]);
        // on the error output the same text is a line of the log, not a fallback
        let mut other = Job::new(2, Kind::Zone, None, 2, 0, t0);
        other.line(&mut seq, &said, true, t0);
        assert_eq!((other.view(t0).fallbacks.len(), other.events_after(None).len()), (0, 1));
    }

    #[test]
    fn the_speed_is_that_of_the_last_twenty_seconds() {
        let t0 = Instant::now();
        let at = |s: f64| t0 + Duration::from_secs_f64(s);
        let mut seq = 0;
        let mut job = Job::new(1, Kind::Map, None, 10, 0, t0);
        let blocks = (0..1000).map(|i| [i, 0]).collect();
        job.line(&mut seq, &line(&Event::Plan { blocks, threads: 10, folder: "x".into(), zone: None }), false, t0);
        // ten blocks a second for 30 s, then two a second for 30 s
        let mut n = 0;
        for (seconds, rate) in [(30, 10), (30, 2)] {
            for step in 0..seconds * rate {
                // (each a little after the tick, so that none falls on the edge of the window)
                let t = if rate == 10 { step as f64 / 10.0 + 0.05 } else { 30.25 + step as f64 / 2.0 };
                job.line(&mut seq, &block(n, 0, BlockState::Started), false, at(t));
                job.line(&mut seq, &block(n, 0, BlockState::Done), false, at(t));
                n += 1;
                if n == 300 {
                    let v = job.view(at(30.0));
                    assert_eq!(v.speed, Some(10.0));
                    assert_eq!(v.remaining_s, Some(70.0));
                }
            }
        }
        let v = job.view(at(60.0));
        assert_eq!((v.blocks_done, v.speed), (360, Some(2.0)));
        assert_eq!(v.remaining_s, Some(320.0));
        // nothing over for more than 20 s: no speed, no time left
        let v = job.view(at(90.0));
        assert_eq!((v.speed, v.remaining_s), (None, None));
        assert!(job.finishes.len() <= 41);
    }

    #[test]
    fn a_job_that_fails_or_is_stopped_says_so() {
        let t0 = Instant::now();
        let mut seq = 0;
        let mut job = Job::new(1, Kind::Verify, None, 0, 0, t0);
        job.line(&mut seq, "game: D:\\GTA V", false, t0);
        job.line(&mut seq, "error: NG keys missing", true, t0);
        job.exited(&mut seq, false, Some(1), t0 + Duration::from_secs(1));
        let v = job.view(t0 + Duration::from_secs(5));
        assert_eq!((v.status, v.message.as_deref(), v.source), (Status::Failed, Some("error: NG keys missing"), None));

        let mut job = Job::new(2, Kind::Groups, None, 0, 0, t0);
        job.exited(&mut seq, false, None, t0);
        assert_eq!(job.view(t0).message.as_deref(), Some("the program exited"));

        let mut job = Job::new(3, Kind::Zone, None, 2, 0, t0);
        job.line(&mut seq, &block(0, 0, BlockState::Started), false, t0);
        job.stop_requested = true;
        job.exited(&mut seq, false, Some(1), t0 + Duration::from_secs(2));
        let v = job.view(t0 + Duration::from_secs(9));
        assert_eq!((v.status, v.message, v.blocks_running, v.duration_ms), (Status::Stopped, None, 0, 2000));
        // asked to stop, but it had ended well by itself: it is done
        let mut job = Job::new(4, Kind::Zone, None, 2, 0, t0);
        job.stop_requested = true;
        job.exited(&mut seq, true, Some(0), t0);
        assert_eq!(job.view(t0).status, Status::Done);
    }

    #[test]
    fn the_text_kept_is_bounded() {
        let t0 = Instant::now();
        let mut seq = 0;
        let mut job = Job::new(1, Kind::Groups, None, 0, 0, t0);
        for i in 0..LOG_LINES + 500 {
            job.line(&mut seq, &format!("line {i}"), i % 2 == 0, t0);
        }
        // a progress line that cannot be read, and one on the error output, are text
        job.line(&mut seq, "@@BTMS {broken", false, t0);
        job.line(&mut seq, &line(&Event::LowerZooms { blocks: 1 }), true, t0);
        job.line(&mut seq, &"x".repeat(100_000), false, t0);
        let all = job.events_after(None);
        assert_eq!(all.len(), LOG_LINES);
        assert_eq!(all[0], PageEvent::Log { text: "line 503".into() });
        assert_eq!(all[LOG_LINES - 3], PageEvent::Log { text: "@@BTMS {broken".into() });
        let PageEvent::Log { text } = &all[LOG_LINES - 1] else { panic!() };
        assert_eq!(text.chars().count(), LOG_LINE_CHARS + 1);
        assert_eq!(seq as usize, LOG_LINES + 503);
    }

    #[test]
    fn the_changes_of_the_resources_are_kept() {
        let t0 = Instant::now();
        let mut seq = 0;
        let mut job = Job::new(1, Kind::ListChanges, None, 0, 0, t0);
        // nothing said yet: null
        assert_eq!(serde_json::to_value(job.view(t0)).unwrap()["changes"], serde_json::Value::Null);
        // the line of the contract (`progress::changes`)
        let island = ResourceChange {
            name: "[maps]/my-island".into(),
            folder: r"C:\server\resources\[maps]\my-island".into(),
            state: "added".into(),
            zone: Some([-1024.0, 6400.0, 512.0, 7680.0]),
            reason: None,
        };
        let hidden = ResourceChange {
            name: "[mlo]/hospital".into(),
            folder: r"C:\server\resources\[mlo]\hospital".into(),
            state: "changed".into(),
            zone: None,
            reason: Some("protected files (FXAP)".into()),
        };
        let said = line(&Event::Changes {
            known: true,
            resources: vec![island.clone(), hidden.clone()],
            zones: vec![[-1024.0, 6400.0, 512.0, 7680.0]],
            blocks: 120,
            note: Some("1 resource whose zone cannot be read".into()),
        });
        assert!(
            said.starts_with(r#"@@BTMS {"e":"changes","known":true,"resources":[{"name":"[maps]/my-island","#),
            "{said}"
        );
        assert!(!job.line(&mut seq, &said, false, t0).tiles_changed);
        let v = job.view(t0);
        let kept = Changes {
            known: true,
            resources: vec![island, hidden],
            zones: vec![[-1024.0, 6400.0, 512.0, 7680.0]],
            blocks: 120,
            note: Some("1 resource whose zone cannot be read".into()),
        };
        assert_eq!(v.changes, Some(kept));
        // as the page gets it: a zone that cannot be read is null and says why; no `reason` otherwise
        let page = serde_json::to_value(&v).unwrap();
        assert_eq!(
            page["changes"],
            serde_json::json!({
                "known": true,
                "resources": [
                    {"name": "[maps]/my-island", "folder": r"C:\server\resources\[maps]\my-island", "state": "added", "zone": [-1024.0, 6400.0, 512.0, 7680.0]},
                    {"name": "[mlo]/hospital", "folder": r"C:\server\resources\[mlo]\hospital", "state": "changed", "zone": null, "reason": "protected files (FXAP)"}
                ],
                "zones": [[-1024.0, 6400.0, 512.0, 7680.0]],
                "blocks": 120,
                "note": "1 resource whose zone cannot be read"
            })
        );
        // neither an event of the page nor a line of its log
        assert_eq!(job.events_after(None), []);
        // a map folder without a record: nothing to compare, the last word replaces the first
        job.line(&mut seq, r#"@@BTMS {"e":"changes","known":false,"resources":[],"zones":[],"blocks":0}"#, false, t0);
        job.line(&mut seq, "error: no record of the resources: --record first", true, t0);
        job.exited(&mut seq, false, Some(1), t0 + Duration::from_secs(1));
        let v = job.view(t0 + Duration::from_secs(2));
        assert_eq!(v.changes, Some(Changes { known: false, resources: vec![], zones: vec![], blocks: 0, note: None }));
        assert_eq!(serde_json::to_value(&v).unwrap()["changes"]["note"], serde_json::Value::Null);
        // kept after a failure, beside its reason
        assert_eq!(
            (v.status, v.message.as_deref()),
            (Status::Failed, Some("error: no record of the resources: --record first"))
        );
        // an update of the map: its changes, then a render followed as any other, into the map
        let mut update = Job::new(2, Kind::Update, None, 0, 0, t0);
        update.line(
            &mut seq,
            r#"@@BTMS {"e":"changes","known":true,"resources":[],"zones":[[0.0,0.0,256.0,128.0]],"blocks":2}"#,
            false,
            t0,
        );
        update.line(
            &mut seq,
            &line(&Event::Plan {
                blocks: vec![[0, 0], [1, 0]],
                threads: 4,
                folder: "output/tiles".into(),
                zone: Some([0.0, 0.0, 256.0, 128.0]),
            }),
            false,
            t0,
        );
        update.line(&mut seq, &block(0, 0, BlockState::Started), false, t0);
        assert!(update.line(&mut seq, &block(0, 0, BlockState::Done), false, t0).tiles_changed);
        let v = update.view(t0);
        assert_eq!((v.source, v.blocks_total, v.blocks_done), (Some(Source::Map), Some(2), 1));
        assert_eq!(v.changes.map(|c| (c.zones.len(), c.blocks)), Some((1, 2)));
        // on the error output the same line is text
        let mut other = Job::new(3, Kind::Record, None, 0, 0, t0);
        other.line(&mut seq, &said, true, t0);
        assert_eq!((other.view(t0).changes, other.events_after(None).len()), (None, 1));
    }

    #[test]
    fn kinds_are_those_of_the_page() {
        for name in ["zone", "map", "verify", "groups", "resources", "import-keys", "update", "list-changes", "record"]
        {
            let kind = Kind::parse(name).unwrap();
            assert_eq!(serde_json::to_string(&kind).unwrap(), format!("\"{name}\""));
        }
        assert_eq!(Kind::parse("all"), None);
        // a command of the program is not a job kind
        assert_eq!(Kind::parse("tiles"), None);
        assert_eq!(
            (Kind::Zone.source(), Kind::Map.source(), Kind::Verify.source()),
            (Some(Source::Preview), Some(Source::Map), None)
        );
        // an update writes into the whole map; listing and recording write no tile
        assert_eq!(
            (Kind::Update.source(), Kind::ListChanges.source(), Kind::Record.source()),
            (Some(Source::Map), None, None)
        );
        let renders: Vec<bool> = [Kind::Zone, Kind::Map, Kind::Update, Kind::ListChanges, Kind::Record, Kind::Verify]
            .map(Kind::renders)
            .into();
        assert_eq!(renders, [true, true, true, false, false, false]);
        let updates: Vec<bool> =
            [Kind::Zone, Kind::Map, Kind::Update, Kind::ListChanges, Kind::Record, Kind::Resources]
                .map(Kind::updates)
                .into();
        assert_eq!(updates, [false, false, true, true, true, false]);
        assert_eq!(
            serde_json::to_string(&[Status::Running, Status::Done, Status::Failed, Status::Stopped]).unwrap(),
            r#"["running","done","failed","stopped"]"#
        );
    }
}
