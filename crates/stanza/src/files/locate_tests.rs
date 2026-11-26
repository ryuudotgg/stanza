use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Output};
use std::sync::LazyLock;

use super::{Location, Repositories};
use crate::os::{access, realpath};
use crate::paths::join;
use crate::testing::scratch::{Scratch, authored_git, git};

#[path = "locate_mount_tests.rs"]
mod mount;

const EXCHANGE: &str = "STANZA_TEST_LOCATE";

const VALUE: &str = "export const value = 1;\n";

const EMPTY_MODULE: &str = "export {};\n";

static REAL_GIT: LazyLock<String> = LazyLock::new(|| {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .filter_map(|dir| std::path::absolute(dir.join("git")).ok())
        .filter_map(|candidate| candidate.into_os_string().into_string().ok())
        .find(|candidate| {
            fs::metadata(candidate).is_ok_and(|meta| meta.is_file())
                && access(candidate, libc::X_OK).is_ok()
        })
        .expect("git is on PATH")
});

#[derive(Clone, Copy, PartialEq, Eq)]
enum Failure {
    Failed,
    Outside,
}

struct Environment {
    changes: Vec<(String, Option<String>)>,
}

const INHERITED: Environment = Environment {
    changes: Vec::new(),
};

struct Traced {
    locations: Vec<Location>,
    calls: Vec<Vec<String>>,
}

impl Traced {
    fn spawned(&self, argument: &str) -> usize {
        self.calls
            .iter()
            .filter(|call| call.iter().any(|arg| arg == argument))
            .count()
    }
}

impl Environment {
    fn isolated(home: &str) -> Environment {
        let inherited = std::env::vars_os()
            .filter_map(|(name, _)| name.into_string().ok())
            .filter(|name| name.starts_with("GIT_"))
            .map(|name| (name, None));

        let mut changes: Vec<(String, Option<String>)> = inherited.collect();
        changes.push(("HOME".to_owned(), Some(home.to_owned())));
        changes.push(("XDG_CONFIG_HOME".to_owned(), Some(join(home, ".config"))));
        Environment { changes }
    }

    fn with(mut self, name: &str, value: &str) -> Environment {
        self.changes.push((name.to_owned(), Some(value.to_owned())));
        self
    }

    fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        for (name, value) in &self.changes {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }

        command
    }

    fn rev_parse(&self, dir: &str) -> Output {
        self.command(&*REAL_GIT)
            .args(["-C", dir, "rev-parse", "--show-toplevel"])
            .output()
            .unwrap()
    }

    fn git_location(&self, dir: &str, failure: Failure) -> Location {
        let output = self.rev_parse(dir);
        let stdout = String::from_utf8(output.stdout).unwrap();
        let root = stdout.strip_suffix('\n').unwrap_or(&stdout);
        if output.status.success() && !root.is_empty() {
            return Location::Repository(root.to_owned());
        }

        if failure == Failure::Outside {
            return Location::Outside;
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let reason = stderr
            .split('\n')
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| match output.status.code() {
                Some(code) => format!("exit {code}"),
                None => format!(
                    "killed by signal {}",
                    output.status.signal().unwrap_or_default()
                ),
            });

        Location::Failed(format!("git rev-parse failed in {dir}: {reason}"))
    }

    fn expect(&self, dir: &str, discoveries: usize) -> Location {
        self.expect_as(dir, discoveries, Failure::Failed)
    }

    fn expect_outside(&self, dir: &str, discoveries: usize) -> Location {
        self.expect_as(dir, discoveries, Failure::Outside)
    }

    fn expect_as(&self, dir: &str, discoveries: usize, failure: Failure) -> Location {
        let expected = self.git_location(dir, failure);

        let traced = self.traced(&[dir]);
        assert_eq!(traced.locations, std::slice::from_ref(&expected), "{dir:?}");
        assert_eq!(
            traced.spawned("--show-toplevel"),
            discoveries,
            "{dir:?}: {:?}",
            traced.calls
        );
        expected
    }

    fn traced(&self, dirs: &[&str]) -> Traced {
        let exchange = Scratch::new("locate");
        let calls = directory(&exchange.join("calls"));
        let bin = directory(&exchange.join("bin"));
        let shim = join(&bin, "git");
        fs::write(
            &shim,
            format!(
                "#!/bin/sh\nprintf '%s\\000' \"$#\" \"$@\" >> {}/$$ || exit 125\nexec {} \"$@\"\n",
                quoted(&calls),
                quoted(&REAL_GIT)
            ),
        )
        .unwrap();

        fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(exchange.join("dirs"), dirs.join("\0")).unwrap();

        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(bin.into()).chain(std::env::split_paths(&inherited)),
        )
        .expect("the shim folder fits on PATH");

        let output = self
            .command(std::env::current_exe().unwrap())
            .args(["--exact", &child_test(), "--ignored", "--nocapture"])
            .current_dir(dirs[0])
            .env(EXCHANGE, exchange.path())
            .env("PATH", path)
            .output()
            .unwrap();

        assert!(output.status.success(), "{}", shown(&output));
        let Ok(located) = fs::read(exchange.join("locations")) else {
            panic!("the child wrote no locations: {}", shown(&output));
        };

        Traced {
            locations: decoded_locations(&located),
            calls: recorded_calls(&calls),
        }
    }
}

fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn child_test() -> String {
    let (_, module) = module_path!().split_once("::").unwrap();
    format!("{module}::answers_its_parent")
}

fn shown(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
#[ignore = "Environment::traced runs it in a child process with the git shim first on PATH"]
fn answers_its_parent() {
    let Ok(exchange) = std::env::var(EXCHANGE) else {
        return;
    };

    let dirs = fs::read_to_string(join(&exchange, "dirs")).unwrap();
    let mut repositories = Repositories::new(None);
    let mut written = String::new();
    for dir in dirs.split('\0') {
        let (kind, value) = match repositories.locate(dir) {
            Location::Repository(root) => ("repository", root),
            Location::Outside => ("outside", String::new()),
            Location::Failed(error) => ("failed", error),
        };

        written.push_str(&format!("{kind}\0{value}\0"));
    }

    fs::write(join(&exchange, "locations"), written).unwrap();
}

fn decoded_locations(bytes: &[u8]) -> Vec<Location> {
    let text = std::str::from_utf8(bytes).unwrap();
    let fields: Vec<&str> = text.strip_suffix('\0').unwrap().split('\0').collect();
    fields
        .chunks(2)
        .map(|pair| match pair {
            ["repository", root] => Location::Repository((*root).to_owned()),
            ["outside", ""] => Location::Outside,
            ["failed", error] => Location::Failed((*error).to_owned()),
            torn => panic!("a torn location {torn:?}"),
        })
        .collect()
}

fn recorded_calls(dir: &str) -> Vec<Vec<String>> {
    let mut calls = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let bytes = fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        let mut fields = text.strip_suffix('\0').unwrap().split('\0');
        while let Some(count) = fields.next() {
            let count: usize = count.parse().unwrap();
            let call: Vec<String> = fields.by_ref().take(count).map(str::to_owned).collect();

            assert_eq!(call.len(), count, "a torn git record in {path:?}");
            calls.push(call);
        }
    }

    calls
}

fn directory(path: &str) -> String {
    fs::create_dir_all(path).unwrap();
    path.to_owned()
}

fn initialized(dir: &str) -> String {
    directory(dir);
    git(dir, &["init", "-q"]);
    dir.to_owned()
}

fn committed(dir: &str, source: &str) -> String {
    initialized(dir);
    fs::write(join(dir, "a.ts"), source).unwrap();
    git(dir, &["add", "a.ts"]);
    authored_git(dir, &["commit", "-qm", "init"]);
    dir.to_owned()
}

