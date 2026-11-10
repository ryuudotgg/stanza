use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use indexmap::IndexMap;

use super::bun_glob::bun_match;
use super::data::{Data, property};
use super::editor_glob::EditorGlob;
use super::shared::{self, Memo, Source};
use crate::directories::{holds, real_directory};
use crate::languages::Layout;
use crate::paths::{basename, dirname, is_absolute, join, relative, resolve};
use crate::text::{is_js_whitespace, js_number, js_trim, js_trim_start};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Formatter {
    Oxfmt,
    Prettier,
    Biome,
}

struct Keys {
    width: &'static str,
    tab: &'static str,
    editorconfig: Option<&'static str>,
    files: &'static str,
    legacy_files: Option<&'static str>,
    options: &'static str,
    default_width: f64,
    basename_only: bool,
}

impl Formatter {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Oxfmt => "oxfmt",
            Self::Prettier => "prettier",
            Self::Biome => "biome",
        }
    }

    const fn keys(self) -> &'static Keys {
        match self {
            Self::Oxfmt => &Keys {
                width: "printWidth",
                tab: "tabWidth",
                editorconfig: None,
                files: "files",
                legacy_files: None,
                options: "options",
                default_width: 100.0,
                basename_only: true,
            },
            Self::Prettier => &Keys {
                width: "printWidth",
                tab: "tabWidth",
                editorconfig: None,
                files: "files",
                legacy_files: None,
                options: "options",
                default_width: 80.0,
                basename_only: true,
            },
            Self::Biome => &Keys {
                width: "lineWidth",
                tab: "indentWidth",
                editorconfig: Some("useEditorconfig"),
                files: "includes",
                legacy_files: Some("include"),
                options: "formatter",
                default_width: 80.0,
                basename_only: false,
            },
        }
    }

    fn of(file: &str) -> Self {
        let name = basename(file);
        if name.starts_with("biome.") {
            Self::Biome
        } else if name.starts_with("oxfmt.") || name.starts_with(".oxfmt") {
            Self::Oxfmt
        } else {
            Self::Prettier
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum WidthSource {
    Config(String),
    Default(Formatter),
    Fallback,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Width {
    pub columns: f64,
    pub tab: f64,
    pub source: WidthSource,
    pub unread: Vec<String>,
}

impl Width {
    pub fn layout(&self) -> Layout {
        Layout {
            columns: self.columns as usize,
            tab: self.tab as usize,
        }
    }
}

pub const FILES: [&str; 29] = [
    ".oxfmtrc.json",
    ".oxfmtrc.jsonc",
    "oxfmt.config.ts",
    "oxfmt.config.mts",
    "oxfmt.config.cts",
    "oxfmt.config.js",
    "oxfmt.config.mjs",
    "oxfmt.config.cjs",
    "biome.json",
    "biome.jsonc",
    ".prettierrc",
    ".prettierrc.toml",
    ".prettierrc.json",
    ".prettierrc.json5",
    ".prettierrc.yaml",
    ".prettierrc.yml",
    ".prettierrc.js",
    ".prettierrc.cjs",
    ".prettierrc.mjs",
    ".prettierrc.ts",
    ".prettierrc.cts",
    ".prettierrc.mts",
    "prettier.config.js",
    "prettier.config.cjs",
    "prettier.config.mjs",
    "prettier.config.ts",
    "prettier.config.cts",
    "prettier.config.mts",
    "package.json",
];

struct ConfigFile {
    file: String,
    formatter: Formatter,
    value: Data,
}

#[derive(Clone)]
enum Read {
    Config(Arc<ConfigFile>),
    Skip,
    Unread,
}

#[derive(Clone)]
struct Directory {
    configs: Vec<Arc<ConfigFile>>,
    unread: Vec<String>,
}

struct Setting {
    columns: Option<f64>,
    tab: Option<f64>,
    editorconfig: bool,
}

enum Scoped {
    Setting(Setting),
    Skip,
    Unread,
}

struct Pattern {
    nested: bool,
    glob: EditorGlob,
}

struct Section {
    pattern: Option<Pattern>,
    values: IndexMap<String, String>,
}

struct Editor {
    root: bool,
    sections: Vec<Section>,
}

struct EditorFile {
    path: String,
    editor: Option<Editor>,
}

#[derive(Default)]
struct EditorValues {
    columns: Option<f64>,
    tab: Option<f64>,
    file: Option<String>,
}

static PARSED: LazyLock<Memo<String, Read>> = LazyLock::new(Memo::default);

static DIRECTORIES: LazyLock<Memo<String, Arc<Directory>>> = LazyLock::new(Memo::default);

static DEPENDENCIES: LazyLock<Memo<String, Arc<Vec<Formatter>>>> = LazyLock::new(Memo::default);

static EDITORS: LazyLock<Memo<String, Arc<EditorFile>>> = LazyLock::new(Memo::default);

// Locked for a whole merge: a per-file Memo would deadlock two threads entering one extends cycle from opposite ends.
static MERGED: LazyLock<Mutex<HashMap<String, Data>>> = LazyLock::new(Mutex::default);

const fn is_object(value: &Data) -> bool {
    matches!(value, Data::Object(_))
}

const fn is_unknown(value: &Data) -> bool {
    matches!(value, Data::Unknown)
}

const fn is_undefined(value: &Data) -> bool {
    matches!(value, Data::Undefined)
}

fn read_text(file: &str) -> Option<String> {
    fs::read(file)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

fn spread(value: &Data, into: &mut IndexMap<String, Data>) {
    match value {
        Data::Object(object) => {
            for key in object.keys() {
                let item = object.get(&key);
                into.insert(key, item);
            }
        }
        Data::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                into.insert(index.to_string(), item.clone());
            }
        }
        Data::String(text) => {
            for (index, unit) in text.encode_utf16().enumerate() {
                into.insert(
                    index.to_string(),
                    Data::String(String::from_utf16_lossy(&[unit]).into()),
                );
            }
        }
        _ => {}
    }
}

fn spread_all(values: &[&Data]) -> IndexMap<String, Data> {
    let mut entries = IndexMap::new();
    for value in values {
        spread(value, &mut entries);
    }

    entries
}

fn object_of(entries: IndexMap<String, Data>) -> Data {
    Data::plain(entries)
}

fn merged(values: &[&Data]) -> Data {
    object_of(spread_all(values))
}

fn without_comment(text: &str) -> &str {
    let mut at = 0;
    while let Some(offset) = text[at..].find(is_js_whitespace) {
        let start = at + offset;
        let run = &text[start..];
        let after = run.trim_start_matches(is_js_whitespace);
        let hash = start + run.len() - after.len();
        if after.starts_with('#') && !after[1..].contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
            return &text[..start];
        }

        at = hash;
    }

    text
}

fn scalar(value: &str) -> Data {
    let text = without_comment(js_trim(value));
    if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Data::Number(js_number(text));
    }

    if text == "true" || text == "false" {
        return Data::Bool(text == "true");
    }

    let quoted = text.len() >= 2
        && (text.starts_with('"') && text.ends_with('"')
            || text.starts_with('\'') && text.ends_with('\''))
        && !text[1..text.len() - 1].contains(['\n', '\r', '\u{2028}', '\u{2029}']);

    if quoted {
        return Data::String(text[1..text.len() - 1].into());
    }

    Data::String(text.into())
}

