use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::panic;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::thread;

use super::index::{PrivateIndex, TrackedAttributes};
use super::{access, exists, lstat, lstat_if_present, realpath};
use crate::paths::{dirname, is_absolute, join, resolve};
use crate::text::js_trim;

static GIT_PATH: LazyLock<Option<String>> = LazyLock::new(|| {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("git").to_string_lossy().into_owned())
        .find(|candidate| {
            fs::metadata(candidate).is_ok_and(|meta| meta.is_file())
                && access(candidate, libc::X_OK).is_ok()
        })
});

pub fn has_git() -> bool {
    GIT_PATH.is_some()
}

pub(super) fn git_path() -> &'static str {
    GIT_PATH.as_deref().unwrap_or("git")
}

pub(super) struct GitOutput {
    pub(super) output: Vec<u8>,
    pub(super) warnings: String,
}

fn failure_reason(stderr: &str, status: ExitStatus) -> String {
    stderr
        .split('\n')
        .map(js_trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| match status.code() {
            Some(code) => format!("exit {code}"),
            None => "exit null".to_owned(),
        })
}

#[derive(Clone, PartialEq, Eq)]
pub(super) enum GitEnv {
    Inherited,
    CLocale,
    NoDiffOpts,
    WorktreeDiff(Option<PathBuf>),
    DefaultEmail,
}

static SPAWNING: Mutex<()> = Mutex::new(());

fn git_command(cwd: &str, args: &[&str], env: GitEnv) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(cwd).args(args);
    match env {
        GitEnv::Inherited => {}
        GitEnv::CLocale => {
            command.env("LC_ALL", "C");
        }
        GitEnv::DefaultEmail => {
            // With no configured email, git var -l derives one from a dotless hostname through a DNS lookup that stalls until the resolver times out.
            command.env("EMAIL", "stanza@localhost");
        }
        GitEnv::NoDiffOpts => {
            command.env_remove("GIT_DIFF_OPTS");
        }
        GitEnv::WorktreeDiff(index) => {
            // git diff runs status in each submodule with GIT_INDEX_FILE unset, and only this keeps that status from refreshing the submodule's index.
            command
                .env_remove("GIT_DIFF_OPTS")
                .env("GIT_OPTIONAL_LOCKS", "0");

            if let Some(index) = index {
                command.env("GIT_INDEX_FILE", index);
            }
        }
    }

    command
}

fn spawn_git(cwd: &str, args: &[&str], stdin: Option<&[u8]>, env: GitEnv) -> io::Result<Output> {
    let mut command = git_command(cwd, args, env);
    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Apple targets create pipes before marking them close on exec, so a concurrent spawn could inherit one.
    let mut child = {
        let _spawning = SPAWNING.lock().unwrap_or_else(PoisonError::into_inner);
        command.spawn()?
    };

    let writer = stdin.zip(child.stdin.take()).map(|(bytes, mut pipe)| {
        let bytes = bytes.to_vec();
        thread::spawn(move || pipe.write_all(&bytes))
    });

    let output = child.wait_with_output()?;
    if let Some(writer) = writer {
        let _ = writer.join();
    }

    Ok(output)
}

pub(super) enum GitError {
    Spawn(io::Error),
    Failed(String),
}

pub(super) fn run_git_spawned(
    cwd: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
    env: GitEnv,
) -> Result<GitOutput, GitError> {
    finished(cwd, args, spawn_git(cwd, args, stdin, env))
}

fn finished(cwd: &str, args: &[&str], output: io::Result<Output>) -> Result<GitOutput, GitError> {
    let command = args.iter().find(|arg| !arg.starts_with('-')).unwrap_or(&"");
    let failed = |reason: String| format!("git {command} failed in {cwd}: {reason}");
    let output = output.map_err(GitError::Spawn)?;
    let stderr = decoded(&output.stderr);
    if output.status.success() {
        return Ok(GitOutput {
            output: output.stdout,
            warnings: stderr,
        });
    }

    Err(GitError::Failed(failed(failure_reason(
        &stderr,
        output.status,
    ))))
}

