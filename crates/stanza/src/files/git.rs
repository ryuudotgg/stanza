use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{
    Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio,
};
use std::rc::Rc;
use std::sync::LazyLock;
use std::thread::{self, JoinHandle};

use super::changed::PreparedListing;
use super::index::{IndexCopy, PrivateIndex, TrackedAttributes};
use super::{access, exists, lstat, lstat_if_present, realpath, reason};
use crate::paths::{dirname, is_absolute, join, relative, resolve};
use crate::signals;
use crate::text::js_trim;

static HAS_GIT: LazyLock<bool> = LazyLock::new(|| {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };

    std::env::split_paths(&path)
        .filter_map(|dir| dir.join("git").into_os_string().into_string().ok())
        .any(|candidate| {
            fs::metadata(&candidate).is_ok_and(|meta| meta.is_file())
                && access(&candidate, libc::X_OK).is_ok()
        })
});

pub fn has_git() -> bool {
    *HAS_GIT
}

#[cfg(test)]
thread_local! {
    pub(crate) static REFUSED_WRITER: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
    static REFUSED_READER: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

const READ_BUFFER: usize = 64 * 1024;

fn start_reader(mut pipe: ChildStderr) -> io::Result<JoinHandle<io::Result<Vec<u8>>>> {
    #[cfg(test)]
    if let Some(code) = REFUSED_READER.get() {
        return Err(io::Error::from_raw_os_error(code));
    }

    thread::Builder::new().spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).map(|_| bytes)
    })
}

fn reaped<T>(child: Child, error: io::Error) -> io::Result<T> {
    let _ = child.wait_with_output();
    Err(error)
}

fn read_child<T>(
    mut child: Child,
    stdin: &[u8],
    read: impl FnOnce(&mut BufReader<ChildStdout>) -> io::Result<T>,
) -> io::Result<(Output, T)> {
    let writer = child
        .stdin
        .take()
        .map(|pipe| start_writer(pipe, stdin.to_vec()))
        .transpose();

    let writer = match writer {
        Ok(writer) => writer,
        Err(error) => return reaped(child, error),
    };

    let stderr = match child.stderr.take().map(start_reader) {
        Some(Ok(stderr)) => stderr,
        Some(Err(error)) => return reaped(child, error),
        None => return reaped(child, io::ErrorKind::BrokenPipe.into()),
    };

    let Some(stdout) = child.stdout.take() else {
        return reaped(child, io::ErrorKind::BrokenPipe.into());
    };

    let mut stdout = BufReader::with_capacity(READ_BUFFER, stdout);
    let parsed = read(&mut stdout);
    let drained = io::copy(&mut stdout, &mut io::sink());
    let status = child.wait()?;
    if let Some(writer) = writer {
        let _ = writer.join();
    }

    let stderr = stderr.join().unwrap_or_else(|_| Ok(Vec::new()))?;
    drained?;
    let output = Output {
        status,
        stdout: Vec::new(),
        stderr,
    };

    Ok((output, parsed?))
}

fn start_writer(mut pipe: ChildStdin, bytes: Vec<u8>) -> io::Result<JoinHandle<io::Result<()>>> {
    #[cfg(test)]
    if let Some(code) = REFUSED_WRITER.get() {
        return Err(io::Error::from_raw_os_error(code));
    }

    thread::Builder::new().spawn(move || pipe.write_all(&bytes))
}

#[derive(Default)]
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
            None => format!("killed by signal {}", status.signal().unwrap_or_default()),
        })
}

fn subcommand<'a>(args: &[&'a str]) -> &'a str {
    let mut args = args.iter();
    while let Some(&arg) = args.next() {
        if arg == "-c" {
            args.next();
        } else if !arg.starts_with('-') {
            return arg;
        }
    }

    ""
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum GitEnv {
    Inherited,
    CLocale,
    NoDiffOpts,
    WorktreeDiff(Option<IndexCopy>),
    DefaultEmail,
}

type Pinned = Vec<(&'static str, OsString)>;

// git -C <dir> reads relative repository variables against <dir>, and with GIT_DIR set and no GIT_WORK_TREE takes <dir> as the work tree, so every call would judge files against a different root.
static PINNED_REPOSITORY: LazyLock<Pinned> = LazyLock::new(|| {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };

    pinned_repository(&cwd, |name| std::env::var_os(name), toplevel)
});

const START_RELATIVE: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
];

fn pinned_repository(
    cwd: &Path,
    var: impl Fn(&str) -> Option<OsString>,
    toplevel: impl FnOnce(&Pinned) -> Option<OsString>,
) -> Pinned {
    let set = |name: &str| var(name).filter(|value| !value.is_empty());
    let mut pinned: Pinned = START_RELATIVE
        .iter()
        .filter_map(|&name| Some((name, cwd.join(set(name)?).into_os_string())))
        .collect();

    let root = match pinned.iter().find(|(name, _)| *name == "GIT_WORK_TREE") {
        Some((_, root)) => root.clone(),
        None if set("GIT_DIR").is_none() => return pinned,
        None => {
            let Some(root) = toplevel(&pinned) else {
                return Vec::new();
            };

            pinned.push(("GIT_WORK_TREE", root.clone()));
            root
        }
    };

    // git reads a relative index path against the work tree root, unlike GIT_DIR and GIT_WORK_TREE.
    if let Some(index) = set("GIT_INDEX_FILE") {
        pinned.push((
            "GIT_INDEX_FILE",
            Path::new(&root).join(index).into_os_string(),
        ));
    }

    pinned
}

