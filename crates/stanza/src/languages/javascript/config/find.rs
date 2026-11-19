use std::fs;

use serde_json::{Map, Value as Json};

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

const REQUIRED: [&str; 4] = ["", ".js", ".json", ".node"];

const REQUIRED_INDEX: [&str; 3] = ["/index.js", "/index.json", "/index.node"];

fn is_file(path: &str) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn file_at(path: &str) -> Option<String> {
    for suffix in SUFFIXES {
        let candidate = format!("{path}{suffix}");
        if is_file(&candidate) {
            return Some(candidate);
        }
    }

    let stem = path.strip_suffix(".js")?;
    file_at(&format!("{stem}.ts")).or_else(|| file_at(&format!("{stem}.mts")))
}

fn first_required(path: &str, suffixes: &[&str]) -> Option<Option<String>> {
    let file = suffixes
        .iter()
        .map(|suffix| format!("{path}{suffix}"))
        .find(|candidate| is_file(candidate))?;

    Some((!file.ends_with(".node")).then_some(file))
}

fn required_directory(dir: &str) -> Option<Option<String>> {
    let path = join(dir, "package.json");
    if is_file(&path) {
        let Some(manifest) = manifest(&path) else {
            return Some(None);
        };

        if let Some(Json::String(main)) = manifest.get("main")
            && !main.is_empty()
        {
            let main = join(dir, main);
            return first_required(&main, &REQUIRED)
                .or_else(|| first_required(&main, &REQUIRED_INDEX))
                .or_else(|| first_required(dir, &REQUIRED_INDEX))
                .or(Some(None));
        }
    }

    first_required(dir, &REQUIRED_INDEX)
}

fn required_search(path: &str) -> Option<Option<String>> {
    first_required(path, &REQUIRED).or_else(|| required_directory(path))
}

#[derive(Clone, Copy)]
enum Search {
    CommonJs,
    Loose,
}

impl Search {
    fn of(loader: Loader, from: &str) -> Self {
        let typescript = [".ts", ".mts", ".cts"]
            .iter()
            .any(|extension| from.ends_with(extension));

        if loader == Loader::Require && !typescript {
            Self::CommonJs
        } else {
            Self::Loose
        }
    }

    fn file(self, path: &str) -> Option<String> {
        match self {
            Self::CommonJs => required_search(path).flatten(),
            Self::Loose => file_at(path),
        }
    }
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

enum Condition {
    Target(String),
    Blocked,
    Unmatched,
}

fn condition(value: &Json, loader: Loader) -> Condition {
    let Json::Object(conditions) = value else {
        return match value {
            Json::String(entry) => Condition::Target(entry.clone()),
            Json::Array(items) => items
                .iter()
                .map(|item| condition(item, loader))
                .find(|found| matches!(found, Condition::Target(entry) if valid_target(entry)))
                .unwrap_or(Condition::Blocked),
            _ => Condition::Blocked,
        };
    };

    if conditions
        .keys()
        .any(|key| key.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Condition::Blocked;
    }

    for (key, target) in conditions {
        let active = ["node", "node-addons", "module-sync", "default"].contains(&key.as_str());
        if active || key == loader.condition() {
            match condition(target, loader) {
                Condition::Unmatched => {}
                decided => return decided,
            }
        }
    }

    Condition::Unmatched
}

fn exported_entry(exports: &Json, subpath: &str, loader: Loader) -> Option<String> {
    let key = if subpath.is_empty() {
        ".".to_owned()
    } else {
        format!("./{subpath}")
    };

    let (value, matched) = match exports {
        Json::Object(map) if map.keys().any(|key| key.starts_with('.')) => {
            if !map.keys().all(|key| key.starts_with('.')) {
                return None;
            }

            match map.get(&key) {
                Some(value) => (value, None),
                None => {
                    let (value, matched) = pattern(map, &key)?;
                    (value, Some(matched))
                }
            }
        }
        _ if subpath.is_empty() => (exports, None),
        _ => return None,
    };

    let Condition::Target(entry) = condition(value, loader) else {
        return None;
    };

    let entry = matched.map_or_else(|| entry.clone(), |matched| entry.replace('*', matched));
    valid_target(&entry).then_some(entry)
}

fn pattern<'a>(map: &'a Map<String, Json>, key: &'a str) -> Option<(&'a Json, &'a str)> {
    map.iter()
        .filter_map(|(pattern, value)| {
            let (prefix, trailer) = pattern.split_once('*')?;
            let fits = !trailer.contains('*')
                && key.len() >= pattern.len()
                && key.starts_with(prefix)
                && key.ends_with(trailer);

            fits.then(|| {
                let matched = &key[prefix.len()..key.len() - trailer.len()];
                ((prefix.len(), pattern.len()), (value, matched))
            })
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, found)| found)
}