pub(super) fn run_git_with(
    cwd: &str,
    args: &[&str],
    stdin: Option<&[u8]>,
    env: GitEnv,
) -> Result<GitOutput, String> {
    with_message(cwd, args, run_git_spawned(cwd, args, stdin, env))
}

fn with_message(
    cwd: &str,
    args: &[&str],
    result: Result<GitOutput, GitError>,
) -> Result<GitOutput, String> {
    result.map_err(|error| match error {
        GitError::Failed(reason) => reason,
        GitError::Spawn(error) => {
            let command = args.iter().find(|arg| !arg.starts_with('-')).unwrap_or(&"");
            format!("git {command} failed in {cwd}: {error}")
        }
    })
}

pub(super) fn run_git(cwd: &str, args: &[&str]) -> Result<String, String> {
    run_git_with(cwd, args, None, GitEnv::Inherited).map(|result| decoded(&result.output))
}

pub(super) struct GitCall {
    pub(super) cwd: String,
    pub(super) args: Vec<String>,
    pub(super) env: GitEnv,
}

impl GitCall {
    fn argv(&self) -> Vec<&str> {
        self.args.iter().map(String::as_str).collect()
    }
}

const WAVE_WIDTH: usize = 16;

fn spawn_wave(calls: &[GitCall]) -> Vec<io::Result<Output>> {
    let spawn = |call: &GitCall| spawn_git(&call.cwd, &call.argv(), None, call.env.clone());
    let next = AtomicUsize::new(0);
    let work = || {
        let mut done = Vec::new();
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let Some(call) = calls.get(index) else {
                return done;
            };

            done.push((index, spawn(call)));
        }
    };

    let mut done = thread::scope(|scope| {
        let workers: Vec<_> = (1..WAVE_WIDTH.min(calls.len()))
            .map_while(|_| thread::Builder::new().spawn_scoped(scope, work).ok())
            .collect();

        let mut done = work();
        for worker in workers {
            done.extend(
                worker
                    .join()
                    .unwrap_or_else(|panic| panic::resume_unwind(panic)),
            );
        }

        done
    });

    done.sort_by_key(|(index, _)| *index);
    done.into_iter().map(|(_, output)| output).collect()
}

pub(super) fn decoded(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)).into_owned()
}

