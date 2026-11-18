use std::collections::HashSet;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use super::args::Source;
use super::{Io, Program};
use crate::engine::types::{Braces, Located, Mode};
use crate::files::{EmptyReason, StagedFile};
use crate::format::{Findings, Formatted};
use crate::paths::{invalid_name, relative, relative_bytes, shown};
use crate::step::Unread;

pub(crate) fn invalid_lines(cwd: Option<&str>, names: &[PathBuf]) -> Vec<String> {
    let mut printed: Vec<Vec<u8>> = names
        .iter()
        .map(|path| {
            let bytes = path.as_os_str().as_bytes();
            match cwd {
                Some(cwd) => relative_bytes(cwd, bytes),
                None => bytes.to_vec(),
            }
        })
        .collect();

    printed.sort();
    printed.dedup();
    printed.iter().map(|bytes| invalid_name(bytes)).collect()
}

pub(crate) fn printed_path(path: &str, cwd: Option<&str>) -> String {
    match cwd.map(|cwd| relative(cwd, path)) {
        Some(output) if !output.is_empty() => output,
        _ => path.to_owned(),
    }
}

pub(crate) const FINDINGS_CHUNK: usize = 1 << 16;

fn write_row(out: &mut Vec<u8>, located: Located) -> io::Result<()> {
    let Located { path, finding } = located;
    let by_hand = if finding.rule.by_hand() {
        " (by hand)"
    } else {
        ""
    };

    writeln!(
        out,
        "{}:{}:{}: {}: {}{by_hand}",
        shown(path),
        finding.line,
        finding.col,
        finding.rule.id(),
        finding.message
    )
}

pub(crate) fn write_findings(
    out: &mut impl Write,
    findings: &Findings,
    json: bool,
) -> io::Result<()> {
    let mut chunk = Vec::new();
    if json {
        chunk.push(b'[');
    }

    for (index, located) in findings.ordered().enumerate() {
        if json {
            if index > 0 {
                chunk.push(b',');
            }

            serde_json::to_writer(&mut chunk, &located)?;
        } else {
            write_row(&mut chunk, located)?;
        }

        if chunk.len() >= FINDINGS_CHUNK {
            out.write_all(&chunk)?;
            chunk.clear();
        }
    }

    if json {
        chunk.extend_from_slice(b"]\n");
    }

    out.write_all(&chunk)
}

type Notice = fn(&str) -> String;

pub(crate) fn unread_lines(cwd: Option<&str>, unread: &Unread) -> Vec<String> {
    let notices: [(&[String], Notice); 2] = [
        (&unread.braces, |files| {
            format!(
                "cannot tell whether {files} enforces braces, so braces stay; pass --braces or --no-braces to settle it"
            )
        }),
        (&unread.width, |files| {
            format!("cannot read the line width from {files}, so the next source sets it")
        }),
    ];

    let mut lines = Vec::new();
    for (paths, message) in notices {
        if paths.is_empty() {
            continue;
        }

        let mut files: Vec<String> = paths.iter().map(|path| printed_path(path, cwd)).collect();
        files.sort();
        files.dedup();
        lines.push(message(&files.join(", ")));
    }

    lines
}

pub(crate) fn warn_unread(io: &mut Io, unread: &Unread) {
    for line in unread_lines(io.cwd.as_deref().ok(), unread) {
        io.error(Program::Stanza, line);
    }
}

pub(crate) fn warn_crashed(io: &mut Io, program: Program, crashed: &[String]) {
    for line in crashed {
        io.error(program, line);
    }
}

pub(crate) fn status(errors: &[String], formatted: &Formatted) -> i32 {
    if !errors.is_empty() || formatted.failed {
        return 2;
    }

    i32::from(!formatted.findings.is_empty())
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    Paths,
    Changed,
    Staged,
}

impl Scope {
    pub(crate) fn of(source: &Source) -> Option<Self> {
        match source {
            Source::Paths { .. } => Some(Scope::Paths),
            Source::Changed { .. } => Some(Scope::Changed),
            Source::Staged { .. } => Some(Scope::Staged),
            Source::Stdin(_) => None,
        }
    }
}

#[derive(Default)]
struct Tally {
    findings: usize,
    fixable: usize,
    files: usize,
    failed: usize,
}

