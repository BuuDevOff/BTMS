//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The interface (`btms interface`, and the program without a command): a small HTTP server on
//! 127.0.0.1 that serves a page held in the executable, opened in a window of its own (Edge as an
//! application), and runs each job (a render, a report) as a child process of this same
//! executable. A child, because the configuration of a run is set once ([`crate::config`]): a
//! second render in this process would keep the settings of the first.
//!
//! ```text
//! btms interface [--port <n>] [--no-window]
//! btms interface --render-line [x1,y1,x2,y2]
//! ```
//!
//! `--render-line` starts no server: it prints the arguments of the render the interface would
//! launch with its settings (a zone, or the whole map), as a JSON array, writes the derived
//! configuration they name, and ends. Nothing is rendered: to check a command line by hand.
//!
//! What the page may ask (JSON; an error is a 4xx or 5xx status and
//! `{"error": "…", "code": "…", "params": {…}}`, see [`Message`]):
//!
//! ```text
//! GET  /                        the page; /css/style.css, /js/app.js, /js/map.js, /js/demo.js, /js/fallback.js, /js/i18n.js, /icon.png
//! GET  /locales/<code>.json     the page's dictionary of a language: en, fr, de, nl, ru, sv, es
//! GET  /tiles/<source>/<z>/<tx>/<ty>   a tile of `map` or `preview`
//! GET  /api/state?since=<n>     the whole state, and the events after the cursor <n>
//! POST /api/settings            a partial set of settings, merged and kept → the state
//! POST /api/choose              a native folder or file dialog → {"path": …}
//! POST /api/launch              a job → {"job_id": id}; 409 when one runs (see [`order`])
//! POST /api/stop                stops the job that runs
//! POST /api/open                a folder in the file explorer, map.toml in its editor
//! POST /api/quit                stops the job, then the server
//! ```
//!
//! Who may ask: the server listens on 127.0.0.1 only; the `Host` header must be this server's
//! (a page of another site that points a name at 127.0.0.1 is refused); every `/api/` route wants
//! the token drawn at startup, which only the page served here is given. Nothing the page sends
//! goes through a command interpreter: a job is this executable with separate arguments, a folder
//! is opened by the file explorer given its path.
//!
//! `BTMS_INTERFACE_DIR=<folder>` serves the page's files from a folder instead of the copies
//! held in the executable (to work on the page without building again): `web/interface` of the
//! repository, whose files have the same paths as the routes. The icon is not in that folder: it
//! is the repository's logo, `assets/icon/btms.png`, read two folders up ([`ICON_FROM_PAGE_DIR`]).
//!
//! One interface at a time for one data folder ([`instance`]): started while another runs, the
//! command shows the page of that one and ends, instead of a second server that would not see
//! the job of the first.
//!
//! The state also holds the machine ([`machine`]: cores, memory, graphics cards), read once on a
//! thread of its own when the server starts, and its load, read every second by another thread
//! whether a job runs or not: a request only takes the last reading, it never waits for one.

mod instance;
mod job;
mod machine;
mod settings;
pub(crate) mod sources;

use std::io::{BufRead, BufReader, Read};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::config::Config;
use job::{Job, Kind, Source};
use settings::{Priority, Resolved, Stored, Threads, Update};

/// The port tried first, and how many of the next ones are tried when it is taken.
const DEFAULT_PORT: u16 = 8791;
const PORTS_TRIED: u16 = 20;
/// Threads answering requests: a slow one (a dialog left open) does not hold the others.
const WORKERS: usize = 8;
/// The longest body a request may have.
const BODY_LIMIT: usize = 1 << 20;
/// With a window: the server ends by itself after this long without a request nor a job.
const IDLE_EXIT: Duration = Duration::from_secs(300);
/// How long what is read from the disk for the state (map.toml, the checks, the folders of
/// tiles) is kept before it is read again.
const SNAPSHOT_AGE: Duration = Duration::from_millis(1000);
/// The header that carries the token.
const TOKEN_HEADER: &str = "X-BTMS-Token";
/// What the page's `index.html` holds in place of the token.
const TOKEN_MARK: &str = "{{TOKEN}}";
/// How often the load of the machine is read.
const LOAD_PERIOD: Duration = Duration::from_secs(1);

/// The page: its files, a closed list. Path (the route without its leading `/`, and the file's
/// path in `web/interface`), media type, the copy held in the executable.
const PAGE: [(&str, &str, &[u8]); 15] = [
    ("index.html", "text/html; charset=utf-8", include_bytes!("../../web/interface/index.html")),
    ("css/style.css", "text/css; charset=utf-8", include_bytes!("../../web/interface/css/style.css")),
    ("js/app.js", "text/javascript; charset=utf-8", include_bytes!("../../web/interface/js/app.js")),
    ("js/map.js", "text/javascript; charset=utf-8", include_bytes!("../../web/interface/js/map.js")),
    ("js/demo.js", "text/javascript; charset=utf-8", include_bytes!("../../web/interface/js/demo.js")),
    ("js/fallback.js", "text/javascript; charset=utf-8", include_bytes!("../../web/interface/js/fallback.js")),
    ("js/i18n.js", "text/javascript; charset=utf-8", include_bytes!("../../web/interface/js/i18n.js")),
    (ICON, "image/png", include_bytes!("../../assets/icon/btms.png")),
    ("locales/en.json", JSON, include_bytes!("../../web/interface/locales/en.json")),
    ("locales/fr.json", JSON, include_bytes!("../../web/interface/locales/fr.json")),
    ("locales/de.json", JSON, include_bytes!("../../web/interface/locales/de.json")),
    ("locales/nl.json", JSON, include_bytes!("../../web/interface/locales/nl.json")),
    ("locales/ru.json", JSON, include_bytes!("../../web/interface/locales/ru.json")),
    ("locales/sv.json", JSON, include_bytes!("../../web/interface/locales/sv.json")),
    ("locales/es.json", JSON, include_bytes!("../../web/interface/locales/es.json")),
];
/// The page's icon: the logo of BTMS, not a file of `web/interface`.
const ICON: &str = "icon.png";
/// Where the icon is read when the page is served from a folder (`BTMS_INTERFACE_DIR`): the
/// logo of the repository, relative to its `web/interface`. When it is not there (a folder
/// elsewhere), the copy held in the executable is served.
const ICON_FROM_PAGE_DIR: &str = "../../assets/icon/btms.png";

const JSON: &str = "application/json; charset=utf-8";

/// A message the page shows: the English text, and a stable code with its parameters, by which
/// the page shows it in its own language instead (the key `server.<code>` of its dictionaries,
/// each `{name}` of which is the parameter of that name). The English template of a code reads
/// exactly as the text sent with it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Message {
    text: String,
    code: String,
    params: Map<String, Value>,
}

impl Message {
    fn new(code: &str, text: impl Into<String>) -> Message {
        Message { text: text.into(), code: code.to_string(), params: Map::new() }
    }

    /// The message with one more parameter.
    fn with(mut self, name: &str, value: impl Into<Value>) -> Message {
        self.params.insert(name.to_string(), value.into());
        self
    }
}

/// What a request is answered.
struct Reply {
    status: u16,
    kind: &'static str,
    body: Vec<u8>,
}

impl Reply {
    fn json(status: u16, value: &impl Serialize) -> Reply {
        match serde_json::to_vec(value) {
            Ok(body) => Reply { status, kind: JSON, body },
            Err(_) => Reply::error(500, Message::new("reply_unwritable", "the reply could not be written")),
        }
    }

    /// An error the page shows by its code: `{"error": <English>, "code": …, "params": {…}}`.
    fn error(status: u16, message: Message) -> Reply {
        let body =
            serde_json::to_vec(&json!({ "error": message.text, "code": message.code, "params": message.params }))
                .unwrap_or_default();
        Reply { status, kind: JSON, body }
    }

    /// An error without a code, `{"error": <English>}`: a message of another module (the
    /// settings), which the page shows as it is.
    fn plain(status: u16, text: impl Into<String>) -> Reply {
        let body = serde_json::to_vec(&json!({ "error": text.into() })).unwrap_or_default();
        Reply { status, kind: JSON, body }
    }
}

/// One of the three checks the page shows: the English `detail`, and its `code` and `params`
/// (see [`Message`]).
#[derive(Clone, Debug, PartialEq, Serialize)]
struct Check {
    ok: bool,
    path: Option<String>,
    detail: String,
    code: Option<String>,
    params: Map<String, Value>,
}

impl Check {
    fn new(ok: bool, path: Option<String>, message: Message) -> Check {
        Check { ok, path, detail: message.text, code: Some(message.code), params: message.params }
    }
}

#[derive(Clone, Serialize)]
struct Checks {
    game: Check,
    keys: Check,
    config: Check,
}

/// What the state needs from the disk, read at most once a second.
#[derive(Clone)]
struct Snapshot {
    at: Instant,
    /// `map.toml` as it is read (the defaults when it is missing or cannot be read).
    base: Config,
    checks: Checks,
    /// The two folders of tiles: the whole map, the preview.
    sources: [sources::Facts; 2],
}

