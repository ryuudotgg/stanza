use std::fs;
use std::os::unix::fs::symlink;

use super::super::tests::{Scratch, dir_with};
use super::*;
use crate::engine::types::Mode;
use crate::step::{Unread, format_text};

const PRETTIER: (&str, &str) = (
    "package.json",
    r#"{ "devDependencies": { "prettier": "3" } }"#,
);
const OXFMT: (&str, &str) = ("package.json", r#"{ "devDependencies": { "oxfmt": "1" } }"#);

fn width_at(root: &Scratch, path: &str) -> Width {
    formatter_width(&root.join(path))
}

fn columns_at(root: &Scratch, path: &str) -> usize {
    width_at(root, path).columns
}

fn config_source(root: &Scratch, file: &str) -> WidthSource {
    WidthSource::Config(root.join(file))
}

fn fixed(root: &Scratch, path: &str, text: &str) -> Option<String> {
    format_text(
        &root.join(path),
        &Ok(text.to_owned()),
        Mode::Fix,
        None,
        &mut Unread::default(),
        None,
    )
    .fixed
}

#[test]
fn editorconfig_values_convert_like_javascript_number() {
    let rows: Vec<(String, serde_json::Value)> =
        serde_json::from_str(include_str!("number.json")).expect("the table is JSON");

    for (text, expected) in rows {
        let converted = js_number(&text);
        match expected {
            serde_json::Value::String(name) => {
                let shown = match converted {
                    value if value.is_nan() => "NaN",
                    f64::INFINITY => "Infinity",
                    f64::NEG_INFINITY => "-Infinity",
                    _ => "finite",
                };

                assert_eq!(shown, name, "{text:?}");
            }
            expected => assert_eq!(Some(converted), expected.as_f64(), "{text:?}"),
        }
    }
}

#[test]
fn widths_past_usize_still_rank_by_their_configured_value() {
    let root = dir_with(&[
        (".oxfmtrc.json", r#"{ "printWidth": 1e22, "tabWidth": 8 }"#),
        (
            ".prettierrc.json",
            r#"{ "printWidth": 1e21, "tabWidth": 3 }"#,
        ),
    ]);

    let width = width_at(&root, "x.ts");
    assert_eq!(width.source, config_source(&root, ".prettierrc.json"));
    assert_eq!(
        (width.columns, width.columns_text.as_str(), width.tab),
        (usize::MAX, "1e+21", 3)
    );
}

#[test]
fn widths_and_sources_follow_configs_overrides_and_defaults() {
    let oxfmt = dir_with(&[(".oxfmtrc.json", r#"{ "printWidth": 120 }"#)]);
    let width = width_at(&oxfmt, "x.ts");
    assert_eq!(width.columns, 120);
    assert_eq!(width.source, config_source(&oxfmt, ".oxfmtrc.json"));

    let biome = dir_with(&[("biome.json", r#"{ "formatter": { "lineWidth": 90 } }"#)]);
    let width = width_at(&biome, "x.ts");
    assert_eq!(width.columns, 90);
    assert_eq!(width.source, config_source(&biome, "biome.json"));

    let prettier = dir_with(&[(
        ".prettierrc",
        r#"{ "printWidth": 70, "overrides": [{ "files": "*.test.ts", "options": { "printWidth": 60 } }] }"#,
    )]);

    assert_eq!(columns_at(&prettier, "x.ts"), 70);
    assert_eq!(columns_at(&prettier, "x.test.ts"), 60);

    let editor = dir_with(&[
        PRETTIER,
        (
            ".editorconfig",
            "root = true\n[*]\nmax_line_length = 110\ntab_width = 4\n",
        ),
    ]);

    let width = width_at(&editor, "x.ts");
    assert_eq!((width.columns, width.tab), (110, 4));
    assert_eq!(width.source, config_source(&editor, ".editorconfig"));

    let default_oxfmt = dir_with(&[OXFMT]);
    let width = width_at(&default_oxfmt, "x.ts");
    assert_eq!(width.columns, 100);
    assert_eq!(width.source, WidthSource::Default(Formatter::Oxfmt));

    let fallback = dir_with(&[]);
    let width = width_at(&fallback, "x.ts");
    assert_eq!(width.columns, 80);
    assert_eq!(width.source, WidthSource::Fallback);
}

#[test]
fn selection_skips_lint_only_biome_and_follows_dependencies_upward() {
    let root = dir_with(&[
        OXFMT,
        ("biome.json", r#"{ "linter": { "enabled": true } }"#),
        ("packages/app/package.json", r#"{ "name": "app" }"#),
        ("packages/app/.oxfmtrc.json", r#"{ "printWidth": 120 }"#),
        ("packages/app/.prettierrc", r#"{ "printWidth": 60 }"#),
    ]);

    let width = width_at(&root, "x.ts");
    assert_eq!(width.columns, 100);
    assert_eq!(width.source, WidthSource::Default(Formatter::Oxfmt));
    assert!(width.unread.is_empty());

    let width = width_at(&root, "packages/app/x.ts");
    assert_eq!(width.columns, 120);
    assert_eq!(
        width.source,
        config_source(&root, "packages/app/.oxfmtrc.json")
    );
}

#[test]
fn editorconfig_sections_and_files_resolve_each_property_in_order() {
    let root = dir_with(&[
        PRETTIER,
        (
            ".editorconfig",
            "[*]\nmax_line_length = 105\nindent_size = 3\ntab_width = 8\n[*.ts]\nmax_line_length = 110\nindent_size = tab\n[*.ts]\nmax_line_length = off\nmax_line_length = 115\n",
        ),
        (
            "child/.editorconfig",
            "root = true\n[*]\nmax_line_length = nonsense\ntab_width = 6\n[*.ts]\nmax_line_length = 90\nindent_size = tab\n",
        ),
    ]);

    let width = width_at(&root, "x.ts");
    assert_eq!((width.columns, width.tab), (115, 8));
    assert_eq!(width.source, config_source(&root, ".editorconfig"));

    let width = width_at(&root, "child/x.ts");
    assert_eq!((width.columns, width.tab), (90, 6));
    assert_eq!(width.source, config_source(&root, "child/.editorconfig"));
}

#[test]
fn a_nearer_editorconfig_tab_width_beats_a_parent_indent_size() {
    let root = dir_with(&[
        PRETTIER,
        (
            ".editorconfig",
            "[*]\nmax_line_length = 110\nindent_size = 3\n",
        ),
        (
            "child/.editorconfig",
            "[*]\nmax_line_length = off\nmax_line_length = invalid\ntab_width = 6\n",
        ),
    ]);

    let width = width_at(&root, "child/x.ts");
    assert_eq!((width.columns, width.tab), (80, 6));
    assert_eq!(width.source, WidthSource::Default(Formatter::Prettier));
}

#[test]
fn oxfmt_reads_only_the_nearest_editorconfig() {
    let root = dir_with(&[
        OXFMT,
        (".editorconfig", "[*]\nmax_line_length = 60\n"),
        ("child/.editorconfig", "[*]\ntab_width = 4\n"),
    ]);

    let width = width_at(&root, "child/a.ts");
    assert_eq!((width.columns, width.tab), (100, 4));
    assert_eq!(width.source, WidthSource::Default(Formatter::Oxfmt));
}

#[test]
fn biome_overrides_merge_formatter_fields_into_the_base() {
    let root = dir_with(&[(
        "biome.json",
        r#"{"formatter":{"lineWidth":120,"indentWidth":4},"javascript":{"formatter":{"lineWidth":110,"indentWidth":6}},"overrides":[{"includes":["src/**"],"formatter":{"indentWidth":2},"javascript":{"formatter":{"indentWidth":3}}}]}"#,
    )]);

    let width = width_at(&root, "src/a.ts");
    assert_eq!((width.columns, width.tab), (110, 3));
    let width = width_at(&root, "other.ts");
    assert_eq!((width.columns, width.tab), (110, 6));
}

#[test]
fn biome_file_filters_skip_a_config_that_excludes_the_target() {
    for config in [
        r#"{"files":{"includes":["src/**"]},"formatter":{"lineWidth":70}}"#,
        r#"{"files":{"include":["src/**"]},"formatter":{"lineWidth":70}}"#,
        r#"{"files":{"ignore":["other.ts"]},"formatter":{"lineWidth":70}}"#,
        r#"{ "formatter": { "lineWidth": 70, "includes": ["src/**"] } }"#,
    ] {
        let root = dir_with(&[
            (".prettierrc", r#"{ "printWidth": 120 }"#),
            ("child/biome.json", config),
        ]);

        assert_eq!(columns_at(&root, "child/other.ts"), 120, "{config}");
    }
}

#[test]
fn unread_editorconfig_files_fall_back_without_a_finding() {
    let root = dir_with(&[PRETTIER]);
    fs::create_dir(root.join(".editorconfig")).unwrap();

    let width = width_at(&root, "a.ts");
    assert_eq!(width.columns, 80);
    assert_eq!(width.unread, [root.join(".editorconfig")]);

    let result = format_text(
        &root.join("a.ts"),
        &Ok("if (a) return a;\n".to_owned()),
        Mode::Check,
        None,
        &mut Unread::default(),
        None,
    );

    assert!(
        result
            .findings
            .iter()
            .all(|finding| finding.rule != crate::engine::rules::Rule::Error)
    );
}

#[test]
fn editorconfig_lookup_stops_at_the_project_root() {
    for marker in [".git", ".hg"] {
        let root = dir_with(&[
            PRETTIER,
            (".editorconfig", "[*]\nmax_line_length = 120\n"),
            ("child/.editorconfig", "[*]\ntab_width = 7\n"),
        ]);

        fs::create_dir(root.join(&format!("child/{marker}"))).unwrap();

        let width = width_at(&root, "child/a.ts");
        assert_eq!((width.columns, width.tab), (80, 7), "{marker}");
        assert_eq!(width.source, WidthSource::Default(Formatter::Prettier));
    }
}

#[test]
fn unknown_formatter_values_and_overrides_leave_the_config_unread() {
    for config in [
        "const w = Number(process.env.W ?? 120); export default { printWidth: w };",
        "const w = Number(process.env.W ?? 2); export default { tabWidth: w };",
        "const w = Number(process.env.W ?? 2); export default { overrides: w };",
        "const w = process.env.W; export default { overrides: [{ files: w, options: { printWidth: 90 } }] };",
        r#"const w = Number(process.env.W ?? 90); export default { overrides: [{ files: "*.ts", options: { printWidth: w } }] };"#,
    ] {
        let root = dir_with(&[
            (".prettierrc", r#"{ "printWidth": 120 }"#),
            ("child/prettier.config.mjs", config),
        ]);

        let width = width_at(&root, "child/a.ts");
        assert_eq!(width.columns, 120, "{config}");
        assert_eq!(
            width.unread,
            [root.join("child/prettier.config.mjs")],
            "{config}"
        );
    }
}

#[test]
fn width_caches_the_resolved_real_path() {
    let root = dir_with(&[(
        ".prettierrc",
        r#"{ "printWidth": 120, "overrides": [{ "files": "src/*.ts", "options": { "printWidth": 90 } }] }"#,
    )]);

    fs::create_dir(root.join("src")).unwrap();
    let link = format!("{}-link", root.0);
    symlink(&root.0, &link).unwrap();

    let first = width_at(&root, "src/a.ts");
    let linked = formatter_width(&format!("{link}/src/a.ts"));
    fs::remove_file(&link).unwrap();
    assert_eq!(first.columns, 90);
    assert_eq!(linked, first);
}

#[test]
fn cached_directories_build_no_candidate_config_paths() {
    let root = dir_with(&[
        PRETTIER,
        (".prettierrc", r#"{ "printWidth": 120 }"#),
        (".editorconfig", "root = true\n[*]\nindent_size = 4\n"),
        ("child/package.json", r#"{ "name": "child" }"#),
    ]);

    let built = || CANDIDATES.get();
    let before = built();
    let width = width_at(&root, "a.ts");
    assert_eq!((width.columns, width.tab), (120, 4));
    let first = built() - before;
    assert_eq!(first, 3);

    assert_eq!(width_at(&root, "b.ts"), width);
    assert_eq!(built() - before, first);

    assert_eq!(width_at(&root, "child/c.ts"), width);
    assert_eq!(built() - before, first + 1);
}

#[test]
fn editorconfig_parsing_is_shared_across_target_files() {
    let root = dir_with(&[PRETTIER, (".editorconfig", "[*]\nmax_line_length = 110\n")]);
    assert_eq!(columns_at(&root, "a.ts"), 110);

    root.write(".editorconfig", "[*]\nmax_line_length = 120\n");
    assert_eq!(columns_at(&root, "b.ts"), 110);
}

#[test]
fn a_comment_after_a_guard_header_prevents_joining() {
    let root = dir_with(&[(".prettierrc", r#"{ "printWidth": 120 }"#)]);
    let text = "function f(a: boolean) {\n  if (a) // TODO (temporary)\n    return a;\n  if (a) return a;\n}\n";
    assert!(fixed(&root, "a.ts", text).is_some_and(|text| text.contains("return a;\n\n  if (a)")));
}

#[test]
fn ties_use_all_dependencies_from_the_first_matching_package() {
    let root = dir_with(&[
        (
            "package.json",
            r#"{ "devDependencies": { "prettier": "3", "oxfmt": "1" } }"#,
        ),
        ("child/.prettierrc", r#"{ "printWidth": 70 }"#),
        ("child/.oxfmtrc.json", r#"{ "printWidth": 120 }"#),
        ("single/.oxfmtrc.json", r#"{ "printWidth": 120 }"#),
    ]);

    let width = width_at(&root, "x.ts");
    assert_eq!(width.columns, 80);
    assert_eq!(width.source, WidthSource::Default(Formatter::Prettier));

    let width = width_at(&root, "child/x.ts");
    assert_eq!(width.columns, 70);
    assert_eq!(width.source, config_source(&root, "child/.prettierrc"));

    let width = width_at(&root, "single/x.ts");
    assert_eq!(width.columns, 120);
    assert_eq!(width.source, config_source(&root, "single/.oxfmtrc.json"));
}

#[test]
fn unread_formatter_configs_are_treated_as_absent() {
    let root = dir_with(&[OXFMT, ("child/.oxfmtrc.json", "{ invalid")]);
    let width = width_at(&root, "child/x.ts");
    assert_eq!(width.columns, 100);
    assert_eq!(width.source, WidthSource::Default(Formatter::Oxfmt));
    assert_eq!(width.unread, [root.join("child/.oxfmtrc.json")]);
}

#[test]
fn a_joined_guard_counts_indentation_tabs_at_the_formatter_tab_width() {
    let text = "function settle(reservation: Reservation) {\n\tif (reservation.active)\n\t\treturn reservation.confirm();\n\tif (reservation.pending)\n\t\treturn reservation.cancel();\n}\n";
    let narrow = dir_with(&[(".prettierrc", r#"{ "printWidth": 58, "tabWidth": 4 }"#)]);
    let wide = dir_with(&[(".prettierrc", r#"{ "printWidth": 58, "tabWidth": 8 }"#)]);

    assert_eq!(fixed(&narrow, "x.ts", text), None);
    assert!(
        fixed(&wide, "x.ts", text)
            .is_some_and(|text| text.contains("return reservation.confirm();\n\n\tif"))
    );
}

#[test]
fn biome_extends_merge_formatter_keys_in_order_before_deciding_activity() {
    let root = dir_with(&[
        (".prettierrc", r#"{ "printWidth": 130 }"#),
        (
            "base.jsonc",
            r#"{ "formatter": { "lineWidth": 90, "indentWidth": 3 } }"#,
        ),
        (
            "next.json",
            r#"{ "formatter": { "lineWidth": 100 }, "javascript": { "formatter": { "indentWidth": 4 } } }"#,
        ),
        (
            "child/biome.json",
            r#"{ "extends": ["../base.jsonc", "../next.json"], "javascript": { "formatter": { "lineWidth": 110 } } }"#,
        ),
        ("child/inherited/biome.json", r#"{ "extends": ["//"] }"#),
    ]);

    for path in ["child/a.ts", "child/inherited/a.ts"] {
        let width = width_at(&root, path);
        assert_eq!((width.columns, width.tab), (110, 4), "{path}");
    }

    let extends = format!(r#"{{ "extends": "{}" }}"#, root.join("base.jsonc"));
    let absolute = dir_with(&[("biome.json", &extends)]);
    let width = width_at(&absolute, "a.ts");
    assert_eq!((width.columns, width.tab), (90, 3));
}

#[test]
fn unread_biome_extends_falls_through_to_an_ancestor_formatter() {
    let root = dir_with(&[
        (".prettierrc", r#"{ "printWidth": 130 }"#),
        (
            "child/biome.json",
            r#"{ "extends": ["@org/config"], "formatter": { "lineWidth": 70 } }"#,
        ),
    ]);

    let width = width_at(&root, "child/a.ts");
    assert_eq!(width.columns, 130);
    assert_eq!(width.unread, [root.join("child/biome.json")]);
}

fn biome_scopes(includes: &str) -> [(&'static str, String); 3] {
    [
        (
            "files",
            format!(r#"{{"formatter":{{"lineWidth":70}},"files":{{"includes":{includes}}}}}"#),
        ),
        (
            "formatter",
            format!(r#"{{"formatter":{{"lineWidth":70,"includes":{includes}}}}}"#),
        ),
        (
            "override",
            format!(
                r#"{{"formatter":{{"lineWidth":70}},"overrides":[{{"includes":{includes},"formatter":{{"lineWidth":60}}}}]}}"#
            ),
        ),
    ]
}

#[test]
fn biome_includes_use_the_last_matching_entry() {
    for (scope, config) in biome_scopes(r#"["**/*.ts","!dist/**","!!dist/reinclude.ts"]"#) {
        let root = dir_with(&[
            (".prettierrc", r#"{ "printWidth": 120 }"#),
            ("child/biome.json", &config),
        ]);

        let over = scope == "override";
        assert_eq!(
            columns_at(&root, "child/src/a.ts"),
            if over { 60 } else { 70 },
            "{scope}"
        );
        assert_eq!(
            columns_at(&root, "child/dist/a.ts"),
            if over { 70 } else { 120 },
            "{scope}"
        );
        assert_eq!(
            columns_at(&root, "child/dist/reinclude.ts"),
            if over { 70 } else { 120 },
            "{scope}"
        );
    }
}

#[test]
fn biome_negated_directory_includes_exclude_descendants() {
    for (scope, config) in biome_scopes(r#"["**/*.ts","!dist"]"#) {
        let root = dir_with(&[
            (".prettierrc", r#"{ "printWidth": 120 }"#),
            ("child/biome.json", &config),
        ]);

        let over = scope == "override";
        assert_eq!(
            columns_at(&root, "child/src/a.ts"),
            if over { 60 } else { 70 },
            "{scope}"
        );
        assert_eq!(
            columns_at(&root, "child/dist/a.ts"),
            if over { 70 } else { 120 },
            "{scope}"
        );
    }
}

#[test]
fn biome_reads_editorconfig_only_next_to_its_config() {
    let root = dir_with(&[
        (
            "biome.json",
            r#"{ "formatter": { "useEditorconfig": true } }"#,
        ),
        (".editorconfig", "[*]\nmax_line_length = 110\n"),
        ("sub/.editorconfig", "[*]\nmax_line_length = 60\n"),
    ]);

    let width = width_at(&root, "sub/a.ts");
    assert_eq!(width.columns, 110);
    assert_eq!(width.source, config_source(&root, ".editorconfig"));
}

#[test]
fn flat_prettier_yaml_accepts_quoted_keys() {
    for (name, key) in [
        (".prettierrc.yaml", r#""printWidth""#),
        (".prettierrc", "'printWidth'"),
    ] {
        let text = format!("{key}: 120\n");
        let root = dir_with(&[(name, &text)]);
        assert_eq!(columns_at(&root, "a.ts"), 120, "{name}");
    }
}

#[test]
fn flat_values_drop_trailing_comments_and_quotes() {
    let root = dir_with(&[(
        ".prettierrc.toml",
        "printWidth = 90 # wide\ntabWidth = \"4\"\n",
    )]);
    let width = width_at(&root, "a.ts");
    assert_eq!((width.columns, width.tab), (90, 2));

    let nested = dir_with(&[
        (
            ".prettierrc.yml",
            "printWidth: 90\noverrides:\n  - files: '*.ts'\n",
        ),
        ("package.json", r#"{ "prettier": { "printWidth": 70 } }"#),
    ]);

    let width = width_at(&nested, "a.ts");
    assert_eq!(width.columns, 70);
    assert_eq!(width.unread, [nested.join(".prettierrc.yml")]);
}

#[test]
fn biome_with_javascript_formatting_disabled_is_skipped() {
    let root = dir_with(&[
        (".prettierrc", r#"{ "printWidth": 120 }"#),
        (
            "sub/biome.json",
            r#"{ "formatter": { "lineWidth": 70 }, "javascript": { "formatter": { "enabled": false } } }"#,
        ),
    ]);

    assert_eq!(columns_at(&root, "sub/a.ts"), 120);
}

#[test]
fn joined_guards_expand_tabs_after_the_indent() {
    let root = dir_with(&[(".prettierrc", r#"{ "printWidth": 22, "tabWidth": 4 }"#)]);
    let source = "function f(x: boolean) {\n  if (x)\n    return \"a\tb\";\n  if (x)\n    return \"a\tb\";\n}\n";
    assert!(
        fixed(&root, "a.ts", source).is_some_and(|text| text.contains("return \"a\tb\";\n\n  if"))
    );
}

#[test]
fn editorconfig_section_patterns_follow_editorconfig_globs() {
    let root = dir_with(&[
        PRETTIER,
        (
            ".editorconfig",
            "[*]\nmax_line_length = 80\n[/src/{a,b}[!0-9]?.ts]\nmax_line_length = 111\n[**/file{1..3}.ts]\nmax_line_length = 122\n",
        ),
    ]);

    assert_eq!(columns_at(&root, "src/abx.ts"), 111);
    assert_eq!(columns_at(&root, "deep/file2.ts"), 122);
    assert_eq!(columns_at(&root, "src/a1x.ts"), 80);
}

#[test]
fn editorconfig_numeric_ranges_match_without_expansion() {
    let root = dir_with(&[
        PRETTIER,
        (
            ".editorconfig",
            "[*]\nmax_line_length = 80\n[{0..4294967295}.ts]\nmax_line_length = 111\n",
        ),
    ]);

    assert_eq!(columns_at(&root, "7.ts"), 111);
    assert_eq!(columns_at(&root, "4294967295.ts"), 111);
    assert_eq!(columns_at(&root, "4294967296.ts"), 80);
    assert_eq!(columns_at(&root, "x.ts"), 80);
}

#[test]
fn editorconfig_numeric_ranges_match_only_canonical_integers() {
    let root = dir_with(&[
        PRETTIER,
        (
            ".editorconfig",
            "[*]\nmax_line_length = 80\n[{-2..9}.ts]\nmax_line_length = 111\n",
        ),
    ]);

    assert_eq!(columns_at(&root, "7.ts"), 111);
    assert_eq!(columns_at(&root, "-2.ts"), 111);
    assert_eq!(columns_at(&root, "+7.ts"), 80);
    assert_eq!(columns_at(&root, "07.ts"), 80);
}

#[test]
fn editorconfig_leading_slash_anchors_a_basename_pattern() {
    let root = dir_with(&[
        PRETTIER,
        (
            ".editorconfig",
            "[*]\nmax_line_length = 80\n[/foo.ts]\nmax_line_length = 111\n",
        ),
    ]);

    assert_eq!(columns_at(&root, "foo.ts"), 111);
    assert_eq!(columns_at(&root, "sub/foo.ts"), 80);
}
