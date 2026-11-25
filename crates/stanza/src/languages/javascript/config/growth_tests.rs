use std::fs;

use super::brace_decisions;
use super::coverage::{Setting, Target};
use super::family::{Decision, Family, READERS};
use super::layers::Config;
use super::module::Session;
use super::tests::{decided, dir_with, shared_config};
use super::yaml;
use crate::paths::dirname;
use crate::testing::scratch::Scratch;
use crate::testing::timing::{LINEAR_GROWTH_CAP, assert_bounded, cpu_ms, on_deep_stack, ratio};

pub(super) fn numbered<T>(count: usize, item: impl Fn(usize) -> T) -> Vec<T> {
    (1..=count).map(item).collect()
}

fn decide(file: &str, family: Family) {
    Config::default().config_at(file, &Target::files_in(dirname(file), Some(".ts")), family);
}

fn index(file: &str) {
    assert!(Session::default().module_at(file).is_some());
}

fn fresh(files: &[(String, String)]) -> (Scratch, String) {
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();

    let scratch = dir_with(&borrowed);
    let entry = scratch.join(&files[0].0);
    (scratch, entry)
}

pub(super) fn assert_linear(
    n: usize,
    files: impl Fn(usize) -> Vec<(String, String)> + Send + 'static,
    run: fn(&str),
) {
    on_deep_stack(move || {
        assert_bounded(
            "config",
            &[n, 4 * n],
            4,
            ratio(LINEAR_GROWTH_CAP),
            files,
            |files| {
                let (_scratch, entry) = fresh(files);
                cpu_ms(|| run(&entry))
            },
        );
    });
}

fn one(name: &str, text: String) -> Vec<(String, String)> {
    vec![(name.to_owned(), text)]
}

#[test]
fn spreading_one_object_into_many_stays_linear() {
    assert_linear(
        250,
        |n| {
            let keys = numbered(n, |k| format!("k{k}: 1")).join(", ");
            let spreads = vec!["{ ...base }"; n].join(", ");
            one(
                "eslint.config.js",
                format!(
                    "const base = {{ {keys} }};\nexport default [{spreads}, {{ rules: {{ curly: \"error\" }} }}];\n"
                ),
            )
        },
        |file| decide(file, Family::Flat),
    );
}

#[test]
fn every_prefix_of_a_spread_chain_read_as_entries_stays_linear() {
    assert_linear(
        2_000,
        |n| {
            let chain = numbered(n, |k| format!("const x{k} = {{ ...x{} }};\n", k - 1)).concat();
            let entries = (0..=n)
                .map(|k| format!("x{k}"))
                .collect::<Vec<_>>()
                .join(", ");
            one(
                "eslint.config.js",
                format!("const x0 = {{}};\n{chain}export default [{entries}];\n"),
            )
        },
        |file| decide(file, Family::Flat),
    );
}

#[test]
fn plugins_spread_one_by_one_and_extended_by_name_stay_linear() {
    assert_linear(
        1_000,
        |n| {
            let plugins = numbered(n, |k| format!("...{{ p{k}: {{}} }}")).join(", ");
            let extends = numbered(n, |k| format!("\"p{k}/x\"")).join(", ");
            one(
                "eslint.config.js",
                format!("export default [{{ plugins: {{ {plugins} }}, extends: [{extends}] }}];\n"),
            )
        },
        |file| decide(file, Family::Flat),
    );
}

#[test]
fn oxlint_overrides_spreading_one_rules_object_stay_linear() {
    assert_linear(
        250,
        |n| {
            let rules = numbered(n, |k| format!("r{k}: \"off\"")).join(", ");
            let overrides = vec!["{ files: [\"*.ts\"], rules: { ...rules } }"; n].join(", ");
            one(
                "oxlint.config.ts",
                format!(
                    "const rules = {{ {rules}, curly: \"off\" }};\nexport default {{ overrides: [{overrides}] }};\n"
                ),
            )
        },
        |file| decide(file, Family::Oxlint),
    );
}

