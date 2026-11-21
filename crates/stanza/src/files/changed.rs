use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufRead, Read};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};

use super::attributes::{
    CONVERSION_ATTRIBUTES, Candidates, SELECTION_ATTRIBUTES, Source, is_filtered,
};
use super::diff::{Changed, diff_changes, diff_sections};
use super::git::{
    GitCall, GitEnv, GitError, GitOutput, Location, Repositories, decoded, decoded_owned,
    git_succeeds, has_git, nul_names, submodules_call,
};
use super::{
    Failure, FolderLinks, PathSelection, Unresolved, failure_lines, file_realpath,
    invalid_candidates, invalid_files, lstat, missing, opted_in_folder, realpath, repository_path,
    sorted, unread_by_git, unreadable_entry, unresolved, within,
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

enum NameCalls {
    Changed {
        diff: GitCall,
        untracked: Option<GitCall>,
    },
    Indexed(GitCall),
}

pub(super) struct PreparedListing {
    root: String,
    written: Option<IndexSet<String>>,
    cli: bool,
    hunks: bool,
    pathspecs: Vec<String>,
    names: NameCalls,
    submodules: Option<GitCall>,
}

impl PreparedListing {
    fn requested(&self) -> impl Iterator<Item = &GitCall> {
        let (names, untracked) = match &self.names {
            NameCalls::Changed { diff, untracked } => (diff, untracked.as_ref()),
            NameCalls::Indexed(indexed) => (indexed, None),
        };

        [Some(names), untracked, self.submodules.as_ref()]
            .into_iter()
            .flatten()
    }
}

fn listing_call(root: &str, head: &[&str], pathspecs: &[String], env: GitEnv) -> GitCall {
    let head = head.iter().copied().map(str::to_owned);
    GitCall {
        cwd: root.to_owned(),
        args: head.chain(pathspecs.iter().cloned()).collect(),
        env,
    }
}

fn listing_calls(
    repositories: &mut Repositories,
    root: &str,
    real_root: &str,
    written: Option<&IndexSet<String>>,
    caller: Caller,
    hunks: bool,
) -> Rc<PreparedListing> {
    let cli = matches!(caller, Caller::Cli(_));
    let prepared = repositories.listings.iter().find(|listing| {
        listing.root == root
            && listing.cli == cli
            && listing.hunks == hunks
            && listing.written.as_ref().map(IndexSet::as_slice) == written.map(IndexSet::as_slice)
    });

    if let Some(prepared) = prepared {
        return prepared.clone();
    }

    let paths: Vec<String> = written
        .into_iter()
        .flatten()
        .filter(|path| within(real_root, path))
        .map(|path| repository_path(real_root, path))
        .collect();

    let budgeted = paths.iter().map(String::len).sum::<usize>() <= PATHSPEC_BUDGET;
    let pathspecs = if budgeted { paths } else { Vec::new() };
    let untracked = written.is_none() || !budgeted || !repositories.tracks_all(root, &pathspecs);
    let names = if written.is_some() && !hunks {
        let head = if untracked {
            &INDEXED_OR_UNTRACKED[..]
        } else {
            &CACHED
        };

        NameCalls::Indexed(listing_call(root, head, &pathspecs, GitEnv::CLocale))
    } else {
        let diff_env = repositories.worktree_diff(root);
        NameCalls::Changed {
            diff: listing_call(root, &DIFF_HEAD, &pathspecs, diff_env),
            untracked: untracked
                .then(|| listing_call(root, &UNTRACKED, &pathspecs, GitEnv::CLocale)),
        }
    };

    let submodules = if cli && written.is_none() {
        submodules_call(root)
    } else {
        None
    };

    let listing = Rc::new(PreparedListing {
        root: root.to_owned(),
        written: written.cloned(),
        cli,
        hunks,
        pathspecs,
        names,
        submodules,
    });

    repositories.listings.push(listing.clone());
    listing
}

#[derive(Clone, Copy)]
pub enum Caller<'a> {
    Cli(&'a str),
    Hook,
}

pub enum Targets<'a> {
    Directory(&'a str),
    Written(&'a IndexSet<String>),
}

type Group = (Location, Option<IndexSet<String>>);

enum Resolved<'a> {
    Directory(&'a str),
    Written(Vec<(&'a str, String)>),
}

impl Resolved<'_> {
    fn groups(&self, mut locate: impl FnMut(&str) -> Location) -> Vec<Group> {
        let written = match self {
            Resolved::Directory(dir) => return vec![(locate(dir), None)],
            Resolved::Written(written) => written,
        };

        let mut found: IndexMap<String, (Location, IndexSet<String>)> = IndexMap::new();
        for (dir, real) in written {
            let location = locate(dir);
            let key = match &location {
                Location::Outside => continue,
                Location::Repository(root) if !within(root, real) => continue,
                Location::Repository(root) => root.clone(),
                Location::Failed(error) => error.clone(),
            };

            found
                .entry(key)
                .or_insert_with(|| (location, IndexSet::new()))
                .1
                .insert(real.clone());
        }

        found
            .into_values()
            .map(|(location, kept)| (location, Some(kept)))
            .collect()
    }
}

pub fn locate_changed(
    targets: Targets,
    caller: Caller,
    hunks: bool,
    repositories: &mut Repositories,
) -> Vec<Group> {
    let resolved = match targets {
        Targets::Directory(dir) => Resolved::Directory(dir),
        Targets::Written(written) => Resolved::Written(
            written
                .iter()
                .filter_map(|path| Some((dirname(path), realpath(path).ok()?)))
                .collect(),
        ),
    };

    let predicted = resolved.groups(|dir| repositories.predict(dir));
    prefetch_changed(&predicted, caller, hunks, repositories);
    resolved.groups(|dir| repositories.locate(dir))
}

fn prefetch_changed(
    groups: &[Group],
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

        let listing = listing_calls(
            repositories,
            root,
            &real_root,
            written.as_ref(),
            caller,
            hunks,
        );

        roots.push(root.as_str());
        calls.extend(listing.requested().cloned());
    }

    repositories.prefetch(&roots, calls);
}

struct Listed {
    changed: Vec<u8>,
    whole: GitOutput,
    diffs_head: bool,
}

fn list_names(
    repositories: &mut Repositories,
    listing: &PreparedListing,
) -> Result<Listed, Vec<String>> {
    let (diff, untracked) = match &listing.names {
        NameCalls::Changed { diff, untracked } => (diff, untracked),
        NameCalls::Indexed(indexed) => {
            return repositories
                .run(indexed)
                .map(|whole| Listed {
                    changed: Vec::new(),
                    whole,
                    diffs_head: false,
                })
                .map_err(|error| vec![error.message()]);
        }
    };

    let root = &listing.root;
    let fallback = |head: &[&str], env| listing_call(root, head, &listing.pathspecs, env);
    let mut changed = repositories.run(diff);
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

        changed = repositories.run(&fallback(&CACHED, GitEnv::Inherited));
    }

    let whole = match untracked {
        Some(untracked) => repositories.run(untracked),
        None if changed.is_err() => repositories.run(&fallback(&UNTRACKED, GitEnv::CLocale)),
        None => Ok(GitOutput::default()),
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
    collect_resolving(
        location,
        repositories,
        written,
        caller,
        hunks,
        file_realpath,
    )
}

fn collect_resolving(
    location: Location,
    repositories: &mut Repositories,
    written: Option<&IndexSet<String>>,
    caller: Caller,
    hunks: bool,
    real_file: impl Fn(&str) -> Result<String, Unresolved>,
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
    let listing = listing_calls(repositories, &root, &real_root, written, caller, hunks);
    let listed = match list_names(repositories, &listing) {
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

        match repositories.drop_invalid_by_attributes(
            invalid_files(Path::new(&root), &invalid, false, &mut links),
            &root,
            &SELECTION_ATTRIBUTES,
            Source::Worktree,
        ) {
            Ok(sorted) => sorted.kept,
            Err(error) => {
                failures.push(Failure::new(root.clone(), error));
                Vec::new()
            }
        }
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

    let listed: IndexSet<&str> = names.iter().chain(&whole_names).copied().collect();
    let mut files = Candidates::default();
    for name in listed {
        let candidacy = selectable(name);
        if candidacy == Candidacy::No {
            continue;
        }

        let path = resolve(&root, name);
        if links.through_link(dirname(&path)) {
            continue;
        }

        match (lstat(&path), caller) {
            (Ok(meta), _) if meta.is_file() => match (real_file(&path), caller) {
                (Ok(real), _) if within(&real_root, &real) => files.push(path, candidacy),
                (Ok(_), _) => {}
                (Err(unresolved), Caller::Cli(cwd)) => failures.push(unresolved.failure(cwd)),
                (Err(unresolved), Caller::Hook) => return Err(unresolved.line(None)),
            },
            (Err(error), Caller::Cli(cwd)) if !missing(&error) => {
                failures.push(unreadable_entry(cwd, &path, &error));
            }
            _ => {}
        }
    }

    let gitlinks = match &listing.submodules {
        Some(call) => repositories.listed_submodules(call),
        None => Rc::default(),
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
            Err(GitError::Failed(error) | GitError::Usage(error)) => {
                return Ok(git_errors(error));
            }
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
    pub unstaged: bool,
    pub lines: Option<Changed>,
}

pub struct Staged {
    pub files: Vec<StagedFile>,
    pub blobs: Vec<Vec<u8>>,
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
    repositories
        .git_read(root, &["cat-file", "--batch"], input.as_bytes(), |reply| {
            plain_blobs(reply, ids)
        })
        .map_err(GitError::message)?
}

type Framed = io::Result<Result<Vec<Vec<u8>>, String>>;

fn plain_blobs(reply: &mut impl BufRead, ids: &[&str]) -> Framed {
    let mut blobs = Vec::with_capacity(ids.len());
    let mut header = Vec::new();
    for id in ids {
        let unreadable = || Ok(Err(format!("git cat-file cannot read blob {id}")));
        header.clear();
        reply.read_until(b'\n', &mut header)?;
        let Some(line) = header.strip_suffix(b"\n") else {
            return unreadable();
        };

        let line = decoded(line);
        let fields: Vec<&str> = line.split(' ').collect();
        let size = fields.get(2).and_then(|size| size.parse::<usize>().ok());
        let (Some(&"blob"), Some(size)) = (fields.get(1), size) else {
            return unreadable();
        };

        let mut blob = Vec::new();
        if blob.try_reserve_exact(size).is_err() {
            return unreadable();
        }

        reply.by_ref().take(size as u64).read_to_end(&mut blob)?;
        let newline = reply.fill_buf()?.len().min(1);
        reply.consume(newline);
        blobs.push(blob);
    }

    Ok(Ok(blobs))
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
    match repositories.git(root, &args, None, &GitEnv::Inherited) {
        Ok(read) => Ok(read.output),
        Err(GitError::Usage(_)) if source.is_some() => Err(ATTR_SOURCE_UNSUPPORTED.to_owned()),
        Err(error) => Err(error.message()),
    }
}

const ATTR_SOURCE_UNSUPPORTED: &str = "git cat-file failed: --attr-source is unsupported; staged files whose conversion attributes differ from the working tree need git 2.41 or later";

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
    repositories
        .git_read(root, &args, input.as_bytes(), |reply| {
            filtered_blobs(reply, requests, &sentinel)
        })
        .map_err(GitError::message)?
}

fn filtered_blobs(reply: &mut impl BufRead, requests: &[(&str, &str)], sentinel: &str) -> Framed {
    let close = format!("\n{sentinel} missing\n");
    let mut blobs = Vec::with_capacity(requests.len());
    let mut header = Vec::new();
    for (id, _) in requests {
        let unreadable = || Ok(Err(format!("git cat-file cannot read blob {id}")));
        header.clear();
        reply.read_until(b'\n', &mut header)?;
        let Some(stored) = header
            .strip_prefix(format!("{id} blob ").as_bytes())
            .and_then(|after| after.strip_suffix(b"\n"))
        else {
            return unreadable();
        };

        let stored: usize = decoded(stored).parse().unwrap_or_default();
        let mut blob = Vec::new();
        let _ = blob.try_reserve_exact(stored.saturating_add(close.len()));
        while !blob.ends_with(close.as_bytes()) {
            if reply.read_until(b'\n', &mut blob)? == 0 {
                return unreadable();
            }
        }

        blob.truncate(blob.len() - close.len());
        blob.shrink_to_fit();
        blobs.push(blob);
    }

    if !reply.fill_buf()?.is_empty() {
        return Ok(Err(
            "git cat-file printed more than the blobs it was asked for".to_owned(),
        ));
    }

    Ok(Ok(blobs))
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

    let selected = |path: &String| attributes.keeps(&root, path, needs_set_stanza.contains(path));

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
        .filter(|path| worktree.keeps(&root, path, needs_set_stanza.contains(path)))
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

    let blobs = paths
        .iter()
        .map(|path| bytes.swap_remove(path.as_str()).unwrap_or_default())
        .collect();

    let files = paths
        .iter()
        .map(|path| StagedFile {
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
    )?;

    let invalid_names = repositories.drop_invalid_by_attributes(
        indexed_invalid.kept,
        &root,
        &["stanza"],
        Source::Worktree,
    )?;

    Ok(Staged {
        files,
        blobs,
        invalid_names: sorted_names(invalid_names.kept),
        submodules: sorted(submodules),
    })
}

#[cfg(test)]
#[path = "changed_tests.rs"]
mod changed_tests;

#[cfg(test)]
#[path = "changed_listing_tests.rs"]
mod changed_listing_tests;

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::step::decode;
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
    fn a_realpath_failure_keeps_changed_siblings() {
        let dir =
            std::env::temp_dir().join(format!("stanza-realpath-sibling-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = realpath(&dir.to_string_lossy()).unwrap();
        let path = |name: &str| resolve(&root, name);

        git(&root, &["init", "-q"]);
        for name in ["bad.ts", "good.ts"] {
            std::fs::write(path(name), "export {};\n").unwrap();
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

        for name in ["bad.ts", "good.ts"] {
            std::fs::write(path(name), "if (a) {\n  b();\n}\n").unwrap();
        }

        let bad = path("bad.ts");
        let denied = |file: &str| {
            if file == bad {
                Err(Unresolved::new(
                    file,
                    io::Error::from_raw_os_error(libc::EACCES),
                ))
            } else {
                file_realpath(file)
            }
        };

        for hunks in [false, true] {
            let location = Location::Repository(root.clone());
            let selected = collect_resolving(
                location,
                &mut Repositories::new(Some(&root)),
                None,
                Caller::Cli(&root),
                hunks,
                denied,
            )
            .unwrap();

            assert_eq!(selected.files, [path("good.ts")], "hunks={hunks}");
            assert_eq!(
                selected.errors,
                ["cannot read bad.ts: permission denied"],
                "hunks={hunks}"
            );
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn groups(targets: Targets) -> Vec<(Location, Option<Vec<String>>)> {
        let mut repositories = Repositories::new(None);
        locate_changed(targets, Caller::Hook, false, &mut repositories)
            .into_iter()
            .map(|(location, kept)| (location, kept.map(|kept| kept.into_iter().collect())))
            .collect()
    }

    #[test]
    fn written_paths_group_by_the_repository_they_were_written_in() {
        let dir =
            std::env::temp_dir().join(format!("stanza-written-groups-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for name in ["one", "two", "plain"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }

        let base = realpath(&dir.to_string_lossy()).unwrap();
        let path = |name: &str| resolve(&base, name);
        for name in ["one", "two"] {
            git(&path(name), &["init", "-q"]);
        }

        for name in ["one/a.ts", "one/c.ts", "two/b.ts", "plain/d.ts"] {
            std::fs::write(path(name), "export {};\n").unwrap();
        }

        std::os::unix::fs::symlink(path("two/b.ts"), path("one/link.ts")).unwrap();
        let written: IndexSet<String> = [
            "one/a.ts",
            "two/b.ts",
            "one/link.ts",
            "one/missing.ts",
            "plain/d.ts",
            "one/c.ts",
        ]
        .map(path)
        .into_iter()
        .collect();

        assert_eq!(
            groups(Targets::Written(&written)),
            [
                (
                    Location::Repository(path("one")),
                    Some(vec![path("one/a.ts"), path("one/c.ts")])
                ),
                (
                    Location::Repository(path("two")),
                    Some(vec![path("two/b.ts")])
                ),
            ]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_target_is_one_group_inside_or_outside_git() {
        let dir =
            std::env::temp_dir().join(format!("stanza-directory-group-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for name in ["repo", "plain"] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
        }

        let base = realpath(&dir.to_string_lossy()).unwrap();
        let path = |name: &str| resolve(&base, name);
        git(&path("repo"), &["init", "-q"]);

        assert_eq!(
            groups(Targets::Directory(&path("repo"))),
            [(Location::Repository(path("repo")), None)]
        );
        assert_eq!(
            groups(Targets::Directory(&path("plain"))),
            [(Location::Outside, None)]
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_name_listed_as_changed_and_untracked_resolves_once() {
        let dir = std::env::temp_dir().join(format!("stanza-listed-twice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let root = realpath(&dir.to_string_lossy()).unwrap();
        let twice = resolve(&root, "twice.ts");

        git(&root, &["init", "-q"]);
        std::fs::write(&twice, "export {};\n").unwrap();
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

        git(&root, &["rm", "-q", "--cached", "twice.ts"]);
        let calls = std::cell::Cell::new(0);
        let counted = |file: &str| {
            calls.set(calls.get() + 1);
            file_realpath(file)
        };

        let location = Location::Repository(root.clone());
        let selected = collect_resolving(
            location,
            &mut Repositories::new(Some(&root)),
            None,
            Caller::Cli(&root),
            false,
            counted,
        )
        .unwrap();

        assert_eq!(selected.files, [twice]);
        assert_eq!(calls.get(), 1);
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

    fn held_once(reply: &[u8], contents: &[Vec<u8>], read: impl Fn(&mut &[u8]) -> Framed) {
        let peak = crate::allocations::peak_bytes(|| {
            let blobs = read(&mut &reply[..]).unwrap().unwrap();
            assert_eq!(blobs.len(), contents.len());
            for (blob, content) in blobs.into_iter().zip(contents) {
                assert_eq!(&blob, content);
                let text = decode(blob);
                match content.as_slice() {
                    [] => assert_eq!(text, Ok(String::new())),
                    [0xff] => assert_eq!(text, Err("not valid UTF-8, left untouched".to_owned())),
                    _ => assert!(text.is_ok()),
                }
            }
        });

        assert!(
            peak <= reply.len() + 8_000,
            "{} blobs peaked at {peak} bytes for a {} byte reply",
            contents.len(),
            reply.len()
        );
    }

    #[test]
    fn staged_blobs_are_held_once() {
        let sentinel = "5".repeat(40);
        let close = format!("\n{sentinel} missing\n");
        for count in [1_000, 4_000] {
            let mut contents: Vec<Vec<u8>> = (0..count)
                .map(|index| format!("{index:08}").repeat(1_000).into_bytes())
                .collect();

            contents.extend([Vec::new(), vec![0xff]]);
            let ids: Vec<String> = (0..contents.len())
                .map(|index| format!("{index:040x}"))
                .collect();

            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            let requests: Vec<(&str, &str)> = ids.iter().map(|id| (*id, "a.ts")).collect();
            let framed = |after: &str| -> Vec<u8> {
                ids.iter()
                    .zip(&contents)
                    .flat_map(|(id, content)| {
                        let header = format!("{id} blob {}\n", content.len()).into_bytes();
                        [header, content.clone(), after.as_bytes().to_vec()].concat()
                    })
                    .collect()
            };

            held_once(&framed("\n"), &contents, |reply| plain_blobs(reply, &ids));
            held_once(&framed(&close), &contents, |reply| {
                filtered_blobs(reply, &requests, &sentinel)
            });
        }

        let id = "a".repeat(40);
        let unreadable = Err(format!("git cat-file cannot read blob {id}"));
        let missing = format!("{id} missing\n");
        assert_eq!(
            plain_blobs(&mut missing.as_bytes(), &[&id]).unwrap(),
            unreadable
        );

        let oversized = format!("{id} blob {}\nx", usize::MAX);
        assert_eq!(
            plain_blobs(&mut oversized.as_bytes(), &[&id]).unwrap(),
            unreadable
        );

        let requests = [(id.as_str(), "a.ts")];
        let oversized = format!("{id} blob {}\nx{close}", usize::MAX);
        assert_eq!(
            filtered_blobs(&mut oversized.as_bytes(), &requests, &sentinel).unwrap(),
            Ok(vec![b"x".to_vec()])
        );

        let unclosed = format!("{id} blob 1\nx");
        assert_eq!(
            filtered_blobs(&mut unclosed.as_bytes(), &requests, &sentinel).unwrap(),
            unreadable
        );

        let extra = format!("{id} blob 1\nx{close}x");
        assert_eq!(
            filtered_blobs(&mut extra.as_bytes(), &requests, &sentinel).unwrap(),
            Err("git cat-file printed more than the blobs it was asked for".to_owned())
        );
    }
}
