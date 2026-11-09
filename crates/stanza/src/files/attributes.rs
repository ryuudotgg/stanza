use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::rc::Rc;

use indexmap::IndexSet;

use super::git::{
    GitEnv, GitVariable, GitVariables, Repositories, ascii_prefix, decoded, git_dirs, run_git_with,
};
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

fn ascii_signature(bytes: &[u8], signature: &[u8]) -> bool {
    bytes
        .iter()
        .map(|byte| byte & 0x7f)
        .eq(signature.iter().copied())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

const GIT_ATTRIBUTE_FILE_LIMIT: u64 = 100 * 1024 * 1024;

struct TrackedAttributes {
    path: String,
    skip_worktree: bool,
}

const INDEX_HEADER: usize = 12;

fn handled_index_header(header: &[u8]) -> bool {
    ascii_signature(&header[..4], b"DIRC") && matches!(read_u32(header, 4), 2 | 3)
}

fn tracked_attributes_in(bytes: &[u8]) -> Option<Vec<TrackedAttributes>> {
    let length = bytes.len() as i64;
    if length < 32 || !handled_index_header(bytes) {
        return None;
    }

    let version = read_u32(bytes, 4);
    let end_of_entries = length - 20;
    let mut offset = INDEX_HEADER as i64;
    let count = read_u32(bytes, 8);
    let mut tracked = Vec::new();
    for _ in 0..count {
        if offset + 62 > end_of_entries {
            return None;
        }

        let at = offset as usize;
        if read_u32(bytes, at + 24) & 0o170000 == 0o040000 {
            return None;
        }

        let flags = u16::from_be_bytes([bytes[at + 60], bytes[at + 61]]);
        let extended = flags & 0x4000 != 0;
        if extended && version != 3 {
            return None;
        }

        let skip_worktree =
            extended && u16::from_be_bytes([bytes[at + 62], bytes[at + 63]]) & 0x4000 != 0;

        let start = at + 62 + if extended { 2 } else { 0 };
        let end = start + bytes.get(start..)?.iter().position(|&byte| byte == 0)?;
        if end as i64 >= end_of_entries {
            return None;
        }

        let name = &bytes[start..end];
        if name == b".gitattributes" || name.ends_with(b"/.gitattributes") {
            tracked.push(TrackedAttributes {
                path: String::from_utf8(name.to_vec()).ok()?,
                skip_worktree,
            });
        }

        offset += (end as i64 + 1 - offset + 7) / 8 * 8;
    }

    while offset < end_of_entries {
        if offset + 8 > end_of_entries {
            return None;
        }

        let at = offset as usize;
        if !ascii_signature(&bytes[at..at + 4], b"TREE") {
            return None;
        }

        offset += 8 + i64::from(read_u32(bytes, at + 4));
    }

    (offset == end_of_entries).then_some(tracked)
}

fn read_tracked_attributes(mut index: impl Read) -> io::Result<Option<Vec<TrackedAttributes>>> {
    let mut bytes = vec![0; INDEX_HEADER];
    match index.read_exact(&mut bytes) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        result => result?,
    }

    if !handled_index_header(&bytes) {
        return Ok(None);
    }

    index.read_to_end(&mut bytes)?;
    Ok(tracked_attributes_in(&bytes))
}

fn index_tracked_attributes(git_dir: &str) -> io::Result<Option<Vec<TrackedAttributes>>> {
    let path = join(git_dir, "index");
    let Some(entry) = lstat_if_present(&path)? else {
        return Ok(Some(Vec::new()));
    };

    if !entry.is_file() {
        return Ok(None);
    }

    read_tracked_attributes(fs::File::open(&path)?)
}

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

        let Some(tracked) = index_tracked_attributes(&dirs.git_dir)? else {
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
        for entry in &tracked {
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
        if source == Source::Worktree
            && selection_only
            && !attributes_may_apply(files, root, names, variables)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn index(version: u32, names: &[(&str, u16)], extension: &[u8]) -> Vec<u8> {
        let mut bytes = b"DIRC".to_vec();
        bytes.extend(version.to_be_bytes());
        bytes.extend((names.len() as u32).to_be_bytes());
        for &(name, extended_flags) in names {
            let start = bytes.len();
            let extended = if extended_flags == 0 { 0 } else { 0x4000 };
            bytes.extend([0u8; 24]);
            bytes.extend(0o100644u32.to_be_bytes());
            bytes.extend([0u8; 32]);
            bytes.extend((extended | name.len() as u16).to_be_bytes());
            if extended_flags != 0 {
                bytes.extend(extended_flags.to_be_bytes());
            }

            bytes.extend(name.as_bytes());
            bytes.push(0);
            while !(bytes.len() - start).is_multiple_of(8) {
                bytes.push(0);
            }
        }

        bytes.extend(extension);
        bytes.extend([0u8; 20]);
        bytes
    }

    fn tracked(bytes: &[u8]) -> Option<Vec<(String, bool)>> {
        let entries = read_tracked_attributes(bytes).expect("in memory read");
        entries.map(|entries| {
            entries
                .into_iter()
                .map(|entry| (entry.path, entry.skip_worktree))
                .collect()
        })
    }

    #[test]
    fn an_index_lists_its_attribute_files_and_is_conservative_on_extensions() {
        let no_attributes = Some(Vec::new());
        assert_eq!(
            tracked(&index(2, &[("a.ts", 0), ("b/c.ts", 0)], b"")),
            no_attributes
        );

        assert_eq!(
            tracked(&index(2, &[("b/.gitattributes", 0), ("b/c.ts", 0)], b"")),
            Some(vec![("b/.gitattributes".to_owned(), false)])
        );

        assert_eq!(
            tracked(&index(
                3,
                &[(".gitattributes", 0x4000), ("x/.gitattributes", 0x2000)],
                b""
            )),
            Some(vec![
                (".gitattributes".to_owned(), true),
                ("x/.gitattributes".to_owned(), false),
            ])
        );

        assert_eq!(tracked(&index(2, &[("a.ts", 0x4000)], b"")), None);
        assert_eq!(tracked(&index(4, &[("a.ts", 0)], b"")), None);
        assert_eq!(
            tracked(&index(2, &[("a.ts", 0)], b"\xd4\xd2\xc5\xc5\0\0\0\0")),
            no_attributes
        );

        assert_eq!(
            tracked(&index(2, &[("a.ts", 0)], b"TREE\0\0\0\0")),
            no_attributes
        );

        assert_eq!(tracked(&index(2, &[("a.ts", 0)], b"link\0\0\0\0")), None);
        assert_eq!(tracked(b"DIRC"), None);
    }

    #[test]
    fn an_unhandled_index_version_stops_after_the_header() {
        let mut bytes = index(4, &[("a.ts", 0)], b"");
        bytes.resize(1 << 20, 0);
        let mut reader = io::Cursor::new(bytes);
        assert!(read_tracked_attributes(&mut reader).unwrap().is_none());
        assert_eq!(reader.position(), INDEX_HEADER as u64);

        let mut reader = io::Cursor::new(index(2, &[("a.ts", 0)], b""));
        assert!(read_tracked_attributes(&mut reader).unwrap().is_some());
        assert_eq!(reader.position(), reader.get_ref().len() as u64);
    }
}