fn valid_target(entry: &str) -> bool {
    !entry.contains('%')
        && entry.strip_prefix("./").is_some_and(|rest| {
            !rest
                .split(['/', '\\'])
                .any(|segment| matches!(segment, "." | ".." | "node_modules"))
        })
}

fn strict_entry(
    root: &str,
    subpath: &str,
    exports: &Json,
    loader: Loader,
    search: Search,
) -> Option<Option<String>> {
    let path = match (exports, search) {
        (Json::Null, Search::CommonJs) => return None,
        (Json::Null, Search::Loose) if subpath.is_empty() => {
            return Some(required_directory(root).flatten());
        }
        (Json::Null, Search::Loose) => node_join(root, subpath),
        _ => match exported_entry(exports, subpath, loader) {
            Some(entry) => node_join(root, &entry),
            None => return Some(None),
        },
    };

    Some(is_file(&path).then_some(path))
}

pub(super) fn manifest(path: &str) -> Option<Json> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(&text)).ok()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Exports {
    Fallback,
    Strict,
}

fn package_entry(
    root: &str,
    subpath: &str,
    loader: Loader,
    search: Search,
    exported: Exports,
) -> Option<Option<String>> {
    let path = join(root, "package.json");
    let Some(manifest) = manifest(&path) else {
        if fs::metadata(&path).is_ok() {
            return Some(None);
        }

        return match search {
            Search::CommonJs => required_search(&node_join(root, subpath)),
            Search::Loose => None,
        };
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

    if exported == Exports::Strict
        && let Some(found) = strict_entry(root, subpath, exports, loader, search)
    {
        return Some(found);
    }

    if let Search::CommonJs = search {
        return match entry {
            Some(entry) => first_required(&node_join(root, &entry), &[""]).or(Some(None)),
            None if exports.is_null() => required_search(&node_join(root, subpath)),
            None => required_search(&node_join(root, subpath)).or(Some(None)),
        };
    }

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
    module_from(specifier, from, loader, Exports::Fallback)
}

pub fn resolve_exported(specifier: &str, from: &str, loader: Loader) -> Option<String> {
    module_from(specifier, from, loader, Exports::Strict)
}

fn module_from(specifier: &str, from: &str, loader: Loader, exported: Exports) -> Option<String> {
    let real = fs::canonicalize(from).ok()?;
    let real = real.to_string_lossy();
    let dir = dirname(&real);
    let search = Search::of(loader, from);

    if specifier.starts_with('.') || is_absolute(specifier) {
        return search.file(&resolve(dirname(from), specifier));
    }

    package_search(specifier, dir, loader, search, exported)
}

pub fn resolve_package(specifier: &str, from_dir: &str, loader: Loader) -> Option<String> {
    let search = Search::of(loader, from_dir);
    package_search(specifier, from_dir, loader, search, Exports::Fallback)
}

fn package_search(
    specifier: &str,
    from_dir: &str,
    loader: Loader,
    search: Search,
    exported: Exports,
) -> Option<String> {
    let mut dir = from_dir;
    let parts: Vec<&str> = specifier.split('/').collect();
    let scoped = usize::from(specifier.starts_with('@')) + 1;
    let name = parts[..scoped.min(parts.len())].join("/");
    let subpath = parts[scoped.min(parts.len())..].join("/");
    loop {
        let root = join(&join(dir, "node_modules"), &name);
        if let Some(found) = package_entry(&root, &subpath, loader, search, exported) {
            return found;
        }

        let parent = dirname(dir);
        if parent == dir {
            return None;
        }

        dir = parent;
    }
}