pub(super) fn decoded_owned(mut bytes: Vec<u8>) -> String {
    if bytes.starts_with(b"\xef\xbb\xbf") {
        bytes.drain(..3);
    }

    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

pub(super) fn nul_items(text: &str) -> impl Iterator<Item = &str> {
    text.split('\0').filter(|item| !item.is_empty())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    Repository(String),
    Outside,
    Failed(String),
}

pub(super) struct GitVariable {
    pub(super) key: String,
    pub(super) value: Option<String>,
}

pub(super) struct GitVariables {
    pub(super) readable: bool,
    text: String,
    pub(super) entries: Vec<GitVariable>,
}

#[derive(Default)]
pub struct Repositories {
    located: HashMap<String, Location>,
    pub(super) variables: HashMap<String, Rc<GitVariables>>,
    predicted: HashMap<String, Location>,
    prefetched: HashMap<(String, Vec<String>), Result<String, String>>,
    pub(super) tracked_scopes: HashMap<(String, Vec<String>), bool>,
    pub(super) index_attributes: HashMap<String, Rc<Option<Vec<TrackedAttributes>>>>,
    directory_listings: HashMap<String, Rc<Result<GitOutput, String>>>,
    pub(super) private_indexes: HashMap<String, Option<PrivateIndex>>,
}

impl Repositories {
    pub fn locate(&mut self, dir: &str) -> Location {
        if let Some(known) = self.located.get(dir) {
            return known.clone();
        }

        let location = self.discover(dir);
        self.located.insert(dir.to_owned(), location.clone());
        location
    }

    fn discover(&mut self, dir: &str) -> Location {
        if !has_git() {
            return Location::Outside;
        }

        if let Some(location) = self.known_location(dir, unsafe { libc::geteuid() }) {
            return location;
        }

        let result = run_git(dir, &["rev-parse", "--show-toplevel"]);
        let root = match &result {
            Ok(output) => output.strip_suffix('\n').unwrap_or(output),
            Err(_) => "",
        };

        if !root.is_empty() {
            return Location::Repository(root.to_owned());
        }

        match result {
            Err(error) if has_git_marker(dir) => Location::Failed(error),
            _ => Location::Outside,
        }
    }

    pub fn predict(&mut self, dir: &str) -> Location {
        if let Some(known) = self.predicted.get(dir) {
            return known.clone();
        }

        let walked = has_git()
            .then(|| self.filesystem_walk(dir, unsafe { libc::geteuid() }))
            .flatten();

        let location = match walked {
            Some(Walk::Repository(root, _)) => Location::Repository(root),
            _ => Location::Outside,
        };
        self.predicted.insert(dir.to_owned(), location.clone());
        location
    }

    pub(super) fn prefetch(&mut self, roots: &[&str], calls: Vec<GitCall>) {
        let unread: Vec<&str> = roots
            .iter()
            .copied()
            .filter(|root| !self.variables.contains_key(*root))
            .collect();

        let mut wave: Vec<GitCall> = unread.iter().map(|root| variables_call(root)).collect();

        wave.extend(calls);
        if wave.len() < 2 {
            return;
        }

        let mut outputs = spawn_wave(&wave).into_iter();
        for (root, output) in unread.iter().zip(outputs.by_ref()) {
            if output.is_ok() {
                self.variables
                    .insert((*root).to_owned(), Rc::new(parsed_variables(output)));
            }
        }

        for (call, output) in wave.into_iter().skip(unread.len()).zip(outputs) {
            if output.is_err() {
                continue;
            }

            let argv = call.argv();
            let result = with_message(&call.cwd, &argv, finished(&call.cwd, &argv, output))
                .map(|result| decoded(&result.output));

            self.prefetched.insert((call.cwd, call.args), result);
        }
    }

    pub(super) fn directory_listing(&mut self, dir: &str) -> Rc<Result<GitOutput, String>> {
        // git translates the warning naming a directory it could not open, and only its C locale text is matched.
        let args = [
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            ".",
        ];

        self.directory_listings
            .entry(dir.to_owned())
            .or_insert_with(|| Rc::new(run_git_with(dir, &args, None, GitEnv::CLocale)))
            .clone()
    }

    pub(super) fn run_git(
        &mut self,
        cwd: &str,
        args: &[&str],
        env: GitEnv,
    ) -> Result<String, String> {
        let key = (
            cwd.to_owned(),
            args.iter().map(|arg| (*arg).to_owned()).collect(),
        );
        self.prefetched.remove(&key).unwrap_or_else(|| {
            run_git_with(cwd, args, None, env).map(|result| decoded(&result.output))
        })
    }

    fn git_variables(&mut self, root: &str) -> Rc<GitVariables> {
        if let Some(cached) = self.variables.get(root) {
            return cached.clone();
        }

        let call = variables_call(root);
        let output = spawn_git(&call.cwd, &call.argv(), None, call.env.clone());
        let variables = Rc::new(parsed_variables(output));
        self.variables.insert(root.to_owned(), variables.clone());
        variables
    }

    pub(super) fn process_filters(&mut self, root: &str) -> Option<HashSet<String>> {
        let variables = self.git_variables(root);
        variables.readable.then(|| {
            variables
                .entries
                .iter()
                .filter_map(|entry| entry.key.strip_prefix("filter.")?.strip_suffix(".process"))
                .map(str::to_owned)
                .collect()
        })
    }

    fn plain_config(&mut self, root: &str, dirs: &GitDirs) -> bool {
        let variables = self.git_variables(root);
        let names_root = |value: &str| {
            let named = if is_absolute(value) {
                value.trim_end_matches('/') == root
            } else {
                pointed_path(&dirs.git_dir, value.as_bytes()).is_some_and(|path| path == root)
            };

            named && single_line_config(dirs.common_dir())
        };

        variables.readable && !discovery_config(&variables.text, dirs.linked(), names_root)
    }

    fn known_location(&mut self, dir: &str, user: u32) -> Option<Location> {
        match self.filesystem_walk(dir, user)? {
            Walk::Repository(root, dirs) => self
                .plain_config(&root, &dirs)
                .then_some(Location::Repository(root)),
            Walk::Outside => Some(Location::Outside),
        }
    }

    fn filesystem_walk(&mut self, dir: &str, user: u32) -> Option<Walk> {
        let environment = std::env::vars_os().any(|(name, _)| {
            let name = name.to_string_lossy();
            DISCOVERY_ENVIRONMENT.contains(&name.as_ref()) || name.starts_with("GIT_CONFIG")
        });

        if environment {
            return None;
        }

        let start = realpath(dir).ok()?;
        let start_meta = fs::metadata(&start).ok()?;
        let printable = start.bytes().all(|byte| (0x20..=0x7e).contains(&byte));
        if !start_meta.is_dir() || !printable || start.split('/').any(|part| part == ".git") {
            return None;
        }

        let device = start_meta.dev();
        let mut current = start;
        loop {
            let marker = join(&current, ".git");
            if let Some(entry) = lstat_if_present(&marker).ok()? {
                let dirs = followed_git_dirs(&marker, &entry)?;
                let plain = plain_repository(&current, &entry, &dirs, user).ok()?;
                return plain.then_some(Walk::Repository(current, dirs));
            }

            if lstat_if_present(&join(&current, "HEAD")).ok()?.is_some() {
                return None;
            }

            let parent = dirname(&current);
            if parent == current {
                return Some(Walk::Outside);
            }

            // git stops at a mount point and rev-parse fails, which reads as Failed only when a .git sits above it.
            if fs::metadata(&parent).ok()?.dev() != device {
                let stops = std::env::var_os("GIT_DISCOVERY_ACROSS_FILESYSTEM").is_none();
                return (stops && !has_git_marker(&parent)).then_some(Walk::Outside);
            }

            current = parent;
        }
    }
}

enum Walk {
    Repository(String, GitDirs),
    Outside,
}

pub(super) struct GitDirs {
    pub(super) git_dir: String,
    commondir: Option<String>,
}

impl GitDirs {
    pub(super) fn common_dir(&self) -> &str {
        self.commondir.as_deref().unwrap_or(&self.git_dir)
    }

    fn linked(&self) -> bool {
        self.commondir.is_some()
    }
}

pub(super) fn git_dirs(worktree: &str) -> Option<GitDirs> {
    let marker = join(worktree, ".git");
    let entry = lstat_if_present(&marker).ok()??;
    let dirs = followed_git_dirs(&marker, &entry)?;
    git_directory(&dirs).ok()?.then_some(dirs)
}

const GIT_FILE_LIMIT: u64 = 4096;

fn followed_git_dirs(marker: &str, entry: &fs::Metadata) -> Option<GitDirs> {
    let git_dir = if entry.is_dir() {
        marker.to_owned()
    } else if entry.is_file() && entry.len() <= GIT_FILE_LIMIT {
        let text = fs::read(marker).ok()?;
        pointed_path(&dirname(marker), text.strip_prefix(b"gitdir: ")?)?
    } else {
        return None;
    };

    let commondir = join(&git_dir, "commondir");
    let commondir = match lstat_if_present(&commondir).ok()? {
        Some(_) => Some(pointed_path(&git_dir, &fs::read(&commondir).ok()?)?),
        None => None,
    };

    Some(GitDirs { git_dir, commondir })
}

// git's discovery reads core.worktree from this file alone, and var -l splits a value holding an escaped or raw line break.
fn single_line_config(common_dir: &str) -> bool {
    fs::read(join(common_dir, "config")).is_ok_and(|text| {
        !text.iter().any(|byte| matches!(byte, b'\\' | b'\r'))
            && !text
                .windows(3)
                .any(|bytes| matches!(bytes, [0xe2, 0x80, 0xa8 | 0xa9]))
    })
}

// Not paths::join: git resolves .. in a pointer through symlinks, which folding it as text would not.
fn pointed_path(base: &str, pointer: &[u8]) -> Option<String> {
    let mut end = pointer.len();
    while end > 0 && matches!(pointer[end - 1], b'\n' | b'\r') {
        end -= 1;
    }

    let pointer = &pointer[..end];
    if pointer.is_empty() || !pointer.iter().all(|byte| (0x20..=0x7e).contains(byte)) {
        return None;
    }

    let path = std::str::from_utf8(pointer).ok()?;
    if is_absolute(path) {
        realpath(path).ok()
    } else {
        realpath(&format!("{base}/{path}")).ok()
    }
}

fn plain_repository(
    worktree: &str,
    marker_meta: &fs::Metadata,
    dirs: &GitDirs,
    user: u32,
) -> io::Result<bool> {
    let owned = marker_meta.uid() == user
        && lstat(worktree)?.uid() == user
        && lstat(&dirs.git_dir)?.uid() == user
        && lstat(dirs.common_dir())?.uid() == user;

    Ok(owned && git_directory(dirs)?)
}

fn git_directory(dirs: &GitDirs) -> io::Result<bool> {
    let head = join(&dirs.git_dir, "HEAD");
    if !lstat(&head)?.is_file() {
        return Ok(false);
    }

    let mut bytes = [0u8; 255];
    let read = fs::File::open(&head)?.read(&mut bytes)?;
    if !valid_head(&bytes[..read]) {
        return Ok(false);
    }

    access(&join(dirs.common_dir(), "objects"), libc::X_OK)?;
    access(&join(dirs.common_dir(), "refs"), libc::X_OK)?;
    Ok(true)
}

fn variables_call(root: &str) -> GitCall {
    GitCall {
        cwd: root.to_owned(),
        args: vec!["var".to_owned(), "-l".to_owned()],
        env: GitEnv::DefaultEmail,
    }
}

fn parsed_variables(output: io::Result<Output>) -> GitVariables {
    let (readable, text) = match output {
        Ok(output) => (
            output.status.success() && output.stderr.is_empty(),
            decoded(&output.stdout),
        ),
        Err(_) => (false, String::new()),
    };

    let entries = text
        .strip_suffix('\n')
        .unwrap_or(&text)
        .split('\n')
        .map(|line| match line.split_once('=') {
            Some((key, value)) => GitVariable {
                key: key.to_owned(),
                value: Some(value.to_owned()),
            },
            None => GitVariable {
                key: line.to_owned(),
                value: None,
            },
        })
        .collect();

    GitVariables {
        readable,
        text,
        entries,
    }
}

const DISCOVERY_ENVIRONMENT: [&str; 6] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_CEILING_DIRECTORIES",
    "GIT_TEST_ASSUME_DIFFERENT_OWNER",
];

