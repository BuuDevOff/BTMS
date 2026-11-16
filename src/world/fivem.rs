//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The server's own mappings: the FiveM resources of `[resources] folders` (`map.toml`), which
//! are not in the game's files.
//!
//! A resource is a folder with an `fxmanifest.lua`; a folder without one is searched for
//! resources (`[my-island]`). Every file under a resource's `stream/` folder is known to the game by
//! its name alone and wins over the game's file of that name (a placement file, an archetype file,
//! a model, a texture dictionary); a name the game does not have is new. Of two streamed files of
//! the same name, the later one wins: the folders in the order of `map.toml`, the resources of a
//! folder and the files of a resource in the order of their names.
//!
//! A streamed file is a file on disk, the same bytes an archive gives for it (`RSC7`…): it is
//! named in the index by [`PREFIX`], its place among the streamed files and its path, and read by
//! [`read`]. It is made by a modding tool, not by the game's own: what the game does not read in
//! it may be wrong ([`Resource::modded`]).
//!
//! What is protected is reported and left out, never decrypted: a file encrypted by FiveM's asset
//! escrow (it starts with `FXAP`), and a model (`.ydr`, `.ydd`, `.yft`) whose vertices are
//! scrambled in a file that otherwise reads ([`scrambled`]), which is what the models of the
//! resources under escrow look like. Such a model is not drawn, and the game's own model of
//! that name, if there is one, is drawn instead ([`usable`]). Textures, placement files and
//! archetype files read.
//!
//! What the resources remove:
//! - a placement file of the same name as one of the game replaces it;
//! - in such a file, the entities moved out of the world (the usual way to remove an entity
//!   without deleting it) and those that only cast a shadow are not shown ([`shown`]);
//! - a placement file a client script removes (`RemoveIpl` with its name written out) is not
//!   drawn ([`report`]).
//!
//! What they add besides plain entities: the entities of their interiors (`CMloInstanceDef`),
//! which a resource uses for whole buildings with glazed or open roofs ([`shown`]).
//!
//! All of it is said when the map loads ([`report`]) and listed by the `resources` command.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use rage_rpf::GtaKeys;

use crate::formats::resource::{Resource, joaat};
use crate::formats::ymap::{Archetype, parse_ymap, parse_ytyp};
use crate::game::index::Index;
use crate::util::profile::{Phase, time};
use crate::world::YmapInfo;

/// Start of the name of a streamed file in the index: `resource:<n>:<path on disk>`, lowercase
/// with `/` like every other path; `<n>` is the file's place among all the streamed files.
pub const PREFIX: &str = "resource:";
/// The rank of the first streamed file: above every copy the game holds (those stay under 1 000).
const RANK: usize = 100_000;
/// First bytes of a file protected by FiveM's asset escrow.
const ESCROW: &[u8] = b"FXAP";
/// The kinds of streamed files this tool reads: they must be `RSC7` resources.
pub const READ: [&str; 6] = ["ymap", "ytyp", "ydr", "ydd", "yft", "ytd"];
/// Under this height an entity is out of the world: the way a resource removes an entity of a
/// placement file it replaces without deleting it (for example to z ≈ −222 230). The lowest
/// placement of the game itself is far above (the `resources` command prints it).
pub const OUT_OF_WORLD_Z: f32 = -2000.0;
/// `RemoveIpl` and `RequestIpl` as `Citizen.InvokeNative` names them.
const REMOVE_IPL: u64 = 0xEE6C_5AD3_ECE0_A82D;
const REQUEST_IPL: u64 = 0x41B4_8938_43BB_DB74;

/// A resource of the server.
pub struct ServerResource {
    pub name: String,
    pub dir: PathBuf,
    /// `this_is_a_map 'yes'`.
    pub map: bool,
}

/// A file a resource streams.
pub struct Streamed {
    /// Its name in the index ([`PREFIX`]).
    pub path: String,
    /// Index of its resource in [`Server::resources`].
    pub resource: usize,
}

/// A call of `RemoveIpl` or `RequestIpl` in a client script.
pub struct IplCall {
    /// True for `RemoveIpl`.
    pub remove: bool,
    /// The name, when it is written in the call (else it is computed by the script: unknown).
    pub name: Option<String>,
    pub file: PathBuf,
    pub line: usize,
}

/// The resources of the server and what they hold.
#[derive(Default)]
pub struct Server {
    pub resources: Vec<ServerResource>,
    /// Every streamed file that can be read, a later one winning over an earlier of the same name.
    pub streamed: Vec<Streamed>,
    /// Texture parent files (`data_file 'GTXD_PARENTING_DATA'`), on disk.
    pub gtxd: Vec<PathBuf>,
    /// `RemoveIpl` / `RequestIpl` calls of the client scripts.
    pub calls: Vec<IplCall>,
    /// Calls of other natives that hide or swap models (`CreateModelHide`…): seen, not applied.
    pub other_hides: Vec<(String, PathBuf, usize)>,
    /// Files left out: protected, unreadable, not followed. (file, why)
    pub skipped: Vec<(PathBuf, String)>,
    /// Folders of `map.toml` that do not exist.
    pub missing: Vec<String>,
}

static SERVER: OnceLock<Server> = OnceLock::new();

/// The resources of `map.toml`, looked for once.
pub fn server() -> &'static Server {
    SERVER.get_or_init(|| Server::scan(&crate::config::get().resources.folders))
}

/// The rank of a streamed file among the copies of a file (a later one wins, all above the
/// game's), None for a file of the game.
pub fn rank(path: &str) -> Option<usize> {
    let (n, _) = path.strip_prefix(PREFIX)?.split_once(':')?;
    Some(RANK + n.parse::<usize>().ok()?)
}

/// Whether `path` names a streamed file.
pub fn is_streamed(path: &str) -> bool {
    path.starts_with(PREFIX)
}

/// The name of a streamed file in the index.
fn index_path(n: usize, disk: &Path) -> String {
    format!("{PREFIX}{n:06}:{}", disk.to_string_lossy().replace('\\', "/").to_lowercase())
}

/// The file on disk a lowercase path names: as is where names ignore case (Windows), else found
/// by comparing the names of each folder without case.
fn disk_file(lower: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(lower);
    if direct.is_file() {
        return Some(direct);
    }
    let mut at = PathBuf::new();
    for (i, part) in lower.split('/').enumerate() {
        if i == 0 && (part.is_empty() || part.ends_with(':')) {
            at.push(format!("{part}/"));
            continue;
        }
        let exact = at.join(part);
        if exact.exists() {
            at = exact;
            continue;
        }
        at = std::fs::read_dir(&at)
            .ok()?
            .flatten()
            .find(|e| e.file_name().to_string_lossy().to_lowercase() == part)?
            .path();
    }
    at.is_file().then_some(at)
}

/// The bytes of a file on disk (a resource file keeps its `RSC7` header, as an archive gives it).
/// A protected file is refused.
pub fn read_disk(file: &Path) -> Result<Vec<u8>> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    if bytes.starts_with(ESCROW) {
        bail!("{} is protected (FiveM escrow, FXAP header): not read", file.display());
    }
    Ok(bytes)
}

