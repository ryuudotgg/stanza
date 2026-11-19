use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use indexmap::IndexSet;

use super::git::{
    GitEnv, GitError, GitVariable, GitVariables, Repositories, ascii_prefix, decoded, git_dirs,
};
use super::index::{TrackedAttributes, tracked_attributes};
use super::{lstat_if_present, repository_path, within};
use crate::languages::Candidacy;
use crate::paths::{dirname, is_absolute, join};

pub(super) fn is_set(value: Option<&str>) -> bool {
    matches!(value, Some("true" | "set"))
}

pub(super) fn opts_out(value: Option<&str>) -> bool {
    matches!(value, Some("unset" | "false"))
}

pub(super) fn is_filtered(value: Option<&str>) -> bool {
    !matches!(value.unwrap_or("unset"), "unspecified" | "unset")
}

pub(super) const SELECTION_ATTRIBUTES: [&str; 2] = ["linguist-generated", "stanza"];

pub(super) const CONVERSION_ATTRIBUTES: [&str; 6] = [
    "filter",
    "working-tree-encoding",
    "text",
    "eol",
    "crlf",
    "ident",
];

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

    pub(super) fn drop_excluded(&self, root: &str, candidates: Candidates) -> (Vec<String>, bool) {
        let Candidates {
            taken,
            in_skipped_folder,
        } = candidates;

        let files = taken
            .into_iter()
            .map(|file| (file, false))
            .chain(in_skipped_folder.into_iter().map(|file| (file, true)));

        let mut opted_out = false;
        let mut kept = Vec::new();
        for (file, needs_opt_in) in files {
            let stanza = self.get(root, &file, "stanza");
            if opts_out(stanza) {
                opted_out = true;
            } else if (!needs_opt_in || is_set(stanza))
                && !is_set(self.get(root, &file, "linguist-generated"))
            {
                kept.push(file);
            }
        }

        (kept, opted_out)
    }
}

#[derive(Default)]
pub(super) struct Candidates {
    pub(super) taken: Vec<String>,
    pub(super) in_skipped_folder: Vec<String>,
}

impl Candidates {
    pub(super) fn push(&mut self, file: String, candidacy: Candidacy) {
        match candidacy {
            Candidacy::Yes => self.taken.push(file),
            Candidacy::InSkippedFolder => self.in_skipped_folder.push(file),
            Candidacy::No => {}
        }
    }

    pub(super) fn extend(&mut self, other: Candidates) {
        self.taken.extend(other.taken);
        self.in_skipped_folder.extend(other.in_skipped_folder);
    }

    pub(super) fn all(&self) -> Vec<String> {
        [self.taken.as_slice(), &self.in_skipped_folder].concat()
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
        let mut walked = HashSet::new();
        for file in files {
            let mut directory = dirname(file);
            while walked.insert(directory) {
                if directory != root && !within(root, directory) {
                    return Ok(true);
                }

                paths.insert(join(directory, ".gitattributes"));
                if directory == root {
                    break;
                }

                directory = dirname(directory);
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
    fn may_apply(&self, files: &[String], root: &str, names: &[&str]) -> bool {
        let variables = self.variables.get(root).map(Rc::as_ref);
        let scanned = self.index_attributes.get(root).map(Rc::as_ref);
        attributes_may_apply(files, root, names, variables, scanned)
    }

    pub(super) fn may_opt_in(&mut self, files: &[String], root: &str) -> bool {
        if files.is_empty() {
            return false;
        }

        if !environment_may_affect_attributes() && !self.index_attributes.contains_key(root) {
            let scanned = git_dirs(root).and_then(|dirs| tracked_attributes(&dirs.git_dir).ok());
            if let Some(scanned) = scanned {
                self.index_attributes
                    .insert(root.to_owned(), Rc::new(scanned));
            }
        }

        self.may_apply(files, root, &["stanza"])
    }

    pub(super) fn read_attributes(
        &mut self,
        files: &[String],
        root: &str,
        names: &[&str],
        source: Source,
    ) -> Result<Attributes, String> {
        if files.is_empty() {
            return Ok(Attributes::default());
        }

        let selection_only = names.iter().all(|name| SELECTION_ATTRIBUTES.contains(name));
        if source == Source::Worktree && selection_only && !self.may_apply(files, root, names) {
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

        let result = self
            .git(root, &args, Some(paths.as_bytes()), &GitEnv::Inherited)
            .map_err(GitError::message)?;
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
        &mut self,
        mut candidates: Candidates,
        root: &str,
        names: &[&str],
    ) -> Result<(Vec<String>, bool), String> {
        if !self.may_opt_in(&candidates.in_skipped_folder, root) {
            candidates.in_skipped_folder.clear();
        }

        let values = self.read_attributes(&candidates.all(), root, names, Source::Worktree)?;
        Ok(values.drop_excluded(root, candidates))
    }

    pub(super) fn drop_invalid_by_attributes(
        &mut self,
        paths: Vec<PathBuf>,
        root: &str,
        names: &[&str],
        source: Source,
    ) -> (Vec<PathBuf>, bool) {
        if paths.is_empty() {
            return (paths, false);
        }

        let named = |path: &Path| {
            path.strip_prefix(root)
                .unwrap_or(path)
                .as_os_str()
                .as_bytes()
                .to_vec()
        };

        let input: Vec<u8> = paths
            .iter()
            .flat_map(|path| [named(path), vec![0]].concat())
            .collect();

        let mut args = vec!["check-attr"];
        if source == Source::Index {
            args.push("--cached");
        }

        args.extend(names);
        args.extend(["-z", "--stdin"]);

        let Ok(result) = self.git(root, &args, Some(&input), &GitEnv::Inherited) else {
            return (paths, false);
        };

        let fields: Vec<&[u8]> = result.output.split(|&byte| byte == 0).collect();
        let mut opted_out = HashSet::new();
        let mut generated = HashSet::new();
        for [path, name, value] in fields.as_chunks::<3>().0 {
            let value = std::str::from_utf8(value).ok();
            match *name {
                b"stanza" if opts_out(value) => opted_out.insert(*path),
                b"linguist-generated" if is_set(value) => generated.insert(*path),
                _ => false,
            };
        }

        let opted = paths
            .iter()
            .any(|path| opted_out.contains(named(path).as_slice()));

        let kept = paths
            .into_iter()
            .filter(|path| {
                let path = named(path);
                !opted_out.contains(path.as_slice()) && !generated.contains(path.as_slice())
            })
            .collect();

        (kept, opted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_in_a_skipped_folder_needs_a_set_stanza_attribute() {
        let values = [
            ("taken.ts", "stanza", "unspecified"),
            ("set.ts", "stanza", "set"),
            ("true.ts", "stanza", "true"),
            ("other.ts", "stanza", "yes"),
            ("unset.ts", "stanza", "unset"),
            ("generated.ts", "stanza", "set"),
            ("generated.ts", "linguist-generated", "set"),
        ];

        let attributes = Attributes {
            values: values
                .iter()
                .map(|(file, name, value)| {
                    let key = (format!("dist/{file}"), (*name).to_owned());
                    (key, (*value).to_owned())
                })
                .collect(),
        };

        let file = |name: &str| format!("/r/dist/{name}");
        let candidates = Candidates {
            taken: vec![file("taken.ts")],
            in_skipped_folder: ["set.ts", "true.ts", "other.ts", "unset.ts", "generated.ts"]
                .map(file)
                .to_vec(),
        };

        let expected = ["taken.ts", "set.ts", "true.ts"].map(file).to_vec();
        assert_eq!(attributes.drop_excluded("/r", candidates), (expected, true));
    }
}
