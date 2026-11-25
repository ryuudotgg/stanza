use std::fmt;
use std::sync::{Arc, OnceLock};

use indexmap::IndexMap;
use serde_json::Value as Json;

use super::evaluator;
use super::object::ordered_keys;
use super::yaml::Tree;
use crate::memo::{Settled, replay, settled};

#[derive(Clone, Debug, Default)]
pub enum Data {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(Arc<str>),
    Unknown,
    Array(Arc<Vec<Data>>),
    Object(Arc<Mirror>),
}

pub struct Mirror {
    address: Option<usize>,
    every_key_empty_array: bool,
    slots: IndexMap<String, OnceLock<Settled<Data>>>,
}

impl fmt::Debug for Mirror {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_list().entries(self.slots.keys()).finish()
    }
}

impl Mirror {
    pub(super) fn lazy(address: usize, every_key_empty_array: bool, keys: Vec<String>) -> Self {
        Self {
            address: Some(address),
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
            settled(|| evaluator::force(self.address.expect("plain slots start filled"), key))
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
            address: None,
            every_key_empty_array: false,
            slots: entries
                .into_iter()
                .map(|(key, value)| (key, OnceLock::from(Ok(value))))
                .collect(),
        }))
    }

    pub fn json(json: &Json) -> Self {
        match json {
            Json::Null => Self::Null,
            Json::Bool(value) => Self::Bool(*value),
            Json::Number(number) => number.as_f64().map_or(Self::Unknown, Self::Number),
            Json::String(text) => Self::String(text.as_str().into()),
            Json::Array(items) => Self::Array(Arc::new(items.iter().map(Self::json).collect())),
            Json::Object(fields) => Self::plain(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), Self::json(value)))
                    .collect(),
            ),
        }
    }

    pub fn tree(tree: &Tree) -> Self {
        match tree {
            Tree::Null => Self::Null,
            Tree::Bool(value) => Self::Bool(*value),
            Tree::Number(number) => Self::Number(*number),
            Tree::String(text) => Self::String(text.as_str().into()),
            Tree::Array(items) => Self::Array(Arc::new(items.iter().map(Self::tree).collect())),
            Tree::Object(fields) => Self::plain(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), Self::tree(value)))
                    .collect(),
            ),
        }
    }
}

pub fn property(value: &Data, key: &str) -> Data {
    match value {
        Data::Undefined => Data::Undefined,
        Data::Object(object) => object.get(key),
        _ => Data::Unknown,
    }
}
