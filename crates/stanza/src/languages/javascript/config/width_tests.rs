use std::cell::Cell;
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
    let result = format_text(
        &root.join(path),
        text,
        Mode::Fix,
        None,
        &mut Unread::default(),
        None,
    );

    result.fixed.map(|fixed| {
        assert_eq!(fixed.error, None);
        fixed.text
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Taken {
    Width(usize),
    Default,
    Refused,
}

const EDITOR_FORMATTERS: [(Formatter, &str); 2] = [
    (Formatter::Prettier, ".prettierrc.json"),
    (Formatter::Oxfmt, ".oxfmtrc.json"),
];

fn taken(formatter: Formatter, config: &str, editorconfig: &str) -> Taken {
    let root = dir_with(&[(config, "{}"), (".editorconfig", editorconfig)]);
    let width = width_at(&root, "a.ts");
    let editor = root.join(".editorconfig");
    if width.source == WidthSource::Config(editor.clone()) {
        assert!(width.unread.is_empty(), "{editorconfig:?}");
        return Taken::Width(width.columns);
    }

    assert_eq!(
        width.source,
        WidthSource::Default(formatter),
        "{editorconfig:?}"
    );
    assert_eq!(
        width.columns,
        formatter.keys().default_width,
        "{editorconfig:?}"
    );
    if width.unread.is_empty() {
        Taken::Default
    } else {
        assert_eq!(width.unread, [editor], "{editorconfig:?}");
        Taken::Refused
    }
}

#[test]
fn editorconfig_numeric_rules() {
    let rows = [
        ("120", Taken::Width(120), Taken::Width(120)),
        (" 120 ", Taken::Width(120), Taken::Width(120)),
        ("1.2e2", Taken::Width(120), Taken::Default),
        ("120.0", Taken::Width(120), Taken::Default),
        ("0x78", Taken::Default, Taken::Default),
        ("120 # x", Taken::Default, Taken::Default),
        ("120 ; x", Taken::Default, Taken::Default),
        ("+120", Taken::Default, Taken::Width(120)),
        ("0120", Taken::Default, Taken::Width(120)),
        ("-120", Taken::Default, Taken::Default),
        ("9007199254740993", Taken::Default, Taken::Width(1)),
        (
            "9007199254740991",
            Taken::Width(9_007_199_254_740_991),
            Taken::Refused,
        ),
        ("1e400", Taken::Default, Taken::Default),
        ("0", Taken::Default, Taken::Refused),
        ("320", Taken::Width(320), Taken::Width(320)),
        ("321", Taken::Width(321), Taken::Refused),
        ("65656", Taken::Width(65656), Taken::Width(120)),
        ("18446744073709551616", Taken::Default, Taken::Default),
    ];

    for (value, prettier, oxfmt) in rows {
        let editorconfig = format!("root = true\n[*]\nmax_line_length = {value}\n");
        let answers =
            EDITOR_FORMATTERS.map(|(formatter, config)| taken(formatter, config, &editorconfig));

        assert_eq!(answers, [prettier, oxfmt], "{value:?}");
    }
}

#[test]
fn editorconfig_property_rules() {
    let rows = [
        (
            "max_line_length = 120\ntab_width = 8\n[*]\n",
            (80, 2),
            (100, 2),
        ),
        ("[*]\nmax_line_length = 120\nnonsense\n", (80, 2), (120, 2)),
        ("[*]\nmax_line_length = 120\n[*.ts\n", (80, 2), (120, 2)),
        (
            "[*]\nindent_style = space\nindent_size = 8\ntab_width = 2\n",
            (80, 8),
            (100, 8),
        ),
        ("[*]\nMAX_LINE_LENGTH = 120\n", (120, 2), (100, 2)),
        ("[*]\nindent_size = 4\n", (80, 4), (100, 2)),
        ("[*]\ntab_width = 0\n", (80, 2), (100, 2)),
        (
            "[*]\nmax_line_length = 120\n[*.ts]\nmax_line_length = abc\n",
            (80, 2),
            (120, 2),
        ),
        (
            "[*]\nmax_line_length = 120\n[*.ts]\nmax_line_length = unset\n",
            (80, 2),
            (100, 2),
        ),
        (
            "[*]\nmax_line_length = 120\n[]\nmax_line_length = 40\n",
            (120, 2),
            (120, 2),
        ),
        (
            "[*]\nindent_size = 8\ntab_width = unset\n",
            (80, 2),
            (100, 2),
        ),
        (
            "[*]\nmax_line_length = 120\nmax_line_length = abc\n",
            (80, 2),
            (100, 2),
        ),
        ("\u{feff}[*]\nmax_line_length = 40\n", (40, 2), (100, 2)),
        (
            "[*]\nmax_line_length = 120\n[a\u{2028}b]\nmax_line_length = 50\n",
            (120, 2),
            (120, 2),
        ),
        (
            "[*]\nmax_line_length = 120\n[*.ts]\nmax_line_length = 80\n[*]\nmax_line_length = 100\n",
            (80, 2),
            (100, 2),
        ),
        (
            "[*]\nmax_line_length = 120\nMAX_LINE_LENGTH = 80\nmax_line_length = 100\n",
            (80, 2),
            (100, 2),
        ),
        (
            "[*]\nindent_style = \"space\"\nindent_size = 8\ntab_width = 2\n",
            (80, 8),
            (100, 2),
        ),
        ("[*]\ntab_width = 256\n", (80, 256), (100, 2)),
        (
            "[*]\nmax_line_length = 120\n[*.md]\nmax_line_length = 400\n",
            (120, 2),
            (120, 2),
        ),
    ];

    for (editorconfig, prettier, oxfmt) in rows {
        let answers = EDITOR_FORMATTERS.map(|(_, config)| {
            let root = dir_with(&[(config, "{}"), (".editorconfig", editorconfig)]);
            let width = width_at(&root, "a.ts");
            assert!(width.unread.is_empty(), "{editorconfig:?}");
            (width.columns, width.tab)
        });

        assert_eq!(answers, [prettier, oxfmt], "{editorconfig:?}");
    }

    for (_, config) in EDITOR_FORMATTERS {
        let root = dir_with(&[
            (config, r#"{ "printWidth": 90, "tabWidth": 3 }"#),
            (".editorconfig", "[*]\nmax_line_length = 0\ntab_width = 8\n"),
        ]);

        let width = width_at(&root, "a.ts");
        assert_eq!((width.columns, width.tab), (90, 3), "{config}");
        assert_eq!(width.source, config_source(&root, config));
        assert!(width.unread.is_empty(), "{config}");
    }

    for editorconfig in [
        "[*]\nmax_line_length = 400\n[*.ts]\nmax_line_length = 90\n",
        "[*]\nmax_line_length = 120\n[*.ts]\nmax_line_length = 400\n",
        "[*]\ntab_width = 25\n",
    ] {
        assert_eq!(
            taken(Formatter::Oxfmt, ".oxfmtrc.json", editorconfig),
            Taken::Refused,
            "{editorconfig:?}"
        );
    }

    let off = dir_with(&[
        (".oxfmtrc.json", "{}"),
        (
            ".editorconfig",
            "[*]\nmax_line_length = 120\n[*.ts]\nmax_line_length = OFF\n",
        ),
    ]);

    assert_eq!(columns_at(&off, "a.ts"), 100);

    let tab_size = dir_with(&[
        (".oxfmtrc.json", "{}"),
        (
            ".editorconfig",
            "[*]\nindent_style = space\nindent_size = 4\ntab_width = 8\n[*.ts]\nindent_size = tab\n",
        ),
    ]);

    assert_eq!(width_at(&tab_size, "a.ts").tab, 4);

    let use_tabs = dir_with(&[
        (".oxfmtrc.json", r#"{ "useTabs": false }"#),
        (".editorconfig", "[*]\nindent_size = 8\ntab_width = 2\n"),
    ]);

    assert_eq!(width_at(&use_tabs, "a.ts").tab, 8);

    let unknown_tabs = dir_with(&[(
        "oxfmt.config.mjs",
        r#"export default { printWidth: 120, useTabs: process.env.TABS === "1" };"#,
    )]);

    let width = width_at(&unknown_tabs, "a.ts");
    assert_eq!(width.columns, 120);
    assert!(width.unread.is_empty());

    let undecided = dir_with(&[
        (
            "oxfmt.config.mjs",
            r#"export default { printWidth: 120, useTabs: process.env.TABS === "1" };"#,
        ),
        (
            ".editorconfig",
            "[*]\nindent_style = space\nindent_size = 8\ntab_width = 4\n",
        ),
    ]);

    let width = width_at(&undecided, "a.ts");
    assert_eq!((width.columns, width.tab), (120, 2));
    assert_eq!(width.unread, [undecided.join(".editorconfig")]);

    let narrowed = dir_with(&[
        (
            "oxfmt.config.mjs",
            r#"export default { useTabs: process.env.TABS === "1" };"#,
        ),
        (
            ".editorconfig",
            "[*]\nindent_style = space\nindent_size = 260\ntab_width = 4\n",
        ),
    ]);

    let width = width_at(&narrowed, "a.ts");
    assert_eq!(width.tab, 4);
    assert!(width.unread.is_empty());

    let numbered = dir_with(&[
        PRETTIER,
        (
            ".editorconfig",
            "[*]\nmax_line_length = 120\n[1]\nmax_line_length = 50\n",
        ),
    ]);

    assert_eq!(columns_at(&numbered, "1"), 120);

    let root_section = dir_with(&[
        PRETTIER,
        (".editorconfig", "[*]\nmax_line_length = 120\n"),
        (
            "child/.editorconfig",
            "root = true\n[root]\nindent_size = 8\n",
        ),
    ]);

    assert_eq!(columns_at(&root_section, "child/a.ts"), 120);

    let unset = dir_with(&[
        PRETTIER,
        (".editorconfig", "[*]\nmax_line_length = 120\n"),
        ("child/.editorconfig", "[*]\nmax_line_length = unset\n"),
    ]);

    assert_eq!(columns_at(&unset, "child/a.ts"), 80);

    let malformed = dir_with(&[
        PRETTIER,
        (".editorconfig", "[*]\nmax_line_length = 120\n"),
        (
            "child/.editorconfig",
            "root = true\n[*]\nmax_line_length = 60\nnonsense\n",
        ),
    ]);

    let width = width_at(&malformed, "child/a.ts");
    assert_eq!(width.columns, 120);
    assert_eq!(width.source, config_source(&malformed, ".editorconfig"));

    let tabs = dir_with(&[
        PRETTIER,
        (
            ".editorconfig",
            "[*]\nindent_style = space\nindent_size = 8\n",
        ),
        ("child/.editorconfig", "[*]\ntab_width = 2\n"),
    ]);

    assert_eq!(width_at(&tabs, "child/a.ts").tab, 8);

    let commented_root = dir_with(&[
        PRETTIER,
        (".editorconfig", "[*]\ntab_width = 7\n"),
        (
            "child/.editorconfig",
            "root = true # x\n[*]\nmax_line_length = 60\n",
        ),
    ]);

    let width = width_at(&commented_root, "child/a.ts");
    assert_eq!((width.columns, width.tab), (60, 2));
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
        (usize::MAX, "1000000000000000000000", 3)
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
fn oxfmt_editorconfig_anchor() {
    for config in [(".oxfmtrc.json", "{}"), OXFMT] {
        let root = dir_with(&[config, ("child/.editorconfig", "[*]\ntab_width = 4\n")]);

        let width = width_at(&root, "child/a.ts");
        assert_eq!((width.columns, width.tab), (100, 2), "{config:?}");
        assert_eq!(width.source, WidthSource::Default(Formatter::Oxfmt));

        let root = dir_with(&[
            config,
            (".editorconfig", "root = true\n[*]\nmax_line_length = 40\n"),
            ("child/.editorconfig", "[*]\ntab_width = 4\n"),
        ]);

        let width = width_at(&root, "child/a.ts");
        assert_eq!((width.columns, width.tab), (40, 2), "{config:?}");
        assert_eq!(width.source, config_source(&root, ".editorconfig"));

        let nested = dir_with(&[
            (&format!("pkg/{}", config.0), config.1),
            (".editorconfig", "root = true\n[*]\nmax_line_length = 40\n"),
        ]);

        assert_eq!(columns_at(&nested, "pkg/a.ts"), 100, "{config:?}");
    }

    let config_over_manifest = dir_with(&[
        (".oxfmtrc.json", "{}"),
        (".editorconfig", "root = true\n[*]\nmax_line_length = 40\n"),
        ("app/package.json", OXFMT.1),
        (
            "app/.editorconfig",
            "root = true\n[*]\nmax_line_length = 60\n",
        ),
    ]);

    assert_eq!(columns_at(&config_over_manifest, "app/a.ts"), 40);

    let ranked = dir_with(&[
        (".oxfmtrc.json", "{}"),
        (".prettierrc.json", r#"{ "printWidth": 90 }"#),
        ("child/.editorconfig", "[*]\nmax_line_length = 60\n"),
    ]);

    let width = width_at(&ranked, "child/a.ts");
    assert_eq!(width.columns, 90);
    assert_eq!(width.source, config_source(&ranked, ".prettierrc.json"));
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
        "if (a) return a;\n",
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
fn prettier_editorconfig_boundaries() {
    for (marker, kind, columns) in [
        (".hg", "file", 120),
        (".hg", "directory", 80),
        (".hg", "link to a directory", 120),
        (".git", "file", 80),
        (".git", "directory", 80),
        (".git", "dangling link", 80),
    ] {
        let root = dir_with(&[
            (".prettierrc.json", "{}"),
            (".editorconfig", "[*]\nmax_line_length = 120\n"),
            ("p/.editorconfig", "[*]\ntab_width = 7\n"),
            ("target/.keep", ""),
        ]);

        let path = root.join(&format!("p/{marker}"));
        match kind {
            "file" => fs::write(&path, "gitdir: elsewhere\n").unwrap(),
            "directory" => fs::create_dir(&path).unwrap(),
            "link to a directory" => symlink(root.join("target"), &path).unwrap(),
            _ => symlink(root.join("missing"), &path).unwrap(),
        }

        let width = width_at(&root, "p/a.ts");
        let found = (marker, kind, width.columns, width.tab);
        assert_eq!(found, (marker, kind, columns, 7));
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
fn an_emoji_guard_joins_at_its_formatters_measure() {
    let guard = "function f(a) {\n  if (a) {\n    log(\"👍🏽\");\n  }\n}\n";
    for (config, template, joins) in [
        (".prettierrc.json", r#"{"printWidth":COLUMNS}"#, 19),
        (".oxfmtrc.json", r#"{"printWidth":COLUMNS}"#, 19),
        ("biome.json", r#"{"formatter":{"lineWidth":COLUMNS}}"#, 21),
    ] {
        for (columns, joined) in [(joins - 1, false), (joins, true)] {
            let value = template.replace("COLUMNS", &columns.to_string());
            let root = dir_with(&[(config, &value)]);
            let text = fixed(&root, "a.ts", guard).expect("the braces come off");
            assert_eq!(
                text.contains("  if (a) log("),
                joined,
                "{config} at {columns}"
            );
        }
    }
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

thread_local! {
    pub(super) static BIOME_READS: Cell<usize> = const { Cell::new(0) };
    pub(super) static BIOME_TARGETS: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn biome_chain(files: usize) -> Scratch {
    let root = dir_with(&[("biome.json", r#"{ "extends": ["./c1.json"] }"#)]);
    for index in 1..files {
        let next = index + 1;
        root.write(
            &format!("c{index}.json"),
            &format!(r#"{{ "extends": ["./c{next}.json"] }}"#),
        );
    }

    root.write(
        &format!("c{files}.json"),
        r#"{ "formatter": { "lineWidth": 90 } }"#,
    );
    root
}

#[test]
fn biome_width_depth_boundary() {
    for (files, columns) in [(10, 90), (11, 80)] {
        for warm in [false, true] {
            let root = biome_chain(files);
            if warm {
                root.write("short/biome.json", r#"{ "extends": ["../c2.json"] }"#);
                let short = width_at(&root, "short/a.ts");
                assert_eq!((short.columns, short.unread.len()), (90, 0), "{files}");
            }

            let width = width_at(&root, "a.ts");
            let unread = if columns == 90 {
                Vec::new()
            } else {
                vec![root.join("biome.json")]
            };

            assert_eq!(
                (width.columns, width.unread),
                (columns, unread),
                "{files} {warm}"
            );
        }
    }
}

#[test]
fn biome_width_depth_boundary_resolves_no_package_past_it() {
    for (files, columns, targets) in [(9, 90, 10), (10, 80, 10)] {
        let root = biome_chain(files);
        root.write(
            &format!("c{files}.json"),
            r#"{ "extends": ["@org/biome-config/biome"] }"#,
        );
        root.write(
            "node_modules/@org/biome-config/package.json",
            r#"{ "exports": { "./biome": "./biome.json" } }"#,
        );
        root.write(
            "node_modules/@org/biome-config/biome.json",
            r#"{ "formatter": { "lineWidth": 90 } }"#,
        );

        BIOME_TARGETS.with(|count| count.set(0));
        let width = width_at(&root, "a.ts");
        assert_eq!(
            (
                width.columns,
                width.unread.len(),
                BIOME_TARGETS.with(Cell::get)
            ),
            (columns, usize::from(columns == 80), targets),
            "{files}"
        );
    }
}

#[test]
fn biome_width_merge_paths() {
    let siblings = |extends: &str| {
        dir_with(&[
            ("biome.json", &format!(r#"{{ "extends": {extends} }}"#)),
            ("shared.json", r#"{ "formatter": { "lineWidth": 60 } }"#),
            (
                "a.json",
                r#"{ "extends": ["./shared.json"], "formatter": { "lineWidth": 70 } }"#,
            ),
            (
                "b.json",
                r#"{ "extends": ["./shared.json"], "formatter": { "lineWidth": 90 } }"#,
            ),
        ])
    };

    for (extends, columns) in [
        (r#"["./a.json", "./b.json"]"#, 90),
        (r#"["./a.json", "./a.json", "./b.json"]"#, 90),
        (r#"["./b.json", "./a.json"]"#, 70),
    ] {
        let width = width_at(&siblings(extends), "a.ts");
        assert_eq!(
            (width.columns, width.unread.len()),
            (columns, 0),
            "{extends}"
        );
    }

    let cycles = [
        dir_with(&[
            (
                "one/biome.json",
                r#"{ "extends": ["./biome.json"], "formatter": { "lineWidth": 90 } }"#,
            ),
            (
                "two/biome.json",
                r#"{ "extends": ["../one/biome.json"], "formatter": { "lineWidth": 90 } }"#,
            ),
        ]),
        dir_with(&[
            (
                "one/biome.json",
                r#"{ "extends": ["../two/biome.json"], "formatter": { "lineWidth": 90 } }"#,
            ),
            (
                "two/biome.json",
                r#"{ "extends": ["../one/biome.json"], "formatter": { "lineWidth": 90 } }"#,
            ),
        ]),
    ];

    for (index, root) in cycles.iter().enumerate() {
        for entry in ["one", "two"] {
            let width = width_at(root, &format!("{entry}/a.ts"));
            assert_eq!(
                (width.columns, width.unread),
                (80, vec![root.join(&format!("{entry}/biome.json"))]),
                "{index} {entry}"
            );
        }
    }
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

const SHARED_PRETTIER: [(&str, &str); 2] = [
    (
        "node_modules/@org/prettier-config/package.json",
        r#"{"main":"index.json"}"#,
    ),
    (
        "node_modules/@org/prettier-config/index.json",
        r#"{"printWidth":60}"#,
    ),
];

const PRETTIER_PACKAGE: &str = "node_modules/@org/prettier-config/package.json";
const PRETTIER_SELECTOR: (&str, &str) = (".prettierrc", r#""@org/prettier-config""#);

#[test]
fn shared_prettier_export_patterns_and_arrays() {
    let cases = [
        (
            r#"{"./*":"./configs/*.json"}"#,
            "@org/prettier-config/base",
            60,
        ),
        (
            r#"{"./*":"./wide/*.json","./narrow/*":"./configs/*.json"}"#,
            "@org/prettier-config/narrow/base",
            60,
        ),
        (r#"["./configs/base.json"]"#, "@org/prettier-config", 60),
        (
            r#"{".":["configs/base.json","./configs/base.json"]}"#,
            "@org/prettier-config",
            60,
        ),
        (
            r#"{"./*":"./configs/*.json"}"#,
            "@org/prettier-config/../base",
            80,
        ),
    ];

    for (exports, specifier, columns) in cases {
        let manifest = format!(r#"{{"exports":{exports}}}"#);
        let selector = format!(r#""{specifier}""#);
        let root = dir_with(&[
            (PRETTIER_PACKAGE, &manifest),
            (
                "node_modules/@org/prettier-config/configs/base.json",
                r#"{"printWidth":60}"#,
            ),
            (
                "node_modules/@org/prettier-config/wide/narrow/base.json",
                r#"{"printWidth":100}"#,
            ),
            (
                "node_modules/@org/prettier-config/base.json",
                r#"{"printWidth":60}"#,
            ),
            (".prettierrc", &selector),
        ]);

        let width = width_at(&root, "a.ts");
        assert_eq!(width.columns, columns, "{exports} {specifier}");
        assert_eq!(
            width.unread.is_empty(),
            columns == 60,
            "{exports} {specifier}"
        );
    }
}

#[test]
fn shared_prettier_width() {
    let read: [&[(&str, &str)]; 6] = [
        &[
            SHARED_PRETTIER[0],
            SHARED_PRETTIER[1],
            ("package.json", r#"{"prettier":"@org/prettier-config"}"#),
        ],
        &[SHARED_PRETTIER[0], SHARED_PRETTIER[1], PRETTIER_SELECTOR],
        &[
            (
                "node_modules/prettier-shared/package.json",
                r#"{"main":"index.json"}"#,
            ),
            (
                "node_modules/prettier-shared/index.json",
                r#"{"printWidth":60}"#,
            ),
            (".prettierrc", "prettier-shared\n"),
        ],
        &[
            (
                "node_modules/prettier-shared/package.json",
                r#"{"main":"index.json"}"#,
            ),
            (
                "node_modules/prettier-shared/index.json",
                r#"{"printWidth":60}"#,
            ),
            (".prettierrc", "\u{feff}prettier-shared\n"),
        ],
        &[
            (
                PRETTIER_PACKAGE,
                r#"{"exports":{".":{"node-addons":"./index.json","default":"./other.json"}}}"#,
            ),
            SHARED_PRETTIER[1],
            (
                "node_modules/@org/prettier-config/other.json",
                r#"{"printWidth":100}"#,
            ),
            PRETTIER_SELECTOR,
        ],
        &[
            (
                PRETTIER_PACKAGE,
                r#"{"exports":{".":{"import":"./index.mjs"}}}"#,
            ),
            (
                "node_modules/@org/prettier-config/index.mjs",
                "export default { printWidth: 60 };\n",
            ),
            PRETTIER_SELECTOR,
        ],
    ];

    for files in read {
        let root = dir_with(files);
        let selector = files[files.len() - 1].0;
        let width = width_at(&root, "a.ts");
        assert_eq!(width.columns, 60, "{files:?}");
        assert_eq!(width.source, config_source(&root, selector), "{files:?}");
        assert!(width.unread.is_empty(), "{files:?}");
    }

    let unread: [&[(&str, &str)]; 15] = [
        &[(".prettierrc", r#""@org/missing-config""#)],
        &[
            ("base.mjs", "export const printWidth = 60;\n"),
            (".prettierrc", r#""./base.mjs""#),
        ],
        &[
            ("base.json", r#""./other.json""#),
            ("other.json", r#"{"printWidth":60}"#),
            (".prettierrc", r#""./base.json""#),
        ],
        &[
            ("base.json", r#"{"printWidth":60,}"#),
            (".prettierrc", r#""./base.json""#),
        ],
        &[
            ("base.config", r#"{"printWidth":60}"#),
            (".prettierrc", r#""./base.config""#),
        ],
        &[
            (
                PRETTIER_PACKAGE,
                r#"{"main":"legacy.json","exports":{".":{"import":"./index.json"}}}"#,
            ),
            SHARED_PRETTIER[1],
            (
                "node_modules/@org/prettier-config/legacy.json",
                r#"{"printWidth":60}"#,
            ),
            PRETTIER_SELECTOR,
        ],
        &[
            (
                PRETTIER_PACKAGE,
                r#"{"exports":{".":{"import":"./config"}}}"#,
            ),
            (
                "node_modules/@org/prettier-config/config.mjs",
                "export default { printWidth: 60 };\n",
            ),
            PRETTIER_SELECTOR,
        ],
        &[
            SHARED_PRETTIER[0],
            SHARED_PRETTIER[1],
            (".prettierrc", "@org/prettier-config\n"),
        ],
        &[
            SHARED_PRETTIER[0],
            SHARED_PRETTIER[1],
            (".prettierrc", r#""@org/prettier-config" // shared"#),
        ],
        &[
            (PRETTIER_PACKAGE, r#"{"exports":{".":"index.json"}}"#),
            SHARED_PRETTIER[1],
            PRETTIER_SELECTOR,
        ],
        &[
            (
                PRETTIER_PACKAGE,
                r#"{"exports":{".":{"node":null,"default":"./index.json"}}}"#,
            ),
            SHARED_PRETTIER[1],
            PRETTIER_SELECTOR,
        ],
        &[
            (
                PRETTIER_PACKAGE,
                r#"{"exports":{".":"./%2e%2e/index.json"}}"#,
            ),
            (
                "node_modules/@org/prettier-config/%2e%2e/index.json",
                r#"{"printWidth":60}"#,
            ),
            PRETTIER_SELECTOR,
        ],
        &[
            (
                PRETTIER_PACKAGE,
                r#"{"exports":{".":"./index.json","default":"./index.json"}}"#,
            ),
            SHARED_PRETTIER[1],
            PRETTIER_SELECTOR,
        ],
        &[
            (
                PRETTIER_PACKAGE,
                r#"{"exports":{".":{"0":"./unused.json","default":"./index.json"}}}"#,
            ),
            SHARED_PRETTIER[1],
            PRETTIER_SELECTOR,
        ],
        &[
            (PRETTIER_PACKAGE, r#"{"main":"config"}"#),
            (
                "node_modules/@org/prettier-config/config.mjs",
                "export default { printWidth: 60 };\n",
            ),
            PRETTIER_SELECTOR,
        ],
    ];

    for files in unread {
        let root = dir_with(files);
        let width = width_at(&root, "a.ts");
        assert_eq!(width.columns, 80, "{files:?}");
        assert_eq!(width.unread, [root.join(".prettierrc")], "{files:?}");
    }

    for value in [r#""""#, "null", "false", "0"] {
        let manifest = format!(r#"{{"prettier":{value}}}"#);
        let root = dir_with(&[
            (".prettierrc", r#"{"printWidth":70}"#),
            ("child/package.json", &manifest),
        ]);

        let width = width_at(&root, "child/a.ts");
        assert_eq!(width.source, config_source(&root, ".prettierrc"), "{value}");
        assert!(width.unread.is_empty(), "{value}");
    }
}

#[test]
fn shared_prettier_overrides() {
    let root = dir_with(&[
        (".prettierrc", r#""./config/base.json""#),
        (
            "config/base.json",
            r#"{"printWidth":100,"overrides":[{"files":"src/*.ts","options":{"printWidth":40}}]}"#,
        ),
    ]);

    for (path, columns) in [("src/a.ts", 40), ("config/src/a.ts", 100)] {
        let width = width_at(&root, path);
        assert_eq!(width.columns, columns, "{path}");
        assert_eq!(width.source, config_source(&root, ".prettierrc"), "{path}");
        assert!(width.unread.is_empty(), "{path}");
    }
}

fn biome_package(exports: &str, files: &[(&str, &str)], local: &str) -> Scratch {
    let root = dir_with(&[
        (".prettierrc", r#"{ "printWidth": 130 }"#),
        (
            "child/node_modules/@org/biome-config/package.json",
            &format!(r#"{{"name":"@org/biome-config","exports":{exports}}}"#),
        ),
        ("child/biome.json", local),
    ]);

    for (name, text) in files {
        root.write(
            &format!("child/node_modules/@org/biome-config/{name}"),
            text,
        );
    }

    root
}

#[test]
fn shared_biome_width() {
    let exported = r#"{"./biome":"./biome.json"}"#;
    let shared = [("biome.json", r#"{"formatter":{"lineWidth":60}}"#)];
    let extends = r#"{"extends":["@org/biome-config/biome"]}"#;
    let local =
        r#"{"extends":["@org/biome-config/biome"],"javascript":{"formatter":{"lineWidth":70}}}"#;
    let suffixed = [("biome.config", r#"{"formatter":{"lineWidth":60}}"#)];
    let named = r#"{"extends":["@org/biome-config/biome.json"]}"#;
    let read = [
        (
            biome_package(r#"{"./biome.json":"./biome.json"}"#, &shared, named),
            60,
        ),
        (biome_package(exported, &shared, extends), 60),
        (biome_package(exported, &shared, local), 70),
        (
            biome_package(r#"{"./biome":"./biome.config"}"#, &suffixed, extends),
            60,
        ),
    ];

    for (root, columns) in read {
        let width = width_at(&root, "child/a.ts");
        assert_eq!(width.columns, columns);
        assert_eq!(width.source, config_source(&root, "child/biome.json"));
        assert!(width.unread.is_empty());
    }

    let beside = biome_package(r#"{"./biome.json":"./biome.json"}"#, &shared, named);
    beside.write(
        "child/@org/biome-config/biome.json",
        r#"{"formatter":{"lineWidth":90}}"#,
    );
    assert_eq!(width_at(&beside, "child/a.ts").columns, 90);

    let script = [(
        "biome.js",
        "export default { formatter: { lineWidth: 60 } };\n",
    )];
    let cycle = [("biome.json", extends)];
    let other = [
        ("other.json", r#"{"formatter":{"lineWidth":60}}"#),
        ("biome.json", r#"{"formatter":{"lineWidth":60}}"#),
    ];
    let unread = [
        biome_package(r#"{"./other":"./other.json"}"#, &other, extends),
        biome_package(r#"{"./biome":"./biome.js"}"#, &script, extends),
        biome_package(exported, &cycle, extends),
    ];

    for root in unread {
        let width = width_at(&root, "child/a.ts");
        assert_eq!(width.columns, 130);
        assert_eq!(width.source, config_source(&root, ".prettierrc"));
        assert_eq!(width.unread, [root.join("child/biome.json")]);
    }
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
        (".editorconfig", "[*]\nindent_size = 5\n"),
        ("sub/.editorconfig", "[*]\nindent_size = 3\n"),
    ]);

    let width = width_at(&root, "sub/a.ts");
    assert_eq!((width.columns, width.tab), (80, 5));
    assert_eq!(width.source, WidthSource::Default(Formatter::Biome));

    let nested = dir_with(&[
        (
            "pkg/biome.json",
            r#"{ "formatter": { "useEditorconfig": true } }"#,
        ),
        (".editorconfig", "[*]\nindent_size = 4\n"),
    ]);

    assert_eq!(width_at(&nested, "pkg/a.ts").tab, 2);

    for (size, tab, unread) in [(24, 24, false), (25, 2, true)] {
        let editorconfig = format!("[*]\nindent_size = {size}\n");
        let root = dir_with(&[
            (
                "biome.json",
                r#"{ "formatter": { "useEditorconfig": true } }"#,
            ),
            (".editorconfig", &editorconfig),
        ]);

        let width = width_at(&root, "a.ts");
        assert_eq!(
            (width.tab, !width.unread.is_empty()),
            (tab, unread),
            "{size}"
        );
    }
}

#[test]
fn biome_editorconfig_properties() {
    let editorconfig = |size: u8| {
        format!("root = true\n[*]\nmax_line_length = 120\ntab_width = 8\nindent_size = {size}\n")
    };

    let rows = [
        (r#"{"formatter":{"useEditorconfig":true}}"#, 2, (80, 2)),
        (
            r#"{"formatter":{"useEditorconfig":true,"lineWidth":90}}"#,
            2,
            (90, 2),
        ),
        (r#"{"formatter":{"useEditorconfig":true}}"#, 3, (80, 3)),
        (r#"{"formatter":{"useEditorconfig":false}}"#, 3, (80, 2)),
    ];

    for (config, size, expected) in rows {
        let root = dir_with(&[
            ("biome.json", config),
            (".editorconfig", &editorconfig(size)),
        ]);
        let width = width_at(&root, "a.ts");
        assert_eq!((width.columns, width.tab), expected, "{config} {size}");
        assert!(width.unread.is_empty(), "{config} {size}");
    }

    for (editorconfig, tab) in [
        ("[*]\nindent_size = tab\ntab_width = 8\n", Some(4)),
        ("indent_size = 30\n[*]\nindent_size = 4\n", Some(4)),
        ("[*]\nindent_size = 4\nnonsense\ntab_width = x\n", Some(4)),
        (
            "[*]\nindent_size = 4\n[*.md]\nmax_line_length = x\n",
            Some(4),
        ),
        (
            "[*]\nindent_size = 4\n[*.md]\nindent_style = off\n",
            Some(4),
        ),
        ("[*]\nindent_size = 4\n[*.md]\nindent_size = off\n", Some(4)),
        ("[*]\nindent_size = 4\n[*.md]\nend_of_line = off\n", Some(4)),
        (
            "[*]\nindent_size = 4\n[*.md]\ninsert_final_newline = off\n",
            Some(4),
        ),
        ("[*]\nindent_size = 4\n[*.ts]\nindent_size = off\n", Some(2)),
        ("root = off\n[*]\nindent_size = 4\n", None),
        ("[*]\nindent_size = 4\n[*.md]\nindent_size = 30\n", None),
        ("[*]\nindent_size = 30\nindent_size = 4\n", None),
        ("[*]\nindent_size = 4\n[]\nindent_size = TAB\n", None),
        ("[*]\nindent_size = 4\n[a\n", None),
        ("root = TRUE\n[*]\nindent_size = 4\n", None),
        ("[*]\nindent_size = 4\n[*.md]\nindent_style = Tab\n", None),
        ("[*]\nindent_size = 4\n[*.md]\nend_of_line = LF\n", None),
        (
            "[*]\nindent_size = 4\n[*.md]\ninsert_final_newline = x\n",
            None,
        ),
    ] {
        let root = dir_with(&[
            ("biome.json", r#"{"formatter":{"useEditorconfig":true}}"#),
            (".editorconfig", editorconfig),
        ]);

        let width = width_at(&root, "a.ts");
        let expected = match tab {
            Some(tab) => (tab, Vec::new()),
            None => (2, vec![root.join(".editorconfig")]),
        };

        assert_eq!((width.tab, width.unread), expected, "{editorconfig:?}");
    }

    for (use_editorconfig, unread) in [(true, true), (false, false)] {
        let config = format!(r#"{{"formatter":{{"useEditorconfig":{use_editorconfig}}}}}"#);
        let root = dir_with(&[("biome.json", &config)]);
        fs::create_dir(root.join(".editorconfig")).unwrap();
        let width = width_at(&root, "a.ts");
        assert_eq!(!width.unread.is_empty(), unread, "{config}");
    }
}

#[test]
fn biome_editorconfig_reader_rules() {
    let rows = [
        ("[*]\nmax_line_length = 120\n", (80, 2)),
        ("[*]\nmax_line_length = 400\n", (80, 2)),
        ("[*]\nindent_size = 4\n", (80, 4)),
        ("[*]\nindent_size = +4\n", (80, 4)),
        ("[*]\nINDENT_SIZE = 4\n", (80, 4)),
        (
            "[*]\nindent_size = 2\nINDENT_SIZE = 8\nindent_size = 4\n",
            (80, 4),
        ),
        ("[*]\nindent_size = 4\nINDENT_SIZE = 8\n", (80, 8)),
        ("indent_size = 4\n[*]\n", (80, 2)),
        (
            "[*]\nindent_size = 4\n[*.ts]\nindent_size = unset\n",
            (80, 2),
        ),
        ("[*]\ntab_width = 8\n", (80, 2)),
        ("[*]\nindent_size = 4\ntab_width = 8\n", (80, 4)),
        (
            "[*]\nindent_style = space\nindent_size = 4\ntab_width = 8\n",
            (80, 4),
        ),
    ];

    for (editorconfig, expected) in rows {
        let root = dir_with(&[
            (
                "biome.json",
                r#"{ "formatter": { "useEditorconfig": true } }"#,
            ),
            (".editorconfig", editorconfig),
        ]);

        let width = width_at(&root, "a.ts");
        assert_eq!((width.columns, width.tab), expected, "{editorconfig:?}");
        assert!(width.unread.is_empty(), "{editorconfig:?}");
    }
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
fn an_inline_tab_counts_as_its_formatter_counts_it() {
    let source = "function f(x) {\n    if (x) {\n        return \"a\tb\";\n    }\n}\n";
    for (name, config, joins) in [
        (
            ".prettierrc.json",
            r#"{"printWidth":COLUMNS,"tabWidth":4}"#,
            23,
        ),
        (
            ".oxfmtrc.json",
            r#"{"printWidth":COLUMNS,"tabWidth":4}"#,
            27,
        ),
        (
            "biome.json",
            r#"{"formatter":{"lineWidth":COLUMNS,"indentWidth":4}}"#,
            27,
        ),
    ] {
        for (columns, joined) in [(joins - 1, false), (joins, true)] {
            let value = config.replace("COLUMNS", &columns.to_string());
            let root = dir_with(&[(name, &value)]);
            let text = fixed(&root, "a.ts", source).expect("the braces come off");
            assert_eq!(
                text.contains("    if (x) return"),
                joined,
                "{name} at {columns}"
            );
        }
    }
}

#[test]
fn editorconfig_section_patterns_follow_editorconfig_globs() {
    let root = dir_with(&[
        PRETTIER,
        (
            ".editorconfig",
            "[*]\nmax_line_length = 80\n[/src/{a,b}[!0-9]?.ts]\nmax_line_length = 111\n[**/file{1..3}.ts]\nmax_line_length = 122\n[{-2..9}.ts]\nmax_line_length = 99\n",
        ),
    ]);

    assert_eq!(columns_at(&root, "src/abx.ts"), 111);
    assert_eq!(columns_at(&root, "deep/file2.ts"), 122);
    assert_eq!(columns_at(&root, "src/a1x.ts"), 80);
    assert_eq!(columns_at(&root, "7.ts"), 99);
    assert_eq!(columns_at(&root, "-2.ts"), 99);
    assert_eq!(columns_at(&root, "+7.ts"), 80);
    assert_eq!(columns_at(&root, "07.ts"), 80);
    assert_eq!(columns_at(&root, "sub/foo.ts"), 80);
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Sectioned {
    Applies,
    Skips,
    Unread,
}

fn section_outcome(config: &str, section: &str, path: &str) -> Sectioned {
    let editorconfig =
        format!("root = true\n[*]\nmax_line_length = 120\n[{section}]\nmax_line_length = 40\n");
    let root = dir_with(&[(config, "{}"), (".editorconfig", &editorconfig), (path, "")]);
    let width = width_at(&root, path);
    let default = Formatter::of(config).keys().default_width;
    match (width.columns, width.unread.as_slice()) {
        (40, []) => Sectioned::Applies,
        (120, []) => Sectioned::Skips,
        (columns, [unread]) if columns == default && *unread == root.join(".editorconfig") => {
            Sectioned::Unread
        }
        found => panic!("{config} [{section}] on {path} gave {found:?}"),
    }
}

fn section_rows(config: &str, rows: &[(&str, &str, Sectioned)]) {
    for &(section, path, expected) in rows {
        assert_eq!(
            section_outcome(config, section, path),
            expected,
            "{config} [{section}] on {path}"
        );
    }
}

#[test]
fn editorconfig_prettier_sections() {
    use Sectioned::{Applies as A, Skips as S, Unread as U};

    section_rows(
        ".prettierrc.json",
        &[
            ("a\\ b.ts", "a b.ts", A),
            ("a\\*.ts", "a*.ts", A),
            ("a\\*.ts", "ab.ts", S),
            ("{a}.ts", "a.ts", S),
            ("{a}.ts", "{a}.ts", A),
            ("file{-2..3}.ts", "file-0.ts", S),
            ("file{-2..3}.ts", "file-2.ts", A),
            ("{1..3}*.ts", "12.ts", A),
            ("src[!x]1/b.ts", "src/1/b.ts", S),
            ("/foo.ts", "foo.ts", A),
            ("/foo.ts", "sub/foo.ts", S),
            ("src/*.ts", "src/a/b.ts", S),
            ("a.ts", "src/a.ts", A),
            ("file{1..3}.ts", "file2.ts", A),
            ("{0..4294967295}.ts", "7.ts", U),
            ("[\\d].ts", "7.ts", S),
            ("[\\d].ts", "d.ts", A),
            ("*", "src/a.ts", A),
            ("*.{js,ts}", "src/a.ts", A),
            ("src/**", "src/a.ts", A),
            ("?.ts", "\u{e9}.ts", A),
            ("??.ts", "\u{1f600}.ts", A),
            ("{,a}b.ts", "b.ts", A),
            ("{3..1}.ts", "2.ts", A),
            ("[]a].ts", "].ts", A),
            ("[z-a].ts", "z.ts", S),
            ("**.ts", "src/a/b.ts", A),
            ("src/**/a.ts", "src/a.ts", A),
            ("Makefile", "src/a.ts", S),
            ("[[:alpha:]].ts", "a.ts", U),
            ("+(a|b).ts", "a.ts", U),
            ("src//a.ts", "src/a.ts", U),
            ("a/../a.ts", "a.ts", U),
        ],
    );
}

#[test]
fn editorconfig_oxfmt_sections() {
    use Sectioned::{Applies as A, Skips as S, Unread as U};

    section_rows(
        ".oxfmtrc.json",
        &[
            ("/foo.ts", "foo.ts", U),
            ("src/*.ts", "src/a/b.ts", U),
            ("a.ts", "src/a.ts", U),
            ("{a}.ts", "a.ts", U),
            ("file{1..3}.ts", "file2.ts", U),
            ("src[!x]1/b.ts", "src/1/b.ts", U),
            ("a*.ts", "src/a.ts", U),
            ("{a,b}.ts", "src/a.ts", U),
            ("Makefile", "src/a.ts", S),
            ("package.json", "src/a.ts", S),
            ("*", "src/a.ts", A),
            ("*.ts", "src/a.ts", A),
            ("src/**", "src/a.ts", A),
            ("a.ts", "a.ts", A),
            ("src/*.ts", "src/a.ts", A),
            ("file{1..3}.ts", "file4.ts", S),
        ],
    );
}

#[test]
fn editorconfig_section_budget() {
    use Sectioned::Unread as U;

    for (_, config) in EDITOR_FORMATTERS {
        section_rows(
            config,
            &[
                ("{1..257}.ts", "7.ts", U),
                (
                    "{9223372036854775808..9223372036854775809}.ts",
                    "9223372036854775808.ts",
                    U,
                ),
            ],
        );
    }
}

#[test]
fn a_prettier_editorconfig_under_a_braced_directory_is_unread() {
    let editorconfig = "root = true\n[*]\nmax_line_length = 120\n";
    let root = dir_with(&[
        ("p{a,b}/.prettierrc.json", "{}"),
        ("p{a,b}/.editorconfig", editorconfig),
        ("q/.prettierrc.json", "{}"),
        ("q/.editorconfig", editorconfig),
    ]);

    let width = width_at(&root, "p{a,b}/a.ts");
    assert_eq!(width.columns, 80);
    assert_eq!(width.unread, [root.join("p{a,b}/.editorconfig")]);
    assert_eq!(columns_at(&root, "q/a.ts"), 120);
}

#[test]
fn an_unreadable_farther_section_leaves_keys_a_nearer_file_set() {
    let root = dir_with(&[
        (".prettierrc.json", "{}"),
        (
            ".editorconfig",
            "root = true\n[*.+(js|ts)]\nmax_line_length = 60\n",
        ),
        ("pkg/.editorconfig", "[*]\nmax_line_length = 100\n"),
    ]);

    let width = width_at(&root, "pkg/a.ts");
    assert_eq!((width.columns, width.unread.len()), (100, 0));

    let width = width_at(&root, "a.ts");
    assert_eq!(width.unread, [root.join(".editorconfig")]);
}

#[test]
fn biome_skips_sections_that_set_no_indent_size() {
    let root = dir_with(&[
        ("biome.json", r#"{"formatter":{"useEditorconfig":true}}"#),
        (
            ".editorconfig",
            "root = true\n[*]\nindent_size = 4\n[*.+(md|mdx)]\nmax_line_length = 80\n",
        ),
    ]);

    let width = width_at(&root, "a.ts");
    assert_eq!((width.tab, width.unread.len()), (4, 0));
}

#[test]
fn editorconfig_sections_without_layout_keys_cost_no_notice() {
    let root = dir_with(&[
        (".prettierrc.json", "{}"),
        (
            ".editorconfig",
            "root = true\n[*]\nmax_line_length = 120\n[+(a|b).ts]\ncharset = utf-8\n",
        ),
    ]);

    let width = width_at(&root, "a.ts");
    assert_eq!((width.columns, width.unread.len()), (120, 0));
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Override {
    Applies,
    Skips,
    Unread,
}

const FORMATTERS: [Formatter; 3] = [Formatter::Prettier, Formatter::Oxfmt, Formatter::Biome];

fn override_config(formatter: Formatter, includes: &str) -> (&'static str, String) {
    match formatter {
        Formatter::Prettier => (
            ".prettierrc.json",
            format!(
                r#"{{"printWidth":120,"overrides":[{{"files":{includes},"options":{{"printWidth":40}}}}]}}"#
            ),
        ),
        Formatter::Oxfmt => (
            ".oxfmtrc.json",
            format!(
                r#"{{"printWidth":120,"overrides":[{{"files":{includes},"options":{{"printWidth":40}}}}]}}"#
            ),
        ),
        Formatter::Biome => (
            "biome.json",
            format!(
                r#"{{"formatter":{{"lineWidth":120}},"overrides":[{{"includes":{includes},"formatter":{{"lineWidth":40}}}}]}}"#
            ),
        ),
    }
}

fn override_outcome(formatter: Formatter, includes: &str, path: &str) -> Override {
    let (name, config) = override_config(formatter, includes);
    let root = dir_with(&[
        (".prettierrc.json", r#"{"printWidth":70}"#),
        (&format!("child/{name}"), &config),
    ]);
    let width = width_at(&root, &format!("child/{path}"));
    match (width.columns, width.unread.as_slice()) {
        (40, []) => Override::Applies,
        (120, []) => Override::Skips,
        (70, [unread]) if *unread == root.join(&format!("child/{name}")) => Override::Unread,
        found => panic!("{formatter:?} {includes} on {path} gave {found:?}"),
    }
}

#[test]
fn override_formatter_rows() {
    use Override::{Applies as A, Skips as S, Unread as U};

    let rows: [(&str, &str, [Override; 3]); 27] = [
        ("*.ts", "src/a.ts", [A, A, S]),
        ("**/*.ts", "a.ts", [A, A, A]),
        ("src/*.ts", "src/a/b.ts", [S, S, S]),
        ("src/**/*.ts", "src/a.ts", [A, A, A]),
        ("./src/**", "src/a.ts", [A, A, A]),
        ("./src/*.ts", "src/a.ts", [A, A, A]),
        ("./*.ts", "src/a.ts", [S, S, S]),
        ("./a.ts", "a.ts", [A, A, A]),
        ("{a,b}.ts", "a.ts", [A, A, A]),
        ("*.ts", ".a.ts", [A, A, A]),
        ("*.TS", "a.ts", [S, S, S]),
        ("?.ts", "a.ts", [A, A, U]),
        ("[ab].ts", "a.ts", [A, A, U]),
        ("[!a].ts", "b.ts", [U, U, U]),
        ("{a}.ts", "a.ts", [U, U, U]),
        ("{src}/**", "src/a.ts", [U, U, U]),
        ("{1..3}.ts", "1.ts", [U, U, U]),
        ("@(a|b).ts", "a.ts", [U, U, U]),
        ("**.ts", "src/a.ts", [U, U, U]),
        ("!a.ts", "a.ts", [U, U, S]),
        ("?.ts", "é.ts", [A, S, U]),
        ("??.ts", "é.ts", [S, A, U]),
        ("??.ts", "😀.ts", [A, S, U]),
        ("????.ts", "😀.ts", [S, A, U]),
        ("[é].ts", "é.ts", [A, S, U]),
        ("[a-c].ts", "b.ts", [A, A, U]),
        ("[id].ts", "[id].ts", [A, S, U]),
    ];

    for (pattern, path, expected) in rows {
        let includes = serde_json::to_string(&[pattern]).unwrap();
        for (formatter, expected) in FORMATTERS.into_iter().zip(expected) {
            assert_eq!(
                override_outcome(formatter, &includes, path),
                expected,
                "{formatter:?} {pattern} on {path}"
            );
        }
    }

    for (files, exclude, prettier, oxfmt) in [
        (r#"["child/*.ts"]"#, "b.ts", 60, 70),
        (r#"["*.ts"]"#, "b.ts", 70, 70),
        (r#"["*.ts"]"#, "child/*.ts", 60, 70),
        (r#"["*.ts"]"#, "child/b.ts", 70, 70),
    ] {
        for (name, expected) in [(".prettierrc.json", prettier), (".oxfmtrc.json", oxfmt)] {
            let config = format!(
                r#"{{"printWidth":70,"overrides":[{{"files":{files},"excludeFiles":["{exclude}"],"options":{{"printWidth":60}}}}]}}"#
            );
            let root = dir_with(&[(name, &config)]);
            let width = width_at(&root, "child/b.ts");
            assert_eq!(
                (width.columns, width.unread.len()),
                (expected, 0),
                "{name} {files} {exclude}"
            );
        }
    }

    let guard = "function f(a) {\n  if (a) {\n    log(\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\");\n  }\n}\n";
    let root = dir_with(&[(
        ".prettierrc.json",
        r#"{"printWidth":120,"overrides":[{"files":"./src/*.ts","options":{"printWidth":40}}]}"#,
    )]);

    assert!(width_at(&root, "src/a.ts").unread.is_empty());
    assert!(
        fixed(&root, "src/a.ts", guard).is_some_and(|text| text.contains("  if (a)\n    log("))
    );
    assert!(fixed(&root, "a.ts", guard).is_some_and(|text| text.contains("  if (a) log(")));
}

#[test]
fn override_unsupported_unread() {
    for (pattern, applies, skips) in [
        ("{src}/**", "src/a.ts", "lib/a.ts"),
        ("{1..3}.ts", "1.ts", "9.ts"),
        ("@(a|b).ts", "a.ts", "c.ts"),
        ("[[:alpha:]].ts", "a.ts", "1.ts"),
        ("[!a].ts", "b.ts", "a.ts"),
        ("!a.ts", "b.ts", "a.ts"),
    ] {
        for formatter in [Formatter::Prettier, Formatter::Oxfmt] {
            let alone = serde_json::to_string(&[pattern]).unwrap();
            let beside = serde_json::to_string(&["*.md", pattern]).unwrap();
            for (includes, path) in [(&alone, applies), (&beside, skips)] {
                assert_eq!(
                    override_outcome(formatter, includes, path),
                    Override::Unread,
                    "{formatter:?} {includes} on {path}"
                );
            }
        }
    }

    for pattern in ["?.ts", "[ab].ts"] {
        for (includes, path) in [
            (format!(r#"["{pattern}"]"#), "a.ts"),
            (format!(r#"["*.md","{pattern}"]"#), "c.ts"),
        ] {
            assert_eq!(
                override_outcome(Formatter::Biome, &includes, path),
                Override::Unread,
                "{includes} on {path}"
            );
        }

        for (scope, config) in biome_scopes(&format!(r#"["**","{pattern}"]"#)) {
            let root = dir_with(&[
                (".prettierrc", r#"{ "printWidth": 120 }"#),
                ("child/biome.json", &config),
            ]);
            let width = width_at(&root, "child/a.ts");
            assert_eq!(width.columns, 120, "{scope} {pattern}");
            assert_eq!(
                width.unread,
                [root.join("child/biome.json")],
                "{scope} {pattern}"
            );
        }
    }

    for config in [
        r#"{"formatter":{"lineWidth":70},"overrides":[{"includes":["*.md"],"formatter":{"includes":["?.ts"]}}]}"#,
        r#"{"formatter":{"lineWidth":70},"overrides":[{"include":["*.md"],"formatter":{"includes":["?.ts"]}}]}"#,
        r#"{"formatter":{"lineWidth":70,"includes":["?.ts"]},"overrides":[{"includes":["**"],"formatter":{"includes":["**"]}}]}"#,
    ] {
        let root = dir_with(&[
            (".prettierrc", r#"{ "printWidth": 120 }"#),
            ("child/biome.json", config),
        ]);
        let width = width_at(&root, "child/a.ts");
        assert_eq!(
            (width.columns, width.unread.clone()),
            (120, vec![root.join("child/biome.json")]),
            "{config}"
        );
    }

    let negated = r#"["**","!b.ts"]"#;
    assert_eq!(
        override_outcome(Formatter::Biome, negated, "a.ts"),
        Override::Applies
    );
    assert_eq!(
        override_outcome(Formatter::Biome, negated, "b.ts"),
        Override::Skips
    );
}

#[test]
fn override_biome_folder_suffix_matches_no_file() {
    assert_eq!(
        override_outcome(Formatter::Biome, r#"["dist/"]"#, "dist/a.ts"),
        Override::Skips
    );
    for path in ["dist/a.ts", "dist/x/b.ts"] {
        assert_eq!(
            override_outcome(Formatter::Biome, r#"["**","!dist/"]"#, path),
            Override::Applies
        );
    }

    for (scope, config) in biome_scopes(r#"["**","!**/node_modules/"]"#) {
        let root = dir_with(&[
            (".prettierrc", r#"{ "printWidth": 120 }"#),
            ("child/biome.json", &config),
        ]);
        let width = width_at(&root, "child/node_modules/a.ts");
        let expected = if scope == "override" { 60 } else { 70 };
        assert_eq!(
            (width.columns, width.unread.len()),
            (expected, 0),
            "{scope}"
        );
    }

    assert_eq!(
        override_outcome(Formatter::Prettier, r#"["dist/"]"#, "dist/a.ts"),
        Override::Unread
    );
}

const JS_FORMATTER_CONFIGS: [&str; 2] = ["prettier.config.mjs", "oxfmt.config.mjs"];

fn svelte_plugin_config(root: &str, entry: &str) -> String {
    format!("export default {{printWidth:{root},overrides:[{entry}]}};")
}

#[test]
fn override_unrelated_unknown() {
    let entries = [
        r#"{files:"*.svelte",options:{plugins:[process.env.PLUGIN]}}"#,
        r#"{files:"*.svelte",options:{printWidth:Number(process.env.W)}}"#,
        r#"{files:"*.ts",options:{semi:process.env.S === "1"}}"#,
    ];

    for name in JS_FORMATTER_CONFIGS {
        for entry in entries {
            let root = dir_with(&[(name, &svelte_plugin_config("100", entry))]);
            let width = width_at(&root, "a.ts");
            assert_eq!(
                (width.columns, width.unread.len()),
                (100, 0),
                "{name} {entry}"
            );
            assert_eq!(width.source, config_source(&root, name), "{name} {entry}");
        }
    }
}

#[test]
fn override_relevant_unknown() {
    let plugin = r#"options:{plugins:[process.env.PLUGIN]}"#;
    let configs = [
        svelte_plugin_config(
            "100",
            r#"{files:"*.ts",options:{printWidth:Number(process.env.W)}}"#,
        ),
        svelte_plugin_config(
            "100",
            r#"{files:"*.ts",options:{tabWidth:Number(process.env.W)}}"#,
        ),
        svelte_plugin_config("100", r#"{files:"*.ts",options:process.env.OPTIONS}"#),
        svelte_plugin_config("100", r#"{files:"*.ts",options:{...process.env.OPTIONS}}"#),
        svelte_plugin_config("100", &format!("{{files:process.env.FILES,{plugin}}}")),
        svelte_plugin_config(
            "100",
            &format!(r#"{{files:"*.svelte",excludeFiles:process.env.FILES,{plugin}}}"#),
        ),
        svelte_plugin_config(
            "Number(process.env.W)",
            &format!(r#"{{files:"*.svelte",{plugin}}}"#),
        ),
    ];

    for name in JS_FORMATTER_CONFIGS {
        for config in &configs {
            let root = dir_with(&[(name, config)]);
            let width = width_at(&root, "a.ts");
            assert_eq!(width.columns, 80, "{name} {config}");
            assert_eq!(width.unread, [root.join(name)], "{name} {config}");
        }
    }
}

#[test]
fn initializer_self_reference_width() {
    for config in [
        "const o = { a: o, printWidth: 100 }; export default o;",
        "const o = { a: o }; export default { printWidth: 100, extra: o };",
        "const a = { b }; const b = { a }; export default { printWidth: 100, overrides: [{ files: \"*.ts\", options: a }] };",
        "const o = { semi: (() => o)() }; export default { printWidth: 100, overrides: [{ files: \"*.ts\", options: o }] };",
    ] {
        let root = dir_with(&[("prettier.config.mjs", config)]);
        let width = width_at(&root, "a.ts");
        assert_eq!(width.columns, 80, "{config}");
        assert_eq!(width.unread, [root.join("prettier.config.mjs")], "{config}");
    }

    let imported = dir_with(&[
        (
            "options.mjs",
            "export const options = { C }; export default class C {}",
        ),
        (
            "prettier.config.mjs",
            r#"import { options } from "./options.mjs"; export default { printWidth: 100, overrides: [{ files: "*.ts", options }] };"#,
        ),
    ]);
    let width = width_at(&imported, "a.ts");
    assert_eq!(width.columns, 80);
    assert_eq!(width.unread, [imported.join("prettier.config.mjs")]);

    let short_circuit =
        "const base = { printWidth: 100 }; export default base || fallback; const fallback = {};";
    let root = dir_with(&[("prettier.config.mjs", short_circuit)]);
    let width = width_at(&root, "a.ts");
    assert_eq!((width.columns, width.unread.len()), (100, 0));

    let shared =
        "const a = { semi: true }; const o = { a, b: a, printWidth: 100 }; export default o;";
    let root = dir_with(&[("prettier.config.mjs", shared)]);
    let width = width_at(&root, "a.ts");
    assert_eq!((width.columns, width.unread.len()), (100, 0));
}