#[test]
fn oxlint_overrides_adding_a_rule_after_a_spread_stay_linear() {
    assert_linear(
        250,
        |n| {
            let rules = numbered(n, |k| format!("r{k}: \"off\"")).join(", ");
            let overrides =
                vec!["{ files: [\"*.ts\"], rules: { ...rules, \"eslint/curly\": \"off\" } }"; n]
                    .join(", ");

            one(
                "oxlint.config.ts",
                format!(
                    "const rules = {{ {rules}, curly: \"error\" }};\nexport default {{ overrides: [{overrides}] }};\n"
                ),
            )
        },
        |file| decide(file, Family::Oxlint),
    );
}

#[test]
fn deeply_nested_inline_extends_stay_linear() {
    assert_linear(
        1_000,
        |n| {
            let mut config = r#"{ rules: { curly: "off" } }"#.to_owned();
            for _ in 0..n {
                config = format!("{{ extends: [{config}] }}");
            }

            one("eslint.config.js", format!("export default [{config}];\n"))
        },
        |file| decide(file, Family::Flat),
    );
}

#[test]
fn extending_many_distinct_files_stays_linear() {
    assert_linear(
        500,
        |n| {
            let names = numbered(n, |k| format!("\"./c{k}.json\"")).join(", ");
            let mut files = one(".eslintrc.json", format!("{{ \"extends\": [{names}] }}\n"));
            files.extend(numbered(n, |k| {
                (
                    format!("c{k}.json"),
                    format!("{{ \"rules\": {{ \"k{k}\": 1 }} }}\n"),
                )
            }));

            files
        },
        |file| decide(file, Family::Legacy),
    );
}

#[test]
fn stable_config_imports_of_one_module_at_every_depth_stay_linear() {
    assert_linear(
        500,
        |n| {
            let objects =
                numbered(n, |k| format!("{{ rules: {{ curly: \"off\", k{k}: 1 }} }}")).join(", ");
            let mut files = one(
                "eslint.config.mjs",
                "import next from \"./l1.mjs\";\nexport default [next, { rules: { curly: \"off\" } }];\n"
                    .to_owned(),
            );

            files.push((
                "shared.mjs".to_owned(),
                format!("export default [{objects}];\n"),
            ));

            files.extend(numbered(30, |k| {
                let link = if k == 30 {
                    "export default [shared];\n".to_owned()
                } else {
                    format!(
                        "import next from \"./l{}.mjs\";\nexport default [next, shared];\n",
                        k + 1
                    )
                };

                (
                    format!("l{k}.mjs"),
                    format!("import shared from \"./shared.mjs\";\n{link}"),
                )
            }));

            files
        },
        |file| {
            let found = Config::default().config_at(
                file,
                &Target::files_in(dirname(file), Some(".ts")),
                Family::Flat,
            );
            assert_eq!(found.setting, Some(Setting::Off));
        },
    );
}

#[test]
fn stable_import_cycles_aliases_of_many_imports_stay_linear() {
    assert_linear(
        500,
        |n| {
            let imports = numbered(n, |k| format!("import p{k} from \"./p{k}.mjs\";\n")).concat();
            let spreads = numbered(n, |k| format!("...p{k}")).join(", ");
            let aliases = numbered(n, |k| format!("const x{k} = base;\n")).concat();
            let listed = numbered(n, |k| format!("x{k}")).join(", ");
            let mut files = one(
                "eslint.config.mjs",
                format!(
                    "{imports}const base = {{ {spreads} }};\n{aliases}export default [{listed}];\n"
                ),
            );

            files.extend(numbered(n, |k| {
                (format!("p{k}.mjs"), "export default {};\n".to_owned())
            }));

            files
        },
        |file| decide(file, Family::Flat),
    );
}

fn decide_every_root(file: &str) {
    let config = Config::default();
    let root = dirname(dirname(file));
    for entry in fs::read_dir(root).unwrap() {
        let dir = entry.unwrap().path();
        if dir.is_dir() {
            let target = Target::files_in(&dir.to_string_lossy(), Some(".ts"));
            let found = Family::Flat.decide(&config, &target);
            assert_eq!(
                found.map(|found| found.setting),
                Some(Setting::Off),
                "{}",
                dir.display()
            );
        }
    }
}