fn is_key_char(char: char) -> bool {
    char.is_ascii_alphanumeric() || char == '_' || char == '-'
}

fn flat_key(line: &str, separator: char) -> Option<(&str, &str)> {
    let (key, rest) = match line.chars().next()? {
        quote @ ('"' | '\'') if separator == ':' => {
            let inner = &line[1..];
            let end = inner.find(|char| !is_key_char(char))?;
            if end == 0 || !inner[end..].starts_with(quote) {
                return None;
            }

            (&inner[..end], &inner[end + 1..])
        }
        _ => {
            let end = line.find(|char| !is_key_char(char)).unwrap_or(line.len());
            if end == 0 {
                return None;
            }

            (&line[..end], &line[end..])
        }
    };

    let rest = js_trim_start(rest).strip_prefix(separator)?;
    let raw = js_trim_start(rest);
    if raw.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return None;
    }

    Some((key, raw))
}

fn flat(text: &str, separator: char) -> Data {
    let mut entries = IndexMap::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if js_trim(line).is_empty() || js_trim_start(line).starts_with('#') {
            continue;
        }

        let Some((key, raw)) = flat_key(line, separator) else {
            return Data::Unknown;
        };

        if key == "overrides" || raw.is_empty() {
            return Data::Unknown;
        }

        if key != "__proto__" {
            entries.insert(key.to_owned(), scalar(raw));
        }
    }

    object_of(entries)
}