/// The bytes of the streamed file `path` ([`PREFIX`]).
pub fn read(path: &str) -> Result<Vec<u8>> {
    let lower = path
        .strip_prefix(PREFIX)
        .and_then(|p| p.split_once(':'))
        .map(|(_, p)| p)
        .with_context(|| format!("{path} is not a resource file"))?;
    let file = disk_file(lower).with_context(|| format!("{lower} not found on disk"))?;
    read_disk(&file)
}

/// The resource (`RSC7`) held by the bytes of the file `path`, marked as made by a modding tool
/// when a server resource streams it ([`Resource::modded`]).
pub fn resource(path: &str, bytes: &[u8]) -> Result<Resource> {
    Ok(Resource::parse(bytes)?.modded(is_streamed(path)))
}

/// The resource a streamed file belongs to, and its path inside it (for messages).
pub fn describe(path: &str) -> String {
    let s = server();
    let lower = path.strip_prefix(PREFIX).and_then(|p| p.split_once(':')).map(|(_, p)| p).unwrap_or(path);
    for r in &s.resources {
        let root = r.dir.to_string_lossy().replace('\\', "/").to_lowercase();
        if let Some(rest) = lower.strip_prefix(&format!("{root}/")) {
            return format!("{}/{rest}", r.name);
        }
    }
    lower.to_string()
}

// ---------------------------------------------------------------------------------------------
// Lua (and JavaScript) read as text: enough to find what a manifest declares and the calls of a
// native with a literal name. Nothing is run.

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Ident(String),
    Text(String),
    Number(String),
    Other(char),
}

/// The tokens of a script with their line, comments left out (`--`, `--[[ ]]`; for JavaScript
/// `//` and `/* */`).
fn tokens(src: &str, javascript: bool) -> Vec<(Token, usize)> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line) = (0usize, 1usize);
    let at = |i: usize, s: &str| s.chars().enumerate().all(|(k, ch)| c.get(i + k) == Some(&ch));
    while i < c.len() {
        let ch = c[i];
        if ch == '\n' {
            line += 1;
            i += 1;
        } else if ch.is_whitespace() {
            i += 1;
        } else if (!javascript && at(i, "--[[")) || (javascript && at(i, "/*")) {
            let end = if javascript { "*/" } else { "]]" };
            while i < c.len() && !at(i, end) {
                line += (c[i] == '\n') as usize;
                i += 1;
            }
            i += 2;
        } else if (!javascript && at(i, "--")) || (javascript && at(i, "//")) {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '\'' || ch == '"' || (javascript && ch == '`') {
            let (start, mut text) = (line, String::new());
            i += 1;
            while i < c.len() && c[i] != ch {
                if c[i] == '\\' && i + 1 < c.len() {
                    i += 1;
                }
                line += (c[i] == '\n') as usize;
                text.push(c[i]);
                i += 1;
            }
            i += 1;
            out.push((Token::Text(text), start));
        } else if !javascript && at(i, "[[") {
            let (start, mut text) = (line, String::new());
            i += 2;
            while i < c.len() && !at(i, "]]") {
                line += (c[i] == '\n') as usize;
                text.push(c[i]);
                i += 1;
            }
            i += 2;
            out.push((Token::Text(text), start));
        } else if ch.is_alphabetic() || ch == '_' {
            let start = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            out.push((Token::Ident(c[start..i].iter().collect()), line));
        } else if ch.is_ascii_digit() {
            let start = i;
            while i < c.len() && (c[i].is_ascii_alphanumeric() || c[i] == '.') {
                i += 1;
            }
            out.push((Token::Number(c[start..i].iter().collect()), line));
        } else {
            out.push((Token::Other(ch), line));
            i += 1;
        }
    }
    out
}

/// What a manifest declares: each directive with the texts that follow it, in a table or not
/// (`data_file 'A' 'b'`, `client_scripts { 'a.lua', 'b.lua' }`, `this_is_a_map 'yes'`).
fn directives(lua: &str) -> Vec<(String, Vec<String>)> {
    let t = tokens(lua, false);
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        let Token::Ident(name) = &t[i].0 else {
            i += 1;
            continue;
        };
        let mut texts = Vec::new();
        let mut j = i + 1;
        while j < t.len() {
            match &t[j].0 {
                Token::Text(s) => texts.push(s.clone()),
                Token::Other('(' | ')' | ',') => {}
                Token::Other('{') => {
                    let mut depth = 1;
                    j += 1;
                    while j < t.len() && depth > 0 {
                        match &t[j].0 {
                            Token::Other('{') => depth += 1,
                            Token::Other('}') => depth -= 1,
                            Token::Text(s) => texts.push(s.clone()),
                            _ => {}
                        }
                        j += 1;
                    }
                    break;
                }
                _ => break,
            }
            j += 1;
        }
        if !texts.is_empty() {
            out.push((name.clone(), texts));
        }
        i = j.max(i + 1);
    }
    out
}

/// An IPL call found in a script: (removes it, the IPL's name when it is a literal, line).
type FoundIpl = (bool, Option<String>, usize);

/// The calls of natives that load, remove or hide map content in a script.
fn native_calls(src: &str, javascript: bool) -> (Vec<FoundIpl>, Vec<(String, usize)>) {
    let t = tokens(src, javascript);
    let (mut ipl, mut other) = (Vec::new(), Vec::new());
    let tok = |i: usize| t.get(i).map(|x| &x.0);
    for (i, item) in t.iter().enumerate() {
        let Token::Ident(name) = &item.0 else { continue };
        if tok(i + 1) != Some(&Token::Other('(')) {
            continue;
        }
        let line = item.1;
        // the argument holding the name, and whether the call removes
        let (remove, arg) = match name.as_str() {
            "RemoveIpl" => (true, i + 2),
            "RequestIpl" => (false, i + 2),
            "InvokeNative" => {
                let Some(Token::Number(n)) = tok(i + 2) else { continue };
                match u64::from_str_radix(n.trim_start_matches("0x").trim_start_matches("0X"), 16) {
                    Ok(REMOVE_IPL) => (true, i + 4),
                    Ok(REQUEST_IPL) => (false, i + 4),
                    _ => continue,
                }
            }
            "CreateModelHide" | "CreateModelHideExcludingScriptObjects" | "CreateModelSwap" | "SetBuildingState" => {
                other.push((name.clone(), line));
                continue;
            }
            _ => continue,
        };
        // a literal name is a text alone in its argument
        let literal = match (tok(arg), tok(arg + 1)) {
            (Some(Token::Text(s)), Some(Token::Other(')' | ','))) => Some(s.clone()),
            _ => None,
        };
        ipl.push((remove, literal, line));
    }
    (ipl, other)
}

/// Whether `name` matches `pattern`, `*` standing for any run of characters.
fn wild(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    fn go(p: &[char], n: &[char]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some(('*', rest)) => (0..=n.len()).any(|i| go(rest, &n[i..])),
            Some((c, rest)) => n.split_first().is_some_and(|(d, tail)| c == d && go(rest, tail)),
        }
    }
    go(&p, &n)
}

