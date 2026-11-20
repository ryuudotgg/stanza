use std::collections::BTreeMap;
use std::fs;
use std::process::Command;

use super::*;
use crate::files::selection_tests::{
    Tree, byte_sorted, cold, fixture_git, ignored_target, moved_directory, tracked_symlink,
};

#[derive(Debug, Default, PartialEq, Eq)]
struct Collected {
    files: Vec<String>,
    errors: Vec<String>,
    warnings: Vec<String>,
    submodules: Vec<String>,
    changed_lines: BTreeMap<String, Changed>,
}

impl Collected {
    fn only(files: Vec<String>) -> Collected {
        Collected {
            files,
            ..Collected::default()
        }
    }

    fn narrowed(&self, written: &IndexSet<String>) -> Collected {
        Collected {
            files: self
                .files
                .iter()
                .filter(|path| written.contains(*path))
                .cloned()
                .collect(),
            errors: self.errors.clone(),
            warnings: self.warnings.clone(),
            submodules: self.submodules.clone(),
            changed_lines: self
                .changed_lines
                .iter()
                .filter(|(path, _)| written.contains(*path))
                .map(|(path, changed)| (path.clone(), changed.clone()))
                .collect(),
        }
    }
}

fn changed(cwd: &str, hunks: bool, written: Option<&IndexSet<String>>) -> Collected {
    let PathSelection {
        files,
        errors,
        warnings,
        submodules,
        changed_lines,
        ..
    } = cold(|| {
        let mut repositories = Repositories::new(Some(cwd));
        let location = repositories.locate(cwd);
        collect_changed(
            location,
            &mut repositories,
            written,
            Caller::Cli(cwd),
            hunks,
        )
        .unwrap()
    });

    Collected {
        files,
        errors,
        warnings,
        submodules,
        changed_lines: changed_lines.into_iter().collect(),
    }
}

fn lines(lines: &[usize], deleted_after: &[usize]) -> Changed {
    Changed {
        lines: lines.iter().copied().collect(),
        deleted_after: deleted_after.iter().copied().collect(),
    }
}

fn written(paths: impl IntoIterator<Item = String>) -> IndexSet<String> {
    paths.into_iter().collect()
}

const CHILD_TREE: &str = "STANZA_TEST_CHILD_TREE";

// collect_changed's git children read GIT_DIFF_OPTS and GIT_INDEX_FILE only from the process environment, and setting one in this process would reach every test running beside it.
fn in_child(test: &str, variable: &str, value: impl FnOnce(&Tree) -> String) -> Option<Tree> {
    if let Ok(root) = std::env::var(CHILD_TREE) {
        return Some(Tree { root });
    }

    let tree = Tree::repository();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("files::changed::changed_tests::{test}"),
            "--nocapture",
        ])
        .env(CHILD_TREE, &tree.root)
        .env(variable, value(&tree))
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "{stderr}"
    );

    None
}

#[test]
fn collects_modified_and_untracked_files() {
    let tree = Tree::repository();
    tree.write("tracked.ts");
    tree.git(&["add", "tracked.ts"]);
    tree.commit();

    tree.write_text("tracked.ts", "export const changed = true;\n");
    tree.write("untracked.ts");

    assert_eq!(
        changed(&tree.root, false, None),
        Collected::only(vec![tree.at("tracked.ts"), tree.at("untracked.ts")])
    );
}

#[test]
fn collects_staged_files_before_the_first_commit() {
    let tree = Tree::repository();
    tree.write("staged.ts");
    tree.git(&["add", "staged.ts"]);

    assert_eq!(
        changed(&tree.root, false, None),
        Collected::only(vec![tree.at("staged.ts")])
    );
}

