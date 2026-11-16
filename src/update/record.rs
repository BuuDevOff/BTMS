//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! What a map folder was drawn with: `<folder>/resources.json` ([`Record`], format [`FORMAT`]).
//!
//! For each resource of the server: its name under the resource folders of `map.toml` ([`name_of`]),
//! its folder, a quick fingerprint of its files ([`fingerprint`]: their relative paths, sizes and
//! dates, not their content), the ground it changes on the map (`zone` in metres, `blocks`: the 128 m
//! blocks, as rectangles), whether all its map files read, and why not. Beside them, what changes
//! the whole map ([`settings`]: `[maps]`, `[lighting]`, `[water]`), the resource folders, and the
//! maps loaded on request that the render drew (`on_demand`, null when not known).
//!
//! Comparing the record with the resources as they are now ([`diff`]) gives those added, changed
//! and removed: `update --changes` draws their ground again.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The file in the map folder.
pub const FILE: &str = "resources.json";
/// What its `format` says.
pub const FORMAT: &str = "btms resources v1";

/// The record of a map folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub format: String,
    /// When it was written (UTC).
    pub date: String,
    /// What wrote it: `tiles`, `update --changes`, `update --record`.
    pub by: String,
    /// What changes the whole map ([`settings`]).
    pub settings: Value,
    /// `[resources] folders` of `map.toml`, by their last name only ([`folder_names`]): the
    /// record sits in the map folder, which is published as it is, and must not tell the paths of
    /// the user's disk.
    pub folders: Vec<String>,
    /// The maps loaded on request that were drawn (names), null when not known (`--record` does not
    /// look for the holes they close).
    pub on_demand: Option<Vec<String>>,
    pub resources: Vec<Entry>,
}

/// One resource of a [`Record`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Its folder under the resource folders of `map.toml` (`[my-island]/my-island-map`).
    pub name: String,
    /// Its folder, in full, as found now: never written to the file (the map folder is published
    /// as it is), so empty for an entry read back.
    #[serde(skip)]
    pub folder: String,
    /// [`fingerprint`] of its files, and how many there are.
    pub fingerprint: String,
    pub files: usize,
    /// The ground it changes, in metres (x1, y1, x2, y2), null when none is known.
    pub zone: Option<[f32; 4]>,
    /// The same ground in 128 m blocks: rectangles (bx1, by1, bx2, by2), both ends included.
    pub blocks: Vec<[i32; 4]>,
    /// All its map files read (none protected or unreadable).
    pub readable: bool,
    /// Why not, or why it has no ground, in English.
    pub reason: Option<String>,
}

/// The last names of folders (`[resources] folders`), for the record: no path of the disk.
pub fn folder_names(dirs: &[String]) -> Vec<String> {
    dirs.iter()
        .map(|d| {
            let d = d.trim_end_matches(['/', '\\']);
            d.rsplit(['/', '\\']).next().unwrap_or(d).to_string()
        })
        .collect()
}

/// A resource as it is now: its name, folder and fingerprint.
#[derive(Clone, Debug, PartialEq)]
pub struct Print {
    pub name: String,
    pub dir: PathBuf,
    pub digest: String,
    pub files: usize,
}

/// The date and time now, UTC (`2026-11-15T14:03:22Z`).
pub fn now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    date(secs)
}

/// A date and time from seconds since 1970, UTC.
pub fn date(secs: u64) -> String {
    let (days, rest) = ((secs / 86_400) as i64, secs % 86_400);
    // the civil calendar from the days since 1970 (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + (month <= 2) as i64;
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest / 60 % 60, rest % 60)
}

/// What changes the whole map: `[maps]`, `[lighting]` and `[water]` of the settings in use.
pub fn settings() -> Value {
    let c = crate::config::get();
    let mut v = json!({
        "maps": serde_json::to_value(&c.maps).unwrap_or(Value::Null),
        "lighting": serde_json::to_value(&c.lighting).unwrap_or(Value::Null),
        "water": serde_json::to_value(&c.water).unwrap_or(Value::Null),
    });
    shortest(&mut v);
    v
}

/// The numbers of the settings (`f32`) written as they are typed (0.6, not 0.6000000238418579).
fn shortest(v: &mut Value) {
    match v {
        Value::Number(n) if n.is_f64() => {
            if let Some(x) = n
                .as_f64()
                .and_then(|x| (x as f32).to_string().parse::<f64>().ok())
                .and_then(serde_json::Number::from_f64)
            {
                *n = x;
            }
        }
        Value::Array(a) => a.iter_mut().for_each(shortest),
        Value::Object(o) => o.values_mut().for_each(shortest),
        _ => {}
    }
}

