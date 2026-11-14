mod attributes;
mod changed;
pub mod diff;
mod git;
mod index;
mod walk;

use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use indexmap::IndexMap;
use regex_lite::Regex;

use crate::directories::{listed, listed_file, real_directory};
use crate::languages::{entry_for, is_candidate, prunes};
use crate::paths::{basename, dirname, is_absolute, join, relative, resolve};
use crate::text::{js_trim, js_trim_start};

use attributes::{SELECTION_ATTRIBUTES, Source};
use git::{nul_names, run_git};
use walk::fallback_files;

pub use changed::{Caller, Staged, StagedFile, collect_changed, collect_staged, prefetch_changed};
pub use diff::Changed;
pub use git::{Location, Repositories, has_git};

const ANY: &str = r"[^\r\n\x{2028}\x{2029}]";

const GENERATED_TAG_SOURCE: &str = r"@generated\b";

const GENERATED_MARKER_SOURCES: [&str; 4] = [
    r"^code generated\b{any}*\bdo not edit\b",
    r"\b(?:this|the) (?:file|code|class) (?:is|was|has been) (?:auto(?:matically)?[- ]?)?generated\b",
    r"\bauto(?:matically)?[- ]?generated (?:file|code|class)\b",
    r"^(?:auto(?:matically)?[- ]?)generated\b",
];

const GENERATOR_ATTRIBUTION_SOURCE: &str = r"^(?:code )?generated (?:by|from|with|using|code)\b";

const EDIT_WARNING_SOURCE: &str = r"\bdo not (?:edit|modify|make (?:direct )?changes)\b";

const UNOPENED_DIRECTORY_SOURCE: &str = r"^warning: could not open directory '(.*)/': (.*)";

// regex-lite's \b is ASCII only, as JavaScript's \b is without the u flag.
fn pattern(source: &str) -> Regex {
    Regex::new(&source.replace("{any}", ANY)).expect("the header patterns compile")
}

// JavaScript /i without u folds only ASCII here, so the comment is ASCII lowercased and the patterns are lowercase.
static GENERATED_TAG: LazyLock<Regex> = LazyLock::new(|| pattern(GENERATED_TAG_SOURCE));

static GENERATED_MARKERS: LazyLock<[Regex; 4]> =
    LazyLock::new(|| GENERATED_MARKER_SOURCES.map(pattern));

static GENERATOR_ATTRIBUTION: LazyLock<Regex> =
    LazyLock::new(|| pattern(GENERATOR_ATTRIBUTION_SOURCE));

static EDIT_WARNING: LazyLock<Regex> = LazyLock::new(|| pattern(EDIT_WARNING_SOURCE));

pub fn is_generated_header(text: &str) -> bool {
    let comments = header_comments(text);
    if comments.is_empty() {
        return false;
    }

    if comments
        .iter()
        .any(|comment| GENERATED_TAG.is_match(comment))
    {
        return true;
    }

    let folded: Vec<String> = comments
        .iter()
        .map(|comment| comment.to_ascii_lowercase())
        .collect();

    let any = |regex: &Regex| folded.iter().any(|comment| regex.is_match(comment));
    GENERATED_MARKERS.iter().any(any) || (any(&GENERATOR_ATTRIBUTION) && any(&EDIT_WARNING))
}

fn header_comments(text: &str) -> Vec<&str> {
    let mut comments = Vec::new();
    let mut in_block = false;
    for line in text.split('\n').take(10) {
        let mut rest = js_trim(line.strip_suffix('\r').unwrap_or(line));
        while !rest.is_empty() {
            if !in_block {
                if let Some(body) = rest.strip_prefix("//") {
                    comments.push(comment_body(body));
                    break;
                }

                let Some(body) = rest.strip_prefix("/*") else {
                    break;
                };

                rest = body;
            }

            let end = rest.find("*/");
            in_block = end.is_none();
            comments.push(comment_body(end.map_or(rest, |end| &rest[..end])));
            rest = end.map_or("", |end| js_trim_start(&rest[end + 2..]));
        }
    }

    comments
}

fn comment_body(text: &str) -> &str {
    js_trim(text.trim_start_matches(['/', '*']))
}

