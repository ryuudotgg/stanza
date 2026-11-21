use std::fs;
use std::path::Path;

use super::changed_tests::written;
use super::*;
use crate::files::git::git_dirs;
use crate::files::selection_tests::{Tree, cold, fixture_git};

const NAME_DIFF: &str =
    "--literal-pathspecs diff --name-only -z --no-renames --submodule=short HEAD --";
const OTHERS: &str = "--literal-pathspecs ls-files -z --others --exclude-standard --";
const INDEXED: &str = "--literal-pathspecs ls-files -z --cached --";
const INDEXED_OR_OTHERS: &str =
    "--literal-pathspecs ls-files -z --cached --others --exclude-standard --";
const GITMODULES: &str = "config -z --file .gitmodules --list";

fn call(root: &str, command: &str, pathspecs: &[&str], env: GitEnv) -> GitCall {
    GitCall {
        cwd: root.to_owned(),
        args: command
            .split(' ')
            .chain(pathspecs.iter().copied())
            .map(str::to_owned)
            .collect(),
        env,
    }
}

fn listing_tree() -> Tree {
    let tree = Tree::repository();
    tree.git(&["config", "index.version", "2"]);
    tree.git(&["config", "core.splitIndex", "false"]);
    tree.write("a.ts");
    tree.git(&["add", "a.ts"]);
    tree.commit();
    tree.write("b.ts");
    tree
}

fn private_index(repositories: &mut Repositories, root: &str) -> GitEnv {
    let env = repositories.worktree_diff(root);
    let GitEnv::WorktreeDiff(Some(index)) = &env else {
        panic!("{root} has no private index");
    };

    let source = git_dirs(root).unwrap().git_dir;
    assert_ne!(index.path, Path::new(&source).join("index"));
    assert_eq!(
        fs::read(&index.path).unwrap(),
        fs::read(Path::new(&source).join("index")).unwrap()
    );

    env
}

fn requested(
    repositories: &mut Repositories,
    root: &str,
    written: Option<&IndexSet<String>>,
    caller: Caller,
    hunks: bool,
) -> Vec<GitCall> {
    listing_calls(repositories, root, root, written, caller, hunks)
        .requested()
        .cloned()
        .collect()
}

#[test]
fn changed_listing_without_a_written_set_requests_the_diff_and_the_untracked_listing() {
    let tree = listing_tree();
    let root = &tree.root;
    for hunks in [false, true] {
        let mut repositories = Repositories::new(None);
        let calls = requested(&mut repositories, root, None, Caller::Hook, hunks);
        let diff_env = private_index(&mut repositories, root);
        assert_eq!(
            calls,
            [
                call(root, NAME_DIFF, &[], diff_env),
                call(root, OTHERS, &[], GitEnv::CLocale),
            ],
            "hunks={hunks}"
        );
    }
}

#[test]
fn changed_listing_of_a_written_set_requests_the_untracked_listing_only_for_an_untracked_path() {
    let tree = listing_tree();
    let root = &tree.root;
    let rows: [(&[&str], bool); 4] = [
        (&[], false),
        (&["a.ts"], false),
        (&["b.ts"], true),
        (&["a.ts", "b.ts"], true),
    ];

    for (names, untracked) in rows {
        let set = written(names.iter().map(|name| tree.at(name)));
        let mut repositories = Repositories::new(None);
        let hunked = requested(&mut repositories, root, Some(&set), Caller::Hook, true);
        let mut expected = vec![call(
            root,
            NAME_DIFF,
            names,
            private_index(&mut repositories, root),
        )];

        if untracked {
            expected.push(call(root, OTHERS, names, GitEnv::CLocale));
        }

        assert_eq!(hunked, expected, "{names:?}");

        let mut repositories = Repositories::new(None);
        let whole = requested(&mut repositories, root, Some(&set), Caller::Hook, false);
        let command = if untracked {
            INDEXED_OR_OTHERS
        } else {
            INDEXED
        };

        assert_eq!(
            whole,
            [call(root, command, names, GitEnv::CLocale)],
            "{names:?}"
        );

        assert!(repositories.private_indexes.is_empty(), "{names:?}");
    }
}

