use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::process::{Command, ExitStatus, Output, Stdio};
use std::rc::Rc;
use std::sync::LazyLock;
use std::thread;

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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum GitEnv {
    Inherited,
    CLocale,
    NoDiffOpts,
}

fn spawn_git(cwd: &str, args: &[&str], stdin: Option<&[u8]>, env: GitEnv) -> io::Result<Output> {
    let mut command = Command::new("git");
    command.arg("-C").arg(cwd).args(args);
    match env {
        GitEnv::Inherited => {}
        GitEnv::CLocale => {
            command.env("LC_ALL", "C");
        }
        GitEnv::NoDiffOpts => {
            command.env_remove("GIT_DIFF_OPTS");
        }
    }

    command
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn()?;
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
    let command = args.iter().find(|arg| !arg.starts_with('-')).unwrap_or(&"");
    let failed = |reason: String| format!("git {command} failed in {cwd}: {reason}");
    let output = spawn_git(cwd, args, stdin, env).map_err(GitError::Spawn)?;
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
    run_git_spawned(cwd, args, stdin, env).map_err(|error| match error {
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

pub(super) fn decoded(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)).into_owned()
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

        if let Some(root) = self.known_root(dir, unsafe { libc::geteuid() }) {
            return Location::Repository(root);
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

    fn git_variables(&mut self, root: &str) -> Rc<GitVariables> {
        if let Some(cached) = self.variables.get(root) {
            return cached.clone();
        }

        let output = spawn_git(root, &["var", "-l"], None, GitEnv::Inherited);

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

        let variables = Rc::new(GitVariables {
            readable,
            text,
            entries,
        });

        self.variables.insert(root.to_owned(), variables.clone());
        variables
    }

    fn plain_config(&mut self, root: &str) -> bool {
        let variables = self.git_variables(root);
        variables.readable && !discovery_config(&variables.text)
    }

    fn plain_repository(
        &mut self,
        worktree: &str,
        marker: &str,
        marker_meta: &fs::Metadata,
        user: u32,
    ) -> io::Result<bool> {
        if lstat(worktree)?.uid() != user || marker_meta.uid() != user {
            return Ok(false);
        }

        let head = join(marker, "HEAD");
        if !lstat(&head)?.is_file() {
            return Ok(false);
        }

        let mut bytes = [0u8; 255];
        let read = fs::File::open(&head)?.read(&mut bytes)?;
        if !valid_head(&bytes[..read]) {
            return Ok(false);
        }

        access(&join(marker, "objects"), libc::X_OK)?;
        access(&join(marker, "refs"), libc::X_OK)?;
        if lstat_if_present(&join(marker, "commondir"))?.is_some() {
            return Ok(false);
        }

        Ok(self.plain_config(worktree))
    }

    fn known_root(&mut self, dir: &str, user: u32) -> Option<String> {
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
                let plain = entry.is_dir()
                    && self
                        .plain_repository(&current, &marker, &entry, user)
                        .ok()?;

                return plain.then_some(current);
            }

            if lstat_if_present(&join(&current, "HEAD")).ok()?.is_some() {
                return None;
            }

            let parent = dirname(&current);
            if parent == current || fs::metadata(&parent).ok()?.dev() != device {
                return None;
            }

            current = parent;
        }
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

fn discovery_config(text: &str) -> bool {
    js_lines(text).any(|line| {
        ascii_prefix(line, "extensions.").is_some()
            || ascii_prefix(line, "core.worktree").is_some_and(|rest| setting_matches(rest, &[]))
            || ascii_prefix(line, "core.bare")
                .is_some_and(|rest| setting_matches(rest, &["false", "no", "off", "0"]))
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

    #[test]
    fn a_repository_owned_by_another_user_has_no_known_root() {
        let root = Scratch::new();
        root.git_init();
        let user = unsafe { libc::geteuid() };
        let mut repositories = Repositories::default();
        assert_eq!(repositories.known_root(&root.0, user), Some(root.0.clone()));
        assert_eq!(repositories.known_root(&root.0, user + 1), None);
    }

    #[test]
    fn reads_discovery_settings_like_the_reference_patterns() {
        assert!(discovery_config("core.worktree=/x\n"));
        assert!(discovery_config("x=1\nCORE.BARE\n"));
        assert!(discovery_config("core.bare=true"));
        assert!(!discovery_config("core.bare=FALSE\ncore.bare=0"));
        assert!(discovery_config("core.repositoryformatversion=2"));
        assert!(!discovery_config(
            "core.repositoryformatversion=1\ncore.worktrees=x"
        ));
        assert!(discovery_config(
            "user.name=a\rextensions.objectformat=sha256"
        ));
    }
}