// std prints an OS error as strerror's text followed by ` (os error N)`, and the words are all stanza shows.
pub fn reason(error: &io::Error) -> String {
    let text = error.to_string();
    let words = error
        .raw_os_error()
        .and_then(|code| text.strip_suffix(&format!(" (os error {code})")))
        .unwrap_or(&text);

    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

pub(super) fn unresolved(cwd: Option<&str>, path: &str, error: &io::Error) -> String {
    let shown = cwd.map_or_else(|| path.to_owned(), |cwd| shown_path(cwd, path));
    format!("cannot read {shown}: {}", reason(error))
}

fn missing(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR))
}

pub struct InvalidArguments {
    pub names: Vec<PathBuf>,
    pub opted_out: bool,
}

pub fn invalid_arguments(
    inputs: &[OsString],
    cwd: &str,
    repositories: &mut Repositories,
) -> InvalidArguments {
    let mut found = InvalidArguments {
        names: Vec::new(),
        opted_out: false,
    };

    for input in inputs {
        let (kept, opted_out) = selected_invalid(Path::new(cwd).join(input), repositories, false);
        found.names.extend(kept);
        found.opted_out |= opted_out;
    }

    found
}

fn selected_invalid(
    path: PathBuf,
    repositories: &mut Repositories,
    keep_generated: bool,
) -> (Option<PathBuf>, bool) {
    let path: PathBuf = path.components().collect();
    let bytes = path.as_os_str().as_bytes();
    let valid = std::str::from_utf8(bytes).map_or_else(|error| error.valid_up_to(), str::len);
    let parent = bytes[..valid]
        .iter()
        .rposition(|&byte| byte == b'/')
        .and_then(|slash| std::str::from_utf8(&bytes[..slash.max(1)]).ok())
        .unwrap_or("/");

    let location = repositories.locate(parent);
    let below = match &location {
        Location::Repository(root) => path.strip_prefix(root).unwrap_or(&path),
        _ => Path::new(path.file_name().unwrap_or_default()),
    };

    let shown = below.to_string_lossy();
    if entry_for(&shown).is_some() && !is_candidate(&shown, keep_generated) {
        return (None, false);
    }

    let Location::Repository(root) = location else {
        return (Some(path), false);
    };

    let names = selection_attributes(keep_generated);
    let (kept, opted_out) =
        repositories.drop_invalid_by_attributes(vec![path], &root, names, Source::Worktree);

    (kept.into_iter().next(), opted_out)
}

pub fn realpath(path: &str) -> io::Result<String> {
    fs::canonicalize(path)?
        .into_os_string()
        .into_string()
        .map_err(|_| io::Error::from_raw_os_error(libc::EILSEQ))
}

// Candidacy reads only ASCII extensions and directory names, so the lossy text classifies a name it never stands in for.
fn invalid_candidates<'a>(
    dir: &'a Path,
    names: &'a [&[u8]],
    keep_generated: bool,
) -> impl Iterator<Item = PathBuf> + 'a {
    names
        .iter()
        .filter(move |name| is_candidate(&String::from_utf8_lossy(name), keep_generated))
        .map(|name| dir.join(OsStr::from_bytes(name)))
}

fn invalid_real(path: &str) -> Option<PathBuf> {
    let real = fs::canonicalize(path).ok()?;
    real.to_str().is_none().then_some(real)
}

fn selection_attributes(keep_generated: bool) -> &'static [&'static str] {
    if keep_generated {
        &["stanza"]
    } else {
        &SELECTION_ATTRIBUTES
    }
}

fn invalid_files(dir: &Path, names: &[&[u8]], keep_generated: bool) -> Vec<PathBuf> {
    invalid_candidates(dir, names, keep_generated)
        .filter(|path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()))
        .collect()
}

fn supplied(cwd: &str, input: &str) -> String {
    if is_absolute(input) {
        input.to_owned()
    } else {
        resolve(cwd, input)
    }
}

fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

fn lstat(path: &str) -> io::Result<fs::Metadata> {
    fs::symlink_metadata(path)
}

