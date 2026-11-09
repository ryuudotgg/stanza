use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::rc::Rc;

use indexmap::IndexMap;

use super::CONFIG;
use super::bun_glob::bun_match;
use super::editor_glob::EditorGlob;
use super::evaluate::{Object, Value, property};
use super::json;
use super::module::Session;
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
    value: Value,
}

#[derive(Clone)]
enum Read {
    Config(Rc<ConfigFile>),
    Skip,
    Unread,
}

#[derive(Clone)]
struct Directory {
    configs: Vec<Rc<ConfigFile>>,
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

struct Section {
    pattern: Option<String>,
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

#[derive(Default)]
struct Caches {
    parsed: HashMap<String, Read>,
    directories: HashMap<String, Directory>,
    dependencies: HashMap<String, Rc<Vec<Formatter>>>,
    editor_globs: HashMap<String, Rc<EditorGlob>>,
    editors: HashMap<String, Rc<EditorFile>>,
    widths: HashMap<String, Width>,
}

thread_local! {
    static CACHES: RefCell<Caches> = RefCell::default();
}

fn cached<K, Q, V>(pick: impl Fn(&mut Caches) -> &mut HashMap<K, V>, key: &Q) -> Option<V>
where
    K: std::hash::Hash + Eq + std::borrow::Borrow<Q>,
    Q: std::hash::Hash + Eq + ?Sized,
    V: Clone,
{
    CACHES.with_borrow_mut(|caches| pick(caches).get(key).cloned())
}

fn remember<K: std::hash::Hash + Eq, V>(
    pick: impl Fn(&mut Caches) -> &mut HashMap<K, V>,
    key: K,
    value: V,
) {
    CACHES.with_borrow_mut(|caches| {
        pick(caches).insert(key, value);
    });
}

const fn is_object(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

const fn is_unknown(value: &Value) -> bool {
    matches!(value, Value::Unknown)
}

const fn is_undefined(value: &Value) -> bool {
    matches!(value, Value::Undefined)
}

fn read_text(file: &str) -> Option<String> {
    fs::read(file)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

fn spread(session: &Session, value: &Value, into: &mut IndexMap<String, Value>) {
    match value {
        Value::Object(object) => {
            for key in object.keys() {
                let item = object.get(session, &key);
                into.insert(key, item);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                into.insert(index.to_string(), item.clone());
            }
        }
        Value::String(text) => {
            for (index, unit) in text.encode_utf16().enumerate() {
                into.insert(
                    index.to_string(),
                    Value::String(String::from_utf16_lossy(&[unit]).into()),
                );
            }
        }
        _ => {}
    }
}

fn spread_all(session: &Session, values: &[&Value]) -> IndexMap<String, Value> {
    let mut entries = IndexMap::new();
    for value in values {
        spread(session, value, &mut entries);
    }

    entries
}

fn object_of(entries: IndexMap<String, Value>) -> Value {
    Value::Object(Object::plain(
        entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect(),
    ))
}

fn merged(session: &Session, values: &[&Value]) -> Value {
    object_of(spread_all(session, values))
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

fn scalar(value: &str) -> Value {
    let text = without_comment(js_trim(value));
    if !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Value::Number(js_number(text));
    }

    if text == "true" || text == "false" {
        return Value::Bool(text == "true");
    }

    let quoted = text.len() >= 2
        && (text.starts_with('"') && text.ends_with('"')
            || text.starts_with('\'') && text.ends_with('\''))
        && !text[1..text.len() - 1].contains(['\n', '\r', '\u{2028}', '\u{2029}']);

    if quoted {
        return Value::String(text[1..text.len() - 1].into());
    }

    Value::String(text.into())
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

fn flat(text: &str, separator: char) -> Value {
    let mut entries = IndexMap::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if js_trim(line).is_empty() || js_trim_start(line).starts_with('#') {
            continue;
        }

        let Some((key, raw)) = flat_key(line, separator) else {
            return Value::Unknown;
        };

        if key == "overrides" || raw.is_empty() {
            return Value::Unknown;
        }

        if key != "__proto__" {
            entries.insert(key.to_owned(), scalar(raw));
        }
    }

    object_of(entries)
}

fn biome_active(session: &Session, value: &Value) -> bool {
    if !is_object(value) {
        return true;
    }

    let formatter = property(session, value, "formatter");
    let javascript = property(
        session,
        &property(session, value, "javascript"),
        "formatter",
    );
    if matches!(property(session, &formatter, "enabled"), Value::Bool(false))
        || matches!(
            property(session, &javascript, "enabled"),
            Value::Bool(false)
        )
    {
        return false;
    }

    if !is_undefined(&formatter) || !is_undefined(&javascript) {
        return true;
    }

    match property(session, value, "overrides") {
        Value::Unknown => true,
        Value::Array(entries) => entries.iter().any(|entry| {
            is_object(entry)
                && (!is_undefined(&property(session, entry, "formatter"))
                    || !is_undefined(&property(
                        session,
                        &property(session, entry, "javascript"),
                        "formatter",
                    )))
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

fn merged_biome(session: &Session, file: &str, value: Value, seen: &mut Vec<String>) -> Value {
    if !is_object(&value) || seen.iter().any(|item| item == file) {
        return Value::Unknown;
    }

    let list = match property(session, &value, "extends") {
        Value::Undefined => return value,
        Value::String(entry) => vec![entry.to_string()],
        Value::Array(items) => {
            let strings: Option<Vec<String>> = items
                .iter()
                .map(|item| match item {
                    Value::String(text) => Some(text.to_string()),
                    _ => None,
                })
                .collect();

            let Some(strings) = strings else {
                return Value::Unknown;
            };

            strings
        }
        _ => return Value::Unknown,
    };

    seen.push(file.to_owned());

    let mut result = object_of(IndexMap::new());
    for entry in list {
        let Some(target) = biome_target(file, &entry)
            .filter(|target| is_jsonc_name(target) && fs::metadata(target).is_ok())
        else {
            return Value::Unknown;
        };

        let parent = session.exported(&target, "default", &Rc::default());
        let parent = merged_biome(session, &target, parent, seen);
        if !is_object(&parent) {
            return Value::Unknown;
        }

        result = merge_biome(session, &result, &parent);
    }

    seen.retain(|item| item != file);
    merge_biome(session, &result, &value)
}

fn merge_biome(session: &Session, base: &Value, next: &Value) -> Value {
    let mut result = spread_all(session, &[base, next]);
    let formatter = (
        property(session, base, "formatter"),
        property(session, next, "formatter"),
    );

    if !is_undefined(&formatter.0) || !is_undefined(&formatter.1) {
        result.insert(
            "formatter".to_owned(),
            merged(session, &[&formatter.0, &formatter.1]),
        );
    }

    let javascript = (
        property(session, base, "javascript"),
        property(session, next, "javascript"),
    );

    if !is_undefined(&javascript.0) || !is_undefined(&javascript.1) {
        let mut inner = spread_all(session, &[&javascript.0, &javascript.1]);
        let formatter = (
            property(session, &javascript.0, "formatter"),
            property(session, &javascript.1, "formatter"),
        );

        if !is_undefined(&formatter.0) || !is_undefined(&formatter.1) {
            inner.insert(
                "formatter".to_owned(),
                merged(session, &[&formatter.0, &formatter.1]),
            );
        }

        result.insert("javascript".to_owned(), object_of(inner));
    }

    object_of(result)
}

fn readable(session: &Session, file: &str) -> Read {
    if let Some(found) = cached(|caches| &mut caches.parsed, file) {
        return found;
    }

    let name = basename(file);
    let formatter = Formatter::of(file);
    let mut value = if name == ".prettierrc.toml" {
        read_text(file).map_or(Value::Unknown, |text| flat(&text, '='))
    } else if name.ends_with(".yaml") || name.ends_with(".yml") {
        read_text(file).map_or(Value::Unknown, |text| flat(&text, ':'))
    } else if name == ".prettierrc" {
        match session.module_at(file) {
            Some(module) if module.program.is_some() => {
                session.exported(file, "default", &Rc::default())
            }
            Some(module) => flat(&module.text, ':'),
            None => Value::Unknown,
        }
    } else if name == "package.json" {
        let manifest = session.exported(file, "default", &Rc::default());
        property(session, &manifest, "prettier")
    } else {
        session.exported(file, "default", &Rc::default())
    };

    if formatter == Formatter::Biome && is_object(&value) {
        value = merged_biome(session, file, value, &mut Vec::new());
    }

    let result = if is_undefined(&value)
        || formatter == Formatter::Biome && is_object(&value) && !biome_active(session, &value)
    {
        Read::Skip
    } else if is_object(&value) {
        Read::Config(Rc::new(ConfigFile {
            file: file.to_owned(),
            formatter,
            value,
        }))
    } else {
        Read::Unread
    };

    remember(|caches| &mut caches.parsed, file.to_owned(), result.clone());
    result
}

#[cfg(test)]
thread_local! {
    static CANDIDATES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn candidate(dir: &str, name: &str) -> String {
    #[cfg(test)]
    CANDIDATES.set(CANDIDATES.get() + 1);

    join(dir, name)
}

fn directory(session: &Session, dir: &str) -> Directory {
    if let Some(found) = cached(|caches| &mut caches.directories, dir) {
        return found;
    }

    let mut found = Directory {
        configs: Vec::new(),
        unread: Vec::new(),
    };

    for name in FILES.into_iter().filter(|name| holds(dir, name)) {
        let file = candidate(dir, name);
        match readable(session, &file) {
            Read::Config(config) => found.configs.push(config),
            Read::Unread => found.unread.push(file),
            Read::Skip => {}
        }
    }

    remember(
        |caches| &mut caches.directories,
        dir.to_owned(),
        found.clone(),
    );
    found
}

fn manifest_formatters(session: &Session, dir: &str) -> Option<Vec<Formatter>> {
    let manifest = json::parse(&read_text(&join(dir, "package.json"))?)?;
    if matches!(manifest, Value::Null) {
        return None;
    }

    let names = spread_all(
        session,
        &[
            &property(session, &manifest, "dependencies"),
            &property(session, &manifest, "devDependencies"),
        ],
    );

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

fn dependent(session: &Session, dir: &str) -> Rc<Vec<Formatter>> {
    let mut visited = Vec::new();
    let mut found = Rc::new(Vec::new());
    let mut current = dir.to_owned();
    loop {
        if let Some(hit) = cached(|caches| &mut caches.dependencies, &current) {
            found = hit;
            break;
        }

        visited.push(current.clone());

        let formatters = holds(&current, "package.json")
            .then(|| manifest_formatters(session, &current))
            .flatten()
            .filter(|formatters| !formatters.is_empty());

        if let Some(formatters) = formatters {
            found = Rc::new(formatters);
            break;
        }

        let parent = dirname(&current);
        if parent == current {
            break;
        }

        current = parent;
    }

    for dir in visited {
        remember(|caches| &mut caches.dependencies, dir, found.clone());
    }

    found
}

fn number(value: &Value) -> Option<f64> {
    match *value {
        Value::Number(number) if number.fract() == 0.0 && number > 0.0 && number.is_finite() => {
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

fn patterns(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::String(text) => Some(vec![text.to_string()]),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => Some(text.to_string()),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn holds_unknown(session: &Session, value: &Value, depth: usize) -> bool {
    if depth > 10_000 {
        return true;
    }

    match value {
        Value::Unknown => true,
        Value::Array(items) => items
            .iter()
            .any(|item| holds_unknown(session, item, depth + 1)),
        Value::Object(object) => object
            .keys()
            .iter()
            .any(|key| holds_unknown(session, &object.get(session, key), depth + 1)),
        _ => false,
    }
}

fn legacy_pattern(pattern: &str) -> String {
    if pattern.contains('/') {
        pattern.to_owned()
    } else {
        format!("**/{pattern}")
    }
}

fn scoped(session: &Session, config: &ConfigFile, path: &str) -> Value {
    let mut value = config.value.clone();
    let overrides = match property(session, &value, "overrides") {
        Value::Undefined => return value,
        Value::Array(entries) => entries,
        _ => return Value::Unknown,
    };

    let keys = config.formatter.keys();
    let biome = keys.options == "formatter";
    let root = dirname(&config.file);
    for entry in overrides.iter() {
        if !is_object(entry) {
            return Value::Unknown;
        }

        let current = property(session, entry, keys.files);
        let fallback = match current {
            Value::Undefined | Value::Null => keys
                .legacy_files
                .map_or(Value::Undefined, |key| property(session, entry, key)),
            _ => current.clone(),
        };

        let files = patterns(&fallback);
        let options = property(session, entry, keys.options);
        let javascript = property(session, entry, "javascript");
        let exclude = property(session, entry, "excludeFiles");
        let excluded = patterns(&exclude);
        let Some(files) = files else {
            return Value::Unknown;
        };

        if holds_unknown(session, &options, 0)
            || holds_unknown(session, &javascript, 0)
            || !is_undefined(&exclude) && excluded.is_none()
            || !is_undefined(&options) && !is_object(&options)
            || !is_undefined(&javascript) && !is_object(&javascript)
        {
            return Value::Unknown;
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

        let base_formatter = property(session, &value, "formatter");
        let base_javascript = property(session, &value, "javascript");
        if holds_unknown(session, &base_formatter, 0) || holds_unknown(session, &base_javascript, 0)
        {
            return Value::Unknown;
        }

        let mut next = spread_all(session, &[&value]);
        if biome {
            next.insert(
                "formatter".to_owned(),
                merged(session, &[&base_formatter, &options]),
            );

            let mut inner = spread_all(session, &[&base_javascript, &javascript]);
            inner.insert(
                "formatter".to_owned(),
                merged(
                    session,
                    &[
                        &property(session, &base_javascript, "formatter"),
                        &property(session, &javascript, "formatter"),
                    ],
                ),
            );

            next.insert("javascript".to_owned(), object_of(inner));
        } else {
            spread(session, &options, &mut next);
        }

        value = object_of(next);
    }

    value
}

fn biome_skips(
    session: &Session,
    value: &Value,
    format: &Value,
    path: &str,
    root: &str,
) -> Option<bool> {
    let files = property(session, value, "files");
    if is_unknown(&files) {
        return None;
    }

    let includes = property(session, &files, "includes");
    let include = property(session, &files, "include");
    let ignore = property(session, &files, "ignore");
    if [&includes, &include, &ignore].into_iter().any(is_unknown) {
        return None;
    }

    let listed = |value: &Value| {
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

    let formatter_includes = property(session, format, "includes");
    if is_unknown(&formatter_includes) {
        return None;
    }

    let scoped = listed(&formatter_includes)?;
    Some(scoped.is_some_and(|scoped| !ordered_includes(&scoped, path, root)))
}

fn setting(session: &Session, config: &ConfigFile, path: &str) -> Scoped {
    let value = scoped(session, config, path);
    if is_unknown(&value) {
        return Scoped::Unread;
    }

    if config.formatter == Formatter::Biome && !biome_active(session, &value) {
        return Scoped::Skip;
    }

    let keys = config.formatter.keys();
    let format = if keys.options == "formatter" {
        property(session, &value, "formatter")
    } else {
        value.clone()
    };

    let javascript = property(
        session,
        &property(session, &value, "javascript"),
        "formatter",
    );
    let width = property(session, &javascript, keys.width);
    let base_width = property(session, &format, keys.width);
    let tab = property(session, &javascript, keys.tab);
    let base_tab = property(session, &format, keys.tab);
    let editorconfig = keys
        .editorconfig
        .map_or(Value::Undefined, |key| property(session, &format, key));

    if [&width, &base_width, &tab, &base_tab, &editorconfig]
        .into_iter()
        .any(is_unknown)
    {
        return Scoped::Unread;
    }

    if config.formatter == Formatter::Biome {
        match biome_skips(session, &value, &format, path, &dirname(&config.file)) {
            None => return Scoped::Unread,
            Some(true) => return Scoped::Skip,
            Some(false) => {}
        }
    }

    Scoped::Setting(Setting {
        columns: number(&width).or_else(|| number(&base_width)),
        tab: number(&tab).or_else(|| number(&base_tab)),
        editorconfig: keys.editorconfig.is_none() || matches!(editorconfig, Value::Bool(true)),
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

fn parsed_editor(dir: &str) -> Rc<EditorFile> {
    if let Some(found) = cached(|caches| &mut caches.editors, dir) {
        return found;
    }

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
                    pattern: Some(pattern.to_owned()),
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

    let found = Rc::new(EditorFile { path, editor });
    remember(|caches| &mut caches.editors, dir.to_owned(), found.clone());
    found
}

fn editor_match(pattern: &str, file: &str, root: &str) -> bool {
    let target = if pattern.contains('/') {
        relative(root, file)
    } else {
        basename(file).to_owned()
    };

    let glob = cached(|caches| &mut caches.editor_globs, pattern).unwrap_or_else(|| {
        let glob = Rc::new(EditorGlob::new(pattern));
        remember(
            |caches| &mut caches.editor_globs,
            pattern.to_owned(),
            glob.clone(),
        );
        glob
    });

    glob.matches(&target)
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
                            .as_deref()
                            .is_none_or(|pattern| editor_match(pattern, path, &dir))
                        {
                            for (key, value) in &section.values {
                                local.insert(key, value);
                            }
                        }
                    }

                    if let Some(width) = local.get("max_line_length").filter(|_| !saw_width) {
                        saw_width = true;
                        values.columns = number(&Value::Number(js_number(width)));
                        if values.columns.is_some() {
                            values.file = Some(found.path.clone());
                        }
                    }

                    let tab_width = local.get("tab_width");
                    let indent_size = local.get("indent_size");
                    if !saw_tab && (tab_width.is_some() || indent_size.is_some()) {
                        saw_tab = true;
                        let as_number = |value: Option<&&str>| {
                            number(&Value::Number(
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
    if let Some(found) = cached(|caches| &mut caches.widths, &file) {
        return found;
    }

    let width = CONFIG.with(|config| measure(&config.session, &dir, &file));
    remember(|caches| &mut caches.widths, file, width.clone());
    width
}

fn measure(session: &Session, dir: &str, file: &str) -> Width {
    let mut unread = Vec::new();
    let dependency = dependent(session, dir);

    let mut choices: Vec<(Rc<ConfigFile>, Setting)> = Vec::new();
    let mut current = dir.to_owned();
    loop {
        let found = directory(session, &current);
        unread.extend(found.unread.iter().cloned());
        let mut valid = Vec::new();
        for config in &found.configs {
            match setting(session, config, file) {
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
