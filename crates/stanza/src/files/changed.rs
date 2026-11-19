use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};

use super::attributes::{
    CONVERSION_ATTRIBUTES, Candidates, SELECTION_ATTRIBUTES, Source, is_filtered, is_set, opts_out,
};
use super::diff::{Changed, diff_changes, diff_sections};
use super::git::{
    GitCall, GitEnv, GitError, GitOutput, Location, Repositories, SUBMODULE_KEYS, decoded,
    decoded_owned, git_succeeds, has_git, has_gitmodules, nul_names,
};
use super::{
    FolderLinks, PathSelection, failure_lines, file_realpath, invalid_candidates, invalid_files,
    lstat, missing, opted_in_folder, realpath, repository_path, sorted, unread_by_git,
    unreadable_entry, unresolved, within,
};
use crate::languages::{Candidacy, candidacy, prunes};
use crate::paths::{dirname, resolve};

const PATHSPEC_BUDGET: usize = 64 * 1024;

// Smaller than PATHSPEC_BUDGET because it also caps the diff text held at once: a 64 KiB batch over a repository with every file edited held about twice its whole diff.
const HUNK_DIFF_BUDGET: usize = 16 * 1024;

const HUNK_ATTRIBUTES: [&str; 3] = ["linguist-generated", "stanza", "filter"];

fn git_errors(error: String) -> PathSelection {
    PathSelection {
        errors: vec![error],
        ..PathSelection::default()
    }
}

struct Batch<'a> {
    pathspecs: Vec<&'a str>,
    members: Vec<usize>,
}

fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(|(slash, _)| &path[..slash])
}

fn batches<'a>(paths: &[&'a str], budget: usize) -> Vec<Batch<'a>> {
    let named: HashSet<&str> = paths.iter().copied().collect();
    let mut selections: IndexMap<&str, Vec<usize>> = IndexMap::new();
    for (index, path) in paths.iter().enumerate() {
        let selector = ancestors(path)
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
    repositories: &mut Repositories,
    root: &str,
    range: &[&str],
    paths: &[&str],
    env: &GitEnv,
    budget: usize,
) -> Result<Vec<Changed>, GitError> {
    let mut lines = vec![Changed::default(); paths.len()];
    for batch in batches(paths, budget) {
        let changed = diff_batch(
            repositories,
            root,
            range,
            &batch.pathspecs,
            batch.members.len(),
            env,
        )?;
        for (index, changed) in batch.members.into_iter().zip(changed) {
            lines[index] = changed;
        }
    }

    Ok(lines)
}