fn toplevel(pinned: &Pinned) -> Option<OsString> {
    let mut command = Command::new("git");
    command
        .args(["rev-parse", "--show-toplevel"])
        .envs(pinned.iter().map(|(name, value)| (*name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let child = signals::spawn_unblocked(|| command.spawn()).ok()?;
    let output = child.wait_with_output().ok()?;
    let root = output.stdout.strip_suffix(b"\n")?;
    (output.status.success() && !root.is_empty()).then(|| OsString::from_vec(root.to_vec()))
}

fn git_command(cwd: &str, args: &[&str], env: &GitEnv) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args(args)
        .envs(PINNED_REPOSITORY.iter().map(|(name, value)| (*name, value)));

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
                command.env("GIT_INDEX_FILE", &index.path);
            }
        }
    }

    command
}

// Apple targets create pipes before marking them close on exec, so a spawn from another thread could inherit one; git children start only from the selecting thread.
fn spawn_git(cwd: &str, args: &[&str], env: &GitEnv, stdin: Stdio) -> io::Result<Child> {
    let mut command = git_command(cwd, args, env);
    command
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    signals::spawn_unblocked(|| command.spawn())
}

#[derive(Debug)]
pub(super) enum GitError {
    Spawn(String),
    Failed(String),
    Usage(String),
}

impl GitError {
    pub(super) fn message(self) -> String {
        match self {
            GitError::Spawn(message) | GitError::Failed(message) | GitError::Usage(message) => {
                message
            }
        }
    }
}

const USAGE_EXIT: i32 = 129;

fn finished(
    place: &str,
    args: &[&str],
    env: &GitEnv,
    output: io::Result<Output>,
) -> Result<GitOutput, GitError> {
    let failed = |reason: String| format!("git {} failed{place}: {reason}", subcommand(args));
    let output = output.map_err(|error| GitError::Spawn(failed(reason(&error))))?;
    let stderr = decoded(&output.stderr);
    if output.status.success() {
        return Ok(GitOutput {
            output: output.stdout,
            warnings: stderr,
        });
    }

    let reason = failure_reason(&stderr, output.status);
    let reason = match env {
        GitEnv::WorktreeDiff(Some(index)) => index.named_as_git(reason),
        _ => reason,
    };

    if output.status.code() == Some(USAGE_EXIT) {
        return Err(GitError::Usage(failed(reason)));
    }

    Err(GitError::Failed(failed(reason)))
}

pub(super) fn git_succeeds(cwd: &str, args: &[&str]) -> bool {
    spawn_git(cwd, args, &GitEnv::Inherited, Stdio::null())
        .and_then(Child::wait_with_output)
        .is_ok_and(|output| output.status.success())
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
    let mut pending = calls
        .iter()
        .map(|call| spawn_git(&call.cwd, &call.argv(), &call.env, Stdio::null()));

    let mut live: VecDeque<_> = pending.by_ref().take(WAVE_WIDTH).collect();
    let mut outputs = Vec::with_capacity(calls.len());
    while let Some(child) = live.pop_front() {
        outputs.push(child.and_then(Child::wait_with_output));
        live.extend(pending.next());
    }

    outputs
}

// Keys are filtered in listed_submodule_paths, not by --get-regexp, whose `.` matches no byte outside UTF-8 under a UTF-8 locale.
const SUBMODULE_KEYS: [&str; 5] = ["config", "-z", "--file", ".gitmodules", "--list"];

pub(super) fn submodules_call(root: &str) -> Option<GitCall> {
    let present = fs::metadata(join(root, ".gitmodules")).is_ok();
    present.then(|| GitCall {
        cwd: root.to_owned(),
        args: SUBMODULE_KEYS.map(str::to_owned).to_vec(),
        env: GitEnv::Inherited,
    })
}

fn listed_submodule_paths(root: &str, output: &[u8]) -> HashSet<String> {
    output
        .split(|&byte| byte == 0)
        .filter(|item| !item.is_empty())
        .filter_map(|item| {
            let newline = item.iter().position(|&byte| byte == b'\n')?;
            let (key, value) = (&item[..newline], &item[newline + 1..]);

            names_submodule_path(key).then_some(value)
        })
        .filter_map(|value| std::str::from_utf8(value).ok())
        .map(|value| resolve(root, value))
        .collect()
}

fn names_submodule_path(key: &[u8]) -> bool {
    const PREFIX: &[u8] = b"submodule.";
    const SUFFIX: &[u8] = b".path";

    key.len() >= PREFIX.len() + SUFFIX.len() && key.starts_with(PREFIX) && key.ends_with(SUFFIX)
}

