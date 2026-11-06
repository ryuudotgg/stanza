use super::evaluate::{Value, property};
use super::find::Loader;
use super::layers::{
    Config, Context, Coverage, Decision, Family, Layers, Mode, Reach, decided, enforcing,
    excluding, layer, level_of, patterns, read, resolved,
};
use crate::paths::basename;

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
    match read(&rules, "curly", context) {
        Value::Undefined | Value::NoSetting => Vec::new(),
        curly => vec![layer(level_of(context.session(), &curly))],
    }
}

fn package_name(name: &str) -> String {
    if name.starts_with('@') {
        let mut parts = name.split('/');
        let scope = parts.next().unwrap_or_default();
        let package = match parts.next() {
            Some(package) if package.starts_with("eslint-config") => package.to_owned(),
            Some(package) if !package.is_empty() => format!("eslint-config-{package}"),
            _ => "eslint-config".to_owned(),
        };

        return format!("{scope}/{package}");
    }

    if name.starts_with("eslint-config-") {
        name.to_owned()
    } else {
        format!("eslint-config-{name}")
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
    let specifier = if !entry.starts_with('.') && !entry.starts_with('/') {
        package_name(entry)
    } else {
        entry.to_owned()
    };

    resolved(&specifier, context, Loader::Require)
}

pub fn legacy_candidate(config: &Config, file: &str) -> bool {
    basename(file) != "package.json" || !matches!(config.eslint_config(file), Value::Undefined)
}

pub fn flat_decide(config: &Config, dir: &str, extension: Option<&str>) -> Option<Decision> {
    let found = config.every_config(dir, extension, Family::Flat);
    decided(Family::Flat, enforcing(&found))
}

pub fn legacy_decide(config: &Config, dir: &str, extension: Option<&str>) -> Option<Decision> {
    let mut has_flat_config = None;
    for files in config.config_directories(dir, Family::Legacy) {
        let Some(file) = files.first() else {
            continue;
        };

        if basename(file) == "package.json" {
            let flat = *has_flat_config.get_or_insert_with(|| {
                config
                    .config_directories(dir, Family::Flat)
                    .any(|files| !files.is_empty())
            });

            if flat {
                continue;
            }
        }

        let found = config.config_at(file, dir, extension, Family::Legacy);
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
        assert_eq!(package_name("airbnb"), "eslint-config-airbnb");
        assert_eq!(package_name("eslint-config-x"), "eslint-config-x");
        assert_eq!(package_name("@scope"), "@scope/eslint-config");
        assert_eq!(package_name("@scope/"), "@scope/eslint-config");
        assert_eq!(package_name("@scope/base"), "@scope/eslint-config-base");
        assert_eq!(
            package_name("@scope/eslint-config-base"),
            "@scope/eslint-config-base"
        );
        assert_eq!(
            package_name("@scope/base/extra"),
            "@scope/eslint-config-base"
        );
    }
}
