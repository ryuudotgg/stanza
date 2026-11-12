use std::collections::{BTreeSet, HashSet};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use super::{Io, Program};
use crate::engine::types::{Braces, Located};
use crate::files::{EmptyReason, StagedFile};
use crate::format::{Findings, Formatted};
use crate::paths::{invalid_name, relative, relative_bytes};
use crate::step::Unread;
use crate::text::code_unit_order;

pub(crate) fn invalid_lines(cwd: Option<&str>, names: &[PathBuf]) -> Vec<String> {
    names
        .iter()
        .map(|path| {
            let bytes = path.as_os_str().as_bytes();
            match cwd {
                Some(cwd) => invalid_name(&relative_bytes(cwd, bytes)),
                None => invalid_name(bytes),
            }
        })
        .collect()
}

pub(crate) fn printed_path(path: &str, cwd: Option<&str>) -> String {
    match cwd.map(|cwd| relative(cwd, path)) {
        Some(output) if !output.is_empty() => output,
        _ => path.to_owned(),
    }
}

pub(crate) const FINDINGS_CHUNK: usize = 1 << 16;

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
            let Located { path, finding } = located;
            writeln!(
                chunk,
                "{path}:{}:{} {} {}",
                finding.line,
                finding.col,
                finding.rule.id(),
                finding.message
            )?;
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
                "could not tell whether {files} enforces braces, so braces stay; pass --braces or --no-braces to settle it"
            )
        }),
        (&unread.width, |files| {
            format!(
                "could not read the line width from {files}, so stanza used the next source for it"
            )
        }),
    ];

    let mut lines = Vec::new();
    for (paths, message) in notices {
        if paths.is_empty() {
            continue;
        }

        let unique: BTreeSet<&String> = paths.iter().collect();
        let mut files: Vec<String> = unique
            .into_iter()
            .map(|path| printed_path(path, cwd))
            .collect();

        files.sort_by(|left, right| code_unit_order(left, right));
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
        list.iter()
            .map(|file| shell_word(&printed_path(&file.path, Some(cwd))))
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
    use crate::text::locale_compare;

    #[test]
    fn findings_of_files_whose_paths_collate_as_equal_print_as_one_interleaved_run() {
        let (nfd, nfc) = ("e\u{301}.ts", "\u{e9}.ts");
        assert!(locale_compare(nfd, nfc).is_eq());

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
            locale_compare(left.path, right.path)
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
                format!("{nfc}:1 wall"),
                format!("{nfd}:2 block-spacing"),
                format!("{nfc}:2 block-spacing"),
                format!("{nfc}:3 wall"),
                format!("{nfd}:4 wall"),
                "f.ts:1 wall".to_owned(),
            ]
        );

        let text: String = flattened
            .iter()
            .map(|located| {
                let finding = located.finding;
                format!(
                    "{}:{}:{} {} {}\n",
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
}
