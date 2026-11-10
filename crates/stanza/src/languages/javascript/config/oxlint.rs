use std::collections::HashSet;
use std::rc::Rc;

use super::evaluate::{Value, property};
use super::find::{Loader, resolve_module};
use super::layers::{
    Config, Context, Coverage, Decision, Family, Layers, Mode, Setting, decided, excluding,
    expanded, last_layers, layer, patterns, read, scoped,
};

pub const FILES: [&str; 4] = [
    ".oxlintrc.json",
    ".oxlintrc.jsonc",
    "oxlint.config.ts",
    "oxlint.config.mts",
];

#[derive(Clone, Copy)]
enum Phase {
    Categories,
    Rules,
    Overrides,
}

impl Phase {
    const fn name(self) -> &'static str {
        match self {
            Self::Categories => "categories",
            Self::Rules => "rules",
            Self::Overrides => "overrides",
        }
    }
}

fn scope(value: &Value, context: &Context) -> Coverage {
    let session = context.session();
    let reach = patterns(&property(session, value, "files"), context, Mode::Legacy);
    excluding(
        reach,
        &property(session, value, "excludeFiles"),
        context,
        Mode::Legacy,
    )
}

fn level(value: &Value) -> Setting {
    let mut value = value.clone();
    let mut seen: HashSet<usize> = HashSet::new();
    loop {
        match &value {
            Value::String(text) => {
                return match text.as_ref() {
                    "off" | "allow" => Setting::Off,
                    "warn" | "error" | "deny" => Setting::On,
                    _ => Setting::Unknown,
                };
            }
            Value::Number(number) if *number == 0.0 => return Setting::Off,
            Value::Number(number) if *number == 1.0 || *number == 2.0 => return Setting::On,
            Value::Number(_) => return Setting::Unknown,
            Value::Array(items) => {
                if !seen.insert(Rc::as_ptr(items).addr()) {
                    return Setting::Unknown;
                }

                value = items.first().cloned().unwrap_or_default();
            }
            _ => return Setting::Unknown,
        }
    }
}

fn own_layers(value: &Value, context: &Context, phase: Phase) -> Layers {
    if matches!(value, Value::Unknown) {
        return vec![context.config.unknown_layer.clone()];
    }

    match phase {
        Phase::Categories => {
            let categories = read(value, "categories", context);
            match read(&categories, "style", context) {
                Value::Undefined | Value::NoSetting => Vec::new(),
                style => vec![layer(level(&style))],
            }
        }
        Phase::Rules => {
            let rules = read(value, "rules", context);
            let Value::Object(object) = &rules else {
                return match rules {
                    Value::Undefined | Value::NoSetting => Vec::new(),
                    _ => vec![context.config.unknown_layer.clone()],
                };
            };

            let mut present: Vec<(usize, &str)> = ["curly", "eslint/curly"]
                .into_iter()
                .filter_map(|key| object.position(key).map(|at| (at, key)))
                .collect();

            present.sort_unstable();
            let mut setting = None;
            for (_, key) in present {
                setting = Some(level(&read(&rules, key, context)));
            }

            setting.map(layer).into_iter().collect()
        }
        Phase::Overrides => {
            let overrides = property(context.session(), value, "overrides");
            let items = match overrides {
                Value::Undefined => return Vec::new(),
                Value::Array(items) => items,
                _ => return vec![context.config.unknown_layer.clone()],
            };

            last_layers(
                items
                    .iter()
                    .flat_map(|item| {
                        let reach = scope(item, context);
                        scoped(&own_layers(item, context, Phase::Rules), &reach)
                    })
                    .collect(),
            )
        }
    }
}

struct Extended<'a> {
    value: Value,
    context: Context<'a>,
}

fn extends<'a>(value: &Value, context: &Context<'a>) -> Option<Vec<Extended<'a>>> {
    let session = context.session();
    let items = match property(session, value, "extends") {
        Value::Undefined => return Some(Vec::new()),
        Value::Array(items) => items,
        _ => return None,
    };

    let mut result = Vec::new();
    for entry in items.iter() {
        let Value::String(name) = entry else {
            result.push(Extended {
                value: entry.clone(),
                context: context.shared(),
            });

            continue;
        };

        if !name.starts_with('.') {
            return None;
        }

        let file = resolve_module(name, &context.file, Loader::Import)?;
        let module = session.module_at(&file)?;
        if context.chain.iter().any(|seen| **seen == *module.file) {
            return None;
        }

        context.add_module(&module);

        let mut chain = context.chain.as_ref().clone();
        chain.push(module.file.clone());
        result.push(Extended {
            value: session.exported(&module.file, "default", &context.chain),
            context: Context {
                file: module.file.clone(),
                shared: true,
                chain: Rc::new(chain),
                ..context.clone()
            },
        });
    }

    Some(result)
}

pub fn layers(value: &Value, context: &Context) -> Layers {
    last_layers(
        [Phase::Categories, Phase::Rules, Phase::Overrides]
            .into_iter()
            .flat_map(|phase| phase_layers(value, context, phase))
            .collect(),
    )
}

fn phase_layers(value: &Value, context: &Context, phase: Phase) -> Layers {
    if matches!(value, Value::NoSetting) {
        return Vec::new();
    }

    if context.exceeded() {
        return vec![context.config.unknown_layer.clone()];
    }

    if context.has_ancestor(value) {
        context.count_cycle();
        return vec![context.config.unknown_layer.clone()];
    }

    expanded(value, context, phase.name(), || {
        if !matches!(value, Value::Object(_)) {
            return vec![context.config.unknown_layer.clone()];
        }

        let next = context.with_ancestor(value);
        let Some(entries) = extends(value, &next) else {
            return vec![context.config.unknown_layer.clone()];
        };

        let mut result: Layers = entries
            .iter()
            .flat_map(|entry| phase_layers(&entry.value, &entry.context, phase))
            .collect();

        result.extend(own_layers(value, &next, phase));
        last_layers(result)
    })
}

pub fn decide(config: &Config, dir: &str, extension: Option<&str>) -> Option<Decision> {
    for files in config.config_directories(dir, Family::Oxlint) {
        if files.len() > 1 {
            return Some(Decision {
                family: Family::Oxlint,
                setting: Setting::Unknown,
                files: files.as_ref().clone(),
            });
        }

        let Some(file) = files.first() else {
            continue;
        };

        let found = config.config_at(file, dir, extension, Family::Oxlint);
        return decided(Family::Oxlint, Some(&found));
    }

    None
}
