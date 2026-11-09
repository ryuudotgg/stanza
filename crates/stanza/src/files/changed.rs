use std::collections::{HashMap, HashSet};

use indexmap::{IndexMap, IndexSet};

use super::attributes::{Source, is_set, opts_out};
use super::git::{
    GitCall, GitEnv, GitError, Location, Repositories, decoded, git_path, has_git, nul_items,
    run_git, run_git_spawned, run_git_with,
};
use super::{
    PathSelection, exists, file_realpath, lstat, realpath, repository_path, sorted,
    submodule_paths, thrown, within,
};
use crate::engine::hunks::{Changed, diff_changes, diff_sections};
use crate::languages::is_candidate;
use crate::paths::resolve;
use crate::text::js_trim;

const PATHSPEC_BUDGET: usize = 64 * 1024;

fn git_errors(error: String) -> PathSelection {
    PathSelection {
        errors: vec![error],
        ..PathSelection::default()
    }
}

enum DiffFailure {
    Thrown(String),
    Git(String),
}

impl DiffFailure {
    fn message(self) -> String {
        match self {
            DiffFailure::Thrown(message) | DiffFailure::Git(message) => message,
        }
    }
}

fn diff_lines(root: &str, range: &[&str], paths: &[&str]) -> Result<Vec<Changed>, DiffFailure> {
    let mut args = vec![
        "-c",
        "diff.suppressBlankEmpty=false",
        "--literal-pathspecs",
        "diff",
        "--unified=999999999",
        "--text",
        "--submodule=short",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--no-renames",
    ];
    args.extend(range);
    args.push("--");
    args.extend(paths);

    let output = match run_git_spawned(root, &args, None, GitEnv::NoDiffOpts) {
        Ok(result) => result.output,
        Err(GitError::Spawn(error)) => {
            return Err(DiffFailure::Thrown(thrown(
                &error,
                "posix_spawn",
                git_path(),
            )));
        }
        Err(GitError::Failed(reason)) => return Err(DiffFailure::Git(reason)),
    };

    let text = decoded(&output);
    let sections = diff_sections(&text);
    if sections.len() != paths.len() {
        return Err(DiffFailure::Git(
            "git diff section count differs from the changed file count".to_owned(),
        ));
    }

    Ok(sections
        .iter()
        .map(|section| diff_changes(section))
        .collect())
}

const DIFF_HEAD: [&str; 8] = [
    "--literal-pathspecs",
    "diff",
    "--name-only",
    "-z",
    "--no-renames",
    "--submodule=short",
    "HEAD",
    "--",
];

const CACHED: [&str; 5] = ["--literal-pathspecs", "ls-files", "-z", "--cached", "--"];

const UNTRACKED: [&str; 6] = [
    "--literal-pathspecs",
    "ls-files",
    "-z",
    "--others",
    "--exclude-standard",
    "--",
];

struct Scope {
    pathspecs: Vec<String>,
    untracked: bool,
}

fn scope(
    repositories: &mut Repositories,
    root: &str,
    real_root: &str,
    written: Option<&IndexSet<String>>,
) -> Scope {
    let Some(written) = written else {
        return Scope {
            pathspecs: Vec::new(),
            untracked: true,
        };
    };

    let paths: Vec<String> = written
        .iter()
        .filter(|path| within(real_root, path))
        .map(|path| repository_path(real_root, path))
        .collect();

    let budgeted = paths.iter().map(String::len).sum::<usize>() <= PATHSPEC_BUDGET;
    if !budgeted {
        return Scope {
            pathspecs: Vec::new(),
            untracked: true,
        };
    }

    let untracked = !repositories.tracks_all(root, &paths);
    Scope {
        pathspecs: paths,
        untracked,
    }
}

fn listing<'a>(head: &[&'a str], pathspecs: &'a [String]) -> Vec<&'a str> {
    let mut args = head.to_vec();
    args.extend(pathspecs.iter().map(String::as_str));
    args
}