fn append(path: &str, text: &str) {
    let mut bytes = fs::read(path).unwrap();
    bytes.extend_from_slice(text.as_bytes());
    fs::write(path, bytes).unwrap();
}

fn config(root: &str) -> String {
    join(root, ".git/config")
}

struct Linked {
    worktree: String,
    gitdir: String,
}

fn linked_worktree(scratch: &Scratch) -> Linked {
    let root = committed(&scratch.join("repo"), VALUE);
    let worktree = scratch.join("worktree");
    git(
        &root,
        &["worktree", "add", "--quiet", "--detach", &worktree],
    );
    Linked {
        worktree,
        gitdir: join(&root, ".git/worktrees/worktree"),
    }
}

struct Submodule {
    checkout: String,
    gitdir: String,
}

fn submodule_checkout(scratch: &Scratch) -> Submodule {
    let source = committed(&scratch.join("source"), VALUE);
    let root = initialized(&scratch.join("repo"));
    git(
        &root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--quiet",
            &source,
            "vendor/sub",
        ],
    );

    Submodule {
        checkout: join(&root, "vendor/sub"),
        gitdir: join(&root, ".git/modules/vendor/sub"),
    }
}

fn is_failed(location: &Location) -> bool {
    matches!(location, Location::Failed(_))
}

#[test]
fn plain_root_and_nested_directory_match_git_without_discovery_spawns() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let nested = directory(&join(&root, "src/nested"));

    let located = Location::Repository(root.clone());
    assert_eq!(INHERITED.expect(&root, 0), located);
    assert_eq!(INHERITED.expect(&nested, 0), located);
}

#[test]
fn nested_repository_takes_precedence_without_a_discovery_spawn() {
    let scratch = Scratch::new("locate");
    let outer = initialized(&scratch.join("repo"));
    let inner = initialized(&join(&outer, "inner"));

    assert_eq!(INHERITED.expect(&inner, 0), Location::Repository(inner));
}

#[test]
fn config_validation_runs_once_per_repository_per_invocation() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let nested = directory(&join(&root, "nested"));
    let located = Location::Repository(root.clone());
    assert_eq!(INHERITED.git_location(&nested, Failure::Failed), located);

    let first = INHERITED.traced(&[&root, &nested]);
    assert_eq!(first.locations, [located.clone(), located.clone()]);
    assert_eq!(first.spawned("var"), 1, "{:?}", first.calls);
    assert_eq!(first.spawned("--show-toplevel"), 0, "{:?}", first.calls);

    let second = INHERITED.traced(&[&nested]);
    assert_eq!(second.locations, [located]);
    assert_eq!(second.spawned("var"), 1, "{:?}", second.calls);
    assert_eq!(second.spawned("--show-toplevel"), 0, "{:?}", second.calls);
}

#[test]
fn symlinked_directory_discovers_the_real_repository_root() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let nested = directory(&join(&root, "nested"));
    let link = scratch.join("linked");
    std::os::unix::fs::symlink(&nested, &link).unwrap();

    assert_eq!(INHERITED.expect(&link, 0), Location::Repository(root));
}

#[test]
fn a_linked_worktree_is_located_without_a_discovery_spawn() {
    let scratch = Scratch::new("locate");
    let Linked { worktree, .. } = linked_worktree(&scratch);
    let nested = directory(&join(&worktree, "src"));

    let located = Location::Repository(worktree.clone());
    assert_eq!(INHERITED.expect(&worktree, 0), located);
    assert_eq!(INHERITED.expect(&nested, 0), located);
}

#[test]
fn a_submodule_checkout_is_located_without_a_discovery_spawn() {
    let scratch = Scratch::new("locate");
    let Submodule { checkout, .. } = submodule_checkout(&scratch);

    assert_eq!(
        INHERITED.expect(&checkout, 0),
        Location::Repository(checkout)
    );
}

