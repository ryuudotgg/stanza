use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};

use super::estree::Node;
use super::evaluate::{Chain, OnUnwind, Origin, Value};
use super::module::{Module, Session};

#[derive(Debug, Default)]
pub struct Object {
    parts: Vec<Part>,
    every_key_empty_array: bool,
    probes: Cell<usize>,
    cost: OnceCell<usize>,
    indexed: Cell<bool>,
    found: RefCell<HashMap<String, Found>>,
    inserted: OnceCell<Rc<IndexSet<String>>>,
}

#[derive(Debug)]
enum Part {
    Own(Rc<Own>),
    Spread(Rc<Object>),
}

#[derive(Debug, Default)]
struct Own {
    slots: IndexMap<String, Slot>,
    origins: HashMap<String, Origin>,
}

#[derive(Clone, Debug, Default)]
struct Found {
    owner: Option<Rc<Own>>,
    origin: Option<Origin>,
}

#[derive(Debug)]
enum Slot {
    Plain(Value),
    Lazy(Lazy),
    Namespace { file: Rc<str>, chain: Chain },
}

struct Lazy {
    item: Node,
    module: Rc<Module>,
    chain: Chain,
    state: RefCell<LazyState>,
}

impl std::fmt::Debug for Lazy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Lazy")
    }
}

#[derive(Debug)]
enum LazyState {
    Pending,
    Resolving,
    Done(Value),
}

fn array_index(key: &str) -> Option<u32> {
    let index: u32 = key.parse().ok()?;
    (index != u32::MAX && index.to_string() == key).then_some(index)
}

pub fn ordered_keys<'a>(keys: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let mut indexes: Vec<(u32, &String)> = Vec::new();
    let mut names: Vec<&String> = Vec::new();
    for key in keys {
        match array_index(key) {
            Some(index) => indexes.push((index, key)),
            None => names.push(key),
        }
    }

    indexes.sort_unstable_by_key(|(index, _)| *index);
    indexes
        .into_iter()
        .map(|(_, key)| key)
        .chain(names)
        .cloned()
        .collect()
}

impl Own {
    fn found(self: &Rc<Self>, key: &str) -> Found {
        Found {
            owner: self.slots.contains_key(key).then(|| self.clone()),
            origin: self.origins.get(key).cloned(),
        }
    }
}

impl Object {
    fn of(parts: Vec<Part>) -> Rc<Self> {
        Rc::new(Self {
            parts,
            ..Self::default()
        })
    }