impl Tally {
    fn of(findings: &Findings) -> Self {
        let mut tally = Tally::default();
        for file in findings.files() {
            let caught = file
                .findings
                .iter()
                .filter(|finding| !finding.rule.is_failure());
            let found = caught.clone().count();
            tally.findings += found;
            tally.fixable += caught.filter(|finding| finding.fixable).count();
            tally.files += usize::from(found > 0);
            tally.failed += usize::from(found < file.findings.len());
        }

        tally
    }
}

fn count(number: usize, noun: &str) -> String {
    let plural = if number == 1 { "" } else { "s" };
    format!("{number} {noun}{plural}")
}

pub(crate) fn summary_line(
    mode: Mode,
    scope: Scope,
    formatted: &Formatted,
    errors: &[String],
) -> Option<String> {
    if formatted.checked == 0 {
        let verb = match mode {
            Mode::Check => "check",
            Mode::Fix => "fix",
        };

        let quiet = formatted.generated > 0 || !errors.is_empty();
        return match scope {
            Scope::Paths => None,
            _ if quiet => None,
            Scope::Changed => Some(format!("no changed files to {verb}")),
            Scope::Staged => Some(format!("no staged files to {verb}")),
        };
    }

    let Tally {
        findings,
        fixable,
        files,
        failed,
    } = Tally::of(&formatted.findings);

    let (file, with_fix) = match scope {
        Scope::Staged => ("staged file", "fixable"),
        Scope::Paths | Scope::Changed => ("file", "fixable with --fix"),
    };

    let checked = format!("checked {}", count(formatted.checked, file));
    let rewritten = formatted.rewritten.len();
    let by_hand = findings - fixable;
    let all = if findings == 1 { "" } else { "all " };
    let line = match mode {
        Mode::Check if findings == 0 => format!("{checked}, no findings"),
        Mode::Check => {
            let found = format!("{} in {}", count(findings, "finding"), count(files, file));
            match (fixable, by_hand) {
                (_, 0) => format!("{found}, {all}{with_fix}"),
                (0, _) => format!("{found}, {all}by hand"),
                _ => format!("{found}: {fixable} {with_fix}, {by_hand} by hand"),
            }
        }
        Mode::Fix => {
            let done = match rewritten {
                0 => checked,
                _ => format!("rewrote {}", count(rewritten, "file")),
            };

            match findings {
                0 if rewritten == 0 && failed == 0 => format!("{done}, nothing to fix"),
                0 => format!("{done}, no findings left"),
                _ => format!("{done}, {} left to fix by hand", count(findings, "finding")),
            }
        }
    };

    match failed {
        0 => Some(line),
        _ => Some(format!("{line}, {} failed", count(failed, "file"))),
    }
}

pub(crate) fn empty_message(reason: EmptyReason, inputs: &[String]) -> String {
    let paths = inputs.join(", ");
    match reason {
        EmptyReason::OptedOut => {
            format!("nothing to format in {paths}, the stanza attribute opts its files out")
        }
        EmptyReason::Ignored => {
            format!("nothing to format in {paths}, git ignores its files")
        }
        EmptyReason::Generated => {
            format!("nothing to format in {paths}, generated files are skipped")
        }
        EmptyReason::Skipped => {
            format!("nothing to format in {paths}, its files are always skipped")
        }
        EmptyReason::None => format!("no TypeScript or JavaScript files under {paths}"),
    }
}

pub(crate) fn warn_submodules(io: &mut Io, submodules: &[String]) {
    for path in submodules {
        let shown = printed_path(path, io.cwd.as_deref().ok());
        io.error(
            Program::Stanza,
            format_args!("skipped git submodule {shown}, pass its path to format its files"),
        );
    }
}

fn shell_word(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_@%+:,./-".contains(&byte));

    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
    }
}

