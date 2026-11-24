use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use indexmap::IndexSet;
use serde_json::Value as Json;

use super::evaluate::{CHAIN_CAP, Chain, Value, property};
use super::find::{Loader, manifest, resolve_module};
use super::glob::{Globs, glob_reach, pattern_list};
use super::module::{Module, Session};
use super::object::Object;
use super::{biome, eslint, oxlint};
use crate::directories::holds;
use crate::paths::{basename, dirname, join, relative};

const EXPANSION_BUDGET: i64 = 100_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Setting {
    On,
    Off,
    Unknown,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reach {
    All,
    None,
    Some,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Name {
    File(String),
    Extension(String),
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Target {
    pub dir: String,
    pub name: Name,
}

impl Target {
    pub fn file(path: &str) -> Self {
        Self {
            dir: dirname(path).to_owned(),
            name: Name::File(basename(path).to_owned()),
        }
    }

    pub fn files_in(dir: &str, extension: Option<&str>) -> Self {
        Self {
            dir: dir.to_owned(),
            name: extension.map_or(Name::Any, |extension| Name::Extension(extension.to_owned())),
        }
    }
}

pub type ReachAt = Rc<dyn Fn(&Target) -> Reach>;

#[derive(Clone)]
pub enum Coverage {
    Fixed(Reach),
    At(ReachAt),
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Rank {
    RootPreset,
    GroupPreset,
    Explicit,
    Unread,
}

pub struct Layer {
    pub reach: Coverage,
    pub setting: Setting,
    pub rank: Rank,
    pub overriding: bool,
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
    pub const fn files(self) -> &'static [&'static str] {
        match self {
            Self::Flat => &eslint::FLAT_FILES,
            Self::Legacy => &eslint::LEGACY_FILES,
            Self::Biome => &biome::FILES,
            Self::Oxlint => &oxlint::FILES,
        }
    }

    const fn chain_cap(self) -> usize {
        match self {
            Self::Biome => biome::CHAIN_CAP,
            _ => CHAIN_CAP,
        }
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

    pub fn decide(self, config: &Config, target: &Target) -> Option<Decision> {
        match self {
            Self::Flat => eslint::flat_decide(config, target),
            Self::Legacy => eslint::legacy_decide(config, target),
            Self::Biome => biome::decide(config, target),
            Self::Oxlint => oxlint::decide(config, target),
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
        Value::Array(items) => Identity::Pointer(Rc::as_ptr(items).addr()),
        Value::Object(object) => Identity::Pointer(Rc::as_ptr(object).addr()),
    }
}

#[derive(Clone, Default)]
struct Reached {
    keys: HashSet<LayerKey>,
    files: HashSet<Rc<str>>,
    depth: usize,
    entry: Option<Rc<str>>,
}

impl Reached {
    fn fits(&self, chain: &[Rc<str>], entry: &Rc<str>, cap: usize) -> bool {
        chain.len() + self.depth <= cap
            && !chain.iter().any(|file| self.files.contains(file))
            && self.entry.as_ref().is_none_or(|bound| bound == entry)
    }
}

#[derive(Default)]
pub(super) struct Work {
    count: i64,
    exceeded: bool,
    cycles: usize,
    cuts: usize,
    memo: HashMap<Identity, (Value, HashMap<MemoKey, Layers>)>,
    charged: IndexSet<LayerKey>,
    reaching: Vec<Reached>,
    active: HashSet<Identity>,
}

impl Work {
    fn reach(&mut self, reached: &Reached, start: usize) {
        for frame in &mut self.reaching {
            frame.keys.extend(reached.keys.iter().cloned());
            frame.files.extend(reached.files.iter().cloned());
            if reached.entry.is_some() {
                frame.entry.clone_from(&reached.entry);
            }
            frame.depth = frame.depth.max(start + reached.depth);
        }
    }
}

pub struct Active {
    work: Rc<RefCell<Work>>,
    identity: Identity,
    // Holding the value keeps its address from being reused as another identity while it is marked.
    _held: Value,
}

impl Drop for Active {
    fn drop(&mut self) {
        self.work.borrow_mut().active.remove(&self.identity);
    }
}

#[derive(Clone)]
pub struct Context<'a> {
    pub config: &'a Config,
    pub reader: Family,
    pub file: Rc<str>,
    pub entry: Rc<str>,
    pub shared: bool,
    pub chain: Chain,
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

    pub fn activate(&self, value: &Value) -> Option<Active> {
        let identity = identity(value);
        let mut work = self.work.borrow_mut();
        if !work.active.insert(identity.clone()) {
            work.cycles += 1;
            return None;
        }

        Some(Active {
            work: self.work.clone(),
            identity,
            _held: value.clone(),
        })
    }

    pub fn exceeded(&self) -> bool {
        self.work.borrow().exceeded
    }

    pub fn bind_entry(&self) {
        let bound = Reached {
            entry: Some(self.entry.clone()),
            ..Reached::default()
        };

        self.work.borrow_mut().reach(&bound, self.chain.len());
    }

    pub fn enter_preset(&self, file: &str, name: &str) -> Option<Self> {
        let marker: Rc<str> = format!("plugin:{file}/{name}").into();
        let start = self.chain.len();
        let mut work = self.work.borrow_mut();
        if start >= self.reader.chain_cap() || self.chain.contains(&marker) {
            work.cuts += 1;
            return None;
        }

        let entered = Reached {
            files: HashSet::from([marker.clone()]),
            depth: 1,
            ..Reached::default()
        };

        work.reach(&entered, start);
        drop(work);

        let mut chain = self.chain.as_ref().clone();
        chain.push(marker);
        Some(Self {
            file: file.into(),
            shared: true,
            chain: Rc::new(chain),
            ..self.clone()
        })
    }

    pub fn unread(&self) -> Rc<Layer> {
        let config = self.config;
        if self.reader == Family::Biome {
            config.unread_layer.clone()
        } else {
            config.unknown_layer.clone()
        }
    }
}

struct Cached {
    layers: Layers,
    root: Value,
    reached: Reached,
}

struct Entered {
    layers: Layers,
    root: Value,
    exceeded: bool,
}

pub struct Config {
    pub session: Session,
    pub unknown_layer: Rc<Layer>,
    unread_layer: Rc<Layer>,
    layer_cache: RefCell<HashMap<LayerKey, Rc<Cached>>>,
    exclusive_cost: RefCell<HashMap<LayerKey, i64>>,
    manifests: RefCell<HashMap<String, Value>>,
    directory_files: RefCell<HashMap<(Family, String), Files>>,
    globs: RefCell<Globs>,
    entered: RefCell<HashMap<(String, Family), Rc<Entered>>>,
    pub(super) varies: Rc<Cell<bool>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            session: Session::default(),
            unknown_layer: layer(Setting::Unknown),
            unread_layer: ranked(Setting::Unknown, Rank::Unread),
            layer_cache: RefCell::default(),
            exclusive_cost: RefCell::default(),
            manifests: RefCell::default(),
            directory_files: RefCell::default(),
            globs: RefCell::default(),
            entered: RefCell::default(),
            varies: Rc::default(),
        }
    }
}

pub fn layer(setting: Setting) -> Rc<Layer> {
    ranked(setting, Rank::Explicit)
}

pub fn ranked(setting: Setting, rank: Rank) -> Rc<Layer> {
    Rc::new(Layer {
        reach: Coverage::Fixed(Reach::All),
        setting,
        rank,
        overriding: false,
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

    let (cycles, cuts) = {
        let work = context.work.borrow();
        (work.cycles, work.cuts)
    };

    let result = produce();

    let mut work = context.work.borrow_mut();
    work.count += result.len() as i64 + 1;
    if work.count > EXPANSION_BUDGET {
        work.exceeded = true;
    }

    if work.exceeded {
        return vec![context.config.unknown_layer.clone()];
    }

    if work.cycles == cycles && work.cuts == cuts {
        work.memo
            .entry(wanted)
            .or_insert_with(|| (value.clone(), HashMap::new()))
            .1
            .insert(key, result.clone());
    }

    result
}

pub fn severity(session: &Session, value: &Value, family: Family) -> Setting {
    let mut value = value.clone();
    let mut seen: HashSet<Identity> = HashSet::new();
    loop {
        let next = match (&value, family) {
            (Value::String(text), _) => {
                return match (text.as_ref(), family) {
                    ("off", _) | ("allow", Family::Oxlint) => Setting::Off,
                    ("warn" | "error" | "deny", _)
                    | (_, Family::Flat | Family::Legacy | Family::Biome) => Setting::On,
                    _ => Setting::Unknown,
                };
            }
            (Value::Number(number), _) => {
                return match (*number, family) {
                    (0.0, _) => Setting::Off,
                    (1.0 | 2.0, _) | (_, Family::Flat | Family::Legacy | Family::Biome) => {
                        Setting::On
                    }
                    _ => Setting::Unknown,
                };
            }
            (Value::Array(items), _) => items.first().cloned().unwrap_or_default(),
            (Value::Object(_), Family::Flat | Family::Legacy | Family::Biome) => {
                property(session, &value, "level")
            }
            _ => return Setting::Unknown,
        };

        if !seen.insert(identity(&value)) {
            return Setting::Unknown;
        }

        value = next;
    }
}

pub fn reach_at(coverage: &Coverage, target: &Target) -> Reach {
    match coverage {
        Coverage::Fixed(reach) => *reach,
        Coverage::At(at) => at(target),
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
            Coverage::At(Rc::new(move |target| {
                intersect(reach_at(&left, target), reach_at(&right, target))
            }))
        }
    }
}

pub fn scoped(layers: &[Rc<Layer>], reach: &Coverage) -> Layers {
    layers
        .iter()
        .map(|layer| {
            let reach = if layer.rank == Rank::Unread {
                layer.reach.clone()
            } else {
                intersect_coverage(&layer.reach, reach)
            };

            Rc::new(Layer {
                reach,
                setting: layer.setting,
                rank: layer.rank,
                overriding: layer.overriding,
            })
        })
        .collect()
}

fn overriding(layer: &Layer) -> Rc<Layer> {
    Rc::new(Layer {
        reach: layer.reach.clone(),
        setting: layer.setting,
        rank: layer.rank,
        overriding: true,
    })
}

fn fold(layers: &[Rc<Layer>], target: &Target) -> Option<Setting> {
    let mut chosen: Option<&Layer> = None;
    let top = layers.iter().filter(|layer| !layer.overriding);
    let late = layers.iter().filter(|layer| layer.overriding);
    for layer in top.chain(late) {
        let reach = reach_at(&layer.reach, target);
        let applies =
            reach == Reach::All || (reach == Reach::Some && layer.setting != Setting::Off);

        if applies && chosen.is_none_or(|chosen| layer.rank >= chosen.rank) {
            chosen = Some(layer);
        }
    }

    chosen.map(|layer| layer.setting)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Mode {
    Anchored,
    Legacy,
    Oxlint,
    Biome,
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

    let anchor = dirname(&context.file).to_owned();
    let list = pattern_list(&mut context.config.globs.borrow_mut(), &patterns, mode);
    let varies = context.config.varies.clone();

    Coverage::At(Rc::new(move |target| {
        let reach = glob_reach(&list, &relative(&anchor, &target.dir), &target.name);
        if reach == Reach::Some && list.names_decide() && !matches!(target.name, Name::File(_)) {
            varies.set(true);
        }

        reach
    }))
}

pub fn excluding(reach: Coverage, exclusion: &Value, context: &Context, mode: Mode) -> Coverage {
    if matches!(exclusion, Value::Undefined) {
        return reach;
    }

    let excluded = patterns(exclusion, context, mode);
    Coverage::At(Rc::new(move |target| {
        match (reach_at(&reach, target), reach_at(&excluded, target)) {
            (Reach::None, _) | (_, Reach::All) => Reach::None,
            (included, Reach::None) => included,
            (_, Reach::Some) => Reach::Some,
        }
    }))
}

fn inherit(entry: &Value, parent: &Value, context: &Context) -> Layers {
    let Value::String(name) = entry else {
        return context.reader.layers(entry, &context.shared());
    };

    if context.reader != Family::Biome
        && let Some(preset) = context.session().presets.known(name)
    {
        return context.reader.layers(&preset, &context.shared());
    }

    context.reader.extend(name, parent, context)
}

pub fn resolved(specifier: &str, context: &Context, loader: Loader) -> Layers {
    match resolve_module(specifier, &context.file, loader) {
        Some(file) => file_layers(&file, &context.shared()).0,
        None => vec![context.unread()],
    }
}

pub fn tree(value: &Value, context: &Context) -> Layers {
    if context.exceeded() {
        return vec![context.config.unknown_layer.clone()];
    }

    let Some(_active) = context.activate(value) else {
        return vec![context.config.unknown_layer.clone()];
    };

    expanded(value, context, "tree", || {
        if let Value::Array(items) = value {
            return last_layers(items.iter().flat_map(|item| tree(item, context)).collect());
        }

        if !matches!(value, Value::Object(_)) {
            return vec![context.unread()];
        }

        let session = context.session();
        let reach = context.reader.scope(value, context);
        let entries = match property(session, value, "extends") {
            Value::Undefined => Vec::new(),
            Value::Array(items) => items.as_ref().clone(),
            single => vec![single],
        };

        let biome = context.reader == Family::Biome;
        let mut result: Layers = entries
            .iter()
            .flat_map(|entry| inherit(entry, value, context))
            .map(|layer| {
                if biome && layer.setting == Setting::Unknown {
                    overriding(&layer)
                } else {
                    layer
                }
            })
            .collect();

        result.extend(context.reader.own(value, context));

        let mut layers = scoped(&last_layers(result), &reach);
        let overrides = property(session, value, "overrides");
        if !matches!(overrides, Value::Undefined) {
            let items = match overrides {
                Value::Array(items) => items.as_ref().clone(),
                _ => vec![Value::Unknown],
            };

            let inner: Layers = items.iter().flat_map(|item| tree(item, context)).collect();
            let inner = scoped(&last_layers(inner), &reach);
            if biome {
                layers.extend(inner.iter().map(|layer| overriding(layer)));
            } else {
                layers.extend(inner);
            }
        }

        layers
    })
}

impl Config {
    fn charge(&self, reached: &Reached, start: usize, work: &mut Work) {
        work.reach(reached, start);

        let costs = self.exclusive_cost.borrow();
        for key in &reached.keys {
            if !work.charged.insert(key.clone()) {
                continue;
            }

            work.count += costs.get(key).copied().unwrap_or_default();
        }

        if work.count > EXPANSION_BUDGET {
            work.exceeded = true;
        }
    }

    pub fn eslint_config(&self, path: &str) -> Value {
        if let Some(cached) = self.manifests.borrow().get(path) {
            return cached.clone();
        }

        let config = match manifest(path) {
            Some(Json::Object(fields)) => fields
                .get("eslintConfig")
                .map_or(Value::Undefined, json_value),
            _ => Value::Unknown,
        };

        self.manifests
            .borrow_mut()
            .insert(path.to_owned(), config.clone());

        config
    }

    pub fn config_directories<'a>(
        &'a self,
        dir: &'a str,
        reader: Family,
    ) -> impl Iterator<Item = Rc<Vec<String>>> + 'a {
        std::iter::successors(Some(dir), |&dir| {
            let parent = dirname(dir);
            (parent != dir).then_some(parent)
        })
        .map(move |dir| self.directory_files(dir, reader))
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

    pub fn config_at(&self, file: &str, target: &Target, reader: Family) -> Found {
        let entered = self.entered(file, reader);
        let setting = if entered.exceeded {
            Some(Setting::Unknown)
        } else {
            fold(&entered.layers, target)
        };

        Found {
            setting,
            root: entered.root.clone(),
            file: file.to_owned(),
        }
    }

    fn entered(&self, file: &str, reader: Family) -> Rc<Entered> {
        let key = (file.to_owned(), reader);
        if let Some(known) = self.entered.borrow().get(&key) {
            return known.clone();
        }

        self.session.depth.begin_root(file);
        let context = Context {
            config: self,
            reader,
            file: file.into(),
            entry: file.into(),
            shared: false,
            chain: Rc::default(),
            work: Rc::default(),
        };

        let (layers, root) = file_layers(file, &context);
        let entered = Rc::new(Entered {
            layers,
            root,
            exceeded: context.exceeded(),
        });

        self.entered.borrow_mut().insert(key, entered.clone());
        entered
    }

    pub fn every_config(&self, target: &Target, reader: Family) -> Vec<Found> {
        self.config_directories(&target.dir, reader)
            .flat_map(|files| {
                files
                    .iter()
                    .map(|file| self.config_at(file, target, reader))
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

pub fn file_layers(file: &str, context: &Context) -> (Layers, Value) {
    let config = context.config;
    let Some(module) = config.session.module_at(file) else {
        return (vec![context.unread()], Value::Undefined);
    };

    let start = context.chain.len();
    let cap = context.reader.chain_cap();
    if start >= cap || context.chain.contains(&module.file) {
        context.work.borrow_mut().cuts += 1;
        return (vec![context.unread()], Value::Undefined);
    }

    let key: LayerKey = (file.into(), context.reader, context.shared);

    let cached = config.layer_cache.borrow().get(&key).cloned();
    if let Some(cached) =
        cached.filter(|cached| cached.reached.fits(&context.chain, &context.entry, cap))
    {
        config.charge(&cached.reached, start, &mut context.work.borrow_mut());
        return (cached.layers.clone(), cached.root.clone());
    }

    let (cycles, cuts, count, charged) = {
        let mut work = context.work.borrow_mut();
        let entered = Reached {
            keys: HashSet::from([key.clone()]),
            files: HashSet::from([module.file.clone()]),
            depth: 1,
            ..Reached::default()
        };

        work.reach(&entered, start);
        work.reaching.push(Reached {
            depth: start + 1,
            ..entered
        });

        (work.cycles, work.cuts, work.count, work.charged.len())
    };

    let depth = &config.session.depth;
    let ((result, import_reach), touched) =
        depth.traced(|| depth.files_reached(|| read_file_layers(file, &module, context)));

    let hit = touched.hit();
    let touched = touched.files();
    let mut work = context.work.borrow_mut();
    if hit || touched.iter().any(|file| context.chain.contains(file)) {
        work.cuts += 1;
    } else {
        let imported = Reached {
            files: touched.into_iter().collect(),
            ..Reached::default()
        };

        work.reach(&imported, start);
    }

    if import_reach > CHAIN_CAP {
        work.cuts += 1;
    } else if import_reach > start {
        let imported = Reached {
            depth: import_reach - start,
            ..Reached::default()
        };

        work.reach(&imported, start);
    }

    let mut reached = work.reaching.pop().unwrap_or_default();
    reached.depth = reached.depth.saturating_sub(start);

    if !work.exceeded && work.cycles == cycles && work.cuts == cuts {
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

    let session = context.session();
    if context.reader == Family::Biome {
        let value = biome::jsonc(&module.text);
        return (
            context.reader.layers(&value, &next),
            property(session, &value, "root"),
        );
    }

    if let Some(value) = &module.data {
        if context.reader != Family::Legacy {
            return (vec![context.unread()], Value::Undefined);
        }

        return (
            context.reader.layers(value, &next),
            property(session, value, "root"),
        );
    }

    let manifest = context.reader == Family::Legacy && basename(file) == "package.json";
    let value = if manifest {
        context.config.eslint_config(file)
    } else {
        session.exported(&module.file, "default", &context.chain)
    };

    (
        context.reader.layers(&value, &next),
        property(session, &value, "root"),
    )
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

pub(super) fn json_value(json: &Json) -> Value {
    match json {
        Json::Null => Value::Null,
        Json::Bool(value) => Value::Bool(*value),
        Json::Number(number) => number.as_f64().map_or(Value::Unknown, Value::Number),
        Json::String(text) => Value::String(text.as_str().into()),
        Json::Array(items) => Value::Array(Rc::new(items.iter().map(json_value).collect())),
        Json::Object(fields) => Value::Object(Object::plain(
            fields
                .iter()
                .map(|(key, value)| (key.as_str(), json_value(value)))
                .collect(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use super::*;

    #[test]
    fn active_values_clear_when_their_frame_ends() {
        let config = Config::default();
        let context = Context {
            config: &config,
            reader: Family::Flat,
            file: "/a/eslint.config.js".into(),
            entry: "/a/eslint.config.js".into(),
            shared: false,
            chain: Rc::default(),
            work: Rc::default(),
        };

        let object = Value::Object(Object::plain(Vec::new()));
        let active = |context: &Context| context.work.borrow().active.contains(&identity(&object));
        let cycles = |context: &Context| context.work.borrow().cycles;

        let entered = context.activate(&object);
        assert!(entered.is_some());
        assert!(active(&context));

        assert!(context.activate(&object).is_none());
        assert_eq!(cycles(&context), 1);
        assert!(active(&context));

        drop(entered);
        assert!(!active(&context));

        let sibling = context.activate(&object);
        assert!(sibling.is_some());
        assert_eq!(cycles(&context), 1);
        drop(sibling);

        let unwound = catch_unwind(AssertUnwindSafe(|| {
            let _entered = context.activate(&object);
            assert!(active(&context));
            panic!("a failed walk");
        }));

        assert!(unwound.is_err());
        assert!(!active(&context));
    }

    #[test]
    fn severity_policies_keep_family_answers() {
        let session = Session::default();
        let looped = Value::Object(Object::holding_itself("level"));
        let Value::Object(object) = &looped else {
            unreachable!();
        };

        assert!(matches!(
            property(&session, &looped, "level"),
            Value::Object(level) if Rc::ptr_eq(&level, object)
        ));

        let word = |text: &str| Value::String(text.into());
        let list = |items: Vec<Value>| Value::Array(Rc::new(items));
        let level = |text: &str| Value::Object(Object::plain(vec![("level", word(text))]));
        let cases = [
            ("\"off\"", word("off"), Setting::Off, Setting::Off),
            ("0", Value::Number(0.0), Setting::Off, Setting::Off),
            (
                "[\"off\"]",
                list(vec![word("off")]),
                Setting::Off,
                Setting::Off,
            ),
            (
                "{ level: \"off\" }",
                level("off"),
                Setting::Off,
                Setting::Unknown,
            ),
            ("\"allow\"", word("allow"), Setting::On, Setting::Off),
            ("3", Value::Number(3.0), Setting::On, Setting::Unknown),
            ("\"deny\"", word("deny"), Setting::On, Setting::On),
            ("\"loud\"", word("loud"), Setting::On, Setting::Unknown),
            ("1", Value::Number(1.0), Setting::On, Setting::On),
            ("2", Value::Number(2.0), Setting::On, Setting::On),
            (
                "[\"warn\"]",
                list(vec![word("warn")]),
                Setting::On,
                Setting::On,
            ),
            ("[]", list(Vec::new()), Setting::Unknown, Setting::Unknown),
            (
                "x = { level: x }",
                looped.clone(),
                Setting::Unknown,
                Setting::Unknown,
            ),
        ];

        for (name, value, eslint, oxlint) in cases {
            for family in [Family::Flat, Family::Legacy, Family::Biome] {
                assert_eq!(
                    severity(&session, &value, family),
                    eslint,
                    "{family:?} {name}"
                );
            }

            assert_eq!(
                severity(&session, &value, Family::Oxlint),
                oxlint,
                "oxlint {name}"
            );
        }
    }
}