/// Whether the path `path` (folders and name) matches a manifest pattern (`*` in a name, `**` for
/// any folders).
fn glob(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|i| glob(rest, &path[i..])),
        Some((seg, rest)) => path.split_first().is_some_and(|(p, tail)| wild(seg, p) && glob(rest, tail)),
    }
}

/// Every file under `dir`, in the order of the names (folders after the files of their parent).
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    entries.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default());
    let (dirs, files): (Vec<PathBuf>, Vec<PathBuf>) = entries.into_iter().partition(|p| p.is_dir());
    out.extend(files);
    for d in dirs {
        walk(&d, out);
    }
}

/// The manifest of a resource folder, if it is one.
fn manifest(dir: &Path) -> Option<PathBuf> {
    ["fxmanifest.lua", "__resource.lua"].iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// The resources under `dir`: itself, or the folders it holds (searched in the order of their
/// names, down to 6 levels).
fn find_resources(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if manifest(dir).is_some() {
        out.push(dir.to_path_buf());
        return;
    }
    if depth >= 6 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default());
    for d in dirs {
        find_resources(&d, depth + 1, out);
    }
}

impl Server {
    /// Looks for the resources of `folders` and lists what they stream and declare.
    pub fn scan(folders: &[String]) -> Server {
        let mut s = Server::default();
        let mut dirs = Vec::new();
        for f in folders {
            let dir = std::path::absolute(f).unwrap_or_else(|_| PathBuf::from(f));
            if !dir.is_dir() {
                s.missing.push(f.clone());
                continue;
            }
            let before = dirs.len();
            find_resources(&dir, 0, &mut dirs);
            if dirs.len() == before {
                s.missing.push(format!("{f} (no resource: no fxmanifest.lua)"));
            }
        }
        let mut streamed: Vec<(PathBuf, usize)> = Vec::new();
        for dir in dirs {
            let resource = s.resources.len();
            let text = manifest(&dir)
                .and_then(|m| std::fs::read(m).ok())
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default();
            let directives = directives(&text);
            let declared = |names: &[&str]| -> Vec<&String> {
                directives.iter().filter(|(d, _)| names.contains(&d.as_str())).flat_map(|(_, t)| t).collect()
            };
            // texture parents
            for d in
                directives.iter().filter(|(d, t)| d == "data_file" && t.len() == 2 && t[0] == "GTXD_PARENTING_DATA")
            {
                let file = dir.join(&d.1[1]);
                if file.is_file() {
                    s.gtxd.push(file);
                } else {
                    s.skipped.push((file, "declared by the manifest (GTXD_PARENTING_DATA), missing".into()));
                }
            }
            // client scripts: what they remove or load
            let patterns: Vec<String> =
                declared(&["client_script", "client_scripts", "shared_script", "shared_scripts"])
                    .into_iter()
                    .filter(|p| !p.starts_with('@'))
                    .map(|p| p.replace('\\', "/").to_lowercase())
                    .collect();
            let mut all = Vec::new();
            walk(&dir, &mut all);
            for file in &all {
                let rel = file.strip_prefix(&dir).unwrap_or(file).to_string_lossy().replace('\\', "/").to_lowercase();
                let parts: Vec<&str> = rel.split('/').collect();
                if !patterns
                    .iter()
                    .any(|p| glob(&p.split('/').filter(|x| !x.is_empty() && *x != ".").collect::<Vec<_>>(), &parts))
                {
                    continue;
                }
                let Ok(bytes) = std::fs::read(file) else {
                    s.skipped.push((file.clone(), "unreadable client script".into()));
                    continue;
                };
                if bytes.starts_with(ESCROW) {
                    s.skipped.push((
                        file.clone(),
                        "protected client script (FiveM escrow, FXAP header): what it removes is not known".into(),
                    ));
                    continue;
                }
                let Ok(text) = std::str::from_utf8(&bytes) else {
                    s.skipped.push((
                        file.clone(),
                        "unreadable client script (binary, neither text nor FXAP): what it removes is not known".into(),
                    ));
                    continue;
                };
                let (ipl, other) = native_calls(text, rel.ends_with(".js"));
                s.calls.extend(ipl.into_iter().map(|(remove, name, line)| IplCall {
                    remove,
                    name,
                    file: file.clone(),
                    line,
                }));
                s.other_hides.extend(other.into_iter().map(|(n, line)| (n, file.clone(), line)));
            }
            // streamed files
            let stream = dir.join("stream");
            let mut files = Vec::new();
            walk(&stream, &mut files);
            for file in files {
                let ext = file.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
                let mut head = [0u8; 4];
                let got =
                    std::fs::File::open(&file).and_then(|mut f| std::io::Read::read(&mut f, &mut head)).unwrap_or(0);
                if got == 4 && head == *ESCROW {
                    s.skipped.push((file, "protected (FiveM escrow, FXAP header)".into()));
                } else if ext == "rpf" {
                    s.skipped.push((file, ".rpf archive in stream: not followed".into()));
                } else if READ.contains(&ext.as_str()) && (got < 4 || head != *b"RSC7") {
                    s.skipped.push((file, "not an RSC7 resource".into()));
                } else {
                    streamed.push((file, resource));
                }
            }
            s.resources.push(ServerResource {
                name: dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                map: directives
                    .iter()
                    .any(|(d, t)| d == "this_is_a_map" && t.first().is_some_and(|v| v != "no" && v != "false")),
                dir,
            });
        }
        // a later file wins; `BTMS_RESOURCE_ORDER=first-wins` turns the order round (a check)
        let first_wins = std::env::var("BTMS_RESOURCE_ORDER").is_ok_and(|v| v == "first-wins");
        let total = streamed.len();
        s.streamed = streamed
            .into_iter()
            .enumerate()
            .map(|(i, (file, resource))| Streamed {
                path: index_path(if first_wins { total - 1 - i } else { i }, &file),
                resource,
            })
            .collect();
        s
    }

    /// The names (hashes) the client scripts load with a literal name (`RequestIpl`): placement
    /// files that are on the server even if the game alone does not load them.
    pub fn requested(&self) -> HashSet<u32> {
        self.calls.iter().filter(|c| !c.remove).filter_map(|c| c.name.as_deref()).map(joaat).collect()
    }

    /// The streamed files by name: (name with extension) → their paths, the winning one last.
    pub fn by_name(&self) -> HashMap<&str, Vec<&str>> {
        let mut map: HashMap<&str, Vec<&str>> = HashMap::new();
        for f in &self.streamed {
            map.entry(f.path.rsplit('/').next().unwrap_or(&f.path)).or_default().push(&f.path);
        }
        for v in map.values_mut() {
            v.sort_by_key(|p| rank(p));
        }
        map
    }
}

// ---------------------------------------------------------------------------------------------
// The resources in the map.

/// Whether the file `hash`.`ext` the game uses is one a resource streams (the game's own copies of
/// that name are then never loaded).
pub fn replaces(index: &Index, hash: u32, ext: &str) -> bool {
    index.copies(hash, ext).first().is_some_and(|p| is_streamed(p))
}

