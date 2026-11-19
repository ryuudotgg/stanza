use super::*;
use crate::engine::tests::{LINEAR_GROWTH_CAP, assert_bounded, cpu_ms, ratio};

fn decided(name: &str, path: &str, rules: Rules) -> Option<bool> {
    SectionGlob::new(name, rules)?.matches(&SectionPath::new(path))
}

fn rows(rules: Rules, rows: &[(&str, &str, Option<bool>)]) {
    for &(name, path, expected) in rows {
        assert_eq!(
            decided(name, path, rules),
            expected,
            "{rules:?} [{name}] on {path}"
        );
    }
}

#[test]
fn editorconfig_rows_follow_prettier() {
    rows(
        Rules::EditorConfig,
        &[
            ("*.ts", "a/b.ts", Some(true)),
            ("*.TS", "a.ts", Some(false)),
            ("a.ts", "src/a.ts", Some(true)),
            ("/foo.ts", "foo.ts", Some(true)),
            ("/foo.ts", "sub/foo.ts", Some(false)),
            ("src/*.ts", "src/a/b.ts", Some(false)),
            ("src/**", "src/a/b.ts", Some(true)),
            ("src/**", "src", Some(false)),
            ("**/x.ts", "x.ts", Some(true)),
            ("/src/**/x.ts", "src/a/b/x.ts", Some(true)),
            ("**.ts", "a/b.ts", Some(true)),
            ("a**/b.ts", "a/b.ts", Some(true)),
            ("a**/b.ts", "ab/x/b.ts", Some(true)),
            ("*/**/a.ts", "a.ts", Some(false)),
            ("{*,a}*/b.ts", "src/deep/b.ts", Some(true)),
            ("{*,a}*/b.ts", "b.ts", Some(true)),
            ("a{*,b}*.ts", "axy.ts", Some(true)),
            ("a\\ b.ts", "a b.ts", Some(true)),
            ("*a\\ b/x.ts", "ya b/x.ts", None),
            ("a\\*.ts", "a*.ts", Some(true)),
            ("a\\*.ts", "ab.ts", Some(false)),
            ("a\\\\b.ts", "a\\b.ts", Some(true)),
            ("{a}.ts", "a.ts", Some(false)),
            ("{a}.ts", "{a}.ts", Some(true)),
            ("{}.ts", "{}.ts", Some(true)),
            ("{,a}b.ts", "b.ts", Some(true)),
            ("{a,{b,c}}.ts", "c.ts", Some(true)),
            ("*.{js,ts}", "src/a.ts", Some(true)),
            ("{a/b,c}.ts", "a/b.ts", Some(true)),
            ("file{1..3}.ts", "file2.ts", Some(true)),
            ("file{-2..3}.ts", "file-0.ts", Some(false)),
            ("{-0..2}.ts", "0.ts", Some(true)),
            ("{3..1}.ts", "2.ts", Some(true)),
            ("{1..3}*.ts", "12.ts", Some(true)),
            ("{1..3}*.ts", "4x.ts", Some(false)),
            ("{a}x,{1..2}.ts", "{a}x,1.ts", None),
            ("{a}{b,c}.ts", "{a}c.ts", Some(true)),
            ("{-2..9}.ts", "+7.ts", Some(false)),
            ("{-2..9}.ts", "07.ts", Some(false)),
        ],
    );
}

#[test]
fn editorconfig_classes_follow_minimatch() {
    rows(
        Rules::EditorConfig,
        &[
            ("[!a].ts", "b.ts", Some(true)),
            ("[^a].ts", "a.ts", Some(false)),
            ("[\\d].ts", "7.ts", Some(false)),
            ("[\\d].ts", "d.ts", Some(true)),
            ("[a-].ts", "-.ts", Some(true)),
            ("[a\\-z].ts", "b.ts", Some(false)),
            ("[a-c-e].ts", "-.ts", Some(true)),
            ("[]a].ts", "].ts", Some(true)),
            ("[a\\]].ts", "].ts", Some(true)),
            ("[z-a].ts", "z.ts", Some(false)),
            ("[!z-a].ts", "b.ts", Some(false)),
            ("?.ts", "\u{e9}.ts", Some(true)),
            ("?.ts", "\u{1f600}.ts", Some(false)),
            ("??.ts", "\u{1f600}.ts", Some(true)),
            ("src[!x]1/b.ts", "src/1/b.ts", Some(false)),
        ],
    );
}