pub(super) fn decoded(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub(super) fn decoded_owned(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

pub(super) struct Names<'a> {
    pub(super) names: Vec<&'a str>,
    pub(super) invalid: Vec<&'a [u8]>,
}

pub(super) fn nul_names(bytes: &[u8]) -> Names<'_> {
    let mut names = Names {
        names: Vec::new(),
        invalid: Vec::new(),
    };

    for item in bytes
        .split(|&byte| byte == 0)
        .filter(|item| !item.is_empty())
    {
        match std::str::from_utf8(item) {
            Ok(name) => names.names.push(name),
            Err(_) => names.invalid.push(item),
        }
    }

    names
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

struct Prefetched {
    env: GitEnv,
    result: Result<GitOutput, GitError>,
}

pub struct Repositories {
    shown_from: Option<String>,
    located: HashMap<String, Location>,
    pub(super) variables: HashMap<String, Rc<GitVariables>>,
    predicted: HashMap<String, Location>,
    prefetched: HashMap<(String, Vec<String>), Prefetched>,
    pub(super) listings: Vec<Rc<PreparedListing>>,
    pub(super) tracked_scopes: HashMap<(String, Vec<String>), bool>,
    pub(super) index_attributes: HashMap<String, Rc<Option<Vec<TrackedAttributes>>>>,
    directory_listings: HashMap<String, Rc<Result<GitOutput, String>>>,
    submodules: HashMap<String, Rc<HashSet<String>>>,
    pub(super) private_indexes: HashMap<String, Option<PrivateIndex>>,
}

impl Repositories {
    pub fn new(shown_from: Option<&str>) -> Self {
        Repositories {
            shown_from: shown_from.map(str::to_owned),
            located: HashMap::new(),
            variables: HashMap::new(),
            predicted: HashMap::new(),
            prefetched: HashMap::new(),
            listings: Vec::new(),
            tracked_scopes: HashMap::new(),
            index_attributes: HashMap::new(),
            directory_listings: HashMap::new(),
            submodules: HashMap::new(),
            private_indexes: HashMap::new(),
        }
    }

    fn place(&self, dir: &str) -> String {
        let Some(base) = &self.shown_from else {
            return format!(" in {dir}");
        };

        let shown = relative(base, dir);
        if shown.is_empty() {
            String::new()
        } else {
            format!(" in {shown}")
        }
    }

    pub(super) fn git(
        &mut self,
        cwd: &str,
        args: &[&str],
        stdin: Option<&[u8]>,
        env: &GitEnv,
    ) -> Result<GitOutput, GitError> {
        if stdin.is_none() && !self.prefetched.is_empty() {
            let key = (
                cwd.to_owned(),
                args.iter().map(|arg| (*arg).to_owned()).collect(),
            );
            if let Entry::Occupied(entry) = self.prefetched.entry(key)
                && entry.get().env == *env
            {
                return entry.remove().result;
            }
        }

        let piped = if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        };

        let output = spawn_git(cwd, args, env, piped).and_then(|mut child| {
            let writer = stdin
                .zip(child.stdin.take())
                .map(|(bytes, pipe)| start_writer(pipe, bytes.to_vec()));

            let output = child.wait_with_output();
            match writer {
                Some(Err(error)) => Err(error),
                Some(Ok(writer)) => {
                    let _ = writer.join();
                    output
                }
                None => output,
            }
        });

        finished(&self.place(cwd), args, env, output)
    }

    pub(super) fn run(&mut self, call: &GitCall) -> Result<GitOutput, GitError> {
        self.git(&call.cwd, &call.argv(), None, &call.env)
    }

    // Reading stdout as it arrives keeps a large reply from passing through doubling buffers, each of which stays resident.
    pub(super) fn git_read<T>(
        &mut self,
        cwd: &str,
        args: &[&str],
        stdin: &[u8],
        read: impl FnOnce(&mut BufReader<ChildStdout>) -> io::Result<Result<T, String>>,
    ) -> Result<Result<T, String>, GitError> {
        let result = spawn_git(cwd, args, &GitEnv::Inherited, Stdio::piped())
            .and_then(|child| read_child(child, stdin, read));

        let (output, parsed) = match result {
            Ok((output, parsed)) => (Ok(output), parsed),
            Err(error) => (Err(error), Err(String::new())),
        };

        finished(&self.place(cwd), args, &GitEnv::Inherited, output)?;
        Ok(parsed)
    }

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

        let result = self
            .git(
                dir,
                &["rev-parse", "--show-toplevel"],
                None,
                &GitEnv::Inherited,
            )
            .map(|result| decoded(&result.output));
        let root = match &result {
            Ok(output) => output.strip_suffix('\n').unwrap_or(output),
            Err(_) => "",
        };

        if !root.is_empty() {
            return Location::Repository(root.to_owned());
        }

        match result {
            Err(error) if has_git_marker(dir) => Location::Failed(error.message()),
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

        let outputs = spawn_wave(&wave);
        for (index, (call, output)) in wave.into_iter().zip(outputs).enumerate() {
            if output.is_err() {
                continue;
            }

            let place = self.place(&call.cwd);
            let result = finished(&place, &call.argv(), &call.env, output);
            if index < unread.len() {
                let variables = Rc::new(parsed_variables(result));
                self.variables.insert(call.cwd, variables);
                continue;
            }

            let entry = Prefetched {
                env: call.env,
                result,
            };

            self.prefetched.insert((call.cwd, call.args), entry);
        }
    }

    #[cfg(test)]
    pub(super) fn prefetched_calls(&self) -> Vec<GitCall> {
        let mut calls: Vec<GitCall> = self
            .prefetched
            .iter()
            .map(|((cwd, args), entry)| GitCall {
                cwd: cwd.clone(),
                args: args.clone(),
                env: entry.env.clone(),
            })
            .collect();

        calls.sort_by(|left, right| (&left.cwd, &left.args).cmp(&(&right.cwd, &right.args)));
        calls
    }

    pub(super) fn directory_listing(&mut self, dir: &str) -> Rc<Result<GitOutput, String>> {
        // git translates the warning naming a directory it cannot open, and only its C locale text is matched.
        let args = [
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            ".",
        ];

        if let Some(known) = self.directory_listings.get(dir) {
            return known.clone();
        }

        let listing = self
            .git(dir, &args, None, &GitEnv::CLocale)
            .map_err(GitError::message);

        let listing = Rc::new(listing);
        self.directory_listings
            .insert(dir.to_owned(), listing.clone());
        listing
    }

    pub(super) fn submodule_paths(&mut self, root: &str) -> Rc<HashSet<String>> {
        if let Some(known) = self.submodules.get(root) {
            return known.clone();
        }

        match submodules_call(root) {
            Some(call) => self.listed_submodules(&call),
            None => self.submodules.entry(root.to_owned()).or_default().clone(),
        }
    }

    pub(super) fn listed_submodules(&mut self, call: &GitCall) -> Rc<HashSet<String>> {
        let root = &call.cwd;
        if let Some(known) = self.submodules.get(root) {
            return known.clone();
        }

        let paths = self
            .run(call)
            .map(|result| listed_submodule_paths(root, &result.output))
            .unwrap_or_default();

        let paths = Rc::new(paths);
        self.submodules.insert(root.clone(), paths.clone());
        paths
    }

    fn git_variables(&mut self, root: &str) -> Rc<GitVariables> {
        if let Some(cached) = self.variables.get(root) {
            return cached.clone();
        }

        let output = self.run(&variables_call(root));
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
        let control = start.bytes().any(|byte| byte.is_ascii_control());
        if !start_meta.is_dir() || control || start.split('/').any(|part| part == ".git") {
            return None;
        }

        let device = start_meta.dev();
        let mut current = start.as_str();
        loop {
            let marker = join(current, ".git");
            if let Some(entry) = lstat_if_present(&marker).ok()? {
                let dirs = followed_git_dirs(&marker, &entry)?;
                let plain = plain_repository(current, &entry, &dirs, user).ok()?;
                return plain.then(|| Walk::Repository(current.to_owned(), dirs));
            }

            if lstat_if_present(&join(current, "HEAD")).ok()?.is_some() {
                return None;
            }

            let parent = dirname(current);
            if parent == current {
                return Some(Walk::Outside);
            }

            // git stops at a mount point and rev-parse fails, which reads as Failed only when a .git sits above it.
            if fs::metadata(parent).ok()?.dev() != device {
                let stops = std::env::var_os("GIT_DISCOVERY_ACROSS_FILESYSTEM").is_none();
                return (stops && !has_git_marker(parent)).then_some(Walk::Outside);
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
        pointed_path(dirname(marker), text.strip_prefix(b"gitdir: ")?)?
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
    if pointer.is_empty() || pointer.iter().any(u8::is_ascii_control) {
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

    access(join(dirs.common_dir(), "objects"), libc::X_OK)?;
    access(join(dirs.common_dir(), "refs"), libc::X_OK)?;
    Ok(true)
}

fn variables_call(root: &str) -> GitCall {
    GitCall {
        cwd: root.to_owned(),
        args: vec!["var".to_owned(), "-l".to_owned()],
        env: GitEnv::DefaultEmail,
    }
}

fn parsed_variables(result: Result<GitOutput, GitError>) -> GitVariables {
    let (readable, text) = match result {
        Ok(result) => (result.warnings.is_empty(), decoded_owned(result.output)),
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
    text.split('\n').any(|line| {
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
    let Ok(real) = realpath(dir) else {
        return false;
    };

    let mut current = real.as_str();
    loop {
        if exists(&join(current, ".git")) {
            return true;
        }

        let parent = dirname(current);
        if parent == current || ceilings.contains(parent) {
            return false;
        }

        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_output_keeps_a_leading_byte_order_mark() {
        let listed = b"\xef\xbb\xbfa.ts\0\xef\xbb\xbfb.ts";
        assert_eq!(decoded(listed), "\u{feff}a.ts\0\u{feff}b.ts");
        assert_eq!(decoded_owned(listed.to_vec()), decoded(listed));
        assert_eq!(decoded(b"a\xff"), "a\u{fffd}");
    }

    #[test]
    fn listed_names_keep_the_bytes_of_a_name_that_is_not_utf8() {
        let names = nul_names(b"\xef\xbb\xbfa.ts\0bad\xff.ts\0\0\xef\xbf\xbd.ts\0");
        assert_eq!(names.names, ["\u{feff}a.ts", "\u{fffd}.ts"]);
        assert_eq!(names.invalid, [&b"bad\xff.ts"[..]]);
    }

    #[test]
    fn a_failed_call_names_the_subcommand_after_config_pairs() {
        let root = Scratch::new();
        let args = [
            "-c",
            "diff.suppressBlankEmpty=false",
            "diff",
            "--no-such-flag",
        ];
        let Err(message) = Repositories::new(None)
            .git(&root.0, &args, None, &GitEnv::Inherited)
            .map_err(GitError::message)
        else {
            panic!("git diff --no-such-flag succeeded");
        };

        assert!(
            message.starts_with(&format!("git diff failed in {}: ", root.0)),
            "{message}"
        );

        let spawn = finished(
            " in /repo",
            &args,
            &GitEnv::Inherited,
            Err(io::Error::other("no git")),
        );
        let Err(message) = spawn.map_err(GitError::message) else {
            unreachable!();
        };

        assert_eq!(message, "git diff failed in /repo: no git");
    }

    #[test]
    fn a_failure_names_its_directory_from_the_working_directory_and_leaves_out_that_directory() {
        let root = Scratch::new();
        let sub = join(&root.0, "sub");
        fs::create_dir(&sub).unwrap();

        let args = ["diff", "--no-such-flag"];
        let message = |shown_from: Option<&str>| {
            let failed = Repositories::new(shown_from)
                .git(&root.0, &args, None, &GitEnv::Inherited)
                .map_err(GitError::message);

            failed.err().expect("git diff --no-such-flag fails")
        };

        assert!(
            message(Some(&root.0)).starts_with("git diff failed: "),
            "{}",
            message(Some(&root.0))
        );

        assert!(
            message(Some(&sub)).starts_with("git diff failed in ..: "),
            "{}",
            message(Some(&sub))
        );
    }

    #[test]
    fn a_git_killed_by_a_signal_names_the_signal() {
        let root = Scratch::new();
        let args = ["-c", "alias.die=!kill -9 $PPID", "die"];
        let Err(message) = Repositories::new(None)
            .git(&root.0, &args, None, &GitEnv::Inherited)
            .map_err(GitError::message)
        else {
            panic!("git die succeeded");
        };

        assert_eq!(
            message,
            format!("git die failed in {}: killed by signal 9", root.0)
        );
    }

    #[test]
    fn git_thread_start_failure_returns_the_error_and_reaps_git() {
        let root = Scratch::new();
        root.git_init();
        let pid_file = join(&root.0, "git.pid");
        for exit in [0, 3] {
            let alias = format!(
                "alias.flood=!echo $PPID > git.pid; head -c 131072 /dev/zero; head -c 131072 /dev/zero >&2; cat > /dev/null; exit {exit}"
            );

            let args = ["-c", alias.as_str(), "flood"];
            let refused = |call: &dyn Fn(&mut Repositories) -> Result<(), GitError>| {
                let _ = fs::remove_file(&pid_file);
                REFUSED_WRITER.set(Some(libc::EAGAIN));
                let result = call(&mut Repositories::new(Some(&root.0)));
                REFUSED_WRITER.set(None);
                let Err(GitError::Spawn(message)) = result else {
                    panic!("git flood exiting {exit} did not fail to start a thread");
                };

                assert_eq!(
                    message,
                    "git flood failed: resource temporarily unavailable"
                );

                let pid: libc::pid_t = fs::read_to_string(&pid_file)
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                let mut status = 0;
                let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                assert_eq!(
                    (waited, io::Error::last_os_error().raw_os_error()),
                    (-1, Some(libc::ECHILD)),
                    "git {pid} exiting {exit} was left unreaped"
                );
            };

            refused(&|repositories| {
                repositories
                    .git(&root.0, &args, Some(b"a.ts\0"), &GitEnv::Inherited)
                    .map(|_| ())
            });

            refused(&|repositories| {
                repositories
                    .git_read(&root.0, &args, b"a.ts\0", |_| Ok(Ok(())))
                    .map(|_| ())
            });

            refused(&|repositories| {
                REFUSED_WRITER.set(None);
                REFUSED_READER.set(Some(libc::EAGAIN));
                let result = repositories
                    .git_read(&root.0, &args, b"a.ts\0", |_| Ok(Ok(())))
                    .map(|_| ());
                REFUSED_READER.set(None);
                result
            });
        }
    }

    const VALUELESS: &[u8] = b"[submodule \"a\"]\n\tpath\n";

    const GITMODULES: [&[u8]; 13] = [
        b"[submodule \"a\"]\n\tpath = vendor/a\n\turl = ../a\n",
        b"[submodule \"a\"]\n\tpath = one\n[submodule \"a\"]\n\tpath = two\n\tpath = three\n",
        b"[Submodule \"MixedCase\"]\n\tPATH = up\n",
        b"[submodule.Old]\n\tpath = old\n",
        VALUELESS,
        b"[submodule \"a\"]\n\tpath =\n",
        b"[submodule \"a\"]\n\tpath = \"line\\nbreak\"\n",
        b"[submodule]\n\tpath = no subsection\n",
        b"[submodule \"\"]\n\tpath = empty subsection\n",
        b"[submodule \"a.path\"]\n\turl = x\n\tpath-x = y\n",
        b"[submodule \"caf\xe9\"]\n\tpath = vendor/sub\n[submodule \"b\"]\n\tpath = bad\xff\n",
        b"[core]\n\tbare = false\n",
        b"[submodule \"a\"\n\tpath = broken header\n",
    ];

    fn listed_by(root: &str, args: &[&str]) -> Vec<u8> {
        let output = Command::new("git")
            .args(["-C", root, "config", "-z", "--file", ".gitmodules"])
            .args(args)
            .env("LC_ALL", "C")
            .output()
            .unwrap();

        output.stdout
    }

    fn matched_paths(root: &str, output: &[u8]) -> HashSet<String> {
        output
            .split(|&byte| byte == 0)
            .filter(|item| !item.is_empty())
            .filter_map(|item| {
                let newline = item.iter().position(|&byte| byte == b'\n')?;
                std::str::from_utf8(&item[newline + 1..]).ok()
            })
            .map(|value| resolve(root, value))
            .collect()
    }

    #[test]
    fn listed_keys_name_the_paths_git_matches() {
        let root = Scratch::new();
        for fixture in GITMODULES {
            fs::write(join(&root.0, ".gitmodules"), fixture).unwrap();
            let shown = String::from_utf8_lossy(fixture);
            let expected = if fixture == VALUELESS {
                HashSet::new()
            } else {
                matched_paths(
                    &root.0,
                    &listed_by(&root.0, &["--get-regexp", r"^submodule\..*\.path$"]),
                )
            };

            let listed = listed_submodule_paths(&root.0, &listed_by(&root.0, &["--list"]));
            assert_eq!(listed, expected, "{shown:?}");
            assert_eq!(
                *Repositories::new(None).submodule_paths(&root.0),
                expected,
                "{shown:?}"
            );
        }

        let broken_after = b"[submodule \"a\"]\n\tpath = ok\n[bad\n";
        fs::write(join(&root.0, ".gitmodules"), broken_after).unwrap();
        assert!(Repositories::new(None).submodule_paths(&root.0).is_empty());

        fs::remove_file(join(&root.0, ".gitmodules")).unwrap();
        assert!(Repositories::new(None).submodule_paths(&root.0).is_empty());
    }

    #[test]
    fn a_submodule_name_outside_utf8_keeps_its_utf8_path() {
        let listed =
            b"submodule.caf\xe9.path\nvendor/sub\0submodule.b.path\nbad\xff\0core.bare\nx\0";
        assert_eq!(
            listed_submodule_paths("/r", listed),
            HashSet::from(["/r/vendor/sub".to_owned()])
        );
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
        let mut repositories = Repositories::new(None);
        assert_eq!(repositories.locate(&root.0), Location::Outside);

        root.git_init();
        assert_eq!(repositories.locate(&root.0), Location::Outside);
        assert_eq!(
            Repositories::new(None).locate(&root.0),
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
    fn a_wave_keeps_sixteen_children_alive() {
        let root = Scratch::new();
        root.git_init();
        let live = join(&root.0, "live");
        let replaced = join(&root.0, "replaced");
        fs::create_dir(&live).unwrap();
        let alias = format!(
            "alias.crowd=!f() {{ touch '{live}'/$1; [ $1 -ge 16 ] && touch '{replaced}'; i=0; while n=$(ls '{live}' | wc -l | tr -d ' '); [ $1 -lt 16 ] && [ $n -lt 16 ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done; i=0; while [ $1 -ge 1 ] && [ $1 -lt 16 ] && [ ! -e '{replaced}' ] && [ $i -lt 200 ]; do sleep 0.05; i=$((i+1)); done; [ -e '{replaced}' ] && seen=1 || seen=0; printf '%s %s %s\\n' $1 $n $seen; head -c 131072 /dev/zero | tr '\\0' o; head -c 131072 /dev/zero | tr '\\0' e >&2; rm '{live}'/$1; }}; f"
        );

        let calls: Vec<GitCall> = (0..33)
            .map(|index| call(&root.0, &["-c", &alias, "crowd", &index.to_string()]))
            .collect();

        let outputs = spawn_wave(&calls);
        assert_eq!(outputs.len(), calls.len());
        let mut counts = Vec::new();
        for (index, output) in outputs.into_iter().enumerate() {
            let output = output.unwrap();
            let stdout = decoded(&output.stdout);
            let (line, body) = stdout.split_once('\n').unwrap();
            let fields: Vec<&str> = line.split(' ').collect();
            assert_eq!(fields[0], index.to_string());
            assert_eq!(body, "o".repeat(131_072));
            assert_eq!(output.stderr, b"e".repeat(131_072));
            if (1..16).contains(&index) {
                assert_eq!(fields[2], "1", "call {index} ended before call 16 started");
            }

            counts.push(fields[1].parse::<usize>().unwrap());
        }

        assert!(counts.iter().all(|&count| count <= 16), "{counts:?}");
        assert!(counts[..16].iter().all(|&count| count == 16), "{counts:?}");
    }

    #[test]
    fn git_var_never_derives_an_email_from_the_hostname() {
        let call = variables_call("/repo");
        let command = git_command(&call.cwd, &call.argv(), &call.env);
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
    fn a_git_dir_pins_absolute_repository_variables_and_the_work_tree() {
        let cwd = Path::new("/repo/sub");
        let environment = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(value))
            }
        };

        let pinned = |pairs: &[(&'static str, &str)]| -> Pinned {
            pairs
                .iter()
                .map(|(name, value)| (*name, OsString::from(value)))
                .collect()
        };

        let unreachable = |_: &Pinned| -> Option<OsString> { panic!("no toplevel spawn") };
        assert!(
            pinned_repository(cwd, environment(&[("GIT_INDEX_FILE", "i")]), unreachable).is_empty()
        );
        assert!(pinned_repository(cwd, environment(&[("GIT_DIR", "")]), unreachable).is_empty());

        let mut asked = None;
        let found = pinned_repository(
            cwd,
            environment(&[("GIT_DIR", "../.git"), ("GIT_INDEX_FILE", ".git/index")]),
            |pinned| {
                asked = Some(pinned.clone());
                Some(OsString::from("/repo"))
            },
        );

        assert_eq!(asked, Some(pinned(&[("GIT_DIR", "/repo/sub/../.git")])));
        assert_eq!(
            found,
            pinned(&[
                ("GIT_DIR", "/repo/sub/../.git"),
                ("GIT_WORK_TREE", "/repo"),
                ("GIT_INDEX_FILE", "/repo/.git/index"),
            ])
        );

        let given = pinned_repository(
            cwd,
            environment(&[
                ("GIT_DIR", "/elsewhere.git"),
                ("GIT_WORK_TREE", ".."),
                ("GIT_OBJECT_DIRECTORY", "objects"),
                ("GIT_COMMON_DIR", "/common"),
            ]),
            unreachable,
        );

        assert_eq!(
            given,
            pinned(&[
                ("GIT_DIR", "/elsewhere.git"),
                ("GIT_WORK_TREE", "/repo/sub/.."),
                ("GIT_COMMON_DIR", "/common"),
                ("GIT_OBJECT_DIRECTORY", "/repo/sub/objects"),
            ])
        );

        let failed = pinned_repository(cwd, environment(&[("GIT_DIR", ".git")]), |_| None);
        assert!(failed.is_empty());
    }

    #[test]
    fn a_relative_repository_variable_pins_to_the_start_folder_without_git_dir() {
        let cwd = Path::new("/repo/sub");
        type Pairs = &'static [(&'static str, &'static str)];
        let cases: [(Pairs, Pairs); 4] = [
            (
                &[("GIT_WORK_TREE", "..")],
                &[("GIT_WORK_TREE", "/repo/sub/..")],
            ),
            (
                &[("GIT_COMMON_DIR", "../.git")],
                &[("GIT_COMMON_DIR", "/repo/sub/../.git")],
            ),
            (
                &[("GIT_OBJECT_DIRECTORY", "../.git/objects")],
                &[("GIT_OBJECT_DIRECTORY", "/repo/sub/../.git/objects")],
            ),
            (
                &[("GIT_WORK_TREE", ".."), ("GIT_INDEX_FILE", ".git/index")],
                &[
                    ("GIT_WORK_TREE", "/repo/sub/.."),
                    ("GIT_INDEX_FILE", "/repo/sub/../.git/index"),
                ],
            ),
        ];

        for (environment, expected) in cases {
            let found = pinned_repository(
                cwd,
                |name| {
                    environment
                        .iter()
                        .find(|(key, _)| *key == name)
                        .map(|(_, value)| OsString::from(value))
                },
                |_| panic!("no toplevel spawn"),
            );

            let expected: Pinned = expected
                .iter()
                .map(|(name, value)| (*name, OsString::from(value)))
                .collect();

            assert_eq!(found, expected);
        }
    }

    #[test]
    fn a_prefetched_answer_serves_one_identical_call() {
        let root = Scratch::new();
        root.git_init();
        let mut repositories = Repositories::new(None);
        repositories.prefetch(&[&root.0], vec![call(&root.0, &["rev-parse", "--git-dir"])]);

        assert!(repositories.variables.contains_key(&root.0));
        assert_eq!(repositories.prefetched.len(), 1);
        assert!(
            repositories
                .git(
                    &root.0,
                    &["rev-parse", "--absolute-git-dir"],
                    None,
                    &GitEnv::Inherited
                )
                .is_ok()
        );
        assert_eq!(repositories.prefetched.len(), 1);
        assert_eq!(
            repositories
                .git(
                    &root.0,
                    &["rev-parse", "--git-dir"],
                    None,
                    &GitEnv::Inherited
                )
                .map(|result| result.output)
                .map_err(GitError::message),
            Ok(b".git\n".to_vec())
        );

        assert!(repositories.prefetched.is_empty());
    }

    #[test]
    fn a_call_with_stdin_never_takes_a_prefetched_answer() {
        let root = Scratch::new();
        root.git_init();
        let args = ["hash-object", "--stdin"];
        let mut repositories = Repositories::new(None);
        repositories.prefetch(&[&root.0], vec![call(&root.0, &args)]);

        let hashed = |repositories: &mut Repositories, stdin: Option<&[u8]>| {
            let result = repositories.git(&root.0, &args, stdin, &GitEnv::Inherited);
            decoded(&result.expect("git hash-object runs").output)
        };

        let empty = hashed(&mut Repositories::new(None), None);
        let written = hashed(&mut Repositories::new(None), Some(b"x"));
        assert_ne!(empty, written);

        assert_eq!(hashed(&mut repositories, Some(b"x")), written);
        assert_eq!(repositories.prefetched.len(), 1);
        assert_eq!(hashed(&mut repositories, None), empty);
        assert!(repositories.prefetched.is_empty());
    }

    #[test]
    fn a_call_with_another_environment_spawns_its_own_git() {
        let root = Scratch::new();
        root.git_init();
        let args = ["-c", "alias.locale=!printf %s \"$LC_ALL\"", "locale"];
        let mut repositories = Repositories::new(None);
        repositories.prefetch(&[&root.0], vec![call(&root.0, &args)]);

        let result = repositories.git(&root.0, &args, None, &GitEnv::CLocale);
        assert_eq!(result.expect("git locale runs").output, b"C");
        assert_eq!(repositories.prefetched.len(), 1);

        assert!(
            repositories
                .git(&root.0, &args, None, &GitEnv::Inherited)
                .is_ok()
        );
        assert!(repositories.prefetched.is_empty());
    }

    #[test]
    fn a_prediction_var_reports_as_configured_elsewhere_is_located_by_git() {
        let root = Scratch::new();
        root.git_init();
        let worktree = join(&root.0, "sub");
        fs::create_dir(&worktree).unwrap();
        assert!(git_succeeds(
            &root.0,
            &["config", "core.worktree", &worktree]
        ));

        let mut repositories = Repositories::new(None);
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

        let mut repositories = Repositories::new(None);
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
        let mut repositories = Repositories::new(None);
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
        let mut repositories = Repositories::new(None);
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
        assert!(git_succeeds(
            &root.0,
            &["worktree", "add", "-q", "--detach", &worktree]
        ));

        let user = unsafe { libc::geteuid() };
        let mut repositories = Repositories::new(None);
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
    fn a_utf8_pointer_resolves_with_any_trailing_line_ending() {
        let root = Scratch::new();
        let target = join(&root.0, "日本");
        fs::create_dir(&target).unwrap();

        for ending in ["", "\n", "\r", "\r\n"] {
            let relative = format!("日本{ending}");
            let absolute = format!("{target}{ending}");
            assert_eq!(
                pointed_path(&root.0, relative.as_bytes()),
                Some(target.clone())
            );
            assert_eq!(
                pointed_path("/elsewhere", absolute.as_bytes()),
                Some(target.clone())
            );
        }

        assert_eq!(pointed_path(&root.0, b"\xe6\x97\xa5\0"), None);
        assert_eq!(pointed_path(&root.0, b"\xff"), None);
    }

    #[test]
    fn a_pointer_with_a_control_byte_is_refused() {
        let root = Scratch::new();
        for name in ["a\u{1}b", "a\u{7f}b"] {
            fs::create_dir(join(&root.0, name)).unwrap();
            assert_eq!(pointed_path(&root.0, name.as_bytes()), None);
            assert_eq!(pointed_path("/", join(&root.0, name).as_bytes()), None);
        }
    }

    fn committed_repository(dir: &str) {
        fs::create_dir_all(dir).unwrap();
        assert!(git_succeeds(dir, &["init", "-q"]));
        assert!(git_succeeds(
            dir,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "a",
            ]
        ));
    }

    fn git_toplevel(dir: &str) -> String {
        let output = Command::new("git")
            .args(["-C", dir, "rev-parse", "--show-toplevel"])
            .output()
            .unwrap();

        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .strip_suffix('\n')
            .unwrap()
            .to_owned()
    }

    #[test]
    fn a_non_ascii_root_is_located_in_process_with_the_bytes_git_prints() {
        let user = unsafe { libc::geteuid() };
        for name in ["ascii", "caf\u{e9}", "cafe\u{301}", "日本"] {
            let scratch = Scratch::new();
            let spelled = join(&scratch.0, name);
            committed_repository(&spelled);
            let root = realpath(&spelled).unwrap();
            let inner = join(&root, "inner");
            fs::create_dir(&inner).unwrap();

            let toplevel = git_toplevel(&inner);
            assert_eq!(toplevel.as_bytes(), root.as_bytes());
            assert_eq!(
                Repositories::new(None).known_location(&inner, user),
                Some(Location::Repository(toplevel))
            );
        }
    }

    #[test]
    fn utf8_gitdir_and_commondir_pointers_are_followed_in_process() {
        let scratch = Scratch::new();
        let main = join(&scratch.0, "日本");
        committed_repository(&main);
        let linked = join(&scratch.0, "linked");
        assert!(git_succeeds(
            &main,
            &["worktree", "add", "-q", "--detach", &linked]
        ));

        let separate = join(&scratch.0, "work");
        let git_dir = join(&scratch.0, "日本-git");
        assert!(git_succeeds(
            &scratch.0,
            &["init", "-q", "--separate-git-dir", &git_dir, &separate]
        ));

        let commondir = join(&join(&main, ".git/worktrees/linked"), "commondir");
        let absolute = join(&scratch.0, "absolute");
        assert!(git_succeeds(
            &main,
            &["worktree", "add", "-q", "--detach", &absolute]
        ));
        fs::write(
            join(&join(&main, ".git/worktrees/absolute"), "commondir"),
            format!("{}\n", join(&main, ".git")),
        )
        .unwrap();

        assert_eq!(fs::read_to_string(commondir).unwrap(), "../..\n");
        let user = unsafe { libc::geteuid() };
        for worktree in [linked, separate, absolute] {
            assert_eq!(git_toplevel(&worktree), worktree);
            assert_eq!(
                Repositories::new(None).known_location(&worktree, user),
                Some(Location::Repository(worktree.clone()))
            );
        }
    }

    #[test]
    fn a_control_byte_in_a_root_or_pointer_component_leaves_discovery_to_git() {
        let user = unsafe { libc::geteuid() };
        for name in ["a\u{1}b", "a\u{7f}b"] {
            let scratch = Scratch::new();
            let root = join(&scratch.0, name);
            committed_repository(&root);
            assert_eq!(Repositories::new(None).known_location(&root, user), None);

            let separate = join(&scratch.0, "work");
            let git_dir = join(&scratch.0, &format!("{name}-git"));
            assert!(git_succeeds(
                &scratch.0,
                &["init", "-q", "--separate-git-dir", &git_dir, &separate]
            ));
            assert_eq!(
                Repositories::new(None).known_location(&separate, user),
                None
            );

            let plain = join(&scratch.0, "plain");
            committed_repository(&plain);
            let linked = join(&scratch.0, "linked");
            assert!(git_succeeds(
                &plain,
                &["worktree", "add", "-q", "--detach", &linked]
            ));
            assert_eq!(
                Repositories::new(None).known_location(&linked, user),
                Some(Location::Repository(linked.clone()))
            );

            let common = join(&scratch.0, &format!("common{name}"));
            std::os::unix::fs::symlink(join(&plain, ".git"), &common).unwrap();
            fs::write(
                join(&join(&plain, ".git/worktrees/linked"), "commondir"),
                format!("{common}\n"),
            )
            .unwrap();
            assert_eq!(Repositories::new(None).known_location(&linked, user), None);
        }
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
        assert!(!main("user.name=a\rextensions.objectformat=sha256"));
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