pub(crate) fn repair_lines(
    findings: &Findings,
    files: &[StagedFile],
    cwd: &str,
    braces: Option<Braces>,
) -> Vec<String> {
    let fixable: HashSet<&str> = findings
        .files()
        .iter()
        .filter(|file| file.findings.iter().any(|finding| finding.fixable))
        .map(|file| file.path.as_str())
        .collect();

    let (scoped, whole): (Vec<&StagedFile>, Vec<&StagedFile>) = files
        .iter()
        .filter(|file| fixable.contains(printed_path(&file.path, Some(cwd)).as_str()))
        .partition(|file| file.lines.is_some());

    let (dirty, clean): (Vec<&StagedFile>, Vec<&StagedFile>) =
        whole.into_iter().partition(|file| file.unstaged);

    let names = |list: &[&StagedFile]| {
        let mut printed: Vec<String> = list
            .iter()
            .map(|file| printed_path(&file.path, Some(cwd)))
            .collect();

        printed.sort();
        printed
            .iter()
            .map(|path| shell_word(path))
            .collect::<Vec<_>>()
            .join(" ")
    };

    let mut lines = Vec::new();
    if !scoped.is_empty() {
        lines.push(format!(
            "fix the staged lines of these by hand and restage them with git add -p, --fix would change the whole file: {}",
            names(&scoped)
        ));
    }

    if !dirty.is_empty() {
        lines.push(format!(
            "these also have unstaged changes, fix them with --fix and restage by hand: {}",
            names(&dirty)
        ));
    }

    if !clean.is_empty() {
        let flags = match braces {
            None => "",
            Some(Braces::On) => " --braces",
            Some(Braces::Off) => " --no-braces",
        };

        let names = names(&clean);
        lines.push(format!(
            "fix and restage with: cd {} && stanza --fix{flags} -- {names} && git --literal-pathspecs add -- {names}",
            shell_word(cwd)
        ));
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::rules::Rule;
    use crate::engine::types::{FileFindings, Finding, compare_in_file};
    use std::ffi::OsStr;

    fn outcome(checked: usize, rewritten: usize, files: &[(&str, &[Rule])]) -> Formatted {
        let files = files
            .iter()
            .map(|(path, rules)| FileFindings {
                path: (*path).to_owned(),
                findings: rules
                    .iter()
                    .enumerate()
                    .map(|(line, &rule)| Finding {
                        line: line + 1,
                        col: 1,
                        rule,
                        message: String::new(),
                        fixable: rule.fixable(),
                    })
                    .collect(),
            })
            .collect();

        Formatted {
            findings: Findings::new(files),
            checked,
            rewritten: vec!["r.ts".to_owned(); rewritten],
            ..Formatted::default()
        }
    }

    #[test]
    fn summary_lines_count_findings_files_and_failures() {
        use Rule::{AfterGuard as Fixable, Parse, Wall as ByHand, Write};

        let check = |scope, formatted: &Formatted| summary_line(Mode::Check, scope, formatted, &[]);
        let fix = |formatted: &Formatted| summary_line(Mode::Fix, Scope::Paths, formatted, &[]);
        let both = outcome(
            12,
            0,
            &[("a.ts", &[Fixable, Fixable]), ("b.ts", &[ByHand, ByHand])],
        );
        let cases = [
            (
                check(Scope::Paths, &both),
                "4 findings in 2 files: 2 fixable with --fix, 2 by hand",
            ),
            (
                check(
                    Scope::Changed,
                    &outcome(
                        12,
                        0,
                        &[("a.ts", &[Fixable, Fixable]), ("b.ts", &[Fixable, Fixable])],
                    ),
                ),
                "4 findings in 2 files, all fixable with --fix",
            ),
            (
                check(
                    Scope::Paths,
                    &outcome(12, 0, &[("a.ts", &[ByHand, ByHand])]),
                ),
                "2 findings in 1 file, all by hand",
            ),
            (
                check(Scope::Paths, &outcome(1, 0, &[("a.ts", &[ByHand])])),
                "1 finding in 1 file, by hand",
            ),
            (
                check(Scope::Paths, &outcome(12, 0, &[])),
                "checked 12 files, no findings",
            ),
            (
                check(Scope::Paths, &outcome(12, 0, &[("a.ts", &[Parse])])),
                "checked 12 files, no findings, 1 file failed",
            ),
            (
                check(
                    Scope::Paths,
                    &outcome(
                        3,
                        0,
                        &[("a.ts", &[Fixable]), ("b.ts", &[Parse]), ("c.ts", &[Parse])],
                    ),
                ),
                "1 finding in 1 file, fixable with --fix, 2 files failed",
            ),
            (
                check(Scope::Staged, &both),
                "4 findings in 2 staged files: 2 fixable, 2 by hand",
            ),
            (
                check(Scope::Staged, &outcome(3, 0, &[])),
                "checked 3 staged files, no findings",
            ),
            (
                check(Scope::Staged, &outcome(1, 0, &[])),
                "checked 1 staged file, no findings",
            ),
            (
                fix(&outcome(12, 2, &[("a.ts", &[ByHand]), ("b.ts", &[ByHand])])),
                "rewrote 2 files, 2 findings left to fix by hand",
            ),
            (
                fix(&outcome(12, 1, &[])),
                "rewrote 1 file, no findings left",
            ),
            (
                fix(&outcome(12, 0, &[("a.ts", &[ByHand, ByHand])])),
                "checked 12 files, 2 findings left to fix by hand",
            ),
            (
                fix(&outcome(12, 0, &[])),
                "checked 12 files, nothing to fix",
            ),
            (
                fix(&outcome(1, 0, &[("a.ts", &[ByHand, Write])])),
                "checked 1 file, 1 finding left to fix by hand, 1 file failed",
            ),
            (
                fix(&outcome(1, 0, &[("a.ts", &[Write])])),
                "checked 1 file, no findings left, 1 file failed",
            ),
        ];

        for (printed, expected) in cases {
            assert_eq!(printed.as_deref(), Some(expected));
        }
    }

    #[test]
    fn a_run_that_checks_no_file_names_only_an_empty_selection() {
        let empty = Formatted::default();
        let generated = Formatted {
            generated: 1,
            ..Formatted::default()
        };

        let error = ["cannot read a.ts".to_owned()];
        let cases = [
            (
                Mode::Check,
                Scope::Changed,
                &empty,
                &[][..],
                Some("no changed files to check"),
            ),
            (
                Mode::Fix,
                Scope::Changed,
                &empty,
                &[],
                Some("no changed files to fix"),
            ),
            (
                Mode::Check,
                Scope::Staged,
                &empty,
                &[],
                Some("no staged files to check"),
            ),
            (Mode::Check, Scope::Paths, &empty, &[], None),
            (Mode::Check, Scope::Changed, &generated, &[], None),
            (Mode::Check, Scope::Staged, &empty, &error, None),
        ];

        for (mode, scope, formatted, errors, expected) in cases {
            assert_eq!(
                summary_line(mode, scope, formatted, errors).as_deref(),
                expected
            );
        }
    }

    #[test]
    fn unread_config_notices_name_files_in_byte_order() {
        let unread = Unread {
            braces: ["z", "\u{e9}", "\u{1f600}", "\u{ff71}", "z"]
                .map(|name| format!("/repo/{name}/.oxlintrc.json"))
                .to_vec(),
            width: Vec::new(),
        };

        let files = "z/.oxlintrc.json, \u{e9}/.oxlintrc.json, \u{ff71}/.oxlintrc.json, \u{1f600}/.oxlintrc.json";
        assert_eq!(
            unread_lines(Some("/repo"), &unread),
            [format!(
                "cannot tell whether {files} enforces braces, so braces stay; pass --braces or --no-braces to settle it"
            )]
        );
    }

    #[test]
    fn not_utf8_names_print_in_the_byte_order_of_the_printed_path() {
        let name = |bytes: &[u8]| PathBuf::from(OsStr::from_bytes(bytes));
        let names = [
            name(b"/repo/web/b\xff.ts"),
            name(b"/repo/zzz/a\xff.ts"),
            name(b"/repo/web/b\xff.ts"),
        ];

        assert_eq!(
            invalid_lines(Some("/repo/web"), &names),
            [
                "name is not valid UTF-8: \"../zzz/a\\377.ts\"",
                "name is not valid UTF-8: \"b\\377.ts\"",
            ]
        );
    }

    #[test]
    fn repair_lines_name_files_in_the_byte_order_of_the_printed_path() {
        let fixable = |path: &str| FileFindings {
            path: path.to_owned(),
            findings: vec![Finding {
                line: 1,
                col: 1,
                rule: Rule::Wall,
                message: String::new(),
                fixable: true,
            }],
        };

        let staged = |path: &str| StagedFile {
            path: path.to_owned(),
            bytes: Vec::new(),
            unstaged: false,
            lines: None,
        };

        let findings = Findings::new(vec![fixable("a.ts"), fixable("../zzz/b.ts")]);
        let files = [staged("/repo/web/a.ts"), staged("/repo/zzz/b.ts")];

        assert_eq!(
            repair_lines(&findings, &files, "/repo/web", None),
            [
                "fix and restage with: cd /repo/web && stanza --fix -- ../zzz/b.ts a.ts && git --literal-pathspecs add -- ../zzz/b.ts a.ts"
            ]
        );
    }

    #[test]
    fn findings_of_two_spellings_of_one_name_print_as_separate_runs_in_byte_order() {
        let (nfd, nfc) = ("e\u{301}.ts", "\u{e9}.ts");

        let finding = |line, rule| Finding {
            line,
            col: 1,
            rule,
            message: format!("at {line}"),
            fixable: false,
        };

        let file = |path: &str, findings| FileFindings {
            path: path.to_owned(),
            findings,
        };

        let files = vec![
            file("f.ts", vec![finding(1, Rule::Wall)]),
            file(
                nfd,
                vec![
                    finding(4, Rule::Wall),
                    finding(1, Rule::Wall),
                    finding(2, Rule::BlockSpacing),
                ],
            ),
            file("a.ts", vec![finding(9, Rule::Wall)]),
            file(
                nfc,
                vec![
                    finding(2, Rule::BlockSpacing),
                    finding(3, Rule::Wall),
                    finding(1, Rule::Wall),
                ],
            ),
        ];

        let mut flattened: Vec<Located> = files.iter().flat_map(FileFindings::located).collect();
        flattened.sort_by(|left, right| {
            left.path
                .cmp(right.path)
                .then_with(|| compare_in_file(left.finding, right.finding))
        });

        let lines: Vec<String> = flattened
            .iter()
            .map(|located| {
                format!(
                    "{}:{} {}",
                    located.path,
                    located.finding.line,
                    located.finding.rule.id()
                )
            })
            .collect();

        assert_eq!(
            lines,
            [
                "a.ts:9 wall".to_owned(),
                format!("{nfd}:1 wall"),
                format!("{nfd}:2 block-spacing"),
                format!("{nfd}:4 wall"),
                "f.ts:1 wall".to_owned(),
                format!("{nfc}:1 wall"),
                format!("{nfc}:2 block-spacing"),
                format!("{nfc}:3 wall"),
            ]
        );

        let text: String = flattened
            .iter()
            .map(|located| {
                let finding = located.finding;
                format!(
                    "{}:{}:{}: {}: {} (by hand)\n",
                    located.path,
                    finding.line,
                    finding.col,
                    finding.rule.id(),
                    finding.message
                )
            })
            .collect();

        let json = format!("{}\n", serde_json::to_string(&flattened).unwrap());
        assert!(json.starts_with(
            "[{\"path\":\"a.ts\",\"line\":9,\"col\":1,\"rule\":\"wall\",\"message\":\"at 9\",\"fixable\":false},"
        ));
        let findings = Findings::new(files);
        for (json_flag, expected) in [(false, text), (true, json)] {
            let mut printed = Vec::new();
            write_findings(&mut printed, &findings, json_flag).unwrap();
            assert_eq!(String::from_utf8(printed).unwrap(), expected);
        }
    }

    #[test]
    fn rows_mark_by_hand_rules_and_quote_paths_that_would_break_the_line() {
        let finding = |rule, fixable| Finding {
            line: 2,
            col: 3,
            rule,
            message: "msg".to_owned(),
            fixable,
        };

        let file = |path: &str, findings| FileFindings {
            path: path.to_owned(),
            findings,
        };

        let findings = Findings::new(vec![
            file(
                "a.ts",
                vec![
                    finding(Rule::AfterMultiline, true),
                    finding(Rule::Parse, false),
                    finding(Rule::Wall, false),
                ],
            ),
            file("b\x1b[31m.ts", vec![finding(Rule::Write, false)]),
            file("c\n\"d.ts", vec![finding(Rule::Error, false)]),
        ]);

        let mut printed = Vec::new();
        write_findings(&mut printed, &findings, false).unwrap();
        assert_eq!(
            String::from_utf8(printed).unwrap(),
            concat!(
                "a.ts:2:3: after-multiline: msg\n",
                "a.ts:2:3: parse: msg\n",
                "a.ts:2:3: wall: msg (by hand)\n",
                "\"b\\033[31m.ts\":2:3: write: msg\n",
                "\"c\\n\\\"d.ts\":2:3: error: msg\n",
            )
        );
    }
}
