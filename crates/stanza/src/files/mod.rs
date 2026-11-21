mod attributes;
mod changed;
mod codes;
pub mod diff;
mod discovery;
mod git;
pub mod gitignore;
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
use crate::languages::{Candidacy, candidacy, entry_for, is_candidate, prunes};
use crate::paths::{basename, dirname, is_absolute, join, relative, resolve, shown};
use crate::text::{is_js_line_terminator, is_js_whitespace, js_trim, js_trim_start};

use attributes::{Candidates, InvalidKept, SELECTION_ATTRIBUTES, Source, Verdict};
#[cfg(test)]
pub(crate) use git::REFUSED_WRITER;
use git::nul_names;
use walk::fallback_files;

pub use changed::{
    Caller, Staged, StagedFile, Targets, collect_changed, collect_staged, locate_changed,
};
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

const UNREADABLE_FILE_SOURCE: &str = r"^warning: unable to access '(.*)': (.*)";

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
    let mut scan = HeaderScan {
        mode: Mode::Code,
        interpolations: Vec::new(),
        heads: Vec::new(),
        regex_allowed: true,
        after_dot: false,
        control: None,
        comments: Vec::new(),
    };

    for (index, line) in text.split('\n').take(10).enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let hashbang = line
            .trim_start_matches('\u{feff}')
            .strip_prefix("#!")
            .filter(|_| index == 0);

        scan.line(hashbang.map_or(line, |rest| {
            &rest[rest.find(is_js_line_terminator).unwrap_or(rest.len())..]
        }));
    }

    scan.comments
}

const REGEX_KEYWORDS: &[&str] = &[
    "await",
    "break",
    "case",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "in",
    "instanceof",
    "new",
    "return",
    "throw",
    "typeof",
    "void",
    "yield",
];

const CONDITION_KEYWORDS: &[&str] = &["for", "if", "while", "with"];

#[derive(Clone, Copy)]
enum Mode {
    Code,
    Block { counted: bool },
    Quote(char),
    Template,
}

struct HeaderScan<'a> {
    mode: Mode,
    interpolations: Vec<usize>,
    heads: Vec<Option<&'a str>>,
    regex_allowed: bool,
    after_dot: bool,
    control: Option<&'a str>,
    comments: Vec<&'a str>,
}

impl<'a> HeaderScan<'a> {
    fn line(&mut self, line: &'a str) {
        let mut leading = matches!(self.mode, Mode::Code | Mode::Block { counted: true });
        let mut at = line.len() - js_trim_start(line).len();
        while at < line.len() {
            let rest = &line[at..];
            at += match self.mode {
                Mode::Code => self.code(rest, &mut leading),
                Mode::Block { counted } => self.block(rest, counted),
                Mode::Quote(quote) => self.quote(rest, quote),
                Mode::Template => self.template(rest),
            };
        }

        let escapes = line.len() - line.trim_end_matches('\\').len();
        if matches!(self.mode, Mode::Quote(_)) && escapes.is_multiple_of(2) {
            self.close_literal();
        }
    }

    fn code(&mut self, rest: &'a str, leading: &mut bool) -> usize {
        let Some(next) = rest.chars().next() else {
            return 0;
        };

        if is_js_whitespace(next) {
            *leading |= is_js_line_terminator(next);
            return next.len_utf8();
        }

        if let Some(body) = rest.strip_prefix("//") {
            let end = body.find(is_js_line_terminator).unwrap_or(body.len());
            if *leading {
                self.comments.push(comment_body(&body[..end]));
            }

            return 2 + end;
        }

        if rest.starts_with("/*") {
            self.mode = Mode::Block { counted: *leading };
            return 2;
        }

        *leading = false;
        let after_dot = std::mem::take(&mut self.after_dot);
        let control = std::mem::take(&mut self.control);
        let regex_allowed = std::mem::replace(&mut self.regex_allowed, true);
        match next {
            '\'' | '"' => self.mode = Mode::Quote(next),
            '`' => self.mode = Mode::Template,
            '/' if regex_allowed => return 1 + self.regex(&rest[1..]),
            '{' => {
                if let Some(depth) = self.interpolations.last_mut() {
                    *depth += 1;
                }
            }
            '}' => match self.interpolations.last_mut() {
                Some(0) => {
                    self.interpolations.pop();
                    self.mode = Mode::Template;
                }
                Some(depth) => *depth -= 1,
                None => {}
            },
            '(' => self.heads.push(control),
            ')' => self.regex_allowed = self.heads.pop().flatten().is_some(),
            ']' => self.regex_allowed = false,
            '!' => self.regex_allowed = regex_allowed,
            '+' | '-' if rest[1..].starts_with(next) => {
                self.regex_allowed = regex_allowed;
                return 2;
            }
            '.' if rest.starts_with("...") => return 3,
            '.' => self.after_dot = true,
            _ if is_word(next) => {
                let number = next.is_ascii_digit();
                let len = rest
                    .find(|char| !is_word(char) && !(number && char == '.'))
                    .unwrap_or(rest.len());

                let keyword = (!after_dot).then_some(&rest[..len]);
                let for_head = self.heads.last() == Some(&Some("for"));
                self.regex_allowed = keyword.is_some_and(|word| {
                    REGEX_KEYWORDS.contains(&word) || (for_head && word == "of")
                });

                self.control = keyword.filter(|word| CONDITION_KEYWORDS.contains(word));
                return len;
            }
            _ => {}
        }

        next.len_utf8()
    }

    fn block(&mut self, rest: &'a str, counted: bool) -> usize {
        let end = rest.find("*/");
        if counted {
            self.comments
                .push(comment_body(end.map_or(rest, |end| &rest[..end])));
        }

        let Some(end) = end else {
            return rest.len();
        };

        self.mode = Mode::Code;
        end + 2
    }