fn diff_batch(
    repositories: &mut Repositories,
    root: &str,
    range: &[&str],
    pathspecs: &[&str],
    selected: usize,
    env: &GitEnv,
) -> Result<Vec<Changed>, GitError> {
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

    let output = repositories.git(root, &args, None, env)?.output;
    let text = decoded_owned(output);
    let sections = diff_sections(&text);
    if sections.len() != selected {
        return Err(GitError::Failed(
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

const INDEXED_OR_UNTRACKED: [&str; 7] = [
    "--literal-pathspecs",
    "ls-files",
    "-z",
    "--cached",
    "--others",
    "--exclude-standard",
    "--",
];

enum Listing {
    Changed { untracked: bool },
    Indexed { untracked: bool },
}

struct Scope {
    pathspecs: Vec<String>,
    listing: Listing,
}

fn scope(
    repositories: &mut Repositories,
    root: &str,
    real_root: &str,
    written: Option<&IndexSet<String>>,
    hunks: bool,
) -> Scope {
    let Some(written) = written else {
        return Scope {
            pathspecs: Vec::new(),
            listing: Listing::Changed { untracked: true },
        };
    };

    let paths: Vec<String> = written
        .iter()
        .filter(|path| within(real_root, path))
        .map(|path| repository_path(real_root, path))
        .collect();

    let budgeted = paths.iter().map(String::len).sum::<usize>() <= PATHSPEC_BUDGET;
    let pathspecs = if budgeted { paths } else { Vec::new() };
    let untracked = !budgeted || !repositories.tracks_all(root, &pathspecs);
    let listing = if hunks {
        Listing::Changed { untracked }
    } else {
        Listing::Indexed { untracked }
    };

    Scope { pathspecs, listing }
}

fn listing<'a>(head: &[&'a str], pathspecs: &'a [String]) -> Vec<&'a str> {
    let mut args = head.to_vec();
    args.extend(pathspecs.iter().map(String::as_str));
    args
}

#[derive(Clone, Copy)]
pub enum Caller<'a> {
    Cli(&'a str),
    Hook,
}

pub fn prefetch_changed(
    groups: &[(Location, Option<IndexSet<String>>)],
    caller: Caller,
    hunks: bool,
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

        let scope = scope(repositories, root, &real_root, written.as_ref(), hunks);
        let heads = match scope.listing {
            Listing::Indexed { untracked } => vec![(indexed_head(untracked), GitEnv::CLocale)],
            Listing::Changed { untracked } => {
                let mut heads = vec![(&DIFF_HEAD[..], repositories.worktree_diff(root))];
                if untracked {
                    heads.push((&UNTRACKED[..], GitEnv::CLocale));
                }

                heads
            }
        };

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

        if matches!(caller, Caller::Cli(_)) && written.is_none() && has_gitmodules(root) {
            calls.push(GitCall {
                cwd: root.clone(),
                args: SUBMODULE_KEYS.map(str::to_owned).to_vec(),
                env: GitEnv::Inherited,
            });
        }
    }

    repositories.prefetch(&roots, calls);
}

fn indexed_head(untracked: bool) -> &'static [&'static str] {
    if untracked {
        &INDEXED_OR_UNTRACKED
    } else {
        &CACHED
    }
}

struct Listed {
    changed: Vec<u8>,
    whole: GitOutput,
    diffs_head: bool,
}

fn list_names(
    repositories: &mut Repositories,
    root: &str,
    scope: &Scope,
) -> Result<Listed, Vec<String>> {
    let pathspecs = &scope.pathspecs;
    let untracked = match scope.listing {
        Listing::Changed { untracked } => untracked,
        Listing::Indexed { untracked } => {
            let head = indexed_head(untracked);
            return repositories
                .git(root, &listing(head, pathspecs), None, &GitEnv::CLocale)
                .map(|whole| Listed {
                    changed: Vec::new(),
                    whole,
                    diffs_head: false,
                })
                .map_err(|error| vec![error.message()]);
        }
    };

    let diff_env = repositories.worktree_diff(root);
    let mut changed = repositories.git(root, &listing(&DIFF_HEAD, pathspecs), None, &diff_env);
    let born = changed.is_ok() || git_succeeds(root, &["rev-parse", "--verify", "-q", "HEAD"]);
    if !born {
        repositories
            .git(
                root,
                &["symbolic-ref", "-q", "HEAD"],
                None,
                &GitEnv::Inherited,
            )
            .map_err(|error| vec![error.message()])?;

        changed = repositories.git(root, &listing(&CACHED, pathspecs), None, &GitEnv::Inherited);
    }

    let whole = if untracked || changed.is_err() {
        repositories.git(
            root,
            &listing(&UNTRACKED, pathspecs),
            None,
            &GitEnv::CLocale,
        )
    } else {
        Ok(GitOutput::default())
    };

    match (changed, whole) {
        (Ok(changed), Ok(whole)) => Ok(Listed {
            changed: changed.output,
            whole,
            diffs_head: born,
        }),
        (changed, whole) => Err([changed.err(), whole.err()]
            .into_iter()
            .flatten()
            .map(GitError::message)
            .collect()),
    }
}

pub fn collect_changed(
    location: Location,
    repositories: &mut Repositories,
    written: Option<&IndexSet<String>>,
    caller: Caller,
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

    let shown_from = match caller {
        Caller::Cli(cwd) => Some(cwd),
        Caller::Hook => None,
    };

    let real_root = realpath(&root).map_err(|error| unresolved(shown_from, &root, &error))?;
    let scope = scope(repositories, &root, &real_root, written, hunks);
    let listed = match list_names(repositories, &root, &scope) {
        Ok(listed) => listed,
        Err(errors) => {
            return Ok(PathSelection {
                errors: super::unique(errors),
                ..PathSelection::default()
            });
        }
    };

    let mut failures = match caller {
        Caller::Cli(cwd) => {
            unread_by_git(&listed.whole.warnings, &root, &root, &root, cwd, |below| {
                prunes(below) && !opted_in_folder(repositories, &root, &resolve(&root, below))
            })
        }
        Caller::Hook => Vec::new(),
    };

    let kept = |name: &&str| written.is_none_or(|set| set.contains(&resolve(&real_root, name)));
    let changed = nul_names(&listed.changed);
    let whole = nul_names(&listed.whole.output);
    let mut links = FolderLinks::new(&root);
    let invalid_names = if written.is_none() {
        let invalid: Vec<&[u8]> = changed
            .invalid
            .iter()
            .chain(&whole.invalid)
            .copied()
            .collect();

        repositories
            .drop_invalid_by_attributes(
                invalid_files(Path::new(&root), &invalid, false, &mut links),
                &root,
                &SELECTION_ATTRIBUTES,
                Source::Worktree,
            )
            .kept
    } else {
        Vec::new()
    };

    let names: Vec<&str> = changed.names.into_iter().filter(kept).collect();
    let whole_names: Vec<&str> = whole.names.into_iter().filter(kept).collect();
    let in_skipped_folder: Vec<String> = names
        .iter()
        .chain(&whole_names)
        .filter(|name| candidacy(name, false) == Candidacy::InSkippedFolder)
        .map(|name| resolve(&root, name))
        .collect();

    let opt_in = repositories.may_opt_in(&in_skipped_folder, &root);
    let selectable = |name: &str| match candidacy(name, false) {
        Candidacy::InSkippedFolder if !opt_in => Candidacy::No,
        candidacy => candidacy,
    };

    let candidates: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| selectable(name) != Candidacy::No)
        .collect();

    let untracked: HashSet<&str> = whole_names.iter().copied().collect();
    let hunked: Vec<&str> = if hunks && listed.diffs_head {
        candidates
            .into_iter()
            .filter(|name| !untracked.contains(name))
            .collect()
    } else {
        Vec::new()
    };

    let mut files = Candidates::default();
    for name in names.iter().chain(&whole_names).copied() {
        let candidacy = selectable(name);
        if candidacy == Candidacy::No {
            continue;
        }

        let path = resolve(&root, name);
        if links.through_link(dirname(&path)) {
            continue;
        }

        match (lstat(&path), caller) {
            (Ok(meta), _)
                if meta.is_file()
                    && within(
                        &real_root,
                        &file_realpath(&path).map_err(|unresolved| unresolved.line(shown_from))?,
                    ) =>
            {
                files.push(path, candidacy);
            }
            (Err(error), Caller::Cli(cwd)) if !missing(&error) => {
                failures.push(unreadable_entry(cwd, &path, &error));
            }
            _ => {}
        }
    }

    let gitlinks = if matches!(caller, Caller::Cli(_)) && written.is_none() {
        repositories.submodule_paths(&root)
    } else {
        Rc::default()
    };

    let skipped = names
        .iter()
        .map(|name| resolve(&root, name))
        .filter(|path| gitlinks.contains(path))
        .collect();

    let names: &[&str] = if hunked.is_empty() {
        &SELECTION_ATTRIBUTES
    } else {
        &HUNK_ATTRIBUTES
    };

    let attributes =
        match repositories.read_attributes(&files.all(), &root, names, Source::Worktree) {
            Ok(attributes) => attributes,
            Err(error) => return Ok(git_errors(error)),
        };

    let (files, _) = attributes.drop_excluded(&root, files);

    let kept: HashSet<&str> = files.iter().map(String::as_str).collect();
    let diffed: HashSet<&str> = hunked
        .iter()
        .copied()
        .filter(|name| {
            let path = resolve(&root, name);
            kept.contains(path.as_str()) && !is_filtered(attributes.get(&root, &path, "filter"))
        })
        .collect();

    // A kept path's pathspec also matches the deleted files of the folder it replaced, and each prints its own diff section.
    let diff_names: Vec<&str> = hunked
        .into_iter()
        .filter(|name| diffed.contains(name) || ancestors(name).any(|dir| diffed.contains(dir)))
        .collect();

    let mut changed_lines = HashMap::new();
    if !diff_names.is_empty() {
        let env = repositories.worktree_diff(&root);
        match diff_lines(
            repositories,
            &root,
            &["HEAD"],
            &diff_names,
            &env,
            HUNK_DIFF_BUDGET,
        ) {
            Ok(lines) => changed_lines.extend(
                diff_names
                    .iter()
                    .map(|name| resolve(&root, name))
                    .zip(lines),
            ),
            Err(GitError::Spawn(error)) => return Err(error),
            Err(GitError::Failed(error)) => return Ok(git_errors(error)),
        }
    }

    Ok(PathSelection {
        files: sorted(files),
        invalid_names: sorted_names(invalid_names),
        errors: failure_lines(failures),
        submodules: sorted(skipped),
        changed_lines,
        ..PathSelection::default()
    })
}

