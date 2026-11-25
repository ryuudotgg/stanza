use std::fs;
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use serde_json::Value as Json;

use super::super::editor_glob::{Rules, SectionGlob, SectionPath};
use super::super::object::array_index;
use super::{Columns, Formatter, Setting, UseTabs, candidate, read_text};
use crate::directories::holds;
use crate::memo::Memo;
use crate::paths::{dirname, join, relative};
use crate::text::js_trim;

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

struct EditorLayout {
    columns: Option<Columns>,
    tab: Option<usize>,
}

#[derive(Default)]
pub(super) struct EditorValues {
    pub(super) width: Option<(Columns, String)>,
    pub(super) tab: Option<usize>,
}

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

static EDITORS: LazyLock<Memo<String, Arc<EditorFile>>> = LazyLock::new(Memo::default);

static PROJECT_ROOTS: LazyLock<Memo<String, bool>> = LazyLock::new(Memo::default);

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

// Prettier finds its markers with lstat: any `.git` entry counts, even a dangling link, but `.hg` only as a real directory.
fn project_root(dir: &str) -> bool {
    PROJECT_ROOTS.get(dir, || {
        let marker = |name| fs::symlink_metadata(join(dir, name));
        marker(".git").is_ok() || marker(".hg").is_ok_and(|found| found.is_dir())
    })
}

pub(super) fn editorconfig(
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