    pub(super) fn namespace<'a>(
        keys: impl Iterator<Item = &'a String>,
        file: &Rc<str>,
        chain: &Chain,
    ) -> Rc<Self> {
        let slots = keys
            .map(|key| {
                let slot = Slot::Namespace {
                    file: file.clone(),
                    chain: chain.clone(),
                };

                (key.clone(), slot)
            })
            .collect();

        Self::owned(Own {
            slots,
            ..Own::default()
        })
    }

    fn owned(own: Own) -> Rc<Self> {
        Self::of(vec![Part::Own(Rc::new(own))])
    }

    pub fn plain(entries: Vec<(&str, Value)>) -> Rc<Self> {
        Self::owned(Own {
            slots: entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), Slot::Plain(value)))
                .collect(),
            ..Own::default()
        })
    }

    pub fn every_key_empty_array() -> Rc<Self> {
        Rc::new(Self {
            every_key_empty_array: true,
            ..Self::default()
        })
    }

    pub fn keys(&self) -> Vec<String> {
        if let [Part::Own(own)] = self.parts.as_slice() {
            return ordered_keys(own.slots.keys());
        }

        ordered_keys(self.inserted_keys().iter())
    }

    pub fn first_seen(&self, key: &str) -> Option<Vec<usize>> {
        let mut path = Vec::new();
        self.trace(key, &mut path).then_some(path)
    }

    fn trace(&self, key: &str, path: &mut Vec<usize>) -> bool {
        for (at, part) in self.parts.iter().enumerate() {
            match part {
                Part::Own(own) => {
                    if let Some(index) = own.slots.get_index_of(key) {
                        path.extend([at, index]);
                        return true;
                    }
                }
                Part::Spread(source) => {
                    if source.lookup(key).owner.is_some() {
                        path.push(at);
                        return source.trace(key, path);
                    }
                }
            }
        }

        false
    }

    fn inserted_keys(&self) -> Rc<IndexSet<String>> {
        if let Some(keys) = self.inserted.get() {
            return keys.clone();
        }

        let keys = match self.parts.as_slice() {
            [Part::Spread(source)] => source.inserted_keys(),
            parts => {
                let mut keys = IndexSet::new();
                for part in parts {
                    match part {
                        Part::Own(own) => keys.extend(own.slots.keys().cloned()),
                        Part::Spread(source) => {
                            keys.extend(source.inserted_keys().iter().cloned());
                        }
                    }
                }

                Rc::new(keys)
            }
        };

        self.inserted.get_or_init(|| keys).clone()
    }

    // The memo keeps a shared spread source from being walked once per path, and the index keeps a wide object from being scanned once per key.
    fn lookup(&self, key: &str) -> Found {
        match self.parts.as_slice() {
            [] => return Found::default(),
            [Part::Own(own)] => return own.found(key),
            _ => {}
        }

        if let Some(found) = self.found.borrow().get(key) {
            return found.clone();
        }

        if self.indexed.get() {
            return Found::default();
        }

        let mut found = Found::default();
        let mut scanned = 0;
        for part in self.parts.iter().rev() {
            scanned += 1;
            let inner = match part {
                Part::Own(own) => own.found(key),
                Part::Spread(source) => source.lookup(key),
            };

            found.owner = found.owner.or(inner.owner);
            found.origin = found.origin.or(inner.origin);
            if found.owner.is_some() && found.origin.is_some() {
                break;
            }
        }

        self.found
            .borrow_mut()
            .insert(key.to_owned(), found.clone());

        let probes = self.probes.get() + scanned;
        self.probes.set(probes);
        if probes > self.parts.len() && probes > self.index_cost() {
            self.index();
        }

        found
    }

    fn index_cost(&self) -> usize {
        *self.cost.get_or_init(|| {
            self.parts
                .iter()
                .map(|part| match part {
                    Part::Own(own) => own.slots.len(),
                    Part::Spread(source) => source.inserted_keys().len(),
                })
                .sum()
        })
    }

    fn index(&self) {
        let mut index: HashMap<String, Found> = HashMap::new();
        let mut write = |key: &str, found: Found| {
            let entry = index.entry(key.to_owned()).or_default();
            entry.owner = found.owner;
            if found.origin.is_some() {
                entry.origin = found.origin;
            }
        };

        for part in &self.parts {
            match part {
                Part::Own(own) => {
                    for key in own.slots.keys() {
                        write(key, own.found(key));
                    }
                }
                Part::Spread(source) => {
                    for key in source.inserted_keys().iter() {
                        write(key, source.lookup(key));
                    }
                }
            }
        }

        *self.found.borrow_mut() = index;
        self.indexed.set(true);
    }

    pub const fn answers_every_key(&self) -> bool {
        self.every_key_empty_array
    }

    pub fn get(&self, session: &Session, key: &str) -> Value {
        if self.every_key_empty_array {
            return Value::Array(Rc::default());
        }

        let Some(owner) = self.lookup(key).owner else {
            return Value::Undefined;
        };

        match &owner.slots[key] {
            Slot::Plain(value) => value.clone(),
            Slot::Namespace { file, chain } => session.exported(file, key, chain),
            Slot::Lazy(lazy) => lazy.force(session),
        }
    }

    pub fn origin(&self, key: &str) -> Option<Origin> {
        self.lookup(key).origin
    }
}

impl Lazy {
    fn force(&self, session: &Session) -> Value {
        match &*self.state.borrow() {
            LazyState::Pending => {}
            LazyState::Resolving => return Value::Unknown,
            LazyState::Done(value) => return value.clone(),
        }

        *self.state.borrow_mut() = LazyState::Resolving;
        let _unwound = OnUnwind(|| {
            if let Ok(mut state) = self.state.try_borrow_mut() {
                *state = LazyState::Pending;
            }
        });

        let value = if self.item.flag("method") || self.item.text("kind") != Some("init") {
            Value::Unknown
        } else {
            self.item.field("value").map_or(Value::Unknown, |node| {
                session.evaluate(node, &self.module, &self.chain)
            })
        };

        *self.state.borrow_mut() = LazyState::Done(value.clone());
        value
    }
}

#[derive(Default)]
pub(super) struct Builder {
    parts: Vec<Part>,
    own: Own,
}

impl Builder {
    fn close_own(&mut self) {
        if !self.own.slots.is_empty() {
            self.parts
                .push(Part::Own(Rc::new(std::mem::take(&mut self.own))));
        }
    }

    pub(super) fn spread(&mut self, source: &Rc<Object>) {
        self.close_own();
        self.parts.push(Part::Spread(source.clone()));
    }

    pub(super) fn lazy(&mut self, key: &str, item: Node, module: &Rc<Module>, chain: &Chain) {
        let lazy = Lazy {
            item,
            module: module.clone(),
            chain: chain.clone(),
            state: RefCell::new(LazyState::Pending),
        };

        self.own.slots.insert(key.to_owned(), Slot::Lazy(lazy));
        self.own.origins.insert(
            key.to_owned(),
            Origin {
                file: module.file.clone(),
                start: item.start(),
            },
        );
    }

    pub(super) fn finish(mut self) -> Rc<Object> {
        self.close_own();
        Object::of(self.parts)
    }
}
