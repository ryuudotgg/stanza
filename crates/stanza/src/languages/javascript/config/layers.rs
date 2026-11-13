use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::rc::Rc;

use indexmap::IndexSet;

use super::evaluate::{ByteOffset, Chain, Origin, Value, origin, property};
use super::find::{Loader, resolve_module};
use super::glob::{Globs, glob_reach, pattern_list};
use super::module::{Module, Session};
use super::syntax::{self, each_node};
use super::{biome, eslint, json, oxlint};
use crate::directories::holds;
use crate::paths::{basename, dirname, join, relative};
use crate::text::is_js_whitespace;

const EXPANSION_BUDGET: i64 = 100_000;
const CHAIN_CAP: usize = 32;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Setting {
    On,
    Off,
    Unknown,
}

impl Setting {
    pub const fn name(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    All,
    None,
    Some,
}

pub type ReachAt = Rc<dyn Fn(&str, Option<&str>) -> Reach>;

#[derive(Clone)]
pub enum Coverage {
    Fixed(Reach),
    At(ReachAt),
}

pub struct Layer {
    pub reach: Coverage,
    pub setting: Setting,
}

pub type Layers = Vec<Rc<Layer>>;

type Files = Rc<Vec<String>>;

type LayerKey = (Rc<str>, Family, bool);

type MemoKey = (Rc<str>, bool, Chain, &'static str);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Family {
    Flat,
    Legacy,
    Biome,
    Oxlint,
}

impl Family {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Flat => "flat",
            Self::Legacy => "legacy",
            Self::Biome => "biome",
            Self::Oxlint => "oxlint",
        }
    }

    pub const fn files(self) -> &'static [&'static str] {
        match self {
            Self::Flat => &eslint::FLAT_FILES,
            Self::Legacy => &eslint::LEGACY_FILES,
            Self::Biome => &biome::FILES,
            Self::Oxlint => &oxlint::FILES,
        }
    }

    const fn backstop(self) -> &'static [&'static str] {
        match self {
            Self::Flat | Self::Legacy => &["curly"],
            Self::Biome => &["useBlockStatements"],
            Self::Oxlint => &["curly", "eslint/curly", "style"],
        }
    }

    const fn backstop_imports(self) -> bool {
        matches!(self, Self::Oxlint)
    }

    fn candidate(self, config: &Config, file: &str) -> bool {
        match self {
            Self::Legacy => eslint::legacy_candidate(config, file),
            _ => true,
        }
    }

    pub fn layers(self, value: &Value, context: &Context) -> Layers {
        match self {
            Self::Oxlint => oxlint::layers(value, context),
            _ => tree(value, context),
        }
    }

    fn scope(self, value: &Value, context: &Context) -> Coverage {
        match self {
            Self::Flat => eslint::flat_scope(value, context),
            Self::Legacy => eslint::legacy_scope(value, context),
            Self::Biome => biome::scope(value, context),
            Self::Oxlint => unreachable!("oxlint layers do not walk a tree"),
        }
    }

    fn own(self, value: &Value, context: &Context) -> Layers {
        match self {
            Self::Flat | Self::Legacy => eslint::own_layers(value, context),
            Self::Biome => biome::own_layers(value, context),
            Self::Oxlint => unreachable!("oxlint layers do not walk a tree"),
        }
    }

    fn extend(self, entry: &str, parent: &Value, context: &Context) -> Layers {
        match self {
            Self::Flat => eslint::flat_extend(entry, parent, context),
            Self::Legacy => eslint::legacy_extend(entry, context),
            Self::Biome => biome::extend(entry, context),
            Self::Oxlint => unreachable!("oxlint layers do not walk a tree"),
        }
    }

    pub fn decide(self, config: &Config, dir: &str, extension: Option<&str>) -> Option<Decision> {
        match self {
            Self::Flat => eslint::flat_decide(config, dir, extension),
            Self::Legacy => eslint::legacy_decide(config, dir, extension),
            Self::Biome => biome::decide(config, dir, extension),
            Self::Oxlint => oxlint::decide(config, dir, extension),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub family: Family,
    pub setting: Setting,
    pub files: Vec<String>,
}

pub struct Found {
    pub setting: Option<Setting>,
    pub root: Value,
    pub file: String,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum Identity {
    Pointer(usize),
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    String(Rc<str>),
    Unknown,
    NoSetting,
}

pub(super) fn identity(value: &Value) -> Identity {
    match value {
        Value::Undefined => Identity::Undefined,
        Value::Null => Identity::Null,
        Value::Bool(value) => Identity::Bool(*value),
        Value::Number(value) if value.is_nan() => Identity::Number(f64::NAN.to_bits()),
        Value::Number(value) if *value == 0.0 => Identity::Number(0),
        Value::Number(value) => Identity::Number(value.to_bits()),
        Value::String(value) => Identity::String(value.clone()),
        Value::Unknown => Identity::Unknown,
        Value::NoSetting => Identity::NoSetting,
        Value::Array(items) => Identity::Pointer(Rc::as_ptr(items).addr()),
        Value::Object(object) => Identity::Pointer(Rc::as_ptr(object).addr()),
    }
}

#[derive(Default)]
pub(super) struct Work {
    count: i64,
    exceeded: bool,
    cycles: usize,
    memo: HashMap<Identity, (Value, HashMap<MemoKey, Layers>)>,
    charged: IndexSet<LayerKey>,
    reaching: Vec<HashSet<LayerKey>>,
}

// Sound only because no context outlives the walk that made it, so live frames form one path.
#[derive(Clone, Default)]
pub(super) struct Ancestors {
    top: Option<Rc<Ancestor>>,
    path: Rc<RefCell<HashMap<Identity, usize>>>,
}

struct Ancestor {
    identity: Identity,
    _value: Value,
    _parent: Option<Rc<Ancestor>>,
    path: Rc<RefCell<HashMap<Identity, usize>>>,
}

impl Drop for Ancestor {
    fn drop(&mut self) {
        let mut path = self.path.borrow_mut();
        if let Some(count) = path.get_mut(&self.identity) {
            *count -= 1;
            if *count == 0 {
                path.remove(&self.identity);
            }
        }
    }
}

#[derive(Clone)]
pub struct Context<'a> {
    pub config: &'a Config,
    pub reader: Family,
    pub file: Rc<str>,
    pub shared: bool,
    pub chain: Chain,
    pub(super) consumed: Rc<RefCell<HashSet<Origin>>>,
    pub(super) ancestors: Ancestors,
    pub modules: Rc<RefCell<Vec<Rc<Module>>>>,
    pub(super) work: Rc<RefCell<Work>>,
}

impl Context<'_> {
    pub fn session(&self) -> &Session {
        &self.config.session
    }

    pub fn shared(&self) -> Self {
        Self {
            shared: true,
            ..self.clone()
        }
    }

    pub fn with_ancestor(&self, value: &Value) -> Self {
        let path = self.ancestors.path.clone();
        let identity = identity(value);
        *path.borrow_mut().entry(identity.clone()).or_default() += 1;
        let ancestor = Ancestor {
            identity,
            _value: value.clone(),
            _parent: self.ancestors.top.clone(),
            path: path.clone(),
        };

        Self {
            ancestors: Ancestors {
                top: Some(Rc::new(ancestor)),
                path,
            },
            ..self.clone()
        }
    }

    pub fn exceeded(&self) -> bool {
        self.work.borrow().exceeded
    }

    pub fn has_ancestor(&self, value: &Value) -> bool {
        self.ancestors.path.borrow().contains_key(&identity(value))
    }

    pub fn count_cycle(&self) {
        self.work.borrow_mut().cycles += 1;
    }

    pub fn add_module(&self, module: &Rc<Module>) {
        let mut modules = self.modules.borrow_mut();
        if !modules.iter().any(|known| Rc::ptr_eq(known, module)) {
            modules.push(module.clone());
        }
    }
}