#[test]
fn changed_listing_drops_its_pathspecs_only_past_the_budget() {
    let tree = listing_tree();
    let root = &tree.root;
    let at_budget = "n".repeat(PATHSPEC_BUDGET);
    let past_budget = "n".repeat(PATHSPEC_BUDGET + 1);
    let rows: [(Vec<&str>, Vec<&str>); 3] = [
        (vec![&at_budget], vec![&at_budget]),
        (vec![&past_budget], vec![]),
        (vec!["a.ts", &at_budget], vec![]),
    ];

    for (names, pathspecs) in rows {
        let set = written(names.iter().map(|name| tree.at(name)));
        let mut repositories = Repositories::new(None);
        let hunked = requested(&mut repositories, root, Some(&set), Caller::Hook, true);
        let diff_env = private_index(&mut repositories, root);
        assert_eq!(
            hunked,
            [
                call(root, NAME_DIFF, &pathspecs, diff_env),
                call(root, OTHERS, &pathspecs, GitEnv::CLocale),
            ]
        );

        let whole = requested(&mut repositories, root, Some(&set), Caller::Hook, false);
        assert_eq!(
            whole,
            [call(root, INDEXED_OR_OTHERS, &pathspecs, GitEnv::CLocale)]
        );
    }
}

#[test]
fn changed_listing_leaves_out_a_written_path_outside_the_root() {
    let tree = listing_tree();
    let root = &tree.root;
    let outside = Tree::new();
    let set = written([
        outside.at("a.ts"),
        tree.at("a.ts"),
        format!("{root}-b/a.ts"),
    ]);
    let mut repositories = Repositories::new(None);
    let calls = requested(&mut repositories, root, Some(&set), Caller::Hook, true);
    let diff_env = private_index(&mut repositories, root);
    assert_eq!(calls, [call(root, NAME_DIFF, &["a.ts"], diff_env)]);

    let only_outside = written([outside.at("a.ts")]);
    assert_eq!(
        requested(
            &mut repositories,
            root,
            Some(&only_outside),
            Caller::Hook,
            false
        ),
        [call(root, INDEXED, &[], GitEnv::CLocale)]
    );
}

#[test]
fn changed_listing_requests_gitmodules_only_for_a_cli_caller_without_a_written_set() {
    let tree = listing_tree();
    let root = &tree.root;
    let set = written([tree.at("a.ts")]);
    let cli = Caller::Cli(root);
    let whole = |repositories: &mut Repositories| {
        vec![
            call(root, NAME_DIFF, &[], private_index(repositories, root)),
            call(root, OTHERS, &[], GitEnv::CLocale),
        ]
    };

    let mut repositories = Repositories::new(None);
    assert_eq!(
        requested(&mut repositories, root, None, cli, false),
        whole(&mut repositories)
    );

    tree.write_text(".gitmodules", "[submodule \"s\"]\n\tpath = s\n");
    let mut repositories = Repositories::new(None);
    let mut with_gitmodules = whole(&mut repositories);
    with_gitmodules.push(call(root, GITMODULES, &[], GitEnv::Inherited));
    assert_eq!(
        requested(&mut repositories, root, None, cli, false),
        with_gitmodules
    );

    assert_eq!(
        requested(&mut repositories, root, None, Caller::Hook, false),
        whole(&mut repositories)
    );

    let diff_env = private_index(&mut repositories, root);
    assert_eq!(
        requested(&mut repositories, root, Some(&set), cli, true),
        [call(root, NAME_DIFF, &["a.ts"], diff_env)]
    );

    assert_eq!(
        requested(&mut repositories, root, Some(&set), cli, false),
        [call(root, INDEXED, &["a.ts"], GitEnv::CLocale)]
    );
}