#[test]
fn a_submodule_whose_core_worktree_names_another_directory_falls_back_to_git() {
    let scratch = Scratch::new("locate");
    let Submodule { checkout, gitdir } = submodule_checkout(&scratch);
    let elsewhere = directory(&scratch.join("elsewhere"));
    git(&gitdir, &["config", "core.worktree", &elsewhere]);

    assert_eq!(
        INHERITED.expect(&checkout, 1),
        Location::Repository(elsewhere)
    );
}

#[test]
fn core_worktree_naming_the_root_needs_no_discovery_spawn() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let start = directory(&join(&root, "sub"));
    append(&config(&root), &format!("[core]\n\tworktree = {root}\n"));

    assert_eq!(INHERITED.expect(&start, 0), Location::Repository(root));
}

#[cfg(target_os = "macos")]
#[test]
fn core_worktree_naming_the_root_in_another_letter_case_matches_git() {
    let scratch = Scratch::new("locate");
    let root = scratch.join("Repo");
    let start = directory(&join(&root, "sub"));
    git(&root, &["init", "-q"]);
    git(&root, &["config", "core.worktree", &scratch.join("REPO")]);

    assert_eq!(
        INHERITED.expect(&start, 1),
        Location::Repository(scratch.join("REPO"))
    );
}

#[test]
fn a_linked_worktree_of_a_submodule_ignores_the_shared_core_worktree_like_git() {
    let scratch = Scratch::new("locate");
    let Submodule { checkout, .. } = submodule_checkout(&scratch);
    let worktree = scratch.join("worktree");
    git(
        &checkout,
        &["worktree", "add", "--quiet", "--detach", &worktree],
    );

    assert_eq!(
        INHERITED.expect(&worktree, 0),
        Location::Repository(worktree)
    );
}

fn split_worktree_value_matches_git(value: fn(&str) -> String) {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let start = directory(&join(&root, "sub"));
    append(
        &config(&root),
        &format!("[core]\n\tworktree = {}\n", value(&root)),
    );

    INHERITED.expect(&start, 1);
}

#[test]
fn core_worktree_naming_the_root_before_a_raw_u_2028_matches_git() {
    split_worktree_value_matches_git(|root| format!("{root}\u{2028}x"));
}

#[test]
fn core_worktree_naming_the_root_before_an_escaped_line_break_matches_git() {
    split_worktree_value_matches_git(|root| format!("\"{root}\\nfoo.bar\""));
}

#[test]
fn a_relative_core_worktree_ending_in_a_quoted_carriage_return_matches_git() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let start = directory(&join(&root, "sub"));
    let named = directory(&join(&root, "\r"));
    append(&config(&root), "[core]\n\tworktree = \"../\r\"\n");

    assert_eq!(INHERITED.expect(&start, 1), Location::Repository(named));
}

#[test]
fn a_linked_worktree_of_a_bare_clone_ignores_the_shared_core_bare_like_git() {
    let scratch = Scratch::new("locate");
    let source = committed(&scratch.join("source"), VALUE);
    let bare = scratch.join("bare.git");
    git(&source, &["clone", "--quiet", "--bare", &source, &bare]);
    let worktree = scratch.join("worktree");
    git(
        &bare,
        &["worktree", "add", "--quiet", "--detach", &worktree],
    );

    assert_eq!(
        INHERITED.expect(&worktree, 0),
        Location::Repository(worktree)
    );
}

fn broken_git_file_falls_back_to_git(damage: fn(&Linked)) {
    let scratch = Scratch::new("locate");
    let linked = linked_worktree(&scratch);
    damage(&linked);

    INHERITED.expect(&linked.worktree, 1);
}

#[test]
fn a_git_file_whose_gitdir_is_missing_falls_back_to_git() {
    broken_git_file_falls_back_to_git(|linked| fs::remove_dir_all(&linked.gitdir).unwrap());
}