#[test]
fn configs_extending_one_shared_file_stay_linear() {
    assert_linear(
        50,
        |n| {
            let mut shared = r#"{ rules: { curly: "off" } }"#.to_owned();
            for _ in 0..10 * n {
                shared = format!("{{ extends: [{shared}] }}");
            }

            let mut files: Vec<(String, String)> = (0..n)
                .map(|k| {
                    (
                        format!("p{k}/eslint.config.js"),
                        "export default [{ extends: [\"../shared.js\"] }];\n".to_owned(),
                    )
                })
                .collect();

            files.push((
                "shared.js".to_owned(),
                format!("export default [{shared}];\n"),
            ));

            files
        },
        decide_every_root,
    );
}

#[test]
fn stable_import_cycles_many_roots_sharing_one_wide_preset_stay_linear() {
    assert_linear(
        300,
        |n| {
            let rules = numbered(n, |k| format!("k{k}: 1")).join(", ");
            let mut files: Vec<(String, String)> = numbered(n, |k| {
                (
                    format!("d{k}/eslint.config.mjs"),
                    "import preset from \"../preset.mjs\";\nexport default [preset];\n".to_owned(),
                )
            });

            files.push((
                "preset.mjs".to_owned(),
                format!(
                    "import {{ defineConfig }} from \"eslint/config\";\nexport default defineConfig({{ rules: {{ curly: \"off\", {rules} }} }});\n"
                ),
            ));

            files
        },
        decide_every_root,
    );
}

#[test]
fn many_roots_running_one_shared_module_graph_stay_linear() {
    assert_linear(
        150,
        |n| {
            let mut files: Vec<(String, String)> = numbered(n, |k| {
                (
                    format!("d{k}/eslint.config.mjs"),
                    "import \"../preset.mjs\";\nexport default [{ rules: { curly: \"off\" } }];\n"
                        .to_owned(),
                )
            });

            let imports = numbered(200, |k| format!("import \"./p{k}.mjs\";\n")).concat();
            files.push(("preset.mjs".to_owned(), imports));
            files.extend(numbered(200, |k| {
                (format!("p{k}.mjs"), "export default {};\n".to_owned())
            }));

            files
        },
        decide_every_root,
    );
}

#[test]
fn many_roots_importing_one_module_many_times_stay_linear() {
    assert_linear(
        150,
        |n| {
            let mut files: Vec<(String, String)> = numbered(n, |k| {
                (
                    format!("d{k}/eslint.config.mjs"),
                    "import \"../preset.mjs\";\nexport default [{ rules: { curly: \"off\" } }];\n"
                        .to_owned(),
                )
            });

            files.push((
                "preset.mjs".to_owned(),
                "import \"./leaf.mjs\";\n".repeat(n),
            ));
            files.push(("leaf.mjs".to_owned(), "export default {};\n".to_owned()));
            files
        },
        decide_every_root,
    );
}

#[test]
fn many_aliases_of_one_object_stay_linear() {
    assert_linear(
        2_500,
        |n| {
            let aliases = numbered(n, |k| format!("const a{k} = x;\n")).concat();
            one(
                "eslint.config.js",
                format!("const x = {{}}; console.log(z);\n{aliases}"),
            )
        },
        index,
    );
}

#[test]
fn scoped_config_aliases_of_many_names_stay_linear() {
    assert_linear(
        2_000,
        |n| {
            let names = numbered(n, |k| format!("x{k}")).join(", ");
            let blocks =
                numbered(n, |k| format!("{{ const a{k} = x{k}; a{k}.push(1); }}\n")).concat();
            let passes = numbered(n, |k| format!("[x{k}].forEach(add);\n")).concat();
            one(
                "eslint.config.js",
                format!(
                    "const x1 = {{}};\n[{names}].forEach(({names}) => 0);\n{blocks}function add({names}) {{ x1.push(1); }}\n{passes}"
                ),
            )
        },
        index,
    );
}

#[test]
fn scoped_config_alias_assignment_chains_stay_linear() {
    assert_linear(
        2_000,
        |n| {
            let names = numbered(n, |k| format!("a{k}")).join(", ");
            let chain = numbered(n, |k| format!("a{k} = ")).concat();
            one(
                "eslint.config.js",
                format!("const x = {{}};\nlet {names};\n{chain}[x];\na1.push(1);\n"),
            )
        },
        index,
    );
}