    fn quote(&mut self, rest: &str, quote: char) -> usize {
        let mut chars = rest.char_indices();
        while let Some((at, char)) = chars.next() {
            if char == '\\' {
                chars.next();
            } else if char == quote {
                self.close_literal();
                return at + 1;
            }
        }

        rest.len()
    }

    fn template(&mut self, rest: &str) -> usize {
        let mut chars = rest.char_indices();
        while let Some((at, char)) = chars.next() {
            match char {
                '\\' => {
                    chars.next();
                }
                '`' => {
                    self.close_literal();
                    return at + 1;
                }
                '$' if rest[at + 1..].starts_with('{') => {
                    self.interpolations.push(0);
                    self.mode = Mode::Code;
                    self.regex_allowed = true;
                    return at + 2;
                }
                _ => {}
            }
        }

        rest.len()
    }

    fn regex(&mut self, body: &str) -> usize {
        self.regex_allowed = false;
        let mut class = false;
        let mut chars = body.char_indices();
        while let Some((at, char)) = chars.next() {
            match char {
                '\\' => {
                    chars.next();
                }
                '[' => class = true,
                ']' => class = false,
                '/' if !class => return at + 1,
                _ => {}
            }
        }

        body.len()
    }

    fn close_literal(&mut self) {
        self.mode = Mode::Code;
        self.regex_allowed = false;
    }
}

fn is_word(char: char) -> bool {
    (matches!(char, '_' | '$' | '#') || char.is_ascii_alphanumeric() || !char.is_ascii())
        && !is_js_whitespace(char)
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

pub(super) struct Unresolved {
    path: String,
    error: io::Error,
}

impl Unresolved {
    fn new(path: &str, error: io::Error) -> Self {
        Unresolved {
            path: path.to_owned(),
            error,
        }
    }

    pub(super) fn line(&self, cwd: Option<&str>) -> String {
        unresolved(cwd, &self.path, &self.error)
    }

    fn failure(&self, cwd: &str) -> Failure {
        Failure::new(shown_path(cwd, &self.path), self.line(Some(cwd)))
    }
}

pub fn missing(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR))
}

pub struct InvalidArguments {
    pub names: Vec<PathBuf>,
    pub arguments: Vec<Seen>,
    pub errors: Vec<String>,
}

pub fn invalid_arguments(
    inputs: &[OsString],
    cwd: &str,
    repositories: &mut Repositories,
) -> InvalidArguments {
    let mut found = InvalidArguments {
        names: Vec::new(),
        arguments: Vec::new(),
        errors: Vec::new(),
    };

    for input in inputs {
        let path = Path::new(cwd).join(input);
        let sorted = match selected_invalid(path.clone(), repositories, false) {
            Ok(sorted) => sorted,
            Err(error) => {
                found.errors.push(error);
                found.arguments.push(Seen::default());
                continue;
            }
        };

        let judged = sorted.opted_out || sorted.generated || !sorted.kept.is_empty();
        found.arguments.push(Seen {
            opted_out: sorted.opted_out,
            generated: sorted.generated,
            kept: !sorted.kept.is_empty(),
            generated_invalid: if judged { Vec::new() } else { vec![path] },
            ..Seen::default()
        });

        found.names.extend(sorted.kept);
    }

    found
}

pub fn invalid_reason(seen: &Seen, repositories: &mut Repositories) -> EmptyReason {
    if seen.opted_out {
        return EmptyReason::OptedOut;
    }

    if seen.generated {
        return EmptyReason::Generated;
    }

    let mut reason = EmptyReason::None;
    for path in &seen.generated_invalid {
        let sorted = selected_invalid(path.clone(), repositories, true)
            .unwrap_or_else(|_| InvalidKept::unjudged(vec![path.clone()]));
        if !sorted.kept.is_empty() {
            return EmptyReason::Generated;
        }

        if sorted.opted_out {
            reason = EmptyReason::OptedOut;
        }
    }

    reason
}

fn selected_invalid(
    path: PathBuf,
    repositories: &mut Repositories,
    keep_generated: bool,
) -> Result<InvalidKept, String> {
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
        return Ok(InvalidKept::unjudged(Vec::new()));
    }

    let Location::Repository(root) = location else {
        return Ok(InvalidKept::unjudged(vec![path]));
    };

    let names = selection_attributes(keep_generated);
    repositories.drop_invalid_by_attributes(vec![path], &root, names, Source::Worktree)
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

fn invalid_files(
    dir: &Path,
    names: &[&[u8]],
    keep_generated: bool,
    links: &mut FolderLinks,
) -> Vec<PathBuf> {
    invalid_candidates(dir, names, keep_generated)
        .filter(|path| {
            !path
                .parent()
                .is_some_and(|parent| links.through_link(parent))
        })
        .filter(|path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_file()))
        .collect()
}

pub fn supplied(cwd: &str, input: &str) -> String {
    let joined = if is_absolute(input) {
        input.to_owned()
    } else {
        format!("{cwd}/{input}")
    };

    let mut path = "/".to_owned();
    let segments: Vec<&str> = joined.split('/').collect();

    for (index, &segment) in segments.iter().enumerate().skip(1) {
        if !matches!(segment, "" | "." | "..") {
            path = format!("{}/{segment}", path.trim_end_matches('/'));
            continue;
        }

        let stepped = enterable(&path).and_then(|()| match segment {
            ".." => parent(&path),
            _ => Ok(path.clone()),
        });

        match stepped {
            Ok(next) => path = next,
            Err(_) => {
                let rest = segments[index..].join("/");
                return format!("{}/{rest}", path.trim_end_matches('/'));
            }
        }
    }

    path
}

// macOS realpath folds `file/..` and `unsearchable/..` without the ENOTDIR and EACCES an open gets.
fn enterable(path: &str) -> io::Result<()> {
    if fs::metadata(path)?.is_dir() {
        access(path, libc::X_OK)
    } else {
        Err(io::Error::from_raw_os_error(libc::ENOTDIR))
    }
}

