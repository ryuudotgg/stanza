use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};

use indexmap::{IndexMap, IndexSet};

use super::attributes::{Source, is_set, opts_out};
use super::diff::{Changed, diff_changes, diff_sections};
use super::git::{
    GitCall, GitEnv, GitError, Location, Repositories, decoded, decoded_owned, git_path, has_git,
    nul_items, run_git, run_git_spawned, run_git_with,
};
use super::{
    PathSelection, exists, file_realpath, lstat, realpath, repository_path, sorted,
    submodule_paths, thrown, within,
};
use crate::languages::is_candidate;
use crate::paths::resolve;
use crate::text::js_trim;

const PATHSPEC_BUDGET: usize = 64 * 1024;

// Smaller than PATHSPEC_BUDGET because it also caps the diff text held at once: a 64 KiB batch over a repository with every file edited held about twice its whole diff.
const HUNK_DIFF_BUDGET: usize = 16 * 1024;

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

struct Batch<'a> {
    pathspecs: Vec<&'a str>,
    members: Vec<usize>,
}

fn batches<'a>(paths: &[&'a str], budget: usize) -> Vec<Batch<'a>> {
    let named: HashSet<&str> = paths.iter().copied().collect();
    let mut selections: IndexMap<&str, Vec<usize>> = IndexMap::new();
    for (index, path) in paths.iter().enumerate() {
        let selector = path
            .match_indices('/')
            .map(|(slash, _)| &path[..slash])
            .find(|ancestor| named.contains(ancestor))
            .unwrap_or(path);

        selections.entry(selector).or_default().push(index);
    }

    let mut batches: Vec<Batch> = Vec::new();
    let mut bytes = 0;
    for (pathspec, members) in selections {
        match batches.last_mut() {
            Some(batch) if bytes + pathspec.len() <= budget => {
                batch.pathspecs.push(pathspec);
                batch.members.extend(members);
            }
            _ => {
                batches.push(Batch {
                    pathspecs: vec![pathspec],
                    members,
                });
                bytes = 0;
            }
        }

        bytes += pathspec.len();
    }

    for batch in &mut batches {
        batch.members.sort_unstable();
    }

    batches
}

fn diff_lines(
    root: &str,
    range: &[&str],
    paths: &[&str],
    env: GitEnv,
    budget: usize,
) -> Result<Vec<Changed>, DiffFailure> {
    let mut lines = vec![Changed::default(); paths.len()];
    for batch in batches(paths, budget) {
        let changed = diff_batch(
            root,
            range,
            &batch.pathspecs,
            batch.members.len(),
            env.clone(),
        )?;
        for (index, changed) in batch.members.into_iter().zip(changed) {
            lines[index] = changed;
        }
    }

    Ok(lines)
}