struct Cached {
    layers: Layers,
    root: Value,
    reached: HashSet<LayerKey>,
}

pub struct Config {
    pub session: Session,
    pub unknown_layer: Rc<Layer>,
    layer_cache: RefCell<HashMap<LayerKey, Rc<Cached>>>,
    exclusive_cost: RefCell<HashMap<LayerKey, i64>>,
    manifests: RefCell<HashMap<String, Value>>,
    directory_files: RefCell<HashMap<(Family, String), Files>>,
    globs: RefCell<Globs>,
    pub overflowed: Cell<bool>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            session: Session::default(),
            unknown_layer: Rc::new(Layer {
                reach: Coverage::Fixed(Reach::All),
                setting: Setting::Unknown,
            }),
            layer_cache: RefCell::default(),
            exclusive_cost: RefCell::default(),
            manifests: RefCell::default(),
            directory_files: RefCell::default(),
            globs: RefCell::default(),
            overflowed: Cell::new(false),
        }
    }
}

pub fn layer(setting: Setting) -> Rc<Layer> {
    Rc::new(Layer {
        reach: Coverage::Fixed(Reach::All),
        setting,
    })
}

pub fn last_layers(layers: Layers) -> Layers {
    let mut seen: HashSet<*const Layer> = HashSet::new();
    let mut result: Layers = Vec::new();
    for layer in layers.into_iter().rev() {
        if seen.insert(Rc::as_ptr(&layer)) {
            result.push(layer);
        }
    }

    result.reverse();
    result
}