struct State {
    settings: Stored,
    /// The job that runs, else the last one.
    job: Option<Job>,
    /// Jobs launched so far: the last identifier given.
    jobs: u64,
    /// Events given so far: the cursor of the page.
    seq: u64,
    /// One per folder of tiles (map, preview): grows whenever its tiles change.
    revs: [u64; 2],
    /// A job is being launched (its folder emptied, its process started).
    launching: bool,
}

/// The process of the job that runs. Its standard input is kept open and never written: the
/// child ends by itself when it closes, that is when this server is gone.
struct Process {
    child: Child,
    _stdin: Option<ChildStdin>,
}

struct Server {
    port: u16,
    token: String,
    /// The server opened a window: it ends by itself when nobody asks anything any more.
    window: bool,
    exe: PathBuf,
    cwd: PathBuf,
    /// The program's local data folder: the settings, the derived configuration.
    data: PathBuf,
    /// `BTMS_INTERFACE_DIR`: the page's files are read there.
    page_dir: Option<PathBuf>,
    cores: usize,
    /// When the server started: the seconds of the load's history count from it.
    started: Instant,
    /// The machine, once it is read (None meanwhile).
    machine: Mutex<Option<machine::Machine>>,
    /// The load of the machine, read every second.
    load: Mutex<machine::Load>,
    state: Mutex<State>,
    process: Mutex<Option<Process>>,
    snapshot: Mutex<Option<Snapshot>>,
    /// The snapshot is to be read again whatever its age.
    stale: AtomicBool,
    /// A native dialog is open.
    dialog: AtomicBool,
    last_request: Mutex<Instant>,
}

/// A lock, also after a thread panicked while holding it: a request that fails must not stop
/// every later one.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether the `Host` header is this server's: the page was reached at 127.0.0.1 or localhost on
/// the port listened on, not under a name another site chose.
fn host_allowed(host: Option<&str>, port: u16) -> bool {
    host.is_some_and(|h| {
        h.eq_ignore_ascii_case(&format!("127.0.0.1:{port}")) || h.eq_ignore_ascii_case(&format!("localhost:{port}"))
    })
}

/// Whether the token given is the server's (compared whole, whatever differs).
fn token_matches(given: Option<&str>, token: &str) -> bool {
    given
        .is_some_and(|g| g.len() == token.len() && g.bytes().zip(token.bytes()).fold(0u8, |d, (a, b)| d | (a ^ b)) == 0)
}

/// A token nobody can guess: 256 bits from the hasher keys the system draws at random.
fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (0..4u64)
        .map(|i| {
            let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
            hasher.write_u64(i);
            hasher.write_u128(nanos);
            hasher.write_u32(std::process::id());
            format!("{:016x}", hasher.finish())
        })
        .collect()
}

/// With a window, the server ends by itself once nothing was asked for five minutes and no job
/// runs. Without one (`--no-window`), never.
fn must_exit(window: bool, busy: bool, idle: Duration) -> bool {
    window && !busy && idle >= IDLE_EXIT
}

/// The cursor a request gives back (`?since=<n>`), None when it gives none.
fn cursor(query: &str) -> Option<u64> {
    query.split('&').find_map(|pair| pair.strip_prefix("since=")).and_then(|v| v.parse().ok())
}

/// The zone of a render asked for: four numbers, put in order, not empty.
fn zone_asked(value: Option<&Value>) -> Result<[f64; 4], Message> {
    let z: [f64; 4] = value
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .ok_or_else(|| Message::new("zone_not_four_numbers", "zone: four numbers expected (x1, y1, x2, y2)"))?;
    if z.iter().any(|v| !v.is_finite() || v.abs() > 100_000.0) {
        return Err(Message::new("zone_outside_world", "zone: coordinates outside the world"));
    }
    let z = [z[0].min(z[2]), z[1].min(z[3]), z[0].max(z[2]), z[1].max(z[3])];
    if z[2] - z[0] <= 0.0 || z[3] - z[1] <= 0.0 {
        return Err(Message::new("zone_empty", "empty zone: it needs a width and a height"));
    }
    Ok(z)
}

/// What `POST /api/launch` asks, read from its body: the kind of job (`kind`) and what it needs.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Order {
    kind: Kind,
    /// The zone of a preview, or of an update of a zone.
    zone: Option<[f64; 4]>,
    /// What `update` is asked (the kinds `update`, `list-changes`, `record`).
    update: Option<Update>,
}

const UNKNOWN_JOB: &str =
    "unknown job: zone, map, update, list-changes, record, verify, groups, resources or import-keys expected";

/// The job a body asks for, refused with the reason when it is not one:
///
/// ```text
/// {"kind": "zone", "zone": [x1, y1, x2, y2]}       tiles <preview> --zone …
/// {"kind": "map"}                                  tiles <output>
/// {"kind": "update", "zone": [x1, y1, x2, y2]}     update <output> --zone …
/// {"kind": "update", "changes": true}              update <output> --changes
/// {"kind": "list-changes"}                         update <output> --list
/// {"kind": "record"}                               update <output> --record
/// {"kind": "verify" | "groups" | "resources"}, {"kind": "import-keys", "file": …}
/// ```
fn order(body: &Value) -> Result<Order, Message> {
    let kind = body
        .get("kind")
        .and_then(Value::as_str)
        .and_then(Kind::parse)
        .ok_or_else(|| Message::new("unknown_job", UNKNOWN_JOB))?;
    let mut order = Order { kind, zone: None, update: None };
    match kind {
        Kind::Zone => order.zone = Some(zone_asked(body.get("zone"))?),
        Kind::Update => {
            // (null and false are as good as absent)
            let zone = body.get("zone").filter(|v| !v.is_null());
            let changes = body.get("changes").filter(|v| !v.is_null() && **v != Value::Bool(false));
            match (zone, changes) {
                (Some(_), Some(_)) => {
                    return Err(Message::new(
                        "update_zone_and_changes",
                        "update: a zone or the changes of the resources, not both",
                    ));
                }
                (Some(zone), None) => {
                    let zone = zone_asked(Some(zone))?;
                    order.zone = Some(zone);
                    order.update = Some(Update::Zone(zone));
                }
                (None, Some(Value::Bool(true))) => order.update = Some(Update::Changes),
                _ => {
                    return Err(Message::new(
                        "update_zone_or_changes",
                        "update: \"zone\" [x1, y1, x2, y2] or \"changes\": true expected",
                    ));
                }
            }
        }
        Kind::ListChanges => order.update = Some(Update::List),
        Kind::Record => order.update = Some(Update::Record),
        _ => {}
    }
    Ok(order)
}

/// The game folder a job will use: the one chosen, else the one FiveM uses.
fn check_game(chosen: Option<&str>) -> Check {
    match chosen {
        Some(dir) => {
            let ok = Path::new(dir).join("GTA5.exe").is_file();
            let detail = if ok {
                Message::new("game_chosen", "folder chosen in the interface")
            } else {
                Message::new("game_no_exe", "no GTA5.exe in this folder")
            };
            Check::new(ok, Some(dir.to_string()), detail)
        }
        None => match crate::game::fivem_game_dir() {
            Some(dir) => Check::new(
                true,
                Some(dir.display().to_string()),
                Message::new("game_fivem", "the folder FiveM uses (CitizenFX.ini)"),
            ),
            None => Check::new(
                false,
                None,
                Message::new(
                    "game_not_found",
                    "GTA V folder not found: FiveM does not record it in CitizenFX.ini. Choose it.",
                ),
            ),
        },
    }
}

/// The key files and the size each must have.
const KEY_FILES: [(&str, u64); 3] = [
    (crate::game::AES_KEY_FILE, 32),
    (crate::game::NG_KEYS_FILE, crate::game::magic::KEYS_LEN as u64),
    (crate::game::NG_TABLES_FILE, crate::game::magic::TABLES_LEN as u64),
];

/// The three key files of `dir`: there, with their size. Only looked at, never read nor made: a
/// job checks each key against its SHA-1 when it reads them.
fn check_keys(dir: Option<&Path>) -> Check {
    let Some(dir) = dir else {
        return Check::new(
            false,
            Some(String::new()),
            Message::new("keys_no_folder", "keys folder not found (LOCALAPPDATA is not set)"),
        );
    };
    let missing: Vec<&str> = KEY_FILES
        .iter()
        .filter(|(name, size)| !std::fs::metadata(dir.join(name)).is_ok_and(|m| m.is_file() && m.len() == *size))
        .map(|(name, _)| *name)
        .collect();
    let detail = match missing.as_slice() {
        [] => Message::new("keys_present", "AES key, 101 NG keys and 272 NG tables present"),
        [only] if *only == crate::game::AES_KEY_FILE => Message::new(
            "keys_aes_missing",
            format!(
                "{only} missing: the AES key is looked for in GTA5.exe by the first job launched (for example \"Check the game and the keys\")"
            ),
        )
        .with("file", *only),
        names => {
            let files = names.join(", ");
            Message::new("keys_ng_missing", format!("{files} missing: import the NG keys from CodeWalker's magic.dat"))
                .with("files", files)
        }
    };
    Check::new(missing.is_empty(), Some(dir.display().to_string()), detail)
}

/// Whether the NG keys are there (the AES key is found by a job itself).
fn ng_keys_present(dir: Option<&Path>) -> bool {
    dir.is_some_and(|dir| {
        KEY_FILES[1..].iter().all(|(name, size)| std::fs::metadata(dir.join(name)).is_ok_and(|m| m.len() == *size))
    })
}

