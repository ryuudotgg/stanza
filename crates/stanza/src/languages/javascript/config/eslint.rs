use super::evaluate::{Value, property};
use super::find::{Loader, resolve_package};
use super::layers::{
    Config, Context, Coverage, Decision, Family, Layers, Mode, Reach, Target, decided, enforcing,
    excluding, layer, patterns, resolved, severity,
};
use crate::paths::{basename, dirname};

pub const FLAT_FILES: [&str; 6] = [
    "eslint.config.js",
    "eslint.config.mjs",
    "eslint.config.cjs",
    "eslint.config.ts",
    "eslint.config.mts",
    "eslint.config.cts",
];

pub const LEGACY_FILES: [&str; 7] = [
    ".eslintrc.js",
    ".eslintrc.cjs",
    ".eslintrc.yaml",
    ".eslintrc.yml",
    ".eslintrc.json",
    ".eslintrc",
    "package.json",
];

pub fn own_layers(value: &Value, context: &Context) -> Layers {
    let rules = property(context.session(), value, "rules");
    match property(context.session(), &rules, "curly") {
        Value::Undefined => Vec::new(),
        curly => vec![layer(severity(context.session(), &curly, context.reader))],
    }
}

pub(super) fn package_name(name: &str, prefix: &str) -> String {
    if name.starts_with('@') {
        let (scope, rest) = name.split_once('/').unwrap_or((name, ""));
        if rest.is_empty() || rest == prefix {
            return format!("{scope}/{prefix}");
        }

        let package = rest.split('/').next().unwrap_or_default();
        if package == prefix
            || package
                .strip_prefix(prefix)
                .is_some_and(|tail| tail.starts_with('-'))
        {
            return name.to_owned();
        }

        return format!("{scope}/{prefix}-{rest}");
    }

    if name
        .strip_prefix(prefix)
        .is_some_and(|tail| tail.starts_with('-'))
    {
        name.to_owned()
    } else {
        format!("{prefix}-{name}")
    }
}

pub fn flat_scope(value: &Value, context: &Context) -> Coverage {
    let session = context.session();
    if !matches!(property(session, value, "basePath"), Value::Undefined) {
        return Coverage::Fixed(Reach::Some);
    }

    let reach = patterns(&property(session, value, "files"), context, Mode::Anchored);
    excluding(
        reach,
        &property(session, value, "ignores"),
        context,
        Mode::Anchored,
    )
}

pub fn flat_extend(entry: &str, parent: &Value, context: &Context) -> Layers {
    if !entry.starts_with('.') && !entry.starts_with('/') {
        let session = context.session();
        let slash = entry.rfind('/');
        let name = match slash {
            Some(slash) => &entry[..slash],
            None => entry
                .char_indices()
                .last()
                .map_or("", |(last, _)| &entry[..last]),
        };

        let plugins = property(session, parent, "plugins");
        let plugin = property(session, &plugins, name);
        if let Some(slash) = slash.filter(|&slash| slash > 0)
            && !matches!(plugin, Value::Undefined)
        {
            let configs = property(session, &plugin, "configs");
            let config = property(session, &configs, &entry[slash + 1..]);
            return context.reader.layers(&config, &context.shared());
        }
    }

    resolved(entry, context, Loader::Import)
}

pub fn legacy_scope(value: &Value, context: &Context) -> Coverage {
    let session = context.session();
    let reach = patterns(&property(session, value, "files"), context, Mode::Legacy);
    excluding(
        reach,
        &property(session, value, "excludedFiles"),
        context,
        Mode::Legacy,
    )
}

pub fn legacy_extend(entry: &str, context: &Context) -> Layers {
    if let Some(preset) = entry.strip_prefix("plugin:") {
        return plugin_preset(preset, context);
    }

    let specifier = if !entry.starts_with('.') && !entry.starts_with('/') {
        package_name(entry, "eslint-config")
    } else {
        entry.to_owned()
    };

    resolved(&specifier, context, Loader::Require)
}

// eslintrc resolves plugins from the directory the entry config was found in, not from the shareable config that names them or a symlink's target.
fn plugin_preset(preset: &str, context: &Context) -> Layers {
    let session = context.session();
    context.bind_entry();

    let found = preset.rsplit_once('/').and_then(|(plugin, name)| {
        let specifier = package_name(plugin, "eslint-plugin");
        let file = resolve_package(&specifier, dirname(&context.entry), Loader::Require)?;
        let module = session.module_at(&file)?;
        let plugin = session.exported(&module.file, "default", &context.chain);
        let configs = property(session, &plugin, "configs");
        let config = property(session, &configs, name);
        let inner = context.enter_preset(&module.file, name)?;
        Some((config, inner))
    });

    match found {
        Some((config, inner)) => context.reader.layers(&config, &inner),
        None => vec![context.config.unknown_layer.clone()],
    }
}

pub fn legacy_candidate(config: &Config, file: &str) -> bool {
    basename(file) != "package.json" || !matches!(config.eslint_config(file), Value::Undefined)
}

pub fn flat_decide(config: &Config, target: &Target) -> Option<Decision> {
    let found = config.every_config(target, Family::Flat);
    decided(Family::Flat, enforcing(&found))
}

pub fn legacy_decide(config: &Config, target: &Target) -> Option<Decision> {
    let mut has_flat_config = None;
    for files in config.config_directories(&target.dir, Family::Legacy) {
        let Some(file) = files.first() else {
            continue;
        };

        if basename(file) == "package.json" {
            let flat = *has_flat_config.get_or_insert_with(|| {
                config
                    .config_directories(&target.dir, Family::Flat)
                    .any(|files| !files.is_empty())
            });

            if flat {
                continue;
            }
        }

        let found = config.config_at(file, target, Family::Legacy);
        if found.setting.is_some() || matches!(found.root, Value::Bool(true)) {
            return decided(Family::Legacy, Some(&found));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_follow_the_legacy_shorthand() {
        for (name, prefix, expected) in [
            ("airbnb", "eslint-config", "eslint-config-airbnb"),
            ("eslint-config-x", "eslint-config", "eslint-config-x"),
            ("@scope", "eslint-config", "@scope/eslint-config"),
            ("@scope/", "eslint-config", "@scope/eslint-config"),
            ("@scope/base", "eslint-config", "@scope/eslint-config-base"),
            (
                "@scope/eslint-config-base",
                "eslint-config",
                "@scope/eslint-config-base",
            ),
            ("local", "eslint-plugin", "eslint-plugin-local"),
            (
                "eslint-plugin-local",
                "eslint-plugin",
                "eslint-plugin-local",
            ),
            ("@scope", "eslint-plugin", "@scope/eslint-plugin"),
            (
                "@scope/extra",
                "eslint-plugin",
                "@scope/eslint-plugin-extra",
            ),
        ] {
            assert_eq!(package_name(name, prefix), expected, "{name}");
        }
    }
}