fn biome_active(value: &Data) -> bool {
    if !is_object(value) {
        return true;
    }

    let formatter = property(value, "formatter");
    let javascript = property(&property(value, "javascript"), "formatter");
    if matches!(property(&formatter, "enabled"), Data::Bool(false))
        || matches!(property(&javascript, "enabled"), Data::Bool(false))
    {
        return false;
    }

    if !is_undefined(&formatter) || !is_undefined(&javascript) {
        return true;
    }

    match property(value, "overrides") {
        Data::Unknown => true,
        Data::Array(entries) => entries.iter().any(|entry| {
            is_object(entry)
                && (!is_undefined(&property(entry, "formatter"))
                    || !is_undefined(&property(&property(entry, "javascript"), "formatter")))
        }),
        _ => false,
    }
}

fn is_jsonc_name(name: &str) -> bool {
    name.ends_with(".json") || name.ends_with(".jsonc")
}

fn biome_target(file: &str, entry: &str) -> Option<String> {
    if entry == "//" {
        let mut dir = dirname(file);
        while dirname(&dir) != dir {
            dir = dirname(&dir);
            for name in ["biome.json", "biome.jsonc"] {
                let candidate = join(&dir, name);
                if fs::metadata(&candidate).is_ok() {
                    return Some(candidate);
                }
            }
        }

        return None;
    }

    (entry.starts_with('.') || is_absolute(entry) || is_jsonc_name(entry))
        .then(|| resolve(&dirname(file), entry))
}

fn merged_biome(file: &str, seen: &mut Vec<String>, done: &mut HashMap<String, Data>) -> Data {
    if seen.iter().any(|item| item == file) {
        return Data::Unknown;
    }

    if let Some(known) = done.get(file) {
        return known.clone();
    }

    let merged = merged_extends(file, shared::exported_default(file), seen, done);
    done.insert(file.to_owned(), merged.clone());
    merged
}

fn merged_extends(
    file: &str,
    value: Data,
    seen: &mut Vec<String>,
    done: &mut HashMap<String, Data>,
) -> Data {
    if !is_object(&value) {
        return Data::Unknown;
    }

    let list = match property(&value, "extends") {
        Data::Undefined => return value,
        Data::String(entry) => vec![entry.to_string()],
        Data::Array(items) => {
            let strings: Option<Vec<String>> = items
                .iter()
                .map(|item| match item {
                    Data::String(text) => Some(text.to_string()),
                    _ => None,
                })
                .collect();

            let Some(strings) = strings else {
                return Data::Unknown;
            };

            strings
        }
        _ => return Data::Unknown,
    };

    seen.push(file.to_owned());

    let mut result = object_of(IndexMap::new());
    for entry in list {
        let Some(target) = biome_target(file, &entry)
            .filter(|target| is_jsonc_name(target) && fs::metadata(target).is_ok())
        else {
            return Data::Unknown;
        };

        let parent = merged_biome(&target, seen, done);
        if !is_object(&parent) {
            return Data::Unknown;
        }

        result = merge_biome(&result, &parent);
    }

    seen.retain(|item| item != file);
    merge_biome(&result, &value)
}

fn merge_biome(base: &Data, next: &Data) -> Data {
    let mut result = spread_all(&[base, next]);
    let formatter = (property(base, "formatter"), property(next, "formatter"));

    if !is_undefined(&formatter.0) || !is_undefined(&formatter.1) {
        result.insert(
            "formatter".to_owned(),
            merged(&[&formatter.0, &formatter.1]),
        );
    }

    let javascript = (property(base, "javascript"), property(next, "javascript"));

    if !is_undefined(&javascript.0) || !is_undefined(&javascript.1) {
        let mut inner = spread_all(&[&javascript.0, &javascript.1]);
        let formatter = (
            property(&javascript.0, "formatter"),
            property(&javascript.1, "formatter"),
        );

        if !is_undefined(&formatter.0) || !is_undefined(&formatter.1) {
            inner.insert(
                "formatter".to_owned(),
                merged(&[&formatter.0, &formatter.1]),
            );
        }

        result.insert("javascript".to_owned(), object_of(inner));
    }

    object_of(result)
}