/// FNV-1a, 64 bits: the same on every machine and every version of the program.
fn fnv(hash: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *hash ^= *b as u64;
        *hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
}

/// Whether a file of a resource can change the map: its manifest, what it streams (`stream/`), its
/// scripts (`RemoveIpl`, `RequestIpl`) and its data files (texture parents…).
fn counts(rel: &str) -> bool {
    let ext = rel.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    rel == "fxmanifest.lua"
        || rel == "__resource.lua"
        || rel.starts_with("stream/")
        || matches!(ext, "lua" | "js" | "meta" | "xml" | "ide")
}

/// The fingerprint of a resource folder: its files that can change the map ([`counts`]), each by
/// its path from the folder (lower case, `/`), its size and its date (to the nanosecond), in the
/// order of the paths; and how many files. Not their content: a quick look, not a reading.
pub fn fingerprint(dir: &Path) -> (String, usize) {
    let mut files: BTreeMap<String, (u64, u128)> = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let path = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path.strip_prefix(dir).unwrap_or(&path).to_string_lossy().replace('\\', "/").to_lowercase();
            if counts(&rel) {
                let when = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                files.insert(rel, (meta.len(), when));
            }
        }
    }
    let mut hash = 0xCBF2_9CE4_8422_2325u64;
    for (rel, (size, when)) in &files {
        fnv(&mut hash, rel.as_bytes());
        fnv(&mut hash, &[0]);
        fnv(&mut hash, &size.to_le_bytes());
        fnv(&mut hash, &when.to_le_bytes());
    }
    (format!("{hash:016x}"), files.len())
}

/// The name of the resource folder `dir` under the resource folders of the settings (`folders`,
/// absolute): the last name of the folder it was found in, then its path from there
/// (`[my-island]/my-island-map`); a folder given as a resource itself keeps its own name.
pub fn name_of(dir: &Path, folders: &[PathBuf]) -> String {
    for f in folders {
        if let Ok(rest) = dir.strip_prefix(f) {
            let top = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let rest = rest.to_string_lossy().replace('\\', "/");
            return if rest.is_empty() { top } else { format!("{top}/{rest}") };
        }
    }
    dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// The resource folders of the settings, absolute.
pub fn folders() -> Vec<PathBuf> {
    crate::config::get()
        .resources
        .folders
        .iter()
        .map(|f| std::path::absolute(f).unwrap_or_else(|_| PathBuf::from(f)))
        .collect()
}

/// Every resource of the server as it is now, with its fingerprint (in the order of `server`).
pub fn prints(server: &crate::world::fivem::Server) -> Vec<Print> {
    let folders = folders();
    server
        .resources
        .iter()
        .map(|r| {
            let (digest, files) = fingerprint(&r.dir);
            Print { name: name_of(&r.dir, &folders), dir: r.dir.clone(), digest, files }
        })
        .collect()
}

/// The record of a map folder, None when it has none.
pub fn read(dir: &Path) -> Result<Option<Record>> {
    let path = dir.join(FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let record: Record = serde_json::from_str(&text).with_context(|| format!("{} is unreadable", path.display()))?;
    if record.format != FORMAT {
        bail!(
            "{}: unknown format \"{}\" (expected \"{FORMAT}\"): make it again with update --record",
            path.display(),
            record.format
        );
    }
    Ok(Some(record))
}

/// Writes the record of a map folder whole (beside, then renamed over).
pub fn write(dir: &Path, record: &Record) -> Result<()> {
    crate::tiles::format::write_whole(&dir.join(FILE), serde_json::to_string_pretty(record)?.as_bytes())
}

/// How a resource changed since the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Added,
    Changed,
    Removed,
}

impl State {
    /// The word of the event `changes`.
    pub fn word(self) -> &'static str {
        match self {
            State::Added => "added",
            State::Changed => "changed",
            State::Removed => "removed",
        }
    }

    /// The text of the lines.
    pub fn label(self) -> &'static str {
        match self {
            State::Added => "added",
            State::Changed => "changed",
            State::Removed => "removed",
        }
    }
}

