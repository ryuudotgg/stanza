use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::panic::{self, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex, OnceLock, PoisonError, RwLock};

use super::data::{Data, Mirror};
use super::evaluate::Value;
use super::json;
use super::layers::{Config, Decision, Family};
use super::object::Object;
use crate::format::panic_text;

const READERS: [Family; 4] = [Family::Flat, Family::Legacy, Family::Biome, Family::Oxlint];

pub type Settled<T> = Result<T, String>;

pub fn settled<T>(work: impl FnOnce() -> T) -> Settled<T> {
    panic::catch_unwind(AssertUnwindSafe(work)).map_err(|payload| panic_text(&*payload))
}

pub fn replay(panic: &str) -> ! {
    panic::panic_any(panic.to_owned())
}

pub struct Memo<K, V> {
    cells: RwLock<HashMap<K, Arc<OnceLock<Settled<V>>>>>,
}

impl<K, V> Default for Memo<K, V> {
    fn default() -> Self {
        Self {
            cells: RwLock::default(),
        }
    }
}

impl<K: Hash + Eq, V: Clone> Memo<K, V> {
    pub fn get<Q>(&self, key: &Q, make: impl FnOnce() -> V) -> V
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ToOwned<Owned = K> + ?Sized,
    {
        let known = self
            .cells
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .and_then(|cell| cell.get().cloned());

        let found = known.unwrap_or_else(|| self.cell(key).get_or_init(|| settled(make)).clone());
        match found {
            Ok(value) => value,
            Err(panic) => replay(&panic),
        }
    }

    fn cell<Q>(&self, key: &Q) -> Arc<OnceLock<Settled<V>>>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ToOwned<Owned = K> + ?Sized,
    {
        let found = self
            .cells
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned();

        found.unwrap_or_else(|| {
            self.cells
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .entry(key.to_owned())
                .or_default()
                .clone()
        })
    }
}

#[derive(Default)]
struct Evaluator {
    config: Config,
    held: Vec<Rc<Object>>,
    mirrored: HashMap<usize, Data>,
    kept: Vec<Value>,
}

// SAFETY: reached only through EVALUATOR's lock, every operation returns owned copies, nothing in the Rc graph points at thread-local state, and the config ASTs it reaches sit in arenas that are never freed and that nothing allocates into or writes after parsing.
unsafe impl Send for Evaluator {}

static EVALUATOR: LazyLock<Mutex<Evaluator>> = LazyLock::new(Mutex::default);

fn evaluated<R: Send>(work: impl FnOnce(&mut Evaluator) -> R + Send) -> R {
    work(&mut EVALUATOR.lock().unwrap_or_else(PoisonError::into_inner))
}

impl Evaluator {
    fn mirror(&mut self, value: &Value) -> Data {
        let address = match value {
            Value::Undefined => return Data::Undefined,
            Value::Null => return Data::Null,
            Value::Bool(value) => return Data::Bool(*value),
            Value::Number(value) => return Data::Number(*value),
            Value::String(text) => return Data::String(Arc::from(&**text)),
            Value::Unknown => return Data::Unknown,
            Value::NoSetting => return Data::NoSetting,
            Value::Array(items) => Rc::as_ptr(items).addr(),
            Value::Object(object) => Rc::as_ptr(object).addr(),
        };

        if let Some(known) = self.mirrored.get(&address) {
            return known.clone();
        }

        let data = match value {
            Value::Array(items) => Data::Array(Arc::new(
                items.iter().map(|item| self.mirror(item)).collect(),
            )),
            Value::Object(object) => {
                self.held.push(object.clone());
                Data::Object(Arc::new(Mirror::held(
                    self.held.len() - 1,
                    object.answers_every_key(),
                    object.keys(),
                )))
            }
            _ => unreachable!("scalars return above"),
        };

        self.kept.push(value.clone());
        self.mirrored.insert(address, data.clone());
        data
    }
}

pub fn force(held: usize, key: &str) -> Data {
    evaluated(|evaluator| {
        let object = evaluator.held[held].clone();
        let value = object.get(&evaluator.config.session, key);
        evaluator.mirror(&value)
    })
}

pub fn decisions(dir: &str, extension: Option<&str>) -> std::io::Result<Vec<Decision>> {
    evaluated(|evaluator| {
        let config = &evaluator.config;
        config.overflowed.set(false);
        let mut decisions = Vec::new();
        for reader in READERS {
            let decision = reader.decide(config, dir, extension);
            if config.overflowed.replace(false) {
                return Err(std::io::Error::other("a config value contains itself"));
            }

            decisions.extend(decision);
        }

        Ok(decisions)
    })
}

pub fn exported_default(file: &str) -> Data {
    evaluated(|evaluator| {
        let value = evaluator
            .config
            .session
            .exported(file, "default", &Rc::default());

        evaluator.mirror(&value)
    })
}

pub enum Source {
    Program(Data),
    Text(String),
    Missing,
}

pub fn source(file: &str) -> Source {
    evaluated(|evaluator| {
        let session = &evaluator.config.session;
        match session.module_at(file) {
            Some(module) if module.program.is_some() => {
                let value = session.exported(file, "default", &Rc::default());
                Source::Program(evaluator.mirror(&value))
            }
            Some(module) => Source::Text(module.text.clone()),
            None => Source::Missing,
        }
    })
}

pub fn json(text: &str) -> Option<Data> {
    evaluated(|evaluator| json::parse(text).map(|value| evaluator.mirror(&value)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_memo_runs_a_panicking_init_once_and_replays_its_text() {
        let memo: Memo<String, usize> = Memo::default();
        let runs = std::sync::atomic::AtomicUsize::new(0);
        let attempt = || {
            settled(|| {
                memo.get("key", || {
                    runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    panic!("broken\n  config")
                })
            })
        };

        assert_eq!(attempt(), Err("broken config".to_owned()));
        assert_eq!(attempt(), Err("broken config".to_owned()));
        assert_eq!(runs.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(memo.get("other", || 7), 7);
    }
}