/// `map.toml` as the command line reads it: the defaults when the file of the current folder is
/// missing, an error when a file chosen (`explicit`) is missing or when it cannot be read.
fn check_config(path: &str, explicit: bool) -> (Check, Config) {
    let check = |ok: bool, detail: Message| Check::new(ok, Some(path.to_string()), detail);
    if !Path::new(path).is_file() {
        let (ok, detail) = if explicit {
            (false, Message::new("config_not_found", "file not found"))
        } else {
            (true, Message::new("config_default", "no map.toml in the current folder: default settings"))
        };
        return (check(ok, detail), Config::default());
    }
    match Config::load(Some(Path::new(path))) {
        Ok(config) => {
            let (kept, folders) = (config.maps.keep.len(), config.resources.folders.len());
            // one code per state of the light, so that each language words the whole sentence
            let detail = if config.lighting.enabled {
                Message::new(
                    "config_read_lighting_on",
                    format!("{kept} on-demand maps kept, {folders} resource folder(s), lighting on"),
                )
            } else {
                Message::new(
                    "config_read_lighting_off",
                    format!("{kept} on-demand maps kept, {folders} resource folder(s), lighting off"),
                )
            };
            (check(true, detail.with("kept", kept).with("folders", folders)), config)
        }
        Err(e) => {
            let error = format!("{e:#}");
            (check(false, Message::new("config_unreadable", error.as_str()).with("error", error)), Config::default())
        }
    }
}

impl Snapshot {
    fn read(resolved: &Resolved, explicit_config: bool) -> Snapshot {
        let (config, base) = check_config(&resolved.config, explicit_config);
        Snapshot {
            at: Instant::now(),
            base,
            checks: Checks {
                game: check_game(resolved.game.as_deref()),
                keys: check_keys(crate::game::keys_dir().ok().as_deref()),
                config,
            },
            sources: [sources::facts(Path::new(&resolved.output)), sources::facts(Path::new(&resolved.preview))],
        }
    }
}

/// The state as the page gets it (`GET /api/state`).
#[derive(Serialize)]
struct StateView<'a> {
    version: &'static str,
    cores: usize,
    /// None until it is read, a moment after the server started.
    machine: Option<machine::Machine>,
    /// None before the first reading.
    load: Option<machine::LoadView>,
    /// The interface settings, `language` among them (None: the page picks its own).
    settings: &'a Resolved,
    base: &'a Config,
    effective: &'a Config,
    checks: &'a Checks,
    sources: SourcesView<'a>,
    job: Option<job::View>,
    cursor: u64,
    events: Vec<job::PageEvent>,
}

#[derive(Serialize)]
struct SourcesView<'a> {
    map: SourceView<'a>,
    preview: SourceView<'a>,
}

#[derive(Serialize)]
struct SourceView<'a> {
    #[serde(flatten)]
    facts: &'a sources::Facts,
    rev: u64,
}

/// Resets the flag of a launch being made when the launch is over, however it ends.
struct Launching<'a>(&'a Server);

impl Drop for Launching<'_> {
    fn drop(&mut self) {
        lock(&self.0.state).launching = false;
    }
}

/// Resets the flag of a dialog being open, however it ends.
struct DialogOpen<'a>(&'a Server);

impl Drop for DialogOpen<'_> {
    fn drop(&mut self) {
        self.0.dialog.store(false, Ordering::SeqCst);
    }
}

impl Server {
    fn resolved(&self) -> Resolved {
        lock(&self.state).settings.resolve(&self.cwd)
    }

    fn busy(&self) -> bool {
        let state = lock(&self.state);
        state.launching || state.job.as_ref().is_some_and(Job::is_running)
    }

    /// What the state needs from the disk, read again when it is more than a second old or when
    /// something said it changed.
    fn snapshot(&self) -> Snapshot {
        let stored = lock(&self.state).settings.clone();
        let stale = self.stale.swap(false, Ordering::SeqCst);
        let mut slot = lock(&self.snapshot);
        if let Some(kept) = slot.as_ref().filter(|s| !stale && s.at.elapsed() < SNAPSHOT_AGE) {
            return kept.clone();
        }
        let fresh = Snapshot::read(&stored.resolve(&self.cwd), stored.config.is_some());
        // a manifest written again without a job of this server telling (a render from the
        // command line into the same folder): the page must fetch its tiles again
        let changed: Vec<usize> = match slot.as_ref() {
            Some(old) => (0..2)
                .filter(|&i| {
                    old.sources[i].folder == fresh.sources[i].folder && old.sources[i].stamp != fresh.sources[i].stamp
                })
                .collect(),
            None => Vec::new(),
        };
        *slot = Some(fresh.clone());
        drop(slot);
        if !changed.is_empty() {
            let mut state = lock(&self.state);
            for i in changed {
                state.revs[i] += 1;
            }
        }
        fresh
    }

    /// `GET /api/state`: everything the page shows, and the events after its cursor (all those of
    /// the job shown when it gives none).
    fn state_reply(&self, after: Option<u64>) -> Reply {
        let snapshot = self.snapshot();
        let now = Instant::now();
        // (each under its own short lock, never two at once: the thread that reads the load takes
        // its lock only to put a reading there)
        let machine = lock(&self.machine).clone();
        let load = lock(&self.load).view();
        let state = lock(&self.state);
        let resolved = state.settings.resolve(&self.cwd);
        let effective =
            settings::effective(&snapshot.base, &state.settings.overrides).unwrap_or_else(|_| snapshot.base.clone());
        let source =
            |which: Source| SourceView { facts: &snapshot.sources[which.index()], rev: state.revs[which.index()] };
        Reply::json(
            200,
            &StateView {
                version: env!("CARGO_PKG_VERSION"),
                cores: self.cores,
                machine,
                load,
                settings: &resolved,
                base: &snapshot.base,
                effective: &effective,
                checks: &snapshot.checks,
                sources: SourcesView { map: source(Source::Map), preview: source(Source::Preview) },
                job: state.job.as_ref().map(|job| job.view(now)),
                cursor: state.seq,
                events: state.job.as_ref().map(|job| job.events_after(after)).unwrap_or_default(),
            },
        )
    }