/// A resource added, changed or removed: its entry in the record (changed, removed) and its place
/// among the resources now (added, changed).
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub name: String,
    pub state: State,
    pub old: Option<usize>,
    pub now: Option<usize>,
}

/// The resources added, changed or removed since `record`, by name: a resource renamed (or moved
/// to another folder of the settings) is one removed and one added. In the order of the names.
pub fn diff(record: &Record, now: &[Print]) -> Vec<Change> {
    let mut by_name: BTreeMap<&str, (Option<usize>, Option<usize>)> = BTreeMap::new();
    for (i, e) in record.resources.iter().enumerate() {
        by_name.entry(e.name.as_str()).or_default().0 = Some(i);
    }
    for (i, p) in now.iter().enumerate() {
        by_name.entry(p.name.as_str()).or_default().1 = Some(i);
    }
    by_name
        .into_iter()
        .filter_map(|(name, (old, new))| {
            let state = match (old, new) {
                (None, Some(_)) => State::Added,
                (Some(_), None) => State::Removed,
                (Some(o), Some(n))
                    if record.resources[o].fingerprint != now[n].digest
                        || record.resources[o].files != now[n].files =>
                {
                    State::Changed
                }
                _ => return None,
            };
            Some(Change { name: name.to_string(), state, old, now: new })
        })
        .collect()
}