#[test]
fn a_git_file_whose_gitdir_is_without_head_falls_back_to_git() {
    broken_git_file_falls_back_to_git(|linked| {
        fs::remove_file(join(&linked.gitdir, "HEAD")).unwrap();
    });
}

#[test]
fn a_git_file_whose_gitdir_is_named_by_a_line_that_is_not_gitdir_falls_back_to_git() {
    broken_git_file_falls_back_to_git(|linked| {
        fs::write(join(&linked.worktree, ".git"), "x\n").unwrap();
    });
}

#[test]
fn a_git_file_whose_gitdir_is_sharing_a_missing_common_dir_falls_back_to_git() {
    broken_git_file_falls_back_to_git(|linked| {
        fs::write(join(&linked.gitdir, "commondir"), "../x\n").unwrap();
    });
}

#[test]
fn a_git_file_whose_gitdir_is_unreadable_falls_back_to_git() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    broken_git_file_falls_back_to_git(|linked| {
        fs::set_permissions(&linked.gitdir, fs::Permissions::from_mode(0o000)).unwrap();
    });
}

#[test]
fn git_directory_missing_head_falls_back_to_git() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    fs::remove_file(join(&root, ".git/HEAD")).unwrap();

    assert!(is_failed(&INHERITED.expect(&root, 1)));
}

#[test]
fn path_inside_git_directory_falls_back_to_git() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));

    assert_eq!(
        INHERITED.expect_outside(&join(&root, ".git"), 1),
        Location::Outside
    );
}

#[test]
fn git_dir_defined_at_call_time_falls_back_to_git() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let environment = INHERITED.with("GIT_DIR", &join(&root, ".git"));

    assert_eq!(environment.expect(&root, 2), Location::Repository(root));
}

#[test]
fn core_worktree_falls_back_to_gits_configured_root() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let worktree = directory(&scratch.join("elsewhere"));
    git(&root, &["config", "core.worktree", &worktree]);

    assert_eq!(INHERITED.expect(&root, 1), Location::Repository(worktree));
}

#[test]
fn core_bare_true_falls_back_to_gits_failure() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    git(&root, &["config", "core.bare", "true"]);

    assert!(is_failed(&INHERITED.expect(&root, 1)));
}

#[test]
fn valueless_core_bare_falls_back_to_gits_failure() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    append(&config(&root), "[core]\n\tbare\n");

    assert!(is_failed(&INHERITED.expect(&root, 1)));
}

#[test]
fn included_config_without_discovery_keys_needs_no_discovery_spawn() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let included = scratch.join("included.config");
    fs::write(&included, "[user]\n\tname = t\n").unwrap();
    git(&root, &["config", "include.path", &included]);

    assert_eq!(INHERITED.expect(&root, 0), Location::Repository(root));
}

fn included_core_worktree_matches_gits_root(include: fn(&str, &str)) {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let included = scratch.join("included.config");
    let elsewhere = directory(&scratch.join("elsewhere"));
    fs::write(&included, format!("[core]\n\tworktree = {elsewhere}\n")).unwrap();
    include(&root, &included);

    assert_eq!(INHERITED.expect(&root, 1), Location::Repository(root));
}

#[test]
fn include_setting_core_worktree_matches_gits_root() {
    included_core_worktree_matches_gits_root(|root, included| {
        git(root, &["config", "include.path", included]);
    });
}

#[test]
fn include_if_gitdir_setting_core_worktree_matches_gits_root() {
    included_core_worktree_matches_gits_root(|root, included| {
        let key = format!("includeIf.gitdir:{root}/.git.path");
        git(root, &["config", &key, included]);
    });
}

