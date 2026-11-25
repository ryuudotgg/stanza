use std::collections::HashMap;
use std::fs;
use std::sync::{LazyLock, Mutex};

use indexmap::IndexMap;

use super::super::biome;
use super::super::data::{Data, property};
use super::super::find::{Loader, resolve_exported};
#[cfg(test)]
use super::tests;
use super::{
    Formatter, Read, bounded, is_object, is_undefined, merged, object_of, read_text, readable,
    spread_all,
};
use crate::paths::{dirname, is_absolute, resolve};

// Locked for a whole merge: a per-file Memo would deadlock two threads entering one extends cycle from opposite ends.
pub(super) static MERGED: LazyLock<Mutex<HashMap<String, Merged>>> = LazyLock::new(Mutex::default);

pub(super) fn under_biome(file: &str, value: &Data) -> bool {
    matches!(property(value, "root"), Data::Bool(false))
        && biome::root_config(file).is_some_and(|parent| {
            Formatter::of(&parent) == Formatter::Biome
                && matches!(readable(&parent), Read::Config(_))
        })
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

#[derive(Clone)]
pub(super) struct Merged {
    pub(super) value: Data,
    height: usize,
}

impl Merged {
    const UNKNOWN: Self = Self {
        value: Data::Unknown,
        height: 0,
    };
}

pub(super) struct TooDeep;

pub(super) fn merged_biome(
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