/// Whether the vertices of a model are scrambled: its file is a plain `RSC7` and its structures
/// read, but its vertex buffers hold random numbers instead of positions. That is what the models
/// (`.ydr`, `.ydd`, `.yft`) of the resources under FiveM's asset escrow look like, while those of
/// a resource whose manifest takes them out of it (`escrow_ignore`) are sound: a protection, as
/// far as the files tell. Such a model is reported and left out, never unscrambled.
///
/// Told by a single vertex that cannot be a position: a coordinate that is not a number, beyond
/// 100 km, or more than a kilometre outside the model's own box. Some models are only scrambled
/// in part (one vertex in 2 871 of `cs1_16_roadz04.ydr`), and one such vertex draws a streak
/// across a whole tile. A model whose box is merely out of date (vertices some metres outside,
/// as some resource models have) is kept.
pub fn scrambled(d: &crate::formats::ydr::Drawable) -> bool {
    const FAR: f32 = 1000.0;
    let lost = |p: &[f32; 3]| {
        (0..3).any(|k| !p[k].is_finite() || p[k].abs() > 1e5 || p[k] < d.bb_min[k] - FAR || p[k] > d.bb_max[k] + FAR)
    };
    d.lods.iter().flatten().flat_map(|m| &m.geometries).flat_map(|g| &g.positions).any(lost)
}

/// Whether some vertex of a model lies outside its own box by more than half the box's size plus
/// a metre, the model not being scrambled: a box out of date, to look at.
pub fn outside_its_box(d: &crate::formats::ydr::Drawable) -> bool {
    let margin: Vec<f32> = (0..3).map(|k| (d.bb_max[k] - d.bb_min[k]).abs() * 0.5 + 1.0).collect();
    let out = |p: &[f32; 3]| (0..3).any(|k| !(p[k] >= d.bb_min[k] - margin[k] && p[k] <= d.bb_max[k] + margin[k]));
    !scrambled(d) && d.lods.iter().flatten().flat_map(|m| &m.geometries).flat_map(|g| &g.positions).any(out)
}

/// The streamed models found scrambled so far (by path), checked once each.
static SCRAMBLED: std::sync::Mutex<Option<HashMap<String, bool>>> = std::sync::Mutex::new(None);
/// Those of them that were asked for while drawing, and left out.
static LEFT_OUT: std::sync::Mutex<Option<HashSet<String>>> = std::sync::Mutex::new(None);

/// Whether the streamed model `path` (its resource `res`) is scrambled ([`scrambled`]).
pub fn is_scrambled(path: &str, res: &Resource) -> bool {
    if let Some(known) = SCRAMBLED.lock().unwrap().get_or_insert_with(HashMap::new).get(path) {
        return *known;
    }
    let found = match path.rsplit('.').next() {
        Some("ydr") => crate::formats::ydr::parse_ydr(res).is_ok_and(|d| scrambled(&d)),
        Some("yft") => crate::formats::ydr::parse_yft(res).is_ok_and(|d| scrambled(&d)),
        Some("ydd") => crate::formats::ydr::parse_ydd(res).is_ok_and(|l| l.iter().any(|(_, d)| scrambled(d))),
        _ => false,
    };
    SCRAMBLED.lock().unwrap().get_or_insert_with(HashMap::new).insert(path.to_string(), found);
    found
}

/// The file `hash`.`ext` as it can be drawn: the copy the game uses, read. When that copy is
/// streamed by a resource and cannot be used (unreadable, or a model with scrambled vertices), the
/// next copy is taken: in the end the game's own file of that name, if it has one.
pub fn usable(game: &Path, keys: &GtaKeys, index: &Index, hash: u32, ext: &str) -> Option<Resource> {
    for path in index.copies(hash, ext) {
        // (counted and timed for `--profile`, as every file read to draw: `render::assets::Assets::read`)
        crate::util::profile::file_read(path);
        let res = crate::cli::archive::read_file(game, path, keys)
            .ok()
            .and_then(|b| time(Phase::Inflate, || resource(path, &b).ok()));
        if !is_streamed(path) {
            return res;
        }
        match res {
            Some(r) if !time(Phase::Models, || is_scrambled(path, &r)) => return Some(r),
            Some(_) => {
                LEFT_OUT.lock().unwrap().get_or_insert_with(HashSet::new).insert(path.clone());
            }
            None => {}
        }
    }
    None
}

/// Says how many streamed models were found scrambled while drawing (nothing when none was).
pub fn say_scrambled(index: &Index) {
    let guard = LEFT_OUT.lock().unwrap();
    let found: Vec<&String> = guard.iter().flatten().collect();
    if found.is_empty() {
        return;
    }
    let with_game = found
        .iter()
        .filter(|p| {
            let name = p.rsplit('/').next().unwrap_or(p);
            name.rsplit_once('.')
                .is_some_and(|(stem, ext)| index.copies(joaat(stem), ext).iter().any(|c| !is_streamed(c)))
        })
        .count();
    println!(
        "protected models: {} resource models asked for by this render have scrambled vertices (protected, probably by FiveM escrow) and are not drawn; for {with_game} of them, the game's model of the same name is drawn instead",
        found.len()
    );
}

/// The flags of the archetypes of a `.ytyp` (`CBaseArchetypeDef.flags`): name → flags.
pub fn archetype_flags(res: &Resource) -> Result<HashMap<u32, u32>> {
    let meta = crate::formats::meta::Meta::parse(res)?;
    let types = meta.root("CMapTypes")?;
    let mut out = HashMap::new();
    for a in meta.pointer_array(types.array("archetypes")?)? {
        out.insert(a.u32("name")?, a.u32("flags")?);
    }
    Ok(out)
}

/// Whether a resource moved this entity out of the world (see [`OUT_OF_WORLD_Z`]).
pub fn out_of_world(e: &crate::formats::ymap::Entity) -> bool {
    e.position[2] < OUT_OF_WORLD_Z
}

/// `lodLevel` given to an entity that is not shown: not a level of the game, so that nothing
/// draws it, and its place in the list stays (the entities of other files name it by index).
pub const NOT_SHOWN: u32 = 0xFFFF;
/// `flags` of an archetype that only casts a shadow (`CBaseArchetypeDef.flags`, bit 11): it is
/// not seen. The game's `dt1_19_shadow` and `dt1_19_cp_01_shadow` (the police station's) bear it.
const SHADOW_ONLY: u32 = 0x800;

