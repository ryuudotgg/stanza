use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{
    Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio,
};
use std::rc::Rc;
use std::sync::LazyLock;
use std::thread::{self, JoinHandle};

use super::changed::PreparedListing;
use super::discovery::{self, Discovered, has_git_marker};
use super::index::{IndexCopy, PrivateIndex, RootIndex};
use super::{access, reason};
use crate::paths::{join, relative, resolve};
use crate::signals;

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

pub const GIT_MISSING: &str =
    "git not found on PATH, file selection fell back to the directory walk";

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
        .map(str::trim)
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
    pub(super) non_utf8: Vec<&'a [u8]>,
}

pub(super) fn nul_names(bytes: &[u8]) -> Names<'_> {
    let mut names = Names {
        names: Vec::new(),
        non_utf8: Vec::new(),
    };

    for item in bytes
        .split(|&byte| byte == 0)
        .filter(|item| !item.is_empty())
    {
        match std::str::from_utf8(item) {
            Ok(name) => names.names.push(name),
            Err(_) => names.non_utf8.push(item),
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
    pub(super) entries: Vec<GitVariable>,
}

struct Prefetched {
    env: GitEnv,
    result: Result<GitOutput, GitError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InheritedEnvironment {
    pub(super) walk_allowed: bool,
    pub(super) attributes_uncertain: bool,
    pub(super) index_relocated: bool,
}

impl InheritedEnvironment {
    pub(super) const UNSET: Self = Self::answers(true, false, false);

    const fn answers(
        walk_allowed: bool,
        attributes_uncertain: bool,
        index_relocated: bool,
    ) -> Self {
        InheritedEnvironment {
            walk_allowed,
            attributes_uncertain,
            index_relocated,
        }
    }

    pub(super) fn of(variables: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        variables
            .into_iter()
            .fold(Self::UNSET, |all, (name, value)| {
                let one = Self::variable(&name.to_string_lossy(), &value);
                Self::answers(
                    all.walk_allowed && one.walk_allowed,
                    all.attributes_uncertain || one.attributes_uncertain,
                    all.index_relocated || one.index_relocated,
                )
            })
    }

    fn variable(name: &str, value: &OsStr) -> Self {
        match name {
            "GIT_DIR" | "GIT_WORK_TREE" | "GIT_COMMON_DIR" => Self::answers(false, true, true),
            "GIT_OBJECT_DIRECTORY"
            | "GIT_CEILING_DIRECTORIES"
            | "GIT_TEST_ASSUME_DIFFERENT_OWNER" => Self::answers(false, true, false),
            "GIT_INDEX_FILE" => Self::answers(true, true, true),
            // git var -l prints GIT_EDITOR, GIT_SEQUENCE_EDITOR and GIT_PAGER raw, so a newline could forge a line.
            "GIT_EDITOR"
            | "GIT_SEQUENCE_EDITOR"
            | "GIT_PAGER"
            | "GIT_TERMINAL_PROMPT"
            | "GIT_ASKPASS"
            | "GIT_SSH"
            | "GIT_SSH_COMMAND"
            | "GIT_SSH_VARIANT"
            | "GIT_AUTHOR_NAME"
            | "GIT_AUTHOR_EMAIL"
            | "GIT_AUTHOR_DATE"
            | "GIT_COMMITTER_NAME"
            | "GIT_COMMITTER_EMAIL"
            | "GIT_COMMITTER_DATE"
            | "GIT_OPTIONAL_LOCKS" => {
                Self::answers(true, value.to_string_lossy().contains('\n'), false)
            }
            _ if name.starts_with("GIT_CONFIG") => Self::answers(false, true, false),
            _ => Self::answers(true, name.starts_with("GIT_"), false),
        }
    }
}

#[derive(Default)]
pub(super) struct RootState {
    pub(super) variables: Option<Rc<GitVariables>>,
    pub(super) index: Option<Option<RootIndex>>,
    pub(super) private_index: Option<Option<PrivateIndex>>,
    submodules: Option<Rc<HashSet<String>>>,
}

pub struct Repositories {
    shown_from: Option<String>,
    pub(super) environment: InheritedEnvironment,
    pub(super) roots: HashMap<String, RootState>,
    located: HashMap<String, Location>,
    predicted: HashMap<String, Location>,
    directory_listings: HashMap<String, Rc<Result<GitOutput, String>>>,
    prefetched: HashMap<(String, Vec<String>), Prefetched>,
    pub(super) listings: Vec<Rc<PreparedListing>>,
}

impl Repositories {
    pub fn new(shown_from: Option<&str>) -> Self {
        Self::in_environment(shown_from, InheritedEnvironment::of(std::env::vars_os()))
    }

    pub(super) fn in_environment(
        shown_from: Option<&str>,
        environment: InheritedEnvironment,
    ) -> Self {
        Repositories {
            shown_from: shown_from.map(str::to_owned),
            environment,
            roots: HashMap::new(),
            located: HashMap::new(),
            predicted: HashMap::new(),
            directory_listings: HashMap::new(),
            prefetched: HashMap::new(),
            listings: Vec::new(),
        }
    }

    pub(super) fn root_mut(&mut self, root: &str) -> &mut RootState {
        self.roots.entry(root.to_owned()).or_default()
    }

    pub(super) fn known_variables(&self, root: &str) -> Option<&GitVariables> {
        self.roots.get(root)?.variables.as_deref()
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

        let allowed = self.environment.walk_allowed;
        let walked = has_git()
            .then(|| discovery::walk(dir, unsafe { libc::geteuid() }, allowed))
            .flatten();

        let location = match walked {
            Some(Discovered::Repository(walked)) => Location::Repository(walked.root),
            _ => Location::Outside,
        };
        self.predicted.insert(dir.to_owned(), location.clone());
        location
    }

    pub(super) fn prefetch(&mut self, roots: &[&str], calls: Vec<GitCall>) {
        let unread: Vec<&str> = roots
            .iter()
            .copied()
            .filter(|root| self.known_variables(root).is_none())
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
                self.root_mut(&call.cwd).variables = Some(variables);
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
    pub(super) fn filled(&self, root: &str) -> Vec<&'static str> {
        let Some(state) = self.roots.get(root) else {
            return Vec::new();
        };

        [
            ("variables", state.variables.is_some()),
            ("index", state.index.is_some()),
            ("private index", state.private_index.is_some()),
            ("submodules", state.submodules.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, filled)| filled.then_some(name))
        .collect()
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

    fn known_submodules(&self, root: &str) -> Option<Rc<HashSet<String>>> {
        self.roots.get(root)?.submodules.clone()
    }

    pub(super) fn submodule_paths(&mut self, root: &str) -> Rc<HashSet<String>> {
        if let Some(known) = self.known_submodules(root) {
            return known;
        }

        match submodules_call(root) {
            Some(call) => self.listed_submodules(&call),
            None => self
                .root_mut(root)
                .submodules
                .get_or_insert_default()
                .clone(),
        }
    }

    pub(super) fn listed_submodules(&mut self, call: &GitCall) -> Rc<HashSet<String>> {
        let root = &call.cwd;
        if let Some(known) = self.known_submodules(root) {
            return known;
        }

        let paths = self
            .run(call)
            .map(|result| listed_submodule_paths(root, &result.output))
            .unwrap_or_default();

        let paths = Rc::new(paths);
        self.root_mut(root).submodules = Some(paths.clone());
        paths
    }

    fn git_variables(&mut self, root: &str) -> Rc<GitVariables> {
        if let Some(cached) = self
            .roots
            .get(root)
            .and_then(|state| state.variables.clone())
        {
            return cached;
        }

        let output = self.run(&variables_call(root));
        let variables = Rc::new(parsed_variables(output));
        self.root_mut(root).variables = Some(variables.clone());
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

    fn known_location(&mut self, dir: &str, user: u32) -> Option<Location> {
        match discovery::walk(dir, user, self.environment.walk_allowed)? {
            Discovered::Repository(walked) => {
                let variables = self.git_variables(&walked.root);
                walked
                    .plain_config(&variables)
                    .then_some(Location::Repository(walked.root))
            }
            Discovered::Outside => Some(Location::Outside),
        }
    }
}

fn variables_call(root: &str) -> GitCall {
    GitCall {
        cwd: root.to_owned(),
        args: vec!["var".to_owned(), "-l".to_owned()],
        env: GitEnv::DefaultEmail,
    }
}

pub(super) fn parsed_variables(result: Result<GitOutput, GitError>) -> GitVariables {
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

    GitVariables { readable, entries }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::discovery::git_dirs;
    use crate::files::realpath;

    #[test]
    fn git_output_keeps_a_leading_byte_order_mark() {
        let listed = b"\xef\xbb\xbfa.ts\0\xef\xbb\xbfb.ts";
        assert_eq!(decoded(listed), "\u{feff}a.ts\0\u{feff}b.ts");
        assert_eq!(decoded(b"a\xff"), "a\u{fffd}");

        let inputs: [&[u8]; 6] = [
            listed,
            b"",
            b"diff --git a/a.ts b/a.ts\n+x\n",
            b"\xef\xbb\xbfdiff\n",
            b"\xef\xbb\xbf\xef\xbb\xbf\xff",
            b"+\xff\xfe bad\n+\xe2\x82 cut",
        ];
        for bytes in inputs {
            assert_eq!(decoded_owned(bytes.to_vec()), decoded(bytes), "{bytes:?}");
        }
    }

    #[test]
    fn listed_names_keep_the_bytes_of_a_name_that_is_not_utf8() {
        let names = nul_names(b"\xef\xbb\xbfa.ts\0bad\xff.ts\0\0\xef\xbf\xbd.ts\0");
        assert_eq!(names.names, ["\u{feff}a.ts", "\u{fffd}.ts"]);
        assert_eq!(names.non_utf8, [&b"bad\xff.ts"[..]]);
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
        let email = command
            .get_envs()
            .find(|(key, _)| *key == "EMAIL")
            .map(|(_, value)| value);

        assert_eq!(call.args, ["var", "-l"]);
        assert_eq!(email, Some(Some(std::ffi::OsStr::new("stanza@localhost"))));
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

        assert!(repositories.known_variables(&root.0).is_some());
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

    fn inherited(variables: &[(&str, &str)]) -> InheritedEnvironment {
        InheritedEnvironment::of(
            variables
                .iter()
                .map(|(name, value)| (OsString::from(name), OsString::from(value))),
        )
    }

    const NAMED_ENVIRONMENT: [(&str, (bool, bool, bool)); 12] = [
        ("GIT_DIR", (false, true, true)),
        ("GIT_WORK_TREE", (false, true, true)),
        ("GIT_COMMON_DIR", (false, true, true)),
        ("GIT_OBJECT_DIRECTORY", (false, true, false)),
        ("GIT_CEILING_DIRECTORIES", (false, true, false)),
        ("GIT_TEST_ASSUME_DIFFERENT_OWNER", (false, true, false)),
        ("GIT_INDEX_FILE", (true, true, true)),
        ("GIT_CONFIG", (false, true, false)),
        ("GIT_CONFIG_COUNT", (false, true, false)),
        ("GIT_CONFIG_GLOBAL", (false, true, false)),
        ("GIT_DISCOVERY_ACROSS_FILESYSTEM", (true, true, false)),
        ("GIT_FUTURE_SETTING", (true, true, false)),
    ];

    const INERT_ENVIRONMENT: [&str; 15] = [
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

    fn answers(environment: InheritedEnvironment) -> (bool, bool, bool) {
        (
            environment.walk_allowed,
            environment.attributes_uncertain,
            environment.index_relocated,
        )
    }

    #[test]
    fn git_environment_answers_follow_the_classification() {
        let unset = (true, false, false);
        assert_eq!(answers(inherited(&[])), unset);
        assert_eq!(answers(InheritedEnvironment::UNSET), unset);

        for (name, expected) in NAMED_ENVIRONMENT {
            for value in ["", "x", "x\ny"] {
                assert_eq!(
                    answers(inherited(&[(name, value)])),
                    expected,
                    "{name}={value:?}"
                );
            }
        }

        for name in INERT_ENVIRONMENT {
            assert_eq!(answers(inherited(&[(name, "")])), unset, "{name}");
            assert_eq!(answers(inherited(&[(name, "x")])), unset, "{name}");
            assert_eq!(
                answers(inherited(&[(name, "x\ny")])),
                (true, true, false),
                "{name}"
            );
        }

        for name in [
            "HOME",
            "EDITOR",
            "XGIT_DIR",
            "git_dir",
            "GIT",
            "GITHUB_TOKEN",
        ] {
            for value in ["", "x", "x\ny"] {
                assert_eq!(
                    answers(inherited(&[(name, value)])),
                    unset,
                    "{name}={value:?}"
                );
            }
        }
    }

    #[test]
    fn git_environment_answers_combine_across_variables() {
        let both = [("GIT_INDEX_FILE", "i"), ("GIT_CONFIG_COUNT", "0")];
        assert_eq!(answers(inherited(&both)), (false, true, true));

        let reversed = [both[1], both[0]];
        assert_eq!(answers(inherited(&reversed)), (false, true, true));

        let inert = [
            ("GIT_PAGER", "cat"),
            ("HOME", "/home"),
            ("GIT_EDITOR", "vi"),
        ];
        assert_eq!(answers(inherited(&inert)), (true, false, false));

        let forged = [("GIT_PAGER", "cat\nx"), ("GIT_OBJECT_DIRECTORY", "")];
        assert_eq!(answers(inherited(&forged)), (false, true, false));

        let every: Vec<(&str, &str)> = NAMED_ENVIRONMENT
            .iter()
            .map(|(name, _)| (*name, ""))
            .chain(INERT_ENVIRONMENT.map(|name| (name, "")))
            .collect();

        assert_eq!(answers(inherited(&every)), (false, true, true));
    }

    #[test]
    fn git_environment_reading_decides_whether_discovery_walks() {
        let user = unsafe { libc::geteuid() };
        let scratch = Scratch::new();
        let root = realpath(&scratch.0).unwrap();
        committed_repository(&root);
        let located = |variables: &[(&str, &str)]| {
            Repositories::in_environment(None, inherited(variables)).known_location(&root, user)
        };

        assert_eq!(located(&[]), Some(Location::Repository(root.clone())));
        assert_eq!(
            located(&[("GIT_FUTURE_SETTING", "1")]),
            Some(Location::Repository(root.clone()))
        );
        assert_eq!(located(&[("GIT_CEILING_DIRECTORIES", "")]), None);
        assert_eq!(located(&[("GIT_CONFIG_COUNT", "0")]), None);
    }

    type Trigger = (
        &'static str,
        InheritedEnvironment,
        fn(&mut Repositories, &str),
        &'static [&'static str],
    );

    #[test]
    fn repository_state_filling_one_root_field_fills_no_others() {
        let root = Scratch::new();
        root.git_init();
        let unset = InheritedEnvironment::UNSET;
        let relocated = inherited(&[("GIT_INDEX_FILE", "")]);
        let triggers: [Trigger; 7] = [
            (
                "process filters",
                unset,
                |repositories, root| {
                    repositories.process_filters(root);
                },
                &["variables"],
            ),
            (
                "submodule paths",
                unset,
                |repositories, root| {
                    repositories.submodule_paths(root);
                },
                &["submodules"],
            ),
            (
                "tracks all",
                unset,
                |repositories, root| {
                    repositories.tracks_all(root, &["a.ts".to_owned()]);
                },
                &["index"],
            ),
            (
                "worktree diff",
                unset,
                |repositories, root| {
                    repositories.worktree_diff(root);
                },
                &["index", "private index"],
            ),
            (
                "relocated worktree diff",
                relocated,
                |repositories, root| {
                    repositories.worktree_diff(root);
                },
                &["private index"],
            ),
            (
                "locate",
                unset,
                |repositories, root| {
                    repositories.locate(root);
                },
                &["variables"],
            ),
            (
                "predict",
                unset,
                |repositories, root| {
                    repositories.predict(root);
                },
                &[],
            ),
        ];

        for (name, environment, trigger, filled) in triggers {
            let mut repositories = Repositories::in_environment(None, environment);
            for _ in 0..2 {
                trigger(&mut repositories, &root.0);
                assert_eq!(repositories.filled(&root.0), filled, "{name}");
                assert_eq!(
                    repositories.roots.len(),
                    usize::from(!filled.is_empty()),
                    "{name}"
                );
            }
        }

        let outside = Scratch::new();
        let mut repositories = Repositories::in_environment(None, unset);
        assert!(!repositories.tracks_all(&outside.0, &["a.ts".to_owned()]));
        assert!(matches!(repositories.roots[&outside.0].index, Some(None)));
    }

    #[test]
    fn repository_state_discovery_answers_a_root_prediction_never_walked() {
        let predicted = Scratch::new();
        predicted.git_init();
        let located = Scratch::new();
        located.git_init();
        let mut repositories = Repositories::in_environment(None, InheritedEnvironment::UNSET);
        assert_eq!(
            repositories.predict(&predicted.0),
            Location::Repository(predicted.0.clone())
        );

        assert_eq!(
            repositories.locate(&located.0),
            Location::Repository(located.0.clone())
        );
        assert_eq!(repositories.filled(&predicted.0), Vec::<&str>::new());
        assert_eq!(repositories.filled(&located.0), ["variables"]);
        assert!(!repositories.roots.contains_key(&predicted.0));
    }

    #[test]
    fn repository_state_a_root_located_only_by_rev_parse_gets_no_variables() {
        let root = Scratch::new();
        root.git_init();
        let mut repositories =
            Repositories::in_environment(None, inherited(&[("GIT_CONFIG_COUNT", "0")]));
        assert_eq!(
            repositories.locate(&root.0),
            Location::Repository(root.0.clone())
        );
        assert!(repositories.roots.is_empty());

        let worktree = join(&root.0, "sub");
        fs::create_dir(&worktree).unwrap();
        assert!(git_succeeds(
            &root.0,
            &["config", "core.worktree", &worktree]
        ));

        let mut repositories = Repositories::in_environment(None, InheritedEnvironment::UNSET);
        assert_eq!(
            repositories.locate(&root.0),
            Location::Repository(worktree.clone())
        );
        assert_eq!(repositories.filled(&root.0), ["variables"]);
        assert!(!repositories.roots.contains_key(&worktree));
    }
}