fn parent(path: &str) -> io::Result<String> {
    if lstat(path)?.is_symlink() {
        Ok(dirname(&realpath(path)?).to_owned())
    } else {
        Ok(dirname(path).to_owned())
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
                let mut current = dir;
                loop {
                    if exists(current) && real_directory(current).is_ok_and(|real| real == anchor) {
                        return relative(current, dir);
                    }

                    let parent = dirname(current);
                    if parent == current {
                        return String::new();
                    }

                    current = parent;
                }
            })
    }

    fn supplied_below(&mut self, anchor: &str, path: &str) -> String {
        let name = basename(path);
        match self.prefix(anchor, dirname(path)) {
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
    ) -> Candidacy {
        candidacy(&repository_path(anchor, file), keep_generated).min(candidacy(
            &self.supplied_below(anchor, supplied),
            keep_generated,
        ))
    }

    fn skipped_folder(&mut self, root: &str, dir: &str, real: &str) -> bool {
        prunes(&repository_path(root, real)) || prunes(self.prefix(root, dir))
    }
}

fn unsearchable(path: &str) -> &str {
    let parent = dirname(path);
    if parent == path || lstat(parent).is_ok() {
        parent
    } else {
        unsearchable(parent)
    }
}

fn file_realpath(path: &str) -> Result<String, Unresolved> {
    realpath(path).or_else(|error| {
        if lstat(path).is_ok_and(|meta| meta.is_file()) {
            let parent = dirname(path);
            let real = realpath(parent).map_err(|error| Unresolved::new(parent, error))?;
            Ok(join(&real, basename(path)))
        } else {
            Err(Unresolved::new(path, error))
        }
    })
}

// realpath returns the name in its on disk case and normalization, so only an exact listing match may join.
// The caller has ruled out a symlink, which realpath would follow.
fn listed_file_realpath(path: &str) -> Result<String, Unresolved> {
    let dir = dirname(path);
    let name = basename(path);
    if !listed(dir, name) {
        return file_realpath(path);
    }

    let real = real_directory(dir).map_err(|error| Unresolved::new(dir, error))?;
    Ok(join(&real, name))
}

struct FolderLinks<'a> {
    anchor: &'a Path,
    linked: HashMap<PathBuf, bool>,
}

impl<'a> FolderLinks<'a> {
    fn new(anchor: &'a str) -> Self {
        Self {
            anchor: Path::new(anchor),
            linked: HashMap::new(),
        }
    }

    fn through_link(&mut self, dir: impl AsRef<Path>) -> bool {
        let dir = dir.as_ref();
        if let Some(&linked) = self.linked.get(dir) {
            return linked;
        }

        let Some(parent) = dir
            .parent()
            .filter(|_| dir != self.anchor && dir.starts_with(self.anchor))
        else {
            return false;
        };

        let linked = self.through_link(parent)
            || fs::symlink_metadata(dir).is_ok_and(|meta| meta.is_symlink());

        self.linked.insert(dir.to_owned(), linked);
        linked
    }
}

static UNOPENED_DIRECTORY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(UNOPENED_DIRECTORY_SOURCE).expect("the warning pattern compiles"));

fn unreadable_entry(cwd: &str, path: &str, error: &io::Error) -> Failure {
    let shown = shown_path(cwd, unsearchable(path));
    let line = format!("cannot read {shown}: {}", reason(error));
    Failure::new(shown, line)
}

static UNREADABLE_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(UNREADABLE_FILE_SOURCE).expect("the warning pattern compiles"));

#[derive(Clone, Copy)]
enum Unread {
    Directory,
    IgnoreFile,
}

struct UnreadEntry<'a> {
    kind: Unread,
    path: &'a str,
    reason: &'a str,
}

impl<'a> UnreadEntry<'a> {
    fn new(kind: Unread, captures: &regex_lite::Captures<'a>) -> Self {
        let group = |index| captures.get(index).map_or("", |group| group.as_str());
        Self {
            kind,
            path: group(1),
            reason: group(2),
        }
    }
}

fn unread_entry(line: &str) -> Option<UnreadEntry<'_>> {
    if let Some(captures) = UNOPENED_DIRECTORY.captures(line) {
        return Some(UnreadEntry::new(Unread::Directory, &captures));
    }

    let unreadable = UnreadEntry::new(Unread::IgnoreFile, &UNREADABLE_FILE.captures(line)?);
    let ignore_file = !is_absolute(unreadable.path) && basename(unreadable.path) == ".gitignore";

    // git opens an ignore file with O_NOFOLLOW and skips a symlinked one after this warning, as the walk does.
    let symlinked =
        unreadable.reason.to_lowercase() == reason(&io::Error::from_raw_os_error(libc::ELOOP));

    (ignore_file && !symlinked).then_some(unreadable)
}

fn opted_in_folder(repositories: &mut Repositories, root: &str, folder: &str) -> bool {
    // The check reads the attribute files above a path, and git reads a folder's own only when it can enter it.
    let probe = if access(folder, libc::X_OK).is_ok() {
        join(folder, "*")
    } else {
        folder.to_owned()
    };

    repositories.may_opt_in(&[probe], root)
}

// git masks control characters as `?`, so a folder named `c\rr` comes back as `c?r`, which does not exist, and git's reason is the only one left.
fn unread_by_git(
    warnings: &str,
    root: &str,
    real: &str,
    dir: &str,
    cwd: &str,
    mut skipped: impl FnMut(&str) -> bool,
) -> Vec<Failure> {
    let mut failures = Vec::new();
    for unread in warnings.split('\n').filter_map(unread_entry) {
        let below = relative(real, &resolve(root, unread.path));
        let folder = match unread.kind {
            Unread::Directory => below.as_str(),
            Unread::IgnoreFile => dirname(&below),
        };

        if skipped(folder) {
            continue;
        }

        let entry = join(dir, &below);
        if let Unread::IgnoreFile = unread.kind
            && let Err(error) = lstat(&entry)
            && !missing(&error)
        {
            failures.push(unreadable_entry(cwd, &entry, &error));
            continue;
        }

        let shown = shown_path(cwd, &entry);
        let text = format!("cannot read {shown}");
        let readable = match unread.kind {
            Unread::Directory => searchable(&entry),
            Unread::IgnoreFile => access(&entry, libc::R_OK),
        };

        let line = match readable {
            Err(error) if !missing(&error) => format!("{text}: {}", reason(&error)),
            _ => format!("{text}: {}", unread.reason.to_lowercase()),
        };

        failures.push(Failure::new(shown, line));
    }

    failures
}

