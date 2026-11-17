use std::collections::HashSet;
use std::fs;

use super::evaluate::{Value, property};
use super::find::Loader;
use super::layers::{
    Config, Context, Coverage, Decision, Family, Identity, Layers, Mode, Rank, Setting, decided,
    enforcing, excluding, file_layers, identity, level_of, patterns, ranked, read, resolved,
};
use crate::paths::{dirname, join};

pub const FILES: [&str; 2] = ["biome.json", "biome.jsonc"];

const SKIPPED: [&str; 5] = ["extends", "overrides", "includes", "include", "ignore"];

#[derive(Default)]
struct Findings {
    rule: Vec<Setting>,
    group: Vec<Setting>,
    group_preset: Vec<Setting>,
    root_preset: Vec<Setting>,
    inside: Vec<Identity>,
    visited: HashSet<(Identity, String)>,
}

fn settings(value: &Value, context: &Context, inside: &str, found: &mut Findings) {
    if matches!(value, Value::Array(_) | Value::Object(_)) {
        let current = identity(value);
        if context.config.overflowed.get() || found.inside.contains(&current) {
            context.config.overflowed.set(true);
            return;
        }

        if !found.visited.insert((current.clone(), inside.to_owned())) {
            return;
        }

        found.inside.push(current);
        visit(value, context, inside, found);
        found.inside.pop();
        return;
    }

    visit(value, context, inside, found);
}

fn visit(value: &Value, context: &Context, inside: &str, found: &mut Findings) {
    match value {
        Value::Unknown => found.rule.push(Setting::Unknown),
        Value::Array(items) => {
            for item in items.iter() {
                settings(item, context, inside, found);
            }
        }
        Value::Object(object) => {
            for key in object.keys() {
                if SKIPPED.contains(&key.as_str()) {
                    continue;
                }

                let item = read(value, &key, context);
                if key == "useBlockStatements" {
                    found.rule.push(level_of(context.session(), &item));
                } else if key == "style" && inside == "rules" && matches!(item, Value::String(_)) {
                    found.group.push(level_of(context.session(), &item));
                } else if key == "all"
                    && matches!(item, Value::Bool(true))
                    && (inside == "rules" || inside == "style")
                {
                    found.group.push(Setting::On);
                } else if key == "preset" && inside == "rules" {
                    found.root_preset.push(preset(&item));
                } else if key == "preset" && inside == "style" {
                    found.group_preset.push(preset(&item));
                } else {
                    settings(&item, context, &key, found);
                }
            }
        }
        _ => {}
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
    settings(value, context, "", &mut findings);
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
        patterns(&includes, context, Mode::Anchored)
    };

    excluding(
        reach,
        &property(session, value, "ignore"),
        context,
        Mode::Anchored,
    )
}

pub fn extend(entry: &str, context: &Context) -> Layers {
    if entry != "//" {
        return resolved(entry, context, Loader::Import);
    }

    let mut dir = dirname(&context.file);
    while dirname(&dir) != dir {
        dir = dirname(&dir);
        for name in FILES {
            let file = join(&dir, name);
            if fs::metadata(&file).is_ok() {
                let inner = Context {
                    shared: false,
                    ..context.clone()
                };

                return file_layers(&file, &inner).0;
            }
        }
    }

    vec![context.config.unknown_layer.clone()]
}

pub fn decide(config: &Config, dir: &str, extension: Option<&str>) -> Option<Decision> {
    let found = config.every_config(dir, extension, Family::Biome);
    let nearest = found.iter().find(|item| item.setting.is_some());
    let chosen = match nearest {
        Some(nearest) if matches!(nearest.root, Value::Bool(false)) => Some(nearest),
        _ => enforcing(&found),
    };

    decided(Family::Biome, chosen)
}