#[test]
fn hunks_keep_their_line_numbers_despite_git_config_environment_and_binary_attributes() {
    let Some(tree) = in_child(
        "hunks_keep_their_line_numbers_despite_git_config_environment_and_binary_attributes",
        "GIT_DIFF_OPTS",
        |_| "--unified=10".to_owned(),
    ) else {
        return;
    };

    let source: String = std::iter::once("\n".to_owned())
        .chain((0..20).map(|index| format!("const n{index} = {index};\n")))
        .collect();

    tree.write_text("a.ts", &source);
    tree.write_text("binary.ts", &source);
    tree.write_text(".gitattributes", "binary.ts binary\n");
    tree.git(&["add", "--", "a.ts", "binary.ts", ".gitattributes"]);
    tree.commit();
    tree.git(&["config", "diff.interHunkContext", "10"]);
    tree.git(&["config", "diff.suppressBlankEmpty", "true"]);

    let edited = source
        .replace("n2 = 2", "n2 = 30")
        .replace("n8 = 8", "n8 = 90");

    for name in ["a.ts", "binary.ts", "untracked.ts"] {
        tree.write_text(name, &edited);
    }

    assert_eq!(
        changed(&tree.root, true, None),
        Collected {
            files: vec![
                tree.at("a.ts"),
                tree.at("binary.ts"),
                tree.at("untracked.ts")
            ],
            changed_lines: BTreeMap::from([
                (tree.at("a.ts"), lines(&[4, 10], &[])),
                (tree.at("binary.ts"), lines(&[4, 10], &[])),
            ]),
            ..Collected::default()
        }
    );
}

#[test]
fn hunks_map_quoted_paths_and_record_deletions_between_lines() {
    let tree = Tree::repository();
    tree.write_text("a \"quoted\".ts", "a();\nb();\nc();\n");
    tree.write_text("z.ts", "x();\n");
    tree.git(&["add", "--", "a \"quoted\".ts", "z.ts"]);
    tree.commit();

    tree.write_text("a \"quoted\".ts", "a();\nc();\n");
    tree.write_text("z.ts", "y();\n");

    let quoted = tree.at("a \"quoted\".ts");
    assert_eq!(
        changed(&tree.root, true, None),
        Collected {
            files: vec![quoted.clone(), tree.at("z.ts")],
            changed_lines: BTreeMap::from([
                (quoted, lines(&[], &[1])),
                (tree.at("z.ts"), lines(&[1], &[])),
            ]),
            ..Collected::default()
        }
    );
}

#[test]
fn hunks_merge_typechange_sections_before_matching_file_names() {
    let tree = Tree::repository();
    tree.write_text("z.ts", "z();\n");
    tree.git(&["add", "--", "z.ts"]);
    tree.link("z.ts", "a.ts");
    tree.git(&["add", "a.ts"]);
    tree.commit();

    fs::remove_file(tree.at("a.ts")).unwrap();
    tree.write_text("a.ts", "a();\nb();\n");
    tree.write_text("z.ts", "y();\n");

    assert_eq!(
        changed(&tree.root, true, None),
        Collected {
            files: vec![tree.at("a.ts"), tree.at("z.ts")],
            changed_lines: BTreeMap::from([
                (tree.at("a.ts"), lines(&[1, 2], &[])),
                (tree.at("z.ts"), lines(&[1], &[])),
            ]),
            ..Collected::default()
        }
    );
}

#[test]
fn a_file_named_head_does_not_widen_the_changed_set() {
    let tree = Tree::repository();
    tree.write("tracked.ts");
    tree.git(&["add", "tracked.ts"]);
    tree.commit();

    tree.write_text("HEAD", "x\n");
    assert_eq!(changed(&tree.root, false, None), Collected::default());
}

#[test]
fn a_set_stanza_attribute_opts_files_below_skipped_folders_in() {
    let tree = Tree::repository();
    tree.write_text(
        ".gitattributes",
        "dist/** stanza\nnode_modules/** stanza=true\nbuild/** stanza=yes\ndist/ling.ts linguist-generated\n",
    );

    for name in [
        "dist/a.ts",
        "node_modules/x/b.ts",
        "build/c.ts",
        "dist/a.d.ts",
        "dist/a.gen.ts",
        "dist/ling.ts",
    ] {
        tree.write(name);
    }

    assert_eq!(
        changed(&tree.root, false, None),
        Collected::only(vec![tree.at("dist/a.ts"), tree.at("node_modules/x/b.ts")])
    );
}

#[test]
fn changed_skips_a_tracked_symlink_so_fixes_never_write_outside_the_repo() {
    let (tree, _outside) = tracked_symlink();
    assert_eq!(
        changed(&tree.root, false, None),
        Collected::only(vec![tree.at("inside.ts")])
    );
}

