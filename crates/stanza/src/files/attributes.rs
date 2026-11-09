use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::rc::Rc;

use indexmap::IndexSet;

use super::git::{
    GitEnv, GitVariable, GitVariables, Repositories, ascii_prefix, decoded, git_dirs, run_git_with,
};
use super::index::{TrackedAttributes, tracked_attributes};
use super::{lstat_if_present, repository_path, within};
use crate::paths::{dirname, is_absolute, join};

pub(super) fn is_set(value: Option<&str>) -> bool {
    matches!(value, Some("true" | "set"))
}

pub(super) fn opts_out(value: Option<&str>) -> bool {
    matches!(value, Some("unset" | "false"))
}

pub(super) const SELECTION_ATTRIBUTES: [&str; 2] = ["linguist-generated", "stanza"];

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Worktree,
    Index,
}

#[derive(Default)]
pub(super) struct Attributes {
    values: HashMap<(String, String), String>,
}

impl Attributes {
    pub(super) fn get(&self, root: &str, file: &str, name: &str) -> Option<&str> {
        self.values
            .get(&(repository_path(root, file), name.to_owned()))
            .map(String::as_str)
    }
}

fn source_may_set_attributes(path: &str, names: &[&str]) -> io::Result<bool> {
    let Some(entry) = lstat_if_present(path)? else {
        return Ok(false);
    };

    if !entry.is_file() {
        return Ok(true);
    }

    let text = String::from_utf8_lossy(&fs::read(path)?).into_owned();
    Ok(text.contains("[attr]") || names.iter().any(|name| text.contains(name)))
}

const GIT_ATTRIBUTE_FILE_LIMIT: u64 = 100 * 1024 * 1024;

// git var -l prints GIT_EDITOR, GIT_SEQUENCE_EDITOR and GIT_PAGER raw, so a newline could forge a line.
const INERT_GIT_ENVIRONMENT: [&str; 15] = [
    "GIT_EDITOR",
    "GIT_SEQUENCE_EDITOR",
    "GIT_PAGER",
    "GIT_TERMINAL_PROMPT",
    "GIT_ASKPASS",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_SSH_VARIANT",
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
    "GIT_OPTIONAL_LOCKS",
];

fn environment_may_affect_attributes() -> bool {
    std::env::vars_os().any(|(name, value)| {
        let name = name.to_string_lossy();
        name.starts_with("GIT_")
            && (!INERT_GIT_ENVIRONMENT.contains(&name.as_ref())
                || value.to_string_lossy().contains('\n'))
    })
}

fn environment(name: &str) -> Option<String> {
    std::env::var_os(name).map(|value| value.to_string_lossy().into_owned())
}

fn rebuildable_path(path: Option<&str>) -> bool {
    path.is_some_and(|path| {
        (path.starts_with("~/") || !path.starts_with('~')) && !path.contains("%(")
    })
}

fn normalize_as_text(path: &str) -> String {
    path.split('/')
        .enumerate()
        .filter(|&(position, segment)| position == 0 || (!segment.is_empty() && segment != "."))
        .map(|(_, segment)| segment)
        .collect::<Vec<_>>()
        .join("/")
}

fn default_global_attributes() -> Option<String> {
    match environment("XDG_CONFIG_HOME") {
        Some(config_home) if !config_home.is_empty() => {
            Some(format!("{config_home}/git/attributes"))
        }
        _ => environment("HOME").map(|home| format!("{home}/.config/git/attributes")),
    }
}

fn expected_global_attributes(configured: &[Option<&str>]) -> Option<String> {
    if !configured.iter().all(|path| rebuildable_path(*path)) {
        return None;
    }

    let Some(path) = configured.last().copied().flatten() else {
        return default_global_attributes();
    };

    match path.strip_prefix("~/") {
        None => Some(path.to_owned()),
        Some(rest) => environment("HOME").map(|home| format!("{home}/{rest}")),
    }
}

const ATTRIBUTE_VARIABLE_ORDER: [&str; 3] =
    ["GIT_ATTR_SYSTEM", "GIT_ATTR_GLOBAL", "GIT_CONFIG_SYSTEM"];

// Once each, adjacent and in git's order, so neither path value spans or forges a line.
fn attribute_variable_values(entries: &[GitVariable]) -> Option<Vec<Option<&str>>> {
    let sequence: Vec<(usize, &GitVariable)> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| ATTRIBUTE_VARIABLE_ORDER.contains(&entry.key.as_str()))
        .collect();

    let start = sequence.first()?.0;
    let exact = sequence.len() == ATTRIBUTE_VARIABLE_ORDER.len()
        && sequence
            .iter()
            .enumerate()
            .all(|(offset, (position, entry))| {
                entry.key == ATTRIBUTE_VARIABLE_ORDER[offset] && position - offset == start
            });

    exact.then(|| {
        sequence
            .iter()
            .map(|(_, entry)| entry.value.as_deref())
            .collect()
    })
}