    /// `POST /api/settings`: a partial set merged into the settings, checked, kept.
    fn change_settings(&self, patch: &Value) -> Result<(), Reply> {
        let mut state = lock(&self.state);
        let next = state.settings.merge(patch).map_err(|e| Reply::plain(400, e))?;
        let (before, after) = (state.settings.resolve(&self.cwd), next.resolve(&self.cwd));
        let moved = [before.output != after.output, before.preview != after.preview];
        // the job that runs writes into the folder it was given: the page must not be shown another one meanwhile
        if (moved[0] || moved[1]) && (state.launching || state.job.as_ref().is_some_and(Job::is_running)) {
            return Err(Reply::error(
                409,
                Message::new("folders_busy", "a job is running: the tile folders can only be changed after it"),
            ));
        }
        settings::save(&self.data, &next).map_err(|e| Reply::plain(500, e))?;
        for (i, &m) in moved.iter().enumerate() {
            if m {
                state.revs[i] += 1;
            }
        }
        state.settings = next;
        drop(state);
        self.stale.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// `POST /api/launch`: one job at a time.
    fn launch(self: &Arc<Self>, body: &Value) -> Result<u64, Reply> {
        let order = order(body).map_err(|e| Reply::error(400, e))?;
        let stored = {
            let mut state = lock(&self.state);
            if state.launching || state.job.as_ref().is_some_and(Job::is_running) {
                return Err(Reply::error(409, Message::new("job_running", "a job is already running")));
            }
            state.launching = true;
            state.settings.clone()
        };
        let _launching = Launching(self);
        self.start(order, body, &stored)
    }

    /// Starts the process of a job: this executable, with `BTMS_PROGRESS=1`, its three standard
    /// streams piped, no console window; a render in the priority asked ([`creation_flags`]).
    fn start(self: &Arc<Self>, order: Order, body: &Value, stored: &Stored) -> Result<u64, Reply> {
        let refuse = |message: Message| Reply::error(400, message);
        let Order { kind, zone, update } = order;
        let resolved = stored.resolve(&self.cwd);
        let (config, base) = check_config(&resolved.config, stored.config.is_some());
        // a job that depends on map.toml is not run on the defaults when the file is broken
        if !config.ok && !matches!(kind, Kind::Verify | Kind::ImportKeys) {
            let detail = config.detail;
            return Err(refuse(
                Message::new("config_broken", format!("map.toml cannot be read, nothing is launched: {detail}"))
                    .with("detail", detail),
            ));
        }
        // `update` works on a map already drawn: none in its folder, nothing to update
        if kind.updates() {
            sources::map_present(Path::new(&resolved.output)).map_err(refuse)?;
        }
        let mut args: Vec<String> = Vec::new();
        let mut threads = 0;
        let render = kind.renders();
        match kind {
            Kind::Zone | Kind::Map | Kind::Update | Kind::ListChanges | Kind::Record => {
                // checked before the preview folder is emptied: a render that cannot start must
                // not cost the last preview (and `update` reads the game's files even to list)
                if !check_game(resolved.game.as_deref()).ok {
                    // a folder chosen without GTA5.exe, or none chosen and none FiveM records
                    return Err(refuse(match resolved.game {
                        Some(_) => Message::new("launch_game_no_exe", "game folder: no GTA5.exe in this folder"),
                        None => Message::new(
                            "launch_game_not_found",
                            "game folder: GTA V folder not found: FiveM does not record it in CitizenFX.ini. Choose it.",
                        ),
                    }));
                }
                if !ng_keys_present(crate::game::keys_dir().ok().as_deref()) {
                    return Err(refuse(Message::new(
                        "ng_keys_missing",
                        "NG keys missing: import them first from CodeWalker's magic.dat",
                    )));
                }
                // `auto`: the run says how many once it has chosen (its `settings` event)
                if render {
                    threads = match resolved.threads {
                        Threads::Count(n) => n,
                        Threads::Auto => 0,
                    };
                }
            }
            Kind::Verify => args.push("verify".into()),
            Kind::Groups => args.push("groups".into()),
            Kind::Resources => args.push("resources".into()),
            Kind::ImportKeys => {
                let given = body.get("file").and_then(Value::as_str).map(str::trim).filter(|f| !f.is_empty());
                let given = given.ok_or_else(|| {
                    refuse(Message::new(
                        "import_keys_no_file",
                        "import-keys: the magic.dat file must be given (\"file\")",
                    ))
                })?;
                let file = std::path::absolute(given).unwrap_or_else(|_| PathBuf::from(given));
                if !file.is_file() {
                    let path = file.display().to_string();
                    return Err(refuse(
                        Message::new("file_not_found", format!("file not found: {path}")).with("path", path),
                    ));
                }
                args.extend(["import-keys".into(), file.display().to_string()]);
            }
        }
        // map.toml is never written: the job reads the base and the overrides from a file of its own
        let effective = settings::effective(&base, &resolved.overrides).map_err(|e| Reply::plain(400, e))?;
        let derived = settings::write_derived(&self.data, &effective).map_err(|e| Reply::plain(500, e))?;
        if let Some(what) = update {
            // into the folder of the whole map, never the preview's: `render_args` is not for it
            args = resolved.update_args(what, &derived);
        } else if render {
            args = resolved.render_args(zone, &derived);
        } else {
            args.extend(["--config".into(), derived.display().to_string()]);
            if let Some(game) = &resolved.game {
                args.extend(["--game".into(), game.clone()]);
            }
        }
        if kind == Kind::Zone {
            sources::empty_preview(Path::new(&resolved.preview), Path::new(&resolved.output)).map_err(refuse)?;
        }

        let mut command = Command::new(&self.exe);
        command
            .args(&args)
            .env("BTMS_PROGRESS", "1")
            .current_dir(&self.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(creation_flags(render, resolved.priority));
        }
        let mut child = command.spawn().map_err(|e| {
            let exe = self.exe.display().to_string();
            Reply::error(
                500,
                Message::new("launch_failed", format!("cannot launch {exe}: {e}"))
                    .with("exe", exe)
                    .with("error", e.to_string()),
            )
        })?;
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let id = {
            let mut state = lock(&self.state);
            state.jobs += 1;
            let id = state.jobs;
            let mut job = Job::new(id, kind, zone, threads, started_ms, Instant::now());
            if render {
                // (what the run then says it took is compared with this: a card asked for that
                // does not draw is shown from the start)
                job.ask(job::Asked {
                    renderer: resolved.renderer.word().into(),
                    shading: resolved.shading.word().into(),
                    lighting: effective.lighting.enabled,
                });
            }
            job.say(&mut state.seq, &format!("> btms {}", args.join(" ")));
            if render && resolved.priority == Priority::Low {
                job.say(&mut state.seq, "(process priority: below normal)");
            }
            if let Some(source) = kind.source() {
                state.revs[source.index()] += 1;
            }
            state.job = Some(job);
            id
        };
        eprintln!("job {id} launched: {}", args.join(" "));
        *lock(&self.process) = Some(Process { child, _stdin: stdin });
        self.stale.store(true, Ordering::SeqCst);

        let pumps: Vec<_> = [stdout.map(|s| self.pump(id, s, false)), stderr.map(|s| self.pump(id, s, true))]
            .into_iter()
            .flatten()
            .collect();
        let server = Arc::clone(self);
        std::thread::spawn(move || {
            for pump in pumps {
                let _ = pump.join();
            }
            // both outputs are closed: the process is over, or about to be. Its handle is only
            // looked at, never waited on under the lock a stop needs
            let status = loop {
                let mut slot = lock(&server.process);
                match slot.as_mut().map(|p| p.child.try_wait()) {
                    Some(Ok(None)) => {}
                    Some(Ok(Some(status))) => {
                        *slot = None;
                        break Some(status);
                    }
                    Some(Err(_)) | None => {
                        *slot = None;
                        break None;
                    }
                }
                drop(slot);
                std::thread::sleep(Duration::from_millis(20));
            };
            let mut state = lock(&server.state);
            let State { job, seq, revs, .. } = &mut *state;
            if let Some(job) = job.as_mut().filter(|job| job.id == id) {
                job.exited(seq, status.is_some_and(|s| s.success()), status.and_then(|s| s.code()), Instant::now());
                eprintln!(
                    "job {id}: {}",
                    match job.status {
                        job::Status::Done => "done",
                        job::Status::Stopped => "stopped",
                        _ => "failed",
                    }
                );
                if let Some(source) = job.kind.source() {
                    revs[source.index()] += 1;
                }
            }
            drop(state);
            server.stale.store(true, Ordering::SeqCst);
            // the five minutes of an idle server count from the end of the job
            *lock(&server.last_request) = Instant::now();
        });
        Ok(id)
    }

    /// Reads one output of a job to its end, line by line (a plan of the whole map is one long
    /// line: no limit on a line here, the text kept for the page is cut in [`Job`]).
    fn pump(
        self: &Arc<Self>,
        id: u64,
        stream: impl Read + Send + 'static,
        error_output: bool,
    ) -> std::thread::JoinHandle<()> {
        let server = Arc::clone(self);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stream);
            let mut bytes = Vec::new();
            loop {
                bytes.clear();
                match reader.read_until(b'\n', &mut bytes) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let text = String::from_utf8_lossy(&bytes);
                let mut state = lock(&server.state);
                let State { job, seq, revs, .. } = &mut *state;
                let Some(job) = job.as_mut().filter(|job| job.id == id) else { continue };
                let effect = job.line(seq, text.trim_end_matches(['\r', '\n']), error_output, Instant::now());
                if let (true, Some(source)) = (effect.tiles_changed, job.kind.source()) {
                    revs[source.index()] += 1;
                }
            }
        })
    }

    /// Stops the job that runs: its process, by the handle kept when it was started.
    fn stop(&self) {
        if let Some(job) = lock(&self.state).job.as_mut().filter(|job| job.is_running()) {
            job.stop_requested = true;
        }
        if let Some(process) = lock(&self.process).as_mut() {
            let _ = process.child.kill();
        }
    }

    /// Stops the job and waits, at most `limit`, for it to be told over.
    fn stop_and_wait(&self, limit: Duration) {
        self.stop();
        let from = Instant::now();
        while self.busy() && from.elapsed() < limit {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Ends the server: the job first.
    fn shutdown(&self) -> ! {
        self.stop_and_wait(Duration::from_secs(3));
        std::process::exit(0);
    }

    /// `POST /api/open`: one of the three things the page may open, nothing else.
    fn open(&self, body: &Value) -> Result<(), Reply> {
        let resolved = self.resolved();
        let folder = |path: &str| -> Result<PathBuf, Reply> {
            if Path::new(path).is_dir() {
                Ok(PathBuf::from(path))
            } else {
                Err(Reply::error(
                    404,
                    Message::new("folder_missing", format!("this folder does not exist yet: {path}"))
                        .with("path", path),
                ))
            }
        };
        let target = match body.get("target").and_then(Value::as_str) {
            Some("output") => folder(&resolved.output)?,
            Some("preview") => folder(&resolved.preview)?,
            Some("config") => {
                let file = PathBuf::from(&resolved.config);
                if !file.is_file() {
                    let path = resolved.config.as_str();
                    return Err(Reply::error(
                        404,
                        Message::new("file_missing", format!("this file does not exist: {path}")).with("path", path),
                    ));
                }
                // the explorer runs what it is given: only a .toml file is handed to it
                if !file.extension().is_some_and(|e| e.eq_ignore_ascii_case("toml")) {
                    return Err(Reply::error(
                        400,
                        Message::new("open_not_toml", "only a .toml file can be opened from here"),
                    ));
                }
                file
            }
            _ => {
                return Err(Reply::error(
                    400,
                    Message::new("open_unknown_target", "to open: output, preview or config"),
                ));
            }
        };
        show(target.as_os_str()).map_err(|e| {
            Reply::error(500, Message::new("open_failed", format!("cannot open: {e}")).with("error", e.to_string()))
        })
    }

    /// `POST /api/choose`: a native dialog, one at a time. The request waits for it; the other
    /// requests go on.
    fn choose(&self, body: &Value) -> Result<Option<String>, Reply> {
        let folder = match body.get("kind").and_then(Value::as_str) {
            Some("folder") => true,
            Some("file") => false,
            _ => return Err(Reply::error(400, Message::new("choose_unknown_kind", "kind: folder or file expected"))),
        };
        let title: String = body
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or(if folder { "Choose a folder" } else { "Choose a file" })
            .chars()
            .take(200)
            .collect();
        // where the dialog opens: the folder given, or the folder of the file given
        let start =
            body.get("start_folder").and_then(Value::as_str).map(PathBuf::from).and_then(|p| {
                if p.is_dir() { Some(p) } else { p.parent().filter(|d| d.is_dir()).map(Path::to_path_buf) }
            });
        if self.dialog.swap(true, Ordering::SeqCst) {
            return Err(Reply::error(409, Message::new("dialog_open", "a dialog is already open")));
        }
        let _open = DialogOpen(self);
        Ok(dialog(folder, &title, start.as_deref()).map(|p| p.display().to_string()))
    }

    /// A file of the page: `/` and the other names of [`PAGE`], nothing else.
    fn page_file(&self, path: &str) -> Reply {
        let wanted =
            if path == "/" { Some("index.html") } else { path.strip_prefix('/').filter(|name| *name != "index.html") };
        let Some((name, kind, held)) = wanted.and_then(|w| PAGE.iter().find(|(name, ..)| *name == w)) else {
            return Reply::error(404, Message::new("unknown_page", "unknown page"));
        };
        let bytes = match &self.page_dir {
            Some(dir) if *name == ICON => std::fs::read(dir.join(ICON_FROM_PAGE_DIR)).unwrap_or_else(|_| held.to_vec()),
            Some(dir) => match std::fs::read(dir.join(name)) {
                Ok(bytes) => bytes,
                Err(e) => {
                    let path = dir.join(name).display().to_string();
                    return Reply::error(
                        404,
                        Message::new("page_file_unreadable", format!("{path}: {e}"))
                            .with("path", path)
                            .with("error", e.to_string()),
                    );
                }
            },
            None => held.to_vec(),
        };
        // the page is given the token it must send back
        let body = if *name == "index.html" {
            String::from_utf8_lossy(&bytes).replace(TOKEN_MARK, &self.token).into_bytes()
        } else {
            bytes
        };
        Reply { status: 200, kind, body }
    }

    /// `GET /tiles/<source>/<z>/<tx>/<ty>`.
    fn tile(&self, path: &str) -> Reply {
        let Some((source, z, tx, ty)) = sources::tile_request(path) else {
            return Reply::error(
                400,
                Message::new("bad_tile_address", "tile address refused: /tiles/<map|preview>/<z>/<tx>/<ty>"),
            );
        };
        let resolved = self.resolved();
        let dir = match source {
            Source::Map => &resolved.output,
            Source::Preview => &resolved.preview,
        };
        match sources::tile_file(Path::new(dir), z, tx, ty)
            .and_then(|(file, kind)| Some((std::fs::read(file).ok()?, kind)))
        {
            Some((body, kind)) => Reply { status: 200, kind, body },
            None => Reply::error(404, Message::new("tile_missing", "no such tile")),
        }
    }

    fn api(self: &Arc<Self>, request: &mut tiny_http::Request, path: &str, query: &str) -> Result<Reply, Reply> {
        let post = *request.method() == tiny_http::Method::Post;
        let wrong_method = || Reply::error(405, Message::new("method_not_allowed", "method not allowed"));
        let done = || Reply::json(200, &json!({ "ok": true }));
        match path {
            "/api/state" if *request.method() == tiny_http::Method::Get => Ok(self.state_reply(cursor(query))),
            "/api/state" => Err(wrong_method()),
            "/api/settings" | "/api/choose" | "/api/launch" | "/api/stop" | "/api/open" | "/api/quit" if !post => {
                Err(wrong_method())
            }
            "/api/settings" => {
                self.change_settings(&body(request)?)?;
                Ok(self.state_reply(cursor(query)))
            }
            "/api/choose" => Ok(Reply::json(200, &json!({ "path": self.choose(&body(request)?)? }))),
            "/api/launch" => Ok(Reply::json(200, &json!({ "job_id": self.launch(&body(request)?)? }))),
            "/api/stop" => {
                // answered once the job is told stopped: the state asked next shows it
                self.stop_and_wait(Duration::from_secs(3));
                Ok(done())
            }
            "/api/open" => {
                self.open(&body(request)?)?;
                Ok(done())
            }
            "/api/quit" => {
                let server = Arc::clone(self);
                std::thread::spawn(move || {
                    // the answer leaves first
                    std::thread::sleep(Duration::from_millis(300));
                    server.shutdown();
                });
                Ok(done())
            }
            _ => Err(Reply::error(404, Message::new("unknown_route", "unknown route"))),
        }
    }

    fn handle(self: &Arc<Self>, request: &mut tiny_http::Request) -> Reply {
        if !host_allowed(header(request, "Host").as_deref(), self.port) {
            return Reply::error(
                403,
                Message::new(
                    "host_refused",
                    "host refused: the interface only answers 127.0.0.1 and localhost, on its port",
                ),
            );
        }
        *lock(&self.last_request) = Instant::now();
        let url = request.url().to_string();
        let (path, query) = url.split_once('?').unwrap_or((&url, ""));
        if path.starts_with("/api/") {
            if !token_matches(header(request, TOKEN_HEADER).as_deref(), &self.token) {
                return Reply::error(
                    403,
                    Message::new(
                        "bad_token",
                        "token missing or wrong: only the page served by this interface can command it",
                    ),
                );
            }
            return self.api(request, path, query).unwrap_or_else(|refusal| refusal);
        }
        if *request.method() != tiny_http::Method::Get {
            return Reply::error(405, Message::new("method_not_allowed", "method not allowed"));
        }
        if path.starts_with("/tiles/") { self.tile(path) } else { self.page_file(path) }
    }
}

/// The flags a job's process is created with on Windows: no console window (`CREATE_NO_WINDOW`:
/// nothing pops up, and a Ctrl+C in the server's console is not the child's), and for a render
/// asked so, a priority below the normal one (`BELOW_NORMAL_PRIORITY_CLASS`): the computer stays
/// usable meanwhile.
#[cfg_attr(not(windows), allow(dead_code))]
fn creation_flags(render: bool, priority: Priority) -> u32 {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
    let low = render && priority == Priority::Low;
    CREATE_NO_WINDOW | if low { BELOW_NORMAL_PRIORITY_CLASS } else { 0 }
}

/// `interface --render-line [x1,y1,x2,y2]`: the arguments of the render the interface would
/// launch with the settings of `data` (a zone, or the whole map), as one JSON array on the
/// standard output. The derived configuration they name is written, as by a launch; nothing is
/// checked of the game nor of the keys, nothing is launched.
fn print_render_line(data: &Path, cwd: &Path, zone: Option<&str>) -> Result<()> {
    let zone = match zone {
        Some(text) => {
            let numbers: Vec<f64> = text
                .split(',')
                .map(|v| v.trim().parse::<f64>())
                .collect::<Result<_, _>>()
                .context("zone: x1,y1,x2,y2 expected")?;
            Some(zone_asked(Some(&json!(numbers))).map_err(|m| anyhow::Error::msg(m.text))?)
        }
        None => None,
    };
    let stored = settings::load(data);
    let resolved = stored.resolve(cwd);
    let (config, base) = check_config(&resolved.config, stored.config.is_some());
    if !config.ok {
        bail!("map.toml cannot be read: {}", config.detail);
    }
    let effective = settings::effective(&base, &resolved.overrides).map_err(anyhow::Error::msg)?;
    let derived = settings::write_derived(data, &effective).map_err(anyhow::Error::msg)?;
    println!("{}", serde_json::to_string(&resolved.render_args(zone, &derived))?);
    let priority = if resolved.priority == Priority::Low { "below normal" } else { "normal" };
    eprintln!("settings of {}; process priority: {priority}", data.join(settings::FILE).display());
    Ok(())
}

/// Whether a request announces a body longer than the server reads ([`BODY_LIMIT`]).
fn body_too_long(announced: Option<usize>) -> bool {
    announced.is_some_and(|length| length > BODY_LIMIT)
}

/// The value of a header of the request.
fn header(request: &tiny_http::Request, name: &'static str) -> Option<String> {
    request.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_string())
}