#[test]
fn changed_skips_a_tracked_directory_replaced_by_a_symlink_out_of_the_repo() {
    let (tree, _elsewhere) = moved_directory();
    assert_eq!(
        changed(&tree.root, false, None),
        Collected::only(vec![tree.at("keep.ts")])
    );
}

#[test]
fn changed_skips_a_tracked_directory_replaced_by_a_symlink_to_an_ignored_folder() {
    let tree = ignored_target();
    assert_eq!(
        changed(&tree.root, false, None),
        Collected::only(vec![tree.at("keep.ts")])
    );
}

#[test]
fn a_replaced_directory_linking_to_an_unsearchable_ignored_folder_is_skipped_without_an_error() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let tree = ignored_target();
    tree.mode("hidden", 0o000);
    let selected = changed(&tree.root, false, None);
    tree.mode("hidden", 0o755);

    assert_eq!(selected, Collected::only(vec![tree.at("keep.ts")]));
}

#[test]
fn a_broken_branch_ref_is_an_error_an_orphan_branch_is_not() {
    let tree = Tree::repository();
    tree.write("tracked.ts");
    tree.git(&["add", "tracked.ts"]);
    tree.commit();

    tree.git(&["checkout", "-q", "--orphan", "fresh"]);
    assert_eq!(
        changed(&tree.root, false, None),
        Collected::only(vec![tree.at("tracked.ts")])
    );

    fs::write(tree.at(".git/refs/heads/fresh"), "junk\n").unwrap();
    let broken = changed(&tree.root, false, None);
    assert!(
        matches!(&broken.errors[..], [error] if error.contains("git symbolic-ref failed")),
        "{broken:?}"
    );

    assert_eq!(
        broken,
        Collected {
            errors: broken.errors.clone(),
            ..Collected::default()
        }
    );
}

#[test]
fn changed_with_a_broken_git_index_is_an_error_not_an_empty_selection() {
    let tree = Tree::repository();
    tree.write("tracked.ts");
    tree.git(&["add", "tracked.ts"]);
    fs::write(tree.at(".git/index"), "junkjunkjunkjunkjunk").unwrap();

    let broken = changed(&tree.root, false, None);
    assert!(!broken.errors.is_empty(), "{broken:?}");
    assert!(
        broken.errors.iter().all(|error| error.contains("git")),
        "{broken:?}"
    );

    assert_eq!(
        broken,
        Collected {
            errors: broken.errors.clone(),
            ..Collected::default()
        }
    );
}

