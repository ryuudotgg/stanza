use std::fs;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::eslint::package_name;
use super::layers::{Config, Family, Setting};
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
    settled(&brace_decisions(dir, None).unwrap()).enforced
}

fn enforced_for(dir: &str, extension: &str) -> bool {
    settled(&brace_decisions(dir, Some(extension)).unwrap()).enforced
}

fn decided(dir: &str, extension: Option<&str>) -> Vec<(Family, Setting, Vec<String>)> {
    brace_decisions(dir, extension)
        .unwrap()
        .into_iter()
        .map(|decision| (decision.family, decision.setting, decision.files))
        .collect()
}

fn shared_config(levels: usize, next: impl Fn(&str) -> String) -> String {
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
        braces_setting(&dir.0, Some(".ts")).unread,
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

fn settings_in_order(dir: &Scratch, names: &[&str]) -> Vec<Setting> {
    let config = Config::default();
    names
        .iter()
        .map(|name| {
            Family::Legacy
                .decide(&config, &dir.join(name), Some(".ts"))
                .unwrap()
                .setting
        })
        .collect()
}

fn assert_stable(dir: &Scratch, expected: &[(&str, Setting)]) {
    let names: Vec<&str> = expected.iter().map(|(name, _)| *name).collect();
    let settings: Vec<Setting> = expected.iter().map(|(_, setting)| *setting).collect();
    assert_eq!(settings_in_order(dir, &names), settings);

    let reversed: Vec<&str> = names.iter().rev().copied().collect();
    let mut flipped = settings.clone();
    flipped.reverse();
    assert_eq!(settings_in_order(dir, &reversed), flipped);

    for (name, setting) in expected {
        assert_eq!(settings_in_order(dir, &[name]), [*setting], "{name} alone");
    }
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
        r#"{ "style": { "useBlockStatements": { "level": "off" } } }"#,
    )]);

    let level_warn = dir_with(&[(
        "biome.json",
        r#"{ "style": { "useBlockStatements": { "level": "warn" } } }"#,
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

#[test]
fn yaml_eslintrc_forms_are_read() {
    for (name, text, expected) in [
        (".eslintrc", "rules:\n  curly: error\n", true),
        (".eslintrc", "rules:\n  curly: off\n", false),
        (".eslintrc", "rules:\n  curly: [off]\n", false),
        (
            ".eslintrc",
            "rules:\n  curly: [\"error\", \"multi\"]\n",
            true,
        ),
        (".eslintrc.yml", "rules:\n  curly:\n    - off\n", false),
        (
            ".eslintrc.yaml",
            "rules:\n  curly:\n    - error\n    - multi\n",
            true,
        ),
        (
            ".eslintrc.yml",
            "rules:\n  curly: off # keep braces optional\n",
            false,
        ),
        (".eslintrc.yml", "rules: { curly: error }\n", true),
        (
            ".eslintrc.yml",
            "rules:\n  curly: off\noverrides: [{ rules: { curly: error } }]\n",
            true,
        ),
        (
            ".eslintrc.yml",
            "rules:\n  curly: off\nother:\n  curly: error\n",
            true,
        ),
    ] {
        assert_eq!(
            enforced(&dir_with(&[(name, text)]).0),
            expected,
            "{name} {text:?}"
        );
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
        assert_eq!(package_name(name), expected, "{name}");
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

#[test]
fn plugin_packages_contribute_no_setting() {
    for source in [
        "eslint-plugin-demo",
        "@demo/eslint-plugin",
        "@demo/eslint-plugin-extra",
        "@typescript-eslint/eslint-plugin",
    ] {
        for binding in [
            format!(r#"import plugin from "{source}";"#),
            format!(r#"const plugin = require("{source}");"#),
        ] {
            let text = format!(
                r#"{binding}
          export default [plugin.configs.recommended, ...plugin.configs.recommended, plugin.configs[unknownName],
            {{ ...plugin.configs.recommended, rules: {{ ...plugin.configs.recommended.rules }} }},
            {{ plugins: {{ demo: plugin }}, extends: ["demo/recommended"] }}];"#
            );

            assert!(
                !enforced(&dir_with(&[("eslint.config.js", &text)]).0),
                "{binding}"
            );
        }
    }

    for extends in [
        r#""plugin:demo/recommended""#,
        r#""plugin:@demo/strict""#,
        r#""plugin:@demo/extra/strict""#,
        r#"["eslint:all","plugin:prettier/recommended"]"#,
        r#"["eslint:all","eslint-plugin-prettier/recommended"]"#,
    ] {
        let text = format!(r#"{{"extends":{extends}}}"#);
        assert!(
            !enforced(&dir_with(&[(".eslintrc.json", &text)]).0),
            "{extends}"
        );
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
            r#"{ "overrides": [{ "includes": ["pkg/src/**"], "style": { "useBlockStatements": "error" } }] }"#,
        ),
        (
            "app/pkg/biome.json",
            r#"{ "root": false, "extends": "//", "overrides": [{ "includes": ["other/**"], "style": { "useBlockStatements": "off" } }] }"#,
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
            r#"{ "style": { "useBlockStatements": "error" } }"#,
        ),
        (
            "workspace/biome.jsonc",
            r#"{ "root": false, "style": { "useBlockStatements": "off" }, "overrides": [{ "includes": ["pkg/src/**"], "style": { "useBlockStatements": "error" } }] }"#,
        ),
        (
            "workspace/pkg/biome.json",
            r#"{ "root": false, "extends": "//", "overrides": [{ "includes": ["other/**"], "style": { "useBlockStatements": "off" } }] }"#,
        ),
    ]);

    let overridden = dir_with(&[
        (
            "biome.json",
            r#"{ "style": { "useBlockStatements": "error" } }"#,
        ),
        (
            "pkg/biome.json",
            r#"{ "root": false, "extends": "//", "style": { "useBlockStatements": "off" } }"#,
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
    let doubling: String = (0..24)
        .map(|index| format!("const a{} = [...a{index}, ...a{index}];", index + 1))
        .collect::<Vec<_>>()
        .join("\n");

    for script in [
        "const a = { rules: a.rules }; export default a;".to_owned(),
        "const a = { extends: [b] }; const b = { extends: [a] }; export default a;".to_owned(),
        "const a = b; const b = a; export default a;".to_owned(),
        "const level = { level }; export default [{ rules: { curly: level } }];".to_owned(),
        format!("export default [{}];", empties(10001)),
        format!(
            "const base = [{}]; export default [...base, {{}}];",
            empties(10000)
        ),
        format!(
            r#"import {{ defineConfig }} from "eslint/config"; const base = [{}]; export default defineConfig(base, {{}});"#,
            empties(10000)
        ),
        format!("const a0 = [{{}}]; {doubling} export default a24;"),
    ] {
        let root = dir_with(&[("eslint.config.js", &script)]);
        assert!(enforced(&root.0), "{}", &script[..script.len().min(80)]);
        assert!(enforced(&root.0));
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

fn biome_config_that_contains_itself() -> Scratch {
    dir_with(&[
        ("biome.json", r#"{ "extends": ["./base.js"] }"#),
        (
            "base.js",
            "const a = { x: b, style: { useBlockStatements: \"off\" } }; const b = { y: a };\nexport default a;\n",
        ),
    ])
}

#[test]
fn a_biome_config_that_contains_itself_keeps_braces_without_a_notice() {
    let dir = biome_config_that_contains_itself();

    assert!(brace_decisions(&dir.0, None).is_err());
    assert_eq!(
        braces_setting(&dir.0, None),
        super::BracesSetting {
            enforced: true,
            unread: Vec::new()
        }
    );
}

#[test]
#[should_panic(expected = "a config value contains itself")]
fn a_failed_reader_fails_the_enforced_helper() {
    let dir = biome_config_that_contains_itself();
    enforced(&dir.0);
}

#[test]
fn a_biome_value_shared_along_many_paths_is_read_once_per_path_shape() {
    let levels: Vec<String> = (1..=40)
        .map(|level| format!("const a{level} = {{ x: a{0}, y: a{0} }};", level - 1))
        .collect();

    let dir = dir_with(&[
        ("biome.json", r#"{ "extends": ["./base.js"] }"#),
        (
            "base.js",
            &format!(
                "const a0 = {{ style: {{ useBlockStatements: \"off\" }} }};\n{}\nexport default a40;\n",
                levels.join("\n")
            ),
        ),
    ]);

    assert!(!enforced(&dir.0));
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
fn biome_preset_braces() {
    let on = Some(Setting::On);
    let off = Some(Setting::Off);
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
    ] {
        let dir = dir_with(&files);
        assert_eq!(setting_of(&dir.0), expected, "{files:?}");
    }

    let scoped = dir_with(&[(
        "biome.json",
        r#"{ "overrides": [{ "includes": ["src/**"], "linter": { "rules": { "preset": "all" } } }] }"#,
    )]);

    assert_eq!(setting_of(&scoped.join("src")), on);
    assert_eq!(setting_of(&scoped.0), None);
}
