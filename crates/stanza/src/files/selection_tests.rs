use std::collections::HashSet;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

static SCRATCH: AtomicUsize = AtomicUsize::new(0);

pub(super) struct Tree {
    pub(super) root: String,
}

impl Tree {
    pub(super) fn new() -> Tree {
        loop {
            let base = std::env::temp_dir().join(format!(
                "stanza-selection-{}-{}",
                std::process::id(),
                SCRATCH.fetch_add(1, Ordering::Relaxed)
            ));

            match fs::DirBuilder::new().mode(0o700).create(&base) {
                Ok(()) => {
                    return Tree {
                        root: realpath(&base.to_string_lossy()).unwrap(),
                    };
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create {}: {error}", base.display()),
            }
        }
    }

    pub(super) fn repository() -> Tree {
        let tree = Tree::new();
        tree.git(&["init", "-q"]);
        tree
    }

    pub(super) fn at(&self, path: &str) -> String {
        join(&self.root, path)
    }

    pub(super) fn write(&self, path: &str) {
        self.write_text(path, "export {};\n");
    }

    pub(super) fn write_text(&self, path: &str, text: &str) {
        let path = self.at(path);
        fs::create_dir_all(dirname(&path)).unwrap();
        fs::write(path, text).unwrap();
    }

    pub(super) fn link(&self, target: &str, path: &str) {
        symlink(target, self.at(path)).unwrap();
    }

    pub(super) fn mode(&self, path: &str, mode: u32) {
        fs::set_permissions(self.at(path), fs::Permissions::from_mode(mode)).unwrap();
    }

    pub(super) fn git(&self, args: &[&str]) {
        git(&self.root, args);
    }

    pub(super) fn commit(&self) {
        self.git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "initial",
        ]);
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(super) fn fixture_git(dir: &str) -> Command {
    let mut command = Command::new("git");
    command.current_dir(dir);
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
    ] {
        command.env_remove(variable);
    }

    command
}

fn git(dir: &str, args: &[&str]) {
    let output = fixture_git(dir).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Selected {
    files: Vec<String>,
    errors: Vec<String>,
    submodules: Vec<String>,
    opted_out: bool,
}

impl Selected {
    fn only(files: Vec<String>) -> Selected {
        Selected {
            files,
            ..Selected::default()
        }
    }
}

fn select(cwd: &str, paths: &[&str]) -> Selected {
    selection(cwd, paths, false)
}

fn select_generated(cwd: &str, paths: &[&str]) -> Selected {
    selection(cwd, paths, true)
}

// crate::directories caches listings per thread, and the CLI starts every run with that cache empty.
pub(super) fn cold<T: Send>(call: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|scope| scope.spawn(call).join().unwrap())
}

fn selection(cwd: &str, paths: &[&str], keep_generated: bool) -> Selected {
    let paths: Vec<String> = paths.iter().map(|path| (*path).to_owned()).collect();
    let PathSelection {
        files,
        errors,
        submodules,
        opted_out,
        ..
    } = cold(|| collect_files(&paths, cwd, keep_generated, &mut Repositories::new(None)));

    Selected {
        files,
        errors,
        submodules,
        opted_out,
    }
}

fn stdin(cwd: &str, input: &str) -> StdinTarget {
    cold(|| stdin_target(input, cwd, &mut Repositories::new(None)))
}

const EXCLUDED: StdinTarget = StdinTarget::Skip(SkipReason::Excluded);
const OPTED_OUT: StdinTarget = StdinTarget::Skip(SkipReason::OptedOut);

pub(super) fn byte_sorted(mut files: Vec<String>) -> Vec<String> {
    files.sort();
    files
}

#[test]
fn accepts_supported_extensions_and_skips_generated_paths() {
    let tree = Tree::new();
    let accepted = [
        "source/file.ts",
        "source/file.tsx",
        "source/file.mts",
        "source/file.cts",
        "source/file.js",
        "source/file.jsx",
        "source/file.mjs",
        "source/file.cjs",
    ];

    let rejected = [
        "source/file.d.ts",
        "source/file.gen.ts",
        "source/file.generated.ts",
        "source/file.min.js",
        "node_modules/file.ts",
        "source/file.txt",
    ];

    for path in accepted.iter().chain(&rejected) {
        tree.write(path);
    }

    let files = byte_sorted(accepted.iter().map(|path| tree.at(path)).collect());
    assert_eq!(select(&tree.root, &["."]), Selected::only(files));
}