/// The JSON body of a request; an empty body is an empty object.
fn body(request: &mut tiny_http::Request) -> Result<Value, Reply> {
    let mut bytes = Vec::new();
    request
        .as_reader()
        .take(BODY_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Reply::error(400, Message::new("body_unreadable", "request body unreadable")))?;
    if bytes.len() > BODY_LIMIT {
        return Err(Reply::error(413, Message::new("body_too_long", "request body too long")));
    }
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(json!({}));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| Reply::error(400, Message::new("body_not_json", "request body: JSON expected")))
}

/// Hands a folder, a file or an address to the system's own opener, as one argument of its own:
/// nothing goes through a command interpreter.
fn show(target: &std::ffi::OsStr) -> std::io::Result<()> {
    #[cfg(windows)]
    let opener =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("explorer.exe");
    #[cfg(target_os = "macos")]
    let opener = PathBuf::from("open");
    #[cfg(not(any(windows, target_os = "macos")))]
    let opener = PathBuf::from("xdg-open");
    Command::new(opener)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Where Edge may be installed.
#[cfg(windows)]
fn edge() -> Option<PathBuf> {
    ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|root| Path::new(&root).join("Microsoft").join("Edge").join("Application").join("msedge.exe"))
        .find(|exe| exe.is_file())
}