#[test]
fn many_export_assignments_stay_linear() {
    assert_linear(
        20_000,
        |n| {
            one(
                ".eslintrc.cjs",
                numbered(n, |k| format!("exports.a{k} = 1;\n")).concat(),
            )
        },
        index,
    );
}

#[test]
fn many_scopes_under_many_parameters_stay_linear() {
    assert_linear(
        2_000,
        |n| {
            let params = numbered(n, |k| format!("p{k}")).join(", ");
            let arrows = vec!["(q) => 0"; n].join(", ");
            one(
                "eslint.config.js",
                format!("f(({params}) => [{arrows}]);\n"),
            )
        },
        index,
    );
}

#[test]
fn a_flat_config_of_deeply_nested_arrays_stays_linear() {
    assert_linear(
        10_000,
        |n| {
            one(
                "eslint.config.js",
                format!("export default {}{};\n", "[".repeat(n), "]".repeat(n)),
            )
        },
        |file| decide(file, Family::Flat),
    );
}

#[test]
fn yaml_comment_markers_before_a_lone_carriage_return_stay_linear() {
    assert_linear(
        5_000,
        |n| one(".eslintrc.yaml", format!("{}\r", " #".repeat(n))),
        |file| decide(file, Family::Legacy),
    );
}

#[test]
fn spread_chain_past_the_depth_limit_stays_linear() {
    assert_linear(
        4_000,
        |n| {
            let chain = numbered(n, |k| format!("const x{k} = {{ ...x{} }};\n", k - 1)).concat();
            one(
                "eslint.config.js",
                format!(
                    "const x0 = {{ rules: {{ curly: \"off\" }} }};\n{chain}export default [x{n}];\n"
                ),
            )
        },
        |file| decide(file, Family::Flat),
    );
}

fn yaml_keys(n: usize) -> String {
    let keys: String = numbered(n, |k| format!("k{k}: true\n")).concat();
    format!("printWidth: 100\n{keys}")
}

fn read_yaml(file: &str) {
    assert!(yaml::load(file).is_some());
}

fn reject_yaml(file: &str) {
    assert!(yaml::load(file).is_none());
}

#[test]
fn bounded_yaml_growth_distinct_keys_stay_linear() {
    assert_linear(10_000, |n| one(".prettierrc.yaml", yaml_keys(n)), read_yaml);
}

#[test]
fn bounded_yaml_growth_a_late_duplicate_key_stays_linear() {
    assert_linear(
        10_000,
        |n| one(".prettierrc.yaml", format!("{}k{n}: false\n", yaml_keys(n))),
        reject_yaml,
    );
}

#[test]
fn bounded_yaml_growth_a_long_scalar_stays_linear() {
    assert_linear(
        10_000,
        |n| {
            one(
                ".prettierrc.yaml",
                format!("printWidth: 100\nx: {}\n", "a:b ".repeat(n)),
            )
        },
        read_yaml,
    );
}

#[test]
fn bounded_yaml_growth_malformed_flow_nesting_stays_linear() {
    assert_linear(
        10_000,
        |n| {
            one(
                ".prettierrc.yaml",
                format!("printWidth: 100\nx: [{}\n", "[0], ".repeat(n)),
            )
        },
        reject_yaml,
    );
}

fn decide_all(file: &str) {
    brace_decisions(&Target::files_in(dirname(file), Some(".ts"))).unwrap();
}

fn assert_level_shape(name: &'static str, next: fn(&str) -> String, decision: (Family, Setting)) {
    let small = dir_with(&[(name, &shared_config(30, next))]);
    assert_eq!(
        decided(small.path(), Some(".ts")),
        [(decision.0, decision.1, vec![small.join(name)])]
    );

    assert_linear(30, move |n| one(name, shared_config(n, next)), decide_all);
}

#[test]
fn shared_flat_config_layers_stay_linear() {
    assert_level_shape(
        "eslint.config.js",
        |previous| format!("[{previous}, {previous}]"),
        (Family::Flat, Setting::Off),
    );
}