fn diff_batch(
    root: &str,
    range: &[&str],
    pathspecs: &[&str],
    selected: usize,
    env: GitEnv,
) -> Result<Vec<Changed>, DiffFailure> {
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
    args.extend(pathspecs);

    let output = match run_git_spawned(root, &args, None, env) {
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

    let text = decoded_owned(output);
    let sections = diff_sections(&text);
    if sections.len() != selected {
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
        let mut heads = vec![(&DIFF_HEAD[..], repositories.worktree_diff(root))];
        if scope.untracked {
            heads.push((&UNTRACKED[..], GitEnv::Inherited));
        }

        roots.push(root.as_str());
        for (head, env) in heads {
            calls.push(GitCall {
                cwd: root.clone(),
                args: listing(head, &scope.pathspecs)
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                env,
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
    let diff_env = repositories.worktree_diff(&root);
    let mut changed = repositories.run_git(&root, &listing(&DIFF_HEAD, &pathspecs), diff_env);
    let born = changed.is_ok() || run_git(&root, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok();
    if !born {
        if let Err(error) = run_git(&root, &["symbolic-ref", "-q", "HEAD"]) {
            return Ok(git_errors(error));
        }

        changed = run_git(&root, &listing(&CACHED, &pathspecs));
    }

    let untracked = if scope.untracked || changed.is_err() {
        repositories.run_git(&root, &listing(&UNTRACKED, &pathspecs), GitEnv::Inherited)
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
        match diff_lines(
            &root,
            &["HEAD"],
            &candidates,
            repositories.worktree_diff(&root),
            HUNK_DIFF_BUDGET,
        ) {
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
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let input = format!("{}\n", ids.join("\n"));
    let output = run_git_with(
        root,
        &["cat-file", "--batch"],
        Some(input.as_bytes()),
        GitEnv::Inherited,
    )?
    .output;
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

fn sentinel(length: usize) -> io::Result<String> {
    let mut bytes = vec![0; length / 2];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

// cat-file --batch --filters prints the stored size, not the filtered one, so a request for a missing object closes each blob.
fn read_filtered_batch(
    root: &str,
    tree: Option<&str>,
    requests: &[(&str, &str)],
) -> Result<Vec<Vec<u8>>, String> {
    let Some((first, _)) = requests.first() else {
        return Ok(Vec::new());
    };

    let sentinel = sentinel(first.len()).map_err(|error| error.to_string())?;
    let input: String = requests
        .iter()
        .map(|(id, name)| format!("{id} {name}\n{sentinel}\n"))
        .collect();

    let source = attr_source(tree);
    let mut args: Vec<&str> = source.iter().map(String::as_str).collect();
    args.extend(["cat-file", "--batch", "--filters"]);
    let output = run_git_with(root, &args, Some(input.as_bytes()), GitEnv::Inherited)?.output;

    let close = format!("\n{sentinel} missing\n");
    let mut rest = output.as_slice();
    let mut blobs = Vec::new();
    for (id, _) in requests {
        let unreadable = || format!("git cat-file could not read blob {id}");
        let start = rest
            .strip_prefix(format!("{id} blob ").as_bytes())
            .and_then(|after| after.iter().position(|&byte| byte == b'\n'))
            .map(|end| id.len() + " blob ".len() + end + 1)
            .ok_or_else(unreadable)?;

        let end = rest[start..]
            .windows(close.len())
            .position(|window| window == close.as_bytes())
            .ok_or_else(unreadable)?
            + start;

        blobs.push(rest[start..end].to_vec());
        rest = &rest[end + close.len()..];
    }

    if !rest.is_empty() {
        return Err("git cat-file printed more than the blobs it was asked for".to_owned());
    }

    Ok(blobs)
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

    let requests: Vec<(&str, &str)> = batched
        .iter()
        .map(|(_, id, name)| (*id, name.as_str()))
        .collect();

    let mut read = Vec::new();
    let unread = match read_filtered_batch(root, tree, &requests) {
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
        let lines = diff_lines(&root, &range, &edited, GitEnv::NoDiffOpts, HUNK_DIFF_BUDGET)
            .map_err(DiffFailure::message)?;
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
    use std::os::unix::fs::PermissionsExt;

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

        let framed: Vec<(&str, &str)> = ids
            .iter()
            .zip(names)
            .filter(|(_, name)| batchable(name))
            .map(|(id, name)| (id.as_str(), name))
            .collect();

        let tree = js_trim(&git(&root, &["write-tree"])).to_owned();
        for smudge in ["tr a-zA-Z n-za-mN-ZA-M", "printf grown; cat"] {
            git(&root, &["config", "filter.rot.smudge", smudge]);
            for tree in [None, Some(tree.as_str())] {
                let singles: Vec<Vec<u8>> = blobs
                    .iter()
                    .zip(names)
                    .map(|((_, id), name)| read_filtered_blob(&root, tree, name, id).unwrap())
                    .collect();

                let batched: HashMap<&str, Vec<u8>> =
                    read_filtered_blobs(&root, tree, &blobs, |_| false)
                        .unwrap()
                        .into_iter()
                        .collect();

                let framed_reads = read_filtered_batch(&root, tree, &framed).unwrap();
                let framed_singles: Vec<&Vec<u8>> = singles
                    .iter()
                    .zip(names)
                    .filter(|(_, name)| batchable(name))
                    .map(|(single, _)| single)
                    .collect();

                assert_eq!(framed_reads.iter().collect::<Vec<_>>(), framed_singles);
                assert_eq!(batched.len(), names.len());
                for (((path, id), name), single) in blobs.iter().zip(names).zip(&singles) {
                    let stored = git(&root, &["cat-file", "blob", id]);
                    assert_ne!(single, stored.as_bytes(), "{name:?}");
                    assert_eq!(&batched[path], single, "{name:?}");
                }
            }
        }

        let kept: Vec<&str> = names.into_iter().filter(|name| !batchable(name)).collect();
        assert_eq!(kept, ["a\nb.ts", " lead.ts", "cr.ts\r"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn hunk_diffs_split_by_budget_match_one_call() {
        let dir = std::env::temp_dir().join(format!("stanza-hunk-batches-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let root = realpath(&dir.to_string_lossy()).unwrap();
        for name in ["a.ts", "c.ts", "d.ts", "d.ts.x.ts", "sub/b.ts"] {
            std::fs::write(dir.join(name), "if (a) {\n  b();\n}\nconst c = 1;\n").unwrap();
        }

        git(&root, &["init", "-q"]);
        git(&root, &["add", "-A"]);
        git(
            &root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "init",
            ],
        );
        std::fs::write(dir.join("a.ts"), "if (a) {\n  b();\n}\nconst c = 2;\n").unwrap();
        std::fs::write(dir.join("sub/b.ts"), "const z = 0;\nif (a) {\n  b();\n}\n").unwrap();
        std::fs::write(dir.join("c.ts"), b"if (a) {\n  \xff();\n}\nconst c = 1;\n").unwrap();
        std::fs::write(dir.join("d.ts.x.ts"), "const d = 1;\n").unwrap();
        std::fs::remove_file(dir.join("d.ts")).unwrap();
        std::fs::create_dir(dir.join("d.ts")).unwrap();
        std::fs::write(dir.join("d.ts/e.ts"), "const e = 1;\n").unwrap();
        git(&root, &["add", "-A"]);

        let matches_one_call = |expected: &[&str]| {
            let listed = git(&root, &["diff", "--name-only", "HEAD"]);
            let names: Vec<&str> = listed.lines().collect();
            assert_eq!(names, expected);

            let diff = |budget| diff_lines(&root, &["HEAD"], &names, GitEnv::Inherited, budget);
            let Ok(single) = diff(usize::MAX) else {
                panic!("the single call failed for {names:?}");
            };

            for budget in [1, "a.ts".len() + "c.ts".len()] {
                let Ok(batched) = diff(budget) else {
                    panic!("budget {budget} failed for {names:?}");
                };

                assert_eq!(
                    format!("{batched:?}"),
                    format!("{single:?}"),
                    "budget {budget}"
                );
            }
        };

        matches_one_call(&["a.ts", "c.ts", "d.ts", "d.ts.x.ts", "d.ts/e.ts", "sub/b.ts"]);
        std::fs::write(dir.join(".git/order"), "d.ts/e.ts\n*\n").unwrap();
        git(&root, &["config", "diff.orderFile", ".git/order"]);
        matches_one_call(&["d.ts/e.ts", "a.ts", "c.ts", "d.ts", "d.ts.x.ts", "sub/b.ts"]);

        let shape = |paths: &[&str], budget| {
            batches(paths, budget)
                .into_iter()
                .map(|batch| (batch.pathspecs.join(" "), batch.members))
                .collect::<Vec<_>>()
        };

        let names = ["a.ts", "c.ts", "sub/b.ts"];
        assert_eq!(
            shape(&names, HUNK_DIFF_BUDGET),
            [("a.ts c.ts sub/b.ts".to_owned(), vec![0, 1, 2])]
        );
        assert_eq!(
            shape(&names, 8),
            [
                ("a.ts c.ts".to_owned(), vec![0, 1]),
                ("sub/b.ts".to_owned(), vec![2])
            ]
        );
        assert_eq!(
            shape(&["g.ts/1.ts", "a.ts", "g.ts", "g.ts/2.ts/3.ts"], 1),
            [
                ("g.ts".to_owned(), vec![0, 2, 3]),
                ("a.ts".to_owned(), vec![1])
            ]
        );
        assert!(batches(&[], HUNK_DIFF_BUDGET).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn owned_diff_text_decodes_as_borrowed_text_does() {
        let inputs: [&[u8]; 5] = [
            b"diff --git a/a.ts b/a.ts\n+x\n",
            b"\xef\xbb\xbfdiff\n",
            b"\xef\xbb\xbf\xef\xbb\xbf\xff",
            b"+\xff\xfe bad\n+\xe2\x82 cut",
            b"",
        ];
        for bytes in inputs {
            assert_eq!(decoded_owned(bytes.to_vec()), decoded(bytes), "{bytes:?}");
        }
    }

    fn index_state(index: &str) -> (Vec<u8>, std::time::SystemTime) {
        let modified = std::fs::metadata(index).unwrap().modified().unwrap();
        (std::fs::read(index).unwrap(), modified)
    }

    fn set_modified(path: &str, time: std::time::SystemTime) {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[test]
    fn changed_selection_answers_as_git_diff_head_and_leaves_the_index_alone() {
        let dir = std::env::temp_dir().join(format!("stanza-index-alone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = realpath(&dir.to_string_lossy()).unwrap();
        let path = |name: &str| format!("{root}/{name}");
        let commit = |message: &str| {
            git(
                &root,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-qm",
                    message,
                ],
            )
        };

        git(&root, &["init", "-q"]);
        git(&root, &["config", "core.trustctime", "false"]);
        git(
            &root,
            &["config", "filter.rot.clean", "tr a-zA-Z n-za-mN-ZA-M"],
        );
        git(
            &root,
            &["config", "filter.rot.smudge", "tr a-zA-Z n-za-mN-ZA-M"],
        );

        let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(978_307_200);
        let files = [
            ("content.ts", "export const a = 1;\n"),
            ("staged.ts", "export const b = 1;\n"),
            ("touched.ts", "export const c = 1;\n"),
            ("mode.ts", "export const d = 1;\n"),
            ("filtered.ts", "export const e = 1;\n"),
            ("eol.ts", "export const f = 1;\r\n"),
            ("held.ts", "export const g = 1;\r\n"),
            ("racy.ts", "export const h = 1;\n"),
        ];
        for (name, text) in files {
            std::fs::write(path(name), text).unwrap();
        }

        set_modified(&path("racy.ts"), past);
        git(&root, &["add", "held.ts"]);
        commit("held");
        std::fs::write(
            path(".gitattributes"),
            "held.ts text=auto\neol.ts text eol=crlf\nfiltered.ts filter=rot\n",
        )
        .unwrap();
        git(&root, &["add", "-A"]);
        commit("init");

        std::fs::write(path("content.ts"), "export const a = 2;\n").unwrap();
        std::fs::write(path("staged.ts"), "export const b = 2;\n").unwrap();
        git(&root, &["add", "staged.ts"]);
        let mut mode = std::fs::metadata(path("mode.ts")).unwrap().permissions();
        mode.set_mode(0o755);
        std::fs::set_permissions(path("mode.ts"), mode).unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        for name in ["touched.ts", "filtered.ts", "eol.ts", "held.ts"] {
            set_modified(&path(name), later);
        }

        std::fs::write(path("racy.ts"), "export const H = 1;\n").unwrap();
        set_modified(&path("racy.ts"), past);
        set_modified(&path(".git/index"), past);

        let before = index_state(&path(".git/index"));
        let select = |hunks: bool| {
            let location = Location::Repository(root.clone());
            let selected =
                collect_changed(location, &mut Repositories::default(), None, hunks).unwrap();
            assert!(selected.errors.is_empty(), "{:?}", selected.errors);
            selected.files
        };

        let selected = select(false);
        assert_eq!(select(true), selected);
        assert!(
            index_state(&path(".git/index")) == before,
            "the index changed"
        );

        let reference: Vec<String> = git(&root, &["diff", "--name-only", "HEAD"])
            .lines()
            .map(path)
            .collect();
        assert_eq!(selected, reference);
        assert!(
            index_state(&path(".git/index")) != before,
            "git diff left it alone too"
        );
        assert_eq!(
            selected,
            ["content.ts", "mode.ts", "racy.ts", "staged.ts"].map(path)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn changed_selection_writes_nothing_for_a_pending_split_index_or_a_submodule() {
        let dir = std::env::temp_dir().join(format!("stanza-index-split-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("source")).unwrap();
        let base = realpath(&dir.to_string_lossy()).unwrap();
        let (source, root) = (format!("{base}/source"), format!("{base}/root"));
        let identity = ["-c", "user.name=t", "-c", "user.email=t@t"];
        let commit = |cwd: &str| {
            let mut args = identity.to_vec();
            args.extend(["-c", "commit.gpgsign=false", "commit", "-qm", "init"]);
            git(cwd, &args);
        };

        git(&source, &["init", "-q"]);
        std::fs::write(format!("{source}/s.ts"), "export const s = 1;\n").unwrap();
        git(&source, &["add", "-A"]);
        commit(&source);

        git(&base, &["init", "-q", "root"]);
        std::fs::write(format!("{root}/a.ts"), "export const a = 1;\n").unwrap();
        let mut add = identity.to_vec();
        add.extend(["-c", "protocol.file.allow=always", "submodule", "add", "-q"]);
        add.extend([source.as_str(), "sub"]);
        git(&root, &add);
        git(&root, &["add", "-A"]);
        commit(&root);
        git(&root, &["config", "core.splitIndex", "true"]);

        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        set_modified(&format!("{root}/a.ts"), later);
        std::fs::write(format!("{root}/sub/s.ts"), "export const s = 2;\n").unwrap();

        let listing = |dir: &str| {
            let mut names: Vec<String> = std::fs::read_dir(dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };

        let git_dir = format!("{root}/.git");
        let state = || {
            (
                listing(&git_dir),
                index_state(&format!("{git_dir}/index")),
                index_state(&format!("{git_dir}/modules/sub/index")),
            )
        };

        let before = state();
        let location = Location::Repository(root.clone());
        let selected = collect_changed(location, &mut Repositories::default(), None, true).unwrap();

        assert!(selected.errors.is_empty(), "{:?}", selected.errors);
        assert!(selected.files.is_empty(), "{:?}", selected.files);
        assert_eq!(selected.submodules, [format!("{root}/sub")]);
        assert!(state() == before, ".git changed");

        git(&root, &["diff", "--name-only", "HEAD"]);
        assert!(state() != before, "git diff left .git alone too");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