#[test]
fn include_if_onbranch_setting_core_worktree_matches_gits_root() {
    included_core_worktree_matches_gits_root(|root, included| {
        git(root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(root, &["config", "includeIf.onbranch:main.path", included]);
    });
}

#[test]
fn include_if_hasconfig_setting_core_worktree_matches_gits_root() {
    included_core_worktree_matches_gits_root(|root, included| {
        git(
            root,
            &["config", "remote.origin.url", "https://example.com/x.git"],
        );
        append(
            &config(root),
            &format!(
                "[includeIf \"hasconfig:remote.*.url:https://example.com/**\"]\n\tpath = {included}\n"
            ),
        );
    });
}

#[test]
fn a_root_ending_in_a_space_keeps_the_space_on_both_discovery_paths() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("trailing "));
    let located = Location::Repository(root.clone());

    assert_eq!(INHERITED.expect(&root, 0), located);
    assert_eq!(
        INHERITED.with("GIT_CONFIG_COUNT", "0").expect(&root, 1),
        located
    );
}

fn root_is_located_in_process_with_gits_bytes(name: &str) {
    let scratch = Scratch::new("locate");
    let root = realpath(&committed(&scratch.join(name), EMPTY_MODULE)).unwrap();

    assert_eq!(
        INHERITED.expect(&root, 0),
        Location::Repository(root.clone())
    );
}

#[test]
fn an_ascii_root_is_located_in_process_with_gits_bytes() {
    root_is_located_in_process_with_gits_bytes("ascii");
}

#[test]
fn an_nfc_root_is_located_in_process_with_gits_bytes() {
    root_is_located_in_process_with_gits_bytes("caf\u{e9}");
}

#[test]
fn an_nfd_root_is_located_in_process_with_gits_bytes() {
    root_is_located_in_process_with_gits_bytes("cafe\u{301}");
}

#[test]
fn a_cjk_root_is_located_in_process_with_gits_bytes() {
    root_is_located_in_process_with_gits_bytes("\u{65e5}\u{672c}");
}

#[test]
fn a_linked_worktree_of_a_utf8_repository_is_located_in_process() {
    let scratch = Scratch::new("locate");
    let main = committed(&scratch.join("\u{65e5}\u{672c}"), EMPTY_MODULE);
    let linked = scratch.join("linked");
    git(&main, &["worktree", "add", "--quiet", "--detach", &linked]);

    assert_eq!(INHERITED.expect(&linked, 0), Location::Repository(linked));
}

#[test]
fn a_utf8_separate_git_directory_is_followed_in_process() {
    let scratch = Scratch::new("locate");
    let work = scratch.join("work");
    let git_dir = scratch.join("\u{65e5}\u{672c}-git");
    git(
        scratch.path(),
        &["init", "-q", "--separate-git-dir", &git_dir, &work],
    );

    fs::write(join(&work, "a.ts"), "export {};\n").unwrap();
    git(&work, &["add", "a.ts"]);
    authored_git(&work, &["commit", "-qm", "init"]);

    assert_eq!(INHERITED.expect(&work, 0), Location::Repository(work));
}

#[test]
fn git_ceiling_directories_stops_discovery_where_git_stops() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let start = directory(&join(&root, "a/b"));
    let environment = INHERITED.with("GIT_CEILING_DIRECTORIES", &join(&root, "a"));

    assert_eq!(environment.expect_outside(&start, 1), Location::Outside);
}

#[test]
fn git_work_tree_makes_gits_work_tree_the_root() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let worktree = join(&root, "sub");
    let start = directory(&join(&worktree, "deeper"));
    let environment = INHERITED.with("GIT_WORK_TREE", &worktree);

    assert_eq!(
        environment.expect(&start, 1),
        Location::Repository(worktree)
    );
}

#[test]
fn git_test_assume_different_owner_fails_with_gits_ownership_error() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let home = directory(&scratch.join("home"));
    let environment = Environment::isolated(&home).with("GIT_TEST_ASSUME_DIFFERENT_OWNER", "1");

    let probe = environment.rev_parse(&root);
    if probe.status.success()
        || !String::from_utf8_lossy(&probe.stderr).contains("dubious ownership")
    {
        return;
    }

    assert!(is_failed(&environment.expect(&root, 1)));
}