#[test]
fn walks_non_git_directories_with_the_root_ignore_file() {
    let tree = Tree::new();
    tree.write_text(".gitignore", "/build\n*.log\ntmp/\n**/generated/**\n");
    tree.write("source/keep.ts");

    tree.write("build/skip.ts");
    tree.write("source/debug.log");
    tree.write("tmp/skip.ts");
    tree.write("source/generated/skip.ts");
    tree.write("source/keep.txt");

    assert_eq!(
        select(&tree.root, &[&tree.root]),
        Selected::only(vec![tree.at("source/keep.ts")])
    );
}

#[test]
fn keeping_generated_files_selects_generated_names_attributes_and_explicit_paths() {
    let repository = Tree::repository();
    let outside = Tree::new();
    repository.write_text(".gitattributes", "marked.ts linguist-generated\n");

    for tree in [&repository, &outside] {
        for name in [
            "schema.gen.ts",
            "api.generated.ts",
            "bundle.min.js",
            "types.d.ts",
        ] {
            tree.write(name);
        }
    }

    repository.write("marked.ts");

    let generated = byte_sorted(vec![
        outside.at("api.generated.ts"),
        outside.at("bundle.min.js"),
        outside.at("schema.gen.ts"),
        repository.at("api.generated.ts"),
        repository.at("bundle.min.js"),
        repository.at("marked.ts"),
        repository.at("schema.gen.ts"),
    ]);

    let both = [repository.root.as_str(), outside.root.as_str()];
    let schema = repository.at("schema.gen.ts");
    assert_eq!(select(&repository.root, &both), Selected::default());
    assert_eq!(
        select_generated(&repository.root, &both),
        Selected::only(generated)
    );

    assert_eq!(
        select_generated(&repository.root, &[&schema]),
        Selected::only(vec![schema.clone()])
    );
}

#[test]
fn generated_names_cover_every_registered_extension() {
    let tree = Tree::new();
    let names = [
        "bundle.min.mjs",
        "bundle.min.cjs",
        "routes.gen.ts",
        "routes.gen.tsx",
        "routes.gen.js",
        "routes.gen.jsx",
        "routes.gen.mjs",
        "routes.gen.cjs",
        "api.gen.mts",
        "api.gen.cts",
        "regen.ts",
        "admin.mjs",
        "min.js",
        "gen.cjs",
        "bundle.min.ts",
    ];

    for name in names {
        tree.write(name);
    }

    let selected = byte_sorted(
        names[names.len() - 5..]
            .iter()
            .map(|name| tree.at(name))
            .collect(),
    );
    let everything = byte_sorted(names.iter().map(|name| tree.at(name)).collect());

    assert_eq!(select(&tree.root, &[&tree.root]), Selected::only(selected));
    assert_eq!(
        select_generated(&tree.root, &[&tree.root]),
        Selected::only(everything)
    );
}

#[test]
fn uses_git_to_include_tracked_and_untracked_files_but_not_ignored_files() {
    let tree = Tree::repository();
    tree.write_text(".gitignore", "ignored/\n");
    tree.write("tracked.ts");
    tree.git(&["add", ".gitignore", "tracked.ts"]);
    tree.commit();

    tree.write("untracked.ts");
    tree.write("ignored/skip.ts");

    assert_eq!(
        select(&tree.root, &[&tree.root]),
        Selected::only(vec![tree.at("tracked.ts"), tree.at("untracked.ts")])
    );
}