pub fn prefetch_changed(
    groups: &[(Location, Option<IndexSet<String>>)],
    repositories: &mut Repositories,
) {
    let mut roots = Vec::new();
    let mut calls = Vec::new();
    for (location, written) in groups {
        let Location::Repository(root) = location else {
            continue;
        };

        let Ok(real_root) = realpath(root) else {
            continue;
        };

        let scope = scope(repositories, root, &real_root, written.as_ref());
        let heads: &[&[&str]] = if scope.untracked {
            &[&DIFF_HEAD, &UNTRACKED]
        } else {
            &[&DIFF_HEAD]
        };

        roots.push(root.as_str());
        for head in heads {
            calls.push(GitCall {
                cwd: root.clone(),
                args: listing(head, &scope.pathspecs)
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            });
        }
    }

    repositories.prefetch(&roots, calls);
}

pub fn collect_changed(
    location: Location,
    repositories: &mut Repositories,
    written: Option<&IndexSet<String>>,
    hunks: bool,
) -> Result<PathSelection, String> {
    if !has_git() {
        return Ok(git_errors(
            "--changed needs git, which was not found on PATH".to_owned(),
        ));
    }

    let root = match location {
        Location::Repository(root) => root,
        Location::Failed(error) => return Ok(git_errors(error)),
        Location::Outside => return Ok(git_errors("not inside a git repository".to_owned())),
    };

    let real_root = realpath(&root).map_err(|error| thrown(&error, "realpath", &root))?;
    let scope = scope(repositories, &root, &real_root, written);
    let pathspecs = scope.pathspecs;
    let mut changed = repositories.run_git(&root, &listing(&DIFF_HEAD, &pathspecs));
    let born = changed.is_ok() || run_git(&root, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok();
    if !born {
        if let Err(error) = run_git(&root, &["symbolic-ref", "-q", "HEAD"]) {
            return Ok(git_errors(error));
        }

        changed = run_git(&root, &listing(&CACHED, &pathspecs));
    }

    let untracked = if scope.untracked || changed.is_err() {
        repositories.run_git(&root, &listing(&UNTRACKED, &pathspecs))
    } else {
        Ok(String::new())
    };

    let (changed, untracked) = match (changed, untracked) {
        (Ok(changed), Ok(untracked)) => (changed, untracked),
        (changed, untracked) => {
            let errors = [changed, untracked]
                .into_iter()
                .filter_map(Result::err)
                .collect();

            return Ok(PathSelection {
                errors: super::unique(errors),
                ..PathSelection::default()
            });
        }
    };

    let kept = |name: &&str| written.is_none_or(|set| set.contains(&resolve(&real_root, name)));
    let names: Vec<&str> = nul_items(&changed).filter(kept).collect();
    let candidates: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| is_candidate(name, false))
        .collect();

    let mut changed_lines = HashMap::new();
    if hunks && born && !candidates.is_empty() {
        match diff_lines(&root, &["HEAD"], &candidates) {
            Ok(lines) => {
                changed_lines.extend(
                    candidates
                        .iter()
                        .map(|name| resolve(&root, name))
                        .zip(lines),
                );
            }
            Err(DiffFailure::Thrown(error)) => return Err(error),
            Err(DiffFailure::Git(error)) => return Ok(git_errors(error)),
        }
    }

    let untracked_names: Vec<&str> = nul_items(&untracked).filter(kept).collect();
    for name in &untracked_names {
        changed_lines.remove(&resolve(&root, name));
    }

    let mut files = Vec::new();
    for name in names.iter().chain(&untracked_names).copied() {
        if !is_candidate(name, false) {
            continue;
        }

        let path = resolve(&root, name);
        if exists(&path)
            && lstat(&path).is_ok_and(|meta| meta.is_file())
            && within(&real_root, &file_realpath(&path)?)
        {
            files.push(path);
        }
    }

    let gitlinks = if written.is_none() {
        submodule_paths(&root)
    } else {
        HashSet::new()
    };

    let submodules = names
        .iter()
        .map(|name| resolve(&root, name))
        .filter(|path| gitlinks.contains(path))
        .collect();

    match repositories.drop_by_attributes(files, &root, &super::attributes::SELECTION_ATTRIBUTES) {
        Ok((files, _)) => Ok(PathSelection {
            files: sorted(files),
            submodules: sorted(submodules),
            changed_lines,
            ..PathSelection::default()
        }),
        Err(error) => Ok(git_errors(error)),
    }
}