#[test]
fn changed_listing_is_prepared_once_per_root_written_set_and_caller() {
    let tree = listing_tree();
    let other = listing_tree();
    let cli = Caller::Cli(&tree.root);
    let a = tree.at("a.ts");
    let b = tree.at("b.ts");
    let sets = [
        written([]),
        written([a.clone()]),
        written([a.clone(), b.clone()]),
        written([b, a]),
    ];

    let mut inputs: Vec<(&str, Option<&IndexSet<String>>, Caller, bool)> = vec![
        (&tree.root, None, cli, false),
        (&tree.root, None, Caller::Hook, false),
        (&other.root, None, cli, false),
    ];

    for set in &sets {
        inputs.push((&tree.root, Some(set), Caller::Hook, false));
        inputs.push((&tree.root, Some(set), Caller::Hook, true));
        inputs.push((&tree.root, Some(set), cli, true));
    }

    let mut repositories = Repositories::new(None);
    let prepare = |repositories: &mut Repositories| -> Vec<Rc<PreparedListing>> {
        inputs
            .iter()
            .map(|&(root, written, caller, hunks)| {
                listing_calls(repositories, root, root, written, caller, hunks)
            })
            .collect()
    };

    let first = prepare(&mut repositories);
    assert_eq!(repositories.listings.len(), inputs.len());

    let again = prepare(&mut repositories);
    assert_eq!(repositories.listings.len(), inputs.len());
    for (index, (first, again)) in first.iter().zip(&again).enumerate() {
        assert!(Rc::ptr_eq(first, again), "input {index}");
        assert!(Rc::ptr_eq(first, &repositories.listings[index]));
    }
}

fn head(tree: &Tree) -> String {
    let output = fixture_git(&tree.root)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();

    String::from_utf8(output.stdout)
        .unwrap()
        .trim_end()
        .to_owned()
}

fn collects_from_the_stored_listing(
    tree: &Tree,
    targets: Targets,
    caller: Caller,
    names: impl FnOnce(&mut Repositories) -> NameCalls + Send,
    files: &[&str],
) {
    let root = &tree.root;
    cold(|| {
        let mut repositories = Repositories::new(Some(root));
        let written = match &targets {
            Targets::Directory(_) => None,
            Targets::Written(written) => Some((*written).clone()),
        };

        let listing = Rc::new(PreparedListing {
            root: root.clone(),
            written,
            cli: matches!(caller, Caller::Cli(_)),
            hunks: false,
            pathspecs: Vec::new(),
            names: names(&mut repositories),
            submodules: None,
        });

        let mut produced: Vec<GitCall> = listing.requested().cloned().collect();
        produced.sort_by(|left, right| left.args.cmp(&right.args));
        repositories.listings.push(listing);

        let mut located = locate_changed(targets, caller, false, &mut repositories);
        let (location, kept) = located.pop().unwrap();
        assert_eq!(repositories.prefetched_calls(), produced, "{files:?}");

        let selected =
            collect_changed(location, &mut repositories, kept.as_ref(), caller, false).unwrap();

        let files: Vec<String> = files.iter().map(|name| tree.at(name)).collect();
        assert_eq!(selected.errors, Vec::<String>::new(), "{files:?}");
        assert_eq!(selected.files, files);
        assert_eq!(selected.submodules, Vec::<String>::new(), "{files:?}");
        assert_eq!(repositories.listings.len(), 1, "{files:?}");
        assert_eq!(repositories.prefetched_calls(), [], "{files:?}");
    });
}