#[test]
fn skipped_directory_names_apply_below_the_argument_not_above_it() {
    let tree = Tree::new();
    let cwd = tree.at("build/project");
    tree.write("build/project/keep.ts");
    tree.write("build/project/dist/skip.ts");

    let base = Tree::new();
    let file = base.at("x/build/project/keep.ts");
    base.write("x/build/project/keep.ts");

    let keep = join(&cwd, "keep.ts");
    let dist = join(&cwd, "dist");
    assert_eq!(select(&cwd, &[&cwd]), Selected::only(vec![keep.clone()]));
    assert_eq!(select(&cwd, &[&keep]), Selected::only(vec![keep.clone()]));
    assert_eq!(
        select(&cwd, &[&dist]),
        Selected::only(vec![join(&dist, "skip.ts")])
    );

    assert_eq!(
        select(&base.root, &["x/build/project/keep.ts"]),
        Selected::only(vec![file.clone()])
    );

    assert_eq!(
        stdin(&base.root, "x/build/project/keep.ts"),
        StdinTarget::Format {
            path: file,
            root: None,
        }
    );
}

#[test]
fn explicit_repository_files_below_skipped_directories_are_skipped() {
    let tree = Tree::repository();
    let node_module = tree.at("node_modules/x/a.ts");
    let distribution = tree.at("dist/a.ts");
    tree.write("node_modules/x/a.ts");
    tree.write("dist/a.ts");

    assert_eq!(
        select(&tree.root, &[&node_module, &distribution]),
        Selected::default()
    );

    assert_eq!(stdin(&tree.root, &node_module), EXCLUDED);
    assert_eq!(stdin(&tree.root, &distribution), EXCLUDED);
}

#[test]
fn a_folder_argument_named_through_a_link_with_a_skipped_name_stays_skipped() {
    let tree = Tree::repository();
    tree.write("src/a.ts");
    tree.link(&tree.at("src"), "dist");

    assert_eq!(select(&tree.root, &[&tree.at("dist")]), Selected::default());
    assert_eq!(
        select(&tree.root, &[&tree.at("src")]),
        Selected::only(vec![tree.at("src/a.ts")])
    );
}

#[test]
fn a_set_stanza_attribute_opts_files_below_skipped_folders_in() {
    let tree = Tree::repository();
    tree.write_text(
        ".gitattributes",
        "dist/** stanza\nnode_modules/** stanza=true\nbuild/** stanza=yes\ndist/ling.ts linguist-generated\n",
    );

    let names = [
        "dist/a.ts",
        "node_modules/x/b.ts",
        "build/c.ts",
        "dist/a.d.ts",
        "dist/a.gen.ts",
        "dist/ling.ts",
    ];

    for name in names {
        tree.write(name);
    }

    let taken = vec![tree.at("dist/a.ts"), tree.at("node_modules/x/b.ts")];
    assert_eq!(select(&tree.root, &["."]), Selected::only(taken.clone()));
    assert_eq!(select(&tree.root, &names), Selected::only(taken.clone()));
    assert_eq!(
        stdin(&tree.root, "dist/a.ts"),
        StdinTarget::Format {
            path: taken[0].clone(),
            root: Some(tree.root.clone()),
        }
    );

    for name in &names[2..] {
        assert_eq!(stdin(&tree.root, name), EXCLUDED, "{name}");
    }
}

#[test]
fn an_unset_stanza_attribute_below_a_skipped_folder_still_opts_out() {
    let tree = Tree::repository();
    tree.write_text(".gitattributes", "dist/** -stanza\n");
    tree.write("dist/a.ts");

    assert_eq!(
        select(&tree.root, &["."]),
        Selected {
            opted_out: true,
            ..Selected::default()
        }
    );

    assert_eq!(stdin(&tree.root, "dist/a.ts"), OPTED_OUT);
}

#[test]
fn an_explicit_path_through_a_skipped_name_or_alias_stays_skipped() {
    let tree = Tree::repository();
    let target = tree.at("src/a.ts");
    tree.write("src/a.ts");
    tree.link(&tree.at("src"), "dist");
    tree.link(&target, "alias.generated.ts");

    let inputs = [tree.at("dist/a.ts"), tree.at("alias.generated.ts")];
    let typed: Vec<&str> = inputs.iter().map(String::as_str).collect();
    assert_eq!(select(&tree.root, &typed), Selected::default());

    for input in &typed {
        assert_eq!(stdin(&tree.root, input), EXCLUDED, "{input}");
    }
}