/// Opens the page in a window of its own: Edge as an application (no tabs, no address bar), else
/// the default browser. Says what it opened.
fn open_window(address: &str) -> String {
    #[cfg(windows)]
    if let Some(exe) = edge() {
        let app = format!("--app={address}");
        if Command::new(&exe).arg(app).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok()
        {
            return format!("window opened with Edge ({})", exe.display());
        }
    }
    match show(address.as_ref()) {
        Ok(()) => "page opened in the default browser".into(),
        Err(e) => format!("no window could be opened ({e}): open {address} in a browser"),
    }
}

/// The window the user is working in (the page's, when a button of the page was just clicked):
/// the dialog is given it as its owner, so that it opens above it and not behind.
#[cfg(windows)]
struct Foreground(std::num::NonZeroIsize);

#[cfg(windows)]
impl Foreground {
    fn find() -> Option<Foreground> {
        #[link(name = "user32")]
        unsafe extern "system" {
            fn GetForegroundWindow() -> isize;
        }
        // SAFETY: takes no argument and returns a window handle, or null
        std::num::NonZeroIsize::new(unsafe { GetForegroundWindow() }).map(Foreground)
    }
}

#[cfg(windows)]
impl raw_window_handle::HasWindowHandle for Foreground {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        let raw = raw_window_handle::RawWindowHandle::Win32(raw_window_handle::Win32WindowHandle::new(self.0));
        // SAFETY: the handle is only given to the dialog as its owner; a window closed meanwhile is refused by the system
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(raw) })
    }
}

#[cfg(windows)]
impl raw_window_handle::HasDisplayHandle for Foreground {
    fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        Ok(raw_window_handle::DisplayHandle::windows())
    }
}

/// A native dialog: a folder or a file. None when the user closes it without choosing.
fn dialog(folder: bool, title: &str, start: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().set_title(title);
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    #[cfg(windows)]
    if let Some(owner) = Foreground::find() {
        dialog = dialog.set_parent(&owner);
    }
    if folder { dialog.pick_folder() } else { dialog.pick_file() }
}

/// Listens on 127.0.0.1, on `first` or one of the ports after it when it is taken.
fn listen(first: u16) -> Result<(tiny_http::Server, u16)> {
    let mut last = None;
    for port in (first..=first.saturating_add(PORTS_TRIED)).take(PORTS_TRIED as usize + 1) {
        match tiny_http::Server::http(("127.0.0.1", port)) {
            Ok(server) => return Ok((server, port)),
            Err(e) => last = Some(e.to_string()),
        }
    }
    bail!(
        "no free port from {first} to {} on 127.0.0.1 ({})",
        first.saturating_add(PORTS_TRIED),
        last.unwrap_or_default()
    )
}

/// The machine, read once, and its load, read every second, each on a thread of its own: what
/// they read is put under its lock when it is read, the requests never wait for a reading. A
/// reading that panics is skipped, the next one comes all the same.
fn watch_machine(server: &Arc<Server>) {
    let reader = Arc::clone(server);
    std::thread::spawn(move || {
        if let Ok(machine) = std::panic::catch_unwind(machine::machine) {
            for card in &machine.cards {
                eprintln!("graphics card: {}", card.line());
            }
            *lock(&reader.machine) = Some(machine);
        }
    });
    let sampler = Arc::clone(server);
    std::thread::spawn(move || {
        let mut readings = machine::Sampler::new();
        loop {
            let tick = Instant::now();
            let main = lock(&sampler.machine)
                .as_ref()
                .and_then(machine::Machine::main_card)
                .and_then(|card| card.luid.clone());
            if let Ok(Some(sample)) = std::panic::catch_unwind(AssertUnwindSafe(|| readings.sample(main.as_deref()))) {
                let seconds = sampler.started.elapsed().as_secs_f64();
                lock(&sampler.load).push(seconds, sample);
            }
            std::thread::sleep(LOAD_PERIOD.saturating_sub(tick.elapsed()));
        }
    });
}