pub fn expanded(
    value: &Value,
    context: &Context,
    phase: &'static str,
    produce: impl FnOnce() -> Layers,
) -> Layers {
    if context.exceeded() {
        return vec![context.config.unknown_layer.clone()];
    }

    let key: MemoKey = (
        context.file.clone(),
        context.shared,
        context.chain.clone(),
        phase,
    );

    let wanted = identity(value);
    let cached = context
        .work
        .borrow()
        .memo
        .get(&wanted)
        .and_then(|(_, entries)| entries.get(&key).cloned());

    if let Some(cached) = cached {
        return cached;
    }

    let cycles = context.work.borrow().cycles;
    let result = produce();

    let mut work = context.work.borrow_mut();
    work.count += result.len() as i64 + 1;
    if work.count > EXPANSION_BUDGET {
        work.exceeded = true;
    }

    if work.exceeded {
        return vec![context.config.unknown_layer.clone()];
    }

    if work.cycles == cycles {
        work.memo
            .entry(wanted)
            .or_insert_with(|| (value.clone(), HashMap::new()))
            .1
            .insert(key, result.clone());
    }

    result
}

pub fn level_of(session: &Session, value: &Value) -> Setting {
    let mut value = value.clone();
    let mut seen: HashSet<Identity> = HashSet::new();
    while seen.insert(identity(&value)) {
        match &value {
            Value::String(text) if text.as_ref() == "off" => return Setting::Off,
            Value::Number(number) if *number == 0.0 => return Setting::Off,
            Value::String(_) | Value::Number(_) => return Setting::On,
            Value::Array(items) => value = items.first().cloned().unwrap_or_default(),
            Value::Object(_) => value = property(session, &value, "level"),
            _ => return Setting::Unknown,
        }
    }

    Setting::Unknown
}

pub fn reach_at(coverage: &Coverage, dir: &str, extension: Option<&str>) -> Reach {
    match coverage {
        Coverage::Fixed(reach) => *reach,
        Coverage::At(at) => at(dir, extension),
    }
}