#[test]
fn distinct_flat_scopes_stay_linear() {
    assert_level_shape(
        "eslint.config.js",
        |previous| {
            format!(
                r#"[{{ files: ["**/*.ts"], extends: [{previous}] }}, {{ files: ["**/*.js"], extends: [{previous}] }}]"#
            )
        },
        (Family::Flat, Setting::Unknown),
    );
}

#[test]
fn shared_oxlint_extends_stay_linear() {
    assert_level_shape(
        "oxlint.config.ts",
        |previous| format!("{{ extends: [{previous}, {previous}] }}"),
        (Family::Oxlint, Setting::Off),
    );
}

fn decided_by(reader: &Config, dir: &str) -> Vec<Decision> {
    READERS
        .into_iter()
        .filter_map(|family| family.decide(reader, &Target::files_in(dir, None)))
        .collect()
}

#[test]
fn scoped_config_objects_across_many_directories_stay_linear() {
    let configs = numbered(60, |k| {
        let index = k - 1;
        format!(
            r#"{{ "files": ["packages/p{index}/**/*.{{ts,tsx}}", "apps/a{index}/src/**/*.{{js,jsx,mjs}}", "**/*.test.{{ts,tsx}}"], "rules": {{ "curly": "error" }} }}"#
        )
    })
    .join(", ");

    let config = format!("export default [{configs}];\n");
    on_deep_stack(move || {
        assert_bounded(
            "config",
            &[700, 2_800],
            1,
            ratio(LINEAR_GROWTH_CAP),
            |n| n,
            |&n| {
                let scratch = dir_with(&[("eslint.config.js", &config)]);
                let dirs: Vec<String> = (0..n)
                    .map(|index| {
                        let dir = scratch.join(&format!("packages/p{}/src/d{index}/e", index % 60));
                        fs::create_dir_all(&dir).unwrap();
                        dir
                    })
                    .collect();

                let reader = Config::default();
                let mut decisions = Vec::with_capacity(n);
                let ms = cpu_ms(|| {
                    for dir in &dirs {
                        decisions.push(decided_by(&reader, dir));
                    }
                });

                let expected = [Decision {
                    family: Family::Flat,
                    setting: Setting::On,
                    files: vec![scratch.join("eslint.config.js")],
                }];

                for (dir, decision) in dirs.iter().zip(&decisions) {
                    assert_eq!(*decision, expected, "{dir}");
                }

                ms
            },
        );
    });
}

#[test]
fn repeated_brace_groups_stay_linear() {
    assert_linear(
        4_000,
        |n| {
            let pattern = format!("{}/*.ts", "{a}".repeat(n));
            let config = serde_json::json!({
                "overrides": [{ "files": [pattern], "rules": { "curly": "error" } }]
            });

            one(".oxlintrc.json", config.to_string())
        },
        |file| decide(file, Family::Oxlint),
    );
}

#[test]
fn per_file_lint_growth() {
    let layers = (0..60)
        .map(|k| format!(r#"{{files:["src/f{k}.ts"],rules:{{curly:"error"}}}}"#))
        .collect::<Vec<_>>()
        .join(",");

    let config = format!("export default [{layers}];\n");
    on_deep_stack(move || {
        assert_bounded(
            "per-file decisions",
            &[700, 2_800],
            4,
            ratio(LINEAR_GROWTH_CAP),
            |n| n,
            |&n| {
                let scratch = dir_with(&[("eslint.config.js", &config), ("src/.keep", "")]);
                let entry = scratch.join("eslint.config.js");
                let paths: Vec<String> = (0..n)
                    .map(|k| scratch.join(&format!("src/f{k}.ts")))
                    .collect();
                let config = Config::default();
                let mut enforced = 0;
                let spent = cpu_ms(|| {
                    for path in &paths {
                        let decision = Family::Flat.decide(&config, &Target::file(path));
                        enforced +=
                            usize::from(decision.is_some_and(|found| found.setting == Setting::On));
                    }
                });

                assert_eq!(enforced, 60);
                assert_eq!(super::module::parses(&entry), 1);
                spent
            },
        );
    });
}
