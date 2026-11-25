mod biome;
mod editorconfig;
mod flat;

use std::fs;
use std::sync::{Arc, LazyLock, PoisonError};

use indexmap::IndexMap;
use serde_json::Value as Json;

use self::biome::{MERGED, merged_biome, under_biome};
use self::editorconfig::{EditorValues, editorconfig};
use self::flat::toml;
use super::data::{Data, property};
use super::evaluator;
use super::find::{Loader, Manifest, json_file, manifest, resolve_exported};
use super::module::script_name;
use super::override_glob::{Anchor, OverrideGlob, Target};
use super::yaml;
use crate::directories::{holds, real_directory};
use crate::engine::doc::Ruler;
use crate::languages::{FALLBACK_COLUMNS, FALLBACK_LAYOUT, Layout, Limit};
use crate::memo::Memo;
use crate::paths::{basename, dirname, is_absolute, join, relative, resolve};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Formatter {
    Oxfmt,
    Prettier,
    Biome,
}

impl Formatter {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Oxfmt => "oxfmt",
            Self::Prettier => "prettier",
            Self::Biome => "biome",
        }
    }

    const fn default_width(self) -> usize {
        match self {
            Self::Oxfmt => 100,
            Self::Prettier | Self::Biome => 80,
        }
    }

    const fn width_key(self) -> &'static str {
        match self {
            Self::Oxfmt | Self::Prettier => "printWidth",
            Self::Biome => "lineWidth",
        }
    }

    const fn ruler(self) -> Ruler {
        match self {
            Self::Oxfmt => Ruler::Oxfmt,
            Self::Prettier => Ruler::Prettier,
            Self::Biome => Ruler::Biome,
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
    pub limit: Limit,
    pub columns_text: String,
    pub tab: usize,
    pub formatter: Option<Formatter>,
    pub source: WidthSource,
    pub unread: Vec<String>,
}

impl Width {
    pub const fn layout(&self) -> Layout {
        Layout {
            limit: self.limit,
            tab: self.tab,
            ruler: match self.formatter {
                Some(formatter) => formatter.ruler(),
                None => FALLBACK_LAYOUT.ruler,
            },
        }
    }
}

pub const FILES: [&str; 30] = [
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
    "package.json",
    "package.yaml",
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.yml",
    ".prettierrc.yaml",
    ".prettierrc.json5",
    ".prettierrc.js",
    "prettier.config.js",
    ".prettierrc.ts",
    "prettier.config.ts",
    ".prettierrc.mjs",
    "prettier.config.mjs",
    ".prettierrc.mts",
    "prettier.config.mts",
    ".prettierrc.cjs",
    "prettier.config.cjs",
    ".prettierrc.cts",
    "prettier.config.cts",
    ".prettierrc.toml",
];

struct ConfigFile {
    file: String,
    formatter: Formatter,
    value: Data,
    under_biome: bool,
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

#[derive(Clone, Copy)]
enum Columns {
    Count(f64),
    Unlimited,
}

impl Columns {
    const fn limit(self) -> Limit {
        match self {
            Self::Count(count) => Limit::Columns(count as usize),
            Self::Unlimited => Limit::Unlimited,
        }
    }

    fn text(self) -> String {
        match self {
            Self::Count(count) => count.to_string(),
            Self::Unlimited => "unlimited".to_owned(),
        }
    }

    const fn rank(self) -> f64 {
        match self {
            Self::Count(count) => count,
            Self::Unlimited => f64::INFINITY,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UseTabs {
    Absent,
    Known(bool),
    Unknown,
}

struct Setting {
    columns: Option<Columns>,
    tab: Option<usize>,
    use_tabs: UseTabs,
    editorconfig: bool,
}

enum Scoped {
    Setting(Setting),
    Skip,
    Unread,
}

struct Dependencies {
    dir: String,
    formatters: Vec<Formatter>,
}

static PARSED: LazyLock<Memo<String, Read>> = LazyLock::new(Memo::default);

static DIRECTORIES: LazyLock<Memo<String, Arc<Directory>>> = LazyLock::new(Memo::default);

static DEPENDENCIES: LazyLock<Memo<String, Option<Arc<Dependencies>>>> =
    LazyLock::new(Memo::default);

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
    if let Data::Object(object) = value {
        for key in object.keys() {
            let item = object.get(&key);
            into.insert(key, item);
        }
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
    if values.iter().any(|value| is_unknown(value)) {
        return Data::Unknown;
    }

    object_of(spread_all(values))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Spelling {
    Yaml,
    JsonOrYaml,
}

// serde_json takes a lone CR as whitespace, but Prettier's YAML loader refuses such a file.
fn strict_json(bytes: &[u8]) -> Option<yaml::Tree> {
    let lone_cr = bytes
        .iter()
        .enumerate()
        .any(|(at, &byte)| byte == b'\r' && bytes.get(at + 1) != Some(&b'\n'));

    if lone_cr {
        return None;
    }

    yaml::json(std::str::from_utf8(bytes).ok()?)
}

fn prettier_yaml(file: &str, spelling: Spelling) -> Data {
    let Some(bytes) = yaml::read(file) else {
        return Data::Unknown;
    };

    let bytes = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(&bytes);
    let json = if spelling == Spelling::JsonOrYaml {
        strict_json(bytes)
    } else {
        None
    };

    json.or_else(|| yaml::parse(bytes))
        .map_or(Data::Unknown, |tree| Data::tree(&tree))
}

fn biome_active(value: &Data, under_biome: bool) -> bool {
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

    if under_biome || !is_undefined(&formatter) || !is_undefined(&javascript) {
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

const BIOME_LANGUAGES: [&str; 8] = [
    "javascript",
    "json",
    "css",
    "graphql",
    "grit",
    "html",
    "markdown",
    "yaml",
];

// oxfmt and Biome refuse the whole file over one width past their limit, even in an override that never applies.
fn bounded(value: Data, formatter: Formatter) -> Data {
    if formatter == Formatter::Prettier {
        return value;
    }

    let over = |options: &Data| matches!(property(options, formatter.width_key()), Data::Number(width) if width > 320.0);
    let biome = |section: &Data| {
        over(&property(section, "formatter"))
            || BIOME_LANGUAGES
                .iter()
                .any(|language| over(&property(&property(section, language), "formatter")))
    };

    let overrides = match property(&value, "overrides") {
        Data::Array(entries) => entries,
        _ => Arc::default(),
    };

    let refused = if formatter == Formatter::Biome {
        biome(&value) || overrides.iter().any(biome)
    } else {
        over(&value)
            || overrides
                .iter()
                .any(|entry| over(&property(entry, "options")))
    };

    if refused { Data::Unknown } else { value }
}

// Prettier loads a string config with require, then imports a package require cannot resolve, and never follows a string that config exports.
fn shared_prettier(file: &str, specifier: &str) -> Data {
    let package = !specifier.starts_with('.') && !is_absolute(specifier);
    let imported = || package.then(|| resolve_exported(specifier, file, Loader::Import))?;
    let value = match resolve_exported(specifier, file, Loader::Require) {
        Some(shared) => loaded(&shared, Loader::Require),
        None => imported().map_or(Data::Unknown, |shared| loaded(&shared, Loader::Import)),
    };

    if is_object(&value) {
        value
    } else {
        Data::Unknown
    }
}

// Node's require parses `.json` strictly and its import refuses JSON without an attribute, which Prettier never passes.
fn loaded(shared: &str, loader: Loader) -> Data {
    if script_name(shared) {
        evaluator::exported_default(shared)
    } else if loader == Loader::Require && shared.ends_with(".json") {
        json_file(shared).map_or(Data::Unknown, |json| Data::json(&json))
    } else {
        Data::Unknown
    }
}

fn falsy(value: &Data) -> bool {
    match value {
        Data::Undefined | Data::Null | Data::Bool(false) => true,
        Data::Number(number) => *number == 0.0 || number.is_nan(),
        Data::String(text) => text.is_empty(),
        _ => false,
    }
}

// A spelling of "prettier" without a backslash escape holds its literal bytes, so a manifest with neither is skipped unparsed.
fn mentions_prettier(bytes: &[u8]) -> bool {
    let key = b"prettier";
    bytes.windows(key.len()).any(|window| window == key) || bytes.contains(&b'\\')
}

fn prettier_key(manifest: &Data) -> Data {
    if is_object(manifest) || is_unknown(manifest) {
        property(manifest, "prettier")
    } else {
        Data::Undefined
    }
}

// Prettier's search skips a manifest JSON.parse rejects, a leading byte order mark included. A manifest the tree cannot answer for, valid JSON serde_json cannot hold included, still reaches the evaluator.
fn package_prettier(file: &str, name: &str) -> Data {
    let value = if name == "package.yaml" {
        match yaml::read(file) {
            None => return Data::Unknown,
            Some(bytes) if !mentions_prettier(&bytes) => return Data::Undefined,
            Some(_) => prettier_key(&prettier_yaml(file, Spelling::Yaml)),
        }
    } else {
        match &*manifest(file) {
            Manifest::Tree {
                json: Json::Object(fields),
                bom: false,
            } => fields.get("prettier").map_or(Data::Undefined, Data::json),
            Manifest::Unholdable { bom: false }
                if fs::read(file).is_ok_and(|bytes| mentions_prettier(&bytes)) =>
            {
                prettier_key(&evaluator::exported_default(file))
            }
            _ => Data::Undefined,
        }
    };

    if falsy(&value) {
        Data::Undefined
    } else {
        value
    }
}

fn readable(file: &str) -> Read {
    PARSED.get(file, || read_config(file))
}

fn read_config(file: &str) -> Read {
    let name = basename(file);
    let formatter = Formatter::of(file);
    let value = if formatter == Formatter::Biome {
        let mut done = MERGED.lock().unwrap_or_else(PoisonError::into_inner);
        merged_biome(file, &mut Vec::new(), &mut done).map_or(Data::Unknown, |merged| merged.value)
    } else if name == "package.json" || name == "package.yaml" {
        package_prettier(file, name)
    } else if name == ".prettierrc.toml" {
        read_text(file).map_or(Data::Unknown, |text| toml(&text))
    } else if name.ends_with(".yaml") || name.ends_with(".yml") {
        prettier_yaml(file, Spelling::Yaml)
    } else if name == ".prettierrc" {
        prettier_yaml(file, Spelling::JsonOrYaml)
    } else {
        bounded(evaluator::exported_default(file), formatter)
    };

    // Prettier loads a falsy config as empty and stops its search there.
    let value =
        if formatter == Formatter::Prettier && falsy(&value) && !name.starts_with("package.") {
            object_of(IndexMap::new())
        } else {
            value
        };

    let value = match value {
        Data::String(specifier) if formatter == Formatter::Prettier => {
            shared_prettier(file, &specifier)
        }
        value => value,
    };

    let is_biome = formatter == Formatter::Biome && is_object(&value);
    let nested = is_biome && under_biome(file, &value);
    if is_undefined(&value) || is_biome && !biome_active(&value, nested) {
        Read::Skip
    } else if is_object(&value) {
        Read::Config(Arc::new(ConfigFile {
            file: file.to_owned(),
            formatter,
            value,
            under_biome: nested,
        }))
    } else {
        Read::Unread
    }
}

fn candidate(dir: &str, name: &str) -> String {
    #[cfg(test)]
    tests::CANDIDATES.with(|count| count.set(count.get() + 1));

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

    let mut prettier_found = false;
    for name in FILES.into_iter().filter(|name| holds(dir, name)) {
        let prettier = Formatter::of(name) == Formatter::Prettier;
        if prettier && prettier_found {
            continue;
        }

        let file = candidate(dir, name);
        match readable(&file) {
            Read::Config(config) => found.configs.push(config),
            Read::Unread => found.unread.push(file),
            Read::Skip => continue,
        }

        prettier_found |= prettier;
    }

    found
}

pub(super) fn manifest_formatters(dir: &str) -> Vec<Formatter> {
    let read = manifest(&join(dir, "package.json"));
    let Some(manifest) = read.json() else {
        return Vec::new();
    };

    let listed = |name: &str| {
        ["dependencies", "devDependencies"].iter().any(|field| {
            manifest
                .get(field)
                .and_then(Json::as_object)
                .is_some_and(|names| names.contains_key(name))
        })
    };

    [
        ("oxfmt", Formatter::Oxfmt),
        ("@biomejs/biome", Formatter::Biome),
        ("prettier", Formatter::Prettier),
    ]
    .into_iter()
    .filter(|(name, _)| listed(name))
    .map(|(_, formatter)| formatter)
    .collect()
}

fn dependent(dir: &str) -> Option<Arc<Dependencies>> {
    DEPENDENCIES.get(dir, || {
        if holds(dir, "package.json") {
            let formatters = manifest_formatters(dir);
            if !formatters.is_empty() {
                return Some(Arc::new(Dependencies {
                    dir: dir.to_owned(),
                    formatters,
                }));
            }
        }

        let parent = dirname(dir);
        if parent == dir {
            None
        } else {
            dependent(parent)
        }
    })
}

fn whole(value: &Data) -> Option<f64> {
    match *value {
        Data::Number(number) if number.fract() == 0.0 && number > 0.0 && number.is_finite() => {
            Some(number)
        }
        _ => None,
    }
}

fn columns(value: &Data) -> Option<Columns> {
    whole(value).map(Columns::Count)
}

fn tab(value: &Data) -> Option<usize> {
    whole(value).map(|number| number as usize)
}

fn compiled(
    patterns: &[String],
    formatter: Formatter,
    target: &Target,
) -> Option<Vec<OverrideGlob>> {
    patterns
        .iter()
        .map(|pattern| OverrideGlob::new(pattern, formatter, target))
        .collect()
}

struct Include {
    negated: bool,
    glob: OverrideGlob,
}

fn ordered(entries: &[String], target: &Target) -> Option<Vec<Include>> {
    entries
        .iter()
        .map(|entry| {
            let pattern = entry.trim_start_matches('!');
            let glob = OverrideGlob::new(pattern, Formatter::Biome, target)?;
            Some(Include {
                negated: pattern.len() < entry.len(),
                glob,
            })
        })
        .collect()
}

fn included(entries: &[Include], target: &Target) -> bool {
    entries.iter().fold(false, |included, entry| {
        let matches = if entry.negated {
            entry.glob.covers(target)
        } else {
            entry.glob.matches(target, Anchor::Path)
        };

        if matches { !entry.negated } else { included }
    })
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

fn scoped(config: &ConfigFile, target: &Target) -> Data {
    let value = config.value.clone();
    let overrides = match property(&value, "overrides") {
        Data::Undefined => return value,
        Data::Array(entries) => entries,
        _ => return Data::Unknown,
    };

    let biome = config.formatter == Formatter::Biome;
    let base_includes = property(&property(&value, "formatter"), "includes");
    if biome && listed(&base_includes, |list| ordered(list, target)).is_none() {
        return Data::Unknown;
    }

    let mut fields: Option<IndexMap<String, Data>> = None;
    for entry in overrides.iter() {
        if !is_object(entry) {
            return Data::Unknown;
        }

        let current = property(entry, if biome { "includes" } else { "files" });
        let fallback = match current {
            Data::Undefined | Data::Null if biome => property(entry, "include"),
            Data::Undefined | Data::Null => Data::Undefined,
            _ => current.clone(),
        };

        let files = patterns(&fallback);
        let options = property(entry, if biome { "formatter" } else { "options" });
        let javascript = property(entry, "javascript");
        let exclude = property(entry, "excludeFiles");
        let excluded = patterns(&exclude);
        let Some(files) = files else {
            return Data::Unknown;
        };

        if !is_undefined(&exclude) && excluded.is_none()
            || !is_undefined(&options) && !is_object(&options)
            || !is_undefined(&javascript) && !is_object(&javascript)
        {
            return Data::Unknown;
        }

        let legacy = biome && is_undefined(&current);
        let Some(excluded) = compiled(&excluded.unwrap_or_default(), config.formatter, target)
        else {
            return Data::Unknown;
        };

        let formatter_includes = property(&options, "includes");
        if biome && listed(&formatter_includes, |list| ordered(list, target)).is_none() {
            return Data::Unknown;
        }

        let applies = if biome && !legacy {
            let Some(entries) = ordered(&files, target) else {
                return Data::Unknown;
            };

            included(&entries, target)
                && !excluded
                    .iter()
                    .any(|glob| glob.matches(target, Anchor::NameIfSlashless))
        } else {
            let Some(globs) = compiled(&files, config.formatter, target) else {
                return Data::Unknown;
            };

            globs.iter().any(|glob| glob.applies(target, &excluded))
        };

        if !applies {
            continue;
        }

        let next = fields.get_or_insert_with(|| spread_all(&[&value]));
        if biome {
            let base_formatter = next.get("formatter").cloned().unwrap_or_default();
            let base_javascript = next.get("javascript").cloned().unwrap_or_default();
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
            spread(&options, next);
        }
    }

    fields.map_or(value, object_of)
}

fn listed<T>(value: &Data, compile: impl FnOnce(&[String]) -> Option<T>) -> Option<Option<T>> {
    if is_undefined(value) {
        Some(None)
    } else {
        compile(&patterns(value)?).map(Some)
    }
}

fn biome_skips(value: &Data, format: &Data, target: &Target) -> Option<bool> {
    let files = property(value, "files");
    if is_unknown(&files) {
        return None;
    }

    let includes = property(&files, "includes");
    let include = property(&files, "include");
    let ignore = property(&files, "ignore");
    let formatter_includes = property(format, "includes");
    if [&includes, &include, &ignore, &formatter_includes]
        .into_iter()
        .any(is_unknown)
    {
        return None;
    }

    let entries = |list: &[String]| ordered(list, target);
    let globs = |list: &[String]| compiled(list, Formatter::Biome, target);
    let v2 = listed(&includes, entries)?;
    let v1 = listed(&include, globs)?;
    let ignored = listed(&ignore, globs)?.unwrap_or_default();
    let scoped = listed(&formatter_includes, entries)?;
    let excluded_by =
        |entries: Option<Vec<Include>>| entries.is_some_and(|entries| !included(&entries, target));
    let legacy = |glob: &OverrideGlob| glob.matches(target, Anchor::NameIfSlashless);
    Some(
        excluded_by(v2)
            || v1.is_some_and(|v1| !v1.iter().any(legacy))
            || ignored.iter().any(legacy)
            || excluded_by(scoped),
    )
}

fn setting(config: &ConfigFile, path: &str) -> Scoped {
    let target = Target::new(&relative(dirname(&config.file), path), config.formatter);
    let value = scoped(config, &target);
    if is_unknown(&value) {
        return Scoped::Unread;
    }

    let biome = config.formatter == Formatter::Biome;
    let format = if biome {
        property(&value, "formatter")
    } else {
        value.clone()
    };

    let biome_skipped = if biome {
        let Some(skipped) = biome_skips(&value, &format, &target) else {
            return Scoped::Unread;
        };

        if !biome_active(&value, config.under_biome) {
            return Scoped::Skip;
        }

        skipped
    } else {
        false
    };

    let javascript = property(&property(&value, "javascript"), "formatter");
    let width_key = config.formatter.width_key();
    let tab_key = if biome { "indentWidth" } else { "tabWidth" };
    let width = property(&javascript, width_key);
    let base_width = property(&format, width_key);
    let javascript_tab = property(&javascript, tab_key);
    let base_tab = property(&format, tab_key);
    let use_tabs = match property(&format, "useTabs") {
        _ if config.formatter != Formatter::Oxfmt => UseTabs::Absent,
        Data::Bool(use_tabs) => UseTabs::Known(use_tabs),
        Data::Undefined => UseTabs::Absent,
        _ => UseTabs::Unknown,
    };

    let editorconfig = if biome {
        property(&format, "useEditorconfig")
    } else {
        Data::Undefined
    };

    if [
        &width,
        &base_width,
        &javascript_tab,
        &base_tab,
        &editorconfig,
    ]
    .into_iter()
    .any(is_unknown)
    {
        return Scoped::Unread;
    }

    if biome_skipped {
        return Scoped::Skip;
    }

    Scoped::Setting(Setting {
        columns: columns(&width).or_else(|| columns(&base_width)),
        tab: tab(&javascript_tab).or_else(|| tab(&base_tab)),
        use_tabs,
        editorconfig: !biome || matches!(editorconfig, Data::Bool(true)),
    })
}

fn anchor<'a>(
    formatter: Formatter,
    config: Option<&'a ConfigFile>,
    manifest: Option<&'a str>,
) -> Option<&'a str> {
    match formatter {
        Formatter::Prettier => None,
        Formatter::Oxfmt | Formatter::Biome => {
            config.map(|config| dirname(&config.file)).or(manifest)
        }
    }
}

fn resolved_width(setting: &Setting, config: &ConfigFile, file: &str) -> Columns {
    let formatter = config.formatter;
    setting
        .columns
        .or_else(|| {
            (setting.editorconfig && formatter != Formatter::Biome).then(|| {
                let anchor = anchor(formatter, Some(config), None);
                editorconfig(file, &mut Vec::new(), anchor, formatter, Some(setting))
                    .width
                    .map(|(columns, _)| columns)
            })?
        })
        .unwrap_or(Columns::Count(formatter.default_width() as f64))
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
    let dir = real_directory(parent).unwrap_or_else(|_| parent.to_owned());
    let file = join(&dir, basename(path));
    measure(&dir, &file)
}

fn measure(dir: &str, file: &str) -> Width {
    let mut unread = Vec::new();
    let dependency = dependent(dir);

    let mut choices: Vec<(Arc<ConfigFile>, Setting)> = Vec::new();
    let mut current = dir;
    loop {
        let found = directory(current);
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

        let parent = dirname(current);
        if parent == current {
            break;
        }

        current = parent;
    }

    let listed_formatters = dependency
        .as_ref()
        .map_or(&[][..], |found| &found.formatters[..]);

    let listed: Vec<usize> = (0..choices.len())
        .filter(|&index| listed_formatters.contains(&choices[index].0.formatter))
        .collect();

    let mut ranked: Vec<usize> = if listed.len() == 1 {
        listed
    } else {
        (0..choices.len()).collect()
    };

    if ranked.len() > 1 {
        let widths: Vec<Columns> = choices
            .iter()
            .map(|(config, setting)| resolved_width(setting, config, file))
            .collect();

        ranked.sort_by(|&left, &right| widths[left].rank().total_cmp(&widths[right].rank()));
    }

    let chosen = ranked.first().map(|&index| &choices[index]);
    let formatter = chosen.map(|(config, _)| config.formatter).or_else(|| {
        let mut sorted = listed_formatters.to_vec();
        sorted.sort_by_key(|formatter| formatter.default_width());
        sorted.first().copied()
    });

    let own = chosen.map(|(_, setting)| setting);
    let reading = formatter
        .filter(|&formatter| own.map_or(formatter != Formatter::Biome, |own| own.editorconfig));

    let chosen_editor = reading.map_or_else(EditorValues::default, |formatter| {
        let config = chosen.map(|(config, _)| &**config);
        let manifest = dependency.as_ref().map(|found| found.dir.as_str());
        editorconfig(
            file,
            &mut unread,
            anchor(formatter, config, manifest),
            formatter,
            own,
        )
    });

    let configured = chosen
        .and_then(|(config, setting)| Some((setting.columns?, config.file.clone())))
        .or(chosen_editor.width);

    let (columns, source) = match (configured, formatter) {
        (Some((columns, file)), _) => (columns, WidthSource::Config(file)),
        (None, Some(formatter)) => (
            Columns::Count(formatter.default_width() as f64),
            WidthSource::Default(formatter),
        ),
        (None, None) => (
            Columns::Count(FALLBACK_COLUMNS as f64),
            WidthSource::Fallback,
        ),
    };

    let tab = own
        .and_then(|own| own.tab)
        .or(chosen_editor.tab)
        .unwrap_or(FALLBACK_LAYOUT.tab);

    Width {
        limit: columns.limit(),
        columns_text: columns.text(),
        tab,
        formatter,
        source,
        unread,
    }
}

#[cfg(test)]
#[path = "../width_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../width_growth_tests.rs"]
mod growth_tests;