#[test]
fn a_written_set_narrows_changed_files_to_it() {
    let names = [
        "plain.ts",
        "star*.ts",
        "question?.ts",
        "bracket[1].ts",
        ":(glob)*.ts",
    ];

    for (born, hunks) in [(false, false), (false, true), (true, false), (true, true)] {
        let tree = Tree::repository();
        for name in names {
            tree.write(name);
        }

        tree.git(&["add", "-A"]);
        if born {
            tree.commit();
        }

        for name in names {
            tree.write_text(name, "export const value = 2;\n");
            tree.write(&format!("untracked-{name}"));
        }

        tree.write("directory/child.ts");

        let whole = changed(&tree.root, hunks, None);
        assert_eq!(
            whole.errors,
            Vec::<String>::new(),
            "born={born}, hunks={hunks}"
        );

        let selections = names
            .iter()
            .map(|name| written([tree.at(name), tree.at(&format!("untracked-{name}"))]))
            .chain([
                written(names.map(|name| tree.at(name))),
                written([tree.at("directory")]),
                written([tree.at("missing.ts")]),
                written([tree.at("../outside.ts")]),
                written([]),
            ]);

        for selected in selections {
            assert_eq!(
                changed(&tree.root, hunks, Some(&selected)),
                whole.narrowed(&selected),
                "born={born}, hunks={hunks}, written={selected:?}"
            );
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Index {
    Default,
    Version4,
    Environment,
}

fn unindexed_and_new_files_select_as_the_whole_run_does(tree: &Tree, index: Index, hunks: bool) {
    let names = ["kept.ts", "removed.ts"];
    for name in names {
        tree.write(name);
    }

    tree.git(&["add", "-A"]);
    tree.commit();

    for name in names {
        tree.write_text(name, "export const value = 2;\n");
    }

    tree.write("new.ts");

    let mut remove = fixture_git(&tree.root);
    if index == Index::Environment {
        let other = std::env::var("GIT_INDEX_FILE").unwrap();
        fs::copy(tree.at(".git/index"), &other).unwrap();
        remove.env("GIT_INDEX_FILE", other);
    }

    let removed = remove
        .args(["rm", "-q", "--cached", "removed.ts"])
        .output()
        .unwrap();

    assert!(removed.status.success(), "{index:?}");
    if index == Index::Version4 {
        tree.git(&["update-index", "--index-version", "4"]);
    }

    let case = format!("{index:?}, hunks={hunks}");
    let whole = changed(&tree.root, hunks, None);
    assert_eq!(whole.errors, Vec::<String>::new(), "{case}");
    assert_eq!(
        whole.files,
        ["kept.ts", "new.ts", "removed.ts"].map(|name| tree.at(name)),
        "{case}"
    );

    let hunked: Vec<&String> = whole.changed_lines.keys().collect();
    let expected = if hunks {
        vec![tree.at("kept.ts")]
    } else {
        vec![]
    };
    assert_eq!(hunked, expected.iter().collect::<Vec<_>>(), "{case}");

    let selections = [
        &["kept.ts"][..],
        &["removed.ts"],
        &["new.ts"],
        &["kept.ts", "new.ts"],
        &["kept.ts", "removed.ts", "new.ts"],
    ];

    for selected in selections {
        let selected = written(selected.iter().map(|name| tree.at(name)));
        assert_eq!(
            changed(&tree.root, hunks, Some(&selected)),
            whole.narrowed(&selected),
            "{case}, written={selected:?}"
        );
    }
}

#[test]
fn a_written_set_selects_unindexed_and_new_files_as_the_whole_run_does() {
    for (index, hunks) in [
        (Index::Default, false),
        (Index::Default, true),
        (Index::Version4, true),
    ] {
        unindexed_and_new_files_select_as_the_whole_run_does(&Tree::repository(), index, hunks);
    }
}

#[test]
fn a_written_set_selects_unindexed_and_new_files_as_the_whole_run_does_under_git_index_file() {
    if let Some(tree) = in_child(
        "a_written_set_selects_unindexed_and_new_files_as_the_whole_run_does_under_git_index_file",
        "GIT_INDEX_FILE",
        |tree| tree.at(".git/other-index"),
    ) {
        unindexed_and_new_files_select_as_the_whole_run_does(&tree, Index::Environment, true);
    }
}

fn long_name(index: usize) -> String {
    format!("{}-{index}.ts", "long".repeat(50))
}

#[test]
fn a_written_set_past_the_pathspec_budget_still_narrows_changed_files() {
    let tree = Tree::repository();
    let selected = written((0..400).map(|index| tree.at(&long_name(index))));
    for path in &selected {
        tree.write(path);
    }

    tree.write("other.ts");

    let whole = changed(&tree.root, false, None);
    assert_eq!(whole.errors, Vec::<String>::new());

    let narrowed = changed(&tree.root, false, Some(&selected));
    assert_eq!(narrowed, whole.narrowed(&selected));
    assert_eq!(
        narrowed,
        Collected::only(byte_sorted(selected.into_iter().collect()))
    );
}

#[test]
fn a_written_set_past_the_pathspec_budget_lists_untracked_files_as_the_whole_run_does() {
    let tree = Tree::repository();
    let names: Vec<String> = std::iter::once("a.ts".to_owned())
        .chain((0..400).map(long_name))
        .collect();

    for name in &names {
        tree.write(name);
    }

    tree.git(&["add", "-A"]);
    tree.commit();
    tree.write("\u{feff}a.ts");

    let whole = changed(&tree.root, false, None);
    let hunked = changed(&tree.root, true, None);
    assert_eq!(whole.errors, Vec::<String>::new());
    assert_eq!(hunked.errors, Vec::<String>::new());

    let selected = written(names.iter().map(|name| tree.at(name)));
    assert_eq!(
        changed(&tree.root, true, Some(&selected)),
        hunked.narrowed(&selected)
    );

    assert_eq!(
        changed(&tree.root, false, Some(&selected)),
        Collected::only(byte_sorted(selected.into_iter().collect()))
    );
}
