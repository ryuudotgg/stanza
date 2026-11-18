use super::*;
use crate::engine::tests::{LINEAR_GROWTH_CAP, assert_bounded, cpu_ms, ratio};

fn decided(pattern: &str, formatter: Formatter, path: &str) -> Option<bool> {
    let target = Target::new(path, formatter);
    OverrideGlob::new(pattern, formatter, &target)
        .map(|glob| glob.matches(&target, Anchor::NameIfSlashless))
}

#[test]
fn override_glob_subset_rows() {
    for (pattern, path, expected) in [
        ("**", "a/b/c.ts", Some(true)),
        ("src/**", "src/a/b.ts", Some(true)),
        ("a/**/b/*.ts", "a/b/c.ts", Some(true)),
        ("a/**/b/*.ts", "a/x/y/b/c.ts", Some(true)),
        ("*.{js,ts}", "a.ts", Some(true)),
        ("{src,test}/**/*.ts", "test/a/b.ts", Some(true)),
        ("{a,b}{c,d}.ts", "bd.ts", Some(true)),
        ("*a*b", "xaybzb", Some(true)),
        ("a*", "a", Some(true)),
        ("[a-c]x.ts", "dx.ts", Some(false)),
        ("{a,[b,c]}.ts", ",.ts", Some(true)),
        ("{a,b", "a", None),
        ("a}", "a", None),
        ("a]", "a", None),
        ("[a", "a", None),
        ("{a,}.ts", "a.ts", None),
        ("{a,{b,c}}", "a", None),
        ("{a/b,c}", "a/b", None),
        ("{**,a}/b.ts", "a/b.ts", None),
        ("*{*,a}/b.ts", "a/b.ts", None),
        ("{a,*}{*,b}.ts", "a.ts", None),
        ("*.{*,a}", "a.ts", Some(true)),
        ("a**/b", "ab/b", None),
        ("a/***", "a/b", None),
        ("a\\*.ts", "a*.ts", None),
        ("[^a].ts", "b.ts", None),
        ("[]a].ts", "a.ts", None),
        ("[a-].ts", "-.ts", Some(true)),
        ("[-a].ts", "-.ts", Some(true)),
        ("[!a].ts", "b.ts", None),
        ("!a.ts", "b.ts", None),
        ("[é-ê].ts", "é.ts", Some(true)),
        ("[id].ts", "[id].ts", Some(true)),
        ("[ab]x[cd].ts", "[ab]xc.ts", Some(true)),
        ("{x,[ab]}.ts", "[ab].ts", Some(true)),
        ("**/[ab].ts", "src/[ab].ts", Some(true)),
        ("**/[a-c].ts", "src/[a-c].ts", Some(false)),
        ("[a-c].ts", "[a-c].ts", Some(true)),
        ("{a,b}.ts", "{a,b}.ts", Some(true)),
        ("\"a\".ts", "a.ts", None),
        ("{1..3,4}.ts", "4.ts", None),
        ("a'b.ts", "a'b.ts", Some(true)),
        ("[ab]+.ts", "aa.ts", None),
        ("{a,b}+.ts", "aa.ts", None),
        ("c++.ts", "c++.ts", Some(true)),
        ("*+.ts", "x+.ts", Some(true)),
        ("src/a[.-0]b.ts", "src/a/b.ts", None),
        ("{a,[b,c]}.ts", "[b.ts", Some(false)),
        ("[c-a].ts", "b.ts", None),
        ("[a/b].ts", "a.ts", None),
        ("+(a).ts", "a.ts", None),
        ("important!.ts", "important!.ts", Some(true)),
        ("", "a.ts", None),
        ("/a.ts", "a.ts", None),
        ("src/", "src", None),
        ("src//a.ts", "src/a.ts", None),
        ("../a.ts", "a.ts", None),
    ] {
        assert_eq!(
            decided(pattern, Formatter::Prettier, path),
            expected,
            "{pattern} on {path}"
        );
    }
}

