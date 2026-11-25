use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::rc::Rc;

use indexmap::IndexSet;
use serde_json::Value as Json;

use super::coverage::{
    Coverage, Layer, Layers, Name, Rank, Reach, Setting, Target, excluding, fold, last_layers,
    layer, overriding, ranked, scoped,
};
use super::evaluate::{CHAIN_CAP, Chain, Value, property};
use super::family::Family;
use super::find::{Loader, Manifest, manifest, resolve_module};
use super::glob::{Globs, Mode, glob_reach, pattern_list};
use super::module::{Module, Session, data_file};
use super::object::Object;
use super::{biome, eslint, oxlint};
use crate::directories::holds;
use crate::paths::{basename, dirname, join, relative};

const EXPANSION_BUDGET: i64 = 100_000;

type Files = Rc<Vec<String>>;

type LayerKey = (Rc<str>, Family, bool);

type MemoKey = (Rc<str>, bool, Chain, Walk);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Walk {
    Tree,
    Oxlint(oxlint::Phase),
}

pub struct ScopeKeys {
    pub anchor: Option<&'static str>,
    pub include: &'static [(&'static str, Mode)],
    pub exclude: (&'static str, Mode),
}

impl ScopeKeys {
    pub fn coverage(&self, value: &Value, context: &Context) -> Coverage {
        let read = |key| property(context.session(), value, key);
        if self
            .anchor
            .is_some_and(|key| !matches!(read(key), Value::Undefined))
        {
            return Coverage::Fixed(Reach::Some);
        }

        let reach = self
            .include
            .iter()
            .find_map(|&(key, mode)| match read(key) {
                Value::Undefined => None,
                list => Some(patterns(&list, context, mode)),
            })
            .unwrap_or(Coverage::Fixed(Reach::All));

        let (key, mode) = self.exclude;
        match read(key) {
            Value::Undefined => reach,
            list => excluding(reach, patterns(&list, context, mode)),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tree {
    Flat,
    Legacy,
    Biome,
}

impl Tree {
    const fn scope(self) -> &'static ScopeKeys {
        match self {
            Self::Flat => &eslint::FLAT_SCOPE,
            Self::Legacy => &eslint::LEGACY_SCOPE,
            Self::Biome => &biome::SCOPE,
        }
    }

    fn own(self, value: &Value, context: &Context) -> Layers {
        match self {
            Self::Flat | Self::Legacy => eslint::own_layers(value, context),
            Self::Biome => biome::own_layers(value, context),
        }
    }

    fn extend(self, entry: &str, parent: &Value, context: &Context) -> Layers {
        match self {
            Self::Flat => eslint::flat_extend(entry, parent, context),
            Self::Legacy => eslint::legacy_extend(entry, context),
            Self::Biome => biome::extend(entry, context),
        }
    }
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

    pub fn layers(&self, value: &Value) -> Layers {
        match self.reader {
            Family::Flat => tree(Tree::Flat, value, self),
            Family::Legacy => tree(Tree::Legacy, value, self),
            Family::Biome => tree(Tree::Biome, value, self),
            Family::Oxlint => oxlint::layers(value, self),
        }
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

pub fn expanded(
    value: &Value,
    context: &Context,
    walk: Walk,
    produce: impl FnOnce() -> Layers,
) -> Layers {
    if context.exceeded() {
        return vec![context.config.unknown_layer.clone()];
    }

    let key: MemoKey = (
        context.file.clone(),
        context.shared,
        context.chain.clone(),
        walk,
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

fn patterns(value: &Value, context: &Context, mode: Mode) -> Coverage {
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

fn inherit(family: Tree, entry: &Value, parent: &Value, context: &Context) -> Layers {
    let Value::String(name) = entry else {
        return tree(family, entry, &context.shared());
    };

    if family != Tree::Biome
        && let Some(preset) = context.session().presets.known(name)
    {
        return tree(family, &preset, &context.shared());
    }

    family.extend(name, parent, context)
}

pub fn resolved(specifier: &str, context: &Context, loader: Loader) -> Layers {
    match resolve_module(specifier, &context.file, loader) {
        Some(file) => file_layers(&file, &context.shared()).0,
        None => vec![context.unread()],
    }
}

fn tree(family: Tree, value: &Value, context: &Context) -> Layers {
    if context.exceeded() {
        return vec![context.config.unknown_layer.clone()];
    }

    let Some(_active) = context.activate(value) else {
        return vec![context.config.unknown_layer.clone()];
    };

    expanded(value, context, Walk::Tree, || {
        if let Value::Array(items) = value {
            return last_layers(
                items
                    .iter()
                    .flat_map(|item| tree(family, item, context))
                    .collect(),
            );
        }

        if !matches!(value, Value::Object(_)) {
            return vec![context.unread()];
        }

        let session = context.session();
        let reach = family.scope().coverage(value, context);
        let entries = match property(session, value, "extends") {
            Value::Undefined => Vec::new(),
            Value::Array(items) => items.as_ref().clone(),
            single => vec![single],
        };

        let biome = family == Tree::Biome;
        let mut result: Layers = entries
            .iter()
            .flat_map(|entry| inherit(family, entry, value, context))
            .map(|layer| {
                if biome && layer.setting == Setting::Unknown {
                    overriding(&layer)
                } else {
                    layer
                }
            })
            .collect();

        result.extend(family.own(value, context));

        let mut layers = scoped(&last_layers(result), &reach);
        let overrides = property(session, value, "overrides");
        if !matches!(overrides, Value::Undefined) {
            let items = match overrides {
                Value::Array(items) => items.as_ref().clone(),
                _ => vec![Value::Unknown],
            };

            let inner: Layers = items
                .iter()
                .flat_map(|item| tree(family, item, context))
                .collect();
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

        let config = match manifest(path).json() {
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

enum Source {
    Manifest(Rc<str>),
    Module(Rc<Module>),
}

impl Source {
    fn at(file: &str, context: &Context) -> Option<Self> {
        if context.reader != Family::Legacy || basename(file) != "package.json" {
            return context.config.session.module_at(file).map(Self::Module);
        }

        let real = fs::canonicalize(file).ok()?;
        // A manifest linked to a YAML or `.eslintrc` file reads in that format, and one linked to a name that is not UTF-8 keys its chain on the module reader's lossy path, as both did before.
        let Some(real) = real.to_str().filter(|real| !data_file(real)) else {
            let module = context.config.session.module_at(file)?;
            return Some(match module.data {
                Some(_) => Self::Module(module),
                None => Self::Manifest(module.file.clone()),
            });
        };

        if matches!(*manifest(file), Manifest::Unread) {
            return None;
        }

        Some(Self::Manifest(real.into()))
    }

    fn file(&self) -> &Rc<str> {
        match self {
            Self::Manifest(file) => file,
            Self::Module(module) => &module.file,
        }
    }
}

pub fn file_layers(file: &str, context: &Context) -> (Layers, Value) {
    let config = context.config;
    let Some(source) = Source::at(file, context) else {
        return (vec![context.unread()], Value::Undefined);
    };

    let start = context.chain.len();
    let cap = context.reader.chain_cap();
    if start >= cap || context.chain.contains(source.file()) {
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
            files: HashSet::from([source.file().clone()]),
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
        depth.traced(|| depth.files_reached(|| read_file_layers(file, &source, context)));

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

fn read_file_layers(file: &str, source: &Source, context: &Context) -> (Layers, Value) {
    let mut chain = context.chain.as_ref().clone();
    chain.push(source.file().clone());
    let next = Context {
        file: file.into(),
        chain: Rc::new(chain),
        ..context.clone()
    };

    let session = context.session();
    let module = match source {
        Source::Manifest(_) => {
            let value = context.config.eslint_config(file);
            return (next.layers(&value), property(session, &value, "root"));
        }
        Source::Module(module) => module,
    };

    if context.reader == Family::Biome {
        let value = biome::jsonc(&module.text);
        return (next.layers(&value), property(session, &value, "root"));
    }

    if let Some(value) = &module.data {
        if context.reader != Family::Legacy {
            return (vec![context.unread()], Value::Undefined);
        }

        return (next.layers(value), property(session, value, "root"));
    }

    let value = session.exported(&module.file, "default", &context.chain);
    (next.layers(&value), property(session, &value, "root"))
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
}
