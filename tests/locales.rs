//  ____                 ____
// | __ )  _   _  _   _ |  _ \   ___ __   __
// |  _ \ | | | || | | || | | | / _ \\ \ / /
// | |_) || |_| || |_| || |_| ||  __/ \ V /
// |____/  \__,_| \__,_||____/  \___|  \_/
//
// BTMS · Buu's Tiles Map Scanner
// Author : BuuDevOff · https://github.com/BuuDevOff
// SPDX-License-Identifier: GPL-3.0-only

//! The dictionaries of the page against English, read from the repository as they are shipped:
//! `web/interface/locales/<code>.json` (the interface) and the inline `dictionaries` block of
//! `web/viewer/index.html` (the map page copied next to `tiles.json`).
//!
//! A translation has the keys of English, and the only keys it may add are plural forms
//! (`key.zero`, `key.one`, `key.two`, `key.few`, `key.many`, `key.other`, the categories of
//! `Intl.PluralRules`) of a key that English writes with a `.other`. No text is empty, and a text
//! has the `{name}` placeholders of its English text. A plural form may instead have those of the
//! English `.other`: a language whose `.one` also covers 21 or 31 (Russian) must show the count
//! there, where English `.one` does not need it.
//!
//! The seven languages are those the server serves (`PAGE` in `src/interface/mod.rs`), accepts
//! as the `language` setting (`LANGUAGES` in `src/interface/settings.rs`) and the page lists
//! (`LANGUAGES` in `web/interface/js/i18n.js`, and the map page's own list).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};

/// The languages of the page, in the order of its selector.
const LANGUAGES: [&str; 7] = ["en", "fr", "de", "nl", "ru", "sv", "es"];
/// The language the others are checked against.
const ENGLISH: &str = "en";
/// The plural categories of `Intl.PluralRules`, the only suffixes a translation may add.
const PLURAL: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];

/// A JSON object as written: its entries in the file's order, and a key written twice kept twice
/// (a map would keep one of them without a word).
struct Ordered<V>(Vec<(String, V)>);

impl<'de, V: Deserialize<'de>> Deserialize<'de> for Ordered<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries<V>(PhantomData<V>);

        impl<'de, V: Deserialize<'de>> Visitor<'de> for Entries<V> {
            type Value = Ordered<V>;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Ordered<V>, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, V>()? {
                    entries.push(entry);
                }
                Ok(Ordered(entries))
            }
        }

        deserializer.deserialize_map(Entries(PhantomData))
    }
}

/// A dictionary: key → text, each text a string.
type Dictionary = Ordered<String>;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

fn locale_path(code: &str) -> PathBuf {
    root().join("web/interface/locales").join(format!("{code}.json"))
}

/// A dictionary of the interface; `problems` gets what is wrong with the file itself.
fn locale(code: &str, problems: &mut Vec<String>) -> Dictionary {
    let path = locale_path(code);
    let text = read(&path);
    if text.starts_with('\u{feff}') {
        problems.push(format!("{code}.json: starts with a byte order mark"));
    }
    if text.contains('\r') {
        problems.push(format!("{code}.json: holds CR characters (the files are LF only)"));
    }
    serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .unwrap_or_else(|e| panic!("{}: not a flat object of strings: {e}", path.display()))
}

/// The names of the `{name}` places of a text, as `fill` in i18n.js finds them (`\{(\w+)\}`).
fn placeholders(text: &str) -> BTreeSet<&str> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        rest = &rest[open + 1..];
        let name_len = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
        if name_len > 0 && rest[name_len..].starts_with('}') {
            found.insert(&rest[..name_len]);
        }
    }
    found
}