/// What the resources brought to the map.
#[derive(Default)]
pub struct Added {
    pub ytyps: usize,
    /// Archetypes of the resources, and how many of them the game already had under that name.
    pub archetypes: usize,
    pub archetypes_replaced: usize,
    pub ymaps: usize,
    /// Placement files of the resources that take the place of one of the game.
    pub ymaps_replaced: usize,
    /// Of those, the ones left without any entity where the game's had some: they only remove.
    pub ymaps_emptied: usize,
    pub gtxd: usize,
    /// Entities of the resources' placement files that are not shown, by file (its path): their
    /// places in the file's list.
    pub not_shown: HashMap<String, Vec<u32>>,
    /// The interiors the resources define (`CMloArchetypeDef` of their `.ytyp`), by name: what
    /// they are made of ([`crate::formats::mlo`]).
    pub interiors: HashMap<u32, crate::formats::mlo::Mlo>,
    /// Interiors of the resources placed by their placement files, and the entities inside them
    /// that are added to the map ([`shown`]).
    pub interiors_placed: usize,
    pub inside: usize,
    /// Interiors left out because they belong to a resource whose models are protected.
    pub interiors_protected: usize,
    /// How many of them were moved out of the world, and how many only cast a shadow.
    pub moved_out: usize,
    pub shadow_only: usize,
    /// Placement files whose declared box was smaller than their entities' and was widened.
    pub widened: usize,
    /// Files that could not be read: (path, why).
    pub failures: Vec<(String, String)>,
}

/// Adds what the resources stream to the map being built: their archetypes (`.ytyp`), their
/// placement files (`.ymap`) and their texture parents. Of several streamed files of one name,
/// only the winning one exists for the game. `gtxd`: (rank, path, bytes).
///
/// In the resources' placement files, the entities the game does not show are noted
/// ([`Added::not_shown`], applied by [`shown`]): those a resource moved out of the world instead of
/// deleting them ([`OUT_OF_WORLD_Z`]), and those that only cast a shadow. The latter matter here
/// because a resource that moves a building away may leave its shadow stand-ins in place.
pub fn add_to_world(
    game: &Path,
    keys: &GtaKeys,
    index: &Index,
    archetypes: &mut HashMap<u32, (Archetype, String)>,
    ymaps: &mut Vec<YmapInfo>,
    gtxd: &mut Vec<(usize, String, Vec<u8>)>,
) -> Added {
    let s = server();
    let mut added = Added::default();
    // the winning copy of each streamed file of a kind
    let winners = |kind: &str| -> Vec<(u32, &str)> {
        s.streamed
            .iter()
            .filter_map(|f| {
                let name = f.path.rsplit('/').next().unwrap_or(&f.path);
                let (stem, ext) = name.rsplit_once('.')?;
                let hash = joaat(stem);
                (ext == kind && index.copies(hash, ext).first() == Some(&f.path)).then_some((hash, f.path.as_str()))
            })
            .collect()
    };
    // archetypes first: the placement files need them all
    let mut flags: HashMap<String, HashMap<u32, u32>> = HashMap::new();
    let mut interior_resource: HashMap<u32, usize> = HashMap::new();
    // (each file read and parsed on any thread, then taken in the order of the files, as before)
    let ytyps = winners("ytyp");
    let parsed = crate::util::parallel::map(&ytyps, |(_, path)| {
        read(path)
            .and_then(|b| Resource::parse(&b))
            .and_then(|r| Ok((parse_ytyp(&r)?, archetype_flags(&r)?, crate::formats::mlo::parse_mlos(&r)?)))
    });
    for ((_, path), parsed) in ytyps.into_iter().zip(parsed) {
        match parsed {
            Ok((list, f, interiors)) => {
                added.ytyps += 1;
                let resource = s.streamed.iter().find(|x| x.path == path).map(|x| x.resource).unwrap_or(usize::MAX);
                interior_resource.extend(interiors.iter().map(|m| (m.name, resource)));
                added.interiors.extend(interiors.into_iter().map(|m| (m.name, m)));
                flags.insert(path.to_string(), f);
                let rank = index.rank(path);
                for a in list {
                    let old = archetypes.get(&a.name).map(|(_, p)| p.as_str());
                    if old.is_none_or(|p| index.rank(p) <= rank) {
                        added.archetypes += 1;
                        added.archetypes_replaced += old.is_some_and(|p| !is_streamed(p)) as usize;
                        archetypes.insert(a.name, (a, path.to_string()));
                    }
                }
            }
            Err(e) => added.failures.push((path.to_string(), format!("{e:#}"))),
        }
    }
    // the interiors of a resource whose models are protected are not placed: the buildings
    // around them cannot be drawn, and their furniture (often the game's own props, which a roof
    // hides in the game) would lie alone on the ground. A resource is told protected by the first
    // three models it streams for one of its interiors.
    // (each interior looked at on any thread: what comes out is a set)
    let interiors: Vec<&crate::formats::mlo::Mlo> = added.interiors.values().collect();
    let is_protected = crate::util::parallel::map(&interiors, |mlo| {
        let models = mlo.entities.iter().filter_map(|c| {
            let (a, _) = archetypes.get(&c.archetype)?;
            let (file, ext) = match a.asset_type {
                crate::formats::ymap::ASSET_FRAGMENT => (a.asset_name, "yft"),
                crate::formats::ymap::ASSET_DRAWABLE_DICTIONARY => (a.drawable_dictionary, "ydd"),
                _ => (a.asset_name, "ydr"),
            };
            index.copies(file, ext).first().filter(|p| is_streamed(p))
        });
        let mut seen = HashSet::new();
        models
            .filter(|p| seen.insert(p.as_str()))
            .take(3)
            .any(|p| read(p).and_then(|b| resource(p, &b)).is_ok_and(|r| is_scrambled(p, &r)))
    });
    let protected: HashSet<usize> = interiors
        .iter()
        .zip(is_protected)
        .filter(|(_, protected)| *protected)
        .filter_map(|(mlo, _)| interior_resource.get(&mlo.name).copied())
        .collect();
    let before = added.interiors.len();
    added.interiors.retain(|name, _| !interior_resource.get(name).is_some_and(|r| protected.contains(r)));
    added.interiors_protected = before - added.interiors.len();
    // the placement files read and parsed on any thread, with the entity count of the game's copy
    // they replace; then the archetype files of the game their entities need, read on any thread
    // too (`shadow_only` below read them one after the other). They are only known to
    // `shadow_only` when it would have read them: a placement file met before still does not see
    // them, as before
    let ymap_files = winners("ymap");
    let parsed = crate::util::parallel::map(&ymap_files, |&(hash, path)| -> Result<_> {
        let m = read(path).and_then(|b| Resource::parse(&b)).and_then(|r| parse_ymap(&r))?;
        let game_copy = index.copies(hash, "ymap").iter().find(|p| !is_streamed(p));
        let before = game_copy.map(|g| {
            crate::cli::archive::read_file(game, g, keys)
                .and_then(|b| Resource::parse(&b))
                .and_then(|r| parse_ymap(&r))
                .map(|g| g.entities.len())
        });
        Ok((m, game_copy, before))
    });
    let mut needed: Vec<&String> = Vec::new();
    let mut asked = HashSet::new();
    for (m, _, _) in parsed.iter().filter_map(|p| p.as_ref().ok()).filter(|(_, game_copy, _)| game_copy.is_some()) {
        for e in m.entities.iter().filter(|e| !out_of_world(e)) {
            if let Some((_, ytyp)) = archetypes.get(&e.archetype).filter(|(_, ytyp)| !flags.contains_key(ytyp))
                && asked.insert(ytyp)
            {
                needed.push(ytyp);
            }
        }
    }
    let read_flags = crate::util::parallel::map(&needed, |ytyp| {
        crate::cli::archive::read_file(game, ytyp, keys)
            .and_then(|b| Resource::parse(&b))
            .and_then(|r| archetype_flags(&r))
            .unwrap_or_default()
    });
    let mut read_before: HashMap<String, HashMap<u32, u32>> = needed.into_iter().cloned().zip(read_flags).collect();
    // whether an archetype only casts a shadow: the flags of the file that defines it, read once.
    // An archetype file of the game is only read (`read_game`) for what a resource left of a
    // placement file of the game: opening its archive each time is slow, and a resource's own
    // placement files do not place the game's shadow stand-ins.
    let mut shadow_only = |archetypes: &HashMap<u32, (Archetype, String)>, name: u32, read_game: bool| -> bool {
        let Some((_, ytyp)) = archetypes.get(&name) else { return false };
        if !flags.contains_key(ytyp) {
            if !read_game {
                return false;
            }
            let read = read_before.remove(ytyp).unwrap_or_else(|| {
                crate::cli::archive::read_file(game, ytyp, keys)
                    .and_then(|b| Resource::parse(&b))
                    .and_then(|r| archetype_flags(&r))
                    .unwrap_or_default()
            });
            flags.insert(ytyp.clone(), read);
        }
        flags[ytyp].get(&name).is_some_and(|f| f & SHADOW_ONLY != 0)
    };
    for (&(hash, path), parsed) in ymap_files.iter().zip(parsed) {
        let (m, game_copy, before) = match parsed {
            Ok(read) => read,
            Err(e) => {
                added.failures.push((path.to_string(), format!("{e:#}")));
                continue;
            }
        };
        added.ymaps += 1;
        if let Some(before) = before {
            added.ymaps_replaced += 1;
            added.ymaps_emptied += (m.entities.is_empty() && before.is_ok_and(|n| n > 0)) as usize;
        }
        // the entities the game does not show, and the box of those it shows
        let mut hidden = Vec::new();
        let (mut lo, mut hi) = (m.entities_min, m.entities_max);
        let mut wider = false;
        for (i, e) in m.entities.iter().enumerate() {
            if out_of_world(e) {
                added.moved_out += 1;
                hidden.push(i as u32);
            } else if shadow_only(archetypes, e.archetype, game_copy.is_some()) {
                added.shadow_only += 1;
                hidden.push(i as u32);
            } else if let Some((a, _)) =
                archetypes.get(&e.archetype).filter(|_| !e.interior && matches!(e.lod_level, 0 | 5))
            {
                let (blo, bhi) = crate::formats::ymap::world_box(e, a, true);
                if blo.iter().chain(&bhi).all(|v| v.is_finite()) {
                    for k in 0..3 {
                        wider |= blo[k] < lo[k] - 1.0 || bhi[k] > hi[k] + 1.0;
                        lo[k] = lo[k].min(blo[k]);
                        hi[k] = hi[k].max(bhi[k]);
                    }
                }
            }
        }
        // the entities of its interiors count in its box, and in what the resources brought
        for e in m.entities.iter().filter(|e| !out_of_world(e)) {
            let rooms = inside(&added, e);
            added.interiors_placed += !rooms.is_empty() as usize;
            added.inside += rooms.len();
            for c in &rooms {
                let Some((a, _)) = archetypes.get(&c.archetype) else { continue };
                let (blo, bhi) = crate::formats::ymap::world_box(c, a, true);
                if blo.iter().chain(&bhi).all(|v| v.is_finite()) {
                    for k in 0..3 {
                        lo[k] = lo[k].min(blo[k]);
                        hi[k] = hi[k].max(bhi[k]);
                    }
                }
            }
        }
        added.widened += wider as usize;
        if !hidden.is_empty() {
            added.not_shown.insert(path.to_string(), hidden);
        }
        // (its parent and flags as the file gives them: the on-demand rule of `groups` leaves a
        // streamed file alone, whatever its bit)
        ymaps.push(YmapInfo {
            path: path.to_string(),
            hash,
            entities_min: lo,
            entities_max: hi,
            entity_count: m.entities.len(),
            parent: m.parent,
            flags: m.flags,
        });
    }
    // texture parents: above the game's, a later file over an earlier one
    for (i, file) in s.gtxd.iter().enumerate() {
        match read_disk(file) {
            Ok(bytes) => {
                added.gtxd += 1;
                gtxd.push((RANK + i, file.to_string_lossy().replace('\\', "/"), bytes));
            }
            Err(e) => added.failures.push((file.to_string_lossy().into_owned(), format!("{e:#}"))),
        }
    }
    added
}