fn lstat_if_present(path: &str) -> io::Result<Option<fs::Metadata>> {
    match lstat(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.raw_os_error() == Some(libc::ENOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

fn access(path: impl AsRef<Path>, mode: libc::c_int) -> io::Result<()> {
    let Ok(text) = CString::new(path.as_ref().as_os_str().as_bytes()) else {
        return Err(io::Error::from_raw_os_error(libc::ENOENT));
    };

    if unsafe { libc::access(text.as_ptr(), mode) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn searchable(path: impl AsRef<Path>) -> io::Result<()> {
    access(path, libc::R_OK | libc::X_OK)
}

fn shown_path(cwd: &str, path: &str) -> String {
    let shown = relative(cwd, path);
    if shown.is_empty() {
        ".".to_owned()
    } else {
        shown
    }
}

fn sorted(mut items: Vec<String>) -> Vec<String> {
    items.sort();
    items.dedup();
    items
}

fn unique(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect()
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Failure {
    path: Vec<u8>,
    line: String,
}

impl Failure {
    pub(super) fn new(path: impl Into<Vec<u8>>, line: String) -> Self {
        Failure {
            path: path.into(),
            line,
        }
    }
}

fn failure_lines(mut failures: Vec<Failure>) -> Vec<String> {
    failures.sort();
    unique(failures.into_iter().map(|failure| failure.line).collect())
}

fn repository_path(root: &str, file: &str) -> String {
    relative(root, file)
}

pub fn within(root: &str, path: &str) -> bool {
    let from_root = relative(root, path);
    from_root != ".." && !from_root.starts_with("../") && !is_absolute(&from_root)
}

#[derive(Default)]
struct Anchors {
    prefixes: HashMap<(String, String), String>,
}

impl Anchors {
    fn prefix(&mut self, anchor: &str, dir: &str) -> &str {
        self.prefixes
            .entry((anchor.to_owned(), dir.to_owned()))
            .or_insert_with(|| {
                let mut current = dir.to_owned();
                loop {
                    if exists(&current) && real_directory(&current).is_ok_and(|real| real == anchor)
                    {
                        return relative(&current, dir);
                    }

                    let parent = dirname(&current);
                    if parent == current {
                        return String::new();
                    }

                    current = parent;
                }
            })
    }

    fn supplied_below(&mut self, anchor: &str, path: &str) -> String {
        let name = basename(path);
        match self.prefix(anchor, &dirname(path)) {
            "" => name.to_owned(),
            prefix => format!("{prefix}/{name}"),
        }
    }

    fn selected_below(
        &mut self,
        anchor: &str,
        supplied: &str,
        file: &str,
        keep_generated: bool,
    ) -> bool {
        is_candidate(&repository_path(anchor, file), keep_generated)
            && is_candidate(&self.supplied_below(anchor, supplied), keep_generated)
    }
}

fn unsearchable(path: &str) -> String {
    let parent = dirname(path);
    if parent == path {
        return path.to_owned();
    }

    if lstat(&parent).is_ok() {
        parent
    } else {
        unsearchable(&parent)
    }
}

fn file_realpath(cwd: Option<&str>, path: &str) -> Result<String, String> {
    realpath(path).or_else(|error| {
        if lstat(path).is_ok_and(|meta| meta.is_file()) {
            let parent = dirname(path);
            let real = realpath(&parent).map_err(|error| unresolved(cwd, &parent, &error))?;
            Ok(join(&real, basename(path)))
        } else {
            Err(unresolved(cwd, path, &error))
        }
    })
}

// realpath returns the name in its on disk case and normalization, so only an exact listing match may join.
// The caller has ruled out a symlink, which realpath would follow.
fn listed_file_realpath(cwd: &str, path: &str) -> Result<String, String> {
    let dir = dirname(path);
    let name = basename(path);
    if !listed(&dir, name) {
        return file_realpath(Some(cwd), path);
    }

    let real = real_directory(&dir).map_err(|error| unresolved(Some(cwd), &dir, &error))?;
    Ok(join(&real, name))
}

static UNOPENED_DIRECTORY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(UNOPENED_DIRECTORY_SOURCE).expect("the warning pattern compiles"));

fn unreadable_entry(cwd: &str, path: &str, error: &io::Error) -> Failure {
    let shown = shown_path(cwd, &unsearchable(path));
    let line = format!("cannot read {shown}: {}", reason(error));
    Failure::new(shown, line)
}

// git masks control characters as `?`, so a folder named `c\rr` comes back as `c?r`, which does not exist, and git's reason is the only one left.
fn unopened_directories(
    warnings: &str,
    root: &str,
    real: &str,
    dir: &str,
    cwd: &str,
) -> Vec<Failure> {
    let mut failures = Vec::new();
    for captures in warnings
        .split('\n')
        .filter_map(|line| UNOPENED_DIRECTORY.captures(line))
    {
        let group = |index| captures.get(index).map_or("", |group| group.as_str());
        let below = relative(real, &resolve(root, group(1)));
        if prunes(&below) {
            continue;
        }

        let directory = join(dir, &below);
        let shown = shown_path(cwd, &directory);
        let text = format!("cannot read {shown}");
        let line = match searchable(&directory) {
            Err(error) if !missing(&error) => format!("{text}: {}", reason(&error)),
            _ => format!("{text}: {}", group(2).to_lowercase()),
        };

        failures.push(Failure::new(shown, line));
    }

    failures
}

struct Listed {
    files: Vec<String>,
    invalid_names: Vec<PathBuf>,
    opted_out: bool,
    submodules: Vec<String>,
    errors: Vec<Failure>,
}

fn directory_files(
    dir: &str,
    root: &str,
    cwd: &str,
    keep_generated: bool,
    repositories: &mut Repositories,
) -> Result<Listed, String> {
    let listing = repositories.directory_listing(dir);
    let result = match &*listing {
        Ok(result) => result,
        Err(error) => {
            return Ok(Listed {
                files: Vec::new(),
                invalid_names: Vec::new(),
                opted_out: false,
                submodules: Vec::new(),
                errors: vec![Failure::new(shown_path(cwd, dir), error.clone())],
            });
        }
    };

    let listed = nul_names(&result.output);
    let paths = listed.names;
    let (invalid_names, opted_out) = repositories.drop_invalid_by_attributes(
        invalid_files(Path::new(dir), &listed.invalid, keep_generated),
        root,
        selection_attributes(keep_generated),
        Source::Worktree,
    );
    let gitlinks = repositories.submodule_paths(root);
    let listed_from = if gitlinks.is_empty() {
        dir.to_owned()
    } else {
        realpath(dir).map_err(|error| unresolved(Some(cwd), dir, &error))?
    };

    let submodules = if gitlinks.is_empty() {
        Vec::new()
    } else {
        paths
            .iter()
            .map(|path| resolve(&listed_from, path))
            .filter(|path| gitlinks.contains(path))
            .collect()
    };

    let mut errors = Vec::new();
    let files: Vec<String> = paths
        .iter()
        .filter(|path| is_candidate(path, keep_generated))
        .map(|path| resolve(dir, path))
        .filter(|path| {
            if let Some(file) = listed_file(&dirname(path), basename(path)) {
                return file;
            }

            match lstat(path) {
                Ok(meta) => meta.is_file(),
                Err(error) => {
                    if !missing(&error) {
                        errors.push(unreadable_entry(cwd, path, &error));
                    }

                    false
                }
            }
        })
        .collect();

    let real = realpath(dir).map_err(|error| unresolved(Some(cwd), dir, &error))?;
    errors.extend(unopened_directories(
        &result.warnings,
        root,
        &real,
        dir,
        cwd,
    ));

    let mut inside = Vec::new();
    for file in &files {
        let real = listed_file_realpath(cwd, file)?;
        if within(root, &real) {
            inside.push(real);
        }
    }

    Ok(Listed {
        files: inside,
        invalid_names,
        opted_out,
        submodules,
        errors,
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PathSelection {
    pub files: Vec<String>,
    pub invalid_names: Vec<PathBuf>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub submodules: Vec<String>,
    pub opted_out: bool,
    pub changed_lines: HashMap<String, Changed>,
}

pub fn collect_files(
    paths: &[String],
    cwd: &str,
    keep_generated: bool,
    repositories: &mut Repositories,
) -> Result<PathSelection, String> {
    let mut by_root: IndexMap<String, Vec<String>> = IndexMap::new();
    let mut outside = Vec::new();
    let mut invalid_names = Vec::new();
    let mut opted_out = false;
    let mut errors = Vec::new();
    let mut submodules = Vec::new();
    let warnings = if has_git() {
        Vec::new()
    } else {
        vec!["git not found on PATH, file selection fell back to the directory walk".to_owned()]
    };

    let mut anchors = Anchors::default();
    for input in paths {
        let path = supplied(cwd, input);
        let kind = lstat(&path).and_then(|meta| {
            let link = meta.is_symlink();
            let meta = if link { fs::metadata(&path)? } else { meta };
            if meta.is_dir() {
                searchable(&path)?;
            }

            Ok((meta, link))
        });

        let (meta, link) = match kind {
            Ok(kind) => kind,
            Err(error) if missing(&error) => {
                errors.push(Failure::new(
                    input.as_str(),
                    format!("no such file: {input}"),
                ));
                continue;
            }
            Err(error) => {
                let line = format!("cannot read {input}: {}", reason(&error));
                errors.push(Failure::new(input.as_str(), line));
                continue;
            }
        };

        if meta.is_dir() {
            if let Some(real) = invalid_real(&path) {
                invalid_names.push(real);
                continue;
            }

            match repositories.locate(&path) {
                Location::Failed(error) => errors.push(Failure::new(input.as_str(), error)),
                Location::Outside => {
                    let walked = fallback_files(&path, cwd, keep_generated);
                    outside.extend(walked.files);
                    invalid_names.extend(walked.invalid_names);
                    errors.extend(walked.errors);
                }
                Location::Repository(root) => {
                    let listed = directory_files(&path, &root, cwd, keep_generated, repositories)?;
                    by_root.entry(root).or_default().extend(listed.files);
                    invalid_names.extend(listed.invalid_names);
                    opted_out |= listed.opted_out;
                    errors.extend(listed.errors);
                    submodules.extend(listed.submodules);
                }
            }

            continue;
        }

        if entry_for(&path).is_none() {
            let line = format!("not a TypeScript or JavaScript file: {input}");
            errors.push(Failure::new(input.as_str(), line));
            continue;
        }

        if !meta.is_file() {
            errors.push(Failure::new(
                input.as_str(),
                format!("not a regular file: {input}"),
            ));
            continue;
        }

        let real = if link {
            file_realpath(Some(cwd), &path)
        } else {
            listed_file_realpath(cwd, &path)
        };

        let file = match real {
            Ok(file) => file,
            Err(error) => match invalid_real(&path) {
                Some(real) => {
                    let anchor = match repositories.locate(&dirname(&path)) {
                        Location::Repository(root) => root,
                        _ => dirname(&path),
                    };

                    if !is_candidate(&anchors.supplied_below(&anchor, &path), keep_generated) {
                        continue;
                    }

                    let (kept, opted) = selected_invalid(real, repositories, keep_generated);
                    invalid_names.extend(kept);
                    opted_out |= opted;
                    continue;
                }
                None => return Err(error),
            },
        };

        let location = repositories.locate(&dirname(&file));
        let anchor = match &location {
            Location::Failed(error) => {
                errors.push(Failure::new(input.as_str(), error.clone()));
                continue;
            }
            Location::Repository(root) => root.clone(),
            Location::Outside => dirname(&file),
        };

        if !anchors.selected_below(&anchor, &path, &file, keep_generated) {
            continue;
        }

        match location {
            Location::Repository(root) => by_root.entry(root).or_default().push(file),
            _ => outside.push(file),
        }
    }

    let mut files = outside;
    let names = selection_attributes(keep_generated);

    for (root, candidates) in by_root {
        match repositories.drop_by_attributes(candidates, &root, names) {
            Ok((kept, out)) => {
                files.extend(kept);
                opted_out |= out;
            }
            Err(error) => errors.push(Failure::new(shown_path(cwd, &root), error)),
        }
    }

    let named: Vec<String> = if submodules.is_empty() {
        Vec::new()
    } else {
        paths
            .iter()
            .map(|input| {
                let path = resolve(cwd, input);
                realpath(&path).unwrap_or(path)
            })
            .collect()
    };

    let skipped = submodules
        .into_iter()
        .filter(|submodule| !named.iter().any(|path| within(submodule, path)))
        .collect();

    invalid_names.sort();
    invalid_names.dedup();
    Ok(PathSelection {
        files: sorted(files),
        invalid_names,
        errors: failure_lines(errors),
        warnings,
        submodules: sorted(skipped),
        opted_out,
        ..PathSelection::default()
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmptyReason {
    OptedOut,
    Generated,
    Ignored,
    Skipped,
    None,
}

struct EmptyWalk<'a> {
    submodules: &'a HashSet<String>,
    saw_supported: bool,
}

impl EmptyWalk<'_> {
    fn candidate(&mut self, path: &str) -> bool {
        if entry_for(path).is_none() {
            return false;
        }

        self.saw_supported = true;
        is_candidate(path, true)
    }

    fn walk(&mut self, dir: &str, root: &str) -> bool {
        let listing = fs::read_dir(dir).and_then(|entries| {
            entries
                .map(|entry| {
                    let entry = entry?;
                    Ok((entry.file_name().into_string(), entry.file_type()))
                })
                .collect::<io::Result<Vec<_>>>()
        });

        let Ok(entries) = listing else {
            return false;
        };

        for (name, kind) in entries {
            let Ok(name) = name else {
                continue;
            };

            let Ok(kind) = kind else {
                continue;
            };

            let path = join(dir, &name);
            let rel = relative(root, &path);
            if kind.is_dir() {
                if name == ".git"
                    || self.submodules.contains(&path)
                    || (self.saw_supported && prunes(&rel))
                {
                    continue;
                }

                if self.walk(&path, root) {
                    return true;
                }
            } else if kind.is_file() && self.candidate(&rel) {
                return true;
            }
        }

        false
    }
}

pub fn empty_reason(
    paths: &[String],
    cwd: &str,
    submodules: &HashSet<String>,
    repositories: &mut Repositories,
) -> Result<EmptyReason, String> {
    let kept = collect_files(paths, cwd, true, repositories)?;
    if !kept.files.is_empty() || !kept.invalid_names.is_empty() {
        return Ok(EmptyReason::Generated);
    }

    if kept.opted_out {
        return Ok(EmptyReason::OptedOut);
    }

    let mut walk = EmptyWalk {
        submodules,
        saw_supported: false,
    };

    for input in paths {
        let path = supplied(cwd, input);
        let Ok(kind) = fs::metadata(&path) else {
            continue;
        };

        if kind.is_file() {
            walk.saw_supported |= entry_for(&path).is_some();
        } else if kind.is_dir() {
            let real = realpath(&path).map_err(|error| unresolved(Some(cwd), &path, &error))?;
            if walk.walk(&real, &real) {
                return Ok(EmptyReason::Ignored);
            }
        }
    }

    Ok(if walk.saw_supported {
        EmptyReason::Skipped
    } else {
        EmptyReason::None
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    OptedOut,
    Excluded,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StdinTarget {
    Format { path: String, root: Option<String> },
    Skip(SkipReason),
    Unsupported(String),
    Unreadable(String),
    Failed(String),
}

fn existing_ancestor(path: &str) -> String {
    let parent = dirname(path);
    if exists(path) || parent == path {
        path.to_owned()
    } else {
        existing_ancestor(&parent)
    }
}

pub fn stdin_target(input: &str, cwd: &str, repositories: &mut Repositories) -> StdinTarget {
    let path = supplied(cwd, input);
    if entry_for(&path).is_none() {
        return StdinTarget::Unsupported(format!("not a TypeScript or JavaScript file: {input}"));
    }

    let directory = existing_ancestor(&dirname(&path));
    let real = match realpath(&directory) {
        Ok(real) => real,
        Err(error) => {
            return StdinTarget::Unreadable(format!("cannot read {input}: {}", reason(&error)));
        }
    };

    let file = join(&real, &relative(&directory, &path));
    let root = match repositories.locate(&real) {
        Location::Failed(error) => return StdinTarget::Failed(error),
        Location::Outside
            if Anchors::default().selected_below(&dirname(&file), &path, &file, false) =>
        {
            return StdinTarget::Format {
                path: file,
                root: None,
            };
        }
        Location::Outside => return StdinTarget::Skip(SkipReason::Excluded),
        Location::Repository(root) => root,
    };

    let skip = |opted_out: bool| {
        StdinTarget::Skip(if opted_out {
            SkipReason::OptedOut
        } else {
            SkipReason::Excluded
        })
    };

    if !Anchors::default().selected_below(&root, &path, &file, false) {
        let named = repositories.drop_by_attributes(vec![file], &root, &["stanza"]);
        return skip(named.is_ok_and(|(_, opted_out)| opted_out));
    }

    match repositories.drop_by_attributes(vec![file.clone()], &root, &SELECTION_ATTRIBUTES) {
        Err(error) => StdinTarget::Failed(error),
        Ok((kept, opted_out)) if kept.is_empty() => skip(opted_out),
        Ok(_) => StdinTarget::Format {
            path: file,
            root: Some(root),
        },
    }
}

pub fn lands_within(root: &str, path: &str) -> bool {
    let mut entry = path.to_owned();
    loop {
        match lstat(&entry) {
            Ok(_) => break,
            Err(error) if error.raw_os_error() != Some(libc::ENOENT) => return false,
            Err(_) if dirname(&entry) == entry => break,
            Err(_) => entry = dirname(&entry),
        }
    }

    realpath(&entry).is_ok_and(|real| within(root, &real))
}

pub fn ignored_by_git(root: &str, path: &str) -> bool {
    let relative = repository_path(root, path);
    run_git(root, &["check-ignore", "-q", "--", &relative]).is_ok()
}

pub fn tracked_in_head(root: &str, path: &str) -> bool {
    let object = format!("HEAD:{}", repository_path(root, path));
    run_git(root, &["cat-file", "-e", &object]).is_ok()
}

#[cfg(test)]
mod regex_parity_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_reads_as_the_system_words_it() {
        let reasons = [libc::EACCES, libc::ENOENT, libc::EISDIR]
            .map(|code| reason(&io::Error::from_raw_os_error(code)));

        assert_eq!(
            reasons,
            [
                "permission denied",
                "no such file or directory",
                "is a directory",
            ]
        );
        assert_eq!(
            reason(&io::Error::other("a config value contains itself")),
            "a config value contains itself"
        );
    }

    #[test]
    fn recognizes_generated_headers() {
        let recognized = [
            "// Code generated by protoc-gen-go. DO NOT EDIT.",
            "// @generated",
            "/* eslint-disable */\n// This file was automatically generated. Do not edit.",
            "// THIS FILE IS AUTOMATICALLY GENERATED. DO NOT EDIT.",
            "/**\n * This file was auto-generated by openapi-typescript.\n * Do not make direct changes to the file.\n */",
            "/* eslint-disable */\n// @ts-nocheck\n\n// noinspection JSUnusedGlobalSymbols\n\n// This file was automatically generated by TanStack Router.",
            "/* tslint:disable */\n/**\n * This file was automatically generated by Payload.\n */",
            "/* THIS FILE WAS GENERATED AUTOMATICALLY BY PAYLOAD. */",
            "// This is an automatically generated file. Do not edit.",
            "/**\n * This class is auto generated by OpenAPI Generator.\n * Do not edit the class manually.\n */",
            "/**\n * Generated by orval v6.31.0\n * Do not edit manually.\n */",
            "// GENERATED CODE -- DO NOT EDIT!",
            "/* eslint-disable */ /* @generated */",
            "// Code generated by sqlc.\n// DO NOT EDIT.",
            "\u{feff}// @generated",
            "// @generated\u{e9}",
        ];

        let ten_lines = format!("{}\n// @generated", ["export {};"; 10].join("\n"));
        let unrecognized = [
            "// Do not edit the order below",
            "// TODO: do not edit this until the API settles",
            "// DO NOT EDIT",
            "/**\n * Generated by Claude Opus 4.6 verified by Undo\n */",
            "// Generated by Claude Opus 4.6 verified by Undo",
            "const note = \"This file was automatically generated\";",
            &ten_lines,
            "export {};",
            "// @generatedness",
            "// \u{17f}his file was generated",
            "// @Generated",
            "/* @GENERATED by tool */",
        ];

        for header in recognized {
            assert!(
                is_generated_header(&format!("{header}\nexport {{}};\n")),
                "{header}"
            );
        }

        for header in unrecognized {
            assert!(
                !is_generated_header(&format!("{header}\nexport {{}};\n")),
                "{header}"
            );
        }
    }
}
