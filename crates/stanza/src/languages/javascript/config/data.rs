use std::fmt;
use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;

use super::evaluate::ordered_keys;
use super::shared::{self, Settled, replay, settled};

#[derive(Clone, Debug, Default)]
pub enum Data {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(Arc<str>),
    Unknown,
    NoSetting,
    Array(Arc<Vec<Data>>),
    Object(Arc<Mirror>),
}

pub struct Mirror {
    held: Option<usize>,
    every_key_empty_array: bool,
    slots: IndexMap<String, OnceLock<Settled<Data>>>,
}

impl fmt::Debug for Mirror {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.slots.keys()).finish()
    }
}

impl Mirror {
    pub(super) fn held(held: usize, every_key_empty_array: bool, keys: Vec<String>) -> Self {
        Self {
            held: Some(held),
            every_key_empty_array,
            slots: keys.into_iter().map(|key| (key, OnceLock::new())).collect(),
        }
    }

    pub fn keys(&self) -> Vec<String> {
        ordered_keys(self.slots.keys())
    }

    pub fn get(&self, key: &str) -> Data {
        if self.every_key_empty_array {
            return Data::Array(Arc::default());
        }

        let Some(slot) = self.slots.get(key) else {
            return Data::Undefined;
        };

        let forced = slot.get_or_init(|| {
            settled(|| shared::force(self.held.expect("plain slots start filled"), key))
        });

        match forced {
            Ok(value) => value.clone(),
            Err(panic) => replay(panic),
        }
    }
}

impl Data {
    pub fn plain(entries: IndexMap<String, Self>) -> Self {
        Self::Object(Arc::new(Mirror {
            held: None,
            every_key_empty_array: false,
            slots: entries
                .into_iter()
                .map(|(key, value)| (key, OnceLock::from(Ok(value))))
                .collect(),
        }))
    }
}

pub fn property(value: &Data, key: &str) -> Data {
    match value {
        Data::Undefined | Data::NoSetting => value.clone(),
        Data::Object(object) => object.get(key),
        _ => Data::Unknown,
    }
}