#[test]
fn a_repository_below_a_skipped_directory_selects_explicit_files() {
    let tree = Tree::new();
    let cwd = tree.at("build/project");
    let file = tree.at("build/project/keep.ts");
    tree.write("build/project/keep.ts");
    git(&cwd, &["init", "-q"]);

    for path in [&file, &cwd] {
        assert_eq!(
            select(&cwd, &[path]),
            Selected::only(vec![file.clone()]),
            "{path}"
        );
    }

    assert_eq!(
        stdin(&cwd, &file),
        StdinTarget::Format {
            path: file.clone(),
            root: Some(cwd.clone()),
        }
    );
}

pub(super) fn tracked_symlink() -> (Tree, Tree) {
    let tree = Tree::repository();
    let outside = Tree::new();
    outside.write("outside.ts");
    tree.write("inside.ts");
    tree.link(&outside.at("outside.ts"), "link.ts");
    tree.git(&["add", "inside.ts", "link.ts"]);
    (tree, outside)
}

#[test]
fn a_tracked_symlink_is_skipped_so_fixes_never_write_outside_the_repo() {
    let (tree, _outside) = tracked_symlink();
    assert_eq!(
        select(&tree.root, &[&tree.root]),
        Selected::only(vec![tree.at("inside.ts")])
    );
}

pub(super) fn moved_directory() -> (Tree, Tree) {
    let tree = Tree::repository();
    let elsewhere = Tree::new();
    tree.write("keep.ts");
    tree.write("pkg/moved.ts");
    tree.git(&["add", "keep.ts", "pkg/moved.ts"]);
    tree.commit();

    fs::remove_dir_all(tree.at("pkg")).unwrap();
    elsewhere.write("moved.ts");
    tree.link(&elsewhere.root, "pkg");
    tree.write_text("keep.ts", "export const changed = true;\n");
    (tree, elsewhere)
}

#[test]
fn a_tracked_directory_replaced_by_a_symlink_out_of_the_repo_is_skipped() {
    let (tree, _elsewhere) = moved_directory();
    assert_eq!(
        select(&tree.root, &[&tree.root]),
        Selected::only(vec![tree.at("keep.ts")])
    );
}

pub(super) fn ignored_target() -> Tree {
    let tree = Tree::repository();
    tree.write_text(".gitignore", "hidden/\n");
    tree.write("keep.ts");
    tree.write("src/a.ts");
    tree.git(&["add", ".gitignore", "keep.ts", "src/a.ts"]);
    tree.commit();

    fs::remove_dir_all(tree.at("src")).unwrap();
    tree.write_text("hidden/a.ts", "export const hidden = true;\n");
    tree.link("hidden", "src");
    tree.write_text("keep.ts", "export const changed = true;\n");
    tree
}

#[test]
fn a_tracked_directory_replaced_by_a_symlink_to_an_ignored_folder_is_skipped() {
    let tree = ignored_target();
    assert_eq!(
        select(&tree.root, &[&tree.root]),
        Selected::only(vec![tree.at("keep.ts")])
    );
}

#[test]
fn a_replaced_directory_linking_to_an_unsearchable_ignored_folder_is_skipped_without_an_error() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let tree = ignored_target();
    tree.mode("hidden", 0o000);
    let selected = select(&tree.root, &[&tree.root]);
    tree.mode("hidden", 0o755);

    assert_eq!(selected, Selected::only(vec![tree.at("keep.ts")]));
}

#[test]
fn naming_a_symlinked_folder_selects_the_files_in_its_target() {
    let tree = Tree::repository();
    tree.write("real/a.ts");
    tree.write("real/sub/b.ts");
    tree.link("real", "alias");

    assert_eq!(
        select(&tree.root, &[&tree.at("alias")]),
        Selected::only(vec![tree.at("real/a.ts"), tree.at("real/sub/b.ts")])
    );
}

#[test]
fn a_broken_git_index_is_an_error_not_an_empty_selection() {
    let tree = Tree::repository();
    tree.write("tracked.ts");
    tree.git(&["add", "tracked.ts"]);
    fs::write(tree.at(".git/index"), "junkjunkjunkjunkjunk").unwrap();

    assert_eq!(
        select(&tree.root, &["."]),
        Selected {
            errors: vec![format!(
                "git ls-files failed in {}: fatal: .git/index: index file smaller than expected",
                tree.root
            )],
            ..Selected::default()
        }
    );
}

