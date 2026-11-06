use std::collections::HashSet;
use std::fs;

use super::evaluate::{Value, property};
use super::find::Loader;
use super::layers::{
    Config, Context, Coverage, Decision, Family, Identity, Layers, Mode, Setting, decided,
    enforcing, excluding, file_layers, identity, layer, level_of, patterns, read, resolved,
};
use crate::paths::{dirname, join};

pub const FILES: [&str; 2] = ["biome.json", "biome.jsonc"];

const SKIPPED: [&str; 5] = ["extends", "overrides", "includes", "include", "ignore"];

#[derive(Default)]
struct Findings {
    rule: Vec<Setting>,
    group: Vec<Setting>,
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
                } else if key == "style"
                    && inside == "rules"
                    && matches!(&item, Value::String(text) if text.as_ref() != "off")
                    || key == "all"
                        && matches!(item, Value::Bool(true))
                        && (inside == "rules" || inside == "style")
                {
                    found.group.push(Setting::On);
                } else {
                    settings(&item, context, &key, found);
                }
            }
        }
        _ => {}
    }
}

pub fn own_layers(value: &Value, context: &Context) -> Layers {
    let mut findings = Findings::default();
    settings(value, context, "", &mut findings);
    let found = if findings.rule.is_empty() {
        findings.group
    } else {
        findings.rule
    };

    if found.is_empty() {
        return Vec::new();
    }

    let setting = if found.iter().all(|&item| item == Setting::Off) {
        Setting::Off
    } else if found.contains(&Setting::Unknown) {
        Setting::Unknown
    } else {
        Setting::On
    };

    vec![layer(setting)]
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