fn readable(file: &str) -> Read {
    PARSED.get(file, || read_config(file))
}

fn read_config(file: &str) -> Read {
    let name = basename(file);
    let formatter = Formatter::of(file);
    let mut value = if name == ".prettierrc.toml" {
        read_text(file).map_or(Data::Unknown, |text| flat(&text, '='))
    } else if name.ends_with(".yaml") || name.ends_with(".yml") {
        read_text(file).map_or(Data::Unknown, |text| flat(&text, ':'))
    } else if name == ".prettierrc" {
        match shared::source(file) {
            Source::Program(value) => value,
            Source::Text(text) => flat(&text, ':'),
            Source::Missing => Data::Unknown,
        }
    } else if name == "package.json" {
        property(&shared::exported_default(file), "prettier")
    } else {
        shared::exported_default(file)
    };

    if formatter == Formatter::Biome && is_object(&value) {
        let mut done = MERGED.lock().unwrap_or_else(PoisonError::into_inner);
        value = merged_biome(file, &mut Vec::new(), &mut done);
    }

    if is_undefined(&value)
        || formatter == Formatter::Biome && is_object(&value) && !biome_active(&value)
    {
        Read::Skip
    } else if is_object(&value) {
        Read::Config(Arc::new(ConfigFile {
            file: file.to_owned(),
            formatter,
            value,
        }))
    } else {
        Read::Unread
    }
}

#[cfg(test)]
struct Candidates(std::sync::Mutex<Vec<(std::thread::ThreadId, usize)>>);

#[cfg(test)]
impl Candidates {
    fn counted(&self, add: usize) -> usize {
        let thread = std::thread::current().id();
        let mut counts = self.0.lock().unwrap();
        let at = counts
            .iter()
            .position(|(id, _)| *id == thread)
            .unwrap_or_else(|| {
                counts.push((thread, 0));
                counts.len() - 1
            });

        counts[at].1 += add;
        counts[at].1
    }

    fn get(&self) -> usize {
        self.counted(0)
    }
}

#[cfg(test)]
static CANDIDATES: Candidates = Candidates(std::sync::Mutex::new(Vec::new()));

fn candidate(dir: &str, name: &str) -> String {
    #[cfg(test)]
    CANDIDATES.counted(1);

    join(dir, name)
}

fn directory(dir: &str) -> Arc<Directory> {
    DIRECTORIES.get(dir, || Arc::new(listed_configs(dir)))
}

fn listed_configs(dir: &str) -> Directory {
    let mut found = Directory {
        configs: Vec::new(),
        unread: Vec::new(),
    };

    for name in FILES.into_iter().filter(|name| holds(dir, name)) {
        let file = candidate(dir, name);
        match readable(&file) {
            Read::Config(config) => found.configs.push(config),
            Read::Unread => found.unread.push(file),
            Read::Skip => {}
        }
    }

    found
}

fn manifest_formatters(dir: &str) -> Option<Vec<Formatter>> {
    let manifest = shared::json(&read_text(&join(dir, "package.json"))?)?;
    if matches!(manifest, Data::Null) {
        return None;
    }

    let names = spread_all(&[
        &property(&manifest, "dependencies"),
        &property(&manifest, "devDependencies"),
    ]);

    let found = [
        ("oxfmt", Formatter::Oxfmt),
        ("@biomejs/biome", Formatter::Biome),
        ("prettier", Formatter::Prettier),
    ]
    .into_iter()
    .filter(|(name, _)| names.contains_key(*name))
    .map(|(_, formatter)| formatter)
    .collect();

    Some(found)
}

fn dependent(dir: &str) -> Arc<Vec<Formatter>> {
    DEPENDENCIES.get(dir, || {
        let formatters = holds(dir, "package.json")
            .then(|| manifest_formatters(dir))
            .flatten()
            .filter(|formatters| !formatters.is_empty());

        if let Some(formatters) = formatters {
            return Arc::new(formatters);
        }

        let parent = dirname(dir);
        if parent == dir {
            Arc::default()
        } else {
            dependent(&parent)
        }
    })
}