#[test]
fn changed_listing_preparation_is_shared() {
    let tree = listing_tree();
    let root = &tree.root;
    tree.write_text("a.ts", "export const changed = true;\n");
    tree.write_text(".gitmodules", "[submodule \"s\"]\n\tpath = s\n");
    fs::create_dir(tree.at("s")).unwrap();
    tree.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{},s", head(&tree)),
    ]);

    let set = written([tree.at("a.ts"), tree.at("b.ts")]);
    let cli = Caller::Cli(root);
    let cases = [
        (Targets::Directory(root), cli, false, 3),
        (Targets::Directory(root), cli, true, 3),
        (Targets::Directory(root), Caller::Hook, false, 2),
        (Targets::Written(&set), Caller::Hook, false, 1),
        (Targets::Written(&set), Caller::Hook, true, 2),
    ];

    for (index, (targets, caller, hunks, count)) in cases.into_iter().enumerate() {
        cold(|| {
            let mut repositories = Repositories::new(Some(root));
            let mut located = locate_changed(targets, caller, hunks, &mut repositories);
            let (location, kept) = located.pop().unwrap();
            assert_eq!(location, Location::Repository(root.clone()), "case {index}");
            assert_eq!(repositories.listings.len(), 1, "case {index}");

            let prepared = repositories.listings[0].clone();
            let mut produced: Vec<GitCall> = prepared.requested().cloned().collect();
            produced.sort_by(|left, right| left.args.cmp(&right.args));
            assert_eq!(produced.len(), count, "case {index}");
            assert_eq!(repositories.prefetched_calls(), produced, "case {index}");

            let selected =
                collect_changed(location, &mut repositories, kept.as_ref(), caller, hunks).unwrap();

            let submodules = if count == 3 {
                vec![tree.at("s")]
            } else {
                vec![]
            };
            assert_eq!(selected.errors, Vec::<String>::new(), "case {index}");
            assert_eq!(
                selected.files,
                [tree.at("a.ts"), tree.at("b.ts")],
                "case {index}"
            );

            assert_eq!(selected.submodules, submodules, "case {index}");
            assert_eq!(repositories.listings.len(), 1, "case {index}");
            assert!(
                Rc::ptr_eq(&repositories.listings[0], &prepared),
                "case {index}"
            );

            assert_eq!(repositories.prefetched_calls(), [], "case {index}");
        });
    }

    let diff = |repositories: &mut Repositories, pathspecs: &[&str]| {
        call(
            root,
            NAME_DIFF,
            pathspecs,
            private_index(repositories, root),
        )
    };

    collects_from_the_stored_listing(
        &tree,
        Targets::Directory(root),
        Caller::Hook,
        |repositories| NameCalls::Changed {
            diff: diff(repositories, &["a.ts"]),
            untracked: None,
        },
        &["a.ts"],
    );

    collects_from_the_stored_listing(
        &tree,
        Targets::Written(&set),
        Caller::Hook,
        |_| NameCalls::Indexed(call(root, INDEXED_OR_OTHERS, &["b.ts"], GitEnv::CLocale)),
        &["b.ts"],
    );

    collects_from_the_stored_listing(
        &tree,
        Targets::Directory(root),
        cli,
        |repositories| NameCalls::Changed {
            diff: diff(repositories, &[]),
            untracked: Some(call(root, OTHERS, &[], GitEnv::CLocale)),
        },
        &["a.ts", "b.ts"],
    );
}

#[test]
fn changed_listing_at_a_located_root_is_prepared_apart_from_its_prediction() {
    let tree = Tree::repository();
    let predicted = &tree.root;
    let located = tree.at("sub");
    tree.write("sub/a.ts");
    tree.write_text("sub/.git", "gitdir: ../.git\n");
    tree.git(&["config", "core.worktree", &located]);
    tree.git(&["add", "a.ts"]);
    tree.commit();
    tree.write("sub/b.ts");

    cold(|| {
        let cli = Caller::Cli(predicted);
        let mut repositories = Repositories::new(Some(predicted));
        let mut groups =
            locate_changed(Targets::Directory(predicted), cli, false, &mut repositories);

        let (location, kept) = groups.pop().unwrap();
        assert_eq!(location, Location::Repository(located.clone()));
        assert_eq!(repositories.listings.len(), 1);

        let at_prediction = repositories.prefetched_calls();
        let predicted_env = private_index(&mut repositories, predicted);
        assert_eq!(
            at_prediction,
            [
                call(predicted, NAME_DIFF, &[], predicted_env.clone()),
                call(predicted, OTHERS, &[], GitEnv::CLocale),
            ]
        );

        let selected =
            collect_changed(location, &mut repositories, kept.as_ref(), cli, false).unwrap();

        assert_eq!(selected.errors, Vec::<String>::new());
        assert_eq!(selected.files, [tree.at("sub/b.ts")]);
        assert_eq!(repositories.listings.len(), 2);
        assert_eq!(repositories.prefetched_calls(), at_prediction);

        let located_env = private_index(&mut repositories, &located);
        assert_ne!(located_env, predicted_env);
        assert_eq!(
            requested(&mut repositories, &located, None, cli, false),
            [
                call(&located, NAME_DIFF, &[], located_env.clone()),
                call(&located, OTHERS, &[], GitEnv::CLocale),
            ]
        );

        assert_eq!(repositories.listings.len(), 2);

        let set = written([tree.at("sub/a.ts")]);
        assert_eq!(
            requested(&mut repositories, &located, Some(&set), Caller::Hook, true),
            [call(&located, NAME_DIFF, &["a.ts"], located_env)]
        );

        assert_eq!(
            requested(&mut repositories, predicted, Some(&set), Caller::Hook, true),
            [
                call(predicted, NAME_DIFF, &["sub/a.ts"], predicted_env),
                call(predicted, OTHERS, &["sub/a.ts"], GitEnv::CLocale),
            ]
        );
    });
}