const fn intersect(left: Reach, right: Reach) -> Reach {
    match (left, right) {
        (Reach::None, _) | (_, Reach::None) => Reach::None,
        (Reach::All, other) | (other, Reach::All) => other,
        _ => Reach::Some,
    }
}

pub fn intersect_coverage(left: &Coverage, right: &Coverage) -> Coverage {
    match (left, right) {
        (Coverage::Fixed(Reach::All), other) | (other, Coverage::Fixed(Reach::All)) => {
            other.clone()
        }
        (Coverage::Fixed(Reach::None), _) | (_, Coverage::Fixed(Reach::None)) => {
            Coverage::Fixed(Reach::None)
        }
        _ => {
            let (left, right) = (left.clone(), right.clone());
            Coverage::At(Rc::new(move |dir, extension| {
                intersect(
                    reach_at(&left, dir, extension),
                    reach_at(&right, dir, extension),
                )
            }))
        }
    }
}

pub fn scoped(layers: &[Rc<Layer>], reach: &Coverage) -> Layers {
    layers
        .iter()
        .map(|layer| {
            Rc::new(Layer {
                reach: intersect_coverage(&layer.reach, reach),
                setting: layer.setting,
            })
        })
        .collect()
}

fn fold(layers: &[Rc<Layer>], dir: &str, extension: Option<&str>) -> Option<Setting> {
    let mut setting = None;
    for layer in layers {
        let reach = reach_at(&layer.reach, dir, extension);
        if reach == Reach::All || (reach == Reach::Some && layer.setting != Setting::Off) {
            setting = Some(layer.setting);
        }
    }

    setting
}

pub fn read(value: &Value, key: &str, context: &Context) -> Value {
    if let Some(location) = origin(value, key) {
        context.consumed.borrow_mut().insert(location);
    }

    property(context.session(), value, key)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Anchored,
    Legacy,
    Biome1,
}