/// What is wrong with a translation (`code`, where `place` says which file) against English.
fn compare(place: &str, english: &Dictionary, translated: &Dictionary) -> Vec<String> {
    let mut problems = Vec::new();
    let mut reference: BTreeMap<&str, &str> = BTreeMap::new();
    for (key, text) in &english.0 {
        if reference.insert(key, text).is_some() {
            problems.push(format!("{place}: the English key {key} is written twice"));
        }
    }
    let mut seen = BTreeSet::new();
    for (key, text) in &translated.0 {
        if !seen.insert(key.as_str()) {
            problems.push(format!("{place}: {key} is written twice"));
        }
        if text.trim().is_empty() {
            problems.push(format!("{place}: {key} is empty"));
        }
        // the English texts this one may take its placeholders from
        let mut models: Vec<&str> = reference.get(key.as_str()).copied().into_iter().collect();
        if let Some((base, category)) = key.rsplit_once('.')
            && PLURAL.contains(&category)
            && let Some(other) = reference.get(format!("{base}.other").as_str())
        {
            models.push(*other);
        }
        if models.is_empty() {
            problems.push(format!("{place}: {key} is not an English key, nor a plural form of one"));
            continue;
        }
        let own = placeholders(text);
        if !models.iter().any(|model| placeholders(model) == own) {
            let wanted: Vec<String> = models.iter().map(|model| format!("{:?}", placeholders(model))).collect();
            problems.push(format!("{place}: {key} has the placeholders {own:?}, English has {}", wanted.join(" or ")));
        }
    }
    for (key, _) in &english.0 {
        if !seen.contains(key.as_str()) {
            problems.push(format!("{place}: {key} is missing"));
        }
    }
    problems
}

/// The text of `source` from the first `start` to the first `end` after it.
fn between<'a>(source: &'a str, start: &str, end: &str, what: &str) -> &'a str {
    let from = source.find(start).unwrap_or_else(|| panic!("{what}: `{start}` not found"));
    let rest = &source[from + start.len()..];
    let to = rest.find(end).unwrap_or_else(|| panic!("{what}: `{end}` not found after `{start}`"));
    &rest[..to]
}

/// The codes of a JavaScript object literal written one entry a line (`  en: 'English',`) or on
/// one line (`{ en: 'English', … }`), with their names.
fn js_languages(literal: &str) -> Vec<(String, String)> {
    literal
        .split(',')
        .filter_map(|entry| {
            let (code, name) = entry.split_once(':')?;
            Some((code.trim().to_string(), name.trim().trim_matches('\'').to_string()))
        })
        .collect()
}

fn expected() -> Vec<String> {
    LANGUAGES.iter().map(|code| code.to_string()).collect()
}

#[test]
fn interface_dictionaries_match_english() {
    let mut problems = Vec::new();
    let english = locale(ENGLISH, &mut problems);
    for (key, text) in &english.0 {
        if text.trim().is_empty() {
            problems.push(format!("en.json: {key} is empty"));
        }
    }
    for code in LANGUAGES.iter().filter(|code| **code != ENGLISH) {
        let translated = locale(code, &mut problems);
        problems.extend(compare(&format!("{code}.json"), &english, &translated));
    }
    assert!(problems.is_empty(), "the interface's dictionaries differ from en.json:\n{}", problems.join("\n"));
}

#[test]
fn the_seven_languages_are_served_and_listed() {
    // the files beside en.json
    let mut files: Vec<String> = std::fs::read_dir(root().join("web/interface/locales"))
        .expect("web/interface/locales cannot be listed")
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension()? != "json" {
                return None;
            }
            Some(path.file_stem()?.to_str()?.to_string())
        })
        .collect();
    files.sort();
    let mut sorted = expected();
    sorted.sort();
    assert_eq!(files, sorted, "web/interface/locales should hold one file per language of the page");

    // the routes of the server, each one held from its own file
    let server = read(&root().join("src/interface/mod.rs"));
    let page = between(&server, "const PAGE:", "];", "src/interface/mod.rs");
    let mut served = Vec::new();
    for line in page.lines().map(str::trim).filter(|line| line.starts_with("(\"locales/")) {
        let code = between(line, "(\"locales/", ".json\"", "a locale route of PAGE");
        let held = format!("include_bytes!(\"../../web/interface/locales/{code}.json\")");
        assert!(line.contains(&held), "the route locales/{code}.json of PAGE should hold its own file: {line}");
        served.push(code.to_string());
    }
    assert_eq!(served, expected(), "PAGE in src/interface/mod.rs should serve the seven dictionaries");

    // the values the server accepts for the `language` setting
    let settings = read(&root().join("src/interface/settings.rs"));
    let declared = between(&settings, "pub const LANGUAGES:", "];", "src/interface/settings.rs");
    let (_, list) = declared.split_once("= [").expect("src/interface/settings.rs: LANGUAGES is not an array literal");
    let accepted: Vec<String> =
        list.split(',').map(|code| code.trim().trim_matches('"').to_string()).filter(|code| !code.is_empty()).collect();
    assert_eq!(accepted, expected(), "LANGUAGES in src/interface/settings.rs should be the seven languages");

    // the selector of the interface
    let i18n = read(&root().join("web/interface/js/i18n.js"));
    let listed =
        js_languages(between(&i18n, "export const LANGUAGES = Object.freeze({", "});", "web/interface/js/i18n.js"));
    let codes: Vec<String> = listed.iter().map(|(code, _)| code.clone()).collect();
    assert_eq!(codes, expected(), "LANGUAGES in web/interface/js/i18n.js should list the seven languages");

    // the selector of the map page, with the same native names
    let viewer = read(&root().join("web/viewer/index.html"));
    let own = js_languages(between(&viewer, "const LANGUAGES = {", "};", "web/viewer/index.html"));
    assert_eq!(own, listed, "the map page should list the languages of the interface, with the same names");
}