pub struct StagedFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub unstaged: bool,
    pub lines: Option<Changed>,
}

pub struct Staged {
    pub files: Vec<StagedFile>,
    pub invalid_names: Vec<PathBuf>,
    pub submodules: Vec<String>,
}

fn sorted_names(mut names: Vec<PathBuf>) -> Vec<PathBuf> {
    names.sort();
    names.dedup();
    names
}

const REGULAR_FILE_MODES: [&str; 2] = ["100644", "100755"];

fn read_blobs(
    repositories: &mut Repositories,
    root: &str,
    ids: &[&str],
) -> Result<Vec<Vec<u8>>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }

    let input = format!("{}\n", ids.join("\n"));
    let output = repositories
        .git(
            root,
            &["cat-file", "--batch"],
            Some(input.as_bytes()),
            &GitEnv::Inherited,
        )
        .map_err(GitError::message)?
        .output;
    let mut blobs = Vec::new();
    let mut offset = 0;
    for id in ids {
        let unreadable = || format!("git cat-file cannot read blob {id}");
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
    repositories: &mut Repositories,
    root: &str,
    tree: Option<&str>,
    name: &str,
    id: &str,
) -> Result<Vec<u8>, String> {
    let source = attr_source(tree);
    let named = format!("--path={name}");
    let mut args: Vec<&str> = source.iter().map(String::as_str).collect();
    args.extend(["cat-file", "--filters", &named, id]);
    Ok(repositories
        .git(root, &args, None, &GitEnv::Inherited)
        .map_err(GitError::message)?
        .output)
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
    repositories: &mut Repositories,
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
    let output = repositories
        .git(root, &args, Some(input.as_bytes()), &GitEnv::Inherited)
        .map_err(GitError::message)?
        .output;

    let close = format!("\n{sentinel} missing\n");
    let mut rest = output.as_slice();
    let mut blobs = Vec::new();
    for (id, _) in requests {
        let unreadable = || format!("git cat-file cannot read blob {id}");
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
    repositories: &mut Repositories,
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
    let unread = match read_filtered_batch(repositories, root, tree, &requests) {
        Ok(contents) => {
            read.extend(batched.iter().map(|(path, ..)| *path).zip(contents));
            single
        }
        Err(_) => named.iter().collect(),
    };

    for (path, id, name) in unread {
        read.push((
            *path,
            read_filtered_blob(repositories, root, tree, name, id)?,
        ));
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
    let diff = repositories
        .git(&root, &args, None, &GitEnv::Inherited)
        .map_err(GitError::message)?
        .output;

    let items: Vec<&[u8]> = diff
        .split(|&byte| byte == 0)
        .filter(|item| !item.is_empty())
        .collect();

    let mut blob_of: IndexMap<String, String> = IndexMap::new();
    let mut modified = Vec::new();
    let mut in_skipped_folder = Vec::new();
    let mut submodules = Vec::new();
    let mut invalid = Vec::new();
    for [header, name] in items.as_chunks::<2>().0 {
        let header = std::str::from_utf8(header).unwrap_or_default();
        let fields: Vec<&str> = header.split(' ').collect();
        let (mode, blob) = (fields.get(1).copied(), fields.get(3).copied());
        let Ok(name) = std::str::from_utf8(name) else {
            if mode.is_some_and(|mode| REGULAR_FILE_MODES.contains(&mode)) {
                invalid.push(*name);
            }

            continue;
        };

        if mode == Some("160000") {
            submodules.push(resolve(&root, name));
        }

        let (Some(mode), Some(blob)) = (mode, blob) else {
            continue;
        };

        if !REGULAR_FILE_MODES.contains(&mode) {
            continue;
        }

        if fields.get(4) == Some(&"M") {
            modified.push(name);
        }

        match candidacy(name, false) {
            Candidacy::Yes => {
                blob_of.insert(resolve(&root, name), blob.to_owned());
            }
            Candidacy::InSkippedFolder => in_skipped_folder.push((resolve(&root, name), blob)),
            Candidacy::No => {}
        }
    }

    let in_skipped_paths: Vec<String> = in_skipped_folder
        .iter()
        .map(|(path, ..)| path.clone())
        .collect();

    let needs_set_stanza: HashSet<String> = if repositories.may_opt_in(&in_skipped_paths, &root) {
        for (path, blob) in in_skipped_folder {
            blob_of.insert(path, blob.to_owned());
        }

        in_skipped_paths.into_iter().collect()
    } else {
        HashSet::new()
    };

    let staged: Vec<String> = blob_of.keys().cloned().collect();
    let names: Vec<&str> = SELECTION_ATTRIBUTES
        .iter()
        .chain(&CONVERSION_ATTRIBUTES)
        .copied()
        .collect();

    let attributes = repositories.read_attributes(&staged, &root, &names, Source::Index)?;

    let selected = |path: &String| {
        let stanza = attributes.get(&root, path, "stanza");
        !is_set(attributes.get(&root, path, "linguist-generated"))
            && !opts_out(stanza)
            && (!needs_set_stanza.contains(path) || is_set(stanza))
    };

    let edited: Vec<&str> = modified
        .into_iter()
        .filter(|name| {
            let path = resolve(&root, name);
            blob_of.contains_key(&path) && selected(&path)
        })
        .collect();

    let mut lines_of: HashMap<String, Changed> = HashMap::new();
    if hunks && !edited.is_empty() {
        let lines = diff_lines(
            repositories,
            &root,
            &range,
            &edited,
            &GitEnv::NoDiffOpts,
            HUNK_DIFF_BUDGET,
        )
        .map_err(GitError::message)?;
        lines_of.extend(edited.iter().map(|name| resolve(&root, name)).zip(lines));
    }

    let diff_env = repositories.worktree_diff(&root);
    let unstaged = repositories
        .git(
            &root,
            &["diff", "--name-only", "-z", "--no-renames"],
            None,
            &diff_env,
        )
        .map_err(GitError::message)?
        .output;
    let dirty: HashSet<String> = nul_names(&unstaged)
        .names
        .into_iter()
        .map(|name| resolve(&root, name))
        .collect();

    let indexed: Vec<String> = sorted(staged).into_iter().filter(selected).collect();

    // --fix reads the worktree's attributes, so an opt out there, or a missing opt in below a skipped folder, must also keep --staged quiet.
    let worktree = repositories.read_attributes(&indexed, &root, &["stanza"], Source::Worktree)?;
    let paths: Vec<String> = indexed
        .into_iter()
        .filter(|path| {
            let stanza = worktree.get(&root, path, "stanza");
            !opts_out(stanza) && (!needs_set_stanza.contains(path) || is_set(stanza))
        })
        .collect();

    let filtered = |path: &str| is_filtered(attributes.get(&root, path, "filter"));

    let (smudge, plain): (Vec<&String>, Vec<&String>) =
        paths.iter().partition(|path| filtered(path));

    let plain_ids: Vec<&str> = plain.iter().map(|path| blob_of[*path].as_str()).collect();
    let mut bytes: IndexMap<&str, Vec<u8>> = plain
        .iter()
        .map(|path| path.as_str())
        .zip(read_blobs(repositories, &root, &plain_ids)?)
        .collect();

    let smudge_paths: Vec<String> = smudge.iter().map(|path| (*path).clone()).collect();
    let worktree = repositories.read_attributes(
        &smudge_paths,
        &root,
        &CONVERSION_ATTRIBUTES,
        Source::Worktree,
    )?;

    let diverged = smudge.iter().any(|path| {
        CONVERSION_ATTRIBUTES
            .iter()
            .any(|name| worktree.get(&root, path, name) != attributes.get(&root, path, name))
    });

    let tree = if diverged {
        Some(repositories.write_tree(&root)?)
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
        repositories,
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

    let indexed_invalid = repositories.drop_invalid_by_attributes(
        invalid_candidates(Path::new(&root), &invalid, false).collect(),
        &root,
        &SELECTION_ATTRIBUTES,
        Source::Index,
    );

    let invalid_names = repositories.drop_invalid_by_attributes(
        indexed_invalid.kept,
        &root,
        &["stanza"],
        Source::Worktree,
    );

    Ok(Staged {
        files,
        invalid_names: sorted_names(invalid_names.kept),
        submodules: sorted(submodules),
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::text::js_trim;

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
    fn a_broken_index_reads_as_git_names_it_from_the_working_directory() {
        let dir = std::env::temp_dir().join(format!("stanza-broken-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("main").join("sub")).unwrap();
        let root = realpath(&dir.join("main").to_string_lossy()).unwrap();
        let file = resolve(&root, "sub/a.ts");
        git(&root, &["init", "-q"]);
        std::fs::write(&file, "export const a = 1;\n").unwrap();
        git(&root, &["add", "."]);
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

        let linked = resolve(&realpath(&dir.to_string_lossy()).unwrap(), "linked");
        git(&root, &["worktree", "add", "-q", "--detach", &linked]);
        for worktree in [&root, &linked] {
            std::fs::write(resolve(worktree, "sub/a.ts"), "export const a = 2;\n").unwrap();
            let git_dir = git(worktree, &["rev-parse", "--absolute-git-dir"]);
            let index = resolve(git_dir.trim_end(), "index");
            let bytes = std::fs::read(&index).unwrap();
            std::fs::write(&index, &bytes[..12]).unwrap();
        }

        let errors = |worktree: &str, cwd: &str| {
            let location = Location::Repository(worktree.to_owned());
            let mut repositories = Repositories::new(Some(cwd));
            collect_changed(location, &mut repositories, None, Caller::Cli(cwd), false)
                .unwrap()
                .errors
        };

        let short = "index file smaller than expected";
        let fatal = format!("fatal: .git/index: {short}");
        assert_eq!(
            errors(&root, &root),
            [
                format!("git diff failed: {fatal}"),
                format!("git ls-files failed: {fatal}")
            ]
        );

        assert_eq!(
            errors(&root, &resolve(&root, "sub"))[0],
            format!("git diff failed in ..: {fatal}")
        );

        let linked_index = format!(
            "{}/index",
            git(&linked, &["rev-parse", "--absolute-git-dir"]).trim_end()
        );
        assert_eq!(
            errors(&linked, &root)[0],
            format!("git diff failed in ../linked: fatal: {linked_index}: {short}")
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_replaced_by_a_link_or_a_file_keeps_hunks() {
        let dir = std::env::temp_dir().join(format!("stanza-linked-hunks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("lib.ts")).unwrap();
        std::fs::create_dir_all(dir.join("doc.ts")).unwrap();
        let root = realpath(&dir.to_string_lossy()).unwrap();
        let path = |name: &str| resolve(&root, name);

        git(&root, &["init", "-q"]);
        std::fs::write(path(".gitignore"), "hidden/\n").unwrap();
        for name in ["keep.ts", "src/a.ts", "lib.ts/a.ts", "doc.ts/a.ts"] {
            std::fs::write(path(name), "export const a = 1;\n").unwrap();
        }

        git(&root, &["add", "."]);
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

        std::fs::remove_dir_all(path("src")).unwrap();
        std::fs::create_dir(path("hidden")).unwrap();
        std::fs::write(path("hidden/a.ts"), "export const a = 2;\n").unwrap();
        std::fs::remove_dir_all(path("lib.ts")).unwrap();
        for link in ["src", "lib.ts"] {
            std::os::unix::fs::symlink("hidden", path(link)).unwrap();
        }

        std::fs::remove_dir_all(path("doc.ts")).unwrap();
        std::fs::write(path("doc.ts"), "export const d = 1;\n").unwrap();
        git(&root, &["add", "-A", "lib.ts", "doc.ts"]);

        std::fs::write(path("keep.ts"), "export const a = 2;\n").unwrap();

        let location = Location::Repository(root.clone());
        let mut repositories = Repositories::new(Some(&root));
        let selected =
            collect_changed(location, &mut repositories, None, Caller::Cli(&root), true).unwrap();

        assert!(selected.errors.is_empty(), "{:?}", selected.errors);
        assert_eq!(selected.files, [path("doc.ts"), path("keep.ts")]);
        assert!(selected.changed_lines.contains_key(&path("keep.ts")));

        std::fs::remove_dir_all(&dir).unwrap();
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
                    .map(|((_, id), name)| {
                        read_filtered_blob(&mut Repositories::new(None), &root, tree, name, id)
                            .unwrap()
                    })
                    .collect();

                let batched: HashMap<&str, Vec<u8>> =
                    read_filtered_blobs(&mut Repositories::new(None), &root, tree, &blobs, |_| {
                        false
                    })
                    .unwrap()
                    .into_iter()
                    .collect();

                let framed_reads =
                    read_filtered_batch(&mut Repositories::new(None), &root, tree, &framed)
                        .unwrap();
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

            let diff = |budget| {
                diff_lines(
                    &mut Repositories::new(None),
                    &root,
                    &["HEAD"],
                    &names,
                    &GitEnv::Inherited,
                    budget,
                )
            };
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
            let selected = collect_changed(
                location,
                &mut Repositories::new(None),
                None,
                Caller::Cli(&root),
                hunks,
            )
            .unwrap();
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
        let selected = collect_changed(
            location,
            &mut Repositories::new(None),
            None,
            Caller::Cli(&root),
            true,
        )
        .unwrap();

        assert!(selected.errors.is_empty(), "{:?}", selected.errors);
        assert!(selected.files.is_empty(), "{:?}", selected.files);
        assert_eq!(selected.submodules, [format!("{root}/sub")]);
        assert!(state() == before, ".git changed");

        git(&root, &["diff", "--name-only", "HEAD"]);
        assert!(state() != before, "git diff left .git alone too");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