pub struct StagedFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub unstaged: bool,
    pub lines: Option<Changed>,
}

pub struct Staged {
    pub files: Vec<StagedFile>,
    pub submodules: Vec<String>,
}

const REGULAR_FILE_MODES: [&str; 2] = ["100644", "100755"];

fn read_blobs(root: &str, ids: &[&str]) -> Result<Vec<Vec<u8>>, String> {
    let input: String = ids.iter().map(|id| format!("{id}\n")).collect();
    read_batch(root, &["cat-file", "--batch"], ids, &input)
}

fn read_batch(
    root: &str,
    args: &[&str],
    ids: &[&str],
    input: &str,
) -> Result<Vec<Vec<u8>>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let output = run_git_with(root, args, Some(input.as_bytes()), GitEnv::Inherited)?.output;
    let mut blobs = Vec::new();
    let mut offset = 0;
    for id in ids {
        let unreadable = || format!("git cat-file could not read blob {id}");
        let end = output
            .get(offset..)
            .and_then(|rest| rest.iter().position(|&byte| byte == b'\n'))
            .map(|position| offset + position);

        let header = end
            .map(|end| decoded(&output[offset..end]))
            .unwrap_or_default();
        let fields: Vec<&str> = header.split(' ').collect();
        let (Some(end), Some(&"blob")) = (end, fields.get(1)) else {
            return Err(unreadable());
        };

        let size: usize = fields
            .get(2)
            .and_then(|size| size.parse().ok())
            .ok_or_else(unreadable)?;

        let start = end + 1;
        let stop = (start + size).min(output.len());
        blobs.push(output[start..stop].to_vec());
        offset = start + size + 1;
    }

    Ok(blobs)
}

fn attr_source(tree: Option<&str>) -> Option<String> {
    tree.map(|tree| format!("--attr-source={tree}"))
}

fn read_filtered_blob(
    root: &str,
    tree: Option<&str>,
    name: &str,
    id: &str,
) -> Result<Vec<u8>, String> {
    let source = attr_source(tree);
    let named = format!("--path={name}");
    let mut args: Vec<&str> = source.iter().map(String::as_str).collect();
    args.extend(["cat-file", "--filters", &named, id]);
    Ok(run_git_with(root, &args, None, GitEnv::Inherited)?.output)
}

// cat-file --batch reads one line per blob, strips a trailing CR, and skips the whitespace before the path.
fn batchable(name: &str) -> bool {
    !name.contains('\n') && !name.ends_with('\r') && !name.starts_with([' ', '\t'])
}

fn read_filtered_blobs<'a>(
    root: &str,
    tree: Option<&str>,
    blobs: &[(&'a str, &'a str)],
    own_process: impl Fn(&str) -> bool,
) -> Result<Vec<(&'a str, Vec<u8>)>, String> {
    let named: Vec<(&str, &str, String)> = blobs
        .iter()
        .map(|&(path, id)| (path, id, repository_path(root, path)))
        .collect();

    let (batched, single): (Vec<_>, Vec<_>) = named
        .iter()
        .partition(|(path, _, name)| batchable(name) && !own_process(path));

    let source = attr_source(tree);
    let mut args: Vec<&str> = source.iter().map(String::as_str).collect();
    args.extend(["cat-file", "--batch", "--filters"]);
    let ids: Vec<&str> = batched.iter().map(|(_, id, _)| *id).collect();
    let input: String = batched
        .iter()
        .map(|(_, id, name)| format!("{id} {name}\n"))
        .collect();

    let mut read = Vec::new();
    let unread = match read_batch(root, &args, &ids, &input) {
        Ok(contents) => {
            read.extend(batched.iter().map(|(path, ..)| *path).zip(contents));
            single
        }
        Err(_) => named.iter().collect(),
    };

    for (path, id, name) in unread {
        read.push((*path, read_filtered_blob(root, tree, name, id)?));
    }

    Ok(read)
}