#[test]
fn override_glob_oxfmt_reads_brackets_as_classes_only() {
    for (pattern, path, expected) in [
        ("[id].ts", "[id].ts", Some(false)),
        ("[id].ts", "i.ts", Some(true)),
        ("important!.ts", "important!.ts", Some(true)),
        ("[-a].ts", "-.ts", Some(true)),
        ("[é-ê].ts", "é.ts", None),
        ("{a,[b,c]}.ts", ",.ts", Some(true)),
        ("{a,b}.ts", "{a,b}.ts", Some(false)),
        ("[a-c].ts", "[a-c].ts", Some(false)),
    ] {
        assert_eq!(
            decided(pattern, Formatter::Oxfmt, path),
            expected,
            "{pattern} on {path}"
        );
    }
}

#[test]
fn override_glob_covers_folders() {
    let target = Target::new("dist/deep/a.ts", Formatter::Biome);
    for (pattern, expected) in [
        ("dist", true),
        ("dist/deep", true),
        ("dist/**", true),
        ("dist/deep/a.ts", true),
        ("deep", false),
        ("dis", false),
    ] {
        let glob = OverrideGlob::new(pattern, Formatter::Biome, &target).unwrap();
        assert_eq!(glob.covers(&target), expected, "{pattern}");
    }
}

#[test]
fn override_glob_budget() {
    let path = "a".repeat(4096);
    let wide = format!(
        "{{{}}}.ts",
        (0..257)
            .map(|at| format!("a{at}"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let doubled = "{a,b}".repeat(9);
    let long = format!("{{a,b}}{}", "x".repeat(EXPANDED_LIMIT / 2));
    let states = "*a".repeat(STATE_LIMIT / 4096 / 2 + 1);
    let within = "*a".repeat(STATE_LIMIT / 4096 / 2 - 1);
    for (pattern, path, expected) in [
        (&wide, "a1.ts", None),
        (&doubled, "a", None),
        (&long, "a", None),
        (&states, path.as_str(), None),
        (&within, path.as_str(), Some(true)),
    ] {
        assert_eq!(
            decided(pattern, Formatter::Oxfmt, path),
            expected,
            "{} bytes",
            pattern.len()
        );
    }

    let fits = format!("{{a,b}}{}", "x".repeat(EXPANDED_LIMIT / 2 - 1));
    assert!(decided(&fits, Formatter::Oxfmt, "a").is_some());

    let members: String = ('\u{4e00}'..).take(2048).collect();
    assert_eq!(
        decided(&format!("*[{members}]"), Formatter::Prettier, &path),
        None
    );
    let duplicated = format!("*[{}]", "a".repeat(1000));
    assert_eq!(decided(&duplicated, Formatter::Prettier, &path), Some(true));

    let prefix = "x".repeat(40_000);
    let spelled = format!("{prefix}{}", "[ab]".repeat(8));
    let spent = cpu_ms(|| assert_eq!(decided(&spelled, Formatter::Prettier, "a"), None));
    assert!(
        spent < 100.0,
        "{spent} ms to refuse spelling a long pattern"
    );

    let worst = format!("*{}b", "a".repeat(1020));
    let alternatives = format!("**/{}*", "{a,b}".repeat(8));
    for (pattern, path) in [
        (&within, &path),
        (&worst, &path),
        (&alternatives, &"a/".repeat(40)),
    ] {
        let spent = cpu_ms(|| {
            assert!(
                decided(pattern, Formatter::Prettier, path).is_some(),
                "{} bytes",
                pattern.len()
            );
        });

        assert!(spent < 100.0, "{spent} ms for {} bytes", pattern.len());
    }

    let braces = "{a,b}".repeat(1024 / 5);
    let nested = format!("{}a{}", "{".repeat(512), "}".repeat(512));
    for pattern in [braces, nested, "*a".repeat(512)] {
        let spent = cpu_ms(|| {
            for formatter in [Formatter::Prettier, Formatter::Oxfmt, Formatter::Biome] {
                decided(&pattern, formatter, &path);
            }
        });

        assert!(spent < 100.0, "{spent} ms for {} bytes", pattern.len());
    }
}

#[test]
fn override_glob_stays_linear_in_the_path() {
    let pattern = format!("{}*a", "*b".repeat(64));
    assert_bounded(
        "override glob",
        &[1024, 4096],
        5,
        ratio(LINEAR_GROWTH_CAP),
        |length| "b".repeat(length),
        |path| cpu_ms(|| assert_eq!(decided(&pattern, Formatter::Prettier, path), Some(false))),
    );
}
