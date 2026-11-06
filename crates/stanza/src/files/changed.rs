use std::collections::HashSet;

use indexmap::{IndexMap, IndexSet};

use super::attributes::{Source, is_set, opts_out};
use super::git::{Location, Repositories, decoded, has_git, nul_items, run_git, run_git_with};
use super::{
    PathSelection, exists, file_realpath, lstat, realpath, repository_path, sorted,
    submodule_paths, thrown, within,
};
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

pub fn collect_changed(
    location: Location,
    repositories: &mut Repositories,
    written: Option<&IndexSet<String>>,
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
    let scope: Vec<String> = written
        .into_iter()
        .flatten()
        .filter(|path| within(&real_root, path))
        .map(|path| repository_path(&real_root, path))
        .collect();

    let budgeted = scope.iter().map(String::len).sum::<usize>() <= PATHSPEC_BUDGET;
    let pathspecs: &[String] = if budgeted { &scope } else { &[] };
    let listing = |head: &[&'static str]| -> Vec<&str> {
        let mut args = head.to_vec();
        args.extend(pathspecs.iter().map(String::as_str));
        args
    };

    let mut changed = run_git(
        &root,
        &listing(&[
            "--literal-pathspecs",
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            "--submodule=short",
            "HEAD",
            "--",
        ]),
    );

    let born = changed.is_ok() || run_git(&root, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok();
    if !born {
        if let Err(error) = run_git(&root, &["symbolic-ref", "-q", "HEAD"]) {
            return Ok(git_errors(error));
        }

        changed = run_git(
            &root,
            &listing(&["--literal-pathspecs", "ls-files", "-z", "--cached", "--"]),
        );
    }

    let untracked = run_git(
        &root,
        &listing(&[
            "--literal-pathspecs",
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
            "--",
        ]),
    );

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
    let untracked_names = nul_items(&untracked).filter(kept);
    let mut files = Vec::new();
    for name in names.iter().copied().chain(untracked_names) {
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
            ..PathSelection::default()
        }),
        Err(error) => Ok(git_errors(error)),
    }
}

pub struct StagedFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub unstaged: bool,
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
        false,
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

fn read_filtered_blob(
    root: &str,
    tree: Option<&str>,
    path: &str,
    id: &str,
) -> Result<Vec<u8>, String> {
    let source = tree.map(|tree| format!("--attr-source={tree}"));
    let named = format!("--path={}", repository_path(root, path));
    let mut args: Vec<&str> = source.iter().map(String::as_str).collect();
    args.extend(["cat-file", "--filters", &named, id]);
    Ok(run_git_with(root, &args, None, false)?.output)
}

pub fn collect_staged(cwd: &str, repositories: &mut Repositories) -> Result<Staged, String> {
    if !has_git() {
        return Err("--staged needs git, which was not found on PATH".to_owned());
    }

    let root = match repositories.locate(cwd) {
        Location::Repository(root) => root,
        Location::Failed(error) => return Err(error),
        Location::Outside => return Err("not inside a git repository".to_owned()),
    };

    let diff = run_git(
        &root,
        &[
            "diff",
            "--cached",
            "--diff-filter=ACMT",
            "--raw",
            "-z",
            "--no-renames",
            "--no-abbrev",
        ],
    )?;

    let items: Vec<&str> = nul_items(&diff).collect();
    let mut blob_of: IndexMap<String, String> = IndexMap::new();
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
        }
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

    for path in &smudge {
        let smudged = read_filtered_blob(&root, tree.as_deref(), path, &blob_of[*path])?;
        bytes.insert(path.as_str(), smudged);
    }

    let files = paths
        .iter()
        .map(|path| StagedFile {
            bytes: bytes.swap_remove(path.as_str()).unwrap_or_default(),
            unstaged: dirty.contains(path),
            path: path.clone(),
        })
        .collect();

    Ok(Staged {
        files,
        submodules: sorted(submodules),
    })
}
