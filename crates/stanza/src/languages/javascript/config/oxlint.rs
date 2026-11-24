use std::rc::Rc;

use super::evaluate::{Value, property};
use super::find::{Loader, resolve_module};
use super::layers::{
    Config, Context, Decision, Family, Layers, Mode, ScopeKeys, Setting, Target, decided, expanded,
    last_layers, layer, scoped, severity,
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

pub const SCOPE: ScopeKeys = ScopeKeys {
    anchor: None,
    include: &[("files", Mode::Oxlint)],
    exclude: ("excludeFiles", Mode::Oxlint),
};

fn own_layers(value: &Value, context: &Context, phase: Phase) -> Layers {
    if matches!(value, Value::Unknown) {
        return vec![context.config.unknown_layer.clone()];
    }

    match phase {
        Phase::Categories => {
            let categories = property(context.session(), value, "categories");
            match property(context.session(), &categories, "style") {
                Value::Undefined => Vec::new(),
                style => vec![layer(severity(context.session(), &style, Family::Oxlint))],
            }
        }
        Phase::Rules => {
            let rules = property(context.session(), value, "rules");
            let Value::Object(object) = &rules else {
                return match rules {
                    Value::Undefined => Vec::new(),
                    _ => vec![context.config.unknown_layer.clone()],
                };
            };

            let mut present: Vec<(Vec<usize>, &str)> = ["curly", "eslint/curly"]
                .into_iter()
                .filter_map(|key| object.first_seen(key).map(|at| (at, key)))
                .collect();

            present.sort_unstable();
            let mut setting = None;
            for (_, key) in present {
                setting = Some(severity(
                    context.session(),
                    &property(context.session(), &rules, key),
                    Family::Oxlint,
                ));
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
                        let reach = Family::Oxlint.scope(item, context);
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
    if context.exceeded() {
        return vec![context.config.unknown_layer.clone()];
    }

    let Some(_active) = context.activate(value) else {
        return vec![context.config.unknown_layer.clone()];
    };

    expanded(value, context, phase.name(), || {
        if !matches!(value, Value::Object(_)) {
            return vec![context.config.unknown_layer.clone()];
        }

        let Some(entries) = extends(value, context) else {
            return vec![context.config.unknown_layer.clone()];
        };

        let mut result: Layers = entries
            .iter()
            .flat_map(|entry| phase_layers(&entry.value, &entry.context, phase))
            .collect();

        result.extend(own_layers(value, context, phase));
        last_layers(result)
    })
}

pub fn decide(config: &Config, target: &Target) -> Option<Decision> {
    for files in config.config_directories(&target.dir, Family::Oxlint) {
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

        let found = config.config_at(file, target, Family::Oxlint);
        return decided(Family::Oxlint, Some(&found));
    }

    None
}