#[test]
fn git_discovery_across_filesystem_without_a_mount_needs_no_discovery_spawn() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let nested = directory(&join(&root, "nested"));
    let environment = INHERITED.with("GIT_DISCOVERY_ACROSS_FILESYSTEM", "1");

    assert_eq!(environment.expect(&nested, 0), Location::Repository(root));
}

fn damaged_inner_repository_defers_to_the_outer_like_git(damage: fn(&str)) {
    let scratch = Scratch::new("locate");
    let outer = initialized(&scratch.join("repo"));
    let inner = initialized(&join(&outer, "inner"));
    damage(&join(&inner, ".git"));
    let start = directory(&join(&inner, "sub"));

    assert_eq!(INHERITED.expect(&start, 1), Location::Repository(outer));
}

#[test]
fn inner_git_directory_without_objects_defers_to_the_outer_repository_like_git() {
    damaged_inner_repository_defers_to_the_outer_like_git(|marker| {
        fs::remove_dir_all(join(marker, "objects")).unwrap();
    });
}

#[test]
fn inner_git_directory_without_refs_defers_to_the_outer_repository_like_git() {
    damaged_inner_repository_defers_to_the_outer_like_git(|marker| {
        fs::remove_dir_all(join(marker, "refs")).unwrap();
    });
}

#[test]
fn inner_git_directory_with_an_invalid_head_defers_to_the_outer_repository_like_git() {
    damaged_inner_repository_defers_to_the_outer_like_git(|marker| {
        fs::write(join(marker, "HEAD"), "garbage\n").unwrap();
    });
}

#[test]
fn environment_config_falls_back_to_git() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let worktree = directory(&scratch.join("elsewhere"));
    let environment = INHERITED
        .with("GIT_CONFIG_COUNT", "1")
        .with("GIT_CONFIG_KEY_0", "core.worktree")
        .with("GIT_CONFIG_VALUE_0", &worktree);

    environment.expect(&root, 1);
}

#[test]
fn malformed_config_falls_back_to_gits_exact_error() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    append(&config(&root), "not a config line\n");

    assert!(is_failed(&INHERITED.expect(&root, 1)));
}

#[test]
fn invalid_core_boolean_falls_back_to_gits_exact_error() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    git(&root, &["config", "core.filemode", "banana"]);

    assert!(is_failed(&INHERITED.expect(&root, 1)));
}

#[test]
fn unsupported_repository_format_falls_back_to_gits_exact_error() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    git(&root, &["config", "core.repositoryformatversion", "2"]);

    assert!(is_failed(&INHERITED.expect(&root, 1)));
}

#[test]
fn invalid_global_config_falls_back_to_gits_exact_error() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    let home = directory(&scratch.join("home"));
    fs::write(join(&home, ".gitconfig"), "not a config line\n").unwrap();

    assert!(is_failed(&INHERITED.with("HOME", &home).expect(&root, 1)));
}

#[test]
fn fallback_location_is_memoized_for_the_exact_directory_string() {
    let scratch = Scratch::new("locate");
    let root = initialized(&scratch.join("repo"));
    append(&config(&root), "not a config line\n");
    let expected = INHERITED.git_location(&root, Failure::Failed);

    let traced = INHERITED.traced(&[&root, &root]);
    assert_eq!(traced.locations, [expected.clone(), expected]);
    assert_eq!(traced.spawned("--show-toplevel"), 1, "{:?}", traced.calls);
}

#[test]
fn a_directory_in_no_repository_is_outside_without_a_git_process() {
    let scratch = Scratch::new("locate");
    let cwd = directory(&scratch.join("cwd"));
    let home = directory(&scratch.join("home"));
    fs::write(join(&cwd, "x.ts"), VALUE).unwrap();

    assert_eq!(
        Environment::isolated(&home).expect_outside(&cwd, 0),
        Location::Outside
    );
}