fn offered(root: &str, calls: Vec<GitCall>) -> Repositories {
    let mut repositories = Repositories::new(Some(root));
    repositories.prefetch(&[], calls);
    repositories
}

#[test]
fn changed_listing_fallbacks_run_only_after_a_failed_diff() {
    let symbolic_ref = |root: &str| call(root, "symbolic-ref -q HEAD", &[], GitEnv::Inherited);
    let fallbacks = |root: &str, pathspecs: &[&str]| {
        vec![
            call(root, INDEXED, pathspecs, GitEnv::Inherited),
            call(root, OTHERS, pathspecs, GitEnv::CLocale),
            symbolic_ref(root),
        ]
    };

    let tree = listing_tree();
    let root = &tree.root;
    let set = written([tree.at("a.ts")]);
    cold(|| {
        let mut repositories = offered(root, fallbacks(root, &["a.ts"]));
        let location = Location::Repository(root.clone());
        let selected =
            collect_changed(location, &mut repositories, Some(&set), Caller::Hook, true).unwrap();

        assert_eq!(selected.errors, Vec::<String>::new());
        assert_eq!(repositories.prefetched_calls(), fallbacks(root, &["a.ts"]));
    });

    let head = head(&tree);
    let (fanout, object) = head.split_at(2);
    fs::remove_file(tree.at(&format!(".git/objects/{fanout}/{object}"))).unwrap();

    cold(|| {
        let mut repositories = offered(root, fallbacks(root, &["a.ts"]));
        let location = Location::Repository(root.clone());
        let selected =
            collect_changed(location, &mut repositories, Some(&set), Caller::Hook, true).unwrap();

        assert!(
            matches!(&selected.errors[..], [error] if error.contains("git diff failed")),
            "{:?}",
            selected.errors
        );

        assert_eq!(
            repositories.prefetched_calls(),
            [
                call(root, INDEXED, &["a.ts"], GitEnv::Inherited),
                symbolic_ref(root),
            ]
        );
    });

    let unborn = Tree::repository();
    let root = &unborn.root;
    unborn.write("a.ts");
    unborn.git(&["add", "a.ts"]);
    unborn.write("b.ts");

    cold(|| {
        let mut repositories = offered(
            root,
            vec![
                call(root, INDEXED, &[], GitEnv::Inherited),
                symbolic_ref(root),
            ],
        );

        let location = Location::Repository(root.clone());
        let selected =
            collect_changed(location, &mut repositories, None, Caller::Hook, false).unwrap();

        assert_eq!(selected.errors, Vec::<String>::new());
        assert_eq!(selected.files, [unborn.at("a.ts"), unborn.at("b.ts")]);
        assert_eq!(repositories.prefetched_calls(), []);
    });

    unborn.git(&["symbolic-ref", "HEAD", "refs/heads/fresh"]);
    fs::create_dir_all(unborn.at(".git/refs/heads")).unwrap();
    fs::write(unborn.at(".git/refs/heads/fresh"), "junk\n").unwrap();

    cold(|| {
        let unreached = vec![
            call(root, INDEXED, &[], GitEnv::Inherited),
            call(root, OTHERS, &[], GitEnv::CLocale),
        ];

        let mut repositories = offered(root, unreached.clone());
        let location = Location::Repository(root.clone());
        let selected =
            collect_changed(location, &mut repositories, None, Caller::Hook, false).unwrap();

        assert!(
            matches!(&selected.errors[..], [error] if error.contains("git symbolic-ref failed")),
            "{:?}",
            selected.errors
        );

        assert_eq!(repositories.prefetched_calls(), unreached);
    });
}