/// `btms interface [--port <n>] [--no-window]`. Does not return while the server lives.
pub fn run(args: &[String]) -> Result<()> {
    let (mut first, mut window) = (DEFAULT_PORT, true);
    let mut line: Option<Option<String>> = None;
    let mut rest = args.iter().skip(1).peekable();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--port" => {
                first = rest
                    .next()
                    .context("--port without a number")?
                    .parse()
                    .ok()
                    .filter(|p| *p > 0)
                    .context("--port: a port number expected")?
            }
            "--no-window" => window = false,
            "--render-line" => line = Some(rest.next_if(|a| !a.starts_with("--")).cloned()),
            other => bail!("interface: unknown option {other} (--port <n>, --no-window, --render-line [x1,y1,x2,y2])"),
        }
    }
    let exe = std::env::current_exe().context("the path of the executable cannot be found")?;
    let cwd = std::env::current_dir().context("the current folder cannot be found")?;
    let data = match std::env::var_os("LOCALAPPDATA") {
        Some(local) => Path::new(&local).join("BTMS"),
        None => std::env::temp_dir().join("BTMS"),
    };
    if let Some(zone) = line {
        return print_render_line(&data, &cwd, zone.as_deref());
    }
    // one interface per data folder: started again while a server runs (its window closed, a
    // render going on), the command shows that server's page instead of starting a second one
    let guard = match instance::claim(&data, instance::PATIENCE) {
        instance::Instance::First(guard) => guard,
        instance::Instance::Running(port) => {
            let address = format!("http://127.0.0.1:{port}/");
            println!("interface: {address}");
            println!("an interface is already open at this address: no other is started");
            if window {
                println!("{}", open_window(&address));
            }
            return Ok(());
        }
        instance::Instance::Silent => bail!(
            "another interface is open but does not tell its address ({}): close it, or try again in a moment if it is starting",
            data.join("interface").display()
        ),
    };
    let (http, port) = listen(first)?;
    guard.publish(port);
    let address = format!("http://127.0.0.1:{port}/");
    let server = Arc::new(Server {
        port,
        token: new_token(),
        window,
        exe,
        cwd,
        page_dir: std::env::var_os("BTMS_INTERFACE_DIR").filter(|d| !d.is_empty()).map(PathBuf::from),
        cores: std::thread::available_parallelism().map(usize::from).unwrap_or(1),
        started: Instant::now(),
        machine: Mutex::new(None),
        load: Mutex::new(machine::Load::default()),
        state: Mutex::new(State {
            settings: settings::load(&data),
            job: None,
            jobs: 0,
            seq: 0,
            revs: [1, 1],
            launching: false,
        }),
        data,
        process: Mutex::new(None),
        snapshot: Mutex::new(None),
        stale: AtomicBool::new(false),
        dialog: AtomicBool::new(false),
        last_request: Mutex::new(Instant::now()),
    });
    println!("interface: {address}");
    if let Some(dir) = &server.page_dir {
        println!("page served from {} (BTMS_INTERFACE_DIR)", dir.display());
    }
    watch_machine(&server);

    let http = Arc::new(http);
    for _ in 0..WORKERS {
        let (http, server) = (Arc::clone(&http), Arc::clone(&server));
        std::thread::spawn(move || {
            loop {
                let Ok(mut request) = http.recv() else {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                };
                // a body announced longer than any the page sends. When a request is dropped,
                // answered or not, tiny_http reads what is left of its body into one buffer of the
                // length announced: a terabyte announced, without a token, would end the process.
                // So this request is neither read, answered nor dropped: its connection stays
                // open and unanswered (one thread of tiny_http waits on it) until the server ends
                if body_too_long(request.body_length()) {
                    eprintln!(
                        "request left unanswered: a body of {} bytes announced ({BODY_LIMIT} at most)",
                        request.body_length().unwrap_or(0)
                    );
                    std::mem::forget(request);
                    continue;
                }
                // a request that panics is answered an error, and the thread goes on
                let reply =
                    std::panic::catch_unwind(AssertUnwindSafe(|| server.handle(&mut request))).unwrap_or_else(|_| {
                        Reply::error(500, Message::new("internal_error", "internal error of the interface"))
                    });
                let header = |name: &str, value: &str| tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes());
                let mut response = tiny_http::Response::from_data(reply.body).with_status_code(reply.status);
                for (name, value) in
                    [("Content-Type", reply.kind), ("Cache-Control", "no-store"), ("X-Content-Type-Options", "nosniff")]
                {
                    if let Ok(header) = header(name, value) {
                        response.add_header(header);
                    }
                }
                let _ = request.respond(response);
            }
        });
    }

    if window {
        println!("{}", open_window(&address));
        println!("The interface closes by itself five minutes after its window is closed, when no job is running.");
    } else {
        println!("no window: the interface runs until Ctrl+C (or /api/quit)");
    }
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let idle = lock(&server.last_request).elapsed();
        if must_exit(server.window, server.busy(), idle) {
            println!("interface: no request for five minutes, stopping");
            server.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_server_s_host_is_answered() {
        for good in ["127.0.0.1:8791", "localhost:8791", "LocalHost:8791"] {
            assert!(host_allowed(Some(good), 8791), "{good}");
        }
        for bad in [
            "127.0.0.1",
            "127.0.0.1:8792",
            "localhost",
            "localhost:80",
            "evil.example:8791",
            "127.0.0.1:8791.evil.example",
            "evil.example",
            "[::1]:8791",
            "0.0.0.0:8791",
            "127.0.0.1:8791 ",
            "",
        ] {
            assert!(!host_allowed(Some(bad), 8791), "{bad}");
        }
        assert!(!host_allowed(None, 8791));
    }

    #[test]
    fn the_token_is_wanted_whole() {
        let token = new_token();
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(token, new_token());
        assert!(token_matches(Some(&token), &token));
        assert!(!token_matches(None, &token));
        assert!(!token_matches(Some(""), &token));
        assert!(!token_matches(Some(&token[..63]), &token));
        assert!(!token_matches(Some(&format!("{token}0")), &token));
        assert!(!token_matches(Some(&token.to_uppercase().replace('0', "1")), &token));
        assert!(!token_matches(Some(&new_token()), &token));
    }

    #[test]
    fn the_server_ends_by_itself_only_with_a_window_and_nothing_to_do() {
        let (short, long) = (Duration::from_secs(299), Duration::from_secs(300));
        assert!(must_exit(true, false, long));
        assert!(!must_exit(true, false, short));
        assert!(!must_exit(true, true, Duration::from_secs(86_400)));
        assert!(!must_exit(false, false, Duration::from_secs(86_400)));
    }

    #[test]
    fn the_cursor_and_the_zone_of_a_request() {
        assert_eq!(cursor("since=42"), Some(42));
        assert_eq!(cursor("v=3&since=0"), Some(0));
        assert_eq!(cursor(""), None);
        assert_eq!(cursor("since="), None);
        assert_eq!(cursor("since=-1"), None);
        assert_eq!(cursor("after=4"), None);
        assert_eq!(zone_asked(Some(&json!([640, -768, 384, -1024]))), Ok([384.0, -1024.0, 640.0, -768.0]));
        assert_eq!(zone_asked(Some(&json!([0.5, 0, 1, 2]))), Ok([0.5, 0.0, 1.0, 2.0]));
        for bad in [
            json!([0, 0, 0, 5]),
            json!([1, 2, 3]),
            json!([1, 2, 3, 4, 5]),
            json!("0,0,1,1"),
            json!([0, 0, 1, "1"]),
            json!([0, 0, 1e9, 1]),
            json!(null),
        ] {
            assert!(zone_asked(Some(&bad)).is_err(), "{bad}");
        }
        assert!(zone_asked(None).is_err());
    }

    #[test]
    fn the_job_a_body_asks_for() {
        let asked = |body: Value| order(&body);
        let zone = [384.0, -1024.0, 640.0, -768.0];
        // an update of a zone: put in order like a preview's, into the whole map
        let o = asked(json!({"kind": "update", "zone": [640, -768, 384, -1024]})).unwrap();
        assert_eq!(o, Order { kind: Kind::Update, zone: Some(zone), update: Some(Update::Zone(zone)) });
        assert_eq!(o.kind.source(), Some(Source::Map));
        // the ground of what changed; null and false are as good as absent
        for body in
            [json!({"kind": "update", "changes": true}), json!({"kind": "update", "changes": true, "zone": null})]
        {
            assert_eq!(asked(body).unwrap(), Order { kind: Kind::Update, zone: None, update: Some(Update::Changes) });
        }
        assert_eq!(
            asked(json!({"kind": "update", "zone": zone, "changes": false})).unwrap().update,
            Some(Update::Zone(zone))
        );
        // listing and recording: nothing else to give
        assert_eq!(
            asked(json!({"kind": "list-changes"})).unwrap(),
            Order { kind: Kind::ListChanges, zone: None, update: Some(Update::List) }
        );
        assert_eq!(
            asked(json!({"kind": "record", "zone": zone})).unwrap(),
            Order { kind: Kind::Record, zone: None, update: Some(Update::Record) }
        );
        // the older jobs are what they were
        assert_eq!(
            asked(json!({"kind": "zone", "zone": zone})).unwrap(),
            Order { kind: Kind::Zone, zone: Some(zone), update: None }
        );
        assert_eq!(
            asked(json!({"kind": "map", "zone": zone})).unwrap(),
            Order { kind: Kind::Map, zone: None, update: None }
        );
        assert!(asked(json!({"kind": "zone"})).is_err());
        // refused, with the reason and its code
        let refusal = |body: Value| asked(body).unwrap_err();
        let r = refusal(json!({"kind": "update"}));
        assert!(r.text.contains("\"zone\"") && r.code == "update_zone_or_changes", "{r:?}");
        assert!(refusal(json!({"kind": "update", "changes": "yes"})).text.contains("\"changes\""));
        let r = refusal(json!({"kind": "update", "zone": zone, "changes": true}));
        assert!(r.text.contains("not both") && r.code == "update_zone_and_changes", "{r:?}");
        let r = refusal(json!({"kind": "update", "zone": [0, 0, 0, 5]}));
        assert!(r.text.starts_with("empty zone") && r.code == "zone_empty", "{r:?}");
        assert!(refusal(json!({"kind": "update", "zone": "0,0,1,1"})).text.starts_with("zone:"));
        for unknown in [json!({"kind": "tiles"}), json!({"kind": "list"}), json!({}), json!({"kind": 3})] {
            let r = refusal(unknown);
            assert_eq!((r.text.as_str(), r.code.as_str()), (UNKNOWN_JOB, "unknown_job"));
        }
    }

    /// A server as `run` makes it, without listening nor watching the machine: its folders under
    /// `dir`, its executable one that does not exist (a test that got as far as starting a job
    /// would see it fail).
    fn test_server(dir: &Path, stored: Stored) -> Arc<Server> {
        let cwd = dir.join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        Arc::new(Server {
            port: 8791,
            token: new_token(),
            window: false,
            exe: dir.join("missing.exe"),
            cwd,
            data: dir.join("data"),
            page_dir: None,
            cores: 4,
            started: Instant::now(),
            machine: Mutex::new(None),
            load: Mutex::new(machine::Load::default()),
            state: Mutex::new(State { settings: stored, job: None, jobs: 0, seq: 0, revs: [1, 1], launching: false }),
            process: Mutex::new(None),
            snapshot: Mutex::new(None),
            stale: AtomicBool::new(false),
            dialog: AtomicBool::new(false),
            last_request: Mutex::new(Instant::now()),
        })
    }

    /// The status and the body of a refused launch: its English text, its code.
    fn refusal(reply: Result<u64, Reply>) -> (u16, String, String) {
        let Err(reply) = reply else { panic!("launched when it should have been refused") };
        let body: Value = serde_json::from_slice(&reply.body).unwrap();
        let text = |key: &str| body[key].as_str().unwrap_or_default().to_string();
        (reply.status, text("error"), text("code"))
    }

    #[test]
    fn an_update_wants_a_map_and_the_place_to_itself() {
        let dir = std::env::temp_dir().join(format!("btms-tests-{}-update", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (map, game) = (dir.join("map"), dir.join("game"));
        std::fs::create_dir_all(&map).unwrap();
        std::fs::create_dir_all(&game).unwrap();
        // the game chosen is a folder without GTA5.exe: no job of this test gets past it (nor
        // reads the keys of the machine)
        let stored = Stored::default()
            .merge(&json!({"output": map.display().to_string(), "game": game.display().to_string()}))
            .unwrap();
        let server = test_server(&dir, stored);
        let nothing_started = |server: &Server| {
            let state = lock(&server.state);
            assert!(!state.launching && state.job.is_none() && lock(&server.process).is_none());
        };
        // no map in its folder: the jobs of `update` are refused, and say why
        for body in [
            json!({"kind": "update", "zone": [0, 0, 128, 128]}),
            json!({"kind": "update", "changes": true}),
            json!({"kind": "list-changes"}),
            json!({"kind": "record"}),
        ] {
            let (status, message, code) = refusal(server.launch(&body));
            assert_eq!((status, code.as_str()), (400, "no_map_to_update"), "{body}");
            assert!(
                message.starts_with("no map to update") && message.contains(&map.display().to_string()),
                "{message}"
            );
            nothing_started(&server);
        }
        // a manifest of someone else is no map either
        std::fs::write(map.join("tiles.json"), r#"{"format": "other"}"#).unwrap();
        assert!(refusal(server.launch(&json!({"kind": "record"}))).1.starts_with("no map"));
        // a map: refused further on, by the game (it went past the map), nothing started
        let manifest =
            json!({"format": crate::tiles::format::MANIFEST_FORMAT, "blocks": 6097, "complete": true, "zone": null});
        std::fs::write(map.join("tiles.json"), manifest.to_string()).unwrap();
        for body in [json!({"kind": "update", "zone": [0, 0, 128, 128]}), json!({"kind": "list-changes"})] {
            let (status, message, code) = refusal(server.launch(&body));
            assert_eq!((status, code.as_str()), (400, "launch_game_no_exe"));
            assert!(message.starts_with("game folder: no GTA5.exe"), "{message}");
            nothing_started(&server);
        }
        // a body that is not one: 400 before anything else
        assert_eq!(refusal(server.launch(&json!({"kind": "update"}))).0, 400);
        // another job runs: 409, whatever is asked, and that job is left as it is
        lock(&server.state).job = Some(Job::new(7, Kind::Map, None, 8, 0, Instant::now()));
        for body in [
            json!({"kind": "update", "zone": [0, 0, 128, 128]}),
            json!({"kind": "update", "changes": true}),
            json!({"kind": "list-changes"}),
            json!({"kind": "record"}),
        ] {
            assert_eq!(
                refusal(server.launch(&body)),
                (409, "a job is already running".to_string(), "job_running".to_string()),
                "{body}"
            );
        }
        let state = lock(&server.state);
        assert!(!state.launching && state.job.as_ref().is_some_and(|job| job.id == 7 && job.is_running()));
        drop(state);
        // the map is untouched
        assert!(std::fs::read_to_string(map.join("tiles.json")).unwrap().contains("6097"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_state_carries_the_settings_and_the_coded_checks() {
        let dir = std::env::temp_dir().join(format!("btms-tests-{}-state", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // a game folder chosen (FiveM's own is not looked for) and a language
        let stored = Stored::default().merge(&json!({"game": dir.display().to_string(), "language": "fr"})).unwrap();
        let server = test_server(&dir, stored);
        let state: Value = serde_json::from_slice(&server.state_reply(None).body).unwrap();
        for key in [
            "version",
            "cores",
            "machine",
            "load",
            "settings",
            "base",
            "effective",
            "checks",
            "sources",
            "job",
            "cursor",
            "events",
        ] {
            assert!(state.get(key).is_some(), "the state without {key}: {state}");
        }
        assert_eq!(state["settings"]["language"], "fr", "{}", state["settings"]);
        let game = &state["checks"]["game"];
        assert_eq!(
            (&game["ok"], &game["code"], &game["detail"]),
            (&json!(false), &json!("game_no_exe"), &json!("no GTA5.exe in this folder"))
        );
        assert!(
            game["params"].is_object()
                && state["checks"]["keys"]["code"].is_string()
                && state["checks"]["config"]["code"] == "config_default",
            "{}",
            state["checks"]
        );
        assert_eq!(state["sources"]["map"]["exists"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_render_asked_low_is_started_below_the_normal_priority() {
        const NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL: u32 = 0x0000_4000;
        assert_eq!(creation_flags(true, Priority::Normal), NO_WINDOW);
        assert_eq!(creation_flags(true, Priority::Low), NO_WINDOW | BELOW_NORMAL);
        // a report or the import of the keys: as before, whatever the setting
        assert_eq!(creation_flags(false, Priority::Low), NO_WINDOW);
        assert_eq!(creation_flags(false, Priority::Normal), NO_WINDOW);
    }

    #[test]
    fn the_render_line_is_printed_from_the_settings_kept() {
        let data = std::env::temp_dir().join(format!("btms-tests-{}-line", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        std::fs::create_dir_all(&data).unwrap();
        let cwd = data.join("cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        let stored = Stored::default()
            .merge(&json!({"threads": 6, "shading": "cpu", "overrides": {"water": {"enabled": false}}}))
            .unwrap();
        settings::save(&data, &stored).unwrap();
        // the line itself is `render_args`'s (tested in settings.rs); here: the file it names is written
        print_render_line(&data, &cwd, Some("640,-768,384,-1024")).unwrap();
        let derived = data.join(settings::DERIVED[0]).join(settings::DERIVED[1]);
        assert!(!Config::load(Some(&derived)).unwrap().water.enabled);
        let line = stored.resolve(&cwd).render_args(Some([384.0, -1024.0, 640.0, -768.0]), &derived);
        assert_eq!(&line[..4], ["tiles", stored.resolve(&cwd).preview.as_str(), "--zone", "384,-1024,640,-768"]);
        assert!(print_render_line(&data, &cwd, Some("1,2,3")).is_err());
        assert!(print_render_line(&data, &cwd, Some("0,0,0,5")).is_err());
        // a map.toml that cannot be read: no line
        settings::save(&data, &Stored { config: Some(cwd.join("missing.toml").display().to_string()), ..stored })
            .unwrap();
        assert!(print_render_line(&data, &cwd, None).is_err());
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn a_body_announced_too_long_is_not_read() {
        assert!(!body_too_long(None));
        assert!(!body_too_long(Some(0)));
        assert!(!body_too_long(Some(BODY_LIMIT)));
        assert!(body_too_long(Some(BODY_LIMIT + 1)));
        assert!(body_too_long(Some(1 << 40)));
        assert!(body_too_long(Some(usize::MAX)));
    }

    #[test]
    fn the_checks_say_what_is_missing() {
        let dir = std::env::temp_dir().join(format!("btms-tests-{}-checks", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // keys: none, the NG ones only, all three, one of the wrong size
        let keys = check_keys(Some(&dir));
        assert!(
            !keys.ok && keys.detail.contains("gtav_ng_key.dat") && keys.detail.contains("magic.dat"),
            "{}",
            keys.detail
        );
        assert_eq!(
            (keys.code.as_deref(), keys.params.get("files").and_then(Value::as_str)),
            (Some("keys_ng_missing"), Some(KEY_FILES.map(|(name, _)| name).join(", ").as_str()))
        );
        assert!(!ng_keys_present(Some(&dir)));
        std::fs::write(dir.join(crate::game::NG_KEYS_FILE), vec![0u8; crate::game::magic::KEYS_LEN]).unwrap();
        std::fs::write(dir.join(crate::game::NG_TABLES_FILE), vec![0u8; crate::game::magic::TABLES_LEN]).unwrap();
        let keys = check_keys(Some(&dir));
        assert!(!keys.ok && keys.detail.contains("GTA5.exe") && !keys.detail.contains("magic.dat"), "{}", keys.detail);
        assert_eq!(keys.code.as_deref(), Some("keys_aes_missing"));
        assert!(ng_keys_present(Some(&dir)));
        std::fs::write(dir.join(crate::game::AES_KEY_FILE), [0u8; 32]).unwrap();
        assert!(check_keys(Some(&dir)).ok);
        std::fs::write(dir.join(crate::game::NG_KEYS_FILE), [0u8; 12]).unwrap();
        assert!(!check_keys(Some(&dir)).ok && !ng_keys_present(Some(&dir)));
        assert!(!check_keys(None).ok && !ng_keys_present(None));

        // map.toml: missing in the current folder (the defaults), missing where chosen, read, broken
        let file = dir.join("map.toml").display().to_string();
        let (check, config) = check_config(&file, false);
        assert!(
            check.ok && config == Config::default() && check.code.as_deref() == Some("config_default"),
            "{}",
            check.detail
        );
        assert!(!check_config(&file, true).0.ok);
        std::fs::write(&file, "[maps]\nkeep = ['a', 'b']\n[lighting]\nenabled = false").unwrap();
        let (check, config) = check_config(&file, true);
        assert!(
            check.ok && check.detail.starts_with("2 on-demand") && check.detail.ends_with("off"),
            "{}",
            check.detail
        );
        assert_eq!(
            (check.code.as_deref(), check.params.get("kept"), check.params.get("folders")),
            (Some("config_read_lighting_off"), Some(&json!(2)), Some(&json!(0)))
        );
        assert_eq!(config.maps.keep, ["a", "b"]);
        std::fs::write(&file, "[lighting]\nenabled = maybe").unwrap();
        let (check, config) = check_config(&file, false);
        assert!(!check.ok && check.detail.contains("unreadable") && config == Config::default(), "{}", check.detail);
        assert_eq!(check.params.get("error").and_then(Value::as_str), Some(check.detail.as_str()));

        // the game folder: the one chosen must hold GTA5.exe
        let game = check_game(Some(&dir.display().to_string()));
        assert!(!game.ok && game.path.is_some());
        std::fs::write(dir.join("GTA5.exe"), b"").unwrap();
        assert!(check_game(Some(&dir.display().to_string())).ok);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_page_is_whole_and_wants_its_token() {
        let names: Vec<&str> = PAGE.iter().map(|(name, ..)| *name).collect();
        assert_eq!(
            names,
            [
                "index.html",
                "css/style.css",
                "js/app.js",
                "js/map.js",
                "js/demo.js",
                "js/fallback.js",
                "js/i18n.js",
                "icon.png",
                "locales/en.json",
                "locales/fr.json",
                "locales/de.json",
                "locales/nl.json",
                "locales/ru.json",
                "locales/sv.json",
                "locales/es.json",
            ]
        );
        let index = String::from_utf8_lossy(PAGE[0].2);
        assert!(index.contains("btms-token") && index.contains(TOKEN_MARK), "index.html must carry the token mark");
        // the places the page writes who draws and what the card left (`fallback.js`),
        // those of the update of the map: its button beside the preview's, the changes of the resources,
        // and the language selector
        for id in [
            "id=\"fallback-banner\"",
            "id=\"devices-line\"",
            "id=\"launch-update\"",
            "id=\"changes\"",
            "id=\"find-changes\"",
            "id=\"update-changes\"",
            "id=\"record-state\"",
            "id=\"language-select\"",
        ] {
            assert!(index.contains(id), "index.html without {id}");
        }
        let app = String::from_utf8_lossy(PAGE[2].2);
        assert!(app.contains("from './fallback.js'"), "app.js must import fallback.js");
    }
}
