use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::Hash;
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use crate::format::panic_text;

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