#[test]
fn explicit_files_are_collected_across_repositories_and_outside_them() {
    let first = Tree::repository();
    let second = Tree::repository();
    let outside = Tree::new();

    first.write_text(".gitattributes", "schema.ts linguist-generated\n");
    first.write("keep.ts");
    first.write("schema.ts");
    second.write("nested/keep.ts");
    outside.write("keep.ts");

    let inputs = [
        first.at("keep.ts"),
        first.at("schema.ts"),
        second.at("nested/keep.ts"),
        outside.at("keep.ts"),
    ];

    let typed: Vec<&str> = inputs.iter().map(String::as_str).collect();
    let expected = byte_sorted(vec![
        first.at("keep.ts"),
        second.at("nested/keep.ts"),
        outside.at("keep.ts"),
    ]);

    assert_eq!(select(&outside.root, &typed), Selected::only(expected));
}

const IGNORE_TABLE: [(&str, &[&str]); 29] = [
    ("?.ts", &["a.ts", "ab.ts"]),
    ("[abc].ts", &["a.ts", "d.ts"]),
    ("[!a].ts", &["a.ts", "b.ts"]),
    ("[^a].ts", &["a.ts", "b.ts"]),
    ("[a-c]x.ts", &["ax.ts", "dx.ts"]),
    ("[a\\-z].ts", &["a.ts", "b.ts", "-.ts", "z.ts"]),
    ("/v?.ts\n/w??.ts", &["v\u{e9}.ts", "w\u{e9}.ts", "vx.ts"]),
    ("[]a].ts", &["].ts", "a.ts", "b.ts"]),
    (
        "[[:alpha:]].ts\n[[:digit:]]x.ts",
        &["a.ts", "5x.ts", "_.ts"],
    ),
    ("[[:space:]].ts", &[" .ts", "a.ts"]),
    ("\\#x.ts", &["#x.ts", "x.ts"]),
    ("\\!bang.ts", &["!bang.ts", "bang.ts"]),
    ("foo\\bar.ts", &["foobar.ts", "foo\\bar.ts"]),
    (
        "sp\\ \ntr ",
        &["sp /f.ts", "sp/f.ts", "tr/f.ts", "tr /f.ts"],
    ),
    (
        "**/deep.ts",
        &["deep.ts", "x/y/deep.ts", "xdeep.ts", "x/ydeep.ts"],
    ),
    (
        "a/**/b.ts",
        &["a/b.ts", "a/x/y/b.ts", "z/a/b.ts", "a/xb.ts"],
    ),
    ("a/***/b.ts", &["a/b.ts", "a/x/y/b.ts", "a/xb.ts"]),
    ("keep/**", &["keep/f.ts", "keep/x/f.ts", "keeps/f.ts"]),
    ("logs.ts/", &["logs.ts/f.ts", "z/logs.ts"]),
    ("/root.ts", &["root.ts", "z/root.ts"]),
    ("*.ts\n!keep.ts", &["skip.ts", "keep.ts", "z/keep.ts"]),
    ("logs/\n!logs/f.ts", &["logs/f.ts", "other.ts"]),
    ("[x.ts", &["[x.ts", "x.ts"]),
    ("bad\\", &["bad/f.ts", "bad\\/f.ts"]),
    ("\u{feff}a.ts", &["a.ts", "b.ts"]),
    ("\u{feff}#a.ts", &["\u{feff}#a.ts", "a.ts"]),
    ("[[:foo:]]x.ts", &["f]x.ts", "x.ts"]),
    ("a/**\\/b.ts", &["a/b.ts", "a/x/y/b.ts"]),
    ("[z-a]x.ts", &["zx.ts", "ax.ts", "mx.ts"]),
];

const NESTED_IGNORES: [(&str, &str); 4] = [
    ("nested/.gitignore", "*.ts\n"),
    ("nested/child/.gitignore", "!keep.ts\nlocal.ts\n"),
    ("bom/.gitignore", "*.ts\n"),
    ("bom/child/.gitignore", "\u{feff}!keep.ts\n"),
];

const NESTED_SOURCES: [&str; 5] = [
    "nested/skip.ts",
    "nested/child/keep.ts",
    "nested/child/local.ts",
    "bom/child/keep.ts",
    "bom/child/skip.ts",
];

