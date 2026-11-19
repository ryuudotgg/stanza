use std::fs;
use std::rc::Rc;

use serde_json::Value as Json;

use super::evaluate::{Value, property};
use super::find::Loader;
use super::layers::{
    Config, Context, Coverage, Decision, Family, Layer, Layers, Mode, Rank, Setting, Target,
    decided, enforcing, excluding, file_layers, json_value, level_of, patterns, ranked, read,
    resolved,
};
use crate::paths::{dirname, is_absolute, join, resolve};

pub const FILES: [&str; 4] = ["biome.json", "biome.jsonc", ".biome.json", ".biome.jsonc"];

const NESTED_EXTENDS: usize = 10;
pub const CHAIN_CAP: usize = NESTED_EXTENDS + 1;

const SKIPPED: [&str; 5] = ["extends", "overrides", "includes", "include", "ignore"];

#[derive(Default)]
struct Findings {
    rule: Vec<Setting>,
    group: Vec<Setting>,
    group_preset: Vec<Setting>,
    root_preset: Vec<Setting>,
    misplaced: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    Config,
    Linter,
    Rules,
    Group,
    Style,
    Elsewhere,
}

pub fn jsonc(text: &str) -> Value {
    let Some(json) = without_comments(text.strip_prefix('\u{feff}').unwrap_or(text)) else {
        return Value::Unknown;
    };

    serde_json::from_slice::<Json>(&json).map_or(Value::Unknown, |json| json_value(&json))
}

fn without_comments(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut json = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        match (byte, bytes.get(index + 1)) {
            (b'"', _) => {
                let end = string_end(bytes, index)?;
                json.extend_from_slice(&bytes[index..end]);
                index = end;
            }
            (b'/', Some(b'/')) => {
                index += bytes[index..]
                    .iter()
                    .position(|&byte| matches!(byte, b'\n' | b'\r'))
                    .unwrap_or(bytes.len() - index);
                json.push(b' ');
            }
            (b'/', Some(b'*')) => {
                let close = bytes[index + 2..]
                    .windows(2)
                    .position(|pair| pair == b"*/")?;
                index += close + 4;
                json.push(b' ');
            }
            (b'}' | b']', _) => {
                drop_trailing_comma(&mut json);
                json.push(byte);
                index += 1;
            }
            _ => {
                json.push(byte);
                index += 1;
            }
        }
    }

    Some(json)
}

fn string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return Some(index + 1),
            _ => index += 1,
        }
    }

    None
}

fn drop_trailing_comma(json: &mut [u8]) {
    let tokens: Vec<(usize, u8)> = json
        .iter()
        .copied()
        .enumerate()
        .rev()
        .filter(|(_, byte)| !byte.is_ascii_whitespace())
        .take(2)
        .collect();

    if let [(comma, b','), (_, before)] = tokens[..]
        && !matches!(before, b'{' | b'[' | b',')
    {
        json[comma] = b' ';
    }
}

fn settings(value: &Value, context: &Context, at: Position, found: &mut Findings) {
    match value {
        Value::Unknown => found.rule.push(Setting::Unknown),
        Value::Array(items) => {
            for item in items.iter() {
                settings(item, context, Position::Elsewhere, found);
            }
        }
        Value::Object(object) => {
            for key in object.keys() {
                if at == Position::Config && SKIPPED.contains(&key.as_str()) {
                    continue;
                }

                let item = read(value, &key, context);
                entry(at, &key, &item, context, found);
            }
        }
        _ => {}
    }
}

fn entry(at: Position, key: &str, item: &Value, context: &Context, found: &mut Findings) {
    let session = context.session();
    match (at, key) {
        (Position::Config, "linter") => settings(item, context, Position::Linter, found),
        (Position::Linter, "rules") => settings(item, context, Position::Rules, found),
        (Position::Rules, "preset") => found.root_preset.push(preset(item)),
        (Position::Rules | Position::Style, "all") => {
            if matches!(item, Value::Bool(true)) {
                found.group.push(Setting::On);
            }
        }
        (Position::Rules, "style") if matches!(item, Value::String(_)) => {
            found.group.push(level_of(session, item));
        }
        (Position::Rules, "style") => settings(item, context, Position::Style, found),
        (Position::Style, "preset") => found.group_preset.push(preset(item)),
        (Position::Style, "useBlockStatements") => found.rule.push(level_of(session, item)),
        (_, "useBlockStatements" | "rules") | (Position::Linter, "style") => found.misplaced = true,
        (_, "style") if matches!(item, Value::Object(_)) => found.misplaced = true,
        (Position::Rules, _) => settings(item, context, Position::Group, found),
        (Position::Style | Position::Group, _) => {}
        _ => settings(item, context, Position::Elsewhere, found),
    }
}

fn preset(value: &Value) -> Setting {
    match value {
        Value::String(name) if name.as_ref() == "all" => Setting::On,
        Value::String(name) if matches!(name.as_ref(), "none" | "recommended") => Setting::Off,
        _ => Setting::Unknown,
    }
}

fn combined(found: &[Setting]) -> Setting {
    if found.iter().all(|&item| item == Setting::Off) {
        Setting::Off
    } else if found.contains(&Setting::Unknown) {
        Setting::Unknown
    } else {
        Setting::On
    }
}

pub fn own_layers(value: &Value, context: &Context) -> Layers {
    let mut findings = Findings::default();
    settings(value, context, Position::Config, &mut findings);
    if findings.misplaced {
        return vec![Rc::new(Layer {
            reach: scope(value, context),
            setting: Setting::Unknown,
            rank: Rank::Unread,
            overriding: false,
        })];
    }

    let explicit = if findings.rule.is_empty() {
        findings.group
    } else {
        findings.rule
    };

    [
        (findings.root_preset, Rank::RootPreset),
        (findings.group_preset, Rank::GroupPreset),
        (explicit, Rank::Explicit),
    ]
    .into_iter()
    .filter(|(found, _)| !found.is_empty())
    .map(|(found, rank)| ranked(combined(&found), rank))
    .collect()
}

pub fn scope(value: &Value, context: &Context) -> Coverage {
    let session = context.session();
    let includes = property(session, value, "includes");
    let reach = if matches!(includes, Value::Undefined) {
        patterns(&property(session, value, "include"), context, Mode::Biome1)
    } else {
        patterns(&includes, context, Mode::Biome)
    };

    excluding(
        reach,
        &property(session, value, "ignore"),
        context,
        Mode::Biome1,
    )
}

pub fn extend(entry: &str, context: &Context) -> Layers {
    if entry == "//" {
        return nearest_root(context);
    }

    if entry.starts_with('.') || is_absolute(entry) {
        let file = resolve(dirname(&context.file), entry);
        return file_layers(&file, &context.shared()).0;
    }

    resolved(entry, context, Loader::Import)
}

fn nearest_root(context: &Context) -> Layers {
    let mut dir = dirname(&context.file);
    while dirname(dir) != dir {
        dir = dirname(dir);
        for name in FILES {
            let file = join(dir, name);
            if fs::metadata(&file).is_ok() {
                let inner = Context {
                    shared: false,
                    ..context.clone()
                };

                return file_layers(&file, &inner).0;
            }
        }
    }

    vec![context.unread()]
}

pub fn decide(config: &Config, target: &Target) -> Option<Decision> {
    let found = config.every_config(target, Family::Biome);
    let nearest = found.iter().find(|item| item.setting.is_some());
    let chosen = match nearest {
        Some(nearest) if matches!(nearest.root, Value::Bool(false)) => Some(nearest),
        _ => enforcing(&found),
    };

    decided(Family::Biome, chosen)
}
