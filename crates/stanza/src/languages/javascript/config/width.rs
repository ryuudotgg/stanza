use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use indexmap::IndexMap;
use serde_json::Value as Json;

use super::biome;
use super::data::{Data, property};
use super::editor_glob::{Rules, SectionGlob, SectionPath};
use super::evaluator;
use super::find::{Loader, Manifest, json_file, manifest, resolve_exported};
use super::module::script_name;
use super::object::array_index;
use super::override_glob::{Anchor, OverrideGlob, Target};
use super::yaml;
use crate::directories::{holds, real_directory};
use crate::engine::doc::Ruler;
use crate::languages::{FALLBACK_COLUMNS, FALLBACK_LAYOUT, Layout, Limit};
use crate::memo::Memo;
use crate::paths::{basename, dirname, is_absolute, join, relative, resolve};
use crate::text::{is_js_whitespace, js_trim, js_trim_start, utf16_len};

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

struct Heading {
    name: String,
    pairs: IndexMap<String, String>,
}

struct Section {
    name: String,
    properties: Properties,
    glob: Option<SectionGlob>,
}

enum Entry {
    Pair(String),
    Section(usize),
}

#[derive(Default)]
struct View {
    root: bool,
    sections: Vec<Section>,
    refused: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reader {
    Prettier,
    Rust,
    Biome,
}

impl Reader {
    const fn of(formatter: Formatter) -> Self {
        match formatter {
            Formatter::Prettier => Self::Prettier,
            Formatter::Oxfmt => Self::Rust,
            Formatter::Biome => Self::Biome,
        }
    }

    const fn breaks(self) -> &'static [char] {
        match self {
            Self::Prettier => &['\n', '\r'],
            Self::Rust | Self::Biome => &['\n'],
        }
    }

    const fn rules(self) -> Rules {
        match self {
            Self::Prettier | Self::Biome => Rules::EditorConfig,
            Self::Rust => Rules::Oxfmt,
        }
    }

    fn trim(self, text: &str) -> &str {
        match self {
            Self::Prettier => js_trim(text),
            Self::Rust | Self::Biome => text.trim(),
        }
    }
}

struct Editor {
    prettier: View,
    rust: View,
    biome: View,
}

impl Editor {
    const fn view(&self, reader: Reader) -> &View {
        match reader {
            Reader::Prettier => &self.prettier,
            Reader::Rust => &self.rust,
            Reader::Biome => &self.biome,
        }
    }
}

struct EditorFile {
    path: String,
    editor: Option<Editor>,
}

#[derive(Clone, Copy)]
enum Key {
    MaxLineLength,
    TabWidth,
    IndentSize,
    IndentStyle,
}

