use std::collections::HashMap;
use std::fs;
use std::io;
use std::rc::Rc;

use indexmap::IndexSet;

use super::git::{GitVariable, GitVariables, Repositories, ascii_prefix, decoded, run_git_with};
use super::{lstat_if_present, repository_path, within};
use crate::paths::{dirname, is_absolute, join};

fn is_set(value: Option<&str>) -> bool {
    matches!(value, Some("true" | "set"))
}

fn opts_out(value: Option<&str>) -> bool {
    matches!(value, Some("unset" | "false"))
}

pub(super) const SELECTION_ATTRIBUTES: [&str; 2] = ["linguist-generated", "stanza"];

type AttributeValues = HashMap<(String, String), String>;

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

fn index_bytes_may_have_attributes(bytes: &[u8]) -> bool {
    let length = bytes.len() as i64;
    if length < 32 || !ascii_signature(&bytes[..4], b"DIRC") {
        return true;
    }

    let version = read_u32(bytes, 4);
    if version != 2 && version != 3 {
        return true;
    }

    let end_of_entries = length - 20;
    let mut offset: i64 = 12;
    let count = read_u32(bytes, 8);
    for _ in 0..count {
        if offset + 62 > end_of_entries {
            return true;
        }

        let at = offset as usize;
        if read_u32(bytes, at + 24) & 0o170000 == 0o040000 {
            return true;
        }

        let flags = u16::from_be_bytes([bytes[at + 60], bytes[at + 61]]);
        let extended = flags & 0x4000 != 0;
        if extended && version != 3 {
            return true;
        }

        let start = at + 62 + if extended { 2 } else { 0 };
        let Some(end) = bytes
            .get(start..)
            .and_then(|rest| rest.iter().position(|&byte| byte == 0))
            .map(|position| start + position)
        else {
            return true;
        };

        if end as i64 >= end_of_entries {
            return true;
        }

        let name = &bytes[start..end];
        if name == b".gitattributes" || name.ends_with(b"/.gitattributes") {
            return true;
        }

        offset += (end as i64 + 1 - offset + 7) / 8 * 8;
    }

    while offset < end_of_entries {
        if offset + 8 > end_of_entries {
            return true;
        }

        let at = offset as usize;
        if !ascii_signature(&bytes[at..at + 4], b"TREE") {
            return true;
        }

        offset += 8 + i64::from(read_u32(bytes, at + 4));
    }

    offset != end_of_entries
}

fn index_may_have_attributes(root: &str) -> io::Result<bool> {
    let path = join(root, ".git/index");
    let Some(entry) = lstat_if_present(&path)? else {
        return Ok(false);
    };

    if !entry.is_file() {
        return Ok(true);
    }

    Ok(index_bytes_may_have_attributes(&fs::read(&path)?))
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
        if index_may_have_attributes(root)? {
            return Ok(true);
        }

        let mut paths = IndexSet::new();
        paths.extend(sources.iter().cloned());
        paths.insert(join(root, ".git/info/attributes"));
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
    fn read_attributes(
        &self,
        files: &[String],
        root: &str,
        names: &[&str],
    ) -> Result<AttributeValues, String> {
        if files.is_empty() {
            return Ok(AttributeValues::new());
        }

        let selection_only = names.iter().all(|name| SELECTION_ATTRIBUTES.contains(name));
        let variables = self.variables.get(root).map(Rc::as_ref);
        if selection_only && !attributes_may_apply(files, root, names, variables) {
            return Ok(AttributeValues::new());
        }

        let paths: String = files
            .iter()
            .map(|file| format!("{}\0", repository_path(root, file)))
            .collect();

        let mut args = vec!["check-attr"];
        args.extend(names);
        args.extend(["-z", "--stdin"]);

        let result = run_git_with(root, &args, Some(paths.as_bytes()), false)?;
        let output = decoded(&result.output);
        let fields: Vec<&str> = output.split('\0').collect();
        let mut values = AttributeValues::new();
        let mut index = 0;
        while index + 2 < fields.len() {
            values.insert(
                (fields[index].to_owned(), fields[index + 1].to_owned()),
                fields[index + 2].to_owned(),
            );

            index += 3;
        }

        Ok(values)
    }

    pub(super) fn drop_by_attributes(
        &self,
        files: Vec<String>,
        root: &str,
        names: &[&str],
    ) -> Result<(Vec<String>, bool), String> {
        let values = self.read_attributes(&files, root, names)?;
        let attribute = |file: &str, name: &str| {
            values
                .get(&(repository_path(root, file), name.to_owned()))
                .map(String::as_str)
        };

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

    #[test]
    fn an_index_with_attribute_files_or_extensions_is_conservative() {
        fn index(version: u32, names: &[&str], extension: &[u8]) -> Vec<u8> {
            let mut bytes = b"DIRC".to_vec();
            bytes.extend(version.to_be_bytes());
            bytes.extend((names.len() as u32).to_be_bytes());
            for name in names {
                let start = bytes.len();
                bytes.extend([0u8; 24]);
                bytes.extend(0o100644u32.to_be_bytes());
                bytes.extend([0u8; 32]);
                bytes.extend((name.len() as u16).to_be_bytes());
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

        assert!(!index_bytes_may_have_attributes(&index(
            2,
            &["a.ts", "b/c.ts"],
            b""
        )));
        assert!(index_bytes_may_have_attributes(&index(
            2,
            &["b/.gitattributes"],
            b""
        )));
        assert!(index_bytes_may_have_attributes(&index(4, &["a.ts"], b"")));
        assert!(!index_bytes_may_have_attributes(&index(
            2,
            &["a.ts"],
            b"\xd4\xd2\xc5\xc5\0\0\0\0"
        )));
        assert!(!index_bytes_may_have_attributes(&index(
            2,
            &["a.ts"],
            b"TREE\0\0\0\0"
        )));
        assert!(index_bytes_may_have_attributes(&index(
            2,
            &["a.ts"],
            b"link\0\0\0\0"
        )));
    }
}