fn valid_head(bytes: &[u8]) -> bool {
    bytes.starts_with(b"ref: refs/")
        || (bytes.len() >= 40
            && bytes[..40]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte)))
}

pub(super) fn js_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\n', '\r', '\u{2028}', '\u{2029}'])
}

pub(super) fn ascii_prefix<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let head = line.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &line[prefix.len()..])
}

fn setting_matches(rest: &str, exempt: &[&str]) -> bool {
    match rest.strip_prefix('=') {
        Some(value) => !exempt.iter().any(|word| value.eq_ignore_ascii_case(word)),
        None => rest.is_empty(),
    }
}

fn discovery_config(text: &str, linked: bool, names_worktree: impl Fn(&str) -> bool) -> bool {
    js_lines(text).any(|line| {
        ascii_prefix(line, "extensions.").is_some()
            || (!linked
                && ascii_prefix(line, "core.worktree").is_some_and(|rest| {
                    match rest.strip_prefix('=') {
                        Some(value) => !names_worktree(value),
                        None => rest.is_empty(),
                    }
                }))
            || (!linked
                && ascii_prefix(line, "core.bare")
                    .is_some_and(|rest| setting_matches(rest, &["false", "no", "off", "0"])))
            || ascii_prefix(line, "core.repositoryformatversion")
                .is_some_and(|rest| setting_matches(rest, &["0", "1"]))
    })
}

