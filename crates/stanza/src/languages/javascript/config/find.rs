use std::fs;

use serde_json::Value as Json;

use crate::paths::{dirname, is_absolute, join, resolve};

const SUFFIXES: [&str; 10] = [
    "",
    ".json",
    ".jsonc",
    ".js",
    ".cjs",
    ".mjs",
    ".ts",
    ".mts",
    ".cts",
    "/index.js",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Loader {
    Import,
    Require,
}

impl Loader {
    const fn condition(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Require => "require",
        }
    }
}

fn file_at(path: &str) -> Option<String> {
    for suffix in SUFFIXES {
        let candidate = format!("{path}{suffix}");
        if fs::metadata(&candidate).is_ok_and(|metadata| metadata.is_file()) {
            return Some(candidate);
        }
    }

    let stem = path.strip_suffix(".js")?;
    file_at(&format!("{stem}.ts")).or_else(|| file_at(&format!("{stem}.mts")))
}

fn entry_of(value: &Json, loader: Loader) -> Option<String> {
    match value {
        Json::String(entry) => Some(entry.clone()),
        Json::Object(conditions) => conditions.iter().find_map(|(key, target)| {
            (key == loader.condition() || key == "node" || key == "default")
                .then(|| entry_of(target, loader))
                .flatten()
        }),
        _ => None,
    }
}

pub(super) fn manifest(path: &str) -> Option<Json> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text)).ok()
}

fn package_entry(root: &str, subpath: &str, loader: Loader) -> Option<Option<String>> {
    let path = join(root, "package.json");
    let Some(manifest) = manifest(&path) else {
        return fs::metadata(&path).is_ok().then_some(None);
    };

    let field = |key: &str| match &manifest {
        Json::Object(fields) => fields.get(key),
        _ => None,
    };

    if !manifest.is_object() && !manifest.is_array() {
        return Some(None);
    }

    let exports = field("exports").unwrap_or(&Json::Null);
    let target = match exports {
        Json::Object(map) => map.get(&if subpath.is_empty() {
            ".".to_owned()
        } else {
            format!("./{subpath}")
        }),
        Json::Array(_) => None,
        _ if subpath.is_empty() => Some(exports),
        _ => None,
    };

    let entry = target
        .and_then(|target| entry_of(target, loader))
        .or_else(|| {
            subpath
                .is_empty()
                .then(|| entry_of(exports, loader))
                .flatten()
        });

    let main = match field("main") {
        Some(Json::String(main)) => main.as_str(),
        _ => "index.js",
    };

    let path = entry.unwrap_or_else(|| {
        if subpath.is_empty() {
            main.to_owned()
        } else {
            subpath.to_owned()
        }
    });

    Some(file_at(&node_join(root, &path)))
}

fn node_join(directory: &str, name: &str) -> String {
    let joined = resolve("/", &format!("{directory}/{name}"));
    if name.ends_with('/') {
        format!("{joined}/")
    } else {
        joined
    }
}

pub fn resolve_module(specifier: &str, from: &str, loader: Loader) -> Option<String> {
    let real = fs::canonicalize(from).ok()?;
    let real = real.to_string_lossy();
    let dir = dirname(&real);

    if specifier.starts_with('.') || is_absolute(specifier) {
        return file_at(&resolve(dirname(from), specifier));
    }

    resolve_package(specifier, dir, loader)
}

pub fn resolve_package(specifier: &str, from_dir: &str, loader: Loader) -> Option<String> {
    let mut dir = from_dir;
    let parts: Vec<&str> = specifier.split('/').collect();
    let scoped = usize::from(specifier.starts_with('@')) + 1;
    let name = parts[..scoped.min(parts.len())].join("/");
    let subpath = parts[scoped.min(parts.len())..].join("/");
    loop {
        let root = join(&join(dir, "node_modules"), &name);
        if let Some(found) = package_entry(&root, &subpath, loader) {
            return found;
        }

        let parent = dirname(dir);
        if parent == dir {
            return None;
        }

        dir = parent;
    }
}