fn ignored_by_git(repository: &str, sources: &[&str]) -> HashSet<String> {
    let mut child = fixture_git(repository)
        .args([
            "-c",
            "core.ignorecase=false",
            "-c",
            "core.excludesFile=/dev/null",
            "-c",
            "core.quotepath=false",
            "check-ignore",
            "--no-index",
            "--stdin",
            "-z",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    let input: Vec<u8> = sources
        .iter()
        .flat_map(|source| source.bytes().chain([0]))
        .collect();

    child.stdin.take().unwrap().write_all(&input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(matches!(output.status.code(), Some(0 | 1)), "{output:?}");

    output
        .stdout
        .split(|&byte| byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| String::from_utf8(name.to_vec()).unwrap())
        .collect()
}

#[test]
fn the_fallback_walk_ignores_exactly_what_git_check_ignore_ignores() {
    let mut tree: Vec<(String, String)> = Vec::new();
    for (index, (pattern, paths)) in IGNORE_TABLE.iter().enumerate() {
        tree.push((format!("case{index}/.gitignore"), format!("{pattern}\n")));
        for path in *paths {
            tree.push((format!("case{index}/{path}"), "export {};\n".to_owned()));
        }
    }

    for (path, text) in NESTED_IGNORES {
        tree.push((path.to_owned(), text.to_owned()));
    }

    for path in NESTED_SOURCES {
        tree.push((path.to_owned(), "export {};\n".to_owned()));
    }

    let repository = Tree::repository();
    let outside = Tree::new();
    for root in [&repository, &outside] {
        for (path, text) in &tree {
            root.write_text(path, text);
        }
    }

    let sources: Vec<&str> = tree
        .iter()
        .map(|(path, _)| path.as_str())
        .filter(|path| path.ends_with(".ts"))
        .collect();

    let ignored = ignored_by_git(&repository.root, &sources);
    let kept = sources
        .iter()
        .filter(|path| !ignored.contains(**path))
        .map(|path| outside.at(path))
        .collect();

    assert_eq!(
        select(&outside.root, &[&outside.root]),
        Selected::only(byte_sorted(kept))
    );
}

#[test]
fn an_unreadable_ignore_file_becomes_a_collection_error() {
    let tree = Tree::new();
    fs::create_dir(tree.at(".gitignore")).unwrap();
    tree.write("keep.ts");

    assert_eq!(
        select(&tree.root, &[&tree.root]),
        Selected {
            files: vec![tree.at("keep.ts")],
            errors: vec!["cannot read .gitignore: is a directory".to_owned()],
            ..Selected::default()
        }
    );
}

#[test]
fn selecting_a_nested_directory_outside_git_applies_the_ignore_files_above_it() {
    let tree = Tree::new();
    tree.write_text(".gitignore", "subdir/skip.ts\nhidden/\n");
    tree.write("subdir/keep.ts");
    tree.write("subdir/skip.ts");
    tree.write("hidden/inner/file.ts");

    assert_eq!(
        select(&tree.root, &[&tree.at("subdir")]),
        Selected::only(vec![tree.at("subdir/keep.ts")])
    );

    assert_eq!(
        select(&tree.root, &[&tree.at("hidden/inner")]),
        Selected::default()
    );
}

#[test]
fn the_stanza_attribute_selects_a_file_it_sets_or_leaves_unspecified_and_never_calls_it_generated()
{
    let tree = Tree::repository();
    tree.write_text(
        ".gitattributes",
        "set.ts stanza\nvalue.ts stanza=yes\nunset.ts !stanza\nboth.ts -stanza linguist-generated\n",
    );

    for name in ["set.ts", "value.ts", "unset.ts", "plain.ts", "both.ts"] {
        tree.write(name);
    }

    let files = ["plain.ts", "set.ts", "unset.ts", "value.ts"]
        .iter()
        .map(|name| tree.at(name))
        .collect();

    assert_eq!(
        select(&tree.root, &[&tree.root]),
        Selected {
            files,
            opted_out: true,
            ..Selected::default()
        }
    );

    assert_eq!(stdin(&tree.root, "both.ts"), OPTED_OUT);
}