fn strings(value: &Value) -> Option<Vec<String>> {
    match value {
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

pub fn patterns(value: &Value, context: &Context, mode: Mode) -> Coverage {
    if matches!(value, Value::Undefined) {
        return Coverage::Fixed(Reach::All);
    }

    let Some(patterns) = strings(value) else {
        return Coverage::Fixed(Reach::Some);
    };

    if context.shared {
        return Coverage::Fixed(Reach::Some);
    }

    if patterns.is_empty() {
        return Coverage::Fixed(Reach::None);
    }

    if mode == Mode::Biome1 {
        return Coverage::Fixed(Reach::Some);
    }

    let anchor = dirname(&context.file);
    let list = pattern_list(
        &mut context.config.globs.borrow_mut(),
        &patterns,
        mode == Mode::Legacy,
    );

    Coverage::At(Rc::new(move |dir, extension| {
        glob_reach(&list, &relative(&anchor, dir), extension)
    }))
}

pub fn excluding(reach: Coverage, exclusion: &Value, context: &Context, mode: Mode) -> Coverage {
    if matches!(exclusion, Value::Undefined) {
        return reach;
    }

    let excluded = patterns(exclusion, context, mode);
    Coverage::At(Rc::new(move |dir, extension| {
        let included = reach_at(&reach, dir, extension);
        if included == Reach::All && reach_at(&excluded, dir, extension) != Reach::None {
            Reach::Some
        } else {
            included
        }
    }))
}

fn inherit(entry: &Value, parent: &Value, context: &Context) -> Layers {
    let Value::String(name) = entry else {
        return context.reader.layers(entry, &context.shared());
    };

    if let Some(preset) = context.session().presets.known(name) {
        return context.reader.layers(&preset, &context.shared());
    }

    context.reader.extend(name, parent, context)
}

pub fn resolved(specifier: &str, context: &Context, loader: Loader) -> Layers {
    match resolve_module(specifier, &context.file, loader) {
        Some(file) => file_layers(&file, &context.shared()).0,
        None => vec![context.config.unknown_layer.clone()],
    }
}

pub fn tree(value: &Value, context: &Context) -> Layers {
    if matches!(value, Value::NoSetting) {
        return Vec::new();
    }

    if context.exceeded() {
        return vec![context.config.unknown_layer.clone()];
    }

    if context.has_ancestor(value) {
        context.count_cycle();
        return vec![context.config.unknown_layer.clone()];
    }

    expanded(value, context, "tree", || {
        let next = context.with_ancestor(value);
        if let Value::Array(items) = value {
            return last_layers(items.iter().flat_map(|item| tree(item, &next)).collect());
        }

        if !matches!(value, Value::Object(_)) {
            return vec![context.config.unknown_layer.clone()];
        }

        let session = context.session();
        let reach = context.reader.scope(value, &next);
        let entries = match property(session, value, "extends") {
            Value::Undefined => Vec::new(),
            Value::Array(items) => items.as_ref().clone(),
            single => vec![single],
        };

        let mut result: Layers = entries
            .iter()
            .flat_map(|entry| inherit(entry, value, &next))
            .collect();

        result.extend(context.reader.own(value, &next));

        let mut layers = scoped(&last_layers(result), &reach);
        let overrides = property(session, value, "overrides");
        if !matches!(overrides, Value::Undefined) {
            let items = match overrides {
                Value::Array(items) => items.as_ref().clone(),
                _ => vec![Value::Unknown],
            };

            let inner: Layers = items.iter().flat_map(|item| tree(item, &next)).collect();
            layers.extend(scoped(&last_layers(inner), &reach));
        }

        layers
    })
}

fn split_lines(text: &str) -> Vec<&str> {
    let pieces: Vec<&str> = text.split('\n').collect();
    let last = pieces.len() - 1;
    pieces
        .into_iter()
        .enumerate()
        .map(|(index, piece)| {
            if index < last {
                piece.strip_suffix('\r').unwrap_or(piece)
            } else {
                piece
            }
        })
        .collect()
}

fn is_line_terminator(char: char) -> bool {
    matches!(char, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn without_comment(line: &str) -> &str {
    let last_terminator = line.rfind(is_line_terminator);
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    for (at, &(index, char)) in chars.iter().enumerate() {
        let rest = index + char.len_utf8() + 1;
        if is_js_whitespace(char)
            && chars.get(at + 1).is_some_and(|&(_, next)| next == '#')
            && last_terminator.is_none_or(|terminator| terminator < rest)
        {
            return &line[..index];
        }
    }

    line
}

fn is_word(char: char) -> bool {
    char.is_ascii_alphanumeric() || char == '_'
}

fn mentions_curly(line: &str) -> bool {
    line.match_indices("curly").any(|(index, _)| {
        let before = line[..index].chars().next_back();
        let after = line[index + 5..].chars().next();
        !before.is_some_and(is_word) && !after.is_some_and(is_word)
    })
}

fn quote(char: char) -> bool {
    char == '"' || char == '\''
}

fn curly_key(line: &str) -> bool {
    let rest = line.trim_start_matches(is_js_whitespace);
    let rest = rest.strip_prefix(quote).unwrap_or(rest);
    let Some(rest) = rest.strip_prefix("curly") else {
        return false;
    };

    let rest = rest.strip_prefix(quote).unwrap_or(rest);
    rest.trim_start_matches(is_js_whitespace).starts_with(':')
}

fn extends_key(line: &str) -> bool {
    ["extends", "overrides"].iter().any(|key| {
        line.strip_prefix(key)
            .is_some_and(|rest| rest.trim_start_matches(is_js_whitespace).starts_with(':'))
    })
}

fn yaml_setting(text: &str) -> Option<Setting> {
    let lines: Vec<&str> = split_lines(text)
        .into_iter()
        .map(|line| without_comment(line).trim_end_matches(is_js_whitespace))
        .collect();

    let index = lines.iter().position(|line| curly_key(line));
    if lines
        .iter()
        .enumerate()
        .any(|(at, line)| Some(at) != index && mentions_curly(line))
    {
        return Some(Setting::Unknown);
    }

    let setting = index.map(|index| {
        let line = lines[index];
        let after = &line[line.find(':').map_or(0, |colon| colon + 1)..];
        let mut value = after.trim_start_matches(is_js_whitespace).to_owned();
        if value.is_empty() {
            value = lines[index + 1..]
                .iter()
                .find(|line| !line.trim_matches(is_js_whitespace).is_empty())
                .map_or("", |line| line.trim_matches(is_js_whitespace))
                .to_owned();
        }

        let value: String = value
            .chars()
            .filter(|&char| char != '[' && char != ']')
            .collect();
        let value = value.trim_start_matches(|char| char == '-' || is_js_whitespace(char));
        let first = value
            .split(',')
            .next()
            .unwrap_or_default()
            .trim_matches(is_js_whitespace);

        let first = first.strip_prefix(quote).unwrap_or(first);
        let first = first.strip_suffix(quote).unwrap_or(first);
        if first == "off" || first == "0" {
            Setting::Off
        } else {
            Setting::On
        }
    });

    if setting != Some(Setting::On) && lines.iter().any(|line| extends_key(line)) {
        return Some(Setting::Unknown);
    }

    setting
}

fn yaml_root(text: &str) -> bool {
    let mut starts = vec![0];
    starts.extend(
        text.char_indices()
            .filter(|&(_, char)| is_line_terminator(char))
            .map(|(index, char)| index + char.len_utf8()),
    );

    starts.into_iter().any(|start| {
        let Some(rest) = text[start..].strip_prefix("root:") else {
            return false;
        };

        let Some(rest) = rest
            .trim_start_matches(is_js_whitespace)
            .strip_prefix("true")
        else {
            return false;
        };

        let run: String = rest
            .chars()
            .take_while(|&char| is_js_whitespace(char))
            .collect();
        run.len() == rest.len() || run.chars().any(is_line_terminator)
    })
}

fn backstop(module: &Rc<Module>, context: &Context, result: &mut Layers) {
    let Some(program) = module.program else {
        return;
    };

    let session = context.session();
    each_node(program, |kind| {
        let Some(property) = syntax::property(kind) else {
            return;
        };

        let consumed = context.consumed.borrow().contains(&Origin {
            file: module.file.clone(),
            start: ByteOffset(property.start),
        });

        let Some(key) = property.key.filter(|_| !consumed) else {
            return;
        };

        if !context.reader.backstop().contains(&key) {
            return;
        }

        let value = session.operand_value(property.value, module, &context.chain);

        if level_of(session, &value) != Setting::Off {
            result.push(context.config.unknown_layer.clone());
        }
    });
}

impl Config {
    fn charge(&self, keys: &HashSet<LayerKey>, work: &mut Work) {
        let costs = self.exclusive_cost.borrow();
        for key in keys {
            for reached in &mut work.reaching {
                reached.insert(key.clone());
            }

            if !work.charged.insert(key.clone()) {
                continue;
            }

            work.count += costs.get(key).copied().unwrap_or_default();
        }

        if work.count > EXPANSION_BUDGET {
            work.exceeded = true;
        }
    }

    pub fn eslint_config(&self, manifest: &str) -> Value {
        if let Some(cached) = self.manifests.borrow().get(manifest) {
            return cached.clone();
        }

        let config = fs::read(manifest)
            .ok()
            .and_then(|bytes| {
                let text = String::from_utf8_lossy(&bytes);
                let parsed = json::parse(text.strip_prefix('\u{feff}').unwrap_or(&text))?;
                Some(property(&self.session, &parsed, "eslintConfig"))
            })
            .unwrap_or(Value::Unknown);

        self.manifests
            .borrow_mut()
            .insert(manifest.to_owned(), config.clone());

        config
    }

    pub fn config_directories(
        &self,
        dir: &str,
        reader: Family,
    ) -> impl Iterator<Item = Rc<Vec<String>>> {
        std::iter::successors(Some(dir.to_owned()), |dir| {
            let parent = dirname(dir);
            (parent != *dir).then_some(parent)
        })
        .map(move |dir| self.directory_files(&dir, reader))
    }

    fn directory_files(&self, dir: &str, reader: Family) -> Rc<Vec<String>> {
        let key = (reader, dir.to_owned());
        if let Some(cached) = self.directory_files.borrow().get(&key) {
            return cached.clone();
        }

        let files: Rc<Vec<String>> = Rc::new(
            reader
                .files()
                .iter()
                .filter(|name| holds(dir, name))
                .map(|name| join(dir, name))
                .filter(|file| reader.candidate(self, file))
                .collect(),
        );

        self.directory_files.borrow_mut().insert(key, files.clone());
        files
    }

    pub fn config_at(
        &self,
        file: &str,
        dir: &str,
        extension: Option<&str>,
        reader: Family,
    ) -> Found {
        let context = Context {
            config: self,
            reader,
            file: file.into(),
            shared: false,
            chain: Rc::default(),
            consumed: Rc::default(),
            ancestors: Ancestors::default(),
            modules: Rc::default(),
            work: Rc::default(),
        };

        let (layers, root) = file_layers(file, &context);
        let setting = if context.exceeded() {
            Some(Setting::Unknown)
        } else {
            fold(&layers, dir, extension)
        };

        Found {
            setting,
            root,
            file: file.to_owned(),
        }
    }

    pub fn every_config(&self, dir: &str, extension: Option<&str>, reader: Family) -> Vec<Found> {
        self.config_directories(dir, reader)
            .flat_map(|files| {
                files
                    .iter()
                    .map(|file| self.config_at(file, dir, extension, reader))
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

pub fn file_layers(file: &str, context: &Context) -> (Layers, Value) {
    let config = context.config;
    if config.overflowed.get() {
        return (vec![config.unknown_layer.clone()], Value::Undefined);
    }

    let module = config.session.module_at(file);
    let Some(module) = module.filter(|module| {
        !context.chain.iter().any(|seen| **seen == *module.file) && context.chain.len() < CHAIN_CAP
    }) else {
        return (vec![config.unknown_layer.clone()], Value::Undefined);
    };

    let key: LayerKey = (file.into(), context.reader, context.shared);

    let cached = config.layer_cache.borrow().get(&key).cloned();
    if let Some(cached) = cached {
        config.charge(&cached.reached, &mut context.work.borrow_mut());
        return (cached.layers.clone(), cached.root.clone());
    }

    let (cycles, count, charged) = {
        let mut work = context.work.borrow_mut();
        for reached in &mut work.reaching {
            reached.insert(key.clone());
        }

        work.reaching.push(HashSet::from([key.clone()]));
        (work.cycles, work.count, work.charged.len())
    };

    let result = read_file_layers(file, &module, context);
    let mut work = context.work.borrow_mut();
    let reached = work.reaching.pop().unwrap_or_default();

    if !work.exceeded && work.cycles == cycles && !config.overflowed.get() {
        let costs = config.exclusive_cost.borrow();
        let mut cost = work.count - count;
        for entry in work.charged.iter().skip(charged) {
            if *entry != key {
                cost -= costs.get(entry).copied().unwrap_or_default();
            }
        }

        drop(costs);
        config.layer_cache.borrow_mut().insert(
            key.clone(),
            Rc::new(Cached {
                layers: result.0.clone(),
                root: result.1.clone(),
                reached,
            }),
        );

        config.exclusive_cost.borrow_mut().insert(key.clone(), cost);
        work.charged.insert(key);
    }

    result
}

fn read_file_layers(file: &str, module: &Rc<Module>, context: &Context) -> (Layers, Value) {
    let mut chain = context.chain.as_ref().clone();
    chain.push(module.file.clone());
    let next = Context {
        file: file.into(),
        chain: Rc::new(chain),
        ..context.clone()
    };

    next.add_module(module);

    if module.yaml {
        let layers = yaml_setting(&module.text).map(layer).into_iter().collect();
        return (layers, Value::Bool(yaml_root(&module.text)));
    }

    let session = context.session();
    let manifest = context.reader == Family::Legacy && basename(file) == "package.json";
    let value = if manifest {
        context.config.eslint_config(file)
    } else {
        session.exported(&module.file, "default", &context.chain)
    };

    let mut result = context.reader.layers(&value, &next);
    if manifest {
        return (result, property(session, &value, "root"));
    }

    if context.reader.backstop_imports() {
        let modules = next.modules.borrow().clone();
        for candidate in &modules {
            let inner = Context {
                file: candidate.file.clone(),
                chain: Rc::default(),
                ..next.clone()
            };

            backstop(candidate, &inner, &mut result);
        }
    } else {
        backstop(module, &next, &mut result);
    }

    (result, property(session, &value, "root"))
}

pub fn decided(reader: Family, found: Option<&Found>) -> Option<Decision> {
    let found = found?;
    Some(Decision {
        family: reader,
        setting: found.setting?,
        files: vec![found.file.clone()],
    })
}

pub fn enforcing(found: &[Found]) -> Option<&Found> {
    let nearest = found.iter().find(|item| item.setting.is_some());
    found
        .iter()
        .find(|item| matches!(item.setting, Some(Setting::On | Setting::Unknown)))
        .or(nearest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_reads_curly_like_the_reference() {
        assert_eq!(yaml_setting("rules:\n  curly: off\n"), Some(Setting::Off));
        assert_eq!(
            yaml_setting("rules:\n  curly: 'error'\n"),
            Some(Setting::On)
        );
        assert_eq!(
            yaml_setting("rules:\n  curly:\n    - 0\n"),
            Some(Setting::Off)
        );
        assert_eq!(
            yaml_setting("rules:\n  \"curly\": [\"off\", \"all\"]\n"),
            Some(Setting::Off)
        );
        assert_eq!(
            yaml_setting("rules:\n  curly: off # curly stays\n"),
            Some(Setting::Off)
        );
        assert_eq!(
            yaml_setting("rules:\n  curly: off\n  x: curly\n"),
            Some(Setting::Unknown)
        );
        assert_eq!(
            yaml_setting("extends: base\nrules:\n  curly: off\n"),
            Some(Setting::Unknown)
        );
        assert_eq!(
            yaml_setting("extends: base\nrules:\n  curly: error\n"),
            Some(Setting::On)
        );
        assert_eq!(yaml_setting("rules:\n  semi: error\n"), None);
        assert_eq!(yaml_setting("curlyBraces: 1\n"), None);
    }

    #[test]
    fn a_comment_marker_counts_only_after_the_last_line_terminator() {
        assert_eq!(without_comment("a # b\rc"), "a # b\rc");
        assert_eq!(without_comment("a\r # b"), "a\r");
        assert_eq!(without_comment("a # b # c\u{2028}"), "a # b # c\u{2028}");
        assert_eq!(without_comment("a\u{2028}b # c # d"), "a\u{2028}b");
        assert_eq!(without_comment("a #\r"), "a #\r");
        assert_eq!(without_comment("a\u{a0}#"), "a");
    }

    #[test]
    fn yaml_root_spans_lines_like_a_multiline_regex() {
        assert!(yaml_root("root: true\n"));
        assert!(yaml_root("a: 1\r\nroot:\n  true  \n"));
        assert!(yaml_root("root: true"));
        assert!(!yaml_root("root: true # no\n"));
        assert!(!yaml_root(" root: true\n"));
    }
}