impl Key {
    fn of(name: &str) -> Option<Self> {
        match name {
            "max_line_length" => Some(Self::MaxLineLength),
            "tab_width" => Some(Self::TabWidth),
            "indent_size" => Some(Self::IndentSize),
            "indent_style" => Some(Self::IndentStyle),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Value {
    Number(u64),
    Off,
    Tab,
    Space,
    Unset,
    Other,
}

#[derive(Clone, Copy, Default)]
struct Properties {
    max_line_length: Option<Value>,
    tab_width: Option<Value>,
    indent_size: Option<Value>,
    indent_style: Option<Value>,
}

impl Properties {
    const fn slot(&mut self, key: Key) -> &mut Option<Value> {
        match key {
            Key::MaxLineLength => &mut self.max_line_length,
            Key::TabWidth => &mut self.tab_width,
            Key::IndentSize => &mut self.indent_size,
            Key::IndentStyle => &mut self.indent_style,
        }
    }

    const fn read_by(self, reader: Reader) -> bool {
        match reader {
            Reader::Biome => self.indent_size.is_some(),
            Reader::Prettier | Reader::Rust => self.fills(Self {
                max_line_length: None,
                tab_width: None,
                indent_size: None,
                indent_style: None,
            }),
        }
    }

    const fn fills(self, known: Self) -> bool {
        self.max_line_length.is_some() && known.max_line_length.is_none()
            || self.tab_width.is_some() && known.tab_width.is_none()
            || self.indent_size.is_some() && known.indent_size.is_none()
            || self.indent_style.is_some() && known.indent_style.is_none()
    }

    fn or(self, farther: Self) -> Self {
        Self {
            max_line_length: self.max_line_length.or(farther.max_line_length),
            tab_width: self.tab_width.or(farther.tab_width),
            indent_size: self.indent_size.or(farther.indent_size),
            indent_style: self.indent_style.or(farther.indent_style),
        }
    }
}

struct Dependencies {
    dir: String,
    formatters: Vec<Formatter>,
}

struct EditorLayout {
    columns: Option<Columns>,
    tab: Option<usize>,
}

#[derive(Default)]
struct EditorValues {
    width: Option<(Columns, String)>,
    tab: Option<usize>,
}

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

static PARSED: LazyLock<Memo<String, Read>> = LazyLock::new(Memo::default);

static DIRECTORIES: LazyLock<Memo<String, Arc<Directory>>> = LazyLock::new(Memo::default);

static DEPENDENCIES: LazyLock<Memo<String, Option<Arc<Dependencies>>>> =
    LazyLock::new(Memo::default);

static EDITORS: LazyLock<Memo<String, Arc<EditorFile>>> = LazyLock::new(Memo::default);

static PROJECT_ROOTS: LazyLock<Memo<String, bool>> = LazyLock::new(Memo::default);

// Locked for a whole merge: a per-file Memo would deadlock two threads entering one extends cycle from opposite ends.
static MERGED: LazyLock<Mutex<HashMap<String, Merged>>> = LazyLock::new(Mutex::default);

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

fn without_comment(text: &str) -> &str {
    let mut at = 0;
    while let Some(offset) = text[at..].find(is_js_whitespace) {
        let start = at + offset;
        let run = &text[start..];
        let after = run.trim_start_matches(is_js_whitespace);
        let hash = start + run.len() - after.len();
        if after.starts_with('#') {
            return &text[..start];
        }

        at = hash;
    }

    text
}

fn scalar(text: &str) -> Data {
    if !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && let Ok(number) = text.parse()
    {
        return Data::Number(number);
    }

    if text == "true" || text == "false" {
        return Data::Bool(text == "true");
    }

    let quoted = text.len() >= 2
        && (text.starts_with('"') && text.ends_with('"')
            || text.starts_with('\'') && text.ends_with('\''));

    if quoted {
        return Data::String(text[1..text.len() - 1].into());
    }

    Data::String(text.into())
}

fn is_key_char(char: char) -> bool {
    char.is_ascii_alphanumeric() || char == '_' || char == '-'
}

fn toml_key(line: &str) -> Option<(&str, &str)> {
    let end = line.find(|char| !is_key_char(char)).unwrap_or(line.len());
    if end == 0 {
        return None;
    }

    let rest = js_trim_start(&line[end..]).strip_prefix('=')?;
    Some((&line[..end], js_trim_start(rest)))
}

fn misread_comment_meets_lone_cr(text: &str, from: usize) -> bool {
    let mut units = 0;
    let start = text
        .char_indices()
        .find(|&(_, char)| {
            let reached = units >= from;
            units += char.len_utf16();
            reached
        })
        .map_or(text.len(), |(at, _)| at);

    let rest = &text[start..];
    let scanned = match rest.find('\n') {
        Some(end) => rest[..end].strip_suffix('\r').unwrap_or(&rest[..end]),
        None => rest,
    };

    scanned.contains('\r')
}

fn tolerates_trailing_cr(raw: &str) -> bool {
    let value = raw[..raw.find('#').unwrap_or(raw.len())].trim_end();
    value.starts_with(|char: char| char.is_ascii_digit() || char == '+' || char == '-')
        || matches!(value, "true" | "false" | "inf" | "nan")
}

fn toml(text: &str) -> Data {
    let mut entries = IndexMap::new();
    for line in text.split_inclusive('\n') {
        let line = line
            .strip_suffix('\n')
            .map_or(line, |line| line.strip_suffix('\r').unwrap_or(line));

        let kept = line.trim_end_matches(is_js_whitespace);
        let trailing_cr = line[kept.len()..].contains('\r');
        if kept.contains('\r') {
            return Data::Unknown;
        }

        let start = js_trim_start(kept);
        if start.is_empty() || start.starts_with('#') {
            if trailing_cr {
                return Data::Unknown;
            }

            continue;
        }

        let Some((key, raw)) = toml_key(kept) else {
            return Data::Unknown;
        };

        if key == "overrides" || raw.is_empty() {
            return Data::Unknown;
        }

        if trailing_cr && !tolerates_trailing_cr(raw) {
            return Data::Unknown;
        }

        // smol-toml scans a bare value's comment from the value's offset into the whole file.
        if !raw.starts_with(['"', '\'', '[', '{'])
            && let Some(hash) = raw.find('#')
            && misread_comment_meets_lone_cr(text, utf16_len(&raw[..hash]))
        {
            return Data::Unknown;
        }

        entries.insert(key.to_owned(), scalar(without_comment(raw)));
    }

    object_of(entries)
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

fn under_biome(file: &str, value: &Data) -> bool {
    matches!(property(value, "root"), Data::Bool(false))
        && biome::root_config(file).is_some_and(|parent| {
            Formatter::of(&parent) == Formatter::Biome
                && matches!(readable(&parent), Read::Config(_))
        })
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

fn is_jsonc_name(name: &str) -> bool {
    name.ends_with(".json") || name.ends_with(".jsonc")
}

fn biome_target(file: &str, entry: &str) -> Option<String> {
    #[cfg(test)]
    tests::BIOME_TARGETS.with(|targets| targets.set(targets.get() + 1));

    if entry == "//" {
        return biome::root_config(file);
    }

    let local = resolve(dirname(file), entry);
    if entry.starts_with('.')
        || is_absolute(entry)
        || is_jsonc_name(entry) && fs::metadata(&local).is_ok()
    {
        return Some(local);
    }

    resolve_exported(entry, file, Loader::Import)
}

fn biome_value(file: &str) -> Data {
    #[cfg(test)]
    tests::BIOME_READS.with(|reads| reads.set(reads.get() + 1));

    let value = read_text(file)
        .and_then(|text| biome::jsonc_tree(&text))
        .map_or(Data::Unknown, |json| Data::json(&json));

    bounded(value, Formatter::Biome)
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

#[derive(Clone)]
struct Merged {
    value: Data,
    height: usize,
}

impl Merged {
    const UNKNOWN: Self = Self {
        value: Data::Unknown,
        height: 0,
    };
}

struct TooDeep;

fn merged_biome(
    file: &str,
    seen: &mut Vec<String>,
    done: &mut HashMap<String, Merged>,
) -> Result<Merged, TooDeep> {
    if seen.iter().any(|item| item == file) {
        return Ok(Merged::UNKNOWN);
    }

    let known = done.get(file);
    if seen.len() + known.map_or(0, |known| known.height) > biome::NESTED_EXTENDS {
        return Err(TooDeep);
    }

    if let Some(known) = known {
        return Ok(known.clone());
    }

    let merged = merged_extends(file, biome_value(file), seen, done)?;
    done.insert(file.to_owned(), merged.clone());
    Ok(merged)
}

fn merged_extends(
    file: &str,
    value: Data,
    seen: &mut Vec<String>,
    done: &mut HashMap<String, Merged>,
) -> Result<Merged, TooDeep> {
    if !is_object(&value) {
        return Ok(Merged::UNKNOWN);
    }

    let list = match property(&value, "extends") {
        Data::Undefined => return Ok(Merged { value, height: 0 }),
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
                return Ok(Merged::UNKNOWN);
            };

            strings
        }
        _ => return Ok(Merged::UNKNOWN),
    };

    seen.push(file.to_owned());

    let mut result = object_of(IndexMap::new());
    let mut height = 0;
    for entry in list {
        if seen.len() > biome::NESTED_EXTENDS {
            return Err(TooDeep);
        }

        let Some(target) = biome_target(file, &entry) else {
            return Ok(Merged::UNKNOWN);
        };

        let parent = merged_biome(&target, seen, done)?;
        if !is_object(&parent.value) {
            return Ok(Merged::UNKNOWN);
        }

        result = merge_biome(&result, &parent.value);
        height = height.max(parent.height + 1);
    }

    seen.retain(|item| item != file);
    Ok(Merged {
        value: merge_biome(&result, &value),
        height,
    })
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

fn heading(line: &str) -> Option<Heading> {
    let name = line.strip_prefix('[')?.strip_suffix(']')?;
    if name.contains('\r') {
        return None;
    }

    Some(Heading {
        name: name.to_owned(),
        pairs: IndexMap::new(),
    })
}

fn prettier_truthy(value: &str) -> bool {
    match serde_json::from_str::<Json>(value) {
        Ok(Json::Bool(value)) => value,
        Ok(Json::Number(number)) => number.as_f64().is_some_and(|number| number != 0.0),
        Ok(Json::String(text)) => !text.is_empty(),
        Ok(_) => true,
        Err(_) => !value.is_empty(),
    }
}

fn biome_accepts(key: &str, value: &str, sectioned: bool) -> bool {
    let one_of = |words: &[&str]| words.contains(&value);
    match (sectioned, key) {
        (false, "root") => one_of(&["true", "false"]),
        (true, _) if one_of(&["unset", "off"]) => true,
        (true, "indent_style") => one_of(&["space", "tab"]),
        (true, "indent_size") => value == "tab" || value.parse::<u8>().is_ok_and(|size| size <= 24),
        (true, "end_of_line") => one_of(&["lf", "cr", "crlf"]),
        (true, "insert_final_newline") => one_of(&["true", "false"]),
        _ => true,
    }
}

fn parse_view(text: &str, reader: Reader) -> View {
    let refused = || View {
        refused: true,
        ..View::default()
    };

    let mut sections: Vec<Heading> = Vec::new();
    let mut entries = IndexMap::new();
    let mut current = None;
    for line in text.split(reader.breaks()) {
        let line = reader.trim(line);
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }

        if line.starts_with('[') {
            match heading(line) {
                Some(found) if reader == Reader::Prettier => {
                    current = Some(match entries.get(&found.name) {
                        Some(&Entry::Section(index)) => index,
                        _ => {
                            entries.insert(found.name.clone(), Entry::Section(sections.len()));
                            sections.push(found);
                            sections.len() - 1
                        }
                    });
                }
                Some(found) => {
                    sections.push(found);
                    current = Some(sections.len() - 1);
                }
                None if reader == Reader::Prettier => return View::default(),
                None if reader == Reader::Biome => return refused(),
                None => {}
            }

            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            if reader == Reader::Prettier {
                return View::default();
            }

            continue;
        };

        let key = reader.trim(key);
        let key = if reader == Reader::Biome {
            key.to_lowercase()
        } else {
            key.to_owned()
        };

        let value = reader.trim(value).to_owned();
        if reader == Reader::Biome && !biome_accepts(&key, &value, current.is_some()) {
            return refused();
        }

        match current {
            Some(index) => {
                sections[index].pairs.insert(key, value);
            }
            None => {
                entries.insert(key, Entry::Pair(value));
            }
        }
    }

    let prettier = reader == Reader::Prettier;
    let root = prettier
        && entries
            .iter()
            .filter_map(|(key, entry)| match entry {
                Entry::Pair(value) => Some((key, value)),
                Entry::Section(_) => None,
            })
            .rfind(|(key, _)| key.to_lowercase() == "root")
            .is_some_and(|(_, value)| prettier_truthy(value));

    if prettier {
        sections.sort_by_key(|section| match array_index(&section.name) {
            Some(index) => (0, u64::from(index)),
            None => (
                1,
                entries.get_index_of(&section.name).unwrap_or(usize::MAX) as u64,
            ),
        });
    }

    let sections = compiled_sections(sections, reader);
    View {
        root,
        sections,
        refused: false,
    }
}

fn compiled_sections(headings: Vec<Heading>, reader: Reader) -> Vec<Section> {
    headings
        .into_iter()
        .filter_map(|heading| {
            let properties = section_properties(&heading.pairs, reader);
            if heading.name.is_empty() || !properties.read_by(reader) {
                return None;
            }

            Some(Section {
                glob: SectionGlob::new(&heading.name, reader.rules()),
                name: heading.name,
                properties,
            })
        })
        .collect()
}

fn parsed_editor(dir: &str) -> Arc<EditorFile> {
    EDITORS.get(dir, || Arc::new(read_editor(dir)))
}

fn read_editor(dir: &str) -> EditorFile {
    let path = candidate(dir, ".editorconfig");
    let editor = read_text(&path).map(|text| {
        let prettier = parse_view(&text, Reader::Prettier);

        // Prettier escapes only `?*()[]` in the directory before each section, so braces and backslashes there stay glob syntax.
        let live_prefix = dir.contains(['{', '}', '\\']) && !prettier.sections.is_empty();
        Editor {
            prettier: if live_prefix {
                View {
                    refused: true,
                    ..View::default()
                }
            } else {
                prettier
            },
            rust: parse_view(&text, Reader::Rust),
            biome: parse_view(&text, Reader::Biome),
        }
    });

    EditorFile { path, editor }
}

fn prettier_value(key: Key, raw: &str) -> Value {
    let known = matches!(key, Key::IndentSize | Key::IndentStyle);
    let word = match serde_json::from_str::<Json>(raw) {
        Ok(Json::Number(number)) => {
            return number
                .as_f64()
                .filter(|&number| number > 0.0 && number <= MAX_SAFE_INTEGER)
                .filter(|number| number.fract() == 0.0)
                .map_or(Value::Other, |number| Value::Number(number as u64));
        }
        Ok(Json::String(text)) => text,
        Ok(_) => return Value::Other,
        Err(_) if known => raw.to_lowercase(),
        Err(_) => raw.to_owned(),
    };

    match word.as_str() {
        "off" if matches!(key, Key::MaxLineLength) => Value::Off,
        "tab" if known => Value::Tab,
        "space" if known => Value::Space,
        _ => Value::Other,
    }
}

fn rust_value(key: Key, raw: &str, reader: Reader) -> Option<Value> {
    let word = |name: &str| raw.eq_ignore_ascii_case(name);
    if word("unset") || reader == Reader::Biome && word("off") {
        return Some(Value::Unset);
    }

    match key {
        Key::IndentStyle if word("tab") => Some(Value::Tab),
        Key::IndentStyle if word("space") => Some(Value::Space),
        Key::IndentStyle => None,
        Key::IndentSize if reader == Reader::Biome && word("tab") => Some(Value::Tab),
        Key::MaxLineLength if word("off") => Some(Value::Other),
        _ => raw.parse::<u64>().ok().map(Value::Number),
    }
}

fn section_properties(pairs: &IndexMap<String, String>, reader: Reader) -> Properties {
    let mut properties = Properties::default();
    for (key, raw) in pairs {
        match reader {
            Reader::Prettier => {
                if let Some(key) = Key::of(&key.to_lowercase()) {
                    *properties.slot(key) = Some(prettier_value(key, raw));
                }
            }
            Reader::Rust | Reader::Biome => {
                if let Some(key) = Key::of(key) {
                    *properties.slot(key) = rust_value(key, raw, reader);
                }
            }
        }
    }

    properties
}

fn number(value: Option<Value>) -> Option<u64> {
    match value {
        Some(Value::Number(number)) => Some(number),
        _ => None,
    }
}

fn editor_layout(
    formatter: Formatter,
    properties: &Properties,
    own: Option<&Setting>,
) -> Option<EditorLayout> {
    let set = |value: Option<Value>| value.filter(|&value| value != Value::Unset);
    let style = set(properties.indent_style);
    let size = set(properties.indent_size);
    let tab_width = set(properties.tab_width);
    let width = set(properties.max_line_length);
    let positive = |number: u64| usize::try_from(number).ok().filter(|&number| number > 0);
    match formatter {
        Formatter::Prettier => {
            let size = size.or((style == Some(Value::Tab)).then_some(Value::Tab));
            let tab_width = tab_width.or(size.filter(|&size| size != Value::Tab));
            let spaces = style == Some(Value::Space) && number(size).is_some();
            Some(EditorLayout {
                columns: match width {
                    Some(Value::Number(width)) => Some(Columns::Count(width as f64)),
                    Some(Value::Off) => Some(Columns::Unlimited),
                    _ => None,
                },
                tab: number(if spaces { size } else { tab_width }).and_then(positive),
            })
        }
        Formatter::Oxfmt => {
            let pick = |use_tabs: Option<bool>| {
                let spaces = use_tabs == Some(false) && number(size).is_some();
                number(if spaces { size } else { tab_width }).map(|tab| tab as u8)
            };

            let tab = match own.map_or(UseTabs::Absent, |own| own.use_tabs) {
                _ if own.is_some_and(|own| own.tab.is_some()) => None,
                UseTabs::Known(use_tabs) => pick(Some(use_tabs)),
                UseTabs::Absent => pick(style.map(|style| style == Value::Tab)),
                UseTabs::Unknown if pick(Some(true)) == pick(Some(false)) => pick(None),
                UseTabs::Unknown => return None,
            };

            let columns = number(width)
                .filter(|_| own.is_none_or(|own| own.columns.is_none()))
                .map(|width| width as u16);

            let refused = columns.is_some_and(|columns| !(1..=320).contains(&columns))
                || tab.is_some_and(|tab| tab > 24);

            (!refused).then(|| EditorLayout {
                columns: columns.map(|columns| Columns::Count(f64::from(columns))),
                tab: tab.filter(|&tab| tab > 0).map(usize::from),
            })
        }
        Formatter::Biome => Some(EditorLayout {
            columns: None,
            tab: match size {
                Some(Value::Tab) => Some(4),
                size => number(size).and_then(positive),
            },
        }),
    }
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

// Prettier finds its markers with lstat: any `.git` entry counts, even a dangling link, but `.hg` only as a real directory.
fn project_root(dir: &str) -> bool {
    PROJECT_ROOTS.get(dir, || {
        let marker = |name| fs::symlink_metadata(join(dir, name));
        marker(".git").is_ok() || marker(".hg").is_ok_and(|found| found.is_dir())
    })
}

fn editorconfig(
    path: &str,
    unread: &mut Vec<String>,
    anchor: Option<&str>,
    formatter: Formatter,
    own: Option<&Setting>,
) -> EditorValues {
    let reader = Reader::of(formatter);
    let mut properties = Properties::default();
    let mut width_file = None;
    let mut nearest = None;
    let mut dir = anchor.unwrap_or_else(|| dirname(path));
    loop {
        let present = holds(dir, ".editorconfig");
        let mut root = false;
        if present {
            let found = parsed_editor(dir);
            match &found.editor {
                None => unread.push(found.path.clone()),
                Some(editor) => {
                    let view = editor.view(reader);
                    if view.refused {
                        unread.push(found.path.clone());
                        return EditorValues::default();
                    }

                    nearest.get_or_insert_with(|| found.path.clone());
                    root = view.root;
                    let target = SectionPath::new(&relative(dir, path));
                    let mut local = Properties::default();
                    for section in &view.sections {
                        if !section.properties.fills(properties) {
                            continue;
                        }

                        let decided = section.glob.as_ref().and_then(|glob| glob.matches(&target));
                        let Some(matched) = decided else {
                            unread.push(found.path.clone());
                            return EditorValues::default();
                        };

                        if matched {
                            local = section.properties.or(local);
                        }
                    }

                    let star = view.sections.iter().find(|section| section.name == "*");

                    let base = star.map(|section| section.properties);
                    if base.is_some_and(|base| editor_layout(formatter, &base, own).is_none()) {
                        unread.push(found.path.clone());
                        return EditorValues::default();
                    }

                    if properties.max_line_length.is_none() && local.max_line_length.is_some() {
                        width_file = Some(found.path.clone());
                    }

                    properties = properties.or(local);
                }
            }
        }

        if anchor.is_some() || root || project_root(dir) {
            break;
        }

        let parent = dirname(dir);
        if parent == dir {
            break;
        }

        dir = parent;
    }

    let Some(layout) = editor_layout(formatter, &properties, own) else {
        unread.extend(nearest);
        return EditorValues::default();
    };

    EditorValues {
        width: layout.columns.zip(width_file),
        tab: layout.tab,
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
#[path = "width_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "width_growth_tests.rs"]
mod growth_tests;