#[test]
fn editorconfig_shapes_outside_the_model_are_unread() {
    for name in [
        "{0..4294967295}.ts",
        "{9223372036854775808..9223372036854775809}.ts",
        "{00..2}.ts",
        "{1..9..2}.ts",
        "{a..c}.ts",
        "{a{b,c}}.ts",
        "{a,b{c}.ts",
        "a}.ts",
        "a].ts",
        "[a.ts",
        "[[:alpha:]].ts",
        "[a[b].ts",
        "[a/b].ts",
        "+(a|b).ts",
        "?(a).ts",
        "a${b,c}.ts",
        "a$**.ts",
        "***.ts",
        "\\**.ts",
        "a\\",
        "a\\/b.ts",
        "src//a.ts",
        "./a.ts",
        "a/../a.ts",
        "src/",
        "{a,b}\u{2028}.ts",
        "src/a\\\\b.ts",
        "src/[a-\\\\]x.ts",
        "*a\\ b.ts",
        "?\\ b.ts",
        &format!("[{}]{{1..128}}.ts", "a".repeat(60_000)),
        "{a}{1..3}.ts",
        "x{a}y{1..2}.ts",
        "{a,b{c}{1..2}}.ts",
        "{-9223372036854775808..-9223372036854775808}.ts",
        &format!("{}a{}", "{,".repeat(20_000), "}".repeat(20_000)),
    ] {
        assert!(
            SectionGlob::new(name, Rules::EditorConfig).is_none(),
            "{name}"
        );
    }
}

#[test]
fn expansions_count_every_globstar_as_prettier_does() {
    rows(
        Rules::EditorConfig,
        &[
            ("{1..128}.ts", "128.ts", Some(true)),
            ("{1..129}.ts", "7.ts", None),
            ("/{1..256}.ts", "256.ts", Some(true)),
            ("/{1..257}.ts", "7.ts", None),
            ("{1..257}.ts", "7.ts", None),
            ("/a/**/**/**/**/**/**/**/**/b", "a/b", Some(true)),
            ("/a/**/**/**/**/**/**/**/**/**/b", "a/b", None),
        ],
    );
}

#[test]
fn oxfmt_reads_a_section_only_where_both_answers_agree() {
    rows(
        Rules::Oxfmt,
        &[
            ("*", "src/a.ts", Some(true)),
            ("*.ts", "src/a.ts", Some(true)),
            ("*.TS", "a.ts", Some(false)),
            ("Makefile", "src/a.ts", Some(false)),
            ("a.ts", "a.ts", Some(true)),
            ("a.ts", "src/a.ts", None),
            ("a*.ts", "src/a.ts", None),
            ("{a,b}.ts", "src/a.ts", None),
            ("/foo.ts", "foo.ts", None),
            ("src/**", "src/a.ts", Some(true)),
            ("src/*.ts", "src/a.ts", Some(true)),
            ("src/*.ts", "src/a/b.ts", None),
            ("**/a.ts", "a.ts", Some(true)),
            ("src/**/a.ts", "src/a.ts", Some(true)),
            ("src/**/a.ts", "src/x/a.ts", Some(true)),
            ("*/**/a.ts", "a.ts", Some(false)),
            ("{a}.ts", "a.ts", None),
            ("{,a}b.ts", "b.ts", None),
            ("{,a}b.ts", "ab.ts", Some(true)),
            ("file{1..3}.ts", "file2.ts", None),
            ("file{1..3}.ts", "file4.ts", Some(false)),
            ("src[!x]1/b.ts", "src/1/b.ts", None),
            ("??.ts", "\u{e9}.ts", None),
            ("?.ts", "\u{e9}.ts", None),
            ("[\u{e9}].ts", "x.ts", None),
            ("{a,**}.ts", "a.ts", None),
            ("{*,a}*/b.ts", "src/b.ts", None),
            ("[a\\-z].ts", "b.ts", None),
            ("[a\\]].ts", "].ts", None),
            ("x[a-c-e].ts", "xd.ts", None),
            ("x[a-c].ts", "xb.ts", Some(true)),
            ("{,}a.ts", "a.ts", Some(true)),
            ("x{,,}a.ts", "xa.ts", Some(true)),
        ],
    );
}

#[test]
fn stars_with_no_match_stay_linear() {
    let name = SectionPath::new(&format!("{}.ts", "a".repeat(60)));
    assert_bounded(
        "stars",
        &[8, 32],
        5,
        ratio(LINEAR_GROWTH_CAP),
        |count| {
            SectionGlob::new(&format!("{}b", "*a".repeat(count)), Rules::Oxfmt)
                .expect("stars compile")
        },
        |glob| {
            cpu_ms(|| {
                for _ in 0..500 {
                    assert_eq!(glob.matches(&name), Some(false));
                }
            })
        },
    );
}

#[test]
fn a_long_literal_section_compiles_in_linear_time() {
    assert_bounded(
        "literal",
        &[4_000, 16_000],
        5,
        ratio(LINEAR_GROWTH_CAP),
        |length| format!("/{}", "a".repeat(length)),
        |name| cpu_ms(|| assert!(SectionGlob::new(name, Rules::EditorConfig).is_some())),
    );
}

#[test]
fn a_long_path_past_the_state_budget_is_unread() {
    let glob = SectionGlob::new(&"*a".repeat(64), Rules::EditorConfig).unwrap();
    let short = SectionPath::new("a.ts");
    let long = SectionPath::new(&"b".repeat(STATE_LIMIT / 64));
    assert_eq!(glob.matches(&short), Some(false));
    assert_eq!(glob.matches(&long), None);
}