fn number(value: &Data) -> Option<f64> {
    match *value {
        Data::Number(number) if number.fract() == 0.0 && number > 0.0 && number.is_finite() => {
            Some(number)
        }
        _ => None,
    }
}

fn glob_match(pattern: &str, file: &str, root: &str, basename_only: bool) -> bool {
    let path = if basename_only && !pattern.contains('/') {
        basename(file).to_owned()
    } else {
        relative(root, file)
    };

    bun_match(pattern, &path)
}

fn ordered_includes(entries: &[String], file: &str, root: &str) -> bool {
    let mut included = false;
    for entry in entries {
        let pattern = entry.trim_start_matches('!');
        let bangs = entry.len() - pattern.len();
        let matches = glob_match(pattern, file, root, false)
            || bangs > 0
                && !pattern.ends_with("/**")
                && glob_match(&format!("{pattern}/**"), file, root, false);

        if matches {
            included = bangs == 0;
        }
    }

    included
}

fn patterns(value: &Data) -> Option<Vec<String>> {
    match value {
        Data::String(text) => Some(vec![text.to_string()]),
        Data::Array(items) => items
            .iter()
            .map(|item| match item {
                Data::String(text) => Some(text.to_string()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

const UNKNOWN_DEPTH: usize = 10_000;

fn holds_unknown(value: &Data) -> bool {
    height_below(value, 0, &mut HashMap::new()).is_none()
}

fn height_below(
    value: &Data,
    depth: usize,
    heights: &mut HashMap<usize, Option<usize>>,
) -> Option<usize> {
    if depth > UNKNOWN_DEPTH {
        return None;
    }

    let address = match value {
        Data::Unknown => return None,
        Data::Array(items) => Arc::as_ptr(items).addr(),
        Data::Object(object) => Arc::as_ptr(object).addr(),
        _ => return Some(0),
    };

    if let Some(&known) = heights.get(&address) {
        return known.filter(|height| depth + height <= UNKNOWN_DEPTH);
    }

    heights.insert(address, None);
    let mut height = 0;
    let mut visit = |child: &Data| {
        height = height.max(1 + height_below(child, depth + 1, heights)?);
        Some(())
    };

    match value {
        Data::Array(items) => items.iter().try_for_each(&mut visit)?,
        Data::Object(object) => object
            .keys()
            .iter()
            .try_for_each(|key| visit(&object.get(key)))?,
        _ => unreachable!("scalars return above"),
    }

    heights.insert(address, Some(height));
    Some(height)
}

fn legacy_pattern(pattern: &str) -> String {
    if pattern.contains('/') {
        pattern.to_owned()
    } else {
        format!("**/{pattern}")
    }
}

fn scoped(config: &ConfigFile, path: &str) -> Data {
    let mut value = config.value.clone();
    let overrides = match property(&value, "overrides") {
        Data::Undefined => return value,
        Data::Array(entries) => entries,
        _ => return Data::Unknown,
    };

    let keys = config.formatter.keys();
    let biome = keys.options == "formatter";
    let root = dirname(&config.file);
    for entry in overrides.iter() {
        if !is_object(entry) {
            return Data::Unknown;
        }

        let current = property(entry, keys.files);
        let fallback = match current {
            Data::Undefined | Data::Null => keys
                .legacy_files
                .map_or(Data::Undefined, |key| property(entry, key)),
            _ => current.clone(),
        };

        let files = patterns(&fallback);
        let options = property(entry, keys.options);
        let javascript = property(entry, "javascript");
        let exclude = property(entry, "excludeFiles");
        let excluded = patterns(&exclude);
        let Some(files) = files else {
            return Data::Unknown;
        };

        if holds_unknown(&options)
            || holds_unknown(&javascript)
            || !is_undefined(&exclude) && excluded.is_none()
            || !is_undefined(&options) && !is_object(&options)
            || !is_undefined(&javascript) && !is_object(&javascript)
        {
            return Data::Unknown;
        }

        let legacy = keys.legacy_files.is_some() && is_undefined(&current);
        let included = if biome && !legacy {
            ordered_includes(&files, path, &root)
        } else {
            files.iter().any(|pattern| {
                let pattern = if legacy {
                    legacy_pattern(pattern)
                } else {
                    pattern.clone()
                };

                glob_match(&pattern, path, &root, keys.basename_only)
            })
        };

        let skipped = excluded.is_some_and(|excluded| {
            excluded
                .iter()
                .any(|pattern| glob_match(pattern, path, &root, true))
        });

        if !included || skipped {
            continue;
        }

        let base_formatter = property(&value, "formatter");
        let base_javascript = property(&value, "javascript");
        if holds_unknown(&base_formatter) || holds_unknown(&base_javascript) {
            return Data::Unknown;
        }

        let mut next = spread_all(&[&value]);
        if biome {
            next.insert("formatter".to_owned(), merged(&[&base_formatter, &options]));

            let mut inner = spread_all(&[&base_javascript, &javascript]);
            inner.insert(
                "formatter".to_owned(),
                merged(&[
                    &property(&base_javascript, "formatter"),
                    &property(&javascript, "formatter"),
                ]),
            );

            next.insert("javascript".to_owned(), object_of(inner));
        } else {
            spread(&options, &mut next);
        }

        value = object_of(next);
    }

    value
}

fn biome_skips(value: &Data, format: &Data, path: &str, root: &str) -> Option<bool> {
    let files = property(value, "files");
    if is_unknown(&files) {
        return None;
    }

    let includes = property(&files, "includes");
    let include = property(&files, "include");
    let ignore = property(&files, "ignore");
    if [&includes, &include, &ignore].into_iter().any(is_unknown) {
        return None;
    }

    let listed = |value: &Data| {
        if is_undefined(value) {
            Some(None)
        } else {
            patterns(value).map(Some)
        }
    };

    let (v2, v1, ignored) = (listed(&includes)?, listed(&include)?, listed(&ignore)?);
    if v2.is_some_and(|v2| !ordered_includes(&v2, path, root)) {
        return Some(true);
    }

    let legacy = |pattern: &String| glob_match(&legacy_pattern(pattern), path, root, false);
    if v1.is_some_and(|v1| !v1.iter().any(legacy)) {
        return Some(true);
    }

    if ignored.is_some_and(|ignored| ignored.iter().any(legacy)) {
        return Some(true);
    }

    let formatter_includes = property(format, "includes");
    if is_unknown(&formatter_includes) {
        return None;
    }

    let scoped = listed(&formatter_includes)?;
    Some(scoped.is_some_and(|scoped| !ordered_includes(&scoped, path, root)))
}

fn setting(config: &ConfigFile, path: &str) -> Scoped {
    let value = scoped(config, path);
    if is_unknown(&value) {
        return Scoped::Unread;
    }

    if config.formatter == Formatter::Biome && !biome_active(&value) {
        return Scoped::Skip;
    }

    let keys = config.formatter.keys();
    let format = if keys.options == "formatter" {
        property(&value, "formatter")
    } else {
        value.clone()
    };

    let javascript = property(&property(&value, "javascript"), "formatter");
    let width = property(&javascript, keys.width);
    let base_width = property(&format, keys.width);
    let tab = property(&javascript, keys.tab);
    let base_tab = property(&format, keys.tab);
    let editorconfig = keys
        .editorconfig
        .map_or(Data::Undefined, |key| property(&format, key));

    if [&width, &base_width, &tab, &base_tab, &editorconfig]
        .into_iter()
        .any(is_unknown)
    {
        return Scoped::Unread;
    }

    if config.formatter == Formatter::Biome {
        match biome_skips(&value, &format, path, &dirname(&config.file)) {
            None => return Scoped::Unread,
            Some(true) => return Scoped::Skip,
            Some(false) => {}
        }
    }

    Scoped::Setting(Setting {
        columns: number(&width).or_else(|| number(&base_width)),
        tab: number(&tab).or_else(|| number(&base_tab)),
        editorconfig: keys.editorconfig.is_none() || matches!(editorconfig, Data::Bool(true)),
    })
}

fn section_pattern(line: &str) -> Option<&str> {
    let rest = js_trim_start(line).strip_prefix('[')?;
    let content = rest.trim_end_matches(is_js_whitespace).strip_suffix(']')?;
    (!content.is_empty() && !content.contains(['\n', '\r', '\u{2028}', '\u{2029}']))
        .then_some(content)
}

fn pair(line: &str) -> Option<(String, String)> {
    let rest = js_trim_start(line);
    let end = rest
        .find(|char: char| !(char.is_ascii_alphanumeric() || char == '_'))
        .unwrap_or(rest.len());

    if end == 0 {
        return None;
    }

    let value = js_trim_start(js_trim_start(&rest[end..]).strip_prefix('=')?);
    let value = &value[..value.find(['#', ';']).unwrap_or(value.len())];
    Some((rest[..end].to_lowercase(), js_trim(value).to_lowercase()))
}

fn parsed_editor(dir: &str) -> Arc<EditorFile> {
    EDITORS.get(dir, || Arc::new(read_editor(dir)))
}

fn read_editor(dir: &str) -> EditorFile {
    let path = candidate(dir, ".editorconfig");
    let editor = read_text(&path).map(|text| {
        let mut sections = vec![Section {
            pattern: None,
            values: IndexMap::new(),
        }];

        for line in text.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if let Some(pattern) = section_pattern(line) {
                sections.push(Section {
                    pattern: Some(Pattern {
                        nested: pattern.contains('/'),
                        glob: EditorGlob::new(pattern),
                    }),
                    values: IndexMap::new(),
                });
                continue;
            }

            if let Some((key, value)) = pair(line).filter(|(key, _)| key != "__proto__") {
                let last = sections.last_mut().expect("the preamble section exists");
                last.values.insert(key, value);
            }
        }

        let root = sections[0]
            .values
            .get("root")
            .is_some_and(|root| root == "true");
        Editor { root, sections }
    });

    EditorFile { path, editor }
}

fn editor_match(pattern: &Pattern, file: &str, root: &str) -> bool {
    let target = if pattern.nested {
        relative(root, file)
    } else {
        basename(file).to_owned()
    };

    pattern.glob.matches(&target)
}

fn editorconfig(
    path: &str,
    unread: &mut Vec<String>,
    biome_dir: Option<&str>,
    nearest: bool,
) -> EditorValues {
    let mut values = EditorValues::default();
    let mut saw_width = false;
    let mut saw_tab = false;
    let mut dir = biome_dir.map_or_else(|| dirname(path), str::to_owned);
    loop {
        let present = holds(&dir, ".editorconfig");
        let mut root = false;
        if present {
            let found = parsed_editor(&dir);
            match &found.editor {
                None => unread.push(found.path.clone()),
                Some(editor) => {
                    root = editor.root;
                    let mut local: IndexMap<&str, &str> = IndexMap::new();
                    for section in &editor.sections {
                        if section
                            .pattern
                            .as_ref()
                            .is_none_or(|pattern| editor_match(pattern, path, &dir))
                        {
                            for (key, value) in &section.values {
                                local.insert(key, value);
                            }
                        }
                    }

                    if let Some(width) = local.get("max_line_length").filter(|_| !saw_width) {
                        saw_width = true;
                        values.columns = number(&Data::Number(js_number(width)));
                        if values.columns.is_some() {
                            values.file = Some(found.path.clone());
                        }
                    }

                    let tab_width = local.get("tab_width");
                    let indent_size = local.get("indent_size");
                    if !saw_tab && (tab_width.is_some() || indent_size.is_some()) {
                        saw_tab = true;
                        let as_number = |value: Option<&&str>| {
                            number(&Data::Number(
                                value.map_or(f64::NAN, |value| js_number(value)),
                            ))
                        };

                        values.tab = as_number(tab_width).or_else(|| as_number(indent_size));
                    }
                }
            }
        }

        if biome_dir.is_some()
            || nearest && present
            || root
            || holds(&dir, ".git")
            || holds(&dir, ".hg")
        {
            break;
        }

        let parent = dirname(&dir);
        if parent == dir {
            break;
        }

        dir = parent;
    }

    values
}

fn resolved_width(setting: &Setting, config: &ConfigFile, file: &str) -> f64 {
    let formatter = config.formatter;
    setting
        .columns
        .or_else(|| {
            setting.editorconfig.then(|| {
                let biome_dir = (formatter == Formatter::Biome).then(|| dirname(&config.file));
                editorconfig(
                    file,
                    &mut Vec::new(),
                    biome_dir.as_deref(),
                    formatter == Formatter::Oxfmt,
                )
                .columns
            })?
        })
        .unwrap_or(formatter.keys().default_width)
}

fn absolute(path: &str) -> String {
    if is_absolute(path) {
        return resolve("/", path);
    }

    let cwd = std::env::current_dir()
        .map_or_else(|_| "/".to_owned(), |cwd| cwd.to_string_lossy().into_owned());
    resolve(&cwd, path)
}

pub fn formatter_width(path: &str) -> Width {
    let resolved = absolute(path);
    let parent = dirname(&resolved);
    let dir = real_directory(&parent).unwrap_or(parent);
    let file = join(&dir, basename(path));
    measure(&dir, &file)
}

fn measure(dir: &str, file: &str) -> Width {
    let mut unread = Vec::new();
    let dependency = dependent(dir);

    let mut choices: Vec<(Arc<ConfigFile>, Setting)> = Vec::new();
    let mut current = dir.to_owned();
    loop {
        let found = directory(&current);
        unread.extend(found.unread.iter().cloned());
        let mut valid = Vec::new();
        for config in &found.configs {
            match setting(config, file) {
                Scoped::Unread => unread.push(config.file.clone()),
                Scoped::Skip => {}
                Scoped::Setting(setting) => valid.push((config.clone(), setting)),
            }
        }

        if !valid.is_empty() {
            choices = valid;
            break;
        }

        let parent = dirname(&current);
        if parent == current {
            break;
        }

        current = parent;
    }

    let listed: Vec<usize> = (0..choices.len())
        .filter(|&index| dependency.contains(&choices[index].0.formatter))
        .collect();

    let mut ranked: Vec<usize> = if listed.len() == 1 {
        listed
    } else {
        (0..choices.len()).collect()
    };

    if ranked.len() > 1 {
        let widths: Vec<f64> = choices
            .iter()
            .map(|(config, setting)| resolved_width(setting, config, file))
            .collect();

        ranked.sort_by(|&left, &right| {
            widths[left]
                .partial_cmp(&widths[right])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    let chosen = ranked.first().map(|&index| &choices[index]);
    let formatter = chosen.map(|(config, _)| config.formatter).or_else(|| {
        let mut sorted = dependency.to_vec();
        sorted.sort_by(|left, right| {
            left.keys()
                .default_width
                .total_cmp(&right.keys().default_width)
        });
        sorted.first().copied()
    });

    let own = chosen.map(|(_, setting)| setting);
    let reads = formatter
        .is_some_and(|formatter| own.map_or(formatter != Formatter::Biome, |own| own.editorconfig));

    let chosen_editor = if reads {
        let biome_dir = chosen
            .filter(|(config, _)| config.formatter == Formatter::Biome)
            .map(|(config, _)| dirname(&config.file));

        editorconfig(
            file,
            &mut unread,
            biome_dir.as_deref(),
            formatter == Some(Formatter::Oxfmt),
        )
    } else {
        EditorValues::default()
    };

    let own_columns = own.and_then(|own| own.columns);
    let columns = own_columns
        .or(chosen_editor.columns)
        .or_else(|| formatter.map(|formatter| formatter.keys().default_width))
        .unwrap_or(80.0);

    let tab = own
        .and_then(|own| own.tab)
        .or(chosen_editor.tab)
        .unwrap_or(2.0);
    let source = match (
        own_columns,
        chosen,
        chosen_editor.columns,
        chosen_editor.file,
        formatter,
    ) {
        (Some(_), Some((config, _)), _, _, _) => WidthSource::Config(config.file.clone()),
        (None, _, Some(_), Some(editor), _) => WidthSource::Config(editor),
        (_, _, _, _, Some(formatter)) => WidthSource::Default(formatter),
        _ => WidthSource::Fallback,
    };

    Width {
        columns,
        tab,
        source,
        unread,
    }
}

#[cfg(test)]
#[path = "width_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "width_growth_tests.rs"]
mod growth_tests;