struct Listed {
    files: Candidates,
    invalid_names: Vec<PathBuf>,
    seen: Seen,
    submodules: Vec<String>,
    errors: Vec<Failure>,
}

impl Listed {
    fn failed(failure: Failure) -> Self {
        Listed {
            files: Candidates::default(),
            invalid_names: Vec::new(),
            seen: Seen::default(),
            submodules: Vec::new(),
            errors: vec![failure],
        }
    }
}

fn listed_regular(path: &str) -> io::Result<bool> {
    match listed_file(dirname(path), basename(path)) {
        Some(regular) => Ok(regular),
        None => lstat(path).map(|meta| meta.is_file()),
    }
}

fn generated_name(path: &str) -> bool {
    candidacy(path, false) == Candidacy::No && candidacy(path, true) != Candidacy::No
}

fn directory_files(
    dir: &str,
    root: &str,
    argument: usize,
    cwd: &str,
    keep_generated: bool,
    repositories: &mut Repositories,
    anchors: &mut Anchors,
) -> Listed {
    let listing = repositories.directory_listing(dir);
    let result = match &*listing {
        Ok(result) => result,
        Err(error) => return Listed::failed(Failure::new(shown_path(cwd, dir), error.clone())),
    };

    let real = match realpath(dir) {
        Ok(real) => real,
        Err(error) => {
            let line = unresolved(Some(cwd), dir, &error);
            return Listed::failed(Failure::new(shown_path(cwd, dir), line));
        }
    };
    let below_skipped = anchors.skipped_folder(root, dir, &real);
    let ceiling = if below_skipped {
        Candidacy::InSkippedFolder
    } else {
        Candidacy::Yes
    };

    let listed = nul_names(&result.output);
    let paths = listed.names;
    let invalid: &[&[u8]] = if below_skipped { &[] } else { &listed.invalid };
    let mut links = FolderLinks::new(dir);
    let mut errors = Vec::new();
    let invalid_kept = match repositories.drop_invalid_by_attributes(
        invalid_files(Path::new(dir), invalid, keep_generated, &mut links),
        root,
        selection_attributes(keep_generated),
        Source::Worktree,
    ) {
        Ok(sorted) => sorted,
        Err(error) => {
            errors.push(Failure::new(shown_path(cwd, dir), error));
            InvalidKept::unjudged(Vec::new())
        }
    };

    let gitlinks = repositories.submodule_paths(root);
    let listed_from = if gitlinks.is_empty() { dir } else { &real };

    let submodules = if gitlinks.is_empty() {
        Vec::new()
    } else {
        paths
            .iter()
            .map(|path| resolve(listed_from, path))
            .filter(|path| gitlinks.contains(path))
            .collect()
    };

    let nested = paths
        .iter()
        .filter_map(|path| path.strip_suffix('/'))
        .filter(|path| !below_skipped && !prunes(path))
        .map(|path| resolve(&real, path))
        .collect();

    let listed_folders = if keep_generated {
        Vec::new()
    } else {
        vec![ListedFolder {
            dir: dir.to_owned(),
            root: root.to_owned(),
            real: real.clone(),
            ceiling,
        }]
    };

    let classified: Vec<(&str, Candidacy)> = paths
        .iter()
        .map(|path| (*path, candidacy(path, keep_generated).min(ceiling)))
        .filter(|(_, candidacy)| *candidacy != Candidacy::No)
        .collect();

    let in_skipped_folder: Vec<String> = classified
        .iter()
        .filter(|(_, candidacy)| *candidacy == Candidacy::InSkippedFolder)
        .map(|(path, _)| resolve(&real, path))
        .collect();

    let opt_in = repositories.may_opt_in(&in_skipped_folder, root);
    let files: Vec<(String, Candidacy)> = classified
        .into_iter()
        .filter(|(_, candidacy)| opt_in || *candidacy == Candidacy::Yes)
        .map(|(path, candidacy)| (resolve(dir, path), candidacy))
        .filter(|(path, _)| {
            if links.through_link(dirname(path)) {
                return false;
            }

            match listed_regular(path) {
                Ok(regular) => regular,
                Err(error) => {
                    if !missing(&error) {
                        errors.push(unreadable_entry(cwd, path, &error));
                    }

                    false
                }
            }
        })
        .collect();

    errors.extend(unread_by_git(
        &result.warnings,
        root,
        &real,
        dir,
        cwd,
        |below| {
            (below_skipped || prunes(below))
                && !opted_in_folder(repositories, root, &resolve(&real, below))
        },
    ));

    let mut inside = Candidates::default();
    for (file, candidacy) in files {
        match listed_file_realpath(&file) {
            Ok(real) if within(root, &real) => inside.add(real, candidacy, false, argument),
            Ok(_) => {}
            Err(unresolved) if missing(&unresolved.error) => {}
            Err(unresolved) => errors.push(unresolved.failure(cwd)),
        }
    }

    Listed {
        files: inside,
        invalid_names: invalid_kept.kept,
        seen: Seen {
            opted_out: invalid_kept.opted_out,
            generated: invalid_kept.generated,
            listed_folders,
            repositories: nested,
            ..Seen::default()
        },
        submodules,
        errors,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedFolder {
    dir: String,
    root: String,
    real: String,
    ceiling: Candidacy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedName {
    root: String,
    path: String,
    candidacy: Candidacy,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub opted_out: bool,
    pub generated: bool,
    pub generated_names: Vec<GeneratedName>,
    pub generated_invalid: Vec<PathBuf>,
    pub listed_folders: Vec<ListedFolder>,
    pub repositories: Vec<String>,
    pub submodules: Vec<String>,
    pub kept: bool,
}

impl Seen {
    fn extend(&mut self, other: Seen) {
        self.opted_out |= other.opted_out;
        self.generated |= other.generated;
        self.generated_names.extend(other.generated_names);
        self.generated_invalid.extend(other.generated_invalid);
        self.listed_folders.extend(other.listed_folders);
        self.repositories.extend(other.repositories);
        self.submodules.extend(other.submodules);
        self.kept |= other.kept;
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct PathSelection {
    pub files: Vec<String>,
    pub invalid_names: Vec<PathBuf>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub submodules: Vec<String>,
    pub opted_out: bool,
    pub arguments: Vec<Seen>,
    pub nested: Vec<String>,
    pub changed_lines: HashMap<String, Changed>,
}

pub fn collect_files(
    paths: &[String],
    cwd: &str,
    keep_generated: bool,
    repositories: &mut Repositories,
) -> PathSelection {
    let mut by_root: IndexMap<String, Candidates> = IndexMap::new();
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

    let mut seen = vec![Seen::default(); paths.len()];
    let mut anchors = Anchors::default();
    for (argument, input) in paths.iter().enumerate() {
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

            let listings = match repositories.locate(&path) {
                Location::Failed(error) => {
                    errors.push(Failure::new(input.as_str(), error));
                    Vec::new()
                }
                Location::Outside => match realpath(&path) {
                    Ok(real) => {
                        let walked =
                            fallback_files(&path, &real, cwd, keep_generated, repositories);
                        seen[argument].kept |= !walked.files.is_empty();
                        seen[argument].generated |= walked.generated;
                        outside.extend(walked.files);
                        invalid_names.extend(walked.invalid_names);
                        errors.extend(walked.errors);
                        walked.nested
                    }
                    Err(error) => {
                        let line = format!("cannot read {input}: {}", reason(&error));
                        errors.push(Failure::new(input.as_str(), line));
                        Vec::new()
                    }
                },
                Location::Repository(root) => vec![(path, root)],
            };

            for (dir, root) in listings {
                let listed = directory_files(
                    &dir,
                    &root,
                    argument,
                    cwd,
                    keep_generated,
                    repositories,
                    &mut anchors,
                );
                by_root.entry(root).or_default().extend(listed.files);
                invalid_names.extend(listed.invalid_names);
                opted_out |= listed.seen.opted_out;
                seen[argument].extend(listed.seen);
                seen[argument]
                    .submodules
                    .extend(listed.submodules.iter().cloned());
                errors.extend(listed.errors);
                submodules.extend(listed.submodules);
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
            file_realpath(&path)
        } else {
            listed_file_realpath(&path)
        };

        let file = match real {
            Ok(file) => file,
            Err(error) => match invalid_real(&path) {
                Some(real) => {
                    let anchor = match repositories.locate(dirname(&path)) {
                        Location::Repository(root) => root,
                        _ => dirname(&path).to_owned(),
                    };

                    let below = anchors.supplied_below(&anchor, &path);
                    if !is_candidate(&below, keep_generated) {
                        if !keep_generated && generated_name(&below) && is_candidate(&below, true) {
                            seen[argument].generated_invalid.push(real);
                        }

                        continue;
                    }

                    let sorted = match selected_invalid(real, repositories, keep_generated) {
                        Ok(sorted) => sorted,
                        Err(error) => {
                            errors.push(Failure::new(input.as_str(), error));
                            continue;
                        }
                    };

                    opted_out |= sorted.opted_out;
                    seen[argument].opted_out |= sorted.opted_out;
                    seen[argument].generated |= sorted.generated;
                    seen[argument].kept |= !sorted.kept.is_empty();
                    invalid_names.extend(sorted.kept);
                    continue;
                }
                None => {
                    errors.push(Failure::new(input.as_str(), error.line(Some(cwd))));
                    continue;
                }
            },
        };

        let location = repositories.locate(dirname(&file));
        let anchor = match &location {
            Location::Failed(error) => {
                errors.push(Failure::new(input.as_str(), error.clone()));
                continue;
            }
            Location::Repository(root) => root.clone(),
            Location::Outside => dirname(&file).to_owned(),
        };

        let candidacy = anchors.selected_below(&anchor, &path, &file, keep_generated);
        let if_generated = match candidacy {
            Candidacy::No if !keep_generated => anchors.selected_below(&anchor, &path, &file, true),
            _ => Candidacy::No,
        };

        match location {
            Location::Repository(root) if if_generated != Candidacy::No => {
                seen[argument].generated_names.push(GeneratedName {
                    root,
                    path: file,
                    candidacy: if_generated,
                });
            }
            Location::Repository(root) => {
                let candidates = by_root.entry(root).or_default();
                candidates.add(file, candidacy, false, argument);
            }
            _ if if_generated == Candidacy::Yes => seen[argument].generated = true,
            _ if candidacy == Candidacy::Yes => {
                seen[argument].kept = true;
                outside.push(file);
            }
            _ => {}
        }
    }

    let mut files = outside;
    let names = selection_attributes(keep_generated);

    for (root, candidates) in by_root {
        let verdicts = match repositories.verdicts_by_attributes(candidates, &root, names) {
            Ok(verdicts) => verdicts,
            Err(error) => {
                errors.push(Failure::new(shown_path(cwd, &root), error));
                continue;
            }
        };

        for (candidate, verdict) in verdicts {
            let seen = &mut seen[candidate.argument];
            match verdict {
                Verdict::Kept => {
                    seen.kept = true;
                    files.push(candidate.file);
                }
                Verdict::OptedOut => {
                    opted_out = true;
                    seen.opted_out = true;
                }
                Verdict::Generated => seen.generated = true,
                Verdict::Excluded => {}
            }
        }
    }

    let nested = seen.iter().any(|seen| !seen.repositories.is_empty());
    let named: Vec<String> = if submodules.is_empty() && !nested {
        Vec::new()
    } else {
        paths
            .iter()
            .map(|input| {
                let path = supplied(cwd, input);
                realpath(&path).unwrap_or(path)
            })
            .collect()
    };

    let unnamed = |repository: &&String| !named.iter().any(|path| within(repository, path));
    let skipped = submodules.iter().filter(unnamed).cloned().collect();
    let nested = seen
        .iter()
        .flat_map(|seen| &seen.repositories)
        .filter(unnamed)
        .cloned()
        .collect();

    invalid_names.sort();
    invalid_names.dedup();
    PathSelection {
        files: sorted(files),
        invalid_names,
        errors: failure_lines(errors),
        warnings,
        submodules: sorted(skipped),
        opted_out,
        arguments: seen,
        nested: sorted(nested),
        ..PathSelection::default()
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum EmptyReason {
    OptedOut,
    Generated,
    Ignored,
    Hidden,
    Skipped,
    None,
}

struct EmptyWalk<'a> {
    boundaries: &'a HashSet<&'a str>,
    saw_supported: bool,
}

impl EmptyWalk<'_> {
    fn candidate(&mut self, path: &str, below_skipped: bool) -> bool {
        if entry_for(path).is_none() {
            return false;
        }

        self.saw_supported = true;
        !below_skipped && is_candidate(path, true)
    }

    fn walk(&mut self, dir: &str, below: &str, below_skipped: bool) -> bool {
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

            let file = kind.is_file();
            if !(kind.is_dir() || file && entry_for(&name).is_some()) || name == ".git" {
                continue;
            }

            let rel = if below.is_empty() {
                name
            } else {
                format!("{below}/{name}")
            };

            if file {
                if self.candidate(&rel, below_skipped) {
                    return true;
                }

                continue;
            }

            let path = join(dir, basename(&rel));
            if self.boundaries.contains(path.as_str())
                || (self.saw_supported && (below_skipped || prunes(&rel)))
            {
                continue;
            }

            if self.walk(&path, &rel, below_skipped) {
                return true;
            }
        }

        false
    }
}

#[derive(Clone, Copy, Default)]
struct GeneratedVerdicts {
    generated: bool,
    opted_out: bool,
}

fn listed_generated(
    folder: &ListedFolder,
    repositories: &mut Repositories,
) -> (Vec<(String, Candidacy)>, Vec<PathBuf>) {
    let listing = repositories.directory_listing(&folder.dir);
    let Ok(result) = &*listing else {
        return (Vec::new(), Vec::new());
    };

    let listed = nul_names(&result.output);
    let mut links = FolderLinks::new(&folder.dir);
    let names = listed
        .names
        .iter()
        .filter(|path| generated_name(path))
        .filter_map(|path| {
            let file = resolve(&folder.dir, path);
            let regular = !links.through_link(dirname(&file))
                && listed_regular(&file).is_ok_and(|regular| regular);

            let real = listed_file_realpath(&file)
                .ok()
                .filter(|real| regular && within(&folder.root, real))?;

            Some((real, candidacy(path, true).min(folder.ceiling)))
        })
        .collect();

    let invalid: Vec<&[u8]> = listed
        .invalid
        .iter()
        .copied()
        .filter(|name| {
            folder.ceiling == Candidacy::Yes && generated_name(&String::from_utf8_lossy(name))
        })
        .collect();

    let invalid = invalid_files(Path::new(&folder.dir), &invalid, true, &mut links);
    (names, invalid)
}

fn generated_verdicts(
    selection: &PathSelection,
    repositories: &mut Repositories,
) -> Vec<GeneratedVerdicts> {
    let mut verdicts = vec![GeneratedVerdicts::default(); selection.arguments.len()];
    let mut by_root: IndexMap<&str, Candidates> = IndexMap::new();
    for (argument, seen) in selection.arguments.iter().enumerate() {
        if seen.opted_out || seen.generated {
            continue;
        }

        for folder in &seen.listed_folders {
            let (names, invalid) = listed_generated(folder, repositories);
            let candidates = by_root.entry(&folder.root).or_default();
            for (path, candidacy) in names {
                candidates.add(path, candidacy, true, argument);
            }

            let sorted = repositories
                .drop_invalid_by_attributes(
                    invalid.clone(),
                    &folder.root,
                    &["stanza"],
                    Source::Worktree,
                )
                .unwrap_or_else(|_| InvalidKept::unjudged(invalid));

            verdicts[argument].generated |= !sorted.kept.is_empty();
            verdicts[argument].opted_out |= sorted.opted_out;
        }

        for name in &seen.generated_names {
            if lstat(&name.path).is_ok_and(|meta| meta.is_file()) {
                let candidates = by_root.entry(&name.root).or_default();
                candidates.add(name.path.clone(), name.candidacy, true, argument);
            }
        }

        for path in &seen.generated_invalid {
            let sorted = selected_invalid(path.clone(), repositories, true)
                .unwrap_or_else(|_| InvalidKept::unjudged(vec![path.clone()]));
            verdicts[argument].generated |= !sorted.kept.is_empty();
            verdicts[argument].opted_out |= sorted.opted_out;
        }
    }

    for (root, candidates) in by_root {
        let Ok(sorted) = repositories.verdicts_by_attributes(candidates, root, &["stanza"]) else {
            continue;
        };

        for (candidate, verdict) in sorted {
            let found = &mut verdicts[candidate.argument];
            match verdict {
                Verdict::Generated => found.generated = true,
                Verdict::OptedOut => found.opted_out = true,
                Verdict::Kept | Verdict::Excluded => {}
            }
        }
    }

    verdicts
}

pub fn empty_reasons(
    paths: &[String],
    cwd: &str,
    selection: &PathSelection,
    repositories: &mut Repositories,
) -> Result<Vec<Option<EmptyReason>>, String> {
    let arguments = paths
        .iter()
        .map(|input| {
            let path = supplied(cwd, input);
            match fs::metadata(&path) {
                Ok(kind) => Ok((path, kind)),
                Err(error) => Err(unresolved(Some(cwd), &path, &error)),
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let boundaries: HashSet<&str> = selection
        .arguments
        .iter()
        .flat_map(|seen| seen.repositories.iter().chain(&seen.submodules))
        .map(String::as_str)
        .collect();

    let generated = generated_verdicts(selection, repositories);
    let mut anchors = Anchors::default();
    let mut reasons = Vec::new();
    for (((path, kind), seen), generated) in arguments
        .into_iter()
        .zip(&selection.arguments)
        .zip(generated)
    {
        if seen.opted_out {
            reasons.push(Some(EmptyReason::OptedOut));
            continue;
        }

        if seen.generated || generated.generated {
            reasons.push(Some(EmptyReason::Generated));
            continue;
        }

        if generated.opted_out {
            reasons.push(Some(EmptyReason::OptedOut));
            continue;
        }

        let mut walk = EmptyWalk {
            boundaries: &boundaries,
            saw_supported: false,
        };

        let mut bounded = false;
        if kind.is_file() {
            walk.saw_supported = entry_for(&path).is_some();
        } else if kind.is_dir() {
            let real = realpath(&path).map_err(|error| unresolved(Some(cwd), &path, &error))?;
            let (ignored, below_skipped) = match repositories.locate(&path) {
                Location::Repository(root) => (
                    EmptyReason::Ignored,
                    anchors.skipped_folder(&root, &path, &real),
                ),
                _ => (EmptyReason::Hidden, false),
            };

            if walk.walk(&real, "", below_skipped) {
                reasons.push(Some(ignored));
                continue;
            }

            bounded = selection
                .submodules
                .iter()
                .chain(&selection.nested)
                .any(|boundary| within(&real, boundary));
        }

        reasons.push(if walk.saw_supported {
            Some(EmptyReason::Skipped)
        } else if bounded {
            None
        } else {
            Some(EmptyReason::None)
        });
    }

    Ok(reasons)
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

fn existing_ancestor(path: &str) -> &str {
    let parent = dirname(path);
    if exists(path) || parent == path {
        path
    } else {
        existing_ancestor(parent)
    }
}

pub fn stdin_target(input: &str, cwd: &str, repositories: &mut Repositories) -> StdinTarget {
    let path = supplied(cwd, input);
    if entry_for(&path).is_none() {
        return StdinTarget::Unsupported(format!(
            "not a TypeScript or JavaScript file: {}",
            shown(input)
        ));
    }

    let directory = existing_ancestor(dirname(&path));
    let real = match realpath(directory) {
        Ok(real) => real,
        Err(error) => {
            return StdinTarget::Unreadable(format!(
                "cannot read {}: {}",
                shown(input),
                reason(&error)
            ));
        }
    };

    let file = join(&real, &relative(directory, &path));
    let root = match repositories.locate(&real) {
        Location::Failed(error) => return StdinTarget::Failed(error),
        Location::Outside
            if Anchors::default().selected_below(dirname(&file), &path, &file, false)
                == Candidacy::Yes =>
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

    let candidacy = Anchors::default().selected_below(&root, &path, &file, false);
    if candidacy == Candidacy::No {
        let mut skipped = Candidates::default();
        skipped.push(file, Candidacy::Yes);

        let named = repositories.drop_by_attributes(skipped, &root, &["stanza"]);
        return skip(named.is_ok_and(|(_, opted_out)| opted_out));
    }

    let mut candidates = Candidates::default();
    candidates.push(file.clone(), candidacy);
    match repositories.drop_by_attributes(candidates, &root, &SELECTION_ATTRIBUTES) {
        Err(error) => StdinTarget::Failed(error),
        Ok((kept, opted_out)) if kept.is_empty() => skip(opted_out),
        Ok(_) => StdinTarget::Format {
            path: file,
            root: Some(root),
        },
    }
}

pub fn lands_within(root: &str, path: &str) -> bool {
    let mut entry = path;
    loop {
        match lstat(entry) {
            Ok(_) => break,
            Err(error) if error.raw_os_error() != Some(libc::ENOENT) => return false,
            Err(_) if dirname(entry) == entry => break,
            Err(_) => entry = dirname(entry),
        }
    }

    realpath(entry).is_ok_and(|real| within(root, &real))
}

#[cfg(test)]
mod regex_parity_tests;

#[cfg(test)]
mod selection_tests;

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
    fn git_warnings_name_the_folders_and_ignore_files_it_could_not_read() {
        let warnings = [
            "warning: unable to access 'sub/.gitignore': Permission denied",
            "warning: unable to access 'link/.gitignore': Too many levels of symbolic links",
            "warning: unable to access '/home/user/.gitignore': Permission denied",
            "warning: unable to access '.git/info/exclude': Permission denied",
            "warning: unable to access 'sub/.gitattributes': Permission denied",
            "warning: could not open directory 'c?r/': Permission denied",
            "warning: unable to access 'dist/.gitignore': Permission denied",
        ]
        .join("\n");

        let failures = unread_by_git(
            &warnings,
            "/absent",
            "/absent",
            "/absent",
            "/absent",
            |folder| folder == "dist",
        );

        let lines: Vec<&str> = failures
            .iter()
            .map(|failure| failure.line.as_str())
            .collect();
        assert_eq!(
            lines,
            [
                "cannot read sub/.gitignore: permission denied",
                "cannot read c?r: permission denied",
            ]
        );
    }

    #[test]
    fn a_supplied_path_steps_up_where_the_system_does() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = std::env::temp_dir().join(format!("stanza-supplied-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("real/inner")).unwrap();
        fs::create_dir(dir.join("locked")).unwrap();
        symlink("real/inner", dir.join("link")).unwrap();
        fs::write(dir.join("a.ts"), "").unwrap();

        let root = realpath(&dir.to_string_lossy()).unwrap();
        let real = format!("{root}/real/a.ts");
        let linked = format!("{root}/link");
        let typed = |input: &str| format!("{root}/{input}");

        assert_eq!(supplied(&root, "link/../a.ts"), real);
        assert_eq!(supplied("/", &typed("link/../a.ts")), real);
        assert_eq!(supplied(&linked, "../a.ts"), real);
        assert_eq!(supplied(&format!("{linked}/.."), "a.ts"), real);
        assert_eq!(supplied(&root, "./real//inner/../a.ts"), real);
        assert_eq!(supplied(&root, "link/"), linked);
        for unopened in ["missing/../a.ts", "a.ts/../a.ts", "a.ts/", "a.ts/."] {
            assert_eq!(supplied(&root, unopened), typed(unopened));
        }

        let locked = typed("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let refused = access(&locked, libc::X_OK).is_err();
        let unsearchable = supplied(&root, "locked/../a.ts");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if refused {
            assert_eq!(unsearchable, typed("locked/../a.ts"));
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_a_linked_folder_below_the_anchor_counts() {
        use std::os::unix::fs::symlink;

        let dir = std::env::temp_dir().join(format!("stanza-linked-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Real/deep")).unwrap();
        symlink("Real", dir.join("link")).unwrap();

        let root = realpath(&dir.to_string_lossy()).unwrap();
        let at = |path: &str| join(&root, path);
        let folded = fs::metadata(at("real")).is_ok();

        let mut links = FolderLinks::new(&root);
        assert!(!links.through_link(&root));
        assert!(!links.through_link(at("Real/deep")));
        assert!(links.through_link(at("link/deep")));
        assert!(links.through_link(at("link")));
        assert!(!links.through_link(dirname(&root)));
        if folded {
            assert!(!links.through_link(at("real/DEEP")));
        }

        let linked = at("link");
        assert!(!FolderLinks::new(&linked).through_link(at("link/deep")));

        fs::remove_dir_all(&dir).unwrap();
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

    #[test]
    fn counts_only_real_header_comments() {
        let literal = [
            "const sample = `\n// @generated\n`;",
            "const single = 'start\\\n// @generated';",
            "const double = \"start\\\n// @generated\";",
            "const crlf = 'start\\\r\n// @generated';",
            "const pattern = /\\/\\/ @generated/;",
            "const sample = `${\n  /`/.source\n}\n// @generated\n`;",
            "const sample = `${\"`\"}\n// @generated\n`;",
            "const sample = `${'}'}\n// @generated\n`;",
            "const sample = `${{ a: 1 }.a}\n// @generated\n`;",
            "const sample = `${`${inner}`}\n// @generated\n`;",
            "const sample = `\\`\n// @generated\n`;",
            "const sample = `\n/* @generated */\n`;",
            "const half = total! / 2; const sample = `\n// @generated\n`;",
            "const of = 2;\nconst sample = `${of / '/'}\n// @generated\n`;",
            "const chars = [...typeof /`/];\nconst sample = `\n// @generated\n`;",
            "// note\rconst sample = `\n// @generated\n`;",
            "// note\u{2028}const sample = `\n// @generated\n`;",
            "export default /`/;\nconst sample = `\n// @generated\n`;",
            "#!/usr/bin/env node\rconst sample = `\n// @generated\n`;",
            "run(); /*\n@generated */",
        ];

        let comment = [
            "const sample = `x\ny`;\n// @generated",
            "const sample = `${\n/* @generated */ value}`;",
            "const sample = `${\n// @generated\nvalue}`;",
            "const tick = /`/;\n// @generated",
            "const tick = /[/`]/g;\n// @generated",
            "const tick = \"`\";\n// @generated",
            "const tick = '`';\n// @generated",
            "const tick = `\\``;\n// @generated",
            "/* ` */\n// @generated",
            "// `\n// @generated",
            "const broken = 'start\n// @generated",
            "const half = total / 2; const tick = '`';\n// @generated",
            "const half = x.return / 2, tick = '/`';\n// @generated",
            "const half = returnValue / 2, tick = '/`';\n// @generated",
            "function tick() { return /`/; }\n// @generated",
            "const path = 'a\\\\\n// @generated",
            "}} const tick = '`';\n// @generated",
            "#!/usr/bin/env node `\n// @generated",
            "\u{feff}#!/usr/bin/env node `\n// @generated",
            "const half = total! / 2, tick = '/`';\n// @generated",
            "const half = this.#delete / 2, tick = '/`';\n// @generated",
            "const half = 1. / 2, tick = '/`';\n// @generated",
            "debugger\n/`/.test(\"\");\n// @generated",
            "if (ready) /`/.test(input);\n// @generated",
            "for (const x of /`/.source) {}\n// @generated",
            "for (const x of (of) / 2) {} const tick = '/`';\n// @generated",
            "while (ready) {\n  if (done) break\n  /`/.test(input);\n}\n// @generated",
            "// note\rconst n = 1;\r// @generated",
            "const half = total++ / 2, tick = '/`';\n// @generated",
            "const half = total-- / 2, tick = '/`';\n// @generated",
            "let count = 0; ++count; const tick = /`/;\n// @generated",
            "// note\r// @generated",
            "// note\u{2028}// @generated",
            "while (next()) /`/.test(input);\n// @generated",
            "const half = (total) / 2, tick = '/`';\n// @generated",
            "const half = x.if(total) / 2, tick = '/`';\n// @generated",
            "/* eslint-disable */ /*\n * @generated\n */",
        ];

        for header in literal {
            assert!(
                !is_generated_header(&format!("{header}\nexport {{}};\n")),
                "{header}"
            );
        }

        for header in comment {
            assert!(
                is_generated_header(&format!("{header}\nexport {{}};\n")),
                "{header}"
            );
        }
    }
}