fn ceiling_directories() -> HashSet<String> {
    let value = std::env::var_os("GIT_CEILING_DIRECTORIES")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();

    value
        .split(':')
        .filter(|path| is_absolute(path))
        .map(|path| {
            if exists(path) {
                realpath(path).unwrap_or_else(|_| resolve("/", path))
            } else {
                resolve("/", path)
            }
        })
        .collect()
}

fn has_git_marker(dir: &str) -> bool {
    if dir.split('/').any(|segment| segment == ".git") {
        return false;
    }

    let ceilings = ceiling_directories();
    let Ok(mut current) = realpath(dir) else {
        return false;
    };

    loop {
        if exists(&join(&current, ".git")) {
            return true;
        }

        let parent = dirname(&current);
        if parent == current || ceilings.contains(&parent) {
            return false;
        }

        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_output_drops_a_leading_byte_order_mark_like_text_decoder() {
        assert_eq!(
            decoded(b"\xef\xbb\xbfa.ts\0\xef\xbb\xbfb.ts"),
            "a.ts\0\u{feff}b.ts"
        );
        assert_eq!(decoded(b"a\xff"), "a\u{fffd}");
    }

    struct Scratch(String);

    impl Scratch {
        fn new() -> Scratch {
            let base = std::env::temp_dir().join(format!(
                "stanza-files-{}-{}",
                std::process::id(),
                SCRATCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));

            fs::create_dir_all(&base).unwrap();
            Scratch(realpath(&base.to_string_lossy()).unwrap())
        }

        fn git_init(&self) {
            let status = Command::new("git")
                .args(["-C", &self.0, "init", "-q"])
                .status()
                .unwrap();

            assert!(status.success());
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    static SCRATCH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    #[test]
    fn a_location_holds_for_the_rest_of_the_invocation() {
        let root = Scratch::new();
        let mut repositories = Repositories::default();
        assert_eq!(repositories.locate(&root.0), Location::Outside);

        root.git_init();
        assert_eq!(repositories.locate(&root.0), Location::Outside);
        assert_eq!(
            Repositories::default().locate(&root.0),
            Location::Repository(root.0.clone())
        );
    }

    fn call(cwd: &str, args: &[&str]) -> GitCall {
        GitCall {
            cwd: cwd.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            env: GitEnv::Inherited,
        }
    }

    #[test]
    fn a_wave_returns_each_output_in_call_order() {
        let root = Scratch::new();
        root.git_init();
        let names: Vec<String> = (0..12)
            .map(|index| format!("refs/heads/name{index}"))
            .collect();
        let calls: Vec<GitCall> = names
            .iter()
            .map(|name| call(&root.0, &["check-ref-format", "--normalize", name]))
            .collect();

        let printed: Vec<String> = spawn_wave(&calls)
            .into_iter()
            .map(|output| decoded(&output.unwrap().stdout))
            .collect();

        let expected: Vec<String> = names.iter().map(|name| format!("{name}\n")).collect();
        assert_eq!(printed, expected);
    }

    #[test]
    fn a_wave_runs_its_calls_at_the_same_time() {
        let root = Scratch::new();
        root.git_init();
        let meeting = join(&root.0, "meeting");
        fs::create_dir(&meeting).unwrap();
        let alias = format!(
            "alias.meet=!f() {{ touch '{meeting}'/$1; i=0; while [ $(ls '{meeting}' | wc -l) -lt 2 ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done; ls '{meeting}' | wc -l | tr -d ' '; }}; f"
        );

        let calls: Vec<GitCall> = ["first", "second"]
            .iter()
            .map(|name| call(&root.0, &["-c", &alias, "meet", name]))
            .collect();

        let met: Vec<String> = spawn_wave(&calls)
            .into_iter()
            .map(|output| decoded(&output.unwrap().stdout))
            .collect();

        assert_eq!(met, ["2\n", "2\n"]);
    }

    #[test]
    fn git_var_never_derives_an_email_from_the_hostname() {
        let call = variables_call("/repo");
        let command = git_command(&call.cwd, &call.argv(), call.env.clone());
        let envs: Vec<_> = command.get_envs().collect();

        assert_eq!(call.args, ["var", "-l"]);
        assert_eq!(
            envs,
            [(
                std::ffi::OsStr::new("EMAIL"),
                Some(std::ffi::OsStr::new("stanza@localhost"))
            )]
        );
    }

    #[test]
    fn a_prefetched_answer_serves_one_identical_call() {
        let root = Scratch::new();
        root.git_init();
        let mut repositories = Repositories::default();
        repositories.prefetch(&[&root.0], vec![call(&root.0, &["rev-parse", "--git-dir"])]);

        assert!(repositories.variables.contains_key(&root.0));
        assert_eq!(repositories.prefetched.len(), 1);
        assert!(
            repositories
                .run_git(
                    &root.0,
                    &["rev-parse", "--absolute-git-dir"],
                    GitEnv::Inherited
                )
                .is_ok()
        );
        assert_eq!(repositories.prefetched.len(), 1);
        assert_eq!(
            repositories.run_git(&root.0, &["rev-parse", "--git-dir"], GitEnv::Inherited),
            Ok(".git\n".to_owned())
        );

        assert!(repositories.prefetched.is_empty());
    }

    #[test]
    fn a_prediction_var_reports_as_configured_elsewhere_is_located_by_git() {
        let root = Scratch::new();
        root.git_init();
        let worktree = join(&root.0, "sub");
        fs::create_dir(&worktree).unwrap();
        run_git(&root.0, &["config", "core.worktree", &worktree]).unwrap();

        let mut repositories = Repositories::default();
        assert_eq!(
            repositories.predict(&root.0),
            Location::Repository(root.0.clone())
        );

        repositories.prefetch(&[&root.0], vec![call(&root.0, &["status", "--short"])]);
        assert_eq!(repositories.locate(&root.0), Location::Repository(worktree));
    }

    #[test]
    fn a_prediction_git_refuses_is_located_as_git_fails() {
        let root = Scratch::new();
        root.git_init();
        let config = join(&root.0, ".git/config");
        let text = fs::read_to_string(&config).unwrap();
        fs::write(&config, format!("{text}not a config line\n")).unwrap();

        let mut repositories = Repositories::default();
        assert_eq!(
            repositories.predict(&root.0),
            Location::Repository(root.0.clone())
        );

        repositories.prefetch(&[&root.0], vec![call(&root.0, &["status", "--short"])]);
        assert!(!repositories.variables[&root.0].readable);
        assert!(
            matches!(repositories.locate(&root.0), Location::Failed(error) if error.contains("rev-parse"))
        );
    }

    #[test]
    fn a_directory_in_no_repository_is_known_outside_without_git() {
        let root = Scratch::new();
        let user = unsafe { libc::geteuid() };
        let mut repositories = Repositories::default();
        assert_eq!(
            repositories.known_location(&root.0, user),
            Some(Location::Outside)
        );

        fs::write(join(&root.0, "HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(repositories.known_location(&root.0, user), None);
    }

    #[test]
    fn a_repository_owned_by_another_user_has_no_known_root() {
        let root = Scratch::new();
        root.git_init();
        let user = unsafe { libc::geteuid() };
        let mut repositories = Repositories::default();
        assert_eq!(
            repositories.known_location(&root.0, user),
            Some(Location::Repository(root.0.clone()))
        );
        assert_eq!(repositories.known_location(&root.0, user + 1), None);
    }

    #[test]
    fn a_linked_worktree_owned_by_another_user_has_no_known_root() {
        let root = Scratch::new();
        root.git_init();
        let commit = Command::new("git")
            .args(["-C", &root.0, "-c", "user.name=t", "-c", "user.email=t@t"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "a",
            ])
            .status()
            .unwrap();

        assert!(commit.success());
        let worktree = join(&root.0, "linked");
        run_git(&root.0, &["worktree", "add", "-q", "--detach", &worktree]).unwrap();

        let user = unsafe { libc::geteuid() };
        let mut repositories = Repositories::default();
        assert_eq!(
            repositories.known_location(&worktree, user),
            Some(Location::Repository(worktree.clone()))
        );
        assert_eq!(repositories.known_location(&worktree, user + 1), None);

        let dirs = git_dirs(&worktree).unwrap();
        assert_eq!(dirs.git_dir, join(&root.0, ".git/worktrees/linked"));
        assert_eq!(dirs.common_dir(), join(&root.0, ".git"));
        assert!(dirs.linked());
    }

    #[test]
    fn a_dangling_commondir_is_no_layout_as_git_dies_on_it() {
        let root = Scratch::new();
        let git_dir = join(&root.0, "gitdir");
        fs::create_dir(&git_dir).unwrap();
        let entry = lstat(&git_dir).unwrap();
        assert!(followed_git_dirs(&git_dir, &entry).is_some_and(|dirs| !dirs.linked()));

        std::os::unix::fs::symlink(join(&root.0, "missing"), join(&git_dir, "commondir")).unwrap();
        assert!(followed_git_dirs(&git_dir, &entry).is_none());
    }

    #[test]
    fn a_pointer_resolves_like_git_or_not_at_all() {
        let root = Scratch::new();
        let target = join(&root.0, "target");
        fs::create_dir_all(join(&target, "inner")).unwrap();
        std::os::unix::fs::symlink(join(&target, "inner"), join(&root.0, "link")).unwrap();

        assert_eq!(pointed_path(&root.0, b"link/..\r\n"), Some(target.clone()));
        assert_eq!(pointed_path("/elsewhere", target.as_bytes()), Some(target));
        assert_eq!(pointed_path(&root.0, b"\n"), None);
        assert_eq!(pointed_path(&root.0, b"target\0x"), None);
        assert_eq!(pointed_path(&root.0, b"missing"), None);
    }

    #[test]
    fn reads_discovery_settings_like_the_reference_patterns() {
        let main = |text: &str| discovery_config(text, false, |_| false);
        assert!(main("core.worktree=/x\n"));
        assert!(main("x=1\nCORE.BARE\n"));
        assert!(main("core.bare=true"));
        assert!(!main("core.bare=FALSE\ncore.bare=0"));
        assert!(main("core.repositoryformatversion=2"));
        assert!(!main("core.repositoryformatversion=1\ncore.worktrees=x"));
        assert!(main("user.name=a\rextensions.objectformat=sha256"));
    }

    #[test]
    fn a_worktree_setting_naming_the_root_or_shadowed_by_commondir_is_plain() {
        let root = |value: &str| value == "/x";
        assert!(!discovery_config("core.worktree=/x\n", false, root));
        assert!(discovery_config(
            "core.worktree=/x\ncore.worktree=/y",
            false,
            root
        ));
        assert!(discovery_config("core.worktree\n", false, |_| true));
        assert!(!discovery_config(
            "core.bare=true\ncore.worktree=/y",
            true,
            root
        ));
        assert!(discovery_config(
            "extensions.worktreeconfig=true",
            true,
            root
        ));
        assert!(discovery_config(
            "core.repositoryformatversion=2",
            true,
            root
        ));
    }
}