/// The product of two quaternions (x, y, z, w): the turn `b`, then the turn `a`.
fn multiply(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

/// An entity of an interior (placed relative to it) as a plain entity of the world.
///
/// The interior is turned by its quaternion as stored, unlike a plain entity, which is turned by
/// the inverse: measured on the game's files, 65 interiors have a declared box that only comes
/// out that way, and none the other way. The entity inside is turned like any entity, by the
/// inverse of its own quaternion: the same measure, on the boxes of the rooms, leans that way
/// without settling it (6 against 1 among the long entities turned askew), and it is what
/// CodeWalker does. The result is written as a plain entity's quaternion: the inverse of
/// (interior's turn after entity's turn).
fn place_inside(
    interior: &crate::formats::ymap::Entity,
    inside: &crate::formats::ymap::Entity,
) -> crate::formats::ymap::Entity {
    let scaled = [
        inside.position[0] * interior.scale_xy,
        inside.position[1] * interior.scale_xy,
        inside.position[2] * interior.scale_z,
    ];
    let p = crate::formats::ymap::rotate(interior.rotation, scaled, false);
    let o = interior.rotation;
    crate::formats::ymap::Entity {
        position: [0, 1, 2].map(|k| interior.position[k] + p[k]),
        rotation: multiply(inside.rotation, [-o[0], -o[1], -o[2], o[3]]),
        scale_xy: inside.scale_xy * interior.scale_xy,
        scale_z: inside.scale_z * interior.scale_z,
        interior: false,
        ..inside.clone()
    }
}

/// The entities of the interior `e` of a resource (a `CMloInstanceDef` whose archetype a resource
/// defines), placed in the world: those of its rooms and openings, at their most detailed level.
/// The sets a script switches on are left out (which ones are on is not known).
fn inside(added: &Added, e: &crate::formats::ymap::Entity) -> Vec<crate::formats::ymap::Entity> {
    let Some(mlo) = added.interiors.get(&e.archetype).filter(|_| e.interior && e.lod_level != NOT_SHOWN) else {
        return Vec::new();
    };
    mlo.entities.iter().filter(|c| !c.interior && matches!(c.lod_level, 0 | 5)).map(|c| place_inside(e, c)).collect()
}

/// Archetypes not to show, by name (`BTMS_HIDE=name,name…`): a check, to look under a roof.
static HIDE: std::sync::LazyLock<HashSet<u32>> = std::sync::LazyLock::new(|| {
    std::env::var("BTMS_HIDE").map(|v| v.split(',').map(|n| joaat(n.trim())).collect()).unwrap_or_default()
});

/// The placement file `path` as the game shows it. In a file a resource streams, the entities
/// that are not shown ([`Added::not_shown`]) are marked [`NOT_SHOWN`], and the entities of its
/// interiors are added after its own as plain entities (an interior of the game stays as it is:
/// not drawn). A file of the game is left as it is.
pub fn shown(added: &Added, path: &str, mut map: crate::formats::ymap::Ymap) -> crate::formats::ymap::Ymap {
    if !is_streamed(path) {
        return map;
    }
    if let Some(hidden) = added.not_shown.get(path) {
        for &i in hidden {
            if let Some(e) = map.entities.get_mut(i as usize) {
                e.lod_level = NOT_SHOWN;
            }
        }
    }
    let rooms: Vec<crate::formats::ymap::Entity> = map.entities.iter().flat_map(|e| inside(added, e)).collect();
    map.entities.extend(rooms);
    if !HIDE.is_empty() {
        for e in map.entities.iter_mut().filter(|e| HIDE.contains(&e.archetype)) {
            e.lod_level = NOT_SHOWN;
        }
    }
    map
}

/// Leaves out the placement files the client scripts remove (`RemoveIpl` with a literal name:
/// the name of a placement file), and says what the resources brought and what was and was not
/// applied. `hidden`: the placement files that are not drawn. Says nothing when `map.toml`
/// names no resource.
///
/// A name stands for one placement file: the 578 "map groups" of the manifests (`CMapDataGroup`)
/// each bear the name of a placement file, and what they list (`Bounds`) are its collision files,
/// not other placement files.
pub fn report(added: &Added, ymaps: &[YmapInfo], hidden: &mut HashSet<u32>) {
    let s = server();
    for m in &s.missing {
        eprintln!("server resources: folder not found {m}");
    }
    if s.resources.is_empty() {
        return;
    }
    eprintln!(
        "server resources: {} resources, {} streamed files; {} placement files ({} of them in place of one of the game's, {} of those emptied); {} archetype files ({} archetypes, {} of them already in the game); {} texture parent files",
        s.resources.len(),
        s.streamed.len(),
        added.ymaps,
        added.ymaps_replaced,
        added.ymaps_emptied,
        added.ytyps,
        added.archetypes,
        added.archetypes_replaced,
        added.gtxd
    );
    if added.interiors_placed + added.interiors_protected > 0 {
        eprintln!(
            "server resources: {} interiors defined, {} of them from resources with protected models (left out); {} placements of the others, their {} entities added to the map (visible where the roof is open or glazed)",
            added.interiors.len() + added.interiors_protected,
            added.interiors_protected,
            added.interiors_placed,
            added.inside
        );
    }
    if !added.not_shown.is_empty() || added.widened > 0 {
        eprintln!(
            "server resources: {} entities not drawn in {} of their placement files ({} moved out of the world, below {OUT_OF_WORLD_Z} m; {} that only cast a shadow); {} declared boxes too small, widened",
            added.moved_out + added.shadow_only,
            added.not_shown.len(),
            added.moved_out,
            added.shadow_only,
            added.widened
        );
    }
    if !s.skipped.is_empty() || !added.failures.is_empty() {
        eprintln!(
            "server resources: {} files left out (protected or unreadable), {} map files that failed to read (details: the resources command)",
            s.skipped.len(),
            added.failures.len()
        );
    }
    // what the client scripts remove: the placement file of that name
    let literal: Vec<&IplCall> = s.calls.iter().filter(|c| c.remove && c.name.is_some()).collect();
    let computed = s.calls.iter().filter(|c| c.remove && c.name.is_none()).count();
    let names: HashSet<u32> = ymaps.iter().map(|m| m.hash).collect();
    let (mut applied, mut unknown) = (0usize, Vec::new());
    for c in &literal {
        let name = c.name.as_deref().unwrap_or_default();
        if names.contains(&joaat(name)) {
            hidden.insert(joaat(name));
            applied += 1;
        } else {
            unknown.push(name.to_string());
        }
    }
    if !s.calls.is_empty() || !s.other_hides.is_empty() {
        eprintln!(
            "client script removals: {} RemoveIpl with a literal name ({applied} applied: the placement file of that name is not drawn; {} with no placement file of that name{}), {computed} with a computed name (not applied), {} RequestIpl with a literal name, {} calls that hide models (CreateModelHide…: not applied)",
            literal.len(),
            unknown.len(),
            if unknown.is_empty() { String::new() } else { format!(": {}", unknown.join(", ")) },
            s.requested().len(),
            s.other_hides.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_directives() {
        let d = directives(
            "fx_version 'cerulean'\n-- data_file 'A' 'no'\nthis_is_a_map 'yes'\n--[[ client_script 'no.lua' ]]\nclient_scripts {\n 'a.lua', -- x\n \"b/*.lua\"\n}\ndata_file 'GTXD_PARENTING_DATA' 'data/gtxd.meta'\ndata_file('DLC_ITYP_REQUEST')('stream/x.ytyp')\nclient_script 'c.lua'",
        );
        let get = |n: &str| d.iter().filter(|(k, _)| k == n).map(|(_, v)| v.clone()).collect::<Vec<_>>();
        assert_eq!(get("this_is_a_map"), [["yes"]]);
        assert_eq!(get("client_scripts"), [["a.lua", "b/*.lua"]]);
        assert_eq!(get("client_script"), [["c.lua"]]);
        assert_eq!(
            get("data_file"),
            [["GTXD_PARENTING_DATA", "data/gtxd.meta"], ["DLC_ITYP_REQUEST", "stream/x.ytyp"]]
        );
    }

    /// An entity inside an interior, written as a plain entity: its vertices land where the
    /// interior's turn (as stored) after the entity's own turn (inverse) puts them.
    #[test]
    fn interior_entities() {
        use crate::formats::ymap::{Entity, rotate};
        let unit = |q: [f32; 4]| {
            let n = q.iter().map(|x| x * x).sum::<f32>().sqrt();
            q.map(|x| x / n)
        };
        let entity = |position, rotation| Entity {
            archetype: 1,
            position,
            rotation,
            scale_xy: 1.0,
            scale_z: 1.0,
            tint: 0,
            lod_level: 0,
            interior: false,
        };
        let interior = Entity { interior: true, ..entity([100.0, -200.0, 30.0], unit([0.1, -0.2, 0.6, 0.7])) };
        let child = entity([4.0, 5.0, -1.0], unit([0.3, 0.1, -0.5, 0.8]));
        let placed = place_inside(&interior, &child);
        assert!(!placed.interior);
        for v in [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.5, -1.5, 3.0]] {
            // as paint() places a vertex of a plain entity
            let got = rotate(placed.rotation, v, true);
            let got = [0, 1, 2].map(|k| got[k] + placed.position[k]);
            // the entity's own turn (inverse), its place in the interior, then the interior's turn (as stored)
            let local = rotate(child.rotation, v, true);
            let local = [0, 1, 2].map(|k| local[k] + child.position[k]);
            let want = rotate(interior.rotation, local, false);
            let want = [0, 1, 2].map(|k| want[k] + interior.position[k]);
            for k in 0..3 {
                assert!((got[k] - want[k]).abs() < 1e-3, "{got:?} / {want:?}");
            }
        }
    }

    /// A model is said scrambled when a vertex of it cannot be a position, not when its own box
    /// is merely out of date.
    #[test]
    fn scrambled_models() {
        use crate::formats::ydr::{Drawable, Geometry, Model};
        let model = |positions: Vec<[f32; 3]>| Drawable {
            bb_min: [-1.0, -1.0, 0.0],
            bb_max: [1.0, 1.0, 2.0],
            lod_dist: [9998.0; 4],
            lods: [
                vec![Model {
                    geometries: vec![Geometry {
                        shader: 0,
                        positions,
                        uvs: vec![],
                        uvs1: vec![],
                        colours0: vec![],
                        colours1: vec![],
                        indices: vec![],
                    }],
                }],
                vec![],
                vec![],
                vec![],
            ],
            shaders: vec![],
            textures: vec![],
        };
        // inside its box
        assert!(!scrambled(&model(vec![[0.5, -0.5, 1.0]; 100])));
        // a box out of date: every vertex 80 m away, none absurd
        assert!(!scrambled(&model(vec![[80.0, 0.0, 1.0]; 100])));
        // one vertex in a thousand that cannot be a position: scrambled in part
        let mut few = vec![[0.0, 0.0, 1.0]; 1000];
        few[0] = [3.0e20, 0.0, 0.0];
        assert!(scrambled(&model(few)));
        let mut few = vec![[0.0, 0.0, 1.0]; 1000];
        few[999] = [0.0, -2460.0, 1.0];
        assert!(scrambled(&model(few)));
        // random numbers
        let mut random = vec![[0.5, 0.5, 1.0]; 100];
        for (i, p) in random.iter_mut().enumerate().take(60) {
            *p = [6.7e9, -2.46e3 * i as f32, f32::NAN];
        }
        assert!(scrambled(&model(random)));
    }

    #[test]
    fn ipl_calls() {
        let lua = "RemoveIpl('hei_dt1_19')\n-- RemoveIpl('comment')\nlocal s = \"RemoveIpl('text')\"\nRequestIpl(\"a_b\")\nfor _, n in ipairs(t) do RemoveIpl(n) end\nRemoveIpl('a' .. x)\nCitizen.InvokeNative(0xEE6C5AD3ECE0A82D, 'native')\nCreateModelHide(1.0, 2.0, 3.0, 1.0, `x`, true)";
        let (ipl, other) = native_calls(lua, false);
        assert_eq!(
            ipl,
            [
                (true, Some("hei_dt1_19".to_string()), 1),
                (false, Some("a_b".to_string()), 4),
                (true, None, 5),
                (true, None, 6),
                (true, Some("native".to_string()), 7),
            ]
        );
        assert_eq!(other, [("CreateModelHide".to_string(), 8)]);
        let (ipl, _) = native_calls("// RemoveIpl('no')\nRemoveIpl(\"yes\"); /* RemoveIpl('no') */", true);
        assert_eq!(ipl, [(true, Some("yes".to_string()), 2)]);
    }

    #[test]
    fn patterns_and_ranks() {
        let g = |p: &str, f: &str| glob(&p.split('/').collect::<Vec<_>>(), &f.split('/').collect::<Vec<_>>());
        assert!(g("water.lua", "water.lua") && !g("water.lua", "client/water.lua"));
        assert!(
            g("client/*.lua", "client/a.lua")
                && !g("client/*.lua", "client/sub/a.lua")
                && !g("client/*.lua", "client/a.js")
        );
        assert!(g("**/*.lua", "a.lua") && g("**/*.lua", "x/y/a.lua") && g("client/**", "client/x/y.lua"));
        let (a, b) = (
            index_path(3, Path::new("C:\\Srv\\[Maps]\\stream\\A.ydr")),
            index_path(12, Path::new("C:\\Srv\\[Maps]\\stream\\b\\A.ydr")),
        );
        assert_eq!(a, "resource:000003:c:/srv/[maps]/stream/a.ydr");
        assert!(rank(&b) > rank(&a) && rank(&a) > Some(1000) && rank("update/update.rpf/x.ydr").is_none());
        assert!(is_streamed(&a) && !is_streamed("x64a.rpf/a.ydr"));
    }

    /// A resource folder on disk: its streamed files are found, a later resource wins, a protected
    /// file is left out and reported, a client script's `RemoveIpl` is read.
    #[test]
    fn scan_folder() {
        let beside = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        let root = beside.join(format!("btms-tests-{}-fivem", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let write = |p: &str, bytes: &[u8]| {
            let f = root.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, bytes).unwrap();
        };
        write(
            "[maps]/b-res/fxmanifest.lua",
            b"this_is_a_map 'yes'\nclient_script 'c.lua'\ndata_file 'GTXD_PARENTING_DATA' 'gtxd.meta'",
        );
        write("[maps]/b-res/c.lua", b"RemoveIpl('gone')\nRequestIpl('kept')");
        write("[maps]/b-res/gtxd.meta", b"<x/>");
        write("[maps]/b-res/stream/sub/Same.ymap", b"RSC7....");
        write("[maps]/b-res/stream/locked.ydr", b"FXAP....");
        write("[maps]/b-res/stream/broken.ytd", b"nope");
        write("[maps]/a-res/fxmanifest.lua", b"client_scripts { 'locked.lua' }");
        write("[maps]/a-res/locked.lua", b"FXAP\x00\x01");
        write("[maps]/a-res/stream/same.ymap", b"RSC7....");
        write("[maps]/not-a-resource/readme.txt", b"x");
        let s = Server::scan(&[root.to_string_lossy().into_owned(), "Z:/nowhere/at/all".into()]);
        assert_eq!(s.resources.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["a-res", "b-res"]);
        assert!(!s.resources[0].map && s.resources[1].map);
        assert_eq!(s.streamed.len(), 2);
        let by = s.by_name();
        let copies = &by["same.ymap"];
        assert!(copies.len() == 2 && copies[1].contains("/b-res/") && copies[1].ends_with("/sub/same.ymap"));
        assert_eq!(read(copies[1]).unwrap(), b"RSC7....");
        assert_eq!(s.gtxd.len(), 1);
        assert!(s.calls.iter().any(|c| c.remove && c.name.as_deref() == Some("gone") && c.line == 1));
        assert_eq!(s.requested(), HashSet::from([joaat("kept")]));
        assert_eq!(s.skipped.len(), 3);
        assert!(s.skipped.iter().any(|(f, why)| f.ends_with("locked.ydr") && why.contains("FXAP")));
        assert!(s.skipped.iter().any(|(f, why)| f.ends_with("locked.lua") && why.contains("FXAP")));
        assert!(s.skipped.iter().any(|(f, why)| f.ends_with("broken.ytd") && why.contains("RSC7")));
        assert_eq!(s.missing, ["Z:/nowhere/at/all"]);
        assert!(read_disk(&root.join("[maps]/b-res/stream/locked.ydr")).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
