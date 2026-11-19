use std::fs;
use std::os::unix::fs::symlink;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::eslint::package_name;
use super::evaluate::{Value, property};
use super::layers::{Config, Family, Setting, Target};
use super::module::Session;
use super::{brace_decisions, braces_setting, settled};

static SCRATCHES: AtomicUsize = AtomicUsize::new(0);

pub(super) struct Scratch(pub(super) String);

impl Scratch {
    pub(super) fn join(&self, path: &str) -> String {
        format!("{}/{path}", self.0)
    }

    pub(super) fn write(&self, name: &str, text: &str) {
        let path = self.join(name);
        fs::create_dir_all(crate::paths::dirname(&path)).unwrap();
        fs::write(path, text).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn dir_with(files: &[(&str, &str)]) -> Scratch {
    let root = fs::canonicalize(std::env::temp_dir()).unwrap();
    let dir = loop {
        let name = format!(
            "stanza-config-{}-{}",
            std::process::id(),
            SCRATCHES.fetch_add(1, Ordering::Relaxed)
        );

        let dir = root.join(name);
        match fs::create_dir(&dir) {
            Ok(()) => break dir,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("cannot create a scratch directory: {error}"),
        }
    };

    let scratch = Scratch(dir.to_string_lossy().into_owned());
    for (name, text) in files {
        scratch.write(name, text);
    }

    scratch
}

fn enforced(dir: &str) -> bool {
    settled(&brace_decisions(&Target::files_in(dir, None)).unwrap()).enforced
}

fn enforced_for(dir: &str, extension: &str) -> bool {
    settled(&brace_decisions(&Target::files_in(dir, Some(extension))).unwrap()).enforced
}

pub(super) fn decided(dir: &str, extension: Option<&str>) -> Vec<(Family, Setting, Vec<String>)> {
    brace_decisions(&Target::files_in(dir, extension))
        .unwrap()
        .into_iter()
        .map(|decision| (decision.family, decision.setting, decision.files))
        .collect()
}

pub(super) fn shared_config(levels: usize, next: impl Fn(&str) -> String) -> String {
    let mut lines = vec![r#"const c0 = { rules: { curly: "off" } };"#.to_owned()];
    for level in 1..=levels {
        lines.push(format!(
            "const c{level} = {};",
            next(&format!("c{}", level - 1))
        ));
    }

    format!("{}\nexport default c{levels};\n", lines.join("\n"))
}

#[test]
fn shared_flat_config_layers_stay_under_the_budget() {
    let dir = dir_with(&[(
        "eslint.config.js",
        &shared_config(30, |previous| format!("[{previous}, {previous}]")),
    )]);

    assert_eq!(
        decided(&dir.0, Some(".ts")),
        [(
            Family::Flat,
            Setting::Off,
            vec![dir.join("eslint.config.js")]
        )]
    );

    assert!(!enforced_for(&dir.0, ".ts"));
}

#[test]
fn distinct_flat_scopes_exhaust_the_expansion_budget() {
    let dir = dir_with(&[(
        "eslint.config.js",
        &shared_config(30, |previous| {
            format!(
                r#"[{{ files: ["**/*.ts"], extends: [{previous}] }}, {{ files: ["**/*.js"], extends: [{previous}] }}]"#
            )
        }),
    )]);

    assert_eq!(
        decided(&dir.0, Some(".ts")),
        [(
            Family::Flat,
            Setting::Unknown,
            vec![dir.join("eslint.config.js")]
        )]
    );

    assert_eq!(
        braces_setting(&dir.join("a.ts")).unread,
        [dir.join("eslint.config.js")]
    );
}

#[test]
fn shared_oxlint_extends_stay_under_the_budget() {
    let dir = dir_with(&[(
        "oxlint.config.ts",
        &shared_config(30, |previous| {
            format!("{{ extends: [{previous}, {previous}] }}")
        }),
    )]);

    assert_eq!(
        decided(&dir.0, Some(".ts")),
        [(
            Family::Oxlint,
            Setting::Off,
            vec![dir.join("oxlint.config.ts")]
        )]
    );
}

fn chain(prefix: &str, levels: usize) -> String {
    let mut lines = vec![format!(
        r#"const {prefix}0 = {{ rules: {{ curly: "off" }} }};"#
    )];
    for level in 1..=levels {
        let previous = format!("{prefix}{}", level - 1);
        lines.push(format!(
            r#"const {prefix}{level} = [{{ files: ["**/*.ts"], extends: [{previous}] }}, {{ files: ["**/*.js"], extends: [{previous}] }}];"#
        ));
    }

    lines.join("\n")
}

fn order_fixture(shared: usize, own: usize, twice: bool) -> Scratch {
    let extra = if twice {
        r#"{ files: ["**/*.ts"], extends: ["../shared.js"] }, "#
    } else {
        ""
    };

    dir_with(&[
        (
            "shared.js",
            &format!(
                "{}\nexport default [s{shared}, {{ rules: {{ curly: \"off\" }} }}];\n",
                chain("s", shared)
            ),
        ),
        (
            "a/eslint.config.js",
            "export default [{ extends: [\"../shared.js\"] }];\n",
        ),
        (
            "b/eslint.config.js",
            &format!(
                "{}\nexport default [{{ extends: [\"../shared.js\"] }}, {extra}o{own}, {{ rules: {{ curly: \"off\" }} }}];\n",
                chain("o", own)
            ),
        ),
    ])
}

fn setting_of(dir: &str) -> Option<Setting> {
    decided(dir, Some(".ts")).first().map(|decision| decision.1)
}

#[test]
fn a_budget_answer_does_not_depend_on_which_directory_warmed_its_shared_extends() {
    let cold = order_fixture(12, 14, false);
    let cold_setting = setting_of(&cold.join("b"));

    let warm = order_fixture(12, 14, false);
    assert_eq!(
        decided(&warm.join("a"), Some(".ts")),
        [(
            Family::Flat,
            Setting::Off,
            vec![warm.join("a/eslint.config.js")]
        )]
    );

    assert_eq!(setting_of(&warm.join("b")), cold_setting);
    assert_eq!(cold_setting, Some(Setting::Unknown));
}

#[test]
fn a_config_that_extends_one_file_twice_counts_that_file_once() {
    let cold = order_fixture(13, 10, true);
    assert_eq!(setting_of(&cold.join("b")), Some(Setting::Off));

    let warm = order_fixture(13, 10, true);
    assert_eq!(setting_of(&warm.join("a")), Some(Setting::Off));
    assert_eq!(setting_of(&warm.join("b")), Some(Setting::Off));
}

fn extends_chain(files: usize) -> Scratch {
    let dir = dir_with(&[(".eslintrc.json", r#"{ "extends": ["./c1.json"] }"#)]);
    for index in 1..files {
        let text = if index == files - 1 {
            r#"{ "rules": { "curly": "off" } }"#.to_owned()
        } else {
            format!(r#"{{ "extends": ["./c{}.json"] }}"#, index + 1)
        };

        dir.write(&format!("c{index}.json"), &text);
    }

    dir
}

#[test]
fn extends_reach_32_files_deep_and_no_further() {
    for (files, expected) in [
        (31, Setting::Off),
        (32, Setting::Off),
        (33, Setting::Unknown),
    ] {
        let dir = extends_chain(files);
        assert_eq!(setting_of(&dir.0), Some(expected), "{files} files");
    }
}

#[test]
fn an_extends_cycle_is_unknown_until_a_later_layer_settles_it() {
    for (a, b, expected) in [
        (
            r#"{ "extends": ["./b.json"], "rules": { "curly": "off" } }"#,
            r#"{ "extends": ["./a.json"] }"#,
            Setting::Off,
        ),
        (
            r#"{ "extends": ["./b.json"] }"#,
            r#"{ "extends": ["./a.json"], "rules": { "curly": "off" } }"#,
            Setting::Off,
        ),
        (
            r#"{ "extends": ["./b.json"] }"#,
            r#"{ "extends": ["./a.json"] }"#,
            Setting::Unknown,
        ),
    ] {
        let dir = dir_with(&[
            (".eslintrc.json", r#"{ "extends": ["./a.json"] }"#),
            ("a.json", a),
            ("b.json", b),
        ]);

        assert_eq!(setting_of(&dir.0), Some(expected), "{a} {b}");
    }
}

fn settings_read_by(reader: Family, dir: &Scratch, names: &[&str]) -> Vec<Setting> {
    let config = Config::default();
    names
        .iter()
        .map(|name| {
            reader
                .decide(&config, &Target::files_in(&dir.join(name), Some(".ts")))
                .unwrap()
                .setting
        })
        .collect()
}

fn settings_in_order(dir: &Scratch, names: &[&str]) -> Vec<Setting> {
    settings_read_by(Family::Legacy, dir, names)
}

fn assert_stable_for(reader: Family, dir: &Scratch, expected: &[(&str, Setting)]) {
    let names: Vec<&str> = expected.iter().map(|(name, _)| *name).collect();
    let settings: Vec<Setting> = expected.iter().map(|(_, setting)| *setting).collect();
    assert_eq!(settings_read_by(reader, dir, &names), settings);

    let reversed: Vec<&str> = names.iter().rev().copied().collect();
    let mut flipped = settings.clone();
    flipped.reverse();
    assert_eq!(settings_read_by(reader, dir, &reversed), flipped);

    for (name, setting) in expected {
        assert_eq!(
            settings_read_by(reader, dir, &[name]),
            [*setting],
            "{name} alone"
        );
    }
}

fn assert_stable(dir: &Scratch, expected: &[(&str, Setting)]) {
    assert_stable_for(Family::Legacy, dir, expected);
}

#[test]
fn stable_config_layers_across_a_capped_chain_and_a_cycle() {
    let capped = dir_with(&[
        (
            "deep/.eslintrc.json",
            r#"{"root":true,"extends":["../c/c1.json"]}"#,
        ),
        (
            "short/.eslintrc.json",
            r#"{"root":true,"extends":["../c/c20.json"]}"#,
        ),
        ("c/c40.json", r#"{"rules":{"curly":"off"}}"#),
    ]);

    for index in 1..40 {
        capped.write(
            &format!("c/c{index}.json"),
            &format!(r#"{{"extends":["./c{}.json"]}}"#, index + 1),
        );
    }

    assert_stable(
        &capped,
        &[("deep", Setting::Unknown), ("short", Setting::Off)],
    );

    for extends in [
        r#"["../shared/b.json"]"#,
        r#"["../shared/b.json","../shared/b.json"]"#,
    ] {
        let cycle = dir_with(&[
            (
                "one/.eslintrc.json",
                &format!(r#"{{"root":true,"extends":{extends},"rules":{{"curly":"off"}}}}"#),
            ),
            ("shared/b.json", r#"{"extends":["../one/.eslintrc.json"]}"#),
            (
                "two/.eslintrc.json",
                r#"{"root":true,"extends":["../shared/b.json"]}"#,
            ),
        ]);

        let alone = settings_in_order(&cycle, &["two"])[0];
        assert_eq!(alone, Setting::Off, "{extends}");
        assert_stable(&cycle, &[("one", Setting::Off), ("two", alone)]);
    }
}

fn import_chain(link: &str, end: &str) -> Scratch {
    let link = |next: &str| link.replace("NEXT", next);
    let dir = dir_with(&[
        ("chain/c40.mjs", end),
        ("a/eslint.config.mjs", &link("../chain/c0.mjs")),
        ("b/eslint.config.mjs", &link("../chain/c20.mjs")),
    ]);

    for index in 0..40 {
        dir.write(
            &format!("chain/c{index}.mjs"),
            &link(&format!("./c{}.mjs", index + 1)),
        );
    }

    dir
}

fn read_in_order(dir: &Scratch, names: &[&str], path: &[&str]) -> Vec<Option<String>> {
    let session = Session::default();
    names
        .iter()
        .map(|name| {
            let file = dir.join(&format!("{name}/eslint.config.mjs"));
            let mut value = session.exported(&file, "default", &Rc::default());
            for key in path {
                value = match (&value, key.parse::<usize>()) {
                    (Value::Array(items), Ok(index)) => {
                        items.get(index).cloned().unwrap_or_default()
                    }
                    _ => property(&session, &value, key),
                };
            }

            match value {
                Value::Unknown => None,
                Value::String(text) => Some(text.to_string()),
                Value::Number(number) => Some(number.to_string()),
                other => Some(format!("{other:?}")),
            }
        })
        .collect()
}

#[test]
fn stable_config_imports_through_extends() {
    let dir = dir_with(&[
        (
            "shared/c29.cjs",
            r#"module.exports = {rules:{curly:"off"}};"#,
        ),
        ("shared/f.cjs", r#"module.exports = require("./c0.cjs");"#),
        (
            "near/.eslintrc.json",
            r#"{"root":true,"extends":["../shared/f.cjs"]}"#,
        ),
        (
            "far/.eslintrc.json",
            r#"{"root":true,"extends":["../g/g1.json"]}"#,
        ),
        ("g/g5.json", r#"{"extends":["../shared/f.cjs"]}"#),
    ]);

    for index in 0..29 {
        dir.write(
            &format!("shared/c{index}.cjs"),
            &format!(r#"module.exports = require("./c{}.cjs");"#, index + 1),
        );
    }

    for index in 1..5 {
        dir.write(
            &format!("g/g{index}.json"),
            &format!(r#"{{"extends":["./g{}.json"]}}"#, index + 1),
        );
    }

    assert_stable(&dir, &[("near", Setting::Off), ("far", Setting::Unknown)]);
}

#[test]
fn stable_import_cycles() {
    let shared = [
        (
            "a/eslint.config.mjs",
            r#"export const preset = { rules: { curly: "off" } }; export default [{ extends: ["../shared/f.mjs"] }];"#,
        ),
        (
            "shared/f.mjs",
            r#"import { preset } from "../a/eslint.config.mjs"; export default preset;"#,
        ),
    ];

    let imported = [(
        "b/eslint.config.mjs",
        r#"import f from "../shared/f.mjs"; export default [f];"#,
    )];

    let bridged = [
        (
            "b/eslint.config.mjs",
            r#"export default [{ extends: ["../bridge.mjs"] }];"#,
        ),
        (
            "bridge.mjs",
            r#"export default [{ extends: ["./shared/f.mjs"] }];"#,
        ),
    ];

    for reader in [&imported[..], &bridged[..]] {
        let dir = dir_with(&[&shared[..], reader].concat());
        assert_stable_for(
            Family::Flat,
            &dir,
            &[("a", Setting::Unknown), ("b", Setting::Off)],
        );
    }

    let unread_by_a = dir_with(&[
        (
            "a/eslint.config.mjs",
            r#"import s from "../s.mjs"; export const p = { rules: { curly: "off" } }; export default [{ rules: s.x }];"#,
        ),
        (
            "s.mjs",
            r#"import { p } from "./a/eslint.config.mjs"; export default { x: { curly: "error" }, y: p.rules };"#,
        ),
        (
            "b/eslint.config.mjs",
            r#"import s from "../s.mjs"; export default [{ rules: s.y }];"#,
        ),
    ]);

    assert_stable_for(
        Family::Flat,
        &unread_by_a,
        &[("a", Setting::On), ("b", Setting::Off)],
    );

    let cut_in_a = dir_with(&[
        (
            "y.mjs",
            r#"import s from "./s.mjs"; export const off = { curly: "off" }; export default s;"#,
        ),
        (
            "s.mjs",
            r#"import { off } from "./y.mjs"; export default { rules: off };"#,
        ),
        ("f.mjs", r#"import s from "./s.mjs"; export default s;"#),
        (
            "a/eslint.config.mjs",
            r#"import y from "../y.mjs"; export default [{ extends: ["../f.mjs"] }, y];"#,
        ),
        (
            "b/eslint.config.mjs",
            r#"export default [{ extends: ["../f.mjs"] }];"#,
        ),
    ]);

    let alone = settings_read_by(Family::Flat, &cut_in_a, &["b"])[0];
    assert_eq!(alone, Setting::Off);
    let a = settings_read_by(Family::Flat, &cut_in_a, &["a"])[0];
    assert_stable_for(Family::Flat, &cut_in_a, &[("a", a), ("b", alone)]);

    cut_in_a.write(
        "a/eslint.config.mjs",
        r#"import y from "../y.mjs"; import f from "../f.mjs"; export default [y, f, { extends: ["../f.mjs"] }];"#,
    );

    let after_a = settings_read_by(Family::Flat, &cut_in_a, &["a", "b"])[1];
    assert_eq!(after_a, alone, "b after an a that forced the cycle first");

    let width_between = dir_with(&[
        (
            "y.mjs",
            r#"import s from "./s.mjs"; export const width = 91; export default s;"#,
        ),
        (
            "s.mjs",
            r#"import { width } from "./y.mjs"; export default { width };"#,
        ),
        ("f.mjs", r#"import s from "./s.mjs"; export default s;"#),
        (
            "prettier.config.mjs",
            r#"import y from "./y.mjs"; import f from "./f.mjs"; export default { ...y, printWidth: f.width };"#,
        ),
        (
            "b/eslint.config.mjs",
            r#"import f from "../f.mjs"; export default [f];"#,
        ),
    ]);

    let width_after = |lint_between: bool| {
        let config = Config::default();
        let session = &config.session;
        let file = width_between.join("prettier.config.mjs");
        session.depth.begin_root(&file);
        let prettier = session.exported(&file, "default", &Rc::default());
        if lint_between {
            let target = Target::files_in(&width_between.join("b"), Some(".ts"));
            Family::Flat.decide(&config, &target);
        }

        format!("{:?}", property(session, &prettier, "printWidth"))
    };

    assert_eq!(width_after(false), width_after(true));

    let width_first = dir_with(&[
        (
            "a/eslint.config.mjs",
            r#"export const preset = { rules: { curly: "off" } }; export default [{ extends: ["../f.mjs"] }];"#,
        ),
        (
            "f.mjs",
            r#"import { preset } from "./a/eslint.config.mjs"; export default { rules: preset.rules, width: 80 };"#,
        ),
        (
            "prettier.config.mjs",
            r#"import f from "./f.mjs"; export default { printWidth: f.width };"#,
        ),
    ]);

    let config = Config::default();
    let session = &config.session;
    let width_file = width_first.join("prettier.config.mjs");
    session.depth.begin_root(&width_file);
    let prettier = session.exported(&width_file, "default", &Rc::default());

    assert!(
        matches!(property(session, &prettier, "printWidth"), Value::Number(width) if width == 80.0)
    );

    let target = Target::files_in(&width_first.join("a"), Some(".ts"));
    let setting = Family::Flat.decide(&config, &target).unwrap().setting;
    assert_eq!(setting, Setting::Unknown, "a after a width read");
}

#[test]
fn stable_config_imports() {
    let off = r#"export default [{rules:{curly:"off"}}];"#;
    let lazy_off = r#"export default {rules:{curly:"off"}};"#;
    let width = "export default {printWidth:40};";
    for (link, end, path, short) in [
        (
            r#"import c from "NEXT"; export default c;"#,
            off,
            &["0", "rules", "curly"][..],
            "off",
        ),
        (
            r#"import * as n from "NEXT"; export default n.default;"#,
            off,
            &["0", "rules", "curly"],
            "off",
        ),
        (
            r#"import c from "NEXT"; export default {rules: c.rules};"#,
            lazy_off,
            &["rules", "curly"],
            "off",
        ),
        (
            r#"import c from "NEXT"; export default c;"#,
            width,
            &["printWidth"],
            "40",
        ),
    ] {
        let dir = import_chain(link, end);
        let short = Some(short.to_owned());
        for (names, expected) in [
            (&["a", "b"][..], &[None, short.clone()][..]),
            (&["b", "a"], &[short.clone(), None]),
            (&["a"], &[None]),
            (&["b"], std::slice::from_ref(&short)),
        ] {
            assert_eq!(
                read_in_order(&dir, names, path),
                expected,
                "{link} {end} {names:?}"
            );
        }
    }
}

#[test]
fn the_decision_names_the_config_file_that_decided_it() {
    let dir = dir_with(&[
        (
            "eslint.config.js",
            r#"export default [{ rules: { curly: "error" } }];"#,
        ),
        (
            "app/eslint.config.js",
            r#"export default [{ rules: { curly: "off" } }];"#,
        ),
    ]);

    assert_eq!(
        decided(&dir.join("app"), None),
        [(
            Family::Flat,
            Setting::On,
            vec![dir.join("eslint.config.js")]
        )]
    );
}

#[test]
fn biome_use_block_statements_off_is_not_enforced() {
    let off = dir_with(&[(
        "biome.json",
        r#"{ "linter": { "rules": { "style": { "useBlockStatements": "off" } } } }"#,
    )]);

    let level_off = dir_with(&[(
        "biome.jsonc",
        r#"{ "linter": { "rules": { "style": { "useBlockStatements": { "level": "off" } } } } }"#,
    )]);

    let level_warn = dir_with(&[(
        "biome.json",
        r#"{ "linter": { "rules": { "style": { "useBlockStatements": { "level": "warn" } } } } }"#,
    )]);

    assert!(!enforced(&off.0));
    assert!(!enforced(&level_off.0));
    assert!(enforced(&level_warn.0));
}

#[test]
fn oxlint_rules_and_categories_control_braces() {
    for (text, expected) in [
        (r#"{ "rules": { "eslint/curly": "error" } }"#, true),
        (r#"{ "categories": { "style": "warn" } }"#, true),
        (
            r#"{ "categories": { "style": "error" }, "rules": { "curly": "off" } }"#,
            false,
        ),
        (r#"{ "rules": { "curly": "allow" } }"#, false),
    ] {
        assert_eq!(
            enforced(&dir_with(&[(".oxlintrc.json", text)]).0),
            expected,
            "{text}"
        );
    }
}

#[test]
fn the_later_of_curly_and_eslint_curly_wins_across_spreads() {
    for (rules, expected) in [
        (r#"{ ...base, "eslint/curly": "off" }"#, false),
        (r#"{ "eslint/curly": "off", ...base }"#, true),
        (r#"{ ...{ "eslint/curly": "off" }, ...base }"#, true),
        (r#"{ ...base, ...{ "eslint/curly": "off" } }"#, false),
    ] {
        let text = format!(
            "const base = {{ semi: \"off\", curly: \"error\" }};\nexport default {{ rules: {rules} }};\n"
        );

        assert_eq!(
            enforced(&dir_with(&[("oxlint.config.ts", &text)]).0),
            expected,
            "{rules}"
        );
    }
}

#[test]
fn oxlint_overrides_use_legacy_file_globs() {
    let root = dir_with(&[(
        ".oxlintrc.json",
        r#"{ "overrides": [{ "files": ["legacy/**"], "rules": { "curly": "error" } }] }"#,
    )]);

    assert!(enforced_for(&root.join("legacy"), ".ts"));
    assert!(!enforced_for(&root.0, ".ts"));
}

#[test]
fn oxlint_typescript_define_config_is_static() {
    for (setting, expected) in [("error", true), ("off", false)] {
        let text = format!(
            "import {{ defineConfig }} from \"oxlint\";\nexport default defineConfig({{ rules: {{ curly: \"{setting}\" }} }});"
        );

        assert_eq!(
            enforced(&dir_with(&[("oxlint.config.ts", &text)]).0),
            expected
        );
    }
}

#[test]
fn the_nearest_oxlint_config_decides_alone() {
    let root = dir_with(&[
        (".oxlintrc.json", r#"{ "rules": { "curly": "error" } }"#),
        ("nested/.oxlintrc.json", "{}"),
    ]);

    assert!(!enforced(&root.join("nested")));
}

#[test]
fn oxlint_extends_apply_categories_rules_then_overrides() {
    for (base, own, expected) in [
        (
            r#"{ "rules": { "curly": "off" } }"#,
            r#""categories": { "style": "warn" } }"#,
            false,
        ),
        (
            r#"{ "rules": { "curly": "warn" } }"#,
            r#""categories": { "style": "off" } }"#,
            true,
        ),
    ] {
        let root = dir_with(&[
            ("base.json", base),
            (
                ".oxlintrc.json",
                &format!(r#"{{ "extends": ["./base.json"], {own}"#),
            ),
        ]);

        assert_eq!(enforced(&root.0), expected);
    }

    for (entries, expected) in [
        (r#""./on.json", "./off.json""#, false),
        (r#""./off.json", "./on.json""#, true),
    ] {
        let root = dir_with(&[
            ("on.json", r#"{ "rules": { "curly": "error" } }"#),
            ("off.json", r#"{ "rules": { "curly": "off" } }"#),
            (
                ".oxlintrc.json",
                &format!(r#"{{ "extends": [{entries}] }}"#),
            ),
        ]);

        assert_eq!(enforced(&root.0), expected);
    }
}

#[test]
fn multiple_oxlint_configs_keep_braces_and_name_every_config() {
    let dir = dir_with(&[
        (".oxlintrc.json", "{}"),
        ("oxlint.config.ts", "export default {};"),
    ]);

    assert!(enforced(&dir.0));
    assert_eq!(
        decided(&dir.0, None),
        [(
            Family::Oxlint,
            Setting::Unknown,
            vec![dir.join(".oxlintrc.json"), dir.join("oxlint.config.ts")]
        )]
    );
}

#[test]
fn eslint_curly_is_checked_even_when_a_biome_config_exists() {
    let both = dir_with(&[
        ("biome.json", "{}"),
        (
            "eslint.config.js",
            r#"export default [{ rules: { curly: "error" } }];"#,
        ),
    ]);

    assert!(enforced(&both.0));
    assert!(enforced(
        &dir_with(&[(
            "eslint.config.mjs",
            r#"export default [{ rules: { curly: ["error", "multi"] } }];"#
        )])
        .0
    ));

    assert!(!enforced(
        &dir_with(&[(".eslintrc.json", r#"{ "rules": { "curly": 0 } }"#)]).0
    ));
    assert!(!enforced(
        &dir_with(&[(".eslintrc.json", r#"{ "rules": { "curly": ["off"] } }"#)]).0
    ));
}

#[test]
fn legacy_eslint_reads_eslint_config_from_package_json() {
    for (curly, expected) in [("error", true), ("off", false)] {
        let text = format!(r#"{{"name":"x","eslintConfig":{{"rules":{{"curly":"{curly}"}}}}}}"#);
        assert_eq!(enforced(&dir_with(&[("package.json", &text)]).0), expected);
    }

    assert_eq!(
        decided(&dir_with(&[("package.json", r#"{ "name": "x" }"#)]).0, None),
        []
    );
}

#[test]
fn a_package_json_with_a_byte_order_mark_is_read_like_eslint_reads_it() {
    let plain = dir_with(&[("package.json", "\u{feff}{ \"name\": \"x\" }")]);
    let off = dir_with(&[(
        "package.json",
        "\u{feff}{ \"eslintConfig\": { \"rules\": { \"curly\": \"off\" } } }",
    )]);

    assert_eq!(decided(&plain.0, None), []);
    assert!(!enforced(&off.0));
}

#[test]
fn legacy_eslint_prefers_eslintrc_json_over_package_json() {
    let dir = dir_with(&[
        (".eslintrc.json", r#"{ "rules": { "curly": "off" } }"#),
        (
            "package.json",
            r#"{ "eslintConfig": { "rules": { "curly": "error" } } }"#,
        ),
    ]);

    assert!(!enforced(&dir.0));
}

#[test]
fn flat_eslint_config_excludes_package_json_eslint_config() {
    let config = r#"export default [{ rules: { curly: "off" } }];"#;
    let manifest = r#"{ "eslintConfig": { "rules": { "curly": "error" } } }"#;
    let same = dir_with(&[("eslint.config.js", config), ("package.json", manifest)]);
    let parent = dir_with(&[
        ("eslint.config.js", config),
        ("child/package.json", manifest),
    ]);

    assert!(!enforced(&same.0));
    assert!(!enforced(&parent.join("child")));
}

#[test]
fn unresolved_and_invalid_package_json_eslint_config_keep_braces() {
    let missing = dir_with(&[(
        "package.json",
        r#"{ "eslintConfig": { "extends": ["missing"] } }"#,
    )]);

    assert!(enforced(&missing.0));

    for manifest in [r#"{ "eslintConfig": {}, }"#, r#"{ "eslintConfig": "off" }"#] {
        let dir = dir_with(&[("package.json", manifest)]);
        assert!(enforced(&dir.0));
        assert_eq!(
            decided(&dir.0, None),
            [(
                Family::Legacy,
                Setting::Unknown,
                vec![dir.join("package.json")]
            )]
        );
    }
}

#[test]
fn package_json_eslint_config_resolves_extends_and_applies_overrides() {
    let dir = dir_with(&[
        (
            "package.json",
            r#"{"eslintConfig":{"extends":["strict"],"overrides":[{"files":["src/**"],"rules":{"curly":"off"}}]}}"#,
        ),
        (
            "node_modules/eslint-config-strict/package.json",
            r#"{ "main": "index.json" }"#,
        ),
        (
            "node_modules/eslint-config-strict/index.json",
            r#"{ "rules": { "curly": "error" } }"#,
        ),
    ]);

    assert!(enforced(&dir.0));
    assert!(!enforced_for(&dir.join("src"), ".ts"));
}

#[test]
fn package_json_eslint_config_root_stops_the_legacy_upward_walk() {
    let root = dir_with(&[
        (".eslintrc.json", r#"{ "rules": { "curly": "error" } }"#),
        (
            "child/package.json",
            r#"{ "eslintConfig": { "root": true } }"#,
        ),
    ]);

    assert!(!enforced(&root.join("child")));
}

#[test]
fn nested_legacy_configs_against_their_ancestors() {
    let silent = dir_with(&[
        (".eslintrc.json", r#"{ "rules": { "curly": "error" } }"#),
        (
            "packages/app/.eslintrc.json",
            r#"{ "rules": { "semi": "error" } }"#,
        ),
    ]);

    let off = dir_with(&[
        (".eslintrc.json", r#"{ "rules": { "curly": "error" } }"#),
        (
            "packages/legacy/.eslintrc.json",
            r#"{ "rules": { "curly": "off" } }"#,
        ),
    ]);

    let notes = dir_with(&[
        (".eslintrc.json", r#"{ "rules": { "curly": "error" } }"#),
        (
            "packages/notes/.eslintrc.json",
            r#"{ "settings": { "note": "curly: off", "other": "useBlockStatements: off" } }"#,
        ),
    ]);

    assert!(enforced(&silent.join("packages/app")));
    assert!(!enforced(&off.join("packages/legacy")));
    assert!(enforced(&off.0));
    assert!(enforced(&notes.join("packages/notes")));
}

#[test]
fn comments_and_strings_are_not_settings() {
    for (name, text, expected) in [
        (
            "eslint.config.js",
            "export default [{ rules: { // curly: \"error\"\n } }];",
            false,
        ),
        (
            "biome.jsonc",
            r#"{ /* "useBlockStatements": "error" */ "linter": {} }"#,
            false,
        ),
        (
            ".eslintrc.json",
            r#"{ "$schema": "https://example.com/x", "rules": { "curly": "error" } }"#,
            true,
        ),
        (
            ".eslintrc.json",
            r#"{ "settings": { "team": "team//web" }, "rules": { "curly": "error" } }"#,
            true,
        ),
        (
            "eslint.config.js",
            r#"export default [{ name: "x // y", rules: { curly: "error" } }];"#,
            true,
        ),
        (
            "biome.jsonc",
            r#"{ "linter": { "rules": { "style": { "useBlockStatements": "error" } } } } // end"#,
            true,
        ),
        (
            "eslint.config.js",
            "export default [{ rules: { curly: ",
            true,
        ),
    ] {
        assert_eq!(enforced(&dir_with(&[(name, text)]).0), expected, "{text}");
    }
}

#[test]
fn biome_off_in_the_same_directory_does_not_hide_eslint_curly() {
    let dir = dir_with(&[
        (
            "biome.json",
            r#"{ "linter": { "rules": { "style": { "useBlockStatements": "off" } } } }"#,
        ),
        (
            "eslint.config.js",
            r#"export default [{ rules: { curly: "error" } }];"#,
        ),
    ]);

    assert!(enforced(&dir.0));
}

#[test]
fn biome_overrides_apply_only_to_their_directories() {
    let root = dir_with(&[(
        "biome.json",
        r#"{ "linter": { "rules": { "style": { "useBlockStatements": "off" } } }, "overrides": [{ "includes": ["legacy/**"], "linter": { "rules": { "style": { "useBlockStatements": { "level": "error" } } } } }] }"#,
    )]);

    let jsonc = dir_with(&[(
        "biome.jsonc",
        "{ // legacy code\n \"overrides\": [{ \"includes\": [\"src/legacy/**\"], \"linter\": { \"rules\": { \"style\": { \"useBlockStatements\": \"error\" } } } }] }",
    )]);

    assert!(!enforced(&root.0));
    assert!(enforced(&root.join("legacy")));
    assert!(enforced(&jsonc.join("src/legacy")));
    assert!(enforced(&jsonc.join("src/legacy/deep")));
    assert!(!enforced(&jsonc.join("src")));
    assert!(!enforced(&jsonc.0));
}

const YAML_ESLINT_NAMES: [&str; 3] = [".eslintrc.yaml", ".eslintrc.yml", ".eslintrc"];

fn legacy_settings(dir: &str) -> [Option<Setting>; 2] {
    [".ts", ".js"].map(|extension| {
        decided(dir, Some(extension))
            .first()
            .map(|decision| decision.1)
    })
}

#[test]
fn yaml_eslint_configs_match_their_json_twins() {
    let on = Some(Setting::On);
    let off = Some(Setting::Off);
    for (yaml, json, expected) in [
        (
            "rules:\n  curly: error\n",
            r#"{"rules":{"curly":"error"}}"#,
            [on, on],
        ),
        (
            "rules:\n  curly: off # braces optional\n",
            r#"{"rules":{"curly":"off"}}"#,
            [off, off],
        ),
        (
            "\"rules\":\n  \"curly\": \"error\"\n",
            r#"{"rules":{"curly":"error"}}"#,
            [on, on],
        ),
        (
            "'rules':\n  'curly': 'off'\n",
            r#"{"rules":{"curly":"off"}}"#,
            [off, off],
        ),
        (
            "rules: {curly: [error, multi]}\n",
            r#"{"rules":{"curly":["error","multi"]}}"#,
            [on, on],
        ),
        (
            "rules:\n  curly:\n    - off\n    - all\n",
            r#"{"rules":{"curly":["off","all"]}}"#,
            [off, off],
        ),
        (
            "rules:\n  curly: [0]\n",
            r#"{"rules":{"curly":[0]}}"#,
            [off, off],
        ),
        ("rules:\n  curly: 2\n", r#"{"rules":{"curly":2}}"#, [on, on]),
        (
            "extends: eslint:all\n",
            r#"{"extends":"eslint:all"}"#,
            [on, on],
        ),
        (
            "extends:\n  - eslint:all\nrules:\n  curly: off\n",
            r#"{"extends":["eslint:all"],"rules":{"curly":"off"}}"#,
            [off, off],
        ),
        (
            "rules:\n  curly: error\noverrides:\n  - files: [\"*.ts\"]\n    rules:\n      curly: off\n",
            r#"{"rules":{"curly":"error"},"overrides":[{"files":["*.ts"],"rules":{"curly":"off"}}]}"#,
            [off, on],
        ),
        (
            "\"rules\":\n  \"curly\": \"off\"\n\"overrides\":\n  - \"files\": [\"*.ts\"]\n    \"extends\": [\"eslint:all\"]\n",
            r#"{"rules":{"curly":"off"},"overrides":[{"files":["*.ts"],"extends":["eslint:all"]}]}"#,
            [on, off],
        ),
    ] {
        let twin = dir_with(&[(".eslintrc.json", json)]);
        assert_eq!(legacy_settings(&twin.0), expected, "{json}");

        for name in YAML_ESLINT_NAMES {
            let dir = dir_with(&[(name, yaml)]);
            assert_eq!(legacy_settings(&dir.0), expected, "{name} {yaml:?}");
        }

        let strict = dir_with(&[(".eslintrc", json)]);
        assert_eq!(legacy_settings(&strict.0), expected, ".eslintrc {json}");
    }
}

#[test]
fn yaml_eslint_configs_follow_roots_and_local_extends() {
    for name in YAML_ESLINT_NAMES {
        let parent = (".eslintrc.json", r#"{"rules":{"curly":"error"}}"#);
        let child = format!("child/{name}");

        let stopped = dir_with(&[parent, (&child, "root: true # stop\n")]);
        assert!(!enforced(&stopped.join("child")), "{name} root: true");

        let open = dir_with(&[parent, (&child, "root: false\n")]);
        assert!(enforced(&open.join("child")), "{name} root: false");

        let empty = dir_with(&[parent, (&child, "# nothing yet\n")]);
        assert!(enforced(&empty.join("child")), "{name} comment only");
        assert_eq!(
            legacy_settings(&empty.join("child")),
            [Some(Setting::On); 2]
        );

        let base = ("base.json", r#"{"rules":{"curly":"error"}}"#);
        let extended = dir_with(&[base, (name, "extends: ./base.json\n")]);
        assert_eq!(
            legacy_settings(&extended.0),
            [Some(Setting::On); 2],
            "{name} extends"
        );

        let overridden = dir_with(&[base, (name, "extends: ./base.json\nrules:\n  curly: off\n")]);
        assert_eq!(
            legacy_settings(&overridden.0),
            [Some(Setting::Off); 2],
            "{name} extends then off"
        );

        let unrelated = dir_with(&[(name, "rules:\n  curly: off\nother:\n  curly: error\n")]);
        assert_eq!(
            legacy_settings(&unrelated.0),
            [Some(Setting::Off); 2],
            "{name} unrelated curly key"
        );
    }
}

#[test]
fn yaml_eslint_configs_read_commented_json_and_stay_legacy_only() {
    let commented = dir_with(&[(
        ".eslintrc",
        "\u{feff}{\n  // braces optional\n  \"rules\": { /* here */ \"curly\": \"off\", },\n}\n",
    )]);
    assert_eq!(legacy_settings(&commented.0), [Some(Setting::Off); 2]);

    let glob = dir_with(&[(
        ".eslintrc",
        "rules:\n  curly: error\noverrides:\n  - files: ['src/**/*.ts']\n    rules:\n      curly: off\n",
    )]);
    assert_eq!(legacy_settings(&glob.join("src")), [Some(Setting::On); 2]);

    let quoted = dir_with(&[(
        ".eslintrc",
        "rules:\n  curly: error\noverrides:\n  - files: [\"src/**/*.ts\"]\n    rules:\n      curly: off\n",
    )]);
    assert_eq!(
        legacy_settings(&quoted.join("src")),
        [Some(Setting::Off), Some(Setting::On)]
    );

    let wide = "rules:\n  curly: off\noverrides:\n  - files: ['src/*\u{e9}*/part/a*.ts']\n    rules:\n      curly: error\n";
    let five = dir_with(&[(".eslintrc", wide)]);
    fs::create_dir_all(five.join("src     part")).unwrap();
    assert_eq!(
        legacy_settings(&five.join("src     part")),
        [Some(Setting::On), Some(Setting::Off)]
    );

    let mut invalid = b"root: true\nrules: {curly: off}\n//".to_vec();
    invalid.extend(b"\xff\n");
    let repaired = dir_with(&[]);
    fs::write(repaired.join(".eslintrc"), invalid).unwrap();
    assert_eq!(
        decided(&repaired.0, Some(".ts")),
        [(
            Family::Legacy,
            Setting::Unknown,
            vec![repaired.join(".eslintrc")]
        )]
    );

    for (base, expected) in [
        ("rules:\n  curly: off\n", Setting::Off),
        ("rules:\n  curly: error\n", Setting::On),
        ("rules:\n  curly: &a off\n", Setting::Unknown),
    ] {
        let dir = dir_with(&[
            (".eslintrc.json", r#"{"extends":["./base.yml"]}"#),
            ("base.yml", base),
        ]);
        assert_eq!(legacy_settings(&dir.0), [Some(expected); 2], "{base:?}");
    }

    let flat = dir_with(&[
        ("base.yml", "rules:\n  curly: off\n"),
        (
            "eslint.config.js",
            r#"export default [{ rules: { curly: "error" } }, { extends: ["./base.yml"] }];"#,
        ),
    ]);
    assert_eq!(
        decided(&flat.0, Some(".ts")),
        [(
            Family::Flat,
            Setting::Unknown,
            vec![flat.join("eslint.config.js")]
        )]
    );
}

#[test]
fn yaml_eslint_configs_outside_the_subset_are_unknown() {
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("yaml-rows.json")).expect("the table is JSON");

    let mut inputs: Vec<String> = rows
        .iter()
        .filter(|row| row["reader"] == "unknown")
        .map(|row| {
            row["input"]
                .as_str()
                .expect("a row has an input")
                .to_owned()
        })
        .collect();

    assert_eq!(inputs.len(), 23);

    let off = "rules:\n  curly: off\n";
    let nested: Vec<String> = (0..65)
        .map(|level| format!("{}k:", "  ".repeat(level)))
        .collect();

    inputs.extend([
        format!("{off}extra: &a 1\n"),
        format!("{off}extra: !!str x\n"),
        format!("{off}extra: |\n  data\n"),
        format!("{off}extra: [1\n"),
        format!("{off}{} 0\n", nested.join("\n")),
        format!("{off}extra: [{}]\n", vec!["0"; 100_000].join(",")),
        format!("{off}#{}", "x".repeat(1_048_576)),
        format!(
            "{{\"root\":true,\"rules\":{{\"curly\":\"off\"}}}}{}x",
            " ".repeat(1_048_576)
        ),
        format!("{off}{}: 1\n", "k".repeat(1025)),
        "rules:\n  curly: off\n  curly: error\n".to_owned(),
        "rules:\n  curly: 0x0\n".to_owned(),
        "- rules:\n    curly: off\n".to_owned(),
        "{rules:{curly:\"off\"}}\n".to_owned(),
        r#"{"root":true,"rules":{"curly":"error","curly":"off"}}"#.to_owned(),
        format!(
            r#"{{"rules":{{"curly":"off"}},"extra":[{}]}}"#,
            vec!["0"; 100_000].join(",")
        ),
        format!(
            r#"{{"rules":{{"curly":"off"}},"extra":{}{}}}"#,
            "[".repeat(65),
            "]".repeat(65)
        ),
    ]);

    for input in &inputs {
        for name in YAML_ESLINT_NAMES {
            let dir = dir_with(&[(name, input)]);
            let shown: String = input.chars().take(80).collect();
            for extension in [".ts", ".js"] {
                assert_eq!(
                    decided(&dir.0, Some(extension)),
                    [(Family::Legacy, Setting::Unknown, vec![dir.join(name)])],
                    "{name} {extension} {shown:?}"
                );
            }
        }
    }
}

#[test]
fn shared_packages_are_read_through_extends_and_imports() {
    for (entry, text) in [
        ("index.json", r#"{ "rules": { "curly": "error" } }"#),
        (
            "index.js",
            r#"module.exports = { rules: { curly: "error" } };"#,
        ),
    ] {
        let root = dir_with(&[
            (
                ".eslintrc.json",
                r#"{ "extends": ["@acme/eslint-config"] }"#,
            ),
            (
                "node_modules/@acme/eslint-config/package.json",
                &format!(r#"{{"name":"@acme/eslint-config","main":"{entry}"}}"#),
            ),
            (&format!("node_modules/@acme/eslint-config/{entry}"), text),
        ]);

        assert!(enforced(&root.0), "{entry}");
    }

    let flat = dir_with(&[
        (
            "eslint.config.js",
            r#"import acme from "@acme/eslint-config"; export default [acme];"#,
        ),
        (
            "node_modules/@acme/eslint-config/package.json",
            r#"{"name":"@acme/eslint-config","main":"index.js"}"#,
        ),
        (
            "node_modules/@acme/eslint-config/index.js",
            r#"export default { rules: { curly: "error" } };"#,
        ),
    ]);

    let off = dir_with(&[
        (
            ".eslintrc.json",
            r#"{ "extends": ["@acme/eslint-config"] }"#,
        ),
        (
            "node_modules/@acme/eslint-config/package.json",
            r#"{"name":"@acme/eslint-config","main":"index.json"}"#,
        ),
        (
            "node_modules/@acme/eslint-config/index.json",
            r#"{ "rules": { "curly": "off" } }"#,
        ),
    ]);

    assert!(enforced(&flat.0));
    assert!(!enforced(&off.0));
}

#[test]
fn scoped_eslint_subpaths() {
    for (name, expected) in [
        ("@acme/base/strict", "@acme/eslint-config-base/strict"),
        ("@acme/eslint-config/strict", "@acme/eslint-config/strict"),
        (
            "@acme/eslint-config-base/strict",
            "@acme/eslint-config-base/strict",
        ),
        (
            "@acme/eslint-configx/strict",
            "@acme/eslint-config-eslint-configx/strict",
        ),
        ("acme/strict", "eslint-config-acme/strict"),
    ] {
        assert_eq!(package_name(name, "eslint-config"), expected, "{name}");
    }

    let root = dir_with(&[
        (
            ".eslintrc.json",
            r#"{"root":true,"extends":["@acme/base/strict"]}"#,
        ),
        (
            "node_modules/@acme/eslint-config-base/package.json",
            r#"{"main":"index.js"}"#,
        ),
        (
            "node_modules/@acme/eslint-config-base/index.js",
            r#"module.exports = {rules:{curly:"off"}};"#,
        ),
        (
            "node_modules/@acme/eslint-config-base/strict.js",
            r#"module.exports = {rules:{curly:"error"}};"#,
        ),
    ]);

    assert_eq!(setting_of(&root.0), Some(Setting::On));
}

#[test]
fn presets_and_unresolvable_extends() {
    let prettier = dir_with(&[
        (".eslintrc.json", r#"{ "extends": ["prettier"] }"#),
        ("nested/.eslintrc.json", r#"{ "extends": ["missing"] }"#),
    ]);

    assert!(enforced(
        &dir_with(&[(".eslintrc.json", r#"{ "extends": ["missing"] }"#)]).0
    ));
    assert!(!enforced(&prettier.0));
    assert!(enforced(&prettier.join("nested")));
}

#[test]
fn an_unevaluable_spread_in_a_flat_config_keeps_braces() {
    let dir = dir_with(&[(
        "eslint.config.js",
        r#"export default [{ ...makeConfig(), rules: { curly: "off" } }];"#,
    )]);

    assert!(enforced(&dir.0));
}

#[test]
fn biome_group_severities() {
    for (text, expected) in [
        (r#"{ "linter": { "rules": { "style": "error" } } }"#, true),
        (r#"{ "linter": { "rules": { "style": "warn" } } }"#, true),
        (
            r#"{ "linter": { "rules": { "style": { "useImportType": { "options": { "style": "separatedType" } } } } } }"#,
            false,
        ),
        (
            r#"{ "javascript": { "formatter": { "style": "interface" } } }"#,
            false,
        ),
        (r#"{ "style": "interface" }"#, false),
        (
            r#"{ "linter": { "rules": { "all": true, "style": { "useBlockStatements": "off" } } } }"#,
            false,
        ),
    ] {
        assert_eq!(
            enforced(&dir_with(&[("biome.json", text)]).0),
            expected,
            "{text}"
        );
    }
}

#[test]
fn a_stale_nested_legacy_config_cannot_disable_the_flat_config() {
    let root = dir_with(&[
        (
            "eslint.config.js",
            r#"export default [{ rules: { curly: "error" } }];"#,
        ),
        (
            "nested/.eslintrc.json",
            r#"{ "rules": { "curly": "off" } }"#,
        ),
    ]);

    assert!(enforced(&root.join("nested")));
}

fn settings(dir: &str) -> Vec<Setting> {
    decided(dir, Some(".ts"))
        .into_iter()
        .map(|(_, setting, _)| setting)
        .collect()
}

const PLUGIN_CONFIGS: &str = r#"{ configs: { strict: { rules: { curly: "error" } }, off: { rules: { curly: "off" } }, built: build("strict") } }"#;

#[test]
fn plugin_brace_presets() {
    let local =
        format!("const build = (name) => ({{ name }});\nexport default {PLUGIN_CONFIGS};\n");
    let scoped = format!(
        "function build(name) {{ return {{ name }}; }}\nmodule.exports = {PLUGIN_CONFIGS};\n"
    );
    let packages = [
        (
            "eslint-plugin-local",
            "local",
            r#"{"type":"module","exports":"./index.js"}"#,
            local.as_str(),
        ),
        (
            "@scope/eslint-plugin",
            "@scope",
            r#"{"main":"index.js"}"#,
            scoped.as_str(),
        ),
    ];

    for (package, short, manifest, index) in packages {
        let flat = [
            format!(r#"import plugin from "{package}"; export default [plugin.configs.PRESET];"#),
            format!(
                r#"import * as plugin from "{package}"; export default [plugin.default.configs.PRESET];"#
            ),
            format!(
                r#"const plugin = require("{package}"); module.exports = [plugin.configs.PRESET];"#
            ),
            format!(
                r#"import plugin from "{package}"; export default [{{ plugins: {{ "{short}": plugin }}, extends: ["{short}/PRESET"] }}];"#
            ),
        ];

        let configs = flat
            .iter()
            .map(|text| ("eslint.config.js", text.clone(), false))
            .chain([
                (
                    "oxlint.config.ts",
                    format!(
                        r#"import plugin from "{package}"; export default {{ extends: [plugin.configs.PRESET] }};"#
                    ),
                    false,
                ),
                (
                    ".eslintrc.json",
                    format!(r#"{{"root":true,"extends":["plugin:{short}/PRESET"]}}"#),
                    true,
                ),
                (
                    "package.json",
                    format!(
                        r#"{{"eslintConfig":{{"root":true,"extends":["plugin:{short}/PRESET"]}}}}"#
                    ),
                    true,
                ),
            ]);

        for (name, text, legacy) in configs {
            let (manifest, index) = if legacy {
                (r#"{"main":"index.js"}"#, scoped.as_str())
            } else {
                (manifest, index)
            };

            for (preset, expected) in [
                ("strict", Setting::On),
                ("off", Setting::Off),
                ("built", Setting::Unknown),
            ] {
                let text = text.replace("PRESET", preset);
                let installed = dir_with(&[
                    (name, &text),
                    (&format!("node_modules/{package}/package.json"), manifest),
                    (&format!("node_modules/{package}/index.js"), index),
                ]);

                assert_eq!(settings(&installed.0), [expected], "{text}");
                if expected == Setting::Unknown {
                    assert_eq!(
                        braces_setting(&installed.join("a.ts")).unread,
                        [installed.join(name)]
                    );
                }

                let missing = dir_with(&[(name, &text)]);
                assert_eq!(settings(&missing.0), [Setting::Unknown], "missing {text}");
                assert_eq!(
                    braces_setting(&missing.join("a.ts")).unread,
                    [missing.join(name)]
                );
            }
        }
    }

    for (name, text, expected) in [
        (
            ".eslintrc.json",
            r#"{"root":true,"extends":["eslint:recommended"]}"#,
            vec![],
        ),
        (
            ".eslintrc.json",
            r#"{"root":true,"extends":["eslint:all"]}"#,
            vec![Setting::On],
        ),
        (
            ".eslintrc.json",
            r#"{"root":true,"extends":["eslint:all","plugin:prettier/recommended"]}"#,
            vec![Setting::Off],
        ),
        (
            ".eslintrc.json",
            r#"{"root":true,"extends":["eslint:all","prettier"]}"#,
            vec![Setting::Off],
        ),
        (
            "eslint.config.js",
            r#"import js from "@eslint/js"; export default [js.configs.all];"#,
            vec![Setting::On],
        ),
        (
            "eslint.config.js",
            r#"import js from "@eslint/js"; export default [js.configs.recommended];"#,
            vec![],
        ),
        (
            "eslint.config.js",
            r#"import ts from "typescript-eslint"; export default [...ts.configs.strict];"#,
            vec![],
        ),
        (
            "eslint.config.js",
            r#"import prettier from "eslint-plugin-prettier/recommended"; export default [{ rules: { curly: "error" } }, prettier];"#,
            vec![Setting::Off],
        ),
        (
            "eslint.config.js",
            r#"import prettier from "eslint-config-prettier/flat"; export default [{ rules: { curly: "error" } }, prettier];"#,
            vec![Setting::Off],
        ),
    ] {
        assert_eq!(settings(&dir_with(&[(name, text)]).0), expected, "{text}");
    }
}

fn local_plugin(configs: &str) -> String {
    format!("module.exports = {{ configs: {{ {configs} }} }};\n")
}

#[test]
fn plugin_brace_presets_resolve_from_the_entry() {
    let manifest = r#"{"main":"index.js"}"#;
    let entry = r#"{"root":true,"extends":["../shared/base.json"]}"#;
    let on = local_plugin(r#"strict: { rules: { curly: "error" } }"#);
    let off = local_plugin(r#"strict: { rules: { curly: "off" } }"#);
    let dir = dir_with(&[
        ("shared/base.json", r#"{"extends":["plugin:local/strict"]}"#),
        (
            "shared/node_modules/eslint-plugin-local/package.json",
            manifest,
        ),
        ("shared/node_modules/eslint-plugin-local/index.js", &off),
        ("on/.eslintrc.json", entry),
        ("on/node_modules/eslint-plugin-local/package.json", manifest),
        ("on/node_modules/eslint-plugin-local/index.js", &on),
        ("off/.eslintrc.json", entry),
        (
            "off/node_modules/eslint-plugin-local/package.json",
            manifest,
        ),
        ("off/node_modules/eslint-plugin-local/index.js", &off),
        ("none/.eslintrc.json", entry),
    ]);

    assert_stable(
        &dir,
        &[
            ("on", Setting::On),
            ("off", Setting::Off),
            ("none", Setting::Unknown),
        ],
    );
}

#[test]
fn plugin_brace_presets_resolve_beside_a_symlinked_entry() {
    let manifest = r#"{"main":"index.js"}"#;
    let on = local_plugin(r#"strict: { rules: { curly: "error" } }"#);
    let off = local_plugin(r#"strict: { rules: { curly: "off" } }"#);
    let dir = dir_with(&[
        (
            "shared/.eslintrc.json",
            r#"{"root":true,"extends":["plugin:local/strict"]}"#,
        ),
        (
            "shared/node_modules/eslint-plugin-local/package.json",
            manifest,
        ),
        ("shared/node_modules/eslint-plugin-local/index.js", &off),
        (
            "app/node_modules/eslint-plugin-local/package.json",
            manifest,
        ),
        ("app/node_modules/eslint-plugin-local/index.js", &on),
    ]);

    symlink(
        dir.join("shared/.eslintrc.json"),
        dir.join("app/.eslintrc.json"),
    )
    .unwrap();

    assert_stable(&dir, &[("app", Setting::On), ("shared", Setting::Off)]);
}

#[test]
fn plugin_brace_presets_bind_outer_configs_through_cache_hits() {
    let manifest = r#"{"main":"index.js"}"#;
    let entry = r#"{"root":true,"extends":["../shared/base.json","../shared/mid.json"]}"#;
    let dir = dir_with(&[
        ("shared/base.json", r#"{"extends":["plugin:local/strict"]}"#),
        ("shared/mid.json", r#"{"extends":["./base.json"]}"#),
        ("on/.eslintrc.json", entry),
        ("on/node_modules/eslint-plugin-local/package.json", manifest),
        (
            "on/node_modules/eslint-plugin-local/index.js",
            &local_plugin(r#"strict: { rules: { curly: "error" } }"#),
        ),
        ("off/.eslintrc.json", entry),
        (
            "off/node_modules/eslint-plugin-local/package.json",
            manifest,
        ),
        (
            "off/node_modules/eslint-plugin-local/index.js",
            &local_plugin(r#"strict: { rules: { curly: "off" } }"#),
        ),
    ]);

    assert_stable(&dir, &[("on", Setting::On), ("off", Setting::Off)]);
}

#[test]
fn plugin_brace_presets_follow_their_own_extends() {
    let mut configs = vec![
        r#"base: { rules: { curly: "error" } }"#.to_owned(),
        r#"strict: { extends: ["plugin:local/base"], rules: {} }"#.to_owned(),
        r#"relative: { extends: ["./relative.json"] }"#.to_owned(),
        r#"looping: { extends: ["plugin:local/looping"] }"#.to_owned(),
        r#"c40: { rules: { curly: "off" } }"#.to_owned(),
    ];

    configs.extend((0..40).map(|level| {
        format!(
            r#"c{level}: {{ extends: ["plugin:local/c{}"] }}"#,
            level + 1
        )
    }));

    let index = local_plugin(&configs.join(", "));
    for (preset, expected) in [
        ("strict", Setting::On),
        ("relative", Setting::On),
        ("looping", Setting::Unknown),
        ("c20", Setting::Off),
        ("c0", Setting::Unknown),
    ] {
        let config = format!(r#"{{"root":true,"extends":["plugin:local/{preset}"]}}"#);
        let dir = dir_with(&[
            (".eslintrc.json", &config),
            (
                "node_modules/eslint-plugin-local/package.json",
                r#"{"main":"index.js"}"#,
            ),
            ("node_modules/eslint-plugin-local/index.js", &index),
            (
                "node_modules/eslint-plugin-local/relative.json",
                r#"{"rules":{"curly":"error"}}"#,
            ),
        ]);

        assert_eq!(settings(&dir.0), [expected], "{preset}");
    }
}

#[test]
fn symlinked_configs_retain_their_discovered_anchors() {
    let root = dir_with(&[
        (
            "shared/config.js",
            r#"export default [{ files: ["src/**"], rules: { curly: "error" } }];"#,
        ),
        ("first/src/a.ts", ""),
        ("second/src/a.ts", ""),
        ("second/lib/a.ts", ""),
    ]);

    for name in ["first", "second"] {
        symlink(
            root.join("shared/config.js"),
            root.join(&format!("{name}/eslint.config.js")),
        )
        .unwrap();
    }

    assert!(enforced(&root.join("first/src")));
    assert!(enforced(&root.join("second/src")));
    assert!(!enforced(&root.join("second/lib")));

    let biome = dir_with(&[
        (
            "shared/config.json",
            r#"{ "overrides": [{ "includes": ["pkg/src/**"], "linter": { "rules": { "style": { "useBlockStatements": "error" } } } }] }"#,
        ),
        (
            "app/pkg/biome.json",
            r#"{ "root": false, "extends": "//", "overrides": [{ "includes": ["other/**"], "linter": { "rules": { "style": { "useBlockStatements": "off" } } } }] }"#,
        ),
    ]);

    symlink(
        biome.join("shared/config.json"),
        biome.join("app/biome.json"),
    )
    .unwrap();
    assert!(enforced(&biome.join("app/pkg/src")));
}

#[test]
fn unsupported_glob_syntax_remains_uncertain_in_includes_and_exclusions() {
    for token in ["?", "[", "]", "(", ")", "+", "@", "!"] {
        for pattern in [
            format!("unrelated/{token}.ts"),
            format!("!unrelated/{token}.ts"),
        ] {
            let quoted = serde_json::to_string(&pattern).unwrap();
            let included = dir_with(&[(
                "eslint.config.js",
                &format!(
                    r#"export default [{{ files: [{quoted}], rules: {{ curly: "error" }} }}];"#
                ),
            )]);

            let excluded = dir_with(&[(
                "eslint.config.js",
                &format!(
                    r#"export default [{{ rules: {{ curly: "error" }} }}, {{ ignores: [{quoted}], rules: {{ curly: "off" }} }}];"#
                ),
            )]);

            assert!(enforced(&included.join("src")), "{pattern}");
            assert!(enforced(&excluded.join("src")), "{pattern}");
        }
    }
}

#[test]
fn uncertain_eslint_scopes() {
    for (pattern, dir) in [
        ("{src}/**", "src"),
        ("{a{b,c}}/**", "ab"),
        ("{}{a}", ""),
        ("src/{1..3}/**", "src/2"),
        ("\\src/**", "src"),
        ("src//**", "src"),
        ("lib/../src/**", "src"),
        ("src/{,generated}/**", "src"),
    ] {
        let quoted = serde_json::to_string(pattern).unwrap();
        for (name, config) in [
            (
                "eslint.config.js",
                format!(
                    r#"export default [{{ rules: {{ curly: "off" }} }}, {{ files: [{quoted}], rules: {{ curly: "error" }} }}];"#
                ),
            ),
            (
                "eslint.config.js",
                format!(
                    r#"export default [{{ rules: {{ curly: "error" }} }}, {{ files: [{quoted}], rules: {{ curly: "off" }} }}];"#
                ),
            ),
            (
                "eslint.config.js",
                format!(
                    r#"export default [{{ rules: {{ curly: "error" }} }}, {{ ignores: [{quoted}], rules: {{ curly: "off" }} }}];"#
                ),
            ),
            (
                ".eslintrc.json",
                format!(
                    r#"{{"root":true,"rules":{{"curly":"off"}},"overrides":[{{"files":[{quoted}],"rules":{{"curly":"error"}}}}]}}"#
                ),
            ),
            (
                ".eslintrc.json",
                format!(
                    r#"{{"root":true,"rules":{{"curly":"error"}},"overrides":[{{"files":[{quoted}],"rules":{{"curly":"off"}}}}]}}"#
                ),
            ),
            (
                ".eslintrc.json",
                format!(
                    r#"{{"root":true,"rules":{{"curly":"error"}},"overrides":[{{"files":["**/*.ts"],"excludedFiles":[{quoted}],"rules":{{"curly":"off"}}}}]}}"#
                ),
            ),
        ] {
            let root = dir_with(&[(name, &config)]);
            let at = if dir.is_empty() {
                root.0.clone()
            } else {
                root.join(dir)
            };

            assert!(enforced_for(&at, ".ts"), "{config}");
        }
    }

    let oxlint = dir_with(&[(
        ".oxlintrc.json",
        r#"{"rules":{"curly":"error"},"overrides":[{"files":["{src}/**"],"rules":{"curly":"off"}}]}"#,
    )]);

    assert!(!enforced_for(&oxlint.join("src"), ".ts"));
    assert!(enforced_for(&oxlint.join("lib"), ".ts"));
}

#[test]
fn negated_lint_scopes() {
    for (name, config, dir, extension) in [
        (
            ".oxlintrc.json",
            r#"{"overrides":[{"files":["!excluded/**"],"rules":{"curly":"error"}}]}"#,
            "src",
            ".ts",
        ),
        (
            ".oxlintrc.json",
            r#"{"rules":{"curly":"error"},"overrides":[{"files":["src/**"],"excludeFiles":["!src/deep/**"],"rules":{"curly":"off"}}]}"#,
            "src",
            ".ts",
        ),
        (
            ".oxlintrc.json",
            r#"{"overrides":[{"files":["*.ts","!src/**"],"rules":{"curly":"error"}}]}"#,
            "lib",
            ".js",
        ),
        (
            ".oxlintrc.json",
            r#"{"overrides":[{"files":["!**/*.test.ts"],"rules":{"curly":"error"}}]}"#,
            "src",
            ".ts",
        ),
        (
            ".eslintrc.json",
            r#"{"rules":{"curly":"error"},"overrides":[{"files":["src/**"],"excludedFiles":["!src/deep/**"],"rules":{"curly":"off"}}]}"#,
            "src",
            ".ts",
        ),
        (
            "eslint.config.js",
            r#"export default [{ files: ["!excluded/**"], rules: { curly: "error" } }];"#,
            "src",
            ".ts",
        ),
    ] {
        let scratch = dir_with(&[(name, config)]);
        assert!(enforced_for(&scratch.join(dir), extension), "{config}");
    }
}

#[test]
fn anchored_legacy_scopes() {
    for (name, root) in [
        (".oxlintrc.json", ""),
        (".eslintrc.json", r#""root":true,"#),
    ] {
        let anchored = dir_with(&[(
            name,
            &format!(
                r#"{{{root}"rules":{{"curly":"error"}},"overrides":[{{"files":["./*.ts"],"rules":{{"curly":"off"}}}}]}}"#
            ),
        )]);

        assert!(!enforced_for(&anchored.0, ".ts"), "{name}");
        assert!(enforced_for(&anchored.join("src"), ".ts"), "{name}");

        let base_name = dir_with(&[(
            name,
            &format!(
                r#"{{{root}"rules":{{"curly":"error"}},"overrides":[{{"files":["*.ts"],"rules":{{"curly":"off"}}}}]}}"#
            ),
        )]);

        assert!(!enforced_for(&base_name.join("src"), ".ts"), "{name}");
    }

    let biome = dir_with(&[(
        "biome.json",
        r#"{"overrides":[{"includes":["!excluded/**"],"linter":{"rules":{"style":{"useBlockStatements":"error"}}}}]}"#,
    )]);

    assert_eq!(biome_setting(&biome.join("src")), None);
}

#[test]
fn base_path_is_always_uncertain_and_language_does_not_scope_a_layer() {
    let root = dir_with(&[(
        "eslint.config.js",
        r#"export default [{ basePath: "src", files: ["other/**"], rules: { curly: "error" } }];"#,
    )]);

    let language = dir_with(&[(
        "eslint.config.js",
        r#"export default [{ rules: { curly: "error" } }, { language: "js/js", rules: { curly: "off" } }];"#,
    )]);

    assert!(enforced(&root.0));
    assert!(enforced(&root.join("src/other")));
    assert!(!enforced(&language.0));
}

#[test]
fn biome_double_slash_inherits_the_nearest_ancestor_at_its_own_anchor() {
    let root = dir_with(&[
        (
            "biome.json",
            r#"{ "linter": { "rules": { "style": { "useBlockStatements": "error" } } } }"#,
        ),
        (
            "workspace/biome.jsonc",
            r#"{ "root": false, "linter": { "rules": { "style": { "useBlockStatements": "off" } } }, "overrides": [{ "includes": ["pkg/src/**"], "linter": { "rules": { "style": { "useBlockStatements": "error" } } } }] }"#,
        ),
        (
            "workspace/pkg/biome.json",
            r#"{ "root": false, "extends": "//", "overrides": [{ "includes": ["other/**"], "linter": { "rules": { "style": { "useBlockStatements": "off" } } } }] }"#,
        ),
    ]);

    let overridden = dir_with(&[
        (
            "biome.json",
            r#"{ "linter": { "rules": { "style": { "useBlockStatements": "error" } } } }"#,
        ),
        (
            "pkg/biome.json",
            r#"{ "root": false, "extends": "//", "linter": { "rules": { "style": { "useBlockStatements": "off" } } } }"#,
        ),
    ]);

    assert!(enforced(&root.join("workspace/pkg/src")));
    assert!(!enforced(&root.join("workspace/pkg/lib")));
    assert!(!enforced(&overridden.join("pkg")));
}

#[test]
fn a_config_nested_in_an_unknown_call_argument_escapes() {
    for (call, expected) in [
        ("weaken({ config });", true),
        ("weaken([config]);", true),
        ("weaken({ ...config });", true),
        ("weaken({ inner: { config } });", true),
        ("weaken(() => config);", true),
        ("weaken(config);", true),
        ("weaken((config) => config);", false),
        ("weaken(function config() { return config; });", false),
        (
            "weaken(() => { const config = {}; return config; });",
            false,
        ),
        (
            "weaken(() => { if (true) { var config = {}; } return config; });",
            false,
        ),
        (
            "weaken(class config { method() { return config; } });",
            false,
        ),
    ] {
        let text = format!(
            r#"const config = {{ rules: {{ curly: "off" }} }}; {call} export default [config];"#
        );
        assert_eq!(
            enforced(&dir_with(&[("eslint.config.js", &text)]).0),
            expected,
            "{call}"
        );
    }

    let defined = dir_with(&[(
        "eslint.config.js",
        r#"import { defineConfig } from "eslint/config"; const config = { rules: { curly: "off" } }; export default defineConfig([config]);"#,
    )]);

    assert!(!enforced(&defined.0));
}

#[test]
fn typescript_config_references() {
    for (call, expected) in [
        ("f(class { constructor(private p = config) {} });", true),
        ("f(class { constructor(readonly p = config) {} });", true),
        (
            "f(class { constructor(private p = use(config)) { var config; } });",
            true,
        ),
        ("f(function (p = use(config)) { var config; });", true),
        ("f((p = use(config)) => { var config; });", true),
        ("f(class { m(@config p) {} });", true),
        ("f(class { m(config, @use(config) p) {} });", true),
        ("f(class { m(config, @use(config) p = 0) {} });", true),
        (
            "f(class { constructor(public config, @use(config) p = 0) {} });",
            true,
        ),
        (
            "f(() => { abstract class C { abstract [config](): T } });",
            true,
        ),
        (
            "f(() => { abstract class C { abstract [config]: T } });",
            true,
        ),
        (
            "f(() => { abstract class C { abstract accessor [config]: T } });",
            true,
        ),
        (
            "f(class { constructor(public config) { config; } });",
            false,
        ),
    ] {
        let text = format!(
            r#"const config = {{ rules: {{ curly: "off" }} }}; {call} export default [config];"#
        );
        assert_eq!(
            enforced(&dir_with(&[("eslint.config.ts", &text)]).0),
            expected,
            "{call}"
        );
    }
}

#[test]
fn only_imported_table_helpers_can_flatten_or_ignore_arguments() {
    for source in ["eslint/config", "eslint-define-config"] {
        let text = format!(
            r#"import {{ defineConfig as define }} from "{source}"; export default define({{ rules: {{ curly: "off" }} }});"#
        );

        assert!(
            !enforced(&dir_with(&[("eslint.config.js", &text)]).0),
            "{source}"
        );
    }

    for script in [
        r#"import { defineConfig } from "elsewhere"; export default defineConfig({});"#,
        "function defineConfig() { return []; } export default defineConfig({});",
        "export default defineConfig({});",
        r#"import { globalIgnores } from "elsewhere"; export default [globalIgnores([])];"#,
        "export default [globalIgnores([])];",
        r#"import tool from "elsewhere"; export default tool.config({});"#,
        "const tool = { config() { return []; } }; export default tool.config({});",
        r#"import { defineConfig } from "eslint/config"; export default defineConfig(makeConfig());"#,
    ] {
        assert!(
            enforced(&dir_with(&[("eslint.config.js", script)]).0),
            "{script}"
        );
    }

    for script in [
        r#"import { defineConfig, globalIgnores } from "eslint/config"; export default defineConfig(globalIgnores(["build/**"]), { rules: { curly: "off" } });"#,
        r#"import tseslint from "typescript-eslint"; export default tseslint.config({ rules: { curly: "off" } });"#,
        "const { defineConfig } = require(\"eslint/config\");\nmodule.exports = defineConfig([{ rules: { curly: \"off\" } }, { ignores: [\"dist/*\"] }]);",
        "import { includeIgnoreFile } from \"@eslint/compat\";\nimport tseslint from \"typescript-eslint\";\nexport default tseslint.config(includeIgnoreFile(\"/x/.gitignore\"), { rules: { curly: \"off\" } });",
    ] {
        assert!(
            !enforced(&dir_with(&[("eslint.config.js", script)]).0),
            "{script}"
        );
    }
}

#[test]
fn namespace_collisions_cycles_and_oversized_arrays_are_safe() {
    for expression in ["{ ...a, ...b }", r#"{ ...a, rules: { curly: "off" } }"#] {
        let root = dir_with(&[
            ("a.js", r#"export const rules = { curly: "error" };"#),
            ("b.js", r#"export const rules = { curly: "off" };"#),
            (
                "eslint.config.js",
                &format!(
                    r#"import * as a from "./a.js"; import * as b from "./b.js"; export default [{expression}];"#
                ),
            ),
        ]);

        assert!(!enforced(&root.0), "{expression}");
    }

    let empties = |count: usize| vec!["{}"; count].join(",");

    for script in [
        "const a = { rules: a.rules }; export default a;".to_owned(),
        "const a = { extends: [b] }; const b = { extends: [a] }; export default a;".to_owned(),
        "const a = b; const b = a; export default a;".to_owned(),
        "const level = { level }; export default [{ rules: { curly: level } }];".to_owned(),
        format!("export default [{}];", empties(10001)),
    ] {
        let root = dir_with(&[("eslint.config.js", &script)]);
        assert!(enforced(&root.0), "{}", &script[..script.len().min(80)]);
    }
}

#[test]
fn typescript_presets_are_arrays_and_recommended_js_rules_are_spreadable() {
    for script in [
        r#"import ts from "typescript-eslint"; export default [...ts.configs.recommended];"#,
        r#"import ts from "typescript-eslint"; export default ts.config({ extends: [...ts.configs.strict] });"#,
        r#"import ts from "typescript-eslint"; export default [{ extends: ts.configs.anything }];"#,
        r#"import js from "@eslint/js"; export default [{ rules: { ...js.configs.recommended.rules } }];"#,
        r#"import js from "@eslint/js"; import ts from "typescript-eslint"; import prettier from "eslint-config-prettier"; export default ts.config(js.configs.recommended, ...ts.configs.recommended, prettier);"#,
    ] {
        assert!(
            !enforced(&dir_with(&[("eslint.config.js", script)]).0),
            "{script}"
        );
    }
}

#[test]
fn scoped_config_objects_fold_across_many_directories() {
    let configs: Vec<String> = (0..60)
        .map(|index| {
            format!(
                r#"{{"files":["packages/p{index}/**/*.{{ts,tsx}}","apps/a{index}/src/**/*.{{js,jsx,mjs}}","**/*.test.{{ts,tsx}}"],"rules":{{"curly":"error"}}}}"#
            )
        })
        .collect();

    let root = dir_with(&[(
        "eslint.config.js",
        &format!("export default [{}];", configs.join(",")),
    )]);

    for index in 0..700 {
        let dir = root.join(&format!("packages/p{}/src/d{index}/e", index % 60));
        fs::create_dir_all(&dir).unwrap();
        assert!(enforced(&dir), "{dir}");
    }
}

#[test]
fn family_specific_rule_reads_work_without_redundant_rule_parameters() {
    for (curly, blocks, expected) in [
        ("off", "error", true),
        ("error", "off", true),
        ("off", "off", false),
    ] {
        let root = dir_with(&[
            (".eslintrc.yml", &format!("rules:\n  curly: {curly}\n")),
            (
                "biome.json",
                &format!(
                    r#"{{"linter":{{"rules":{{"style":{{"useBlockStatements":"{blocks}"}}}}}}}}"#
                ),
            ),
        ]);

        assert_eq!(enforced(&root.0), expected, "{curly} {blocks}");
    }
}

#[test]
fn package_entries_follow_import_conditions_in_listed_order() {
    for (exports, expected) in [
        (r#"{".":{"require":"./off.js","import":"./on.js"}}"#, true),
        (r#"{".":{"default":"./on.js","import":"./off.js"}}"#, true),
    ] {
        let root = dir_with(&[
            (
                "eslint.config.js",
                r#"import acme from "@acme/eslint-config"; export default [acme];"#,
            ),
            (
                "node_modules/@acme/eslint-config/package.json",
                &format!(r#"{{"name":"@acme/eslint-config","exports":{exports}}}"#),
            ),
            (
                "node_modules/@acme/eslint-config/on.js",
                r#"export default { rules: { curly: "error" } };"#,
            ),
            (
                "node_modules/@acme/eslint-config/off.js",
                r#"export default { rules: { curly: "off" } };"#,
            ),
        ]);

        assert_eq!(enforced(&root.0), expected, "{exports}");
    }
}

#[test]
fn a_config_passed_to_an_unknown_function_is_uncertain() {
    let root = dir_with(&[
        (
            "eslint.config.js",
            "import { strict } from \"./strict.js\";\nconst config = { rules: { curly: \"off\" } };\nstrict(config);\nexport default [config];",
        ),
        (
            "strict.js",
            r#"export function strict(config) { config.rules.curly = "error"; }"#,
        ),
    ]);

    assert!(enforced(&root.0));
}

#[test]
fn flat_files_globs_are_resolved_per_file_extension() {
    let root = dir_with(&[(
        "eslint.config.js",
        r#"export default [{ rules: { curly: "error" } }, { files: ["src/**/*.ts"], rules: { curly: "off" } }];"#,
    )]);

    let src = root.join("src");
    assert!(!enforced_for(&src, ".ts"));
    assert!(enforced_for(&src, ".js"));
    assert!(enforced(&src));
}

const DEEPEST_MANIFEST: usize = 127;
const CURLY_OFF: &str = r#""eslintConfig":{"rules":{"curly":"off"}}"#;
const PRETTIER_LISTED: &str = r#""devDependencies":{"prettier":"3"}"#;

fn nested_manifest(depth: usize, fields: &str) -> String {
    let arrays = depth - 1;
    format!(
        r#"{{{fields},"x":{}0{}}}"#,
        "[".repeat(arrays),
        "]".repeat(arrays)
    )
}

fn manifest_unread(dir: &Scratch) -> bool {
    decided(&dir.0, None)
        == [(
            Family::Legacy,
            Setting::Unknown,
            vec![dir.join("package.json")],
        )]
}

fn width_lists_prettier(manifest: &str) -> bool {
    let dir = dir_with(&[("package.json", manifest)]);
    super::width::manifest_formatters(&dir.0) == [super::width::Formatter::Prettier]
}

fn dependency_reads_off(manifest: &str) -> bool {
    let dir = dir_with(&[
        (
            "eslint.config.js",
            r#"import base from "deep-config"; export default base;"#,
        ),
        ("node_modules/deep-config/package.json", manifest),
        (
            "node_modules/deep-config/off.js",
            r#"export default [{ rules: { curly: "off" } }];"#,
        ),
    ]);

    !enforced(&dir.0)
}

#[test]
fn manifest_json_depth_reads_the_deepest_nesting_serde_json_accepts() {
    for (depth, read) in [(DEEPEST_MANIFEST, true), (DEEPEST_MANIFEST + 1, false)] {
        let project = dir_with(&[("package.json", &nested_manifest(depth, CURLY_OFF))]);
        assert_eq!(
            manifest_unread(&project),
            !read,
            "project manifest at {depth}"
        );
        assert_eq!(enforced(&project.0), !read, "project manifest at {depth}");

        let width = nested_manifest(depth, PRETTIER_LISTED);
        assert_eq!(width_lists_prettier(&width), read, "width list at {depth}");

        let entry = nested_manifest(depth, r#""main":"off.js""#);
        assert_eq!(
            dependency_reads_off(&entry),
            read,
            "dependency entry at {depth}"
        );
    }
}

#[test]
fn manifest_json_depth_strips_a_byte_order_mark_and_keeps_the_last_duplicate() {
    let project = dir_with(&[("package.json", &format!("\u{feff}{{{CURLY_OFF}}}"))]);

    assert!(!enforced(&project.0));
    assert!(width_lists_prettier(&format!(
        "\u{feff}{{{PRETTIER_LISTED}}}"
    )));
    assert!(dependency_reads_off("\u{feff}{\"main\":\"off.js\"}"));

    let duplicate = dir_with(&[(
        "package.json",
        &format!(r#"{{"eslintConfig":{{"rules":{{"curly":"error"}}}},{CURLY_OFF}}}"#),
    )]);

    assert!(!enforced(&duplicate.0));
    assert!(width_lists_prettier(&format!(
        r#"{{"devDependencies":{{"oxfmt":"1"}},{PRETTIER_LISTED}}}"#
    )));
    assert!(dependency_reads_off(
        r#"{"main":"missing.js","main":"off.js"}"#
    ));
}

#[test]
fn manifest_json_depth_leaves_a_manifest_serde_json_rejects_unread() {
    for field in [r#""x":1e400"#, r#""description":"\ud83d""#] {
        let project = dir_with(&[("package.json", &format!("{{{field},{CURLY_OFF}}}"))]);
        assert!(manifest_unread(&project), "{field}");
        assert!(enforced(&project.0), "{field}");
        assert!(
            !width_lists_prettier(&format!("{{{field},{PRETTIER_LISTED}}}")),
            "{field}"
        );
        assert!(
            !dependency_reads_off(&format!(r#"{{{field},"main":"off.js"}}"#)),
            "{field}"
        );
    }
}

#[test]
fn manifest_json_depth_stops_resolution_at_an_unread_dependency_manifest() {
    let off = r#"export default [{ rules: { curly: "off" } }];"#;
    let dir = dir_with(&[
        (
            "node_modules/deep-config/package.json",
            r#"{"main":"off.js"}"#,
        ),
        ("node_modules/deep-config/off.js", off),
        (
            "app/eslint.config.js",
            r#"import base from "deep-config"; export default base;"#,
        ),
        (
            "app/node_modules/deep-config/package.json",
            &nested_manifest(DEEPEST_MANIFEST + 1, r#""main":"off.js""#),
        ),
        ("app/node_modules/deep-config/off.js", off),
    ]);

    assert!(enforced(&dir.join("app")));
}

fn linked_source(links: usize, link: fn(usize) -> String) -> String {
    let chain: String = (1..=links)
        .map(|k| format!("const x{k} = {};\n", link(k - 1)))
        .collect();

    format!("const x0 = {{ rules: {{ curly: \"off\" }} }};\n{chain}export default [x{links}];\n")
}

fn spread_link(previous: usize) -> String {
    format!("{{ ...x{previous} }}")
}

fn alias_link(previous: usize) -> String {
    format!("x{previous}")
}

fn flat_settings_on_the_smallest_stack(dirs: Vec<String>) -> Vec<Setting> {
    std::thread::Builder::new()
        .stack_size(crate::pool::STACK_LADDER[2])
        .spawn(move || {
            dirs.iter()
                .map(|dir| match decided(dir, None).as_slice() {
                    [(Family::Flat, setting, _)] => *setting,
                    other => panic!("expected one flat decision, got {other:?}"),
                })
                .collect()
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn config_value_depth_reads_a_thousand_spread_or_alias_links() {
    for link in [spread_link, alias_link] {
        let dir = dir_with(&[("eslint.config.js", &linked_source(1_000, link))]);
        assert_eq!(
            flat_settings_on_the_smallest_stack(vec![dir.0.clone()]),
            [Setting::Off]
        );
    }
}

#[test]
fn config_value_depth_past_a_thousand_links_is_unknown() {
    for link in [spread_link, alias_link] {
        let dir = dir_with(&[("eslint.config.js", &linked_source(1_001, link))]);
        assert_eq!(
            flat_settings_on_the_smallest_stack(vec![dir.0.clone()]),
            [Setting::Unknown]
        );
    }
}

#[test]
fn config_value_depth_cut_through_one_config_leaves_the_config_it_imports_alone() {
    let links: String = (1..=500)
        .map(|k| format!("const y{k} = {{ ...y{} }};\n", k - 1))
        .collect();

    let root = dir_with(&[
        ("b/eslint.config.js", &linked_source(1_000, spread_link)),
        (
            "a/eslint.config.js",
            &format!(
                "import b from \"../b/eslint.config.js\";\nconst y0 = b[0];\n{links}export default [y500];\n"
            ),
        ),
    ]);

    assert_eq!(
        flat_settings_on_the_smallest_stack(vec![root.join("a"), root.join("b")]),
        [Setting::Unknown, Setting::Off]
    );
}

#[test]
fn config_value_depth_counts_inline_spreads_across_bindings() {
    let wrapped =
        |previous: usize| format!("{}x{previous}{}", "{ ...".repeat(2_000), " }".repeat(2_000));

    let four = dir_with(&[("eslint.config.js", &linked_source(4, wrapped))]);
    let six = dir_with(&[("eslint.config.js", &linked_source(6, wrapped))]);
    assert_eq!(
        flat_settings_on_the_smallest_stack(vec![four.0.clone(), six.0.clone()]),
        [Setting::Off, Setting::Unknown]
    );
}

#[test]
fn config_value_depth_leaves_long_wrappers_and_member_chains_on_one_expression_alone() {
    let base = "const x0 = { rules: { curly: \"off\" }, x0 };\n";
    let configs = [
        format!("{base}export default [x0{}];\n", " as any".repeat(20_000)),
        format!("{base}export default [x0{}];\n", "!".repeat(20_000)),
        format!("{base}export default [x0{}];\n", ".x0".repeat(20_000)),
        format!(
            "import {{ defineConfig as d }} from \"oxlint\";\n{base}export default [{}x0{}];\n",
            "d(".repeat(5_000),
            ").x0".repeat(5_000)
        ),
    ];

    let dirs: Vec<Scratch> = configs
        .iter()
        .map(|config| dir_with(&[("eslint.config.ts", config)]))
        .collect();

    assert_eq!(
        flat_settings_on_the_smallest_stack(dirs.iter().map(|dir| dir.0.clone()).collect()),
        [Setting::Off; 4]
    );
}

fn biome_setting(dir: &str) -> Option<Setting> {
    decided(dir, Some(".ts"))
        .into_iter()
        .find(|decision| decision.0 == Family::Biome)
        .map(|decision| decision.1)
}

#[test]
fn config_assignment_targets_make_the_export_unknown() {
    for (name, write) in [
        ("eslint.config.js", r#"[config.rules.curly] = ["error"];"#),
        (
            "eslint.config.js",
            r#"[[config.rules.curly]] = [["error"]];"#,
        ),
        (
            "eslint.config.js",
            r#"({ rules: { curly: config.rules.curly } } = { rules: { curly: "error" } });"#,
        ),
        (
            "eslint.config.js",
            r#"({ curly: config.rules.curly = "error" } = {});"#,
        ),
        (
            "eslint.config.js",
            r#"[config.rules.curly = "error"] = [];"#,
        ),
        ("eslint.config.js", r#"[, ...config.rules.extra] = [1, 2];"#),
        (
            "eslint.config.js",
            r#"({ ...config.rules } = { curly: "error" });"#,
        ),
        (
            "eslint.config.js",
            r#"for (config.rules.curly in { error: 1 });"#,
        ),
        (
            "eslint.config.js",
            r#"for (config.rules.curly of ["error"]);"#,
        ),
        (
            "eslint.config.js",
            r#"for ([config.rules.curly] of [["error"]]);"#,
        ),
        (
            "eslint.config.js",
            r#"for ({ curly: config.rules.curly } of [{ curly: "error" }]);"#,
        ),
        ("eslint.config.ts", r#"[config.rules.curly!] = ["error"];"#),
        ("eslint.config.ts", r#"config.rules.curly! = "error";"#),
        (
            "eslint.config.ts",
            r#"[(config.rules.curly as string)] = ["error"];"#,
        ),
    ] {
        let dir = dir_with(&[(
            name,
            &format!(
                r#"const config = {{ rules: {{ curly: "off" }} }}; {write} export default [config];"#
            ),
        )]);

        assert_eq!(
            decided(&dir.0, Some(".ts")),
            [(Family::Flat, Setting::Unknown, vec![dir.join(name)])],
            "{write}"
        );
    }

    for write in [
        "({ rules: config.rules } = strict);",
        "for (config.rules of [strict.rules]);",
    ] {
        let dir = dir_with(&[
            (
                "eslint.config.js",
                &format!(
                    r#"import strict from "./strict.js"; const config = {{ rules: {{}} }}; {write} export default [config];"#
                ),
            ),
            (
                "strict.js",
                r#"export default { rules: { curly: "error" } };"#,
            ),
        ]);

        assert_eq!(
            decided(&dir.0, Some(".ts")),
            [(
                Family::Flat,
                Setting::Unknown,
                vec![dir.join("eslint.config.js")]
            )],
            "{write}"
        );
    }
}

#[test]
fn unrelated_global_writes() {
    let off = r#"export default [{ rules: { curly: "off" } }];"#;
    let strict = r#"globalThis.strict = { rules: { curly: "error" } };"#;
    let tweak = r#"globalThis.tweak = () => { globalThis.x.rules.curly = "error"; };"#;
    let setter = r#"Object.defineProperty(globalThis, "DEBUG", { set() { module.parent.exports.rules.curly = "error"; } });"#;
    let setter2 = r#"Object.defineProperty(globalThis, "DEBUG", { set() { require("./.eslintrc.cjs").rules.curly = "error"; } });"#;
    let setup = r#"globalThis.DEBUG = { valueOf() { module.parent.exports.rules.curly = "error"; return 0; } };"#;
    let base = r#"exports.config = { rules: { curly: "off" } }; globalThis.DEBUG = this.config.rules.__defineGetter__("curly", () => "error");"#;
    let cases = [
        ("eslint.config.js", format!(r#"process.env.FOO ??= "1"; {off}"#), Setting::Off),
        ("eslint.config.js", format!("globalThis.DEBUG = true; {off}"), Setting::Off),
        (
            "eslint.config.js",
            format!(r#"import path from "node:path"; process.env.FOO ??= "1"; {off}"#),
            Setting::Off,
        ),
        (
            "eslint.config.ts",
            format!(r#"import type {{ Linter }} from "./types.js"; process.env.FOO ??= "1"; {off}"#),
            Setting::Off,
        ),
        (
            "eslint.config.js",
            r#"const c = { rules: { curly: "off" } }; globalThis.x = c; x.rules.curly = "error"; export default [c];"#.to_owned(),
            Setting::Unknown,
        ),
        (
            "eslint.config.js",
            r#"const e = process.env; e.FOO = "1"; export default [{ rules: { curly: e.FOO } }];"#.to_owned(),
            Setting::Unknown,
        ),
        (
            "eslint.config.js",
            r#"const process = { rules: { curly: "off" } }; process.rules.curly = "error"; export default [process];"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"module.exports = { rules: { curly: "off" } }; module.exports.rules.curly = "error";"#.to_owned(),
            Setting::Unknown,
        ),
        (
            "eslint.config.js",
            r#"import "./strict.js"; const c = { rules: { curly: "off" } }; globalThis.x = c; x.rules = globalThis.strict.rules; export default [c];"#.to_owned(),
            Setting::Unknown,
        ),
        (
            "eslint.config.js",
            r#"import "./strict.js"; const c = { rules: { curly: "off" } }; function get() { return c; } globalThis.x = get(); x.rules = globalThis.strict.rules; export default [c];"#.to_owned(),
            Setting::Unknown,
        ),
        (
            "eslint.config.js",
            r#"import "./strict.js"; const process = { rules: { curly: "off" } }; process.rules = globalThis.strict.rules; export default [process];"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"require("./strict.js"); module.exports = { rules: { curly: "off" } }; module.exports.rules = globalThis.strict.rules;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"require("./strict.js"); module.exports = { rules: { curly: "off" } }; arguments[2].exports.rules = globalThis.strict.rules;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"require("./strict.js"); module.exports = { rules: { curly: "off" } }; require.cache[__filename].exports.rules = globalThis.strict.rules;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"require("./strict.js"); module.exports = { rules: { curly: "off" } }; globalThis.m = module; m.exports.rules = globalThis.strict.rules;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"require("./strict.js"); module.exports = { rules: { curly: "off" } }; globalThis.a = arguments; a[2].exports.rules = globalThis.strict.rules;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"require("./strict.js"); module.exports = { rules: { curly: "off" } }; globalThis.r = require; r.cache[__filename].exports.rules = globalThis.strict.rules;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"require("./strict.js"); const c = { rules: { curly: "off" } }; globalThis.x = eval("c"); x.rules = globalThis.strict.rules; module.exports = c;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"require("./strict.js"); const c = { rules: { curly: "off" } }; with (c) { rules.curly = globalThis.strict.rules.curly; } module.exports = c;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"module.exports = { rules: { curly: "off" } }; process.mainModule.constructor._cache[__filename].exports.rules.curly = "error";"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"const c = { global: { curly: "off" } }; with (c) { global.curly = "error"; } module.exports = { rules: c.global };"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"const global = {}; global.extends = "eslint:all"; module.exports = global;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            "eslint.config.js",
            r#"import "./tweak.js"; const c = { rules: { curly: "off" } }; globalThis.x = c; globalThis.tweak(); export default [c];"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"const config = {}; global = config; global.extends = "eslint:all"; module.exports = config;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"const { config } = require("./base.cjs"); module.exports = config;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"module.exports = { rules: { curly: "off" } }; globalThis.DEBUG = (() => {}).constructor("process.getBuiltinModule('module')._cache[process.cwd() + '/.eslintrc.cjs'].exports.rules.curly = 'error'")();"#.to_owned(),
            Setting::Unknown,
        ),
        (
            "eslint.config.ts",
            r#"const config = { rules: { curly: "off" } }; enum Local { global = config.rules as any, change = (global["curly"] = "error") as any } export default [config];"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"const config = { rules: { curly: "off" } }; eval("var global = config.rules"); global.curly = "error"; module.exports = config;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"require("./setup.cjs"); module.exports = { rules: { curly: "off" } }; globalThis.DEBUG += 1;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"require("./setter.cjs"); module.exports = { rules: { curly: "off" } }; globalThis.DEBUG = true;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            ".eslintrc.cjs",
            r#"process.mainModule.require(__dirname + "/setter2.cjs"); module.exports = { rules: { curly: "off" } }; globalThis.DEBUG = true;"#.to_owned(),
            Setting::Unknown,
        ),
        (
            "eslint.config.mjs",
            r#"import shared from "./shared.mjs"; import "./hook.mjs"; globalThis.DEBUG = true; export default [shared];"#.to_owned(),
            Setting::Unknown,
        ),
    ];

    for (name, config, setting) in cases {
        let dir = dir_with(&[
            (name, &config),
            ("strict.js", strict),
            ("tweak.js", tweak),
            ("base.cjs", base),
            ("setup.cjs", setup),
            ("setter.cjs", setter),
            ("setter2.cjs", setter2),
            (
                "shared.mjs",
                r#"export default { rules: { curly: "off" } };"#,
            ),
            (
                "hook.mjs",
                r#"import shared from "./shared.mjs"; Object.defineProperty(globalThis, "DEBUG", { set() { shared.rules.curly = "error"; } });"#,
            ),
        ]);
        let family = if name == ".eslintrc.cjs" {
            Family::Legacy
        } else {
            Family::Flat
        };

        assert_eq!(
            decided(&dir.0, Some(".ts")),
            [(family, setting, vec![dir.join(name)])],
            "{config}"
        );
    }
    let hooked = [
        (
            "eslint.config.mjs",
            r#"import shared from "shared-pkg"; import "file://{dir}/hook.mjs"; globalThis.DEBUG = true; export default [shared];"#,
        ),
        (
            "eslint.config.mjs",
            r#"import shared from "shared-pkg"; await import("./hook.mjs"); globalThis.DEBUG = true; export default [shared];"#,
        ),
        (
            "eslint.config.mjs",
            r#"export { marker } from "./marker.mjs"; export const shared = { rules: { curly: "off" } }; globalThis.DEBUG = true; export default [shared];"#,
        ),
        (
            "eslint.config.mjs",
            r#"import shared from "shared-pkg"; import { createRequire } from "node:module"; createRequire(import.meta.url)("./hook.cjs"); globalThis.DEBUG = true; export default [shared];"#,
        ),
        (
            "eslint.config.cjs",
            r#"const shared = require("shared-pkg"); const load = require; load("./hook.cjs"); globalThis.DEBUG = true; module.exports = [shared];"#,
        ),
        (
            "eslint.config.cjs",
            r#"const shared = require("shared-pkg"); process.getBuiltinModule("module").createRequire(__filename)("./hook.cjs"); globalThis.DEBUG = true; module.exports = [shared];"#,
        ),
        (
            "eslint.config.cjs",
            r#"const shared = require("shared-pkg"); (() => {}).constructor("process.mainModule.require(process.cwd() + '/hook.cjs')")(); globalThis.DEBUG = true; module.exports = [shared];"#,
        ),
        (
            "eslint.config.cjs",
            r#"const shared = require("shared-pkg"); Function("process.mainModule.require(process.cwd() + '/hook.cjs')")(); globalThis.DEBUG = true; module.exports = [shared];"#,
        ),
        (
            "eslint.config.cjs",
            r#"const shared = require("shared-pkg"); require("node:vm").runInThisContext("process.mainModule.require(process.cwd() + '/hook.cjs')"); globalThis.DEBUG = true; module.exports = [shared];"#,
        ),
        (
            "eslint.config.mjs",
            r#"export * from "./marker.mjs"; export const shared = { rules: { curly: "off" } }; globalThis.DEBUG = true; export default [shared];"#,
        ),
        (
            "eslint.config.cjs",
            r#"const shared = require("shared-pkg"); const k = "constructor"; (() => {})[k]("process.mainModule.require(process.cwd() + '/hook.cjs')")(); globalThis.DEBUG = true; module.exports = [shared];"#,
        ),
    ];

    for (name, config) in hooked {
        let dir = dir_with(&[
            (
                "node_modules/shared-pkg/package.json",
                r#"{"name":"shared-pkg","main":"index.js"}"#,
            ),
            (
                "node_modules/shared-pkg/index.js",
                r#"module.exports = { rules: { curly: "off" } };"#,
            ),
            (
                "hook.mjs",
                r#"import shared from "shared-pkg"; Object.defineProperty(globalThis, "DEBUG", { set() { shared.rules.curly = "error"; } });"#,
            ),
            (
                "hook.cjs",
                r#"const shared = require("shared-pkg"); Object.defineProperty(globalThis, "DEBUG", { set() { shared.rules.curly = "error"; } });"#,
            ),
            (
                "marker.mjs",
                r#"import { shared } from "./eslint.config.mjs"; export const marker = 1; Object.defineProperty(globalThis, "DEBUG", { set() { shared.rules.curly = "error"; } });"#,
            ),
        ]);

        dir.write(name, &config.replace("{dir}", &dir.0));
        assert_eq!(
            decided(&dir.0, Some(".ts")),
            [(Family::Flat, Setting::Unknown, vec![dir.join(name)])],
            "{config}"
        );
    }
}

#[test]
fn biome_preset_braces() {
    let on = Some(Setting::On);
    let off = Some(Setting::Off);
    let all_files = |rules: &str| {
        format!(
            r#"{{ "overrides": [{{ "includes": ["**"], "linter": {{ "rules": {rules} }} }}] }}"#
        )
    };

    let inherited_style_all = all_files(r#"{ "style": { "preset": "all" } }"#);
    let inherited_style_error = all_files(r#"{ "style": "error" }"#);
    let inherited_rule_error = all_files(r#"{ "style": { "useBlockStatements": "error" } }"#);
    let style_error = r#"{ "linter": { "rules": { "style": "error" } } }"#;
    let style_all = r#"{ "linter": { "rules": { "style": { "preset": "all" } } } }"#;
    let root_all = r#"{ "linter": { "rules": { "preset": "all" } } }"#;
    for (files, expected) in [
        (vec![("biome.json", root_all)], on),
        (vec![("biome.json", style_all)], on),
        (
            vec![(
                "biome.json",
                r#"{ "linter": { "rules": { "preset": "none", "style": { "preset": "all" } } } }"#,
            )],
            on,
        ),
        (
            vec![(
                "biome.json",
                r#"{ "linter": { "rules": { "all": true } } }"#,
            )],
            on,
        ),
        (
            vec![(
                "biome.json",
                r#"{ "linter": { "rules": { "preset": "all", "style": "off" } } }"#,
            )],
            off,
        ),
        (
            vec![(
                "biome.json",
                r#"{ "linter": { "rules": { "preset": "all", "style": { "preset": "none" } } } }"#,
            )],
            off,
        ),
        (
            vec![(
                "biome.json",
                r#"{ "linter": { "rules": { "preset": "all", "style": { "useBlockStatements": "off" } } } }"#,
            )],
            off,
        ),
        (
            vec![(
                "biome.json",
                r#"{ "linter": { "rules": { "preset": "recommended" } } }"#,
            )],
            off,
        ),
        (
            vec![
                (
                    "biome.json",
                    r#"{ "extends": ["./base.json"], "linter": { "rules": { "preset": "none" } } }"#,
                ),
                ("base.json", style_all),
            ],
            on,
        ),
        (
            vec![
                (
                    "biome.json",
                    r#"{ "extends": ["./base.json"], "linter": { "rules": { "style": { "preset": "none" } } } }"#,
                ),
                ("base.json", style_error),
            ],
            on,
        ),
        (
            vec![
                (
                    "biome.json",
                    r#"{ "extends": ["./base.json"], "linter": { "rules": { "preset": "recommended" } } }"#,
                ),
                ("base.json", root_all),
            ],
            off,
        ),
        (
            vec![(
                "biome.json",
                r#"{ "linter": { "rules": { "style": { "useBlockStatements": "error" } } }, "overrides": [{ "includes": ["**"], "linter": { "rules": { "style": "off" } } }] }"#,
            )],
            off,
        ),
        (
            vec![(
                "biome.json",
                r#"{ "linter": { "rules": { "style": "off" } }, "overrides": [{ "includes": ["**"], "linter": { "rules": { "preset": "all" } } }] }"#,
            )],
            off,
        ),
        (
            vec![
                (
                    "biome.json",
                    r#"{ "extends": ["./base.json"], "linter": { "rules": { "style": { "preset": "none" } } } }"#,
                ),
                ("base.json", inherited_style_all.as_str()),
            ],
            on,
        ),
        (
            vec![
                (
                    "biome.json",
                    r#"{ "extends": ["./base.json"], "linter": { "rules": { "style": "off" } } }"#,
                ),
                ("base.json", inherited_style_error.as_str()),
            ],
            on,
        ),
        (
            vec![
                (
                    "biome.json",
                    r#"{ "extends": ["./base.json"], "linter": { "rules": { "style": { "useBlockStatements": "off" } } } }"#,
                ),
                ("base.json", inherited_rule_error.as_str()),
            ],
            on,
        ),
        (
            vec![(
                "biome.json",
                r#"{ "extends": ["./missing.json"], "linter": { "rules": { "style": "off" } } }"#,
            )],
            Some(Setting::Unknown),
        ),
    ] {
        let dir = dir_with(&files);
        assert_eq!(biome_setting(&dir.0), expected, "{files:?}");
    }

    let scoped = dir_with(&[(
        "biome.json",
        r#"{ "overrides": [{ "includes": ["src/**"], "linter": { "rules": { "preset": "all" } } }] }"#,
    )]);

    assert_eq!(biome_setting(&scoped.join("src")), on);
    assert_eq!(biome_setting(&scoped.0), None);
}

#[test]
fn hidden_biome_braces() {
    let enforcing = r#"{ "linter": { "rules": { "style": { "useBlockStatements": "error" } } } }"#;
    let commented = format!("// hidden\n{enforcing}");
    for (name, text) in [(".biome.json", enforcing), (".biome.jsonc", &commented)] {
        let here = dir_with(&[(name, text)]);
        let ancestor = dir_with(&[(name, text), ("src/a.ts", "")]);
        let extended = dir_with(&[
            (name, text),
            ("sub/biome.json", r#"{ "root": false, "extends": "//" }"#),
        ]);

        let beside = dir_with(&[("biome.json", "{}"), (name, text)]);
        for dir in [
            here.0.clone(),
            ancestor.join("src"),
            extended.join("sub"),
            beside.0.clone(),
        ] {
            assert_eq!(biome_setting(&dir), Some(Setting::On), "{name} {dir}");
            assert!(enforced_for(&dir, ".ts"), "{name} {dir}");
        }
    }
}

#[test]
fn biome_rule_positions() {
    let misplaced = [
        r#"{ "style": { "useBlockStatements": "off" }, "linter": { "rules": { "style": "error" } } }"#,
        r#"{ "useBlockStatements": "error" }"#,
        r#"{ "javascript": { "useBlockStatements": "error" } }"#,
        r#"{ "rules": { "style": "error" } }"#,
        r#"{ "rules": { "all": true } }"#,
        r#"{ "linter": { "rules": { "useBlockStatements": "error" } } }"#,
        r#"{ "linter": { "rules": { "suspicious": { "useBlockStatements": "error" } } } }"#,
        r#"{ "linter": { "rules": { "style": { "rules": { "all": true } } } } }"#,
        r#"{ "style": { "all": true } }"#,
        r#"{ "style": { "preset": "all" } }"#,
        r#"{ "linter": { "style": { "preset": "all" } } }"#,
        r#"{ "linter": { "style": "error" } }"#,
        r#"{ "javascript": { "style": { "all": true } } }"#,
    ];

    for text in misplaced {
        let dir = dir_with(&[("biome.json", text)]);
        assert_eq!(biome_setting(&dir.0), Some(Setting::Unknown), "{text}");

        let members = &text[2..text.len() - 2];
        let override_text =
            format!(r#"{{ "overrides": [{{ "includes": ["src/**"], {members} }}] }}"#);

        let scoped = dir_with(&[("biome.json", override_text.as_str())]);

        assert_eq!(
            biome_setting(&scoped.join("src")),
            Some(Setting::Unknown),
            "{text}"
        );

        assert_eq!(biome_setting(&scoped.join("other")), None, "{text}");
    }

    for (level, expected) in [("error", Setting::On), ("off", Setting::Off)] {
        let text = format!(
            r#"{{ "linter": {{ "rules": {{ "style": {{ "useBlockStatements": "{level}" }} }} }} }}"#
        );
        let dir = dir_with(&[("biome.json", text.as_str())]);
        assert_eq!(biome_setting(&dir.0), Some(expected), "{text}");
    }
}

#[test]
fn member_mutations_make_the_flat_decision_unknown() {
    for mutation in [
        r#"const { rules } = config; rules.curly = "error";"#,
        r#"for (const item of [config]) item.rules.curly = "error";"#,
        r#"function change(item) { item.rules.curly = "error"; }"#,
        r#"const list = [config]; list[0].rules.curly = "error";"#,
        "config.rules.curly++;",
        "delete config.rules.curly;",
        "Object.assign(config.rules, strict);",
        r#"Object.defineProperty(config.rules, "curly", { value: "error" });"#,
        r#"Object.defineProperties(config.rules, { curly: { value: "error" } });"#,
        "Object.setPrototypeOf(config.rules, strict);",
        r#"Reflect.set(config.rules, "curly", "error");"#,
        r#"Reflect.get(config.rules, "curly");"#,
    ] {
        let dir = dir_with(&[
            ("strict.js", r#"export default { curly: "error" };"#),
            (
                "eslint.config.js",
                &format!(
                    r#"import strict from "./strict.js"; const config = {{ rules: {{ curly: "off" }} }}; const clean = {{ rules: {{ curly: "off" }} }}; {mutation} export {{ config, clean }}; export default clean;"#
                ),
            ),
        ]);

        assert_eq!(
            decided(&dir.0, None),
            [(
                Family::Flat,
                Setting::Unknown,
                vec![dir.join("eslint.config.js")]
            )],
            "{mutation}"
        );
    }
}

#[test]
fn legacy_export_writes_after_module_exports_decide_the_legacy_setting() {
    for (suffix, setting) in [
        ("try { module.exports = {}; } catch {}", Setting::Unknown),
        ("if (true) module.exports = {};", Setting::Unknown),
        ("{ module.exports = {}; }", Setting::Unknown),
        (
            "function change() { module.exports = {}; }",
            Setting::Unknown,
        ),
        ("module.exports = {};", Setting::Unknown),
        ("try { exports.extra = {}; } catch {}", Setting::Unknown),
        ("exports.extra = {}; exports.extra = {};", Setting::Unknown),
        ("exports.extra = {};", Setting::Off),
    ] {
        let dir = dir_with(&[(
            ".eslintrc.cjs",
            &format!(r#"module.exports = {{ rules: {{ curly: "off" }} }}; {suffix}"#),
        )]);

        assert_eq!(
            decided(&dir.0, None),
            [(Family::Legacy, setting, vec![dir.join(".eslintrc.cjs")])],
            "{suffix}"
        );
    }
}

#[test]
fn a_push_through_an_alias_makes_the_flat_decision_unknown() {
    let dir = dir_with(&[(
        "eslint.config.js",
        "const config = []; const alias = config; alias.push({}); export default config;",
    )]);

    assert_eq!(
        decided(&dir.0, None),
        [(
            Family::Flat,
            Setting::Unknown,
            vec![dir.join("eslint.config.js")]
        )]
    );
}

#[test]
fn wrapped_config_receivers_make_the_flat_decision_unknown() {
    let strict = r#"export default { rules: { curly: "error" } };"#;
    let cases = [
        (
            "eslint.config.ts",
            "(config satisfies object[]).push(strict);",
            Setting::Unknown,
        ),
        (
            "eslint.config.ts",
            "config!.push(strict);",
            Setting::Unknown,
        ),
        (
            "eslint.config.ts",
            "(config).push(strict);",
            Setting::Unknown,
        ),
        (
            "eslint.config.ts",
            "(config as any[]).push(strict);",
            Setting::Unknown,
        ),
        (
            "eslint.config.ts",
            "(<any[]>config).push(strict);",
            Setting::Unknown,
        ),
        (
            "eslint.config.ts",
            "(config.push as any)(strict);",
            Setting::Unknown,
        ),
        (
            "eslint.config.mjs",
            "(config.push)(strict);",
            Setting::Unknown,
        ),
        (
            "eslint.config.ts",
            "const typed = config as any[]; typed.push(strict);",
            Setting::Unknown,
        ),
        (
            "eslint.config.mjs",
            "const add = (s, c) => c.push(strict); add`${config}`;",
            Setting::Unknown,
        ),
        ("eslint.config.mjs", "String.raw`a`;", Setting::Off),
    ];

    for (name, route, setting) in cases {
        let script = format!(
            r#"import strict from "./strict.mjs"; const config = [{{ rules: {{ curly: "off" }} }}]; {route} export default config;"#
        );

        let dir = dir_with(&[("strict.mjs", strict), (name, &script)]);
        assert_eq!(
            decided(&dir.0, None),
            [(Family::Flat, setting, vec![dir.join(name)])],
            "{route}"
        );
    }

    let dir = dir_with(&[(
        "eslint.config.mjs",
        r#"String.raw`a`; export default [{ rules: { curly: "off" } }];"#,
    )]);

    assert_eq!(
        decided(&dir.0, None),
        [(
            Family::Flat,
            Setting::Off,
            vec![dir.join("eslint.config.mjs")]
        )]
    );
}

#[test]
fn self_references_and_cycles_decide_unknown_on_every_query() {
    for script in [
        "const a = { rules: a.rules }; export default a;",
        "const a = { extends: [b] }; const b = { extends: [a] }; export default a;",
        "const a = b; const b = a; export default a;",
        "const level = { level }; export default [{ rules: { curly: level } }];",
    ] {
        let dir = dir_with(&[("eslint.config.js", script)]);
        let expected = [(
            Family::Flat,
            Setting::Unknown,
            vec![dir.join("eslint.config.js")],
        )];

        assert_eq!(decided(&dir.0, None), expected, "{script}");
        assert_eq!(decided(&dir.0, None), expected, "{script}");
    }
}

#[test]
fn an_esm_import_reads_the_import_entry_not_the_require_entry() {
    let dir = dir_with(&[
        (
            "eslint.config.js",
            r#"import acme from "@acme/eslint-config"; export default [acme];"#,
        ),
        (
            "node_modules/@acme/eslint-config/package.json",
            r#"{ "name": "@acme/eslint-config", "exports": { ".": { "require": "./off.cjs", "import": "./on.js" } } }"#,
        ),
        (
            "node_modules/@acme/eslint-config/off.cjs",
            r#"module.exports = { rules: { curly: "off" } };"#,
        ),
        (
            "node_modules/@acme/eslint-config/on.js",
            r#"export default { rules: { curly: "error" } };"#,
        ),
    ]);

    assert_eq!(
        decided(&dir.0, None),
        [(
            Family::Flat,
            Setting::On,
            vec![dir.join("eslint.config.js")]
        )]
    );
}

#[test]
fn require_resolution_order() {
    const OFF: &str = r#"{ "rules": { "curly": "off" } }"#;
    const ON: &str = r#"{ "rules": { "curly": "error" } }"#;
    const OFF_JS: &str = r#"module.exports = { rules: { curly: "off" } };"#;
    const ON_JS: &str = r#"module.exports = { rules: { curly: "error" } };"#;
    const LEGACY: (&str, &str) = (
        ".eslintrc.json",
        r#"{ "root": true, "extends": ["./base"] }"#,
    );
    const LEGACY_DIR: (&str, &str) = (
        ".eslintrc.json",
        r#"{ "root": true, "extends": ["./dir"] }"#,
    );

    let setting = |files: &[(&str, &str)]| {
        let dir = dir_with(files);
        let decisions = decided(&dir.0, Some(".ts"));
        assert_eq!(decisions.len(), 1, "{files:?}");
        decisions[0].1
    };

    for (files, expected) in [
        (
            vec![LEGACY, ("base", OFF), ("base.js", ON_JS)],
            Setting::Off,
        ),
        (
            vec![LEGACY, ("base.js", OFF_JS), ("base.json", ON)],
            Setting::Off,
        ),
        (
            vec![LEGACY, ("base.node", ""), ("base/index.js", OFF_JS)],
            Setting::Unknown,
        ),
        (
            vec![LEGACY_DIR, ("dir/index.js", OFF_JS), ("dir/index.json", ON)],
            Setting::Off,
        ),
        (vec![LEGACY_DIR, ("dir/index.json", OFF)], Setting::Off),
        (
            vec![
                LEGACY_DIR,
                ("dir/package.json", r#"{ "main": "main.js" }"#),
                ("dir/main.js", OFF_JS),
                ("dir/index.js", ON_JS),
            ],
            Setting::Off,
        ),
    ] {
        assert_eq!(setting(&files), expected, "{files:?}");
    }

    let plugin = local_plugin(r#"strict: { rules: { curly: "off" } }"#);
    let preset = [
        (
            ".eslintrc.json",
            r#"{ "root": true, "extends": ["plugin:local/strict"] }"#,
        ),
        (
            "node_modules/eslint-plugin-local/package.json",
            r#"{ "main": "lib" }"#,
        ),
        ("node_modules/eslint-plugin-local/lib.js", &plugin),
        (
            "node_modules/eslint-plugin-local/lib.json",
            r#"{ "configs": { "strict": { "rules": { "curly": "error" } } } }"#,
        ),
    ];

    assert_eq!(setting(&preset), Setting::Off, "{preset:?}");

    for (specifier, files) in [
        (
            "./base",
            vec![
                ("base.json", OFF),
                ("base.js", "export default { rules: { curly: \"error\" } };"),
            ],
        ),
        (
            "./base.js",
            vec![
                ("base.ts", "export default { rules: { curly: \"off\" } };"),
                ("base.json", ON),
            ],
        ),
    ] {
        let config = format!("import base from \"{specifier}\";\nexport default [base];\n");
        let mut files = files;
        files.push(("eslint.config.js", &config));
        assert_eq!(setting(&files), Setting::Off, "{files:?}");
    }

    let shadowed = dir_with(&[
        ("node_modules/base/package.json", "{}"),
        ("node_modules/base/index.js", OFF_JS),
        ("app/node_modules/base.js", ON_JS),
        (
            "app/eslint.config.js",
            "module.exports = [require(\"base\")];\n",
        ),
    ]);

    assert_eq!(
        decided(&shadowed.join("app"), Some(".ts"))[0].1,
        Setting::On
    );

    let broken_main = dir_with(&[
        ("node_modules/pkg/index.js", OFF_JS),
        ("node_modules/pkg/package.json", "{}"),
        (
            "app/node_modules/pkg/package.json",
            r#"{ "main": "missing.js" }"#,
        ),
        (
            "app/eslint.config.js",
            "module.exports = [require(\"pkg\")];\n",
        ),
    ]);

    assert_eq!(
        decided(&broken_main.join("app"), Some(".ts"))[0].1,
        Setting::Unknown
    );

    let unexported_root = dir_with(&[
        ("node_modules/pkg/index.js", OFF_JS),
        ("node_modules/pkg/package.json", "{}"),
        (
            "app/node_modules/pkg/package.json",
            r#"{ "exports": { "./strict": "./strict.js" } }"#,
        ),
        ("app/node_modules/pkg/strict.js", ON_JS),
        (
            "app/eslint.config.js",
            "module.exports = [require(\"pkg\")];\n",
        ),
    ]);

    assert_eq!(
        decided(&unexported_root.join("app"), Some(".ts"))[0].1,
        Setting::Unknown
    );

    let exported_directory = [
        (
            "node_modules/pkg/package.json",
            r#"{ "exports": { ".": "./config" } }"#,
        ),
        (
            "node_modules/pkg/config/package.json",
            r#"{ "main": "off.js" }"#,
        ),
        ("node_modules/pkg/config/off.js", OFF_JS),
        ("eslint.config.js", "module.exports = [require(\"pkg\")];\n"),
    ];

    assert_eq!(setting(&exported_directory), Setting::Unknown);

    let typescript = [
        ("base.json", OFF),
        ("base.js", ON_JS),
        (
            "eslint.config.ts",
            "const base = require(\"./base\");\nexport default [base];\n",
        ),
    ];

    assert_eq!(setting(&typescript), Setting::Off, "{typescript:?}");
}

#[test]
fn shadowed_config_require() {
    const BASE: (&str, &str) = (
        "base.cjs",
        r#"module.exports = [{ rules: { curly: "off" } }];"#,
    );
    const PARTS: (&str, &str) = ("parts.cjs", r#"exports.x = [{ rules: { curly: "off" } }];"#);
    const STRICT: (&str, &str) = (
        "strict.js",
        r#"export default { rules: { curly: "error" } };"#,
    );
    const DIRECT: &str = r#"export default require("./base.cjs");"#;
    const DESTRUCTURED: &str = r#"const { x } = require("./parts.cjs"); export default x;"#;
    const CONFIG: &str = r#"const config = { rules: { curly: "off" } };"#;

    let setting = |file: &str, script: &str| {
        let dir = dir_with(&[BASE, PARTS, STRICT, (file, script)]);
        decided(&dir.0, Some(".ts"))[0].1
    };

    assert_eq!(setting("eslint.config.js", DIRECT), Setting::Off);
    assert_eq!(setting("eslint.config.js", DESTRUCTURED), Setting::Off);
    assert_eq!(
        setting(
            "eslint.config.js",
            &format!(r#"{CONFIG} require("./base.cjs", config); export default [config];"#)
        ),
        Setting::Off
    );
    assert_eq!(
        setting(
            "eslint.config.ts",
            r#"type require = string; interface I<require> { f: <require>() => require } function g<require>() {} export default require("./base.cjs");"#
        ),
        Setting::Off
    );

    for erased in [
        r#"import type { T as require } from "./types";"#,
        r#"import { type T as require } from "./types";"#,
        "declare const require: (id: string) => unknown;",
        "declare function require(id: string): unknown;",
        "function load(require: string): void; function load(id: string) {}",
        "abstract class C { abstract f(require: unknown): void; }",
        "declare class require {}",
        "declare enum require { A }",
        "declare namespace require { const x: number; }",
        r#"declare module "m" { const require: unknown; }"#,
        "declare global { const require: unknown; }",
    ] {
        let script = format!(r#"{erased} export default require("./base.cjs");"#);
        assert_eq!(
            setting("eslint.config.ts", &script),
            Setting::Off,
            "{script}"
        );
    }

    for binder in [
        "function require() { return []; }",
        "const require = () => [];",
        r#"import require from "./strict.js";"#,
        "function load(require) { return require; }",
    ] {
        for call in [DIRECT, DESTRUCTURED] {
            let script = format!("{binder} {call}");
            assert_eq!(
                setting("eslint.config.js", &script),
                Setting::Unknown,
                "{script}"
            );
        }
    }

    for (file, script) in [
        (
            "eslint.config.cjs",
            r#"require = () => []; module.exports = require("./base.cjs");"#,
        ),
        (
            "eslint.config.js",
            r#"({ require } = { require: () => [] }); export default require("./base.cjs");"#,
        ),
        (
            "eslint.config.js",
            &format!(
                r#"{CONFIG} {{ const require = () => []; require("./base.cjs", config); }} export default [config];"#
            ),
        ),
        (
            "eslint.config.ts",
            r#"(require as any) = () => []; export default require("./base.cjs");"#,
        ),
        (
            "eslint.config.js",
            &format!(
                r#"{CONFIG} function load(require) {{ require("./base.cjs", config); }} export default [config];"#
            ),
        ),
        (
            "eslint.config.js",
            &format!(
                r#"const require = () => []; {CONFIG} require("./base.cjs", config); export default [config];"#
            ),
        ),
    ] {
        assert_eq!(setting(file, script), Setting::Unknown, "{script}");
    }
}

fn oxlint_curly_on(pattern: &str) -> Scratch {
    let config = serde_json::json!({
        "overrides": [{ "files": [pattern], "rules": { "curly": "error" } }]
    });

    dir_with(&[(".oxlintrc.json", &config.to_string())])
}

#[test]
fn bounded_lint_expansion_keeps_braces_for_patterns_past_the_bound() {
    let repeated = format!("{}/*.ts", "{a}".repeat(16_000));
    let nested = format!("{}a{}/*.ts", "{".repeat(16_000), "}".repeat(16_000));
    let doubling = format!("{}b{}/*.ts", "{a,".repeat(9), "}".repeat(9));
    let shrinking = format!(
        "{{x,{}}}{}/*.ts",
        "a".repeat(1_000_000),
        "{a}".repeat(16_000)
    );
    for pattern in [repeated, nested, doubling, shrinking] {
        let root = oxlint_curly_on(&pattern);
        assert!(enforced_for(&root.0, ".ts"), "{}", &pattern[..24]);
        assert!(
            enforced_for(&root.join("elsewhere"), ".ts"),
            "{}",
            &pattern[..24]
        );
    }

    let pair = oxlint_curly_on("{a,b}/*.ts");
    assert!(enforced_for(&pair.join("a"), ".ts"));
    assert!(enforced_for(&pair.join("b"), ".ts"));
    assert!(!enforced_for(&pair.join("c"), ".ts"));
    assert!(!enforced_for(&pair.0, ".ts"));

    let nested_dir = oxlint_curly_on("src/{a,b}/**");
    assert!(enforced_for(&nested_dir.join("src/a/deep"), ".ts"));
    assert!(enforced_for(&nested_dir.join("src/b"), ".ts"));
    assert!(!enforced_for(&nested_dir.join("src/c"), ".ts"));

    let names: Vec<String> = (1..=256).map(|k| format!("d{k}")).collect();
    let wide = oxlint_curly_on(&format!("{{{}}}/**", names.join(",")));
    assert!(enforced_for(&wide.join("d1"), ".ts"));
    assert!(enforced_for(&wide.join("d256"), ".ts"));
    assert!(!enforced_for(&wide.join("d257"), ".ts"));
}

const BIOME_STYLE_OFF: &str =
    r#"{ "linter": { "rules": { "style": { "useBlockStatements": "off" } } } }"#;

fn biome_nested(levels: usize) -> Vec<(String, String)> {
    let mut files = vec![(
        "biome.json".to_owned(),
        r#"{ "extends": ["./c1.json"] }"#.to_owned(),
    )];

    for level in 1..=levels {
        let text = if level == levels {
            BIOME_STYLE_OFF.to_owned()
        } else {
            format!(r#"{{ "extends": ["./c{}.json"] }}"#, level + 1)
        };

        files.push((format!("c{level}.json"), text));
    }

    files
}

fn biome_read(files: &[(String, String)]) -> Setting {
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();

    let dir = dir_with(&files);
    let found = decided(&dir.0, None);
    let [(Family::Biome, setting, files)] = &found[..] else {
        panic!("{found:?}");
    };

    assert_eq!(*files, [dir.join("biome.json")]);
    *setting
}

fn owned(files: &[(&str, &str)]) -> Vec<(String, String)> {
    files
        .iter()
        .map(|(name, text)| ((*name).to_owned(), (*text).to_owned()))
        .collect()
}

#[test]
fn biome_json_inheritance_reads_every_base_as_jsonc_whatever_its_suffix() {
    let extending = r#"{ "extends": ["./base.js"] }"#;
    for (base, expected) in [
        (
            "// shared\n{ \"linter\": { \"rules\": { \"style\": { \"useBlockStatements\": \"off\", }, }, }, /* end */ }\n",
            Setting::Off,
        ),
        (
            r#"{ "linter": { "rules": { "style": { "useBlockStatements": "error" } } } }"#,
            Setting::On,
        ),
        (
            r#"export default { linter: { rules: { style: { useBlockStatements: "off" } } } };"#,
            Setting::Unknown,
        ),
        ("{ \"linter\": /* open", Setting::Unknown),
    ] {
        let files = owned(&[("biome.json", extending), ("base.js", base)]);
        assert_eq!(biome_read(&files), expected, "{base}");
    }
}

#[test]
fn biome_json_inheritance_follows_ten_nested_extends_and_no_more() {
    assert_eq!(biome_read(&biome_nested(10)), Setting::Off);
    assert_eq!(biome_read(&biome_nested(11)), Setting::Unknown);
}

#[test]
fn biome_json_inheritance_reads_a_list_of_eleven_bases() {
    let names: Vec<String> = (1..=11)
        .map(|index| format!(r#""./s{index}.json""#))
        .collect();
    let mut files = vec![(
        "biome.json".to_owned(),
        format!(r#"{{ "extends": [{}] }}"#, names.join(", ")),
    )];

    for index in 1..=11 {
        let text = if index == 11 { BIOME_STYLE_OFF } else { "{}" };
        files.push((format!("s{index}.json"), text.to_owned()));
    }

    assert_eq!(biome_read(&files), Setting::Off);
}

#[test]
fn biome_json_inheritance_leaves_a_cycle_unknown() {
    let files = owned(&[
        ("biome.json", r#"{ "extends": ["./b.json"] }"#),
        (
            "b.json",
            r#"{ "extends": ["./biome.json"], "linter": { "rules": { "style": { "useBlockStatements": "off" } } } }"#,
        ),
    ]);

    assert_eq!(biome_read(&files), Setting::Unknown);
}

#[test]
fn biome_json_inheritance_lets_an_eslint_curly_hide_an_unread_base() {
    let dir = dir_with(&[
        ("biome.json", r#"{ "extends": ["./base.js"] }"#),
        (
            "base.js",
            r#"export default { linter: { rules: { style: { useBlockStatements: "off" } } } };"#,
        ),
        (
            "eslint.config.js",
            r#"export default [{ rules: { curly: "error" } }];"#,
        ),
    ]);

    assert_eq!(
        braces_setting(&dir.join("a.js")),
        super::BracesSetting {
            enforced: true,
            unread: Vec::new()
        }
    );
}

#[test]
fn biome_json_inheritance_reads_a_yaml_suffixed_base_as_jsonc() {
    for (base, expected) in [
        (
            r#"{ "linter": { "rules": { "style": { "useBlockStatements": "error" } } } }"#,
            Setting::On,
        ),
        (BIOME_STYLE_OFF, Setting::Off),
        ("linter:\n  rules: {}\n", Setting::Unknown),
    ] {
        let files = owned(&[
            ("biome.json", r#"{ "extends": ["./base.yaml"] }"#),
            ("base.yaml", base),
        ]);

        assert_eq!(biome_read(&files), expected, "{base}");
    }
}

#[test]
fn biome_json_inheritance_keeps_an_unread_base_past_a_later_override() {
    let off_everywhere = r#""overrides": [{ "includes": ["**"], "linter": { "rules": { "style": { "useBlockStatements": "off" } } } }]"#;
    for files in [
        vec![
            (
                "biome.json",
                format!(r#"{{ "extends": ["./base.js"], {off_everywhere} }}"#),
            ),
            (
                "base.js",
                r#"export default { linter: { rules: { style: { useBlockStatements: "off" } } } };"#
                    .to_owned(),
            ),
        ],
        vec![
            (
                "biome.json",
                format!(r#"{{ "extends": ["./b.json"], {off_everywhere} }}"#),
            ),
            ("b.json", r#"{ "extends": ["./biome.json"] }"#.to_owned()),
        ],
        vec![(
            "biome.json",
            format!(r#"{{ "extends": ["eslint-config-prettier"], {off_everywhere} }}"#),
        )],
    ] {
        let files: Vec<(String, String)> = files
            .into_iter()
            .map(|(name, text)| (name.to_owned(), text))
            .collect();

        assert_eq!(biome_read(&files), Setting::Unknown, "{files:?}");
    }
}

#[test]
fn biome_json_inheritance_resolves_an_eslint_preset_name_as_a_package() {
    let files = owned(&[
        ("biome.json", r#"{ "extends": ["eslint-config-prettier"] }"#),
        (
            "node_modules/eslint-config-prettier/package.json",
            r#"{ "name": "eslint-config-prettier", "exports": { ".": "./biome.json" } }"#,
        ),
        (
            "node_modules/eslint-config-prettier/biome.json",
            r#"{ "linter": { "rules": { "style": { "useBlockStatements": "error" } } } }"#,
        ),
    ]);

    assert_eq!(biome_read(&files), Setting::On);
    assert_eq!(biome_read(&files[..1]), Setting::Unknown);
}

#[test]
fn biome_json_inheritance_keeps_an_unread_base_outside_the_config_scope() {
    let files = owned(&[(
        "biome.json",
        r#"{ "extends": ["./missing.json"], "includes": [] }"#,
    )]);

    assert_eq!(biome_read(&files), Setting::Unknown);
}

#[test]
fn biome_json_inheritance_resolves_a_relative_base_literally() {
    for (entry, file) in [
        ("./base.js", "base.ts"),
        ("./base", "base.json"),
        ("./cfg", "cfg/index.json"),
    ] {
        let files = owned(&[
            ("biome.json", &format!(r#"{{ "extends": ["{entry}"] }}"#)),
            (file, BIOME_STYLE_OFF),
        ]);

        assert_eq!(biome_read(&files), Setting::Unknown, "{entry}");
    }
}

fn quoted_list(patterns: &[&str]) -> String {
    patterns
        .iter()
        .map(|pattern| format!("\"{pattern}\""))
        .collect::<Vec<_>>()
        .join(",")
}

fn per_file_configs(
    root: &str,
    scoped: &str,
    files: &[&str],
    excluded: Option<&str>,
) -> [(&'static str, String); 4] {
    let list = quoted_list(files);
    let exclusion =
        |key: &str| excluded.map_or(String::new(), |file| format!(r#""{key}":["{file}"],"#));
    let includes = excluded.map_or(list.clone(), |file| format!(r#"{list},"!{file}""#));
    [
        (
            "eslint.config.js",
            format!(
                r#"export default [{{rules:{{curly:"{root}"}}}},{{files:[{list}],{}rules:{{curly:"{scoped}"}}}}];"#,
                exclusion("ignores")
            ),
        ),
        (
            ".eslintrc.json",
            format!(
                r#"{{"root":true,"rules":{{"curly":"{root}"}},"overrides":[{{"files":[{list}],{}"rules":{{"curly":"{scoped}"}}}}]}}"#,
                exclusion("excludedFiles")
            ),
        ),
        (
            ".oxlintrc.json",
            format!(
                r#"{{"rules":{{"curly":"{root}"}},"overrides":[{{"files":[{list}],{}"rules":{{"curly":"{scoped}"}}}}]}}"#,
                exclusion("excludeFiles")
            ),
        ),
        (
            "biome.json",
            format!(
                r#"{{"linter":{{"rules":{{"style":{{"useBlockStatements":"{root}"}}}}}},"overrides":[{{"includes":[{includes}],"linter":{{"rules":{{"style":{{"useBlockStatements":"{scoped}"}}}}}}}}]}}"#
            ),
        ),
    ]
}

#[test]
fn per_file_lint_braces() {
    let cases = [
        (
            "off",
            "error",
            &["**/*.config.ts"][..],
            None,
            ["src/a.ts", "src/vite.config.ts"],
            [false, true],
        ),
        (
            "error",
            "off",
            &["**/*.test.ts"][..],
            None,
            ["src/a.ts", "src/a.test.ts"],
            [true, false],
        ),
        (
            "off",
            "error",
            &["src/**"][..],
            Some("src/a.ts"),
            ["src/a.ts", "src/vite.config.ts"],
            [false, true],
        ),
        (
            "off",
            "error",
            &["src/[ab].ts"][..],
            None,
            ["src/a.ts", "src/vite.config.ts"],
            [true, true],
        ),
    ];

    for (root, scoped, files, excluded, paths, expected) in cases {
        for (name, text) in per_file_configs(root, scoped, files, excluded) {
            let dir = dir_with(&[(name, &text), ("src/.keep", "")]);
            let found = paths.map(|path| {
                settled(&brace_decisions(&Target::file(&dir.join(path))).unwrap()).enforced
            });

            assert_eq!(found, expected, "{name}: {text}");
        }
    }
}

#[test]
fn per_file_lint_braces_reach_the_folder_memo() {
    for (name, text) in per_file_configs("off", "error", &["**/*.config.ts"], None) {
        let dir = dir_with(&[(name, &text), ("src/.keep", "")]);
        let found = ["src/a.ts", "src/vite.config.ts", "src/b.ts"]
            .map(|path| braces_setting(&dir.join(path)).enforced);

        assert_eq!(found, [false, true, false], "{name}: {text}");
    }

    let dir = dir_with(&[(
        "eslint.config.js",
        r#"export default [{rules:{curly:"off"}},{files:["src/**"],rules:{curly:"error"}}];"#,
    )]);

    let found = ["src/a.ts", "lib/a.ts"].map(|path| braces_setting(&dir.join(path)).enforced);
    assert_eq!(found, [true, false]);
}