#[test]
fn viewer_dictionaries_match_english() {
    let path = root().join("web/viewer/index.html");
    let page = read(&path);
    let block =
        between(&page, "<script type=\"application/json\" id=\"dictionaries\">", "</script>", "web/viewer/index.html");
    let dictionaries: Ordered<Dictionary> = serde_json::from_str(block)
        .unwrap_or_else(|e| panic!("{}: the dictionaries block is not JSON: {e}", path.display()));
    let codes: Vec<String> = dictionaries.0.iter().map(|(code, _)| code.clone()).collect();
    assert_eq!(codes, expected(), "the map page should have one dictionary per language, in the selector's order");

    let english = &dictionaries.0[0].1;
    let mut problems = Vec::new();
    for (key, text) in &english.0 {
        if text.trim().is_empty() {
            problems.push(format!("viewer en: {key} is empty"));
        }
    }
    for (code, translated) in dictionaries.0.iter().skip(1) {
        problems.extend(compare(&format!("viewer {code}"), english, translated));
    }
    assert!(problems.is_empty(), "the map page's dictionaries differ from its English one:\n{}", problems.join("\n"));
}

#[test]
fn placeholders_are_found_as_the_page_finds_them() {
    assert_eq!(placeholders("{done} of {n} blocks, {n} left"), BTreeSet::from(["done", "n"]));
    assert_eq!(placeholders("{ not one } {} {a-b} {x_1}"), BTreeSet::from(["x_1"]));
    assert_eq!(placeholders("{{value}}"), BTreeSet::from(["value"]));
    assert!(placeholders("no place").is_empty());
}

#[test]
fn each_kind_of_fault_is_reported() {
    let dictionary = |json: &str| -> Dictionary { serde_json::from_str(json).expect("a test dictionary") };
    let english = dictionary(
        r#"{"title": "Map", "blocks.one": "one block", "blocks.other": "{n} blocks", "done": "{done} of {n}"}"#,
    );
    // a language with more plural forms, whose `.one` shows the count (as Russian's does)
    let good = dictionary(
        r#"{"title": "The map", "blocks.one": "{n} block (one)", "blocks.few": "{n} blocks (few)",
            "blocks.many": "{n} blocks (many)", "blocks.other": "{n} blocks (other)", "done": "{done} out of {n}"}"#,
    );
    assert_eq!(compare("good", &english, &good), Vec::<String>::new());

    let bad = dictionary(
        r#"{"title": " ", "title": "The map", "blocks.one": "{count} block", "blocks.other": "{n} blocks",
            "blocks.dual": "{n} blocks", "title.few": "Maps", "extra": "more", "done": "{done} out of {total}"}"#,
    );
    let problems = compare("bad", &english, &bad);
    for wanted in [
        "bad: title is written twice",
        "bad: title is empty",
        "bad: blocks.one has the placeholders",
        "bad: blocks.dual is not an English key",
        "bad: title.few is not an English key",
        "bad: extra is not an English key",
        "bad: done has the placeholders",
    ] {
        assert!(problems.iter().any(|p| p.starts_with(wanted)), "`{wanted}` not reported in {problems:#?}");
    }
    let missing = dictionary(r#"{"title": "The map", "blocks.other": "{n} blocks"}"#);
    let problems = compare("missing", &english, &missing);
    assert!(problems.contains(&"missing: blocks.one is missing".to_string()), "{problems:#?}");
    assert!(problems.contains(&"missing: done is missing".to_string()), "{problems:#?}");
}