/// The remark when what changes the whole map is not what the record says, None when it is.
pub fn settings_changed(record: &Record) -> Option<String> {
    let now = settings();
    if record.settings == now {
        return None;
    }
    let differ: Vec<&str> =
        ["maps", "lighting", "water"].into_iter().filter(|k| record.settings.get(*k) != now.get(*k)).collect();
    Some(format!(
        "the map settings changed: render the whole map again (they differ since {}: {})",
        record.date,
        differ.iter().map(|k| format!("[{k}]")).collect::<Vec<_>>().join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("btms-tests-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn dates_are_utc() {
        assert_eq!(date(0), "1970-01-01T00:00:00Z");
        assert_eq!(date(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(date(1_794_756_202), "2026-11-15T15:23:22Z");
        assert_eq!(date(4_107_542_399), "2100-02-28T23:59:59Z");
    }

    #[test]
    fn the_fingerprint_sees_what_changes_the_map_only() {
        let dir = scratch("fingerprint");
        write(&dir.join("fxmanifest.lua"), b"this_is_a_map 'yes'");
        write(&dir.join("stream").join("a.ymap"), b"RSC7aaaa");
        write(&dir.join("client").join("c.lua"), b"RemoveIpl('x')");
        write(&dir.join("html").join("ui.png"), b"png");
        let (a, n) = fingerprint(&dir);
        assert_eq!((a.len(), n), (16, 3));
        // what does not change the map: same fingerprint
        write(&dir.join("html").join("ui.png"), b"another picture");
        assert_eq!(fingerprint(&dir), (a.clone(), 3));
        // a file of the stream grows, one is added, one is renamed, its date changes: another one each time
        write(&dir.join("stream").join("a.ymap"), b"RSC7aaaaa");
        let (b, _) = fingerprint(&dir);
        assert_ne!(a, b);
        write(&dir.join("stream").join("sub").join("b.ytd"), b"RSC7");
        let (c, n) = fingerprint(&dir);
        assert!(c != b && n == 4);
        std::fs::rename(dir.join("stream").join("sub").join("b.ytd"), dir.join("stream").join("sub").join("c.ytd"))
            .unwrap();
        let (d, _) = fingerprint(&dir);
        assert!(d != c);
        let file = std::fs::File::options().write(true).open(dir.join("stream").join("a.ymap")).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000)).unwrap();
        drop(file);
        assert_ne!(fingerprint(&dir).0, d);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_are_those_under_the_folders_of_the_settings() {
        let folders = [PathBuf::from("C:/srv/[my-island]"), PathBuf::from("C:/srv/my_station")];
        assert_eq!(name_of(Path::new("C:/srv/[my-island]/my-island-map"), &folders), "[my-island]/my-island-map");
        assert_eq!(name_of(Path::new("C:/srv/[my-island]/[sub]/x"), &folders), "[my-island]/[sub]/x");
        assert_eq!(name_of(Path::new("C:/srv/my_station"), &folders), "my_station");
        assert_eq!(name_of(Path::new("D:/other/res"), &folders), "res");
    }

    #[test]
    fn the_record_tells_no_path_of_the_disk() {
        assert_eq!(
            folder_names(&[r"C:\Users\a\srv\[maps]".into(), "D:/srv/addons/".into(), "single".into()]),
            ["[maps]", "addons", "single"]
        );
        let json = serde_json::to_string(&entry("x", "9")).unwrap();
        assert!(!json.contains("C:/srv") && !json.contains("folder"), "{json}");
        let back: Entry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.folder, "");
    }

    fn entry(name: &str, fingerprint: &str) -> Entry {
        Entry {
            name: name.into(),
            folder: format!("C:/srv/{name}"),
            fingerprint: fingerprint.into(),
            files: 2,
            zone: None,
            blocks: vec![],
            readable: true,
            reason: None,
        }
    }

    fn print(name: &str, digest: &str) -> Print {
        Print { name: name.into(), dir: PathBuf::from(format!("C:/srv/{name}")), digest: digest.into(), files: 2 }
    }

    #[test]
    fn the_diff_finds_added_changed_removed_and_renamed() {
        let record = Record {
            format: FORMAT.into(),
            date: "2026-11-15T10:00:00Z".into(),
            by: "tiles".into(),
            settings: settings(),
            folders: vec![],
            on_demand: None,
            resources: vec![entry("a", "1"), entry("b", "2"), entry("c", "3"), entry("old-name", "4")],
        };
        let now = [print("a", "1"), print("b", "22"), print("d", "5"), print("new-name", "4")];
        let d = diff(&record, &now);
        let got: Vec<(&str, State, Option<usize>, Option<usize>)> =
            d.iter().map(|c| (c.name.as_str(), c.state, c.old, c.now)).collect();
        assert_eq!(
            got,
            [
                ("b", State::Changed, Some(1), Some(1)),
                ("c", State::Removed, Some(2), None),
                ("d", State::Added, None, Some(2)),
                ("new-name", State::Added, None, Some(3)),
                ("old-name", State::Removed, Some(3), None),
            ]
        );
        // the same number of files under another fingerprint, or the other way round: changed
        let mut other = now.clone();
        other[0].files = 3;
        assert!(diff(&record, &other).iter().any(|c| c.name == "a" && c.state == State::Changed));
        // nothing changed
        let same = [print("a", "1"), print("b", "2"), print("c", "3"), print("old-name", "4")];
        assert!(diff(&record, &same).is_empty());
        assert_eq!(
            (State::Added.word(), State::Changed.word(), State::Removed.word()),
            ("added", "changed", "removed")
        );
    }

    #[test]
    fn a_change_of_the_settings_is_told() {
        let mut record = Record {
            format: FORMAT.into(),
            date: "2026-11-15T10:00:00Z".into(),
            by: "tiles".into(),
            settings: settings(),
            folders: vec![],
            on_demand: Some(vec!["hei_carrier".into()]),
            resources: vec![],
        };
        assert_eq!(settings_changed(&record), None);
        record.settings["lighting"]["azimuth"] = json!(90.0);
        record.settings["water"]["enabled"] = json!(false);
        assert_eq!(
            settings_changed(&record).unwrap(),
            "the map settings changed: render the whole map again (they differ since 2026-11-15T10:00:00Z: [lighting], [water])"
        );
        // written and read back the same, floats included
        let dir = scratch("record");
        record.settings = settings();
        // (the folder of a resource is not written: read back, it is empty)
        record.resources = vec![Entry {
            zone: Some([-128.0, 256.5, 0.1, 384.0]),
            blocks: vec![[-1, 2, 0, 2]],
            readable: false,
            reason: Some("protected".into()),
            folder: String::new(),
            ..entry("x", "9")
        }];
        super::write(&dir, &record).unwrap();
        assert_eq!(read(&dir).unwrap(), Some(record.clone()));
        assert_eq!(settings_changed(&read(&dir).unwrap().unwrap()), None);
        assert_eq!(settings()["lighting"]["strength"], json!(0.6));
        assert!(!dir.join("resources.tmp").exists());
        // none, another format
        assert_eq!(read(&scratch("no-record")).unwrap(), None);
        std::fs::write(dir.join(FILE), r#"{"format": "other", "date": "", "by": "", "settings": null, "folders": [], "on_demand": null, "resources": []}"#).unwrap();
        assert!(format!("{:#}", read(&dir).unwrap_err()).contains("unknown format \"other\""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