pub fn collect_staged(
    cwd: &str,
    repositories: &mut Repositories,
    hunks: bool,
) -> Result<Staged, String> {
    if !has_git() {
        return Err("--staged needs git, which was not found on PATH".to_owned());
    }

    let root = match repositories.locate(cwd) {
        Location::Repository(root) => root,
        Location::Failed(error) => return Err(error),
        Location::Outside => return Err("not inside a git repository".to_owned()),
    };

    let range = ["--cached", "--diff-filter=ACMT"];
    let mut args = vec!["diff"];
    args.extend(range);
    args.extend(["--raw", "-z", "--no-renames", "--no-abbrev"]);
    let diff = run_git(&root, &args)?;

    let items: Vec<&str> = nul_items(&diff).collect();
    let mut blob_of: IndexMap<String, String> = IndexMap::new();
    let mut edited = Vec::new();
    let mut submodules = Vec::new();
    for [header, name] in items.as_chunks::<2>().0 {
        let name = *name;
        let fields: Vec<&str> = header.split(' ').collect();
        let (mode, blob) = (fields.get(1).copied(), fields.get(3).copied());
        if mode == Some("160000") {
            submodules.push(resolve(&root, name));
        }

        let (Some(mode), Some(blob)) = (mode, blob) else {
            continue;
        };

        if REGULAR_FILE_MODES.contains(&mode) && is_candidate(name, false) {
            blob_of.insert(resolve(&root, name), blob.to_owned());
            if fields.get(4) == Some(&"M") {
                edited.push(name);
            }
        }
    }

    let mut lines_of: HashMap<String, Changed> = HashMap::new();
    if hunks && !edited.is_empty() {
        let lines = diff_lines(&root, &range, &edited).map_err(DiffFailure::message)?;
        lines_of.extend(edited.iter().map(|name| resolve(&root, name)).zip(lines));
    }

    let staged: Vec<String> = blob_of.keys().cloned().collect();
    let attributes = repositories.read_attributes(
        &staged,
        &root,
        &["linguist-generated", "stanza", "filter"],
        Source::Index,
    )?;

    let unstaged = run_git(&root, &["diff", "--name-only", "-z", "--no-renames"])?;
    let dirty: HashSet<String> = nul_items(&unstaged)
        .map(|name| resolve(&root, name))
        .collect();

    let indexed: Vec<String> = sorted(staged)
        .into_iter()
        .filter(|path| {
            !is_set(attributes.get(&root, path, "linguist-generated"))
                && !opts_out(attributes.get(&root, path, "stanza"))
        })
        .collect();

    // --fix reads the worktree's attributes, so an opt out there must also keep --staged quiet.
    let worktree_opt_out =
        repositories.read_attributes(&indexed, &root, &["stanza"], Source::Worktree)?;

    let paths: Vec<String> = indexed
        .into_iter()
        .filter(|path| !opts_out(worktree_opt_out.get(&root, path, "stanza")))
        .collect();

    let filtered = |path: &str| {
        !matches!(
            attributes.get(&root, path, "filter").unwrap_or("unset"),
            "unspecified" | "unset"
        )
    };

    let (smudge, plain): (Vec<&String>, Vec<&String>) =
        paths.iter().partition(|path| filtered(path));

    let plain_ids: Vec<&str> = plain.iter().map(|path| blob_of[*path].as_str()).collect();
    let mut bytes: IndexMap<&str, Vec<u8>> = plain
        .iter()
        .map(|path| path.as_str())
        .zip(read_blobs(&root, &plain_ids)?)
        .collect();

    let smudge_paths: Vec<String> = smudge.iter().map(|path| (*path).clone()).collect();
    let worktree =
        repositories.read_attributes(&smudge_paths, &root, &["filter"], Source::Worktree)?;

    let diverged = smudge
        .iter()
        .any(|path| worktree.get(&root, path, "filter") != attributes.get(&root, path, "filter"));

    let tree = if diverged {
        Some(js_trim(&run_git(&root, &["write-tree"])?).to_owned())
    } else {
        None
    };

    let smudge_blobs: Vec<(&str, &str)> = smudge
        .iter()
        .map(|path| (path.as_str(), blob_of[*path].as_str()))
        .collect();

    // A process filter that answers abort is off for the rest of its git process, so one batch would leave later blobs unfiltered.
    let process_filters = (!smudge.is_empty())
        .then(|| repositories.process_filters(&root))
        .flatten();

    let own_process = |path: &str| {
        process_filters.as_ref().is_none_or(|drivers| {
            attributes
                .get(&root, path, "filter")
                .is_some_and(|driver| drivers.contains(driver))
        })
    };

    bytes.extend(read_filtered_blobs(
        &root,
        tree.as_deref(),
        &smudge_blobs,
        own_process,
    )?);

    let files = paths
        .iter()
        .map(|path| StagedFile {
            bytes: bytes.swap_remove(path.as_str()).unwrap_or_default(),
            unstaged: dirty.contains(path),
            lines: if filtered(path) {
                None
            } else {
                lines_of.remove(path)
            },
            path: path.clone(),
        })
        .collect();

    Ok(Staged {
        files,
        submodules: sorted(submodules),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &str, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();

        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn batched_filter_reads_match_per_file_reads() {
        let dir = std::env::temp_dir().join(format!("stanza-filter-batch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = realpath(&dir.to_string_lossy()).unwrap();
        git(&root, &["init", "-q"]);
        git(
            &root,
            &["config", "filter.rot.clean", "tr a-zA-Z n-za-mN-ZA-M"],
        );
        git(
            &root,
            &["config", "filter.rot.smudge", "tr a-zA-Z n-za-mN-ZA-M"],
        );
        std::fs::write(
            dir.join(".gitattributes"),
            "* filter=rot\n.gitattributes -filter\n",
        )
        .unwrap();

        let names = [
            "a.ts",
            "sub/b.ts",
            "two  spaces.ts",
            "a\nb.ts",
            " lead.ts",
            "cr.ts\r",
            "c.ts",
        ];
        for (index, name) in names.iter().enumerate() {
            let path = dir.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!("export const Value{index} = \"{name}\";\r\n\0end\n"),
            )
            .unwrap();
        }

        git(&root, &["add", "-A"]);
        let paths: Vec<String> = names.iter().map(|name| resolve(&root, name)).collect();
        let ids: Vec<String> = names
            .iter()
            .map(|name| js_trim(&git(&root, &["rev-parse", &format!(":{name}")])).to_owned())
            .collect();

        let blobs: Vec<(&str, &str)> = paths
            .iter()
            .map(String::as_str)
            .zip(ids.iter().map(String::as_str))
            .collect();

        for (path, id) in &blobs {
            let stored = git(&root, &["cat-file", "blob", id]);
            assert_ne!(stored.as_bytes(), std::fs::read(path).unwrap());
        }

        for tree in [None, Some(js_trim(&git(&root, &["write-tree"])).to_owned())] {
            let batched: HashMap<&str, Vec<u8>> =
                read_filtered_blobs(&root, tree.as_deref(), &blobs, |_| false)
                    .unwrap()
                    .into_iter()
                    .collect();

            assert_eq!(batched.len(), names.len());
            for ((path, id), name) in blobs.iter().zip(names) {
                let single = read_filtered_blob(&root, tree.as_deref(), name, id).unwrap();
                assert_eq!(batched[path], single, "{name:?}");
                assert_eq!(single, std::fs::read(path).unwrap(), "{name:?}");
            }
        }

        let kept: Vec<&str> = names.into_iter().filter(|name| !batchable(name)).collect();
        assert_eq!(kept, ["a\nb.ts", " lead.ts", "cr.ts\r"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
