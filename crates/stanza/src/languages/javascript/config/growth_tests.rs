use super::layers::{Config, Family};
use super::module::Session;
use super::tests::{Scratch, dir_with};
use crate::engine::tests::{LINEAR_GROWTH_CAP, assert_bounded, cpu_ms, on_deep_stack, ratio};
use crate::paths::dirname;

fn numbered<T>(count: usize, item: impl Fn(usize) -> T) -> Vec<T> {
    (1..=count).map(item).collect()
}

fn decide(file: &str, family: Family) {
    Config::default().config_at(file, &dirname(file), Some(".ts"), family);
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

fn assert_linear(
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
fn prettier_overrides_with_doubled_spread_options_stay_linear() {
    assert_linear(
        500,
        |n| {
            let chain = numbered(n, |k| {
                format!("const x{k} = {{ ...x{0}, ...x{0} }};\n", k - 1)
            })
            .concat();

            let overrides =
                numbered(n, |k| format!("{{ files: \"*.none\", options: x{k} }}")).join(", ");

            vec![
                ("a.ts".to_owned(), String::new()),
                (
                    "package.json".to_owned(),
                    r#"{ "devDependencies": { "prettier": "3" } }"#.to_owned(),
                ),
                (
                    "prettier.config.mjs".to_owned(),
                    format!(
                        "const x0 = {{}};\n{chain}export default {{ overrides: [{overrides}] }};\n"
                    ),
                ),
            ]
        },
        |file| {
            super::width::formatter_width(file);
        },
    );
}

#[test]
fn reading_every_key_of_many_one_key_spreads_stays_linear() {
    assert_linear(
        2_000,
        |n| {
            let spreads = numbered(n, |k| format!("...{{ k{k}: 1 }}")).join(", ");
            vec![
                ("a.ts".to_owned(), String::new()),
                (
                    "package.json".to_owned(),
                    r#"{ "devDependencies": { "prettier": "3" } }"#.to_owned(),
                ),
                (
                    "prettier.config.mjs".to_owned(),
                    format!(
                        "export default {{ overrides: [{{ files: \"*.ts\", options: {{ {spreads} }} }}] }};\n"
                    ),
                ),
            ]
        },
        |file| {
            super::width::formatter_width(file);
        },
    );
}

#[test]
fn one_key_spreads_after_a_doubled_spread_chain_stay_linear() {
    assert_linear(
        2_000,
        |n| {
            let doubled = numbered(64, |k| {
                format!("const x{k} = {{ ...x{0}, ...x{0} }};\n", k - 1)
            })
            .concat();

            let spreads = numbered(n, |k| format!("...{{ k{k}: 1 }}")).join(", ");
            vec![
                ("a.ts".to_owned(), String::new()),
                (
                    "package.json".to_owned(),
                    r#"{ "devDependencies": { "prettier": "3" } }"#.to_owned(),
                ),
                (
                    "prettier.config.mjs".to_owned(),
                    format!(
                        "const x0 = {{ padding: 1 }};\n{doubled}export default {{ overrides: [{{ files: \"*.ts\", options: {{ ...x64, {spreads} }} }}] }};\n"
                    ),
                ),
            ]
        },
        |file| {
            super::width::formatter_width(file);
        },
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