fn attribute_source_paths(variables: Option<&GitVariables>) -> Option<Vec<String>> {
    let variables = variables.filter(|variables| variables.readable)?;
    let entries = &variables.entries;
    if entries
        .iter()
        .any(|entry| ascii_prefix(&entry.key, "attr.").is_some())
    {
        return None;
    }

    let values = attribute_variable_values(entries)?;
    let system = values[0]?;
    let global = values[1]?;
    let configured: Vec<Option<&str>> = entries
        .iter()
        .filter(|entry| entry.key.to_lowercase() == "core.attributesfile")
        .map(|entry| entry.value.as_deref())
        .collect();

    // git var collapses .. as text but check-attr opens the raw path, where a symlink resolves first.
    let expected = expected_global_attributes(&configured)?;
    if normalize_as_text(&expected) != global {
        return None;
    }

    let paths = [system, global];
    paths
        .iter()
        .all(|path| is_absolute(path))
        .then(|| paths.map(str::to_owned).to_vec())
}

fn attributes_may_apply(
    files: &[String],
    root: &str,
    names: &[&str],
    variables: Option<&GitVariables>,
    scanned: Option<&Option<Vec<TrackedAttributes>>>,
) -> bool {
    if environment_may_affect_attributes() {
        return true;
    }

    let Some(sources) = attribute_source_paths(variables) else {
        return true;
    };

    let reachable = || -> io::Result<bool> {
        let Some(dirs) = git_dirs(root) else {
            return Ok(true);
        };

        let read;
        let tracked = match scanned {
            Some(scanned) => scanned.as_deref(),
            None => {
                read = tracked_attributes(&dirs.git_dir)?;
                read.as_deref()
            }
        };

        let Some(tracked) = tracked else {
            return Ok(true);
        };

        let mut paths = IndexSet::new();
        paths.extend(sources.iter().cloned());
        paths.insert(join(dirs.common_dir(), "info/attributes"));
        for file in files {
            if !within(root, file) {
                return Ok(true);
            }

            let mut directory = dirname(file);
            loop {
                paths.insert(join(&directory, ".gitattributes"));
                if directory == root {
                    break;
                }

                if !within(root, &directory) {
                    return Ok(true);
                }

                directory = dirname(&directory);
            }
        }

        let folded: HashSet<String> = paths.iter().map(|path| path.to_ascii_lowercase()).collect();
        for entry in tracked {
            let path = join(root, &entry.path);
            if !folded.contains(&path.to_ascii_lowercase()) {
                continue;
            }

            // git ignores a worktree copy that is missing or at least ATTR_MAX_FILE_SIZE and reads the index copy.
            let read_from_worktree = lstat_if_present(&path)?
                .is_some_and(|entry| entry.is_file() && entry.len() < GIT_ATTRIBUTE_FILE_LIMIT);

            if entry.skip_worktree || !read_from_worktree {
                return Ok(true);
            }
        }

        for path in &paths {
            if source_may_set_attributes(path, names)? {
                return Ok(true);
            }
        }

        Ok(false)
    };

    reachable().unwrap_or(true)
}

impl Repositories {
    pub(super) fn read_attributes(
        &self,
        files: &[String],
        root: &str,
        names: &[&str],
        source: Source,
    ) -> Result<Attributes, String> {
        if files.is_empty() {
            return Ok(Attributes::default());
        }

        let selection_only = names.iter().all(|name| SELECTION_ATTRIBUTES.contains(name));
        let variables = self.variables.get(root).map(Rc::as_ref);
        let scanned = self.index_attributes.get(root).map(Rc::as_ref);
        if source == Source::Worktree
            && selection_only
            && !attributes_may_apply(files, root, names, variables, scanned)
        {
            return Ok(Attributes::default());
        }

        let paths: String = files
            .iter()
            .map(|file| format!("{}\0", repository_path(root, file)))
            .collect();

        let mut args = vec!["check-attr"];
        if source == Source::Index {
            args.push("--cached");
        }

        args.extend(names);
        args.extend(["-z", "--stdin"]);

        let result = run_git_with(root, &args, Some(paths.as_bytes()), GitEnv::Inherited)?;
        let output = decoded(&result.output);
        let fields: Vec<&str> = output.split('\0').collect();
        let mut values = HashMap::new();
        let mut index = 0;
        while index + 2 < fields.len() {
            values.insert(
                (fields[index].to_owned(), fields[index + 1].to_owned()),
                fields[index + 2].to_owned(),
            );

            index += 3;
        }

        Ok(Attributes { values })
    }

    pub(super) fn drop_by_attributes(
        &self,
        files: Vec<String>,
        root: &str,
        names: &[&str],
    ) -> Result<(Vec<String>, bool), String> {
        let values = self.read_attributes(&files, root, names, Source::Worktree)?;
        let attribute = |file: &str, name: &str| values.get(root, file, name);

        let mut opted_out = false;
        let kept = files
            .into_iter()
            .filter(|file| {
                if opts_out(attribute(file, "stanza")) {
                    opted_out = true;
                    return false;
                }

                !is_set(attribute(file, "linguist-generated"))
            })
            .collect();

        Ok((kept, opted_out))
    }
}
